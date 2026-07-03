//! A minimal insertion-ordered string map. Mirrors the subset of
//! `java.util.Map` semantics the xhtml code relies on, but with INSERTION
//! order for iteration (see `node.rs` module docs for why this is the correct
//! choice for the C3 byte-parity substrate).
//!
//! `put` on an existing key updates the value in place WITHOUT changing its
//! position — matching `java.util.LinkedHashMap` (and the observable behavior
//! we need: a re-`put` of the same attribute keeps its original position).

#[derive(Debug, Clone, Default)]
pub struct OrderMap<K, V> {
    entries: Vec<(K, V)>,
}

impl<K: PartialEq, V> OrderMap<K, V> {
    pub fn new() -> Self {
        OrderMap {
            entries: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Java `Map.put`: insert or update-in-place; position preserved on update.
    pub fn put(&mut self, key: K, value: V) {
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.entries.push((key, value));
        }
    }

    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        self.entries
            .iter()
            .find(|(k, _)| k.borrow() == key)
            .map(|(_, v)| v)
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        self.entries.iter().any(|(k, _)| k.borrow() == key)
    }

    /// Java `Map.remove`.
    pub fn remove<Q>(&mut self, key: &Q)
    where
        K: std::borrow::Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        self.entries.retain(|(k, _)| k.borrow() != key);
    }

    /// Iterate in insertion order (Java `keySet()` iteration order for us).
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }
}
