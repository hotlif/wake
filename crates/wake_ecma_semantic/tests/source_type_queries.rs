use wake_common::{Interner, Span};
use wake_ecma_parser::{ParseOptions, SourceType, parse_source};
use wake_ecma_semantic::{
    SourceSemanticInput, SourceTypeQueryResolution as Resolution, analyze, analyze_source,
};

#[test]
fn ambient_module_code_units_control_value_and_type_declaration_merging() {
    let source = r#"
declare module '\ud800' { const shared: number; interface Shape {} type A=typeof shared; type B=Shape; }
declare module '\u{d800}' { type A2=typeof shared; type B2=Shape; }
declare module '\ud801' { const shared: number; interface Shape {} type C=typeof shared; type D=Shape; }
declare module '\ufffd' { const shared: number; interface Shape {} type E=typeof shared; type F=Shape; }
"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let values: Vec<_> = facts
        .type_queries
        .iter()
        .map(|query| match query.resolution {
            Resolution::Resolved(id) => id,
            other => panic!("ambient value not resolved: {other:?}"),
        })
        .collect();
    assert_eq!(values.len(), 4);
    assert_eq!(values[0], values[1]);
    assert_ne!(values[0], values[2]);
    assert_ne!(values[0], values[3]);
    assert_ne!(values[2], values[3]);
    let types = wake_ecma_semantic::analyze_source_types(
        &interner,
        wake_ecma_semantic::SourceTypeInput {
            identifiers: &parsed.identifiers,
            syntax: &parsed.syntax,
            scopes: &parsed.type_scopes,
            declarations: &parsed.type_declarations,
            imports: &parsed.imports,
            exports: &parsed.exports,
            namespaces: &parsed.namespaces,
        },
    );
    let shapes: Vec<_> = types
        .references
        .iter()
        .filter(|reference| reference.name == interner.intern("Shape"))
        .map(|reference| match reference.resolution {
            wake_ecma_semantic::TypeResolution::Resolved(id) => id,
            other => panic!("ambient type not resolved: {other:?}"),
        })
        .collect();
    assert_eq!(shapes.len(), 4);
    assert_eq!(shapes[0], shapes[1]);
    assert_ne!(shapes[0], shapes[2]);
    assert_ne!(shapes[0], shapes[3]);
    assert_ne!(shapes[2], shapes[3]);
}

#[test]
fn external_global_ambient_projection_resolves_reads_and_type_queries() {
    let source = "type Query = typeof shared; const copy = shared;";
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    let mut facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let shared = interner.intern("shared");
    assert!(
        facts
            .type_queries
            .iter()
            .any(|query| query.name == shared && query.resolution == Resolution::Unresolved)
    );
    assert!(
        facts
            .model
            .references
            .iter()
            .any(|reference| reference.name == shared && reference.resolved.is_none())
    );
    facts.project_external_ambient_values(&["shared".into()], &interner);
    let symbol = facts
        .type_queries
        .iter()
        .find_map(|query| (query.name == shared).then_some(query.resolution))
        .and_then(|resolution| match resolution {
            Resolution::Resolved(symbol) => Some(symbol),
            _ => None,
        })
        .expect("external ambient type query should resolve");
    assert!(
        facts
            .model
            .references
            .iter()
            .any(|reference| reference.name == shared && reference.resolved == Some(symbol))
    );
    assert!(facts.ambient_value_symbols.contains(&symbol));
}

#[test]
fn requested_async_feature_does_not_invalidate_an_unlowered_function() {
    let interner = Interner::new();
    let mut options = ParseOptions::default();
    options
        .transform_features
        .insert(wake_ecma_transform::EcmaFeature::AsyncAwait);
    let parsed = parse_source(
        "async function outer(){let local=1;type A=typeof local;await wait;return local}",
        &interner,
        SourceType::TypeScript,
        options,
    );
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    parsed.parsed.module.with_ast(|program| assert!(matches!(program.body[0], wake_ecma_ast::Statement::FunctionDeclaration(function) if function.is_async)));
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    assert_eq!(facts.type_queries.len(), 1);
    let Resolution::Resolved(symbol) = facts.type_queries[0].resolution else {
        panic!("unlowered local must resolve")
    };
    assert_eq!(
        facts.model.symbols[symbol as usize].decl_kind,
        wake_ecma_semantic::DeclKind::Let
    );
}

#[test]
fn lowered_arrows_cannot_supply_a_synthetic_arguments_binding_to_source_type_queries() {
    let source =
        "function outer(){const arrow=()=>{type A=typeof arguments;return 1};return arrow}";
    for lowered in [false, true] {
        let interner = Interner::new();
        let mut options = ParseOptions::default();
        if lowered {
            options
                .transform_features
                .insert(wake_ecma_transform::EcmaFeature::ArrowFunction);
        }
        let parsed = parse_source(source, &interner, SourceType::TypeScript, options);
        assert!(
            !parsed.parsed.has_errors(),
            "{:?}",
            parsed.parsed.diagnostics
        );
        let facts = parsed.parsed.module.with_ast(|program| {
            analyze_source(
                program,
                &interner,
                SourceSemanticInput {
                    identifiers: &parsed.identifiers,
                    exports: &parsed.exports,
                    syntax: &parsed.syntax,
                    functions: &parsed.functions,
                    namespaces: &parsed.namespaces,
                },
            )
        });
        assert_eq!(facts.type_queries.len(), 1);
        if lowered {
            assert_eq!(facts.type_queries[0].resolution, Resolution::Unavailable);
        } else {
            assert!(matches!(
                facts.type_queries[0].resolution,
                Resolution::Resolved(_)
            ));
        }
    }
}

#[test]
fn enum_member_queries_use_the_enum_scope_without_capturing_outer_values() {
    let source = "const Member = 9; const Outer = 8; enum Enum { Member = 0, Other = (1 as typeof Member), Third = (1 as typeof Outer) } type Outside = typeof Enum;";
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    assert_eq!(facts.type_queries.len(), 3);
    let Resolution::Resolved(member) = facts.type_queries[0].resolution else {
        panic!("enum member initializer must resolve its enum member binding")
    };
    let member_start = source.find("enum Enum { Member").unwrap() + "enum Enum { ".len();
    assert_eq!(
        facts.model.symbols[member as usize].span.lo,
        member_start as u32
    );
    let Resolution::Resolved(outer) = facts.type_queries[1].resolution else {
        panic!("enum initializers must retain enclosing value bindings")
    };
    let outer_start = source.find("const Outer").unwrap() + "const ".len();
    assert_eq!(
        facts.model.symbols[outer as usize].span.lo,
        outer_start as u32
    );
    assert!(matches!(
        facts.type_queries[2].resolution,
        Resolution::Resolved(_)
    ));
}

#[test]
fn enum_member_references_use_the_enum_scope_for_forward_use_before_define() {
    let source = "enum State { A = B, B = 1 }";
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let member_span = Span::new(
        source.rfind("B = 1").unwrap() as u32,
        source.rfind("B = 1").unwrap() as u32 + 1,
    );
    let member = facts
        .model
        .binding_occurrences
        .iter()
        .find(|binding| binding.span == member_span)
        .expect("enum member binding");
    let reference_start = source.find("A = B").unwrap() as u32 + "A = ".len() as u32;
    let reference = facts
        .model
        .references
        .iter()
        .find(|reference| reference.span == Span::new(reference_start, reference_start + 1))
        .expect("forward enum member reference");
    assert_eq!(reference.resolved, Some(member.symbol));
    assert_eq!(reference.scope, member.scope);
}

#[test]
fn type_queries_resolve_original_value_environments_without_evaluated_reads() {
    let source = r#"const value = 0;
type Root = typeof value.member;
{ const value = 1; type Block = typeof value; }
function fn(value) { type Body = typeof value; }
const obj = { [key as typeof value](value: typeof value): typeof value { type Method = typeof value; } };
switch (key as typeof value) { case key as typeof value: let value = 2; type Case = typeof value; }
for (let value of input as typeof value) { type Loop = typeof value; }
try {} catch (value) { type Catch = typeof value; } finally { const value = 3; type Finally = typeof value; }
const C = class Named { static { const value = 4; type Static = typeof value; } method() { type Self = typeof Named; type Args = typeof arguments; } };
type Missing = typeof missing.member;
"#;
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
    let (ordinary, facts) = parsed.parsed.module.with_ast(|program| {
        (
            analyze(program, &interner),
            analyze_source(
                program,
                &interner,
                SourceSemanticInput {
                    identifiers: &parsed.identifiers,
                    exports: &parsed.exports,
                    syntax: &parsed.syntax,
                    functions: &parsed.functions,
                    namespaces: &parsed.namespaces,
                },
            ),
        )
    });
    assert_eq!(format!("{:?}", ordinary), format!("{:?}", facts.model));
    assert_eq!(facts.type_queries.len(), 18);
    let check = |query: &str, declaration: &str, offset: usize| {
        let lo = source.find(query).unwrap() + query.rfind("typeof ").unwrap() + 7;
        let usage = facts
            .type_queries
            .iter()
            .find(|usage| usage.span.lo as usize == lo)
            .unwrap();
        let Resolution::Resolved(symbol) = usage.resolution else {
            panic!("{query}: {usage:?}")
        };
        assert_eq!(
            facts.model.symbols[symbol as usize].span.lo as usize,
            source.find(declaration).unwrap() + offset,
            "{query}"
        );
    };
    check("Root = typeof value", "const value = 0", 6);
    check("Block = typeof value", "const value = 1", 6);
    check("Body = typeof value", "fn(value)", 3);
    check("[key as typeof value", "const value = 0", 6);
    check("(value: typeof value", "(value: typeof value", 1);
    check("): typeof value", "(value: typeof value", 1);
    check("Method = typeof value", "(value: typeof value", 1);
    check("switch (key as typeof value", "const value = 0", 6);
    check("case key as typeof value", "let value = 2", 4);
    check("Case = typeof value", "let value = 2", 4);
    check("input as typeof value", "let value of", 4);
    check("Loop = typeof value", "let value of", 4);
    check("Catch = typeof value", "catch (value)", 7);
    check("Finally = typeof value", "const value = 3", 6);
    check("Static = typeof value", "const value = 4", 6);
    check("Self = typeof Named", "class Named", 6);
    // An implicit arguments binding has no source declaration; its query remains unevaluated.
    let args = facts
        .type_queries
        .iter()
        .find(|q| interner.resolve(q.name) == "arguments")
        .unwrap();
    assert!(matches!(args.resolution, Resolution::Resolved(_)));
    let missing = facts.type_queries.last().unwrap();
    assert_eq!(missing.resolution, Resolution::Unresolved);
    assert!(
        !facts
            .model
            .references
            .iter()
            .any(|r| facts.type_queries.iter().any(|q| q.span == r.span))
    );
}

#[test]
fn incomplete_erased_environments_never_silently_capture_an_outer_binding() {
    let source = r#"const erased = 0;
{ declare const erased: number; type Erased = typeof erased; }
type AlsoConservative = typeof erased;
namespace Box { const inner = 1; type Namespace = typeof inner; }
declare module 'ambient' { const ambient: number; type Ambient = typeof ambient; }
type Signature = (parameter: string) => typeof parameter;
function overload(overloaded: unknown): typeof overloaded;
function defaults(p = 0 as typeof local): typeof local { var local = 1; type Body = typeof local; return local; }
"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    assert_eq!(facts.type_queries.len(), 9);
    let query = |name: &str| {
        facts
            .type_queries
            .iter()
            .find(|query| interner.resolve(query.name) == name)
            .unwrap_or_else(|| panic!("missing query for {name}"))
    };
    let erased_queries: Vec<_> = facts
        .type_queries
        .iter()
        .filter(|query| interner.resolve(query.name) == "erased")
        .collect();
    assert_eq!(erased_queries.len(), 2);
    assert!(matches!(
        erased_queries[0].resolution,
        Resolution::Resolved(_)
    ));
    let Resolution::Resolved(outer_erased_symbol) = erased_queries[1].resolution else {
        panic!("outer erased query should resolve to the module declaration");
    };
    assert_eq!(
        facts.model.symbols[outer_erased_symbol as usize].span.lo as usize,
        source.find("const erased = 0").unwrap() + "const ".len()
    );
    let Resolution::Resolved(parameter_symbol) = query("parameter").resolution else {
        panic!(
            "pure function type signature parameter should resolve: {:?}",
            query("parameter")
        );
    };
    assert_ne!(
        facts.model.symbols[parameter_symbol as usize].span.lo as usize,
        source.find("const erased = 0").unwrap() + "const ".len()
    );
    assert!(matches!(
        query("overloaded").resolution,
        Resolution::Resolved(_)
    ));
    assert!(matches!(
        query("ambient").resolution,
        Resolution::Resolved(_)
    ));
    assert!(
        facts
            .type_queries
            .iter()
            .filter(|query| interner.resolve(query.name) == "local")
            .any(|query| matches!(query.resolution, Resolution::Resolved(_)))
    );
}

#[test]
fn incomplete_ambient_names_do_not_mask_a_resolved_value_in_another_scope() {
    let source = r#"declare module 'ambient' {
        const value: number;
        type Ambient = typeof value;
    }
    const value = 1;
    { declare const value: number; type Block = typeof value; }
    type Local = typeof value;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let ambient = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == source.find("typeof value").unwrap() + 7)
        .expect("ambient query");
    assert!(
        matches!(ambient.resolution, Resolution::Resolved(_)),
        "ambient local value must resolve in its own scope: {ambient:?}"
    );
    let block_start = source.find("type Block = typeof value").unwrap();
    let block_end = block_start + "type Block = typeof value".len();
    let block = facts
        .type_queries
        .iter()
        .find(|query| {
            let offset = query.span.lo as usize;
            block_start < offset && offset < block_end
        })
        .expect("block query");
    assert!(
        matches!(block.resolution, Resolution::Resolved(_)),
        "{block:?}"
    );
    let local_offset = source.rfind("typeof value").unwrap() + 7;
    let local = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == local_offset)
        .expect("local query");
    assert!(
        matches!(local.resolution, Resolution::Resolved(_)),
        "{local:?}"
    );
}

#[test]
fn string_ambient_module_local_values_resolve_without_capturing_outer_names() {
    let source = r#"const value = 1;
    declare module 'ambient' {
        const value: number;
        type Local = typeof value;
        type Missing = typeof outside;
    }
    declare module 'ambient' {
        const merged: string;
        type Merged = typeof value;
    }
    type Outer = typeof value;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let local_offset =
        source.find("type Local = typeof value").unwrap() + "type Local = typeof ".len();
    let local = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == local_offset)
        .expect("ambient local query");
    assert!(
        matches!(local.resolution, Resolution::Resolved(_)),
        "ambient local query must resolve: {local:?}"
    );
    let missing_offset =
        source.find("type Missing = typeof outside").unwrap() + "type Missing = typeof ".len();
    let missing = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == missing_offset)
        .expect("ambient external query");
    assert_eq!(
        missing.resolution,
        Resolution::Unavailable,
        "ambient external query must remain unavailable: {missing:?}"
    );
    let merged_offset =
        source.find("type Merged = typeof value").unwrap() + "type Merged = typeof ".len();
    let merged = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == merged_offset)
        .expect("merged ambient query");
    assert!(
        matches!(merged.resolution, Resolution::Resolved(_)),
        "same-file ambient declarations must share a scope: {merged:?}"
    );
    let outer_offset =
        source.rfind("type Outer = typeof value").unwrap() + "type Outer = typeof ".len();
    let outer = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outer_offset)
        .expect("outer query");
    assert!(
        matches!(outer.resolution, Resolution::Resolved(_)),
        "outer value query should remain resolved: {outer:?}"
    );
}

#[test]
fn erased_scopes_inside_ambient_modules_keep_the_module_parent() {
    let source = r#"const outer = 0;
    declare module 'ambient' {
        const outer: number;
        { declare const inner: number; type Inner = typeof inner | typeof outer; }
        function read() { type Function = typeof outer; }
    }"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let outer_offset = source.find("typeof outer").unwrap() + "typeof ".len();
    let outer = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outer_offset)
        .expect("nested ambient outer query");
    let Resolution::Resolved(ambient_outer) = outer.resolution else {
        panic!("nested ambient query should resolve its module value: {outer:?}");
    };
    assert_eq!(
        facts.model.symbols[ambient_outer as usize].span.lo as usize,
        source.find("const outer: number").unwrap() + "const ".len()
    );
    let function_offset = source.rfind("typeof outer").unwrap() + "typeof ".len();
    let function_query = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == function_offset)
        .expect("nested ambient function query");
    assert_eq!(function_query.resolution, outer.resolution);
}

#[test]
fn erased_declaration_signature_parameters_resolve_only_inside_the_signature() {
    let source = r#"const parameter = 0;
    const outer = 1;
    declare function overload(parameter: string): typeof parameter | typeof outer;
    type Outside = typeof parameter;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let signature_offset = source.find("typeof parameter").unwrap() + "typeof ".len();
    let signature = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == signature_offset)
        .expect("signature query");
    let Resolution::Resolved(signature_symbol) = signature.resolution else {
        panic!("signature parameter should resolve in its own scope: {signature:?}");
    };
    assert_eq!(
        facts.model.symbols[signature_symbol as usize].span.lo as usize,
        source.find("overload(parameter").unwrap() + "overload(".len()
    );
    let outer_signature_offset = source.find("typeof outer").unwrap() + "typeof ".len();
    let outer_signature = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outer_signature_offset)
        .expect("outer signature query");
    let Resolution::Resolved(outer_signature_symbol) = outer_signature.resolution else {
        panic!("signature should see its enclosing module scope: {outer_signature:?}");
    };
    assert_eq!(
        facts.model.symbols[outer_signature_symbol as usize].span.lo as usize,
        source.find("const outer").unwrap() + "const ".len()
    );
    let outside_offset = source.rfind("typeof parameter").unwrap() + "typeof ".len();
    let outside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outside_offset)
        .expect("outer query");
    let Resolution::Resolved(outer_symbol) = outside.resolution else {
        panic!("outer query should resolve to the module binding: {outside:?}");
    };
    assert_eq!(
        facts.model.symbols[outer_symbol as usize].span.lo as usize,
        source.find("const parameter").unwrap() + "const ".len()
    );
    assert_ne!(signature_symbol, outer_symbol);
}

#[test]
fn pure_type_signature_parameters_resolve_without_leaking() {
    let source = r#"const parameter = 0;
    const outer = 1;
    type Signature = (parameter: string) => typeof parameter | typeof outer;
    type Outside = typeof parameter;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let signature_offset = source.find("typeof parameter").unwrap() + "typeof ".len();
    let signature = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == signature_offset)
        .expect("pure signature query");
    let Resolution::Resolved(signature_symbol) = signature.resolution else {
        panic!("pure signature parameter should resolve in its own scope: {signature:?}");
    };
    assert_eq!(
        facts.model.symbols[signature_symbol as usize].span.lo as usize,
        source.find("Signature = (parameter").unwrap() + "Signature = (".len()
    );
    let outer_signature_offset = source.find("typeof outer").unwrap() + "typeof ".len();
    let outer_signature = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outer_signature_offset)
        .expect("outer pure signature query");
    let Resolution::Resolved(outer_symbol) = outer_signature.resolution else {
        panic!("pure signature should see its enclosing module scope: {outer_signature:?}");
    };
    assert_eq!(
        facts.model.symbols[outer_symbol as usize].span.lo as usize,
        source.find("const outer").unwrap() + "const ".len()
    );
    let outside_offset = source.rfind("typeof parameter").unwrap() + "typeof ".len();
    let outside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outside_offset)
        .expect("outer pure signature query");
    let Resolution::Resolved(module_symbol) = outside.resolution else {
        panic!("outer query should resolve to the module binding: {outside:?}");
    };
    assert_eq!(
        facts.model.symbols[module_symbol as usize].span.lo as usize,
        source.find("const parameter").unwrap() + "const ".len()
    );
    assert_ne!(signature_symbol, module_symbol);
}

#[test]
fn pure_type_signatures_without_parameters_see_enclosing_values() {
    let source = r#"const outer = 0;
type Signature = () => typeof outer;
type Missing = () => typeof missing;
"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let outer_offset = source.find("typeof outer").unwrap() + "typeof ".len();
    let outer = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outer_offset)
        .expect("outer query");
    let Resolution::Resolved(symbol) = outer.resolution else {
        panic!("pure signature should resolve its enclosing value: {outer:?}");
    };
    assert_eq!(
        facts.model.symbols[symbol as usize].span.lo as usize,
        source.find("outer =").unwrap()
    );
    let missing_offset = source.find("typeof missing").unwrap() + "typeof ".len();
    let missing = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == missing_offset)
        .expect("missing query");
    assert_eq!(missing.resolution, Resolution::Unavailable);
}

#[test]
fn pure_method_signature_parameters_resolve_without_leaking() {
    let source = r#"const parameter = 0;
    interface Methods { method(parameter: string): typeof parameter; }
    type Outside = typeof parameter;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let method_offset = source.find("typeof parameter").unwrap() + "typeof ".len();
    let method = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == method_offset)
        .expect("method signature query");
    let Resolution::Resolved(method_symbol) = method.resolution else {
        panic!("method signature parameter should resolve: {method:?}");
    };
    assert_eq!(
        facts.model.symbols[method_symbol as usize].span.lo as usize,
        source.find("method(parameter").unwrap() + "method(".len()
    );
    let outside_offset = source.rfind("typeof parameter").unwrap() + "typeof ".len();
    let outside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outside_offset)
        .expect("outside method query");
    let Resolution::Resolved(module_symbol) = outside.resolution else {
        panic!("outside method query should resolve to the module binding: {outside:?}");
    };
    assert_ne!(method_symbol, module_symbol);
}

#[test]
fn pure_type_signature_keeps_an_enclosing_function_value_scope() {
    let source = r#"function outer(outer: string) {
        type Signature = (parameter: string) => typeof parameter | typeof outer;
    }"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let outer_offset = source.find("typeof outer").unwrap() + "typeof ".len();
    let outer = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outer_offset)
        .expect("enclosing function query");
    let Resolution::Resolved(outer_symbol) = outer.resolution else {
        panic!("pure signature should see the enclosing function parameter: {outer:?}");
    };
    assert_eq!(
        facts.model.symbols[outer_symbol as usize].span.lo as usize,
        source.find("outer: string").unwrap()
    );
}

#[test]
fn erased_class_member_signatures_keep_outer_values_and_isolate_parameters() {
    let source = r#"
        declare const outer: number;
        declare class Box {
            value: typeof outer;
            method(parameter: string): typeof parameter;
        }
        type Outside = typeof outer;
        type Missing = typeof parameter;
        type Member = typeof value;
    "#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let outer_queries: Vec<_> = facts
        .type_queries
        .iter()
        .filter(|query| interner.resolve(query.name) == "outer")
        .collect();
    assert_eq!(outer_queries.len(), 2);
    assert!(
        outer_queries
            .iter()
            .all(|query| matches!(query.resolution, Resolution::Resolved(_)))
    );
    let parameter_queries: Vec<_> = facts
        .type_queries
        .iter()
        .filter(|query| interner.resolve(query.name) == "parameter")
        .collect();
    assert_eq!(parameter_queries.len(), 2);
    let inside = parameter_queries
        .iter()
        .find(|query| query.span.lo < source.find("type Missing").unwrap() as u32)
        .expect("method signature parameter query");
    assert!(matches!(inside.resolution, Resolution::Resolved(_)));
    let outside = parameter_queries
        .iter()
        .find(|query| query.span.lo > source.find("type Missing").unwrap() as u32)
        .expect("outer parameter query");
    assert_eq!(outside.resolution, Resolution::Unresolved);
    let member = facts
        .type_queries
        .iter()
        .find(|query| interner.resolve(query.name) == "value")
        .expect("class member query");
    assert_eq!(member.resolution, Resolution::Unresolved);
}

#[test]
fn pure_type_signature_inside_ambient_namespace_keeps_namespace_values() {
    let source = r#"const outer = 0;
    declare namespace Box {
        const outer: number;
        type Signature = (parameter: string) => typeof parameter | typeof outer;
    }"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let namespace_offset = source.find("typeof outer").unwrap() + "typeof ".len();
    let namespace_query = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == namespace_offset)
        .expect("namespace signature query");
    let Resolution::Resolved(namespace_symbol) = namespace_query.resolution else {
        panic!("namespace signature should see its namespace value: {namespace_query:?}");
    };
    assert_eq!(
        facts.model.symbols[namespace_symbol as usize].span.lo as usize,
        source.find("const outer: number").unwrap() + "const ".len()
    );
}

#[test]
fn erased_function_body_declarations_use_the_existing_function_scope_without_leaking() {
    let source = r#"const value = 0;
    function read() { declare const value: number; type Inside = typeof value; const copy = value; }
    type Outside = typeof value;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let inside_offset =
        source.find("type Inside = typeof value").unwrap() + "type Inside = typeof ".len();
    let inside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == inside_offset)
        .expect("function body query");
    let Resolution::Resolved(inner_symbol) = inside.resolution else {
        panic!("function body declaration should resolve in its function scope: {inside:?}");
    };
    assert_eq!(
        facts.model.symbols[inner_symbol as usize].span.lo as usize,
        source.find("declare const value").unwrap() + "declare const ".len()
    );
    let reference = facts
        .model
        .references
        .iter()
        .find(|reference| {
            reference.name == interner.intern("value")
                && reference.span.lo > source.find("declare const value").unwrap() as u32
        })
        .expect("function body runtime reference");
    assert_eq!(reference.resolved, Some(inner_symbol));
    let outside_offset = source.rfind("typeof value").unwrap() + "typeof ".len();
    let outside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outside_offset)
        .expect("outside function query");
    let Resolution::Resolved(outer_symbol) = outside.resolution else {
        panic!("outside query should resolve to the module binding: {outside:?}");
    };
    assert_ne!(inner_symbol, outer_symbol);
}

#[test]
fn separated_function_parameter_environment_resolves_source_type_queries() {
    let source = r#"const outer = 0;
function read(first: number, second = 0 as typeof first) {
    type Body = typeof first;
    return second;
}
"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let query_offset = source.find("typeof first").unwrap() + "typeof ".len();
    let query = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == query_offset)
        .expect("default parameter type query");
    let Resolution::Resolved(symbol) = query.resolution else {
        panic!("default parameter should resolve its earlier parameter: {query:?}");
    };
    assert_eq!(
        facts.model.symbols[symbol as usize].span.lo as usize,
        source.find("first: number").unwrap()
    );
}

#[test]
fn named_function_expression_queries_keep_the_function_name_outside_parameters() {
    let source = r#"const factory = function named(parameter = 0 as typeof named): typeof parameter {
    type Self = typeof named;
    return parameter;
};
type Outside = typeof named;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let query_at = |offset: usize| {
        facts
            .type_queries
            .iter()
            .find(|query| query.span.lo as usize == offset)
            .unwrap_or_else(|| panic!("missing query at {offset}"))
    };
    let named_offset = source.find("typeof named").unwrap() + "typeof ".len();
    let Resolution::Resolved(function_name) = query_at(named_offset).resolution else {
        panic!("default parameter must resolve the named function expression")
    };
    assert_eq!(
        facts.model.symbols[function_name as usize].span.lo as usize,
        source.find("named(parameter").unwrap()
    );
    let parameter_offset = source.find("typeof parameter").unwrap() + "typeof ".len();
    let Resolution::Resolved(parameter) = query_at(parameter_offset).resolution else {
        panic!("return type must resolve the function parameter")
    };
    assert_eq!(
        facts.model.symbols[parameter as usize].span.lo as usize,
        source.find("parameter =").unwrap()
    );
    let body_named_offset =
        source.find("type Self = typeof named").unwrap() + "type Self = typeof ".len();
    assert!(matches!(
        query_at(body_named_offset).resolution,
        Resolution::Resolved(symbol) if symbol == function_name
    ));
    let outside_offset = source.rfind("typeof named").unwrap() + "typeof ".len();
    assert_eq!(query_at(outside_offset).resolution, Resolution::Unresolved);
}

#[test]
fn erased_switch_declarations_use_the_existing_switch_scope_without_leaking() {
    let source = r#"const value = 0;
    switch (value) { case 0: declare const value: number; type Inside = typeof value; break; }
    type Outside = typeof value;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let inside_offset =
        source.find("type Inside = typeof value").unwrap() + "type Inside = typeof ".len();
    let inside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == inside_offset)
        .expect("switch query");
    let Resolution::Resolved(inner_symbol) = inside.resolution else {
        panic!("switch declaration should resolve in its switch scope: {inside:?}");
    };
    assert_eq!(
        facts.model.symbols[inner_symbol as usize].span.lo as usize,
        source.find("declare const value").unwrap() + "declare const ".len()
    );
    let outside_offset = source.rfind("typeof value").unwrap() + "typeof ".len();
    let outside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outside_offset)
        .expect("outside switch query");
    let Resolution::Resolved(outer_symbol) = outside.resolution else {
        panic!("outside query should resolve to the module binding: {outside:?}");
    };
    assert_ne!(inner_symbol, outer_symbol);
}

#[test]
fn erased_switch_declarations_inside_runtime_namespaces_do_not_leak_to_namespace_scope() {
    let source = r#"namespace Box { switch (tag) { case 0: declare const value: number; type Inside = typeof value; break; } type Outside = typeof value; }"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let inside_offset =
        source.find("type Inside = typeof value").unwrap() + "type Inside = typeof ".len();
    let inside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == inside_offset)
        .expect("namespace switch query");
    let Resolution::Resolved(_) = inside.resolution else {
        panic!("namespace switch declaration should resolve in its switch scope: {inside:?}");
    };
    let outside_offset = source.rfind("typeof value").unwrap() + "typeof ".len();
    let outside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outside_offset)
        .expect("namespace outside query");
    assert_eq!(outside.resolution, Resolution::Unresolved);
}

#[test]
fn catch_parameters_inside_runtime_namespaces_do_not_leak_to_namespace_scope() {
    let source = r#"namespace Box { try {} catch (error) { type Inside = typeof error; } type Outside = typeof error; }"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let inside_offset =
        source.find("type Inside = typeof error").unwrap() + "type Inside = typeof ".len();
    let inside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == inside_offset)
        .expect("namespace catch query");
    assert!(
        matches!(inside.resolution, Resolution::Resolved(_)),
        "catch parameter should resolve in the catch scope: {inside:?}"
    );
    let outside_offset = source.rfind("typeof error").unwrap() + "typeof ".len();
    let outside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outside_offset)
        .expect("namespace outside catch query");
    assert_eq!(outside.resolution, Resolution::Unresolved);
}

#[test]
fn for_bindings_inside_runtime_namespaces_do_not_leak_to_namespace_scope() {
    for source in [
        r#"namespace Box { for (let value = 0; value < 1; value++) { type Inside = typeof value; } type Outside = typeof value; }"#,
        r#"namespace Box { for (let value in values) { type Inside = typeof value; } type Outside = typeof value; }"#,
        r#"namespace Box { for (const value of values) { type Inside = typeof value; } type Outside = typeof value; }"#,
    ] {
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
        let facts = parsed.parsed.module.with_ast(|program| {
            analyze_source(
                program,
                &interner,
                SourceSemanticInput {
                    identifiers: &parsed.identifiers,
                    exports: &parsed.exports,
                    syntax: &parsed.syntax,
                    functions: &parsed.functions,
                    namespaces: &parsed.namespaces,
                },
            )
        });
        let inside_offset =
            source.find("type Inside = typeof value").unwrap() + "type Inside = typeof ".len();
        let inside = facts
            .type_queries
            .iter()
            .find(|query| query.span.lo as usize == inside_offset)
            .expect("namespace for query");
        assert!(
            matches!(inside.resolution, Resolution::Resolved(_)),
            "for binding should resolve in its loop scope: {inside:?}"
        );
        let outside_offset = source.rfind("typeof value").unwrap() + "typeof ".len();
        let outside = facts
            .type_queries
            .iter()
            .find(|query| query.span.lo as usize == outside_offset)
            .expect("namespace outside for query");
        assert_eq!(outside.resolution, Resolution::Unresolved);
    }
}

#[test]
fn using_bindings_follow_block_and_iteration_scope_without_leaking() {
    for (source, inside_marker, outside_marker) in [
        (
            r#"function useResource() { using resource = acquire(); type Inside = typeof resource; { type Nested = typeof resource; } } type Outside = typeof resource;"#,
            "type Inside = typeof resource",
            "type Outside = typeof resource",
        ),
        (
            r#"for (using resource of resources) { type Inside = typeof resource; } type Outside = typeof resource;"#,
            "type Inside = typeof resource",
            "type Outside = typeof resource",
        ),
        (
            r#"async function useResource() { await using resource = acquireAsync(); type Inside = typeof resource; } type Outside = typeof resource;"#,
            "type Inside = typeof resource",
            "type Outside = typeof resource",
        ),
    ] {
        let interner = Interner::new();
        let parsed = parse_source(
            source,
            &interner,
            SourceType::TypeScript,
            ParseOptions::default(),
        );
        assert!(
            !parsed.parsed.has_errors(),
            "{source}: {:?}",
            parsed.parsed.diagnostics
        );
        let facts = parsed.parsed.module.with_ast(|program| {
            analyze_source(
                program,
                &interner,
                SourceSemanticInput {
                    identifiers: &parsed.identifiers,
                    exports: &parsed.exports,
                    syntax: &parsed.syntax,
                    functions: &parsed.functions,
                    namespaces: &parsed.namespaces,
                },
            )
        });
        let inside_offset = source.find(inside_marker).unwrap() + "type Inside = typeof ".len();
        let inside = facts
            .type_queries
            .iter()
            .find(|query| query.span.lo as usize == inside_offset)
            .expect("using binding inside query");
        assert!(
            matches!(inside.resolution, Resolution::Resolved(_)),
            "resource binding should resolve in its owning scope: {inside:?}"
        );
        let outside_offset = source.rfind(outside_marker).unwrap() + "type Outside = typeof ".len();
        let outside = facts
            .type_queries
            .iter()
            .find(|query| query.span.lo as usize == outside_offset)
            .expect("using binding outside query");
        assert_eq!(outside.resolution, Resolution::Unresolved);
    }
}

#[test]
fn using_bindings_at_runtime_namespace_top_level_stay_in_the_iife_scope() {
    let source = r#"namespace Box { using resource = acquire(); type Inside = typeof resource; const copy = resource; } type Outside = typeof resource;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let inside_offset =
        source.find("type Inside = typeof resource").unwrap() + "type Inside = typeof ".len();
    let inside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == inside_offset)
        .expect("runtime namespace using query");
    assert!(
        matches!(inside.resolution, Resolution::Resolved(_)),
        "namespace resource should resolve inside the IIFE scope: {inside:?}"
    );
    let reference = facts
        .model
        .references
        .iter()
        .find(|reference| {
            reference.name == interner.intern("resource")
                && reference.span.lo as usize > source.find("const copy").unwrap()
        })
        .expect("runtime namespace using reference");
    assert!(
        reference.resolved.is_some(),
        "namespace resource read: {reference:?}"
    );
    let outside_offset =
        source.rfind("type Outside = typeof resource").unwrap() + "type Outside = typeof ".len();
    let outside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outside_offset)
        .expect("runtime namespace outside using query");
    assert_eq!(outside.resolution, Resolution::Unresolved);
}

#[test]
fn erased_block_declarations_use_the_existing_block_scope_without_leaking() {
    let source = r#"const value = 0;
    { declare const value: number; type Inner = typeof value; const copy = value; }
    type Outer = typeof value;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let inner_offset =
        source.find("type Inner = typeof value").unwrap() + "type Inner = typeof ".len();
    let inner = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == inner_offset)
        .expect("inner block query");
    let Resolution::Resolved(inner_symbol) = inner.resolution else {
        panic!("block declaration should resolve in its existing scope: {inner:?}");
    };
    assert_eq!(
        facts.model.symbols[inner_symbol as usize].span.lo as usize,
        source.find("declare const value").unwrap() + "declare const ".len()
    );
    let reference = facts
        .model
        .references
        .iter()
        .find(|reference| {
            reference.name == interner.intern("value")
                && reference.span.lo as usize > source.find("declare const value").unwrap()
        })
        .expect("block runtime reference");
    assert_eq!(reference.resolved, Some(inner_symbol));
    let outer_offset =
        source.rfind("type Outer = typeof value").unwrap() + "type Outer = typeof ".len();
    let outer = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outer_offset)
        .expect("outer query");
    let Resolution::Resolved(outer_symbol) = outer.resolution else {
        panic!("outer query should resolve to the module declaration: {outer:?}");
    };
    assert_eq!(
        facts.model.symbols[outer_symbol as usize].span.lo as usize,
        source.find("const value").unwrap() + "const ".len()
    );
    assert_ne!(inner_symbol, outer_symbol);
}

#[test]
fn top_level_ambient_value_bindings_resolve_without_leaking_nested_scopes() {
    let source =
        "declare const ambient: number; type Value = typeof ambient; const copy = ambient;";
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let query = facts
        .type_queries
        .iter()
        .find(|query| interner.resolve(query.name) == "ambient")
        .expect("ambient type query");
    let Resolution::Resolved(symbol) = query.resolution else {
        panic!("top-level ambient binding must resolve: {query:?}");
    };
    assert_eq!(
        interner.resolve(facts.model.symbols[symbol as usize].name),
        "ambient"
    );
    let reference = facts
        .model
        .references
        .iter()
        .find(|reference| interner.resolve(reference.name) == "ambient")
        .expect("ambient runtime reference");
    assert_eq!(reference.resolved, Some(symbol));
}

#[test]
fn global_ambient_declarations_restore_source_value_identity_across_declaration_kinds() {
    let source = r#"declare global {
        const globalValue: number;
        function globalFn(value: string): number;
        class GlobalClass { field: string; method(classValue: string): void; }
        enum GlobalEnum { Member }
    }
    type Value = typeof globalValue;
    type Fn = typeof globalFn;
    type Class = typeof GlobalClass;
    type Enum = typeof GlobalEnum;
    type Outside = typeof value;
    type OutsideClassParameter = typeof classValue;
    const uses = [globalValue, globalFn, GlobalClass, GlobalEnum];"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    for name in ["globalValue", "globalFn", "GlobalClass", "GlobalEnum"] {
        let query = facts
            .type_queries
            .iter()
            .find(|query| interner.resolve(query.name) == name)
            .unwrap_or_else(|| panic!("missing typeof query for {name}"));
        assert!(
            matches!(query.resolution, Resolution::Resolved(_)),
            "{name}: {query:?}"
        );
        assert!(
            facts
                .ambient_value_symbols
                .contains(&match query.resolution {
                    Resolution::Resolved(symbol) => symbol,
                    _ => unreachable!(),
                })
        );
    }
    for name in ["globalValue", "globalFn", "GlobalClass", "GlobalEnum"] {
        let reference = facts
            .model
            .references
            .iter()
            .find(|reference| interner.resolve(reference.name) == name)
            .unwrap_or_else(|| panic!("missing runtime reference for {name}"));
        assert!(reference.resolved.is_some(), "{name}: {reference:?}");
    }
    let leaked = facts
        .type_queries
        .iter()
        .find(|query| interner.resolve(query.name) == "value")
        .expect("signature parameter query");
    assert_eq!(leaked.resolution, Resolution::Unresolved, "{leaked:?}");
    let leaked_class = facts
        .type_queries
        .iter()
        .find(|query| interner.resolve(query.name) == "classValue")
        .expect("class signature parameter query");
    assert_eq!(
        leaked_class.resolution,
        Resolution::Unresolved,
        "{leaked_class:?}"
    );
}

#[test]
fn global_ambient_projection_does_not_relabel_a_local_binding() {
    let source = "declare global { const value: number; } const value = 1; value;";
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let local = parsed
        .identifiers
        .iter()
        .filter(|identifier| {
            identifier.role == wake_ecma_ast::SourceIdentifierRole::ValueBinding
                && identifier.name == "value"
                && identifier.span.lo > source.find('}').unwrap() as u32
        })
        .max_by_key(|identifier| identifier.span.lo)
        .expect("local value binding");
    let binding = facts
        .model
        .binding_occurrences
        .iter()
        .find(|binding| binding.span == local.span)
        .expect("local semantic binding");
    assert!(!facts.ambient_value_symbols.contains(&binding.symbol));
    let reference = facts
        .model
        .references
        .iter()
        .find(|reference| {
            reference.name == interner.intern("value") && reference.span.lo > local.span.hi
        })
        .expect("local value reference");
    assert_eq!(reference.resolved, Some(binding.symbol));
}

#[test]
fn erased_values_inside_class_static_blocks_use_the_static_block_scope() {
    let source = "class Holder { static { declare const value: number; type Inside = typeof value; const copy = value; } }";
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let query = facts
        .type_queries
        .iter()
        .find(|query| interner.resolve(query.name) == "value")
        .expect("static block type query");
    let Resolution::Resolved(symbol) = query.resolution else {
        panic!("static block query was not resolved: {query:?}");
    };
    let reference = facts
        .model
        .references
        .iter()
        .find(|reference| interner.resolve(reference.name) == "value")
        .expect("static block runtime reference");
    assert_eq!(reference.resolved, Some(symbol));
}

#[test]
fn nested_blocks_inside_runtime_namespace_static_blocks_keep_the_static_parent() {
    let source = r#"namespace Box { class Holder { static { let outer = 1; { type Inside = typeof outer; } type Static = typeof outer; } } type Outside = typeof outer; }"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    for marker in ["type Inside = typeof outer", "type Static = typeof outer"] {
        let offset =
            source.find(marker).unwrap() + marker.find("typeof ").unwrap() + "typeof ".len();
        let query = facts
            .type_queries
            .iter()
            .find(|query| query.span.lo as usize == offset)
            .expect("static block query");
        assert!(
            matches!(query.resolution, Resolution::Resolved(_)),
            "static block binding should resolve through its parent scope: {query:?}"
        );
    }
    let outside_offset = source.rfind("typeof outer").unwrap() + "typeof ".len();
    let outside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outside_offset)
        .expect("namespace outside static query");
    assert_eq!(outside.resolution, Resolution::Unresolved);
}

#[test]
fn functions_inside_runtime_namespace_static_blocks_keep_the_static_parent() {
    let source = r#"namespace Box { class Holder { static { const outer = 1; function read(): typeof outer { return outer; } } } type Outside = typeof outer; }"#;
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let read_offset = source.find("typeof outer").unwrap() + "typeof ".len();
    let read_query = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == read_offset)
        .expect("static function type query");
    assert!(
        matches!(read_query.resolution, Resolution::Resolved(_)),
        "{read_query:?}"
    );
    let return_offset = source.rfind("return outer").unwrap() + "return ".len();
    let reference = facts
        .model
        .references
        .iter()
        .find(|reference| reference.span.lo as usize == return_offset)
        .expect("static function runtime reference");
    assert!(reference.resolved.is_some());
    let outside_offset = source.rfind("typeof outer").unwrap() + "typeof ".len();
    let outside = facts
        .type_queries
        .iter()
        .find(|query| query.span.lo as usize == outside_offset)
        .expect("namespace outside static function query");
    assert_eq!(outside.resolution, Resolution::Unresolved);
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
}

#[test]
fn erased_values_inside_catch_bodies_use_the_catch_block_scope() {
    let source = "try { throw 0; } catch (error) { declare const value: number; type Inside = typeof value; const copy = value; } type Outside = typeof value;";
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let values: Vec<_> = facts
        .type_queries
        .iter()
        .filter(|query| interner.resolve(query.name) == "value")
        .collect();
    assert_eq!(values.len(), 2, "{values:?}");
    assert!(
        values
            .iter()
            .any(|query| matches!(query.resolution, Resolution::Resolved(_)))
    );
    assert!(
        values
            .iter()
            .any(|query| matches!(query.resolution, Resolution::Unresolved))
    );
    let resolved = values
        .iter()
        .find_map(|query| match query.resolution {
            Resolution::Resolved(symbol) => Some(symbol),
            _ => None,
        })
        .expect("catch value query");
    let reference = facts
        .model
        .references
        .iter()
        .find(|reference| interner.resolve(reference.name) == "value")
        .expect("catch runtime reference");
    assert_eq!(reference.resolved, Some(resolved));
}

#[test]
fn erased_values_inside_runtime_namespaces_use_the_namespace_scope() {
    let source = r#"const outer = 0;
namespace Box {
    declare const value: number;
    type Inside = typeof value;
    const copy = value;
}
type Outside = typeof value;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let inside = facts
        .type_queries
        .iter()
        .find(|query| interner.resolve(query.name) == "value")
        .expect("runtime namespace query");
    let Resolution::Resolved(symbol) = inside.resolution else {
        panic!("runtime namespace value should resolve: {inside:?}");
    };
    let reference = facts
        .model
        .references
        .iter()
        .find(|reference| {
            interner.resolve(reference.name) == "value"
                && reference.span.lo > source.find("declare const value").unwrap() as u32
        })
        .expect("runtime namespace reference");
    assert_eq!(reference.resolved, Some(symbol));
    assert!(
        facts
            .type_queries
            .iter()
            .any(|query| interner.resolve(query.name) == "value"
                && matches!(query.resolution, Resolution::Unresolved))
    );
}

#[test]
fn nested_runtime_namespaces_keep_erased_values_in_the_nearest_body() {
    let source = r#"const value = 0;
namespace Outer {
    declare const value: number;
    type OuterValue = typeof value;
    namespace Inner {
        declare const value: string;
        type InnerValue = typeof value;
        const copy = value;
    }
}
type Outside = typeof value;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let query = |label: &str| {
        let offset = source.find(label).unwrap() + label.find("typeof ").unwrap() + 7;
        facts
            .type_queries
            .iter()
            .find(|query| query.span.lo as usize == offset)
            .unwrap_or_else(|| panic!("missing query for {label}"))
    };
    let Resolution::Resolved(outer) = query("type OuterValue = typeof value").resolution else {
        panic!("outer namespace value should resolve")
    };
    let Resolution::Resolved(inner) = query("type InnerValue = typeof value").resolution else {
        panic!("inner namespace value should resolve")
    };
    assert_ne!(outer, inner);
    let Resolution::Resolved(module) = query("type Outside = typeof value").resolution else {
        panic!("module value should resolve")
    };
    assert_ne!(module, outer);
    assert_ne!(module, inner);
}

#[test]
fn top_level_erased_declarations_keep_their_original_value_kinds() {
    let source = "declare var variable: number; declare let lexical: number; declare const constant: number;";
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    for (name, expected) in [
        ("variable", wake_ecma_semantic::DeclKind::Var),
        ("lexical", wake_ecma_semantic::DeclKind::Let),
        ("constant", wake_ecma_semantic::DeclKind::Const),
    ] {
        let binding = facts
            .model
            .binding_occurrences
            .iter()
            .find(|binding| interner.resolve(binding.name) == name)
            .unwrap_or_else(|| panic!("missing erased binding {name}"));
        assert_eq!(binding.decl_kind, expected, "{name}: {binding:?}");
    }
}

#[test]
fn ambient_namespace_values_use_independent_source_scopes() {
    let source = r#"declare namespace Ambient {
        const value: number;
        function fn(parameter: string): number;
        namespace Nested { const nestedValue: string; type NestedMember = typeof nestedValue; }
        type Member = typeof value;
    }
    type Root = typeof Ambient;
    type Outside = typeof value;
    const copy = Ambient;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let ambient = facts
        .type_queries
        .iter()
        .find(|query| interner.resolve(query.name) == "Ambient")
        .expect("namespace root query");
    assert!(matches!(ambient.resolution, Resolution::Resolved(_)));
    let values: Vec<_> = facts
        .type_queries
        .iter()
        .filter(|query| interner.resolve(query.name) == "value")
        .collect();
    assert_eq!(values.len(), 2, "{values:?}");
    assert!(
        values
            .iter()
            .any(|query| matches!(query.resolution, Resolution::Resolved(_)))
    );
    assert!(
        values
            .iter()
            .any(|query| matches!(query.resolution, Resolution::Unresolved))
    );
    let nested = facts
        .type_queries
        .iter()
        .find(|query| interner.resolve(query.name) == "nestedValue")
        .expect("nested namespace query");
    assert!(matches!(nested.resolution, Resolution::Resolved(_)));
    let root = facts
        .model
        .references
        .iter()
        .find(|reference| interner.resolve(reference.name) == "Ambient")
        .expect("namespace root reference");
    assert!(root.resolved.is_some());
}

#[test]
fn nested_ambient_namespace_inside_runtime_namespace_keeps_runtime_parent() {
    let source = r#"namespace Outer {
        declare namespace Inner {
            const value: number;
            type Member = typeof value;
        }
        type Root = typeof Inner;
    }
    type Outside = typeof Inner;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let inner_queries: Vec<_> = facts
        .type_queries
        .iter()
        .filter(|query| interner.resolve(query.name) == "Inner")
        .collect();
    assert_eq!(inner_queries.len(), 2, "{inner_queries:?}");
    assert!(matches!(
        inner_queries[0].resolution,
        Resolution::Resolved(_)
    ));
    assert!(matches!(
        inner_queries[1].resolution,
        Resolution::Unresolved
    ));
    let value = facts
        .type_queries
        .iter()
        .find(|query| interner.resolve(query.name) == "value")
        .expect("nested ambient value query");
    assert!(matches!(value.resolution, Resolution::Resolved(_)));
}

#[test]
fn runtime_and_ambient_namespace_declarations_merge_one_value_scope() {
    let source = r#"declare namespace Outer {
        const ambientValue: string;
        type AmbientMember = typeof runtimeValue;
    }
    namespace Outer {
        declare const runtimeValue: number;
        type RuntimeMember = typeof ambientValue;
    }
    type Root = typeof Outer;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    for name in ["ambientValue", "runtimeValue", "Outer"] {
        let queries: Vec<_> = facts
            .type_queries
            .iter()
            .filter(|query| interner.resolve(query.name) == name)
            .collect();
        assert!(!queries.is_empty(), "{name}: {queries:?}");
        assert!(
            queries
                .iter()
                .all(|query| matches!(query.resolution, Resolution::Resolved(_))),
            "{name}: {queries:?}"
        );
    }
}

#[test]
fn repeated_runtime_namespace_declarations_merge_one_value_scope() {
    let source = r#"namespace Outer {
        const first = "";
        type First = typeof second;
    }
    namespace Outer {
        const second = "";
        type Second = typeof first;
    }
    declare namespace Outer {
        const ambient: string;
        type Ambient = typeof second;
    }
    type Root = typeof Outer;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    for name in ["first", "second", "Outer"] {
        let queries: Vec<_> = facts
            .type_queries
            .iter()
            .filter(|query| interner.resolve(query.name) == name)
            .collect();
        assert!(!queries.is_empty(), "{name}: {queries:?}");
        assert!(
            queries
                .iter()
                .all(|query| matches!(query.resolution, Resolution::Resolved(_))),
            "{name}: {queries:?}"
        );
    }
}

#[test]
fn dotted_runtime_and_ambient_namespaces_merge_each_path_scope() {
    let source = r#"namespace Outer.Inner {
        const runtimeValue = "";
        type RuntimeMember = typeof ambientValue;
    }
    declare namespace Outer {
        namespace Inner {
            const ambientValue = "";
            type AmbientMember = typeof runtimeValue;
        }
    }
    type Root = typeof Outer;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    for name in ["ambientValue", "runtimeValue", "Outer"] {
        let queries: Vec<_> = facts
            .type_queries
            .iter()
            .filter(|query| interner.resolve(query.name) == name)
            .collect();
        assert!(!queries.is_empty(), "{name}: {queries:?}");
        assert!(
            queries
                .iter()
                .all(|query| matches!(query.resolution, Resolution::Resolved(_))),
            "{name}: {queries:?}"
        );
    }
}

#[test]
fn global_and_module_namespaces_keep_distinct_value_scopes() {
    let source = r#"declare global {
        namespace Outer {
            const globalValue = "";
            type GlobalMember = typeof localValue;
        }
    }
    namespace Outer {
        const localValue = "";
        type LocalMember = typeof globalValue;
    }
    type Root = typeof Outer;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let queries: Vec<_> = facts
        .type_queries
        .iter()
        .map(|query| (interner.resolve(query.name), query.resolution))
        .collect();
    assert!(queries.iter().any(|(name, resolution)| {
        *name == "localValue" && !matches!(resolution, Resolution::Resolved(_))
    }));
    assert!(queries.iter().any(|(name, resolution)| {
        *name == "globalValue" && !matches!(resolution, Resolution::Resolved(_))
    }));
}

#[test]
fn global_namespace_root_is_visible_without_module_shadowing() {
    let source = r#"declare global {
        namespace Global {
            const value = "";
            type Member = typeof value;
        }
    }
    type Root = typeof Global;"#;
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
    let facts = parsed.parsed.module.with_ast(|program| {
        analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    for name in ["value", "Global"] {
        let query = facts
            .type_queries
            .iter()
            .find(|query| interner.resolve(query.name) == name)
            .unwrap_or_else(|| panic!("missing query for {name}"));
        assert!(
            matches!(query.resolution, Resolution::Resolved(_)),
            "{query:?}"
        );
    }
}
