use wake_common::Interner;
use wake_ecma_ast::{SourceIdentifierRole, SourceImportBindingKind};
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

#[test]
fn imports_preserve_decoded_names_type_modifiers_attributes_and_equals_targets() {
    let source = r#"import Main, { value as local, type Shape, "external-name" as external } from './m\u006fdule' with { type: 'json' };
import type DefaultType from './types';
import type * as Namespace from './types';
import type { Original as Alias } from './types' with { 'resolution-mode': 'import' };
import './side-effect';
import type TypeEquals = require('./types');
import ValueEquals = require('./values');
import Entity = Namespace.Member;
import { type Alone } from './only-types';"#;
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
    assert_eq!(output.imports.len(), 9);
    let first = &output.imports[0];
    assert_eq!(first.source.as_ref().unwrap().value, "./module");
    assert_eq!(
        &source[first.source.as_ref().unwrap().span.lo as usize
            ..first.source.as_ref().unwrap().span.hi as usize],
        "'./m\\u006fdule'"
    );
    assert_eq!(
        first
            .bindings
            .iter()
            .map(|binding| binding.local.name.as_str())
            .collect::<Vec<_>>(),
        ["Main", "local", "Shape", "external"]
    );
    assert_eq!(first.bindings[0].kind, SourceImportBindingKind::Default);
    assert_eq!(first.bindings[1].imported.as_deref(), Some("value"));
    assert_eq!(
        first.bindings[2].local.role,
        SourceIdentifierRole::TypeBinding
    );
    assert_eq!(first.bindings[3].imported.as_deref(), Some("external-name"));
    assert_eq!(first.attributes.as_ref().unwrap().keyword, "with");
    assert_eq!(first.attributes.as_ref().unwrap().entries[0].value, "json");
    assert!(output.imports[1..4].iter().all(|import| import.type_only));
    assert_eq!(
        output.imports[2].bindings[0].kind,
        SourceImportBindingKind::Namespace
    );
    assert_eq!(
        output.imports[3].attributes.as_ref().unwrap().entries[0].key,
        "resolution-mode"
    );
    assert!(output.imports[4].bindings.is_empty());
    assert_eq!(
        output.imports[5].bindings[0].kind,
        SourceImportBindingKind::Equals
    );
    assert!(output.imports[5].type_only);
    assert_eq!(output.imports[5].source.as_ref().unwrap().value, "./types");
    assert_eq!(output.imports[6].source.as_ref().unwrap().value, "./values");
    assert!(output.imports[7].source.is_none());
    let target = output.imports[7].equals_target.unwrap();
    assert_eq!(
        &source[target.lo as usize..target.hi as usize],
        "Namespace.Member"
    );
    assert!(!output.imports[8].type_only);
    assert!(output.imports[8].bindings[0].type_only);
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert_eq!(
        ordinary.module.structure_hash(),
        output.parsed.module.structure_hash()
    );
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", output.parsed.dependencies)
    );
    assert!(
        output
            .imports
            .windows(2)
            .all(|pair| pair[0].span.hi <= pair[1].span.lo)
    );
}

#[test]
fn import_capture_preserves_ambient_ownership_and_ignores_dynamic_and_literal_impostors() {
    let source = "declare module 'ambient' { import type { Thing } from './types'; export type Value = Thing; } import { value } from './runtime'; const lazy = import('./dynamic'); const text = \"import X from 'fake'\"; type T = import('./query').Value;";
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
    assert_eq!(output.imports.len(), 2);
    let parent = output.imports[0].parent.unwrap();
    assert_eq!(
        output.syntax[parent].kind,
        wake_ecma_ast::SourceNodeKind::TsAmbientModule
    );
    assert!(output.imports[1].parent.is_none());
    for import in &output.imports {
        assert!(source.is_char_boundary(import.span.lo as usize));
        assert!(source.is_char_boundary(import.span.hi as usize));
    }
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", output.parsed.dependencies)
    );
    assert_eq!(
        ordinary.module.structure_hash(),
        output.parsed.module.structure_hash()
    );
}
