//! Record reads and writes — the engine's `Store` trait on SQLite
//! (spec/storage/storage-format.md §2–§4).

use crate::GraphFile;
use mygraph_core::Reason;
use mygraph_core::model::{
    Actor, Citation, Evidence, Link, Node, PropMap, RecordMeta, Role, Row, Status, TargetKind,
};
use mygraph_core::store::{
    BatchSummary, BlobStore, ChangeSet, Counters, LoadSink, LogRecord, RowOp, Store, StoreFailure,
    TextHit, TextTarget, VocabularyNote,
};
use mygraph_core::values::{self, Value};
use rusqlite::{OptionalExtension, Row as SqlRow, Transaction, params};
use serde_json::{Map, Value as Json};

fn json_list(items: &[String]) -> Option<String> {
    (!items.is_empty()).then(|| Json::from(items.to_vec()).to_string())
}

fn read_list(text: Option<String>) -> Result<Vec<String>, StoreFailure> {
    Ok(match text {
        None => Vec::new(),
        Some(t) => serde_json::from_str(&t)?,
    })
}

fn read_object(text: Option<String>) -> Result<Option<Map<String, Json>>, StoreFailure> {
    Ok(match text {
        None => None,
        Some(t) => Some(serde_json::from_str(&t)?),
    })
}

fn object_text(m: &Option<Map<String, Json>>) -> Option<String> {
    m.as_ref().map(|m| Json::Object(m.clone()).to_string())
}

fn read_temporal(text: Option<String>) -> Result<Option<Value>, StoreFailure> {
    Ok(text.map(|t| values::parse_temporal_text(&t)).transpose()?)
}

fn temporal_text(v: &Option<Value>) -> Option<String> {
    v.as_ref().and_then(values::temporal_text)
}

fn status(code: i64) -> Result<Status, StoreFailure> {
    Ok(Status::from_code(code).ok_or(Reason("storage.status_invalid"))?)
}

fn meta(r: &SqlRow, at: usize) -> rusqlite::Result<RecordMeta> {
    Ok(RecordMeta {
        version: r.get(at)?,
        created_at: r.get(at + 1)?,
        created_by: r.get(at + 2)?,
        updated_at: r.get(at + 3)?,
        updated_by: r.get(at + 4)?,
    })
}

/// [STORE-FTSYNC-1] props text.
fn props_text(props: &PropMap) -> String {
    let mut s = String::new();
    for v in props.values() {
        match v {
            Value::String(t) => {
                s.push_str(t);
                s.push('\n');
            }
            Value::List(items) => {
                for i in items {
                    if let Value::String(t) = i {
                        s.push_str(t);
                        s.push('\n');
                    }
                }
            }
            _ => {}
        }
    }
    s
}

const NODE_COLS: &str = "idx, key, type, name, aliases, labels, summary, props, status, merged_into, version, created_at, created_by, updated_at, updated_by";
const LINK_COLS: &str = "idx, key, type, src, dst, directed, multi, props, valid_from, valid_to, status, version, created_at, created_by, updated_at, updated_by";
const EVIDENCE_COLS: &str = "idx, key, kind, title, content, content_sha256, uri, blob_sha256, blob_size, mime, parent, locator, source_date, source_version, supersedes, captured_at, author, publisher, language, license, reliability, meta, status, version, created_at, created_by, updated_at, updated_by";
const CITATION_COLS: &str = "idx, evidence, target_kind, target, prop, role, confidence, quote, note, status, version, created_at, created_by, updated_at, updated_by";

fn placeholders(cols: &str) -> String {
    (1..=cols.split(',').count())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn upsert_sql(table: &str, cols: &str, insert: bool) -> String {
    if insert {
        format!(
            "INSERT INTO {table} ({cols}) VALUES ({})",
            placeholders(cols)
        )
    } else {
        let sets: Vec<String> = cols
            .split(',')
            .map(str::trim)
            .enumerate()
            .skip(1)
            .map(|(i, c)| format!("{c} = ?{}", i + 1))
            .collect();
        format!("UPDATE {table} SET {} WHERE idx = ?1", sets.join(", "))
    }
}

impl GraphFile {
    fn write_node(tx: &Transaction, n: &Node, insert: bool) -> Result<(), StoreFailure> {
        let m = n.meta;
        tx.execute(
            &upsert_sql("node", NODE_COLS, insert),
            params![
                n.idx,
                n.key,
                n.type_,
                n.name,
                json_list(&n.aliases),
                json_list(&n.labels),
                n.summary,
                values::encode_props(&n.props),
                n.status.code(),
                n.merged_into,
                m.version,
                m.created_at,
                m.created_by,
                m.updated_at,
                m.updated_by,
            ],
        )?;
        // [STORE-FTSYNC-2]
        if !insert {
            tx.execute("DELETE FROM fts_node WHERE rowid = ?1", [n.idx])?;
            tx.execute("DELETE FROM fts_name WHERE rowid = ?1", [n.idx])?;
        }
        let aliases = n.aliases.join("\n");
        tx.execute(
            "INSERT INTO fts_node (rowid, name, aliases, summary, props_text) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![n.idx, n.name, aliases, n.summary, props_text(&n.props)],
        )?;
        tx.execute(
            "INSERT INTO fts_name (rowid, name, aliases) VALUES (?1, ?2, ?3)",
            params![n.idx, n.name, aliases],
        )?;
        Ok(())
    }

    fn write_link(tx: &Transaction, l: &Link, insert: bool) -> Result<(), StoreFailure> {
        let m = l.meta;
        tx.execute(
            &upsert_sql("link", LINK_COLS, insert),
            params![
                l.idx,
                l.key,
                l.type_,
                l.src,
                l.dst,
                l.directed,
                l.multi,
                values::encode_props(&l.props),
                temporal_text(&l.valid_from),
                temporal_text(&l.valid_to),
                l.status.code(),
                m.version,
                m.created_at,
                m.created_by,
                m.updated_at,
                m.updated_by,
            ],
        )?;
        if !insert {
            tx.execute("DELETE FROM fts_link WHERE rowid = ?1", [l.idx])?;
        }
        tx.execute(
            "INSERT INTO fts_link (rowid, type_text, props_text) VALUES (?1, ?2, ?3)",
            params![l.idx, l.type_.replace('_', " "), props_text(&l.props)],
        )?;
        Ok(())
    }

    fn write_evidence(tx: &Transaction, e: &Evidence, insert: bool) -> Result<(), StoreFailure> {
        let m = e.meta;
        if insert {
            tx.execute(
                &upsert_sql("evidence", EVIDENCE_COLS, true),
                params![
                    e.idx,
                    e.key,
                    e.kind,
                    e.title,
                    e.content,
                    e.content_sha256,
                    e.uri,
                    e.blob_sha256,
                    e.blob_size,
                    e.mime,
                    e.parent,
                    object_text(&e.locator),
                    temporal_text(&e.source_date),
                    e.source_version,
                    e.supersedes,
                    e.captured_at,
                    e.author,
                    e.publisher,
                    e.language,
                    e.license,
                    e.reliability,
                    object_text(&e.extra),
                    e.status.code(),
                    m.version,
                    m.created_at,
                    m.created_by,
                    m.updated_at,
                    m.updated_by,
                ],
            )?;
            tx.execute(
                "INSERT INTO fts_evidence (rowid, title, content) VALUES (?1, ?2, ?3)",
                params![e.idx, e.title, e.content],
            )?;
            return Ok(());
        }
        // Content, blob, uri, kind, parent never change ([OBJ-EV-2]); update the rest.
        let (old_title, content): (Option<String>, Option<String>) = tx.query_row(
            "SELECT title, content FROM evidence WHERE idx = ?1",
            [e.idx],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        tx.execute(
            "UPDATE evidence SET key = ?2, title = ?3, mime = ?4, locator = ?5, source_date = ?6, source_version = ?7,
               supersedes = ?8, captured_at = ?9, author = ?10, publisher = ?11, language = ?12, license = ?13,
               reliability = ?14, meta = ?15, status = ?16, version = ?17, updated_at = ?18, updated_by = ?19
             WHERE idx = ?1",
            params![
                e.idx, e.key, e.title, e.mime, object_text(&e.locator), temporal_text(&e.source_date),
                e.source_version, e.supersedes, e.captured_at, e.author, e.publisher, e.language, e.license,
                e.reliability, object_text(&e.extra), e.status.code(), m.version, m.updated_at, m.updated_by,
            ],
        )?;
        if old_title != e.title {
            tx.execute(
                "INSERT INTO fts_evidence (fts_evidence, rowid, title, content) VALUES ('delete', ?1, ?2, ?3)",
                params![e.idx, old_title, content],
            )?;
            tx.execute(
                "INSERT INTO fts_evidence (rowid, title, content) VALUES (?1, ?2, ?3)",
                params![e.idx, e.title, content],
            )?;
        }
        Ok(())
    }

    fn write_citation(tx: &Transaction, c: &Citation, insert: bool) -> Result<(), StoreFailure> {
        let m = c.meta;
        tx.execute(
            &upsert_sql("citation", CITATION_COLS, insert),
            params![
                c.idx,
                c.evidence,
                c.target_kind.code(),
                c.target,
                c.prop,
                c.role.code(),
                c.confidence,
                c.quote,
                c.note,
                c.status.code(),
                m.version,
                m.created_at,
                m.created_by,
                m.updated_at,
                m.updated_by,
            ],
        )?;
        Ok(())
    }
}

impl Store for GraphFile {
    fn load_into(&self, sink: &mut dyn LoadSink) -> Result<(), StoreFailure> {
        let c = self.connection();

        let mut st = c.prepare("SELECT idx, name, kind FROM actor")?;
        for a in st.query_map([], |r| {
            Ok(Actor {
                idx: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
            })
        })? {
            sink.actor(a?);
        }

        let mut st = c.prepare(&format!("SELECT {NODE_COLS} FROM node"))?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            sink.row(Row::Node(Node {
                idx: r.get(0)?,
                key: r.get(1)?,
                type_: r.get(2)?,
                name: r.get(3)?,
                aliases: read_list(r.get(4)?)?,
                labels: read_list(r.get(5)?)?,
                summary: r.get(6)?,
                props: values::decode_props(r.get::<_, Option<String>>(7)?.as_deref())?,
                status: status(r.get(8)?)?,
                merged_into: r.get(9)?,
                meta: meta(r, 10)?,
            }));
        }

        let mut st = c.prepare(&format!("SELECT {LINK_COLS} FROM link"))?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            sink.row(Row::Link(Link {
                idx: r.get(0)?,
                key: r.get(1)?,
                type_: r.get(2)?,
                src: r.get(3)?,
                dst: r.get(4)?,
                directed: r.get(5)?,
                multi: r.get(6)?,
                props: values::decode_props(r.get::<_, Option<String>>(7)?.as_deref())?,
                valid_from: read_temporal(r.get(8)?)?,
                valid_to: read_temporal(r.get(9)?)?,
                status: status(r.get(10)?)?,
                meta: meta(r, 11)?,
            }));
        }

        let cols = EVIDENCE_COLS.replace("title, content,", "title, NULL,");
        let mut st = c.prepare(&format!("SELECT {cols} FROM evidence"))?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            sink.row(Row::Evidence(Evidence {
                idx: r.get(0)?,
                key: r.get(1)?,
                kind: r.get(2)?,
                title: r.get(3)?,
                content: None,
                content_sha256: r.get(5)?,
                uri: r.get(6)?,
                blob_sha256: r.get(7)?,
                blob_size: r.get(8)?,
                mime: r.get(9)?,
                parent: r.get(10)?,
                locator: read_object(r.get(11)?)?,
                source_date: read_temporal(r.get(12)?)?,
                source_version: r.get(13)?,
                supersedes: r.get(14)?,
                captured_at: r.get(15)?,
                author: r.get(16)?,
                publisher: r.get(17)?,
                language: r.get(18)?,
                license: r.get(19)?,
                reliability: r.get(20)?,
                extra: read_object(r.get(21)?)?,
                status: status(r.get(22)?)?,
                meta: meta(r, 23)?,
            }));
        }

        let mut st = c.prepare(&format!("SELECT {CITATION_COLS} FROM citation"))?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let tk: i64 = r.get(2)?;
            sink.row(Row::Citation(Citation {
                idx: r.get(0)?,
                evidence: r.get(1)?,
                target_kind: if tk == 0 {
                    TargetKind::Node
                } else {
                    TargetKind::Link
                },
                target: r.get(3)?,
                prop: r.get(4)?,
                role: Role::from_code(r.get(5)?).ok_or(Reason("storage.role_invalid"))?,
                confidence: r.get(6)?,
                quote: r.get(7)?,
                note: r.get(8)?,
                status: status(r.get(9)?)?,
                meta: meta(r, 10)?,
            }));
        }

        let mut st = c.prepare("SELECT name FROM type_info WHERE kind = 1 AND symmetric = 1")?;
        for n in st.query_map([], |r| r.get(0))? {
            sink.symmetric_link_type(n?);
        }
        Ok(())
    }

    fn counters(&self) -> Result<Counters, StoreFailure> {
        let mut c = Counters::default();
        let conn = self.connection();
        let mut st = conn.prepare("SELECT name, seq FROM sqlite_sequence")?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let (name, seq): (String, i64) = (r.get(0)?, r.get(1)?);
            match name.as_str() {
                "node" => c.node = seq,
                "link" => c.link = seq,
                "evidence" => c.evidence = seq,
                "citation" => c.citation = seq,
                "batch" => c.batch = seq,
                "actor" => c.actor = seq,
                "changelog" => c.seq = seq,
                _ => {}
            }
        }
        Ok(c)
    }

    fn apply(&mut self, cs: &ChangeSet) -> Result<(), StoreFailure> {
        let tx = self.conn_mut().transaction()?;
        for a in &cs.new_actors {
            tx.execute(
                "INSERT INTO actor (idx, name, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![a.idx, a.name, a.kind, cs.ts],
            )?;
        }
        let b = &cs.batch;
        tx.execute(
            "INSERT INTO batch (idx, actor, op, created_at, note) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![b.idx, b.actor, b.op, b.created_at, b.note],
        )?;
        for op in &cs.ops {
            let (row, insert) = match op {
                RowOp::Insert(r) => (r, true),
                RowOp::Update(r) => (r, false),
            };
            match row {
                Row::Node(n) => Self::write_node(&tx, n, insert)?,
                Row::Link(l) => Self::write_link(&tx, l, insert)?,
                Row::Evidence(e) => Self::write_evidence(&tx, e, insert)?,
                Row::Citation(c) => Self::write_citation(&tx, c, insert)?,
            }
        }
        for e in &cs.log {
            tx.execute(
                "INSERT INTO changelog (seq, ts, actor, batch, op, obj_kind, obj, diff) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![e.seq, cs.ts, b.actor, b.idx, e.op, e.obj_kind, e.obj, e.diff.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn last_retraction(&self, obj_kind: i64, obj: i64) -> Result<Option<i64>, StoreFailure> {
        Ok(self.connection().query_row(
            "SELECT max(seq) FROM changelog WHERE obj_kind = ?1 AND obj = ?2 AND op = 'retract'",
            [obj_kind, obj],
            |r| r.get(0),
        )?)
    }

    fn cascaded_from(&self, seq: i64) -> Result<Vec<(i64, i64)>, StoreFailure> {
        let c = self.connection();
        let mut st = c.prepare(
            "SELECT obj_kind, obj FROM changelog WHERE op = 'retract' AND json_extract(diff, '$.cascade_of') = ?1 ORDER BY seq",
        )?;
        let rows = st.query_map([seq], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn batch_objects(&self, batch: i64, op: &str) -> Result<Vec<(i64, i64)>, StoreFailure> {
        let sql = if op == "retract" {
            "SELECT obj_kind, obj FROM changelog WHERE batch = ?1 AND op = 'retract' AND json_extract(diff, '$.cascade_of') IS NULL AND obj IS NOT NULL ORDER BY seq"
        } else {
            "SELECT obj_kind, obj FROM changelog WHERE batch = ?1 AND op = 'create' AND obj IS NOT NULL ORDER BY seq"
        };
        let c = self.connection();
        let mut st = c.prepare(sql)?;
        let rows = st.query_map([batch], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn history(
        &self,
        object: Option<(i64, i64)>,
        since: Option<i64>,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<LogRecord>, StoreFailure> {
        const COLS: &str = "seq, ts, actor, batch, op, obj_kind, obj, diff";
        let take = (limit + 1) as i64;
        let c = self.connection();
        let row = |r: &rusqlite::Row| {
            Ok(LogRecord {
                seq: r.get(0)?,
                ts: r.get(1)?,
                actor: r.get(2)?,
                batch: r.get(3)?,
                op: r.get(4)?,
                obj_kind: r.get(5)?,
                obj: r.get(6)?,
                diff: r.get(7)?,
            })
        };
        let rows = match object {
            Some((kind, obj)) => {
                let mut st = c.prepare_cached(&format!(
                    "SELECT {COLS} FROM changelog WHERE obj_kind = ?1 AND obj = ?2 AND seq < ?3 ORDER BY seq DESC LIMIT ?4"
                ))?;
                st.query_map((kind, obj, before.unwrap_or(i64::MAX), take), row)?
                    .collect::<Result<Vec<_>, _>>()?
            }
            None => {
                let mut st = c.prepare_cached(&format!(
                    "SELECT {COLS} FROM changelog WHERE seq > ?1 ORDER BY seq LIMIT ?2"
                ))?;
                st.query_map((since.unwrap_or(0), take), row)?
                    .collect::<Result<Vec<_>, _>>()?
            }
        };
        Ok(rows)
    }

    fn changed_since(&self, seq: i64) -> Result<Vec<(i64, i64)>, StoreFailure> {
        let c = self.connection();
        let mut st = c.prepare(
            "SELECT DISTINCT obj_kind, obj FROM changelog WHERE seq > ?1 AND obj IS NOT NULL",
        )?;
        let rows = st.query_map([seq], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn evidence_content(&self, idx: i64) -> Result<Option<String>, StoreFailure> {
        Ok(self
            .connection()
            .prepare_cached("SELECT content FROM evidence WHERE idx = ?1")?
            .query_row([idx], |r| r.get(0))
            .optional()?
            .flatten())
    }

    fn blobs(&self) -> &dyn BlobStore {
        &self.blobs
    }

    fn text_search(
        &self,
        target: TextTarget,
        tokens: &[String],
        limit: usize,
    ) -> Result<Vec<TextHit>, StoreFailure> {
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        // Tokens are letters, marks and numbers only, so quoting them is enough.
        let mut query: Vec<String> = tokens.iter().map(|t| format!("\"{t}\"")).collect();
        if let Some(last) = query.last_mut() {
            last.push('*');
        }
        let query = query.join(" ");
        let sql = match target {
            TextTarget::Nodes => {
                "SELECT rowid, NULL FROM fts_node WHERE fts_node MATCH ?1 ORDER BY rank LIMIT ?2"
            }
            TextTarget::Links => {
                "SELECT rowid, NULL FROM fts_link WHERE fts_link MATCH ?1 ORDER BY rank LIMIT ?2"
            }
            TextTarget::Evidence => {
                "SELECT rowid, snippet(fts_evidence, -1, '«', '»', '…', 12) FROM fts_evidence WHERE fts_evidence MATCH ?1 ORDER BY rank LIMIT ?2"
            }
        };
        let c = self.connection();
        let mut st = c.prepare_cached(sql)?;
        let rows = st.query_map((query, limit as i64), |r| {
            Ok(TextHit {
                idx: r.get(0)?,
                snippet: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn checkpoint(&self) -> Result<(), StoreFailure> {
        self.connection()
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    fn recent_batches(&self, limit: usize) -> Result<Vec<BatchSummary>, StoreFailure> {
        let c = self.connection();
        let mut st = c.prepare_cached(
            "SELECT b.idx, b.actor, b.op, b.created_at, b.note, (SELECT count(*) FROM changelog l WHERE l.batch = b.idx) FROM batch b ORDER BY b.idx DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| {
            Ok(BatchSummary {
                idx: r.get(0)?,
                actor: r.get(1)?,
                op: r.get(2)?,
                created_at: r.get(3)?,
                note: r.get(4)?,
                changes: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn actor_batches(&self) -> Result<Vec<(i64, i64, i64)>, StoreFailure> {
        let c = self.connection();
        let mut st =
            c.prepare_cached("SELECT actor, count(*), max(created_at) FROM batch GROUP BY actor")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn vocabulary_notes(&self) -> Result<Vec<VocabularyNote>, StoreFailure> {
        let c = self.connection();
        let mut st =
            c.prepare_cached("SELECT kind, name, description FROM type_info ORDER BY kind, name")?;
        let rows = st.query_map([], |r| {
            Ok(VocabularyNote {
                kind: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
