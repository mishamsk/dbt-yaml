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
//! The collected keys also give the exact number of entries written, so
//! such structs can be started with their real length.
//!
//! This relies on `Serialize` impls making the same calls every time they
//! run. Writers check a struct's name and length, and for a struct with a
//! plan every key and the entry count, against what the probe saw; a
//! mismatch fails with [`NONDETERMINISTIC`] rather than writing wrong output.

use crate::error::Error;
use crate::value::tagged::{self, MaybeTag};
use serde::ser::{self, Impossible, Serialize};
use std::collections::HashMap;
use std::fmt::Display;

type Result<T, E = Error> = std::result::Result<T, E>;

/// The error for a `Serialize` impl that made different calls when probed.
pub(crate) const NONDETERMINISTIC: &str = "Serialize impl made different calls when probed";

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
    /// Only recorded when probing for an extra entry. A single-entry map
    /// whose key is a `!tag` is a tagged value, not a mapping, and is
    /// recorded as `Other`.
    Map,
}

/// Which entries of one struct are overwritten by a later entry.
#[derive(Clone)]
pub(crate) struct Plan {
    /// Name and length of the struct, checked by the YAML serializer.
    #[cfg_attr(not(feature = "flatten_dunder"), allow(dead_code))]
    name: &'static str,
    #[cfg_attr(not(feature = "flatten_dunder"), allow(dead_code))]
    len: usize,
    /// Every entry's key, flattened entries inlined, in order.
    keys: Vec<Key>,
    skip: Vec<bool>,
}

impl Plan {
    fn new(name: &'static str, len: usize, keys: Vec<Key>) -> Option<Self> {
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
        skip.contains(&true).then_some(Plan {
            name,
            len,
            keys,
            skip,
        })
    }

    /// Whether this plan was probed from a struct with this name and length.
    #[cfg(feature = "flatten_dunder")]
    pub(crate) fn matches(&self, name: &str, len: usize) -> bool {
        self.name == name && self.len == len
    }

    /// Whether the entry at `index` is overwritten by a later one, or `None`
    /// if its key is not the one probed there.
    pub(crate) fn entry(&self, index: usize, key: &Key) -> Option<bool> {
        let probed = self.keys.get(index)?;
        probed.same(key).then(|| self.skip[index])
    }

    /// The number of entries the probe saw, skipped ones included.
    pub(crate) fn entries(&self) -> usize {
        self.keys.len()
    }

    fn skipped(&self) -> usize {
        self.skip.iter().filter(|skip| **skip).count()
    }
}

#[derive(Default)]
pub(crate) struct Analysis {
    pub(crate) shape: Shape,
    /// The number of entries written, when the keys were collected: for a
    /// struct with dunder fields, or a struct or map with an extra entry.
    /// Otherwise the length the value itself reports is exact.
    pub(crate) len: Option<usize>,
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
        // Keep the shape: the impl fails the same way when it is written,
        // and that error is the one to report.
        return Analysis {
            shape: probed.shape,
            ..Analysis::default()
        };
    }
    if probed.tagged {
        return Analysis::default();
    }
    let mut analysis = Analysis {
        shape: probed.shape,
        len: None,
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
    let entries = probed.keys.len();
    analysis.plan = Plan::new(name, len, probed.keys);
    analysis.len = Some(entries - analysis.plan.as_ref().map_or(0, Plan::skipped));
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

/// Whether `key`, as the first key of a single-entry map, makes the map a
/// tagged value for the Value serializer: it is written with `collect_str`
/// and reads as a `!tag`.
pub(crate) fn is_tag<K>(key: &K) -> bool
where
    K: ?Sized + Serialize,
{
    key.serialize(IsTag).unwrap_or(false)
}

#[derive(Default)]
struct Probed {
    shape: Shape,
    has_flatten: bool,
    /// Whether a top-level map turned out to be a tagged value.
    tagged: bool,
    /// Whether to record the keys of a top-level map.
    top_map: bool,
    /// Whether to record keys, or only look for dunder fields.
    collect: bool,
    /// Every entry of the struct, flattened entries inlined, in order.
    keys: Vec<Key>,
}

/// A key as the Value serializer would see it.
#[derive(Clone)]
pub(crate) enum Key {
    Static(&'static str),
    Owned(String),
    /// A key that is not a string; never treated as colliding.
    Opaque,
}

impl Key {
    /// Captures a map key as the string the Value serializer would turn it
    /// into, or [`Key::Opaque`].
    pub(crate) fn capture<K>(key: &K) -> Key
    where
        K: ?Sized + Serialize,
    {
        key.serialize(KeyCapture).unwrap_or(Key::Opaque)
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Key::Static(key) => Some(key),
            Key::Owned(key) => Some(key),
            Key::Opaque => None,
        }
    }

    fn same(&self, other: &Key) -> bool {
        match (self.as_str(), other.as_str()) {
            (Some(a), Some(b)) => a == b,
            (None, None) => true,
            _ => false,
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

    fn serialize_map(self, len: Option<usize>) -> Result<ProbeMap<'a>> {
        // A flattened map contributes entries to the probed struct; a
        // top-level one only matters when an extra entry is appended to it.
        let (active, check_tag) = if self.flatten {
            (true, false)
        } else {
            self.out.shape = Shape::Map;
            (
                self.out.top_map && self.out.collect,
                self.out.top_map && len == Some(1),
            )
        };
        Ok(ProbeMap {
            out: self.out,
            active,
            check_tag,
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
        T: ?Sized + Display,
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
    /// Whether this is a top-level single-entry map that may be a tagged
    /// value.
    check_tag: bool,
}

impl ser::SerializeMap for ProbeMap<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<K>(&mut self, key: &K) -> Result<()>
    where
        K: ?Sized + Serialize,
    {
        if self.active {
            self.out.keys.push(Key::capture(key));
        }
        Ok(())
    }

    fn serialize_value<V>(&mut self, _: &V) -> Result<()>
    where
        V: ?Sized + Serialize,
    {
        Ok(())
    }

    fn serialize_entry<K, V>(&mut self, key: &K, value: &V) -> Result<()>
    where
        K: ?Sized + Serialize,
        V: ?Sized + Serialize,
    {
        // The Value serializer only recognizes a tag in a whole entry.
        if std::mem::take(&mut self.check_tag) && is_tag(key) {
            self.out.tagged = true;
        }
        self.serialize_key(key)?;
        self.serialize_value(value)
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

    scalars!(@ Ok(Key::Opaque);
        serialize_bool: bool,
        serialize_i8: i8,
        serialize_i16: i16,
        serialize_i32: i32,
        serialize_i64: i64,
        serialize_u8: u8,
        serialize_u16: u16,
        serialize_u32: u32,
        serialize_u64: u64,
        serialize_f32: f32,
        serialize_f64: f64,
        serialize_bytes: &[u8],
        serialize_unit_struct: &'static str
    );

    fn serialize_unit(self) -> Result<Key> {
        Ok(Key::Opaque)
    }

    fn serialize_none(self) -> Result<Key> {
        Ok(Key::Opaque)
    }

    fn serialize_i128(self, v: i128) -> Result<Key> {
        // The Value serializer turns integers wider than 64 bits into strings.
        Ok(if i64::try_from(v).is_ok() || u64::try_from(v).is_ok() {
            Key::Opaque
        } else {
            Key::Owned(v.to_string())
        })
    }

    fn serialize_u128(self, v: u128) -> Result<Key> {
        Ok(if u64::try_from(v).is_ok() {
            Key::Opaque
        } else {
            Key::Owned(v.to_string())
        })
    }

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

/// Answers whether a key is written with `collect_str` as a `!tag`.
struct IsTag;

impl ser::Serializer for IsTag {
    type Ok = bool;
    type Error = Error;

    type SerializeSeq = Impossible<bool, Error>;
    type SerializeTuple = Impossible<bool, Error>;
    type SerializeTupleStruct = Impossible<bool, Error>;
    type SerializeTupleVariant = Impossible<bool, Error>;
    type SerializeMap = Impossible<bool, Error>;
    type SerializeStruct = Impossible<bool, Error>;
    type SerializeStructVariant = Impossible<bool, Error>;

    scalars!(Ok(false));

    fn serialize_char(self, _: char) -> Result<bool> {
        Ok(false)
    }

    fn serialize_str(self, _: &str) -> Result<bool> {
        Ok(false)
    }

    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<bool> {
        Ok(false)
    }

    fn serialize_newtype_struct<T>(self, _: &'static str, _: &T) -> Result<bool>
    where
        T: ?Sized + Serialize,
    {
        Ok(false)
    }

    fn serialize_newtype_variant<T>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<bool>
    where
        T: ?Sized + Serialize,
    {
        Ok(false)
    }

    fn serialize_some<T>(self, _: &T) -> Result<bool>
    where
        T: ?Sized + Serialize,
    {
        Ok(false)
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

    fn collect_str<T>(self, value: &T) -> Result<bool>
    where
        T: ?Sized + Display,
    {
        Ok(matches!(tagged::check_for_tag(value), MaybeTag::Tag(_)))
    }
}
