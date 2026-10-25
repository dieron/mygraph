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

pub trait Store {
    fn load(&self) -> Result<Snapshot, StoreFailure>;
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
}
