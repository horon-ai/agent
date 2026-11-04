//! Content-addressed blob storage and the byte accounting the budget uses.
//!
//! Bytes are addressed by SHA-256, so identical content is stored once no
//! matter how many file states refer to it. A blob lives while at least one
//! recorded state holds a reference; dropping the last reference frees its
//! bytes from the usage total.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const HEX: &[u8; 16] = b"0123456789abcdef";

/// SHA-256 digest of one file's bytes, as 64 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlobId(String);

impl BlobId {
    /// Digest `bytes` without storing them, so a caller can compare a file it
    /// just read against a recorded state.
    pub fn of(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut hex = String::with_capacity(digest.len() * 2);
        for &byte in digest.as_slice() {
            hex.push(HEX[(byte >> 4) as usize] as char);
            hex.push(HEX[(byte & 0x0f) as usize] as char);
        }
        Self(hex)
    }
}

impl std::fmt::Display for BlobId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

struct StoredBlob {
    bytes: Vec<u8>,
    refs: usize,
}

pub(crate) struct BlobStore {
    budget_bytes: usize,
    used_bytes: usize,
    blobs: HashMap<BlobId, StoredBlob>,
}

impl BlobStore {
    pub(crate) fn new(budget_bytes: usize) -> Self {
        Self {
            budget_bytes,
            used_bytes: 0,
            blobs: HashMap::new(),
        }
    }

    /// Store `bytes` and take one reference. Existing content is reused, so a
    /// repeated write only raises the reference count.
    pub(crate) fn insert(&mut self, bytes: &[u8]) -> BlobId {
        let id = BlobId::of(bytes);
        match self.blobs.get_mut(&id) {
            Some(blob) => blob.refs += 1,
            None => {
                self.used_bytes += bytes.len();
                self.blobs.insert(
                    id.clone(),
                    StoredBlob {
                        bytes: bytes.to_vec(),
                        refs: 1,
                    },
                );
            }
        }
        id
    }

    /// Drop one reference. The bytes are freed with the last one.
    pub(crate) fn release(&mut self, id: &BlobId) {
        let Some(blob) = self.blobs.get_mut(id) else {
            return;
        };
        blob.refs = blob.refs.saturating_sub(1);
        if blob.refs > 0 {
            return;
        }
        if let Some(blob) = self.blobs.remove(id) {
            self.used_bytes = self.used_bytes.saturating_sub(blob.bytes.len());
        }
    }

    pub(crate) fn get(&self, id: &BlobId) -> Option<&[u8]> {
        self.blobs.get(id).map(|blob| blob.bytes.as_slice())
    }

    pub(crate) fn blob_count(&self) -> usize {
        self.blobs.len()
    }

    pub(crate) fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    pub(crate) fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }
}
