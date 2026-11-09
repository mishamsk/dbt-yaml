use dbt_yaml::{AsValue, Value};
use indoc::indoc;
use serde_derive::Serialize;
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
