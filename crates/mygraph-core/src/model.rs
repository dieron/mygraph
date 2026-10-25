//! Objects as stored — spec/model/objects.md. Records use internal numbers (`idx`) for objects
//! and actors; [`record_json`] renders the API form used by change-log diffs and responses.

use crate::ids::{ObjectId, ObjectKind};
use crate::values::{DateTime, Value};
use serde_json::{Map, Value as Json, json};
use std::collections::BTreeMap;

/// Property map; `BTreeMap` keeps keys in code point order ([VAL-STORE-1]).
pub type PropMap = BTreeMap<String, Value>;

/// [OBJ-LIFE-1] `Merged` exists only for nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Active,
    Retracted,
    Merged,
}

impl Status {
    pub fn code(self) -> i64 {
        match self {
            Self::Active => 0,
            Self::Retracted => 1,
            Self::Merged => 2,
        }
    }

    pub fn from_code(c: i64) -> Option<Self> {
        match c {
            0 => Some(Self::Active),
            1 => Some(Self::Retracted),
            2 => Some(Self::Merged),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retracted => "retracted",
            Self::Merged => "merged",
        }
    }
}

/// [OBJ-COMMON-1] record-time fields; `*_by` are actor indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordMeta {
    pub version: i64,
    pub created_at: i64,
    pub created_by: i64,
    pub updated_at: i64,
    pub updated_by: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub idx: i64,
    pub key: Option<String>,
    pub type_: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub labels: Vec<String>,
    pub summary: Option<String>,
    pub props: PropMap,
    pub status: Status,
    pub merged_into: Option<i64>,
    pub meta: RecordMeta,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub idx: i64,
    pub key: Option<String>,
    pub type_: String,
    pub src: i64,
    pub dst: i64,
    pub directed: bool,
    pub multi: bool,
    pub props: PropMap,
    /// `Value::Date` or `Value::DateTime`.
    pub valid_from: Option<Value>,
    pub valid_to: Option<Value>,
    pub status: Status,
    pub meta: RecordMeta,
}

/// [OBJ §5] evidence kinds.
pub const EVIDENCE_KINDS: [&str; 9] = [
    "text",
    "url",
    "file",
    "image",
    "document",
    "conversation",
    "observation",
    "dataset",
    "other",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    pub idx: i64,
    pub key: Option<String>,
    pub kind: String,
    pub title: Option<String>,
    /// Inline text. Not kept in memory after loading; `content_sha256` identifies it.
    pub content: Option<String>,
    pub content_sha256: Option<String>,
    pub uri: Option<String>,
    pub blob_sha256: Option<String>,
    pub blob_size: Option<i64>,
    pub mime: Option<String>,
    pub parent: Option<i64>,
    pub locator: Option<Map<String, Json>>,
    pub source_date: Option<Value>,
    pub source_version: Option<String>,
    pub supersedes: Option<i64>,
    pub captured_at: i64,
    pub author: Option<String>,
    pub publisher: Option<String>,
    pub language: Option<String>,
    pub license: Option<String>,
    pub reliability: Option<f64>,
    pub extra: Option<Map<String, Json>>,
    pub status: Status,
    pub meta: RecordMeta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetKind {
    Node,
    Link,
}

impl TargetKind {
    pub fn code(self) -> i64 {
        match self {
            Self::Node => 0,
            Self::Link => 1,
        }
    }

    pub fn object_kind(self) -> ObjectKind {
        match self {
            Self::Node => ObjectKind::Node,
            Self::Link => ObjectKind::Link,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Supports,
    Contradicts,
    Mentions,
    DerivedFrom,
}

impl Role {
    pub fn code(self) -> i64 {
        match self {
            Self::Supports => 0,
            Self::Contradicts => 1,
            Self::Mentions => 2,
            Self::DerivedFrom => 3,
        }
    }

    pub fn from_code(c: i64) -> Option<Self> {
        [
            Self::Supports,
            Self::Contradicts,
            Self::Mentions,
            Self::DerivedFrom,
        ]
        .into_iter()
        .find(|r| r.code() == c)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
            Self::Mentions => "mentions",
            Self::DerivedFrom => "derived_from",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Supports,
            Self::Contradicts,
            Self::Mentions,
            Self::DerivedFrom,
        ]
        .into_iter()
        .find(|r| r.name() == s)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Citation {
    pub idx: i64,
    pub evidence: i64,
    pub target_kind: TargetKind,
    pub target: i64,
    pub prop: Option<String>,
    pub role: Role,
    pub confidence: Option<f64>,
    pub quote: Option<String>,
    pub note: Option<String>,
    pub status: Status,
    pub meta: RecordMeta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub idx: i64,
    pub name: String,
    pub kind: String,
}

/// Any stored object. Rows are short-lived values moved between the engine and the store, so
/// boxing the larger variants would only add allocations.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Node(Node),
    Link(Link),
    Evidence(Evidence),
    Citation(Citation),
}

impl Row {
    pub fn kind(&self) -> ObjectKind {
        match self {
            Self::Node(_) => ObjectKind::Node,
            Self::Link(_) => ObjectKind::Link,
            Self::Evidence(_) => ObjectKind::Evidence,
            Self::Citation(_) => ObjectKind::Citation,
        }
    }

    pub fn idx(&self) -> i64 {
        match self {
            Self::Node(n) => n.idx,
            Self::Link(l) => l.idx,
            Self::Evidence(e) => e.idx,
            Self::Citation(c) => c.idx,
        }
    }

    pub fn status(&self) -> Status {
        match self {
            Self::Node(n) => n.status,
            Self::Link(l) => l.status,
            Self::Evidence(e) => e.status,
            Self::Citation(c) => c.status,
        }
    }

    pub fn meta(&self) -> RecordMeta {
        match self {
            Self::Node(n) => n.meta,
            Self::Link(l) => l.meta,
            Self::Evidence(e) => e.meta,
            Self::Citation(c) => c.meta,
        }
    }

    pub fn meta_mut(&mut self) -> &mut RecordMeta {
        match self {
            Self::Node(n) => &mut n.meta,
            Self::Link(l) => &mut l.meta,
            Self::Evidence(e) => &mut e.meta,
            Self::Citation(c) => &mut c.meta,
        }
    }
}

/// `obj_kind` / `target_kind` code of an object kind ([STORE-COL-1]).
pub fn obj_kind_code(kind: ObjectKind) -> i64 {
    match kind {
        ObjectKind::Node => 0,
        ObjectKind::Link => 1,
        ObjectKind::Evidence => 2,
        ObjectKind::Citation => 3,
        ObjectKind::Batch => -1,
    }
}

pub fn id_string(kind: ObjectKind, idx: i64) -> String {
    ObjectId::new(kind, idx as u64).encode()
}

fn opt<T: Into<Json>>(v: Option<T>) -> Json {
    v.map_or(Json::Null, Into::into)
}

/// `{"node": "n_…"}` or `{"link": "l_…"}`.
pub fn target_json(kind: TargetKind, idx: i64) -> Json {
    let name = match kind {
        TargetKind::Node => "node",
        TargetKind::Link => "link",
    };
    let mut m = Map::new();
    m.insert(name.into(), id_string(kind.object_kind(), idx).into());
    Json::Object(m)
}

pub fn props_json(p: &PropMap) -> Json {
    Json::Object(p.iter().map(|(k, v)| (k.clone(), v.to_json())).collect())
}

pub fn time_json(ms: i64) -> Json {
    Json::String(DateTime { ms }.canonical())
}

/// API form of a record ([STORE-LOG-3]). `actor` maps actor indexes to names.
pub fn record_json(row: &Row, actor: &dyn Fn(i64) -> String) -> Map<String, Json> {
    let mut m = match row {
        Row::Node(n) => json!({
            "id": id_string(ObjectKind::Node, n.idx),
            "key": opt(n.key.clone()),
            "type": n.type_,
            "name": n.name,
            "aliases": n.aliases,
            "labels": n.labels,
            "summary": opt(n.summary.clone()),
            "props": props_json(&n.props),
            "status": n.status.name(),
            "merged_into": opt(n.merged_into.map(|i| id_string(ObjectKind::Node, i))),
        }),
        Row::Link(l) => json!({
            "id": id_string(ObjectKind::Link, l.idx),
            "key": opt(l.key.clone()),
            "type": l.type_,
            "from": id_string(ObjectKind::Node, l.src),
            "to": id_string(ObjectKind::Node, l.dst),
            "directed": l.directed,
            "multi": l.multi,
            "props": props_json(&l.props),
            "valid_from": l.valid_from.as_ref().map_or(Json::Null, Value::to_json),
            "valid_to": l.valid_to.as_ref().map_or(Json::Null, Value::to_json),
            "status": l.status.name(),
        }),
        Row::Evidence(e) => json!({
            "id": id_string(ObjectKind::Evidence, e.idx),
            "key": opt(e.key.clone()),
            "kind": e.kind,
            "title": opt(e.title.clone()),
            "content_sha256": opt(e.content_sha256.clone()),
            "uri": opt(e.uri.clone()),
            "blob": e.blob_sha256.as_ref().map_or(Json::Null, |h| json!({"sha256": h, "size": e.blob_size})),
            "mime": opt(e.mime.clone()),
            "parent": opt(e.parent.map(|i| id_string(ObjectKind::Evidence, i))),
            "locator": opt(e.locator.clone().map(Json::Object)),
            "source_date": e.source_date.as_ref().map_or(Json::Null, Value::to_json),
            "source_version": opt(e.source_version.clone()),
            "supersedes": opt(e.supersedes.map(|i| id_string(ObjectKind::Evidence, i))),
            "captured_at": time_json(e.captured_at),
            "author": opt(e.author.clone()),
            "publisher": opt(e.publisher.clone()),
            "language": opt(e.language.clone()),
            "license": opt(e.license.clone()),
            "reliability": opt(e.reliability),
            "meta": opt(e.extra.clone().map(Json::Object)),
            "status": e.status.name(),
        }),
        Row::Citation(c) => json!({
            "id": id_string(ObjectKind::Citation, c.idx),
            "evidence": id_string(ObjectKind::Evidence, c.evidence),
            "target": target_json(c.target_kind, c.target),
            "prop": opt(c.prop.clone()),
            "role": c.role.name(),
            "confidence": opt(c.confidence),
            "quote": opt(c.quote.clone()),
            "note": opt(c.note.clone()),
            "status": c.status.name(),
        }),
    };
    let meta = row.meta();
    let obj = m.as_object_mut().expect("object");
    obj.insert("version".into(), meta.version.into());
    obj.insert("created_at".into(), time_json(meta.created_at));
    obj.insert("created_by".into(), actor(meta.created_by).into());
    obj.insert("updated_at".into(), time_json(meta.updated_at));
    obj.insert("updated_by".into(), actor(meta.updated_by).into());
    std::mem::take(obj)
}

/// [OBJ-GRAPH-1] graph policies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policies {
    pub require_evidence: RequireEvidence,
    pub inline_text_limit: u32,
    pub normalize_types: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequireEvidence {
    Enforce,
    Warn,
    Off,
}

impl RequireEvidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enforce => "enforce",
            Self::Warn => "warn",
            Self::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "enforce" => Some(Self::Enforce),
            "warn" => Some(Self::Warn),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

impl Default for Policies {
    fn default() -> Self {
        Self {
            require_evidence: RequireEvidence::Enforce,
            inline_text_limit: 65_536,
            normalize_types: true,
        }
    }
}

impl Policies {
    /// [OBJ-GRAPH-1] value ranges.
    pub fn validate(&self) -> Result<(), crate::Reason> {
        if !(1024..=1_048_576).contains(&self.inline_text_limit) {
            return Err(crate::Reason("graph.policy_invalid"));
        }
        Ok(())
    }

    pub fn to_json(&self) -> Json {
        json!({
            "require_evidence": self.require_evidence.as_str(),
            "inline_text_limit": self.inline_text_limit,
            "normalize_types": self.normalize_types,
        })
    }

    pub fn from_json(v: &Json) -> Option<Self> {
        Some(Self {
            require_evidence: RequireEvidence::parse(v["require_evidence"].as_str()?)?,
            inline_text_limit: u32::try_from(v["inline_text_limit"].as_u64()?).ok()?,
            normalize_types: v["normalize_types"].as_bool()?,
        })
    }
}
