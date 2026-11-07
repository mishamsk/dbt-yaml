#![cfg(feature = "flatten_dunder")]

use dbt_yaml::Value;
use indoc::indoc;
use serde_derive::Serialize;
use std::collections::BTreeMap;

/// Without colliding keys, streaming matches the `to_value` path byte for
/// byte.
fn test_same_as_value<T: serde::Serialize>(thing: &T, expected: &str) {
    let streamed = dbt_yaml::to_string(thing).unwrap();
    assert_eq!(streamed, expected);
    let via_value = dbt_yaml::to_string(&dbt_yaml::to_value(thing).unwrap()).unwrap();
    assert_eq!(streamed, via_value);
}

/// With colliding keys, the last value wins as in `to_value`, but the entry
/// sits at its last occurrence, so only the data (not the order) matches.
fn test_same_data_as_value<T: serde::Serialize>(thing: &T, expected: &str) {
    let streamed = dbt_yaml::to_string(thing).unwrap();
    assert_eq!(streamed, expected);
    let reparsed: Value = dbt_yaml::from_str(&streamed).unwrap();
    assert_eq!(reparsed, dbt_yaml::to_value(thing).unwrap());
}

fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn test_flatten_struct_and_map() {
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

    let thing = Outer {
        name: "x",
        __attrs__: Attrs {
            size: 1,
            __more__: Some(map(&[("deep", "true story")])),
        },
        __other__: map(&[("extra", "e")]),
    };
    test_same_as_value(
        &thing,
        indoc! {"
            name: x
            size: 1
            deep: true story
            extra: e
        "},
    );
}

#[test]
fn test_flatten_collision_last_wins() {
    // The shape of fusion's semantic model node: `unrendered_config` exists
    // in both the base attributes and the node's own attributes, and the
    // latter is authoritative.
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

    let thing = Node {
        __base_attr__: Base {
            name: "m",
            unrendered_config: map(&[("from", "base")]),
        },
        __semantic_model_attr__: Attr {
            model: "ref('m')",
            unrendered_config: map(&[("from", "attr")]),
        },
        deprecated_config: 1,
    };
    test_same_data_as_value(
        &thing,
        indoc! {"
            name: m
            model: ref('m')
            unrendered_config:
              from: attr
            deprecated_config: 1
        "},
    );

    // Each element of a sequence is probed on its own.
    test_same_data_as_value(
        &vec![thing],
        indoc! {"
            - name: m
              model: ref('m')
              unrendered_config:
                from: attr
              deprecated_config: 1
        "},
    );
}

#[test]
fn test_flatten_collision_with_later_field() {
    #[derive(Serialize)]
    struct Outer {
        __rest__: BTreeMap<String, String>,
        a: &'static str,
        inner: Inner,
    }

    #[derive(Serialize)]
    struct Inner {
        b: &'static str,
        __rest__: BTreeMap<String, String>,
    }

    let thing = Outer {
        __rest__: map(&[("a", "flattened"), ("c", "kept")]),
        a: "field",
        inner: Inner {
            b: "field",
            __rest__: map(&[("b", "flattened")]),
        },
    };
    test_same_data_as_value(
        &thing,
        indoc! {"
            c: kept
            a: field
            inner:
              b: flattened
        "},
    );
}

#[test]
fn test_flatten_not_mapping() {
    #[derive(Serialize)]
    struct Outer {
        a: u32,
        __rest__: Option<BTreeMap<String, String>>,
    }

    let thing = Outer {
        a: 1,
        __rest__: None,
    };
    let streamed = dbt_yaml::to_string(&thing).unwrap_err();
    let via_value = dbt_yaml::to_value(&thing).unwrap_err();
    assert_eq!(streamed.to_string(), via_value.to_string());
}
