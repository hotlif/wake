use wake_common::Interner;
use wake_ecma_parser::{SourceType, parse};
use wake_ecma_semantic::{
    CallExecution, CallExecutionFacts, CallExecutionLimits, ExecutionRegionKind,
    analyze_call_execution, analyze_call_execution_with_limits,
};

fn facts(source: &str) -> CallExecutionFacts {
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Tsx);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    parsed.module.with_ast(analyze_call_execution).unwrap()
}

fn call<'a>(facts: &'a CallExecutionFacts, source: &str, text: &str) -> &'a CallExecution {
    let matching = facts
        .calls
        .iter()
        .filter(|call| &source[call.span.lo as usize..call.span.hi as usize] == text)
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "one owned record for {text}: {matching:?}"
    );
    matching[0]
}

#[test]
fn calls_follow_branches_short_circuit_and_normal_completion_dominance() {
    let source = "function Component(flag){start(); if(flag){branch();} flag && short(); if(flag)return finish(); later();} function Throws(flag){if(flag)throw fail(); always();} function Dead(){return; unreachable();}";
    let facts = facts(source);
    assert!(call(&facts, source, "start()").unconditional);
    for text in ["branch()", "short()", "finish()", "later()"] {
        let call = call(&facts, source, text);
        assert!(call.reachable && call.on_normal_path);
        assert!(!call.unconditional, "{text}");
    }
    assert!(call(&facts, source, "always()").unconditional);
    assert!(!call(&facts, source, "fail()").on_normal_path);
    assert!(!call(&facts, source, "unreachable()").reachable);
}

#[test]
fn optional_chains_and_default_binding_expressions_have_real_conditional_edges() {
    let source = "function Component(obj, value=defaultValue()){obj?.method(optionalArg()); (obj?.method)(ordinaryArg()); obj.call?.(optionalCallArg()); const {a=fallback()}=obj; [value=assignedDefault()]=obj; value ||= assigned();}";
    let facts = facts(source);
    for text in [
        "defaultValue()",
        "optionalArg()",
        "optionalCallArg()",
        "fallback()",
        "assignedDefault()",
        "assigned()",
    ] {
        assert!(!call(&facts, source, text).unconditional, "{text}");
    }
    assert!(call(&facts, source, "ordinaryArg()").unconditional);
    assert!(call(&facts, source, "defaultValue()").in_parameters);
    assert!(!call(&facts, source, "fallback()").in_parameters);
}

#[test]
fn loops_switches_and_labeled_transfers_keep_repetition_separate_from_initialization() {
    let source = "function Component(items){for(init(); test(); update()){body(); if(stop())break;} for(const item of iterable()){each();} do{once();}while(false); outer: while(again()){switch(choice()){case 1: continue outer; default: break outer;} ignored();} after();}";
    let facts = facts(source);
    assert!(call(&facts, source, "init()").unconditional);
    assert!(!call(&facts, source, "init()").inside_loop);
    for text in ["test()", "update()", "body()", "each()"] {
        assert!(call(&facts, source, text).may_repeat, "{text}");
        assert!(call(&facts, source, text).inside_loop);
    }
    assert!(!call(&facts, source, "iterable()").may_repeat);
    assert!(call(&facts, source, "once()").inside_loop);
    assert!(!call(&facts, source, "once()").may_repeat);
    assert!(!call(&facts, source, "ignored()").reachable);
    assert!(call(&facts, source, "after()").on_normal_path);
}

#[test]
fn finally_aggregates_cloned_call_sites_and_preserves_pending_return_and_catch() {
    let source = "function Component(flag){try{if(flag)return returned(); attempt();}catch(error){caught();}finally{cleanup();} after();} function Override(){try{return 1;}finally{throw failed();} never();}";
    let facts = facts(source);
    assert!(call(&facts, source, "cleanup()").unconditional);
    assert!(call(&facts, source, "cleanup()").inside_exception);
    assert!(!call(&facts, source, "after()").unconditional);
    assert!(call(&facts, source, "caught()").reachable);
    assert!(call(&facts, source, "caught()").inside_exception);
    assert!(!call(&facts, source, "failed()").on_normal_path);
    assert!(!call(&facts, source, "never()").reachable);
}

#[test]
fn nested_functions_and_class_initializers_have_independent_regions() {
    let source = "top(); function Component(){outer(); const callback=()=>{inner();}; class C { field=initialize(); static {staticCall();} method(){methodCall();} } tail();}";
    let facts = facts(source);
    assert_eq!(
        call(&facts, source, "top()").region_kind,
        ExecutionRegionKind::Module
    );
    assert_eq!(
        call(&facts, source, "inner()").region_kind,
        ExecutionRegionKind::Arrow
    );
    assert_eq!(
        call(&facts, source, "initialize()").region_kind,
        ExecutionRegionKind::ClassInitializer
    );
    assert_eq!(
        call(&facts, source, "staticCall()").region_kind,
        ExecutionRegionKind::StaticBlock
    );
    assert_ne!(
        call(&facts, source, "outer()").region,
        call(&facts, source, "inner()").region
    );
    assert_ne!(
        call(&facts, source, "outer()").region,
        call(&facts, source, "methodCall()").region
    );
    assert!(call(&facts, source, "inner()").unconditional);
    assert!(call(&facts, source, "tail()").unconditional);
}

#[test]
fn graph_limits_fail_explicitly_and_large_straight_line_functions_remain_linear() {
    let source = format!(
        "function Component(){{{}}}",
        (0..2000).map(|i| format!("call{i}();")).collect::<String>()
    );
    let interner = Interner::new();
    let parsed = parse(&source, &interner, SourceType::Module);
    assert!(!parsed.has_errors());
    let facts = parsed.module.with_ast(analyze_call_execution).unwrap();
    assert_eq!(facts.calls.len(), 2000);
    assert!(
        facts
            .calls
            .iter()
            .all(|call| call.unconditional && !call.may_repeat)
    );
    for limits in [
        CallExecutionLimits {
            max_nodes: 10,
            ..Default::default()
        },
        CallExecutionLimits {
            max_edges: 10,
            ..Default::default()
        },
        CallExecutionLimits {
            max_steps: 10,
            ..Default::default()
        },
    ] {
        assert!(
            parsed
                .module
                .with_ast(|program| analyze_call_execution_with_limits(program, limits))
                .is_err()
        );
    }
}

#[test]
fn generator_resumption_can_complete_early_and_finally_still_executes() {
    let source = "function* iterator(){before(); try{yield 1; afterYield();}finally{cleanup();}}";
    let facts = facts(source);
    assert!(call(&facts, source, "before()").unconditional);
    assert!(!call(&facts, source, "afterYield()").unconditional);
    assert!(call(&facts, source, "cleanup()").unconditional);
}

#[test]
fn finally_break_continue_and_static_initialization_preserve_outer_completion() {
    let source = "function Component(flag){outer:while(flag){try{if(flag)break outer; continue outer;}finally{cleanup();}} after();} function Static(){class C{static{throw failed();}} never();}";
    let facts = facts(source);
    assert!(call(&facts, source, "cleanup()").may_repeat);
    assert!(call(&facts, source, "cleanup()").inside_exception);
    assert!(call(&facts, source, "after()").unconditional);
    assert!(!call(&facts, source, "never()").reachable);
    assert!(!call(&facts, source, "failed()").on_normal_path);
}
