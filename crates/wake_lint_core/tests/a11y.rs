use wake_lint_core::{LintOptions, SourceType, lint_text};

fn check(source: &str, id: &str) -> Vec<(String, String)> {
    let options: LintOptions =
        serde_json::from_value(serde_json::json!({"recommended":false,"rules":{id:"error"}}))
            .unwrap();
    let result = lint_text(source, SourceType::Tsx, &options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{source}: {:?}",
        result.parse_diagnostics
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.fix.is_none())
    );
    result
        .diagnostics
        .into_iter()
        .map(|diagnostic| {
            (
                diagnostic.message_id,
                source[diagnostic.start as usize..diagnostic.end as usize].into(),
            )
        })
        .collect()
}

#[test]
fn image_alternatives_distinguish_empty_decoration_invalid_literals_and_spread_order() {
    let source = r#"const view=<><img/><img alt/><img alt={null}/><img alt=""/><img alt="a&amp;b"/><img alt={label}/><img {...props}/><img alt={null} {...props}/><img {...props} alt={null}/><area/><area alt=""/><area alt="Map"/><input type="image"/><input type="text"/><input type={kind}/><object/><object title="Preview"/><object>Fallback</object><Image/><img aria-label="Icon"/></>;"#;
    // A spread can still supply an ARIA name after alt is explicitly cleared.
    assert_eq!(check(source, "a11y/alt-text").len(), 7);
    assert_eq!(
        check(
            "const el=<img {...props} alt={null} aria-label={null} aria-labelledby={null}/>;",
            "a11y/alt-text"
        )
        .len(),
        1
    );
    assert!(check("const el=<><img alt={undefined}/><img alt={void run()} aria-label={name}/><object><Content/></object></>;", "a11y/alt-text").is_empty());
}

#[test]
fn anchors_need_accessible_content_and_real_destinations() {
    let source = r#"const view=<><a/><a>{/* empty */}{false}{null}</a><a> </a><a>{0}</a><a><span aria-hidden="true">Hidden</span></a><a><img alt="Icon"/></a><a aria-label="Label"/><a>{value}</a><a><Component/></a><a hidden/><a>{<span/>}</a><a>{<span>Text</span>}</a></>;"#;
    assert_eq!(check(source, "a11y/anchor-has-content").len(), 5);
    let hrefs = r##"const view=<><a/><a href=""/><a href="#"/><a href="j&#97;vascript:run()"/><a href={false}/><a href="/docs"/><a href="#section"/><a href={path}/><a {...props}/><a {...props} href={null}/><Link/></>;"##;
    assert_eq!(check(hrefs, "a11y/anchor-is-valid").len(), 6);
}

#[test]
fn labels_require_text_and_association_without_treating_hidden_inputs_as_controls() {
    let source = r#"const view=<><label>Name</label><label htmlFor="field"/><label htmlFor="field">Name</label><label>Name<input/></label><label>Name<input type="hidden"/></label><label>Name<Field/></label><label htmlFor={id}>Name</label><label hidden/><label>Outer<label>Inner<input/></label></label><label aria-label="Name"><input/></label></>;"#;
    let messages = check(source, "a11y/label-has-associated-control");
    assert_eq!(
        messages
            .iter()
            .map(|message| message.0.as_str())
            .collect::<Vec<_>>(),
        ["control", "content", "control", "control"]
    );
}

#[test]
fn a11y_rules_use_jsx_facts_and_real_comment_directives() {
    assert!(check("const text='<img>'; const o={alt:null};\n// wake-lint-disable-next-line a11y/alt-text\nconst image=<img/>;", "a11y/alt-text").is_empty());
    assert!(
        check(
            "const view=<><Image/><ui.Image/><a {...props}/><label {...props}/></>;",
            "a11y/anchor-has-content"
        )
        .is_empty()
    );
}

#[test]
fn aria_names_follow_the_pinned_vocabulary_and_keep_components_separate() {
    let source = r#"const el=<><div aria-labl="a" aria-Hidden="true" ARIA-label="b" aria-label="good" aria-description="Longer" aria-braillelabel="Dots" aria-colindextext="C" aria-grabbed={false}/><Component aria-labl="custom"/><svg aria-roledescription="Chart"/></>;"#;
    assert_eq!(check(source, "a11y/aria-props").len(), 3);
}

#[test]
fn aria_value_types_validate_static_final_values_and_dom_primitive_strings() {
    let invalid = r#"const el=<div aria-hidden="maybe" aria-checked="partial" aria-level="1.5" aria-valuenow="Infinity" aria-activedescendant="one two" aria-controls=" " aria-relevant="additions other" aria-sort="up" aria-label={null}/>;"#;
    assert_eq!(check(invalid, "a11y/aria-proptypes").len(), 8);
    let valid = r#"const el=<div aria-hidden={false} aria-checked="mixed" aria-level={2} aria-valuenow="1.5e2" aria-activedescendant="one" aria-controls="one two" aria-relevant="additions text" aria-sort="ascending" aria-label={42} aria-current={true} aria-details="one two" aria-grabbed="undefined" aria-description=""/>;"#;
    assert!(check(valid, "a11y/aria-proptypes").is_empty());
    let uncertain = r#"const el=<><div aria-level="bad" {...props}/><div aria-level={value}/><div aria-level={null}/><div aria-level={void run()}/><div {...props} aria-level="bad"/><div aria-level="bad" aria-level="2"/></>;"#;
    assert_eq!(check(uncertain, "a11y/aria-proptypes").len(), 1);
}

#[test]
fn aria_roles_accept_concrete_fallbacks_and_extension_roles_but_not_abstract_only() {
    let source = r#"const el=<><div role=""/><div role="widget"/><div role="madeup"/><div role={false}/><div role="BUTTON"/><div role="future button"/><div role="widget checkbox"/><div role="doc-pageheader"/><svg role="graphics-document"/><div role="image"/><div role={null}/><div role={choice}/><div role="bad" {...props}/><div {...props} role="bad"/><Component role="custom"/></>;"#;
    assert_eq!(check(source, "a11y/aria-role").len(), 6);
}

#[test]
fn click_interactions_need_keyboard_handlers_unless_native_hidden_or_uncertain() {
    let source = r#"const el=<><div onClick={run}/><div onClick={run} onKeyDown={key}/><div onClick={run} onKeyUp={null}/><button onClick={run}/><a href="" onClick={run}/><a onClick={run}/><input type="hidden" onClick={run}/><video controls onClick={run}/><audio onClick={run}/><div contentEditable onClick={run}/><div role="presentation" onClick={run}/><div aria-disabled="true" onClick={run}/><div hidden><div onClick={run}/></div><div {...props} onClick={run}/><div onClick={null}/><div onClick={void run()}/><Button onClick={run}/><details><summary onClick={run}/><summary onClick={run}/></details></>;"#;
    assert_eq!(check(source, "a11y/click-events-have-key-events").len(), 5);
    assert!(check("const el=<><div onClick={run} onKeyPress={key}/><div onClick={run} {...props}/><div onClick={run} onKeyDown={false} {...props}/></>;", "a11y/click-events-have-key-events").is_empty());
}

#[test]
fn interactive_roles_require_focus_without_rejecting_programmatic_focus() {
    let source = r#"const el=<><div role="button" onClick={run}/><div role="checkbox" onKeyDown={run}/><div role="button" onClick={run} tabIndex={false}/><div role="button" onClick={run} tabIndex="bad"/><div role="button" onClick={run} tabIndex="0"/><div role="treeitem" onClick={run} tabIndex={-1}/><button role="button" onClick={run}/><a href="/" role="link" onClick={run}/><div role="button" onClick={run} tabIndex={value}/><div role={role} onClick={run}/><div role="button"/><div role="progressbar" onClick={run}/><div role="button" onClick={run} aria-disabled/><div hidden><div role="button" onClick={run}/></div><div {...props} role="button" onClick={run}/></>;"#;
    assert_eq!(check(source, "a11y/interactive-supports-focus").len(), 4);
}

#[test]
fn accessible_content_rules_respect_ancestor_visibility_and_explicit_empty_label_for() {
    let source = "const el=<div hidden><a/><label/></div>;";
    assert!(check(source, "a11y/anchor-has-content").is_empty());
    assert!(check(source, "a11y/label-has-associated-control").is_empty());
    assert_eq!(
        check(
            "const el=<label htmlFor=\"\">Text<input/></label>;",
            "a11y/label-has-associated-control"
        )
        .len(),
        1
    );
}
