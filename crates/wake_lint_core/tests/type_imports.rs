use wake_lint_core::{LintOptions, SourceType, lint_text};

fn options() -> LintOptions {
    serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
        "ts/consistent-type-imports":"error"
    }}))
    .unwrap()
}

#[test]
fn ambient_namespace_members_do_not_count_as_outer_import_usage() {
    let source = "import { Shared } from 'pkg'; declare namespace N { interface Shared {} } declare namespace N { const value: Shared; }";
    let result = lint_text(source, SourceType::TypeScript, &options()).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn ambient_module_unknown_members_do_not_capture_outer_imports() {
    let source = "import { Shared } from 'pkg'; declare module 'augmented' { export interface Added { value: Shared } }";
    let result = lint_text(source, SourceType::TypeScript, &options()).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn type_imports_use_both_semantic_namespaces_without_counting_shadowed_names() {
    let source = r#"import Default, { Shape, Runtime, Generic, Block, Unused, Queried } from 'pkg';
    import * as Types from 'types';
    import type { Existing } from 'existing';
    let value: Default; type Alias = Shape; type Member = Types.Member;
    const runtime = Runtime; type Return = typeof Queried;
    type Fn<Generic> = Generic;
    { interface Block {} let local: Block }
    type Other = Existing;
    const element = <Runtime value={runtime} />;"#;
    let result = lint_text(source, SourceType::Tsx, &options()).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| &source[d.start as usize..d.end as usize])
            .collect::<Vec<_>>(),
        ["Default", "Shape", "Queried", "Types"]
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| d.message_id == "type" && d.fix.is_none())
    );
}

#[test]
fn value_exports_writes_and_unsupported_import_forms_do_not_become_type_imports() {
    let source = r#"import { Exported, Written, TypedExport, Duplicate } from 'pkg';
    import { Attributed } from 'json' with { type: 'json' };
    import Alias = require('alias');
    import 'side-effect';
    type Types = [Exported, Written, Attributed, Alias];
    export { Exported }; Written = replacement;
    export type { TypedExport };
    type Local<Duplicate> = Duplicate;"#;
    let result = lint_text(source, SourceType::TypeScript, &options()).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| &source[d.start as usize..d.end as usize])
            .collect::<Vec<_>>(),
        ["TypedExport"]
    );
}

#[test]
fn type_import_rule_has_closed_options_defaults_language_gate_and_real_suppression() {
    let source = "import { T } from 'pkg'; let t: T;";
    assert!(
        lint_text(source, SourceType::TypeScript, &LintOptions::default())
            .unwrap()
            .diagnostics
            .is_empty()
    );
    let suppressed = "// wake-lint-disable-next-line ts/consistent-type-imports\nimport { T } from 'pkg'; let t: T;";
    assert!(
        lint_text(suppressed, SourceType::TypeScript, &options())
            .unwrap()
            .diagnostics
            .is_empty()
    );
    assert!(
        lint_text(
            "import { T } from 'pkg'; T();",
            SourceType::Module,
            &options()
        )
        .unwrap()
        .diagnostics
        .is_empty()
    );
    let invalid: LintOptions = serde_json::from_value(serde_json::json!({"rules":{
        "ts/consistent-type-imports":{"level":"off","options":{"prefer":"type-imports"}}
    }}))
    .unwrap();
    assert!(lint_text("", SourceType::TypeScript, &invalid).is_err());
}
