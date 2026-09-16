use wake_common::Interner;
use wake_ecma_ast::{Expression, Statement};
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

const INVALID: &[&str] = &[
    r"\1",
    r"\7",
    r"\8",
    r"\9",
    r"\00",
    r"\08",
    r"\09",
    r"\x",
    r"\x0",
    r"\xzz",
    r"\u",
    r"\u0",
    r"\u00",
    r"\u000",
    r"\u12zz",
    r"\u{",
    r"\u{}",
    r"\u{z}",
    r"\u{123z}",
    r"\u{110000}",
    r"\u{ffffffffffffffffffffffffffffffff}",
];

#[test]
fn tagged_template_invalid_escapes_have_no_cooked_value_in_only_the_affected_quasi() {
    for raw in INVALID {
        for (source, bad_index) in [
            (format!("tag`{raw}`;"), 0),
            (format!("tag`{raw}${{value}}valid\\n`;"), 0),
            (
                format!("tag`valid\\n${{value}}{raw}${{other}}valid\\n`;"),
                1,
            ),
            (format!("tag`valid\\n${{value}}{raw}`;"), 1),
        ] {
            for kind in [
                SourceType::Script,
                SourceType::Module,
                SourceType::TypeScript,
            ] {
                let interner = Interner::new();
                let output = parse(&source, &interner, kind);
                assert!(!output.has_errors(), "{source}: {:?}", output.diagnostics);
                output.module.with_ast(|program| {
                    let Statement::Expression(statement) = program.body[0] else {
                        panic!("expected expression")
                    };
                    let Expression::TaggedTemplate(tagged) = statement.expression else {
                        panic!("expected tagged template")
                    };
                    for (index, quasi) in tagged.quasi.quasis.iter().enumerate() {
                        assert_eq!(
                            interner.resolve(quasi.raw),
                            if index == bad_index { *raw } else { r"valid\n" }
                        );
                        if index == bad_index {
                            assert_eq!(quasi.cooked, None, "{source}");
                        } else {
                            assert_eq!(interner.resolve_js(quasi.cooked.unwrap()), "valid\n");
                        }
                    }
                });
            }
        }
    }
}

#[test]
fn untagged_templates_and_template_types_reject_invalid_escapes() {
    for raw in INVALID {
        for source in [
            format!("`{raw}`;"),
            format!("`{raw}${{value}}ok`;"),
            format!("`ok${{value}}{raw}${{other}}ok`;"),
            format!("`ok${{value}}{raw}`;"),
            format!("tag`outer${{`{raw}`}}ok`;"),
            format!("type T = `{raw}`;"),
            format!("type T = `{raw}${{string}}ok`;"),
            format!("type T = `ok${{string}}{raw}${{number}}ok`;"),
            format!("type T = `ok${{string}}{raw}`;"),
            format!("type T = {{ value: `{raw}` }};"),
            format!("type T = [`{raw}`];"),
            format!("type T = (`{raw}`);"),
            format!("type T = (value: `{raw}`) => void;"),
            format!("type T = Record<string, string>[`{raw}`];"),
            format!("interface T {{ value: `{raw}`; }}"),
            format!("declare const value: {{ nested: `{raw}` }};"),
            format!("declare class C {{ value: `{raw}`; }}"),
            format!("declare module 'module' {{ type T = `{raw}`; }}"),
            format!("class C {{ [key: `{raw}`]: number; }}"),
        ] {
            let interner = Interner::new();
            let output = parse(&source, &interner, SourceType::TypeScript);
            assert!(output.has_errors(), "must reject {source}");
            let collected = parse_source(
                &source,
                &interner,
                SourceType::TypeScript,
                ParseOptions::default(),
            );
            assert!(
                collected.parsed.has_errors(),
                "source facts must reject {source}"
            );
        }
    }
}

#[test]
fn tagged_templates_do_not_suppress_unrelated_syntax_errors() {
    for source in [
        r"tag`\x",
        r"tag`ok${}`;",
        r"tag`ok${",
        r#"tag`ok${"\x"}`;"#,
        r#"tag`\x${tag`ok${"\u{}"}`}`;"#,
    ] {
        let interner = Interner::new();
        assert!(
            parse(source, &interner, SourceType::TypeScript).has_errors(),
            "{source}"
        );
    }
}

#[test]
fn nested_tagged_templates_keep_source_facts_through_typescript_speculation() {
    let source = r"const result = (tag<string>`\u${tag<number>`\8`}ok`); const fn = <T>(value: T = tag`\x`) => value;";
    let interner = Interner::new();
    let output = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    assert_eq!(output.templates.len(), 3);
    assert!(output.templates.iter().all(|site| site.tagged));
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert!(!ordinary.has_errors(), "{:?}", ordinary.diagnostics);
    assert_eq!(
        output.parsed.module.structure_hash(),
        ordinary.module.structure_hash()
    );
}

#[test]
fn nested_valid_template_types_erase_without_runtime_imports_or_structure_changes() {
    let source = r#"
declare module 'ambient' {
  import value from './erased';
  export type T = { text: `\u{00000041}${string}\ud800` };
}
declare class Keys { [key: `prefix${string}`]: string; }
declare const value: [`\0`, (input: `\q`) => `\x41`];
interface Shape { value: (`\udfff`); }
type Mapped = { [K in 'a' | 'b' as `key${K}`]: `value${K}` };
type Index = Record<string, string>[`key`];
const result = 1;
"#;
    let interner = Interner::new();
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    let collected = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(!ordinary.has_errors(), "{:?}", ordinary.diagnostics);
    assert!(
        !collected.parsed.has_errors(),
        "{:?}",
        collected.parsed.diagnostics
    );
    assert!(ordinary.dependencies.is_empty());
    assert!(collected.parsed.dependencies.is_empty());
    assert_eq!(
        ordinary.module.structure_hash(),
        collected.parsed.module.structure_hash()
    );
}
