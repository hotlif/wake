use std::collections::{BTreeSet, HashSet};

use wake_common::{Interner, JsAtom, JsString};

#[test]
fn all_individual_utf16_code_units_round_trip() {
    for unit in 0..=u16::MAX {
        let value = JsString::from_utf16(&[unit]);
        assert_eq!(value.code_units().collect::<Vec<_>>(), [unit]);
        assert_eq!(value.len_utf16(), 1);
        assert_eq!(value.as_str().is_none(), (0xd800..=0xdfff).contains(&unit));
    }
}

#[test]
fn scalar_values_have_one_identity_across_constructors() {
    for text in ["", "hello", "界面", "👍", "\0", "\u{fffd}"] {
        let text_value = JsString::from(text);
        let utf16_value = JsString::from_utf16(&text.encode_utf16().collect::<Vec<_>>());
        assert_eq!(text_value, utf16_value);
        assert_eq!(utf16_value.as_str(), Some(text));
        assert_eq!(text_value.len_utf16(), text.encode_utf16().count());
        assert_eq!(HashSet::from([text_value, utf16_value]).len(), 1);
    }
}

#[test]
fn invalid_sequences_remain_distinct_from_displayable_strings() {
    let values = [
        JsString::from_utf16(&[0xd800]),
        JsString::from_utf16(&[0xdc00]),
        JsString::from_utf16(&[0xdc00, 0xd800]),
        JsString::from("\u{fffd}"),
        JsString::from("\\ud800"),
        JsString::from(""),
    ];
    assert_eq!(HashSet::from(values.clone()).len(), values.len());
    assert_eq!(BTreeSet::from(values.clone()).len(), values.len());
    assert!(values[0].as_str().is_none());
    assert!(!values[0].is_empty());
    assert!(values[5].is_empty());
}

#[test]
fn concatenation_can_form_pairs_across_the_boundary() {
    let high = JsString::from_utf16(&[0xd83d]);
    let low = JsString::from_utf16(&[0xdc4d]);
    let combined = high.concat(&low);
    assert_eq!(combined, JsString::from("👍"));
    assert_eq!(combined.as_str(), Some("👍"));
    assert_eq!(HashSet::from([combined, JsString::from("👍")]).len(), 1);
    assert_eq!(
        low.concat(&high).code_units().collect::<Vec<_>>(),
        [0xdc4d, 0xd83d]
    );
    assert_eq!(
        high.concat(&JsString::from("x"))
            .code_units()
            .collect::<Vec<_>>(),
        [0xd83d, 120]
    );
    assert_eq!(
        JsString::from("x")
            .concat(&low)
            .code_units()
            .collect::<Vec<_>>(),
        [120, 0xdc4d]
    );
    assert_eq!(
        JsString::from("a").concat(&JsString::from("b")),
        JsString::from("ab")
    );
}

#[test]
fn comparison_uses_utf16_order_rather_than_unicode_scalar_order() {
    // D800 DC00 sorts before E000 in ECMAScript, unlike UTF-8/scalar ordering.
    let supplementary = JsString::from("\u{10000}");
    let bmp = JsString::from("\u{e000}");
    let high = JsString::from_utf16(&[0xd800]);
    let short = JsString::from("a");
    let long = JsString::from("aa");
    assert!(supplementary < bmp);
    assert!(high < supplementary);
    assert!(short < long);
}

#[test]
fn every_surrogate_pair_boundary_is_lossless() {
    for high in [0xd800, 0xd801, 0xdbfe, 0xdbff] {
        for low in 0xdc00..=0xdfff {
            let value = JsString::from_utf16(&[high, low]);
            assert_eq!(value.code_units().collect::<Vec<_>>(), [high, low]);
            assert!(value.as_str().is_some());
            assert_eq!(value.len_utf16(), 2);
        }
    }
}

#[test]
fn interned_strings_keep_code_unit_identity_without_crossing_atom_types() {
    let interner = Interner::new();
    assert_eq!(std::mem::size_of::<JsAtom>(), 4);
    let plain = JsString::from("👍");
    let escaped = JsString::from_utf16(&[0xd83d, 0xdc4d]);
    let scalar = interner.intern_js(&plain);
    assert_eq!(scalar, interner.intern_js(&escaped));
    assert_eq!(interner.resolve_js(scalar), plain);
    let lone = interner.intern_js(JsString::from_utf16(&[0xd800]));
    assert_ne!(lone, interner.intern_js("\u{fffd}"));
    assert_ne!(lone, interner.intern_js("\\ud800"));
    assert_eq!(
        interner.resolve_js(lone).code_units().collect::<Vec<_>>(),
        [0xd800]
    );
    // Ordinary identifier atoms still round-trip through their existing UTF-8 table.
    let name = interner.intern("name");
    assert_eq!(interner.resolve(name), "name");
}

#[test]
fn concurrent_interning_deduplicates_lossless_values() {
    let interner = std::sync::Arc::new(Interner::new());
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let interner = interner.clone();
            std::thread::spawn(move || {
                (0xd800..=0xdfff)
                    .map(|unit| interner.intern_js(JsString::from_utf16(&[unit])))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    for other in &results[1..] {
        assert_eq!(other, &results[0]);
    }
    for (unit, atom) in (0xd800..=0xdfff).zip(&results[0]) {
        assert_eq!(
            interner.resolve_js(*atom).code_units().collect::<Vec<_>>(),
            [unit]
        );
    }
}
