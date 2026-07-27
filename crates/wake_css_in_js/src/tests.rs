//! CSS-in-JS 转换测试：求值子集、类名生成、CSS 抽取、失败降级。

use wake_common::Interner;
use wake_ecma_ast::SourceType;
use wake_ecma_parser::parse;

use crate::value::{Scope, StaticValue, collect_imports, collect_static_exports};
use crate::{TransformResult, transform};

fn run(src: &str) -> TransformResult {
    run_with(src, &Scope::default())
}

fn run_with(src: &str, imported: &Scope) -> TransformResult {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Tsx);
    assert!(!out.has_errors(), "parse 失败: {:?}", out.diagnostics);
    out.module
        .with_ast(|p| transform(p, &it, "src/a.tsx", imported))
}

const IMPORT: &str = "import { css } from '@linaria/core';\n";

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
fn only_transforms_css_imported_from_linaria() {
    // 未从 linaria import → 不认，保持原样
    let r = run("const css = (x) => x;\nconst box = css`color: red;`;");
    assert!(r.replacements.is_empty(), "非 linaria 的 css 不应被转换");
    assert!(r.css.is_empty());
}

#[test]
fn supports_import_alias() {
    let r = run("import { css as c } from '@linaria/core';\nconst box = c`color: red;`;");
    assert_eq!(r.replacements.len(), 1, "应支持 import 别名");
    assert!(r.css.contains("color: red"));
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
fn evaluates_cross_module_imported_value() {
    // 模拟 `import token from './token.js'`：调用方已解析出静态值
    let mut imported = Scope::default();
    imported.insert(
        "token".to_string(),
        StaticValue::Obj(vec![(
            "pad".to_string(),
            StaticValue::Str("12px".to_string()),
        )]),
    );
    let src = format!(
        "{IMPORT}import token from './token.js';\nconst box = css`padding: ${{token.pad}};`;"
    );
    let r = run_with(&src, &imported);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert!(r.css.contains("padding: 12px"), "{}", r.css);
}

#[test]
fn unevaluatable_interpolation_warns_and_drops_that_declaration() {
    let src = format!(
        "{IMPORT}const box = css`\n  color: red;\n  width: ${{compute()}};\n  height: 10px;\n`;"
    );
    let r = run(&src);
    assert_eq!(r.diagnostics.len(), 1, "应有一条警告");
    assert!(r.diagnostics[0].message.contains("无法在构建期求值"));
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
        .with_ast(|p| transform(p, &it, "src/a.tsx", &Scope::default()));
    let b = out
        .module
        .with_ast(|p| transform(p, &it, "src/b.tsx", &Scope::default()));
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
// 对齐 @linaria/core 的完整插值语义（templateProcessor + toCSS）
// ======================================================================

#[test]
fn css_reference_interpolates_bare_class_name() {
    // Linaria 语义：css`` 的求值结果是**裸类名字符串**（不带点），
    // 要当选择器用须自己写 `.${x}`（带点只用于 styled 组件的 __wyw_meta）。
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
    // 类名只由 路径+序号 决定 → 改样式不改类名（对齐 Linaria slug 规则，
    // 也是 css 互相引用得以在求值前确定类名的前提）。
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
    // Linaria：undefined 与 "" 直接跳过，不报错、不留痕
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
