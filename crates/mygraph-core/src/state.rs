//! In-memory indexes (docs/05-architecture.md §5). Every change goes through `put_*` /
//! `remove_*`, which keep all indexes consistent, so the engine can undo a batch by putting back
//! the old rows.

use crate::ids::ObjectKind;
use crate::model::{Actor, Citation, Evidence, Link, Node, Role, Row, Status, TargetKind};
use crate::name_key::name_key;
use crate::names::NameIndex;
use crate::store::{Counters, LoadSink, Snapshot, Store, StoreFailure};
use crate::table::{DenseIndex, IdSet, Table};
use crate::values::Sym;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

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

    fn matches(&self, l: &Link) -> bool {
        let ends = if l.directed || l.src <= l.dst {
            (l.src, l.dst)
        } else {
            (l.dst, l.src)
        };
        l.status == Status::Active
            && !l.multi
            && l.key.is_none()
            && l.directed == self.directed
            && ends == (self.a, self.b)
            && *l.type_ == *self.type_
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

    /// Only the hash is indexed; candidates are compared field by field.
    fn hash64(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
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
    fn matches(&self, c: &Citation) -> bool {
        c.status == Status::Active
            && c.evidence == self.evidence
            && c.target_kind == self.target_kind
            && c.target == self.target
            && c.prop == self.prop
            && c.role == self.role
    }
}

#[derive(Debug, Default)]
pub struct State {
    pub nodes: Table<Node>,
    pub links: Table<Link>,
    pub evidence: Table<Evidence>,
    pub citations: Table<Citation>,
    pub actors: HashMap<i64, Actor>,
    pub actor_by_name: HashMap<String, i64>,
    pub node_keys: HashMap<String, i64>,
    pub link_keys: HashMap<String, i64>,
    pub evidence_keys: HashMap<String, i64>,
    /// name key -> active nodes (any type) with that name or alias.
    pub names: NameIndex,
    /// Active nodes and links by type ([OP-FIND-*] starts from these when a condition fixes
    /// the type).
    pub nodes_by_type: HashMap<Sym, IdSet>,
    pub links_by_type: HashMap<Sym, IdSet>,
    /// node -> links (any status) with that node as an endpoint.
    pub adjacency: DenseIndex,
    /// The type of every link (any status) by internal number, as a number from
    /// `link_type_numbers` (0: no such link), so a traversal can pass over links of other types
    /// without reading their records.
    link_type_at: Vec<u32>,
    link_type_numbers: HashMap<Sym, u32>,
    /// Hash of an [`EvidenceIdentity`] -> active evidence with that hash.
    evidence_identity: HashMap<u64, IdSet>,
    /// Citations (any status) by target node and by target link.
    cites_by_node: DenseIndex,
    cites_by_link: DenseIndex,
    pub cites_by_evidence: DenseIndex,
    /// Usage counts over objects of any status: node types, link types, property keys.
    pub node_types: HashMap<Sym, usize>,
    pub link_types: HashMap<Sym, usize>,
    pub prop_keys: HashMap<Sym, usize>,
    /// Active evidence by kind and the number of active citations, kept up to date so
    /// `overview` need not scan ([OP-OVER-2]).
    pub active_evidence_kinds: HashMap<String, usize>,
    pub active_citations: usize,
    pub symmetric_link_types: HashSet<String>,
    /// One shared copy of every type name and property key in use or used before.
    syms: HashSet<Sym>,
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

fn set_add<K: Eq + std::hash::Hash>(m: &mut HashMap<K, IdSet>, k: K, v: i64) {
    m.entry(k).or_default().insert(v);
}

fn set_remove<K: Eq + std::hash::Hash>(m: &mut HashMap<K, IdSet>, k: &K, v: i64) {
    if let Some(s) = m.get_mut(k) {
        s.remove(&v);
        if s.is_empty() {
            m.remove(k);
        }
    }
}

impl LoadSink for State {
    fn actor(&mut self, a: Actor) {
        self.put_actor(a);
    }

    fn row(&mut self, row: Row) {
        self.put(row);
    }

    fn symmetric_link_type(&mut self, name: String) {
        self.symmetric_link_types.insert(name);
    }
}

impl State {
    /// Loads the state from a store, streaming rows into the indexes ([docs/05 §7]).
    pub fn load(store: &dyn Store, counters: &Counters) -> Result<Self, StoreFailure> {
        let mut st = Self::default();
        st.nodes.reserve_to(counters.node);
        st.links.reserve_to(counters.link);
        st.evidence.reserve_to(counters.evidence);
        st.citations.reserve_to(counters.citation);
        store.load_into(&mut st)?;
        Ok(st)
    }

    pub fn from_snapshot(s: Snapshot) -> Self {
        let mut st = Self::default();
        for a in s.actors {
            st.put_actor(a);
        }
        st.symmetric_link_types = s.symmetric_link_types.into_iter().collect();
        let max = |ids: &mut dyn Iterator<Item = i64>| ids.max().unwrap_or(0);
        st.nodes.reserve_to(max(&mut s.nodes.iter().map(|n| n.idx)));
        st.links.reserve_to(max(&mut s.links.iter().map(|l| l.idx)));
        st.evidence
            .reserve_to(max(&mut s.evidence.iter().map(|e| e.idx)));
        st.citations
            .reserve_to(max(&mut s.citations.iter().map(|c| c.idx)));
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

    fn intern(syms: &mut HashSet<Sym>, s: &Sym) -> Sym {
        if let Some(x) = syms.get(s) {
            return x.clone();
        }
        syms.insert(s.clone());
        s.clone()
    }

    pub fn put_node(&mut self, mut n: Node) -> Option<Node> {
        let old = self.remove_node(n.idx);
        n.type_ = Self::intern(&mut self.syms, &n.type_);
        n.props.intern_keys(|k| Self::intern(&mut self.syms, k));
        if let Some(k) = &n.key {
            self.node_keys.insert(k.clone(), n.idx);
        }
        if n.status == Status::Active {
            for name in std::iter::once(&n.name).chain(&n.aliases) {
                self.names.add(name_key(name), n.idx);
            }
            set_add(&mut self.nodes_by_type, n.type_.clone(), n.idx);
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
        set_remove(&mut self.nodes_by_type, &n.type_, idx);
        if let Some(k) = &n.key {
            self.node_keys.remove(k);
        }
        for name in std::iter::once(&n.name).chain(&n.aliases) {
            self.names.remove(&name_key(name), idx);
        }
        sub(&mut self.node_types, &n.type_);
        for k in n.props.keys() {
            sub(&mut self.prop_keys, k);
        }
        Some(n)
    }

    // ---- links -------------------------------------------------------------------------

    /// The number standing for link type `t`, or `None` when no link ever had that type.
    pub fn link_type_number(&self, t: &str) -> Option<u32> {
        self.link_type_numbers.get(t).copied()
    }

    /// The type number of link `idx` (0 when there is no such link).
    pub fn link_type_number_at(&self, idx: i64) -> u32 {
        usize::try_from(idx)
            .ok()
            .and_then(|i| self.link_type_at.get(i))
            .copied()
            .unwrap_or(0)
    }

    pub fn put_link(&mut self, mut l: Link) -> Option<Link> {
        let old = self.remove_link(l.idx);
        l.type_ = Self::intern(&mut self.syms, &l.type_);
        let next = self.link_type_numbers.len() as u32 + 1;
        let number = *self
            .link_type_numbers
            .entry(l.type_.clone())
            .or_insert(next);
        let i = usize::try_from(l.idx).expect("internal numbers are not negative");
        if i >= self.link_type_at.len() {
            self.link_type_at.resize(i + 1, 0);
        }
        self.link_type_at[i] = number;
        l.props.intern_keys(|k| Self::intern(&mut self.syms, k));
        if let Some(k) = &l.key {
            self.link_keys.insert(k.clone(), l.idx);
        }
        if l.status == Status::Active {
            set_add(&mut self.links_by_type, l.type_.clone(), l.idx);
        }
        self.adjacency.add(l.src, l.idx);
        self.adjacency.add(l.dst, l.idx);
        add(&mut self.link_types, l.type_.clone());
        for k in l.props.keys() {
            add(&mut self.prop_keys, k.clone());
        }
        self.links.insert(l.idx, l);
        old
    }

    pub fn remove_link(&mut self, idx: i64) -> Option<Link> {
        let l = self.links.remove(&idx)?;
        if let Some(t) = usize::try_from(idx)
            .ok()
            .and_then(|i| self.link_type_at.get_mut(i))
        {
            *t = 0;
        }
        set_remove(&mut self.links_by_type, &l.type_, idx);
        if let Some(k) = &l.key {
            self.link_keys.remove(k);
        }
        self.adjacency.remove(l.src, idx);
        self.adjacency.remove(l.dst, idx);
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
            set_add(&mut self.evidence_identity, id.hash64(), e.idx);
        }
        if e.status == Status::Active {
            match self.active_evidence_kinds.get_mut(&e.kind) {
                Some(n) => *n += 1,
                None => {
                    self.active_evidence_kinds.insert(e.kind.clone(), 1);
                }
            }
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
            set_remove(&mut self.evidence_identity, &id.hash64(), idx);
        }
        if e.status == Status::Active {
            sub(&mut self.active_evidence_kinds, &e.kind);
        }
        Some(e)
    }

    // ---- citations ---------------------------------------------------------------------

    fn cites_index(&mut self, kind: TargetKind) -> &mut DenseIndex {
        match kind {
            TargetKind::Node => &mut self.cites_by_node,
            TargetKind::Link => &mut self.cites_by_link,
        }
    }

    pub fn put_citation(&mut self, c: Citation) -> Option<Citation> {
        let old = self.remove_citation(c.idx);
        self.cites_index(c.target_kind).add(c.target, c.idx);
        self.cites_by_evidence.add(c.evidence, c.idx);
        if c.status == Status::Active {
            self.active_citations += 1;
        }
        self.citations.insert(c.idx, c);
        old
    }

    pub fn remove_citation(&mut self, idx: i64) -> Option<Citation> {
        let c = self.citations.remove(&idx)?;
        self.cites_index(c.target_kind).remove(c.target, idx);
        self.cites_by_evidence.remove(c.evidence, idx);
        if c.status == Status::Active {
            self.active_citations -= 1;
        }
        Some(c)
    }

    // ---- queries -----------------------------------------------------------------------

    /// Citations (any status) of a node or link.
    pub fn cites_of(&self, kind: TargetKind, idx: i64) -> Option<&IdSet> {
        match kind {
            TargetKind::Node => self.cites_by_node.get(&idx),
            TargetKind::Link => self.cites_by_link.get(&idx),
        }
    }

    /// The active link with this identity, found through the endpoint with fewer links.
    pub fn find_link(&self, id: &LinkIdentity) -> Option<i64> {
        let (a, b) = (self.adjacency.get(&id.a), self.adjacency.get(&id.b));
        let smaller = match (a, b) {
            (Some(a), Some(b)) => Some(if a.len() <= b.len() { a } else { b }),
            _ => None,
        }?;
        smaller
            .iter()
            .copied()
            .find(|l| self.links.get(l).is_some_and(|l| id.matches(l)))
    }

    /// The lowest-numbered active evidence with this identity.
    pub fn find_evidence(&self, id: &EvidenceIdentity) -> Option<i64> {
        self.evidence_identity
            .get(&id.hash64())?
            .iter()
            .copied()
            .find(|e| EvidenceIdentity::of(&self.evidence[e]).as_ref() == Some(id))
    }

    /// The active citation with this identity.
    pub fn find_citation(&self, id: &CitationIdentity) -> Option<i64> {
        self.cites_of(id.target_kind, id.target)?
            .iter()
            .copied()
            .find(|c| id.matches(&self.citations[c]))
    }

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
        self.cites_of(kind, idx).is_some_and(|cs| {
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
