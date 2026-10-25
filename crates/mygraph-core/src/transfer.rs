//! Export and import of a whole graph as JSONL — spec/api/export-format.md.

use crate::engine::{Graph, OpError, storage_error};
use crate::ids::{ObjectId, ObjectKind};
use crate::model::{
    Actor, Citation, Evidence, Link, Node, RecordMeta, Role, Row, Status, TargetKind,
    obj_kind_code, record_json, time_json,
};
use crate::store::{BatchRow, ChangeSet, LogEntry, RowOp, Store, StoreFailure, sha256_hex};
use crate::values::{self, DateTime, Parsed, PropMap, Value};
use serde_json::{Map, Value as Json, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};

/// [EXP-1] format version.
const VERSION: i64 = 1;
/// Objects per import batch.
const CHUNK: usize = 10_000;
/// Batches between checkpoints during an import.
const CHECKPOINT_EVERY: usize = 20;
const ACTOR_KINDS: [&str; 5] = ["agent", "user", "admin", "system", "import"];

impl<S: Store> Graph<S> {
    /// Writes the graph as JSONL ([EXP-1], [EXP-2]); returns the header's counts.
    pub fn export(&self, out: &mut dyn Write, now_ms: i64) -> Result<Json, StoreFailure> {
        let st = &self.state;
        let counts = json!({
            "actors": st.actors.len(), "nodes": st.nodes.len(), "links": st.links.len(),
            "evidence": st.evidence.len(), "citations": st.citations.len(),
        });
        let header = json!({
            "mygraph_export": VERSION, "graph": self.name(), "spec_version": crate::SPEC_VERSION,
            "exported_at": time_json(now_ms), "last_seq": self.counters.seq,
            "policies": self.policies.to_json(), "counts": counts,
        });
        let mut line = |v: &Json| -> Result<(), StoreFailure> {
            serde_json::to_writer(&mut *out, v)?;
            out.write_all(b"\n")?;
            Ok(())
        };
        line(&header)?;
        let mut actors: Vec<&Actor> = st.actors.values().collect();
        actors.sort_by_key(|a| a.idx);
        for a in actors {
            line(&json!({ "actor": { "name": a.name, "kind": a.kind } }))?;
        }
        let names = |i: i64| st.actor_name(i);
        for (_, n) in st.nodes.iter() {
            line(&Json::Object(record_json(&Row::Node(n.clone()), &names)))?;
        }
        for (_, l) in st.links.iter() {
            line(&Json::Object(record_json(&Row::Link(l.clone()), &names)))?;
        }
        for (idx, e) in st.evidence.iter() {
            let mut rec = record_json(&Row::Evidence(e.clone()), &names);
            let content = self.store.evidence_content(idx)?;
            rec.insert("content".into(), content.map_or(Json::Null, Json::String));
            line(&Json::Object(rec))?;
        }
        for (_, c) in st.citations.iter() {
            line(&Json::Object(record_json(
                &Row::Citation(c.clone()),
                &names,
            )))?;
        }
        out.flush()?;
        Ok(counts)
    }

    /// Reads a JSONL export into this graph, which must be empty ([EXP-4], [EXP-5]). Returns
    /// the numbers of objects imported.
    pub fn import(
        &mut self,
        actor: &str,
        input: &mut dyn BufRead,
        now_ms: i64,
    ) -> Result<Json, OpError> {
        crate::ids::validate_actor_name(actor)?;
        let st = &self.state;
        if self.counters.seq != 0
            || !(st.nodes.is_empty() && st.links.is_empty() && st.evidence.is_empty())
        {
            return Err(OpError::new(
                "import.graph_not_empty",
                "import needs a graph without objects or history",
            ));
        }
        let mut lines = input.lines().enumerate();
        let header = match lines.next() {
            Some((_, Ok(l))) => serde_json::from_str::<Json>(&l).ok(),
            _ => None,
        };
        let version = header.as_ref().and_then(|h| h["mygraph_export"].as_i64());
        if version != Some(VERSION) {
            return Err(OpError::new(
                "import.unsupported_version",
                format!("expected a mygraph_export {VERSION} header line"),
            ));
        }
        let mut imp = Importer {
            actors: self.state.actor_by_name.clone(),
            new_actors: Vec::new(),
            next_actor: self.counters.actor,
            known: HashSet::new(),
            chunk: Vec::new(),
            deferred: Vec::new(),
            counts: HashMap::new(),
        };
        let me = imp.actor(actor, "admin");
        let mut flushed = 0usize;
        for (n, line) in lines {
            let line = line.map_err(|e| OpError::new("import.invalid_line", e.to_string()))?;
            if line.trim().is_empty() {
                continue;
            }
            let bad = |why: String| {
                let code = if why.starts_with("import.content_mismatch") {
                    "import.content_mismatch"
                } else {
                    "import.invalid_line"
                };
                OpError::new(code, format!("line {}: {why}", n + 1))
            };
            let v: Json = serde_json::from_str(&line).map_err(|e| bad(e.to_string()))?;
            if let Some(a) = v.get("actor") {
                let name = a["name"]
                    .as_str()
                    .ok_or_else(|| bad("actor without name".into()))?;
                crate::ids::validate_actor_name(name).map_err(|r| bad(r.to_string()))?;
                let kind = a["kind"]
                    .as_str()
                    .filter(|k| ACTOR_KINDS.contains(k))
                    .unwrap_or("agent");
                imp.actor(name, kind);
                continue;
            }
            let Json::Object(mut rec) = v else {
                return Err(bad("expected an object".into()));
            };
            let content = match rec.remove("content") {
                Some(Json::String(s)) => Some(s),
                _ => None,
            };
            let row = imp.row(&rec, content).map_err(bad)?;
            imp.offer(&self.state, row, rec);
            if imp.chunk.len() >= CHUNK {
                self.flush_import(&mut imp, me, now_ms)?;
                flushed += 1;
                if flushed % CHECKPOINT_EVERY == 0 {
                    self.store.checkpoint().map_err(storage_error)?;
                }
            }
        }
        imp.retry(&self.state);
        self.flush_import(&mut imp, me, now_ms)?;
        self.store.checkpoint().map_err(storage_error)?;
        if let Some((row, _)) = imp.deferred.first() {
            return Err(OpError::new(
                "import.dangling_reference",
                format!(
                    "{} (and {} more) refer to objects not in the file",
                    crate::model::id_string(row.kind(), row.idx()),
                    imp.deferred.len() - 1
                ),
            ));
        }
        let mut counts = Map::new();
        for k in ["nodes", "links", "evidence", "citations"] {
            counts.insert(k.into(), imp.counts.get(k).copied().unwrap_or(0).into());
        }
        Ok(Json::Object(counts))
    }

    /// Commits the rows gathered so far as one `import` batch.
    fn flush_import(&mut self, imp: &mut Importer, actor: i64, now_ms: i64) -> Result<(), OpError> {
        imp.retry(&self.state);
        if imp.chunk.is_empty() && imp.new_actors.is_empty() {
            return Ok(());
        }
        let mut counters = self.counters;
        counters.batch += 1;
        counters.actor = imp.next_actor;
        let mut log = Vec::with_capacity(imp.chunk.len());
        let mut ops = Vec::with_capacity(imp.chunk.len());
        for (row, rec) in imp.chunk.drain(..) {
            counters.seq += 1;
            let slot = match row.kind() {
                ObjectKind::Node => &mut counters.node,
                ObjectKind::Link => &mut counters.link,
                ObjectKind::Evidence => &mut counters.evidence,
                _ => &mut counters.citation,
            };
            *slot = (*slot).max(row.idx());
            log.push(LogEntry {
                seq: counters.seq,
                op: "create",
                obj_kind: obj_kind_code(row.kind()),
                obj: Some(row.idx()),
                diff: json!({ "after": rec }),
            });
            ops.push(RowOp::Insert(row));
        }
        let changes = ChangeSet {
            ts: now_ms,
            batch: BatchRow {
                idx: counters.batch,
                actor,
                op: "import",
                created_at: now_ms,
                note: None,
            },
            new_actors: std::mem::take(&mut imp.new_actors),
            ops,
            log,
        };
        self.store.apply(&changes).map_err(storage_error)?;
        for a in &changes.new_actors {
            self.state.put_actor(a.clone());
        }
        for op in &changes.ops {
            let (RowOp::Insert(row) | RowOp::Update(row)) = op;
            *imp.counts.entry(plural(row.kind())).or_default() += 1;
            self.state.put(row.clone());
        }
        imp.known.clear();
        self.counters = counters;
        self.update_text(&changes);
        Ok(())
    }
}

fn plural(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Node => "nodes",
        ObjectKind::Link => "links",
        ObjectKind::Evidence => "evidence",
        _ => "citations",
    }
}

struct Importer {
    actors: HashMap<String, i64>,
    new_actors: Vec<Actor>,
    next_actor: i64,
    /// Objects in the current chunk (not yet in the state).
    known: HashSet<(ObjectKind, i64)>,
    chunk: Vec<(Row, Map<String, Json>)>,
    /// Rows whose references are not imported yet.
    deferred: Vec<(Row, Map<String, Json>)>,
    counts: HashMap<&'static str, usize>,
}

impl Importer {
    fn actor(&mut self, name: &str, kind: &str) -> i64 {
        if let Some(&i) = self.actors.get(name) {
            return i;
        }
        self.next_actor += 1;
        let a = Actor {
            idx: self.next_actor,
            name: name.to_owned(),
            kind: kind.to_owned(),
        };
        self.actors.insert(a.name.clone(), a.idx);
        self.new_actors.push(a);
        self.next_actor
    }

    /// Objects the row refers to.
    fn refs(row: &Row) -> Vec<(ObjectKind, i64)> {
        match row {
            Row::Node(n) => n
                .merged_into
                .map(|m| (ObjectKind::Node, m))
                .into_iter()
                .collect(),
            Row::Link(l) => vec![(ObjectKind::Node, l.src), (ObjectKind::Node, l.dst)],
            Row::Evidence(e) => [e.parent, e.supersedes]
                .into_iter()
                .flatten()
                .map(|i| (ObjectKind::Evidence, i))
                .collect(),
            Row::Citation(c) => vec![
                (ObjectKind::Evidence, c.evidence),
                (c.target_kind.object_kind(), c.target),
            ],
        }
    }

    fn ready(&self, st: &crate::state::State, row: &Row) -> bool {
        Self::refs(row)
            .into_iter()
            .all(|(k, i)| self.known.contains(&(k, i)) || st.get(k, i).is_some())
    }

    fn offer(&mut self, st: &crate::state::State, row: Row, rec: Map<String, Json>) {
        if self.ready(st, &row) {
            self.known.insert((row.kind(), row.idx()));
            self.chunk.push((row, rec));
        } else {
            self.deferred.push((row, rec));
        }
    }

    /// Moves deferred rows whose references arrived into the chunk, until nothing moves.
    fn retry(&mut self, st: &crate::state::State) {
        loop {
            let before = self.deferred.len();
            let waiting = std::mem::take(&mut self.deferred);
            for (row, rec) in waiting {
                self.offer(st, row, rec);
            }
            if self.deferred.len() == before {
                break;
            }
        }
        if self.chunk.is_empty() {
            self.known.clear();
        }
    }

    /// A record line as a row ([EXP-2]).
    fn row(&mut self, r: &Map<String, Json>, content: Option<String>) -> Result<Row, String> {
        let id = r.get("id").and_then(Json::as_str).ok_or("missing id")?;
        let oid = ObjectId::decode(id).map_err(|e| format!("id {id}: {e}"))?;
        let idx = oid.number as i64;
        let meta = RecordMeta {
            version: r
                .get("version")
                .and_then(Json::as_i64)
                .ok_or("missing version")?,
            created_at: time(r, "created_at")?,
            created_by: self.actor(
                text(r, "created_by")?.ok_or("missing created_by")?,
                "import",
            ),
            updated_at: time(r, "updated_at")?,
            updated_by: self.actor(
                text(r, "updated_by")?.ok_or("missing updated_by")?,
                "import",
            ),
        };
        let status = text(r, "status")?
            .and_then(|s| {
                [Status::Active, Status::Retracted, Status::Merged]
                    .into_iter()
                    .find(|x| x.name() == s)
            })
            .ok_or("bad status")?;
        Ok(match oid.kind {
            ObjectKind::Node => Row::Node(Node {
                idx,
                key: owned(r, "key")?,
                type_: text(r, "type")?.ok_or("missing type")?.into(),
                name: owned(r, "name")?.ok_or("missing name")?,
                aliases: strings(r, "aliases")?,
                labels: strings(r, "labels")?,
                summary: owned(r, "summary")?,
                props: props(r)?,
                status,
                merged_into: reference(r, "merged_into", ObjectKind::Node)?,
                meta,
            }),
            ObjectKind::Link => Row::Link(Link {
                idx,
                key: owned(r, "key")?,
                type_: text(r, "type")?.ok_or("missing type")?.into(),
                src: reference(r, "from", ObjectKind::Node)?.ok_or("missing from")?,
                dst: reference(r, "to", ObjectKind::Node)?.ok_or("missing to")?,
                directed: r
                    .get("directed")
                    .and_then(Json::as_bool)
                    .ok_or("missing directed")?,
                multi: r.get("multi").and_then(Json::as_bool).unwrap_or(false),
                props: props(r)?,
                valid_from: value(r, "valid_from")?,
                valid_to: value(r, "valid_to")?,
                status,
                meta,
            }),
            ObjectKind::Evidence => {
                let sha = owned(r, "content_sha256")?;
                if let Some(c) = &content
                    && sha.as_deref() != Some(sha256_hex(c.as_bytes()).as_str())
                {
                    return Err(format!("import.content_mismatch: {id}"));
                }
                let blob = r.get("blob").filter(|b| !b.is_null());
                Row::Evidence(Evidence {
                    idx,
                    key: owned(r, "key")?,
                    kind: owned(r, "kind")?.ok_or("missing kind")?,
                    title: owned(r, "title")?,
                    content,
                    content_sha256: sha,
                    uri: owned(r, "uri")?,
                    blob_sha256: blob.and_then(|b| b["sha256"].as_str()).map(str::to_owned),
                    blob_size: blob.and_then(|b| b["size"].as_i64()),
                    mime: owned(r, "mime")?,
                    parent: reference(r, "parent", ObjectKind::Evidence)?,
                    locator: object(r, "locator"),
                    source_date: value(r, "source_date")?,
                    source_version: owned(r, "source_version")?,
                    supersedes: reference(r, "supersedes", ObjectKind::Evidence)?,
                    captured_at: time(r, "captured_at")?,
                    author: owned(r, "author")?,
                    publisher: owned(r, "publisher")?,
                    language: owned(r, "language")?,
                    license: owned(r, "license")?,
                    reliability: r.get("reliability").and_then(Json::as_f64),
                    extra: object(r, "meta"),
                    status,
                    meta,
                })
            }
            ObjectKind::Citation => {
                let t = r
                    .get("target")
                    .and_then(Json::as_object)
                    .ok_or("missing target")?;
                let (target_kind, target) = match (t.get("node"), t.get("link")) {
                    (Some(n), None) => (TargetKind::Node, id_of(n, ObjectKind::Node)?),
                    (None, Some(l)) => (TargetKind::Link, id_of(l, ObjectKind::Link)?),
                    _ => return Err("bad target".into()),
                };
                Row::Citation(Citation {
                    idx,
                    evidence: reference(r, "evidence", ObjectKind::Evidence)?
                        .ok_or("missing evidence")?,
                    target_kind,
                    target,
                    prop: owned(r, "prop")?,
                    role: text(r, "role")?
                        .and_then(|s| (0..4).filter_map(Role::from_code).find(|x| x.name() == s))
                        .ok_or("bad role")?,
                    confidence: r.get("confidence").and_then(Json::as_f64),
                    quote: owned(r, "quote")?,
                    note: owned(r, "note")?,
                    status,
                    meta,
                })
            }
            ObjectKind::Batch => return Err("batches are not exported".into()),
        })
    }
}

fn text<'a>(r: &'a Map<String, Json>, k: &str) -> Result<Option<&'a str>, String> {
    match r.get(k) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{k}: expected a string")),
    }
}

fn owned(r: &Map<String, Json>, k: &str) -> Result<Option<String>, String> {
    Ok(text(r, k)?.map(str::to_owned))
}

fn strings(r: &Map<String, Json>, k: &str) -> Result<Vec<String>, String> {
    match r.get(k) {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(Json::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or(format!("{k}: expected strings"))
            })
            .collect(),
        Some(_) => Err(format!("{k}: expected a list")),
    }
}

fn object(r: &Map<String, Json>, k: &str) -> Option<Map<String, Json>> {
    r.get(k).and_then(Json::as_object).cloned()
}

fn time(r: &Map<String, Json>, k: &str) -> Result<i64, String> {
    let s = text(r, k)?.ok_or(format!("missing {k}"))?;
    DateTime::parse(s)
        .map(|d| d.ms)
        .map_err(|e| format!("{k}: {e}"))
}

fn id_of(v: &Json, kind: ObjectKind) -> Result<i64, String> {
    let s = v.as_str().ok_or("expected an ID")?;
    match ObjectId::decode(s) {
        Ok(o) if o.kind == kind => Ok(o.number as i64),
        _ => Err(format!("{s}: expected a {} ID", kind.name())),
    }
}

fn reference(r: &Map<String, Json>, k: &str, kind: ObjectKind) -> Result<Option<i64>, String> {
    match r.get(k) {
        None | Some(Json::Null) => Ok(None),
        Some(v) => id_of(v, kind).map(Some).map_err(|e| format!("{k}: {e}")),
    }
}

fn value(r: &Map<String, Json>, k: &str) -> Result<Option<Value>, String> {
    match r.get(k) {
        None | Some(Json::Null) => Ok(None),
        Some(v) => match values::parse(v).map_err(|e| format!("{k}: {e}"))? {
            Parsed::Value(v) => Ok(Some(v)),
            Parsed::Remove => Ok(None),
        },
    }
}

fn props(r: &Map<String, Json>) -> Result<PropMap, String> {
    let mut p = PropMap::new();
    if let Some(m) = r.get("props").and_then(Json::as_object) {
        for (k, v) in m {
            if let Parsed::Value(v) = values::parse(v).map_err(|e| format!("props.{k}: {e}"))? {
                p.insert(k.as_str(), v);
            }
        }
    }
    Ok(p)
}
