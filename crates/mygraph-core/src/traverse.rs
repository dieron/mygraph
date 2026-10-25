//! Operations `neighbors` and `paths` — spec/api/operations/{neighbors,paths}.md,
//! spec/algorithms/traversal.md.

use crate::engine::{Graph, OpError};
use crate::ids::ObjectKind;
use crate::model::{Link, Status, TargetKind, id_string};
use crate::store::Store;
use crate::values::{self, Parsed, temporal_bounds};
use crate::vocab::VocabKind;
use serde_json::{Map, Value as Json, json};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

/// Nodes the reverse search of `k_shortest` may visit, and steps its path search may take
/// ([OP-PATHS-3]).
const VISIT_BUDGET: usize = 300_000;
const STEP_BUDGET: usize = 2_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Out,
    In,
    Both,
}

impl Dir {
    fn reversed(self) -> Self {
        match self {
            Dir::Out => Dir::In,
            Dir::In => Dir::Out,
            Dir::Both => Dir::Both,
        }
    }
}

/// Link filter of one hop ([TRAV-1]).
#[derive(Clone)]
struct Rule {
    types: Option<HashSet<String>>,
    dir: Dir,
    /// `as_of` interval in milliseconds.
    as_of: Option<(i64, i64)>,
}

impl Rule {
    /// The other end when `l` can be followed from `n`.
    fn follow(&self, n: i64, l: &Link) -> Option<i64> {
        if l.status != Status::Active || self.types.as_ref().is_some_and(|t| !t.contains(&*l.type_))
        {
            return None;
        }
        let other = if l.src == n { l.dst } else { l.src };
        if l.directed {
            let ok = match self.dir {
                Dir::Both => true,
                Dir::Out => l.src == n,
                Dir::In => l.dst == n,
            };
            if !ok {
                return None;
            }
        }
        if let Some((start, end)) = self.as_of {
            let bound = |v: &Option<values::Value>| v.as_ref().and_then(temporal_bounds);
            if bound(&l.valid_from).is_some_and(|(s, _)| s > end)
                || bound(&l.valid_to).is_some_and(|(_, e)| e < start)
            {
                return None;
            }
        }
        Some(other)
    }
}

fn bad(op: &str, field: &str, expected: &str) -> OpError {
    OpError::new(format_reason(op), format!("{field}: expected {expected}"))
}

fn format_reason(op: &str) -> &'static str {
    match op {
        "neighbors" => "neighbors.invalid_field",
        _ => "paths.invalid_field",
    }
}

fn opt_u64(
    o: &Map<String, Json>,
    op: &str,
    key: &str,
    lo: u64,
    hi: u64,
    default: u64,
) -> Result<usize, OpError> {
    match o.get(key) {
        None | Some(Json::Null) => Ok(default as usize),
        Some(v) => v
            .as_u64()
            .filter(|n| (lo..=hi).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| bad(op, key, &format!("an integer from {lo} to {hi}"))),
    }
}

impl<S: Store> Graph<S> {
    fn parse_rule(
        &self,
        op: &str,
        o: &Map<String, Json>,
        as_of: Option<(i64, i64)>,
    ) -> Result<Rule, OpError> {
        let types = self.type_set(op, o, "link_types", VocabKind::LinkType)?;
        let dir = match o.get("direction").map(Json::as_str) {
            None | Some(Some("both")) => Dir::Both,
            Some(Some("out")) => Dir::Out,
            Some(Some("in")) => Dir::In,
            _ => return Err(bad(op, "direction", "out, in or both")),
        };
        Ok(Rule { types, dir, as_of })
    }

    fn type_set(
        &self,
        op: &str,
        o: &Map<String, Json>,
        key: &str,
        kind: VocabKind,
    ) -> Result<Option<HashSet<String>>, OpError> {
        match o.get(key) {
            None | Some(Json::Null) => Ok(None),
            Some(Json::Array(a)) => a
                .iter()
                .map(|t| {
                    t.as_str()
                        .and_then(|t| self.canon(kind, t).ok())
                        .ok_or_else(|| bad(op, key, "a list of type names"))
                })
                .collect::<Result<HashSet<_>, _>>()
                .map(Some),
            Some(_) => Err(bad(op, key, "a list of type names")),
        }
    }

    fn parse_as_of(&self, op: &str, o: &Map<String, Json>) -> Result<Option<(i64, i64)>, OpError> {
        match o.get("as_of") {
            None | Some(Json::Null) => Ok(None),
            Some(v) => match values::parse(v) {
                Ok(Parsed::Value(v)) => temporal_bounds(&v)
                    .map(Some)
                    .ok_or_else(|| bad(op, "as_of", "a $date or $datetime value")),
                _ => Err(bad(op, "as_of", "a $date or $datetime value")),
            },
        }
    }

    fn node_ref(&self, op: &str, field: &str, r: &Json) -> Result<i64, OpError> {
        match self.resolve_any(r) {
            Ok((ObjectKind::Node, idx, _)) => Ok(idx),
            Ok(_) => Err(bad(op, field, "a node reference")),
            Err(e) => Err(e),
        }
    }

    /// Distinct active evidence citing a node or link ([OP-GET-5]).
    pub(crate) fn sources_of(&self, t: TargetKind, idx: i64) -> usize {
        let st = &self.state;
        let mut set = HashSet::new();
        for c in st.cites_of(t, idx).into_iter().flatten() {
            let c = &st.citations[c];
            if c.status == Status::Active
                && st
                    .evidence
                    .get(&c.evidence)
                    .is_some_and(|e| e.status == Status::Active)
            {
                set.insert(c.evidence);
            }
        }
        set.len()
    }

    fn node_brief(&self, n: i64) -> Map<String, Json> {
        let node = &self.state.nodes[&n];
        let mut m = Map::new();
        m.insert("id".into(), id_string(ObjectKind::Node, n).into());
        m.insert("type".into(), (*node.type_).into());
        m.insert("name".into(), node.name.clone().into());
        m
    }

    fn link_brief(&self, l: i64) -> Map<String, Json> {
        let link = &self.state.links[&l];
        let mut m = Map::new();
        m.insert("id".into(), id_string(ObjectKind::Link, l).into());
        m.insert("type".into(), (*link.type_).into());
        m.insert("from".into(), id_string(ObjectKind::Node, link.src).into());
        m.insert("to".into(), id_string(ObjectKind::Node, link.dst).into());
        m.insert("directed".into(), link.directed.into());
        m
    }

    /// Followable (link, other end) pairs of `n` in link order ([TRAV-3]).
    fn followable(&self, n: i64, rule: &Rule) -> Vec<(i64, i64)> {
        let st = &self.state;
        let mut out: Vec<(&str, i64, i64)> = st
            .adjacency
            .get(&n)
            .into_iter()
            .flatten()
            .filter_map(|l| {
                let link = &st.links[l];
                rule.follow(n, link).map(|o| (&*link.type_, *l, o))
            })
            .collect();
        out.sort_unstable();
        out.into_iter().map(|(_, l, o)| (l, o)).collect()
    }

    /// Operation `neighbors` ([OP-NEIGH-1] .. [OP-NEIGH-4]).
    pub fn neighbors(&self, request: &Json) -> Result<Json, OpError> {
        const OP: &str = "neighbors";
        let req = request
            .as_object()
            .ok_or_else(|| bad(OP, "request", "an object"))?;
        let starts = req
            .get("start")
            .and_then(Json::as_array)
            .filter(|a| (1..=50).contains(&a.len()))
            .ok_or_else(|| bad(OP, "start", "a list of 1 to 50 node references"))?;
        let as_of = self.parse_as_of(OP, req)?;
        let steps: Vec<(Rule, Option<HashSet<String>>)> = match (req.get("steps"), req.get("depth"))
        {
            (Some(_), Some(_)) => return Err(bad(OP, "steps", "either steps or depth, not both")),
            (Some(Json::Array(a)), None) if (1..=5).contains(&a.len()) => a
                .iter()
                .map(|s| {
                    let o = s
                        .as_object()
                        .ok_or_else(|| bad(OP, "steps", "a list of objects"))?;
                    Ok((
                        self.parse_rule(OP, o, as_of)?,
                        self.type_set(OP, o, "node_types", VocabKind::NodeType)?,
                    ))
                })
                .collect::<Result<_, OpError>>()?,
            (Some(_), None) => return Err(bad(OP, "steps", "a list of 1 to 5 steps")),
            (None, _) => {
                let depth = opt_u64(req, OP, "depth", 1, 5, 1)?;
                let rule = self.parse_rule(OP, req, as_of)?;
                let types = self.type_set(OP, req, "node_types", VocabKind::NodeType)?;
                vec![(rule, types); depth]
            }
        };
        let per_node = opt_u64(req, OP, "limit_per_node", 1, 1000, 25)?;
        let limit = opt_u64(req, OP, "limit", 1, 5000, 200)?;
        let support = req
            .get("include")
            .and_then(|i| i.get("support"))
            .and_then(Json::as_bool)
            .unwrap_or(false);

        let st = &self.state;
        let mut hop_of: HashMap<i64, usize> = HashMap::new();
        let mut nodes: Vec<i64> = Vec::new();
        for r in starts {
            let n = self.node_ref(OP, "start", r)?;
            if hop_of.insert(n, 0).is_none() {
                nodes.push(n);
            }
        }
        let mut links: Vec<i64> = Vec::new();
        let mut taken: HashSet<i64> = HashSet::new();
        let mut notes = Vec::new();
        let mut stopped = nodes.len() > limit;
        nodes.truncate(limit);
        let mut frontier: Vec<i64> = nodes.clone();
        'hops: for (i, (rule, node_types)) in steps.iter().enumerate() {
            let hop = i + 1;
            frontier.sort_unstable();
            let mut next = Vec::new();
            for &n in &frontier {
                let cand: Vec<(i64, i64)> = self
                    .followable(n, rule)
                    .into_iter()
                    .filter(|(_, o)| {
                        node_types
                            .as_ref()
                            .is_none_or(|t| t.contains(&*st.nodes[o].type_))
                    })
                    .collect();
                if cand.len() > per_node {
                    notes.push(json!({
                        "node": id_string(ObjectKind::Node, n), "hop": hop,
                        "links_taken": per_node, "links_total": cand.len(),
                    }));
                }
                for (l, o) in cand.into_iter().take(per_node) {
                    if taken.contains(&l) {
                        continue;
                    }
                    if let Entry::Vacant(e) = hop_of.entry(o) {
                        if nodes.len() >= limit {
                            stopped = true;
                            break 'hops;
                        }
                        e.insert(hop);
                        nodes.push(o);
                        next.push(o);
                    }
                    taken.insert(l);
                    links.push(l);
                }
            }
            frontier = next;
        }
        let nodes_json: Vec<Json> = nodes
            .iter()
            .map(|&n| {
                let mut m = self.node_brief(n);
                m.insert("hop".into(), hop_of[&n].into());
                if support {
                    m.insert(
                        "sources".into(),
                        self.sources_of(TargetKind::Node, n).into(),
                    );
                }
                Json::Object(m)
            })
            .collect();
        let links_json: Vec<Json> = links
            .iter()
            .map(|&l| {
                let mut m = self.link_brief(l);
                if support {
                    m.insert(
                        "sources".into(),
                        self.sources_of(TargetKind::Link, l).into(),
                    );
                }
                Json::Object(m)
            })
            .collect();
        Ok(json!({
            "nodes": nodes_json, "links": links_json,
            "truncated": stopped || !notes.is_empty(), "notes": notes,
        }))
    }

    /// Followable pairs of `n` without ordering (for distance searches).
    fn each_followable(&self, n: i64, rule: &Rule, mut f: impl FnMut(i64, i64)) {
        let st = &self.state;
        for l in st.adjacency.get(&n).into_iter().flatten() {
            if let Some(o) = rule.follow(n, &st.links[l]) {
                f(*l, o);
            }
        }
    }

    /// Operation `paths` ([OP-PATHS-1] .. [OP-PATHS-4]).
    pub fn paths(&self, request: &Json) -> Result<Json, OpError> {
        const OP: &str = "paths";
        let req = request
            .as_object()
            .ok_or_else(|| bad(OP, "request", "an object"))?;
        let from = self.node_ref(
            OP,
            "from",
            req.get("from")
                .ok_or_else(|| bad(OP, "from", "a node reference"))?,
        )?;
        let to = self.node_ref(
            OP,
            "to",
            req.get("to")
                .ok_or_else(|| bad(OP, "to", "a node reference"))?,
        )?;
        let max_len = opt_u64(req, OP, "max_length", 1, 10, 4)?;
        let as_of = self.parse_as_of(OP, req)?;
        let fwd = self.parse_rule(OP, req, as_of)?;
        let back = Rule {
            dir: fwd.dir.reversed(),
            ..fwd.clone()
        };
        let (want, mode_all) = match req.get("mode").map(Json::as_str) {
            None | Some(Some("shortest")) => (1, false),
            Some(Some("all_shortest")) => (100, true),
            Some(Some("k_shortest")) => (opt_u64(req, OP, "k", 1, 20, 3)?, false),
            _ => return Err(bad(OP, "mode", "shortest, all_shortest or k_shortest")),
        };
        let k_mode = matches!(req.get("mode").and_then(Json::as_str), Some("k_shortest"));

        let mut found: Vec<Vec<(i64, i64)>> = Vec::new(); // (link, node) steps after `from`
        let mut truncated = false;
        if from == to {
            found.push(Vec::new());
        } else if let Some((d, split, df, dt)) = self.meet(from, to, max_len, &fwd, &back) {
            self.shortest_paths(from, to, d, split, &df, &dt, &fwd, want, &mut found);
            if k_mode && found.len() < want && d < max_len {
                truncated = !self.longer_paths(from, to, d, max_len, &fwd, &back, want, &mut found);
            }
        }
        let _ = mode_all;
        let paths: Vec<Json> = found
            .iter()
            .map(|steps| {
                let mut nodes = vec![Json::Object(self.node_brief(from))];
                let mut links = Vec::new();
                let mut min_sources: Option<usize> = None;
                for &(l, n) in steps {
                    let mut lj = self.link_brief(l);
                    let s = self.sources_of(TargetKind::Link, l);
                    min_sources = Some(min_sources.map_or(s, |m| m.min(s)));
                    lj.insert("sources".into(), s.into());
                    links.push(Json::Object(lj));
                    nodes.push(Json::Object(self.node_brief(n)));
                }
                let mut p = json!({ "length": steps.len(), "nodes": nodes, "links": links });
                if let Some(m) = min_sources {
                    p["min_sources"] = m.into();
                }
                p
            })
            .collect();
        Ok(json!({ "paths": paths, "truncated": truncated }))
    }

    /// Bidirectional BFS: the shortest length `d`, the position `split` where the searches met,
    /// and the distances found from `from` (df) and to `to` (dt).
    #[allow(clippy::type_complexity)]
    fn meet(
        &self,
        from: i64,
        to: i64,
        max_len: usize,
        fwd: &Rule,
        back: &Rule,
    ) -> Option<(usize, usize, HashMap<i64, usize>, HashMap<i64, usize>)> {
        let mut df = HashMap::from([(from, 0usize)]);
        let mut dt = HashMap::from([(to, 0usize)]);
        let (mut fa, mut fb) = (vec![from], vec![to]);
        let (mut a, mut b) = (0usize, 0usize);
        while a + b < max_len && !fa.is_empty() && !fb.is_empty() {
            let side_a = fa.len() <= fb.len();
            let (frontier, mine, other, rule, depth) = if side_a {
                (&mut fa, &mut df, &dt, fwd, &mut a)
            } else {
                (&mut fb, &mut dt, &df, back, &mut b)
            };
            let mut next = Vec::new();
            for &n in frontier.iter() {
                self.each_followable(n, rule, |_, o| {
                    if let Entry::Vacant(e) = mine.entry(o) {
                        e.insert(*depth + 1);
                        next.push(o);
                    }
                });
            }
            *depth += 1;
            let best = next.iter().filter_map(|n| other.get(n)).min().copied();
            *frontier = next;
            if let Some(o) = best {
                let d = *depth + o;
                // Position (from `from`) of the meeting layer.
                let split = if side_a { a } else { d - b };
                return (d <= max_len).then_some((d, split, df, dt));
            }
        }
        None
    }

    /// All shortest paths in path order ([TRAV-4]), at most `want`: walk the layers of `df`
    /// up to the meeting layer (only through nodes that lead to it), then the layers of `dt`.
    #[allow(clippy::too_many_arguments)]
    fn shortest_paths(
        &self,
        from: i64,
        to: i64,
        d: usize,
        split: usize,
        df: &HashMap<i64, usize>,
        dt: &HashMap<i64, usize>,
        fwd: &Rule,
        want: usize,
        found: &mut Vec<Vec<(i64, i64)>>,
    ) {
        // Nodes on the `from` side that reach the meeting layer.
        let mut layers: Vec<Vec<i64>> = vec![Vec::new(); split + 1];
        for (&n, &i) in df {
            if i <= split {
                layers[i].push(n);
            }
        }
        let mut good: HashSet<i64> = layers[split]
            .iter()
            .copied()
            .filter(|n| dt.get(n) == Some(&(d - split)))
            .collect();
        for i in (0..split).rev() {
            for &n in &layers[i] {
                let mut ok = false;
                self.each_followable(n, fwd, |_, o| {
                    if df.get(&o) == Some(&(i + 1)) && good.contains(&o) {
                        ok = true;
                    }
                });
                if ok {
                    good.insert(n);
                }
            }
        }
        let next_ok = |pos: usize, o: i64| {
            if pos < split {
                df.get(&o) == Some(&(pos + 1)) && good.contains(&o)
            } else {
                dt.get(&o) == Some(&(d - pos - 1))
            }
        };
        let mut path = Vec::new();
        self.walk(from, to, 0, d, &next_ok, fwd, &mut path, want, found);
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        n: i64,
        to: i64,
        pos: usize,
        d: usize,
        next_ok: &dyn Fn(usize, i64) -> bool,
        fwd: &Rule,
        path: &mut Vec<(i64, i64)>,
        want: usize,
        found: &mut Vec<Vec<(i64, i64)>>,
    ) {
        if found.len() >= want {
            return;
        }
        if pos == d {
            if n == to {
                found.push(path.clone());
            }
            return;
        }
        let mut cand: Vec<(i64, i64)> = Vec::new();
        self.each_followable(n, fwd, |l, o| {
            if next_ok(pos, o) {
                cand.push((o, l));
            }
        });
        cand.sort_unstable();
        for (o, l) in cand {
            path.push((l, o));
            self.walk(o, to, pos + 1, d, next_ok, fwd, path, want, found);
            path.pop();
            if found.len() >= want {
                return;
            }
        }
    }

    /// `k_shortest` beyond the shortest length: simple paths of each longer length in path
    /// order, pruned by the distance to `to`. Returns `false` when a budget stopped the search.
    #[allow(clippy::too_many_arguments)]
    fn longer_paths(
        &self,
        from: i64,
        to: i64,
        d: usize,
        max_len: usize,
        fwd: &Rule,
        back: &Rule,
        want: usize,
        found: &mut Vec<Vec<(i64, i64)>>,
    ) -> bool {
        // Full distances to `to`, up to max_len - 1.
        let mut dist = HashMap::from([(to, 0usize)]);
        let mut frontier = vec![to];
        for depth in 1..max_len {
            let mut next = Vec::new();
            for &n in &frontier {
                self.each_followable(n, back, |_, o| {
                    if let Entry::Vacant(e) = dist.entry(o) {
                        e.insert(depth);
                        next.push(o);
                    }
                });
            }
            if dist.len() > VISIT_BUDGET {
                return false;
            }
            frontier = next;
        }
        let mut steps = 0usize;
        for len in d + 1..=max_len {
            let mut path = Vec::new();
            let mut on_path = HashSet::from([from]);
            if !self.simple_walk(
                from,
                to,
                0,
                len,
                &dist,
                fwd,
                &mut path,
                &mut on_path,
                want,
                found,
                &mut steps,
            ) {
                return false;
            }
            if found.len() >= want {
                break;
            }
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn simple_walk(
        &self,
        n: i64,
        to: i64,
        pos: usize,
        len: usize,
        dist: &HashMap<i64, usize>,
        fwd: &Rule,
        path: &mut Vec<(i64, i64)>,
        on_path: &mut HashSet<i64>,
        want: usize,
        found: &mut Vec<Vec<(i64, i64)>>,
        steps: &mut usize,
    ) -> bool {
        *steps += 1;
        if *steps > STEP_BUDGET {
            return false;
        }
        if pos == len {
            if n == to {
                found.push(path.clone());
            }
            return true;
        }
        let rest = len - pos - 1;
        let mut cand: Vec<(i64, i64)> = Vec::new();
        self.each_followable(n, fwd, |l, o| {
            let reach = dist
                .get(&o)
                .is_some_and(|&x| x <= rest && (o != to || rest == 0));
            if reach && !on_path.contains(&o) {
                cand.push((o, l));
            }
        });
        cand.sort_unstable();
        for (o, l) in cand {
            path.push((l, o));
            on_path.insert(o);
            let ok = self.simple_walk(
                o,
                to,
                pos + 1,
                len,
                dist,
                fwd,
                path,
                on_path,
                want,
                found,
                steps,
            );
            on_path.remove(&o);
            path.pop();
            if !ok {
                return false;
            }
            if found.len() >= want {
                return true;
            }
        }
        true
    }
}
