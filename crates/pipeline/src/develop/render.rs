//! The RawTherapee backend: writes a `.pp3` pair and drives `rawtherapee-cli`
//! as a subprocess to a 16-bit sRGB TIFF.
//!
//! photopipe never links against RawTherapee, only executes it, so its GPL-3
//! licence does not propagate (spec §3).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::DevelopConfig;
use crate::develop::decide::EditRecipe;
use crate::develop::pp3::{emit_pp3, BASE_PP3};
use crate::develop::DevelopError;

/// Stored in `edits.renderer`.
pub const RENDERER_NAME: &str = "rawtherapee";

/// A completed baseline render.
pub struct RenderedTiff {
    /// 16-bit sRGB TIFF in the temp directory. Large — delete it as soon as the
    /// JPEG is encoded (a 60MP raw is roughly 350 MB here).
    pub tiff: PathBuf,
    /// The per-photo profile, kept so it can be copied next to the output JPEG
    /// as an escape hatch for reopening the photo in RawTherapee.
    pub pp3: PathBuf,
}

pub struct Pp3Renderer {
    pub(crate) exe: PathBuf,
}

impl Pp3Renderer {
    pub fn new(cfg: &DevelopConfig) -> Self {
        let exe = if cfg.rawtherapee_path.is_empty() {
            // Bare name: let the OS search PATH.
            PathBuf::from("rawtherapee-cli")
        } else {
            crate::config::expand_tilde(Path::new(&cfg.rawtherapee_path))
        };
        Self { exe }
    }

    /// The resolved executable path (configured value, or the bare name if the
    /// OS is expected to search `PATH`). Exposed for diagnostics such as
    /// `photopipe doctor`.
    pub fn exe_path(&self) -> &Path {
        &self.exe
    }

    /// Confirm the binary exists and runs. Called once before a run rather than
    /// per photo, so a missing dependency fails immediately instead of
    /// producing hundreds of identical per-file warnings.
    ///
    /// **Do not gate on the exit status here.** Verified against RawTherapee
    /// 5.13: `--version` exits 2 and `-h` exits 255, while a real render exits
    /// 0. Treating a non-zero status as failure would make `probe()` fail on
    /// every machine and abort `finish` unconditionally. The presence of a
    /// parseable version banner is the actual success signal, and it arrives on
    /// stdout or stderr depending on build.
    pub fn probe(&self) -> Result<String, DevelopError> {
        let out = Command::new(&self.exe)
            .arg("--version")
            .output()
            .map_err(|e| DevelopError::Render {
                path: self.exe.clone(),
                reason: format!("cannot execute: {e}"),
            })?;
        let banner = [&out.stdout, &out.stderr]
            .into_iter()
            .filter_map(|buf| {
                String::from_utf8_lossy(buf)
                    .lines()
                    .find(|l| l.contains("RawTherapee"))
                    .map(|l| l.trim().to_string())
            })
            .next();
        banner.ok_or_else(|| DevelopError::Render {
            path: self.exe.clone(),
            reason: "ran, but printed no RawTherapee version banner".into(),
        })
    }

    /// Render `raw` through `recipe` into `tmp_dir`.
    ///
    /// Writes both profiles into `tmp_dir` — never beside the original, which
    /// would violate the non-destructive contract. RawTherapee's own convention
    /// is to write `photo.raw.pp3` next to the source; we deliberately do not.
    pub fn render(
        &self,
        raw: &Path,
        recipe: &EditRecipe,
        tmp_dir: &Path,
    ) -> Result<RenderedTiff, DevelopError> {
        let stem = raw
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("photo")
            .to_string();

        let base_path = tmp_dir.join("base.pp3");
        let photo_path = tmp_dir.join(format!("{stem}.pp3"));
        write_file(&base_path, BASE_PP3)?;
        write_file(&photo_path, &emit_pp3(recipe))?;

        let args = build_args(&base_path, &photo_path, tmp_dir, raw);
        let out =
            Command::new(&self.exe)
                .args(&args)
                .output()
                .map_err(|e| DevelopError::Render {
                    path: raw.to_path_buf(),
                    reason: format!("cannot execute {}: {e}", self.exe.display()),
                })?;

        if !out.status.success() {
            return Err(DevelopError::Render {
                path: raw.to_path_buf(),
                reason: format!(
                    "exited {}: {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            });
        }

        // RawTherapee derives the output name from the input stem.
        let tiff = tmp_dir.join(format!("{stem}.tif"));
        if !tiff.exists() {
            return Err(DevelopError::Render {
                path: raw.to_path_buf(),
                reason: format!("expected output {} was not created", tiff.display()),
            });
        }
        Ok(RenderedTiff {
            tiff,
            pp3: photo_path,
        })
    }
}

/// Build the argument vector.
///
/// Split out as a free function so the ordering contract is unit-testable
/// without a RawTherapee installation. `OsString` throughout, because a photo
/// path is not guaranteed to be valid UTF-8 on any platform.
fn build_args(base_pp3: &Path, photo_pp3: &Path, out_dir: &Path, input: &Path) -> Vec<OsString> {
    vec![
        // Overwrite without prompting; otherwise the CLI blocks on stdin.
        OsString::from("-Y"),
        // 16-bit TIFF: the domain the look stage operates in.
        OsString::from("-t"),
        OsString::from("-b16"),
        // Profiles stack in order, so the per-photo one wins.
        OsString::from("-p"),
        base_pp3.as_os_str().to_owned(),
        OsString::from("-p"),
        photo_pp3.as_os_str().to_owned(),
        OsString::from("-o"),
        out_dir.as_os_str().to_owned(),
        // -c must be last: everything after it is treated as input.
        OsString::from("-c"),
        input.as_os_str().to_owned(),
    ]
}

fn write_file(path: &Path, contents: &str) -> Result<(), DevelopError> {
    std::fs::write(path, contents).map_err(|source| DevelopError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DevelopConfig;

    fn cfg_with(path: &str) -> DevelopConfig {
        DevelopConfig {
            rawtherapee_path: path.into(),
            ..Default::default()
        }
    }

    /// An empty configured path means "search PATH" — not "run the empty string".
    #[test]
    fn empty_path_falls_back_to_bare_name() {
        let r = Pp3Renderer::new(&cfg_with(""));
        assert_eq!(r.exe, std::path::PathBuf::from("rawtherapee-cli"));
    }

    #[test]
    fn configured_path_is_used_verbatim() {
        let r = Pp3Renderer::new(&cfg_with("/opt/rt/rawtherapee-cli"));
        assert_eq!(r.exe, std::path::PathBuf::from("/opt/rt/rawtherapee-cli"));
    }

    /// The argument vector is the contract with RawTherapee. Order matters:
    /// `-c <input>` must come last, and the two `-p` flags must be base-then-photo
    /// so the per-photo profile wins.
    #[test]
    fn arguments_are_ordered_base_then_photo_then_input() {
        let args = build_args(
            std::path::Path::new("/tmp/base.pp3"),
            std::path::Path::new("/tmp/photo.pp3"),
            std::path::Path::new("/tmp/out"),
            std::path::Path::new("/photos/a.arw"),
        );
        let strs: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(strs.last().unwrap(), "/photos/a.arw");
        assert_eq!(strs[strs.len() - 2], "-c");

        let base_at = strs.iter().position(|s| s == "/tmp/base.pp3").unwrap();
        let photo_at = strs.iter().position(|s| s == "/tmp/photo.pp3").unwrap();
        assert!(base_at < photo_at, "base profile must be applied first");

        // -Y overwrites without prompting; without it the CLI blocks on stdin.
        assert!(strs.contains(&"-Y".to_string()));
        // -t -b16 is the 16-bit TIFF the look stage needs.
        assert!(strs.contains(&"-t".to_string()));
        assert!(strs.contains(&"-b16".to_string()));
        // -d would read the user's GUI default profile (spec §4). Never.
        assert!(!strs.contains(&"-d".to_string()));
    }

    /// A missing binary must surface as a typed error naming the file, not a
    /// panic and not a silent skip.
    #[test]
    fn missing_binary_is_a_typed_error() {
        let r = Pp3Renderer::new(&cfg_with("/nonexistent/rawtherapee-cli"));
        let err = r.probe().expect_err("probe should fail");
        assert!(matches!(err, DevelopError::Render { .. }), "got {err:?}");
    }

    /// A binary that runs but is not RawTherapee must be rejected. Guards the
    /// exit-status trap from the other side: since probe() cannot gate on the
    /// status (RawTherapee 5.13 exits 2 on --version), the version banner is
    /// the only success signal, so a successful non-RawTherapee binary must
    /// still fail.
    #[test]
    fn a_non_rawtherapee_binary_is_rejected() {
        let exe = if cfg!(windows) { "cmd" } else { "true" };
        let r = Pp3Renderer::new(&cfg_with(exe));
        assert!(r.probe().is_err(), "`{exe}` must not pass as RawTherapee");
    }
}
