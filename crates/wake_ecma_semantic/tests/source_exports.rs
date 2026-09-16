use wake_common::Interner;
use wake_ecma_parser::{ParseOptions, SourceType, parse_source};
use wake_ecma_semantic::{SourceExportUseKind as Kind, analyze, analyze_source};

#[test]
fn local_export_uses_preserve_compilation_identity_without_becoming_evaluated_reads() {
    let source = r#"export { later as "公共", missing };
const later = 1;
const foreign = 2;
type Shape = string;
export { foreign as remote, type Shape } from './other';
export type { Shape };
export const { first, key: second = fallback, ...rest } = input;
export function run(parameter) { const internal = parameter; return internal; }
export default class Default { method(argument) { return argument; } }
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
                wake_ecma_semantic::SourceSemanticInput {
                    identifiers: &parsed.identifiers,
                    exports: &parsed.exports,
                    syntax: &parsed.syntax,
                    functions: &parsed.functions,
                    namespaces: &parsed.namespaces,
                },
            ),
        )
    });
    assert_eq!(
        facts
            .exports
            .iter()
            .map(|usage| interner.resolve(usage.name))
            .collect::<Vec<_>>(),
        [
            "later", "missing", "first", "second", "rest", "run", "Default"
        ]
    );
    assert_eq!(facts.exports[0].kind, Kind::Named);
    assert!(facts.exports[0].resolved.is_some());
    assert!(facts.exports[1].resolved.is_none());
    assert!(
        facts.exports[2..]
            .iter()
            .all(|usage| usage.kind == Kind::Declaration && usage.resolved.is_some())
    );
    assert!(facts.exports.iter().all(|usage| usage.scope == 0));
    assert_eq!(facts.model.symbols.len(), ordinary.symbols.len());
    for (left, right) in facts.model.symbols.iter().zip(&ordinary.symbols) {
        assert_eq!(
            (left.name, left.span, left.scope, left.decl_kind),
            (right.name, right.span, right.scope, right.decl_kind)
        );
    }
    assert_eq!(
        facts.model.binding_occurrences,
        ordinary.binding_occurrences
    );
    assert_eq!(facts.model.references.len(), ordinary.references.len());
    for (left, right) in facts.model.references.iter().zip(&ordinary.references) {
        assert_eq!(
            (left.name, left.span, left.scope, left.resolved, left.access),
            (
                right.name,
                right.span,
                right.scope,
                right.resolved,
                right.access
            )
        );
    }
    assert!(!facts.references.iter().any(|index| {
        facts
            .exports
            .iter()
            .any(|usage| usage.span == facts.model.references[*index].span)
    }));
}

#[test]
fn namespace_exports_keep_inner_identity_and_skip_erased_types_and_initializer_bindings() {
    let source = r#"const item = 0;
namespace Box {
    const item = 1;
    export { item as publicItem };
    export const result = function privateName(parameter) { return parameter; };
    export type { item };
    export interface Shape { value: string }
}
export default function named() { return Box; }
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
            wake_ecma_semantic::SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    assert_eq!(
        facts
            .exports
            .iter()
            .map(|usage| interner.resolve(usage.name))
            .collect::<Vec<_>>(),
        ["item", "result", "named"]
    );
    let exported_item = facts.exports[0].resolved.unwrap();
    assert_ne!(facts.model.symbols[exported_item as usize].scope, 0);
    assert_eq!(
        facts.model.symbols[exported_item as usize].span.lo as usize,
        source.find("const item = 1").unwrap() + 6
    );
    assert!(
        facts
            .exports
            .iter()
            .all(|usage| facts.source_symbols.contains(&usage.resolved.unwrap()))
    );
    assert!(
        !facts
            .references
            .iter()
            .any(|index| facts.model.references[*index].span == facts.exports[0].span)
    );
}
