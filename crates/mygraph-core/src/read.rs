//! Operation `get` — spec/api/operations/get.md.

use crate::engine::{Graph, OpError, storage_error};
use crate::ids::{ObjectId, ObjectKind};
use crate::model::{Role, Row, Status, TargetKind, id_string, record_json};
use crate::store::Store;
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, HashSet};

fn bad(field: &str, expected: &str) -> OpError {
    OpError::new("get.invalid_field", format!("{field}: expected {expected}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Detail {
    Brief,
    Normal,
    Full,
}

struct Include {
    links: Option<(usize, Option<HashSet<String>>)>,
    support: bool,
    evidence: Option<usize>,
    /// (from, max_chars) in code points.
    content: Option<(usize, Option<usize>)>,
    retracted: bool,
}

/// `limit` of an include object, within `0..=max`.
fn include_limit(
    o: &Map<String, Json>,
    field: &str,
    default: usize,
    max: u64,
) -> Result<usize, OpError> {
    match o.get("limit") {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| *n <= max)
            .map(|n| n as usize)
            .ok_or_else(|| bad(field, &format!("limit from 0 to {max}"))),
    }
}

pub(crate) const BRIEF: [(&str, &[&str]); 4] = [
    ("node", &["id", "type", "name", "status"]),
    ("link", &["id", "type", "from", "to", "directed", "status"]),
    ("evidence", &["id", "kind", "title", "uri", "status"]),
    ("citation", &["id", "evidence", "target", "role", "status"]),
];
const RECORD_TIME: [&str; 5] = [
    "version",
    "created_at",
    "created_by",
    "updated_at",
    "updated_by",
];

impl<S: Store> Graph<S> {
    /// Operation `get` ([OP-GET-1] .. [OP-GET-7]).
    pub fn get(&self, request: &Json) -> Result<Json, OpError> {
        let req = request
            .as_object()
            .ok_or_else(|| bad("request", "an object"))?;
        let refs = req
            .get("refs")
            .and_then(Json::as_array)
            .filter(|a| (1..=100).contains(&a.len()))
            .ok_or_else(|| bad("refs", "a list of 1 to 100 references"))?;
        let detail = match req.get("detail").map(Json::as_str) {
            None | Some(Some("normal")) => Detail::Normal,
            Some(Some("brief")) => Detail::Brief,
            Some(Some("full")) => Detail::Full,
            _ => return Err(bad("detail", "brief, normal or full")),
        };
        let inc = self.parse_include(req, detail)?;
        let items = refs
            .iter()
            .map(|r| match self.resolve_any(r) {
                Ok((kind, idx, merged_from)) => {
                    let mut item = self.item_json(kind, idx, detail, &inc)?;
                    if let Some(m) = merged_from {
                        item.insert("merged_from".into(), m.into());
                    }
                    Ok(Json::Object(item))
                }
                Err(e) => Ok(
                    json!({ "ref": r, "error": { "code": e.reason.code(), "message": e.message } }),
                ),
            })
            .collect::<Result<Vec<_>, OpError>>()?;
        Ok(json!({ "items": items }))
    }

    fn parse_include(&self, req: &Map<String, Json>, detail: Detail) -> Result<Include, OpError> {
        let on = detail != Detail::Brief;
        let inc = match req.get("include") {
            None => Map::new(),
            Some(Json::Object(m)) => m.clone(),
            Some(_) => return Err(bad("include", "an object")),
        };
        let links = match inc.get("links") {
            None if on => Some((10, None)),
            None | Some(Json::Bool(false)) => None,
            Some(Json::Bool(true)) => Some((10, None)),
            Some(Json::Object(o)) => {
                let types = match o.get("types") {
                    None | Some(Json::Null) => None,
                    Some(Json::Array(a)) => Some(
                        a.iter()
                            .map(|t| {
                                t.as_str()
                                    .and_then(|t| {
                                        self.canon(crate::vocab::VocabKind::LinkType, t).ok()
                                    })
                                    .ok_or_else(|| bad("include.links.types", "link type names"))
                            })
                            .collect::<Result<HashSet<_>, _>>()?,
                    ),
                    Some(_) => return Err(bad("include.links.types", "a list of link types")),
                };
                Some((include_limit(o, "include.links", 10, 500)?, types))
            }
            Some(_) => return Err(bad("include.links", "true, false or an object")),
        };
        let support = match inc.get("support") {
            None => on,
            Some(v) => v
                .as_bool()
                .ok_or_else(|| bad("include.support", "true or false"))?,
        };
        let evidence = match inc.get("evidence") {
            None if on => Some(3),
            None | Some(Json::Bool(false)) => None,
            Some(Json::Bool(true)) => Some(3),
            Some(Json::Object(o)) => Some(include_limit(o, "include.evidence", 3, 100)?),
            Some(_) => return Err(bad("include.evidence", "true, false or an object")),
        };
        let content = match inc.get("content") {
            None | Some(Json::Bool(false)) => None,
            Some(Json::Bool(true)) => Some((0, None)),
            Some(Json::Object(o)) => {
                let n = |k: &str| -> Result<Option<usize>, OpError> {
                    o.get(k)
                        .map(|v| {
                            v.as_u64().map(|n| n as usize).ok_or_else(|| {
                                bad("include.content", "from and max_chars as integers")
                            })
                        })
                        .transpose()
                };
                Some((n("from")?.unwrap_or(0), n("max_chars")?))
            }
            Some(_) => return Err(bad("include.content", "true, false or an object")),
        };
        let retracted = match req.get("include_retracted") {
            None => false,
            Some(v) => v
                .as_bool()
                .ok_or_else(|| bad("include_retracted", "true or false"))?,
        };
        Ok(Include {
            links,
            support,
            evidence,
            content,
            retracted,
        })
    }

    /// Any reference: (kind, number, merged-from ID).
    pub(crate) fn resolve_any(
        &self,
        r: &Json,
    ) -> Result<(ObjectKind, i64, Option<String>), OpError> {
        let not_found = || OpError::new("ref.not_found", format!("nothing matches {r}"));
        match r {
            Json::String(s) => {
                let id = ObjectId::decode(s).map_err(|_| not_found())?;
                let idx = id.number as i64;
                let st = &self.state;
                let exists = match id.kind {
                    ObjectKind::Node => st.nodes.contains_key(&idx),
                    ObjectKind::Link => st.links.contains_key(&idx),
                    ObjectKind::Evidence => st.evidence.contains_key(&idx),
                    ObjectKind::Citation => st.citations.contains_key(&idx),
                    ObjectKind::Batch => false,
                };
                if !exists {
                    return Err(not_found());
                }
                if id.kind == ObjectKind::Node {
                    let to = st.resolve_node(idx).ok_or_else(not_found)?;
                    return Ok((id.kind, to, (to != idx).then(|| s.clone())));
                }
                Ok((id.kind, idx, None))
            }
            Json::Object(o) if o.contains_key("key") => {
                let kind = match o.get("kind").map(Json::as_str) {
                    None | Some(Some("node")) => ObjectKind::Node,
                    Some(Some("link")) => ObjectKind::Link,
                    Some(Some("evidence")) => ObjectKind::Evidence,
                    _ => return Err(bad("refs", "kind node, link or evidence")),
                };
                let key = o.get("key").and_then(Json::as_str).ok_or_else(not_found)?;
                let keys = match kind {
                    ObjectKind::Node => &self.state.node_keys,
                    ObjectKind::Link => &self.state.link_keys,
                    _ => &self.state.evidence_keys,
                };
                let idx = *keys.get(&crate::text::nfc(key)).ok_or_else(not_found)?;
                Ok((kind, idx, None))
            }
            Json::Object(_) => {
                let idx = self.resolve_ref(&Default::default(), r, ObjectKind::Node)?;
                Ok((ObjectKind::Node, idx, None))
            }
            _ => Err(not_found()),
        }
    }

    fn item_json(
        &self,
        kind: ObjectKind,
        idx: i64,
        detail: Detail,
        inc: &Include,
    ) -> Result<Map<String, Json>, OpError> {
        let st = &self.state;
        let row = st.get(kind, idx).expect("resolved reference");
        let mut m = record_json(&row, &|a| st.actor_name(a));
        let kind_name = kind.name();
        match detail {
            Detail::Brief => {
                let keep = BRIEF
                    .iter()
                    .find(|(k, _)| *k == kind_name)
                    .map_or(&[][..], |(_, f)| f);
                m.retain(|k, _| keep.contains(&k.as_str()));
            }
            Detail::Normal => m.retain(|k, _| !RECORD_TIME.contains(&k.as_str())),
            Detail::Full => {}
        }
        let target = match kind {
            ObjectKind::Node => Some(TargetKind::Node),
            ObjectKind::Link => Some(TargetKind::Link),
            _ => None,
        };
        if let Some(t) = target {
            if inc.support {
                m.insert("support".into(), self.support_json(t, idx));
            }
            if let Some(limit) = inc.evidence {
                m.insert(
                    "evidence".into(),
                    self.cited_by_json(t, idx, limit, inc.retracted),
                );
            }
        }
        // [OP-GET-3] who a link connects, readable without another call.
        if let (Row::Link(l), false) = (&row, matches!(detail, Detail::Brief)) {
            let end = |n: i64| {
                let node = &st.nodes[&n];
                json!({ "id": id_string(ObjectKind::Node, n), "type": &*node.type_, "name": node.name })
            };
            m.insert(
                "ends".into(),
                json!({ "from": end(l.src), "to": end(l.dst) }),
            );
        }
        if let (ObjectKind::Node, Some((limit, types))) = (kind, &inc.links) {
            m.insert(
                "links".into(),
                self.neighbourhood_json(idx, *limit, types.as_ref(), inc.retracted),
            );
        }
        if let (Row::Evidence(_), Some((from, max))) = (&row, inc.content) {
            let content = self.store.evidence_content(idx).map_err(storage_error)?;
            match content {
                None => {
                    m.insert("content".into(), Json::Null);
                }
                Some(text) => {
                    let total = text.chars().count();
                    let take = max.unwrap_or(usize::MAX);
                    let part: String = text.chars().skip(from).take(take).collect();
                    let shown = part.chars().count();
                    m.insert("content".into(), part.into());
                    m.insert("content_length".into(), total.into());
                    m.insert(
                        "content_truncated".into(),
                        (from.saturating_add(shown) < total).into(),
                    );
                }
            }
        }
        Ok(m)
    }

    /// [OP-GET-5]
    fn support_json(&self, t: TargetKind, idx: i64) -> Json {
        let st = &self.state;
        let mut by_role: BTreeMap<&str, u64> = BTreeMap::new();
        let mut sources = HashSet::new();
        for c in st.cites_of(t, idx).into_iter().flatten() {
            let c = &st.citations[c];
            if c.status == Status::Active
                && st
                    .evidence
                    .get(&c.evidence)
                    .is_some_and(|e| e.status == Status::Active)
            {
                *by_role.entry(c.role.name()).or_default() += 1;
                sources.insert(c.evidence);
            }
        }
        let mut m: Map<String, Json> = [
            Role::Supports,
            Role::Contradicts,
            Role::Mentions,
            Role::DerivedFrom,
        ]
        .iter()
        .filter_map(|r| {
            by_role
                .get(r.name())
                .map(|n| (r.name().to_owned(), json!(n)))
        })
        .collect();
        m.insert("sources".into(), sources.len().into());
        Json::Object(m)
    }

    /// [OP-GET-6]
    fn cited_by_json(&self, t: TargetKind, idx: i64, limit: usize, retracted: bool) -> Json {
        let st = &self.state;
        let cites: Vec<_> = st
            .cites_of(t, idx)
            .into_iter()
            .flatten()
            .map(|c| &st.citations[c])
            .filter(|c| retracted || c.status == Status::Active)
            .collect();
        let items: Vec<Json> = cites
            .iter()
            .take(limit)
            .map(|c| {
                let e = &st.evidence[&c.evidence];
                let mut j = json!({
                    "id": id_string(ObjectKind::Evidence, e.idx), "kind": &e.kind,
                    "title": &e.title, "uri": &e.uri, "role": c.role.name(),
                    "citation": id_string(ObjectKind::Citation, c.idx),
                });
                if let Some(p) = &c.prop {
                    j["prop"] = p.clone().into();
                }
                if let Some(q) = &c.quote {
                    j["quote"] = q.clone().into();
                }
                if c.status != Status::Active {
                    j["status"] = c.status.name().into();
                }
                j
            })
            .collect();
        json!({ "items": items, "truncated": cites.len() > limit })
    }

    /// [OP-GET-4]
    fn neighbourhood_json(
        &self,
        idx: i64,
        limit: usize,
        types: Option<&HashSet<String>>,
        retracted: bool,
    ) -> Json {
        let st = &self.state;
        let mut links: Vec<(&str, &'static str, i64, i64)> = st
            .adjacency
            .get(&idx)
            .into_iter()
            .flatten()
            .map(|l| &st.links[l])
            .filter(|l| retracted || l.status == Status::Active)
            .filter(|l| types.is_none_or(|t| t.contains(&*l.type_)))
            .map(|l| {
                let (dir, other) = if !l.directed {
                    ("both", if l.src == idx { l.dst } else { l.src })
                } else if l.src == idx {
                    ("out", l.dst)
                } else {
                    ("in", l.src)
                };
                (&*l.type_, dir, l.idx, other)
            })
            .collect();
        links.sort_unstable();
        let mut counts: Vec<Json> = Vec::new();
        for (t, d, _, _) in &links {
            match counts.last_mut() {
                Some(c) if c["type"] == *t && c["direction"] == *d => {
                    c["count"] = json!(c["count"].as_u64().unwrap_or(0) + 1);
                }
                _ => counts.push(json!({ "type": t, "direction": d, "count": 1 })),
            }
        }
        let items: Vec<Json> = links
            .iter()
            .take(limit)
            .map(|(t, d, l, other)| {
                let o = &st.nodes[other];
                let mut j = json!({
                    "id": id_string(ObjectKind::Link, *l), "type": t, "direction": d,
                    "other": { "id": id_string(ObjectKind::Node, *other), "type": &*o.type_, "name": &o.name },
                });
                let status = st.links[l].status;
                if status != Status::Active {
                    j["status"] = status.name().into();
                }
                j
            })
            .collect();
        json!({ "counts": counts, "items": items, "truncated": links.len() > limit })
    }
}
