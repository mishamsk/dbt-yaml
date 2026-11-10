//! Streaming support for `flatten_dunder` in the YAML serializer.
//!
//! Dunder-named struct fields are written as entries of the enclosing
//! mapping, as the Value serializer does. Colliding keys are resolved by the
//! plan from [`crate::probe`]: only the last occurrence of a key is written.

use super::Serializer;
use crate::error::{self, Error, ErrorImpl};
use crate::probe::{is_tag, scalars, Key, NONDETERMINISTIC};
use serde::ser::{self, Impossible, Serialize};
use std::io;
use std::mem;

type Result<T, E = Error> = std::result::Result<T, E>;

fn nondeterministic() -> Error {
    ser::Error::custom(NONDETERMINISTIC)
}

impl<W> Serializer<W> {
    /// Enters a struct, adopting the plan probed for it if there is one.
    ///
    /// The plan was probed from the value whose `Serialize` impl is now
    /// calling `serialize_struct`, so it describes this struct unless the impl
    /// made different calls when probed.
    pub(super) fn push_plan(&mut self, name: &'static str, len: usize) -> Result<()> {
        if self.depth == 0 {
            // A new document: drop what an earlier failed one left behind.
            self.plans.clear();
        }
        let plan = self.next_plan.take();
        if plan.as_ref().is_some_and(|plan| !plan.matches(name, len)) {
            return Err(nondeterministic());
        }
        self.plans.push(plan.map(|plan| (plan, 0)));
        Ok(())
    }

    /// Leaves a struct, checking that it wrote as many entries as probed.
    pub(super) fn pop_plan(&mut self) -> Result<()> {
        match self.plans.pop() {
            Some(Some((plan, next))) if next != plan.entries() => Err(nondeterministic()),
            _ => Ok(()),
        }
    }

    /// Advances past one entry of the current struct, returning true if a
    /// later entry overwrites it. `key` is only computed when the struct has
    /// a plan, to check it against the probed key.
    pub(super) fn skip_entry(&mut self, key: impl FnOnce() -> Key) -> Result<bool> {
        match self.plans.last_mut() {
            Some(Some((plan, next))) => {
                let skip = plan.entry(*next, &key()).ok_or_else(nondeterministic)?;
                *next += 1;
                Ok(skip)
            }
            _ => Ok(false),
        }
    }
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

    fn serialize_map(self, len: Option<usize>) -> Result<FlattenMap<'a, W>> {
        Ok(FlattenMap {
            ser: self.ser,
            skip_value: false,
            check_tag: len == Some(1),
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
    /// Set for a single-entry map, which is a tagged value rather than a
    /// mapping if its key is a `!tag`.
    check_tag: bool,
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
        if self.ser.skip_entry(|| Key::capture(key))? {
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

    fn serialize_entry<K, V>(&mut self, key: &K, value: &V) -> Result<()>
    where
        K: ?Sized + Serialize,
        V: ?Sized + Serialize,
    {
        // Like the Value serializer, recognize a tag only in a whole entry.
        if mem::take(&mut self.check_tag) && is_tag(key) {
            return Err(not_mapping());
        }
        self.serialize_key(key)?;
        self.serialize_value(value)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}
