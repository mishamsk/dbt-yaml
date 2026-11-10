use dbt_yaml::{AsValue, Value};
use indoc::indoc;
use serde::ser::{Impossible, SerializeStruct as _};
use serde_derive::Serialize;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

// serde_json is a dev-dependency only for these tests: `AsValue` must behave
// the same through any serde serializer, so each case also goes through a
// serializer other than this crate's own.

/// `AsValue` writes exactly what serializing the `to_value` result writes.
fn test_as_value<T>(thing: &T, yaml: &str)
where
    T: serde::Serialize,
{
    let value = dbt_yaml::to_value(thing).unwrap();
    let serialized = dbt_yaml::to_string(&value).unwrap();
    assert_eq!(yaml, serialized);

    let serialized = dbt_yaml::to_string(&AsValue(thing)).unwrap();
    assert_eq!(yaml, serialized);

    let json = serde_json::to_string(&value).unwrap();
    let serialized = serde_json::to_string(&AsValue(thing)).unwrap();
    assert_eq!(json, serialized);
}

/// `thing` writes the same data as `expected`, with entries in another
/// order.
fn test_as_value_reordered<T>(thing: &T, expected: &Value, yaml: &str)
where
    T: serde::Serialize,
{
    let serialized = dbt_yaml::to_string(thing).unwrap();
    assert_eq!(yaml, serialized);
    let reparsed: Value = dbt_yaml::from_str(&serialized).unwrap();
    assert_eq!(*expected, reparsed);

    let json: serde_json::Value = serde_json::to_value(thing).unwrap();
    assert_eq!(serde_json::to_value(expected).unwrap(), json);
}

#[test]
fn test_int() {
    #[derive(Serialize)]
    struct Ints {
        small: i8,
        positive: i32,
        max: u64,
    }
    let thing = Ints {
        small: -3,
        positive: 7,
        max: u64::MAX,
    };
    let yaml = indoc! {"
        small: -3
        positive: 7
        max: 18446744073709551615
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_int_wider_than_64_bits() {
    #[derive(Serialize)]
    struct Wide {
        max: u128,
        min: i128,
    }
    let thing = Wide {
        max: u128::MAX,
        min: i128::MIN,
    };
    let yaml = indoc! {"
        max: '340282366920938463463374607431768211455'
        min: '-170141183460469231731687303715884105728'
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_float32() {
    let thing = 1.1f32;
    let yaml = indoc! {"
        1.100000023841858
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_nan() {
    let thing = -f64::NAN;
    let yaml = indoc! {"
        .nan
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_char() {
    let thing = 'a';
    let yaml = indoc! {"
        a
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_bytes() {
    struct Bytes(&'static [u8]);

    impl serde::Serialize for Bytes {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bytes(self.0)
        }
    }

    let thing = Bytes(b"hi");
    let yaml = indoc! {"
        - 104
        - 105
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_option() {
    #[derive(Serialize)]
    struct Options {
        none: Option<u32>,
        some: Option<&'static str>,
    }
    let thing = Options {
        none: None,
        some: Some("x"),
    };
    let yaml = indoc! {"
        none: null
        some: x
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_unit_struct() {
    #[derive(Serialize)]
    struct Foo;
    let thing = Foo;
    let yaml = indoc! {"
        null
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_newtype_struct() {
    #[derive(Serialize)]
    struct OriginalType {
        v: u16,
    }
    #[derive(Serialize)]
    struct NewType(OriginalType);
    let thing = NewType(OriginalType { v: 1 });
    let yaml = indoc! {"
        v: 1
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_tuple() {
    let thing = (1, "one");
    let yaml = indoc! {"
        - 1
        - one
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_map_integer_keys() {
    let thing = BTreeMap::from([(1, "one"), (2, "two")]);
    let yaml = indoc! {"
        1: one
        2: two
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_unit_variant() {
    #[derive(Serialize)]
    enum Variant {
        First,
    }
    let thing = Variant::First;
    let yaml = indoc! {"
        First
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_newtype_variant() {
    #[derive(Serialize)]
    enum Variant {
        Size(usize),
    }
    let thing = Variant::Size(127);
    let yaml = indoc! {"
        !Size 127
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_tuple_variant() {
    #[derive(Serialize)]
    enum Variant {
        Rgb(u8, u8, u8),
    }
    let thing = Variant::Rgb(32, 64, 96);
    let yaml = indoc! {"
        !Rgb
        - 32
        - 64
        - 96
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_struct_variant() {
    #[derive(Serialize)]
    enum Variant {
        Color { r: u8, g: u8, b: u8 },
    }
    let thing = Variant::Color {
        r: 32,
        g: 64,
        b: 96,
    };
    let yaml = indoc! {"
        !Color
        r: 32
        g: 64
        b: 96
    "};
    test_as_value(&thing, yaml);

    let thing = vec![thing];
    let yaml = indoc! {"
        - !Color
          r: 32
          g: 64
          b: 96
    "};
    test_as_value(&thing, yaml);
}

#[test]
fn test_tagged_value() {
    let thing: Value = dbt_yaml::from_str("k: !Tag v").unwrap();
    let yaml = indoc! {"
        k: !Tag v
    "};
    test_as_value(&thing, yaml);
}

/// The `to_value` result of `thing` with an entry inserted into its mapping.
fn insert_entry<T>(thing: &T, key: &str, entry: &str) -> Value
where
    T: serde::Serialize,
{
    let mut value = dbt_yaml::to_value(thing).unwrap();
    if let Value::Mapping(mapping, ..) = &mut value {
        mapping.insert(
            Value::string(key.to_owned()),
            Value::string(entry.to_owned()),
        );
    }
    value
}

#[test]
fn test_with_entry() {
    #[derive(Serialize)]
    struct Basic {
        x: u32,
    }
    let thing = Basic { x: 1 };
    let serialized = dbt_yaml::to_string(&AsValue(&thing).with_entry("y", "two")).unwrap();
    let expected = dbt_yaml::to_string(&insert_entry(&thing, "y", "two")).unwrap();
    assert_eq!(expected, serialized);
    let yaml = indoc! {"
        x: 1
        y: two
    "};
    assert_eq!(yaml, serialized);
}

#[test]
fn test_with_entry_existing_key() {
    #[derive(Serialize)]
    struct Basic {
        x: u32,
        y: &'static str,
    }
    let thing = Basic { x: 1, y: "one" };
    // The entry replaces the struct's own, but moves to the end.
    let yaml = indoc! {"
        x: 1
        y: two
    "};
    test_as_value_reordered(
        &AsValue(&thing).with_entry("y", "two"),
        &insert_entry(&thing, "y", "two"),
        yaml,
    );
}

#[test]
fn test_with_entry_not_mapping() {
    let thing = 1;
    let serialized = dbt_yaml::to_string(&AsValue(&thing).with_entry("y", "two")).unwrap();
    let yaml = indoc! {"
        1
    "};
    assert_eq!(yaml, serialized);
}

#[cfg(feature = "flatten_dunder")]
#[test]
fn test_flatten() {
    #[derive(Serialize)]
    struct Outer {
        name: &'static str,
        __inner__: Inner,
        __extra__: BTreeMap<&'static str, &'static str>,
    }
    #[derive(Serialize)]
    struct Inner {
        width: u32,
        __more__: Option<BTreeMap<&'static str, &'static str>>,
    }
    let thing = Outer {
        name: "box",
        __inner__: Inner {
            width: 1,
            __more__: Some(BTreeMap::from([("height", "2")])),
        },
        __extra__: BTreeMap::from([("color", "red")]),
    };
    let yaml = indoc! {"
        name: box
        width: 1
        height: '2'
        color: red
    "};
    test_as_value(&thing, yaml);
}

#[cfg(feature = "flatten_dunder")]
#[test]
fn test_flatten_struct_variant() {
    #[derive(Serialize)]
    enum Variant {
        Shape { name: &'static str, __size__: Size },
    }
    #[derive(Serialize)]
    struct Size {
        width: u32,
        height: u32,
    }
    let thing = Variant::Shape {
        name: "box",
        __size__: Size {
            width: 1,
            height: 2,
        },
    };
    let yaml = indoc! {"
        !Shape
        name: box
        width: 1
        height: 2
    "};
    test_as_value(&thing, yaml);
}

#[cfg(feature = "flatten_dunder")]
#[test]
fn test_flatten_overwrite() {
    #[derive(Serialize)]
    struct Record {
        __header__: Header,
        __body__: Body,
        version: u32,
    }
    #[derive(Serialize)]
    struct Header {
        id: u32,
        labels: Vec<&'static str>,
    }
    #[derive(Serialize)]
    struct Body {
        text: &'static str,
        labels: Vec<&'static str>,
    }
    let thing = Record {
        __header__: Header {
            id: 1,
            labels: vec!["header"],
        },
        __body__: Body {
            text: "hello",
            labels: vec!["body"],
        },
        version: 2,
    };
    // The body's labels win, at their own position.
    let yaml = indoc! {"
        id: 1
        text: hello
        labels:
        - body
        version: 2
    "};
    let expected = dbt_yaml::to_value(&thing).unwrap();
    test_as_value_reordered(&AsValue(&thing), &expected, yaml);

    let yaml = indoc! {"
        text: hello
        labels:
        - body
        version: 2
        id: 3
    "};
    let mut expected = expected;
    if let Value::Mapping(mapping, ..) = &mut expected {
        mapping.insert(Value::string("id".to_owned()), Value::from(3));
    }
    test_as_value_reordered(&AsValue(&thing).with_entry("id", &3), &expected, yaml);
}

#[cfg(feature = "flatten_dunder")]
#[test]
fn test_flatten_not_mapping() {
    #[derive(Serialize)]
    struct Outer {
        name: &'static str,
        __extra__: Option<BTreeMap<&'static str, &'static str>>,
    }
    let thing = Outer {
        name: "box",
        __extra__: None,
    };
    let error = serde_json::to_string(&AsValue(&thing)).unwrap_err();
    let expected = dbt_yaml::to_value(&thing).unwrap_err();
    assert_eq!(expected.to_string(), error.to_string());
}

#[cfg(feature = "yaml_11")]
#[test]
fn test_timestamp() {
    #[derive(Serialize)]
    struct Event {
        at: dbt_yaml::Timestamp,
    }
    let thing = Event {
        at: dbt_yaml::Timestamp::parse("2001-12-14 21:59:43.10 -5").unwrap(),
    };
    let yaml = indoc! {"
        at: 2001-12-14T21:59:43.100-05:00
    "};
    test_as_value(&thing, yaml);
}

/// Reports the length a value's top-level map is started with.
struct MapLen;

struct MapLenEntries(Option<usize>);

type MapLenError = serde::de::value::Error;

fn not_a_map<T>() -> Result<T, MapLenError> {
    Err(serde::ser::Error::custom("not a map"))
}

impl serde::Serializer for MapLen {
    type Ok = Option<usize>;
    type Error = MapLenError;
    type SerializeSeq = Impossible<Self::Ok, MapLenError>;
    type SerializeTuple = Impossible<Self::Ok, MapLenError>;
    type SerializeTupleStruct = Impossible<Self::Ok, MapLenError>;
    type SerializeTupleVariant = Impossible<Self::Ok, MapLenError>;
    type SerializeMap = MapLenEntries;
    type SerializeStruct = Impossible<Self::Ok, MapLenError>;
    type SerializeStructVariant = Impossible<Self::Ok, MapLenError>;

    fn serialize_map(self, len: Option<usize>) -> Result<MapLenEntries, MapLenError> {
        Ok(MapLenEntries(len))
    }

    fn serialize_bool(self, _: bool) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_i64(self, _: i64) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_i8(self, _: i8) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_i16(self, _: i16) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_i32(self, _: i32) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_u8(self, _: u8) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_u16(self, _: u16) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_u32(self, _: u32) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_u64(self, _: u64) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_f32(self, _: f32) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_f64(self, _: f64) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_char(self, _: char) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_str(self, _: &str) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_none(self) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_some<T: ?Sized + serde::Serialize>(self, _: &T) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_unit(self) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_newtype_struct<T: ?Sized + serde::Serialize>(
        self,
        _: &'static str,
        _: &T,
    ) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_newtype_variant<T: ?Sized + serde::Serialize>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<Self::Ok, MapLenError> {
        not_a_map()
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, MapLenError> {
        not_a_map()
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, MapLenError> {
        not_a_map()
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, MapLenError> {
        not_a_map()
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, MapLenError> {
        not_a_map()
    }
    fn serialize_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStruct, MapLenError> {
        not_a_map()
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, MapLenError> {
        not_a_map()
    }
}

impl serde::ser::SerializeMap for MapLenEntries {
    type Ok = Option<usize>;
    type Error = MapLenError;

    fn serialize_key<T: ?Sized + serde::Serialize>(&mut self, _: &T) -> Result<(), MapLenError> {
        Ok(())
    }
    fn serialize_value<T: ?Sized + serde::Serialize>(&mut self, _: &T) -> Result<(), MapLenError> {
        Ok(())
    }
    fn end(self) -> Result<Self::Ok, MapLenError> {
        Ok(self.0)
    }
}

fn map_len<T: serde::Serialize>(thing: &T) -> Option<usize> {
    thing.serialize(MapLen).unwrap()
}

#[test]
fn test_map_len() {
    #[derive(Serialize)]
    struct Basic {
        x: u32,
        y: u32,
    }
    let thing = Basic { x: 1, y: 2 };
    assert_eq!(Some(2), map_len(&AsValue(&thing)));
    assert_eq!(Some(3), map_len(&AsValue(&thing).with_entry("z", "three")));
    assert_eq!(Some(2), map_len(&AsValue(&thing).with_entry("y", "two")));

    let thing = BTreeMap::from([("x", 1), ("y", 2)]);
    assert_eq!(Some(2), map_len(&AsValue(&thing)));
    assert_eq!(Some(3), map_len(&AsValue(&thing).with_entry("z", &3)));
    assert_eq!(Some(2), map_len(&AsValue(&thing).with_entry("y", &3)));
}

#[cfg(feature = "flatten_dunder")]
#[test]
fn test_flatten_map_len() {
    #[derive(Serialize)]
    struct Outer {
        name: &'static str,
        __inner__: Inner,
        __extra__: BTreeMap<&'static str, &'static str>,
    }
    #[derive(Serialize)]
    struct Inner {
        width: u32,
        height: u32,
    }
    let thing = Outer {
        name: "box",
        __inner__: Inner {
            width: 1,
            height: 2,
        },
        __extra__: BTreeMap::from([("color", "red"), ("shape", "square")]),
    };
    let expected = map_len(&dbt_yaml::to_value(&thing).unwrap());
    assert_eq!(Some(5), expected);
    assert_eq!(expected, map_len(&AsValue(&thing)));

    // A repeated key is counted once.
    let thing = Outer {
        name: "box",
        __inner__: Inner {
            width: 1,
            height: 2,
        },
        __extra__: BTreeMap::from([("width", "wide")]),
    };
    let expected = map_len(&dbt_yaml::to_value(&thing).unwrap());
    assert_eq!(Some(3), expected);
    assert_eq!(expected, map_len(&AsValue(&thing)));
}

#[test]
fn test_nondeterministic() {
    // Names itself differently on every call after the first.
    struct Fickle {
        calls: Cell<u32>,
    }

    impl serde::Serialize for Fickle {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let calls = self.calls.replace(self.calls.get() + 1);
            let name = if calls == 0 { "First" } else { "Later" };
            let mut state = serializer.serialize_struct(name, 1)?;
            state.serialize_field("x", &1)?;
            state.end()
        }
    }

    let thing = Fickle {
        calls: Cell::new(0),
    };
    let error = serde_json::to_string(&AsValue(&thing)).unwrap_err();
    assert_eq!(
        "Serialize impl made different calls when probed",
        error.to_string()
    );
}

/// Serializes as a map with the entries in the given order.
#[cfg(feature = "flatten_dunder")]
struct Pairs(Vec<(&'static str, u32)>);

#[cfg(feature = "flatten_dunder")]
impl serde::Serialize for Pairs {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().map(|(key, value)| (key, value)))
    }
}

#[test]
fn test_with_entry_ignored() {
    #[derive(Serialize)]
    enum Variant {
        Size(u32),
        Rgb(u8, u8),
        Color { r: u8, g: u8 },
    }
    let things = [
        Variant::Size(1),
        Variant::Rgb(1, 2),
        Variant::Color { r: 1, g: 2 },
    ];
    for thing in &things {
        let expected = serde_json::to_string(&dbt_yaml::to_value(thing).unwrap()).unwrap();
        let serialized =
            serde_json::to_string(&AsValue(thing).with_entry("r", "replaced")).unwrap();
        assert_eq!(expected, serialized);
    }

    let thing: Value = dbt_yaml::from_str("!Tag v").unwrap();
    let serialized = dbt_yaml::to_string(&AsValue(&thing).with_entry("r", "replaced")).unwrap();
    let yaml = indoc! {"
        !Tag v
    "};
    assert_eq!(yaml, serialized);
}

#[test]
fn test_single_use_serialize() {
    // Hands out its elements once, as an iterator would.
    struct Once(RefCell<Option<Vec<u32>>>);

    impl serde::Serialize for Once {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_seq(self.0.borrow_mut().take().unwrap_or_default())
        }
    }

    #[derive(Serialize)]
    struct Outer {
        items: Once,
    }

    let thing = Outer {
        items: Once(RefCell::new(Some(vec![1, 2]))),
    };
    let serialized = serde_json::to_string(&AsValue(&thing)).unwrap();
    assert_eq!(r#"{"items":[1,2]}"#, serialized);
}

#[cfg(feature = "flatten_dunder")]
#[test]
fn test_flatten_tagged_value() {
    #[derive(Serialize)]
    struct Outer {
        name: &'static str,
        __extra__: Value,
    }
    let thing = Outer {
        name: "box",
        __extra__: dbt_yaml::from_str("!Tag v").unwrap(),
    };
    let error = serde_json::to_string(&AsValue(&thing)).unwrap_err();
    let expected = dbt_yaml::to_value(&thing).unwrap_err();
    assert_eq!(expected.to_string(), error.to_string());
}

#[cfg(feature = "flatten_dunder")]
#[test]
fn test_flatten_wide_integer_key() {
    #[derive(Serialize)]
    struct Outer {
        __numbers__: BTreeMap<u128, &'static str>,
        __names__: BTreeMap<&'static str, &'static str>,
    }
    // The Value serializer turns the number into the same string key.
    let thing = Outer {
        __numbers__: BTreeMap::from([(u128::MAX, "number")]),
        __names__: BTreeMap::from([("340282366920938463463374607431768211455", "name")]),
    };
    let yaml = indoc! {"
        '340282366920938463463374607431768211455': name
    "};
    test_as_value(&thing, yaml);
}

#[cfg(all(feature = "flatten_dunder", feature = "yaml_11"))]
#[test]
fn test_flatten_timestamp_key() {
    #[derive(Serialize)]
    struct Outer {
        __dates__: Value,
        __names__: Value,
    }
    // A timestamp key and a string key that reads the same are distinct.
    let thing = Outer {
        __dates__: dbt_yaml::from_str("2002-01-01: date").unwrap(),
        __names__: dbt_yaml::from_str("'2002-01-01': name").unwrap(),
    };
    let yaml = indoc! {"
        2002-01-01: date
        '2002-01-01': name
    "};
    test_as_value(&thing, yaml);
}

#[cfg(feature = "flatten_dunder")]
#[test]
fn test_flatten_nondeterministic_keys() {
    // Lists its entries in another order on the first call, which is the one
    // written.
    struct Fickle {
        calls: Cell<u32>,
    }

    impl serde::Serialize for Fickle {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let calls = self.calls.replace(self.calls.get() + 1);
            let extra = if calls == 0 {
                Pairs(vec![("y", 1), ("x", 2)])
            } else {
                Pairs(vec![("x", 2), ("y", 1)])
            };
            let mut state = serializer.serialize_struct("Fickle", 2)?;
            state.serialize_field("__extra__", &extra)?;
            state.serialize_field("x", &3)?;
            state.end()
        }
    }

    let thing = Fickle {
        calls: Cell::new(0),
    };
    let error = serde_json::to_string(&AsValue(&thing)).unwrap_err();
    assert_eq!(
        "Serialize impl made different calls when probed",
        error.to_string()
    );
}

#[cfg(feature = "flatten_dunder")]
#[test]
fn test_flatten_nondeterministic_count() {
    // Has one more entry on the first call, which is the one written.
    struct Fickle {
        calls: Cell<u32>,
    }

    impl serde::Serialize for Fickle {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let calls = self.calls.replace(self.calls.get() + 1);
            let extra = if calls == 0 {
                Pairs(vec![("a", 1), ("b", 2)])
            } else {
                Pairs(vec![("a", 1)])
            };
            let mut state = serializer.serialize_struct("Fickle", 2)?;
            state.serialize_field("__extra__", &extra)?;
            state.serialize_field("z", &3)?;
            state.end()
        }
    }

    let thing = Fickle {
        calls: Cell::new(0),
    };
    let error = serde_json::to_string(&AsValue(&thing)).unwrap_err();
    assert_eq!(
        "Serialize impl made different calls when probed",
        error.to_string()
    );
}
