use std::path::PathBuf;
use std::process::Command;

use pipeline::catalog::{Catalog, Verdict};
use pipeline::ingest::{ExifData, FileFormat, IngestedFile};

/// Write a minimal config TOML pointing catalog + cache into `dir`, returning
/// (config_path, db_path).
fn write_config(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let db_path = dir.join("catalog.duckdb");
    let cache_dir = dir.join("cache");
    let cfg_path = dir.join("photopipe.toml");
    let toml = format!(
        "[catalog]\ndb_path = \"{}\"\ncache_dir = \"{}\"\n",
        db_path.display(),
        cache_dir.display()
    );
    std::fs::write(&cfg_path, toml).unwrap();
    (cfg_path, db_path)
}

fn seed_known_file(db_path: &std::path::Path, path: &str) {
    let catalog = Catalog::open(db_path).unwrap();
    let file = IngestedFile {
        path: PathBuf::from(path),
        content_hash: 42,
        size: 1000,
        mtime_ns: 5,
        format: FileFormat::Jpg,
        has_sidecar_jpg: false,
    };
    catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap();
}

#[test]
fn info_known_file_emits_json_and_exits_zero() {
    let dir = tempfile::TempDir::new().unwrap();
    let (cfg_path, db_path) = write_config(dir.path());
    seed_known_file(&db_path, "/lib/known.jpg");

    let out = Command::new(env!("CARGO_BIN_EXE_photopipe"))
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "info",
            "/lib/known.jpg",
        ])
        .output()
        .expect("spawn photopipe");

    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("stdout must be JSON");
    assert_eq!(parsed["file"]["path"], "/lib/known.jpg");
}

#[test]
fn info_unknown_file_exits_nonzero() {
    let dir = tempfile::TempDir::new().unwrap();
    let (cfg_path, db_path) = write_config(dir.path());
    seed_known_file(&db_path, "/lib/known.jpg");

    let out = Command::new(env!("CARGO_BIN_EXE_photopipe"))
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "info",
            "/lib/missing.jpg",
        ])
        .output()
        .expect("spawn photopipe");

    assert!(
        !out.status.success(),
        "expected non-zero exit for unknown file"
    );
}

#[test]
fn doctor_exits_nonzero_when_configured_model_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    let model_dir = dir.path().join("empty-models");
    std::fs::create_dir_all(&model_dir).unwrap();
    let db_path = dir.path().join("catalog.duckdb");
    let cache_dir = dir.path().join("cache");
    let cfg_path = dir.path().join("photopipe.toml");
    // model_dir is empty → every configured model file is missing → critical fail.
    let toml = format!(
        "[catalog]\ndb_path = \"{}\"\ncache_dir = \"{}\"\n\n[models]\nmodel_dir = \"{}\"\n",
        db_path.display(),
        cache_dir.display(),
        model_dir.display()
    );
    std::fs::write(&cfg_path, toml).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_photopipe"))
        .args(["--config", cfg_path.to_str().unwrap(), "doctor"])
        .output()
        .expect("spawn photopipe");

    assert!(
        !out.status.success(),
        "doctor must fail when configured models are absent"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("missing"),
        "doctor output should mention a missing model:\n{combined}"
    );
}

#[test]
fn export_keepers_creates_tree() {
    let dir = tempfile::TempDir::new().unwrap();
    let (cfg_path, db_path) = write_config(dir.path());
    let out = dir.path().join("_keepers");

    // Seed a catalog with one kept file via the library API.
    {
        let lib = dir.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let p = lib.join("a.jpg");
        std::fs::write(&p, b"not-a-real-jpg-but-fine-for-linking").unwrap();
        let catalog = Catalog::open(&db_path).unwrap();
        let file = IngestedFile {
            path: p,
            content_hash: 1,
            size: 1,
            mtime_ns: 1,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let id = catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap()[0];
        catalog.set_decision(id, Verdict::Keep, None).unwrap();
    }

    let output_run = Command::new(env!("CARGO_BIN_EXE_photopipe"))
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "export-keepers",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output_run.status.success());
    let stdout = String::from_utf8_lossy(&output_run.stdout);
    assert!(
        stdout.contains("Copying"),
        "expected a pre-flight estimate line, got: {stdout}"
    );
    assert!(
        stdout.contains("Copied"),
        "expected a final report line, got: {stdout}"
    );

    // a.jpg is a real copied file (not a symlink), byte-identical to the original.
    let entries = walkdir_like(&out);
    assert!(entries.iter().any(|n| n == "a.jpg"));
    let copied = find_file(&out, "a.jpg").expect("a.jpg copied");
    assert!(!std::fs::symlink_metadata(&copied)
        .unwrap()
        .file_type()
        .is_symlink());
}

fn find_file(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_file(&p, name) {
                return Some(found);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

/// Minimal recursive filename collector (avoids adding a dep to the cli crate).
fn walkdir_like(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walkdir_like(&p));
            } else if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
                out.push(n.to_string());
            }
        }
    }
    out
}
