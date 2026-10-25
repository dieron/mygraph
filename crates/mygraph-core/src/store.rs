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
    pub new_actors: Vec<Actor>,
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
    /// Change-log entries ([OP-HIST-2]): of one object (obj_kind code, number) newest first,
    /// before `before` when given; or, without an object, those after `since` oldest first.
    /// Returns at most `limit` + 1 rows, so callers can tell whether more exist.
    fn history(
        &self,
        object: Option<(i64, i64)>,
        since: Option<i64>,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<LogRecord>, StoreFailure>;
    /// Objects (obj_kind code, number) with change-log entries after `seq`.
    fn changed_since(&self, seq: i64) -> Result<Vec<(i64, i64)>, StoreFailure>;
    /// Full-text search ([RANK-6]): objects of any status in relevance order, at most `limit`.
    /// `tokens` all match; the last one also as a word prefix.
    fn text_search(
        &self,
        target: TextTarget,
        tokens: &[String],
        limit: usize,
    ) -> Result<Vec<TextHit>, StoreFailure>;
    /// Copies the write-ahead log into the database and empties it, waiting for readers;
    /// bulk operations call it between batches so the log stays small.
    fn checkpoint(&self) -> Result<(), StoreFailure>;
    /// The most recent batches, newest first.
    fn recent_batches(&self, limit: usize) -> Result<Vec<BatchSummary>, StoreFailure>;
    /// Every vocabulary note.
    fn vocabulary_notes(&self) -> Result<Vec<VocabularyNote>, StoreFailure>;
}

/// A full-text index kept outside the store (docs/05 §2, `TextIndex` seam): a rebuildable cache
/// derived from the records ([STORE-TEXT-1]). It remembers the change-log sequence number up to
/// which it durably holds every change, so it can catch up after a restart or a crash.
pub trait TextIndex: Send + Sync {
    /// Sequence number of the last change the index durably holds; `None` for a new, empty index.
    fn applied_seq(&self) -> Option<i64>;
    /// Adds or replaces documents (one per object), covering changes up to `seq`. They are
    /// searchable at once and durable after the next [`TextIndex::commit`].
    fn index(&self, docs: Vec<TextDoc>, seq: i64) -> Result<(), StoreFailure>;
    /// Makes everything indexed so far durable and records its `seq`. Committing is slow, so
    /// callers batch it (the server commits in the background).
    fn commit(&self) -> Result<(), StoreFailure>;
    /// Number of documents indexed since the last commit.
    fn pending(&self) -> usize;
    /// Removes every document (durably).
    fn clear(&self) -> Result<(), StoreFailure>;
    /// Same contract as [`Store::text_search`].
    fn search(
        &self,
        target: TextTarget,
        tokens: &[String],
        limit: usize,
    ) -> Result<Vec<TextHit>, StoreFailure>;
}

/// The searchable text of one object, as in [STORE-FTSYNC-1].
#[derive(Debug, Clone, PartialEq)]
pub struct TextDoc {
    pub target: TextTarget,
    pub idx: i64,
    pub text: String,
}

/// Text of a node, link or evidence for a [`TextIndex`]; `content` is the evidence's inline text,
/// which the in-memory rows do not keep.
pub fn text_doc(row: &Row, content: Option<&str>) -> Option<TextDoc> {
    fn props(p: &crate::model::PropMap, out: &mut String) {
        for v in p.values() {
            match v {
                crate::values::Value::String(s) => {
                    out.push_str(s);
                    out.push('\n');
                }
                crate::values::Value::List(items) => {
                    for i in items {
                        if let crate::values::Value::String(s) = i {
                            out.push_str(s);
                            out.push('\n');
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut text = String::new();
    let (target, idx) = match row {
        Row::Node(n) => {
            text.push_str(&n.name);
            for a in &n.aliases {
                text.push('\n');
                text.push_str(a);
            }
            text.push('\n');
            if let Some(s) = &n.summary {
                text.push_str(s);
                text.push('\n');
            }
            props(&n.props, &mut text);
            (TextTarget::Nodes, n.idx)
        }
        Row::Link(l) => {
            text.push_str(&l.type_.replace('_', " "));
            text.push('\n');
            props(&l.props, &mut text);
            (TextTarget::Links, l.idx)
        }
        Row::Evidence(e) => {
            if let Some(t) = &e.title {
                text.push_str(t);
                text.push('\n');
            }
            if let Some(c) = content {
                text.push_str(c);
            }
            (TextTarget::Evidence, e.idx)
        }
        Row::Citation(_) => return None,
    };
    Some(TextDoc { target, idx, text })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextTarget {
    Nodes,
    Links,
    Evidence,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextHit {
    pub idx: i64,
    /// Evidence only ([OP-SEARCH-4]).
    pub snippet: Option<String>,
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

/// A stored change-log row ([STORE-LOG-2]).
#[derive(Debug, Clone, PartialEq)]
pub struct LogRecord {
    pub seq: i64,
    pub ts: i64,
    pub actor: i64,
    pub batch: i64,
    pub op: String,
    pub obj_kind: i64,
    pub obj: Option<i64>,
    pub diff: Option<String>,
}

/// A batch with the number of change-log entries it wrote ([OP-OVER-4]).
#[derive(Debug, Clone, PartialEq)]
pub struct BatchSummary {
    pub idx: i64,
    pub actor: i64,
    pub op: String,
    pub created_at: i64,
    pub note: Option<String>,
    pub changes: i64,
}

/// A vocabulary note ([STORE] `type_info`): kind 0 node type, 1 link type, 2 property key.
#[derive(Debug, Clone, PartialEq)]
pub struct VocabularyNote {
    pub kind: i64,
    pub name: String,
    pub description: Option<String>,
}
