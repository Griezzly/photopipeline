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

use std::path::Path;

use crate::catalog::EditIdentity;

/// True when the recorded render still satisfies what we now want.
///
/// Mirrors the "missing or differs" semantics of `output::copy_file`: the
/// identity must match *and* the output must still be on disk at the recorded
/// size. Re-running `finish` over an unchanged library must do zero work — a
/// correctness requirement, not a perf goal.
pub fn is_up_to_date(
    existing: &EditIdentity,
    wanted: &EditIdentity,
    output_path: Option<&Path>,
    output_size: Option<i64>,
) -> bool {
    if existing != wanted {
        return false;
    }
    let (Some(path), Some(size)) = (output_path, output_size) else {
        return false;
    };
    match std::fs::metadata(path) {
        Ok(m) => m.len() == size as u64,
        Err(_) => false,
    }
}
