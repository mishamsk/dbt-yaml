//! Shallow probing of a value's `Serialize` impl.
//!
//! The Value serializer flattens dunder-named struct fields into the parent
//! mapping, and an entry whose key is already present overwrites the earlier
//! one. Serializers that write as they go cannot take back an entry, so
//! before a struct is written it is probed: its own `Serialize` impl is run
//! against a serializer that records the shape of the value and, for structs
//! with dunder fields, the keys of all entries. Ordinary field values are
//! never visited. When a key occurs more than once, every occurrence but the
//! last is skipped while writing, so the surviving entry sits at its last
//! position rather than its first.
//!
//! This relies on `Serialize` impls making the same calls every time they
//! run.

use crate::error::Error;
use serde::ser::{self, Impossible, Serialize};
use std::collections::HashMap;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Expands the `Serializer` methods that the probing and flattening
/// serializers all treat alike.
macro_rules! scalars {
    ($ret:expr) => {
        $crate::probe::scalars!(@ $ret;
            serialize_bool: bool,
            serialize_i8: i8,
            serialize_i16: i16,
            serialize_i32: i32,
            serialize_i64: i64,
            serialize_i128: i128,
            serialize_u8: u8,
            serialize_u16: u16,
            serialize_u32: u32,
            serialize_u64: u64,
            serialize_u128: u128,
            serialize_f32: f32,
            serialize_f64: f64,
            serialize_bytes: &[u8],
            serialize_unit_struct: &'static str
        );

        fn serialize_unit(self) -> std::result::Result<Self::Ok, Self::Error> {
            $ret
        }

        fn serialize_none(self) -> std::result::Result<Self::Ok, Self::Error> {
            $ret
        }
    };
    (@ $ret:expr; $($method:ident: $ty:ty),*) => {
        $(
            fn $method(self, _: $ty) -> std::result::Result<Self::Ok, Self::Error> {
                $ret
            }
        )*
    };
}

pub(crate) use scalars;

/// The first call a value's `Serialize` impl makes on its serializer.
#[derive(Clone, Copy, Default)]
pub(crate) enum Shape {
    #[default]
    Other,
    Struct {
        name: &'static str,
        len: usize,
    },
    StructVariant {
        variant: &'static str,
        len: usize,
    },
    TupleVariant {
        variant: &'static str,
    },
    /// Only recorded when probing for an extra entry.
    Map,
}

/// Which entries of one struct are overwritten by a later entry.
pub(crate) struct Plan {
    /// Name and length of the struct, checked by the YAML serializer.
    #[cfg_attr(not(feature = "flatten_dunder"), allow(dead_code))]
    name: &'static str,
    #[cfg_attr(not(feature = "flatten_dunder"), allow(dead_code))]
    len: usize,
    skip: Vec<bool>,
}

impl Plan {
    fn new(name: &'static str, len: usize, keys: &[Key]) -> Option<Self> {
        let mut last = HashMap::with_capacity(keys.len());
        for (i, key) in keys.iter().enumerate() {
            if let Some(key) = key.as_str() {
                last.insert(key, i);
            }
        }
        let skip: Vec<bool> = keys
            .iter()
            .enumerate()
            .map(|(i, key)| key.as_str().is_some_and(|key| last[key] != i))
            .collect();
        skip.contains(&true).then_some(Plan { name, len, skip })
    }

    /// Whether this plan was probed from a struct with this name and length.
    #[cfg(feature = "flatten_dunder")]
    pub(crate) fn matches(&self, name: &str, len: usize) -> bool {
        self.name == name && self.len == len
    }

    /// Whether the entry at `index` is overwritten by a later one.
    pub(crate) fn skips(&self, index: usize) -> bool {
        self.skip.get(index).copied().unwrap_or(false)
    }
}

#[derive(Default)]
pub(crate) struct Analysis {
    pub(crate) shape: Shape,
    /// Whether a struct has a dunder field, so its entry count is unknown.
    pub(crate) has_flatten: bool,
    pub(crate) plan: Option<Plan>,
}

/// Probes `value`. `extra` names an entry the caller appends to a struct or
/// map after its own entries; it takes part in the plan like any other key.
pub(crate) fn analyze<V>(value: &V, extra: Option<&'static str>) -> Analysis
where
    V: ?Sized + Serialize,
{
    // The first pass allocates nothing and only looks for dunder fields. A
    // struct without one cannot collide: serde rejects duplicate field names.
    let mut probed = Probed {
        top_map: extra.is_some(),
        ..Probed::default()
    };
    if value.serialize(Probe::top(&mut probed)).is_err() {
        return Analysis::default();
    }
    let mut analysis = Analysis {
        shape: probed.shape,
        has_flatten: probed.has_flatten,
        plan: None,
    };
    let (name, len) = match probed.shape {
        Shape::Struct { name, len } => (name, len),
        Shape::StructVariant { variant, len } => (variant, len),
        Shape::Map if extra.is_some() => ("", 0),
        _ => return analysis,
    };
    if !probed.has_flatten && extra.is_none() {
        return analysis;
    }
    probed.collect = true;
    if value.serialize(Probe::top(&mut probed)).is_err() {
        return analysis;
    }
    probed.keys.extend(extra.map(Key::Static));
    analysis.plan = Plan::new(name, len, &probed.keys);
    analysis
}

/// Probes `value` and returns a plan if it serializes as a struct whose
/// flattened entries collide.
#[cfg(feature = "flatten_dunder")]
pub(crate) fn plan_for<V>(value: &V) -> Option<Plan>
where
    V: ?Sized + Serialize,
{
    analyze(value, None).plan
}

#[derive(Default)]
struct Probed {
    shape: Shape,
    has_flatten: bool,
    /// Whether to record the keys of a top-level map.
    top_map: bool,
    /// Whether to record keys, or only look for dunder fields.
    collect: bool,
    /// Every entry of the struct, flattened entries inlined, in order.
    keys: Vec<Key>,
}

enum Key {
    Static(&'static str),
    Owned(String),
    /// A key that is not a string; never treated as colliding.
    Opaque,
}

impl Key {
    fn as_str(&self) -> Option<&str> {
        match self {
            Key::Static(key) => Some(key),
            Key::Owned(key) => Some(key),
            Key::Opaque => None,
        }
    }
}

struct Probe<'a> {
    out: &'a mut Probed,
    /// Whether this is the value of a dunder field rather than the probed
    /// value itself.
    flatten: bool,
}

impl<'a> Probe<'a> {
    fn top(out: &'a mut Probed) -> Self {
        Probe {
            out,
            flatten: false,
        }
    }
}

impl<'a> ser::Serializer for Probe<'a> {
    type Ok = ();
    type Error = Error;

    type SerializeSeq = Ignore;
    type SerializeTuple = Ignore;
    type SerializeTupleStruct = Ignore;
    type SerializeTupleVariant = Ignore;
    type SerializeMap = ProbeMap<'a>;
    type SerializeStruct = ProbeStruct<'a>;
    type SerializeStructVariant = ProbeStruct<'a>;

    scalars!(Ok(()));

    fn serialize_char(self, _: char) -> Result<()> {
        Ok(())
    }

    fn serialize_str(self, _: &str) -> Result<()> {
        Ok(())
    }

    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<()> {
        Ok(())
    }

    fn serialize_newtype_struct<T>(self, _: &'static str, value: &T) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        // The probed value's own wrappers are probed again by the writer when
        // it reaches the inner value; only flattened ones are inlined.
        if self.flatten {
            value.serialize(self)
        } else {
            Ok(())
        }
    }

    fn serialize_newtype_variant<T>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        Ok(())
    }

    fn serialize_some<T>(self, value: &T) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        if self.flatten {
            value.serialize(self)
        } else {
            Ok(())
        }
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Ignore> {
        Ok(Ignore)
    }

    fn serialize_tuple(self, _: usize) -> Result<Ignore> {
        Ok(Ignore)
    }

    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Ignore> {
        Ok(Ignore)
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Ignore> {
        if !self.flatten {
            self.out.shape = Shape::TupleVariant { variant };
        }
        Ok(Ignore)
    }

    fn serialize_map(self, _: Option<usize>) -> Result<ProbeMap<'a>> {
        // A flattened map contributes entries to the probed struct; a
        // top-level one only matters when an extra entry is appended to it.
        let active = if self.flatten {
            true
        } else {
            self.out.shape = Shape::Map;
            self.out.top_map && self.out.collect
        };
        Ok(ProbeMap {
            out: self.out,
            active,
        })
    }

    fn serialize_struct(self, name: &'static str, len: usize) -> Result<ProbeStruct<'a>> {
        if !self.flatten {
            self.out.shape = Shape::Struct { name, len };
        }
        Ok(ProbeStruct {
            out: self.out,
            active: true,
        })
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<ProbeStruct<'a>> {
        // A flattened struct variant is not a mapping; writing it fails.
        let active = !self.flatten;
        if active {
            self.out.shape = Shape::StructVariant { variant, len };
        }
        Ok(ProbeStruct {
            out: self.out,
            active,
        })
    }

    fn collect_str<T>(self, _: &T) -> Result<()>
    where
        T: ?Sized + std::fmt::Display,
    {
        Ok(())
    }
}

struct ProbeStruct<'a> {
    out: &'a mut Probed,
    active: bool,
}

impl ProbeStruct<'_> {
    fn field<V>(&mut self, key: &'static str, value: &V) -> Result<()>
    where
        V: ?Sized + Serialize,
    {
        if !self.active {
            return Ok(());
        }
        if crate::is_flatten_key(key.as_bytes()) {
            self.out.has_flatten = true;
            if self.out.collect {
                value.serialize(Probe {
                    out: self.out,
                    flatten: true,
                })?;
            }
        } else if self.out.collect {
            self.out.keys.push(Key::Static(key));
        }
        Ok(())
    }
}

impl ser::SerializeStruct for ProbeStruct<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<V>(&mut self, key: &'static str, value: &V) -> Result<()>
    where
        V: ?Sized + Serialize,
    {
        self.field(key, value)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl ser::SerializeStructVariant for ProbeStruct<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<V>(&mut self, key: &'static str, value: &V) -> Result<()>
    where
        V: ?Sized + Serialize,
    {
        self.field(key, value)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

struct ProbeMap<'a> {
    out: &'a mut Probed,
    active: bool,
}

impl ser::SerializeMap for ProbeMap<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<K>(&mut self, key: &K) -> Result<()>
    where
        K: ?Sized + Serialize,
    {
        if self.active {
            let key = key.serialize(KeyCapture).unwrap_or(Key::Opaque);
            self.out.keys.push(key);
        }
        Ok(())
    }

    fn serialize_value<V>(&mut self, _: &V) -> Result<()>
    where
        V: ?Sized + Serialize,
    {
        Ok(())
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

/// Compound serializer for probed values that contribute no entries.
struct Ignore;

impl ser::SerializeSeq for Ignore {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T>(&mut self, _: &T) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        Ok(())
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl ser::SerializeTuple for Ignore {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T>(&mut self, _: &T) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        Ok(())
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl ser::SerializeTupleStruct for Ignore {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, _: &T) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        Ok(())
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl ser::SerializeTupleVariant for Ignore {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, _: &T) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        Ok(())
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

/// Captures a map key as the string the Value serializer would turn it into.
struct KeyCapture;

impl ser::Serializer for KeyCapture {
    type Ok = Key;
    type Error = Error;

    type SerializeSeq = Impossible<Key, Error>;
    type SerializeTuple = Impossible<Key, Error>;
    type SerializeTupleStruct = Impossible<Key, Error>;
    type SerializeTupleVariant = Impossible<Key, Error>;
    type SerializeMap = Impossible<Key, Error>;
    type SerializeStruct = Impossible<Key, Error>;
    type SerializeStructVariant = Impossible<Key, Error>;

    scalars!(Ok(Key::Opaque));

    fn serialize_char(self, v: char) -> Result<Key> {
        Ok(Key::Owned(v.to_string()))
    }

    fn serialize_str(self, v: &str) -> Result<Key> {
        Ok(Key::Owned(v.to_owned()))
    }

    fn serialize_unit_variant(self, _: &'static str, _: u32, variant: &'static str) -> Result<Key> {
        Ok(Key::Static(variant))
    }

    fn serialize_newtype_struct<T>(self, _: &'static str, value: &T) -> Result<Key>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<Key>
    where
        T: ?Sized + Serialize,
    {
        Ok(Key::Opaque)
    }

    fn serialize_some<T>(self, value: &T) -> Result<Key>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq> {
        Err(opaque_key())
    }

    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple> {
        Err(opaque_key())
    }

    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        Err(opaque_key())
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        Err(opaque_key())
    }

    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap> {
        Err(opaque_key())
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeStruct> {
        Err(opaque_key())
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant> {
        Err(opaque_key())
    }
}

/// Signals a compound key; the probe records it as [`Key::Opaque`].
fn opaque_key() -> Error {
    ser::Error::custom("compound key")
}
