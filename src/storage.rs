//! Filesystem blob store — replaces minikeyvalue entirely.
//!
//! Every uploaded file (and every derived artifact: coords.json, events.json,
//! sprite.jpg, transcoded HLS) is addressed by an opaque string key. The key is
//! sharded into subdirectories so a single directory never holds millions of
//! files. Keys follow the reference scheme so parsing/URL code ports cleanly:
//!
//!   blob key: `{dongle}_{timestamp}--{segment}--{file}`
//!
//! e.g. `1d3...f9_2024-01-02--03-04-05--7--qcamera.ts`

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        BlobStore { root: root.into() }
    }

    /// Map a key to an on-disk path, sharded by the first 4 hex chars of its
    /// sha256 (so listing/cleanup stays cheap). The raw key is preserved as the
    /// filename (sanitised) so paths remain debuggable.
    pub fn path_for(&self, key: &str) -> PathBuf {
        let digest = Sha256::digest(key.as_bytes());
        let hex = hex::encode(digest);
        let (a, b) = (&hex[0..2], &hex[2..4]);
        self.root.join(a).join(b).join(sanitize(key))
    }

    /// Store bytes under `key`, creating parent dirs. Overwrites.
    pub async fn put(&self, key: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let path = self.path_for(key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // Write to a temp file then rename for atomicity.
        let tmp = path.with_extension("part");
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(path)
    }

    pub async fn get(&self, key: &str) -> std::io::Result<Vec<u8>> {
        tokio::fs::read(self.path_for(key)).await
    }

    pub async fn exists(&self, key: &str) -> bool {
        tokio::fs::try_exists(self.path_for(key)).await.unwrap_or(false)
    }

    pub async fn size(&self, key: &str) -> Option<u64> {
        tokio::fs::metadata(self.path_for(key))
            .await
            .ok()
            .map(|m| m.len())
    }

    pub async fn delete(&self, key: &str) -> std::io::Result<()> {
        match tokio::fs::remove_file(self.path_for(key)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Build the canonical blob key from its parts.
pub fn blob_key(dongle: &str, timestamp: &str, segment: i64, file: &str) -> String {
    format!("{dongle}_{timestamp}--{segment}--{file}")
}

/// Replace path-hostile characters so the key is safe as a single filename.
fn sanitize(key: &str) -> String {
    key.chars()
        .map(|c| match c {
            '/' | '\\' | '\0' => '_',
            other => other,
        })
        .collect()
}
