//! Operation `write` — spec/api/operations/write.md, spec/algorithms/upsert-matching.md.

use crate::Reason;
use crate::engine::{Batch, Graph, OpError};
use crate::ids::{ObjectId, ObjectKind, validate_batch_ref, validate_key};
use crate::model::{
    Citation, EVIDENCE_KINDS, Evidence, Link, Node, PropMap, RecordMeta, Role, Row, Status,
    TargetKind, id_string, props_json,
};
use crate::name_key::{check_name, name_key};
use crate::state::{CitationIdentity, EvidenceIdentity, LinkIdentity};
use crate::store::Store;
use crate::text::nfc;
use crate::values::{self, Parsed, Value, temporal_bounds, values_equal};
use crate::vocab::{self, VocabKind};
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub(crate) type Obj = Map<String, Json>;
pub(crate) type Refs = HashMap<String, (ObjectKind, i64)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnConflict {
    Keep,
    Overwrite,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Created,
    Matched,
    Updated,
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Matched => "matched",
            Self::Updated => "updated",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Field helpers
// ---------------------------------------------------------------------------------------------

fn invalid(field: &str, expected: &str) -> OpError {
    OpError::new(
        "write.invalid_field",
        format!("field {field:?} must be {expected}"),
    )
    .details(json!({ "field": field }))
}

pub(crate) fn as_obj<'a>(v: &'a Json, what: &str) -> Result<&'a Obj, OpError> {
    v.as_object().ok_or_else(|| invalid(what, "an object"))
}

/// A present, non-null field.
pub(crate) fn get<'a>(o: &'a Obj, name: &str) -> Option<&'a Json> {
    o.get(name).filter(|v| !v.is_null())
}

pub(crate) fn opt_str(o: &Obj, name: &str) -> Result<Option<String>, OpError> {
    match get(o, name) {
        None => Ok(None),
        Some(Json::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(invalid(name, "a string")),
    }
}

/// NFC, control characters, byte limit (objects.md preamble).
pub(crate) fn clean_text(s: &str, max: usize, too_long: &'static str) -> Result<String, Reason> {
    let s = nfc(s);
    if s.chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
    {
        return Err(Reason("text.control_char"));
    }
    if s.len() > max {
        return Err(Reason(too_long));
    }
    Ok(s)
}

fn opt_text(
    o: &Obj,
    name: &str,
    max: usize,
    too_long: &'static str,
) -> Result<Option<String>, OpError> {
    opt_str(o, name)?
        .map(|s| clean_text(&s, max, too_long).map_err(|r| field_err(r, name)))
        .transpose()
}

fn field_err(r: Reason, field: &str) -> OpError {
    OpError::new(r.0, format!("{field}: {}", r.0)).details(json!({ "field": field }))
}

fn opt_bool(o: &Obj, name: &str) -> Result<Option<bool>, OpError> {
    match get(o, name) {
        None => Ok(None),
        Some(Json::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(invalid(name, "true or false")),
    }
}

fn opt_unit(o: &Obj, name: &str, range_reason: &'static str) -> Result<Option<f64>, OpError> {
    match get(o, name) {
        None => Ok(None),
        Some(Json::Number(n)) => {
            let x = n
                .to_string()
                .parse::<f64>()
                .map_err(|_| invalid(name, "a number"))?;
            if (0.0..=1.0).contains(&x) {
                Ok(Some(x))
            } else {
                Err(field_err(Reason(range_reason), name))
            }
        }
        Some(_) => Err(invalid(name, "a number")),
    }
}

fn str_list(o: &Obj, name: &str) -> Result<Vec<String>, OpError> {
    match get(o, name) {
        None => Ok(Vec::new()),
        Some(Json::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid(name, "a list of strings"))
            })
            .collect(),
        Some(_) => Err(invalid(name, "a list of strings")),
    }
}

fn json_object_field(
    o: &Obj,
    name: &str,
    max: usize,
) -> Result<Option<Map<String, Json>>, OpError> {
    match get(o, name) {
        None => Ok(None),
        Some(Json::Object(m)) => {
            if Json::Object(m.clone()).to_string().len() > max {
                return Err(field_err(Reason("evidence.field_too_long"), name));
            }
            Ok(Some(m.clone()))
        }
        Some(_) => Err(invalid(name, "an object")),
    }
}

/// A `date` or `datetime` value field.
fn temporal(o: &Obj, name: &str) -> Result<Option<Value>, OpError> {
    let Some(v) = get(o, name) else {
        return Ok(None);
    };
    match values::parse(v).map_err(|r| field_err(r, name))? {
        Parsed::Value(v @ (Value::Date(_) | Value::DateTime(_))) => Ok(Some(v)),
        _ => Err(invalid(
            name,
            "a {\"$date\": …} or {\"$datetime\": …} value",
        )),
    }
}

fn datetime_ms(o: &Obj, name: &str) -> Result<Option<i64>, OpError> {
    match temporal(o, name)? {
        None => Ok(None),
        Some(Value::DateTime(t)) => Ok(Some(t.ms)),
        Some(_) => Err(invalid(name, "a {\"$datetime\": …} value")),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn valid_uri(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once(':') else {
        return false;
    };
    let mut cs = scheme.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphabetic())
        && cs.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        && !rest.is_empty()
        && !s.chars().any(char::is_whitespace)
}

fn valid_language(s: &str) -> bool {
    let mut parts = s.split('-');
    let first = parts.next().unwrap_or("");
    (2..=8).contains(&first.len())
        && first.chars().all(|c| c.is_ascii_alphabetic())
        && parts.all(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

fn conflict_warning(
    b: &mut Batch,
    id: &str,
    field: &str,
    stored: Json,
    incoming: Json,
    mode: OnConflict,
) -> Result<(), OpError> {
    match mode {
        OnConflict::Error => Err(OpError::new(
            "write.property_conflict",
            format!("{id}: {field} differs from the stored value"),
        )
        .details(json!({ "id": id, "field": field, "stored": stored, "incoming": incoming }))),
        OnConflict::Keep => {
            b.warn(
                "PROPERTY_CONFLICT",
                format!("{id}: kept stored {field}"),
                json!({ "id": id, "field": field, "kept": stored, "incoming": incoming }),
            );
            Ok(())
        }
        OnConflict::Overwrite => Ok(()),
    }
}

/// [WRITE-FIELD-1] [WRITE-FIELD-2] for a single-valued field.
fn combine<T: PartialEq + Clone>(
    b: &mut Batch,
    id: &str,
    field: &str,
    stored: &mut Option<T>,
    incoming: Option<T>,
    mode: OnConflict,
    to_json: impl Fn(&T) -> Json,
) -> Result<(), OpError> {
    let Some(new) = incoming else { return Ok(()) };
    match stored {
        None => *stored = Some(new),
        Some(old) if *old == new => {}
        Some(old) => {
            conflict_warning(b, id, field, to_json(old), to_json(&new), mode)?;
            if mode == OnConflict::Overwrite {
                *stored = Some(new);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The operation
// ---------------------------------------------------------------------------------------------

struct Ctx {
    refs: Refs,
    on_conflict: OnConflict,
    items: Vec<Json>,
    counts: HashMap<&'static str, HashMap<&'static str, u64>>,
    created: Vec<(ObjectKind, i64)>,
}

impl Ctx {
    fn record(
        &mut self,
        section: &str,
        index: usize,
        outcome: Outcome,
        kind: ObjectKind,
        idx: i64,
    ) {
        self.items.push(json!({
            "section": section, "index": index, "outcome": outcome.name(),
            "id": id_string(kind, idx),
        }));
        *self
            .counts
            .entry(outcome.name())
            .or_default()
            .entry(kind.name())
            .or_default() += 1;
        if outcome == Outcome::Created {
            self.created.push((kind, idx));
        }
    }

    fn define_ref(&mut self, o: &Obj, kind: ObjectKind, idx: i64) -> Result<(), OpError> {
        let Some(r) = opt_str(o, "ref")? else {
            return Ok(());
        };
        validate_batch_ref(&r)?;
        if self.refs.insert(r.clone(), (kind, idx)).is_some() {
            return Err(OpError::new(
                "ref.duplicate",
                format!("batch reference {r} is defined twice"),
            ));
        }
        Ok(())
    }
}

impl<S: Store> Graph<S> {
    /// Operation `write` ([OP-WRITE-1] .. [OP-WRITE-6]).
    pub fn write(&mut self, actor: &str, request: &Json, now_ms: i64) -> Result<Json, OpError> {
        let req = as_obj(request, "request")?;
        let options = get(req, "options")
            .map(|v| as_obj(v, "options"))
            .transpose()?;
        let on_conflict = match options
            .and_then(|o| o.get("on_conflict"))
            .and_then(Json::as_str)
        {
            None | Some("keep") => OnConflict::Keep,
            Some("overwrite") => OnConflict::Overwrite,
            Some("error") => OnConflict::Error,
            Some(_) => return Err(invalid("options.on_conflict", "keep, overwrite or error")),
        };
        let dry_run = options
            .and_then(|o| o.get("dry_run"))
            .and_then(Json::as_bool)
            .unwrap_or(false);

        let sections = ["evidence", "nodes", "links", "citations", "updates"];
        let mut total = 0usize;
        for s in sections {
            if let Some(v) = get(req, s) {
                let items = v.as_array().ok_or_else(|| invalid(s, "a list"))?;
                total += items.len();
                for item in items {
                    if let Some(Json::Array(c)) = item.get("cite") {
                        total += c.len();
                    }
                }
            }
        }
        if total > 1000 {
            return Err(OpError::new(
                "write.too_many_items",
                format!("{total} items; the limit is 1000"),
            ));
        }

        let mut b = self.begin(actor, "write", now_ms)?;
        let mut ctx = Ctx {
            refs: Refs::new(),
            on_conflict,
            items: Vec::new(),
            counts: HashMap::new(),
            created: Vec::new(),
        };
        if let Err(e) = self.write_sections(&mut b, &mut ctx, req) {
            self.rollback(b);
            return Err(e);
        }
        let (batch, warnings) = self.finish(b, dry_run)?;
        let mut refs = Map::new();
        for (r, (kind, idx)) in &ctx.refs {
            let created = ctx.created.contains(&(*kind, *idx));
            let id = if dry_run && created {
                Json::Null
            } else {
                id_string(*kind, *idx).into()
            };
            refs.insert(r.clone(), id);
        }
        if dry_run {
            for item in &mut ctx.items {
                if item["outcome"] == "created" {
                    item["id"] = Json::Null;
                }
            }
        }
        Ok(json!({
            "batch": batch,
            "dry_run": dry_run,
            "refs": refs,
            "items": ctx.items,
            "counts": {
                "created": ctx.counts.get("created").cloned().unwrap_or_default(),
                "updated": ctx.counts.get("updated").cloned().unwrap_or_default(),
                "matched": ctx.counts.get("matched").cloned().unwrap_or_default(),
            },
            "warnings": warnings,
        }))
    }

    fn write_sections(&mut self, b: &mut Batch, ctx: &mut Ctx, req: &Obj) -> Result<(), OpError> {
        for (section, f) in [
            (
                "evidence",
                Self::write_evidence
                    as fn(
                        &mut Self,
                        &mut Batch,
                        &mut Ctx,
                        &Obj,
                    ) -> Result<(Outcome, ObjectKind, i64), OpError>,
            ),
            ("nodes", Self::write_node),
            ("links", Self::write_link),
            ("citations", Self::write_citation_item),
            ("updates", Self::write_update),
        ] {
            let Some(Json::Array(items)) = get(req, section) else {
                continue;
            };
            for (index, item) in items.iter().enumerate() {
                let o = as_obj(item, section).map_err(|e| e.at(section, index))?;
                let (outcome, kind, idx) = f(self, b, ctx, o).map_err(|e| e.at(section, index))?;
                ctx.record(section, index, outcome, kind, idx);
                if section != "updates" && section != "citations" {
                    ctx.define_ref(o, kind, idx)
                        .map_err(|e| e.at(section, index))?;
                }
                if let Some(cites) = get(o, "cite") {
                    let target = match kind {
                        ObjectKind::Node => TargetKind::Node,
                        ObjectKind::Link => TargetKind::Link,
                        _ => {
                            return Err(invalid("cite", "used on node, link or update items only")
                                .at(section, index));
                        }
                    };
                    let list = cites
                        .as_array()
                        .ok_or_else(|| invalid("cite", "a list").at(section, index))?;
                    for c in list {
                        let (outcome, cidx) = self
                            .write_cite(b, ctx, c, target, idx)
                            .map_err(|e| e.at(section, index))?;
                        ctx.record("cite", index, outcome, ObjectKind::Citation, cidx);
                    }
                }
            }
        }
        Ok(())
    }

    // ---- references ---------------------------------------------------------------------------

    pub(crate) fn resolve_ref(
        &self,
        refs: &Refs,
        v: &Json,
        kind: ObjectKind,
    ) -> Result<i64, OpError> {
        let not_found = || {
            OpError::new("ref.not_found", format!("no {} matches {v}", kind.name()))
                .details(json!({ "ref": v }))
        };
        match v {
            Json::String(s) if s.starts_with('@') => {
                validate_batch_ref(s)?;
                match refs.get(s) {
                    Some(&(k, idx)) if k == kind => Ok(idx),
                    Some(_) => Err(OpError::new(
                        "ref.wrong_kind",
                        format!("{s} is not a {}", kind.name()),
                    )),
                    None => Err(OpError::new(
                        "ref.undefined",
                        format!("batch reference {s} is not defined earlier in this request"),
                    )),
                }
            }
            Json::String(s) => {
                let id = ObjectId::decode_of_kind(s, kind)?;
                let idx = id.number as i64;
                if kind == ObjectKind::Node {
                    return self.state.resolve_node(idx).ok_or_else(not_found);
                }
                self.state.get(kind, idx).map(|_| idx).ok_or_else(not_found)
            }
            Json::Object(o) if o.contains_key("key") => {
                let key = validate_key(
                    o["key"]
                        .as_str()
                        .ok_or_else(|| invalid("key", "a string"))?,
                )?;
                let map = match kind {
                    ObjectKind::Node => &self.state.node_keys,
                    ObjectKind::Link => &self.state.link_keys,
                    ObjectKind::Evidence => &self.state.evidence_keys,
                    _ => return Err(invalid("key", "used for nodes, links or evidence")),
                };
                let idx = *map.get(&key).ok_or_else(not_found)?;
                if kind == ObjectKind::Node {
                    return self.state.resolve_node(idx).ok_or_else(not_found);
                }
                Ok(idx)
            }
            Json::Object(o) if kind == ObjectKind::Node && o.contains_key("name") => {
                let type_ = self.canon(
                    VocabKind::NodeType,
                    o.get("type")
                        .and_then(Json::as_str)
                        .ok_or_else(|| invalid("type", "a string"))?,
                )?;
                let name = o["name"]
                    .as_str()
                    .ok_or_else(|| invalid("name", "a string"))?;
                match self.name_matches(&type_, name).as_slice() {
                    [] => Err(not_found()),
                    [one] => Ok(*one),
                    many => Err(self.ambiguous(&type_, name, many)),
                }
            }
            _ => Err(OpError::new(
                "write.invalid_ref",
                format!("{v} is not a reference"),
            )),
        }
    }

    pub(crate) fn canon(&self, kind: VocabKind, s: &str) -> Result<String, Reason> {
        if self.policies.normalize_types {
            vocab::canonicalize(kind, s)
        } else {
            vocab::validate_verbatim(s)
        }
    }

    fn name_matches(&self, type_: &str, name: &str) -> Vec<i64> {
        self.state
            .names
            .get(&(type_.to_owned(), name_key(name)))
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    fn ambiguous(&self, type_: &str, name: &str, many: &[i64]) -> OpError {
        let candidates: Vec<Json> = many
            .iter()
            .map(|i| {
                let n = &self.state.nodes[i];
                json!({ "id": id_string(ObjectKind::Node, *i), "name": n.name, "summary": n.summary })
            })
            .collect();
        OpError::new(
            "write.ambiguous_match",
            format!("{} active {type_} nodes are named {name:?}; pass an id, a key, or create_new: true", many.len()),
        )
        .details(json!({ "candidates": candidates }))
    }

    fn require_active_node(&self, idx: i64) -> Result<(), OpError> {
        match self.state.nodes.get(&idx).map(|n| n.status) {
            Some(Status::Active) => Ok(()),
            _ => Err(OpError::new(
                "link.endpoint_not_active",
                format!("{} is not active", id_string(ObjectKind::Node, idx)),
            )),
        }
    }

    // ---- properties, aliases, labels ---------------------------------------------------------

    /// Parses a property object into canonical keys and values.
    fn parse_props(&self, v: &Json) -> Result<Vec<(String, Parsed)>, OpError> {
        let o = as_obj(v, "props")?;
        o.iter()
            .map(|(k, v)| {
                let key = self
                    .canon(VocabKind::PropKey, k)
                    .map_err(|r| field_err(r, "props"))?;
                let parsed = values::parse(v).map_err(|r| field_err(r, &format!("props.{key}")))?;
                Ok((key, parsed))
            })
            .collect()
    }

    fn new_prop_key_warnings(&self, b: &mut Batch, keys: impl Iterator<Item = String>) {
        for k in keys {
            if !self.state.prop_keys.contains_key(&k)
                && !b.warnings.iter().any(|w| w["details"]["name"] == *k)
            {
                b.warn(
                    "NEW_VOCABULARY",
                    format!("new property key {k}"),
                    json!({ "kind": "prop_key", "name": k, "similar": [] }),
                );
            }
        }
    }

    /// [WRITE-FIELD-1] .. [WRITE-FIELD-3] combine incoming properties into a map.
    fn combine_props(
        &self,
        b: &mut Batch,
        id: &str,
        props: &mut PropMap,
        incoming: Vec<(String, Parsed)>,
        mode: OnConflict,
        force: bool,
    ) -> Result<(), OpError> {
        self.new_prop_key_warnings(b, incoming.iter().map(|(k, _)| k.clone()));
        for (k, p) in incoming {
            match p {
                Parsed::Remove => {
                    props.remove(&k);
                }
                Parsed::Value(v) => match props.get(&k) {
                    None => {
                        props.insert(k, v);
                    }
                    Some(old) if values_equal(old, &v) => {}
                    Some(old) => {
                        if force {
                            props.insert(k, v);
                        } else {
                            conflict_warning(
                                b,
                                id,
                                &format!("props.{k}"),
                                old.to_json(),
                                v.to_json(),
                                mode,
                            )?;
                            if mode == OnConflict::Overwrite {
                                props.insert(k, v);
                            }
                        }
                    }
                },
            }
        }
        if props.len() > 256 {
            return Err(OpError::new(
                "value.props.too_many",
                format!("{id}: more than 256 properties"),
            ));
        }
        if values::encode_props(props).is_some_and(|s| s.len() > 65_536) {
            return Err(OpError::new(
                "value.props.too_large",
                format!("{id}: properties exceed 65536 bytes"),
            ));
        }
        Ok(())
    }

    /// Alias rules of objects.md §3.
    pub(crate) fn add_aliases(
        node: &mut Node,
        aliases: impl IntoIterator<Item = String>,
    ) -> Result<(), OpError> {
        for a in aliases {
            let a =
                clean_text(&a, 512, "node.alias_too_long").map_err(|r| field_err(r, "aliases"))?;
            check_name(&a).map_err(|r| field_err(r, "aliases"))?;
            let k = name_key(&a);
            if name_key(&node.name) == k || node.aliases.iter().any(|x| name_key(x) == k) {
                continue;
            }
            node.aliases.push(a);
        }
        if node.aliases.len() > 64 {
            return Err(OpError::new(
                "node.too_many_aliases",
                "a node has at most 64 aliases",
            ));
        }
        Ok(())
    }

    fn add_labels(&self, node: &mut Node, labels: Vec<String>) -> Result<(), OpError> {
        for l in labels {
            let l = self
                .canon(VocabKind::NodeType, &l)
                .map_err(|r| field_err(r, "labels"))?;
            if !node.labels.contains(&l) {
                node.labels.push(l);
            }
        }
        if node.labels.len() > 32 {
            return Err(OpError::new(
                "node.too_many_labels",
                "a node has at most 32 labels",
            ));
        }
        Ok(())
    }

    fn key_field(o: &Obj) -> Result<Option<String>, OpError> {
        opt_str(o, "key")?
            .map(|k| validate_key(&k).map_err(|r| field_err(r, "key")))
            .transpose()
    }

    /// [OBJ-KEY-1] set a key on an object that has none.
    fn set_key(
        map: &HashMap<String, i64>,
        stored: &mut Option<String>,
        incoming: Option<String>,
        idx: i64,
    ) -> Result<(), OpError> {
        let Some(k) = incoming else { return Ok(()) };
        match stored {
            Some(s) if *s == k => Ok(()),
            Some(_) => Err(OpError::new(
                "key.immutable",
                "an existing key cannot be changed through the agent API",
            )),
            None => {
                if map.get(&k).is_some_and(|&other| other != idx) {
                    return Err(OpError::new(
                        "key.taken",
                        format!("key {k:?} is used by another object"),
                    ));
                }
                *stored = Some(k);
                Ok(())
            }
        }
    }

    fn fresh_meta() -> RecordMeta {
        RecordMeta::default() // stamped by `finish`
    }

    // ---- nodes --------------------------------------------------------------------------------

    fn write_node(
        &mut self,
        b: &mut Batch,
        ctx: &mut Ctx,
        o: &Obj,
    ) -> Result<(Outcome, ObjectKind, i64), OpError> {
        let key = Self::key_field(o)?;
        let type_ = opt_str(o, "type")?
            .map(|t| {
                self.canon(VocabKind::NodeType, &t)
                    .map_err(|r| field_err(r, "type"))
            })
            .transpose()?;
        let name = opt_text(o, "name", 512, "node.name_too_long")?;
        if let Some(n) = &name {
            check_name(n).map_err(|r| field_err(r, "name"))?;
        }
        let create_new = opt_bool(o, "create_new")?.unwrap_or(false);

        // [WRITE-MATCH-NODE-1]
        let mut by_identity = true;
        let existing = if let Some(id) = get(o, "id") {
            Some(self.resolve_ref(&ctx.refs, id, ObjectKind::Node)?)
        } else if let Some(k) = &key {
            self.state
                .node_keys
                .get(k)
                .and_then(|&i| self.state.resolve_node(i))
        } else if create_new {
            None
        } else {
            by_identity = false;
            let (Some(t), Some(n)) = (&type_, &name) else {
                return Err(missing(if type_.is_none() { "type" } else { "name" }));
            };
            match self.name_matches(t, n).as_slice() {
                [] => None,
                [one] => Some(*one),
                many => return Err(self.ambiguous(t, n, many)),
            }
        };

        let Some(idx) = existing else {
            let (Some(type_), Some(name)) = (type_, name) else {
                return Err(missing(if o.get("type").is_none() {
                    "type"
                } else {
                    "name"
                }));
            };
            if !self.state.node_types.contains_key(&type_) {
                b.warn(
                    "NEW_VOCABULARY",
                    format!("new node type {type_}"),
                    json!({ "kind": "node_type", "name": type_, "similar": [] }),
                );
            }
            let idx = b.next(ObjectKind::Node);
            let mut node = Node {
                idx,
                key: None,
                type_,
                name,
                aliases: Vec::new(),
                labels: Vec::new(),
                summary: opt_text(o, "summary", 4096, "node.summary_too_long")?,
                props: PropMap::new(),
                status: Status::Active,
                merged_into: None,
                meta: Self::fresh_meta(),
            };
            Self::set_key(&self.state.node_keys, &mut node.key, key, idx)?;
            Self::add_aliases(&mut node, str_list(o, "aliases")?)?;
            self.add_labels(&mut node, str_list(o, "labels")?)?;
            if let Some(p) = get(o, "props") {
                let parsed = self.parse_props(p)?;
                let id = id_string(ObjectKind::Node, idx);
                self.combine_props(b, &id, &mut node.props, parsed, ctx.on_conflict, false)?;
            }
            self.put(b, Row::Node(node));
            b.new_facts.push((TargetKind::Node, idx));
            return Ok((Outcome::Created, ObjectKind::Node, idx));
        };

        // [WRITE-MATCH-NODE-2] [WRITE-MATCH-NODE-3]
        let before = self.state.nodes[&idx].clone();
        if before.status == Status::Retracted {
            return Err(OpError::new(
                "lifecycle.retracted",
                format!(
                    "{} is retracted; restore it first",
                    id_string(ObjectKind::Node, idx)
                ),
            ));
        }
        if by_identity && type_.as_ref().is_some_and(|t| *t != before.type_) {
            return Err(OpError::new(
                "write.type_mismatch",
                format!(
                    "{} has type {}",
                    id_string(ObjectKind::Node, idx),
                    before.type_
                ),
            ));
        }
        let id = id_string(ObjectKind::Node, idx);
        let mut node = before.clone();
        Self::set_key(&self.state.node_keys, &mut node.key, key, idx)?;
        let mut aliases = Vec::new();
        if let Some(n) = name.filter(|n| *n != node.name) {
            aliases.push(n);
        }
        aliases.extend(str_list(o, "aliases")?);
        Self::add_aliases(&mut node, aliases)?;
        self.add_labels(&mut node, str_list(o, "labels")?)?;
        let summary = opt_text(o, "summary", 4096, "node.summary_too_long")?;
        combine(
            b,
            &id,
            "summary",
            &mut node.summary,
            summary,
            ctx.on_conflict,
            |s| json!(s),
        )?;
        if let Some(p) = get(o, "props") {
            let parsed = self.parse_props(p)?;
            self.combine_props(b, &id, &mut node.props, parsed, ctx.on_conflict, false)?;
        }
        Ok(self.put_if_changed(b, Row::Node(before), Row::Node(node)))
    }

    fn put_if_changed(
        &mut self,
        b: &mut Batch,
        before: Row,
        after: Row,
    ) -> (Outcome, ObjectKind, i64) {
        let (kind, idx) = (after.kind(), after.idx());
        if before == after {
            (Outcome::Matched, kind, idx)
        } else {
            self.put(b, after);
            (Outcome::Updated, kind, idx)
        }
    }

    // ---- links --------------------------------------------------------------------------------

    fn write_link(
        &mut self,
        b: &mut Batch,
        ctx: &mut Ctx,
        o: &Obj,
    ) -> Result<(Outcome, ObjectKind, i64), OpError> {
        let key = Self::key_field(o)?;
        let type_ = opt_str(o, "type")?
            .map(|t| {
                self.canon(VocabKind::LinkType, &t)
                    .map_err(|r| field_err(r, "type"))
            })
            .transpose()?;
        let from = get(o, "from")
            .map(|v| self.resolve_ref(&ctx.refs, v, ObjectKind::Node))
            .transpose()?;
        let to = get(o, "to")
            .map(|v| self.resolve_ref(&ctx.refs, v, ObjectKind::Node))
            .transpose()?;
        let multi = opt_bool(o, "multi")?.unwrap_or(false);
        let directed_in = opt_bool(o, "directed")?;
        let valid_from = temporal(o, "valid_from")?;
        let valid_to = temporal(o, "valid_to")?;

        // [WRITE-MATCH-LINK-2]
        let by_identity_ref = get(o, "id").is_some() || key.is_some();
        let existing = if let Some(id) = get(o, "id") {
            Some(self.resolve_ref(&ctx.refs, id, ObjectKind::Link)?)
        } else if let Some(k) = &key {
            self.state.link_keys.get(k).copied()
        } else if multi {
            None
        } else {
            let (Some(t), Some(f), Some(d)) = (&type_, from, to) else {
                return Err(missing(if type_.is_none() {
                    "type"
                } else if from.is_none() {
                    "from"
                } else {
                    "to"
                }));
            };
            let directed = directed_in.unwrap_or(!self.state.symmetric_link_types.contains(t));
            self.state
                .link_identity
                .get(&LinkIdentity::new(t, directed, f, d))
                .copied()
        };

        let Some(idx) = existing else {
            let (Some(type_), Some(src), Some(dst)) = (type_, from, to) else {
                return Err(missing(if o.get("type").is_none() {
                    "type"
                } else if o.get("from").is_none() {
                    "from"
                } else {
                    "to"
                }));
            };
            self.require_active_node(src)?;
            self.require_active_node(dst)?;
            check_validity(&valid_from, &valid_to)?;
            if !self.state.link_types.contains_key(&type_) {
                b.warn(
                    "NEW_VOCABULARY",
                    format!("new link type {type_}"),
                    json!({ "kind": "link_type", "name": type_, "similar": [] }),
                );
            }
            let directed = directed_in.unwrap_or(!self.state.symmetric_link_types.contains(&type_));
            let idx = b.next(ObjectKind::Link);
            let mut link = Link {
                idx,
                key: None,
                type_,
                src,
                dst,
                directed,
                multi,
                props: PropMap::new(),
                valid_from,
                valid_to,
                status: Status::Active,
                meta: Self::fresh_meta(),
            };
            Self::set_key(&self.state.link_keys, &mut link.key, key, idx)?;
            if let Some(p) = get(o, "props") {
                let parsed = self.parse_props(p)?;
                let id = id_string(ObjectKind::Link, idx);
                self.combine_props(b, &id, &mut link.props, parsed, ctx.on_conflict, false)?;
            }
            self.put(b, Row::Link(link));
            b.new_facts.push((TargetKind::Link, idx));
            return Ok((Outcome::Created, ObjectKind::Link, idx));
        };

        // [WRITE-MATCH-LINK-3] [WRITE-MATCH-LINK-4]
        let before = self.state.links[&idx].clone();
        let id = id_string(ObjectKind::Link, idx);
        if before.status == Status::Retracted {
            return Err(OpError::new(
                "lifecycle.retracted",
                format!("{id} is retracted; restore it first"),
            ));
        }
        if by_identity_ref {
            let same_ends = match (from, to) {
                (Some(f), Some(t)) => {
                    LinkIdentity::new(&before.type_, before.directed, f, t)
                        == LinkIdentity::new(&before.type_, before.directed, before.src, before.dst)
                }
                (None, None) => true,
                _ => false,
            };
            if type_.as_ref().is_some_and(|t| *t != before.type_)
                || directed_in.is_some_and(|d| d != before.directed)
                || !same_ends
            {
                return Err(OpError::new(
                    "write.link_mismatch",
                    format!("{id} has a different type, direction or endpoints"),
                ));
            }
        }
        let mut link = before.clone();
        Self::set_key(&self.state.link_keys, &mut link.key, key, idx)?;
        combine(
            b,
            &id,
            "valid_from",
            &mut link.valid_from,
            valid_from,
            ctx.on_conflict,
            Value::to_json,
        )?;
        combine(
            b,
            &id,
            "valid_to",
            &mut link.valid_to,
            valid_to,
            ctx.on_conflict,
            Value::to_json,
        )?;
        check_validity(&link.valid_from, &link.valid_to)?;
        if let Some(p) = get(o, "props") {
            let parsed = self.parse_props(p)?;
            self.combine_props(b, &id, &mut link.props, parsed, ctx.on_conflict, false)?;
        }
        Ok(self.put_if_changed(b, Row::Link(before), Row::Link(link)))
    }

    // ---- evidence -----------------------------------------------------------------------------

    /// Changeable evidence fields from an item; `None` entries were absent.
    fn evidence_fields(&self, o: &Obj, refs: &Refs) -> Result<EvidenceFields, OpError> {
        let mime = opt_text(o, "mime", 255, "evidence.field_too_long")?;
        if mime.as_ref().is_some_and(|m| {
            !m.split_once('/')
                .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
        }) {
            return Err(invalid("mime", "type/subtype"));
        }
        let language = opt_str(o, "language")?;
        if language.as_ref().is_some_and(|l| !valid_language(l)) {
            return Err(field_err(Reason("evidence.language_invalid"), "language"));
        }
        Ok(EvidenceFields {
            title: opt_text(o, "title", 1024, "evidence.field_too_long")?,
            mime,
            locator: json_object_field(o, "locator", 4096)?,
            source_date: temporal(o, "source_date")?,
            source_version: opt_text(o, "source_version", 256, "evidence.field_too_long")?,
            supersedes: get(o, "supersedes")
                .map(|v| self.resolve_ref(refs, v, ObjectKind::Evidence))
                .transpose()?,
            captured_at: datetime_ms(o, "captured_at")?,
            author: opt_text(o, "author", 512, "evidence.field_too_long")?,
            publisher: opt_text(o, "publisher", 512, "evidence.field_too_long")?,
            license: opt_text(o, "license", 512, "evidence.field_too_long")?,
            language,
            reliability: opt_unit(o, "reliability", "evidence.reliability_range")?,
            extra: json_object_field(o, "meta", 16_384)?,
        })
    }

    fn write_evidence(
        &mut self,
        b: &mut Batch,
        ctx: &mut Ctx,
        o: &Obj,
    ) -> Result<(Outcome, ObjectKind, i64), OpError> {
        if get(o, "blob").is_some() || get(o, "blob_base64").is_some() {
            return Err(OpError::new(
                "evidence.blob_unsupported",
                "blob upload is not implemented yet; use content or uri",
            ));
        }
        let key = Self::key_field(o)?;
        let kind = opt_str(o, "kind")?;
        if kind
            .as_ref()
            .is_some_and(|k| !EVIDENCE_KINDS.contains(&k.as_str()))
        {
            return Err(invalid(
                "kind",
                "one of text, url, file, image, document, conversation, observation, dataset, other",
            ));
        }
        let content = opt_str(o, "content")?;
        if let Some(c) = &content
            && c.len() > self.policies.inline_text_limit as usize
        {
            return Err(field_err(Reason("evidence.content_too_long"), "content"));
        }
        let content_sha256 = content.as_ref().map(|c| sha256_hex(c.as_bytes()));
        let uri = opt_str(o, "uri")?;
        if uri
            .as_ref()
            .is_some_and(|u| u.len() > 4096 || !valid_uri(u))
        {
            return Err(field_err(Reason("evidence.uri_invalid"), "uri"));
        }
        let parent = get(o, "parent")
            .map(|v| self.resolve_ref(&ctx.refs, v, ObjectKind::Evidence))
            .transpose()?;
        let fields = self.evidence_fields(o, &ctx.refs)?;

        // [WRITE-MATCH-EV-1]
        let explicit = get(o, "id").is_some() || key.is_some();
        let existing = if let Some(id) = get(o, "id") {
            Some(self.resolve_ref(&ctx.refs, id, ObjectKind::Evidence)?)
        } else if let Some(k) = &key {
            self.state.evidence_keys.get(k).copied()
        } else if let Some(kind) = &kind {
            let ident = EvidenceIdentity {
                kind: kind.clone(),
                uri: uri.clone(),
                blob: None,
                parent,
                content: content_sha256.clone(),
            };
            self.state
                .evidence_identity
                .get(&ident)
                .and_then(|s| s.iter().next().copied())
        } else {
            return Err(missing("kind"));
        };

        let Some(idx) = existing else {
            let kind = kind.ok_or_else(|| missing("kind"))?;
            if content.is_none() && uri.is_none() {
                return Err(OpError::new(
                    "evidence.empty",
                    "evidence needs content or uri",
                ));
            }
            let idx = b.next(ObjectKind::Evidence);
            let mut e = Evidence {
                idx,
                key: None,
                kind,
                title: fields.title,
                content,
                content_sha256,
                uri,
                blob_sha256: None,
                blob_size: None,
                mime: fields.mime,
                parent,
                locator: fields.locator,
                source_date: fields.source_date,
                source_version: fields.source_version,
                supersedes: fields.supersedes,
                captured_at: fields.captured_at.unwrap_or(b.ts),
                author: fields.author,
                publisher: fields.publisher,
                language: fields.language,
                license: fields.license,
                reliability: fields.reliability,
                extra: fields.extra,
                status: Status::Active,
                meta: Self::fresh_meta(),
            };
            Self::set_key(&self.state.evidence_keys, &mut e.key, key, idx)?;
            // The in-memory state drops content; the store needs it on insert.
            self.put(b, Row::Evidence(e.clone()));
            b_pending_content(b, idx, e.content);
            return Ok((Outcome::Created, ObjectKind::Evidence, idx));
        };

        // [WRITE-MATCH-EV-2]
        let before = self.state.evidence[&idx].clone();
        let id = id_string(ObjectKind::Evidence, idx);
        if before.status == Status::Retracted {
            return Err(OpError::new(
                "lifecycle.retracted",
                format!("{id} is retracted; restore it first"),
            ));
        }
        if explicit {
            let differs = kind.as_ref().is_some_and(|k| *k != before.kind)
                || content_sha256.is_some() && content_sha256 != before.content_sha256
                || uri.is_some() && uri != before.uri
                || parent.is_some() && parent != before.parent;
            if differs {
                return Err(OpError::new(
                    "evidence.immutable_field",
                    format!("{id}: kind, content, uri and parent cannot change"),
                ));
            }
        }
        let mut e = before.clone();
        Self::set_key(&self.state.evidence_keys, &mut e.key, key, idx)?;
        self.combine_evidence(b, &id, &mut e, fields, ctx.on_conflict)?;
        Ok(self.put_if_changed(b, Row::Evidence(before), Row::Evidence(e)))
    }

    fn combine_evidence(
        &self,
        b: &mut Batch,
        id: &str,
        e: &mut Evidence,
        f: EvidenceFields,
        mode: OnConflict,
    ) -> Result<(), OpError> {
        let s = |v: &String| json!(v);
        combine(b, id, "title", &mut e.title, f.title, mode, s)?;
        combine(b, id, "mime", &mut e.mime, f.mime, mode, s)?;
        combine(b, id, "locator", &mut e.locator, f.locator, mode, |m| {
            Json::Object(m.clone())
        })?;
        combine(
            b,
            id,
            "source_date",
            &mut e.source_date,
            f.source_date,
            mode,
            Value::to_json,
        )?;
        combine(
            b,
            id,
            "source_version",
            &mut e.source_version,
            f.source_version,
            mode,
            s,
        )?;
        combine(
            b,
            id,
            "supersedes",
            &mut e.supersedes,
            f.supersedes,
            mode,
            |i| json!(id_string(ObjectKind::Evidence, *i)),
        )?;
        let mut captured = Some(e.captured_at);
        combine(
            b,
            id,
            "captured_at",
            &mut captured,
            f.captured_at,
            mode,
            |ms| crate::model::time_json(*ms),
        )?;
        e.captured_at = captured.unwrap_or(e.captured_at);
        combine(b, id, "author", &mut e.author, f.author, mode, s)?;
        combine(b, id, "publisher", &mut e.publisher, f.publisher, mode, s)?;
        combine(b, id, "license", &mut e.license, f.license, mode, s)?;
        combine(b, id, "language", &mut e.language, f.language, mode, s)?;
        combine(
            b,
            id,
            "reliability",
            &mut e.reliability,
            f.reliability,
            mode,
            |x| json!(x),
        )?;
        combine(b, id, "meta", &mut e.extra, f.extra, mode, |m| {
            Json::Object(m.clone())
        })?;
        if e.supersedes == Some(e.idx) {
            return Err(OpError::new(
                "evidence.supersedes_cycle",
                format!("{id} cannot supersede itself"),
            ));
        }
        Ok(())
    }

    // ---- citations ----------------------------------------------------------------------------

    fn write_citation_item(
        &mut self,
        b: &mut Batch,
        ctx: &mut Ctx,
        o: &Obj,
    ) -> Result<(Outcome, ObjectKind, i64), OpError> {
        let target = as_obj(get(o, "target").ok_or_else(|| missing("target"))?, "target")?;
        let (tk, tv) = if let Some(v) = get(target, "node") {
            (TargetKind::Node, v)
        } else if let Some(v) = get(target, "link") {
            (TargetKind::Link, v)
        } else {
            return Err(invalid("target", "{\"node\": ref} or {\"link\": ref}"));
        };
        let tidx = self.resolve_ref(&ctx.refs, tv, tk.object_kind())?;
        let (outcome, idx) = self.write_cite(b, ctx, &Json::Object(o.clone()), tk, tidx)?;
        Ok((outcome, ObjectKind::Citation, idx))
    }

    /// [WRITE-MATCH-CIT-1] [WRITE-MATCH-CIT-2]: a citation of the given target.
    fn write_cite(
        &mut self,
        b: &mut Batch,
        ctx: &mut Ctx,
        c: &Json,
        tk: TargetKind,
        tidx: i64,
    ) -> Result<(Outcome, i64), OpError> {
        let (ev_ref, o) = match c {
            Json::Object(o) => (
                get(o, "evidence")
                    .ok_or_else(|| missing("evidence"))?
                    .clone(),
                Some(o),
            ),
            other => (other.clone(), None),
        };
        let empty = Obj::new();
        let o = o.unwrap_or(&empty);
        let ev = self.resolve_ref(&ctx.refs, &ev_ref, ObjectKind::Evidence)?;
        if self.state.evidence[&ev].status != Status::Active {
            return Err(OpError::new(
                "citation.evidence_not_active",
                format!("{} is not active", id_string(ObjectKind::Evidence, ev)),
            ));
        }
        let target_active = match tk {
            TargetKind::Node => self
                .state
                .nodes
                .get(&tidx)
                .is_some_and(|n| n.status == Status::Active),
            TargetKind::Link => self
                .state
                .links
                .get(&tidx)
                .is_some_and(|l| l.status == Status::Active),
        };
        if !target_active {
            return Err(OpError::new(
                "citation.target_not_active",
                format!("{} is not active", id_string(tk.object_kind(), tidx)),
            ));
        }
        let prop = opt_str(o, "prop")?
            .map(|p| {
                self.canon(VocabKind::PropKey, &p)
                    .map_err(|r| field_err(r, "prop"))
            })
            .transpose()?;
        let role = match opt_str(o, "role")? {
            None => Role::Supports,
            Some(r) => Role::parse(&r).ok_or_else(|| {
                invalid("role", "supports, contradicts, mentions or derived_from")
            })?,
        };
        let confidence = opt_unit(o, "confidence", "citation.confidence_range")?;
        let quote = opt_text(o, "quote", 1024, "citation.field_too_long")?;
        let note = opt_text(o, "note", 2048, "citation.field_too_long")?;

        let ident = CitationIdentity {
            evidence: ev,
            target_kind: tk,
            target: tidx,
            prop: prop.clone(),
            role,
        };
        if let Some(&idx) = self.state.citation_identity.get(&ident) {
            let before = self.state.citations[&idx].clone();
            let mut cit = before.clone();
            let id = id_string(ObjectKind::Citation, idx);
            combine(
                b,
                &id,
                "confidence",
                &mut cit.confidence,
                confidence,
                ctx.on_conflict,
                |x| json!(x),
            )?;
            combine(
                b,
                &id,
                "quote",
                &mut cit.quote,
                quote,
                ctx.on_conflict,
                |s| json!(s),
            )?;
            combine(b, &id, "note", &mut cit.note, note, ctx.on_conflict, |s| {
                json!(s)
            })?;
            let (outcome, _, _) = self.put_if_changed(b, Row::Citation(before), Row::Citation(cit));
            return Ok((outcome, idx));
        }
        let idx = b.next(ObjectKind::Citation);
        let cit = Citation {
            idx,
            evidence: ev,
            target_kind: tk,
            target: tidx,
            prop,
            role,
            confidence,
            quote,
            note,
            status: Status::Active,
            meta: Self::fresh_meta(),
        };
        self.put(b, Row::Citation(cit));
        Ok((Outcome::Created, idx))
    }

    // ---- updates ------------------------------------------------------------------------------

    fn write_update(
        &mut self,
        b: &mut Batch,
        ctx: &mut Ctx,
        o: &Obj,
    ) -> Result<(Outcome, ObjectKind, i64), OpError> {
        let target = as_obj(get(o, "target").ok_or_else(|| missing("target"))?, "target")?;
        let (kind, v) = ["node", "link", "evidence", "citation"]
            .into_iter()
            .find_map(|k| get(target, k).map(|v| (ObjectKind::from_name(k).expect("kind"), v)))
            .ok_or_else(|| {
                invalid(
                    "target",
                    "{\"node\"|\"link\"|\"evidence\"|\"citation\": ref}",
                )
            })?;
        let idx = self.resolve_ref(&ctx.refs, v, kind)?;
        let before = self.state.get(kind, idx).expect("resolved");
        let id = id_string(kind, idx);
        if before.status() != Status::Active {
            return Err(OpError::new(
                "lifecycle.retracted",
                format!("{id} is not active; restore it first"),
            ));
        }
        if let Some(Json::Number(n)) = get(o, "if_version") {
            let current = before.meta().version;
            if n.to_string() != current.to_string() {
                return Err(OpError::new(
                    "write.version_conflict",
                    format!("{id} is at version {current}"),
                )
                .details(json!({ "current_version": current })));
            }
        }
        let set_key = Self::opt_set_key(o)?;
        let mut after = before.clone();
        match &mut after {
            Row::Node(n) => {
                Self::set_key(&self.state.node_keys, &mut n.key, set_key, idx)?;
                if let Some(new_name) = opt_text(o, "rename", 512, "node.name_too_long")? {
                    check_name(&new_name).map_err(|r| field_err(r, "rename"))?;
                    let old = std::mem::replace(&mut n.name, new_name);
                    let new_key = name_key(&n.name);
                    n.aliases.retain(|a| name_key(a) != new_key);
                    if opt_bool(o, "keep_old_name_as_alias")?.unwrap_or(true) {
                        Self::add_aliases(n, [old])?;
                    }
                }
                if o.contains_key("summary") {
                    n.summary = opt_text(o, "summary", 4096, "node.summary_too_long")?;
                }
                Self::add_aliases(n, str_list(o, "add_aliases")?)?;
                let remove: Vec<String> = str_list(o, "remove_aliases")?
                    .iter()
                    .map(|a| name_key(a))
                    .collect();
                n.aliases.retain(|a| !remove.contains(&name_key(a)));
                let labels = str_list(o, "add_labels")?;
                self.add_labels(n, labels)?;
                let remove: Vec<String> = str_list(o, "remove_labels")?
                    .iter()
                    .map(|l| self.canon(VocabKind::NodeType, l).unwrap_or_default())
                    .collect();
                n.labels.retain(|l| !remove.contains(l));
                let changes = self.prop_changes(o)?;
                self.combine_props(b, &id, &mut n.props, changes, OnConflict::Overwrite, true)?;
            }
            Row::Link(l) => {
                Self::set_key(&self.state.link_keys, &mut l.key, set_key, idx)?;
                if o.contains_key("valid_from") {
                    l.valid_from = temporal(o, "valid_from")?;
                }
                if o.contains_key("valid_to") {
                    l.valid_to = temporal(o, "valid_to")?;
                }
                check_validity(&l.valid_from, &l.valid_to)?;
                let changes = self.prop_changes(o)?;
                self.combine_props(b, &id, &mut l.props, changes, OnConflict::Overwrite, true)?;
            }
            Row::Evidence(e) => {
                Self::set_key(&self.state.evidence_keys, &mut e.key, set_key, idx)?;
                for f in ["kind", "content", "uri", "parent", "blob"] {
                    if o.contains_key(f) {
                        return Err(OpError::new(
                            "evidence.immutable_field",
                            format!("{id}: {f} cannot change"),
                        ));
                    }
                }
                let f = self.evidence_fields(o, &ctx.refs)?;
                // Updates overwrite; explicit nulls remove.
                macro_rules! apply {
                    ($field:ident, $name:literal) => {
                        if o.contains_key($name) {
                            e.$field = f.$field;
                        }
                    };
                }
                apply!(title, "title");
                apply!(mime, "mime");
                apply!(locator, "locator");
                apply!(source_date, "source_date");
                apply!(source_version, "source_version");
                apply!(supersedes, "supersedes");
                apply!(author, "author");
                apply!(publisher, "publisher");
                apply!(license, "license");
                apply!(language, "language");
                apply!(reliability, "reliability");
                apply!(extra, "meta");
                if let Some(ms) = f.captured_at {
                    e.captured_at = ms;
                }
                if e.supersedes
                    .is_some_and(|s| self.supersedes_reaches(s, e.idx))
                {
                    return Err(OpError::new(
                        "evidence.supersedes_cycle",
                        format!("{id}: supersedes would form a cycle"),
                    ));
                }
            }
            Row::Citation(c) => {
                if o.contains_key("confidence") {
                    c.confidence = opt_unit(o, "confidence", "citation.confidence_range")?;
                }
                if o.contains_key("quote") {
                    c.quote = opt_text(o, "quote", 1024, "citation.field_too_long")?;
                }
                if o.contains_key("note") {
                    c.note = opt_text(o, "note", 2048, "citation.field_too_long")?;
                }
            }
        }
        Ok(self.put_if_changed(b, before, after))
    }

    fn opt_set_key(o: &Obj) -> Result<Option<String>, OpError> {
        opt_str(o, "set_key")?
            .map(|k| validate_key(&k).map_err(|r| field_err(r, "set_key")))
            .transpose()
    }

    /// `set` and `unset` of an update item.
    fn prop_changes(&self, o: &Obj) -> Result<Vec<(String, Parsed)>, OpError> {
        let mut changes = match get(o, "set") {
            Some(v) => self.parse_props(v)?,
            None => Vec::new(),
        };
        for k in str_list(o, "unset")? {
            let key = self
                .canon(VocabKind::PropKey, &k)
                .map_err(|r| field_err(r, "unset"))?;
            changes.push((key, Parsed::Remove));
        }
        Ok(changes)
    }

    fn supersedes_reaches(&self, mut from: i64, target: i64) -> bool {
        for _ in 0..10_000 {
            if from == target {
                return true;
            }
            match self.state.evidence.get(&from).and_then(|e| e.supersedes) {
                Some(next) => from = next,
                None => return false,
            }
        }
        true
    }

    /// Props of a node as JSON (used by responses and tests).
    pub fn node_props_json(&self, idx: i64) -> Option<Json> {
        self.state.nodes.get(&idx).map(|n| props_json(&n.props))
    }
}

/// Evidence content waiting to be written: the in-memory state does not keep content, so the
/// row put into the batch is re-read with its content by `finish` through the pending map.
fn b_pending_content(b: &mut Batch, idx: i64, content: Option<String>) {
    if let Some(c) = content {
        b.pending_content.insert(idx, c);
    }
}

struct EvidenceFields {
    title: Option<String>,
    mime: Option<String>,
    locator: Option<Map<String, Json>>,
    source_date: Option<Value>,
    source_version: Option<String>,
    supersedes: Option<i64>,
    captured_at: Option<i64>,
    author: Option<String>,
    publisher: Option<String>,
    license: Option<String>,
    language: Option<String>,
    reliability: Option<f64>,
    extra: Option<Map<String, Json>>,
}

fn missing(field: &str) -> OpError {
    OpError::new(
        "write.missing_field",
        format!("missing required field {field:?}"),
    )
    .details(json!({ "field": field }))
}

/// [OBJ-LINK-3]
pub(crate) fn check_validity(from: &Option<Value>, to: &Option<Value>) -> Result<(), OpError> {
    if let (Some(f), Some(t)) = (from, to) {
        let (start, _) = temporal_bounds(f).expect("temporal");
        let (_, end) = temporal_bounds(t).expect("temporal");
        if start > end {
            return Err(OpError::new(
                "link.invalid_validity",
                "valid_from is later than valid_to",
            ));
        }
    }
    Ok(())
}
