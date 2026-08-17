//! # wake_css_in_js — `@crab-dev/css` 的安全零运行时编译器
//!
//! WAKE-COMPATIBILITY §M5。把源码里的 `` css`...` `` 标签模板在**构建期**求值并抽取为静态 CSS，
//! 表达式本身替换为类名字符串字面量——运行时不含任何样式计算。
//!
//! ```js
//! import { css } from '@crab-dev/css';
//! const box = css`
//!   padding: ${token.space};
//!   &:hover { color: red; }
//! `;
//! ```
//! 编译为 `const box = "box_a1b2c3";`，并产出
//! `.box_a1b2c3{padding:8px}.box_a1b2c3:hover{color:red}`。
//!
//! Wake 只解释一个无副作用、可证明的纯数据子集（[`value`]），绝不启动 JavaScript VM。
//! `@crab-dev/css` 的插值无法静态证明时会产生阻断构建的错误。

use wake_common::{Atom, Diagnostic, FxHashMap, FxHashSet, Interner, Span};
use wake_css::syntax::{
    CssSyntaxContext, CssSyntaxItem, CssSyntaxItemKind, CssSyntaxKind, CssSyntaxNode, CssSyntaxTree,
};
use wake_ecma_ast::*;
use wake_ecma_semantic::{DeclKind, SymbolId, analyze};

pub mod nesting;
pub mod value;

pub use value::{StaticExports, StaticValue};

const CRAB_CSS_SOURCE: &str = "@crab-dev/css";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BindingKind {
    Css,
    Cx,
    Keyframes,
    GlobalStyle,
    CreateVar,
    AssignVars,
}

/// A build-time tagged-template API recognized from `@crab-dev/css`.
///
/// This is also the editor language service's source of truth: callers must not infer a template
/// kind from the tag's spelling because aliases and lexical shadowing are semantically significant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CssTemplateKind {
    Css,
    Keyframes,
    GlobalStyle,
}

/// Stable source ranges for a recognized Crab CSS tagged template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CssTemplate {
    pub kind: CssTemplateKind,
    /// The complete tagged expression, including the tag.
    pub span: Span,
    /// The template literal, including its backticks when present.
    pub template_span: Span,
    /// Editable raw literal ranges in source order, excluding template delimiters.
    pub literal_spans: Vec<Span>,
    /// JavaScript expression ranges in source order, excluding `${` and `}` delimiters.
    pub interpolations: Vec<Span>,
}

/// Return the source range containing editable literal text for a template token.
///
/// `index` is the token's position in the template and `tail` says whether it is the final token.
/// The parser token spans include `` ` ``, `${` or `}` delimiters; this helper removes them without
/// decoding the raw template text.
pub fn css_template_literal_span(token: Span, _index: usize, tail: bool) -> Span {
    let lo = token.lo.saturating_add(1);
    let trailing = if tail { 1 } else { 2 };
    Span::new(
        lo.min(token.hi),
        token.hi.saturating_sub(trailing).max(lo.min(token.hi)),
    )
}

impl BindingKind {
    fn from_import(name: &str) -> Option<Self> {
        match name {
            "css" => Some(Self::Css),
            "cx" => Some(Self::Cx),
            "keyframes" => Some(Self::Keyframes),
            "globalStyle" => Some(Self::GlobalStyle),
            "createVar" => Some(Self::CreateVar),
            "assignVars" => Some(Self::AssignVars),
            _ => None,
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::Css => "class",
            Self::Keyframes => "keyframes",
            Self::CreateVar => "variable",
            Self::Cx | Self::GlobalStyle | Self::AssignVars => "runtime",
        }
    }

    fn template_kind(self) -> Option<CssTemplateKind> {
        match self {
            Self::Css => Some(CssTemplateKind::Css),
            Self::Keyframes => Some(CssTemplateKind::Keyframes),
            Self::GlobalStyle => Some(CssTemplateKind::GlobalStyle),
            Self::Cx | Self::CreateVar | Self::AssignVars => None,
        }
    }

    fn syntax_context(self) -> CssSyntaxContext {
        match self {
            Self::Css => CssSyntaxContext::StyleBlock,
            Self::Keyframes => CssSyntaxContext::Keyframes,
            Self::GlobalStyle => CssSyntaxContext::Stylesheet,
            Self::Cx | Self::CreateVar | Self::AssignVars => CssSyntaxContext::ComponentValues,
        }
    }
}

#[derive(Clone, Debug)]
struct StyleBinding {
    kind: BindingKind,
    local: Atom,
    symbol: SymbolId,
    declaration: Span,
}

/// Import bindings plus semantic references. Binding identity is a `SymbolId`, not a spelling,
/// so a parameter or block-local variable named `css` cannot be mistaken for the imported marker.
struct BindingRegistry {
    bindings: Vec<StyleBinding>,
    references: FxHashMap<Span, SymbolId>,
    /// Whether an identifier reference is backed by an immutable module binding that the
    /// allow-listed evaluator can safely read. Keeping this beside marker binding identity lets
    /// Crab templates reject a shadowing parameter/block binding instead of accidentally
    /// looking up a same-spelled top-level value in [`value::Scope`].
    static_references: FxHashMap<Span, bool>,
}

impl BindingRegistry {
    fn collect(program: &Program, interner: &Interner) -> Self {
        let semantic = analyze(program);
        let mut imported_symbols = FxHashMap::default();
        for (id, symbol) in semantic.symbols.iter().enumerate() {
            if symbol.decl_kind == DeclKind::Import {
                imported_symbols.insert(symbol.span, id as SymbolId);
            }
        }
        let references = semantic
            .references
            .iter()
            .filter_map(|reference| reference.resolved.map(|id| (reference.span, id)))
            .collect();
        let static_references = semantic
            .references
            .iter()
            .map(|reference| {
                let safe = match reference.resolved {
                    Some(symbol) => {
                        let symbol = &semantic.symbols[symbol as usize];
                        symbol.scope == 0
                            && matches!(symbol.decl_kind, DeclKind::Const | DeclKind::Import)
                    }
                    // `undefined` is the only unresolved identifier intentionally understood by
                    // the evaluator. Every other global would require executing host JavaScript.
                    None => interner.resolve(reference.name) == "undefined",
                };
                (reference.span, safe)
            })
            .collect();
        let mut bindings = Vec::new();
        for statement in program.body.iter() {
            let Statement::Import(import) = statement else {
                continue;
            };
            let source = interner.resolve(import.source);
            if !is_css_in_js_source(&source) {
                continue;
            }
            for specifier in import.specifiers.iter() {
                let ImportSpecifier::Named {
                    local, imported, ..
                } = specifier
                else {
                    continue;
                };
                let imported_name = match imported {
                    ModuleExportName::Ident(id) => interner.resolve(id.name),
                    ModuleExportName::String(atom) => interner.resolve(*atom),
                };
                let Some(kind) = BindingKind::from_import(&imported_name) else {
                    continue;
                };
                let Some(&symbol) = imported_symbols.get(&local.span) else {
                    continue;
                };
                bindings.push(StyleBinding {
                    kind,
                    local: local.name,
                    symbol,
                    declaration: local.span,
                });
            }
        }
        Self {
            bindings,
            references,
            static_references,
        }
    }

    fn binding_for_expression(&self, expression: &Expression) -> Option<&StyleBinding> {
        let Expression::Identifier(identifier) = expression else {
            return None;
        };
        let symbol = self.references.get(&identifier.span)?;
        self.bindings
            .iter()
            .find(|binding| binding.symbol == *symbol)
    }

    fn binding_for_declaration(&self, span: Span) -> Option<&StyleBinding> {
        self.bindings
            .iter()
            .find(|binding| binding.declaration == span)
    }

    fn expression_is_module_static(&self, expression: &Expression) -> bool {
        struct StaticReferenceCheck<'a> {
            references: &'a FxHashMap<Span, bool>,
            safe: bool,
        }

        impl<'ast> Visit<'ast> for StaticReferenceCheck<'_> {
            fn visit_expression(&mut self, expression: &Expression<'ast>) {
                if let Expression::Identifier(identifier) = expression {
                    if !self
                        .references
                        .get(&identifier.span)
                        .copied()
                        .unwrap_or(false)
                    {
                        self.safe = false;
                    }
                    return;
                }
                walk_expression(self, expression);
            }
        }

        let mut check = StaticReferenceCheck {
            references: &self.static_references,
            safe: true,
        };
        check.visit_expression(expression);
        check.safe
    }

    fn structured_values_may_escape_through_user_tags(&self, program: &Program) -> bool {
        struct UserTagCheck<'a> {
            bindings: &'a BindingRegistry,
            found: bool,
        }

        impl<'ast> Visit<'ast> for UserTagCheck<'_> {
            fn visit_expression(&mut self, expression: &Expression<'ast>) {
                if let Expression::TaggedTemplate(template) = expression {
                    let compiler_tag = self
                        .bindings
                        .binding_for_expression(&template.tag)
                        .is_some_and(|binding| {
                            matches!(
                                binding.kind,
                                BindingKind::Css
                                    | BindingKind::Keyframes
                                    | BindingKind::GlobalStyle
                            )
                        });
                    if !compiler_tag && !template.quasi.expressions.is_empty() {
                        self.found = true;
                        return;
                    }
                }
                walk_expression(self, expression);
            }
        }

        let mut check = UserTagCheck {
            bindings: self,
            found: false,
        };
        check.visit_program(program);
        check.found
    }
}

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
    let bindings = BindingRegistry::collect(program, interner);
    if bindings.bindings.is_empty() {
        return out;
    }
    // 只有被 export 的编译期值才需登记，供下游模块安全静态求值。
    let exported = exported_names(program, interner);
    for style in assign_style_names(program, interner, &bindings, seed) {
        if let Some(exported_as) = style
            .export_as
            .as_ref()
            .map(std::slice::from_ref)
            .or_else(|| exported.get(&style.declaration).map(Vec::as_slice))
        {
            for export_as in exported_as {
                out.insert(export_as.clone(), StaticValue::Str(style.value.clone()));
            }
        }
    }
    for variable in assign_create_vars(program, interner, &bindings, seed) {
        if let Some(exported_as) = exported.get(&variable.declaration) {
            for export_as in exported_as {
                out.insert(export_as.clone(), StaticValue::Str(variable.value.clone()));
            }
        }
    }
    out
}

/// 本地名 → 导出名（`export const x`、`export { x as y }`、`export default x`）。
fn exported_names(program: &Program, interner: &Interner) -> FxHashMap<Span, Vec<String>> {
    let mut m: FxHashMap<Span, Vec<String>> = FxHashMap::default();
    for stmt in program.body.iter() {
        match stmt {
            Statement::ExportNamed(e) => {
                if let Some(Statement::VariableDeclaration(d)) = e.declaration {
                    for decl in d.declarations.iter() {
                        if let Pattern::Ident(id) = &decl.id {
                            let n = interner.resolve(id.name);
                            m.entry(id.span).or_default().push(n);
                        }
                    }
                }
                if e.source.is_none() {
                    for spec in e.specifiers.iter() {
                        let local = match &spec.local {
                            ModuleExportName::Ident(id) => id,
                            ModuleExportName::String(_) => continue,
                        };
                        let exported = match &spec.exported {
                            ModuleExportName::Ident(id) => interner.resolve(id.name),
                            ModuleExportName::String(a) => interner.resolve(*a),
                        };
                        // Resolve the local export back to its declaration span, so a nested
                        // binding with the same spelling cannot overwrite the exported style.
                        if let Some(declaration) = top_level_declaration(program, local.name) {
                            m.entry(declaration).or_default().push(exported);
                        }
                    }
                }
            }
            Statement::ExportDefault(d) => {
                if let ExportDefaultKind::Expression(Expression::Identifier(id)) = &d.declaration
                    && let Some(declaration) = top_level_declaration(program, id.name)
                {
                    m.entry(declaration)
                        .or_default()
                        .push("default".to_string());
                }
            }
            _ => {}
        }
    }
    m
}

fn top_level_declaration(program: &Program, name: Atom) -> Option<Span> {
    for statement in program.body.iter() {
        let Some(declaration) = (match statement {
            Statement::VariableDeclaration(declaration) => Some(*declaration),
            Statement::ExportNamed(export) => export.declaration.and_then(|statement| {
                if let Statement::VariableDeclaration(declaration) = statement {
                    Some(declaration)
                } else {
                    None
                }
            }),
            _ => None,
        }) else {
            continue;
        };
        for declarator in declaration.declarations.iter() {
            if let Pattern::Ident(identifier) = &declarator.id
                && identifier.name == name
            {
                return Some(identifier.span);
            }
        }
    }
    None
}

/// 一次模块转换的产出。
#[derive(Debug, Default)]
pub struct TransformResult {
    /// `span → 替换源码`（标签模板 span → JS 字符串字面量）。喂给 codegen 的
    /// `MinifyCtx::expression_replacements`。
    pub replacements: FxHashMap<Span, String>,
    /// 替换文本直接复用了原源码的表达式范围；mangler 必须保留其中引用的绑定名。
    pub verbatim_replacement_spans: Vec<Span>,
    /// 已被完整静态消解、可从 codegen 删除的 CSS-in-JS import 语句。
    pub removable_import_spans: Vec<Span>,
    /// mixed import 中已静态消解、可不生成局部读取的 import binding span。
    pub removable_import_binding_spans: Vec<Span>,
    /// 本模块抽取出的 CSS（已按声明序拼接）。
    pub css: String,
    /// 静态求值与 API 使用诊断；不安全插值为 error。
    pub diagnostics: Vec<Diagnostic>,
}

impl TransformResult {
    pub fn is_empty(&self) -> bool {
        self.replacements.is_empty() && self.css.is_empty()
    }
}

/// 唯一受支持的 CSS-in-JS 包。
///
/// 限定来源可避免误伤同名的普通函数（如自定义的 `css()` 工具）。
pub const CSS_IN_JS_SOURCES: &[&str] = &[CRAB_CSS_SOURCE];

/// 该模块说明符是否是 CSS-in-JS 的来源包。
///
/// 供打包器**在扫描后、codegen 前**廉价判断「本次构建是否用得上 CSS-in-JS」：全项目无人
/// import 时可整体跳过静态导出求值，使未用 Crab CSS 的项目零开销（故本功能可默认开启）。
pub fn is_css_in_js_source(specifier: &str) -> bool {
    CSS_IN_JS_SOURCES.contains(&specifier)
}

/// Return imported locals that the CSS compiler consumes completely.
///
/// The bundler uses this before graph liveness is computed. Without this hand-off, an import such
/// as `import { css, createVar, assignVars } from '@crab-dev/css'` would keep the runtime exports for
/// `css` and `createVar` alive even though codegen replaces every reference. This analysis uses the
/// same semantic binding identity and conservative rules as [`transform`]: any non-marker reference
/// makes the import runtime-live.
pub fn compiler_consumed_imports(program: &Program, interner: &Interner) -> FxHashSet<Atom> {
    let bindings = BindingRegistry::collect(program, interner);
    if bindings.bindings.is_empty() {
        return FxHashSet::default();
    }
    let consumed_create_var_calls: Vec<Span> = assign_create_vars(program, interner, &bindings, "")
        .into_iter()
        .map(|variable| variable.span)
        .collect();
    let mut usage = CompilerImportUsage {
        bindings: &bindings,
        safe: bindings
            .bindings
            .iter()
            .map(|binding| {
                (
                    binding.symbol,
                    matches!(
                        binding.kind,
                        BindingKind::Css
                            | BindingKind::Keyframes
                            | BindingKind::GlobalStyle
                            | BindingKind::CreateVar
                    ),
                )
            })
            .collect(),
        consumed_create_var_calls: &consumed_create_var_calls,
    };
    usage.visit_program(program);
    bindings
        .bindings
        .iter()
        .filter(|binding| usage.safe.get(&binding.symbol).copied().unwrap_or(false))
        .map(|binding| binding.local)
        .collect()
}

/// Discover tagged templates governed by the Crab CSS compiler contract.
///
/// Results are in source traversal order and use the same binding registry as [`transform`].
pub fn discover_css_templates(program: &Program, interner: &Interner) -> Vec<CssTemplate> {
    struct TemplateCollector<'a> {
        bindings: &'a BindingRegistry,
        templates: Vec<CssTemplate>,
    }

    impl<'ast> Visit<'ast> for TemplateCollector<'_> {
        fn visit_expression(&mut self, expression: &Expression<'ast>) {
            if let Expression::TaggedTemplate(template) = expression
                && let Some(binding) = self.bindings.binding_for_expression(&template.tag)
                && let Some(kind) = binding.kind.template_kind()
            {
                self.templates.push(CssTemplate {
                    kind,
                    span: template.span,
                    template_span: template.quasi.span,
                    literal_spans: template
                        .quasi
                        .quasis
                        .iter()
                        .enumerate()
                        .map(|(index, quasi)| {
                            css_template_literal_span(quasi.span, index, quasi.tail)
                        })
                        .collect(),
                    interpolations: template
                        .quasi
                        .expressions
                        .iter()
                        .map(Expression::span)
                        .collect(),
                });
            }
            walk_expression(self, expression);
        }
    }

    let bindings = BindingRegistry::collect(program, interner);
    let mut collector = TemplateCollector {
        bindings: &bindings,
        templates: Vec::new(),
    };
    collector.visit_program(program);
    collector.templates
}

struct CompilerImportUsage<'a> {
    bindings: &'a BindingRegistry,
    safe: FxHashMap<SymbolId, bool>,
    consumed_create_var_calls: &'a [Span],
}

impl<'ast> Visit<'ast> for CompilerImportUsage<'_> {
    fn visit_statement(&mut self, statement: &Statement<'ast>) {
        if let Statement::ExportNamed(export) = statement
            && export.source.is_none()
        {
            for specifier in export.specifiers.iter() {
                if let ModuleExportName::Ident(identifier) = &specifier.local
                    && let Some(binding) = self
                        .bindings
                        .bindings
                        .iter()
                        .find(|binding| binding.local == identifier.name)
                {
                    self.safe.insert(binding.symbol, false);
                }
            }
        }
        walk_statement(self, statement);
    }

    fn visit_expression(&mut self, expression: &Expression<'ast>) {
        if let Expression::TaggedTemplate(template) = expression
            && self
                .bindings
                .binding_for_expression(&template.tag)
                .is_some_and(|binding| {
                    matches!(
                        binding.kind,
                        BindingKind::Css | BindingKind::Keyframes | BindingKind::GlobalStyle
                    )
                })
        {
            for interpolation in template.quasi.expressions.iter() {
                self.visit_expression(interpolation);
            }
            return;
        }
        if let Expression::Call(call) = expression
            && let Some(binding) = self.bindings.binding_for_expression(&call.callee)
            && binding.kind == BindingKind::CreateVar
            && self.consumed_create_var_calls.contains(&call.span)
        {
            for argument in call.arguments.iter() {
                self.visit_expression(argument);
            }
            return;
        }
        if let Expression::Identifier(identifier) = expression
            && let Some(symbol) = self.bindings.references.get(&identifier.span)
            && self.safe.contains_key(symbol)
        {
            self.safe.insert(*symbol, false);
        }
        walk_expression(self, expression);
    }
}

/// 转换一个模块。
///
/// - `seed`：类名 hash 的种子，应为模块路径（保证跨模块不撞名、同输入稳定）。
/// - `imported`：本模块 import 绑定的已求值静态值（本地名 → 值），由调用方按
///   [`collect_static_exports`] 跨模块解析后传入。
pub fn transform(
    program: &Program,
    interner: &Interner,
    source: &str,
    seed: &str,
    imported: &value::Scope,
) -> TransformResult {
    let mut out = TransformResult::default();

    // 1) 以语义符号而非名字登记编译期 marker，支持 import alias 且正确处理局部遮蔽。
    let bindings = BindingRegistry::collect(program, interner);
    if bindings.bindings.is_empty() {
        return out;
    }

    // 2) 模块顶层常量（可引用 import 进来的值）。
    let mut imported = value::safe_imported_scope(program, interner, imported);
    // An exporting module cannot prove that a different importer did not mutate a shared object
    // before this module executes. Until graph-wide provenance/mutation analysis is available,
    // propagate only copy-safe primitive/class/variable values across ESM.
    imported.retain(|_, value| !matches!(value, StaticValue::Obj(_) | StaticValue::Arr(_)));
    let mut scope = value::collect_module_scope(program, interner, &imported);
    if bindings.structured_values_may_escape_through_user_tags(program) {
        scope.retain(|_, value| !matches!(value, StaticValue::Obj(_) | StaticValue::Arr(_)));
    }

    // 2′) 预分配 css/keyframes 名称，打破样式互相引用时的求值循环。
    for style in assign_style_names(program, interner, &bindings, seed) {
        scope.insert(style.local, value::StaticValue::Str(style.value));
    }

    // 2″) `createVar()` 是编译器内建 marker：静态替换成可直接插值的 `var(--…)`，动态值
    // 只通过 `assignVars` 进入 inline style，绝不需要执行用户模块。
    let create_vars = assign_create_vars(program, interner, &bindings, seed);
    let mut consumed_create_var_calls = Vec::with_capacity(create_vars.len());
    for variable in create_vars {
        scope.insert(
            variable.local,
            value::StaticValue::Str(variable.value.clone()),
        );
        out.replacements
            .insert(variable.span, js_string_literal(&variable.value));
        consumed_create_var_calls.push(variable.span);
    }

    let ctx = value::EvalCtx {
        interner,
        scope: &scope,
        imported: &imported,
    };

    // 3) 抽取 css/keyframes/globalStyle，并写入 span replacement。
    {
        let mut collector = Collector {
            interner,
            bindings: &bindings,
            ctx: &ctx,
            seed,
            out: &mut out,
            name_hint: None,
            counters: FxHashMap::default(),
            allow_global_style: false,
        };
        collector.visit_program(program);
    }

    // 4) `cx` 的 atomic-class 冲突语义不能盲目降级成 join。只有能证明每个可能出现的
    // class 都是单个非 atomic token 时才替换；未知调用保留原包依赖。
    let mut usage = CssInJsUsage {
        source,
        bindings: &bindings,
        ctx: &ctx,
        out: &mut out,
        safe: bindings
            .bindings
            .iter()
            .map(|binding| (binding.symbol, true))
            .collect(),
        consumed_create_var_calls: &consumed_create_var_calls,
    };
    usage.visit_program(program);

    // import 的全部 specifier 都已消解时删整条语句，codegen 不再发出 require，
    // 随后的 dead-module elimination 就能把 @crab-dev/css 从模块图中移除。
    for stmt in program.body.iter() {
        let Statement::Import(imp) = stmt else {
            continue;
        };
        if !is_css_in_js_source(&interner.resolve(imp.source)) || imp.specifiers.is_empty() {
            continue;
        }
        let consumed: Vec<Span> = imp
            .specifiers
            .iter()
            .filter_map(|specifier| {
                let ImportSpecifier::Named { local, .. } = specifier else {
                    return None;
                };
                usage
                    .bindings
                    .binding_for_declaration(local.span)
                    .and_then(|binding| usage.safe.get(&binding.symbol))
                    .copied()
                    .unwrap_or(false)
                    .then_some(local.span)
            })
            .collect();
        if consumed.len() == imp.specifiers.len() {
            usage.out.removable_import_spans.push(imp.span);
        } else {
            usage.out.removable_import_binding_spans.extend(consumed);
        }
    }
    out
}

struct CssInJsUsage<'a, 'b> {
    source: &'a str,
    bindings: &'a BindingRegistry,
    ctx: &'a value::EvalCtx<'a>,
    out: &'b mut TransformResult,
    safe: FxHashMap<SymbolId, bool>,
    consumed_create_var_calls: &'a [Span],
}

impl<'ast> Visit<'ast> for CssInJsUsage<'_, '_> {
    fn visit_statement(&mut self, statement: &Statement<'ast>) {
        if let Statement::ExportNamed(export) = statement
            && export.source.is_none()
        {
            for specifier in export.specifiers.iter() {
                if let ModuleExportName::Ident(identifier) = &specifier.local
                    && let Some(binding) = self
                        .bindings
                        .bindings
                        .iter()
                        .find(|binding| binding.local == identifier.name)
                {
                    self.safe.insert(binding.symbol, false);
                }
            }
        }
        walk_statement(self, statement);
    }

    fn visit_expression(&mut self, expr: &Expression<'ast>) {
        if let Expression::TaggedTemplate(tt) = expr
            && self
                .bindings
                .binding_for_expression(&tt.tag)
                .is_some_and(|binding| {
                    matches!(
                        binding.kind,
                        BindingKind::Css | BindingKind::Keyframes | BindingKind::GlobalStyle
                    )
                })
        {
            for expression in tt.quasi.expressions.iter() {
                self.visit_expression(expression);
            }
            return;
        }
        if let Expression::Call(call) = expr
            && let Some(binding) = self.bindings.binding_for_expression(&call.callee)
            && binding.kind == BindingKind::Cx
        {
            if call.optional
                || !call
                    .arguments
                    .iter()
                    .all(|arg| known_non_atomic_class(arg, self.ctx))
            {
                self.safe.insert(binding.symbol, false);
            } else if let Some(replacement) = cx_replacement(call, self.source) {
                self.out.replacements.insert(call.span, replacement);
                self.out.verbatim_replacement_spans.push(call.span);
            } else {
                self.safe.insert(binding.symbol, false);
            }
            for argument in call.arguments.iter() {
                self.visit_expression(argument);
            }
            return;
        }
        if let Expression::Call(call) = expr
            && let Some(binding) = self.bindings.binding_for_expression(&call.callee)
            && binding.kind == BindingKind::CreateVar
            && self.consumed_create_var_calls.contains(&call.span)
        {
            for argument in call.arguments.iter() {
                self.visit_expression(argument);
            }
            return;
        }
        if let Expression::Identifier(id) = expr
            && let Some(symbol) = self.bindings.references.get(&id.span)
            && self.safe.contains_key(symbol)
        {
            self.safe.insert(*symbol, false);
        }
        walk_expression(self, expr);
    }
}

fn known_non_atomic_class(expr: &Expression, ctx: &value::EvalCtx) -> bool {
    match value::eval(expr, ctx) {
        Some(StaticValue::Str(value)) => {
            value.is_empty()
                || (!value.chars().any(char::is_whitespace) && !value.starts_with("atm_"))
        }
        Some(StaticValue::Null | StaticValue::Undefined | StaticValue::Bool(false)) => true,
        _ => match expr {
            // `condition && knownClass`: truthy 时一定得到右侧 class，falsy 时会被 filter 删除。
            Expression::Logical(logical) if logical.operator == LogicalOperator::And => {
                known_non_atomic_class(&logical.right, ctx)
            }
            Expression::Conditional(conditional) => {
                known_non_atomic_class(&conditional.consequent, ctx)
                    && known_non_atomic_class(&conditional.alternate, ctx)
            }
            _ => false,
        },
    }
}

fn cx_replacement(call: &CallExpression, source: &str) -> Option<String> {
    let mut out = String::from("[");
    for (index, argument) in call.arguments.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        let span = argument.span();
        out.push_str(source.get(span.lo as usize..span.hi as usize)?);
    }
    out.push_str("].filter(Boolean).join(\" \")");
    Some(out)
}

/// 名称身份由「schema + 规范化模块 id + API 种类 + binding 名 + 同名 ordinal」组成，刻意
/// 不混入 CSS 内容。向文件前方插入另一个不同 binding 的样式不会让已有名称 churn。
fn generated_name(kind: BindingKind, hint: &str, seed: &str, ordinal: u32) -> String {
    const SCHEMA: &str = "crab-css-v1";
    let seed = normalize_style_seed(seed);
    let key = format!("{SCHEMA}\0{seed}\0{}\0{hint}\0{ordinal}", kind.slug());
    format!(
        "{}_{:012x}",
        sanitize_ident(hint),
        fnv1a64(&key) & 0x0000_ffff_ffff_ffff
    )
}

fn normalize_style_seed(seed: &str) -> String {
    let mut normalized = seed.replace('\\', "/");
    if let Some(rest) = normalized.strip_prefix("//?/") {
        normalized = rest.to_string();
    }
    if normalized.as_bytes().get(1) == Some(&b':') {
        normalized.replace_range(0..1, &normalized[..1].to_ascii_lowercase());
    }
    normalized
}

fn next_ordinal(counters: &mut FxHashMap<String, u32>, kind: BindingKind, hint: &str) -> u32 {
    let key = format!("{}\0{hint}", kind.slug());
    let slot = counters.entry(key).or_default();
    let ordinal = *slot;
    *slot += 1;
    ordinal
}

/// 前序遍历，为 `css` 与 `keyframes` 预分配名称。
struct AssignedStyle {
    local: String,
    declaration: Span,
    export_as: Option<String>,
    value: String,
}

fn assign_style_names(
    program: &Program,
    interner: &Interner,
    bindings: &BindingRegistry,
    seed: &str,
) -> Vec<AssignedStyle> {
    let mut assigner = NameAssigner {
        interner,
        bindings,
        seed,
        out: Vec::new(),
        name_hint: None,
        counters: FxHashMap::default(),
    };
    assigner.visit_program(program);
    assigner.out
}

struct NameAssigner<'a> {
    interner: &'a Interner,
    bindings: &'a BindingRegistry,
    seed: &'a str,
    out: Vec<AssignedStyle>,
    name_hint: Option<(String, Span)>,
    counters: FxHashMap<String, u32>,
}

impl<'ast> Visit<'ast> for NameAssigner<'_> {
    fn visit_statement(&mut self, statement: &Statement<'ast>) {
        if let Statement::VariableDeclaration(declaration) = statement {
            for declarator in declaration.declarations.iter() {
                let hint = match &declarator.id {
                    Pattern::Ident(identifier) => {
                        Some((self.interner.resolve(identifier.name), identifier.span))
                    }
                    _ => None,
                };
                if let Some(initializer) = &declarator.init {
                    let saved = std::mem::replace(&mut self.name_hint, hint);
                    self.visit_expression(initializer);
                    self.name_hint = saved;
                }
            }
            return;
        }
        if let Statement::ExportDefault(export) = statement
            && let ExportDefaultKind::Expression(expression) = &export.declaration
        {
            let saved = self.name_hint.replace(("default".to_string(), export.span));
            let before = self.out.len();
            self.visit_expression(expression);
            if let Some(style) = self.out.get_mut(before) {
                style.export_as = Some("default".to_string());
            }
            self.name_hint = saved;
            return;
        }
        walk_statement(self, statement);
    }

    fn visit_expression(&mut self, expression: &Expression<'ast>) {
        if let Expression::TaggedTemplate(template) = expression
            && let Some(binding) = self.bindings.binding_for_expression(&template.tag)
            && matches!(binding.kind, BindingKind::Css | BindingKind::Keyframes)
        {
            let fallback = if binding.kind == BindingKind::Css {
                "css"
            } else {
                "keyframes"
            };
            let hint = self
                .name_hint
                .as_ref()
                .map(|(name, _)| name.as_str())
                .unwrap_or(fallback);
            let ordinal = next_ordinal(&mut self.counters, binding.kind, hint);
            let generated = generated_name(binding.kind, hint, self.seed, ordinal);
            if let Some((local, declaration)) = self.name_hint.clone() {
                self.out.push(AssignedStyle {
                    local,
                    declaration,
                    export_as: None,
                    value: generated,
                });
            }
            return;
        }
        walk_expression(self, expression);
    }
}

struct AssignedVariable {
    local: String,
    declaration: Span,
    value: String,
    span: Span,
}

/// `createVar` 只在模块顶层 immutable binding 上静态消解；其它位置保留小型 runtime。
fn assign_create_vars(
    program: &Program,
    interner: &Interner,
    bindings: &BindingRegistry,
    seed: &str,
) -> Vec<AssignedVariable> {
    let mut out = Vec::new();
    for statement in program.body.iter() {
        let declaration = match statement {
            Statement::VariableDeclaration(declaration) => Some(*declaration),
            Statement::ExportNamed(export) => export.declaration.and_then(|declaration| {
                if let Statement::VariableDeclaration(declaration) = declaration {
                    Some(declaration)
                } else {
                    None
                }
            }),
            _ => None,
        };
        let Some(declaration) = declaration else {
            continue;
        };
        if declaration.kind != VarKind::Const {
            continue;
        }
        for declarator in declaration.declarations.iter() {
            let (Pattern::Ident(identifier), Some(Expression::Call(call))) =
                (&declarator.id, &declarator.init)
            else {
                continue;
            };
            let Some(binding) = bindings.binding_for_expression(&call.callee) else {
                continue;
            };
            if binding.kind != BindingKind::CreateVar
                || call.optional
                || call.arguments.len() > 1
                || call
                    .arguments
                    .first()
                    .is_some_and(|argument| !matches!(argument, Expression::StringLiteral(_)))
            {
                continue;
            }
            let local = interner.resolve(identifier.name);
            let slug = generated_name(BindingKind::CreateVar, &local, seed, 0);
            out.push(AssignedVariable {
                local,
                declaration: identifier.span,
                value: format!("var(--crab-css-{slug})"),
                span: call.span,
            });
        }
    }
    out
}

struct Collector<'a, 'b> {
    interner: &'a Interner,
    bindings: &'a BindingRegistry,
    ctx: &'a value::EvalCtx<'a>,
    seed: &'a str,
    out: &'b mut TransformResult,
    name_hint: Option<String>,
    counters: FxHashMap<String, u32>,
    allow_global_style: bool,
}

impl<'ast> Visit<'ast> for Collector<'_, '_> {
    fn visit_program(&mut self, program: &Program<'ast>) {
        for statement in program.body.iter() {
            if let Statement::Expression(expression_statement) = statement
                && let Expression::TaggedTemplate(template) = &expression_statement.expression
                && self
                    .bindings
                    .binding_for_expression(&template.tag)
                    .is_some_and(|binding| binding.kind == BindingKind::GlobalStyle)
            {
                let saved = std::mem::replace(&mut self.allow_global_style, true);
                self.visit_expression(&expression_statement.expression);
                self.allow_global_style = saved;
            } else {
                self.visit_statement(statement);
            }
        }
    }

    fn visit_statement(&mut self, statement: &Statement<'ast>) {
        if let Statement::VariableDeclaration(declaration) = statement {
            for declarator in declaration.declarations.iter() {
                let hint = match &declarator.id {
                    Pattern::Ident(identifier) => Some(self.interner.resolve(identifier.name)),
                    _ => None,
                };
                if let Some(initializer) = &declarator.init {
                    let saved = std::mem::replace(&mut self.name_hint, hint);
                    self.visit_expression(initializer);
                    self.name_hint = saved;
                }
            }
            return;
        }
        if let Statement::ExportDefault(export) = statement
            && let ExportDefaultKind::Expression(expression) = &export.declaration
        {
            let saved = self.name_hint.replace("default".to_string());
            self.visit_expression(expression);
            self.name_hint = saved;
            return;
        }
        walk_statement(self, statement);
    }

    fn visit_expression(&mut self, expression: &Expression<'ast>) {
        if let Expression::TaggedTemplate(template) = expression
            && let Some(binding) = self.bindings.binding_for_expression(&template.tag)
            && matches!(
                binding.kind,
                BindingKind::Css | BindingKind::Keyframes | BindingKind::GlobalStyle
            )
        {
            self.handle_template(template, binding);
            return;
        }
        walk_expression(self, expression);
    }
}

impl Collector<'_, '_> {
    fn handle_template(&mut self, template: &TaggedTemplateExpression, binding: &StyleBinding) {
        let label = match binding.kind {
            BindingKind::Css => "css",
            BindingKind::Keyframes => "keyframes",
            BindingKind::GlobalStyle => "globalStyle",
            _ => return,
        };
        let (body, mut diagnostics) = render_template(
            template.quasi,
            self.ctx,
            self.interner,
            self.bindings,
            label,
            binding.kind.syntax_context(),
        );
        self.out.diagnostics.append(&mut diagnostics);
        let syntax = CssSyntaxTree::parse_with_context(
            &body,
            Span::new(0, body.len() as u32),
            binding.kind.syntax_context(),
        );

        if binding.kind == BindingKind::Css && contains_global_escape(&syntax.nodes, &syntax.items)
        {
            self.out.diagnostics.push(
                Diagnostic::error(
                    "css`` 中的 :global 逃逸无法跟随局部 binding 做可靠的样式存活分析",
                )
                .with_code("CRAB_CSS_GLOBAL_ESCAPE")
                .with_primary(template.span, "请把全局规则改为模块顶层的 globalStyle``"),
            );
            return;
        }
        if binding.kind == BindingKind::Css
            && let Some(at_rule) = unsupported_scoped_at_rule(&syntax.nodes, &syntax.items)
        {
            self.out.diagnostics.push(
                Diagnostic::error(format!(
                    "css`` 中的 @{at_rule} 会产生无法跟随局部 binding 摇树的全局副作用"
                ))
                .with_code("CRAB_CSS_GLOBAL_AT_RULE")
                .with_primary(template.span, "请把该规则移到模块顶层的 globalStyle``"),
            );
            return;
        }
        if contains_relative_css_url(&syntax.nodes) {
            self.out.diagnostics.push(
                Diagnostic::error(
                    "@crab-dev/css 暂不支持模板中的相对 url() 资源重写",
                )
                .with_code("CRAB_CSS_RELATIVE_URL")
                .with_primary(template.span, "相对资源在聚合 CSS 中会改变解析基准")
                .with_note(
                    "请暂时把含相对 url() 的规则放入普通 .css 文件；绝对 URL、根路径、data: 和片段引用仍可用",
                ),
            );
            return;
        }

        match binding.kind {
            BindingKind::Css | BindingKind::Keyframes => {
                let fallback = if binding.kind == BindingKind::Css {
                    "css"
                } else {
                    "keyframes"
                };
                let hint = self.name_hint.as_deref().unwrap_or(fallback);
                let ordinal = next_ordinal(&mut self.counters, binding.kind, hint);
                let generated = generated_name(binding.kind, hint, self.seed, ordinal);
                if binding.kind == BindingKind::Css {
                    self.out.css.push_str(&nesting::flatten_tree(
                        &format!(".{generated}"),
                        &body,
                        &syntax,
                    ));
                } else {
                    self.out.css.push_str("@keyframes ");
                    self.out.css.push_str(&generated);
                    self.out.css.push('{');
                    self.out.css.push_str(&body);
                    self.out.css.push('}');
                }
                self.out
                    .replacements
                    .insert(template.span, js_string_literal(&generated));
            }
            BindingKind::GlobalStyle => {
                if !self.allow_global_style {
                    self.out.diagnostics.push(
                        Diagnostic::error(
                            "globalStyle`` 必须是模块顶层的直接表达式语句，不能位于函数或控制流中",
                        )
                        .with_code("CRAB_CSS_GLOBAL_SCOPE")
                        .with_primary(template.span, "把 globalStyle`` 移到模块顶层"),
                    );
                    return;
                }
                self.out
                    .css
                    .push_str(&nesting::flatten_tree("", &body, &syntax));
                self.out
                    .replacements
                    .insert(template.span, "void 0".to_string());
            }
            _ => {}
        }
    }
}

fn contains_global_escape(nodes: &[CssSyntaxNode], items: &[CssSyntaxItem]) -> bool {
    items.iter().any(|item| {
        (matches!(item.kind, CssSyntaxItemKind::QualifiedRule)
            && selector_contains_global_escape(item.nodes(nodes)))
            || item
                .block(nodes)
                .is_some_and(|block| contains_global_escape(&block.children, &item.children))
    })
}

fn selector_contains_global_escape(nodes: &[CssSyntaxNode]) -> bool {
    for (index, node) in nodes.iter().enumerate() {
        if matches!(node.kind, CssSyntaxKind::Colon)
            && nodes[index + 1..]
                .iter()
                .find(|candidate| !candidate.is_trivia())
                .is_some_and(|candidate| {
                matches!(&candidate.kind, CssSyntaxKind::Function(name) if name.eq_ignore_ascii_case("global"))
            })
        {
            return true;
        }
        if selector_contains_global_escape(&node.children) {
            return true;
        }
    }
    false
}

fn contains_relative_css_url(nodes: &[CssSyntaxNode]) -> bool {
    nodes.iter().any(|node| match &node.kind {
        CssSyntaxKind::Url(value) => !is_absolute_css_url(value.trim()),
        CssSyntaxKind::Function(name) if name.eq_ignore_ascii_case("url") => {
            let value = node.children.iter().find(|child| !child.is_trivia());
            match value.map(|child| &child.kind) {
                Some(CssSyntaxKind::QuotedString(value) | CssSyntaxKind::Url(value)) => {
                    !is_absolute_css_url(value.trim())
                }
                _ => true,
            }
        }
        _ => contains_relative_css_url(&node.children),
    })
}

fn is_absolute_css_url(value: &str) -> bool {
    if value.starts_with(['/', '#']) {
        return true;
    }
    let Some(colon) = value.find(':') else {
        return false;
    };
    let scheme = &value[..colon];
    !scheme.is_empty()
        && scheme.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || (index > 0 && (byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')))
        })
}

fn unsupported_scoped_at_rule(nodes: &[CssSyntaxNode], items: &[CssSyntaxItem]) -> Option<String> {
    for item in items {
        if let CssSyntaxItemKind::AtRule { name } = &item.kind {
            let normalized = name.to_ascii_lowercase();
            // Only rules whose block is safely scoped by nesting::flatten_tree are allowed.
            // Unknown rules fail closed because many are global statements or descriptors.
            if !matches!(
                normalized.as_str(),
                "media" | "supports" | "container" | "scope" | "document" | "keyframes" | "layer"
            ) {
                return Some(normalized);
            }
            if normalized == "layer" && item.block_index.is_none() {
                return Some(normalized);
            }
        }
        if let Some(block) = item.block(nodes)
            && let Some(name) = unsupported_scoped_at_rule(&block.children, &item.children)
        {
            return Some(name);
        }
    }
    None
}

/// 渲染标签模板为 CSS 文本：静态片段原样，插值求值后替换。
///
/// 无法安全求值的插值会产生错误；为避免产出非法 CSS，同时丢弃其所在的那条声明。
fn render_template(
    quasi: &TemplateLiteral,
    ctx: &value::EvalCtx,
    interner: &Interner,
    bindings: &BindingRegistry,
    tag_name: &str,
    syntax_context: CssSyntaxContext,
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
        // 插值分派：
        //   undefined / "" → 静默跳过（不追加任何文本，也不报警）
        //   可 CSS 化的值（字符串/数字/数组/对象）→ toCSS 后折行为空格
        //   其余（含函数、无法静态求值）→ 报错并回删该条声明
        let evaluated = if bindings.expression_is_module_static(expr) {
            value::eval(expr, ctx)
        } else {
            None
        };
        if matches!(
            evaluated,
            Some(value::StaticValue::Undefined)
                | Some(value::StaticValue::Null)
                | Some(value::StaticValue::Bool(false))
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
                    Diagnostic::error(format!(
                        "{tag_name}`` 插值无法安全地在构建期求值"
                    ))
                    .with_code("CRAB_CSS_STATIC_VALUE")
                    .with_primary(expr.span(), "此插值不是可静态求值的纯表达式")
                    .with_note(
                        "支持：字面量、模板字符串、对象/数组字面量、成员访问、顶层 const 与它们的静态 ESM import",
                    )
                    .with_note(
                        "动态值请使用 createVar() 声明 CSS 变量，并通过 assignVars() 显式赋值；Wake 不执行用户模块或函数",
                    ),
                );
            }
        }
    }

    if !bad_spots.is_empty() {
        out = drop_declarations_at(&out, &bad_spots, syntax_context);
    }
    (out, diags)
}

/// 删除包含指定位置的声明。声明边界由共享 CSS CST 提供。
fn drop_declarations_at(src: &str, spots: &[usize], syntax_context: CssSyntaxContext) -> String {
    let tree =
        CssSyntaxTree::parse_with_context(src, Span::new(0, src.len() as u32), syntax_context);
    let mut remove = spots
        .iter()
        .filter_map(|spot| {
            let offset = (*spot).min(src.len()) as u32;
            tree.declarations
                .iter()
                .find(|declaration| declaration.span.lo <= offset && offset <= declaration.span.hi)
                .map(|declaration| declaration.span)
        })
        .collect::<Vec<_>>();
    remove.sort_by_key(|span| (span.lo, span.hi));
    remove.dedup();
    let mut out = String::with_capacity(src.len());
    let mut pos = 0;
    for span in remove {
        let s = span.lo as usize;
        let e = span.hi as usize;
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

/// 折叠插值结果中的换行为空格并 trim。
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

/// FNV-1a 64 位；名称展示 48 位，碰撞面显著大于旧实现的 32 位且无需引入运行时依赖。
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
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

#[cfg(test)]
mod tests;
