//! Compact containers for the in-memory indexes (docs/05-architecture.md §5).
//!
//! Internal numbers are assigned densely from 1 ([ID-LIFE-2]), so records and per-object index
//! entries live in vectors indexed by number instead of hash maps: no hashing, no stored keys,
//! no load-factor slack.

use std::fmt;

/// Records by internal number. Behaves like a map from `i64` to `T`.
#[derive(Clone)]
pub struct Table<T> {
    slots: Vec<Option<T>>,
    len: usize,
}

impl<T> Default for Table<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            len: 0,
        }
    }
}

fn slot(idx: i64) -> Option<usize> {
    usize::try_from(idx).ok()
}

impl<T> Table<T> {
    pub fn get(&self, idx: &i64) -> Option<&T> {
        self.slots.get(slot(*idx)?)?.as_ref()
    }

    pub fn contains_key(&self, idx: &i64) -> bool {
        self.get(idx).is_some()
    }

    pub fn insert(&mut self, idx: i64, value: T) -> Option<T> {
        let i = slot(idx).expect("internal numbers are not negative");
        if i >= self.slots.len() {
            self.slots.resize_with(i + 1, || None);
        }
        let old = self.slots[i].replace(value);
        if old.is_none() {
            self.len += 1;
        }
        old
    }

    pub fn remove(&mut self, idx: &i64) -> Option<T> {
        let old = self.slots.get_mut(slot(*idx)?)?.take();
        if old.is_some() {
            self.len -= 1;
        }
        old
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Entries in ascending number order.
    pub fn iter(&self) -> impl Iterator<Item = (i64, &T)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.as_ref().map(|v| (i as i64, v)))
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().flatten()
    }

    /// One past the highest number with a slot: the length of a dense per-record array.
    pub fn end(&self) -> usize {
        self.slots.len()
    }

    /// Reserves room for numbers up to `max_idx`.
    pub fn reserve_to(&mut self, max_idx: i64) {
        if let Some(n) = slot(max_idx) {
            self.slots.reserve((n + 1).saturating_sub(self.slots.len()));
        }
    }
}

impl<T> std::ops::Index<&i64> for Table<T> {
    type Output = T;
    fn index(&self, idx: &i64) -> &T {
        self.get(idx).expect("no record with this number")
    }
}

impl<T: PartialEq> PartialEq for Table<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<T: fmt::Debug> fmt::Debug for Table<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

/// A sorted set of internal numbers. One element is stored inline, without a heap allocation;
/// most citation targets have exactly one citation.
#[derive(Clone, PartialEq, Eq)]
pub struct IdSet(Repr);

#[derive(Clone, PartialEq, Eq)]
enum Repr {
    One(i64),
    Many(Vec<i64>),
}

impl Default for IdSet {
    fn default() -> Self {
        Self(Repr::Many(Vec::new()))
    }
}

impl IdSet {
    pub fn as_slice(&self) -> &[i64] {
        match &self.0 {
            Repr::One(x) => std::slice::from_ref(x),
            Repr::Many(v) => v,
        }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, i64> {
        self.as_slice().iter()
    }

    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }

    pub fn contains(&self, x: &i64) -> bool {
        self.as_slice().binary_search(x).is_ok()
    }

    /// Returns whether `x` was added.
    pub fn insert(&mut self, x: i64) -> bool {
        match &mut self.0 {
            Repr::Many(v) if v.is_empty() => {
                self.0 = Repr::One(x);
                true
            }
            Repr::One(y) if *y == x => false,
            Repr::One(y) => {
                let y = *y;
                self.0 = Repr::Many(if y < x { vec![y, x] } else { vec![x, y] });
                true
            }
            Repr::Many(v) => match v.binary_search(&x) {
                Ok(_) => false,
                Err(at) => {
                    v.insert(at, x);
                    true
                }
            },
        }
    }

    /// Returns whether `x` was present.
    pub fn remove(&mut self, x: &i64) -> bool {
        match &mut self.0 {
            Repr::One(y) if y == x => {
                self.0 = Repr::Many(Vec::new());
                true
            }
            Repr::One(_) => false,
            Repr::Many(v) => match v.binary_search(x) {
                Ok(at) => {
                    v.remove(at);
                    if v.len() == 1 {
                        self.0 = Repr::One(v[0]);
                    } else if v.len() * 4 < v.capacity() {
                        v.shrink_to_fit();
                    }
                    true
                }
                Err(_) => false,
            },
        }
    }
}

impl<'a> IntoIterator for &'a IdSet {
    type Item = &'a i64;
    type IntoIter = std::slice::Iter<'a, i64>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl fmt::Debug for IdSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

/// A set of numbers per internal number (adjacency, citations by target or by evidence).
#[derive(Clone, Default)]
pub struct DenseIndex(Vec<IdSet>);

impl PartialEq for DenseIndex {
    fn eq(&self, other: &Self) -> bool {
        let n = self.0.len().max(other.0.len()) as i64;
        (0..n).all(|i| self.get(&i) == other.get(&i))
    }
}

impl DenseIndex {
    /// The set for `idx`, or `None` when it is empty.
    pub fn get(&self, idx: &i64) -> Option<&IdSet> {
        self.0.get(slot(*idx)?).filter(|s| !s.is_empty())
    }

    pub fn add(&mut self, idx: i64, x: i64) {
        let i = slot(idx).expect("internal numbers are not negative");
        if i >= self.0.len() {
            self.0.resize_with(i + 1, IdSet::default);
        }
        self.0[i].insert(x);
    }

    pub fn remove(&mut self, idx: i64, x: i64) {
        if let Some(s) = slot(idx).and_then(|i| self.0.get_mut(i)) {
            s.remove(&x);
        }
    }

    /// Number of non-empty sets.
    pub fn len(&self) -> usize {
        self.0.iter().filter(|s| !s.is_empty()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for DenseIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.iter().enumerate().filter(|(_, s)| !s.is_empty()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_set_stays_sorted_and_inline_for_one() {
        let mut s = IdSet::default();
        assert!(s.insert(5));
        assert!(matches!(s.0, Repr::One(5)));
        assert!(s.insert(2) && s.insert(9) && !s.insert(5));
        assert_eq!(s.as_slice(), &[2, 5, 9]);
        assert!(s.remove(&5) && !s.remove(&5) && s.remove(&9));
        assert!(matches!(s.0, Repr::One(2)));
        assert!(s.remove(&2) && s.is_empty());
        assert_eq!(size_of::<IdSet>(), 24);
    }

    #[test]
    fn table_behaves_like_a_map() {
        let mut t = Table::default();
        assert_eq!(t.insert(3, "c"), None);
        assert_eq!(t.insert(1, "a"), None);
        assert_eq!(t.insert(3, "C"), Some("c"));
        assert_eq!(t.len(), 2);
        assert_eq!(t.iter().collect::<Vec<_>>(), vec![(1, &"a"), (3, &"C")]);
        assert_eq!(t.remove(&1), Some("a"));
        assert_eq!(t.get(&1), None);
        assert_eq!(t.get(&-1), None);
        assert_eq!(t.len(), 1);
    }
}
