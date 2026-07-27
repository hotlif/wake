//! # wake_css_in_js — 零运行时 CSS-in-JS（Linaria / wyw-in-js 子集）
//!
//! CRUSTIFY-PARITY §M5。把源码里的 `` css`...` `` 标签模板在**构建期**求值并抽取为静态 CSS，
//! 表达式本身替换为类名字符串字面量——运行时不含任何样式计算。
//!
//! ```js
//! import { css } from '@linaria/core';
//! const box = css`
//!   padding: ${token.space};
//!   &:hover { color: red; }
//! `;
//! ```
//! 编译为 `const box = "box_a1b2c3";`，并产出
//! `.box_a1b2c3{padding:8px}.box_a1b2c3:hover{color:red}`。
//!
//! ## 与真 Linaria 的差异
//! Linaria 在 Node VM 里**真实执行**模块来求值插值；wake 无 JS 运行时，改为对纯数据子集做
//! 静态求值（[`value`]）。函数调用/条件表达式等求值失败时**报警并跳过该条声明**，不猜语义。
//! `styled` 组件工厂需要 React 运行时，不在本实现范围（目标项目零使用）。

use wake_common::{Diagnostic, FxHashMap, Interner, Span};
use wake_ecma_ast::*;

pub mod nesting;
pub mod value;

pub use value::{StaticExports, StaticValue};

/// 收集模块可被其它模块静态引用的导出常量，**并把 `css` 绑定的类名一并导出**。
///
/// 后者使 css 互相引用可跨模块：`import { base } from './base.js'` 后 `.${base}` 能求值。
/// 类名规则与 [`transform`] 完全一致（同一 `seed` + 同一遍历序）。
pub fn collect_static_exports(program: &Program, interner: &Interner, seed: &str) -> StaticExports {
    collect_static_exports_with(program, interner, seed, &value::Scope::default())
}

/// 同上，但可传入本模块 import 绑定的已求值静态值，支持**跨模块多层**常量传播。
///
/// 调用方（打包器）按依赖拓扑序推进：先算叶子模块的导出，再逐层把已算出的值装配成
/// 下游模块的 `imported`。如此 `a.js → b.js → c.js` 的常量链可完整传播。
pub fn collect_static_exports_with(
    program: &Program,
    interner: &Interner,
    seed: &str,
    imported: &value::Scope,
) -> StaticExports {
    let mut out = value::collect_static_exports_with(program, interner, imported);
    let tags = collect_css_tags(program, interner);
    if tags.is_empty() {
        return out;
    }
    // 只有被 `export` 出去的 css 绑定才需要登记（其余对外不可见）。
    let exported = exported_names(program, interner);
    for (name, class) in assign_class_names(program, interner, &tags, seed) {
        if let Some(export_as) = exported.get(&name) {
            out.insert(export_as.clone(), StaticValue::Str(class));
        }
    }
    out
}

/// 本地名 → 导出名（`export const x`、`export { x as y }`、`export default x`）。
fn exported_names(program: &Program, interner: &Interner) -> FxHashMap<String, String> {
    let mut m = FxHashMap::default();
    for stmt in program.body.iter() {
        match stmt {
            Statement::ExportNamed(e) => {
                if let Some(Statement::VariableDeclaration(d)) = e.declaration {
                    for decl in d.declarations.iter() {
                        if let Pattern::Ident(id) = &decl.id {
                            let n = interner.resolve(id.name);
                            m.insert(n.clone(), n);
                        }
                    }
                }
                if e.source.is_none() {
                    for spec in e.specifiers.iter() {
                        let local = match &spec.local {
                            ModuleExportName::Ident(id) => interner.resolve(id.name),
                            ModuleExportName::String(a) => interner.resolve(*a),
                        };
                        let exported = match &spec.exported {
                            ModuleExportName::Ident(id) => interner.resolve(id.name),
                            ModuleExportName::String(a) => interner.resolve(*a),
                        };
                        m.insert(local, exported);
                    }
                }
            }
            Statement::ExportDefault(d) => {
                if let ExportDefaultKind::Expression(Expression::Identifier(id)) = &d.declaration {
                    m.insert(interner.resolve(id.name), "default".to_string());
                }
            }
            _ => {}
        }
    }
    m
}

/// 一次模块转换的产出。
#[derive(Debug, Default)]
pub struct TransformResult {
    /// `span → 替换源码`（标签模板 span → JS 字符串字面量）。喂给 codegen 的
    /// `MinifyCtx::expression_replacements`。
    pub replacements: FxHashMap<Span, String>,
    /// 本模块抽取出的 CSS（已按声明序拼接）。
    pub css: String,
    /// 求值失败等诊断（警告级，不中断构建）。
    pub diagnostics: Vec<Diagnostic>,
}

impl TransformResult {
    pub fn is_empty(&self) -> bool {
        self.replacements.is_empty() && self.css.is_empty()
    }
}

/// Linaria 包名：从这些模块 import 的 `css` 才被识别为 CSS-in-JS 标签。
///
/// 限定来源可避免误伤同名的普通函数（如自定义的 `css()` 工具）。
pub const CSS_IN_JS_SOURCES: &[&str] = &["@linaria/core", "@wyw-in-js/core", "@wake/css"];

/// 该模块说明符是否是 CSS-in-JS 的来源包。
///
/// 供打包器**在扫描后、codegen 前**廉价判断「本次构建是否用得上 CSS-in-JS」：全项目无人
/// import 时可整体跳过静态导出求值，使未用 Linaria 的项目零开销（故本功能可默认开启）。
pub fn is_css_in_js_source(specifier: &str) -> bool {
    CSS_IN_JS_SOURCES.contains(&specifier)
}

/// 转换一个模块。
///
/// - `seed`：类名 hash 的种子，应为模块路径（保证跨模块不撞名、同输入稳定）。
/// - `imported`：本模块 import 绑定的已求值静态值（本地名 → 值），由调用方按
///   [`collect_static_exports`] 跨模块解析后传入。
pub fn transform(
    program: &Program,
    interner: &Interner,
    seed: &str,
    imported: &value::Scope,
) -> TransformResult {
    let mut out = TransformResult::default();

    // 1) 找出本模块把 `css` 绑定成了哪个本地名（支持 `import { css as c }`）。
    let tags = collect_css_tags(program, interner);
    if tags.is_empty() {
        return out;
    }

    // 2) 模块顶层常量（可引用 import 进来的值）。
    let mut scope = value::collect_module_scope(program, interner, imported);

    // 2′) **css 互相引用**：预先算出本模块每个 `const X = css\`…\`` 的类名并注入作用域。
    // 类名只由「模块路径 + 序号」决定、不依赖 CSS 内容（对齐 Linaria 的 slug 规则），
    // 因此可在求值**之前**确定，从而打破「求值需要类名、类名需要求值」的循环。
    // 之后 `${X}` 求值即得裸类名字符串——与 Linaria 一致（要当选择器须自己写 `.${X}`）。
    for (name, class) in assign_class_names(program, interner, &tags, seed) {
        scope.insert(name, value::StaticValue::Str(class));
    }

    let ctx = value::EvalCtx {
        interner,
        scope: &scope,
        imported,
    };

    // 3) 遍历 AST，处理每个 `css` 标签模板。
    let mut collector = Collector {
        interner,
        tags: &tags,
        ctx: &ctx,
        seed,
        out: &mut out,
        name_hint: None,
        counter: 0,
    };
    collector.visit_program(program);
    out
}

/// 收集 `import { css } from '@linaria/core'` 绑定的本地名。
fn collect_css_tags(program: &Program, interner: &Interner) -> Vec<String> {
    let mut tags = Vec::new();
    for stmt in program.body.iter() {
        let Statement::Import(imp) = stmt else {
            continue;
        };
        let source = interner.resolve(imp.source);
        if !CSS_IN_JS_SOURCES.iter().any(|s| *s == source) {
            continue;
        }
        for s in imp.specifiers.iter() {
            if let ImportSpecifier::Named {
                local, imported, ..
            } = s
            {
                let imported_name = match imported {
                    ModuleExportName::Ident(id) => interner.resolve(id.name),
                    ModuleExportName::String(a) => interner.resolve(*a),
                };
                if imported_name == "css" {
                    tags.push(interner.resolve(local.name));
                }
            }
        }
    }
    tags
}

/// 类名生成：`变量名_hash8`，hash 只由「模块路径 + 模块内序号」决定。
///
/// **刻意不混入 CSS 内容**——对齐 Linaria（slug 取自 `相对路径:序号`）。这既让类名在改样式时
/// 保持稳定（利于调试与缓存），也使类名可在求值前算出，从而支持 css 之间互相引用。
fn class_name_for(hint: &str, seed: &str, index: u32) -> String {
    let key = format!("{seed}\u{0}{index}");
    format!("{}_{:08x}", sanitize_ident(hint), fnv1a(&key))
}

/// 前序遍历，给每个 `const X = css\`…\`` 预分配类名，返回 `(变量名, 类名)`。
///
/// 遍历顺序与计数必须与 [`Collector`] 完全一致（同一份 `visit_*` 逻辑，见 `NameAssigner`）。
fn assign_class_names(
    program: &Program,
    interner: &Interner,
    tags: &[String],
    seed: &str,
) -> Vec<(String, String)> {
    let mut a = NameAssigner {
        interner,
        tags,
        seed,
        out: Vec::new(),
        name_hint: None,
        counter: 0,
    };
    a.visit_program(program);
    a.out
}

/// 与 [`Collector`] 同构的轻量遍历：只分配类名、不渲染 CSS。
struct NameAssigner<'a> {
    interner: &'a Interner,
    tags: &'a [String],
    seed: &'a str,
    out: Vec<(String, String)>,
    name_hint: Option<String>,
    counter: u32,
}

/// 两趟遍历（预分配类名 / 真正抽取）共用的遍历骨架。
///
/// 两者**必须**走完全相同的顺序与计数，否则预注入作用域的类名会与产出的类名错位。
/// 用同一份 `visit_*` 默认实现保证这一点，杜绝两份逻辑各自漂移。
trait CssWalk {
    fn interner(&self) -> &Interner;
    fn tags(&self) -> &[String];
    /// 换入新的类名提示（变量名），返回旧值以便恢复。
    fn swap_hint(&mut self, hint: Option<String>) -> Option<String>;
    /// 命中一个 `css` 标签模板。
    fn on_css(&mut self, tt: &TaggedTemplateExpression);

    fn visit_program(&mut self, program: &Program) {
        for stmt in program.body.iter() {
            self.visit_statement(stmt);
        }
    }

    fn visit_statement(&mut self, stmt: &Statement) {
        // 变量声明：记录名字作为类名提示，再递归其初始化器。
        if let Statement::VariableDeclaration(d) = stmt {
            for decl in d.declarations.iter() {
                let hint = match &decl.id {
                    Pattern::Ident(id) => Some(self.interner().resolve(id.name)),
                    _ => None,
                };
                if let Some(init) = &decl.init {
                    let saved = self.swap_hint(hint);
                    self.visit_expression(init);
                    self.swap_hint(saved);
                }
            }
            return;
        }
        walk_statement_children(self, stmt);
    }

    fn visit_expression(&mut self, expr: &Expression) {
        if let Expression::TaggedTemplate(tt) = expr
            && is_css_tag(self.interner(), self.tags(), &tt.tag)
        {
            self.on_css(tt);
            return;
        }
        walk_expression_children(self, expr);
    }
}

/// 标签是否是本模块绑定的 `css`。
fn is_css_tag(interner: &Interner, tags: &[String], tag: &Expression) -> bool {
    match tag {
        Expression::Identifier(id) => tags.contains(&interner.resolve(id.name)),
        _ => false,
    }
}

impl CssWalk for NameAssigner<'_> {
    fn interner(&self) -> &Interner {
        self.interner
    }
    fn tags(&self) -> &[String] {
        self.tags
    }
    fn swap_hint(&mut self, hint: Option<String>) -> Option<String> {
        std::mem::replace(&mut self.name_hint, hint)
    }
    fn on_css(&mut self, _tt: &TaggedTemplateExpression) {
        let hint = self.name_hint.as_deref().unwrap_or("css");
        let class = class_name_for(hint, self.seed, self.counter);
        self.counter += 1;
        // 只有绑定到变量的 css 才可被引用（匿名的没有名字可引用）。
        if let Some(name) = self.name_hint.clone() {
            self.out.push((name, class));
        }
    }
}

impl CssWalk for Collector<'_, '_> {
    fn interner(&self) -> &Interner {
        self.interner
    }
    fn tags(&self) -> &[String] {
        self.tags
    }
    fn swap_hint(&mut self, hint: Option<String>) -> Option<String> {
        std::mem::replace(&mut self.name_hint, hint)
    }
    fn on_css(&mut self, tt: &TaggedTemplateExpression) {
        self.handle_css_template(tt);
    }
}

struct Collector<'a, 'b> {
    interner: &'a Interner,
    tags: &'a [String],
    ctx: &'a value::EvalCtx<'a>,
    seed: &'a str,
    out: &'b mut TransformResult,
    /// 当前所在变量声明的名字，用作类名前缀（`const box = css\`\`` → `box_xxxx`）。
    name_hint: Option<String>,
    /// 同模块内的序号，保证匿名/同名场景类名唯一。
    counter: u32,
}

impl Collector<'_, '_> {
    fn handle_css_template(&mut self, tt: &TaggedTemplateExpression) {
        let (body, mut diags) = render_template(tt.quasi, self.ctx, self.interner);
        self.out.diagnostics.append(&mut diags);

        // 类名与 `assign_class_names` 必须**同规则同序**（同一次前序遍历、同一计数器），
        // 否则预注入作用域的类名与真正产出的类名会对不上。
        let hint = self.name_hint.as_deref().unwrap_or("css");
        let class = class_name_for(hint, self.seed, self.counter);
        self.counter += 1;

        let css = nesting::flatten(&format!(".{class}"), &body);
        self.out.css.push_str(&css);
        self.out
            .replacements
            .insert(tt.span, js_string_literal(&class));
    }
}

/// 渲染标签模板为 CSS 文本：静态片段原样，插值求值后替换。
///
/// 无法求值的插值 → 记一条警告，并**丢弃其所在的那条声明**（从上一个 `;`/`{`/`}` 到下一个 `;`），
/// 使产出仍是合法 CSS。
fn render_template(
    quasi: &TemplateLiteral,
    ctx: &value::EvalCtx,
    interner: &Interner,
) -> (String, Vec<Diagnostic>) {
    let mut out = String::new();
    let mut diags = Vec::new();
    // 记录求值失败的插值在 out 中的位置，稍后按声明边界回删。
    let mut bad_spots: Vec<usize> = Vec::new();

    for (i, q) in quasi.quasis.iter().enumerate() {
        if let Some(text) = value::cooked_text(q, interner) {
            out.push_str(&text);
        }
        let Some(expr) = quasi.expressions.get(i) else {
            continue;
        };
        // 插值分派，对齐 Linaria `templateProcessor`：
        //   undefined / "" → 静默跳过（不追加任何文本，也不报警）
        //   可 CSS 化的值（字符串/数字/数组/对象）→ toCSS 后折行为空格
        //   其余（含函数、无法静态求值）→ 报警并回删该条声明
        let evaluated = value::eval(expr, ctx);
        if matches!(
            evaluated,
            Some(value::StaticValue::Undefined) | Some(value::StaticValue::Null)
        ) {
            continue;
        }
        if let Some(value::StaticValue::Str(s)) = &evaluated
            && s.is_empty()
        {
            continue;
        }
        match evaluated.as_ref().and_then(|v| v.to_css()) {
            Some(text) => out.push_str(&strip_lines(&text)),
            None => {
                bad_spots.push(out.len());
                diags.push(
                    Diagnostic::warning("css`` 插值无法在构建期求值，已跳过该条声明")
                        .with_primary(expr.span(), "此插值不是可静态求值的表达式")
                        .with_note(
                            "支持：字面量、模板字符串、对象/数组字面量、成员访问、以及它们引用的顶层 const（含跨模块 import）",
                        ),
                );
            }
        }
    }

    if !bad_spots.is_empty() {
        out = drop_declarations_at(&out, &bad_spots);
    }
    (out, diags)
}

/// 删除包含指定位置的那几条声明（按 `;`/`{`/`}` 切分边界）。
fn drop_declarations_at(src: &str, spots: &[usize]) -> String {
    let bytes = src.as_bytes();
    let mut remove: Vec<(usize, usize)> = Vec::new();
    for &spot in spots {
        // 向前找声明起点：上一个 `;`、`{` 或 `}` 之后
        let mut start = 0;
        for i in (0..spot.min(bytes.len())).rev() {
            if matches!(bytes[i], b';' | b'{' | b'}') {
                start = i + 1;
                break;
            }
        }
        // 向后找声明终点：下一个 `;`（含）；没有则到下一个 `}` 之前
        let mut end = bytes.len();
        for (i, &b) in bytes.iter().enumerate().skip(spot.min(bytes.len())) {
            if b == b';' {
                end = i + 1;
                break;
            }
            if b == b'}' {
                end = i;
                break;
            }
        }
        remove.push((start, end));
    }
    remove.sort_unstable();
    let mut out = String::with_capacity(src.len());
    let mut pos = 0;
    for (s, e) in remove {
        if s >= pos {
            out.push_str(&src[pos..s]);
            pos = e;
        } else if e > pos {
            pos = e;
        }
    }
    out.push_str(&src[pos.min(src.len())..]);
    out
}

/// 折叠插值结果中的换行为空格并 trim（对齐 Linaria `stripLines`）。
///
/// CSS 字符串字面量内不允许裸换行，插值来的多行文本（如对象展开）必须压成一行。
fn strip_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_ws = false;
    for c in text.chars() {
        if c == '\n' || c == '\r' {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

/// 把任意标识符文本规整为合法 CSS 类名前缀。
fn sanitize_ident(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // CSS 标识符不能以数字开头
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if out.is_empty() {
        out.push_str("css");
    }
    out
}

/// FNV-1a 32 位——与 `wake_css` 的 CSS Modules 作用域化同族算法，保证产物确定性。
fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// 生成合法的 JS 字符串字面量（`expression_replacements` 是裸文本插入，必须自行转义）。
fn js_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ======================================================================
// 极简 AST 递归（只需覆盖「表达式可能出现的位置」，不依赖 visit crate 的可变借用）
// ======================================================================

fn walk_statement_children<W: CssWalk + ?Sized>(c: &mut W, stmt: &Statement) {
    match stmt {
        Statement::Expression(e) => c.visit_expression(&e.expression),
        Statement::Return(r) => {
            if let Some(a) = &r.argument {
                c.visit_expression(a);
            }
        }
        Statement::Block(b) => {
            for s in b.body.iter() {
                c.visit_statement(s);
            }
        }
        Statement::If(i) => {
            c.visit_expression(&i.test);
            c.visit_statement(&i.consequent);
            if let Some(a) = &i.alternate {
                c.visit_statement(a);
            }
        }
        Statement::FunctionDeclaration(f) => {
            if let Some(body) = &f.body {
                for s in body.statements.iter() {
                    c.visit_statement(s);
                }
            }
        }
        Statement::ExportNamed(e) => {
            if let Some(d) = &e.declaration {
                c.visit_statement(d);
            }
        }
        Statement::ExportDefault(d) => {
            if let ExportDefaultKind::Expression(e) = &d.declaration {
                c.visit_expression(e);
            }
        }
        Statement::VariableDeclaration(d) => {
            for decl in d.declarations.iter() {
                if let Some(init) = &decl.init {
                    c.visit_expression(init);
                }
            }
        }
        _ => {}
    }
}

fn walk_expression_children<W: CssWalk + ?Sized>(c: &mut W, expr: &Expression) {
    match expr {
        Expression::Call(call) => {
            c.visit_expression(&call.callee);
            for a in call.arguments.iter() {
                c.visit_expression(a);
            }
        }
        Expression::New(n) => {
            c.visit_expression(&n.callee);
            for a in n.arguments.iter() {
                c.visit_expression(a);
            }
        }
        Expression::Member(m) => c.visit_expression(&m.object),
        Expression::Binary(b) => {
            c.visit_expression(&b.left);
            c.visit_expression(&b.right);
        }
        Expression::Logical(l) => {
            c.visit_expression(&l.left);
            c.visit_expression(&l.right);
        }
        Expression::Conditional(cond) => {
            c.visit_expression(&cond.test);
            c.visit_expression(&cond.consequent);
            c.visit_expression(&cond.alternate);
        }
        Expression::Assignment(a) => c.visit_expression(&a.right),
        Expression::Array(a) => {
            for el in a.elements.iter().flatten() {
                c.visit_expression(el);
            }
        }
        Expression::Object(o) => {
            for m in o.properties.iter() {
                match m {
                    ObjectMember::Property(p) => c.visit_expression(&p.value),
                    ObjectMember::Spread(s) => c.visit_expression(&s.argument),
                }
            }
        }
        Expression::Arrow(a) => match &a.body {
            ArrowBody::Expression(e) => c.visit_expression(e),
            ArrowBody::Block(b) => {
                for s in b.statements.iter() {
                    c.visit_statement(s);
                }
            }
        },
        Expression::Function(f) => {
            if let Some(body) = &f.body {
                for s in body.statements.iter() {
                    c.visit_statement(s);
                }
            }
        }
        Expression::TemplateLiteral(t) => {
            for e in t.expressions.iter() {
                c.visit_expression(e);
            }
        }
        Expression::TaggedTemplate(t) => {
            for e in t.quasi.expressions.iter() {
                c.visit_expression(e);
            }
        }
        Expression::Sequence(s) => {
            for e in s.expressions.iter() {
                c.visit_expression(e);
            }
        }
        Expression::Unary(u) => c.visit_expression(&u.argument),
        Expression::Await(a) => c.visit_expression(&a.argument),
        Expression::Spread(s) => c.visit_expression(&s.argument),
        _ => {}
    }
}

#[cfg(test)]
mod tests;
