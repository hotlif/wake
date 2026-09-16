use wake_common::Interner;
use wake_ecma_parser::{SourceType, parse};

#[test]
fn legacy_function_declarations_only_occupy_permitted_statement_positions() {
    for source in [
        "if(flag) function f(){} else function g(){}",
        "label: function f(){}",
        "label: inner: function f(){}",
        "while(flag) {function f(){}}",
        "if(flag) {async function f(){}}",
    ] {
        let parsed = parse(source, &Interner::new(), SourceType::Script);
        assert!(!parsed.has_errors(), "{source}: {:?}", parsed.diagnostics);
    }
    for (source, kind) in [
        ("if(flag) function f(){}", SourceType::Module),
        ("'use strict'; if(flag) function f(){}", SourceType::Script),
        ("label: function f(){}", SourceType::Module),
        ("if(flag) async function f(){}", SourceType::Script),
        ("if(flag) function* f(){}", SourceType::Script),
        ("if(flag) label: function f(){}", SourceType::Script),
        ("while(flag) function f(){}", SourceType::Script),
        ("do function f(){} while(flag)", SourceType::Script),
        ("for(;;) function f(){}", SourceType::Script),
        ("for(var x in obj) function f(){}", SourceType::Script),
        ("for(var x of list) function f(){}", SourceType::Script),
        ("with(obj) function f(){}", SourceType::Script),
        ("while(flag) label: function f(){}", SourceType::Script),
        ("label: async function f(){}", SourceType::Script),
        ("label: function* f(){}", SourceType::Script),
    ] {
        let parsed = parse(source, &Interner::new(), kind);
        assert!(
            parsed.has_errors(),
            "invalid function placement accepted: {source}"
        );
    }
}
