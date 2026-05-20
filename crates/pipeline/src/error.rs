use thiserror::Error;

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("IO error for {path}: {source}")]
    Io { path: std::path::PathBuf, #[source] source: std::io::Error },
    #[error("EXIF parse error for {path}: {reason}")]
    Exif { path: std::path::PathBuf, reason: String },
    #[error("preview extraction failed for {path}: {reason}")]
    Preview { path: std::path::PathBuf, reason: String },
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("database error: {0}")]
    Db(String),
    #[error("schema migration failed at version {version}: {reason}")]
    Migration { version: u32, reason: String },
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("model file not found: {0}")]
    NotFound(std::path::PathBuf),
    #[error("ONNX Runtime error: {0}")]
    Ort(String),
    #[error("unsupported execution provider: {0}")]
    Provider(String),
}
