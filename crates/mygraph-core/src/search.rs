//! Operation `search` — spec/api/operations/search.md, spec/algorithms/search-ranking.md.

use crate::engine::{Graph, OpError, storage_error};
use crate::ids::ObjectKind;
use crate::model::{Status, id_string};
use crate::name_key::name_key;
use crate::store::{Closeness, Store, TextTarget, passages};
use crate::text::nfc;
use crate::ucd::{is_letter, is_mark, is_number};
use crate::vocab::VocabKind;
use serde_json::{Map, Value as Json, json};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

const PREFIX_SCAN: usize = 1000;
const FUZZY_THRESHOLD: f64 = 0.3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Exact,
    Prefix,
    Fuzzy,
    Text,
    Semantic,
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

/// [RANK-9] reciprocal rank fusion: each item scores the sum of 1 / (60 + rank) over the lists
/// it is in (ranks from 1); higher first, ties by the best rank, then by the earliest list with
/// that rank, then by position there. Items repeated within a list count once, at their first
/// place.
pub fn fuse<K: Copy + Eq + Hash>(lists: &[&[K]]) -> Vec<K> {
    // item -> (score, best rank, list of the best rank)
    let mut acc: HashMap<K, (f64, usize, usize)> = HashMap::new();
    for (l, list) in lists.iter().enumerate() {
        let mut seen = HashSet::new();
        for &k in list.iter() {
            if !seen.insert(k) {
                continue;
            }
            let rank = seen.len();
            let e = acc.entry(k).or_insert((0.0, usize::MAX, l));
            e.0 += 1.0 / (60.0 + rank as f64);
            if rank < e.1 {
                e.1 = rank;
                e.2 = l;
            }
        }
    }
    let mut out: Vec<(K, (f64, usize, usize))> = acc.into_iter().collect();
    out.sort_by(|(_, a), (_, b)| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    out.into_iter().map(|(k, _)| k).collect()
}

/// [OP-SEARCH-4] for a hit found by meaning only: the start of its closest passage.
fn passage_snippet(text: &str, passage: usize) -> String {
    let all = passages(text);
    let Some(p) = all.get(passage).or(all.first()) else {
        return String::new();
    };
    let words: Vec<&str> = p.split_whitespace().collect();
    let mut out = String::new();
    if passage > 0 {
        out.push('…');
    }
    out.push_str(&words[..words.len().min(12)].join(" "));
    if words.len() > 12 || passage + 1 < all.len() {
        out.push('…');
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
            Some(Some("semantic")) => Mode::Semantic,
            _ => return Err(bad("mode", "auto, exact, prefix, fuzzy, text or semantic")),
        };
        // [OP-SEARCH-5]
        let semantic = match (mode, self.semantic_index()) {
            (Mode::Auto | Mode::Semantic, Some(s)) if s.ready() => Some(s),
            (Mode::Semantic, index) => {
                let why = if index.is_some() {
                    "the graph's semantic index is still being built"
                } else {
                    "this server has no semantic index for the graph"
                };
                return Err(OpError::new(
                    "search.semantic_unavailable",
                    format!("mode semantic: {why}"),
                ));
            }
            _ => None,
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
                    // A name may be a node type or a link type: it must be acceptable as one of
                    // them; when neither accepts it, the node type's error says why.
                    let as_node = self.vocab_name(VocabKind::NodeType, s);
                    let as_link = self.vocab_name(VocabKind::LinkType, s);
                    match (as_node, as_link) {
                        (Err(e), Err(_)) => return Err(e.at("types")),
                        (n, l) => set.extend(n.into_iter().chain(l)),
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
        let meaning = semantic.is_some();
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
            // name hits than the limit, full text and meaning cannot change the answer.
            if (text || meaning) && !(mode == Mode::Auto && hits.len() > limit) {
                let found = self.text_tier(
                    TextTarget::Nodes,
                    &query,
                    if text { Some(&tokens) } else { None },
                    semantic,
                    fetch,
                )?;
                let n = found.len() as f64;
                for (i, (idx, matched, _)) in found.into_iter().enumerate() {
                    if node_ok(&idx) {
                        let score = 0.1 + 0.3 * (n - i as f64) / n;
                        offer(&mut hits, ObjectKind::Node, idx, score, matched, None);
                    }
                }
            }
        }
        // [OP-SEARCH-2] links: full text only; evidence: full text and meaning.
        if (text || meaning) && !(mode == Mode::Auto && hits.len() > limit) {
            for (section, target, kind) in [
                ("links", TextTarget::Links, ObjectKind::Link),
                ("evidence", TextTarget::Evidence, ObjectKind::Evidence),
            ] {
                if !sections.contains(section) {
                    continue;
                }
                let semantic = semantic.filter(|_| kind == ObjectKind::Evidence);
                let found = self.text_tier(
                    target,
                    &query,
                    if text { Some(&tokens) } else { None },
                    semantic,
                    fetch,
                )?;
                let n = found.len() as f64;
                for (i, (idx, matched, snippet)) in found.into_iter().enumerate() {
                    let ok = match kind {
                        ObjectKind::Link => self
                            .state
                            .links
                            .get(&idx)
                            .is_some_and(|l| status_ok(l.status) && type_ok(&l.type_)),
                        _ => self
                            .state
                            .evidence
                            .get(&idx)
                            .is_some_and(|e| status_ok(e.status)),
                    };
                    if ok {
                        let score = 0.1 + 0.3 * (n - i as f64) / n;
                        offer(&mut hits, kind, idx, score, matched, snippet);
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

/// One hit of the text tier: internal number, `matched`, snippet (evidence only).
type TierHit = (i64, &'static str, Option<String>);

impl<S: Store> Graph<S> {
    /// [RANK-9] the text tier of one section: full-text hits (when `tokens` are given) and hits
    /// by meaning (when `semantic` is given), fused, best first.
    fn text_tier(
        &self,
        target: TextTarget,
        query: &str,
        tokens: Option<&[String]>,
        semantic: Option<&dyn crate::store::SemanticIndex>,
        fetch: usize,
    ) -> Result<Vec<TierHit>, OpError> {
        let words = match tokens {
            Some(t) => self.full_text(target, t, fetch)?,
            None => Vec::new(),
        };
        let close = match semantic {
            Some(s) => s
                .nearest(target, query, fetch, Closeness::Related)
                .map_err(storage_error)?
                .unwrap_or_default(),
            None => Vec::new(),
        };
        if close.is_empty() {
            return Ok(words
                .into_iter()
                .map(|h| (h.idx, "text", h.snippet))
                .collect());
        }
        let word_ids: Vec<i64> = words.iter().map(|h| h.idx).collect();
        let close_ids: Vec<i64> = close.iter().map(|h| h.idx).collect();
        let mut snippets: HashMap<i64, Option<String>> =
            words.into_iter().map(|h| (h.idx, h.snippet)).collect();
        let passage: HashMap<i64, usize> = close.iter().map(|h| (h.idx, h.passage)).collect();
        let mut out = Vec::new();
        for idx in fuse(&[&word_ids, &close_ids]) {
            match snippets.remove(&idx) {
                Some(snippet) => out.push((idx, "text", snippet)),
                None => {
                    let snippet = match target {
                        TextTarget::Evidence => {
                            let text = self.evidence_meaning(idx)?;
                            Some(passage_snippet(&text, passage[&idx]))
                        }
                        _ => None,
                    };
                    out.push((idx, "semantic", snippet));
                }
            }
        }
        // Word matches before meaning-only hits; each keeps its fused order.
        out.sort_by_key(|(_, matched, _)| *matched == "semantic");
        Ok(out)
    }

    /// An evidence's text as embedded ([`crate::store::TextDoc::meaning`]).
    fn evidence_meaning(&self, idx: i64) -> Result<String, OpError> {
        let content = self.store.evidence_content(idx).map_err(storage_error)?;
        let doc = self
            .state
            .get(ObjectKind::Evidence, idx)
            .and_then(|row| crate::store::text_doc(&row, content.as_deref()));
        Ok(doc.map(|d| d.meaning().to_owned()).unwrap_or_default())
    }

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
