//! 作用域 / 符号 / 引用分析（PLAN §2.5，DESIGN §4.5）。
//!
//! Phase 2 先做 **后置遍历** 版（一遍 Visit 建 scope 树 + symbol 表 + 解析引用）。DESIGN 目标是
//! 保持独立语义所有权，供 tree-shaking（P6）与 minifier（P7）复用。是否融合遍历属于未来
//! 性能实验，不能重新引入 parser façade 或反向依赖。
//!
//! 独立 crate 边界避免 parser 改动触发 minifier 与 codegen 的级联重编译。
//!
//! 覆盖：作用域层级（module/function/block/catch）、var/function 提升（hoisting）、
//! let/const/class 块级绑定、参数/导入/catch 绑定、标识符引用解析（解析不到 = 全局/未声明）。

use wake_common::{Atom, FxHashMap, FxHashSet, Span};
use wake_ecma_ast::*;

/// 作用域 id（`scopes` 索引）。
pub type ScopeId = u32;
/// 符号 id（`symbols` 索引）。
pub type SymbolId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    /// 模块/脚本顶层。
    Module,
    /// 函数体（var 提升到此）。
    Function,
    /// 块 `{}` / for / switch 等。
    Block,
    /// catch 子句。
    Catch,
}

/// 声明类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclKind {
    Var,
    Let,
    Const,
    Function,
    Class,
    Param,
    Import,
    CatchParam,
    /// `using` / `await using` 绑定。作用域规则同 `Const`，但**带副作用**（离开作用域时
    /// 调用 dispose），故 minify 的「无引用即删」「单次引用内联」都必须放过它。
    Using,
}

#[derive(Debug)]
pub struct Scope {
    pub kind: ScopeKind,
    pub parent: Option<ScopeId>,
    /// 名字 → 符号（本作用域直接绑定）。
    pub bindings: FxHashMap<Atom, SymbolId>,
}

#[derive(Debug)]
pub struct Symbol {
    pub name: Atom,
    pub decl_kind: DeclKind,
    pub scope: ScopeId,
    pub span: Span,
}

/// One concrete declaration occurrence and the stable symbol it contributes to.
///
/// [`Symbol::span`] is the canonical declaration location.  JavaScript permits the same `var` or
/// function binding to be declared more than once, however, and parser transforms can preserve
/// each occurrence at a different source anchor.  Consumers which attach semantic identity to an
/// owned syntax tree must therefore use this occurrence table instead of guessing that every
/// declaration has the canonical span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindingOccurrence {
    pub name: Atom,
    pub span: Span,
    pub scope: ScopeId,
    pub symbol: SymbolId,
}

#[derive(Debug)]
pub struct Reference {
    pub name: Atom,
    pub span: Span,
    pub scope: ScopeId,
    /// 解析到的符号；`None` 表示全局/未声明。
    pub resolved: Option<SymbolId>,
}

/// 语义模型：作用域树 + 符号表 + 引用列表。
#[derive(Debug)]
pub struct SemanticModel {
    pub scopes: Vec<Scope>,
    pub symbols: Vec<Symbol>,
    pub binding_occurrences: Vec<BindingOccurrence>,
    pub references: Vec<Reference>,
}

impl SemanticModel {
    /// 未解析（全局/未声明）的引用数。
    pub fn unresolved_count(&self) -> usize {
        self.references
            .iter()
            .filter(|r| r.resolved.is_none())
            .count()
    }

    /// 在 `scope` 及其祖先中查找名字对应的符号。
    pub fn resolve_in(&self, mut scope: ScopeId, name: Atom) -> Option<SymbolId> {
        loop {
            if let Some(&sym) = self.scopes[scope as usize].bindings.get(&name) {
                return Some(sym);
            }
            scope = self.scopes[scope as usize].parent?;
        }
    }
}

/// 分析一个 Program，产出语义模型。
pub fn analyze(program: &Program) -> SemanticModel {
    let mut r = Resolver {
        scopes: Vec::new(),
        symbols: Vec::new(),
        binding_occurrences: Vec::new(),
        binding_occurrence_keys: FxHashSet::default(),
        references: Vec::new(),
        stack: Vec::new(),
    };
    let module = r.push_scope(ScopeKind::Module, None);
    r.stack.push(module);
    // 顶层提升。
    r.hoist(&program.body, module);
    // Lexical/module bindings are instantiated before any statement executes. Besides ordinary
    // TDZ correctness, this is required by transform-generated dormant declarations whose
    // binding deliberately affects expressions that precede the declaration node.
    r.predeclare_lexical(&program.body, module);
    for stmt in program.body.iter() {
        r.visit_statement(stmt);
    }
    r.stack.pop();
    SemanticModel {
        scopes: r.scopes,
        symbols: r.symbols,
        binding_occurrences: r.binding_occurrences,
        references: r.references,
    }
}

struct Resolver {
    scopes: Vec<Scope>,
    symbols: Vec<Symbol>,
    binding_occurrences: Vec<BindingOccurrence>,
    binding_occurrence_keys: FxHashSet<(Atom, Span, ScopeId, SymbolId)>,
    references: Vec<Reference>,
    stack: Vec<ScopeId>,
}

impl Resolver {
    fn cur_scope(&self) -> ScopeId {
        *self.stack.last().unwrap()
    }

    fn push_scope(&mut self, kind: ScopeKind, parent: Option<ScopeId>) -> ScopeId {
        let id = self.scopes.len() as ScopeId;
        self.scopes.push(Scope {
            kind,
            parent,
            bindings: FxHashMap::default(),
        });
        id
    }

    fn enter(&mut self, kind: ScopeKind) -> ScopeId {
        let parent = self.cur_scope();
        let id = self.push_scope(kind, Some(parent));
        self.stack.push(id);
        id
    }

    fn exit(&mut self) {
        self.stack.pop();
    }

    /// 最近的函数/模块作用域（var 提升目标）。
    fn nearest_var_scope(&self) -> ScopeId {
        for &s in self.stack.iter().rev() {
            if matches!(
                self.scopes[s as usize].kind,
                ScopeKind::Function | ScopeKind::Module
            ) {
                return s;
            }
        }
        self.stack[0]
    }

    fn declare(&mut self, scope: ScopeId, name: Atom, decl_kind: DeclKind, span: Span) -> SymbolId {
        // 已存在同名绑定（如 var 重复声明）时复用，避免重复。
        let symbol = if let Some(&existing) = self.scopes[scope as usize].bindings.get(&name)
            && self.symbols[existing as usize].decl_kind == decl_kind
        {
            existing
        } else {
            let id = self.symbols.len() as SymbolId;
            self.symbols.push(Symbol {
                name,
                decl_kind,
                scope,
                span,
            });
            self.scopes[scope as usize].bindings.insert(name, id);
            id
        };
        if self
            .binding_occurrence_keys
            .insert((name, span, scope, symbol))
        {
            self.binding_occurrences.push(BindingOccurrence {
                name,
                span,
                scope,
                symbol,
            });
        }
        symbol
    }

    fn reference(&mut self, name: Atom, span: Span) {
        let scope = self.cur_scope();
        let resolved = SemanticModel::resolve_in_impl(&self.scopes, scope, name);
        self.references.push(Reference {
            name,
            span,
            scope,
            resolved,
        });
    }

    // ==================================================================
    // 提升（hoisting）：把 var / function 声明预绑定到目标作用域
    // ==================================================================

    /// 在 `func_scope` 上提升语句列表里的 var（穿透块，不穿透函数）与 function 声明（当前块）。
    fn hoist(&mut self, stmts: &AVec<Statement>, func_scope: ScopeId) {
        for stmt in stmts.iter() {
            self.hoist_stmt(stmt, func_scope);
        }
    }

    fn hoist_stmt(&mut self, stmt: &Statement, func_scope: ScopeId) {
        match stmt {
            Statement::VariableDeclaration(d) if d.kind == VarKind::Var => {
                for decl in d.declarations.iter() {
                    self.declare_pattern(func_scope, &decl.id, DeclKind::Var);
                }
            }
            Statement::FunctionDeclaration(f) => {
                if let Some(id) = f.id {
                    self.declare(func_scope, id.name, DeclKind::Function, id.span);
                }
            }
            // var 穿透以下控制流结构（但不穿透嵌套函数/类）。
            Statement::Block(b) => self.hoist(&b.body, func_scope),
            Statement::If(s) => {
                self.hoist_stmt(&s.consequent, func_scope);
                if let Some(a) = &s.alternate {
                    self.hoist_stmt(a, func_scope);
                }
            }
            Statement::For(s) => {
                if let Some(ForInit::Variable(d)) = &s.init
                    && d.kind == VarKind::Var
                {
                    for decl in d.declarations.iter() {
                        self.declare_pattern(func_scope, &decl.id, DeclKind::Var);
                    }
                }
                self.hoist_stmt(&s.body, func_scope);
            }
            Statement::ForIn(s) => self.hoist_for_left(&s.left, &s.body, func_scope),
            Statement::ForOf(s) => self.hoist_for_left(&s.left, &s.body, func_scope),
            Statement::While(s) => self.hoist_stmt(&s.body, func_scope),
            Statement::DoWhile(s) => self.hoist_stmt(&s.body, func_scope),
            Statement::Switch(s) => {
                for case in s.cases.iter() {
                    self.hoist(&case.consequent, func_scope);
                }
            }
            Statement::Try(s) => {
                self.hoist(&s.block.body, func_scope);
                if let Some(h) = &s.handler {
                    self.hoist(&h.body.body, func_scope);
                }
                if let Some(f) = &s.finalizer {
                    self.hoist(&f.body, func_scope);
                }
            }
            Statement::Labeled(s) => self.hoist_stmt(&s.body, func_scope),
            // `export function/var …`：把被包裹声明按同规则提升到 enclosing scope（否则 export 函数名
            // 只落在 own scope，兄弟 export 函数 mangle 时同名碰撞）。const/let 不提升（visit 时声明）。
            Statement::ExportNamed(s) => {
                if let Some(d) = &s.declaration {
                    self.hoist_stmt(d, func_scope);
                }
            }
            // `export default function foo`：foo 是模块作用域的提升声明（供内部/导出引用一致解析）。
            Statement::ExportDefault(s) => {
                if let ExportDefaultKind::Function(f) = &s.declaration
                    && let Some(id) = f.id
                {
                    self.declare(func_scope, id.name, DeclKind::Function, id.span);
                }
            }
            _ => {}
        }
    }

    fn hoist_for_left(&mut self, left: &ForLeft, body: &Statement, func_scope: ScopeId) {
        if let ForLeft::Variable(d) = left
            && d.kind == VarKind::Var
        {
            for decl in d.declarations.iter() {
                self.declare_pattern(func_scope, &decl.id, DeclKind::Var);
            }
        }
        self.hoist_stmt(body, func_scope);
    }

    /// Predeclare direct lexical bindings for one statement list.
    ///
    /// ECMAScript creates `let`/`const`/`class`/`using` and import bindings when entering their
    /// scope, not when execution reaches the declaration. Keeping this phase separate from
    /// [`Self::hoist`] is useful: these bindings resolve earlier references, but retain TDZ
    /// runtime behavior and must never be treated as `var`/function declarations.
    fn predeclare_lexical(&mut self, stmts: &[Statement], scope: ScopeId) {
        for statement in stmts {
            self.predeclare_statement_lexical(statement, scope);
        }
    }

    fn predeclare_statement_lexical(&mut self, statement: &Statement, scope: ScopeId) {
        match statement {
            Statement::VariableDeclaration(declaration) if declaration.kind != VarKind::Var => {
                let kind = match declaration.kind {
                    VarKind::Let => DeclKind::Let,
                    VarKind::Const => DeclKind::Const,
                    VarKind::Using | VarKind::AwaitUsing => DeclKind::Using,
                    VarKind::Var => unreachable!("var was excluded by the match guard"),
                };
                for declarator in declaration.declarations.iter() {
                    self.declare_pattern(scope, &declarator.id, kind);
                }
            }
            Statement::ClassDeclaration(class) => {
                if let Some(id) = class.id {
                    self.declare(scope, id.name, DeclKind::Class, id.span);
                }
            }
            Statement::Import(declaration) => {
                for specifier in declaration.specifiers.iter() {
                    let local = match specifier {
                        ImportSpecifier::Named { local, .. }
                        | ImportSpecifier::Default { local, .. }
                        | ImportSpecifier::Namespace { local, .. } => local,
                    };
                    self.declare(scope, local.name, DeclKind::Import, local.span);
                }
            }
            Statement::ExportNamed(export) => {
                if let Some(declaration) = &export.declaration {
                    self.predeclare_statement_lexical(declaration, scope);
                }
            }
            Statement::ExportDefault(export) => {
                if let ExportDefaultKind::Class(class) = export.declaration
                    && let Some(id) = class.id
                {
                    self.declare(scope, id.name, DeclKind::Class, id.span);
                }
            }
            _ => {}
        }
    }

    // ==================================================================
    // 绑定模式
    // ==================================================================

    fn declare_pattern(&mut self, scope: ScopeId, pat: &Pattern, kind: DeclKind) {
        match pat {
            Pattern::Ident(id) => {
                self.declare(scope, id.name, kind, id.span);
            }
            Pattern::Array(a) => {
                for el in a.elements.iter().flatten() {
                    self.declare_pattern(scope, el, kind);
                }
            }
            Pattern::Object(o) => {
                for p in o.properties.iter() {
                    self.declare_pattern(scope, &p.value, kind);
                }
                if let Some(rest) = &o.rest {
                    self.declare_pattern(scope, &rest.argument, kind);
                }
            }
            Pattern::Assignment(a) => self.declare_pattern(scope, &a.left, kind),
            Pattern::Rest(r) => self.declare_pattern(scope, &r.argument, kind),
        }
    }

    /// 参数模式里的默认值表达式需要在函数作用域内解析引用。
    fn visit_pattern_defaults(&mut self, pat: &Pattern) {
        match pat {
            Pattern::Ident(_) => {}
            Pattern::Array(a) => {
                for el in a.elements.iter().flatten() {
                    self.visit_pattern_defaults(el);
                }
            }
            Pattern::Object(o) => {
                for p in o.properties.iter() {
                    if let PropertyKey::Computed(e) = &p.key {
                        self.visit_expression(e);
                    }
                    self.visit_pattern_defaults(&p.value);
                }
                if let Some(rest) = &o.rest {
                    self.visit_pattern_defaults(&rest.argument);
                }
            }
            Pattern::Assignment(a) => {
                self.visit_pattern_defaults(&a.left);
                self.visit_expression(&a.right);
            }
            Pattern::Rest(r) => self.visit_pattern_defaults(&r.argument),
        }
    }

    // ==================================================================
    // 遍历
    // ==================================================================

    fn visit_statement(&mut self, stmt: &Statement) {
        match stmt {
            Statement::VariableDeclaration(d) => {
                let scope = if d.kind == VarKind::Var {
                    self.nearest_var_scope()
                } else {
                    self.cur_scope()
                };
                for decl in d.declarations.iter() {
                    // let/const 在当前作用域绑定（var 已在 hoist 阶段绑定，这里重复 declare 会命中复用）。
                    let kind = match d.kind {
                        VarKind::Var => DeclKind::Var,
                        VarKind::Let => DeclKind::Let,
                        VarKind::Const => DeclKind::Const,
                        VarKind::Using | VarKind::AwaitUsing => DeclKind::Using,
                    };
                    self.declare_pattern(scope, &decl.id, kind);
                    // 默认值/computed key 里的引用。
                    self.visit_pattern_defaults(&decl.id);
                    if let Some(init) = &decl.init {
                        self.visit_expression(init);
                    }
                }
            }
            // 函数声明：名字已由外层 hoist 声明在 enclosing scope → 传 true 跳过 own-scope 重复声明。
            Statement::FunctionDeclaration(f) => self.visit_function(f, true),
            Statement::ClassDeclaration(c) => {
                if let Some(id) = c.id {
                    self.declare(self.cur_scope(), id.name, DeclKind::Class, id.span);
                }
                self.visit_class(c);
            }
            Statement::Block(b) => {
                let scope = self.enter(ScopeKind::Block);
                self.predeclare_lexical(&b.body, scope);
                for s in b.body.iter() {
                    self.visit_statement(s);
                }
                self.exit();
            }
            Statement::Expression(e) => self.visit_expression(&e.expression),
            Statement::If(s) => {
                self.visit_expression(&s.test);
                self.visit_statement(&s.consequent);
                if let Some(a) = &s.alternate {
                    self.visit_statement(a);
                }
            }
            Statement::For(s) => {
                self.enter(ScopeKind::Block);
                if let Some(init) = &s.init {
                    match init {
                        ForInit::Variable(d) => {
                            self.visit_statement(&Statement::VariableDeclaration(d))
                        }
                        ForInit::Expression(e) => self.visit_expression(e),
                    }
                }
                if let Some(t) = &s.test {
                    self.visit_expression(t);
                }
                if let Some(u) = &s.update {
                    self.visit_expression(u);
                }
                self.visit_statement(&s.body);
                self.exit();
            }
            Statement::ForIn(s) => self.visit_for_in_of(&s.left, &s.right, &s.body),
            Statement::ForOf(s) => self.visit_for_in_of(&s.left, &s.right, &s.body),
            Statement::While(s) => {
                self.visit_expression(&s.test);
                self.visit_statement(&s.body);
            }
            Statement::DoWhile(s) => {
                self.visit_statement(&s.body);
                self.visit_expression(&s.test);
            }
            Statement::Switch(s) => {
                self.visit_expression(&s.discriminant);
                let scope = self.enter(ScopeKind::Block);
                for case in s.cases.iter() {
                    self.predeclare_lexical(&case.consequent, scope);
                }
                for case in s.cases.iter() {
                    if let Some(t) = &case.test {
                        self.visit_expression(t);
                    }
                    for st in case.consequent.iter() {
                        self.visit_statement(st);
                    }
                }
                self.exit();
            }
            Statement::Return(s) => {
                if let Some(a) = &s.argument {
                    self.visit_expression(a);
                }
            }
            Statement::Throw(s) => self.visit_expression(&s.argument),
            Statement::Try(s) => {
                let scope = self.enter(ScopeKind::Block);
                self.predeclare_lexical(&s.block.body, scope);
                for st in s.block.body.iter() {
                    self.visit_statement(st);
                }
                self.exit();
                if let Some(h) = &s.handler {
                    self.enter(ScopeKind::Catch);
                    if let Some(p) = &h.param {
                        self.declare_pattern(self.cur_scope(), p, DeclKind::CatchParam);
                        self.visit_pattern_defaults(p);
                    }
                    self.predeclare_lexical(&h.body.body, self.cur_scope());
                    for st in h.body.body.iter() {
                        self.visit_statement(st);
                    }
                    self.exit();
                }
                if let Some(f) = &s.finalizer {
                    let scope = self.enter(ScopeKind::Block);
                    self.predeclare_lexical(&f.body, scope);
                    for st in f.body.iter() {
                        self.visit_statement(st);
                    }
                    self.exit();
                }
            }
            Statement::Labeled(s) => self.visit_statement(&s.body),
            Statement::With(s) => {
                self.visit_expression(&s.object);
                self.visit_statement(&s.body);
            }
            Statement::ExportNamed(s) => {
                if let Some(d) = &s.declaration {
                    self.visit_statement(d);
                }
            }
            Statement::ExportDefault(s) => match s.declaration {
                // 默认导出的命名函数：名字已由 hoist 提升到模块作用域 → true 跳过 own-scope 重复声明。
                ExportDefaultKind::Function(f) => self.visit_function(f, true),
                ExportDefaultKind::Class(c) => self.visit_class(c),
                ExportDefaultKind::Expression(e) => self.visit_expression(&e),
            },
            Statement::Import(d) => {
                for spec in d.specifiers.iter() {
                    let (span, name) = match spec {
                        ImportSpecifier::Named { local, .. }
                        | ImportSpecifier::Default { local, .. }
                        | ImportSpecifier::Namespace { local, .. } => (local.span, local.name),
                    };
                    self.declare(self.cur_scope(), name, DeclKind::Import, span);
                }
            }
            Statement::Empty(_)
            | Statement::Debugger(_)
            | Statement::Break(_)
            | Statement::Continue(_)
            | Statement::ExportAll(_) => {}
        }
    }

    fn visit_for_in_of(&mut self, left: &ForLeft, right: &Expression, body: &Statement) {
        self.enter(ScopeKind::Block);
        match left {
            ForLeft::Variable(d) => self.visit_statement(&Statement::VariableDeclaration(d)),
            ForLeft::Target(e) => self.visit_expression(e),
        }
        self.visit_expression(right);
        self.visit_statement(body);
        self.exit();
    }

    /// `name_hoisted`：函数**声明**的名字已由外层 `hoist` 声明在 enclosing scope（true）——此时**不得**
    /// 在函数自身作用域再声明一次，否则各函数自作用域名字计数各自从 0 → mangle 时兄弟函数同名碰撞
    /// （`function a`/`function a`）。具名函数**表达式**/默认导出/方法（false）则名字仅在函数作用域可见。
    fn visit_function(&mut self, f: &Function, name_hoisted: bool) {
        self.enter(ScopeKind::Function);
        let fscope = self.cur_scope();
        if let Some(id) = f.id
            && !name_hoisted
        {
            // 具名函数表达式：名字在函数作用域内可见（供自引用）。声明名走 enclosing hoist，此处跳过。
            self.declare(fscope, id.name, DeclKind::Function, id.span);
        }
        for p in f.params.iter() {
            self.declare_pattern(fscope, p, DeclKind::Param);
        }
        for p in f.params.iter() {
            self.visit_pattern_defaults(p);
        }
        if let Some(body) = f.body {
            self.hoist(&body.statements, fscope);
            self.predeclare_lexical(&body.statements, fscope);
            for st in body.statements.iter() {
                self.visit_statement(st);
            }
        }
        self.exit();
    }

    fn visit_class(&mut self, c: &Class) {
        // 装饰器表达式是真实运行时引用，必须计入作用域分析——否则 mangler 重命名被引用的
        // 装饰器函数后，装饰器处仍写旧名（ReferenceError），tree-shaking 也会误删它们。
        for d in c.decorators.iter() {
            self.visit_expression(d);
        }
        if let Some(sc) = &c.super_class {
            self.visit_expression(sc);
        }
        for member in c.body.iter() {
            match member {
                ClassMember::Method(m) => {
                    for d in m.decorators.iter() {
                        self.visit_expression(d);
                    }
                    if let PropertyKey::Computed(e) = &m.key {
                        self.visit_expression(e);
                    }
                    // 方法：其函数无独立声明名进入 enclosing，own-scope 处理即可。
                    self.visit_function(m.value, false);
                }
                ClassMember::Property(p) => {
                    for d in p.decorators.iter() {
                        self.visit_expression(d);
                    }
                    if let PropertyKey::Computed(e) = &p.key {
                        self.visit_expression(e);
                    }
                    if let Some(v) = &p.value {
                        self.visit_expression(v);
                    }
                }
                ClassMember::StaticBlock(b) => {
                    self.enter(ScopeKind::Block);
                    for st in b.body.iter() {
                        self.visit_statement(st);
                    }
                    self.exit();
                }
            }
        }
    }

    fn visit_expression(&mut self, expr: &Expression) {
        match expr {
            Expression::Identifier(id) => self.reference(id.name, id.span),
            Expression::NumberLiteral(_)
            | Expression::StringLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
            | Expression::BigIntLiteral(_)
            | Expression::RegExpLiteral(_)
            | Expression::This(_)
            | Expression::Super(_)
            | Expression::MetaProperty(_) => {}
            Expression::TemplateLiteral(t) => {
                for e in t.expressions.iter() {
                    self.visit_expression(e);
                }
            }
            Expression::Array(a) => {
                for el in a.elements.iter().flatten() {
                    self.visit_expression(el);
                }
            }
            Expression::Object(o) => {
                for m in o.properties.iter() {
                    match m {
                        ObjectMember::Property(p) => {
                            if let PropertyKey::Computed(e) = &p.key {
                                self.visit_expression(e);
                            }
                            self.visit_expression(&p.value);
                        }
                        ObjectMember::Spread(s) => self.visit_expression(&s.argument),
                    }
                }
            }
            // 具名函数表达式：名字仅在函数自身作用域可见（供自引用）→ false，own-scope 声明。
            Expression::Function(f) => self.visit_function(f, false),
            Expression::Arrow(a) => {
                self.enter(ScopeKind::Function);
                let fscope = self.cur_scope();
                for p in a.params.iter() {
                    self.declare_pattern(fscope, p, DeclKind::Param);
                }
                for p in a.params.iter() {
                    self.visit_pattern_defaults(p);
                }
                match a.body {
                    ArrowBody::Block(b) => {
                        self.hoist(&b.statements, fscope);
                        self.predeclare_lexical(&b.statements, fscope);
                        for st in b.statements.iter() {
                            self.visit_statement(st);
                        }
                    }
                    ArrowBody::Expression(e) => self.visit_expression(&e),
                }
                self.exit();
            }
            Expression::Class(c) => self.visit_class(c),
            Expression::Unary(u) => self.visit_expression(&u.argument),
            Expression::Update(u) => self.visit_expression(&u.argument),
            Expression::Binary(b) => {
                self.visit_expression(&b.left);
                self.visit_expression(&b.right);
            }
            Expression::Logical(l) => {
                self.visit_expression(&l.left);
                self.visit_expression(&l.right);
            }
            Expression::Assignment(a) => {
                self.visit_expression(&a.left);
                self.visit_expression(&a.right);
            }
            Expression::Conditional(c) => {
                self.visit_expression(&c.test);
                self.visit_expression(&c.consequent);
                self.visit_expression(&c.alternate);
            }
            Expression::Call(c) => {
                self.visit_expression(&c.callee);
                for arg in c.arguments.iter() {
                    self.visit_expression(arg);
                }
            }
            Expression::New(n) => {
                self.visit_expression(&n.callee);
                for arg in n.arguments.iter() {
                    self.visit_expression(arg);
                }
            }
            Expression::Member(m) => {
                self.visit_expression(&m.object);
                if let MemberProperty::Computed(e) = &m.property {
                    self.visit_expression(e);
                }
            }
            Expression::Sequence(s) => {
                for e in s.expressions.iter() {
                    self.visit_expression(e);
                }
            }
            Expression::TaggedTemplate(t) => {
                self.visit_expression(&t.tag);
                for e in t.quasi.expressions.iter() {
                    self.visit_expression(e);
                }
            }
            Expression::Spread(s) => self.visit_expression(&s.argument),
            Expression::Await(a) => self.visit_expression(&a.argument),
            Expression::Yield(y) => {
                if let Some(a) = &y.argument {
                    self.visit_expression(a);
                }
            }
            Expression::Import(i) => {
                self.visit_expression(&i.source);
                if let Some(o) = &i.options {
                    self.visit_expression(o);
                }
            }
        }
    }
}

impl SemanticModel {
    fn resolve_in_impl(scopes: &[Scope], mut scope: ScopeId, name: Atom) -> Option<SymbolId> {
        loop {
            if let Some(&sym) = scopes[scope as usize].bindings.get(&name) {
                return Some(sym);
            }
            scope = scopes[scope as usize].parent?;
        }
    }
}
