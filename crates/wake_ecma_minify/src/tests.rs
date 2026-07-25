//! wake_ecma_minify integration tests.

use crate::mangle::{
    Mangling, plan_mangle, nth_name, is_reserved
};
use wake_common::Interner;
use wake_ecma_ast::SourceType;
use wake_ecma_parser::parse;

fn plan(src: &str) -> (Mangling, Interner) {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "parse errors: {:?}", out.diagnostics);
    let m = out.module.with_ast(|p| plan_mangle(p, &it));
    (m, it)
}

fn new_names(m: &Mangling, it: &Interner) -> std::collections::BTreeSet<String> {
    m.table().values().map(|a| it.resolve(*a)).collect()
}

#[test]
fn nth_name_distinct_and_valid() {
    let mut seen = std::collections::HashSet::new();
    for i in 0..5000 {
        let n = nth_name(i);
        assert!(!n.is_empty());
        assert!(n.as_bytes()[0].is_ascii_alphabetic(), "{n}");
        assert!(n.bytes().all(|b| b.is_ascii_alphanumeric()), "{n}");
        assert!(seen.insert(n.clone()), "重复: {n} @ {i}");
    }
    assert_eq!(nth_name(0), "a");
    assert_eq!(nth_name(1), "b");
    assert_eq!(nth_name(25), "z");
    assert_eq!(nth_name(26), "A");
    assert_eq!(nth_name(51), "Z");
    assert_eq!(nth_name(52).len(), 2);
}

#[test]
fn reserved_words_flagged() {
    for w in ["if", "in", "do", "for", "var", "new", "let", "class", "arguments", "eval"] {
        assert!(is_reserved(w), "{w} 应保留");
    }
    assert!(!is_reserved("a"));
    assert!(!is_reserved("foo"));
    assert!(!is_reserved("bar"));
}

#[test]
fn module_level_non_exported_renamed() {
    let (m, it) = plan("const x = 1; function foo(){ return x; } foo;");
    assert_eq!(m.renamed_symbols, 1);
    assert_eq!(m.table().len(), 2);
    assert_eq!(new_names(&m, &it), ["a".to_string()].into_iter().collect());
}

#[test]
fn imports_not_renamed() {
    let (m, _) = plan("import def, { named } from 'mod'; def; named;");
    assert_eq!(m.renamed_symbols, 0);
}

#[test]
fn function_params_and_locals_renamed() {
    let (m, it) = plan("function f(x){ return x + 1; } f;");
    assert_eq!(m.renamed_symbols, 1);
    assert_eq!(m.table().len(), 2);
    assert_eq!(new_names(&m, &it), ["a".to_string()].into_iter().collect());
}

#[test]
fn eval_bails_whole_module() {
    let (m, _) = plan("function f(){ var secret = 1; return eval('secret'); } f;");
    assert!(m.is_empty(), "含 eval 应放弃 mangling");
}

#[test]
fn new_name_avoids_global_reference() {
    let (m2, it2) = plan("function f(){ var p = GLOBAL_X; return p; } f;");
    assert_eq!(m2.renamed_symbols, 1);
    assert!(new_names(&m2, &it2).contains("a"));
}

// ── Module-level rename safety ──

#[test]
fn exported_const_not_renamed() {
    let (m, _) = plan("export const x = 1;");
    assert_eq!(m.renamed_symbols, 0);
}

#[test]
fn exported_via_specifier_not_renamed() {
    let (m, _) = plan("const x = 1; export { x };");
    assert_eq!(m.renamed_symbols, 0);
}

#[test]
fn export_function_not_renamed() {
    let (m, _) = plan("export function foo(){}");
    assert_eq!(m.renamed_symbols, 0);
}

#[test]
fn mixed_exported_and_non_exported() {
    let (m, it) = plan("const x = 1; export const y = 2; console.log(x, y);");
    assert_eq!(m.renamed_symbols, 1);
    assert_eq!(new_names(&m, &it), ["a".to_string()].into_iter().collect());
}

#[test]
fn reexport_import_not_renamed() {
    let (m, _) = plan("import { x } from 'mod'; export { x };");
    assert_eq!(m.renamed_symbols, 0);
}

#[test]
fn module_level_multi_non_exported_renamed() {
    let (m, it) = plan("const x = 1; const y = 2; console.log(x + y);");
    // Both x and y are non-exported module-level → both renamed
    assert_eq!(m.renamed_symbols, 2);
    assert_eq!(new_names(&m, &it), ["a", "b"].into_iter().map(str::to_string).collect());
}
