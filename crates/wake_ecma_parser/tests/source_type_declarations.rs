use wake_common::Interner;
use wake_ecma_ast::SourceNodeKind;
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

#[test]
fn erased_type_declarations_keep_names_and_original_lexical_containers() {
    let source = r#"type \u0054op<T> = T;
    { interface Local {} }
    function fn() { type Inside = Top<string>; }
    switch (value) { case 0: type Case = string; break; default: interface Other {} }
    class Box { static { interface Static {} } }
    namespace \u0041.B { export interface Member {} namespace Inner { type Private = Member } }
    declare module '\u0070kg' { export type Ambient = string; }"#;
    let interner = Interner::new();
    let output = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    let declarations: Vec<_> = output
        .type_declarations
        .iter()
        .filter(|d| {
            matches!(
                output.syntax[d.node].kind,
                SourceNodeKind::TsInterface | SourceNodeKind::TsTypeAlias,
            )
        })
        .collect();
    assert_eq!(
        declarations
            .iter()
            .map(|d| d.name.name.as_str())
            .collect::<Vec<_>>(),
        [
            "Top", "Local", "Inside", "Case", "Other", "Static", "Member", "Private", "Ambient"
        ]
    );
    let parent = |i: usize| {
        output.syntax[declarations[i].node]
            .parent
            .map(|p| output.syntax[p].kind)
    };
    assert_eq!(parent(0), None);
    assert_eq!(parent(1), Some(SourceNodeKind::JsBlock));
    assert_eq!(parent(2), Some(SourceNodeKind::JsFunctionBody));
    assert_eq!(parent(3), Some(SourceNodeKind::JsSwitchBody));
    assert_eq!(parent(4), Some(SourceNodeKind::JsSwitchBody));
    assert_eq!(parent(5), Some(SourceNodeKind::JsBlock));
    assert_eq!(parent(6), Some(SourceNodeKind::TsNamespace));
    assert_eq!(parent(7), Some(SourceNodeKind::TsNamespace));
    assert_eq!(parent(8), Some(SourceNodeKind::TsAmbientModule));
    for declaration in &declarations {
        assert!(output.identifiers.contains(&declaration.name));
        let node = &output.syntax[declaration.node];
        assert!(matches!(
            node.kind,
            SourceNodeKind::TsTypeAlias | SourceNodeKind::TsInterface
        ));
        assert!(
            node.span.lo <= declaration.name.span.lo && declaration.name.span.hi <= node.span.hi
        );
    }
    let namespaces = &output.namespaces;
    assert_eq!(namespaces.len(), 3);
    assert_eq!(
        namespaces[0]
            .names
            .iter()
            .map(|n| n.name.as_str())
            .collect::<Vec<_>>(),
        ["A", "B"]
    );
    assert_eq!(namespaces[1].names[0].name, "Inner");
    assert_eq!(
        output.syntax[namespaces[1].node].parent,
        Some(namespaces[0].node)
    );
    assert_eq!(namespaces[2].ambient.as_ref().unwrap().value, "pkg");
    assert!(namespaces[2].names.is_empty());
    for namespace in namespaces {
        let span = namespace.body.unwrap();
        let text = &source[span.lo as usize..span.hi as usize];
        assert!(text.starts_with('{') && text.ends_with('}'));
    }
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert_eq!(
        output.parsed.module.structure_hash(),
        ordinary.module.structure_hash()
    );
}

#[test]
fn classes_and_enums_keep_type_names_and_expression_locality_before_lowering() {
    let source = "@decorate(Outer) class Outer<T> {} const cls = class Inner<I> {}; enum E { A } declare class Ambient {}";
    let interner = Interner::new();
    let output = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    assert_eq!(
        output
            .type_declarations
            .iter()
            .map(|d| (d.name.name.as_str(), d.in_own_scope))
            .collect::<Vec<_>>(),
        [
            ("Outer", false),
            ("Inner", true),
            ("E", false),
            ("Ambient", false)
        ]
    );
    assert_eq!(
        output.syntax[output.type_declarations[0].node].kind,
        SourceNodeKind::JsClass
    );
    assert_eq!(
        output.syntax[output.type_declarations[2].node].kind,
        SourceNodeKind::TsEnum
    );
    let outer = output.syntax[output.type_declarations[0].node].span;
    assert_eq!(
        &source[outer.lo as usize..outer.hi as usize],
        "class Outer<T> {}"
    );
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert_eq!(
        output.parsed.module.structure_hash(),
        ordinary.module.structure_hash()
    );
}
