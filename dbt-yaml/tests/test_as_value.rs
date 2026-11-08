use dbt_yaml::{AsValue, Value};
use serde::Serialize as SerializeTrait;
use serde_derive::Serialize;
use std::collections::BTreeMap;

/// `AsValue` writes byte for byte what serializing the `to_value` result
/// writes, through serde_json and through the YAML serializer.
fn test_same_as_value<T: SerializeTrait>(thing: &T) {
    let value = dbt_yaml::to_value(thing).unwrap();
    assert_eq!(
        serde_json::to_string(&AsValue(thing)).unwrap(),
        serde_json::to_string(&value).unwrap(),
    );
    assert_eq!(
        dbt_yaml::to_string(&AsValue(thing)).unwrap(),
        dbt_yaml::to_string(&value).unwrap(),
    );
}

struct Bytes(&'static [u8]);

impl SerializeTrait for Bytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0)
    }
}

#[derive(Serialize)]
enum Kind {
    Unit,
    Newtype(u32),
    Tuple(u32, &'static str),
    Struct { a: u32 },
}

#[derive(Serialize)]
struct Unit;

#[derive(Serialize)]
struct Wrapper(f32);

#[derive(Serialize)]
struct Scalars {
    flag: bool,
    small: i8,
    negative: i64,
    positive: i32,
    huge: u128,
    tiny: i128,
    single: f32,
    double: f64,
    nan: f64,
    letter: char,
    digits: &'static str,
    none: Option<u32>,
    some: Option<&'static str>,
    unit: Unit,
    wrapper: Wrapper,
    bytes: Bytes,
    list: Vec<u8>,
    pair: (u32, &'static str),
    numbered: BTreeMap<u32, &'static str>,
    kinds: Vec<Kind>,
    value: Value,
}

#[test]
fn test_data_model() {
    test_same_as_value(&Scalars {
        flag: true,
        small: -3,
        negative: -5,
        positive: 7,
        huge: u128::MAX,
        tiny: i128::MIN,
        single: 1.1,
        double: 2.5,
        nan: -f64::NAN,
        letter: 'a',
        digits: "007",
        none: None,
        some: Some("x"),
        unit: Unit,
        wrapper: Wrapper(0.1),
        bytes: Bytes(b"hi"),
        list: vec![1, 2],
        pair: (1, "one"),
        numbered: [(1, "one"), (2, "two")].into(),
        kinds: vec![
            Kind::Unit,
            Kind::Newtype(1),
            Kind::Tuple(2, "two"),
            Kind::Struct { a: 3 },
        ],
        value: dbt_yaml::from_str("map: {k: !Tag v}\nseq: [1, -2, 3.5, null]").unwrap(),
    });
}

#[test]
fn test_top_level_variants() {
    test_same_as_value(&Kind::Unit);
    test_same_as_value(&Kind::Newtype(1));
    test_same_as_value(&Kind::Tuple(2, "two"));
    test_same_as_value(&Kind::Struct { a: 3 });
    test_same_as_value(&Some(Kind::Struct { a: 3 }));
}

#[derive(Serialize)]
struct Node {
    name: &'static str,
    resource_type: &'static str,
}

#[derive(Serialize)]
struct Other {
    name: &'static str,
}

#[test]
fn test_with_entry() {
    fn via_value<T: SerializeTrait>(thing: &T) -> String {
        let mut value = dbt_yaml::to_value(thing).unwrap();
        if let Value::Mapping(mapping, ..) = &mut value {
            mapping.insert(
                Value::string("resource_type".into()),
                Value::string("source".into()),
            );
        }
        serde_json::to_string(&value).unwrap()
    }

    // A new key lands at the end, as inserting into the mapping puts it.
    let other = Other { name: "n" };
    assert_eq!(
        serde_json::to_string(&AsValue(&other).with_entry("resource_type", "source")).unwrap(),
        via_value(&other),
    );

    // An existing key is replaced; it moves to the end, unlike an insert.
    let node = Node {
        name: "n",
        resource_type: "model",
    };
    let streamed = serde_json::to_string(&AsValue(&node).with_entry("resource_type", "source"));
    assert_eq!(
        streamed.unwrap(),
        r#"{"name":"n","resource_type":"source"}"#
    );

    // Values that are not mappings are left alone.
    assert_eq!(
        serde_json::to_string(&AsValue(&1).with_entry("resource_type", "source")).unwrap(),
        via_value(&1),
    );
}

#[cfg(feature = "flatten_dunder")]
mod flatten {
    use super::*;

    /// With colliding keys, the data matches `to_value` but the surviving
    /// entry sits at its last position.
    fn test_same_data_as_value<T: SerializeTrait>(thing: &T, expected: &str) {
        let streamed = serde_json::to_string(&AsValue(thing)).unwrap();
        assert_eq!(streamed, expected);
        let reparsed: serde_json::Value = serde_json::from_str(&streamed).unwrap();
        let via_value = serde_json::to_value(dbt_yaml::to_value(thing).unwrap()).unwrap();
        assert_eq!(reparsed, via_value);
    }

    fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[derive(Serialize)]
    struct Outer {
        name: &'static str,
        __attrs__: Attrs,
        __other__: BTreeMap<String, String>,
    }

    #[derive(Serialize)]
    struct Attrs {
        size: u32,
        __more__: Option<BTreeMap<String, String>>,
    }

    #[derive(Serialize)]
    enum Variant {
        Flat { a: u32, __rest__: Attrs },
    }

    #[test]
    fn test_flatten() {
        let outer = Outer {
            name: "x",
            __attrs__: Attrs {
                size: 1,
                __more__: Some(map(&[("deep", "d")])),
            },
            __other__: map(&[("extra", "e")]),
        };
        test_same_as_value(&outer);
        test_same_as_value(&vec![outer]);
        test_same_as_value(&Variant::Flat {
            a: 1,
            __rest__: Attrs {
                size: 2,
                __more__: Some(map(&[("deep", "d")])),
            },
        });
    }

    #[test]
    fn test_flatten_not_mapping() {
        #[derive(Serialize)]
        struct Bad {
            __rest__: Option<BTreeMap<String, String>>,
        }

        let bad = Bad { __rest__: None };
        let streamed = serde_json::to_string(&AsValue(&bad)).unwrap_err();
        let via_value = dbt_yaml::to_value(&bad).unwrap_err();
        assert_eq!(streamed.to_string(), via_value.to_string());
    }

    #[test]
    fn test_flatten_collision() {
        // The shape of fusion's semantic model node: `unrendered_config` is
        // in both the base attributes and the node's own attributes.
        #[derive(Serialize)]
        struct Node {
            __base_attr__: Base,
            __semantic_model_attr__: Attr,
            deprecated_config: u32,
        }

        #[derive(Serialize)]
        struct Base {
            name: &'static str,
            unrendered_config: BTreeMap<String, String>,
        }

        #[derive(Serialize)]
        struct Attr {
            model: &'static str,
            unrendered_config: BTreeMap<String, String>,
        }

        let node = Node {
            __base_attr__: Base {
                name: "m",
                unrendered_config: map(&[("from", "base")]),
            },
            __semantic_model_attr__: Attr {
                model: "m",
                unrendered_config: map(&[("from", "attr")]),
            },
            deprecated_config: 1,
        };
        test_same_data_as_value(
            &node,
            r#"{"name":"m","model":"m","unrendered_config":{"from":"attr"},"deprecated_config":1}"#,
        );
        test_same_data_as_value(
            &AsValue(&node).with_entry("name", "renamed"),
            r#"{"model":"m","unrendered_config":{"from":"attr"},"deprecated_config":1,"name":"renamed"}"#,
        );
    }
}
