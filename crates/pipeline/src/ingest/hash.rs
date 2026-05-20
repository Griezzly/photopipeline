use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

use xxhash_rust::xxh3::Xxh3;

const CHUNK: usize = 1 << 20; // 1 MiB

/// Hash the file at `path` using XXH3-128 and return the digest.
pub fn hash_file(path: &Path) -> std::io::Result<u128> {
    let f = File::open(path)?;
    let mut reader = BufReader::with_capacity(CHUNK, f);
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.digest128())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn hash_known_input() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello world").unwrap();
        let h = hash_file(f.path()).unwrap();
        assert_ne!(h, 0);
        // Idempotent.
        assert_eq!(h, hash_file(f.path()).unwrap());
    }

    #[test]
    fn different_content_different_hash() {
        let mut f1 = NamedTempFile::new().unwrap();
        let mut f2 = NamedTempFile::new().unwrap();
        f1.write_all(b"content A").unwrap();
        f2.write_all(b"content B").unwrap();
        assert_ne!(hash_file(f1.path()).unwrap(), hash_file(f2.path()).unwrap());
    }
}
