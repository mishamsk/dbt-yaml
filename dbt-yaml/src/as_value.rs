//! Serializing a value through any serializer as its `Value` would.

use crate::probe::{self, scalars, Plan, Shape};
use crate::value::tagged::nobang;
use serde::ser::{self, Error as _, Impossible, Serialize, SerializeMap, SerializeSeq, Serializer};
use std::fmt::{self, Display};
use std::mem;

const EMPTY_TAG: &str = "empty YAML tag is not allowed";
const NOT_MAPPING: &str = "expected the flatten field to be a mapping";
const NONDETERMINISTIC: &str = "Serialize impl made different calls when probed";

/// Serializes the wrapped value through any serializer exactly as
/// [`to_value`](crate::to_value) of it would serialize, without building the
/// [`Value`](crate::Value).
///
/// The output follows the Value data model: structs become maps, dunder
/// fields are flattened (with the `flatten_dunder` feature), enum variants
/// with data become single-entry maps keyed by `!Variant`, integers become
/// `u64` or `i64` (wider ones strings), `f32` widens to `f64`, and bytes
/// become sequences of integers.
///
/// ```
/// # use serde_derive::Serialize;
/// #[derive(Serialize)]
/// struct Point {
///     x: f32,
///     y: f32,
/// }
///
/// let point = Point { x: 1.5, y: -2.0 };
/// let streamed = serde_json::to_string(&dbt_yaml::AsValue(&point)).unwrap();
/// let via_value = serde_json::to_string(&dbt_yaml::to_value(&point).unwrap()).unwrap();
/// assert_eq!(streamed, via_value);
/// ```
///
/// Each nested value is probed before it is written: its `Serialize` impl
/// runs once more against a serializer that records only its shape (twice
/// for structs with dunder fields). Impls must therefore make the same calls
/// every time. Where dunder fields produce the same key more than once, the
/// last value wins as in `to_value`, but the entry is written at its last
/// position rather than its first.
pub struct AsValue<'a, T: ?Sized>(pub &'a T);

impl<'a, T: ?Sized> AsValue<'a, T> {
    /// Appends an entry to the value's top-level mapping after its own
    /// entries, replacing any entry with the same key, as inserting into the
    /// `to_value` result would. Ignored when the value is not a struct or map.
    pub fn with_entry<E: ?Sized>(self, key: &'static str, value: &'a E) -> WithEntry<'a, T, E> {
        WithEntry {
            value: self.0,
            key,
            entry: value,
        }
    }
}

/// An [`AsValue`] with an extra top-level entry; see [`AsValue::with_entry`].
pub struct WithEntry<'a, T: ?Sized, E: ?Sized> {
    value: &'a T,
    key: &'static str,
    entry: &'a E,
}

impl<T> Serialize for AsValue<'_, T>
where
    T: ?Sized + Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_as_value(self.0, serializer, None)
    }
}

impl<T, E> Serialize for WithEntry<'_, T, E>
where
    T: ?Sized + Serialize,
    E: ?Sized + Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let emit = |map: &mut S::SerializeMap| map.serialize_entry(self.key, &AsValue(self.entry));
        let extra = Extra {
            key: self.key,
            emit: &emit,
        };
        serialize_as_value(self.value, serializer, Some(extra))
    }
}

/// An entry appended to a top-level mapping.
struct Extra<'a, M: SerializeMap> {
    key: &'static str,
    emit: &'a dyn Fn(&mut M) -> Result<(), M::Error>,
}

impl<M: SerializeMap> Clone for Extra<'_, M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M: SerializeMap> Copy for Extra<'_, M> {}

fn serialize_as_value<T, S>(
    value: &T,
    serializer: S,
    extra: Option<Extra<'_, S::SerializeMap>>,
) -> Result<S::Ok, S::Error>
where
    T: ?Sized + Serialize,
    S: Serializer,
{
    let analysis = probe::analyze(value, extra.map(|extra| extra.key));
    let plan = analysis.plan.as_ref();
    match analysis.shape {
        // The variant's data is the value of a single-entry map, but serde
        // hands it over field by field. Writing the map's value runs the
        // value's `Serialize` impl again and keeps only the data.
        Shape::TupleVariant { variant } | Shape::StructVariant { variant, .. } => {
            if variant.is_empty() {
                return Err(S::Error::custom(EMPTY_TAG));
            }
            let mut map = serializer.serialize_map(Some(1))?;
            map.serialize_entry(
                &Tag(variant),
                &VariantData {
                    value,
                    plan,
                    has_flatten: analysis.has_flatten,
                },
            )?;
            map.end()
        }
        _ => value.serialize(Adapter {
            inner: serializer,
            plan,
            has_flatten: analysis.has_flatten,
            extra,
            variant_data: false,
        }),
    }
}

/// A `!Variant` map key, as the Value serializer writes a tag.
struct Tag(&'static str);

impl Serialize for Tag {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl Display for Tag {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "!{}", nobang(self.0))
    }
}

/// The data of a tuple or struct variant, without its tag.
struct VariantData<'a, T: ?Sized> {
    value: &'a T,
    plan: Option<&'a Plan>,
    has_flatten: bool,
}

impl<T> Serialize for VariantData<'_, T>
where
    T: ?Sized + Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.value.serialize(Adapter {
            inner: serializer,
            plan: self.plan,
            has_flatten: self.has_flatten,
            extra: None,
            variant_data: true,
        })
    }
}

struct Adapter<'a, S: Serializer> {
    inner: S,
    /// Entries to skip, if the value is a struct with colliding keys.
    plan: Option<&'a Plan>,
    /// Whether the value is a struct with dunder fields, so its entry count
    /// is unknown.
    has_flatten: bool,
    extra: Option<Extra<'a, S::SerializeMap>>,
    /// Whether to write only the data of a tuple or struct variant.
    variant_data: bool,
}

impl<'a, S> Serializer for Adapter<'a, S>
where
    S: Serializer,
{
    type Ok = S::Ok;
    type Error = S::Error;

    type SerializeSeq = Seq<S::SerializeSeq>;
    type SerializeTuple = Seq<S::SerializeSeq>;
    type SerializeTupleStruct = Seq<S::SerializeSeq>;
    type SerializeTupleVariant = Seq<S::SerializeSeq>;
    type SerializeMap = Map<'a, S::SerializeMap>;
    type SerializeStruct = Map<'a, S::SerializeMap>;
    type SerializeStructVariant = Map<'a, S::SerializeMap>;

    fn serialize_bool(self, v: bool) -> Result<S::Ok, S::Error> {
        self.inner.serialize_bool(v)
    }

    fn serialize_i8(self, v: i8) -> Result<S::Ok, S::Error> {
        self.serialize_i64(v.into())
    }

    fn serialize_i16(self, v: i16) -> Result<S::Ok, S::Error> {
        self.serialize_i64(v.into())
    }

    fn serialize_i32(self, v: i32) -> Result<S::Ok, S::Error> {
        self.serialize_i64(v.into())
    }

    fn serialize_i64(self, v: i64) -> Result<S::Ok, S::Error> {
        // `Number` stores non-negative integers as unsigned.
        match u64::try_from(v) {
            Ok(v) => self.inner.serialize_u64(v),
            Err(_) => self.inner.serialize_i64(v),
        }
    }

    fn serialize_i128(self, v: i128) -> Result<S::Ok, S::Error> {
        if let Ok(v) = u64::try_from(v) {
            self.inner.serialize_u64(v)
        } else if let Ok(v) = i64::try_from(v) {
            self.inner.serialize_i64(v)
        } else {
            self.inner.serialize_str(&v.to_string())
        }
    }

    fn serialize_u8(self, v: u8) -> Result<S::Ok, S::Error> {
        self.inner.serialize_u64(v.into())
    }

    fn serialize_u16(self, v: u16) -> Result<S::Ok, S::Error> {
        self.inner.serialize_u64(v.into())
    }

    fn serialize_u32(self, v: u32) -> Result<S::Ok, S::Error> {
        self.inner.serialize_u64(v.into())
    }

    fn serialize_u64(self, v: u64) -> Result<S::Ok, S::Error> {
        self.inner.serialize_u64(v)
    }

    fn serialize_u128(self, v: u128) -> Result<S::Ok, S::Error> {
        match u64::try_from(v) {
            Ok(v) => self.inner.serialize_u64(v),
            Err(_) => self.inner.serialize_str(&v.to_string()),
        }
    }

    fn serialize_f32(self, v: f32) -> Result<S::Ok, S::Error> {
        self.serialize_f64(v.into())
    }

    fn serialize_f64(self, v: f64) -> Result<S::Ok, S::Error> {
        // `Number` keeps a single NaN.
        let v = if v.is_nan() {
            f64::NAN.copysign(1.0)
        } else {
            v
        };
        self.inner.serialize_f64(v)
    }

    fn serialize_char(self, v: char) -> Result<S::Ok, S::Error> {
        self.inner.serialize_str(v.encode_utf8(&mut [0u8; 4]))
    }

    fn serialize_str(self, v: &str) -> Result<S::Ok, S::Error> {
        self.inner.serialize_str(v)
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<S::Ok, S::Error> {
        let mut seq = self.inner.serialize_seq(Some(v.len()))?;
        for byte in v {
            seq.serialize_element(&u64::from(*byte))?;
        }
        seq.end()
    }

    fn serialize_none(self) -> Result<S::Ok, S::Error> {
        self.inner.serialize_unit()
    }

    fn serialize_some<T>(self, value: &T) -> Result<S::Ok, S::Error>
    where
        T: ?Sized + Serialize,
    {
        serialize_as_value(value, self.inner, self.extra)
    }

    fn serialize_unit(self) -> Result<S::Ok, S::Error> {
        self.inner.serialize_unit()
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result<S::Ok, S::Error> {
        self.inner.serialize_unit()
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<S::Ok, S::Error> {
        self.inner.serialize_str(variant)
    }

    fn serialize_newtype_struct<T>(self, _: &'static str, value: &T) -> Result<S::Ok, S::Error>
    where
        T: ?Sized + Serialize,
    {
        serialize_as_value(value, self.inner, self.extra)
    }

    fn serialize_newtype_variant<T>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<S::Ok, S::Error>
    where
        T: ?Sized + Serialize,
    {
        if variant.is_empty() {
            return Err(S::Error::custom(EMPTY_TAG));
        }
        let mut map = self.inner.serialize_map(Some(1))?;
        map.serialize_entry(&Tag(variant), &AsValue(value))?;
        map.end()
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, S::Error> {
        self.inner.serialize_seq(len).map(Seq)
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, S::Error> {
        self.inner.serialize_seq(Some(len)).map(Seq)
    }

    fn serialize_tuple_struct(
        self,
        _: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, S::Error> {
        self.inner.serialize_seq(Some(len)).map(Seq)
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, S::Error> {
        if !self.variant_data {
            return Err(S::Error::custom(NONDETERMINISTIC));
        }
        self.inner.serialize_seq(Some(len)).map(Seq)
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, S::Error> {
        let len = if self.plan.is_some() || self.extra.is_some() {
            None
        } else {
            len
        };
        let map = self.inner.serialize_map(len)?;
        Ok(Map::new(map, self.plan, self.extra))
    }

    fn serialize_struct(
        self,
        _: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, S::Error> {
        let len = if self.has_flatten || self.extra.is_some() {
            None
        } else {
            Some(len)
        };
        let map = self.inner.serialize_map(len)?;
        Ok(Map::new(map, self.plan, self.extra))
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, S::Error> {
        if !self.variant_data {
            return Err(S::Error::custom(NONDETERMINISTIC));
        }
        let len = if self.has_flatten { None } else { Some(len) };
        let map = self.inner.serialize_map(len)?;
        Ok(Map::new(map, self.plan, None))
    }

    fn collect_str<T>(self, value: &T) -> Result<S::Ok, S::Error>
    where
        T: ?Sized + Display,
    {
        self.inner.collect_str(value)
    }

    fn is_human_readable(&self) -> bool {
        // As the Value serializer, whatever the inner one is.
        true
    }
}

struct Seq<Q>(Q);

impl<Q: SerializeSeq> SerializeSeq for Seq<Q> {
    type Ok = Q::Ok;
    type Error = Q::Error;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Q::Error>
    where
        T: ?Sized + Serialize,
    {
        self.0.serialize_element(&AsValue(value))
    }

    fn end(self) -> Result<Q::Ok, Q::Error> {
        self.0.end()
    }
}

impl<Q: SerializeSeq> ser::SerializeTuple for Seq<Q> {
    type Ok = Q::Ok;
    type Error = Q::Error;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Q::Error>
    where
        T: ?Sized + Serialize,
    {
        self.0.serialize_element(&AsValue(value))
    }

    fn end(self) -> Result<Q::Ok, Q::Error> {
        self.0.end()
    }
}

impl<Q: SerializeSeq> ser::SerializeTupleStruct for Seq<Q> {
    type Ok = Q::Ok;
    type Error = Q::Error;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Q::Error>
    where
        T: ?Sized + Serialize,
    {
        self.0.serialize_element(&AsValue(value))
    }

    fn end(self) -> Result<Q::Ok, Q::Error> {
        self.0.end()
    }
}

impl<Q: SerializeSeq> ser::SerializeTupleVariant for Seq<Q> {
    type Ok = Q::Ok;
    type Error = Q::Error;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Q::Error>
    where
        T: ?Sized + Serialize,
    {
        self.0.serialize_element(&AsValue(value))
    }

    fn end(self) -> Result<Q::Ok, Q::Error> {
        self.0.end()
    }
}

/// Writes a struct or map, inlining dunder fields and skipping the entries
/// the plan marks as overwritten.
struct Map<'a, M: SerializeMap> {
    map: M,
    plan: Option<&'a Plan>,
    /// Index of the next entry, counting flattened and skipped ones.
    next: usize,
    /// Set when the current key was skipped, so its value is skipped too.
    skip_value: bool,
    extra: Option<Extra<'a, M>>,
}

impl<'a, M: SerializeMap> Map<'a, M> {
    fn new(map: M, plan: Option<&'a Plan>, extra: Option<Extra<'a, M>>) -> Self {
        Map {
            map,
            plan,
            next: 0,
            skip_value: false,
            extra,
        }
    }

    /// Advances past one entry, returning true if a later one overwrites it.
    fn skip_next(&mut self) -> bool {
        let skip = self.plan.is_some_and(|plan| plan.skips(self.next));
        self.next += 1;
        skip
    }

    fn key<K>(&mut self, key: &K) -> Result<(), M::Error>
    where
        K: ?Sized + Serialize,
    {
        if self.skip_next() {
            self.skip_value = true;
            return Ok(());
        }
        self.map.serialize_key(&AsValue(key))
    }

    fn value<V>(&mut self, value: &V) -> Result<(), M::Error>
    where
        V: ?Sized + Serialize,
    {
        if mem::take(&mut self.skip_value) {
            return Ok(());
        }
        self.map.serialize_value(&AsValue(value))
    }

    /// Passes whole entries on: the YAML serializer only turns a `!Tag` key
    /// into a tag when it sees the entry at once.
    fn entry<K, V>(&mut self, key: &K, value: &V) -> Result<(), M::Error>
    where
        K: ?Sized + Serialize,
        V: ?Sized + Serialize,
    {
        if self.skip_next() {
            return Ok(());
        }
        self.map.serialize_entry(&AsValue(key), &AsValue(value))
    }

    fn field<V>(&mut self, key: &'static str, value: &V) -> Result<(), M::Error>
    where
        V: ?Sized + Serialize,
    {
        if crate::is_flatten_key(key.as_bytes()) {
            return value.serialize(Flatten { map: self });
        }
        if self.skip_next() {
            return Ok(());
        }
        self.map.serialize_entry(key, &AsValue(value))
    }

    fn finish(mut self) -> Result<M::Ok, M::Error> {
        if let Some(extra) = self.extra {
            // The extra entry comes last, so it is never the one skipped.
            self.skip_next();
            (extra.emit)(&mut self.map)?;
        }
        self.map.end()
    }
}

impl<M: SerializeMap> SerializeMap for Map<'_, M> {
    type Ok = M::Ok;
    type Error = M::Error;

    fn serialize_key<K>(&mut self, key: &K) -> Result<(), M::Error>
    where
        K: ?Sized + Serialize,
    {
        self.key(key)
    }

    fn serialize_value<V>(&mut self, value: &V) -> Result<(), M::Error>
    where
        V: ?Sized + Serialize,
    {
        self.value(value)
    }

    fn serialize_entry<K, V>(&mut self, key: &K, value: &V) -> Result<(), M::Error>
    where
        K: ?Sized + Serialize,
        V: ?Sized + Serialize,
    {
        self.entry(key, value)
    }

    fn end(self) -> Result<M::Ok, M::Error> {
        self.finish()
    }
}

impl<M: SerializeMap> ser::SerializeStruct for Map<'_, M> {
    type Ok = M::Ok;
    type Error = M::Error;

    fn serialize_field<V>(&mut self, key: &'static str, value: &V) -> Result<(), M::Error>
    where
        V: ?Sized + Serialize,
    {
        self.field(key, value)
    }

    fn end(self) -> Result<M::Ok, M::Error> {
        self.finish()
    }
}

impl<M: SerializeMap> ser::SerializeStructVariant for Map<'_, M> {
    type Ok = M::Ok;
    type Error = M::Error;

    fn serialize_field<V>(&mut self, key: &'static str, value: &V) -> Result<(), M::Error>
    where
        V: ?Sized + Serialize,
    {
        self.field(key, value)
    }

    fn end(self) -> Result<M::Ok, M::Error> {
        self.finish()
    }
}

/// Writes the value of a dunder field as entries of the enclosing map.
struct Flatten<'m, 'a, M: SerializeMap> {
    map: &'m mut Map<'a, M>,
}

fn not_mapping<E: ser::Error>() -> E {
    E::custom(NOT_MAPPING)
}

impl<'m, 'a, M: SerializeMap> Serializer for Flatten<'m, 'a, M> {
    type Ok = ();
    type Error = M::Error;

    type SerializeSeq = Impossible<(), M::Error>;
    type SerializeTuple = Impossible<(), M::Error>;
    type SerializeTupleStruct = Impossible<(), M::Error>;
    type SerializeTupleVariant = Impossible<(), M::Error>;
    type SerializeMap = FlattenEntries<'m, 'a, M>;
    type SerializeStruct = FlattenEntries<'m, 'a, M>;
    type SerializeStructVariant = Impossible<(), M::Error>;

    scalars!(Err(not_mapping()));

    fn serialize_char(self, _: char) -> Result<(), M::Error> {
        Err(not_mapping())
    }

    fn serialize_str(self, _: &str) -> Result<(), M::Error> {
        Err(not_mapping())
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<(), M::Error> {
        Err(not_mapping())
    }

    fn serialize_newtype_struct<T>(self, _: &'static str, value: &T) -> Result<(), M::Error>
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
    ) -> Result<(), M::Error>
    where
        T: ?Sized + Serialize,
    {
        Err(not_mapping())
    }

    fn serialize_some<T>(self, value: &T) -> Result<(), M::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, M::Error> {
        Err(not_mapping())
    }

    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, M::Error> {
        Err(not_mapping())
    }

    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, M::Error> {
        Err(not_mapping())
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, M::Error> {
        Err(not_mapping())
    }

    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, M::Error> {
        Ok(FlattenEntries { map: self.map })
    }

    fn serialize_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStruct, M::Error> {
        Ok(FlattenEntries { map: self.map })
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, M::Error> {
        Err(not_mapping())
    }
}

struct FlattenEntries<'m, 'a, M: SerializeMap> {
    map: &'m mut Map<'a, M>,
}

impl<M: SerializeMap> ser::SerializeStruct for FlattenEntries<'_, '_, M> {
    type Ok = ();
    type Error = M::Error;

    fn serialize_field<V>(&mut self, key: &'static str, value: &V) -> Result<(), M::Error>
    where
        V: ?Sized + Serialize,
    {
        self.map.field(key, value)
    }

    fn end(self) -> Result<(), M::Error> {
        Ok(())
    }
}

impl<M: SerializeMap> SerializeMap for FlattenEntries<'_, '_, M> {
    type Ok = ();
    type Error = M::Error;

    fn serialize_key<K>(&mut self, key: &K) -> Result<(), M::Error>
    where
        K: ?Sized + Serialize,
    {
        self.map.key(key)
    }

    fn serialize_value<V>(&mut self, value: &V) -> Result<(), M::Error>
    where
        V: ?Sized + Serialize,
    {
        self.map.value(value)
    }

    fn serialize_entry<K, V>(&mut self, key: &K, value: &V) -> Result<(), M::Error>
    where
        K: ?Sized + Serialize,
        V: ?Sized + Serialize,
    {
        self.map.entry(key, value)
    }

    fn end(self) -> Result<(), M::Error> {
        Ok(())
    }
}
