//! Operation `stats` — spec/api/operations/stats.md, and the admin-only `actors` section of
//! spec/api/admin.md [ADMIN-GRAPH-9]. Everything is computed from the in-memory indexes; only
//! `actors` asks the store (batches per actor).

use crate::engine::{Graph, OpError};
use crate::ids::ObjectKind;
use crate::model::{RecordMeta, Status, time_json};
use crate::store::Store;
use serde_json::{Map, Value as Json, json};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, PoisonError};

const DAY_MS: i64 = 86_400_000;
const KINDS: [&str; 4] = ["nodes", "links", "evidence", "citations"];

/// A section of the response ([OP-STATS-2], [ADMIN-GRAPH-9]). `Storage` is filled in by the
/// server, which knows the files; the engine computes the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Counts,
    Degree,
    Growth,
    Evidence,
    Actors,
    Storage,
}

impl Section {
    pub const AGENT: [Section; 4] = [Self::Counts, Self::Degree, Self::Growth, Self::Evidence];
    pub const ADMIN: [Section; 6] = [
        Self::Counts,
        Self::Degree,
        Self::Growth,
        Self::Evidence,
        Self::Actors,
        Self::Storage,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Counts => "counts",
            Self::Degree => "degree",
            Self::Growth => "growth",
            Self::Evidence => "evidence",
            Self::Actors => "actors",
            Self::Storage => "storage",
        }
    }
}

/// Sections computed at one `(last_seq, UTC day)`, by section and the parameter it depends on.
/// A section only changes with the graph (a new `last_seq`) or, for `growth`, the day, so a
/// cached one equals a fresh one; repeated calls between writes cost nothing.
#[derive(Debug, Default)]
pub struct StatsCache(Mutex<Option<CacheState>>);

type CacheState = ((i64, i64), HashMap<(Section, i64), Json>);

impl StatsCache {
    fn get(&self, at: (i64, i64), key: (Section, i64)) -> Option<Json> {
        let c = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        c.as_ref()
            .filter(|(a, _)| *a == at)
            .and_then(|(_, m)| m.get(&key).cloned())
    }

    fn put(&self, at: (i64, i64), key: (Section, i64), v: &Json) {
        let mut c = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let state = c.get_or_insert_with(|| (at, HashMap::new()));
        if state.0 != at || state.1.len() >= 64 {
            *state = (at, HashMap::new());
        }
        state.1.insert(key, v.clone());
    }
}

/// A validated `stats` request ([OP-STATS-1]).
#[derive(Debug, Clone, PartialEq)]
pub struct StatsRequest {
    pub sections: Vec<Section>,
    pub days: i64,
    pub top: usize,
}

impl StatsRequest {
    pub fn wants(&self, s: Section) -> bool {
        self.sections.contains(&s)
    }

    /// Parses a request; `allowed` is [`Section::AGENT`] for agents and [`Section::ADMIN`] for
    /// the admin API, and is also the default list.
    pub fn parse(request: &Json, allowed: &[Section]) -> Result<Self, OpError> {
        let bad = |field: &str, expected: &str| {
            OpError::new(
                "stats.invalid_field",
                format!("{field}: expected {expected}"),
            )
        };
        let empty = Map::new();
        let req = match request {
            Json::Null => &empty,
            other => other
                .as_object()
                .ok_or_else(|| bad("request", "an object"))?,
        };
        for k in req.keys() {
            if !["sections", "days", "top"].contains(&k.as_str()) {
                return Err(bad(k, "no such field"));
            }
        }
        let names: Vec<&str> = allowed.iter().map(|s| s.name()).collect();
        let expected = format!(
            "a non-empty list of distinct names from {}",
            names.join(", ")
        );
        let sections = match req.get("sections") {
            None => allowed.to_vec(),
            Some(v) => {
                let list = v
                    .as_array()
                    .filter(|l| !l.is_empty())
                    .ok_or_else(|| bad("sections", &expected))?;
                let mut out = Vec::new();
                for item in list {
                    let s = item
                        .as_str()
                        .and_then(|n| allowed.iter().find(|s| s.name() == n))
                        .filter(|s| !out.contains(*s))
                        .ok_or_else(|| bad("sections", &expected))?;
                    out.push(*s);
                }
                out
            }
        };
        let int = |key: &str, lo: u64, hi: u64, default: u64| {
            req.get(key).map_or(Ok(default), |v| {
                v.as_u64()
                    .filter(|n| (lo..=hi).contains(n))
                    .ok_or_else(|| bad(key, &format!("an integer from {lo} to {hi}")))
            })
        };
        Ok(Self {
            sections,
            days: int("days", 1, 366, 30)? as i64,
            top: int("top", 1, 100, 10)? as usize,
        })
    }
}

/// `YYYY-MM-DD` of a UTC day number.
fn day_string(day: i64) -> String {
    let t = time_json(day * DAY_MS);
    t.as_str().unwrap_or_default()[..10].to_owned()
}

fn kinds_json(n: [usize; 4]) -> Json {
    Json::Object(
        KINDS
            .iter()
            .zip(n)
            .map(|(k, n)| ((*k).to_owned(), n.into()))
            .collect(),
    )
}

/// The `top` entries of `items` by count descending, then key ascending.
fn top_by_count<K: Ord + Copy>(
    items: impl Iterator<Item = (K, usize)>,
    top: usize,
) -> Vec<(K, usize)> {
    let mut v: Vec<_> = items.collect();
    v.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(top);
    v
}

/// `s` without the ASCII-case-insensitive `prefix`.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len())
        .filter(|p| p.eq_ignore_ascii_case(prefix))
        .map(|_| &s[prefix.len()..])
}

/// [OP-STATS-6] Host of an http(s) URI: lower case, no user info, no port, no leading `www.`.
/// Allocates only when the host has upper-case letters.
pub fn uri_domain(uri: &str) -> Option<Cow<'_, str>> {
    let rest = strip_prefix_ci(uri, "https://").or_else(|| strip_prefix_ci(uri, "http://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    let host = if host.starts_with('[') {
        host.split_inclusive(']').next().unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or_default()
    };
    let host = strip_prefix_ci(host, "www.").unwrap_or(host);
    if host.is_empty() {
        None
    } else if host.bytes().any(|b| b.is_ascii_uppercase()) {
        Some(Cow::Owned(host.to_ascii_lowercase()))
    } else {
        Some(Cow::Borrowed(host))
    }
}

impl<S: Store> Graph<S> {
    /// Operation `stats` ([OP-STATS-1] .. [OP-STATS-6]).
    pub fn stats(&self, request: &Json, now_ms: i64) -> Result<Json, OpError> {
        let req = StatsRequest::parse(request, &Section::AGENT)?;
        self.compute_stats(&req, now_ms)
    }

    /// Every requested section except `storage` ([ADMIN-GRAPH-9]).
    pub fn compute_stats(&self, req: &StatsRequest, now_ms: i64) -> Result<Json, OpError> {
        let mut out = Map::new();
        out.insert("graph".into(), self.name().into());
        out.insert("last_seq".into(), self.counters.seq.into());
        out.insert("computed_at".into(), time_json(now_ms));
        let at = (self.counters.seq, now_ms.div_euclid(DAY_MS));
        for &s in &req.sections {
            let key = match s {
                Section::Growth => (s, req.days),
                Section::Actors | Section::Storage => (s, 0),
                _ => (s, req.top as i64),
            };
            if let Some(v) = self.stats_cache.get(at, key) {
                out.insert(s.name().into(), v);
                continue;
            }
            let v = match s {
                Section::Counts => self.counts_section(req.top),
                Section::Degree => self.degree_section(req.top),
                Section::Growth => self.growth_section(req.days, now_ms),
                Section::Evidence => self.evidence_section(req.top),
                Section::Actors => self.actors_section()?,
                Section::Storage => continue,
            };
            self.stats_cache.put(at, key, &v);
            out.insert(s.name().into(), v);
        }
        Ok(Json::Object(out))
    }

    /// Calls `f(kind, meta, status)` for every row, kind being the index in [`KINDS`].
    fn each_meta(&self, mut f: impl FnMut(usize, &RecordMeta, Status)) {
        let st = &self.state;
        st.nodes.values().for_each(|r| f(0, &r.meta, r.status));
        st.links.values().for_each(|r| f(1, &r.meta, r.status));
        st.evidence.values().for_each(|r| f(2, &r.meta, r.status));
        st.citations.values().for_each(|r| f(3, &r.meta, r.status));
    }

    /// [OP-STATS-3]
    fn counts_section(&self, top: usize) -> Json {
        let st = &self.state;
        let active = self.counts().map(|(_, n)| n);
        let merged = st
            .nodes
            .values()
            .filter(|n| n.status == Status::Merged)
            .count();
        let totals = [
            st.nodes.len(),
            st.links.len(),
            st.evidence.len(),
            st.citations.len(),
        ];
        let mut retracted = [0; 4];
        for i in 0..4 {
            retracted[i] = totals[i] - active[i] - if i == 0 { merged } else { 0 };
        }
        let types = |m: &HashMap<crate::values::Sym, crate::table::IdSet>| -> Json {
            top_by_count(
                m.iter()
                    .filter(|(_, s)| !s.is_empty())
                    .map(|(t, s)| (&**t, s.len())),
                top,
            )
            .into_iter()
            .map(|(t, n)| json!({"type": t, "count": n}))
            .collect()
        };
        let kinds: BTreeMap<&str, usize> = st
            .active_evidence_kinds
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(k, n)| (k.as_str(), *n))
            .collect();
        json!({
            "active": kinds_json(active), "retracted": kinds_json(retracted),
            "merged": {"nodes": merged}, "actors": st.actors.len(),
            "node_types": st.nodes_by_type.values().filter(|s| !s.is_empty()).count(),
            "link_types": st.links_by_type.values().filter(|s| !s.is_empty()).count(),
            "top_node_types": types(&st.nodes_by_type), "top_link_types": types(&st.links_by_type),
            "evidence_kinds": kinds,
        })
    }

    /// [OP-STATS-4]
    fn degree_section(&self, top: usize) -> Json {
        let st = &self.state;
        // One sequential pass over the links; a self-loop adds 2 to its node.
        let mut deg = vec![0u32; st.nodes.end()];
        for l in st.links.values().filter(|l| l.status == Status::Active) {
            for end in [l.src, l.dst] {
                if let Some(d) = deg.get_mut(end as usize) {
                    *d += 1;
                }
            }
        }
        let degrees: Vec<(u32, i64)> = st
            .nodes
            .iter()
            .filter(|(_, n)| n.status == Status::Active)
            .map(|(i, _)| (deg[i as usize], i))
            .collect();
        let n = degrees.len();
        if n == 0 {
            return json!({"nodes": 0, "min": 0, "max": 0, "mean": 0.0, "p50": 0, "p90": 0,
                          "p99": 0, "orphans": 0, "hubs": []});
        }
        let order = |a: &(u32, i64), b: &(u32, i64)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
        let mut hubs: Vec<(u32, i64)> = degrees.iter().copied().filter(|d| d.0 > 0).collect();
        if hubs.len() > top {
            hubs.select_nth_unstable_by(top - 1, order);
            hubs.truncate(top);
        }
        hubs.sort_unstable_by(order);
        let mut sorted: Vec<u32> = degrees.iter().map(|d| d.0).collect();
        sorted.sort_unstable();
        let total: u64 = sorted.iter().map(|&d| u64::from(d)).sum();
        let pct = |x: usize| sorted[(x * n).div_ceil(100) - 1];
        let mean = (total as f64 / n as f64 * 100.0).round() / 100.0;
        json!({
            "nodes": n, "min": sorted[0], "max": sorted[n - 1], "mean": mean,
            "p50": pct(50), "p90": pct(90), "p99": pct(99),
            "orphans": sorted.partition_point(|&d| d == 0),
            "hubs": hubs.into_iter().map(|(d, i)| {
                let mut b = self.brief(ObjectKind::Node, i);
                b["degree"] = d.into();
                b
            }).collect::<Vec<_>>(),
        })
    }

    /// [OP-STATS-5]
    fn growth_section(&self, days: i64, now_ms: i64) -> Json {
        let to = now_ms.div_euclid(DAY_MS);
        let from = to - days + 1;
        // Per day of the window: (created, retracted) per kind.
        let mut per_day = vec![([0usize; 4], [0usize; 4]); days as usize];
        let slot = |ms: i64| {
            let d = ms.div_euclid(DAY_MS) - from;
            (0..days).contains(&d).then_some(d as usize)
        };
        self.each_meta(|k, meta, status| {
            if let Some(d) = slot(meta.created_at) {
                per_day[d].0[k] += 1;
            }
            if status == Status::Retracted
                && let Some(d) = slot(meta.updated_at)
            {
                per_day[d].1[k] += 1;
            }
        });
        let (mut created, mut retracted) = ([0; 4], [0; 4]);
        let entries: Vec<Json> = per_day
            .into_iter()
            .enumerate()
            .filter(|(_, (c, r))| c.iter().chain(r).any(|&n| n > 0))
            .map(|(d, (c, r))| {
                for k in 0..4 {
                    created[k] += c[k];
                    retracted[k] += r[k];
                }
                json!({"day": day_string(from + d as i64), "created": kinds_json(c),
                       "retracted": kinds_json(r)})
            })
            .collect();
        json!({
            "days": days, "from": day_string(from), "to": day_string(to), "per_day": entries,
            "totals": {"created": kinds_json(created), "retracted": kinds_json(retracted)},
        })
    }

    /// [OP-STATS-6]
    fn evidence_section(&self, top: usize) -> Json {
        let st = &self.state;
        // Active citations per evidence, from one sequential pass over the citations.
        let mut cites = vec![0usize; st.evidence.end()];
        for c in st.citations.values().filter(|c| c.status == Status::Active) {
            if let Some(n) = cites.get_mut(c.evidence as usize) {
                *n += 1;
            }
        }
        let mut cited = vec![false; st.evidence.end()];
        let mut citations: Vec<(i64, usize)> = Vec::new();
        let mut domains: HashMap<Cow<str>, usize> = HashMap::new();
        let (mut with_blob, mut blobs) = (0usize, HashMap::<&str, i64>::new());
        let mut active = 0usize;
        for e in st.evidence.values().filter(|e| e.status == Status::Active) {
            active += 1;
            let n = cites[e.idx as usize];
            if n > 0 {
                citations.push((e.idx, n));
                // The evidence and every active ancestor through `parent` count as cited.
                let mut cur = Some(e.idx);
                while let Some(i) = cur {
                    if std::mem::replace(&mut cited[i as usize], true) {
                        break;
                    }
                    cur = st.evidence.get(&i).and_then(|x| x.parent).filter(|p| {
                        st.evidence
                            .get(p)
                            .is_some_and(|x| x.status == Status::Active)
                    });
                }
            }
            if let Some(d) = e.uri.as_deref().and_then(uri_domain) {
                *domains.entry(d).or_default() += 1;
            }
            if let Some(h) = e.blob_sha256.as_deref() {
                with_blob += 1;
                blobs.insert(h, e.blob_size.unwrap_or(0));
            }
        }
        let order = |a: &(i64, usize), b: &(i64, usize)| b.1.cmp(&a.1).then(a.0.cmp(&b.0));
        if citations.len() > top {
            citations.select_nth_unstable_by(top - 1, order);
            citations.truncate(top);
        }
        citations.sort_unstable_by(order);
        let kinds: BTreeMap<&str, usize> = st
            .active_evidence_kinds
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(k, n)| (k.as_str(), *n))
            .collect();
        json!({
            "active": active, "kinds": kinds,
            "uncited": active - cited.iter().filter(|&&c| c).count(),
            "domains": top_by_count(domains.iter().map(|(d, n)| (d.as_ref(), *n)), top)
                .into_iter().map(|(d, n)| json!({"domain": d, "count": n})).collect::<Vec<_>>(),
            "most_cited": citations.into_iter().map(|(i, n)| {
                let mut b = self.brief(ObjectKind::Evidence, i);
                b["citations"] = n.into();
                b
            }).collect::<Vec<_>>(),
            "blobs": {"evidence": with_blob, "distinct": blobs.len(),
                      "bytes": blobs.values().sum::<i64>()},
        })
    }

    /// [ADMIN-GRAPH-9] `actors`
    fn actors_section(&self) -> Result<Json, OpError> {
        let st = &self.state;
        let batches: HashMap<i64, (i64, i64)> = self
            .store_query(self.store.actor_batches())?
            .into_iter()
            .map(|(a, n, last)| (a, (n, last)))
            .collect();
        // Actor numbers are small and dense: arrays indexed by number.
        let end = st.actors.keys().max().map_or(0, |&m| m as usize + 1);
        let mut created = vec![[0usize; 4]; end];
        let mut retracted = vec![0usize; end];
        self.each_meta(|k, meta, status| {
            if let Some(c) = created.get_mut(meta.created_by as usize) {
                c[k] += 1;
            }
            if status == Status::Retracted
                && let Some(r) = retracted.get_mut(meta.updated_by as usize)
            {
                *r += 1;
            }
        });
        let mut actors: Vec<_> = st.actors.values().collect();
        actors.sort_unstable_by(|a, b| {
            let n = |x: i64| batches.get(&x).map_or(0, |b| b.0);
            n(b.idx).cmp(&n(a.idx)).then(a.name.cmp(&b.name))
        });
        Ok(actors
            .into_iter()
            .map(|a| {
                let (n, last) = batches.get(&a.idx).copied().unwrap_or((0, 0));
                json!({
                    "actor": a.name, "kind": a.kind, "batches": n,
                    "last_batch_at": if n > 0 { time_json(last) } else { Json::Null },
                    "created": kinds_json(created[a.idx as usize]),
                    "retracted": retracted[a.idx as usize],
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::uri_domain;

    #[test]
    fn domains() {
        let d = |u: &'static str| uri_domain(u);
        assert_eq!(
            d("https://www.Example.org/a?b").as_deref(),
            Some("example.org")
        );
        assert_eq!(
            d("http://user@en.wikipedia.org:8080/wiki").as_deref(),
            Some("en.wikipedia.org")
        );
        assert_eq!(d("https://[::1]:80/x").as_deref(), Some("[::1]"));
        assert_eq!(d("ftp://example.org"), None);
        assert_eq!(d("https://"), None);
    }
}
