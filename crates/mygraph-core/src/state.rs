//! In-memory indexes (docs/05-architecture.md §5). Every change goes through `put_*` /
//! `remove_*`, which keep all indexes consistent, so the engine can undo a batch by putting back
//! the old rows.

use crate::ids::ObjectKind;
use crate::model::{Actor, Citation, Evidence, Link, Node, Role, Row, Status, TargetKind};
use crate::name_key::name_key;
use crate::store::Snapshot;
use std::collections::{BTreeSet, HashMap, HashSet};

/// [STORE-LINK-1] identity of an active, non-multi, unkeyed link.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LinkIdentity {
    pub type_: String,
    pub directed: bool,
    pub a: i64,
    pub b: i64,
}

impl LinkIdentity {
    pub fn new(type_: &str, directed: bool, src: i64, dst: i64) -> Self {
        let (a, b) = if directed || src <= dst {
            (src, dst)
        } else {
            (dst, src)
        };
        Self {
            type_: type_.to_owned(),
            directed,
            a,
            b,
        }
    }

    fn of(l: &Link) -> Option<Self> {
        (l.status == Status::Active && !l.multi && l.key.is_none())
            .then(|| Self::new(&l.type_, l.directed, l.src, l.dst))
    }
}

/// [WRITE-MATCH-EV-1] content identity of evidence without key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EvidenceIdentity {
    pub kind: String,
    pub uri: Option<String>,
    pub blob: Option<String>,
    pub parent: Option<i64>,
    pub content: Option<String>,
}

impl EvidenceIdentity {
    fn of(e: &Evidence) -> Option<Self> {
        (e.status == Status::Active).then(|| Self {
            kind: e.kind.clone(),
            uri: e.uri.clone(),
            blob: e.blob_sha256.clone(),
            parent: e.parent,
            content: e.content_sha256.clone(),
        })
    }
}

/// [OBJ-CIT-1] identity of an active citation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CitationIdentity {
    pub evidence: i64,
    pub target_kind: TargetKind,
    pub target: i64,
    pub prop: Option<String>,
    pub role: Role,
}

impl CitationIdentity {
    fn of(c: &Citation) -> Option<Self> {
        (c.status == Status::Active).then(|| Self {
            evidence: c.evidence,
            target_kind: c.target_kind,
            target: c.target,
            prop: c.prop.clone(),
            role: c.role,
        })
    }
}

#[derive(Debug, Default)]
pub struct State {
    pub nodes: HashMap<i64, Node>,
    pub links: HashMap<i64, Link>,
    pub evidence: HashMap<i64, Evidence>,
    pub citations: HashMap<i64, Citation>,
    pub actors: HashMap<i64, Actor>,
    pub actor_by_name: HashMap<String, i64>,
    pub node_keys: HashMap<String, i64>,
    pub link_keys: HashMap<String, i64>,
    pub evidence_keys: HashMap<String, i64>,
    /// (node type, name key) -> active nodes with that name or alias.
    pub names: HashMap<(String, String), BTreeSet<i64>>,
    /// node -> links (any status) with that node as an endpoint.
    pub adjacency: HashMap<i64, BTreeSet<i64>>,
    pub link_identity: HashMap<LinkIdentity, i64>,
    pub evidence_identity: HashMap<EvidenceIdentity, BTreeSet<i64>>,
    pub citation_identity: HashMap<CitationIdentity, i64>,
    /// (target kind, target) -> citations (any status).
    pub cites_by_target: HashMap<(TargetKind, i64), BTreeSet<i64>>,
    pub cites_by_evidence: HashMap<i64, BTreeSet<i64>>,
    /// Usage counts over objects of any status: node types, link types, property keys.
    pub node_types: HashMap<String, usize>,
    pub link_types: HashMap<String, usize>,
    pub prop_keys: HashMap<String, usize>,
    pub symmetric_link_types: HashSet<String>,
}

fn add<K: Eq + std::hash::Hash>(m: &mut HashMap<K, usize>, k: K) {
    *m.entry(k).or_default() += 1;
}

fn sub<K: Eq + std::hash::Hash>(m: &mut HashMap<K, usize>, k: &K) {
    if let Some(n) = m.get_mut(k) {
        *n -= 1;
        if *n == 0 {
            m.remove(k);
        }
    }
}

fn set_add<K: Eq + std::hash::Hash>(m: &mut HashMap<K, BTreeSet<i64>>, k: K, v: i64) {
    m.entry(k).or_default().insert(v);
}

fn set_remove<K: Eq + std::hash::Hash>(m: &mut HashMap<K, BTreeSet<i64>>, k: &K, v: i64) {
    if let Some(s) = m.get_mut(k) {
        s.remove(&v);
        if s.is_empty() {
            m.remove(k);
        }
    }
}

impl State {
    pub fn from_snapshot(s: Snapshot) -> Self {
        let mut st = Self::default();
        for a in s.actors {
            st.put_actor(a);
        }
        st.symmetric_link_types = s.symmetric_link_types.into_iter().collect();
        for n in s.nodes {
            st.put_node(n);
        }
        for l in s.links {
            st.put_link(l);
        }
        for e in s.evidence {
            st.put_evidence(e);
        }
        for c in s.citations {
            st.put_citation(c);
        }
        st
    }

    pub fn put_actor(&mut self, a: Actor) {
        self.actor_by_name.insert(a.name.clone(), a.idx);
        self.actors.insert(a.idx, a);
    }

    pub fn remove_actor(&mut self, idx: i64) {
        if let Some(a) = self.actors.remove(&idx) {
            self.actor_by_name.remove(&a.name);
        }
    }

    pub fn actor_name(&self, idx: i64) -> String {
        self.actors
            .get(&idx)
            .map_or_else(|| format!("#{idx}"), |a| a.name.clone())
    }

    /// Puts any row, returning the previous version.
    pub fn put(&mut self, row: Row) -> Option<Row> {
        match row {
            Row::Node(n) => self.put_node(n).map(Row::Node),
            Row::Link(l) => self.put_link(l).map(Row::Link),
            Row::Evidence(e) => self.put_evidence(e).map(Row::Evidence),
            Row::Citation(c) => self.put_citation(c).map(Row::Citation),
        }
    }

    pub fn remove(&mut self, kind: ObjectKind, idx: i64) -> Option<Row> {
        match kind {
            ObjectKind::Node => self.remove_node(idx).map(Row::Node),
            ObjectKind::Link => self.remove_link(idx).map(Row::Link),
            ObjectKind::Evidence => self.remove_evidence(idx).map(Row::Evidence),
            ObjectKind::Citation => self.remove_citation(idx).map(Row::Citation),
            ObjectKind::Batch => None,
        }
    }

    pub fn get(&self, kind: ObjectKind, idx: i64) -> Option<Row> {
        match kind {
            ObjectKind::Node => self.nodes.get(&idx).cloned().map(Row::Node),
            ObjectKind::Link => self.links.get(&idx).cloned().map(Row::Link),
            ObjectKind::Evidence => self.evidence.get(&idx).cloned().map(Row::Evidence),
            ObjectKind::Citation => self.citations.get(&idx).cloned().map(Row::Citation),
            ObjectKind::Batch => None,
        }
    }

    // ---- nodes -------------------------------------------------------------------------

    pub fn put_node(&mut self, n: Node) -> Option<Node> {
        let old = self.remove_node(n.idx);
        if let Some(k) = &n.key {
            self.node_keys.insert(k.clone(), n.idx);
        }
        if n.status == Status::Active {
            for name in std::iter::once(&n.name).chain(&n.aliases) {
                set_add(&mut self.names, (n.type_.clone(), name_key(name)), n.idx);
            }
        }
        add(&mut self.node_types, n.type_.clone());
        for k in n.props.keys() {
            add(&mut self.prop_keys, k.clone());
        }
        self.nodes.insert(n.idx, n);
        old
    }

    pub fn remove_node(&mut self, idx: i64) -> Option<Node> {
        let n = self.nodes.remove(&idx)?;
        if let Some(k) = &n.key {
            self.node_keys.remove(k);
        }
        for name in std::iter::once(&n.name).chain(&n.aliases) {
            set_remove(&mut self.names, &(n.type_.clone(), name_key(name)), idx);
        }
        sub(&mut self.node_types, &n.type_);
        for k in n.props.keys() {
            sub(&mut self.prop_keys, k);
        }
        Some(n)
    }

    // ---- links -------------------------------------------------------------------------

    pub fn put_link(&mut self, l: Link) -> Option<Link> {
        let old = self.remove_link(l.idx);
        if let Some(k) = &l.key {
            self.link_keys.insert(k.clone(), l.idx);
        }
        if let Some(id) = LinkIdentity::of(&l) {
            self.link_identity.insert(id, l.idx);
        }
        set_add(&mut self.adjacency, l.src, l.idx);
        set_add(&mut self.adjacency, l.dst, l.idx);
        add(&mut self.link_types, l.type_.clone());
        for k in l.props.keys() {
            add(&mut self.prop_keys, k.clone());
        }
        self.links.insert(l.idx, l);
        old
    }

    pub fn remove_link(&mut self, idx: i64) -> Option<Link> {
        let l = self.links.remove(&idx)?;
        if let Some(k) = &l.key {
            self.link_keys.remove(k);
        }
        if let Some(id) = LinkIdentity::of(&l)
            && self.link_identity.get(&id) == Some(&idx)
        {
            self.link_identity.remove(&id);
        }
        set_remove(&mut self.adjacency, &l.src, idx);
        set_remove(&mut self.adjacency, &l.dst, idx);
        sub(&mut self.link_types, &l.type_);
        for k in l.props.keys() {
            sub(&mut self.prop_keys, k);
        }
        Some(l)
    }

    // ---- evidence ----------------------------------------------------------------------

    pub fn put_evidence(&mut self, mut e: Evidence) -> Option<Evidence> {
        e.content = None; // [docs/05 §5] content stays on disk
        let old = self.remove_evidence(e.idx);
        if let Some(k) = &e.key {
            self.evidence_keys.insert(k.clone(), e.idx);
        }
        if let Some(id) = EvidenceIdentity::of(&e) {
            set_add(&mut self.evidence_identity, id, e.idx);
        }
        self.evidence.insert(e.idx, e);
        old
    }

    pub fn remove_evidence(&mut self, idx: i64) -> Option<Evidence> {
        let e = self.evidence.remove(&idx)?;
        if let Some(k) = &e.key {
            self.evidence_keys.remove(k);
        }
        if let Some(id) = EvidenceIdentity::of(&e) {
            set_remove(&mut self.evidence_identity, &id, idx);
        }
        Some(e)
    }

    // ---- citations ---------------------------------------------------------------------

    pub fn put_citation(&mut self, c: Citation) -> Option<Citation> {
        let old = self.remove_citation(c.idx);
        if let Some(id) = CitationIdentity::of(&c) {
            self.citation_identity.insert(id, c.idx);
        }
        set_add(&mut self.cites_by_target, (c.target_kind, c.target), c.idx);
        set_add(&mut self.cites_by_evidence, c.evidence, c.idx);
        self.citations.insert(c.idx, c);
        old
    }

    pub fn remove_citation(&mut self, idx: i64) -> Option<Citation> {
        let c = self.citations.remove(&idx)?;
        if let Some(id) = CitationIdentity::of(&c)
            && self.citation_identity.get(&id) == Some(&idx)
        {
            self.citation_identity.remove(&id);
        }
        set_remove(&mut self.cites_by_target, &(c.target_kind, c.target), idx);
        set_remove(&mut self.cites_by_evidence, &c.evidence, idx);
        Some(c)
    }

    // ---- queries -----------------------------------------------------------------------

    /// [OBJ-REF-1] follows `merged_into`.
    pub fn resolve_node(&self, mut idx: i64) -> Option<i64> {
        for _ in 0..1000 {
            let n = self.nodes.get(&idx)?;
            match n.merged_into {
                Some(next) if n.status == Status::Merged => idx = next,
                _ => return Some(idx),
            }
        }
        None
    }

    /// [OBJ-CIT-3]
    pub fn is_supported(&self, kind: TargetKind, idx: i64) -> bool {
        self.cites_by_target.get(&(kind, idx)).is_some_and(|cs| {
            cs.iter().any(|c| {
                let c = &self.citations[c];
                c.status == Status::Active
                    && c.role != Role::Contradicts
                    && self
                        .evidence
                        .get(&c.evidence)
                        .is_some_and(|e| e.status == Status::Active)
            })
        })
    }
}
