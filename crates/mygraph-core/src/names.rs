//! Name index: active nodes by name key ([NORM-NAME-7]), ordered for prefix search, with a
//! trigram index for fuzzy search ([RANK-2], [RANK-3]). Used by write matching and by `search`.

use crate::table::IdSet;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

/// [RANK-2] trigrams of a name key, sorted and without duplicates.
pub fn trigrams(key: &str) -> Vec<[char; 3]> {
    let mut out = Vec::new();
    for w in key.split(' ').filter(|w| !w.is_empty()) {
        let cs: Vec<char> = "  ".chars().chain(w.chars()).chain([' ']).collect();
        out.extend(cs.windows(3).map(|t| [t[0], t[1], t[2]]));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// [RANK-3]
pub fn trigram_similarity(a: &str, b: &str) -> f64 {
    let (ta, tb) = (trigrams(a), trigrams(b));
    let shared = ta.iter().filter(|t| tb.binary_search(t).is_ok()).count();
    let union = ta.len() + tb.len() - shared;
    if union == 0 {
        0.0
    } else {
        shared as f64 / union as f64
    }
}

thread_local! {
    /// Per-key shared-trigram counters, reused across fuzzy queries on this thread; every
    /// query leaves them at zero.
    static COUNTS: std::cell::RefCell<Vec<u16>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn pack(t: &[char; 3]) -> u64 {
    (u64::from(t[0]) << 42) | (u64::from(t[1]) << 21) | u64::from(t[2])
}

#[derive(Default)]
struct Entry {
    nodes: IdSet,
    /// Number in the trigram index.
    id: u32,
}

#[derive(Default)]
pub struct NameIndex {
    map: BTreeMap<Box<str>, Entry>,
    /// Trigram index over the keys of `map`: entry number -> key and trigram count.
    keys: Vec<Option<Box<str>>>,
    sizes: Vec<u16>,
    free: Vec<u32>,
    postings: HashMap<u64, Vec<u32>>,
}

impl NameIndex {
    pub fn get(&self, key: &str) -> Option<&IdSet> {
        self.map.get(key).map(|e| &e.nodes)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// Number of distinct name keys.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn add(&mut self, key: String, node: i64) {
        if let Some(e) = self.map.get_mut(key.as_str()) {
            e.nodes.insert(node);
            return;
        }
        let key = key.into_boxed_str();
        let id = self.index_key(&key);
        let mut nodes = IdSet::default();
        nodes.insert(node);
        self.map.insert(key, Entry { nodes, id });
    }

    pub fn remove(&mut self, key: &str, node: i64) {
        let Some(e) = self.map.get_mut(key) else {
            return;
        };
        e.nodes.remove(&node);
        if e.nodes.is_empty() {
            let id = e.id;
            self.map.remove(key);
            self.unindex(id);
        }
    }

    fn index_key(&mut self, key: &str) -> u32 {
        let id = self.free.pop().unwrap_or(self.keys.len() as u32);
        let tri = trigrams(key);
        if id as usize == self.keys.len() {
            self.keys.push(Some(key.into()));
            self.sizes.push(tri.len() as u16);
        } else {
            self.keys[id as usize] = Some(key.into());
            self.sizes[id as usize] = tri.len() as u16;
        }
        for t in &tri {
            let list = self.postings.entry(pack(t)).or_default();
            match list.last() {
                Some(&last) if last > id => {
                    let at = list.binary_search(&id).unwrap_err();
                    list.insert(at, id);
                }
                _ => list.push(id),
            }
        }
        id
    }

    fn unindex(&mut self, id: u32) {
        let Some(key) = self.keys[id as usize].take() else {
            return;
        };
        for t in trigrams(&key) {
            let p = pack(&t);
            if let Some(list) = self.postings.get_mut(&p) {
                if let Ok(at) = list.binary_search(&id) {
                    list.remove(at);
                }
                if list.is_empty() {
                    self.postings.remove(&p);
                }
            }
        }
        self.free.push(id);
    }

    /// Keys longer than `q` that start with it, in key order ([RANK-5]).
    pub fn with_prefix<'a>(
        &'a self,
        q: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a IdSet)> + 'a {
        self.map
            .range::<str, _>((std::ops::Bound::Excluded(q), std::ops::Bound::Unbounded))
            .take_while(move |(k, _)| k.starts_with(q))
            .map(|(k, e)| (&**k, &e.nodes))
    }

    /// Keys whose trigram similarity with `q` is at least `threshold` ([RANK-3]).
    pub fn fuzzy(&self, q: &str, threshold: f64) -> Vec<(&str, f64)> {
        let tri = trigrams(q);
        if tri.is_empty() {
            return Vec::new();
        }
        // A key with similarity >= s shares at least `need` = ceil(s * |q|) trigrams with q, so
        // it appears in at least one of the |q| - need + 1 rarest posting lists (prefix
        // filtering). Those lists produce the candidates; the common lists only add counts.
        let need = ((threshold * tri.len() as f64).ceil() as usize).max(1);
        let mut lists: Vec<&[u32]> = tri
            .iter()
            .map(|t| self.postings.get(&pack(t)).map_or(&[][..], Vec::as_slice))
            .collect();
        lists.sort_by_key(|l| l.len());
        let probe = tri.len() - need + 1;
        COUNTS.with_borrow_mut(|counts| {
            if counts.len() < self.keys.len() {
                counts.resize(self.keys.len(), 0);
            }
            let mut touched = Vec::new();
            for list in &lists[..probe] {
                for &id in *list {
                    let c = &mut counts[id as usize];
                    if *c == 0 {
                        touched.push(id);
                    }
                    *c += 1;
                }
            }
            for list in &lists[probe..] {
                if list.len() <= touched.len() * 8 {
                    for &id in *list {
                        if counts[id as usize] > 0 {
                            counts[id as usize] += 1;
                        }
                    }
                } else {
                    for &id in &touched {
                        if list.binary_search(&id).is_ok() {
                            counts[id as usize] += 1;
                        }
                    }
                }
            }
            let mut out = Vec::new();
            for &id in &touched {
                let shared = std::mem::take(&mut counts[id as usize]) as usize;
                if shared < need {
                    continue;
                }
                let union = tri.len() + self.sizes[id as usize] as usize - shared;
                let sim = shared as f64 / union as f64;
                if sim >= threshold
                    && let Some(k) = &self.keys[id as usize]
                {
                    out.push((&**k, sim));
                }
            }
            out
        })
    }
}

impl PartialEq for NameIndex {
    fn eq(&self, other: &Self) -> bool {
        self.map.len() == other.map.len()
            && self
                .map
                .iter()
                .zip(&other.map)
                .all(|((ka, a), (kb, b))| ka == kb && a.nodes == b.nodes)
    }
}

impl fmt::Debug for NameIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.map.iter().map(|(k, e)| (k, &e.nodes)))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_and_prefix_follow_adds_and_removes() {
        let mut n = NameIndex::default();
        n.add("charles babbage".into(), 1);
        n.add("ada lovelace".into(), 2);
        n.add("ada".into(), 3);
        let hits = n.fuzzy("charles babage", 0.3);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "charles babbage");
        let p: Vec<_> = n.with_prefix("ada").map(|(k, _)| k).collect();
        assert_eq!(p, vec!["ada lovelace"]);
        n.remove("charles babbage", 1);
        assert!(n.fuzzy("charles babage", 0.3).is_empty());
        n.add("charles babbage".into(), 4); // reuses the freed trigram entry
        assert_eq!(n.fuzzy("charles babage", 0.3).len(), 1);
        assert_eq!(n.get("charles babbage").unwrap().as_slice(), &[4]);
    }
}
