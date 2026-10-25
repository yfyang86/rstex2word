//! An insertion-ordered map with Python `dict` semantics.
//!
//! Iteration follows first-insertion order and re-assigning an existing key
//! updates its value in place (it does not move to the end) — exactly how the
//! Python reference's dicts behave, which matters wherever the port iterates a
//! map (label numbering, CSL field order, the round-trip manifest).

use crate::json::{FromJson, JsonError, ToJson, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Dict<V> {
    entries: Vec<(String, V)>,
}

impl<V> Default for Dict<V> {
    fn default() -> Self {
        Dict {
            entries: Vec::new(),
        }
    }
}

impl<V> Dict<V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn position(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|(k, _)| k == key)
    }

    /// `d[key] = value`: update in place, or append a new key.
    pub fn insert(&mut self, key: impl Into<String>, value: V) {
        let key = key.into();
        match self.position(&key) {
            Some(i) => self.entries[i].1 = value,
            None => self.entries.push((key, value)),
        }
    }

    /// `d.setdefault(key, value)`: insert only when absent; returns the stored value.
    pub fn set_default(&mut self, key: impl Into<String>, value: V) -> &mut V {
        let key = key.into();
        let i = match self.position(&key) {
            Some(i) => i,
            None => {
                self.entries.push((key, value));
                self.entries.len() - 1
            }
        };
        &mut self.entries[i].1
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.position(key).map(|i| &self.entries[i].1)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.position(key).map(move |i| &mut self.entries[i].1)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.position(key).is_some()
    }

    /// `d.pop(key, None)`.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        self.position(key).map(|i| self.entries.remove(i).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut V)> {
        self.entries.iter_mut().map(|(k, v)| (&*k, v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.iter().map(|(k, _)| k)
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, v)| v)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl<V> FromIterator<(String, V)> for Dict<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        let mut d = Dict::new();
        for (k, v) in iter {
            d.insert(k, v);
        }
        d
    }
}

impl<V> IntoIterator for Dict<V> {
    type Item = (String, V);
    type IntoIter = std::vec::IntoIter<(String, V)>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<V: ToJson> ToJson for Dict<V> {
    fn to_json(&self) -> Value {
        Value::Object(
            self.entries
                .iter()
                .map(|(k, v)| (k.clone(), v.to_json()))
                .collect(),
        )
    }
}

impl<V: FromJson> FromJson for Dict<V> {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        let obj = v
            .as_object()
            .ok_or_else(|| JsonError::new("expected an object"))?;
        obj.iter()
            .map(|(k, v)| Ok((k.clone(), V::from_json(v)?)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reassignment_keeps_first_position() {
        let mut d = Dict::new();
        d.insert("a", 1);
        d.insert("b", 2);
        d.insert("a", 3);
        let got: Vec<_> = d.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(got, vec![("a", 3), ("b", 2)]);
    }
}
