//! Operation `overview` — spec/api/operations/overview.md.

use crate::engine::{Graph, OpError};
use crate::ids::ObjectKind;
use crate::model::{Row, Status, id_string, record_json, time_json};
use crate::read::BRIEF;
use crate::store::Store;
use crate::values::Sym;
use crate::vocab::VocabKind;
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// [OP-OVER-5]
const SAMPLE: usize = 10_000;

const CONVENTIONS: &str = "Node types are PascalCase, link types UPPER_SNAKE_CASE, property keys \
lower_snake_case; reuse the types listed here. Before creating nodes, call resolve (many names at \
once) or search. Record each source with one write: the source as evidence, then the nodes and \
links that cite it. Cite an observation evidence for your own conclusions. Retract instead of \
deleting; retracted objects can be restored.";

fn bad(field: &str, expected: &str) -> OpError {
    OpError::new(
        "overview.invalid_field",
        format!("{field}: expected {expected}"),
    )
}

/// `items` sorted by count descending, then name ([OP-OVER-2]).
fn by_count<'a>(items: impl Iterator<Item = (&'a str, usize)>) -> Vec<(&'a str, usize)> {
    let mut v: Vec<_> = items.collect();
    v.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    v
}

impl<S: Store> Graph<S> {
    /// Numbers of active nodes, links, evidence and citations ([OP-OVER-2]).
    pub fn counts(&self) -> [(&'static str, usize); 4] {
        let st = &self.state;
        [
            ("nodes", st.nodes_by_type.values().map(|s| s.len()).sum()),
            ("links", st.links_by_type.values().map(|s| s.len()).sum()),
            ("evidence", st.active_evidence_kinds.values().sum()),
            ("citations", st.active_citations),
        ]
    }

    /// Operation `overview` ([OP-OVER-1] .. [OP-OVER-5]).
    pub fn overview(&self, request: &Json) -> Result<Json, OpError> {
        let empty = Map::new();
        let req = match request {
            Json::Null => &empty,
            other => other
                .as_object()
                .ok_or_else(|| bad("request", "an object"))?,
        };
        for k in req.keys() {
            if !["type", "recent", "types_limit"].contains(&k.as_str()) {
                return Err(bad(k, "no such field"));
            }
        }
        let int = |key: &str, range: std::ops::RangeInclusive<u64>, default: u64| {
            req.get(key).map_or(Ok(default as usize), |v| {
                v.as_u64()
                    .filter(|n| range.contains(n))
                    .map(|n| n as usize)
                    .ok_or_else(|| {
                        bad(
                            key,
                            &format!("an integer from {} to {}", range.start(), range.end()),
                        )
                    })
            })
        };
        let recent = int("recent", 0..=50, 5)?;
        let types_limit = int("types_limit", 1..=1000, 100)?;
        let st = &self.state;
        let notes = self.store_query(self.store.vocabulary_notes())?;
        let description = |kind: i64, name: &str| {
            notes
                .iter()
                .find(|n| n.kind == kind && n.name == name)
                .and_then(|n| n.description.clone())
        };

        let mut out = Map::new();
        out.insert("graph".into(), self.name().into());
        out.insert("spec_version".into(), crate::SPEC_VERSION.into());
        let counts: Map<String, Json> = self
            .counts()
            .into_iter()
            .map(|(k, n)| (k.to_owned(), n.into()))
            .collect();
        out.insert("counts".into(), Json::Object(counts));
        let node_types = by_count(st.nodes_by_type.iter().map(|(t, s)| (&**t, s.len())));
        let link_types = by_count(st.links_by_type.iter().map(|(t, s)| (&**t, s.len())));
        let truncated = node_types.len() > types_limit || link_types.len() > types_limit;
        let type_entry = |kind: i64, (t, n): &(&str, usize)| {
            let mut m = Map::new();
            m.insert("type".into(), (*t).into());
            m.insert("count".into(), (*n).into());
            if let Some(d) = description(kind, t) {
                m.insert("description".into(), d.into());
            }
            if kind == 1 && st.symmetric_link_types.contains(*t) {
                m.insert("symmetric".into(), true.into());
            }
            Json::Object(m)
        };
        out.insert(
            "node_types".into(),
            node_types
                .iter()
                .take(types_limit)
                .map(|e| type_entry(0, e))
                .collect(),
        );
        out.insert(
            "link_types".into(),
            link_types
                .iter()
                .take(types_limit)
                .map(|e| type_entry(1, e))
                .collect(),
        );
        out.insert("types_truncated".into(), truncated.into());
        let kinds: BTreeMap<&str, usize> = st
            .active_evidence_kinds
            .iter()
            .map(|(k, n)| (k.as_str(), *n))
            .collect();
        out.insert("evidence_kinds".into(), json!(kinds));
        out.insert("policies".into(), self.policies.to_json());
        out.insert("search".into(), self.search_settings());
        out.insert("last_seq".into(), self.counters.seq.into());
        let batches = if recent == 0 {
            Vec::new()
        } else {
            self.store_query(self.store.recent_batches(recent))?
        };
        out.insert(
            "recent".into(),
            batches
                .into_iter()
                .map(|b| {
                    json!({
                        "batch": id_string(ObjectKind::Batch, b.idx), "at": time_json(b.created_at),
                        "actor": st.actor_name(b.actor), "op": b.op, "note": b.note,
                        "changes": b.changes,
                    })
                })
                .collect(),
        );
        out.insert("conventions".into(), CONVENTIONS.into());

        if let Some(t) = req.get("type") {
            let t = t.as_str().ok_or_else(|| bad("type", "a string"))?;
            // The name may be a node type or a link type, so a spelling one kind refuses is
            // tried as the other; a miss names the styled spellings that do exist.
            let as_node = self.vocab_name(VocabKind::NodeType, t).ok();
            let as_link = self.vocab_name(VocabKind::LinkType, t).ok();
            let near: Vec<String> = [VocabKind::NodeType, VocabKind::LinkType]
                .into_iter()
                .filter_map(|k| crate::vocab::canonicalize(k, t).ok())
                .filter(|c| c != t)
                .filter(|c| {
                    st.nodes_by_type
                        .get(c.as_str())
                        .is_some_and(|s| !s.is_empty())
                        || st
                            .links_by_type
                            .get(c.as_str())
                            .is_some_and(|s| !s.is_empty())
                })
                .collect();
            let detail = if let Some((name, set)) = as_node
                .as_deref()
                .and_then(|n| st.nodes_by_type.get_key_value(n))
            {
                self.node_type_detail(name, set.as_slice(), description(0, name))
            } else if let Some((name, set)) = as_link
                .as_deref()
                .and_then(|n| st.links_by_type.get_key_value(n))
            {
                self.link_type_detail(name, set.as_slice(), description(1, name))
            } else {
                let hint = if near.is_empty() {
                    String::new()
                } else {
                    format!("; did you mean {}?", near.join(" or "))
                };
                return Err(OpError::new(
                    "ref.not_found",
                    format!("no active node or link has type {t}{hint}"),
                ));
            };
            out.insert("type_detail".into(), Json::Object(detail));
        }
        Ok(Json::Object(out))
    }

    pub(crate) fn brief(&self, kind: ObjectKind, idx: i64) -> Json {
        let row: Row = self.state.get(kind, idx).expect("active object");
        let keep = BRIEF
            .iter()
            .find(|(k, _)| *k == kind.name())
            .map_or(&[][..], |(_, f)| f);
        let mut m = record_json(&row, &|a| self.state.actor_name(a));
        m.retain(|k, _| keep.contains(&k.as_str()));
        Json::Object(m)
    }

    fn type_header(
        kind: &str,
        name: &Sym,
        count: usize,
        sample: usize,
        description: Option<String>,
    ) -> Map<String, Json> {
        let mut m = Map::new();
        m.insert("kind".into(), kind.into());
        m.insert("type".into(), (**name).into());
        m.insert("count".into(), count.into());
        if let Some(d) = description {
            m.insert("description".into(), d.into());
        }
        m.insert("sample".into(), sample.into());
        m
    }

    fn node_type_detail(
        &self,
        name: &Sym,
        all: &[i64],
        description: Option<String>,
    ) -> Map<String, Json> {
        let st = &self.state;
        let sample = sample_of(all);
        let mut m = Self::type_header("node", name, all.len(), sample.len(), description);
        m.insert(
            "props".into(),
            props_json(sample.iter().map(|i| &st.nodes[i].props), sample.len()),
        );
        // (link type, direction) -> (count, other end type -> count)
        type Ends<'a> = HashMap<(&'a str, &'static str), (usize, HashMap<&'a str, usize>)>;
        let mut ends: Ends = HashMap::new();
        for &n in &sample {
            for l in st.adjacency.get(&n).into_iter().flatten() {
                let link = &st.links[l];
                if link.status != Status::Active {
                    continue;
                }
                let sides: &[(&'static str, i64)] = if !link.directed {
                    &[("both", if link.src == n { link.dst } else { link.src })]
                } else if link.src == link.dst {
                    &[("out", n), ("in", n)]
                } else if link.src == n {
                    &[("out", link.dst)]
                } else {
                    &[("in", link.src)]
                };
                for &(dir, other) in sides {
                    let e = ends.entry((&link.type_, dir)).or_default();
                    e.0 += 1;
                    *e.1.entry(&st.nodes[&other].type_).or_default() += 1;
                }
            }
        }
        let mut groups: Vec<_> = ends.into_iter().collect();
        groups.sort_unstable_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
        m.insert(
            "links".into(),
            groups
                .into_iter()
                .take(20)
                .map(|((t, dir), (count, others))| {
                    let others: Vec<Json> = by_count(others.into_iter())
                        .into_iter()
                        .take(5)
                        .map(|(t, c)| json!({"type": t, "count": c}))
                        .collect();
                    json!({"type": t, "direction": dir, "count": count, "other_types": others})
                })
                .collect(),
        );
        m.insert(
            "examples".into(),
            sample
                .iter()
                .take(5)
                .map(|&i| self.brief(ObjectKind::Node, i))
                .collect(),
        );
        m
    }

    fn link_type_detail(
        &self,
        name: &Sym,
        all: &[i64],
        description: Option<String>,
    ) -> Map<String, Json> {
        let st = &self.state;
        let sample = sample_of(all);
        let mut m = Self::type_header("link", name, all.len(), sample.len(), description);
        m.insert(
            "symmetric".into(),
            st.symmetric_link_types.contains(&**name).into(),
        );
        m.insert(
            "props".into(),
            props_json(sample.iter().map(|i| &st.links[i].props), sample.len()),
        );
        let mut pairs: HashMap<(&str, &str), usize> = HashMap::new();
        for l in &sample {
            let link = &st.links[l];
            let (a, b) = if link.directed || link.src <= link.dst {
                (link.src, link.dst)
            } else {
                (link.dst, link.src)
            };
            *pairs
                .entry((&st.nodes[&a].type_, &st.nodes[&b].type_))
                .or_default() += 1;
        }
        let mut pairs: Vec<_> = pairs.into_iter().collect();
        pairs.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        m.insert(
            "endpoints".into(),
            pairs
                .into_iter()
                .take(20)
                .map(|((f, t), c)| json!({"from": f, "to": t, "count": c}))
                .collect(),
        );
        m.insert(
            "examples".into(),
            sample
                .iter()
                .take(5)
                .map(|&i| self.brief(ObjectKind::Link, i))
                .collect(),
        );
        m
    }
}

/// Property keys with value kinds and fill rates ([OP-OVER-5]).
fn props_json<'a>(maps: impl Iterator<Item = &'a crate::values::PropMap>, sample: usize) -> Json {
    let mut keys: HashMap<&str, (usize, BTreeSet<String>)> = HashMap::new();
    for p in maps {
        for (k, v) in p.iter() {
            let e = keys.entry(k).or_default();
            e.0 += 1;
            e.1.insert(v.kind_name());
        }
    }
    let mut keys: Vec<_> = keys.into_iter().collect();
    keys.sort_unstable_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
    keys.into_iter()
        .map(|(k, (n, kinds))| {
            let fill = (n as f64 / sample.max(1) as f64 * 100.0).round() / 100.0;
            json!({"key": k, "kinds": kinds, "fill": fill})
        })
        .collect()
}

/// [OP-OVER-5] At most `SAMPLE` objects at equal steps through `all` (in internal-number order):
/// positions floor(i · n / SAMPLE).
fn sample_of(all: &[i64]) -> Vec<i64> {
    let n = all.len();
    if n <= SAMPLE {
        return all.to_vec();
    }
    (0..SAMPLE).map(|i| all[i * n / SAMPLE]).collect()
}
