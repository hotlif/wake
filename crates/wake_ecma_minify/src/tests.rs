use std::sync::Arc;

use wake_common::{Interner, Span};
use wake_ecma_ast::SourceType;
use wake_ecma_parser::parse;

use crate::{
    ConstVal, FINAL_PASS_ORDER, FIXED_POINT_PASS_ORDER, LinkerExportLiveness, LinkerExportStar,
    MinifyDiagnosticKind, NodeOrigin, ONE_TIME_PASS_ORDER, OptimizationPass, OptimizeDependency,
    OptimizeInput, PIPELINE_VERSION, TrustedExpression, TrustedExpressionEdit, ValidatedDefine,
    optimize, optimize_one_shot,
};

fn parsed(source: &str, interner: &Interner) -> wake_ecma_parser::ParseOutput {
    let parsed = parse(source, interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    parsed
}

#[test]
fn public_optimize_returns_only_valid_owned_typed_state() {
    let source = "const answer=40+2;console.log(answer);";
    let interner = Interner::new();
    let parsed = parsed(source, &interner);
    let optimized = optimize(
        Arc::clone(&parsed.module),
        &interner,
        &OptimizeInput::new(source),
    )
    .expect("owned optimization");

    optimized
        .typed_program()
        .validate()
        .expect("valid typed IR");
    assert_eq!(
        Some(optimized.typed_program().revision()),
        optimized.typed_module_plan().sealed_revision()
    );
    assert!(optimized.minify());
    assert!(optimized.stats().iterations > 0);
    assert_eq!(optimized.stats(), &optimized.typed_report().stats);
}

#[test]
fn pass_order_is_an_explicit_stable_contract() {
    assert_eq!(PIPELINE_VERSION, "wake-closure-minifier-v15");
    assert_eq!(
        ONE_TIME_PASS_ORDER,
        &[
            OptimizationPass::ApplyTrustedEdits,
            OptimizationPass::BuildSemanticModel,
        ]
    );
    assert_eq!(FIXED_POINT_PASS_ORDER.len(), 7);
    assert_eq!(
        FIXED_POINT_PASS_ORDER.last(),
        Some(&OptimizationPass::LatePeephole)
    );
    assert_eq!(
        FINAL_PASS_ORDER,
        &[
            OptimizationPass::MangleProperties,
            OptimizationPass::ReuseVariableSlots,
            OptimizationPass::MangleIdentifiers,
        ]
    );
}

#[test]
fn parser_owner_source_and_interner_identity_are_mandatory() {
    let source = "export const value=1;";
    let interner = Interner::new();
    let parsed = parsed(source, &interner);

    let wrong_source = OptimizeInput::new("export const value=2;");
    let error = optimize(Arc::clone(&parsed.module), &interner, &wrong_source).unwrap_err();
    assert_eq!(error.kind, MinifyDiagnosticKind::OptimizerInputMismatch);

    let other_interner = Interner::new();
    let error = optimize(parsed.module, &other_interner, &OptimizeInput::new(source)).unwrap_err();
    assert_eq!(error.kind, MinifyDiagnosticKind::OptimizerInputMismatch);
}

#[test]
fn structured_defines_and_expression_edits_enter_owned_ir() {
    let source = "const value=PLACEHOLDER;console.log(value,FLAG);";
    let interner = Interner::new();
    let parsed_source = parsed(source, &interner);

    let replacement_interner = Interner::new();
    let replacement = parsed("1+2", &replacement_interner);
    let start = source.find("PLACEHOLDER").unwrap() as u32;
    let mut input = OptimizeInput::new(source);
    input.defines = vec![ValidatedDefine::primitive("FLAG", ConstVal::Bool(false))];
    input.add_expression_edit(TrustedExpressionEdit::from_parsed_program(
        Span::new(start, start + "PLACEHOLDER".len() as u32),
        &replacement.module,
        &replacement_interner,
    ));

    let optimized = optimize(parsed_source.module, &interner, &input).expect("structured edits");
    optimized.typed_program().validate().unwrap();
    assert!(
        optimized
            .stats()
            .pass(OptimizationPass::ApplyTrustedEdits)
            .changes
            >= 2
    );
}

#[test]
fn invalid_trusted_expression_is_a_build_diagnostic() {
    let source = "console.log(VALUE);";
    let interner = Interner::new();
    let parsed = parsed(source, &interner);
    let bad_interner = Interner::new();
    let bad = parse("1+", &bad_interner, SourceType::Module);
    let start = source.find("VALUE").unwrap() as u32;
    let mut input = OptimizeInput::new(source);
    input.add_expression_edit(TrustedExpressionEdit::from_parsed_program(
        Span::new(start, start + 5),
        &bad.module,
        &bad_interner,
    ));

    let error = optimize(parsed.module, &interner, &input).unwrap_err();
    assert_eq!(error.kind, MinifyDiagnosticKind::InvalidTrustedEdit);
    assert_eq!(error.pass, Some(OptimizationPass::ApplyTrustedEdits));
}

#[test]
fn unsupported_complex_decorator_lowering_reports_module_and_pass() {
    let source = concat!(
        "function dec(value){return value}",
        "class C extends Base{@dec field=1;constructor(flag){flag?super(1):super(2)}}"
    );
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::TypeScript);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let mut input = OptimizeInput::new(source);
    input.module_name = Some("src/complex-super.ts".into());

    let error = optimize(parsed.module, &interner, &input).unwrap_err();
    assert_eq!(
        error.kind,
        MinifyDiagnosticKind::UnsupportedTransform,
        "unsupported lowering must be classified independently from trusted-edit failures"
    );
    assert_eq!(error.module_name.as_deref(), Some("src/complex-super.ts"));
    assert_eq!(error.pass, Some(OptimizationPass::ApplyTrustedEdits));
    assert!(
        error
            .message
            .contains("derived constructor requires expression-position super initialization")
    );
    let display = error.to_string();
    assert!(display.contains("src/complex-super.ts"), "{display}");
    assert!(display.contains("apply-trusted-edits"), "{display}");
}

#[test]
fn linker_liveness_accepts_public_names_and_scopes_the_module_id() {
    let source =
        "const value=1;{const value=2;void value}export {value as answer};export const dead=3;";
    let interner = Interner::new();
    let parsed = parsed(source, &interner);
    let mut input = OptimizeInput::new(source);
    input.linker_liveness = Some(LinkerExportLiveness::new(19, ["answer"]));

    let optimized = optimize(parsed.module, &interner, &input).expect("name liveness");
    assert_eq!(optimized.linker_module_id(), Some(19));
}

#[test]
fn sealed_module_plan_drives_retained_dependency_edges() {
    let source = "FLAG?import('live'):import('dead');";
    let interner = Interner::new();
    let parsed = parsed(source, &interner);
    assert_eq!(parsed.dependencies.len(), 2);
    let dependencies = parsed
        .dependencies
        .iter()
        .map(|dependency| OptimizeDependency {
            specifier: interner.resolve(dependency.specifier),
            kind: dependency.kind.into(),
            origin: NodeOrigin::Source(dependency.span),
        })
        .collect::<Vec<_>>();
    let mut input = OptimizeInput::new(source);
    input.defines = vec![ValidatedDefine::primitive("FLAG", ConstVal::Bool(true))];
    input.dependencies = dependencies;

    let optimized = optimize(parsed.module, &interner, &input).expect("dependency filtering");
    assert_eq!(optimized.retained_dependencies().len(), 1);
    assert_eq!(optimized.retained_dependencies()[0].specifier, "live");
}

#[test]
fn fingerprints_are_deterministic_and_cover_policy_inputs() {
    let source = "const value=1;console.log(value);";
    let interner = Interner::new();
    let first = parsed(source, &interner);
    let second = parsed(source, &interner);
    let baseline = optimize(first.module, &interner, &OptimizeInput::new(source)).unwrap();
    let repeated = optimize(second.module, &interner, &OptimizeInput::new(source)).unwrap();
    assert_eq!(baseline.fingerprint(), repeated.fingerprint());

    let changed = parsed(source, &interner);
    let mut input = OptimizeInput::new(source);
    input.drop_console = true;
    let changed = optimize(changed.module, &interner, &input).unwrap();
    assert_ne!(baseline.fingerprint(), changed.fingerprint());

    let liveness_changed = parsed(source, &interner);
    let mut input = OptimizeInput::new(source);
    input.linker_liveness = Some(LinkerExportLiveness::new(0, Vec::<String>::new()));
    let liveness_changed = optimize(liveness_changed.module, &interner, &input).unwrap();
    assert_ne!(
        baseline.fingerprint(),
        liveness_changed.fingerprint(),
        "absent linker liveness and an authoritative empty root set are different cache inputs"
    );

    let export_source = "const value=1;export {value as answer};";
    let retained_only = parsed(export_source, &interner);
    let mut retained_input = OptimizeInput::new(export_source);
    retained_input.linker_liveness = Some(LinkerExportLiveness::from_parts(
        0,
        ["answer"],
        Vec::<String>::new(),
    ));
    let retained_only = optimize(retained_only.module, &interner, &retained_input).unwrap();

    let publicly_observed = parsed(export_source, &interner);
    let mut observed_input = OptimizeInput::new(export_source);
    observed_input.linker_liveness =
        Some(LinkerExportLiveness::from_parts(0, ["answer"], ["answer"]));
    let publicly_observed = optimize(publicly_observed.module, &interner, &observed_input).unwrap();
    assert_ne!(
        retained_only.fingerprint(),
        publicly_observed.fingerprint(),
        "public observation is distinct from local binding retention in cache identity"
    );

    let define_source = "globalThis.value=FLAG;";
    let string_define = parsed(define_source, &interner);
    let mut string_input = OptimizeInput::new(define_source);
    string_input.defines = vec![ValidatedDefine::primitive(
        "FLAG",
        ConstVal::Str("null".into()),
    )];
    let string_define = optimize(string_define.module, &interner, &string_input).unwrap();

    let null_define = parsed(define_source, &interner);
    let mut null_input = OptimizeInput::new(define_source);
    null_input.defines = vec![ValidatedDefine::primitive("FLAG", ConstVal::Null)];
    let null_define = optimize(null_define.module, &interner, &null_input).unwrap();
    assert_ne!(
        string_define.fingerprint(),
        null_define.fingerprint(),
        "define value type is part of the stable cache fingerprint"
    );
}

#[test]
fn fingerprints_include_the_linker_export_star_plan() {
    let source = "export * from 'dep';";
    let interner = Interner::new();
    let runtime = parsed(source, &interner);
    let mut runtime_input = OptimizeInput::new(source);
    runtime_input.set_bundled_commonjs(true);
    let runtime = optimize(runtime.module, &interner, &runtime_input).unwrap();

    let exact = parsed(source, &interner);
    let mut exact_input = OptimizeInput::new(source);
    exact_input.set_bundled_commonjs(true);
    exact_input.set_linker_export_stars([LinkerExportStar::exact("dep", vec!["value".to_owned()])]);
    let exact = optimize(exact.module, &interner, &exact_input).unwrap();

    assert_ne!(runtime.fingerprint(), exact.fingerprint());
}

#[test]
fn one_shot_optimization_omits_only_the_unconsumed_stable_fingerprint() {
    let source = "export function dead(){return 1}globalThis.registry=1;";
    let interner = Interner::new();
    let regular_owner = parsed(source, &interner);
    let transient_owner = parsed(source, &interner);
    let mut input = OptimizeInput::new(source);
    input.minify = true;
    input.set_bundled_commonjs(true);
    input.linker_liveness = Some(LinkerExportLiveness::new(7, Vec::<String>::new()));

    let regular = optimize(regular_owner.module, &interner, &input).unwrap();
    let transient = optimize_one_shot(transient_owner.module, &interner, &input).unwrap();

    assert_ne!(regular.fingerprint(), 0);
    assert_eq!(transient.fingerprint(), 0);
    assert_eq!(
        regular.typed_program().fingerprint(),
        transient.typed_program().fingerprint()
    );
    assert_eq!(regular.typed_module_plan(), transient.typed_module_plan());
    assert_eq!(regular.typed_report(), transient.typed_report());
    assert_eq!(
        regular.retained_dependencies(),
        transient.retained_dependencies()
    );
}

#[test]
fn direct_eval_is_reported_as_a_local_dynamic_scope_fact() {
    let source = "function outer(){let secret=1;eval('secret');return secret}outer();";
    let interner = Interner::new();
    let parsed = parsed(source, &interner);
    let optimized = optimize(parsed.module, &interner, &OptimizeInput::new(source)).unwrap();
    assert!(optimized.has_dynamic_scope_hazard());
}

#[test]
fn trusted_expression_owner_is_parser_and_interner_bound() {
    let source = "({mode:'test'})";
    let interner = Interner::new();
    let parsed = parsed(source, &interner);
    let expression = TrustedExpression::from_parsed_program(&parsed.module, &interner);
    assert!(expression.is_valid());
    assert!(expression.owner().is_some());

    let wrong_interner = Interner::new();
    let expression = TrustedExpression::from_parsed_program(&parsed.module, &wrong_interner);
    assert!(!expression.is_valid());
    assert!(expression.owner().is_none());
}
