use wake_lint_core::{LintOptions, SourceType, fix_text};

fn fixed(source: &str, mode: &str) -> String {
    let options: LintOptions =
        serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
            "style/comma-dangle":{"level":"warn","options":{"mode":mode}}
        }}))
        .unwrap();
    let result = fix_text(source, SourceType::Tsx, &options).unwrap();
    assert!(
        result.result.parse_diagnostics.is_empty(),
        "{:?}",
        result.result.parse_diagnostics
    );
    assert!(
        result.result.diagnostics.is_empty(),
        "{:?}",
        result.result.diagnostics
    );
    result.output
}

#[test]
fn lists_keep_original_comments_and_types_before_erasure() {
    let source = "import type {T /*type*/} from 'm'; export {T}; const 名=[1 /*last*/]; const object={a:1}; const [x]=名; const {a}=object; function f(x:T){call(x)}; type Tuple=[T]; enum E {A=1} type G<T> = T;";
    let expected = "import type {T, /*type*/} from 'm'; export {T,}; const 名=[1, /*last*/]; const object={a:1,}; const [x,]=名; const {a,}=object; function f(x:T,){call(x,)}; type Tuple=[T,]; enum E {A=1,} type G<T,> = T;";
    assert_eq!(fixed(source, "always"), expected);
    assert_eq!(fixed(expected, "never"), source);
}

#[test]
fn multiline_modes_use_last_element_and_closing_line() {
    let source =
        "const a=[\r\n  1 /* last */\r\n]; const b=[\n  2]; const c={x:1,}; call(\n x,\n);";
    assert_eq!(
        fixed(source, "always-multiline"),
        "const a=[\r\n  1, /* last */\r\n]; const b=[\n  2]; const c={x:1}; call(\n x,\n);"
    );
    assert_eq!(
        fixed(source, "only-multiline"),
        "const a=[\r\n  1 /* last */\r\n]; const b=[\n  2]; const c={x:1}; call(\n x,\n);"
    );
    assert_eq!(
        fixed("const a=[1\u{2028}];", "always-multiline"),
        "const a=[1,\u{2028}];"
    );
}

#[test]
fn holes_rest_spread_empty_lists_and_parenthesized_expressions_are_preserved() {
    let source = "const a=[,,]; const b=[1,,]; const [x,...rest]=b; const {...props}=obj; const c=[...rest]; const d={...props}; function f(...args:T[]){} call(...args); const empty=[]; call(); const e=();";
    // An empty parenthesized expression is invalid; only an empty arrow is a parameter list.
    let source = source.replace("const e=();", "const e=()=>({}); const seq=(a,b);");
    assert_eq!(fixed(&source, "always"), source);
    assert_eq!(fixed(&source, "never"), source);
    assert_eq!(
        fixed("const a=[1,]; const b=[1,,];", "never"),
        "const a=[1]; const b=[1,,];"
    );
}

#[test]
fn arrow_parameters_are_distinct_from_cover_expressions_and_keep_tsx_disambiguation() {
    let source = "const f=<T,>(x:T)=>x; const g=<T extends U,>(x:T,)=>x; const h=(x = function(a:T){return a})=>x; const seq=(a,b);";
    assert_eq!(
        fixed(source, "never"),
        "const f=<T,>(x:T)=>x; const g=<T extends U>(x:T)=>x; const h=(x = function(a:T){return a})=>x; const seq=(a,b);"
    );
    assert_eq!(
        fixed("const f=(x:T)=>x; const seq=(a,b);", "always"),
        "const f=(x:T,)=>x; const seq=(a,b);"
    );
}

#[test]
fn import_attributes_named_reexports_and_suppressions_use_source_lists() {
    let source = "import data, { item } from 'm' with {type:'json'}; export type {T} from 'm'; export {item};";
    assert_eq!(
        fixed(source, "always"),
        "import data, { item, } from 'm' with {type:'json',}; export type {T,} from 'm'; export {item,};"
    );
    let source = "// wake-lint-disable-next-line style/comma-dangle\nconst array=[1]\n";
    assert_eq!(fixed(source, "always"), source);
}

#[test]
fn type_signature_parameters_keep_rest_and_return_types_intact() {
    let source = "type F=(x:T)=>U; interface I { method(x:T):U; new(x:T):I; } type Rest=(...args:T[])=>U; declare function f(this:I,x:T):U;";
    let expected = "type F=(x:T,)=>U; interface I { method(x:T,):U; new(x:T,):I; } type Rest=(...args:T[])=>U; declare function f(this:I,x:T,):U;";
    assert_eq!(fixed(source, "always"), expected);
    assert_eq!(fixed(expected, "never"), source);
}
