use std::path::PathBuf;
use std::process::Command;

use pipeline::catalog::Catalog;
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
