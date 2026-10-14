//! Types shared by the server and the web client: wire protocol, document
//! schema and identifiers.

pub mod doc_id;
pub mod schema;

pub use doc_id::DocId;

/// Generated protobuf types.
pub mod proto {
    pub mod sync {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/supp.sync.v1.rs"));
        }
    }
}

/// Protocol version spoken by this build. Bump when a change cannot be
/// handled through protobuf's own evolution rules.
pub const PROTOCOL_VERSION: u32 = 1;

/// Oldest client protocol version the server still accepts.
pub const MIN_PROTOCOL_VERSION: u32 = 1;
