//! CSS-in-JS 转换测试：求值子集、类名生成、CSS 抽取、失败降级。

use wake_common::Interner;
use wake_ecma_ast::SourceType;
use wake_ecma_parser::parse;

use crate::value::{Scope, StaticValue, collect_imports, collect_static_exports};
use crate::{
    CSS_IN_JS_SOURCES, CssTemplateKind, TransformResult, compiler_consumed_imports,
    discover_css_templates, transform,
};

fn run(src: &str) -> TransformResult {
    run_with(src, &Scope::default())
}

fn run_with(src: &str, imported: &Scope) -> TransformResult {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Tsx);
    assert!(!out.has_errors(), "parse 失败: {:?}", out.diagnostics);
    out.module
        .with_ast(|p| transform(p, &it, src, "src/a.tsx", imported))
}

const IMPORT: &str = "import { css } from '@crab-dev/css';\n";

#[test]
fn crab_css_is_the_only_compiler_source() {
    assert_eq!(CSS_IN_JS_SOURCES, ["@crab-dev/css"]);
}

#[test]
fn extracts_plain_declarations_and_replaces_with_class() {
    let r = run(&format!(
        "{IMPORT}const box = css`\n  color: red;\n  padding: 8px;\n`;"
    ));
    assert_eq!(r.replacements.len(), 1, "应替换一个标签模板");
    let class = r.replacements.values().next().unwrap();
    // 替换成 JS 字符串字面量，且类名以变量名为前缀
    assert!(class.starts_with("\"box_"), "类名应以变量名为前缀: {class}");
    assert!(class.ends_with('"'));
    assert!(r.css.contains("color: red"), "{}", r.css);
    assert!(r.css.contains("padding: 8px"), "{}", r.css);
    // CSS 选择器与替换出的类名一致
    let name = class.trim_matches('"');
    assert!(r.css.starts_with(&format!(".{name}{{")), "{}", r.css);
    assert!(r.diagnostics.is_empty());
}

#[test]
fn only_transforms_css_imported_from_crab_package() {
    // 未从 @crab-dev/css import → 不认，保持原样
    let r = run("const css = (x) => x;\nconst box = css`color: red;`;");
    assert!(r.replacements.is_empty(), "非 Crab CSS 的 css 不应被转换");
    assert!(r.css.is_empty());

    let r = run("import { css } from '@other/css';\nconst box = css`color: red;`;");
    assert!(r.replacements.is_empty(), "其他包的 css 不应被转换");
    assert!(r.css.is_empty());
}

#[test]
fn supports_import_alias() {
    let r = run("import { css as c } from '@crab-dev/css';\nconst box = c`color: red;`;");
    assert_eq!(r.replacements.len(), 1, "应支持 import 别名");
    assert!(r.css.contains("color: red"));
}

#[test]
fn lowers_safe_cx_calls_and_removes_fully_consumed_import() {
    let src = "import { css, cx as merge } from '@crab-dev/css';\n\
               const base = css`color:red;`;\n\
               const active = css`font-weight:bold;`;\n\
               const value = merge(base, enabled && active);";
    let r = run(src);
    assert_eq!(r.replacements.len(), 3);
    assert_eq!(r.removable_import_spans.len(), 1);
    assert!(
        r.replacements
            .values()
            .any(|text| text.contains(".filter(Boolean).join(\" \")")),
        "{:?}",
        r.replacements
    );
}

#[test]
fn keeps_cx_runtime_when_atomic_or_unknown_classes_are_possible() {
    for src in [
        "import { cx } from '@crab-dev/css'; const value = cx('atm_color_a', 'atm_color_b');",
        "import { cx } from '@crab-dev/css'; const value = cx(props.className);",
    ] {
        let r = run(src);
        assert!(r.replacements.is_empty(), "{:?}", r.replacements);
        assert!(r.removable_import_spans.is_empty());
    }
}

#[test]
fn evaluates_module_local_const_interpolation() {
    let r = run(&format!(
        "{IMPORT}const SIZE = 16;\nconst box = css`font-size: ${{SIZE}}px;`;"
    ));
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert!(r.css.contains("font-size: 16px"), "{}", r.css);
}

#[test]
fn evaluates_nested_object_member_access() {
    // design token 的真实形态：嵌套对象 + 模板字符串 + 括号键
    let src = format!(
        "{IMPORT}\
         const vars = {{ 'a.b': '--x' }};\n\
         const token = {{ space: {{ sm: `var(${{vars['a.b']}}, 8px)` }} }};\n\
         const box = css`padding: ${{token.space.sm}};`;"
    );
    let r = run(&src);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert!(
        r.css.contains("padding: var(--x, 8px)"),
        "嵌套对象+模板求值失败: {}",
        r.css
    );
}

#[test]
fn evaluates_cross_module_imported_primitive() {
    // 模拟 `import { pad } from './token.js'`：调用方已解析出可安全复制的静态值。
    let mut imported = Scope::default();
    imported.insert("pad".to_string(), StaticValue::Str("12px".to_string()));
    let src = format!(
        "{IMPORT}import {{ pad }} from './token.js';\nconst box = css`padding: ${{pad}};`;"
    );
    let r = run_with(&src, &imported);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert!(r.css.contains("padding: 12px"), "{}", r.css);
}

#[test]
fn unevaluatable_interpolation_errors_and_drops_that_declaration() {
    let src = format!(
        "{IMPORT}const box = css`\n  color: red;\n  width: ${{compute()}};\n  height: 10px;\n`;"
    );
    let r = run(&src);
    assert_eq!(r.diagnostics.len(), 1, "应有一条错误");
    assert!(r.diagnostics[0].is_error(), "{:?}", r.diagnostics);
    assert_eq!(
        r.diagnostics[0].code.as_deref(),
        Some("CRAB_CSS_STATIC_VALUE")
    );
    assert!(r.diagnostics[0].message.contains("无法安全地在构建期求值"));
    // 该条声明被丢弃，其余声明保留 → 仍是合法 CSS
    assert!(!r.css.contains("compute"), "不得残留原表达式: {}", r.css);
    assert!(!r.css.contains("width"), "该声明应被丢弃: {}", r.css);
    assert!(r.css.contains("color: red"), "{}", r.css);
    assert!(r.css.contains("height: 10px"), "{}", r.css);
}

#[test]
fn nesting_is_flattened_against_generated_class() {
    let src = format!("{IMPORT}const box = css`color: red; &:hover {{ color: blue; }}`;");
    let r = run(&src);
    let name = r
        .replacements
        .values()
        .next()
        .unwrap()
        .trim_matches('"')
        .to_string();
    assert!(
        r.css.contains(&format!(".{name}{{color: red;}}")),
        "{}",
        r.css
    );
    assert!(
        r.css.contains(&format!(".{name}:hover{{color: blue;}}")),
        "{}",
        r.css
    );
}

#[test]
fn class_names_are_deterministic_and_unique_per_template() {
    let src = format!("{IMPORT}const a = css`color: red;`;\nconst b = css`color: red;`;");
    let r1 = run(&src);
    let r2 = run(&src);
    let mut n1: Vec<_> = r1.replacements.values().cloned().collect();
    let mut n2: Vec<_> = r2.replacements.values().cloned().collect();
    n1.sort();
    n2.sort();
    assert_eq!(n1, n2, "同输入必须产出相同类名（产物确定性）");
    assert_eq!(n1.len(), 2);
    assert_ne!(n1[0], n1[1], "内容相同但不同声明的类名须唯一");
}

#[test]
fn class_name_differs_across_modules() {
    let it = Interner::new();
    let src = format!("{IMPORT}const box = css`color: red;`;");
    let out = parse(&src, &it, SourceType::Tsx);
    let a = out
        .module
        .with_ast(|p| transform(p, &it, &src, "src/a.tsx", &Scope::default()));
    let b = out
        .module
        .with_ast(|p| transform(p, &it, &src, "src/b.tsx", &Scope::default()));
    assert_ne!(
        a.replacements.values().next(),
        b.replacements.values().next(),
        "不同模块的同名变量不得撞类名"
    );
}

#[test]
fn css_inside_jsx_attribute_is_found() {
    // 真实用法：className={css`...`}
    let src = format!("{IMPORT}const C = () => <div className={{css`color: red;`}}>x</div>;");
    let r = run(&src);
    assert_eq!(r.replacements.len(), 1, "JSX 属性里的 css`` 应被发现");
    assert!(r.css.contains("color: red"), "{}", r.css);
}

#[test]
fn collects_static_exports_including_default() {
    let it = Interner::new();
    let src = "export const vars = { a: '--x' };\n\
               const token = { pad: `var(${vars.a}, 4px)` };\n\
               export default token;";
    let out = parse(src, &it, SourceType::TypeScript);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let ex = out.module.with_ast(|p| collect_static_exports(p, &it));

    assert!(ex.contains_key("vars"), "具名导出应被收集: {ex:?}");
    let d = ex.get("default").expect("默认导出应被收集");
    assert_eq!(
        d.get("pad"),
        Some(&StaticValue::Str("var(--x, 4px)".to_string())),
        "默认导出的嵌套模板应求值"
    );
}

#[test]
fn collects_import_bindings() {
    let it = Interner::new();
    let src = "import token from './t.js';\n\
               import { a as b } from './u.js';\n\
               import * as ns from './v.js';";
    let out = parse(src, &it, SourceType::TypeScript);
    let imports = out.module.with_ast(|p| collect_imports(p, &it));
    assert!(imports.contains(&(
        "token".to_string(),
        "./t.js".to_string(),
        "default".to_string()
    )));
    assert!(imports.contains(&("b".to_string(), "./u.js".to_string(), "a".to_string())));
    assert!(imports.contains(&("ns".to_string(), "./v.js".to_string(), "*".to_string())));
}

#[test]
fn numbers_format_like_js() {
    let r = run(&format!(
        "{IMPORT}const N = 8;\nconst F = 1.5;\nconst box = css`a: ${{N}}px; b: ${{F}}em;`;"
    ));
    assert!(r.css.contains("a: 8px"), "整数不应带小数点: {}", r.css);
    assert!(r.css.contains("b: 1.5em"), "{}", r.css);
}

#[test]
fn large_integer_does_not_saturate_to_i64_max() {
    let r = run(&format!(
        "{IMPORT}const N = 100000000000000000000;\nconst box = css`z-index: ${{N}};`;"
    ));
    assert!(
        r.css.contains("z-index: 100000000000000000000"),
        "{}",
        r.css
    );
    assert!(!r.css.contains("9223372036854775807"), "{}", r.css);
}

#[test]
fn string_interpolation_has_no_quotes() {
    // 关键差异：ConstVal::to_source 会给字符串加引号，CSS 场景必须裸值
    let r = run(&format!(
        "{IMPORT}const C = 'red';\nconst box = css`color: ${{C}};`;"
    ));
    assert!(
        r.css.contains("color: red"),
        "字符串插值不应带引号: {}",
        r.css
    );
    assert!(!r.css.contains("\"red\""), "{}", r.css);
}

#[test]
fn no_css_import_means_no_work() {
    let r = run("const x = 1;");
    assert!(r.is_empty());
}

// ======================================================================
// Crab CSS 的完整静态插值语义
// ======================================================================

#[test]
fn css_reference_interpolates_bare_class_name() {
    // css`` 的求值结果是**裸类名字符串**（不带点），作为选择器使用时需显式写 `.${x}`。
    let src = format!(
        "{IMPORT}const base = css`color: red;`;\n\
         const wrap = css`.${{base}} {{ margin: 4px; }}`;"
    );
    let r = run(&src);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);

    // 取出 base 的类名
    let base_cls = r
        .css
        .split('{')
        .next()
        .unwrap()
        .trim_start_matches('.')
        .to_string();
    assert!(base_cls.starts_with("base_"), "{base_cls}");
    // wrap 的规则里应含 `.base_xxx` 作为后代选择器
    assert!(
        r.css.contains(&format!(".{base_cls}{{margin: 4px;}}")),
        "css 互相引用未生效: {}",
        r.css
    );
}

#[test]
fn class_name_is_independent_of_css_content() {
    // 类名只由路径和声明身份决定：改样式不改类名，也是互相引用能预分配名称的前提。
    let a = run(&format!("{IMPORT}const box = css`color: red;`;"));
    let b = run(&format!(
        "{IMPORT}const box = css`color: blue; padding: 9px;`;"
    ));
    assert_eq!(
        a.replacements.values().next(),
        b.replacements.values().next(),
        "类名不应随 CSS 内容变化"
    );
}

#[test]
fn object_interpolation_expands_to_declarations() {
    // CSSProperties 对象插值：驼峰转连字符 + 数字补 px
    let src = format!(
        "{IMPORT}const s = {{ fontSize: 14, marginTop: 8, color: 'red' }};\n\
         const box = css`${{s}}`;"
    );
    let r = run(&src);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert!(r.css.contains("font-size: 14px"), "{}", r.css);
    assert!(r.css.contains("margin-top: 8px"), "{}", r.css);
    assert!(r.css.contains("color: red"), "{}", r.css);
}

#[test]
fn unitless_properties_get_no_px() {
    let src = format!(
        "{IMPORT}const s = {{ lineHeight: 1.5, zIndex: 10, opacity: 1, flexGrow: 2, width: 20 }};\n\
         const box = css`${{s}}`;"
    );
    let r = run(&src);
    assert!(r.css.contains("line-height: 1.5;"), "{}", r.css);
    assert!(r.css.contains("z-index: 10;"), "{}", r.css);
    assert!(r.css.contains("opacity: 1;"), "{}", r.css);
    assert!(r.css.contains("flex-grow: 2;"), "{}", r.css);
    // 非 unitless 的仍补 px
    assert!(r.css.contains("width: 20px;"), "{}", r.css);
}

#[test]
fn zero_never_gets_px_and_custom_property_kept_as_is() {
    let src =
        format!("{IMPORT}const s = {{ margin: 0, '--my-var': 4 }};\nconst box = css`${{s}}`;");
    let r = run(&src);
    assert!(r.css.contains("margin: 0;"), "0 不应补 px: {}", r.css);
    // 自定义属性名原样保留（不转连字符规则）
    assert!(r.css.contains("--my-var:"), "{}", r.css);
}

#[test]
fn vendor_prefixed_unitless_is_recognized() {
    // `WebkitBoxFlex` 剥前缀后是 `boxFlex`（unitless）→ 不补 px
    let src = format!("{IMPORT}const s = {{ WebkitBoxFlex: 1 }};\nconst box = css`${{s}}`;");
    let r = run(&src);
    assert!(
        r.css.contains("-webkit-box-flex: 1;"),
        "厂商前缀 unitless 判定错误: {}",
        r.css
    );
    assert!(!r.css.contains("1px"), "{}", r.css);
}

#[test]
fn nested_object_value_becomes_selector_block() {
    // 值为对象时，键当选择器（**不**转连字符）
    let src = format!(
        "{IMPORT}const s = {{ color: 'red', '&:hover': {{ color: 'blue' }} }};\n\
         const box = css`${{s}}`;"
    );
    let r = run(&src);
    assert!(r.css.contains("color: red"), "{}", r.css);
    // 嵌套块经 nesting 展开为独立规则
    assert!(r.css.contains(":hover{color: blue;}"), "{}", r.css);
}

#[test]
fn falsy_object_values_are_dropped_but_zero_kept() {
    let src = format!(
        "{IMPORT}const s = {{ a: 0, b: null, c: undefined, d: false, e: '', f: 'x' }};\n\
         const box = css`${{s}}`;"
    );
    let r = run(&src);
    assert!(r.css.contains("a: 0;"), "数字 0 应保留: {}", r.css);
    assert!(r.css.contains("f: x;"), "{}", r.css);
    for k in ["b:", "c:", "d:", "e:"] {
        assert!(!r.css.contains(k), "falsy 值 {k} 应被丢弃: {}", r.css);
    }
}

#[test]
fn array_interpolation_joins_items() {
    let src =
        format!("{IMPORT}const s = ['color: red;', 'padding: 2px;'];\nconst box = css`${{s}}`;");
    let r = run(&src);
    assert!(r.css.contains("color: red"), "{}", r.css);
    assert!(r.css.contains("padding: 2px"), "{}", r.css);
}

#[test]
fn undefined_and_empty_string_interpolations_are_skipped_silently() {
    // undefined 与 "" 直接跳过，不报错、不留痕。
    let src = format!(
        "{IMPORT}const E = '';\nconst box = css`color: red;${{E}}${{undefined}} padding: 1px;`;"
    );
    let r = run(&src);
    assert!(
        r.diagnostics.is_empty(),
        "undefined/空串不应报警: {:?}",
        r.diagnostics
    );
    assert!(r.css.contains("color: red"), "{}", r.css);
    assert!(r.css.contains("padding: 1px"), "{}", r.css);
}

#[test]
fn a_bound_identifier_named_undefined_uses_its_static_value() {
    let r = run("import { css } from '@crab-dev/css';\n\
         const undefined = 'rebeccapurple';\n\
         const box = css`color: ${undefined};`;");
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert!(r.css.contains("color: rebeccapurple"), "{}", r.css);
}

#[test]
fn false_interpolation_is_skipped_silently() {
    let r = run(&format!(
        "{CRAB_IMPORT}const disabled = false;\nconst box = css`color: red; ${{disabled}}`;"
    ));
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert!(r.css.contains("color: red"), "{}", r.css);
    assert!(!r.css.contains("false"), "{}", r.css);
}

#[test]
fn interpolated_multiline_value_is_collapsed_to_one_line() {
    // stripLines：插值来的多行文本压成一行（CSS 字符串内不允许裸换行）
    let src =
        format!("{IMPORT}const s = ['color: red;', 'padding: 2px;'];\nconst box = css`${{s}}`;");
    let r = run(&src);
    // 数组以 \n 连接，插值后必须被折成空格
    assert!(!r.css.contains('\n'), "插值换行未折叠: {:?}", r.css);
}

#[test]
fn arithmetic_in_interpolation() {
    let r = run(&format!(
        "{IMPORT}const U = 4;\nconst box = css`padding: ${{U * 2}}px; margin: ${{U + 1}}px; gap: ${{U / 2}}px;`;"
    ));
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert!(r.css.contains("padding: 8px"), "{}", r.css);
    assert!(r.css.contains("margin: 5px"), "{}", r.css);
    assert!(r.css.contains("gap: 2px"), "{}", r.css);
}

#[test]
fn string_concat_in_interpolation() {
    let r = run(&format!(
        "{IMPORT}const P = 'var(--';\nconst box = css`color: ${{P + 'x)'}};`;"
    ));
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert!(r.css.contains("color: var(--x)"), "{}", r.css);
}

#[test]
fn unsupported_operators_still_fail_safely() {
    // 比较/位运算不在支持范围 —— 应报警跳过，而不是猜一个值
    let r = run(&format!(
        "{IMPORT}const A = 1;\nconst box = css`color: red; z-index: ${{A > 0}};`;"
    ));
    assert_eq!(r.diagnostics.len(), 1, "{:?}", r.diagnostics);
    assert!(r.css.contains("color: red"), "{}", r.css);
    assert!(!r.css.contains("z-index"), "该声明应被跳过: {}", r.css);
}

const CRAB_IMPORT: &str = "import { css } from '@crab-dev/css';\n";

#[test]
fn crab_css_is_a_first_class_source_and_fails_closed() {
    let r = run(&format!(
        "{CRAB_IMPORT}const box = css`color: red; width: ${{compute()}};`;"
    ));
    assert_eq!(r.replacements.len(), 1);
    assert_eq!(r.diagnostics.len(), 1, "{:?}", r.diagnostics);
    assert!(r.diagnostics[0].is_error(), "{:?}", r.diagnostics);
    assert_eq!(
        r.diagnostics[0].code.as_deref(),
        Some("CRAB_CSS_STATIC_VALUE")
    );
    assert!(
        r.diagnostics[0]
            .notes
            .iter()
            .any(|note| note.contains("createVar")),
        "{:?}",
        r.diagnostics
    );
}

#[test]
fn semantic_binding_identity_prevents_shadowed_tag_transforms() {
    let r = run("import { css } from '@crab-dev/css';\n\
         const outer = css`color: red;`;\n\
         function local(css) { return css`color: blue;`; }");
    assert_eq!(r.replacements.len(), 1, "{:?}", r.replacements);
    assert!(r.css.contains("color: red"), "{}", r.css);
    assert!(!r.css.contains("color: blue"), "{}", r.css);
    // 因为 import binding 还有一次正确消费，局部参数的同名引用不应把它判成逃逸使用。
    assert_eq!(r.removable_import_spans.len(), 1);
}

#[test]
fn complete_ast_visitor_finds_styles_in_control_flow() {
    let r = run("import { css } from '@crab-dev/css';\n\
         function choose() { while (ready) { return css`color: teal;`; } }");
    assert_eq!(r.replacements.len(), 1);
    assert!(r.css.contains("color: teal"), "{}", r.css);
}

fn selector_for(css: &str, declaration: &str) -> String {
    let declaration_at = css.find(declaration).expect("declaration exists");
    let rule_start = css[..declaration_at].rfind('.').expect("class selector");
    let rule_end = css[rule_start..]
        .find('{')
        .map(|offset| rule_start + offset)
        .expect("rule opening brace");
    css[rule_start..rule_end].to_string()
}

#[test]
fn unrelated_style_insertion_does_not_churn_existing_binding_name() {
    let before = run(&format!(
        "{CRAB_IMPORT}const target = css`color: royalblue;`;"
    ));
    let after = run(&format!(
        "{CRAB_IMPORT}const unrelated = css`color: tomato;`;\n\
         const target = css`color: royalblue;`;"
    ));
    assert_eq!(
        selector_for(&before.css, "color: royalblue"),
        selector_for(&after.css, "color: royalblue")
    );
}

#[test]
fn module_seed_is_stable_across_windows_path_spellings() {
    let it = Interner::new();
    let src = format!("{CRAB_IMPORT}const box = css`color: red;`;");
    let parsed = parse(&src, &it, SourceType::Tsx);
    let windows = parsed.module.with_ast(|program| {
        transform(
            program,
            &it,
            &src,
            r"C:\project\src\button.tsx",
            &Scope::default(),
        )
    });
    let portable = parsed.module.with_ast(|program| {
        transform(
            program,
            &it,
            &src,
            "c:/project/src/button.tsx",
            &Scope::default(),
        )
    });
    assert_eq!(windows.replacements, portable.replacements);
    assert_eq!(windows.css, portable.css);
}

#[test]
fn compiles_keyframes_global_style_and_dynamic_variable_contract() {
    let r = run(
        "import { css, keyframes, globalStyle, createVar } from '@crab-dev/css';\n\
         const accent = createVar('accent');\n\
         const spin = keyframes`from { opacity: 0; } to { opacity: 1; }`;\n\
         globalStyle`:root { color-scheme: light dark; }`;\n\
         const box = css`color: ${accent}; animation: ${spin} 1s linear;`;",
    );
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert!(r.css.contains("@keyframes spin_"), "{}", r.css);
    assert!(
        r.css.contains(":root{color-scheme: light dark;}"),
        "{}",
        r.css
    );
    assert!(r.css.contains("color: var(--crab-css-accent_"), "{}", r.css);
    assert!(r.css.contains("animation: spin_"), "{}", r.css);
    assert!(r.replacements.values().any(|value| value == "void 0"));
    assert_eq!(r.removable_import_spans.len(), 1);
}

#[test]
fn assign_vars_keeps_only_the_needed_runtime_import() {
    let r = run(
        "import { css, createVar, assignVars } from '@crab-dev/css';\n\
         const accent = createVar('accent');\n\
         const box = css`color: ${accent};`;\n\
         export const style = assignVars({ [accent]: 'tomato' });",
    );
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert!(r.css.contains("var(--crab-css-accent_"), "{}", r.css);
    // 一条 mixed import 无法部分删除；assignVars 是明确且必要的小 runtime。
    assert!(r.removable_import_spans.is_empty());
}

#[test]
fn reexported_marker_import_is_kept_for_runtime_semantics() {
    let it = Interner::new();
    let source = "import { css } from '@crab-dev/css';\nexport { css };";
    let parsed = parse(source, &it, SourceType::TypeScript);
    let result = parsed
        .module
        .with_ast(|program| transform(program, &it, source, "src/reexport.ts", &Scope::default()));
    assert!(result.removable_import_spans.is_empty());
    assert!(result.removable_import_binding_spans.is_empty());
    let consumed = parsed
        .module
        .with_ast(|program| compiler_consumed_imports(program, &it));
    assert!(consumed.is_empty());
}

#[test]
fn global_style_in_runtime_control_flow_is_rejected() {
    let r = run("import { globalStyle } from '@crab-dev/css';\n\
         if (enabled) { globalStyle`body { color: red; }`; }");
    assert!(
        r.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some("CRAB_CSS_GLOBAL_SCOPE") && diagnostic.is_error()
        }),
        "{:?}",
        r.diagnostics
    );
    assert!(!r.css.contains("body"), "{}", r.css);
}

#[test]
fn scoped_crab_styles_cannot_hide_global_side_effects() {
    let r = run("import { css } from '@crab-dev/css';\n\
         const unused = css`:global(body) { color: red; }`; ");
    assert!(r.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("CRAB_CSS_GLOBAL_ESCAPE") && diagnostic.is_error()
    }));
    assert!(!r.css.contains("body"), "{}", r.css);
}

#[test]
fn scoped_crab_styles_reject_global_at_rules_but_global_style_keeps_layer_statements() {
    let scoped = run("import { css } from '@crab-dev/css';\n\
         const unused = css`@font-face { font-family: X; src: url('data:x/y,z'); }`; ");
    assert!(scoped.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("CRAB_CSS_GLOBAL_AT_RULE") && diagnostic.is_error()
    }));
    assert!(scoped.css.is_empty(), "{}", scoped.css);

    let future_global = run("import { css } from '@crab-dev/css';\n\
         const unused = css`@view-transition { navigation: auto; }`; ");
    assert!(future_global.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("CRAB_CSS_GLOBAL_AT_RULE") && diagnostic.is_error()
    }));

    let global = run("import { globalStyle } from '@crab-dev/css';\n\
         globalStyle`@layer reset, theme; @layer reset { body { margin: 0; } }`; ");
    assert!(global.diagnostics.is_empty(), "{:?}", global.diagnostics);
    assert!(
        global.css.starts_with("@layer reset, theme;"),
        "{}",
        global.css
    );
}

#[test]
fn relative_template_urls_fail_instead_of_resolving_against_the_output_directory() {
    let relative = run("import { css } from '@crab-dev/css';\n\
         const card = css`background: url('./pixel.png');`; ");
    assert!(relative.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("CRAB_CSS_RELATIVE_URL") && diagnostic.is_error()
    }));
    assert!(relative.css.is_empty(), "{}", relative.css);

    let absolute = run("import { css } from '@crab-dev/css';\n\
         const card = css`background: url('data:image/gif;base64,R0lGODlhAQABAAAAACw=');`; ");
    assert!(
        absolute.diagnostics.is_empty(),
        "{:?}",
        absolute.diagnostics
    );
    assert!(absolute.css.contains("data:image/gif"), "{}", absolute.css);
}

#[test]
fn nested_same_name_style_cannot_replace_a_top_level_static_export() {
    let it = Interner::new();
    let source = "import { css } from '@crab-dev/css';\n\
        export const box = css`color: red;`;\n\
        function local() { const box = css`color: blue;`; return box; }";
    let parsed = parse(source, &it, SourceType::TypeScript);
    let exports = parsed
        .module
        .with_ast(|program| crate::collect_static_exports(program, &it, "src/shadow.ts"));
    let exported = exports
        .get("box")
        .and_then(StaticValue::to_css_text)
        .expect("top-level style export");
    let result = parsed
        .module
        .with_ast(|program| transform(program, &it, source, "src/shadow.ts", &Scope::default()));
    let red_selector = selector_for(&result.css, "color: red");
    assert_eq!(exported, red_selector.trim_start_matches('.'));
}

#[test]
fn every_alias_of_a_static_css_value_is_exported() {
    let it = Interner::new();
    let source = "import { css, createVar } from '@crab-dev/css';\n\
        const box = css`color: red;`;\n\
        const accent = createVar('accent');\n\
        export { box as first, box as second, accent as a, accent as b };";
    let parsed = parse(source, &it, SourceType::TypeScript);
    let exports = parsed
        .module
        .with_ast(|program| crate::collect_static_exports(program, &it, "src/aliases.ts"));
    assert_eq!(exports.get("first"), exports.get("second"));
    assert_eq!(exports.get("a"), exports.get("b"));
    assert!(exports.contains_key("first"));
    assert!(exports.contains_key("a"));
}

#[test]
fn a_direct_default_style_export_can_be_referenced_cross_module() {
    let it = Interner::new();
    let source = "import { css } from '@crab-dev/css'; export default css`color: red;`;";
    let parsed = parse(source, &it, SourceType::TypeScript);
    let exports = parsed
        .module
        .with_ast(|program| crate::collect_static_exports(program, &it, "src/default.ts"));
    let value = exports
        .get("default")
        .and_then(StaticValue::to_css_text)
        .expect("direct default style export");
    assert!(value.starts_with("default_"), "{value}");
}

#[test]
fn mutable_bindings_are_never_frozen_during_static_evaluation() {
    let r = run("import { css } from '@crab-dev/css';\n\
         let color = 'red'; color = 'blue';\n\
         const box = css`color: ${color};`;");
    assert_eq!(r.diagnostics.len(), 1, "{:?}", r.diagnostics);
    assert!(r.diagnostics[0].is_error());
    assert!(!r.css.contains("color: red"), "{}", r.css);
}

#[test]
fn mutated_const_objects_are_never_frozen_during_static_evaluation() {
    let r = run("import { css } from '@crab-dev/css';\n\
         const tokens = { color: 'red' };\n\
         tokens.color = 'blue';\n\
         const box = css`color: ${tokens.color};`;");
    assert!(
        r.diagnostics.iter().any(|diagnostic| diagnostic.is_error()),
        "{:?}",
        r.diagnostics
    );
    assert!(!r.css.contains("color: red"), "{}", r.css);
}

#[test]
fn object_alias_mutation_cannot_freeze_the_original_value() {
    let r = run("import { css } from '@crab-dev/css';\n\
         const tokens = { color: 'red' };\n\
         const alias = tokens; alias.color = 'blue';\n\
         const box = css`color: ${tokens.color};`;");
    assert!(
        r.diagnostics.iter().any(|diagnostic| diagnostic.is_error()),
        "{:?}",
        r.diagnostics
    );
    assert!(!r.css.contains("color: red"), "{}", r.css);
}

#[test]
fn nested_object_alias_mutation_cannot_freeze_the_root_value() {
    let r = run("import { css } from '@crab-dev/css';\n\
         const tokens = { nested: { color: 'red' } };\n\
         const nested = tokens.nested; nested.color = 'blue';\n\
         const box = css`color: ${tokens.nested.color};`;");
    assert!(
        r.diagnostics.iter().any(|diagnostic| diagnostic.is_error()),
        "{:?}",
        r.diagnostics
    );
    assert!(!r.css.contains("color: red"), "{}", r.css);
}

#[test]
fn method_and_delete_mutations_cannot_freeze_structured_values() {
    for source in [
        "const xs = ['red']; xs.splice(0, 1, 'blue');\n\
         const box = css`color: ${xs[0]};`;",
        "const tokens = { color: 'red' }; delete tokens.color;\n\
         const box = css`color: ${tokens.color};`;",
    ] {
        let r = run(&format!("import {{ css }} from '@crab-dev/css';\n{source}"));
        assert!(
            r.diagnostics.iter().any(|diagnostic| diagnostic.is_error()),
            "{:?}",
            r.diagnostics
        );
        assert!(!r.css.contains("color: red"), "{}", r.css);
    }
}

#[test]
fn user_tagged_templates_cannot_mutate_a_frozen_object() {
    let r = run("import { css } from '@crab-dev/css';\n\
         const tokens = { color: 'red' };\n\
         mutate`${tokens}`;\n\
         const box = css`color: ${tokens.color};`;");
    assert!(
        r.diagnostics.iter().any(|diagnostic| diagnostic.is_error()),
        "{:?}",
        r.diagnostics
    );
    assert!(!r.css.contains("color: red"), "{}", r.css);
}

#[test]
fn mutating_an_imported_object_prevents_static_interpolation() {
    let mut imported = Scope::default();
    imported.insert(
        "tokens".to_string(),
        StaticValue::Obj(vec![(
            "color".to_string(),
            StaticValue::Str("red".to_string()),
        )]),
    );
    let r = run_with(
        "import { css } from '@crab-dev/css';\n\
         import { tokens } from './tokens.js';\n\
         tokens.color = 'blue';\n\
         const box = css`color: ${tokens.color};`;",
        &imported,
    );
    assert!(
        r.diagnostics.iter().any(|diagnostic| diagnostic.is_error()),
        "{:?}",
        r.diagnostics
    );
    assert!(!r.css.contains("color: red"), "{}", r.css);
}

#[test]
fn strict_crab_never_freezes_shared_objects_from_another_module() {
    let mut imported = Scope::default();
    imported.insert(
        "tokens".to_string(),
        StaticValue::Obj(vec![(
            "color".to_string(),
            StaticValue::Str("red".to_string()),
        )]),
    );
    let r = run_with(
        "import { css } from '@crab-dev/css';\n\
         import { tokens } from './tokens.js';\n\
         const box = css`color: ${tokens.color};`;",
        &imported,
    );
    assert!(
        r.diagnostics.iter().any(|diagnostic| diagnostic.is_error()),
        "{:?}",
        r.diagnostics
    );
    assert!(!r.css.contains("color: red"), "{}", r.css);
}

#[test]
fn shadowing_local_values_are_never_replaced_by_same_named_module_constants() {
    let r = run("import { css } from '@crab-dev/css';\n\
         const color = 'red';\n\
         function render(color) { return css`color: ${color};`; }");
    assert!(
        r.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some("CRAB_CSS_STATIC_VALUE") && diagnostic.is_error()
        }),
        "{:?}",
        r.diagnostics
    );
    assert!(!r.css.contains("color: red"), "{}", r.css);
}

#[test]
fn non_finite_arithmetic_is_rejected_instead_of_emitting_invalid_css() {
    let r = run("import { css } from '@crab-dev/css';\n\
         const box = css`width: ${1 / 0}px;`;");
    assert_eq!(r.diagnostics.len(), 1, "{:?}", r.diagnostics);
    assert!(r.diagnostics[0].is_error());
    assert!(!r.css.contains("inf"), "{}", r.css);
}

#[test]
fn language_discovery_uses_alias_binding_identity() {
    let source = "import { css as c, keyframes, globalStyle as global } from '@crab-dev/css';\n\
        const box = c`color: red;`;\n\
        const animation = keyframes`from { opacity: 0; }`;\n\
        global`:root { color-scheme: dark; }`;";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Tsx);
    let templates = parsed
        .module
        .with_ast(|program| discover_css_templates(program, &interner));
    assert_eq!(
        templates
            .iter()
            .map(|template| template.kind)
            .collect::<Vec<_>>(),
        [
            CssTemplateKind::Css,
            CssTemplateKind::Keyframes,
            CssTemplateKind::GlobalStyle,
        ]
    );
    assert_eq!(templates[0].literal_spans[0].slice(source), "color: red;");
}

#[test]
fn language_discovery_ignores_shadowed_and_unrelated_tags() {
    let source = "import { css as c } from '@crab-dev/css';\n\
        function render(c) { return c`color: red;`; }\n\
        const css = value => value;\n\
        const local = css`display: block;`;\n\
        const real = c`display: grid;`;";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Tsx);
    let templates = parsed
        .module
        .with_ast(|program| discover_css_templates(program, &interner));
    assert_eq!(templates.len(), 1);
    assert_eq!(
        templates[0].literal_spans[0].slice(source),
        "display: grid;"
    );
}
