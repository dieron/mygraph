//! The engine of one graph: batches, versioning, change log, rollback, evidence policy.
//! Operations live in `write.rs` (write) and `lifecycle.rs` (retract, restore, merge).

use crate::Reason;
use crate::ids::ObjectKind;
use crate::model::{
    Actor, Policies, RequireEvidence, Row, Status, TargetKind, id_string, obj_kind_code,
    record_json,
};
use crate::state::State;
use crate::store::{
    BatchRow, ChangeSet, Counters, DerivedIndex, LogEntry, RowOp, SemanticIndex, Store,
    StoreFailure, TextDoc, TextIndex, text_doc,
};
use serde_json::{Map, Value as Json, json};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// An operation error: reason code, message, failing item, details ([OP-WRITE-6]).
#[derive(Debug, Clone, PartialEq)]
pub struct OpError {
    pub reason: Reason,
    pub message: String,
    pub section: Option<String>,
    pub index: Option<usize>,
    pub details: Json,
}

impl OpError {
    pub fn new(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason: Reason(reason),
            message: message.into(),
            section: None,
            index: None,
            details: Json::Null,
        }
    }

    pub fn details(mut self, details: Json) -> Self {
        self.details = details;
        self
    }

    pub(crate) fn at(mut self, section: &str, index: usize) -> Self {
        if self.section.is_none() {
            self.section = Some(section.to_owned());
            self.index = Some(index);
        }
        self
    }

    pub fn to_json(&self) -> Json {
        json!({
            "error": {
                "code": self.reason.code(),
                "message": self.message,
                "section": self.section,
                "index": self.index,
                "details": self.details,
            }
        })
    }
}

impl fmt::Display for OpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason, self.message)
    }
}

impl std::error::Error for OpError {}

impl From<Reason> for OpError {
    fn from(r: Reason) -> Self {
        Self::new(r.0, r.0)
    }
}

pub(crate) fn storage_error(e: StoreFailure) -> OpError {
    OpError::new("storage.failure", e.to_string())
}

/// A status change recorded for the change log.
#[derive(Debug, Clone)]
pub(crate) struct StatusOp {
    pub op: &'static str,
    pub reason: Option<String>,
    pub cause: Option<(ObjectKind, i64)>,
    pub extra: Map<String, Json>,
}

#[derive(Debug)]
struct Touch {
    original: Option<Row>,
    status_op: Option<StatusOp>,
}

/// Work in progress of one batch. All changes are already applied to the in-memory state;
/// `undo` restores it.
pub(crate) struct Batch {
    pub ts: i64,
    pub actor: i64,
    new_actor: Option<Actor>,
    op: &'static str,
    note: Option<String>,
    order: Vec<(ObjectKind, i64)>,
    touched: HashMap<(ObjectKind, i64), Touch>,
    undo: Vec<(ObjectKind, i64, Option<Row>)>,
    pub counters: Counters,
    pub warnings: Vec<Json>,
    /// Facts created or restored by the batch ([OBJ-POLICY-1]).
    pub new_facts: Vec<(TargetKind, i64)>,
    /// Facts that may have lost support ([OBJ-POLICY-3]): (fact, supported before).
    pub support_watch: Vec<((TargetKind, i64), bool)>,
    /// Content of evidence created in this batch (the in-memory state does not keep content).
    pub pending_content: HashMap<i64, String>,
}

impl Batch {
    pub fn next(&mut self, kind: ObjectKind) -> i64 {
        let c = &mut self.counters;
        let slot = match kind {
            ObjectKind::Node => &mut c.node,
            ObjectKind::Link => &mut c.link,
            ObjectKind::Evidence => &mut c.evidence,
            ObjectKind::Citation => &mut c.citation,
            ObjectKind::Batch => &mut c.batch,
        };
        *slot += 1;
        *slot
    }

    pub fn warn(&mut self, code: &str, message: impl Into<String>, details: Json) {
        self.warnings
            .push(json!({ "code": code, "message": message.into(), "details": details }));
    }
}

/// One open graph.
pub struct Graph<S: Store> {
    pub(crate) store: S,
    pub(crate) state: State,
    pub(crate) policies: Policies,
    pub(crate) counters: Counters,
    name: String,
    /// External full-text index; `search` falls back to the store's own full text when it is
    /// absent, stale or failing to commit.
    text: Option<Arc<dyn TextIndex>>,
    /// The index missed updates (or a repair failed): it no longer takes updates, and only
    /// [`Graph::repair_text_index`] (or reopening) brings it back.
    text_stale: AtomicBool,
    /// Commits (and repairs) that failed since the last one that succeeded. While there are
    /// any, the index still takes updates and later commits retry them.
    text_failures: AtomicU32,
    /// Semantic index ([RANK-8]); without it (or while it is stale or building), search matches
    /// words only.
    semantic: Option<Arc<dyn SemanticIndex>>,
    semantic_stale: AtomicBool,
    pub(crate) stats_cache: crate::stats::StatsCache,
}

impl<S: Store> Graph<S> {
    /// Loads the in-memory indexes from the store ([docs/05 §7]).
    pub fn open(store: S, name: &str, policies: Policies) -> Result<Self, StoreFailure> {
        let counters = store.counters()?;
        Ok(Self {
            state: State::load(&store, &counters)?,
            store,
            policies,
            counters,
            name: name.to_owned(),
            text: None,
            text_stale: AtomicBool::new(false),
            text_failures: AtomicU32::new(0),
            semantic: None,
            semantic_stale: AtomicBool::new(false),
            stats_cache: Default::default(),
        })
    }

    /// Attaches an external full-text index, first bringing it up to date: catching up from the
    /// change log, or rebuilding it when it is new or ahead of the store.
    pub fn attach_text_index(&mut self, index: Box<dyn TextIndex>) -> Result<(), StoreFailure> {
        let index: Arc<dyn TextIndex> = Arc::from(index);
        self.sync_index(index.as_ref(), false)?;
        self.text = Some(index);
        self.text_stale.store(false, Ordering::Relaxed);
        self.text_failures.store(0, Ordering::Relaxed);
        Ok(())
    }

    /// Attaches a semantic index, bringing it up to date like [`Graph::attach_text_index`]. Its
    /// documents are embedded by its own commits, so after a rebuild it may stay unready
    /// ([`SemanticIndex::ready`]) until background commits have embedded them.
    pub fn attach_semantic_index(
        &mut self,
        index: Box<dyn SemanticIndex>,
    ) -> Result<(), StoreFailure> {
        let index: Arc<dyn SemanticIndex> = Arc::from(index);
        self.sync_index(index.as_ref(), false)?;
        self.semantic = Some(index);
        self.semantic_stale.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Brings a derived index up to date: catching up from the change log, or rebuilding it when
    /// it is new or ahead of the store, or when `rebuild` asks.
    fn sync_index(&self, index: &dyn DerivedIndex, rebuild: bool) -> Result<(), StoreFailure> {
        let seq = self.counters.seq;
        match index.applied_seq().filter(|_| !rebuild) {
            Some(s) if s == seq => {}
            Some(s) if s >= 0 && s < seq => {
                let docs = self
                    .store
                    .changed_since(s)?
                    .into_iter()
                    .filter_map(|(kind, idx)| match kind {
                        0 => Some((ObjectKind::Node, idx)),
                        1 => Some((ObjectKind::Link, idx)),
                        2 => Some((ObjectKind::Evidence, idx)),
                        _ => None,
                    })
                    .map(|(kind, idx)| self.text_doc_of(kind, idx))
                    .collect::<Result<Vec<_>, _>>()?;
                index.index(docs.into_iter().flatten().collect(), seq)?;
                index.commit()?;
            }
            _ => {
                // Rebuild in chunks; only the last chunk records `seq`, so an interrupted
                // rebuild (recorded as -1) is redone on the next attach.
                index.clear()?;
                let mut all: Vec<(ObjectKind, i64)> = Vec::new();
                all.extend(self.state.nodes.iter().map(|(i, _)| (ObjectKind::Node, i)));
                all.extend(self.state.links.iter().map(|(i, _)| (ObjectKind::Link, i)));
                all.extend(
                    self.state
                        .evidence
                        .iter()
                        .map(|(i, _)| (ObjectKind::Evidence, i)),
                );
                let chunks: Vec<_> = all.chunks(100_000).collect();
                for (n, chunk) in chunks.iter().enumerate() {
                    let docs = chunk
                        .iter()
                        .map(|&(kind, idx)| self.text_doc_of(kind, idx))
                        .collect::<Result<Vec<_>, _>>()?;
                    let mark = if n + 1 == chunks.len() { seq } else { -1 };
                    index.index(docs.into_iter().flatten().collect(), mark)?;
                    index.commit()?;
                }
                if chunks.is_empty() {
                    index.index(Vec::new(), seq)?;
                    index.commit()?;
                }
            }
        }
        Ok(())
    }

    fn text_doc_of(&self, kind: ObjectKind, idx: i64) -> Result<Option<TextDoc>, StoreFailure> {
        let Some(row) = self.state.get(kind, idx) else {
            return Ok(None);
        };
        let content = match kind {
            ObjectKind::Evidence => self.store.evidence_content(idx)?,
            _ => None,
        };
        Ok(text_doc(&row, content.as_deref()))
    }

    /// Makes recent full-text index updates durable, when there are any. After a failure,
    /// search falls back to the store's full text until a later commit succeeds.
    pub fn commit_text(&self) -> Result<(), StoreFailure> {
        let Some(t) = self.text_handle() else {
            return Ok(());
        };
        if t.pending() == 0 && self.text_commit_failures() == 0 {
            return Ok(());
        }
        let result = t.commit();
        self.record_text_commit(result.is_ok());
        result
    }

    /// Records the outcome of a commit through [`Graph::text_handle`]: after a failure, search
    /// falls back to the store's full text until a commit succeeds. Returns the number of
    /// failures in a row.
    pub fn record_text_commit(&self, ok: bool) -> u32 {
        if ok {
            self.text_failures.store(0, Ordering::Relaxed);
            0
        } else {
            self.text_failures
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1)
        }
    }

    /// Full-text commits and repairs that failed in a row (0 while the index is current).
    pub fn text_commit_failures(&self) -> u32 {
        self.text_failures.load(Ordering::Relaxed)
    }

    /// Whether the full-text index missed updates, so that only [`Graph::repair_text_index`]
    /// brings it back.
    pub fn text_index_lost_updates(&self) -> bool {
        self.text.is_some() && self.text_stale.load(Ordering::Relaxed)
    }

    /// Brings the full-text index back to `current`: catches it up from the change log (with
    /// `rebuild`, rebuilds it from the records) and commits. Search uses the store's full text
    /// meanwhile, and still after a failure, which counts as a failed commit. Holding the graph
    /// shared keeps writes out while it runs.
    pub fn repair_text_index(&self, rebuild: bool) -> Result<(), StoreFailure> {
        let Some(t) = self.text.clone() else {
            return Ok(());
        };
        self.text_stale.store(true, Ordering::Relaxed);
        let result = self
            .sync_index(t.as_ref(), rebuild)
            .and_then(|()| t.commit());
        if result.is_ok() {
            self.text_stale.store(false, Ordering::Relaxed);
        }
        self.record_text_commit(result.is_ok());
        result
    }

    /// Embeds and saves recent semantic index updates until none are left (each commit embeds
    /// for a few seconds). A failure marks the index stale (search matches words only until the
    /// graph is reopened).
    pub fn commit_semantic(&self) -> Result<(), StoreFailure> {
        if let Some(t) = self.semantic_handle() {
            while t.pending() > 0 {
                if let Err(e) = t.commit() {
                    self.semantic_stale.store(true, Ordering::Relaxed);
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// A handle on the semantic index (when attached and not stale), to commit it without
    /// holding the graph.
    pub fn semantic_handle(&self) -> Option<Arc<dyn SemanticIndex>> {
        self.semantic
            .clone()
            .filter(|_| !self.semantic_stale.load(Ordering::Relaxed))
    }

    /// After a failed commit through [`Graph::semantic_handle`]: search matches words only until
    /// the graph is reopened.
    pub fn mark_semantic_stale(&self) {
        self.semantic_stale.store(true, Ordering::Relaxed);
    }

    /// The semantic index, when attached and not stale.
    pub(crate) fn semantic_index(&self) -> Option<&dyn SemanticIndex> {
        self.semantic
            .as_deref()
            .filter(|_| !self.semantic_stale.load(Ordering::Relaxed))
    }

    /// State of the semantic index: `current`, `building` (embedding many documents, as after a
    /// rebuild; search matches words only meanwhile), `stale` (an update failed) or `none`.
    pub fn semantic_index_status(&self) -> &'static str {
        match (&self.semantic, self.semantic_stale.load(Ordering::Relaxed)) {
            (None, _) => "none",
            (Some(_), true) => "stale",
            (Some(s), false) if !s.ready() => "building",
            (Some(_), false) => "current",
        }
    }

    /// [OP-OVER-6] How `search` and `resolve` match in this graph: whether by meaning (a
    /// semantic index is attached and not stale), the least similarity of a hit found by meaning,
    /// and the index's state.
    pub fn search_settings(&self) -> serde_json::Value {
        match self.semantic_index() {
            Some(s) => serde_json::json!({
                "semantic": true,
                "semantic_threshold": (f64::from(s.threshold()) * 1000.0).round() / 1000.0,
                "semantic_index": self.semantic_index_status(),
            }),
            None => serde_json::json!({
                "semantic": false, "semantic_index": self.semantic_index_status(),
            }),
        }
    }

    /// A handle on the full-text index (when attached and holding every update, even while its
    /// commits fail), so it can be committed without holding the graph: writes then never wait
    /// for a commit. Report the outcome with [`Graph::record_text_commit`].
    pub fn text_handle(&self) -> Option<Arc<dyn TextIndex>> {
        self.text
            .clone()
            .filter(|_| !self.text_stale.load(Ordering::Relaxed))
    }

    /// The external full-text index, when attached and current (search uses it).
    pub(crate) fn text_index(&self) -> Option<&dyn TextIndex> {
        self.text.as_deref().filter(|_| {
            !self.text_stale.load(Ordering::Relaxed)
                && self.text_failures.load(Ordering::Relaxed) == 0
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn policies(&self) -> &Policies {
        &self.policies
    }

    /// The latest change-log sequence number (0 for an empty graph).
    pub fn last_seq(&self) -> i64 {
        self.counters.seq
    }

    /// State of the external full-text index: `current`, `stale` (an update or commit failed;
    /// search uses the store's own full text) or `none`.
    pub fn text_index_status(&self) -> &'static str {
        match &self.text {
            None => "none",
            Some(_) if self.text_index().is_none() => "stale",
            Some(_) => "current",
        }
    }

    pub(crate) fn begin(
        &mut self,
        actor: &str,
        op: &'static str,
        ts: i64,
    ) -> Result<Batch, OpError> {
        crate::ids::validate_actor_name(actor)?;
        let mut counters = self.counters;
        let (actor_idx, new_actor) = match self.state.actor_by_name.get(actor) {
            Some(&i) => (i, None),
            None => {
                counters.actor += 1;
                let a = Actor {
                    idx: counters.actor,
                    name: actor.to_owned(),
                    kind: "agent".into(),
                };
                self.state.put_actor(a.clone());
                (a.idx, Some(a))
            }
        };
        Ok(Batch {
            ts,
            actor: actor_idx,
            new_actor,
            op,
            note: None,
            order: Vec::new(),
            touched: HashMap::new(),
            undo: Vec::new(),
            counters,
            warnings: Vec::new(),
            new_facts: Vec::new(),
            support_watch: Vec::new(),
            pending_content: HashMap::new(),
        })
    }

    /// Applies a changed or new row to the state, remembering how to undo it.
    pub(crate) fn put(&mut self, b: &mut Batch, row: Row) {
        let key = (row.kind(), row.idx());
        let old = self.state.put(row);
        b.undo.push((key.0, key.1, old.clone()));
        b.touched.entry(key).or_insert_with(|| {
            b.order.push(key);
            Touch {
                original: old,
                status_op: None,
            }
        });
    }

    /// Changes an object's status (the caller has set any other fields on `row`).
    pub(crate) fn put_status(&mut self, b: &mut Batch, mut row: Row, status: Status, op: StatusOp) {
        match &mut row {
            Row::Node(n) => n.status = status,
            Row::Link(l) => l.status = status,
            Row::Evidence(e) => e.status = status,
            Row::Citation(c) => c.status = status,
        }
        let key = (row.kind(), row.idx());
        self.put(b, row);
        if let Some(t) = b.touched.get_mut(&key) {
            t.status_op = Some(op);
        }
    }

    pub(crate) fn rollback(&mut self, b: Batch) {
        for (kind, idx, old) in b.undo.into_iter().rev() {
            match old {
                Some(row) => {
                    self.state.put(row);
                }
                None => {
                    self.state.remove(kind, idx);
                }
            }
        }
        if let Some(a) = b.new_actor {
            self.state.remove_actor(a.idx);
        }
    }

    /// [OBJ-POLICY-1] .. [OBJ-POLICY-3]
    fn check_policy(&self, b: &mut Batch) -> Result<(), OpError> {
        let mut unsupported: Vec<String> = Vec::new();
        for &(kind, idx) in &b.new_facts {
            let active = match kind {
                TargetKind::Node => self
                    .state
                    .nodes
                    .get(&idx)
                    .is_some_and(|n| n.status == Status::Active),
                TargetKind::Link => self
                    .state
                    .links
                    .get(&idx)
                    .is_some_and(|l| l.status == Status::Active),
            };
            let id = id_string(kind.object_kind(), idx);
            if active && !self.state.is_supported(kind, idx) && !unsupported.contains(&id) {
                unsupported.push(id);
            }
        }
        match self.policies.require_evidence {
            RequireEvidence::Enforce if !unsupported.is_empty() => {
                return Err(OpError::new(
                    "write.evidence_required",
                    format!(
                        "{} fact(s) have no supporting evidence; cite an evidence (kind \"observation\" for your own conclusions)",
                        unsupported.len()
                    ),
                )
                .details(json!({ "unsupported": unsupported })));
            }
            RequireEvidence::Warn => {
                for id in &unsupported {
                    b.warn(
                        "UNSUPPORTED_FACT",
                        format!("{id} has no supporting evidence"),
                        json!({ "id": id }),
                    );
                }
            }
            _ => {}
        }
        let watch = std::mem::take(&mut b.support_watch);
        for ((kind, idx), was_supported) in watch {
            let active = match kind {
                TargetKind::Node => self
                    .state
                    .nodes
                    .get(&idx)
                    .is_some_and(|n| n.status == Status::Active),
                TargetKind::Link => self
                    .state
                    .links
                    .get(&idx)
                    .is_some_and(|l| l.status == Status::Active),
            };
            if was_supported && active && !self.state.is_supported(kind, idx) {
                let id = id_string(kind.object_kind(), idx);
                b.warn(
                    "UNSUPPORTED_FACT",
                    format!("{id} lost its supporting evidence"),
                    json!({ "id": id }),
                );
            }
        }
        Ok(())
    }

    /// Ends a batch: policy, versions, change log, store commit. Returns the batch ID, or `None`
    /// when nothing changed or on a dry run.
    pub(crate) fn finish(
        &mut self,
        mut b: Batch,
        dry_run: bool,
    ) -> Result<(Option<String>, Vec<Json>), OpError> {
        if let Err(e) = self.check_policy(&mut b) {
            self.rollback(b);
            return Err(e);
        }
        let mut ops = Vec::new();
        let mut log: Vec<(LogEntry, Option<(ObjectKind, i64)>)> = Vec::new();
        let mut status_seq: HashMap<(ObjectKind, i64), usize> = HashMap::new();
        let actor_name = |st: &State, i: i64| st.actor_name(i);
        let order = b.order.clone();
        for key in order {
            let Some(mut row) = self.state.get(key.0, key.1) else {
                continue;
            };
            let touch = &b.touched[&key];
            let obj_kind = obj_kind_code(key.0);
            match &touch.original {
                None => {
                    *row.meta_mut() = crate::model::RecordMeta {
                        version: 1,
                        created_at: b.ts,
                        created_by: b.actor,
                        updated_at: b.ts,
                        updated_by: b.actor,
                    };
                    self.state.put(row.clone());
                    let after = record_json(&row, &|i| actor_name(&self.state, i));
                    log.push((
                        LogEntry {
                            seq: 0,
                            op: "create",
                            obj_kind,
                            obj: Some(key.1),
                            diff: json!({ "after": after }),
                        },
                        None,
                    ));
                    if let Row::Evidence(e) = &mut row {
                        e.content = b.pending_content.get(&e.idx).cloned();
                    }
                    ops.push(RowOp::Insert(row));
                }
                Some(original) => {
                    let mut same = row.clone();
                    *same.meta_mut() = original.meta();
                    if same == *original && touch.status_op.is_none() {
                        continue;
                    }
                    let meta = row.meta_mut();
                    *meta = original.meta();
                    meta.version += 1;
                    meta.updated_at = b.ts;
                    meta.updated_by = b.actor;
                    self.state.put(row.clone());
                    let names = |i| actor_name(&self.state, i);
                    let before_map = record_json(original, &names);
                    let after_map = record_json(&row, &names);
                    let skip = [
                        "version",
                        "updated_at",
                        "updated_by",
                        "status",
                        "merged_into",
                    ];
                    let (mut before, mut after) = (Map::new(), Map::new());
                    for (k, v) in &after_map {
                        if skip.contains(&k.as_str()) {
                            continue;
                        }
                        let old = before_map.get(k).cloned().unwrap_or(Json::Null);
                        if old != *v {
                            before.insert(k.clone(), old);
                            after.insert(k.clone(), v.clone());
                        }
                    }
                    if !after.is_empty() {
                        log.push((
                            LogEntry {
                                seq: 0,
                                op: "update",
                                obj_kind,
                                obj: Some(key.1),
                                diff: json!({ "before": before, "after": after }),
                            },
                            None,
                        ));
                    }
                    if let Some(s) = &touch.status_op {
                        let mut diff = s.extra.clone();
                        if s.op != "merge" {
                            diff.insert(
                                "reason".into(),
                                s.reason.clone().map_or(Json::Null, Json::String),
                            );
                            diff.insert("cascade_of".into(), Json::Null);
                        }
                        status_seq.insert(key, log.len());
                        log.push((
                            LogEntry {
                                seq: 0,
                                op: s.op,
                                obj_kind,
                                obj: Some(key.1),
                                diff: Json::Object(diff),
                            },
                            s.cause,
                        ));
                    }
                    ops.push(RowOp::Update(row));
                }
            }
        }
        if ops.is_empty() || dry_run {
            let warnings = std::mem::take(&mut b.warnings);
            self.rollback(b);
            return Ok((None, warnings));
        }
        let first_seq = b.counters.seq + 1;
        for (i, (entry, _)) in log.iter_mut().enumerate() {
            entry.seq = first_seq + i as i64;
        }
        let causes: Vec<Option<i64>> = log
            .iter()
            .map(|(_, cause)| {
                cause
                    .and_then(|c| status_seq.get(&c))
                    .map(|&i| first_seq + i as i64)
            })
            .collect();
        for ((entry, _), cause) in log.iter_mut().zip(causes) {
            if let (Some(seq), Some(obj)) = (cause, entry.diff.as_object_mut())
                && obj.contains_key("cascade_of")
            {
                obj.insert("cascade_of".into(), seq.into());
            }
        }
        b.counters.seq += log.len() as i64;
        let batch_idx = b.next(ObjectKind::Batch);
        let changes = ChangeSet {
            ts: b.ts,
            batch: BatchRow {
                idx: batch_idx,
                actor: b.actor,
                op: b.op,
                created_at: b.ts,
                note: b.note.clone(),
            },
            new_actors: b.new_actor.clone().into_iter().collect(),
            ops,
            log: log.into_iter().map(|(e, _)| e).collect(),
        };
        if let Err(e) = self.store.apply(&changes) {
            self.rollback(b);
            return Err(storage_error(e));
        }
        self.counters = b.counters;
        self.update_text(&changes);
        Ok((Some(id_string(ObjectKind::Batch, batch_idx)), b.warnings))
    }

    pub(crate) fn store_query<T>(&self, r: Result<T, StoreFailure>) -> Result<T, OpError> {
        r.map_err(storage_error)
    }
}

impl<S: Store> Graph<S> {
    /// After a commit: re-index every node, link and evidence the batch wrote.
    pub(crate) fn update_text(&mut self, changes: &ChangeSet) {
        let text = self.text.is_some() && !self.text_stale.load(Ordering::Relaxed);
        let semantic = self.semantic.is_some() && !self.semantic_stale.load(Ordering::Relaxed);
        if !text && !semantic {
            return;
        }
        let seq = changes.log.last().map_or(self.counters.seq, |e| e.seq);
        let mut docs = Vec::new();
        for op in &changes.ops {
            let (RowOp::Insert(row) | RowOp::Update(row)) = op;
            let content = match row {
                Row::Evidence(e) if e.content.is_some() => e.content.clone(),
                Row::Evidence(e) => self.store.evidence_content(e.idx).ok().flatten(),
                _ => None,
            };
            docs.extend(text_doc(row, content.as_deref()));
        }
        if semantic {
            let ok = self
                .semantic
                .as_ref()
                .is_some_and(|t| t.index(docs.clone(), seq).is_ok());
            if !ok {
                self.semantic_stale.store(true, Ordering::Relaxed);
            }
        }
        if text {
            let ok = self
                .text
                .as_ref()
                .is_some_and(|t| t.index(docs, seq).is_ok());
            if !ok {
                self.text_stale.store(true, Ordering::Relaxed);
            }
        }
    }
}
