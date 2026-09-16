use wake_lint_core::{LintOptions, LintResult, SourceType, lint_text};

fn check(source: &str, separate: bool) -> LintResult {
    let options = serde_json::from_value::<LintOptions>(serde_json::json!({ "recommended":false, "rules": {
        "js/no-duplicate-imports": {"level":"error", "options":{"allow_separate_type_imports":separate}}
    }})).unwrap();
    let result = lint_text(source, SourceType::TypeScript, &options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{source}: {:?}",
        result.parse_diagnostics
    );
    result
}

#[test]
fn duplicate_static_imports_use_decoded_module_and_attribute_identity() {
    let source = r#"import First from './mod'; import { second } from './m\u006fd'; import './mod';
import json from './data' with { type:'json', mode:'strict' };
import other from './data' with { mode:'strict', type:'json' };
import text from './data' with { type:'text' };
const dynamic = import('./mod'); const literal = "import X from './mod'";"#;
    let result = check(source, false);
    let ranges = result
        .diagnostics
        .iter()
        .map(|diagnostic| &source[diagnostic.start as usize..diagnostic.end as usize])
        .collect::<Vec<_>>();
    assert_eq!(ranges, ["'./m\\u006fd'", "'./mod'", "'./data'"]);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.message_id == "duplicate" && diagnostic.fix.is_none())
    );
}

#[test]
fn duplicate_imports_preserve_type_spaces_ambient_owners_and_equals_imports() {
    let source = "import Value from 'pkg'; import type Shape from 'pkg'; import { type Other } from 'pkg'; import { Next } from 'pkg'; import Equals = require('pkg'); declare module 'ambient' { import { Value } from 'pkg'; }";
    assert_eq!(check(source, false).diagnostics.len(), 3);
    assert_eq!(check(source, true).diagnostics.len(), 2);
    assert!(check("import type Shape from 'pkg'; import Value from 'pkg'; import Equals = require('pkg');", true).diagnostics.is_empty());
    assert!(check("import Value from 'pkg'; // wake-lint-disable-next-line js/no-duplicate-imports\nimport Other from 'pkg';", false).diagnostics.is_empty());
}
