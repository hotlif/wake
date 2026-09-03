use std::sync::Arc;

use wake_compiler_core::{
    CompilerBackend, CompilerEmission, CompilerError, LifetimeMode, MapMode, ModuleFinalizeFacts,
    ModuleRequestKind, OptimizeLinkFacts, OptimizeOptions, OptimizedModule, ParseInput,
    ParsedModule, SourceType, TransformEdits,
};

type OptimizeModuleFn = fn(
    &CompilerBackend,
    &ParsedModule,
    &OptimizeOptions,
    &OptimizeLinkFacts,
    &TransformEdits,
    LifetimeMode,
) -> Result<Arc<OptimizedModule>, CompilerError>;

fn optimize(
    backend: &CompilerBackend,
    source: &str,
    options: &OptimizeOptions,
    lifetime: LifetimeMode,
) -> Arc<OptimizedModule> {
    let parsed = backend
        .parse_module(ParseInput::new(source, SourceType::Module))
        .expect("parse module");
    assert!(
        !parsed.has_errors(),
        "parse diagnostics: {:?}",
        parsed.diagnostics()
    );
    backend
        .optimize_module(
            &parsed,
            options,
            &OptimizeLinkFacts::default(),
            &TransformEdits::default(),
            lifetime,
        )
        .expect("optimize")
}

#[test]
fn compiler_backend_exposes_only_the_canonical_three_stage_contract() {
    let _: for<'source> fn(
        &CompilerBackend,
        ParseInput<'source>,
    ) -> Result<ParsedModule, CompilerError> = CompilerBackend::parse_module;
    let _: OptimizeModuleFn = CompilerBackend::optimize_module;
    let _: fn(
        &CompilerBackend,
        &OptimizedModule,
        &ModuleFinalizeFacts,
        MapMode,
    ) -> Result<CompilerEmission, CompilerError> = CompilerBackend::emit_module;
}

#[test]
fn parse_exposes_owned_module_syntax_facts() {
    let backend = CompilerBackend::new();
    let parsed = backend
        .parse_module(ParseInput::new(
            "import data from './data.json' with { type: 'json' };\n\
         export * from './dep.js';\n\
         export const url = import.meta.url;\n\
         export const value = await load();",
            SourceType::Module,
        ))
        .expect("parse module");
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics());

    let syntax = parsed.syntax();
    assert!(syntax.has_top_level_await(), "top-level await");
    assert!(syntax.has_import_meta(), "import.meta");
    assert!(syntax.has_import_attributes(), "import attributes");
    assert!(syntax.has_export_star(), "export *");
}

#[test]
fn preserve_esm_rewrites_requests_and_returns_module_mappings() {
    let backend = CompilerBackend::new();
    let source = "export { value } from './dep.js';";
    let optimized = optimize(
        &backend,
        source,
        &OptimizeOptions::preserve_esm(),
        LifetimeMode::Retained,
    );
    let mut facts = ModuleFinalizeFacts::default();
    facts.rewrite_request("./dep.js", ModuleRequestKind::StaticImport, "./dep.mjs");

    let mapped = backend
        .emit_module(&optimized, &facts, MapMode::SourceMap)
        .expect("mapped ESM emit");
    let plain = backend
        .emit_module(&optimized, &facts, MapMode::None)
        .expect("plain ESM emit");

    assert_eq!(mapped.code(), plain.code());
    assert!(mapped.code().contains("./dep.mjs"), "{}", mapped.code());
    assert!(mapped.code().contains("export"), "{}", mapped.code());
    assert!(mapped.mappings().is_some_and(|map| !map.is_empty()));
    let source_map = mapped
        .source_map_json("src/entry.js", source)
        .expect("detached source map");
    assert!(source_map.contains("\"version\":3"), "{source_map}");
    assert!(
        source_map.contains("\"sources\":[\"src/entry.js\"]"),
        "{source_map}"
    );
    assert!(source_map.contains("\"sourcesContent\""), "{source_map}");
    assert!(plain.mappings().is_none());
    assert!(plain.source_map_json("src/entry.js", source).is_none());
    assert!(mapped.generated_module_requests().is_empty());
    assert!(mapped.runtime_names().is_none());
}

#[test]
fn preserve_commonjs_uses_the_final_kind_specific_request() {
    let backend = CompilerBackend::new();
    let source = "import { value } from './dep.js'; export const answer = value;";
    let optimized = optimize(
        &backend,
        source,
        &OptimizeOptions::preserve_commonjs(),
        LifetimeMode::Retained,
    );
    let mut facts = ModuleFinalizeFacts::default();
    facts.rewrite_request("./dep.js", ModuleRequestKind::StaticImport, "./dep.cjs");
    facts.set_lower_external_dynamic_to_require(true);

    let emitted = backend
        .emit_module(&optimized, &facts, MapMode::SourceMap)
        .expect("CommonJS emit");

    assert!(emitted.code().contains("require"), "{}", emitted.code());
    assert!(emitted.code().contains("./dep.cjs"), "{}", emitted.code());
    assert!(
        !emitted.code().contains("export const"),
        "{}",
        emitted.code()
    );
    assert!(emitted.mappings().is_some_and(|map| !map.is_empty()));
}

#[test]
fn bundled_emit_uses_owned_final_link_facts_and_reports_proven_ranges() {
    let backend = CompilerBackend::new();
    let source = "import { value } from './dep.js'; console.log(value);";
    let parsed = backend
        .parse_module(ParseInput::new(source, SourceType::Module))
        .expect("parse module");
    assert!(!parsed.has_errors());
    assert_eq!(parsed.dependencies().len(), 1);
    assert_eq!(parsed.dependencies()[0].specifier(), "./dep.js");

    let mut link = OptimizeLinkFacts::default();
    link.add_internal_esm_request("./dep.js", ModuleRequestKind::StaticImport);
    let optimized = backend
        .optimize_module(
            &parsed,
            &OptimizeOptions::bundled_commonjs(),
            &link,
            &TransformEdits::default(),
            LifetimeMode::Retained,
        )
        .expect("bundled optimize");
    assert_eq!(
        optimized.retained_requests(),
        &[wake_compiler_core::ModuleRequest::new(
            "./dep.js",
            ModuleRequestKind::StaticImport
        )]
    );

    let mut facts = ModuleFinalizeFacts::default();
    facts.resolve_internal("./dep.js", ModuleRequestKind::StaticImport, 7, false, None);
    let emitted = backend
        .emit_module(&optimized, &facts, MapMode::SourceMap)
        .expect("bundled emit");

    assert!(emitted.code().contains("7"), "{}", emitted.code());
    assert_eq!(emitted.generated_module_requests().len(), 1);
    assert_eq!(emitted.generated_module_requests()[0].target_module_id, 7);
    assert!(emitted.runtime_names().is_some());
}

#[test]
fn one_shot_changes_only_cross_generation_fingerprint_work() {
    let backend = CompilerBackend::new();
    let source = "export const answer = 40 + 2;";
    let options = OptimizeOptions::preserve_esm();
    let retained = optimize(&backend, source, &options, LifetimeMode::Retained);
    let transient = optimize(&backend, source, &options, LifetimeMode::OneShot);

    assert_ne!(retained.fingerprint(), 0);
    assert_eq!(transient.fingerprint(), 0);
    let facts = ModuleFinalizeFacts::default();
    let retained = backend
        .emit_module(&retained, &facts, MapMode::SourceMap)
        .expect("retained emit");
    let transient = backend
        .emit_module(&transient, &facts, MapMode::SourceMap)
        .expect("one-shot emit");
    assert_eq!(retained, transient);
}
