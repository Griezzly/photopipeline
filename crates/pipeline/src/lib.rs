pub mod cache;
pub mod calibration;
pub mod catalog;
pub mod config;
pub mod dedupe;
pub mod defect;
pub mod error;
pub mod ingest;
pub mod library;
pub mod ml;
pub mod models;
pub mod output;

pub use calibration::{run_calibration, CalibrationReport};
pub use dedupe::{run_dedupe, DedupeReport};
pub use defect::analyze_defects;
pub use ingest::ingest_directory;
pub use ingest::preview::{downscale_webp, render_webp};
pub use library::{
    find_library_for_file, library_key, list_libraries, open_existing_library,
    open_or_create_library, Library, LibraryInfo, LibraryRoots,
};
pub use ml::analyze_ml;
pub use output::{
    build_keepers_tree, build_review_tree, estimate_keepers_copy, estimate_review_copy,
    humanize_bytes, CopyEstimate, KeepersReport, ReviewTreeReport,
};
