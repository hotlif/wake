use wake_common::Interner;
use wake_ecma_ast::{SourceIdentifierRole, SourceValueBindingKind};
use wake_ecma_parser::{ParseOptions, SourceType, parse_source};

#[test]
fn source_identifier_facts_keep_user_roles_cooked_names_and_jsx_roots() {
    let source = r#"const \u0061 = value; const { field: local, short } = obj; const data = { a, key: local }; const el = <UI.Item value={a}><x-tag /></UI.Item>;"#;
    let output = parse_source(
        source,
        &Interner::new(),
        SourceType::Tsx,
        ParseOptions::default(),
    );
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    let refs: Vec<_> = output
        .identifiers
        .iter()
        .filter(|id| id.role == SourceIdentifierRole::ValueReference)
        .map(|id| id.name.as_str())
        .collect();
    assert_eq!(refs, ["value", "obj", "a", "local", "UI", "a"]);
    let bindings: Vec<_> = output
        .identifiers
        .iter()
        .filter(|id| id.role == SourceIdentifierRole::ValueBinding)
        .map(|id| id.name.as_str())
        .collect();
    assert_eq!(bindings, ["a", "local", "short", "data", "el"]);
    assert!(
        output
            .identifiers
            .iter()
            .all(|id| !id.name.starts_with("_jsx"))
    );
}

#[test]
fn type_identifiers_are_distinct_from_type_query_values_and_speculative_comparisons() {
    let source = "type Alias<T> = { value: T; query: typeof VALUE }; interface Shape { x: Alias<string> } const result = left < right > other;";
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
    let of = |role| {
        output
            .identifiers
            .iter()
            .filter(|id| id.role == role)
            .map(|id| id.name.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        of(SourceIdentifierRole::TypeBinding),
        ["Alias", "T", "Shape"]
    );
    assert_eq!(
        of(SourceIdentifierRole::TypeReference),
        ["T", "Alias", "string"]
    );
    assert_eq!(of(SourceIdentifierRole::TypeQuery), ["VALUE"]);
    assert_eq!(
        of(SourceIdentifierRole::ValueReference),
        ["left", "right", "other"]
    );
}

#[test]
fn import_locals_have_complete_value_and_type_binding_roles() {
    let source = r#"import Main, { short, imported as renamed, "external" as stringName, type Shape, type Other as Local } from 'one';
import type DefaultType from 'types';
import type * as Types from 'types';
import type { Pair, Original as LocalType } from 'types';
import type \u0041lias = require('types');
import Runtime = require('runtime');
import type from 'value';
import * as Namespace from 'value';"#;
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
    let of = |role| {
        output
            .identifiers
            .iter()
            .filter(|id| id.role == role)
            .map(|id| id.name.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        of(SourceIdentifierRole::ValueBinding),
        [
            "Main",
            "short",
            "renamed",
            "stringName",
            "Runtime",
            "type",
            "Namespace"
        ]
    );
    assert_eq!(
        of(SourceIdentifierRole::TypeBinding),
        [
            "Shape",
            "Local",
            "DefaultType",
            "Types",
            "Pair",
            "LocalType",
            "Alias"
        ]
    );
    for identifier in &output.identifiers {
        assert!(
            !["imported", "Other", "Original"].contains(&identifier.name.as_str()),
            "external import name became a local identifier: {identifier:?}"
        );
    }
    assert!(
        output
            .identifiers
            .windows(2)
            .all(|pair| pair[0].span.lo <= pair[1].span.lo)
    );
    let ordinary = wake_ecma_parser::parse(source, &interner, SourceType::TypeScript);
    assert_eq!(
        ordinary.module.structure_hash(),
        output.parsed.module.structure_hash()
    );
    assert_eq!(
        ordinary.module.with_ast(|program| format!("{program:?}")),
        output
            .parsed
            .module
            .with_ast(|program| format!("{program:?}"))
    );
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", output.parsed.dependencies)
    );
}

#[test]
fn value_bindings_retain_their_original_declaration_kinds() {
    let source = r#"declare var ambientVar: number;
declare let ambientLet: number;
declare const ambientConst: number;
declare function ambientFunction(): void;
declare class AmbientClass {}
using resource = acquire();"#;
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
    let kind = |name: &str| {
        output
            .identifiers
            .iter()
            .find(|identifier| {
                identifier.role == SourceIdentifierRole::ValueBinding && identifier.name == name
            })
            .and_then(|identifier| identifier.value_kind)
    };
    assert_eq!(kind("ambientVar"), Some(SourceValueBindingKind::Var));
    assert_eq!(kind("ambientLet"), Some(SourceValueBindingKind::Let));
    assert_eq!(kind("ambientConst"), Some(SourceValueBindingKind::Const));
    assert_eq!(
        kind("ambientFunction"),
        Some(SourceValueBindingKind::Function)
    );
    assert_eq!(kind("AmbientClass"), Some(SourceValueBindingKind::Class));
    assert_eq!(kind("resource"), Some(SourceValueBindingKind::Using));
}

#[test]
fn enum_member_names_are_source_scoped_and_string_members_are_not_identifiers() {
    let source = r#"enum State { "ready" = 1, Pending = 2 }"#;
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
    let members: Vec<_> = output
        .identifiers
        .iter()
        .filter(|id| id.role == SourceIdentifierRole::EnumMemberBinding)
        .map(|id| id.name.as_str())
        .collect();
    assert_eq!(members, ["Pending"]);
    assert!(
        !output
            .identifiers
            .iter()
            .any(|id| id.role == SourceIdentifierRole::ValueBinding && id.name == "Pending")
    );
}
