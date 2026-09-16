use wake_lint_core::{LintOptions, RuleLevel, SourceType, lint_text};

fn check(source: &str, rule: &str) -> wake_lint_core::LintResult {
    let mut options = LintOptions {
        recommended: false,
        ..Default::default()
    };
    options.rules.insert(rule.into(), RuleLevel::Error.into());
    let result = lint_text(source, SourceType::Tsx, &options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    result
}

#[test]
fn duplicate_jsx_props_keep_key_names_and_nested_tag_identity() {
    let source = "const x = <UI.C<T> key='one' {...props} key='two' Key='three' render={<span key='inner' />} />;";
    let result = check(source, "react/jsx-no-duplicate-props");
    assert_eq!(result.diagnostics.len(), 1);
    let d = &result.diagnostics[0];
    assert_eq!(&source[d.start as usize..d.end as usize], "key");
    assert_eq!(d.start as usize, source.find("key='two'").unwrap());
    assert!(
        check("<><a x='1'/><b x='2'/></>", "react/jsx-no-duplicate-props")
            .diagnostics
            .is_empty()
    );
}

#[test]
fn jsx_keys_apply_to_original_array_elements_and_callback_results_only() {
    let source = "const view=<><A/><B/></>; const list=[<A/>,<B key='b'/>, ...more, cond ? <C/> : <D key='d'/>, wrap(<E/>), <><F/></>]; items.map(item=><Row><Child/></Row>); items.flatMap(item=>{function nested(){return <Inner/>} return <Row key={item.id}/>});";
    let result = check(source, "react/jsx-key");
    assert_eq!(result.diagnostics.len(), 4, "{:?}", result.diagnostics);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.fix.is_none())
    );
    assert!(
        check(
            "const [item=<Row/>]=data; ([value=<C/>])=>value; const obj={render:<A/>};",
            "react/jsx-key"
        )
        .diagnostics
        .is_empty()
    );
}

#[test]
fn jsx_key_presence_handles_parentheses_type_erasure_spreads_and_void() {
    let source = "const list=[(<A/>), <B {...props}/>, <C key={void run()}/>, <D key={null}/>, <E key={value}/>, <F key={void run()} {...props}/>]; items.map(item=>(<Row/> as Node));";
    assert_eq!(check(source, "react/jsx-key").diagnostics.len(), 3);
    assert!(
        check(
            "// wake-lint-disable-next-line react/jsx-key\nconst list=[<A/>];",
            "react/jsx-key"
        )
        .diagnostics
        .is_empty()
    );
}

#[test]
fn array_index_keys_follow_callback_symbols_and_effective_key_values() {
    let source = r#"const index='stable'; items.map((item,index)=><Row key={index}/>); items.map((item,i)=><Row key={`row-${i}`}/>); items.flatMap(function(item,position){return <Row key={position+1}/>}); items.map((item,i)=>{const render=(i)=><Row key={i}/>;return <Row key={index}/>}); items.map((item,i)=><Row key={i} {...props}/>); items.map((item,i)=><Row {...props} key={i}/>); items.map((item,i)=><Row key={()=>i}/>); items.map((item,i)=><Row key={(i=0)}/>);"#;
    assert_eq!(
        check(source, "react/no-array-index-key").diagnostics.len(),
        4
    );
    assert!(check("items.map((item,idx)=><Row key={item.id}/>); unrelated((item,index)=><Row key={index}/>);", "react/no-array-index-key").diagnostics.is_empty());
}

#[test]
fn jsx_attribute_rules_do_not_scan_objects_strings_or_spreads() {
    for (rule, name) in [
        ("react/no-danger", "dangerouslySetInnerHTML"),
        ("react/no-children-prop", "children"),
    ] {
        let source = format!(
            "const o = {{{name}: true}}; const text = '<div {name} />'; const x = <><Custom {name}={{o}} /><div {{...o}} /></>;"
        );
        let result = check(&source, rule);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].start as usize,
            source.find(&format!("{name}={{o}}")).unwrap()
        );
    }
}

#[test]
fn self_closing_only_reports_elements_without_child_source() {
    let source = "<><div></div><C /><p> </p><b>{/* keep */}</b><i>{}</i><UI.C<T>></UI.C></>";
    let result = check(source, "react/self-closing-comp");
    assert_eq!(result.diagnostics.len(), 2);
    assert_eq!(
        result.diagnostics[0].start as usize,
        source.find("<div>").unwrap()
    );
}

#[test]
fn source_rules_share_suppression_and_remain_opt_in() {
    assert!(check("// wake-lint-disable-next-line react/no-danger\n<div dangerouslySetInnerHTML={value} />", "react/no-danger").diagnostics.is_empty());
    let result = lint_text(
        "<div children={value}></div>",
        SourceType::Tsx,
        &LintOptions::default(),
    )
    .unwrap();
    assert!(result.diagnostics.is_empty());
}
