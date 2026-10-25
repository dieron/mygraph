//! The storage seam (docs/05-architecture.md §2, "Internal seams").
//!
//! The engine computes every change of a batch — rows, change-log entries, internal numbers —
//! and hands the store one [`ChangeSet`] to apply in a single transaction ([STORE-TXN-1]).

use crate::model::{Actor, Citation, Evidence, Link, Node, Row};
use serde_json::Value as Json;

pub type StoreFailure = Box<dyn std::error::Error + Send + Sync>;

/// Last internal number used per table (from the store's sequences), so the engine can assign
/// the next ones itself ([ID-LIFE-2]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub node: i64,
    pub link: i64,
    pub evidence: i64,
    pub citation: i64,
    pub batch: i64,
    pub actor: i64,
    pub seq: i64,
}

/// Everything the engine keeps in memory, read at open. Evidence comes without `content`.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
    pub evidence: Vec<Evidence>,
    pub citations: Vec<Citation>,
    pub actors: Vec<Actor>,
    /// Link types declared symmetric in `type_info`.
    pub symmetric_link_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BatchRow {
    pub idx: i64,
    pub actor: i64,
    /// `write`, `retract`, `restore`, `merge`, `admin`, `import`.
    pub op: &'static str,
    pub created_at: i64,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowOp {
    Insert(Row),
    Update(Row),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub seq: i64,
    /// `create`, `update`, `retract`, `restore`, `merge`, `purge`.
    pub op: &'static str,
    pub obj_kind: i64,
    pub obj: Option<i64>,
    pub diff: Json,
}

/// One batch, applied atomically. An empty change set is never applied.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeSet {
    pub ts: i64,
    pub batch: BatchRow,
    pub new_actor: Option<Actor>,
    /// In dependency order (a row only references rows inserted before it or already stored).
    pub ops: Vec<RowOp>,
    pub log: Vec<LogEntry>,
}

/// Receives what the engine keeps in memory as the store reads it, so loading never holds a full
/// copy of the rows next to the indexes being built.
pub trait LoadSink {
    fn actor(&mut self, a: Actor);
    /// A node, link, evidence (without `content`) or citation row.
    fn row(&mut self, row: Row);
    fn symmetric_link_type(&mut self, name: String);
}

impl LoadSink for Snapshot {
    fn actor(&mut self, a: Actor) {
        self.actors.push(a);
    }

    fn row(&mut self, row: Row) {
        match row {
            Row::Node(n) => self.nodes.push(n),
            Row::Link(l) => self.links.push(l),
            Row::Evidence(e) => self.evidence.push(e),
            Row::Citation(c) => self.citations.push(c),
        }
    }

    fn symmetric_link_type(&mut self, name: String) {
        self.symmetric_link_types.push(name);
    }
}

pub trait Store {
    /// Streams every actor, row and symmetric link type to `sink`.
    fn load_into(&self, sink: &mut dyn LoadSink) -> Result<(), StoreFailure>;
    /// Everything [`Store::load_into`] reads, collected.
    fn load(&self) -> Result<Snapshot, StoreFailure> {
        let mut s = Snapshot::default();
        self.load_into(&mut s)?;
        Ok(s)
    }
    fn counters(&self) -> Result<Counters, StoreFailure>;
    fn apply(&mut self, changes: &ChangeSet) -> Result<(), StoreFailure>;
    /// Seq of the most recent `retract` entry of an object ([STORE-LOG-4]).
    fn last_retraction(&self, obj_kind: i64, obj: i64) -> Result<Option<i64>, StoreFailure>;
    /// Objects whose `retract` entry has `cascade_of = seq`.
    fn cascaded_from(&self, seq: i64) -> Result<Vec<(i64, i64)>, StoreFailure>;
    /// Objects a batch created, or retracted directly (not by cascade).
    fn batch_objects(&self, batch: i64, op: &str) -> Result<Vec<(i64, i64)>, StoreFailure>;
    /// Reads evidence content, which is not kept in memory.
    fn evidence_content(&self, idx: i64) -> Result<Option<String>, StoreFailure>;
    /// The blob store this graph's evidence refers to ([STORE-BLOB-1]).
    fn blobs(&self) -> &dyn BlobStore;
}

/// Content-addressed bytes shared by all graphs ([STORE-BLOB-1] .. [STORE-BLOB-5]). Hashes are
/// 64 lowercase hexadecimal digits of SHA-256.
pub trait BlobStore {
    /// Stores bytes durably (written and synced before returning) and returns their hash.
    fn put(&self, bytes: &[u8]) -> Result<String, StoreFailure>;
    /// Size in bytes of a stored blob, or `None` when it is not in the store.
    fn size(&self, sha256: &str) -> Result<Option<u64>, StoreFailure>;
    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, StoreFailure>;
}

/// SHA-256 as 64 lowercase hexadecimal digits.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
