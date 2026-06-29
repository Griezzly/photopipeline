use std::sync::{Arc, Mutex};

use image::{ImageBuffer, Rgb};
use pipeline::analyze::{analyze_folder, count_pending, ProgressSink};
use pipeline::config::Config;
use pipeline::library::{open_or_create_library, LibraryRoots};
use pipeline::models::ModelHub;
use tempfile::TempDir;

#[derive(Default)]
struct RecordingSink {
    stages: Mutex<Vec<String>>,
    total: Mutex<u64>,
    ticks: Mutex<u64>,
}
impl ProgressSink for RecordingSink {
    fn stage(&self, s: &str) {
        self.stages.lock().unwrap().push(s.to_string());
    }
    fn set_total(&self, t: u64) {
        *self.total.lock().unwrap() = t;
    }
    fn inc(&self) {
        *self.ticks.lock().unwrap() += 1;
    }
}

fn make_jpg(dir: &std::path::Path, name: &str) {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(48, 32, |x, _| Rgb([(x % 255) as u8, 1, 2]));
    img.save(dir.join(name)).unwrap();
}

#[test]
fn analyze_folder_runs_chain_ml_skipped_and_is_idempotent() {
    let d = TempDir::new().unwrap();
    let roots = LibraryRoots {
        data: d.path().join("data"),
        cache: d.path().join("cache"),
    };
    let folder = d.path().join("photos");
    std::fs::create_dir_all(&folder).unwrap();
    make_jpg(&folder, "a.jpg");
    make_jpg(&folder, "b.jpg");

    let lib = open_or_create_library(&roots, &folder).unwrap();
    let cfg = Config::default();
    let hub = ModelHub::empty();
    let sink = Arc::new(RecordingSink::default());

    // count_pending sees both files before scanning.
    assert_eq!(
        count_pending(&folder, &lib.catalog, &cfg.ingest).unwrap(),
        2
    );

    let report =
        analyze_folder(&folder, &lib.catalog, &lib.cache, &hub, &cfg, sink.as_ref()).unwrap();
    assert!(!report.ml_ran);
    assert_eq!(report.processed, 2);

    let stages = sink.stages.lock().unwrap().clone();
    assert!(stages.contains(&"scanning".to_string()));
    assert!(stages.contains(&"calibrating".to_string()));
    assert!(stages.contains(&"deduping".to_string()));
    assert_eq!(*sink.total.lock().unwrap(), 2);
    assert_eq!(*sink.ticks.lock().unwrap(), 2);

    // last_analyzed stamped.
    assert!(lib.catalog.library_meta().unwrap().unwrap().2.is_some());

    // idempotent: nothing pending, re-run processes 0.
    assert_eq!(
        count_pending(&folder, &lib.catalog, &cfg.ingest).unwrap(),
        0
    );
    let sink2 = Arc::new(RecordingSink::default());
    let r2 = analyze_folder(
        &folder,
        &lib.catalog,
        &lib.cache,
        &hub,
        &cfg,
        sink2.as_ref(),
    )
    .unwrap();
    assert_eq!(r2.processed, 0);
}
