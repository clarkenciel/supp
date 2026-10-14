use std::fmt;

use uuid::Uuid;

/// Identifies one list document. Generated on the client so lists can be
/// created offline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocId(Uuid);

#[derive(Debug, thiserror::Error)]
#[error("doc id must be 16 bytes, got {0}")]
pub struct InvalidDocId(usize);

impl DocId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, InvalidDocId> {
        Uuid::from_slice(bytes)
            .map(Self)
            .map_err(|_| InvalidDocId(bytes.len()))
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl Default for DocId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_round_trip() {
        let id = DocId::new();
        assert_eq!(DocId::from_bytes(id.as_bytes()).unwrap(), id);
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(DocId::from_bytes(&[0; 15]).is_err());
    }
}
