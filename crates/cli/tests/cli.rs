use std::path::Path;
use std::process::{Command, Output};

use image::{ImageBuffer, Rgb};

/// A config that only sets the model dir (catalog paths are no longer config).
fn write_config(dir: &Path) -> std::path::PathBuf {
    let cfg_path = dir.join("photopipe.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[models]\nmodel_dir = \"{}\"\n",
            dir.join("models").display()
        ),
    )
    .unwrap();
    cfg_path
}

/// Run the binary with app-data redirected into `appdata` (so no real
/// libraries are touched). `cfg` is the config path.
fn run_pp(appdata: &Path, cfg: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_photopipe"))
        .env("XDG_DATA_HOME", appdata.join("data"))
        .env("XDG_CACHE_HOME", appdata.join("cache"))
        .args(["--config", cfg.to_str().unwrap()])
        .args(args)
        .output()
        .expect("spawn photopipe")
}

/// Make a folder with one tiny real JPEG; return the folder + the image path.
fn photo_folder(root: &Path, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let folder = root.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    let img = folder.join("DSC0001.jpg");
    let buf: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(48, 32, |_, _| Rgb([7, 8, 9]));
    buf.save(&img).unwrap();
    (folder, img)
}

#[test]
fn scan_then_stats_and_libraries() {
    let t = tempfile::TempDir::new().unwrap();
    let cfg = write_config(t.path());
    let appdata = t.path().join("app");
    let (folder, _img) = photo_folder(t.path(), "trip");

    let scan = run_pp(
        &appdata,
        &cfg,
        &["scan", "--no-models", folder.to_str().unwrap()],
    );
    assert!(
        scan.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&scan.stderr)
    );

    let stats = run_pp(&appdata, &cfg, &["stats", folder.to_str().unwrap()]);
    assert!(
        stats.status.success(),
        "stats failed: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    assert!(String::from_utf8_lossy(&stats.stdout).contains("Total files"));

    let libs = run_pp(&appdata, &cfg, &["libraries"]);
    assert!(libs.status.success());
    let out = String::from_utf8_lossy(&libs.stdout);
    assert!(
        out.contains("trip"),
        "libraries output missing folder: {out}"
    );
}

#[test]
fn doctor_runs_without_a_catalog() {
    let t = tempfile::TempDir::new().unwrap();
    let cfg = write_config(t.path());
    let appdata = t.path().join("app");
    // Empty model dir → models check fails → doctor exits non-zero, but it must
    // not reference any catalog and must not panic.
    std::fs::create_dir_all(t.path().join("models")).unwrap();
    let out = run_pp(&appdata, &cfg, &["doctor"]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.to_lowercase().contains("db schema"),
        "doctor should not check a fixed catalog: {combined}"
    );
}

#[test]
fn read_only_commands_resolve_library() {
    let t = tempfile::TempDir::new().unwrap();
    let cfg = write_config(t.path());
    let appdata = t.path().join("app");
    let (folder, img) = photo_folder(t.path(), "trip");

    let scan = run_pp(
        &appdata,
        &cfg,
        &["scan", "--no-models", folder.to_str().unwrap()],
    );
    assert!(
        scan.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&scan.stderr)
    );

    // stats <folder> succeeds and shows the one file.
    let stats = run_pp(&appdata, &cfg, &["stats", folder.to_str().unwrap()]);
    assert!(
        stats.status.success(),
        "stats failed: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    assert!(String::from_utf8_lossy(&stats.stdout).contains("Total files"));

    // stats on an un-scanned folder errors non-zero.
    let other = t.path().join("unscanned");
    std::fs::create_dir_all(&other).unwrap();
    let bad = run_pp(&appdata, &cfg, &["stats", other.to_str().unwrap()]);
    assert!(
        !bad.status.success(),
        "stats on un-scanned folder should fail"
    );
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("no library"),
        "expected 'no library' message"
    );

    // info <file> resolves the library by walking up to the folder.
    let info = run_pp(&appdata, &cfg, &["info", img.to_str().unwrap()]);
    assert!(
        info.status.success(),
        "info failed: {}",
        String::from_utf8_lossy(&info.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&info.stdout).expect("info JSON");
    assert_eq!(v["file"]["path"], img.to_string_lossy().as_ref());
}
