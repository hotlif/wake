//! 语句 / 声明 / 函数 / 类 / 模式 / 模块（import·export）解析 + ASI + 依赖提取。DESIGN §4.4

use wake_common::Span;
use wake_ecma_ast::*;
use wake_ecma_lexer::{Keyword, TokenKind};

use crate::Parser;

impl<'a, 'src, const LOWER: bool> Parser<'a, 'src, LOWER> {
    // ==================================================================
    // 语句分派
    // ==================================================================

    pub(crate) fn parse_statement(&mut self) -> Statement<'a> {
        let lo = self.start();
        let kind = self.cur.kind;
        match kind {
            TokenKind::LBrace => Statement::Block(self.parse_block()),
            TokenKind::Semicolon => {
                let s = self.cur.span;
                self.bump();
                Statement::Empty(s)
            }
            TokenKind::Keyword(Keyword::Var) => {
                let d = self.parse_var_declaration(VarKind::Var, true);
                Statement::VariableDeclaration(d)
            }
            TokenKind::Keyword(Keyword::Const) => {
                // TS：`const enum E {..}` → 值转换（IIFE）。
                if self.ts && self.peek().kind == TokenKind::Keyword(Keyword::Enum) {
                    self.bump(); // const
                    return self.parse_enum(lo);
                }
                let d = self.parse_var_declaration(VarKind::Const, true);
                Statement::VariableDeclaration(d)
            }
            TokenKind::Keyword(Keyword::Let) if self.let_is_declaration() => {
                let d = self.parse_var_declaration(VarKind::Let, true);
                Statement::VariableDeclaration(d)
            }
            TokenKind::Keyword(Keyword::Function) => {
                Statement::FunctionDeclaration(self.parse_function(lo, false))
            }
            TokenKind::Keyword(Keyword::Async)
                if self.peek().kind == TokenKind::Keyword(Keyword::Function)
                    && !self.peek().newline_before =>
            {
                self.bump(); // async
                Statement::FunctionDeclaration(self.parse_function(lo, true))
            }
            TokenKind::Keyword(Keyword::Class) => Statement::ClassDeclaration(self.parse_class(lo)),
            TokenKind::Keyword(Keyword::If) => self.parse_if(lo),
            TokenKind::Keyword(Keyword::For) => self.parse_for(lo, &[]),
            TokenKind::Keyword(Keyword::While) => self.parse_while(lo),
            TokenKind::Keyword(Keyword::Do) => self.parse_do_while(lo),
            TokenKind::Keyword(Keyword::Switch) => self.parse_switch(lo),
            TokenKind::Keyword(Keyword::Return) => self.parse_return(lo),
            TokenKind::Keyword(Keyword::Break) => self.parse_break_continue(lo, true),
            TokenKind::Keyword(Keyword::Continue) => self.parse_break_continue(lo, false),
            TokenKind::Keyword(Keyword::Throw) => self.parse_throw(lo),
            TokenKind::Keyword(Keyword::Try) => self.parse_try(lo),
            TokenKind::Keyword(Keyword::With) => self.parse_with(lo),
            TokenKind::Keyword(Keyword::Debugger) => {
                self.bump();
                self.semicolon();
                Statement::Debugger(self.span_to(lo))
            }
            TokenKind::Keyword(Keyword::Import) if !self.import_is_expression() => {
                self.parse_import_declaration(lo)
            }
            TokenKind::Keyword(Keyword::Export) => self.parse_export(lo),
            // TS：装饰器 `@dec class C {}` / `@dec export class ..`。装饰器被消费（当前不应用其
            // 运行时语义，见 ts.rs `skip_decorators` 说明）；随后解析被装饰的声明。
            // 装饰器 `@dec class C {}`（TC39 Stage-3）：解析后转交给被装饰的类。
            TokenKind::At if self.ts => {
                let decs = self.parse_decorators();
                if self.at_keyword(Keyword::Class) {
                    let clo = self.start();
                    Statement::ClassDeclaration(self.parse_class_with_decorators(clo, decs))
                } else {
                    // `@dec export class ..` 等：装饰器已消费，继续解析后续声明。
                    self.parse_statement()
                }
            }
            // TS：`interface X { .. }` 整体擦除。
            TokenKind::Keyword(Keyword::Interface) if self.ts => self.skip_interface(lo),
            // TS：`enum E { .. }` → 值转换（IIFE）。
            TokenKind::Keyword(Keyword::Enum) if self.ts => self.parse_enum(lo),
            _ => {
                // TC39 显式资源管理：`using x = e;` / `await using x = e;`。非 TS-only，纯 JS 同样合法。
                if let Some(kind) = self.using_decl_here() {
                    let d = self.parse_var_declaration(kind, true);
                    return Statement::VariableDeclaration(d);
                }
                if self.ts {
                    // `type X = ..;` 类型别名擦除（`type` 后接绑定名，区别于 `type` 作变量）。
                    if self.at_contextual("type") && self.peek_is_binding_name() {
                        return self.skip_type_alias(lo);
                    }
                    // `declare ...` 环境声明 → 整体擦除。
                    if self.at_contextual("declare") && self.ts_declare_is_declaration() {
                        return self.parse_declare(lo);
                    }
                    // `abstract class ..` → 消费 abstract 后按 class 解析。
                    if self.at_contextual("abstract")
                        && self.peek().kind == TokenKind::Keyword(Keyword::Class)
                    {
                        self.bump(); // abstract
                        return Statement::ClassDeclaration(self.parse_class(self.start()));
                    }
                    // `namespace N { .. }` / `module N { .. }` → 值转换（IIFE）。
                    if (self.at_contextual("namespace") || self.at_contextual("module"))
                        && self.ts_namespace_is_declaration()
                    {
                        return self.parse_namespace(lo);
                    }
                }
                // 标签语句：ident `:`。
                if self.at_ident_name() && self.peek().kind == TokenKind::Colon {
                    return self.parse_labeled_statement(lo);
                }
                self.parse_expression_statement(lo)
            }
        }
    }

    /// Parse a complete contiguous label chain in one step. If it directly targets a `for`
    /// statement, pass the labels into `parse_for`: synchronous for-of lowering must relocate
    /// them onto its generated inner loop so `continue label` stays valid. Other statements keep
    /// the ordinary outer wrappers.
    fn parse_labeled_statement(&mut self, lo: u32) -> Statement<'a> {
        let mut labels = Vec::new();
        loop {
            labels.push(self.parse_ident_name());
            self.expect(TokenKind::Colon);
            if !(self.at_ident_name() && self.peek().kind == TokenKind::Colon) {
                break;
            }
        }

        if self.at_keyword(Keyword::For) {
            let for_lo = self.start();
            return self.parse_for(for_lo, &labels);
        }

        let body = self.parse_statement();
        self.wrap_statement_labels(body, &labels, lo)
    }

    fn wrap_statement_labels(
        &self,
        mut body: Statement<'a>,
        labels: &[Ident],
        fallback_lo: u32,
    ) -> Statement<'a> {
        for label in labels.iter().rev() {
            let lo = if label.span.is_dummy() {
                fallback_lo
            } else {
                label.span.lo
            };
            body = Statement::Labeled(self.alloc(LabeledStatement {
                span: Span::new(lo, body.span().hi),
                label: *label,
                body,
            }));
        }
        body
    }

    /// `let` 是否作为声明起始（后跟 `[`/`{`/标识符名）。
    fn let_is_declaration(&mut self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::LBracket | TokenKind::LBrace | TokenKind::Ident
        ) || matches!(self.peek().kind, TokenKind::Keyword(kw) if !kw.is_reserved())
    }

    /// `import` 是否是表达式（`import(...)` / `import.meta`）而非声明。
    fn import_is_expression(&mut self) -> bool {
        matches!(self.peek().kind, TokenKind::LParen | TokenKind::Dot)
    }

    /// 当前位置是否起始一个 `using` / `await using` 声明（TC39 显式资源管理）。
    ///
    /// `using` 是**上下文关键字**：只有同一行紧跟一个可作绑定名的标识符时才是声明。这样
    /// `using = 1` / `using.foo()` / `using(x)` / `using` + 换行（ASI）里的 `using` 仍是普通
    /// 标识符。规范也不允许解构模式（`using {a} = o` 非法），故只认标识符。
    pub(crate) fn using_decl_here(&mut self) -> Option<VarKind> {
        if self.at_contextual("using") {
            return self.using_binding_follows().then_some(VarKind::Using);
        }
        // `await using`：需要 2 个 token 的前瞻，用 checkpoint 试探后回退。
        // （`await usingFoo()` / `await using(x)` 都不是声明，必须能正确回退。）
        //
        // 只在 async 上下文里识别——与 `await` 运算符的口径一致（expr.rs 亦要求 `in_async`）。
        // 模块顶层即 async 上下文（ES2022 顶层 await），故顶层 `await using` 合法；但它**必须**
        // 一并把模块标成 async 子图的种子——否则 bundler 会把它包进非 async 的
        // `function(module, exports, __wake_require__)`，产物加载即抛 SyntaxError。
        // `await` 运算符走 expr.rs 的分支置位，`await using` 声明不经那条路径，故在此补。
        if self.at_keyword(Keyword::Await) && self.ctx.in_async {
            let cp = self.checkpoint();
            self.bump(); // await
            let ok = !self.newline_before()
                && self.at_contextual("using")
                && self.using_binding_follows();
            self.rewind(cp);
            if ok && self.ctx.top_level {
                self.has_top_level_await = true;
            }
            return ok.then_some(VarKind::AwaitUsing);
        }
        None
    }

    /// `using` 之后是否**同行**紧跟一个可作绑定名的标识符。
    fn using_binding_follows(&mut self) -> bool {
        let p = self.peek();
        // `using` 与绑定名之间不允许换行（否则 ASI 将 `using` 断成表达式语句）。
        if p.newline_before {
            return false;
        }
        matches!(p.kind, TokenKind::Ident)
            || matches!(p.kind, TokenKind::Keyword(kw) if !kw.is_reserved())
    }

    fn parse_expression_statement(&mut self, lo: u32) -> Statement<'a> {
        let expression = self.parse_expression();
        self.semicolon();
        Statement::Expression(self.alloc(ExpressionStatement {
            span: self.span_to(lo),
            expression,
        }))
    }

    /// `require("x")` 依赖提取——在每处 CallExpression 构建后调用（覆盖任意位置）。
    pub(crate) fn maybe_record_require(&mut self, call: &CallExpression<'a>) {
        if let Expression::Identifier(id) = &call.callee
            && id.name == self.require_atom
            && call.arguments.len() == 1
            && let Expression::StringLiteral(s) = &call.arguments[0]
        {
            self.record_dependency(Dependency {
                specifier: s.value,
                kind: DependencyKind::Require,
                span: call.span,
            });
        }
    }

    // ==================================================================
    // 块 / 变量声明
    // ==================================================================

    pub(crate) fn parse_block(&mut self) -> &'a BlockStatement<'a> {
        let lo = self.start();
        self.expect(TokenKind::LBrace);
        let mut body = self.new_vec::<Statement>();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let before = self.cur.span.lo;
            body.push(self.parse_statement());
            if self.cur.span.lo == before && !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof)
            {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        self.alloc(BlockStatement {
            span: self.span_to(lo),
            body,
        })
    }

    fn parse_var_declaration(
        &mut self,
        kind: VarKind,
        need_semi: bool,
    ) -> &'a VariableDeclaration<'a> {
        let lo = self.start();
        self.bump(); // var / let / const / using / await
        if kind == VarKind::AwaitUsing {
            self.bump(); // using（`await` 已在上一行消费）
        }
        let mut declarations = self.new_vec::<VariableDeclarator>();
        loop {
            let dlo = self.start();
            let id = self.parse_binding_pattern();
            if self.ts {
                self.eat(TokenKind::Bang); // 明确赋值断言 `let x!: T`
                self.ts_type_annotation(); // `: T`
            }
            let init = if self.eat(TokenKind::Eq) {
                Some(self.parse_assignment_expression())
            } else {
                None
            };
            let declarator = VariableDeclarator {
                span: self.span_to(dlo),
                id,
                init,
            };
            if LOWER
                && wake_ecma_transform::binding_pattern_needs_lowering(
                    self.options.transform_features,
                    id,
                )
                && init.is_some()
            {
                let iterator_helper = self.spread_helper_atom();
                let object_helper = if wake_ecma_transform::pattern_has_object_rest(id) {
                    self.object_spread_helper_atom()
                } else {
                    iterator_helper
                };
                let temporary_count =
                    wake_ecma_transform::destructuring_temporary_count(declarator.id);
                let temporaries = (0..temporary_count)
                    .map(|_| self.fresh_transform_atom())
                    .collect::<Vec<_>>();
                declarations.extend(wake_ecma_transform::lower_variable_destructuring(
                    self.arena,
                    self.interner,
                    iterator_helper,
                    object_helper,
                    self.options.transform_features,
                    kind,
                    declarator,
                    &temporaries,
                ));
            } else {
                declarations.push(declarator);
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        if need_semi {
            self.semicolon();
        }
        self.alloc(VariableDeclaration {
            span: self.span_to(lo),
            kind,
            declarations,
        })
    }

    // ==================================================================
    // TypeScript 声明擦除（DESIGN §4.1）——仅 ts 模式。类型语法的结构化消费在 `ts.rs`。
    // 本节：interface / type 别名 / declare 的整体擦除（值语义的 enum/namespace 见 `parse_enum`/`parse_namespace`）。
    // ==================================================================

    /// `interface X<..> extends A, B { .. }` → 整体擦除为空语句（语法结构化消费）。
    fn skip_interface(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // interface
        if self.at_ident_name() {
            self.bump(); // 名字
        }
        self.ts_type_parameters(); // <T ..>
        if self.eat_keyword(Keyword::Extends) {
            loop {
                self.ts_type();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
        }
        if self.at(TokenKind::LBrace) {
            self.ts_skip_balanced(); // 成员体（对象类型，自平衡）
        }
        Statement::Empty(self.span_to(lo))
    }

    /// `type X<..> = Type;` → 整体擦除为空语句（RHS 用完整类型文法消费）。
    fn skip_type_alias(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // type
        if self.at_ident_name() {
            self.bump(); // 别名名
        }
        self.ts_type_parameters();
        if self.eat(TokenKind::Eq) {
            self.ts_type();
        }
        self.semicolon();
        Statement::Empty(self.span_to(lo))
    }

    /// 当前是否为上下文关键字 `name`（Ident 且文本相符）。
    pub(crate) fn at_contextual(&self, name: &str) -> bool {
        self.cur.kind == TokenKind::Ident && self.slice(self.cur.span) == name
    }

    /// 前瞻是否为可作绑定名的标识符（Ident 或非保留关键字）。
    fn peek_is_binding_name(&mut self) -> bool {
        matches!(self.peek().kind, TokenKind::Ident)
            || matches!(self.peek().kind, TokenKind::Keyword(kw) if !kw.is_reserved())
    }

    // ==================================================================
    // 控制流
    // ==================================================================

    fn parse_if(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // if
        self.expect(TokenKind::LParen);
        let test = self.with_allow_in(true, |p| p.parse_expression());
        self.expect(TokenKind::RParen);
        let consequent = self.parse_statement();
        let alternate = if self.eat_keyword(Keyword::Else) {
            Some(self.parse_statement())
        } else {
            None
        };
        Statement::If(self.alloc(IfStatement {
            span: self.span_to(lo),
            test,
            consequent,
            alternate,
        }))
    }

    fn parse_while(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // while
        self.expect(TokenKind::LParen);
        let test = self.with_allow_in(true, |p| p.parse_expression());
        self.expect(TokenKind::RParen);
        let body = self.parse_statement();
        Statement::While(self.alloc(WhileStatement {
            span: self.span_to(lo),
            test,
            body,
        }))
    }

    fn parse_do_while(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // do
        let body = self.parse_statement();
        self.expect(TokenKind::Keyword(Keyword::While));
        self.expect(TokenKind::LParen);
        let test = self.with_allow_in(true, |p| p.parse_expression());
        self.expect(TokenKind::RParen);
        self.eat(TokenKind::Semicolon);
        Statement::DoWhile(self.alloc(DoWhileStatement {
            span: self.span_to(lo),
            body,
            test,
        }))
    }

    fn parse_for(&mut self, lo: u32, labels: &[Ident]) -> Statement<'a> {
        self.bump(); // for
        let is_await = self.eat_keyword(Keyword::Await);
        if is_await && self.ctx.top_level {
            self.has_top_level_await = true;
        }
        self.expect(TokenKind::LParen);

        // 初始化：可能是变量声明或表达式，或空。
        let init: Option<ForInit<'a>> = if self.at(TokenKind::Semicolon) {
            None
        } else if let Some(kind) = self.for_head_var_kind() {
            // for-init 内禁 `in` 运算符。
            let decl = self.with_allow_in(false, |p| p.parse_var_declaration_no_semi(kind));
            Some(ForInit::Variable(decl))
        } else {
            let previous_for_head = self.in_for_head_init;
            self.in_for_head_init = true;
            let expr = self.with_allow_in(false, |p| p.parse_expression());
            self.in_for_head_init = previous_for_head;
            Some(ForInit::Expression(expr))
        };

        // for-in / for-of。
        if self.at_keyword(Keyword::In) || self.at_keyword(Keyword::Of) {
            let is_of = self.at_keyword(Keyword::Of);
            self.bump();
            let left = match init {
                Some(ForInit::Variable(d)) => ForLeft::Variable(d),
                Some(ForInit::Expression(e)) => ForLeft::Target(e),
                None => {
                    self.error(self.cur.span, "for-in/of 缺少左侧");
                    ForLeft::Target(Expression::NullLiteral(self.cur.span))
                }
            };
            let right = if is_of {
                self.with_allow_in(true, |p| p.parse_assignment_expression())
            } else {
                self.with_allow_in(true, |p| p.parse_expression())
            };
            self.expect(TokenKind::RParen);
            let body = self.parse_statement();
            let tdz_pattern = match left {
                ForLeft::Variable(declaration)
                    if matches!(declaration.kind, VarKind::Let | VarKind::Const)
                        && declaration.declarations.len() == 1
                        && declaration.declarations[0].init.is_none() =>
                {
                    Some(declaration.declarations[0].id)
                }
                _ => None,
            };
            let (left, body) = self.lower_for_destructuring(left, body);
            return if is_of {
                let statement = self.alloc(ForOfStatement {
                    span: self.span_to(lo),
                    left,
                    right,
                    body,
                    is_await,
                });
                if LOWER
                    && wake_ecma_transform::for_of_needs_lowering(
                        self.options.transform_features,
                        statement,
                    )
                {
                    let helper = self.for_of_helper_atom();
                    let state = self.fresh_transform_atom();
                    let error = self.fresh_transform_atom();
                    let tdz_label = self.fresh_transform_atom();
                    wake_ecma_transform::lower_for_of(
                        self.arena,
                        self.interner,
                        helper,
                        state,
                        error,
                        tdz_label,
                        self.options.transform_features,
                        labels,
                        tdz_pattern,
                        statement,
                    )
                } else {
                    self.wrap_statement_labels(Statement::ForOf(statement), labels, lo)
                }
            } else {
                let statement = Statement::ForIn(self.alloc(ForInStatement {
                    span: self.span_to(lo),
                    left,
                    right,
                    body,
                }));
                self.wrap_statement_labels(statement, labels, lo)
            };
        }

        // 传统 for(init; test; update)。
        self.expect(TokenKind::Semicolon);
        let test = if self.at(TokenKind::Semicolon) {
            None
        } else {
            Some(self.with_allow_in(true, |p| p.parse_expression()))
        };
        self.expect(TokenKind::Semicolon);
        let update = if self.at(TokenKind::RParen) {
            None
        } else {
            Some(self.with_allow_in(true, |p| p.parse_expression()))
        };
        self.expect(TokenKind::RParen);
        let body = self.parse_statement();
        let statement = Statement::For(self.alloc(ForStatement {
            span: self.span_to(lo),
            init,
            test,
            update,
            body,
        }));
        self.wrap_statement_labels(statement, labels, lo)
    }

    fn var_kind_here(&self) -> Option<VarKind> {
        match self.cur.kind {
            TokenKind::Keyword(Keyword::Var) => Some(VarKind::Var),
            TokenKind::Keyword(Keyword::Const) => Some(VarKind::Const),
            TokenKind::Keyword(Keyword::Let) => Some(VarKind::Let),
            _ => None,
        }
    }

    /// for-head 的声明种类：var/let/const，外加 `using` / `await using`（`for (using x of xs)`）。
    ///
    /// 例外：`for (using of xs)` 中的 `using` 是**循环变量名**而非声明——规范为消除这处歧义
    /// 显式禁止了 `for (using of ...)` 形态的 using 声明。
    fn for_head_var_kind(&mut self) -> Option<VarKind> {
        if let Some(k) = self.var_kind_here() {
            return Some(k);
        }
        let kind = self.using_decl_here()?;
        // `of` 词法上是 `Keyword::Of`（非 `Ident`），故不能用 `peek_contextual`。
        if kind == VarKind::Using && self.peek().kind == TokenKind::Keyword(Keyword::Of) {
            return None;
        }
        Some(kind)
    }

    fn parse_var_declaration_no_semi(&mut self, kind: VarKind) -> &'a VariableDeclaration<'a> {
        self.parse_var_declaration(kind, false)
    }

    /// Lower declaration and assignment-pattern `for-in` / `for-of` heads to a collision-free
    /// value binding, then initialize the original bindings/targets at the start of each iteration.
    ///
    /// Keep an existing block body as a nested block. The loop-head pattern is initialized before
    /// the body's lexical environment exists; flattening the generated declaration into that body
    /// would make defaults incorrectly observe body-local `let` / `const` bindings in their TDZ.
    fn lower_for_destructuring(
        &mut self,
        left: ForLeft<'a>,
        body: Statement<'a>,
    ) -> (ForLeft<'a>, Statement<'a>) {
        if !LOWER {
            return (left, body);
        }
        match left {
            ForLeft::Variable(declaration) => {
                self.lower_for_declaration_destructuring(declaration, body)
            }
            ForLeft::Target(target) => self.lower_for_target_destructuring(target, body),
        }
    }

    fn lower_for_declaration_destructuring(
        &mut self,
        declaration: &'a VariableDeclaration<'a>,
        body: Statement<'a>,
    ) -> (ForLeft<'a>, Statement<'a>) {
        let original_left = ForLeft::Variable(declaration);
        if declaration.kind.is_using() || declaration.declarations.len() != 1 {
            return (original_left, body);
        }
        let declarator = &declaration.declarations[0];
        if !wake_ecma_transform::binding_pattern_needs_lowering(
            self.options.transform_features,
            declarator.id,
        ) || declarator.init.is_some()
        {
            return (original_left, body);
        }

        let value_atom = self.fresh_transform_atom();
        let value_ident = self.alloc(Ident::new(declarator.id.span(), value_atom));
        let head_declarator = VariableDeclarator {
            span: declarator.span,
            id: Pattern::Ident(value_ident),
            init: None,
        };
        let mut head_declarations = self.new_vec::<VariableDeclarator>();
        head_declarations.push(head_declarator);
        let head = self.alloc(VariableDeclaration {
            span: declaration.span,
            kind: declaration.kind,
            declarations: head_declarations,
        });

        let initialization = self.lower_destructuring_binding_declaration(
            declaration.kind,
            declarator.id,
            Expression::Identifier(value_ident),
        );
        let mut statements = self.new_vec::<Statement>();
        statements.push(Statement::VariableDeclaration(initialization));
        statements.push(body);
        let body = Statement::Block(self.alloc(BlockStatement {
            span: body.span(),
            body: statements,
        }));
        (ForLeft::Variable(head), body)
    }

    /// Lower an assignment-pattern loop head (`for ([a, ...rest] of values)`) through the same
    /// assignment transform used outside loops. A fresh `var` receives each iteration value; the
    /// generated assignment runs before the untouched source body in a separate outer block.
    ///
    /// If assignment lowering conservatively declines (for example an `await` default moved into
    /// a synchronous IIFE), keep the original head and body together.
    fn lower_for_target_destructuring(
        &mut self,
        target: Expression<'a>,
        body: Statement<'a>,
    ) -> (ForLeft<'a>, Statement<'a>) {
        let original_left = ForLeft::Target(target);
        if !wake_ecma_transform::destructuring_assignment_needs_lowering(
            self.options.transform_features,
            target,
        ) {
            return (original_left, body);
        }

        let value_atom = self.fresh_transform_atom();
        let value_ident = self.alloc(Ident::new(target.span(), value_atom));
        let initialization = self.lower_destructuring_assignment_expression(
            target.span(),
            target,
            Expression::Identifier(value_ident),
        );
        if matches!(
            initialization,
            Expression::Assignment(assignment)
                if assignment.operator == AssignmentOperator::Assign
                    && wake_ecma_transform::is_destructuring_assignment_target(assignment.left)
        ) {
            return (original_left, body);
        }

        let mut head_declarations = self.new_vec::<VariableDeclarator>();
        head_declarations.push(VariableDeclarator {
            span: target.span(),
            id: Pattern::Ident(value_ident),
            init: None,
        });
        let head = self.alloc(VariableDeclaration {
            span: target.span(),
            kind: VarKind::Var,
            declarations: head_declarations,
        });

        let initialization = Statement::Expression(self.alloc(ExpressionStatement {
            span: target.span(),
            expression: initialization,
        }));
        let mut statements = self.new_vec::<Statement>();
        statements.push(initialization);
        statements.push(body);
        let body = Statement::Block(self.alloc(BlockStatement {
            span: body.span(),
            body: statements,
        }));
        (ForLeft::Variable(head), body)
    }

    fn lower_destructuring_binding_declaration(
        &mut self,
        kind: VarKind,
        pattern: Pattern<'a>,
        value: Expression<'a>,
    ) -> &'a VariableDeclaration<'a> {
        let iterator_helper = self.spread_helper_atom();
        let object_helper = if wake_ecma_transform::pattern_has_object_rest(pattern) {
            self.object_spread_helper_atom()
        } else {
            iterator_helper
        };
        let temporary_count = wake_ecma_transform::destructuring_temporary_count(pattern);
        let temporaries = (0..temporary_count)
            .map(|_| self.fresh_transform_atom())
            .collect::<Vec<_>>();
        let declarator = VariableDeclarator {
            span: pattern.span(),
            id: pattern,
            init: Some(value),
        };
        let declarations = wake_ecma_transform::lower_variable_destructuring(
            self.arena,
            self.interner,
            iterator_helper,
            object_helper,
            self.options.transform_features,
            kind,
            declarator,
            &temporaries,
        );
        self.alloc(VariableDeclaration {
            span: pattern.span(),
            kind,
            declarations,
        })
    }

    fn parse_switch(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // switch
        self.expect(TokenKind::LParen);
        let discriminant = self.with_allow_in(true, |p| p.parse_expression());
        self.expect(TokenKind::RParen);
        self.expect(TokenKind::LBrace);
        let mut cases = self.new_vec::<SwitchCase>();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let clo = self.start();
            let test = if self.eat_keyword(Keyword::Case) {
                let t = self.with_allow_in(true, |p| p.parse_expression());
                Some(t)
            } else {
                self.expect(TokenKind::Keyword(Keyword::Default));
                None
            };
            self.expect(TokenKind::Colon);
            let mut consequent = self.new_vec::<Statement>();
            while !self.at(TokenKind::RBrace)
                && !self.at_keyword(Keyword::Case)
                && !self.at_keyword(Keyword::Default)
                && !self.at(TokenKind::Eof)
            {
                consequent.push(self.parse_statement());
            }
            cases.push(SwitchCase {
                span: self.span_to(clo),
                test,
                consequent,
            });
        }
        self.expect(TokenKind::RBrace);
        Statement::Switch(self.alloc(SwitchStatement {
            span: self.span_to(lo),
            discriminant,
            cases,
        }))
    }

    fn parse_return(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // return
        let argument = if self.newline_before() || self.at_statement_end() {
            None
        } else {
            Some(self.with_allow_in(true, |p| p.parse_expression()))
        };
        self.semicolon();
        Statement::Return(self.alloc(ReturnStatement {
            span: self.span_to(lo),
            argument,
        }))
    }

    fn parse_break_continue(&mut self, lo: u32, is_break: bool) -> Statement<'a> {
        self.bump(); // break/continue
        let label = if !self.newline_before() && self.at_ident_name() {
            Some(self.parse_ident_name())
        } else {
            None
        };
        self.semicolon();
        let span = self.span_to(lo);
        if is_break {
            Statement::Break(self.alloc(BreakStatement { span, label }))
        } else {
            Statement::Continue(self.alloc(ContinueStatement { span, label }))
        }
    }

    fn parse_throw(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // throw
        if self.newline_before() {
            self.error(self.cur.span, "`throw` 后不能换行");
        }
        let argument = self.with_allow_in(true, |p| p.parse_expression());
        self.semicolon();
        Statement::Throw(self.alloc(ThrowStatement {
            span: self.span_to(lo),
            argument,
        }))
    }

    fn parse_try(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // try
        let block = self.parse_block();
        let handler = if self.eat_keyword(Keyword::Catch) {
            let clo = self.span_to(lo).lo;
            let param = if self.eat(TokenKind::LParen) {
                let p = self.parse_binding_pattern();
                self.expect(TokenKind::RParen);
                Some(p)
            } else {
                None
            };
            let param = if param.is_none()
                && self.lowers(wake_ecma_transform::EcmaFeature::OptionalCatchBinding)
            {
                let temp = self.fresh_transform_atom();
                wake_ecma_transform::lower_optional_catch_binding(
                    self.arena,
                    temp,
                    self.options.transform_features,
                    param,
                    self.span_to(clo),
                )
            } else {
                param
            };
            let body = self.parse_block();
            let (param, body) = match param {
                Some(pattern)
                    if LOWER
                        && wake_ecma_transform::binding_pattern_needs_lowering(
                            self.options.transform_features,
                            pattern,
                        ) =>
                {
                    let (pattern, body) = self.lower_catch_destructuring(pattern, body);
                    (Some(pattern), body)
                }
                _ => (param, body),
            };
            Some(self.alloc(CatchClause {
                span: self.span_to(clo),
                param,
                body,
            }))
        } else {
            None
        };
        let finalizer = if self.eat_keyword(Keyword::Finally) {
            Some(self.parse_block())
        } else {
            None
        };
        if handler.is_none() && finalizer.is_none() {
            self.error(self.span_to(lo), "`try` 需要 catch 或 finally");
        }
        Statement::Try(self.alloc(TryStatement {
            span: self.span_to(lo),
            block,
            handler,
            finalizer,
        }))
    }

    /// Catch-pattern bindings live outside the catch body lexical environment. Preserve that
    /// separation by keeping the original body as a nested block after the generated initializer.
    fn lower_catch_destructuring(
        &mut self,
        pattern: Pattern<'a>,
        body: &'a BlockStatement<'a>,
    ) -> (Pattern<'a>, &'a BlockStatement<'a>) {
        let value_atom = self.fresh_transform_atom();
        let value_ident = self.alloc(Ident::new(pattern.span(), value_atom));
        let initialization = self.lower_destructuring_binding_declaration(
            VarKind::Let,
            pattern,
            Expression::Identifier(value_ident),
        );
        let mut statements = self.new_vec::<Statement>();
        statements.push(Statement::VariableDeclaration(initialization));
        statements.push(Statement::Block(body));
        let lowered_body = self.alloc(BlockStatement {
            span: body.span,
            body: statements,
        });
        (Pattern::Ident(value_ident), lowered_body)
    }

    fn parse_with(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // with
        self.expect(TokenKind::LParen);
        let object = self.with_allow_in(true, |p| p.parse_expression());
        self.expect(TokenKind::RParen);
        let body = self.parse_statement();
        Statement::With(self.alloc(WithStatement {
            span: self.span_to(lo),
            object,
            body,
        }))
    }

    // ==================================================================
    // 函数 / 类
    // ==================================================================

    pub(crate) fn parse_function_expression(&mut self, lo: u32, is_async: bool) -> Expression<'a> {
        Expression::Function(self.parse_function(lo, is_async))
    }

    /// `function` 声明/表达式共用（`function` 关键字尚未消费；async 已由调用方消费）。
    pub(crate) fn parse_function(&mut self, lo: u32, is_async: bool) -> &'a Function<'a> {
        self.expect(TokenKind::Keyword(Keyword::Function));
        let is_generator = self.eat(TokenKind::Star);
        let id = if self.at_ident_name() {
            Some(self.parse_binding_ident())
        } else {
            None
        };
        self.ts_type_parameters(); // `function foo<T>(...)`

        let saved = (self.ctx.in_async, self.ctx.in_generator, self.ctx.top_level);
        self.ctx.in_async = is_async;
        self.ctx.in_generator = is_generator;
        self.ctx.top_level = false;
        self.push_transform_temp_scope(false);
        let params = self.parse_params();
        self.ts_type_annotation(); // 返回类型 `): T {`（类型文法遇 `{` 自然停）
        let _ = self.pop_transform_temp_scope();
        let body = self.parse_function_body();
        self.ctx.in_async = saved.0;
        self.ctx.in_generator = saved.1;
        self.ctx.top_level = saved.2;

        let function = self.alloc(Function {
            span: self.span_to(lo),
            id,
            params,
            body: Some(body),
            is_async,
            is_generator,
        });
        self.lower_parsed_function_parameters(function)
    }

    pub(crate) fn parse_method_function(
        &mut self,
        lo: u32,
        is_async: bool,
        is_generator: bool,
    ) -> &'a Function<'a> {
        self.ts_type_parameters(); // 方法泛型 `m<T>()`
        let saved = (self.ctx.in_async, self.ctx.in_generator, self.ctx.top_level);
        self.ctx.in_async = is_async;
        self.ctx.in_generator = is_generator;
        self.ctx.top_level = false;
        self.push_transform_temp_scope(false);
        let (params, param_props) = self.parse_params_collecting();
        self.ts_type_annotation(); // 方法返回类型（含类型谓词）
        let _ = self.pop_transform_temp_scope();
        // 无函数体 → 重载签名 / abstract / declare 方法：body 置 None，供 class 层擦除。
        let body = if self.at(TokenKind::LBrace) {
            let b = self.parse_function_body();
            // TS 参数属性：在 `super(...)` 之后（或体首）注入 `this.x = x`（仅构造函数会有 props）。
            if param_props.is_empty() {
                Some(b)
            } else {
                Some(self.inject_param_props(b, &param_props))
            }
        } else {
            self.semicolon();
            None
        };
        self.ctx.in_async = saved.0;
        self.ctx.in_generator = saved.1;
        self.ctx.top_level = saved.2;
        let function = self.alloc(Function {
            span: self.span_to(lo),
            id: None,
            params,
            body,
            is_async,
            is_generator,
        });
        self.lower_parsed_function_parameters(function)
    }

    fn lower_parsed_function_parameters(&mut self, function: &'a Function<'a>) -> &'a Function<'a> {
        if !LOWER {
            return function;
        }

        let temporary_count = wake_ecma_transform::complex_parameter_temporary_count_for_features(
            self.options.transform_features,
            &function.params,
        );
        let needs_binding_lowering = function.params.iter().copied().any(|param| {
            wake_ecma_transform::binding_pattern_needs_lowering(
                self.options.transform_features,
                param,
            )
        });
        let needs_parameter_lowering = self
            .lowers(wake_ecma_transform::EcmaFeature::FunctionParameters)
            || needs_binding_lowering;
        if !needs_parameter_lowering {
            return function;
        }
        if function.body.is_some_and(|body| {
            wake_ecma_transform::parameter_lowering_has_body_binding_conflict(
                self.options.transform_features,
                &function.params,
                body,
            )
        }) {
            // Source parameter expressions execute outside the body lexical/variable
            // environments. Preserve the whole list when moving one would capture a body
            // declaration; importantly, do this before allocating any lowering helper/temp.
            return function;
        }
        let iterator_helper = if wake_ecma_transform::complex_parameters_need_iterator_helper(
            self.options.transform_features,
            &function.params,
        ) {
            self.spread_helper_atom()
        } else {
            self.interner.intern("__wake_unused_iterator_helper")
        };
        let object_helper = if function
            .params
            .iter()
            .copied()
            .any(wake_ecma_transform::pattern_has_object_rest)
        {
            self.object_spread_helper_atom()
        } else {
            iterator_helper
        };
        let temporaries = (0..temporary_count)
            .map(|_| self.fresh_transform_atom())
            .collect::<Vec<_>>();
        wake_ecma_transform::lower_complex_parameters(
            self.arena,
            self.interner,
            iterator_helper,
            object_helper,
            self.options.transform_features,
            function,
            &temporaries,
        )
    }

    /// 在函数体的 `super(...)` 之后（无则体首）注入参数属性赋值 `this.name = name`。
    fn inject_param_props(
        &self,
        body: &'a FunctionBody<'a>,
        props: &[(wake_common::Atom, Span)],
    ) -> &'a FunctionBody<'a> {
        // 找到 super 调用语句的位置（其后插入）。
        let insert_at = body
            .statements
            .iter()
            .position(|s| is_super_call_stmt(s))
            .map(|i| i + 1)
            .unwrap_or(0);
        let mut stmts = self.new_vec::<Statement>();
        for (i, s) in body.statements.iter().enumerate() {
            if i == insert_at {
                for &(name, span) in props {
                    stmts.push(self.this_assign_stmt(name, span));
                }
            }
            stmts.push(*s);
        }
        // super 在末尾或空体：补在最后。
        if insert_at >= body.statements.len() {
            for &(name, span) in props {
                stmts.push(self.this_assign_stmt(name, span));
            }
        }
        self.alloc(FunctionBody {
            span: body.span,
            statements: stmts,
            strict: body.strict,
        })
    }

    /// 构造 `this.name = name;` 语句。
    ///
    /// 属性名与右值标识符复用参数在源码中的真实 span（`name_span`），使它们与源码里对同名
    /// 属性 / 同一参数的其它访问在 prop-mangle / identifier-mangle 侧表中被一致处理；否则若
    /// 用 `Span::DUMMY`，多个参数属性会在按 span 索引的侧表上互相碰撞（last-write-wins），
    /// 且与源码访问的重命名不一致。外层 Member/Assign/Statement 仍用 DUMMY（codegen 对
    /// DUMMY 语句/表达式一律原样发射，不参与语句级合并与常量折叠）。
    fn this_assign_stmt(&self, name: wake_common::Atom, name_span: Span) -> Statement<'a> {
        let member = Expression::Member(self.alloc(MemberExpression {
            span: Span::DUMMY,
            object: Expression::This(Span::DUMMY),
            property: MemberProperty::Ident(Ident::new(name_span, name)),
            optional: false,
        }));
        let rhs = Expression::Identifier(self.alloc(Ident::new(name_span, name)));
        let assign = Expression::Assignment(self.alloc(AssignmentExpression {
            span: Span::DUMMY,
            operator: AssignmentOperator::Assign,
            left: member,
            right: rhs,
        }));
        Statement::Expression(self.alloc(ExpressionStatement {
            span: Span::DUMMY,
            expression: assign,
        }))
    }

    fn parse_params(&mut self) -> AVec<'a, Pattern<'a>> {
        let (params, _props) = self.parse_params_collecting();
        params
    }

    /// 解析形参，并返回「参数属性」名字（带修饰符的简单标识符参数）——供构造函数注入 `this.x = x`。
    fn parse_params_collecting(
        &mut self,
    ) -> (AVec<'a, Pattern<'a>>, Vec<(wake_common::Atom, Span)>) {
        self.expect(TokenKind::LParen);
        let mut params = self.new_vec::<Pattern>();
        let mut param_props: Vec<(wake_common::Atom, Span)> = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            // 参数装饰器 `@dec` —— 消费（暂不应用到参数）。
            if self.ts {
                self.skip_decorators();
            }
            // TS：`this` 参数（`function f(this: T, ..)`）—— 纯类型，擦除。
            if self.ts && self.at_keyword(Keyword::This) && self.peek().kind == TokenKind::Colon {
                self.bump(); // this
                self.ts_type_annotation(); // : T
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                continue;
            }
            // TS：参数属性/修饰符（public/private/protected/readonly/override）。
            let is_param_prop = self.ts && self.ts_skip_param_modifiers();
            if self.at(TokenKind::DotDotDot) {
                let rlo = self.start();
                self.bump();
                let argument = self.parse_binding_pattern();
                if self.ts {
                    self.ts_type_annotation(); // `...args: T[]`
                }
                params.push(Pattern::Rest(self.alloc(RestElement {
                    span: self.span_to(rlo),
                    argument,
                })));
                break;
            }
            let param = self.parse_binding_element();
            if is_param_prop && let Some(name_span) = param_prop_name(&param) {
                param_props.push(name_span);
            }
            params.push(param);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen);
        (params, param_props)
    }

    /// `declare ...` 环境声明 → 整体擦除为空语句。
    fn parse_declare(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // declare
        self.ts_skip_ambient();
        Statement::Empty(self.span_to(lo))
    }

    pub(crate) fn parse_function_body(&mut self) -> &'a FunctionBody<'a> {
        self.parse_function_body_with_transform_temps(true)
    }

    pub(crate) fn parse_function_body_with_transform_temps(
        &mut self,
        enable_transform_temps: bool,
    ) -> &'a FunctionBody<'a> {
        let lo = self.start();
        // Once a function/method/arrow body starts, an enclosing parenthesized cover no longer
        // makes expressions inside this independent scope ambiguous arrow parameters. Restore the
        // outer flag after the body so the containing cover can still finish conservatively.
        let previous_cover = self.in_cover_paren;
        self.in_cover_paren = false;
        self.push_transform_temp_scope(enable_transform_temps);
        self.expect(TokenKind::LBrace);
        let mut statements = self.new_vec::<Statement>();
        let strict = self.parse_directive_prologue(&mut statements);
        let saved_strict = self.ctx.strict;
        if strict {
            self.ctx.strict = true;
        }
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let before = self.cur.span.lo;
            statements.push(self.parse_statement());
            if self.cur.span.lo == before && !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof)
            {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        self.ctx.strict = saved_strict;
        let transform_temps = self.pop_transform_temp_scope();
        self.in_cover_paren = previous_cover;
        let statements = self.inject_transform_temp_declaration(statements, &transform_temps);
        self.alloc(FunctionBody {
            span: self.span_to(lo),
            statements,
            strict,
        })
    }

    pub(crate) fn parse_class(&mut self, lo: u32) -> &'a Class<'a> {
        let decorators = self.new_vec::<Expression>();
        self.parse_class_with_decorators(lo, decorators)
    }

    /// 同上，但带已解析的**类装饰器**（`@dec class C {}`）。
    pub(crate) fn parse_class_with_decorators(
        &mut self,
        lo: u32,
        class_decorators: wake_ecma_ast::AVec<'a, Expression<'a>>,
    ) -> &'a Class<'a> {
        self.expect(TokenKind::Keyword(Keyword::Class));
        let id = if self.at_ident_name() && !self.at_keyword(Keyword::Extends) {
            Some(self.parse_binding_ident())
        } else {
            None
        };
        self.ts_type_parameters(); // class C<T>
        let super_class = if self.eat_keyword(Keyword::Extends) {
            let sc = self.parse_lhs_expression();
            // TS：`extends Base<T>` 的超类类型实参擦除。
            if self.ts && self.at(TokenKind::Lt) {
                self.ts_type_arguments();
            }
            Some(sc)
        } else {
            None
        };
        // TS：`implements I, J<K>` 擦除。
        if self.ts && self.at_keyword(Keyword::Implements) {
            self.bump();
            loop {
                self.ts_type();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(TokenKind::LBrace);
        let saved_strict = self.ctx.strict;
        self.ctx.strict = true; // 类体恒严格。
        self.push_transform_temp_scope(false);
        let mut body = self.new_vec::<ClassMember>();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if self.eat(TokenKind::Semicolon) {
                continue;
            }
            let before = self.cur.span.lo;
            if let Some(member) = self.parse_class_member() {
                body.push(member);
            }
            if self.cur.span.lo == before && !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof)
            {
                self.bump();
            }
        }
        let _ = self.pop_transform_temp_scope();
        self.expect(TokenKind::RBrace);
        self.ctx.strict = saved_strict;
        self.alloc(Class {
            decorators: class_decorators,
            span: self.span_to(lo),
            id,
            super_class,
            body,
        })
    }

    fn parse_class_member(&mut self) -> Option<ClassMember<'a>> {
        let lo = self.start();

        // 成员装饰器 `@dec method() {}` / `@dec prop = 1`（TC39 Stage-3）。
        let mut member_decorators = self.new_vec::<Expression>();
        if self.at(TokenKind::At) {
            member_decorators = self.parse_decorators();
        }

        // —— 成员修饰符（含 TS：public/private/protected/readonly/abstract/override/declare/accessor）——
        // 某词仅在其后不是「成员终止符」时才算修饰符，否则它本身是成员名（如 `private() {}`）。
        let mut is_static = false;
        let mut erase_member = false; // abstract / declare 成员 → 擦除
        // `accessor x = 1`（auto-accessor，TC39）：需单独记录以便降级为私有存储 + get/set 对。
        let mut is_accessor = false;
        loop {
            if self.at_keyword(Keyword::Static) && !self.peek_is_member_terminator() {
                self.bump();
                is_static = true;
                continue;
            }
            if matches!(
                self.cur.kind,
                TokenKind::Keyword(Keyword::Public)
                    | TokenKind::Keyword(Keyword::Private)
                    | TokenKind::Keyword(Keyword::Protected)
            ) && !self.peek_is_member_terminator()
            {
                self.bump();
                continue;
            }
            // `accessor` 不是 TS-only：TC39 auto-accessor 在纯 JS 中同样合法。
            if self.at_contextual("accessor") && !self.peek_is_member_terminator() {
                self.bump();
                is_accessor = true;
                continue;
            }
            if self.ts
                && (self.at_contextual("readonly") || self.at_contextual("override"))
                && !self.peek_is_member_terminator()
            {
                self.bump();
                continue;
            }
            if self.ts
                && (self.at_contextual("abstract") || self.at_contextual("declare"))
                && !self.peek_is_member_terminator()
            {
                self.bump();
                erase_member = true;
                continue;
            }
            break;
        }

        // static 块。
        if is_static && self.at(TokenKind::LBrace) {
            self.bump(); // {
            // static 块不是 async 上下文，也不再是模块顶层（规范禁止其中出现 `await`）。
            let saved = (self.ctx.in_async, self.ctx.top_level);
            self.ctx.in_async = false;
            self.ctx.top_level = false;
            let mut body = self.new_vec::<Statement>();
            while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                let before = self.cur.span.lo;
                body.push(self.parse_statement());
                if self.cur.span.lo == before && !self.at(TokenKind::RBrace) {
                    self.bump();
                }
            }
            self.ctx.in_async = saved.0;
            self.ctx.top_level = saved.1;
            self.expect(TokenKind::RBrace);
            return Some(ClassMember::StaticBlock(self.alloc(StaticBlock {
                span: self.span_to(lo),
                body,
            })));
        }

        // TS 索引签名 `[k: T]: U;` → 擦除。
        if self.ts && self.at(TokenKind::LBracket) && self.is_index_signature() {
            self.ts_skip_balanced(); // [ ... ]
            self.ts_type_annotation(); // : U
            self.semicolon();
            return None;
        }

        let mut kind = MethodKind::Method;
        let mut is_async = false;
        let mut is_generator = false;

        if self.at_keyword(Keyword::Get) && !self.peek_is_member_terminator() {
            self.bump();
            kind = MethodKind::Get;
        } else if self.at_keyword(Keyword::Set) && !self.peek_is_member_terminator() {
            self.bump();
            kind = MethodKind::Set;
        } else {
            if self.at_keyword(Keyword::Async)
                && !self.peek_is_member_terminator()
                && !self.peek().newline_before
            {
                self.bump();
                is_async = true;
            }
            if self.at(TokenKind::Star) {
                self.bump();
                is_generator = true;
            }
        }

        let (key, computed) = self.parse_property_key();

        // TS：成员名后的可选 `?` / 明确赋值 `!`。
        if self.ts {
            self.eat(TokenKind::Question);
            self.eat(TokenKind::Bang);
        }

        // 方法（含泛型方法 `m<T>()`）。
        if self.at(TokenKind::LParen)
            || (self.ts && self.at(TokenKind::Lt))
            || matches!(kind, MethodKind::Get | MethodKind::Set)
            || is_async
            || is_generator
        {
            let is_ctor = !is_static
                && !computed
                && matches!(kind, MethodKind::Method)
                && self.key_is_named(&key, "constructor");
            let mkind = if is_ctor {
                MethodKind::Constructor
            } else {
                kind
            };
            let value = self.parse_method_function(lo, is_async, is_generator);
            // 无函数体（重载签名 / abstract / declare 方法）→ 擦除。
            value.body?;
            return Some(ClassMember::Method(self.alloc(MethodDefinition {
                span: self.span_to(lo),
                key,
                value,
                kind: mkind,
                is_static,
                computed,
                decorators: member_decorators,
            })));
        }

        // 字段（class property）：TS 类型注解已随 `?`/`!` 之后消费。
        self.ts_type_annotation();
        let value = if self.eat(TokenKind::Eq) {
            Some(self.parse_assignment_expression())
        } else {
            None
        };
        self.semicolon();
        // abstract / declare 字段 → 擦除（无运行时）。
        if erase_member {
            return None;
        }
        Some(ClassMember::Property(self.alloc(PropertyDefinition {
            span: self.span_to(lo),
            key,
            value,
            is_static,
            computed,
            decorators: member_decorators,
            accessor: is_accessor,
        })))
    }

    /// 试探判断 `[` 是否为 TS 索引签名 `[ident: type]`（而非计算成员名 `[expr]`）。
    fn is_index_signature(&mut self) -> bool {
        // `[` 后紧跟 标识符 + `:`。用 checkpoint 试探。
        let cp = self.checkpoint();
        self.bump(); // [
        let yes = self.at_ident_name() && self.peek().kind == TokenKind::Colon;
        self.rewind(cp);
        yes
    }

    fn key_is_named(&self, key: &PropertyKey<'a>, name: &str) -> bool {
        matches!(key, PropertyKey::Ident(id) if self.interner_eq(id.name, name))
    }

    /// 比较一个 Atom 是否等于给定字符串（冷路径：类/对象属性名判定）。热路径应预驻留 Atom 后比 u32。
    fn interner_eq(&self, atom: wake_common::Atom, s: &str) -> bool {
        self.interner.with_resolved(atom, |resolved| resolved == s)
    }

    fn peek_is_member_terminator(&mut self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::LParen
                | TokenKind::Eq
                | TokenKind::Semicolon
                | TokenKind::RBrace
                | TokenKind::Colon
        )
    }

    // ==================================================================
    // 模式（绑定 / 解构）
    // ==================================================================

    pub(crate) fn parse_binding_ident(&mut self) -> Ident {
        let span = self.cur.span;
        if self.at_ident_name() {
            let name = self.intern_ident(span);
            self.bump();
            Ident::new(span, name)
        } else {
            self.error(span, "期望绑定标识符");
            Ident::new(span, self.interner.intern("__error__"))
        }
    }

    /// 标识符名（成员/属性/标签用，接受任意关键字作名字）。
    pub(crate) fn parse_ident_name(&mut self) -> Ident {
        let span = self.cur.span;
        match self.cur.kind {
            TokenKind::Ident | TokenKind::Keyword(_) => {
                let name = self.intern_ident(span);
                self.bump();
                Ident::new(span, name)
            }
            _ => {
                self.error(span, "期望标识符名");
                Ident::new(span, self.interner.intern("__error__"))
            }
        }
    }

    pub(crate) fn parse_binding_pattern(&mut self) -> Pattern<'a> {
        match self.cur.kind {
            TokenKind::LBracket => self.parse_array_pattern(),
            TokenKind::LBrace => self.parse_object_pattern(),
            _ => {
                let id = self.parse_binding_ident();
                Pattern::Ident(self.alloc(id))
            }
        }
    }

    fn parse_binding_element(&mut self) -> Pattern<'a> {
        let lo = self.start();
        let pat = self.parse_binding_pattern();
        if self.ts {
            self.eat(TokenKind::Question); // 可选参数 `x?`
            self.ts_type_annotation(); // `: T`
        }
        if self.eat(TokenKind::Eq) {
            let right = self.parse_assignment_expression();
            Pattern::Assignment(self.alloc(AssignmentPattern {
                span: self.span_to(lo),
                left: pat,
                right,
            }))
        } else {
            pat
        }
    }

    fn parse_array_pattern(&mut self) -> Pattern<'a> {
        let lo = self.start();
        self.bump(); // [
        let mut elements = self.new_vec::<Option<Pattern>>();
        while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::Comma) {
                self.bump();
                elements.push(None);
                continue;
            }
            if self.at(TokenKind::DotDotDot) {
                let rlo = self.start();
                self.bump();
                let argument = self.parse_binding_pattern();
                elements.push(Some(Pattern::Rest(self.alloc(RestElement {
                    span: self.span_to(rlo),
                    argument,
                }))));
                break;
            }
            elements.push(Some(self.parse_binding_element()));
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBracket);
        Pattern::Array(self.alloc(ArrayPattern {
            span: self.span_to(lo),
            elements,
        }))
    }

    fn parse_object_pattern(&mut self) -> Pattern<'a> {
        let lo = self.start();
        self.bump(); // {
        let mut properties = self.new_vec::<ObjectPatternProperty>();
        let mut rest = None;
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::DotDotDot) {
                let rlo = self.start();
                self.bump();
                let argument = self.parse_binding_pattern();
                rest = Some(self.alloc(RestElement {
                    span: self.span_to(rlo),
                    argument,
                }));
                break;
            }
            let plo = self.start();
            let (key, computed) = self.parse_property_key();
            let value = if self.eat(TokenKind::Colon) {
                self.parse_binding_element()
            } else {
                // 简写 `{ x }` 或 `{ x = default }`。
                let id = match key {
                    PropertyKey::Ident(id) => id,
                    _ => {
                        self.error(self.span_to(plo), "解构简写需要标识符");
                        Ident::new(self.span_to(plo), self.interner.intern("__error__"))
                    }
                };
                if self.eat(TokenKind::Eq) {
                    let right = self.parse_assignment_expression();
                    Pattern::Assignment(self.alloc(AssignmentPattern {
                        span: self.span_to(plo),
                        left: Pattern::Ident(self.alloc(id)),
                        right,
                    }))
                } else {
                    Pattern::Ident(self.alloc(id))
                }
            };
            properties.push(ObjectPatternProperty {
                span: self.span_to(plo),
                key,
                value,
                shorthand: !computed,
                computed,
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace);
        Pattern::Object(self.alloc(ObjectPattern {
            span: self.span_to(lo),
            properties,
            rest,
        }))
    }

    // ==================================================================
    // 模块：import / export（含依赖提取）
    // ==================================================================

    fn parse_import_declaration(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // import

        // TS：`import type ...`（类型-only 导入）→ 整体擦除，不产生运行时依赖。
        // 区分 `import type from '...'`（`type` 是默认绑定名，非类型导入）。
        if self.ts && self.at_contextual("type") {
            let p = self.peek();
            let type_only = match p.kind {
                TokenKind::LBrace | TokenKind::Star => true,
                TokenKind::Ident => self.slice(p.span) != "from",
                TokenKind::Keyword(kw) if !kw.is_reserved() => self.slice(p.span) != "from",
                _ => false,
            };
            if type_only {
                self.bump(); // type
                // `import type A = require('m')` / `import type A = N.B`：右侧按表达式消费
                // （其中的 `require('m')` 会被 `maybe_record_require` 记为依赖，故解析后
                // 截断依赖列表——类型-only 导入不产生任何运行时依赖）。
                if self.at_ident_name() && self.peek().kind == TokenKind::Eq {
                    self.bump(); // A
                    self.bump(); // =
                    let dep_mark = self.dependencies.len();
                    let _ = self.with_allow_in(true, |p| p.parse_assignment_expression());
                    self.dependencies.truncate(dep_mark);
                    self.semicolon();
                    return Statement::Empty(self.span_to(lo));
                }
                // `import type X from 'm'` / `import type { A } from 'm'` / `import type * as N from 'm'`
                while !self.at(TokenKind::Str)
                    && !self.at(TokenKind::Semicolon)
                    && !self.at(TokenKind::Eof)
                {
                    self.bump();
                }
                self.eat(TokenKind::Str);
                let _ = self.parse_import_attributes();
                self.semicolon();
                return Statement::Empty(self.span_to(lo));
            }
        }

        // TS：`import x = require('m')` / `import A = N.B.C`（import-equals）。
        // 放在 default 导入分支之前——两者都以标识符起始，靠其后的 `=` 区分。
        if self.ts && self.at_ident_name() && self.peek().kind == TokenKind::Eq {
            return self.parse_import_equals(lo);
        }

        let mut specifiers = self.new_vec::<ImportSpecifier>();

        // import 'side-effect';
        if self.at(TokenKind::Str) {
            let source = self.string_atom(self.cur.span);
            let sp = self.cur.span;
            self.bump();
            let attributes = self.parse_import_attributes();
            self.semicolon();
            self.record_dependency(Dependency {
                specifier: source,
                kind: DependencyKind::Import,
                span: sp,
            });
            return Statement::Import(self.alloc(ImportDeclaration {
                span: self.span_to(lo),
                specifiers,
                source,
                attributes,
            }));
        }

        // default 导入。
        if self.at_ident_name() {
            let slo = self.start();
            let local = self.parse_binding_ident();
            specifiers.push(ImportSpecifier::Default {
                span: self.span_to(slo),
                local,
            });
            self.eat(TokenKind::Comma);
        }

        // namespace 或 named。
        if self.at(TokenKind::Star) {
            let slo = self.start();
            self.bump();
            self.expect(TokenKind::Keyword(Keyword::As));
            let local = self.parse_binding_ident();
            specifiers.push(ImportSpecifier::Namespace {
                span: self.span_to(slo),
                local,
            });
        } else if self.at(TokenKind::LBrace) {
            self.bump();
            while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                let slo = self.start();
                // TS：内联类型说明符 `import { type A, B }` → 跳过该说明符（`type` 后接名字且非 `as`）。
                if self.ts && self.at_contextual("type") && self.ts_inline_type_specifier_ahead() {
                    self.bump(); // type
                    let _ = self.parse_module_export_name();
                    if self.eat_keyword(Keyword::As) {
                        self.parse_binding_ident();
                    }
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                    continue;
                }
                let imported = self.parse_module_export_name();
                let local = if self.eat_keyword(Keyword::As) {
                    self.parse_binding_ident()
                } else {
                    match imported {
                        ModuleExportName::Ident(id) => id,
                        ModuleExportName::String(_) => {
                            self.error(self.span_to(slo), "字符串导入名需 `as` 本地绑定");
                            Ident::new(self.span_to(slo), self.interner.intern("__error__"))
                        }
                    }
                };
                specifiers.push(ImportSpecifier::Named {
                    span: self.span_to(slo),
                    imported,
                    local,
                });
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBrace);
        }

        // from 'source'
        self.expect(TokenKind::Keyword(Keyword::From));
        let source = self.expect_string_specifier();
        let attributes = self.parse_import_attributes();
        self.semicolon();
        self.record_dependency(Dependency {
            specifier: source,
            kind: DependencyKind::Import,
            span: self.span_to(lo),
        });
        Statement::Import(self.alloc(ImportDeclaration {
            span: self.span_to(lo),
            specifiers,
            source,
            attributes,
        }))
    }

    /// TS `import x = require('m');` / `import A = N.B.C;` → `const x = require('m');` /
    /// `var A = N.B.C;`（对齐 tsc 的 CommonJS emit）。
    ///
    /// 右侧**直接按表达式解析**：`require('m')` 天然构成 CallExpression，依赖由
    /// [`Parser::maybe_record_require`] 在构建时自动记为 [`DependencyKind::Require`]，
    /// codegen 的 `emit_require_call` 再把它改写成 `__wake_require__(id)`——整条链路复用既有
    /// CJS 机制，bundler 无需改动。实体名 `N.B.C` 则天然构成成员链。
    ///
    /// kind 的选择对齐 tsc：`require` 形态用 `const`（不可变导入绑定，且避免 `var` 让模块失去
    /// `{}` 块隔离资格）；实体名别名用 `var`，因为命名空间声明合并允许在别名之后才补齐段，
    /// `var` 的提升语义可避免 TDZ。
    fn parse_import_equals(&mut self, lo: u32) -> Statement<'a> {
        let name = self.parse_binding_ident();
        self.expect(TokenKind::Eq);
        let init = self.with_allow_in(true, |p| p.parse_assignment_expression());
        self.semicolon();
        let span = self.span_to(lo);
        let kind = if matches!(init, Expression::Call(_)) {
            VarKind::Const
        } else {
            VarKind::Var
        };
        let mut declarations = self.new_vec::<VariableDeclarator>();
        declarations.push(VariableDeclarator {
            span,
            id: Pattern::Ident(self.alloc(name)),
            init: Some(init),
        });
        Statement::VariableDeclaration(self.alloc(VariableDeclaration {
            span,
            kind,
            declarations,
        }))
    }

    /// 模块说明符之后的引入属性子句 `with { type: "json" }`（或已废弃的 `assert { .. }`）。
    ///
    /// 关键字前**不允许换行**（规范的 `[no LineTerminator here]`）——否则
    /// `import x from 'm'` 换行后的 `with (o) {}` 会被误吞成属性子句而非 with 语句。
    fn parse_import_attributes(&mut self) -> Option<&'a ImportAttributes<'a>> {
        if self.newline_before() {
            return None;
        }
        let keyword = if self.at_keyword(Keyword::With) {
            AttributesKeyword::With
        } else if self.at_contextual("assert") {
            AttributesKeyword::Assert
        } else {
            return None;
        };
        let lo = self.start();
        self.bump(); // with / assert
        self.expect(TokenKind::LBrace);
        let mut items = self.new_vec::<ImportAttribute>();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let ilo = self.start();
            let key = self.parse_module_export_name();
            self.expect(TokenKind::Colon);
            // 属性值只能是字符串字面量（规范限定）。
            let value = if self.at(TokenKind::Str) {
                let a = self.string_atom(self.cur.span);
                self.bump();
                a
            } else {
                self.error_expected("引入属性值（字符串字面量）");
                self.interner.intern("")
            };
            items.push(ImportAttribute {
                span: self.span_to(ilo),
                key,
                value,
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace);
        Some(self.alloc(ImportAttributes {
            span: self.span_to(lo),
            keyword,
            items: items.into_bump_slice(),
        }))
    }

    fn parse_export(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // export

        // TS：`export = expr;`（CommonJS 整体导出）→ `module.exports = expr;`，对齐 tsc 的
        // commonjs emit。wake 的模块包装器签名就是 `function(module, exports, __wake_require__)`，
        // 且 bundler 已识别「整体重新赋值 module.exports」的模块（incremental.rs
        // `reassigns_module_exports`）并保留其为独立注册模块，故无需额外运行时支持。
        // 降级结果不含任何 ESM 语句 → `program_is_esm` 为假 → 不打 `__esModule` 标记，
        // 默认导入 interop 因而拿到整个 exports 对象，与 TS 的 `export =` 语义一致。
        if self.ts && self.at(TokenKind::Eq) {
            self.bump(); // =
            let value = self.with_allow_in(true, |p| p.parse_assignment_expression());
            self.semicolon();
            return self.module_exports_assign(self.span_to(lo), value);
        }

        // TS：`export as namespace X;`（UMD 全局声明）→ 纯类型，擦除。
        if self.ts && self.at_keyword(Keyword::As) && self.peek_contextual("namespace") {
            self.bump(); // as
            self.bump(); // namespace
            if self.at_ident_name() {
                self.bump(); // X
            }
            self.semicolon();
            return Statement::Empty(self.span_to(lo));
        }

        // TS：`export type { .. } (from '..')?` / `export type * from '..'` → 整体擦除（无运行时）。
        if self.ts
            && self.at_contextual("type")
            && matches!(self.peek().kind, TokenKind::LBrace | TokenKind::Star)
        {
            self.bump(); // type
            if self.at(TokenKind::LBrace) {
                self.ts_skip_balanced();
            } else if self.eat(TokenKind::Star) && self.eat_keyword(Keyword::As) {
                let _ = self.parse_module_export_name();
            }
            if self.eat_keyword(Keyword::From) {
                self.eat(TokenKind::Str); // 类型-only：不记录运行时依赖
            }
            self.semicolon();
            return Statement::Empty(self.span_to(lo));
        }

        // export default ...
        if self.eat_keyword(Keyword::Default) {
            let declaration = if self.at_keyword(Keyword::Function) {
                ExportDefaultKind::Function(self.parse_function(self.start(), false))
            } else if self.at_keyword(Keyword::Async)
                && self.peek().kind == TokenKind::Keyword(Keyword::Function)
            {
                self.bump();
                ExportDefaultKind::Function(self.parse_function(self.start(), true))
            } else if self.at_keyword(Keyword::Class) {
                ExportDefaultKind::Class(self.parse_class(self.start()))
            } else {
                let e = self.with_allow_in(true, |p| p.parse_assignment_expression());
                self.semicolon();
                ExportDefaultKind::Expression(e)
            };
            return Statement::ExportDefault(self.alloc(ExportDefaultDeclaration {
                span: self.span_to(lo),
                declaration,
            }));
        }

        // export * (as ns)? from '...'
        if self.at(TokenKind::Star) {
            self.bump();
            let exported = if self.eat_keyword(Keyword::As) {
                Some(self.parse_module_export_name())
            } else {
                None
            };
            self.expect(TokenKind::Keyword(Keyword::From));
            let source = self.expect_string_specifier();
            let attributes = self.parse_import_attributes();
            self.semicolon();
            self.record_dependency(Dependency {
                specifier: source,
                kind: DependencyKind::ExportFrom,
                span: self.span_to(lo),
            });
            return Statement::ExportAll(self.alloc(ExportAllDeclaration {
                span: self.span_to(lo),
                exported,
                source,
                attributes,
            }));
        }

        // export { a, b as c } (from '...')?
        if self.at(TokenKind::LBrace) {
            self.bump();
            let mut specifiers = self.new_vec::<ExportSpecifier>();
            while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                let slo = self.start();
                // TS：内联类型说明符 `export { type A, B }` → 跳过该说明符。
                if self.ts && self.at_contextual("type") && self.ts_inline_type_specifier_ahead() {
                    self.bump(); // type
                    let _ = self.parse_module_export_name();
                    if self.eat_keyword(Keyword::As) {
                        let _ = self.parse_module_export_name();
                    }
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                    continue;
                }
                let local = self.parse_module_export_name();
                let exported = if self.eat_keyword(Keyword::As) {
                    self.parse_module_export_name()
                } else {
                    local
                };
                specifiers.push(ExportSpecifier {
                    span: self.span_to(slo),
                    local,
                    exported,
                });
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBrace);
            let mut attributes = None;
            let source = if self.eat_keyword(Keyword::From) {
                let s = self.expect_string_specifier();
                attributes = self.parse_import_attributes();
                self.record_dependency(Dependency {
                    specifier: s,
                    kind: DependencyKind::ExportFrom,
                    span: self.span_to(lo),
                });
                Some(s)
            } else {
                None
            };
            self.semicolon();
            return Statement::ExportNamed(self.alloc(ExportNamedDeclaration {
                span: self.span_to(lo),
                declaration: None,
                specifiers,
                source,
                attributes,
            }));
        }

        // export <declaration>
        let declaration = self.parse_statement();
        Statement::ExportNamed(self.alloc(ExportNamedDeclaration {
            span: self.span_to(lo),
            declaration: Some(declaration),
            specifiers: self.new_vec(),
            source: None,
            attributes: None,
        }))
    }

    fn parse_module_export_name(&mut self) -> ModuleExportName {
        if self.at(TokenKind::Str) {
            let atom = self.string_atom(self.cur.span);
            self.bump();
            ModuleExportName::String(atom)
        } else {
            ModuleExportName::Ident(self.parse_ident_name())
        }
    }

    /// 期望一个字符串说明符并返回其驻留 Atom（用于 from 子句）。
    fn expect_string_specifier(&mut self) -> wake_common::Atom {
        if self.at(TokenKind::Str) {
            let atom = self.string_atom(self.cur.span);
            self.bump();
            atom
        } else {
            self.error_expected("模块说明符字符串");
            self.interner.intern("")
        }
    }

    fn string_atom(&self, span: Span) -> wake_common::Atom {
        self.interner.intern(&self.lexer.string_value(span))
    }

    // ==================================================================
    // ASI
    // ==================================================================

    /// 消费语句结尾分号（ASI：换行 / `}` / EOF 处可省略）。
    fn semicolon(&mut self) {
        if self.eat(TokenKind::Semicolon) {
            return;
        }
        if self.newline_before() || self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
            return;
        }
        self.error_expected("`;`");
    }

    fn at_statement_end(&self) -> bool {
        matches!(
            self.cur.kind,
            TokenKind::Semicolon | TokenKind::RBrace | TokenKind::Eof
        )
    }
}

/// 参数属性名：简单标识符参数（含带默认值 `private x = 1`）的名字；解构参数不作参数属性。
/// 参数属性的名字 + 其在源码中的真实 span（用于注入 `this.x = x` 时保持 mangle 一致性）。
fn param_prop_name(pat: &Pattern) -> Option<(wake_common::Atom, Span)> {
    match pat {
        Pattern::Ident(id) => Some((id.name, id.span)),
        Pattern::Assignment(a) => param_prop_name(&a.left),
        _ => None,
    }
}

/// 语句是否为 `super(...)` 调用（参数属性注入的锚点）。
fn is_super_call_stmt(s: &Statement) -> bool {
    if let Statement::Expression(es) = s
        && let Expression::Call(c) = &es.expression
    {
        return matches!(c.callee, Expression::Super(_));
    }
    false
}
