use wake_common::Interner;
use wake_ecma_ast::{SourceExportKind as Kind, SourceIdentifierRole as Role};
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

#[test]
fn exports_keep_original_names_kinds_and_local_reference_roles() {
    let source = r#"const value = 1;
type Shape = string;
export { value as "外部", type Shape as PublicShape };
export type { Shape as PublicType };
export { remote as external, type Foreign } from './m\u006fdule' with { type: 'json' };
export * as Everything from './all';
export type * as Types from './types';
export default function named() { return value; }
export const { first, key: second } = input;
export interface Contract { value: string }
export = value;
export as namespace Library;
"#;
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
    assert_eq!(output.exports.len(), 10);
    assert_eq!(
        output
            .exports
            .iter()
            .map(|export| export.kind)
            .collect::<Vec<_>>(),
        [
            Kind::Named,
            Kind::Named,
            Kind::Named,
            Kind::All,
            Kind::All,
            Kind::Default,
            Kind::Declaration,
            Kind::Declaration,
            Kind::Assignment,
            Kind::Namespace
        ]
    );
    let first = &output.exports[0];
    assert_eq!(first.specifiers[0].local.value, "value");
    assert_eq!(first.specifiers[0].exported.value, "外部");
    assert!(!first.specifiers[0].exported.identifier);
    assert!(first.specifiers[1].type_only);
    assert!(output.exports[1].type_only);
    let remote = &output.exports[2];
    assert_eq!(remote.source.as_ref().unwrap().value, "./module");
    assert_eq!(remote.attributes.as_ref().unwrap().entries[0].value, "json");
    assert_eq!(
        output.exports[3].exported.as_ref().unwrap().value,
        "Everything"
    );
    assert!(output.exports[4].type_only);
    assert_eq!(output.exports[4].exported.as_ref().unwrap().value, "Types");
    assert_eq!(
        output.exports[9].exported.as_ref().unwrap().value,
        "Library"
    );
    for export in &output.exports {
        assert!(export.parent.is_none());
        assert!(source[export.span.lo as usize..export.span.hi as usize].starts_with("export"));
        for specifier in &export.specifiers {
            let occurrences = output
                .identifiers
                .iter()
                .filter(|id| id.span == specifier.local.span)
                .collect::<Vec<_>>();
            if export.source.is_some() {
                assert!(
                    occurrences.is_empty(),
                    "foreign name is not a local reference: {specifier:?}"
                );
            } else {
                assert_eq!(occurrences.len(), 1);
                assert_eq!(
                    occurrences[0].role,
                    if specifier.type_only {
                        Role::TypeReference
                    } else {
                        Role::ValueReference
                    }
                );
            }
            if specifier.local.span != specifier.exported.span {
                assert!(
                    !output
                        .identifiers
                        .iter()
                        .any(|id| id.span == specifier.exported.span)
                );
            }
        }
    }
    let assignment = output.exports[8].target.unwrap();
    assert_eq!(
        &source[assignment.lo as usize..assignment.hi as usize],
        "value"
    );
    let interface = output.exports[7].target.unwrap();
    assert_eq!(
        &source[interface.lo as usize..interface.hi as usize],
        "interface Contract { value: string }"
    );
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert_eq!(
        ordinary.module.structure_hash(),
        output.parsed.module.structure_hash()
    );
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", output.parsed.dependencies)
    );
}

#[test]
fn erased_exports_retain_ambient_parent_and_type_only_attributes() {
    let source = r#"declare module 'ambient' {
        export type { "foreign-name" as Public } from './types' with { 'resolution-mode': 'import' };
        export { type Other } from './more';
        export type Alias = string;
    }
    const text = "export { fake }"; const lazy = import('./dynamic');
    export { value } from './runtime' assert { type: 'json' };
    "#;
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
    assert_eq!(output.exports.len(), 4);
    for export in &output.exports[..3] {
        let parent = export.parent.unwrap();
        assert_eq!(
            output.syntax[parent].kind,
            wake_ecma_ast::SourceNodeKind::TsAmbientModule
        );
    }
    assert!(output.exports[3].parent.is_none());
    assert_eq!(
        output.exports[0].attributes.as_ref().unwrap().entries[0].value,
        "import"
    );
    assert!(!output.exports[0].specifiers[0].local.identifier);
    assert_eq!(
        output.exports[3].attributes.as_ref().unwrap().keyword,
        "assert"
    );
    for export in &output.exports[..2] {
        assert!(!output.identifiers.iter().any(|id| {
            export
                .specifiers
                .iter()
                .any(|spec| spec.local.span == id.span)
        }));
    }
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert!(!ordinary.has_errors(), "{:?}", ordinary.diagnostics);
    assert_eq!(
        ordinary.module.structure_hash(),
        output.parsed.module.structure_hash()
    );
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", output.parsed.dependencies)
    );
}
