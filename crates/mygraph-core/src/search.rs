//! Operation `search` — spec/api/operations/search.md, spec/algorithms/search-ranking.md.

use crate::engine::{Graph, OpError, storage_error};
use crate::ids::ObjectKind;
use crate::model::{Status, id_string};
use crate::name_key::name_key;
use crate::store::{Store, TextTarget};
use crate::text::nfc;
use crate::ucd::{is_letter, is_mark, is_number};
use crate::vocab::VocabKind;
use serde_json::{Map, Value as Json, json};
use std::collections::{HashMap, HashSet};

const PREFIX_SCAN: usize = 1000;
const FUZZY_THRESHOLD: f64 = 0.3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Exact,
    Prefix,
    Fuzzy,
    Text,
}

fn bad(field: &str, expected: &str) -> OpError {
    OpError::new(
        "search.invalid_field",
        format!("{field}: expected {expected}"),
    )
}

/// [RANK-6] full-text tokens.
pub fn text_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if is_letter(c) || is_mark(c) || is_number(c) {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Kind order of [RANK-7].
fn kind_rank(k: ObjectKind) -> u8 {
    match k {
        ObjectKind::Node => 0,
        ObjectKind::Link => 1,
        _ => 2,
    }
}

struct Hit {
    score: f64,
    matched: &'static str,
    snippet: Option<String>,
}

impl<S: Store> Graph<S> {
    /// Operation `search` ([OP-SEARCH-1] .. [OP-SEARCH-4]).
    pub fn search(&self, request: &Json) -> Result<Json, OpError> {
        let req = request
            .as_object()
            .ok_or_else(|| bad("request", "an object"))?;
        let query = req
            .get("query")
            .and_then(Json::as_str)
            .map(nfc)
            .filter(|q| !q.is_empty() && q.len() <= 512)
            .ok_or_else(|| OpError::new("search.query_invalid", "query: 1 to 512 bytes of text"))?;
        let mut sections = HashSet::new();
        match req.get("in") {
            None => {
                sections.insert("nodes");
            }
            Some(Json::Array(a)) if !a.is_empty() => {
                for v in a {
                    match v.as_str() {
                        Some(s @ ("nodes" | "links" | "evidence")) => {
                            sections.insert(s);
                        }
                        _ => return Err(bad("in", "a list of nodes, links, evidence")),
                    }
                }
            }
            _ => return Err(bad("in", "a list of nodes, links, evidence")),
        }
        let mode = match req.get("mode").map(|m| m.as_str()) {
            None | Some(Some("auto")) => Mode::Auto,
            Some(Some("exact")) => Mode::Exact,
            Some(Some("prefix")) => Mode::Prefix,
            Some(Some("fuzzy")) => Mode::Fuzzy,
            Some(Some("text")) => Mode::Text,
            _ => return Err(bad("mode", "auto, exact, prefix, fuzzy or text")),
        };
        let limit = match req.get("limit") {
            None => 20,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=500).contains(n))
                .ok_or_else(|| bad("limit", "an integer from 1 to 500"))?
                as usize,
        };
        let include_retracted = match req.get("include_retracted") {
            None => false,
            Some(v) => v
                .as_bool()
                .ok_or_else(|| bad("include_retracted", "true or false"))?,
        };
        let types: Option<HashSet<String>> = match req.get("types") {
            None => None,
            Some(Json::Array(a)) => {
                let mut set = HashSet::new();
                for v in a {
                    let s = v
                        .as_str()
                        .ok_or_else(|| bad("types", "a list of type names"))?;
                    // A name may be a node type or a link type; accept either spelling.
                    for kind in [VocabKind::NodeType, VocabKind::LinkType] {
                        if let Ok(c) = self.canon(kind, s) {
                            set.insert(c);
                        }
                    }
                }
                Some(set)
            }
            Some(_) => return Err(bad("types", "a list of type names")),
        };
        let type_ok = |t: &str| types.as_ref().is_none_or(|s| s.contains(t));
        let status_ok =
            |s: Status| s == Status::Active || include_retracted && s == Status::Retracted;

        let mut hits: HashMap<(ObjectKind, i64), Hit> = HashMap::new();
        let offer = |hits: &mut HashMap<(ObjectKind, i64), Hit>,
                     kind: ObjectKind,
                     idx: i64,
                     score: f64,
                     matched: &'static str,
                     snippet: Option<String>| {
            let h = hits.entry((kind, idx)).or_insert(Hit {
                score: -1.0,
                matched,
                snippet: None,
            });
            if score > h.score {
                h.score = score;
                h.matched = matched;
            }
            if snippet.is_some() {
                h.snippet = snippet;
            }
        };

        let q = name_key(&query);
        let qlen = q.chars().count();
        let text = matches!(mode, Mode::Auto | Mode::Fuzzy | Mode::Text);
        let tokens = if text {
            text_tokens(&query)
        } else {
            Vec::new()
        };
        let fetch = (limit * 5).min(2500);

        if sections.contains("nodes") {
            let st = &self.state;
            let node_ok = |n: &i64| {
                st.nodes
                    .get(n)
                    .is_some_and(|n| status_ok(n.status) && type_ok(&n.type_))
            };
            // Whether `key` is the node's own name (otherwise one of its aliases).
            let is_name = |n: i64, key: &str| {
                let node = &st.nodes[&n];
                node.aliases.is_empty() || name_key(&node.name) == key
            };
            if matches!(mode, Mode::Auto | Mode::Exact | Mode::Prefix)
                && let Some(set) = st.names.get(&q)
            {
                for &n in set.iter().filter(|n| node_ok(n)) {
                    if is_name(n, &q) {
                        offer(&mut hits, ObjectKind::Node, n, 1.0, "name:exact", None);
                    } else {
                        offer(&mut hits, ObjectKind::Node, n, 0.95, "alias:exact", None);
                    }
                }
            }
            if matches!(mode, Mode::Auto | Mode::Prefix) {
                for (key, set) in st.names.with_prefix(&q).take(PREFIX_SCAN) {
                    let ratio = qlen as f64 / key.chars().count() as f64;
                    for &n in set.iter().filter(|n| node_ok(n)) {
                        if is_name(n, key) {
                            offer(
                                &mut hits,
                                ObjectKind::Node,
                                n,
                                0.8 + 0.1 * ratio,
                                "name:prefix",
                                None,
                            );
                        } else {
                            offer(
                                &mut hits,
                                ObjectKind::Node,
                                n,
                                0.75 + 0.1 * ratio,
                                "alias:prefix",
                                None,
                            );
                        }
                    }
                }
            }
            if matches!(mode, Mode::Auto | Mode::Fuzzy) && qlen >= 3 {
                // Only the best `limit` hits are returned: take keys from the most similar down,
                // and stop once more than `limit` nodes are found, finishing the current
                // similarity so ties are still broken by [RANK-7].
                let mut keys = st.names.fuzzy(&q, FUZZY_THRESHOLD);
                keys.sort_by(|a, b| b.1.total_cmp(&a.1));
                let mut found = 0usize;
                let mut stop_below = None;
                for (key, sim) in keys {
                    if stop_below.is_some_and(|s| sim < s) {
                        break;
                    }
                    let score = 0.4 + 0.3 * sim;
                    for &n in st
                        .names
                        .get(key)
                        .into_iter()
                        .flatten()
                        .filter(|n| node_ok(n))
                    {
                        let m = if is_name(n, key) {
                            "name:fuzzy"
                        } else {
                            "alias:fuzzy"
                        };
                        offer(&mut hits, ObjectKind::Node, n, score, m, None);
                        found += 1;
                    }
                    if found > limit && stop_below.is_none() {
                        stop_below = Some(sim);
                    }
                }
            }
            // Text scores (at most 0.4) never outrank name matches (at least 0.49): with more
            // name hits than the limit, full text cannot change the answer.
            let text = text && !(mode == Mode::Auto && hits.len() > limit);
            if text {
                let found = self.full_text(TextTarget::Nodes, &tokens, fetch)?;
                let n = found.len() as f64;
                for (i, h) in found.into_iter().enumerate() {
                    if node_ok(&h.idx) {
                        offer(
                            &mut hits,
                            ObjectKind::Node,
                            h.idx,
                            0.1 + 0.3 * (n - i as f64) / n,
                            "text",
                            None,
                        );
                    }
                }
            }
        }
        // [OP-SEARCH-2] links and evidence: full text only.
        if text && !(mode == Mode::Auto && hits.len() > limit) {
            for (section, target, kind) in [
                ("links", TextTarget::Links, ObjectKind::Link),
                ("evidence", TextTarget::Evidence, ObjectKind::Evidence),
            ] {
                if !sections.contains(section) {
                    continue;
                }
                let found = self.full_text(target, &tokens, fetch)?;
                let n = found.len() as f64;
                for (i, h) in found.into_iter().enumerate() {
                    let ok = match kind {
                        ObjectKind::Link => self
                            .state
                            .links
                            .get(&h.idx)
                            .is_some_and(|l| status_ok(l.status) && type_ok(&l.type_)),
                        _ => self
                            .state
                            .evidence
                            .get(&h.idx)
                            .is_some_and(|e| status_ok(e.status)),
                    };
                    if ok {
                        offer(
                            &mut hits,
                            kind,
                            h.idx,
                            0.1 + 0.3 * (n - i as f64) / n,
                            "text",
                            h.snippet,
                        );
                    }
                }
            }
        }

        // [RANK-7]
        let mut ranked: Vec<((ObjectKind, i64), Hit)> = hits.into_iter().collect();
        ranked.sort_by(|(ka, a), (kb, b)| {
            b.score
                .total_cmp(&a.score)
                .then(kind_rank(ka.0).cmp(&kind_rank(kb.0)))
                .then(ka.1.cmp(&kb.1))
        });
        let truncated = ranked.len() > limit;
        let out: Vec<Json> = ranked
            .into_iter()
            .take(limit)
            .map(|((kind, idx), h)| self.hit_json(kind, idx, h))
            .collect();
        Ok(json!({ "hits": out, "truncated": truncated }))
    }

    fn hit_json(&self, kind: ObjectKind, idx: i64, h: Hit) -> Json {
        let mut m = Map::new();
        let status = match kind {
            ObjectKind::Node => {
                let n = &self.state.nodes[&idx];
                m.insert("kind".into(), "node".into());
                m.insert("id".into(), id_string(kind, idx).into());
                m.insert("type".into(), (*n.type_).into());
                m.insert("name".into(), n.name.clone().into());
                n.status
            }
            ObjectKind::Link => {
                let l = &self.state.links[&idx];
                m.insert("kind".into(), "link".into());
                m.insert("id".into(), id_string(kind, idx).into());
                m.insert("type".into(), (*l.type_).into());
                m.insert("from".into(), id_string(ObjectKind::Node, l.src).into());
                m.insert("to".into(), id_string(ObjectKind::Node, l.dst).into());
                l.status
            }
            _ => {
                let e = &self.state.evidence[&idx];
                m.insert("kind".into(), "evidence".into());
                m.insert("id".into(), id_string(kind, idx).into());
                m.insert("evidence_kind".into(), e.kind.clone().into());
                if let Some(t) = &e.title {
                    m.insert("title".into(), t.clone().into());
                }
                e.status
            }
        };
        m.insert("score".into(), json!((h.score * 1000.0).round() / 1000.0));
        m.insert("matched".into(), h.matched.into());
        if let Some(s) = h.snippet {
            m.insert("snippet".into(), s.into());
        }
        if status != Status::Active {
            m.insert("status".into(), status.name().into());
        }
        Json::Object(m)
    }
}

/// [OP-SEARCH-4] about 12 words around the first matching word, matches marked with « ».
pub(crate) fn snippet(text: &str, tokens: &[String]) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let keys: Vec<String> = tokens.iter().map(|t| name_key(t)).collect();
    let hit = |w: &str| {
        let k = name_key(&text_tokens(w).concat());
        !k.is_empty()
            && keys
                .iter()
                .enumerate()
                .any(|(i, t)| k == *t || (i + 1 == keys.len() && k.starts_with(t.as_str())))
    };
    let first = words.iter().position(|w| hit(w)).unwrap_or(0);
    let start = first.saturating_sub(4);
    let end = (start + 12).min(words.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    for (i, w) in words[start..end].iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if hit(w) {
            // Keep punctuation outside the marks: «Engine»'s
            let core = text_tokens(w).concat();
            match w.find(core.chars().next().unwrap_or(' ')) {
                Some(at) if !core.is_empty() && w[at..].starts_with(&core) => {
                    out.push_str(&w[..at]);
                    out.push('«');
                    out.push_str(&core);
                    out.push('»');
                    out.push_str(&w[at + core.len()..]);
                }
                _ => {
                    out.push('«');
                    out.push_str(w);
                    out.push('»');
                }
            }
        } else {
            out.push_str(w);
        }
    }
    if end < words.len() {
        out.push('…');
    }
    out
}

impl<S: Store> Graph<S> {
    /// Full text from the attached index, or from the store's own tables.
    fn full_text(
        &self,
        target: TextTarget,
        tokens: &[String],
        limit: usize,
    ) -> Result<Vec<crate::store::TextHit>, OpError> {
        let Some(index) = self.text_index() else {
            return self
                .store
                .text_search(target, tokens, limit)
                .map_err(storage_error);
        };
        let mut hits = index.search(target, tokens, limit).map_err(storage_error)?;
        if target == TextTarget::Evidence {
            for h in hits.iter_mut().filter(|h| h.snippet.is_none()) {
                let e = self.state.evidence.get(&h.idx);
                let content = self.store.evidence_content(h.idx).map_err(storage_error)?;
                let text = match (e.and_then(|e| e.title.as_deref()), content) {
                    (Some(t), Some(c)) => format!("{t} {c}"),
                    (Some(t), None) => t.to_owned(),
                    (None, c) => c.unwrap_or_default(),
                };
                h.snippet = Some(snippet(&text, tokens));
            }
        }
        Ok(hits)
    }
}
