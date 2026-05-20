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
}
