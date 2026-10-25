//! Local blob store — spec/storage/storage-format.md §5.

use mygraph_core::store::{BlobStore, StoreFailure, sha256_hex};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// `<data_dir>/blobs/` ([STORE-DIR-1]).
#[derive(Debug, Clone)]
pub struct BlobDir {
    root: PathBuf,
}

fn valid_hash(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Distinguishes temporary files written at the same time by one process.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl BlobDir {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("blobs"),
        }
    }

    /// [STORE-BLOB-2] `sha256/<h1h2>/<h3h4>/<hash>`.
    pub fn path_of(&self, sha256: &str) -> PathBuf {
        self.root
            .join("sha256")
            .join(&sha256[..2])
            .join(&sha256[2..4])
            .join(sha256)
    }
}

impl BlobStore for BlobDir {
    /// [STORE-BLOB-3] temporary file in the same folder, synced, then renamed.
    fn put(&self, bytes: &[u8]) -> Result<String, StoreFailure> {
        let hash = sha256_hex(bytes);
        let path = self.path_of(&hash);
        if path.is_file() {
            return Ok(hash);
        }
        let dir = path.parent().expect("blob folder");
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            // Another writer stored the same bytes first.
            if !path.is_file() {
                return Err(e.into());
            }
        }
        Ok(hash)
    }

    fn size(&self, sha256: &str) -> Result<Option<u64>, StoreFailure> {
        if !valid_hash(sha256) {
            return Ok(None);
        }
        match std::fs::metadata(self.path_of(sha256)) {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, StoreFailure> {
        if !valid_hash(sha256) {
            return Ok(None);
        }
        match std::fs::read(self.path_of(sha256)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_is_content_addressed_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobDir::new(dir.path());
        let h = blobs.put(b"hello").unwrap();
        assert_eq!(
            h,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(
            blobs
                .path_of(&h)
                .ends_with(Path::new("sha256/2c/f2").join(&h))
        );
        assert_eq!(blobs.put(b"hello").unwrap(), h);
        assert_eq!(blobs.size(&h).unwrap(), Some(5));
        assert_eq!(blobs.get(&h).unwrap().as_deref(), Some(&b"hello"[..]));
        assert_eq!(blobs.size(&"0".repeat(64)).unwrap(), None);
        assert_eq!(blobs.size("../etc").unwrap(), None);
        // No temporary files are left behind.
        let left: Vec<_> = std::fs::read_dir(blobs.path_of(&h).parent().unwrap())
            .unwrap()
            .collect();
        assert_eq!(left.len(), 1);
    }
}
