//! Serializing a value through any serializer as its `Value` would.

use crate::probe::{self, is_tag, scalars, Analysis, Key, Plan, Shape, NONDETERMINISTIC};
use crate::value::tagged::nobang;
use serde::ser::{self, Error as _, Impossible, Serialize, SerializeMap, SerializeSeq, Serializer};
use std::borrow::Cow;
use std::fmt::{self, Display};
use std::mem;

const EMPTY_TAG: &str = "empty YAML tag is not allowed";
const NOT_MAPPING: &str = "expected the flatten field to be a mapping";

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
/// When writing reaches a struct or an enum variant, its `Serialize` impl
/// runs again, once or twice, against a serializer that records only its
/// shape and, for structs with dunder fields, its keys; field values are
/// not visited. That tells which entries to skip where dunder fields repeat
/// a key, and how many entries the struct writes, so it is started with its
/// exact length. Impls must therefore make the same calls every time. A
/// struct or variant whose impl makes different calls (another name or
/// length, other keys, or another number of entries) fails with an error
/// rather than writing a wrong plan or length.
///
/// # Limitations
///
/// Where `to_value` and `AsValue` write the same data, these differ from
/// serializing the `to_value` result:
///
/// - Where dunder fields produce the same key more than once, the last value
///   wins as in `to_value`, but the entry is written at its last position
///   rather than its first.
/// - Only repeats produced by dunder fields, and by [`with_entry`], are
///   resolved. Repeats from serde's own `#[serde(flatten)]`, from fields
///   renamed to the same key, or from a map impl writing a key twice are
///   written twice, where `to_value` keeps the last.
/// - Maps and sequences that do not report their length (serde's own
///   `#[serde(flatten)]`, iterators) are started without one, where
///   serializing a `Value` always reports it.
/// - An error in a value that a later dunder entry overwrites is not
///   reported, since the overwritten value is never written.
/// - Map keys other than strings, chars, unit variants and integers wider
///   than 64 bits never count as repeated.
///
/// Through this crate's YAML serializer, a map key that is a wrapped
/// (`Some`, newtype) value written with `collect_str` as a `!tag` becomes a
/// YAML tag, where the `to_value` result writes it as a quoted string.
///
/// [`with_entry`]: AsValue::with_entry
pub struct AsValue<'a, T: ?Sized>(pub &'a T);

impl<'a, T: ?Sized> AsValue<'a, T> {
    /// Appends an entry to the value's top-level mapping after its own
    /// entries, replacing any entry with the same key, as inserting into the
    /// `to_value` result would. Ignored when the value is not a struct or a
    /// map: a scalar, a sequence, an enum variant or a tagged value.
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

fn serialize_as_value<V, S>(
    value: &V,
    serializer: S,
    extra: Option<Extra<'_, S::SerializeMap>>,
) -> Result<S::Ok, S::Error>
where
    V: ?Sized + Serialize,
    S: Serializer,
{
    value.serialize(Adapter {
        inner: serializer,
        value,
        extra,
        variant_data: None,
    })
}

fn nondeterministic<E: ser::Error>() -> E {
    E::custom(NONDETERMINISTIC)
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
struct VariantData<'a, V: ?Sized> {
    value: &'a V,
    analysis: &'a Analysis,
}

impl<V> Serialize for VariantData<'_, V>
where
    V: ?Sized + Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.value.serialize(Adapter {
            inner: serializer,
            value: self.value,
            extra: None,
            variant_data: Some(self.analysis),
        })
    }
}

/// Writes one value through the inner serializer. Holds the value itself so
/// that a struct or variant can be probed once its impl reaches it.
struct Adapter<'a, S: Serializer, V: ?Sized> {
    inner: S,
    value: &'a V,
    extra: Option<Extra<'a, S::SerializeMap>>,
    /// Set when writing only the data of a tuple or struct variant: what
    /// probing the variant found.
    variant_data: Option<&'a Analysis>,
}

impl<'a, S, V> Adapter<'a, S, V>
where
    S: Serializer,
    V: ?Sized + Serialize,
{
    /// Writes a tuple or struct variant as the single-entry map `to_value`
    /// makes of it. The variant's data is the value of that entry, but serde
    /// hands it over field by field, so the entry's value runs the impl
    /// again and keeps only the data; the rest of this run is ignored.
    fn tagged_variant(self, variant: &'static str, analysis: &Analysis) -> Result<S::Ok, S::Error> {
        if variant.is_empty() {
            return Err(S::Error::custom(EMPTY_TAG));
        }
        let mut map = self.inner.serialize_map(Some(1))?;
        map.serialize_entry(
            &Tag(variant),
            &VariantData {
                value: self.value,
                analysis,
            },
        )?;
        map.end()
    }
}

impl<'a, S, V> Serializer for Adapter<'a, S, V>
where
    S: Serializer,
    V: ?Sized + Serialize,
{
    type Ok = S::Ok;
    type Error = S::Error;

    type SerializeSeq = Seq<S::SerializeSeq>;
    type SerializeTuple = Seq<S::SerializeSeq>;
    type SerializeTupleStruct = Seq<S::SerializeSeq>;
    type SerializeTupleVariant = Variant<Seq<S::SerializeSeq>, S::Ok>;
    type SerializeMap = Map<'a, S::SerializeMap>;
    type SerializeStruct = Map<'a, S::SerializeMap>;
    type SerializeStructVariant = Variant<Map<'a, S::SerializeMap>, S::Ok>;

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

    fn serialize_newtype_struct<T>(self, name: &'static str, value: &T) -> Result<S::Ok, S::Error>
    where
        T: ?Sized + Serialize,
    {
        // A timestamp becomes `Value::Timestamp`, which serializes in its
        // canonical form.
        #[cfg(feature = "yaml_11")]
        if name == crate::timestamp::TOKEN {
            let repr = value
                .serialize(crate::timestamp::ExtractString)
                .map_err(S::Error::custom)?;
            let timestamp = crate::Timestamp::parse(&repr)
                .ok_or_else(|| S::Error::custom("invalid timestamp"))?;
            return timestamp.serialize(self.inner);
        }
        let _ = name;
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
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, S::Error> {
        match self.variant_data {
            Some(analysis) => {
                if !matches!(analysis.shape, Shape::TupleVariant { variant: v } if v == variant) {
                    return Err(nondeterministic());
                }
                self.inner
                    .serialize_seq(Some(len))
                    .map(|seq| Variant::Data(Seq(seq)))
            }
            // This run's own call is all a tuple variant needs probing for.
            None => {
                let analysis = Analysis {
                    shape: Shape::TupleVariant { variant },
                    ..Analysis::default()
                };
                self.tagged_variant(variant, &analysis).map(Variant::Done)
            }
        }
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, S::Error> {
        let Some(extra) = self.extra else {
            let map = self.inner.serialize_map(len)?;
            return Ok(Map::new(map, None, None, None));
        };
        let analysis = probe::analyze(self.value, Some(extra.key));
        match analysis.shape {
            // The probe counted the entries, the extra one included.
            Shape::Map => {
                let map = self.inner.serialize_map(analysis.len)?;
                let plan = analysis.plan.map(Cow::Owned);
                Ok(Map::new(map, plan, analysis.len, Some(extra)))
            }
            // A tagged value is not a mapping; the entry is not added.
            Shape::Other => {
                let map = self.inner.serialize_map(len)?;
                Ok(Map::new(map, None, None, None))
            }
            _ => Err(nondeterministic()),
        }
    }

    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, S::Error> {
        let analysis = probe::analyze(self.value, self.extra.map(|extra| extra.key));
        if !matches!(analysis.shape, Shape::Struct { name: n, len: l } if (n, l) == (name, len)) {
            return Err(nondeterministic());
        }
        // The probe counted the entries when dunder fields or an extra entry
        // change them; otherwise the struct's own length is exact.
        let map = self
            .inner
            .serialize_map(Some(analysis.len.unwrap_or(len)))?;
        let plan = analysis.plan.map(Cow::Owned);
        Ok(Map::new(map, plan, analysis.len, self.extra))
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, S::Error> {
        match self.variant_data {
            Some(analysis) => {
                if !matches!(analysis.shape, Shape::StructVariant { variant: v, len: l } if (v, l) == (variant, len))
                {
                    return Err(nondeterministic());
                }
                let map = self
                    .inner
                    .serialize_map(Some(analysis.len.unwrap_or(len)))?;
                let plan = analysis.plan.as_ref().map(Cow::Borrowed);
                Ok(Variant::Data(Map::new(map, plan, analysis.len, None)))
            }
            None => {
                let analysis = probe::analyze(self.value, None);
                if !matches!(analysis.shape, Shape::StructVariant { variant: v, len: l } if (v, l) == (variant, len))
                {
                    return Err(nondeterministic());
                }
                self.tagged_variant(variant, &analysis).map(Variant::Done)
            }
        }
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

/// A tuple or struct variant: either writing its data, or already written
/// as a tagged map, in which case the rest of the run is ignored.
enum Variant<D, O> {
    Data(D),
    Done(O),
}

impl<Q: SerializeSeq> ser::SerializeTupleVariant for Variant<Seq<Q>, Q::Ok> {
    type Ok = Q::Ok;
    type Error = Q::Error;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Q::Error>
    where
        T: ?Sized + Serialize,
    {
        match self {
            Variant::Data(seq) => seq.0.serialize_element(&AsValue(value)),
            Variant::Done(_) => Ok(()),
        }
    }

    fn end(self) -> Result<Q::Ok, Q::Error> {
        match self {
            Variant::Data(seq) => seq.0.end(),
            Variant::Done(ok) => Ok(ok),
        }
    }
}

impl<M: SerializeMap> ser::SerializeStructVariant for Variant<Map<'_, M>, M::Ok> {
    type Ok = M::Ok;
    type Error = M::Error;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), M::Error>
    where
        T: ?Sized + Serialize,
    {
        match self {
            Variant::Data(map) => map.field(key, value),
            Variant::Done(_) => Ok(()),
        }
    }

    fn end(self) -> Result<M::Ok, M::Error> {
        match self {
            Variant::Data(map) => map.finish(),
            Variant::Done(ok) => Ok(ok),
        }
    }
}

/// Writes a struct or map, inlining dunder fields and skipping the entries
/// the plan marks as overwritten.
struct Map<'a, M: SerializeMap> {
    map: M,
    plan: Option<Cow<'a, Plan>>,
    /// Index of the next entry, counting flattened and skipped ones.
    next: usize,
    /// The number of entries the probe counted, which the map was started
    /// with, and the number written so far.
    expected: Option<usize>,
    written: usize,
    /// Set when the current key was skipped, so its value is skipped too.
    skip_value: bool,
    extra: Option<Extra<'a, M>>,
}

impl<'a, M: SerializeMap> Map<'a, M> {
    fn new(
        map: M,
        plan: Option<Cow<'a, Plan>>,
        expected: Option<usize>,
        extra: Option<Extra<'a, M>>,
    ) -> Self {
        Map {
            map,
            plan,
            next: 0,
            expected,
            written: 0,
            skip_value: false,
            extra,
        }
    }

    /// Advances past one entry, returning true if a later one overwrites it.
    /// `key` is only computed when there is a plan, to check it against the
    /// probed key.
    fn skip_next(&mut self, key: impl FnOnce() -> Key) -> Result<bool, M::Error> {
        let skip = match &self.plan {
            Some(plan) => plan.entry(self.next, &key()).ok_or_else(nondeterministic)?,
            None => false,
        };
        self.next += 1;
        if !skip {
            self.written += 1;
        }
        Ok(skip)
    }

    fn key<K>(&mut self, key: &K) -> Result<(), M::Error>
    where
        K: ?Sized + Serialize,
    {
        if self.skip_next(|| Key::capture(key))? {
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
        if self.skip_next(|| Key::capture(key))? {
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
        if self.skip_next(|| Key::Static(key))? {
            return Ok(());
        }
        self.map.serialize_entry(key, &AsValue(value))
    }

    fn finish(mut self) -> Result<M::Ok, M::Error> {
        if let Some(extra) = self.extra {
            // The extra entry comes last, so it is never the one skipped.
            self.skip_next(|| Key::Static(extra.key))?;
            (extra.emit)(&mut self.map)?;
        }
        let probed = self.plan.as_ref().map(|plan| plan.entries());
        if probed.is_some_and(|probed| probed != self.next)
            || self
                .expected
                .is_some_and(|expected| expected != self.written)
        {
            return Err(nondeterministic());
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

    fn serialize_newtype_struct<T>(self, name: &'static str, value: &T) -> Result<(), M::Error>
    where
        T: ?Sized + Serialize,
    {
        #[cfg(feature = "yaml_11")]
        if name == crate::timestamp::TOKEN {
            return Err(not_mapping());
        }
        let _ = name;
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

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, M::Error> {
        Ok(FlattenEntries {
            map: self.map,
            check_tag: len == Some(1),
        })
    }

    fn serialize_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStruct, M::Error> {
        Ok(FlattenEntries {
            map: self.map,
            check_tag: false,
        })
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
    /// Set for a single-entry map, which is a tagged value rather than a
    /// mapping if its key is a `!tag`.
    check_tag: bool,
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
        // Like the Value serializer, recognize a tag only in a whole entry.
        if mem::take(&mut self.check_tag) && is_tag(key) {
            return Err(not_mapping());
        }
        self.map.entry(key, value)
    }

    fn end(self) -> Result<(), M::Error> {
        Ok(())
    }
}
