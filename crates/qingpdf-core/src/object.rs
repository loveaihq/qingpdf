//! The PDF object model (ISO 32000-1 7.3).

/// An indirect reference `num gen R` (7.3.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjRef {
    pub num: u32,
    pub generation: u16,
}

impl ObjRef {
    pub const fn new(num: u32, generation: u16) -> Self {
        ObjRef { num, generation }
    }
}

/// A name object (7.3.5), stored as raw bytes after `#xx` escapes are decoded,
/// without the leading `/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(pub Vec<u8>);

impl Name {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Name(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<&str> for Name {
    fn from(s: &str) -> Self {
        Name(s.as_bytes().to_vec())
    }
}

impl PartialEq<str> for Name {
    fn eq(&self, other: &str) -> bool {
        self.0 == other.as_bytes()
    }
}

impl PartialEq<&str> for Name {
    fn eq(&self, other: &&str) -> bool {
        self.0 == other.as_bytes()
    }
}

/// A string object (7.3.4): the decoded bytes, plus whether it was written in
/// hexadecimal form so it can be written back the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfString {
    pub bytes: Vec<u8>,
    pub hex: bool,
}

impl PdfString {
    pub fn literal(bytes: impl Into<Vec<u8>>) -> Self {
        PdfString { bytes: bytes.into(), hex: false }
    }

    pub fn hex(bytes: impl Into<Vec<u8>>) -> Self {
        PdfString { bytes: bytes.into(), hex: true }
    }
}

/// A dictionary (7.3.7). Keeps the order entries appeared in the file so that
/// round trips are easy to diff. Dictionaries are small, so lookup is linear.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dict(Vec<(Name, Object)>);

impl Dict {
    pub fn new() -> Self {
        Dict(Vec::new())
    }

    pub fn get(&self, key: &str) -> Option<&Object> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Object> {
        self.0.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Insert or replace. A `Null` value is the same as the key being absent
    /// (7.3.7), so inserting `Null` removes the key.
    pub fn set(&mut self, key: impl Into<Name>, value: Object) {
        let key = key.into();
        if matches!(value, Object::Null) {
            self.0.retain(|(k, _)| k != &key);
            return;
        }
        match self.0.iter_mut().find(|(k, _)| k == &key) {
            Some(slot) => slot.1 = value,
            None => self.0.push((key, value)),
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Object> {
        let pos = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(pos).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Name, &Object)> {
        self.0.iter().map(|(k, v)| (k, v))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&Name, &mut Object)> {
        self.0.iter_mut().map(|(k, v)| (&*k, v))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Convenience: the value as a name, if it is one.
    pub fn get_name(&self, key: &str) -> Option<&Name> {
        match self.get(key) {
            Some(Object::Name(n)) => Some(n),
            _ => None,
        }
    }

    /// Convenience: the value as an integer, if it is one.
    pub fn get_int(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Some(Object::Integer(i)) => Some(*i),
            _ => None,
        }
    }
}

impl From<&str> for Object {
    fn from(name: &str) -> Self {
        Object::Name(Name::from(name))
    }
}

impl FromIterator<(Name, Object)> for Dict {
    fn from_iter<I: IntoIterator<Item = (Name, Object)>>(iter: I) -> Self {
        let mut d = Dict::new();
        for (k, v) in iter {
            d.set(k, v);
        }
        d
    }
}

/// A stream (7.3.8): its dictionary and its raw, still-encoded bytes.
/// Filters are applied only when someone asks for the decoded data.
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub dict: Dict,
    pub data: Vec<u8>,
}

/// Any PDF object (7.3).
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    String(PdfString),
    Name(Name),
    Array(Vec<Object>),
    Dict(Dict),
    Stream(Stream),
    Ref(ObjRef),
}

impl Object {
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Object::Dict(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&Name> {
        match self {
            Object::Name(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Object::Integer(i) => Some(*i),
            _ => None,
        }
    }

    /// Integers and reals both count as numbers (7.3.3).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Object::Integer(i) => Some(*i as f64),
            Object::Real(r) => Some(*r),
            _ => None,
        }
    }

    pub fn as_obj_ref(&self) -> Option<ObjRef> {
        match self {
            Object::Ref(r) => Some(*r),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_null_removes_key() {
        let mut d = Dict::new();
        d.set("Type", Object::from("Page"));
        assert!(d.contains_key("Type"));
        d.set("Type", Object::Null);
        assert!(!d.contains_key("Type"));
    }

    #[test]
    fn set_replaces_in_place() {
        let mut d = Dict::new();
        d.set("A", Object::Integer(1));
        d.set("B", Object::Integer(2));
        d.set("A", Object::Integer(3));
        let keys: Vec<_> = d.iter().map(|(k, _)| k.as_bytes().to_vec()).collect();
        assert_eq!(keys, vec![b"A".to_vec(), b"B".to_vec()]);
        assert_eq!(d.get_int("A"), Some(3));
    }
}
