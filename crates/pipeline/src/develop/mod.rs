//! Automatic RAW development: measure → decide → render → look.
//!
//! Stage boundaries are deliberate. `measure` touches pixels but makes no
//! decisions; `decide` makes every decision but never touches a pixel; `render`
//! and `pp3` translate a decision into RawTherapee's vocabulary. Keeping those
//! separate is what makes the tuning logic testable over plain numbers.

pub mod decide;
pub mod illuminant;
pub mod measure;
pub mod pp3;
pub mod render;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DevelopError {
    #[error("raw decode failed for {path}: {reason}")]
    Decode {
        path: std::path::PathBuf,
        reason: String,
    },
    #[error("renderer failed for {path}: {reason}")]
    Render {
        path: std::path::PathBuf,
        reason: String,
    },
    #[error("IO error for {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}
