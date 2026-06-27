use std::{fs, path::PathBuf};

pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn open(root: PathBuf) -> std::io::Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Absolute path where the WebP preview for `hash` is stored.
    pub fn path(&self, hash: u128) -> PathBuf {
        let hex = format!("{:032x}", hash);
        self.root
            .join("previews")
            .join(&hex[..2])
            .join(format!("{}.webp", hex))
    }

    /// Returns `true` if a cached preview exists for `hash`.
    pub fn has(&self, hash: u128) -> bool {
        self.path(hash).exists()
    }

    /// Write `bytes` to the cache for `hash`, creating parent dirs as needed.
    pub fn write(&self, hash: u128, bytes: &[u8]) -> std::io::Result<()> {
        let p = self.path(hash);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&p, bytes)
    }

    /// Absolute path where the small grid thumbnail for `hash` is stored.
    pub fn thumb_path(&self, hash: u128) -> PathBuf {
        let hex = format!("{:032x}", hash);
        self.root
            .join("thumbs")
            .join(&hex[..2])
            .join(format!("{}.webp", hex))
    }

    pub fn has_thumb(&self, hash: u128) -> bool {
        self.thumb_path(hash).exists()
    }

    pub fn write_thumb(&self, hash: u128, bytes: &[u8]) -> std::io::Result<()> {
        let p = self.thumb_path(hash);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&p, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_bucketing() {
        let cache = Cache {
            root: PathBuf::from("/tmp/test"),
        };
        let hash: u128 = 0xdeadbeef_00000000_00000000_00000000;
        let p = cache.path(hash);
        let s = p.to_string_lossy();
        assert!(s.contains("de")); // first two hex chars of hash
        assert!(s.ends_with(".webp"));
    }

    #[test]
    fn write_and_has() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache = Cache::open(dir.path().to_owned()).unwrap();
        let hash: u128 = 42;
        assert!(!cache.has(hash));
        cache.write(hash, b"fake webp data").unwrap();
        assert!(cache.has(hash));
    }

    #[test]
    fn thumb_path_is_separate_namespace() {
        let cache = Cache {
            root: PathBuf::from("/tmp/test"),
        };
        let hash: u128 = 0x1234;
        let t = cache.thumb_path(hash);
        let p = cache.path(hash);
        assert!(t.to_string_lossy().contains("thumbs"));
        assert!(p.to_string_lossy().contains("previews"));
        assert_ne!(t, p);
    }

    #[test]
    fn write_and_has_thumb() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache = Cache::open(dir.path().to_owned()).unwrap();
        let hash: u128 = 7;
        assert!(!cache.has_thumb(hash));
        cache.write_thumb(hash, b"webp").unwrap();
        assert!(cache.has_thumb(hash));
    }
}
