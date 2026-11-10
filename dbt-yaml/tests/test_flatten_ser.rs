#![cfg(feature = "flatten_dunder")]

use dbt_yaml::Value;
use indoc::indoc;
use serde_derive::Serialize;
use std::collections::BTreeMap;

/// Without repeated keys, `to_string` writes exactly what serializing the
/// `to_value` result writes.
fn test_flatten<T>(thing: &T, yaml: &str)
where
    T: serde::Serialize,
{
    let serialized = dbt_yaml::to_string(thing).unwrap();
    assert_eq!(yaml, serialized);

    let value = dbt_yaml::to_value(thing).unwrap();
    let serialized = dbt_yaml::to_string(&value).unwrap();
    assert_eq!(yaml, serialized);
}

/// With repeated keys, the last value wins as in `to_value`, but the entry
/// is written at its last position, so only the data matches.
fn test_flatten_overwrite<T>(thing: &T, yaml: &str)
where
    T: serde::Serialize,
{
    let serialized = dbt_yaml::to_string(thing).unwrap();
    assert_eq!(yaml, serialized);

    let reparsed: Value = dbt_yaml::from_str(&serialized).unwrap();
    assert_eq!(dbt_yaml::to_value(thing).unwrap(), reparsed);
}

#[test]
fn test_flatten_struct() {
    #[derive(Serialize)]
    struct Outer {
        name: &'static str,
        __inner__: Inner,
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
    };
    let yaml = indoc! {"
        name: box
        width: 1
        height: 2
    "};
    test_flatten(&thing, yaml);
}

#[test]
fn test_flatten_map() {
    #[derive(Serialize)]
    struct Outer {
        name: &'static str,
        __extra__: BTreeMap<&'static str, &'static str>,
    }

    let thing = Outer {
        name: "box",
        __extra__: BTreeMap::from([("color", "red")]),
    };
    let yaml = indoc! {"
        name: box
        color: red
    "};
    test_flatten(&thing, yaml);
}

#[test]
fn test_flatten_nested() {
    #[derive(Serialize)]
    struct Outer {
        name: &'static str,
        __inner__: Inner,
    }

    #[derive(Serialize)]
    struct Inner {
        width: u32,
        __extra__: Option<BTreeMap<&'static str, &'static str>>,
    }

    let thing = Outer {
        name: "box",
        __inner__: Inner {
            width: 1,
            __extra__: Some(BTreeMap::from([("color", "red")])),
        },
    };
    let yaml = indoc! {"
        name: box
        width: 1
        color: red
    "};
    test_flatten(&thing, yaml);
}

#[test]
fn test_flatten_overwrite_between_flattened() {
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
    let yaml = indoc! {"
        id: 1
        text: hello
        labels:
        - body
        version: 2
    "};
    test_flatten_overwrite(&thing, yaml);

    // Each element of a sequence is probed on its own.
    let thing = vec![thing];
    let yaml = indoc! {"
        - id: 1
          text: hello
          labels:
          - body
          version: 2
    "};
    test_flatten_overwrite(&thing, yaml);
}

#[test]
fn test_flatten_overwrite_by_field() {
    #[derive(Serialize)]
    struct Outer {
        __extra__: BTreeMap<&'static str, &'static str>,
        color: &'static str,
        inner: Inner,
    }

    #[derive(Serialize)]
    struct Inner {
        size: &'static str,
        __extra__: BTreeMap<&'static str, &'static str>,
    }

    let thing = Outer {
        __extra__: BTreeMap::from([("color", "red"), ("shape", "round")]),
        color: "blue",
        inner: Inner {
            size: "small",
            __extra__: BTreeMap::from([("size", "large")]),
        },
    };
    let yaml = indoc! {"
        shape: round
        color: blue
        inner:
          size: large
    "};
    test_flatten_overwrite(&thing, yaml);
}

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
    let error = dbt_yaml::to_string(&thing).unwrap_err();
    let expected = dbt_yaml::to_value(&thing).unwrap_err();
    assert_eq!(expected.to_string(), error.to_string());
}

#[test]
fn test_flatten_nondeterministic() {
    // Names itself differently after the first call, which is the probe.
    struct Fickle {
        calls: std::cell::Cell<u32>,
    }

    impl serde::Serialize for Fickle {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeStruct as _;
            let calls = self.calls.replace(self.calls.get() + 1);
            let name = if calls == 0 { "First" } else { "Later" };
            let mut state = serializer.serialize_struct(name, 2)?;
            state.serialize_field("__extra__", &BTreeMap::from([("x", 0)]))?;
            state.serialize_field("x", &1)?;
            state.end()
        }
    }

    let thing = Fickle {
        calls: std::cell::Cell::new(0),
    };
    let error = dbt_yaml::to_string(&thing).unwrap_err();
    assert_eq!(
        "Serialize impl made different calls when probed",
        error.to_string()
    );
}

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
    let error = dbt_yaml::to_string(&thing).unwrap_err();
    let expected = dbt_yaml::to_value(&thing).unwrap_err();
    assert_eq!(expected.to_string(), error.to_string());
}

#[test]
fn test_flatten_nondeterministic_keys() {
    // Serializes as a map with the entries in the given order.
    struct Pairs(Vec<(&'static str, u32)>);

    impl serde::Serialize for Pairs {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_map(self.0.iter().map(|(key, value)| (key, value)))
        }
    }

    // Lists its entries in another order once probed twice.
    struct Fickle {
        calls: std::cell::Cell<u32>,
    }

    impl serde::Serialize for Fickle {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeStruct as _;
            let calls = self.calls.replace(self.calls.get() + 1);
            let extra = if calls < 2 {
                Pairs(vec![("x", 2), ("y", 1)])
            } else {
                Pairs(vec![("y", 1), ("x", 2)])
            };
            let mut state = serializer.serialize_struct("Fickle", 2)?;
            state.serialize_field("__extra__", &extra)?;
            state.serialize_field("x", &3)?;
            state.end()
        }
    }

    let thing = Fickle {
        calls: std::cell::Cell::new(0),
    };
    let error = dbt_yaml::to_string(&thing).unwrap_err();
    assert_eq!(
        "Serialize impl made different calls when probed",
        error.to_string()
    );
}
