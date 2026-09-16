use wake_common::Interner;
use wake_ecma_parser::{ParseOptions, SourceNodeKind, SourceType, parse_source, parse_with};

#[test]
fn original_type_references_arrays_indexed_accesses_and_operators_are_distinct() {
    let source = r#"type A = readonly Array<string>[]; type B = (Left | Right)[][]; type C = Data[Key][]; type D = typeof value[]; type E = ns.Array<number>; type F = Arr\u0061y<ReadonlyArray<boolean>>;"#;
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
    let texts = |kind| {
        parsed
            .syntax
            .iter()
            .filter(|node| node.kind == kind)
            .map(|node| &source[node.span.lo as usize..node.span.hi as usize])
            .collect::<Vec<_>>()
    };
    assert_eq!(
        texts(SourceNodeKind::TsArrayType),
        [
            "Array<string>[]",
            "(Left | Right)[]",
            "(Left | Right)[][]",
            "Data[Key][]",
            "typeof value[]"
        ]
    );
    assert_eq!(texts(SourceNodeKind::TsIndexedAccessType), ["Data[Key]"]);
    assert_eq!(
        texts(SourceNodeKind::TsTypeOperator),
        ["readonly Array<string>[]", "typeof value[]"]
    );
    assert!(texts(SourceNodeKind::TsTypeReference).contains(&"ns.Array<number>"));
    let array = parsed
        .syntax
        .iter()
        .position(|node| {
            node.kind == SourceNodeKind::TsTypeReference
                && source[node.span.lo as usize..node.span.hi as usize].starts_with("Arr\\u")
        })
        .unwrap();
    assert!(
        parsed
            .syntax
            .iter()
            .any(|node| node.kind == SourceNodeKind::TsTypeArguments && node.parent == Some(array))
    );
    assert!(
        parsed
            .identifiers
            .iter()
            .any(|identifier| identifier.name == "Array"
                && &source[identifier.span.lo as usize..identifier.span.hi as usize]
                    == "Arr\\u0061y")
    );
    let ordinary = parse_with(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert_eq!(
        ordinary.module.structure_hash(),
        parsed.parsed.module.structure_hash()
    );
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", parsed.parsed.dependencies)
    );
}

#[test]
fn original_function_bodies_exclude_namespace_lowering() {
    let source = "function f() { return 1; } const g = () => { return; }; class C { m() { return 2; } } namespace N { export const x = 1; }";
    let output = parse_source(
        source,
        &Interner::new(),
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    let bodies: Vec<_> = output
        .syntax
        .iter()
        .filter(|node| node.kind == SourceNodeKind::JsFunctionBody)
        .map(|node| &source[node.span.lo as usize..node.span.hi as usize])
        .collect();
    assert_eq!(bodies, ["{ return 1; }", "{ return; }", "{ return 2; }"]);
}

#[test]
fn static_block_source_container_includes_the_static_header() {
    let source =
        "class Holder { static { declare const value: number; type Inside = typeof value; } }";
    let output = parse_source(
        source,
        &Interner::new(),
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    let blocks: Vec<_> = output
        .syntax
        .iter()
        .filter(|node| node.kind == SourceNodeKind::JsBlock)
        .map(|node| &source[node.span.lo as usize..node.span.hi as usize])
        .collect();
    assert_eq!(
        blocks,
        ["static { declare const value: number; type Inside = typeof value; }"]
    );
}

#[test]
fn declarations_survive_erasure_and_ambient_module_interiors_are_structured() {
    let source =
        "namespace A.B { interface Empty {} } declare module 'ambient' { interface X { x: any } }";
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
    let texts = |kind| {
        output
            .syntax
            .iter()
            .filter(|node| node.kind == kind)
            .map(|node| &source[node.span.lo as usize..node.span.hi as usize])
            .collect::<Vec<_>>()
    };
    assert_eq!(
        texts(SourceNodeKind::TsNamespace),
        ["namespace A.B { interface Empty {} }"]
    );
    assert_eq!(
        texts(SourceNodeKind::TsInterface),
        ["interface Empty {}", "interface X { x: any }"]
    );
    assert_eq!(texts(SourceNodeKind::TsAny), ["any"]);
    for node in &output.syntax {
        if let Some(parent) = node.parent {
            assert!(
                output.syntax[parent].span.lo <= node.span.lo
                    && output.syntax[parent].span.hi >= node.span.hi
            );
        }
    }
    let ordinary = parse_with(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert_eq!(
        format!(
            "{:?}",
            ordinary.module.with_ast(|program| format!("{program:?}"))
        ),
        format!(
            "{:?}",
            output
                .parsed
                .module
                .with_ast(|program| format!("{program:?}"))
        )
    );
}

#[test]
fn retains_erased_type_ranges_and_nested_angle_brackets() {
    let source = "const f = <T extends object,>(x: Map<string, Array<T>>): T | undefined => x as T; f<Map<string, number>>(value);";
    let output = parse_source(
        source,
        &Interner::new(),
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    let texts = |kind| {
        output
            .syntax
            .iter()
            .filter(|node| node.kind == kind)
            .map(|node| &source[node.span.lo as usize..node.span.hi as usize])
            .collect::<Vec<_>>()
    };
    assert_eq!(
        texts(SourceNodeKind::TsTypeParameters),
        ["<T extends object,>"]
    );
    assert_eq!(
        texts(SourceNodeKind::TsTypeAnnotation),
        [": Map<string, Array<T>>", ": T | undefined"]
    );
    assert_eq!(
        texts(SourceNodeKind::TsTypeArguments),
        [
            "<string, Array<T>>",
            "<T>",
            "<Map<string, number>>",
            "<string, number>"
        ]
    );
    assert!(texts(SourceNodeKind::TsType).contains(&"T | undefined"));
    for (i, node) in output.syntax.iter().enumerate() {
        if let Some(parent) = node.parent {
            assert!(parent < i);
            let parent = &output.syntax[parent];
            assert!(parent.span.lo <= node.span.lo && parent.span.hi >= node.span.hi);
        }
    }
}

#[test]
fn structured_type_interiors_capture_any_without_matching_property_or_value_names() {
    let source = "type A = { any: string; f: (any: number) => any; tuple: [any, { nested: any }]; mapped: { [P in keyof T]: any } }; interface B { call(x: any): any; } let any = value; const query: typeof any = any;";
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
    let any: Vec<_> = output
        .syntax
        .iter()
        .filter(|node| node.kind == SourceNodeKind::TsAny)
        .collect();
    assert_eq!(any.len(), 6);
    assert!(
        any.iter()
            .all(|node| &source[node.span.lo as usize..node.span.hi as usize] == "any")
    );
    for kind in [
        SourceNodeKind::TsObjectType,
        SourceNodeKind::TsTupleType,
        SourceNodeKind::TsSignature,
        SourceNodeKind::TsTypeMember,
    ] {
        assert!(
            output.syntax.iter().any(|node| node.kind == kind),
            "missing {kind:?}"
        );
    }
    let plain = parse_with(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert_eq!(
        plain.module.with_ast(|p| format!("{p:?}")),
        output.parsed.module.with_ast(|p| format!("{p:?}"))
    );
    for node in &output.syntax {
        if let Some(parent) = node.parent {
            assert!(output.syntax[parent].span.contains(node.span));
        }
    }
}

#[test]
fn non_null_assertions_are_original_postfix_syntax_and_speculation_is_rolled_back() {
    let source = "const x = value!.field!; new C!(); const y = (fn?.call)!(x); if (!ready) run(); let definite!: T; const check = left < right > other;";
    let output = parse_source(
        source,
        &Interner::new(),
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    let nodes: Vec<_> = output
        .syntax
        .iter()
        .filter(|node| node.kind == SourceNodeKind::TsNonNullAssertion)
        .collect();
    assert_eq!(nodes.len(), 4);
    assert!(
        nodes
            .iter()
            .all(|node| &source[node.span.lo as usize..node.span.hi as usize] == "!")
    );
}

#[test]
fn failed_type_argument_speculation_leaves_no_source_nodes() {
    let source = "const result = left < /* compare */ right > other;";
    let output = parse_source(
        source,
        &Interner::new(),
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    assert!(output.syntax.is_empty(), "{:?}", output.syntax);
    assert_eq!(output.comments.len(), 1);
}

#[test]
fn type_capture_preserves_existing_compilation_and_declaration_erasure() {
    let source = "type Item<T> = { value: T }; interface Props { item: Item<string> } const el: Props = value;";
    let interner = Interner::new();
    let output = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    let ordinary = parse_with(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert_eq!(
        ordinary.module.with_ast(|p| format!("{p:?}")),
        output.parsed.module.with_ast(|p| format!("{p:?}"))
    );
    assert_eq!(
        format!("{:?}", ordinary.diagnostics),
        format!("{:?}", output.parsed.diagnostics)
    );
    assert_eq!(
        ordinary.module.structure_hash(),
        output.parsed.module.structure_hash()
    );
}
