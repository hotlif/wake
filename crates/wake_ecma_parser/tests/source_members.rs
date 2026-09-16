use wake_common::{Interner, Span};
use wake_ecma_ast::SourceMemberKind;
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

fn text(source: &str, span: Span) -> &str {
    &source[span.lo as usize..span.hi as usize]
}

#[test]
fn original_members_keep_erased_receivers_keys_private_names_and_constructor_boundaries() {
    let source = "(value as any)!.prop?.[key as string]; new holder!.Ctor<T>(); class C { #field; read(x) { return x.#field; } } const view=<UI.View>{value.prop}</UI.View>; type T = Value['prop']; namespace N { export const v = value.x; }";
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    let actual: Vec<_> = parsed
        .members
        .iter()
        .map(|member| {
            (
                text(source, member.span),
                text(source, member.object),
                text(source, member.property),
                member.kind,
                member.optional,
            )
        })
        .collect();
    assert_eq!(
        actual,
        [
            (
                "(value as any)!.prop",
                "(value as any)!",
                "prop",
                SourceMemberKind::Named,
                false
            ),
            (
                "(value as any)!.prop?.[key as string]",
                "(value as any)!.prop",
                "key as string",
                SourceMemberKind::Computed,
                true
            ),
            (
                "holder!.Ctor",
                "holder!",
                "Ctor",
                SourceMemberKind::Named,
                false
            ),
            ("x.#field", "x", "#field", SourceMemberKind::Private, false),
            (
                "value.prop",
                "value",
                "prop",
                SourceMemberKind::Named,
                false
            ),
            ("value.x", "value", "x", SourceMemberKind::Named, false),
        ]
    );
    assert_eq!(
        parsed.parsed.module.structure_hash(),
        parse(source, &interner, SourceType::Tsx)
            .module
            .structure_hash()
    );
}

#[test]
fn member_metadata_rewinds_speculation_and_preserves_nested_computed_expressions() {
    let source = "const fn = <T>(x = value.prop) => x; const compare = value < other.prop > final.key; const item = new (factory as any)[key!.name](arg?.value); obj[key.inner].last;";
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
    let actual: Vec<_> = parsed
        .members
        .iter()
        .map(|member| text(source, member.span))
        .collect();
    assert_eq!(
        actual,
        [
            "value.prop",
            "other.prop",
            "final.key",
            "key!.name",
            "(factory as any)[key!.name]",
            "arg?.value",
            "key.inner",
            "obj[key.inner]",
            "obj[key.inner].last"
        ]
    );
}
