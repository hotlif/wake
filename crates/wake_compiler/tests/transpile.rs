use std::process::Command;
use std::sync::Arc;

use wake_compiler::{
    AutomaticJsxOptions, Language, ModuleOutput, ModuleRequestKind, ModuleRequestOrigin,
    SourceMapMode, SourceText, TranspileErrorKind, TranspileOptions, transpile_module,
};

#[test]
fn typescript_react_jsx_transpiles_to_owned_esm_with_detached_map() {
    let source = "export const View = (name: string) => <section>{name}</section>;";
    let options = TranspileOptions::new(Language::TypeScript)
        .with_jsx(AutomaticJsxOptions::production())
        .with_source_map(SourceMapMode::Detached);

    let output = transpile_module(SourceText::new("src/view.tsx", source), &options)
        .expect("TSX should transpile");

    assert!(
        output.code().contains("react/jsx-runtime"),
        "{}",
        output.code()
    );
    assert!(!output.code().contains(": string"), "{}", output.code());
    assert!(!output.code().contains("sourceMappingURL"));
    let map: serde_json::Value =
        serde_json::from_str(output.source_map().expect("detached map").json())
            .expect("valid source-map JSON");
    assert_eq!(map["version"], 3);
    assert_eq!(map["sources"][0], "src/view.tsx");
    assert_eq!(map["sourcesContent"][0], source);
    assert!(output.module_requests().iter().any(|request| {
        request.specifier() == "react/jsx-runtime"
            && request.kind() == ModuleRequestKind::Import
            && matches!(request.origin(), ModuleRequestOrigin::Synthetic)
    }));
}

#[test]
fn source_map_collection_never_changes_javascript_bytes() {
    let source = "export const View = () => <><i /><b /></>;";
    let base =
        TranspileOptions::new(Language::JavaScript).with_jsx(AutomaticJsxOptions::production());
    let plain =
        transpile_module(SourceText::new("view.jsx", source), &base).expect("plain transpile");
    let mapped = transpile_module(
        SourceText::new("view.jsx", source),
        &base.clone().with_source_map(SourceMapMode::Detached),
    )
    .expect("mapped transpile");

    assert_eq!(plain.code(), mapped.code());
    assert!(plain.source_map().is_none());
    assert!(mapped.source_map().is_some());
}

#[test]
fn commonjs_output_lowers_static_modules_without_minifying() {
    let source = "import { value } from './dep.js'; export const answer = value + 1;";
    let options = TranspileOptions::new(Language::JavaScript)
        .with_module_output(ModuleOutput::CommonJs)
        .with_source_map(SourceMapMode::Detached);
    let output = transpile_module(SourceText::new("src/answer.js", source), &options)
        .expect("supported CommonJS transform");

    assert!(output.code().contains("require"), "{}", output.code());
    assert!(output.code().contains("answer"), "{}", output.code());
    assert!(!output.code().contains("export const"), "{}", output.code());
    assert!(
        output.code().contains(" + 1"),
        "transpile must stay readable: {}",
        output.code()
    );
    assert!(
        output.code().contains("const __wake_namespace_0"),
        "non-minified output keeps descriptive compiler bindings: {}",
        output.code()
    );
    assert_eq!(output.module_requests().len(), 1);
    assert_eq!(output.module_requests()[0].specifier(), "./dep.js");
}

#[test]
fn commonjs_preserves_explicit_import_export_and_reexport_runtime_semantics() {
    let source = "import base, { value } from './dep.js';\nexport { other as forwarded } from './other.js';\nexport const answer = base + value;\nexport default answer + 1;";
    let options =
        TranspileOptions::new(Language::JavaScript).with_module_output(ModuleOutput::CommonJs);
    let output = transpile_module(SourceText::new("entry.js", source), &options)
        .expect("explicit module syntax has a standalone CommonJS lowering");

    assert_eq!(
        output
            .module_requests()
            .iter()
            .map(|request| (request.specifier(), request.kind()))
            .collect::<Vec<_>>(),
        vec![
            ("./dep.js", ModuleRequestKind::Import),
            ("./other.js", ModuleRequestKind::ExportFrom),
        ]
    );

    if Command::new("node").arg("--version").output().is_err() {
        return;
    }
    let script = format!(
        "module.exports = {{}}; exports = module.exports;\nrequire = id => id === './dep.js' ? {{ __esModule: true, default: 2, value: 40 }} : {{ other: 7 }};\n{}\nprocess.stdout.write(JSON.stringify([exports.answer, exports.default, exports.forwarded]));",
        output.code()
    );
    let executed = Command::new("node")
        .args(["-e", &script])
        .output()
        .expect("node should execute generated CommonJS");
    assert!(
        executed.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&executed.stdout),
        String::from_utf8_lossy(&executed.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&executed.stdout),
        "[42,43,7]",
        "{}",
        output.code()
    );
}

#[test]
fn commonjs_tracks_raw_require_and_dynamic_import_without_conflating_kinds() {
    let source = "const eager = require('./eager.cjs'); export const lazy = () => import('./lazy.js'); export { eager };";
    let options =
        TranspileOptions::new(Language::JavaScript).with_module_output(ModuleOutput::CommonJs);
    let output = transpile_module(SourceText::new("entry.js", source), &options)
        .expect("raw require and dynamic import are representable");

    assert!(
        output.code().contains("require(\"./eager.cjs\")"),
        "{}",
        output.code()
    );
    assert!(
        output.code().contains("import(\"./lazy.js\")"),
        "{}",
        output.code()
    );
    assert_eq!(
        output
            .module_requests()
            .iter()
            .map(|request| (request.specifier(), request.kind()))
            .collect::<Vec<_>>(),
        vec![
            ("./eager.cjs", ModuleRequestKind::Require),
            ("./lazy.js", ModuleRequestKind::DynamicImport),
        ]
    );
}

#[test]
fn commonjs_converts_the_synthetic_react_runtime_import() {
    let source = "export const view = <><i />{2}</>;";
    let options = TranspileOptions::new(Language::JavaScript)
        .with_jsx(AutomaticJsxOptions::production())
        .with_module_output(ModuleOutput::CommonJs);
    let output = transpile_module(SourceText::new("view.jsx", source), &options)
        .expect("React automatic runtime should lower with the module");

    assert!(
        !output.code().contains("from \"react/jsx-runtime\""),
        "{}",
        output.code()
    );
    assert!(
        output.code().contains("require(\"react/jsx-runtime\")"),
        "{}",
        output.code()
    );
    assert!(output.module_requests().iter().any(|request| {
        request.specifier() == "react/jsx-runtime"
            && request.kind() == ModuleRequestKind::Import
            && matches!(request.origin(), ModuleRequestOrigin::Synthetic)
    }));
}

#[test]
fn react_runtime_imports_only_helpers_used_by_the_lowered_module() {
    let production =
        TranspileOptions::new(Language::JavaScript).with_jsx(AutomaticJsxOptions::production());
    let single = transpile_module(
        SourceText::new("single.jsx", "export const view = <i />;"),
        &production,
    )
    .expect("single production element");
    assert!(single.code().contains("jsx as"), "{}", single.code());
    assert!(!single.code().contains("jsxs as"), "{}", single.code());
    assert!(!single.code().contains("Fragment as"), "{}", single.code());
    assert_eq!(single.module_requests().len(), 1);
    assert!(matches!(
        single.module_requests()[0].origin(),
        ModuleRequestOrigin::Synthetic
    ));

    let multiple = transpile_module(
        SourceText::new(
            "multiple.jsx",
            "const a=1,b=2; export const view = <i>{a}{b}</i>;",
        ),
        &production,
    )
    .expect("multiple production children");
    assert!(!multiple.code().contains("jsx as"), "{}", multiple.code());
    assert!(multiple.code().contains("jsxs as"), "{}", multiple.code());
    assert!(
        !multiple.code().contains("Fragment as"),
        "{}",
        multiple.code()
    );

    let development = transpile_module(
        SourceText::new("dev.jsx", "export const view = <i />;"),
        &TranspileOptions::new(Language::JavaScript).with_jsx(AutomaticJsxOptions::development()),
    )
    .expect("single development element");
    assert!(
        development.code().contains("jsxDEV as"),
        "{}",
        development.code()
    );
    assert!(
        !development.code().contains("Fragment as"),
        "{}",
        development.code()
    );

    let commonjs = transpile_module(
        SourceText::new("single.jsx", "export const view = <i />;"),
        &production
            .clone()
            .with_module_output(ModuleOutput::CommonJs),
    )
    .expect("single CommonJS React module");
    assert!(
        commonjs.code().contains("require(\"react/jsx-runtime\")"),
        "{}",
        commonjs.code()
    );
    assert!(!commonjs.code().contains("jsxs"), "{}", commonjs.code());
    assert!(!commonjs.code().contains("Fragment"), "{}", commonjs.code());
    assert_eq!(commonjs.module_requests().len(), 1);
}

#[test]
fn commonjs_fails_closed_for_graph_or_runtime_owned_module_semantics() {
    let options =
        TranspileOptions::new(Language::JavaScript).with_module_output(ModuleOutput::CommonJs);
    for (source, feature) in [
        ("export const value = await load();", "top-level await"),
        ("export const url = import.meta.url;", "import.meta"),
        (
            "import data from './data.json' with { type: 'json' }; export { data };",
            "import attributes",
        ),
        (
            "export const load = () => import('./data.json', { with: { type: 'json' } });",
            "import attributes",
        ),
        (
            "export * from './value.js'; export { VisualElement } from './value.js';",
            "export *",
        ),
    ] {
        let error = transpile_module(SourceText::new("unsupported.js", source), &options)
            .expect_err(feature);
        assert_eq!(error.kind(), TranspileErrorKind::UnsupportedTransform);
        assert!(error.to_string().contains(feature), "{error}");
    }
}

#[test]
fn unsupported_syntax_lowering_has_a_stable_public_error_kind() {
    let source = concat!(
        "function dec(value){return value}",
        "class C extends Base{@dec field=1;constructor(flag){flag?super(1):super(2)}}"
    );
    let error = transpile_module(
        SourceText::new("src/complex-super.ts", source),
        &TranspileOptions::new(Language::TypeScript),
    )
    .expect_err("unsupported decorator semantics must fail closed");

    assert_eq!(error.kind(), TranspileErrorKind::UnsupportedTransform);
    assert!(
        error
            .to_string()
            .contains("derived constructor requires expression-position super initialization"),
        "{error}"
    );
}

#[test]
fn syntax_errors_fail_closed_and_never_return_partial_output() {
    let options = TranspileOptions::new(Language::JavaScript);
    let error = transpile_module(SourceText::new("broken.js", "export const = ;"), &options)
        .expect_err("invalid syntax");

    assert_eq!(error.kind(), TranspileErrorKind::Syntax);
    assert!(!error.diagnostics().is_empty());
    assert!(
        error
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.path() == Some("broken.js"))
    );
}

#[test]
fn output_is_owned_and_concurrent_calls_are_deterministic() {
    let source = String::from("export const View = () => <div />;");
    let options = Arc::new(
        TranspileOptions::new(Language::JavaScript)
            .with_jsx(AutomaticJsxOptions::development())
            .with_source_map(SourceMapMode::Detached),
    );
    let expected = transpile_module(SourceText::new("view.jsx", &source), &options)
        .expect("reference transpile")
        .code()
        .to_owned();
    let workers = (0..8)
        .map(|_| {
            let options = Arc::clone(&options);
            std::thread::spawn(move || {
                let local = String::from("export const View = () => <div />;");
                let output = transpile_module(SourceText::new("view.jsx", &local), &options)
                    .expect("parallel transpile");
                (
                    output.code().to_owned(),
                    output.source_map().unwrap().json().to_owned(),
                )
            })
        })
        .collect::<Vec<_>>();
    drop(source);

    for worker in workers {
        let (code, map) = worker.join().expect("worker");
        assert_eq!(code, expected);
        assert!(map.contains("view.jsx"));
    }
}
