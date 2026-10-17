//! Maps and sets of whole numbers, through one copy of the hash table code.
//!
//! Every `HashMap<K, V>` with a key and a value type of its own is compiled into a copy of the table's code (growing, inserting,
//! looking up, dropping: 1.5 to 2.5 KB each in the code that is built for speed, and the engine had about 40 of them). Here the table
//! only ever holds `u64 -> u32`: the number that stands for the key and the place of the entry in a `Vec` that holds the keys and the
//! values. All the maps of numbers share the one table type, and only the thin layer of `Vec` code is compiled for each value type.
//! Iteration is in the order of insertion (a removal moves the last entry into the hole), which is a fixed order, where the order of
//! a `HashMap` changes from run to run: nothing depends on either.
//!
//! The hasher is the randomized one, as the `HashMap`s had: a file cannot choose keys that pile up in one place.

use std::collections::HashMap;

use crate::object::ObjRef;

/// A key that stands for a whole number: the same key, the same number; different keys, different numbers.
pub trait IdKey: Copy {
    fn id(self) -> u64;
}

macro_rules! id_keys {
    ($($t:ty),*) => {$(
        impl IdKey for $t {
            fn id(self) -> u64 {
                self as u64
            }
        }
    )*};
}

id_keys!(u8, u16, u32, u64, usize, char);

impl IdKey for ObjRef {
    fn id(self) -> u64 {
        u64::from(self.num) | (u64::from(self.generation) << 32)
    }
}

/// The table: the key's number to the place of the entry. It is not generic, so that its code is made once and not once for each
/// map, and its calls are not inlined into every user for the same reason.
#[derive(Clone)]
struct Index {
    places: HashMap<u64, u32>,
}

impl Index {
    fn new() -> Self {
        Index { places: HashMap::new() }
    }

    fn with_capacity(room: usize) -> Self {
        Index { places: HashMap::with_capacity(room) }
    }

    #[inline(never)]
    fn find(&self, id: u64) -> Option<usize> {
        self.places.get(&id).map(|&at| at as usize)
    }

    /// `id` is at `place` from now on.
    #[inline(never)]
    fn put(&mut self, id: u64, place: usize) {
        self.places.insert(id, u32::try_from(place).unwrap_or(u32::MAX));
    }

    #[inline(never)]
    fn take(&mut self, id: u64) -> Option<usize> {
        self.places.remove(&id).map(|at| at as usize)
    }

    #[inline(never)]
    fn clear(&mut self) {
        self.places.clear();
    }
}

/// A map from whole-number keys to values.
#[derive(Clone)]
pub struct IdMap<K, V> {
    index: Index,
    entries: Vec<(K, V)>,
}

impl<K, V> Default for IdMap<K, V> {
    fn default() -> Self {
        IdMap { index: Index::new(), entries: Vec::new() }
    }
}

impl<K: IdKey, V> IdMap<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(room: usize) -> Self {
        IdMap { index: Index::with_capacity(room), entries: Vec::with_capacity(room) }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.index.clear();
        self.entries.clear();
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.entries.get(self.index.find(key.id())?).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let at = self.index.find(key.id())?;
        self.entries.get_mut(at).map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.index.find(key.id()).is_some()
    }

    /// Puts `value` at `key`; the value that was there is handed back.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        if let Some(at) = self.index.find(key.id()) {
            return self.entries.get_mut(at).map(|(_, old)| std::mem::replace(old, value));
        }
        self.index.put(key.id(), self.entries.len());
        self.entries.push((key, value));
        None
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let at = self.index.take(key.id())?;
        if at >= self.entries.len() {
            return None;
        }
        let (_, value) = self.entries.swap_remove(at);
        // The entry that was last now stands where the removed one was.
        if let Some((moved, _)) = self.entries.get(at) {
            self.index.put(moved.id(), at);
        }
        Some(value)
    }

    /// Changes the value at `key` with `change`; where there is none, `change` is given `start` and what comes out is put at `key`
    /// (what `entry(key).or_insert(start)` followed by a change did).
    pub fn update(&mut self, key: K, start: V, change: impl FnOnce(&mut V)) {
        if let Some(at) = self.index.find(key.id()) {
            if let Some((_, value)) = self.entries.get_mut(at) {
                change(value);
            }
            return;
        }
        let mut value = start;
        change(&mut value);
        self.insert(key, value);
    }

    /// Puts `value` at `key` unless there is a value there already (the first one stays).
    pub fn insert_first(&mut self, key: K, value: V) {
        if !self.contains_key(&key) {
            self.insert(key, value);
        }
    }

    /// Keeps the entries for which `keep` says yes.
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        self.entries.retain_mut(|(k, v)| keep(k, v));
        self.index.clear();
        for (at, (k, _)) in self.entries.iter().enumerate() {
            self.index.put(k.id(), at);
        }
    }

    /// Puts every entry of `other` in; where a key is in both, `other`'s value stays.
    pub fn extend(&mut self, other: Self) {
        for (k, v) in other.entries {
            self.insert(k, v);
        }
    }

    /// Takes every entry out: the map is empty afterwards.
    pub fn drain(&mut self) -> std::vec::IntoIter<(K, V)> {
        self.index.clear();
        std::mem::take(&mut self.entries).into_iter()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, v)| v)
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.entries.iter_mut().map(|(_, v)| v)
    }

    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.iter().map(|(k, _)| k)
    }
}

impl<K, V> std::fmt::Debug for IdMap<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdMap").field("len", &self.entries.len()).finish()
    }
}

impl<K, V> IntoIterator for IdMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<(K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a, K, V> IntoIterator for &'a IdMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::iter::Map<std::slice::Iter<'a, (K, V)>, fn(&'a (K, V)) -> (&'a K, &'a V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter().map(|(k, v)| (k, v))
    }
}

impl<K: IdKey, V> FromIterator<(K, V)> for IdMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(pairs: I) -> Self {
        let mut map = Self::new();
        for (k, v) in pairs {
            map.insert(k, v);
        }
        map
    }
}

/// A set of whole-number keys (the same table as the maps', and the keys in a `Vec`).
#[derive(Clone)]
pub struct IdSet<K> {
    index: Index,
    keys: Vec<K>,
}

impl<K> Default for IdSet<K> {
    fn default() -> Self {
        IdSet { index: Index::new(), keys: Vec::new() }
    }
}

impl<K: IdKey> IdSet<K> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn clear(&mut self) {
        self.index.clear();
        self.keys.clear();
    }

    /// Adds `key`; true when it was not in the set.
    pub fn insert(&mut self, key: K) -> bool {
        if self.index.find(key.id()).is_some() {
            return false;
        }
        self.index.put(key.id(), self.keys.len());
        self.keys.push(key);
        true
    }

    pub fn contains(&self, key: &K) -> bool {
        self.index.find(key.id()).is_some()
    }

    pub fn remove(&mut self, key: &K) -> bool {
        let Some(at) = self.index.take(key.id()) else { return false };
        if at >= self.keys.len() {
            return false;
        }
        self.keys.swap_remove(at);
        // The key that was last now stands where the removed one was.
        if let Some(moved) = self.keys.get(at) {
            self.index.put(moved.id(), at);
        }
        true
    }

    pub fn iter(&self) -> impl Iterator<Item = &K> {
        self.keys.iter()
    }
}

impl<K: IdKey> FromIterator<K> for IdSet<K> {
    fn from_iter<I: IntoIterator<Item = K>>(keys: I) -> Self {
        let mut set = Self::new();
        for k in keys {
            set.insert(k);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_map_holds_what_it_is_given_and_hands_back_what_it_replaces() {
        let mut m: IdMap<u32, String> = IdMap::new();
        assert!(m.is_empty());
        assert_eq!(m.insert(7, "seven".to_string()), None);
        assert_eq!(m.insert(9, "nine".to_string()), None);
        assert_eq!(m.insert(7, "SEVEN".to_string()), Some("seven".to_string()));
        assert_eq!(m.len(), 2);
        assert_eq!(m.get(&7).map(String::as_str), Some("SEVEN"));
        assert_eq!(m.get(&8), None);
        assert!(m.contains_key(&9) && !m.contains_key(&10));
        if let Some(v) = m.get_mut(&9) {
            v.push('!');
        }
        assert_eq!(m.get(&9).map(String::as_str), Some("nine!"));
        assert_eq!(m.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![7, 9], "in the order of insertion");
    }

    #[test]
    fn removing_keeps_every_other_entry_reachable() {
        let mut m: IdMap<usize, usize> = IdMap::new();
        for k in 0..100 {
            m.insert(k, k * 10);
        }
        // From the front, the middle and the end: the last entry is moved into each hole.
        for k in [0, 50, 99, 1, 98, 49] {
            assert_eq!(m.remove(&k), Some(k * 10));
            assert_eq!(m.remove(&k), None);
        }
        assert_eq!(m.len(), 94);
        for k in 0..100 {
            let gone = [0, 50, 99, 1, 98, 49].contains(&k);
            assert_eq!(m.get(&k).copied(), if gone { None } else { Some(k * 10) }, "key {k}");
        }
        assert_eq!(m.values().sum::<usize>(), (0..100).filter(|k| ![0, 50, 99, 1, 98, 49].contains(k)).map(|k| k * 10).sum::<usize>());
        // Insert again after removing.
        m.insert(50, 1);
        assert_eq!(m.get(&50), Some(&1));
    }

    #[test]
    fn retain_clear_and_the_value_iterators() {
        let mut m: IdMap<u8, u32> = IdMap::new();
        for k in 0..=255u8 {
            m.insert(k, u32::from(k));
        }
        assert_eq!(m.len(), 256);
        m.retain(|k, v| {
            *v += 1;
            k % 2 == 0
        });
        assert_eq!(m.len(), 128);
        assert_eq!(m.get(&4), Some(&5));
        assert_eq!(m.get(&5), None);
        for v in m.values_mut() {
            *v = 0;
        }
        assert_eq!(m.values().sum::<u32>(), 0);
        assert_eq!(m.keys().count(), 128);
        m.clear();
        assert!(m.is_empty() && m.get(&4).is_none());
    }

    #[test]
    fn object_references_with_different_generations_are_different_keys() {
        let mut m: IdMap<ObjRef, &str> = IdMap::new();
        m.insert(ObjRef::new(5, 0), "a");
        m.insert(ObjRef::new(5, 1), "b");
        m.insert(ObjRef::new(6, 0), "c");
        assert_eq!(m.len(), 3);
        assert_eq!(m.get(&ObjRef::new(5, 1)), Some(&"b"));
        assert_eq!(m.remove(&ObjRef::new(5, 0)), Some("a"));
        assert_eq!(m.get(&ObjRef::new(5, 1)), Some(&"b"));
        assert_eq!(m.get(&ObjRef::new(5, 0)), None);
    }

    #[test]
    fn a_set_says_whether_the_key_was_new() {
        let mut s: IdSet<ObjRef> = IdSet::new();
        assert!(s.insert(ObjRef::new(1, 0)));
        assert!(!s.insert(ObjRef::new(1, 0)));
        assert!(s.insert(ObjRef::new(1, 2)));
        assert!(s.contains(&ObjRef::new(1, 2)) && !s.contains(&ObjRef::new(2, 0)));
        assert!(s.remove(&ObjRef::new(1, 0)) && !s.remove(&ObjRef::new(1, 0)));
        assert_eq!(s.len(), 1);
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn many_keys_that_a_cheap_hash_would_pile_up_still_work() {
        // Multiples of a big power of two: the numbers differ only in their high bits.
        let mut m: IdMap<u64, u64> = IdMap::new();
        for k in 0..5000u64 {
            m.insert(k << 20, k);
        }
        assert_eq!(m.len(), 5000);
        assert!((0..5000u64).all(|k| m.get(&(k << 20)) == Some(&k)));
    }
}
