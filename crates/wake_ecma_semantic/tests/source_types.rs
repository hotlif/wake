use wake_common::{Interner, Span};
use wake_ecma_parser::{ParseOptions, SourceType, parse_source};
use wake_ecma_semantic::{SourceTypeInput, TypeResolution, analyze_source_types};

#[test]
fn global_augmentation_symbols_are_separate_from_module_local_imports() {
    let source = "import { Shared } from 'pkg'; declare global { interface Shared {} interface Global { field: Shared } } let local: Shared; let global: Global;";
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    let model = analyze_source_types(
        &interner,
        SourceTypeInput {
            identifiers: &parsed.identifiers,
            syntax: &parsed.syntax,
            scopes: &parsed.type_scopes,
            declarations: &parsed.type_declarations,
            imports: &parsed.imports,
            exports: &parsed.exports,
            namespaces: &parsed.namespaces,
        },
    );
    let shared: Vec<_> = model
        .symbols
        .iter()
        .filter(|s| s.name == interner.intern("Shared"))
        .collect();
    assert_eq!(
        shared.len(),
        2,
        "module and global declarations must not merge"
    );
    let occurrence =
        |text: &str, name: &str| (source.find(text).unwrap() + text.rfind(name).unwrap()) as u32;
    let import = shared
        .iter()
        .find(|s| {
            s.declarations
                .iter()
                .any(|d| d.span.lo == occurrence("{ Shared }", "Shared"))
        })
        .unwrap()
        .id;
    let global = shared.iter().find(|s| s.id != import).unwrap().id;
    assert_eq!(
        model
            .references
            .iter()
            .find(|r| r.span.lo == occurrence("local: Shared", "Shared"))
            .unwrap()
            .resolution,
        TypeResolution::Resolved(import)
    );
    assert_eq!(
        model
            .references
            .iter()
            .find(|r| r.span.lo == occurrence("field: Shared", "Shared"))
            .unwrap()
            .resolution,
        TypeResolution::Resolved(global)
    );
    assert!(matches!(
        model
            .references
            .iter()
            .find(|r| r.span.lo == occurrence("global: Global", "Global"))
            .unwrap()
            .resolution,
        TypeResolution::Resolved(_)
    ));
}

#[test]
fn repeated_infer_names_share_identity_and_preserve_every_declaration() {
    let source = "type Test<T> = T extends [infer U, infer U extends U] ? U : never;";
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    let model = analyze_source_types(
        &interner,
        SourceTypeInput {
            identifiers: &parsed.identifiers,
            syntax: &parsed.syntax,
            scopes: &parsed.type_scopes,
            declarations: &parsed.type_declarations,
            imports: &parsed.imports,
            exports: &parsed.exports,
            namespaces: &parsed.namespaces,
        },
    );
    let symbols: Vec<_> = model
        .symbols
        .iter()
        .filter(|symbol| symbol.name == interner.intern("U"))
        .collect();
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].declarations.len(), 2);
    assert!(
        model
            .references
            .iter()
            .filter(|r| r.name == interner.intern("U"))
            .all(|r| r.resolution == TypeResolution::Resolved(symbols[0].id))
    );
}

#[test]
fn original_types_bind_by_local_identity_across_erasure_and_shadowing() {
    let source = r#"import { Imported } from 'pkg';
    interface Merged { first: Imported } interface Merged { second: Imported }
    type Outer<T extends U, U = T> = { value: T; mapped: { [K in keyof K as K]: K };
        inferred: T extends [infer X extends X, X] ? X : X };
    { type Imported = Merged; let inside: Imported; }
    let outside: Imported;
    function f<T>(arg: T): T { type Body = T; let body: Body; }
    let absent: Body;
    class Box<T> { item: T; method<M>(m: M): Box<T> { return this } }
    const expression = class Inner { item: Inner };
    let noInner: Inner;
    enum E { A } let enumeration: E;
    const runtime = Imported; type Query = typeof runtime;"#;
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    let model = analyze_source_types(
        &interner,
        SourceTypeInput {
            identifiers: &parsed.identifiers,
            syntax: &parsed.syntax,
            scopes: &parsed.type_scopes,
            declarations: &parsed.type_declarations,
            imports: &parsed.imports,
            exports: &parsed.exports,
            namespaces: &parsed.namespaces,
        },
    );
    let range = |text: &str, name: &str| {
        let lo = source.find(text).unwrap() + text.rfind(name).unwrap();
        Span::new(lo as u32, (lo + name.len()) as u32)
    };
    let reference = |text: &str, name: &str| {
        model
            .references
            .iter()
            .find(|r| r.span == range(text, name))
            .unwrap_or_else(|| panic!("missing type reference: {text} / {name}"))
            .resolution
    };
    let symbol = |text: &str, name: &str| {
        model
            .symbols
            .iter()
            .find(|s| s.declarations.iter().any(|d| d.span == range(text, name)))
            .unwrap()
            .id
    };
    assert_eq!(
        reference("outside: Imported", "Imported"),
        TypeResolution::Resolved(symbol("{ Imported } from", "Imported"))
    );
    assert_eq!(
        reference("inside: Imported", "Imported"),
        TypeResolution::Resolved(symbol("type Imported =", "Imported"))
    );
    assert_eq!(
        reference("value: T", "T"),
        TypeResolution::Resolved(symbol("<T extends U", "T"))
    );
    assert_eq!(reference("keyof K", "K"), TypeResolution::Unresolved);
    let mapped = symbol("[K in", "K");
    assert_eq!(reference("as K", "K"), TypeResolution::Resolved(mapped));
    assert_eq!(reference("]: K", "K"), TypeResolution::Resolved(mapped));
    let inferred = symbol("infer X", "X");
    assert_eq!(
        reference("extends X,", "X"),
        TypeResolution::Resolved(inferred)
    );
    assert_eq!(reference(", X]", "X"), TypeResolution::Unresolved);
    assert_eq!(reference("? X", "X"), TypeResolution::Resolved(inferred));
    assert_eq!(reference(": X }", "X"), TypeResolution::Unresolved);
    assert_eq!(
        reference("body: Body", "Body"),
        TypeResolution::Resolved(symbol("type Body", "Body"))
    );
    assert_eq!(
        reference("absent: Body", "Body"),
        TypeResolution::Unresolved
    );
    assert_eq!(
        reference("item: Inner", "Inner"),
        TypeResolution::Resolved(symbol("class Inner", "Inner"))
    );
    assert_eq!(
        reference("noInner: Inner", "Inner"),
        TypeResolution::Unresolved
    );
    assert_eq!(
        reference("enumeration: E", "E"),
        TypeResolution::Resolved(symbol("enum E", "E"))
    );
    let merged = &model.symbols[symbol("interface Merged", "Merged").0];
    assert_eq!(merged.declarations.len(), 2);
    assert!(
        !model
            .references
            .iter()
            .any(|r| r.span == range("typeof runtime", "runtime"))
    );
    assert!(
        !model
            .references
            .iter()
            .any(|r| r.span == range("runtime = Imported", "Imported"))
    );
}

#[test]
fn namespace_exports_merge_without_leaking_private_members_or_dotted_names() {
    let source = r#"namespace N { export interface Shared {} interface Private {} }
    namespace N { let shared: Shared; let private: Private; }
    namespace N.Inner { export type Nested = Shared; }
    namespace N.Inner { let nested: Nested; }
    let outside: Shared; let noInner: Inner; let qualified: N.Inner.Nested;
    declare module 'ambient' { import type { Remote } from 'remote'; export type A = Remote; }
    let noRemote: Remote;"#;
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    let model = analyze_source_types(
        &interner,
        SourceTypeInput {
            identifiers: &parsed.identifiers,
            syntax: &parsed.syntax,
            scopes: &parsed.type_scopes,
            declarations: &parsed.type_declarations,
            imports: &parsed.imports,
            exports: &parsed.exports,
            namespaces: &parsed.namespaces,
        },
    );
    let resolution = |text: &str, name: &str| {
        let lo = (source.find(text).unwrap() + text.rfind(name).unwrap()) as u32;
        model
            .references
            .iter()
            .find(|r| r.span.lo == lo)
            .unwrap_or_else(|| panic!("missing type reference: {text} / {name}"))
            .resolution
    };
    assert!(matches!(
        resolution("shared: Shared", "Shared"),
        TypeResolution::Resolved(_)
    ));
    assert!(matches!(
        resolution("Nested = Shared", "Shared"),
        TypeResolution::Resolved(_)
    ));
    assert!(matches!(
        resolution("nested: Nested", "Nested"),
        TypeResolution::Resolved(_)
    ));
    assert!(matches!(
        resolution("A = Remote", "Remote"),
        TypeResolution::Resolved(_)
    ));
    for (text, name) in [
        ("private: Private", "Private"),
        ("outside: Shared", "Shared"),
        ("noInner: Inner", "Inner"),
        ("noRemote: Remote", "Remote"),
    ] {
        assert_eq!(resolution(text, name), TypeResolution::Unresolved);
    }
}

#[test]
fn repeated_ambient_module_declarations_share_type_members() {
    let source = r#"declare module 'ambient' { interface First {} }
    declare module 'ambient' { interface Second { field: First } type Use = First; }"#;
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    let model = analyze_source_types(
        &interner,
        SourceTypeInput {
            identifiers: &parsed.identifiers,
            syntax: &parsed.syntax,
            scopes: &parsed.type_scopes,
            declarations: &parsed.type_declarations,
            imports: &parsed.imports,
            exports: &parsed.exports,
            namespaces: &parsed.namespaces,
        },
    );
    let first = model
        .symbols
        .iter()
        .find(|symbol| {
            symbol.name == interner.intern("First")
                && symbol.declarations.iter().any(|declaration| {
                    declaration.span.lo as usize
                        == source.find("interface First").unwrap() + "interface ".len()
                })
        })
        .expect("first ambient member")
        .id;
    let first_references: Vec<_> = model
        .references
        .iter()
        .filter(|reference| reference.name == interner.intern("First"))
        .collect();
    assert_eq!(first_references.len(), 2);
    assert!(
        first_references
            .iter()
            .all(|reference| reference.resolution == TypeResolution::Resolved(first))
    );
}
