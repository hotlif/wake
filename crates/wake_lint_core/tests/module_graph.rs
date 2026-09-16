use std::sync::Arc;
use wake_lint_core::{
    LintError, LintOptions, ModuleFile, ModuleGraph, ModuleId, ModuleResolution as Resolution,
    PackageDependency, RuleConfiguration, RuleLevel, RuleSetting, SourceType, lint_text,
    rule_catalog,
};

fn file(identity: &str, path: &str, source: &str) -> ModuleFile {
    ModuleFile::new(
        identity.into(),
        path.into(),
        Arc::from(source),
        SourceType::TypeScript,
    )
    .unwrap()
}

fn link(target: usize) -> Resolution {
    Resolution::File {
        target: ModuleId(target),
        package: None,
    }
}

fn options(rule: &str, values: serde_json::Value) -> LintOptions {
    LintOptions {
        recommended: false,
        rules: [(
            rule.into(),
            RuleSetting::Options(RuleConfiguration {
                level: RuleLevel::Error,
                options: serde_json::from_value(values).unwrap(),
            }),
        )]
        .into(),
        ..Default::default()
    }
}

fn messages(graph: &ModuleGraph, id: usize, rule: &str, values: serde_json::Value) -> Vec<String> {
    graph
        .lint(ModuleId(id), &options(rule, values))
        .unwrap()
        .diagnostics
        .into_iter()
        .map(|d| {
            assert!(d.fix.is_none());
            d.message_id
        })
        .collect()
}

#[test]
fn import_order_preserves_surrogates_during_case_folding_and_code_unit_sorting() {
    fn graph(source: &str) -> ModuleGraph {
        let mut input = file("a", "a.ts", source);
        for index in 0..input.requests().len() {
            input
                .resolve(
                    index,
                    Resolution::Unresolved {
                        reason: "unsupported UTF-16 path".into(),
                        package: None,
                    },
                )
                .unwrap();
        }
        ModuleGraph::new(vec![input]).unwrap()
    }
    let ordered = graph(
        r#"
import type A from 'a\ud800';
import type B from 'B\ud800';
import type C from 'a\ud801';
"#,
    );
    assert_eq!(
        messages(
            &ordered,
            0,
            "import/order",
            serde_json::json!({"alphabetize":"asc","case_insensitive":true})
        ),
        ["alphabetical"]
    );
    let folded = graph(
        r#"
import type A from 'ΟΣ\ud800';
import type B from 'ος\ud800';
"#,
    );
    assert!(
        messages(
            &folded,
            0,
            "import/order",
            serde_json::json!({"alphabetize":"asc","case_insensitive":true})
        )
        .is_empty()
    );
}

#[test]
fn module_rules_require_owned_project_facts_and_have_closed_metadata() {
    let ids = [
        "import/no-unresolved",
        "import/no-cycle",
        "import/no-duplicates",
        "import/no-restricted-paths",
        "import/no-extraneous-dependencies",
        "import/order",
    ];
    for id in ids {
        let metadata = rule_catalog()
            .into_iter()
            .find(|r| r.id == id)
            .expect("module rule registered");
        assert_eq!(metadata.analysis, "module-graph");
        assert_eq!(metadata.default_level, RuleLevel::Off);
        assert!(!metadata.fixable);
        assert!(matches!(
            lint_text("", SourceType::Module, &options(id, serde_json::json!({}))),
            Err(LintError::Analysis(_))
        ));
    }
    let graph = ModuleGraph::new(vec![file("a", "a.ts", "")]).unwrap();
    for (id, value) in [
        ("import/no-unresolved", serde_json::json!({"ignore":["["]})),
        (
            "import/no-restricted-paths",
            serde_json::json!({"zones":[{"from":"x","to":"y","extra":true}]}),
        ),
        (
            "import/no-restricted-paths",
            serde_json::json!({"zones":[{"from":"x","to":"["}]}),
        ),
        (
            "import/order",
            serde_json::json!({"groups":["external","external"]}),
        ),
        (
            "import/no-restricted-paths",
            serde_json::json!({"zones":[{"from":"x","to":"y","message":null}]}),
        ),
    ] {
        let mut config = options(id, value);
        let RuleSetting::Options(rule) = config.rules.get_mut(id).unwrap() else {
            panic!()
        };
        rule.level = RuleLevel::Off;
        assert!(matches!(
            graph.lint(ModuleId(0), &config),
            Err(LintError::Configuration(_))
        ));
    }
}

#[test]
fn graph_freeze_rejects_missing_targets_duplicate_identities_and_invented_dynamic_edges() {
    assert!(ModuleGraph::new(vec![file("a", "a.ts", "import './b';")]).is_err());
    assert!(ModuleGraph::new(vec![file("a", "a.ts", ""), file("a", "other.ts", "")]).is_err());
    let mut invalid = file("a", "a.ts", "import './b';");
    invalid.resolve(0, link(4)).unwrap();
    assert!(ModuleGraph::new(vec![invalid]).is_err());
    let mut dynamic = file("a", "a.ts", "import(value);");
    assert!(dynamic.resolve(0, link(0)).is_err());
    assert!(
        dynamic
            .resolve(5, Resolution::Unknown("dynamic".into()))
            .is_err()
    );
    dynamic
        .resolve(0, Resolution::Unknown("dynamic".into()))
        .unwrap();
    let graph = ModuleGraph::new(vec![dynamic]).unwrap();
    assert!(matches!(
        graph.lint(ModuleId(2), &LintOptions::default()),
        Err(LintError::Analysis(_))
    ));
}

#[test]
fn unresolved_requests_preserve_original_ranges_filter_kinds_and_apply_real_directives() {
    let source = "// wake-lint-disable-next-line import/no-unresolved\nimport './missing';\nimport type T from './type';\nrequire('./cjs'); import(value); import('./dynamic');";
    let mut input = file("a", "a.ts", source);
    for index in 0..input.requests().len() {
        input
            .resolve(
                index,
                if index == 3 {
                    Resolution::Unknown("nonliteral".into())
                } else {
                    Resolution::Unresolved {
                        reason: "not found".into(),
                        package: None,
                    }
                },
            )
            .unwrap();
    }
    let graph = ModuleGraph::new(vec![input]).unwrap();
    assert_eq!(
        messages(&graph, 0, "import/no-unresolved", serde_json::json!({})),
        ["unresolved", "unresolved", "unknown", "unresolved"]
    );
    assert_eq!(
        messages(
            &graph,
            0,
            "import/no-unresolved",
            serde_json::json!({
        "include_types":false,"commonjs":false,"dynamic_imports":false})
        ),
        Vec::<String>::new()
    );
    let result = graph
        .lint(
            ModuleId(0),
            &options(
                "import/no-unresolved",
                serde_json::json!({
        "ignore":["^\\./type$"],"report_unknown":false}),
            ),
        )
        .unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| &source[d.start as usize..d.end as usize])
            .collect::<Vec<_>>(),
        ["'./cjs'", "'./dynamic'"]
    );
}

#[test]
fn cycle_components_distinguish_types_dynamic_commonjs_and_unknown_reachable_nodes() {
    let mut a = file(
        "a",
        "a.ts",
        "import './b'; import type T from './c'; import('./d'); require('./e'); import './f';",
    );
    for (i, target) in [1, 2, 3, 4, 5].into_iter().enumerate() {
        a.resolve(i, link(target)).unwrap();
    }
    let mut nodes = vec![a];
    for (id, source) in [
        ("b", "import './a';"),
        ("c", "import './a';"),
        ("d", "import './a';"),
        ("e", "import './a';"),
        ("f", "import './missing';"),
    ] {
        let mut input = file(id, &format!("{id}.ts"), source);
        input
            .resolve(
                0,
                if id == "f" {
                    Resolution::Unresolved {
                        reason: "missing".into(),
                        package: None,
                    }
                } else {
                    link(0)
                },
            )
            .unwrap();
        nodes.push(input);
    }
    let graph = ModuleGraph::new(nodes).unwrap();
    assert_eq!(
        messages(&graph, 0, "import/no-cycle", serde_json::json!({})),
        ["cycle", "incomplete"]
    );
    assert_eq!(
        messages(
            &graph,
            0,
            "import/no-cycle",
            serde_json::json!({"include_types":true,"dynamic_imports":true,"commonjs":true})
        ),
        ["cycle", "cycle", "cycle", "cycle", "incomplete"]
    );
    let mut parent = file("parent", "parent.ts", "import './bad';");
    parent.resolve(0, link(1)).unwrap();
    let broken = ModuleGraph::new(vec![parent, file("bad", "bad.ts", "const = ;")]).unwrap();
    assert_eq!(
        messages(&broken, 0, "import/no-cycle", serde_json::json!({})),
        ["incomplete"]
    );
    assert!(
        !broken
            .lint(
                ModuleId(1),
                &options("import/no-cycle", serde_json::json!({}))
            )
            .unwrap()
            .parse_diagnostics
            .is_empty()
    );
}

#[test]
fn duplicate_imports_use_full_identity_attributes_types_and_container() {
    let mut a = file(
        "a",
        "a.ts",
        "import A from './b'; import B from '@alias'; import type T from './b'; import C from './b' with {type:'json'}; import D from 'peer'; declare module 'ambient' { import E from './b'; }",
    );
    for (i, target) in [1, 1, 1, 1, 2, 1].into_iter().enumerate() {
        a.resolve(i, link(target)).unwrap();
    }
    let graph = ModuleGraph::new(vec![
        a,
        file("pkg@1/peer-a", "b.ts", ""),
        file("pkg@1/peer-b", "peer.ts", ""),
    ])
    .unwrap();
    assert_eq!(
        messages(&graph, 0, "import/no-duplicates", serde_json::json!({})),
        ["duplicate"]
    );
    assert_eq!(
        messages(
            &graph,
            0,
            "import/no-duplicates",
            serde_json::json!({"separate_type_imports":false})
        ),
        ["duplicate", "duplicate"]
    );
}

#[test]
fn package_declarations_are_issuer_facts_and_external_cycles_can_be_excluded() {
    let mut input = file(
        "a",
        "src/a.ts",
        "import 'prod'; import 'dev/sub'; import 'optional'; import 'peer'; import 'missing'; import 'self'; import './b';",
    );
    for (index, name) in ["prod", "dev", "optional", "peer", "missing", "self"]
        .into_iter()
        .enumerate()
    {
        input
            .resolve(
                index,
                Resolution::File {
                    target: ModuleId(1),
                    package: Some(PackageDependency {
                        name: name.into(),
                        production: index == 0,
                        development: index == 1,
                        optional: index == 2,
                        peer: index == 3,
                        self_reference: index == 5,
                    }),
                },
            )
            .unwrap();
    }
    input.resolve(6, link(1)).unwrap();
    let mut b = file("b", "lib/b.ts", "import '../src/a';");
    b.resolve(0, link(0)).unwrap();
    let graph = ModuleGraph::new(vec![input, b]).unwrap();
    assert_eq!(
        messages(
            &graph,
            0,
            "import/no-extraneous-dependencies",
            serde_json::json!({})
        ),
        ["extraneous", "extraneous"]
    );
    assert_eq!(
        messages(
            &graph,
            0,
            "import/no-extraneous-dependencies",
            serde_json::json!({"dev_dependencies":true,"optional_dependencies":false,"peer_dependencies":false})
        ),
        ["extraneous", "extraneous", "extraneous"]
    );
    assert_eq!(
        messages(
            &graph,
            0,
            "import/no-cycle",
            serde_json::json!({"ignore_external":true})
        ),
        ["cycle", "cycle"] // A self-reference remains an internal dependency.
    );
    assert_eq!(
        messages(
            &graph,
            0,
            "import/no-restricted-paths",
            serde_json::json!({"zones":[{"to":"^src/","from":"^lib/","message":"Layer violation"}]})
        )
        .len(),
        7
    );
    assert!(
        messages(
            &graph,
            0,
            "import/no-restricted-paths",
            serde_json::json!({"zones":[{"to":"^src/","from":"^lib/","except":["/b\\.ts$"]}]})
        )
        .is_empty()
    );
}

#[test]
fn import_order_checks_groups_alphabetization_and_actual_blank_lines_without_fixes() {
    let mut input = file(
        "a",
        "src/a.ts",
        "import z from './z';\nimport a from './a';\n// not a blank line\nimport fs from 'node:fs';\n\nimport type T from './types';",
    );
    input.resolve(0, link(1)).unwrap();
    input.resolve(1, link(2)).unwrap();
    input
        .resolve(2, Resolution::Builtin("node:fs".into()))
        .unwrap();
    input.resolve(3, link(3)).unwrap();
    let graph = ModuleGraph::new(vec![
        input,
        file("z", "src/z.ts", ""),
        file("b", "src/b.ts", ""),
        file("t", "src/types.ts", ""),
    ])
    .unwrap();
    assert_eq!(
        messages(
            &graph,
            0,
            "import/order",
            serde_json::json!({"alphabetize":"asc","newlines":"always"})
        ),
        ["alphabetical", "group", "newline"]
    );
    assert_eq!(
        messages(
            &graph,
            0,
            "import/order",
            serde_json::json!({"newlines":"never"})
        ),
        ["group", "newline"]
    );
}

#[test]
fn deep_module_cycles_are_iterative_and_repeated_files_share_topology() {
    let count = 4_000;
    let mut nodes = Vec::new();
    for index in 0..count {
        let mut input = file(
            &format!("{index}"),
            &format!("{index}.ts"),
            "import './next';",
        );
        input.resolve(0, link((index + 1) % count)).unwrap();
        nodes.push(input);
    }
    let graph = ModuleGraph::new(nodes).unwrap();
    for index in [0, 1, 2, count - 1] {
        assert_eq!(
            messages(&graph, index, "import/no-cycle", serde_json::json!({})),
            ["cycle"]
        );
    }
}

#[test]
fn unresolved_packages_still_carry_issuer_declarations() {
    let mut input = file("a", "a.ts", "import 'absent'; import type T from 'dev';");
    for (index, name) in ["absent", "dev"].into_iter().enumerate() {
        input
            .resolve(
                index,
                Resolution::Unresolved {
                    reason: "not installed".into(),
                    package: Some(PackageDependency {
                        name: name.into(),
                        development: index == 1,
                        ..Default::default()
                    }),
                },
            )
            .unwrap();
    }
    let graph = ModuleGraph::new(vec![input]).unwrap();
    assert_eq!(
        messages(
            &graph,
            0,
            "import/no-extraneous-dependencies",
            serde_json::json!({})
        ),
        ["extraneous", "extraneous"]
    );
    assert_eq!(
        messages(
            &graph,
            0,
            "import/no-extraneous-dependencies",
            serde_json::json!({"include_types":false})
        ),
        ["extraneous"]
    );
    assert_eq!(
        messages(&graph, 0, "import/no-unresolved", serde_json::json!({})),
        ["unresolved", "unresolved"]
    );
}
