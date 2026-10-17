//! Maps from names (strings and byte strings), through one copy of the hash table code for each kind of key.
//!
//! The same reason as [`crate::idmap`]: a `HashMap<K, V>` compiles into a copy of the table's code for each pair of key and value types
//! (1.4 to 1.9 KB each in the code built for speed). Here the table holds `K -> u32` only, the place of the value in a `Vec`, so that
//! the maps of one kind of key share it, and only the thin layer of `Vec` code is compiled for each value type. The keys are in the
//! table, not in the `Vec`, so a key is not kept twice. Nothing is taken out of a map (the maps of these names only grow until they are
//! dropped), which is why there is no `remove`.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash};

/// A map from names to values.
#[derive(Clone)]
pub struct KeyMap<K, V, S = RandomState> {
    places: HashMap<K, u32, S>,
    values: Vec<V>,
}

impl<K, V, S: Default> Default for KeyMap<K, V, S> {
    fn default() -> Self {
        KeyMap { places: HashMap::with_hasher(S::default()), values: Vec::new() }
    }
}

impl<K: Hash + Eq, V, S: BuildHasher + Default> KeyMap<K, V, S> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The place of the value at `key`. Not inlined: it is made once for each kind of key and not once for each map.
    #[inline(never)]
    fn place<Q: ?Sized + Hash + Eq>(&self, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
    {
        self.places.get(key).map(|&at| at as usize)
    }

    /// `key` stands for the value at `place` from now on.
    #[inline(never)]
    fn put(&mut self, key: K, place: usize) {
        self.places.insert(key, u32::try_from(place).unwrap_or(u32::MAX));
    }

    pub fn get<Q: ?Sized + Hash + Eq>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.values.get(self.place(key)?)
    }

    pub fn get_mut<Q: ?Sized + Hash + Eq>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        let at = self.place(key)?;
        self.values.get_mut(at)
    }

    pub fn contains_key<Q: ?Sized + Hash + Eq>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.place(key).is_some()
    }

    /// Puts `value` at `key`; the value that was there is handed back.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        if let Some(at) = self.place(&key) {
            return self.values.get_mut(at).map(|old| std::mem::replace(old, value));
        }
        self.put(key, self.values.len());
        self.values.push(value);
        None
    }

    pub fn clear(&mut self) {
        self.places.clear();
        self.values.clear();
    }

    /// The values, in the order they were put in.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.values.iter()
    }

    /// The keys, in no order.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.places.keys()
    }
}

impl<K: Hash + Eq, V, S: BuildHasher + Default> FromIterator<(K, V)> for KeyMap<K, V, S> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(pairs: I) -> Self {
        let mut map = Self::new();
        for (k, v) in pairs {
            map.insert(k, v);
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_string_keys_are_found_by_a_slice_and_replaced_in_place() {
        let mut m: KeyMap<Vec<u8>, i32> = KeyMap::new();
        assert!(m.is_empty());
        assert_eq!(m.insert(b"F1".to_vec(), 1), None);
        assert_eq!(m.insert(b"F2".to_vec(), 2), None);
        assert_eq!(m.insert(b"F1".to_vec(), 10), Some(1));
        assert_eq!(m.len(), 2);
        assert_eq!(m.get(&b"F1"[..]), Some(&10));
        assert_eq!(m.get(&b"F3"[..]), None);
        assert!(m.contains_key(&b"F2"[..]));
        if let Some(v) = m.get_mut(&b"F2"[..]) {
            *v += 5;
        }
        assert_eq!(m.get(&b"F2"[..]), Some(&7));
        assert_eq!(m.values().copied().collect::<Vec<_>>(), vec![10, 7], "in the order they were put in");
        assert_eq!(m.keys().count(), 2);
    }

    #[test]
    fn string_keys_work_through_a_str_and_collect_from_pairs() {
        let m: KeyMap<String, usize> = ["a", "bb", "ccc", "bb"].iter().enumerate().map(|(i, s)| (s.to_string(), i)).collect();
        assert_eq!(m.len(), 3);
        assert_eq!(m.get("bb"), Some(&3), "the later one of a repeated key stays");
        assert_eq!(m.get("ccc"), Some(&2));
        assert_eq!(m.get("d"), None);
    }

    #[test]
    fn many_keys() {
        let mut m: KeyMap<Vec<u8>, usize> = KeyMap::new();
        for i in 0..3000usize {
            m.insert(format!("name{i}").into_bytes(), i);
        }
        assert_eq!(m.len(), 3000);
        assert!((0..3000usize).all(|i| m.get(format!("name{i}").as_bytes()) == Some(&i)));
    }
}
