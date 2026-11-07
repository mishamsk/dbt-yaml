//! Streaming support for `flatten_dunder`.
//!
//! The Value serializer flattens dunder-named struct fields into the parent
//! mapping, and an entry whose key is already present overwrites the earlier
//! one. A streaming emitter cannot take back what it has written, so before a
//! struct is emitted it is probed: a shallow run of the struct's own
//! `Serialize` impl that records keys and never visits ordinary field values.
//! When a key occurs more than once, every occurrence but the last is skipped
//! during emission.
//!
//! The surviving entry therefore sits at the position of its last occurrence
//! rather than its first; that is the one way this output differs from
//! `to_value`.

use super::Serializer;
use crate::error::{self, Error, ErrorImpl};
use serde::ser::{self, Impossible, Serialize};
use std::collections::HashMap;
use std::io;
use std::mem;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Expands the `Serializer` methods that every serializer here treats alike.
macro_rules! scalars {
    ($ret:expr) => {
        scalars!(@ $ret;
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

        fn serialize_unit(self) -> Result<Self::Ok> {
            $ret
        }

        fn serialize_none(self) -> Result<Self::Ok> {
            $ret
        }
    };
    (@ $ret:expr; $($method:ident: $ty:ty),*) => {
        $(
            fn $method(self, _: $ty) -> Result<Self::Ok> {
                $ret
            }
        )*
    };
}

/// Which entries of one struct are overwritten by a later entry.
pub(super) struct Plan {
    name: &'static str,
    len: usize,
    skip: Vec<bool>,
    cursor: usize,
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
        skip.contains(&true).then_some(Plan {
            name,
            len,
            skip,
            cursor: 0,
        })
    }
}

impl<W> Serializer<W> {
    /// Enters a struct, adopting the plan probed for it if there is one.
    pub(super) fn push_plan(&mut self, name: &'static str, len: usize) {
        let plan = self
            .next_plan
            .take()
            .filter(|plan| plan.name == name && plan.len == len);
        self.plans.push(plan);
    }

    pub(super) fn pop_plan(&mut self) {
        self.plans.pop();
    }

    /// Advances past one entry of the current struct, returning true if a
    /// later entry overwrites it.
    pub(super) fn skip_entry(&mut self) -> bool {
        match self.plans.last_mut() {
            Some(Some(plan)) => {
                let skip = plan.skip.get(plan.cursor).copied().unwrap_or(false);
                plan.cursor += 1;
                skip
            }
            _ => false,
        }
    }
}

/// Probes `value` and returns a plan if it serializes as a struct whose
/// flattened entries collide.
pub(super) fn plan_for<V>(value: &V) -> Option<Plan>
where
    V: ?Sized + Serialize,
{
    // The first pass allocates nothing and only looks for dunder fields. A
    // struct without one cannot collide: serde rejects duplicate field names.
    let mut probed = Probed::default();
    value.serialize(Probe::top(&mut probed)).ok()?;
    let (name, len) = probed.found?;
    if !probed.has_flatten {
        return None;
    }
    probed.collect = true;
    value.serialize(Probe::top(&mut probed)).ok()?;
    Plan::new(name, len, &probed.keys)
}

#[derive(Default)]
struct Probed {
    /// Name and length of the probed struct, if it is one.
    found: Option<(&'static str, usize)>,
    has_flatten: bool,
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
        // The probed value's own wrappers are probed again by the emitter
        // when it reaches the inner value; only flattened ones are inlined.
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
        _: &'static str,
        _: usize,
    ) -> Result<Ignore> {
        Ok(Ignore)
    }

    fn serialize_map(self, _: Option<usize>) -> Result<ProbeMap<'a>> {
        // Only a flattened map contributes entries to the probed struct.
        let active = self.flatten;
        Ok(ProbeMap {
            out: self.out,
            active,
        })
    }

    fn serialize_struct(self, name: &'static str, len: usize) -> Result<ProbeStruct<'a>> {
        if !self.flatten {
            self.out.found = Some((name, len));
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
        // A flattened struct variant is not a mapping; emitting it fails.
        let active = !self.flatten;
        if active {
            self.out.found = Some((variant, len));
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

/// Captures a flattened map key as the string the Value serializer would
/// turn it into.
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

fn not_mapping() -> Error {
    error::new(ErrorImpl::FlattenNotMapping)
}

/// Emits the value of a dunder field as entries of the enclosing mapping.
pub(super) struct FlattenSerializer<'a, W> {
    pub(super) ser: &'a mut Serializer<W>,
}

impl<'a, W> ser::Serializer for FlattenSerializer<'a, W>
where
    W: io::Write,
{
    type Ok = ();
    type Error = Error;

    type SerializeSeq = Impossible<(), Error>;
    type SerializeTuple = Impossible<(), Error>;
    type SerializeTupleStruct = Impossible<(), Error>;
    type SerializeTupleVariant = Impossible<(), Error>;
    type SerializeMap = FlattenMap<'a, W>;
    type SerializeStruct = FlattenStruct<'a, W>;
    type SerializeStructVariant = Impossible<(), Error>;

    scalars!(Err(not_mapping()));

    fn serialize_char(self, _: char) -> Result<()> {
        Err(not_mapping())
    }

    fn serialize_str(self, _: &str) -> Result<()> {
        Err(not_mapping())
    }

    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<()> {
        Err(not_mapping())
    }

    fn serialize_newtype_struct<T>(self, _: &'static str, value: &T) -> Result<()>
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
    ) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        Err(not_mapping())
    }

    fn serialize_some<T>(self, value: &T) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq> {
        Err(not_mapping())
    }

    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple> {
        Err(not_mapping())
    }

    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        Err(not_mapping())
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        Err(not_mapping())
    }

    fn serialize_map(self, _: Option<usize>) -> Result<FlattenMap<'a, W>> {
        Ok(FlattenMap {
            ser: self.ser,
            skip_value: false,
        })
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<FlattenStruct<'a, W>> {
        Ok(FlattenStruct { ser: self.ser })
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant> {
        Err(not_mapping())
    }
}

pub(super) struct FlattenStruct<'a, W> {
    ser: &'a mut Serializer<W>,
}

impl<W> ser::SerializeStruct for FlattenStruct<'_, W>
where
    W: io::Write,
{
    type Ok = ();
    type Error = Error;

    fn serialize_field<V>(&mut self, key: &'static str, value: &V) -> Result<()>
    where
        V: ?Sized + Serialize,
    {
        self.ser.serialize_struct_field(key, value)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

pub(super) struct FlattenMap<'a, W> {
    ser: &'a mut Serializer<W>,
    /// Set when the current key was skipped, so its value is skipped too.
    skip_value: bool,
}

impl<W> ser::SerializeMap for FlattenMap<'_, W>
where
    W: io::Write,
{
    type Ok = ();
    type Error = Error;

    fn serialize_key<K>(&mut self, key: &K) -> Result<()>
    where
        K: ?Sized + Serialize,
    {
        if self.ser.skip_entry() {
            self.skip_value = true;
            return Ok(());
        }
        self.ser.serialize_child(key)
    }

    fn serialize_value<V>(&mut self, value: &V) -> Result<()>
    where
        V: ?Sized + Serialize,
    {
        if mem::take(&mut self.skip_value) {
            return Ok(());
        }
        self.ser.serialize_child(value)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}
