//! MyGraph engine core.
//!
//! Every module implements one part of the normative specification in `spec/`; module docs name
//! the spec file and code comments cite rule IDs. The shared vectors in `conformance/vectors/`
//! are the tests (see `tests/vectors.rs`).

pub mod engine;
pub mod ids;
mod lifecycle;
pub mod model;
pub mod name_key;
pub mod names;
mod read;
pub mod reason;
pub mod resolve;
mod search;
pub mod state;
pub mod store;
pub mod table;
pub mod text;
mod ucd;
pub mod values;
pub mod vocab;
mod write;

pub use reason::Reason;

/// Version of the specification this build implements (`spec/VERSION`).
pub const SPEC_VERSION: &str = include_str!("../../../spec/VERSION").trim_ascii();

/// Storage format version this build reads and writes (`spec/storage/schema.sql`).
pub const STORAGE_FORMAT_VERSION: u32 = 1;
