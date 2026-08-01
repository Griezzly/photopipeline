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
fn count_pending_ignores_sidecar_jpgs() {
    let d = TempDir::new().unwrap();
    let roots = LibraryRoots {
        data: d.path().join("data"),
        cache: d.path().join("cache"),
    };
    let folder = d.path().join("photos");
    std::fs::create_dir_all(&folder).unwrap();

    // A RAW with a same-stem JPG beside it — the pair ingest collapses to the RAW
    // alone. The RAW's bytes are irrelevant to this test: both the walk and the
    // sidecar rule work on paths, and a preview that fails to decode is a warning,
    // not an ingest failure.
    std::fs::write(folder.join("IMG_0001.dng"), b"not a decodable raw").unwrap();
    make_jpg(&folder, "IMG_0001.jpg");
    make_jpg(&folder, "standalone.jpg");

    let lib = open_or_create_library(&roots, &folder).unwrap();
    let cfg = Config::default();

    // Before any scan: the RAW and the standalone JPG are pending. The sidecar is
    // not — ingest will never process it, so counting it can never reach zero.
    assert_eq!(
        count_pending(&folder, &lib.catalog, &cfg.ingest).unwrap(),
        2
    );

    let hub = ModelHub::empty();
    let sink = Arc::new(RecordingSink::default());
    analyze_folder(&folder, &lib.catalog, &lib.cache, &hub, &cfg, sink.as_ref()).unwrap();

    // After the scan nothing is left over — this is what the UI's "N new photos"
    // banner reads, and it must be able to clear.
    assert_eq!(
        count_pending(&folder, &lib.catalog, &cfg.ingest).unwrap(),
        0
    );
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
    assert!(stages.contains(&"detecting defects".to_string()));
    assert!(stages.contains(&"scoring quality".to_string()));
    assert!(stages.contains(&"calibrating".to_string()));
    assert!(stages.contains(&"grouping duplicates".to_string()));
    assert_eq!(*sink.total.lock().unwrap(), 2);
    // Progress now spans multiple phases: 2 files ingested + 2 files defect-analyzed
    // (ML skipped here — empty hub returns before reporting).
    assert_eq!(*sink.ticks.lock().unwrap(), 4);

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
