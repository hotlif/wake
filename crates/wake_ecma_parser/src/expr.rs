//! 表达式解析（Pratt 优先级爬升）+ 主表达式 + cover grammar 箭头 + 模式转换。DESIGN §4.4

use wake_common::Span;
use wake_ecma_ast::*;
use wake_ecma_lexer::{Keyword, TokenKind};

use crate::Parser;

/// cover 括号：可能是箭头参数，也可能是括号/序列表达式。
struct CoverParen<'a> {
    items: AVec<'a, Expression<'a>>,
    rest: Option<&'a RestElement<'a>>,
}

#[inline]
fn has_optional_chain(expression: Expression<'_>) -> bool {
    match expression {
        Expression::Member(member) => member.optional || has_optional_chain(member.object),
        Expression::Call(call) => call.optional || has_optional_chain(call.callee),
        _ => false,
    }
}

impl<'a, 'src, const LOWER: bool> Parser<'a, 'src, LOWER> {
    // ==================================================================
    // 顶层表达式
    // ==================================================================

    /// 完整表达式（含逗号序列）。
    pub(crate) fn parse_expression(&mut self) -> Expression<'a> {
        let lo = self.start();
        let first = self.parse_assignment_expression();
        if !self.at(TokenKind::Comma) {
            return first;
        }
        let mut exprs = self.new_vec::<Expression>();
        exprs.push(first);
        while self.eat(TokenKind::Comma) {
            exprs.push(self.parse_assignment_expression());
        }
        Expression::Sequence(self.alloc(SequenceExpression {
            span: self.span_to(lo),
            expressions: exprs,
        }))
    }

    /// 赋值表达式（含箭头、yield、赋值运算符）。
    pub(crate) fn parse_assignment_expression(&mut self) -> Expression<'a> {
        let lo = self.start();

        // yield（生成器内）。
        if self.ctx.in_generator && self.at_keyword(Keyword::Yield) {
            return self.parse_yield(lo);
        }

        // async 函数 / async 箭头。
        if self.at_keyword(Keyword::Async) && !self.peek().newline_before {
            let p = self.peek();
            match p.kind {
                TokenKind::Keyword(Keyword::Function) => {
                    self.bump(); // async
                    return self.parse_function_expression(lo, true);
                }
                TokenKind::LParen => {
                    self.bump(); // async
                    let cover = self.parse_cover_paren();
                    self.skip_arrow_return_type_if_arrow();
                    if self.at(TokenKind::Arrow) && !self.newline_before() {
                        return self.finish_arrow(lo, cover, true);
                    }
                    // 实为调用 `async(args)`。
                    let callee = Expression::Identifier(self.alloc(Ident::new(
                        Span::new(lo, lo + 5),
                        self.interner.intern("async"),
                    )));
                    return Expression::Call(self.alloc(CallExpression {
                        span: self.span_to(lo),
                        callee,
                        arguments: cover.items,
                        optional: false,
                    }));
                }
                TokenKind::Lt if self.ts => {
                    // TSX 中 `async <T,>(value: T) => value` 必须先按泛型箭头试探，
                    // 否则 `<T>` 会落入比较/JSX 分支并产生级联诊断。
                    let checkpoint = self.checkpoint();
                    self.bump(); // async
                    self.ts_type_parameters();
                    if self.at(TokenKind::LParen) {
                        let cover = self.parse_cover_paren();
                        self.skip_arrow_return_type_if_arrow();
                        if self.at(TokenKind::Arrow) && !self.newline_before() {
                            return self.finish_arrow(lo, cover, true);
                        }
                    }
                    self.rewind(checkpoint);
                }
                _ if is_ident_name_kind(p.kind) => {
                    self.bump(); // async
                    let id = self.parse_binding_ident();
                    return self.finish_single_arrow(lo, id, true);
                }
                _ => {}
            }
        }

        // 单标识符箭头 `x => ...`。
        if self.at_ident_name()
            && self.peek().kind == TokenKind::Arrow
            && !self.peek().newline_before
        {
            let id = self.parse_binding_ident();
            return self.finish_single_arrow(lo, id, false);
        }

        let left = self.parse_conditional_expression();

        if let Some(op) = assignment_op(self.cur.kind) {
            self.bump();
            // In cover grammar only the left side may later become an arrow binding pattern. The
            // initializer/RHS is an ordinary expression and can safely run its own transforms.
            let previous_cover = self.in_cover_paren;
            self.in_cover_paren = false;
            let right = self.parse_assignment_expression();
            self.in_cover_paren = previous_cover;
            let features = self.options.transform_features;
            let span = self.span_to(lo);
            if !LOWER {
                return Expression::Assignment(self.alloc(AssignmentExpression {
                    span,
                    operator: op,
                    left,
                    right,
                }));
            }
            if op == AssignmentOperator::Assign
                && wake_ecma_transform::destructuring_assignment_needs_lowering(features, left)
                && !self.in_cover_paren
            {
                return self.lower_destructuring_assignment_expression(span, left, right);
            }
            let temps = wake_ecma_transform::assignment_needs_temporaries(features, op, left)
                .then_some(())
                .and_then(|()| {
                    Some([
                        self.fresh_scoped_transform_atom()?,
                        self.fresh_scoped_transform_atom()?,
                        self.fresh_scoped_transform_atom()?,
                    ])
                });
            return wake_ecma_transform::lower_assignment(
                self.arena,
                self.interner,
                temps,
                features,
                span,
                op,
                left,
                right,
            );
        }
        left
    }

    fn parse_yield(&mut self, lo: u32) -> Expression<'a> {
        self.bump(); // yield
        let delegate = self.eat(TokenKind::Star);
        // yield 无参数：遇到表达式结束标记或换行。
        let argument = if self.newline_before() || self.at_expression_end() {
            None
        } else {
            Some(self.parse_assignment_expression())
        };
        Expression::Yield(self.alloc(YieldExpression {
            span: self.span_to(lo),
            argument,
            delegate,
        }))
    }

    /// 三元条件。
    fn parse_conditional_expression(&mut self) -> Expression<'a> {
        let lo = self.start();
        let test = self.parse_binary_expression(1);
        if !self.eat(TokenKind::Question) {
            return test;
        }
        // `?:` 的两个分支：consequent 里 `in` 允许，用 assignment 级。
        let consequent = self.with_allow_in(true, |p| p.parse_assignment_expression());
        self.expect(TokenKind::Colon);
        let alternate = self.parse_assignment_expression();
        Expression::Conditional(self.alloc(ConditionalExpression {
            span: self.span_to(lo),
            test,
            consequent,
            alternate,
        }))
    }

    // ==================================================================
    // Pratt 二元 / 逻辑
    // ==================================================================

    fn parse_binary_expression(&mut self, min_prec: u8) -> Expression<'a> {
        let lo = self.start();
        let mut left = self.parse_unary_expression();

        let features = self.options.transform_features;
        loop {
            // TS：`expr as Type` / `expr satisfies Type`（同行）→ 擦除类型，保留表达式。
            if self.ts
                && !self.cur.newline_before
                && (self.at_keyword(Keyword::As) || self.at_contextual("satisfies"))
            {
                self.bump(); // as / satisfies
                // `as const` 也是类型位置；`const` 是保留字，由 ts_primary 显式消费。
                self.ts_type();
                continue;
            }
            let Some((op, prec, logical)) = self.peek_binary_op() else {
                break;
            };
            if prec < min_prec {
                break;
            }
            self.bump(); // 运算符
            // 指数右结合；其余左结合。
            let right_assoc = matches!(op, BinOp::Binary(BinaryOperator::Exp));
            let next_min = if right_assoc { prec } else { prec + 1 };
            let right = self.parse_binary_expression(next_min);
            let span = self.span_to(lo);
            if !LOWER {
                left = match op {
                    BinOp::Binary(operator) => Expression::Binary(self.alloc(BinaryExpression {
                        span,
                        operator,
                        left,
                        right,
                    })),
                    BinOp::Logical(operator) => {
                        Expression::Logical(self.alloc(LogicalExpression {
                            span,
                            operator,
                            left,
                            right,
                        }))
                    }
                };
                continue;
            }
            left = match op {
                BinOp::Binary(b) => wake_ecma_transform::lower_binary(
                    self.arena,
                    self.interner,
                    features,
                    span,
                    b,
                    left,
                    right,
                ),
                BinOp::Logical(l) => {
                    let temp = if l == LogicalOperator::Coalesce
                        && features.contains(wake_ecma_transform::EcmaFeature::NullishCoalescing)
                        && !wake_ecma_transform::is_repeatable(left)
                    {
                        if features.contains(wake_ecma_transform::EcmaFeature::ArrowFunction) {
                            self.fresh_scoped_transform_atom()
                        } else {
                            Some(self.fresh_transform_atom())
                        }
                    } else {
                        None
                    };
                    wake_ecma_transform::lower_logical(
                        self.arena, temp, features, span, l, left, right,
                    )
                }
            };
            let _ = logical;
        }
        left
    }

    /// 读取当前 token 对应的二元/逻辑运算符与优先级。`in` 在 `!allow_in` 时不作运算符。
    fn peek_binary_op(&self) -> Option<(BinOp, u8, bool)> {
        use TokenKind as T;
        let (op, prec, logical) = match self.cur.kind {
            T::Keyword(Keyword::Instanceof) => {
                (BinOp::Binary(BinaryOperator::Instanceof), 8, false)
            }
            T::Keyword(Keyword::In) if self.ctx.allow_in => {
                (BinOp::Binary(BinaryOperator::In), 8, false)
            }
            T::QuestionQuestion => (BinOp::Logical(LogicalOperator::Coalesce), 1, true),
            T::PipePipe => (BinOp::Logical(LogicalOperator::Or), 2, true),
            T::AmpAmp => (BinOp::Logical(LogicalOperator::And), 3, true),
            T::Pipe => (BinOp::Binary(BinaryOperator::BitOr), 4, false),
            T::Caret => (BinOp::Binary(BinaryOperator::BitXor), 5, false),
            T::Amp => (BinOp::Binary(BinaryOperator::BitAnd), 6, false),
            T::EqEq => (BinOp::Binary(BinaryOperator::Eq), 7, false),
            T::NotEq => (BinOp::Binary(BinaryOperator::NotEq), 7, false),
            T::EqEqEq => (BinOp::Binary(BinaryOperator::StrictEq), 7, false),
            T::NotEqEq => (BinOp::Binary(BinaryOperator::StrictNotEq), 7, false),
            T::Lt => (BinOp::Binary(BinaryOperator::Lt), 8, false),
            T::Gt => (BinOp::Binary(BinaryOperator::Gt), 8, false),
            T::LtEq => (BinOp::Binary(BinaryOperator::LtEq), 8, false),
            T::GtEq => (BinOp::Binary(BinaryOperator::GtEq), 8, false),
            T::Shl => (BinOp::Binary(BinaryOperator::Shl), 9, false),
            T::Shr => (BinOp::Binary(BinaryOperator::Shr), 9, false),
            T::Ushr => (BinOp::Binary(BinaryOperator::Ushr), 9, false),
            T::Plus => (BinOp::Binary(BinaryOperator::Add), 10, false),
            T::Minus => (BinOp::Binary(BinaryOperator::Sub), 10, false),
            T::Star => (BinOp::Binary(BinaryOperator::Mul), 11, false),
            T::Slash => (BinOp::Binary(BinaryOperator::Div), 11, false),
            T::Percent => (BinOp::Binary(BinaryOperator::Rem), 11, false),
            T::StarStar => (BinOp::Binary(BinaryOperator::Exp), 12, false),
            _ => return None,
        };
        Some((op, prec, logical))
    }

    // ==================================================================
    // 一元 / 更新
    // ==================================================================

    fn parse_unary_expression(&mut self) -> Expression<'a> {
        let lo = self.start();

        // TS 类型断言 `<Type>expr`（仅 .ts；.tsx 用 `as`，且 `<` 在 .tsx 是 JSX）。
        if self.ts && !self.jsx && self.at(TokenKind::Lt) {
            self.bump(); // <
            self.ts_type();
            self.consume_type_gt(); // >
            return self.parse_unary_expression();
        }

        // 前缀更新 ++ --。
        if matches!(self.cur.kind, TokenKind::PlusPlus | TokenKind::MinusMinus) {
            let operator = if self.at(TokenKind::PlusPlus) {
                UpdateOperator::Increment
            } else {
                UpdateOperator::Decrement
            };
            self.bump();
            let argument = self.parse_unary_expression();
            return Expression::Update(self.alloc(UpdateExpression {
                span: self.span_to(lo),
                operator,
                prefix: true,
                argument,
            }));
        }

        // 一元运算符。
        if let Some(operator) = unary_op(self.cur.kind) {
            self.bump();
            if operator == UnaryOperator::Delete
                && !(self.ts && !self.jsx && self.at(TokenKind::Lt))
                && !matches!(self.cur.kind, TokenKind::PlusPlus | TokenKind::MinusMinus)
                && unary_op(self.cur.kind).is_none()
                && !(self.at_keyword(Keyword::Await) && self.ctx.in_async)
            {
                let argument_lo = self.start();
                let (mut argument, mut optional_delete_lowered) = self
                    .parse_lhs_expression_with_optional_mode(
                        wake_ecma_transform::OptionalChainMode::Delete,
                    );
                if matches!(self.cur.kind, TokenKind::PlusPlus | TokenKind::MinusMinus)
                    && !self.newline_before()
                {
                    let update_operator = if self.at(TokenKind::PlusPlus) {
                        UpdateOperator::Increment
                    } else {
                        UpdateOperator::Decrement
                    };
                    self.bump();
                    argument = Expression::Update(self.alloc(UpdateExpression {
                        span: self.span_to(argument_lo),
                        operator: update_operator,
                        prefix: false,
                        argument,
                    }));
                    optional_delete_lowered = false;
                }
                if optional_delete_lowered {
                    return argument;
                }
                return Expression::Unary(self.alloc(UnaryExpression {
                    span: self.span_to(lo),
                    operator,
                    argument,
                }));
            }
            let argument = self.parse_unary_expression();
            return Expression::Unary(self.alloc(UnaryExpression {
                span: self.span_to(lo),
                operator,
                argument,
            }));
        }

        // await（async 内，或模块顶层）。
        if self.at_keyword(Keyword::Await) && self.ctx.in_async {
            if self.ctx.top_level {
                self.has_top_level_await = true;
            }
            self.bump();
            let argument = self.parse_unary_expression();
            return Expression::Await(self.alloc(AwaitExpression {
                span: self.span_to(lo),
                argument,
            }));
        }

        // 后缀更新。
        let expr = self.parse_lhs_expression();
        if matches!(self.cur.kind, TokenKind::PlusPlus | TokenKind::MinusMinus)
            && !self.newline_before()
        {
            let operator = if self.at(TokenKind::PlusPlus) {
                UpdateOperator::Increment
            } else {
                UpdateOperator::Decrement
            };
            self.bump();
            return Expression::Update(self.alloc(UpdateExpression {
                span: self.span_to(lo),
                operator,
                prefix: false,
                argument: expr,
            }));
        }
        expr
    }

    // ==================================================================
    // 左值 / 调用 / 成员 / new
    // ==================================================================

    pub(crate) fn parse_lhs_expression(&mut self) -> Expression<'a> {
        self.parse_lhs_expression_with_optional_mode(wake_ecma_transform::OptionalChainMode::Value)
            .0
    }

    fn parse_lhs_expression_with_optional_mode(
        &mut self,
        mode: wake_ecma_transform::OptionalChainMode,
    ) -> (Expression<'a>, bool) {
        let lo = self.start();
        let delete_cover =
            mode == wake_ecma_transform::OptionalChainMode::Delete && self.at(TokenKind::LParen);
        let previous_delete_cover = self.suppress_optional_chain_in_delete_cover;
        if delete_cover {
            self.suppress_optional_chain_in_delete_cover = true;
        }
        let expr = if self.at_keyword(Keyword::New) {
            self.parse_new_expression()
        } else {
            self.parse_primary_expression()
        };
        self.suppress_optional_chain_in_delete_cover = previous_delete_cover;
        self.parse_call_member_tail(lo, expr, mode)
    }

    fn parse_new_expression(&mut self) -> Expression<'a> {
        let lo = self.start();
        self.bump(); // new
        // new.target
        if self.at(TokenKind::Dot) {
            self.bump();
            let prop = self.parse_ident_name();
            return Expression::MetaProperty(self.alloc(MetaProperty {
                span: self.span_to(lo),
                meta: self.interner.intern("new"),
                property: prop.name,
            }));
        }
        // callee：member 表达式（不含调用）。
        let mut callee_base = if self.at_keyword(Keyword::New) {
            self.parse_new_expression()
        } else {
            self.parse_primary_expression()
        };
        if std::mem::take(&mut self.preserve_optional_chain_tail) {
            // `new (obj?.Ctor)()` has the same parenthesized chain boundary as an ordinary
            // call, but constructor invocation does not consume a Reference receiver. Lower the
            // inner value when possible and retain explicit grouping in every target mode.
            if self.lowers(wake_ecma_transform::EcmaFeature::OptionalChaining) {
                callee_base = self.lower_optional_chain_expression(
                    callee_base,
                    wake_ecma_transform::OptionalChainMode::Value,
                );
            }
            callee_base = self.parenthesized_expression(callee_base);
        }
        let callee = self.parse_member_tail_no_call(lo, callee_base);
        // TS：`new C<T>(...)` 的类型实参（仅当其后紧跟 `(` 才认定，避免误吃比较）。
        if self.ts && self.at(TokenKind::Lt) {
            self.try_ts_type_arguments();
        }
        let arguments = if self.at(TokenKind::LParen) {
            self.parse_arguments()
        } else {
            self.new_vec()
        };
        let new = self.alloc(NewExpression {
            span: self.span_to(lo),
            callee,
            arguments,
        });
        if self.lowers(wake_ecma_transform::EcmaFeature::Spread)
            && new
                .arguments
                .iter()
                .any(|argument| matches!(argument, Expression::Spread(_)))
        {
            let helper = self.spread_helper_atom();
            wake_ecma_transform::lower_new_spread(
                self.arena,
                self.interner,
                helper,
                self.options.transform_features,
                new,
            )
        } else {
            Expression::New(new)
        }
    }

    /// 只解析成员访问（`.x` / `[x]`），不解析调用——用于 `new` 的 callee。
    fn parse_member_tail_no_call(&mut self, lo: u32, mut expr: Expression<'a>) -> Expression<'a> {
        loop {
            match self.cur.kind {
                TokenKind::Dot => {
                    self.bump();
                    let property = self.parse_member_property();
                    expr = Expression::Member(self.alloc(MemberExpression {
                        span: self.span_to(lo),
                        object: expr,
                        property,
                        optional: false,
                    }));
                }
                TokenKind::LBracket => {
                    self.bump();
                    let idx = self.with_allow_in(true, |p| p.parse_expression());
                    self.expect(TokenKind::RBracket);
                    expr = Expression::Member(self.alloc(MemberExpression {
                        span: self.span_to(lo),
                        object: expr,
                        property: MemberProperty::Computed(idx),
                        optional: false,
                    }));
                }
                _ => break,
            }
        }
        expr
    }

    fn parse_call_member_tail(
        &mut self,
        lo: u32,
        mut expr: Expression<'a>,
        mode: wake_ecma_transform::OptionalChainMode,
    ) -> (Expression<'a>, bool) {
        let mut preserve_optional_chain = std::mem::take(&mut self.preserve_optional_chain_tail);
        let defer_parenthesized_invocation = preserve_optional_chain
            && (matches!(
                self.cur.kind,
                TokenKind::LParen | TokenKind::TemplateNoSub | TokenKind::TemplateHead
            ) || self.ts_parenthesized_optional_invocation_ahead());
        let mut preserve_native_parenthesized_call = false;
        let has_outer_tail = matches!(
            self.cur.kind,
            TokenKind::Dot
                | TokenKind::QuestionDot
                | TokenKind::LBracket
                | TokenKind::LParen
                | TokenKind::TemplateNoSub
                | TokenKind::TemplateHead
        ) || (self.ts
            && matches!(self.cur.kind, TokenKind::Bang | TokenKind::Lt));
        if !defer_parenthesized_invocation
            && preserve_optional_chain
            && has_outer_tail
            && self.lowers(wake_ecma_transform::EcmaFeature::OptionalChaining)
        {
            // Parentheses end an optional chain. Lower the inner chain before rebuilding an outer
            // `.x`/`()` tail so codegen cannot accidentally merge the two chain units.
            expr = self.lower_optional_chain_expression(
                expr,
                wake_ecma_transform::OptionalChainMode::Value,
            );
            preserve_optional_chain = wake_ecma_transform::has_optional_chain(expr);
        } else if preserve_optional_chain
            && mode == wake_ecma_transform::OptionalChainMode::Delete
            && !self.lowers(wake_ecma_transform::EcmaFeature::ArrowFunction)
        {
            // Arrow-capable targets can use a lexical capture after the cover has been resolved.
            // This makes `delete (get()?.x)` delete `x`, rather than deleting a conditional value.
            preserve_optional_chain = false;
        }
        loop {
            match self.cur.kind {
                TokenKind::Dot => {
                    self.bump();
                    let property = self.parse_member_property();
                    expr = Expression::Member(self.alloc(MemberExpression {
                        span: self.span_to(lo),
                        object: expr,
                        property,
                        optional: false,
                    }));
                }
                TokenKind::QuestionDot => {
                    self.bump();
                    match self.cur.kind {
                        TokenKind::LParen => {
                            let arguments = self.parse_arguments();
                            expr = Expression::Call(self.alloc(CallExpression {
                                span: self.span_to(lo),
                                callee: expr,
                                arguments,
                                optional: true,
                            }));
                        }
                        TokenKind::LBracket => {
                            self.bump();
                            let idx = self.with_allow_in(true, |p| p.parse_expression());
                            self.expect(TokenKind::RBracket);
                            expr = Expression::Member(self.alloc(MemberExpression {
                                span: self.span_to(lo),
                                object: expr,
                                property: MemberProperty::Computed(idx),
                                optional: true,
                            }));
                        }
                        _ => {
                            let property = self.parse_member_property();
                            expr = Expression::Member(self.alloc(MemberExpression {
                                span: self.span_to(lo),
                                object: expr,
                                property,
                                optional: true,
                            }));
                        }
                    }
                }
                TokenKind::LBracket => {
                    self.bump();
                    let idx = self.with_allow_in(true, |p| p.parse_expression());
                    self.expect(TokenKind::RBracket);
                    expr = Expression::Member(self.alloc(MemberExpression {
                        span: self.span_to(lo),
                        object: expr,
                        property: MemberProperty::Computed(idx),
                        optional: false,
                    }));
                }
                // TS：非空断言 `expr!`（同行）→ 擦除，继续 member 链（`a!.b`）。
                TokenKind::Bang if self.ts && !self.cur.newline_before => {
                    self.bump();
                }
                // TS：调用/标签模板的类型实参 `expr<T>(...)` / `` expr<T>`...` ``。
                // 试探跳过平衡角括号，成功且后跟 `(`/模板才擦除；否则回溯，把 `<` 交回给
                // Pratt 层当二元小于（`a < b`）。这是 `useState<number>(0)` 等高频写法的关键。
                TokenKind::Lt if self.ts => {
                    // 语法精确的类型实参试探（`f<T>(...)` / `` tag<T>`...` ``）；失败则回溯当二元 `<`。
                    if !self.try_ts_type_arguments() {
                        break;
                    }
                    // 成功：cur 现在是 `(` 或模板头，交由下一轮循环处理调用。
                }
                TokenKind::LParen => {
                    if preserve_optional_chain {
                        (expr, preserve_native_parenthesized_call) =
                            self.prepare_parenthesized_optional_callee(expr);
                        preserve_optional_chain = false;
                    }
                    let arguments = self.parse_arguments();
                    let call = self.alloc(CallExpression {
                        span: self.span_to(lo),
                        callee: expr,
                        arguments,
                        optional: false,
                    });
                    self.maybe_record_require(call);
                    if self.lowers(wake_ecma_transform::EcmaFeature::Spread)
                        && !preserve_native_parenthesized_call
                        && !wake_ecma_transform::has_optional_chain(call.callee)
                        && call
                            .arguments
                            .iter()
                            .any(|argument| matches!(argument, Expression::Spread(_)))
                    {
                        // All ordinary member calls need receiver/function captures: a getter may
                        // rebind even a simple identifier receiver before spread arguments run.
                        // Only execution scopes that can own the corresponding `var` declarations
                        // may lower; conservative regions retain the complete native call and do
                        // not allocate an unused iterator helper.
                        let can_lower = self.has_scoped_transform_temp_scope()
                            && !matches!(call.callee, Expression::Super(_));
                        let temps = if can_lower
                            && matches!(
                                call.callee,
                                Expression::Member(member)
                                    if !matches!(member.object, Expression::Super(_))
                            ) {
                            Some([
                                self.fresh_scoped_transform_atom()
                                    .expect("enabled spread-call temp scope"),
                                self.fresh_scoped_transform_atom()
                                    .expect("enabled spread-call temp scope"),
                            ])
                        } else {
                            None
                        };
                        if can_lower {
                            let helper = self.spread_helper_atom();
                            expr = wake_ecma_transform::lower_call_spread(
                                self.arena,
                                self.interner,
                                helper,
                                temps,
                                self.options.transform_features,
                                call,
                            );
                        } else {
                            expr = Expression::Call(call);
                        }
                    } else {
                        expr = Expression::Call(call);
                    }
                    preserve_native_parenthesized_call = false;
                }
                // 标签模板 `` tag`...` ``。
                TokenKind::TemplateNoSub | TokenKind::TemplateHead => {
                    if preserve_optional_chain {
                        let (prepared, _) = self.prepare_parenthesized_optional_callee(expr);
                        expr = prepared;
                        preserve_optional_chain = false;
                    }
                    let quasi = self.parse_template_literal();
                    if let Expression::TemplateLiteral(q) = quasi {
                        expr = Expression::TaggedTemplate(self.alloc(TaggedTemplateExpression {
                            span: self.span_to(lo),
                            tag: expr,
                            quasi: q,
                        }));
                    }
                    preserve_native_parenthesized_call = false;
                }
                _ => break,
            }
        }
        if !preserve_optional_chain
            && self.lowers(wake_ecma_transform::EcmaFeature::OptionalChaining)
            && wake_ecma_transform::has_optional_chain(expr)
        {
            let lowered = self.lower_optional_chain_expression(expr, mode);
            let delete_lowered = mode == wake_ecma_transform::OptionalChainMode::Delete
                && !wake_ecma_transform::has_optional_chain(lowered);
            (lowered, delete_lowered)
        } else {
            (expr, false)
        }
    }

    fn lower_optional_chain_expression(
        &mut self,
        expression: Expression<'a>,
        mode: wake_ecma_transform::OptionalChainMode,
    ) -> Expression<'a> {
        let has_call_spread = wake_ecma_transform::has_call_spread(expression);
        let needs_spread_helper =
            self.lowers(wake_ecma_transform::EcmaFeature::Spread) && has_call_spread;
        // Spread arguments may contain lexical `await`/`yield`/`this`/`arguments`. If this region
        // cannot own scope temporaries, retain the whole optional call instead of lowering it
        // through a nested lexical-IIFE path and allocating a helper that cannot be used safely.
        let force_sequence_capture = has_call_spread && self.has_scoped_transform_temp_scope();
        let temporaries = if has_call_spread {
            if force_sequence_capture {
                Some([
                    self.fresh_scoped_transform_atom()
                        .expect("enabled optional-spread temp scope"),
                    self.fresh_scoped_transform_atom()
                        .expect("enabled optional-spread temp scope"),
                ])
            } else {
                None
            }
        } else {
            self.optional_chain_temporaries()
        };
        let spread_helper = if needs_spread_helper && temporaries.is_some() {
            Some(self.spread_helper_atom())
        } else {
            None
        };
        wake_ecma_transform::lower_optional_chain(
            self.arena,
            self.interner,
            spread_helper,
            temporaries,
            force_sequence_capture,
            self.interner.intern("call"),
            self.options.transform_features,
            mode,
            expression,
        )
    }

    fn lower_parenthesized_optional_callee_expression(
        &mut self,
        expression: Expression<'a>,
    ) -> Expression<'a> {
        let temporaries = [
            self.fresh_scoped_transform_atom()
                .expect("enabled parenthesized optional-call temp scope"),
            self.fresh_scoped_transform_atom()
                .expect("enabled parenthesized optional-call temp scope"),
        ];
        let spread_helper = if self.lowers(wake_ecma_transform::EcmaFeature::Spread)
            && wake_ecma_transform::has_call_spread(expression)
        {
            Some(self.spread_helper_atom())
        } else {
            None
        };
        wake_ecma_transform::lower_parenthesized_optional_callee(
            self.arena,
            self.interner,
            spread_helper,
            temporaries,
            self.interner.intern("call"),
            self.options.transform_features,
            expression,
        )
    }

    fn prepare_parenthesized_optional_callee(
        &mut self,
        expression: Expression<'a>,
    ) -> (Expression<'a>, bool) {
        if self.lowers(wake_ecma_transform::EcmaFeature::OptionalChaining)
            && self.has_scoped_transform_temp_scope()
        {
            // Parentheses preserve a Member Reference when it is immediately invoked:
            // `(obj?.method)()` must use `obj` as `this`, while a nullish base must still
            // evaluate the outer arguments before throwing. Lower the inner chain to a
            // forwarding callable rather than flattening the outer invocation into that chain.
            (
                self.lower_parenthesized_optional_callee_expression(expression),
                false,
            )
        } else {
            // A single-item sequence is emitted with parentheses in callee/tag position. It is
            // used as an AST-level grouping marker for modern targets and conservative regions,
            // whose optional syntax must remain native without merging the outer invocation.
            (self.parenthesized_expression(expression), true)
        }
    }

    fn ts_parenthesized_optional_invocation_ahead(&mut self) -> bool {
        if !self.ts || !matches!(self.cur.kind, TokenKind::Bang | TokenKind::Lt) {
            return false;
        }
        let checkpoint = self.checkpoint();
        while self.at(TokenKind::Bang) && !self.newline_before() {
            self.bump();
        }
        let follows = if matches!(
            self.cur.kind,
            TokenKind::LParen | TokenKind::TemplateNoSub | TokenKind::TemplateHead
        ) {
            true
        } else if self.at(TokenKind::Lt) {
            self.try_ts_type_arguments()
        } else {
            false
        };
        self.rewind(checkpoint);
        follows
    }

    fn parenthesized_expression(&mut self, expression: Expression<'a>) -> Expression<'a> {
        let mut expressions = self.new_vec::<Expression>();
        expressions.push(expression);
        Expression::Sequence(self.alloc(SequenceExpression {
            span: expression.span(),
            expressions,
        }))
    }

    fn optional_chain_temporaries(&mut self) -> Option<[wake_common::Atom; 2]> {
        if self.suppress_optional_chain_in_delete_cover {
            return None;
        }
        if self.in_cover_paren
            && self.at(TokenKind::RParen)
            && matches!(
                self.peek().kind,
                TokenKind::LParen | TokenKind::TemplateNoSub | TokenKind::TemplateHead
            )
        {
            // Defer `(chain?.member)(...)` until the cover has closed. Lowering the member to a
            // lexical-IIFE value here would discard the Reference receiver before the parser can
            // see that the parenthesized expression is immediately called.
            return None;
        }
        if self.lowers(wake_ecma_transform::EcmaFeature::ArrowFunction) {
            Some([
                self.fresh_scoped_transform_atom()?,
                self.fresh_scoped_transform_atom()?,
            ])
        } else {
            Some([self.fresh_transform_atom(), self.fresh_transform_atom()])
        }
    }

    fn parse_member_property(&mut self) -> MemberProperty<'a> {
        if self.at(TokenKind::PrivateIdent) {
            let span = self.cur.span;
            self.bump();
            // 名字去掉前导 `#`（`#` 是语法记号，不入名字）。
            let name = self.intern_ident(Span::new(span.lo + 1, span.hi));
            MemberProperty::Private(Ident::new(span, name))
        } else {
            let id = self.parse_ident_name();
            MemberProperty::Ident(id)
        }
    }

    /// 解析调用实参 `( ... )`（含 `...spread`）。
    fn parse_arguments(&mut self) -> AVec<'a, Expression<'a>> {
        self.expect(TokenKind::LParen);
        let mut args = self.new_vec::<Expression>();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::DotDotDot) {
                let lo = self.start();
                self.bump();
                let argument = self.parse_assignment_expression();
                args.push(Expression::Spread(self.alloc(SpreadElement {
                    span: self.span_to(lo),
                    argument,
                })));
            } else {
                args.push(self.with_allow_in(true, |p| p.parse_assignment_expression()));
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen);
        args
    }

    // ==================================================================
    // 主表达式
    // ==================================================================

    fn parse_primary_expression(&mut self) -> Expression<'a> {
        let lo = self.start();
        let span = self.cur.span;
        match self.cur.kind {
            // JSX：表达式起始处的 `<` 解析为 JSX 元素（仅 .jsx/.tsx，DESIGN §4.3）。
            TokenKind::Lt if self.jsx => self.parse_jsx_root(),
            TokenKind::Number => {
                let value = self.lexer_number(span);
                self.bump();
                Expression::NumberLiteral(self.alloc(NumberLiteral { span, value }))
            }
            TokenKind::Str => {
                let value = self.intern_string(span);
                self.bump();
                Expression::StringLiteral(self.alloc(StringLiteral { span, value }))
            }
            TokenKind::BigInt => {
                self.bump();
                let raw = self.intern_slice(Span::new(span.lo, span.hi.saturating_sub(1)));
                Expression::BigIntLiteral(self.alloc(BigIntLiteral { span, raw }))
            }
            TokenKind::Regex => {
                self.bump();
                let (pattern, flags) = self.split_regex(span);
                Expression::RegExpLiteral(self.alloc(RegExpLiteral {
                    span,
                    pattern,
                    flags,
                }))
            }
            TokenKind::TemplateNoSub | TokenKind::TemplateHead => {
                let template = self.parse_template_literal();
                if let Expression::TemplateLiteral(template) = template {
                    if !LOWER {
                        return Expression::TemplateLiteral(template);
                    }
                    wake_ecma_transform::lower_template(
                        self.arena,
                        self.interner,
                        self.options.transform_features,
                        template,
                    )
                } else {
                    template
                }
            }
            TokenKind::Keyword(Keyword::True) => {
                self.bump();
                Expression::BooleanLiteral(self.alloc(BooleanLiteral { span, value: true }))
            }
            TokenKind::Keyword(Keyword::False) => {
                self.bump();
                Expression::BooleanLiteral(self.alloc(BooleanLiteral { span, value: false }))
            }
            TokenKind::Keyword(Keyword::Null) => {
                self.bump();
                Expression::NullLiteral(span)
            }
            TokenKind::Keyword(Keyword::This) => {
                self.bump();
                Expression::This(span)
            }
            TokenKind::Keyword(Keyword::Super) => {
                self.bump();
                Expression::Super(span)
            }
            TokenKind::Keyword(Keyword::Function) => self.parse_function_expression(lo, false),
            TokenKind::Keyword(Keyword::Class) => {
                let class = self.parse_class(lo);
                Expression::Class(class)
            }
            TokenKind::Keyword(Keyword::Import) => self.parse_import_expression(lo),
            TokenKind::LParen => {
                let cover = self.parse_cover_paren();
                self.skip_arrow_return_type_if_arrow();
                if self.at(TokenKind::Arrow) && !self.newline_before() {
                    return self.finish_arrow(lo, cover, false);
                }
                self.cover_to_expression(lo, cover)
            }
            TokenKind::LBracket => self.parse_array_expression(),
            TokenKind::LBrace => self.parse_object_expression(),
            TokenKind::Ident => {
                self.bump();
                let name = self.intern_ident(span);
                Expression::Identifier(self.alloc(Ident::new(span, name)))
            }
            TokenKind::Keyword(kw) if !kw.is_reserved() => {
                // 上下文关键字作标识符（async/of/let/...）。
                self.bump();
                let name = self.intern_ident(span);
                Expression::Identifier(self.alloc(Ident::new(span, name)))
            }
            _ => {
                self.error(span, "期望一个表达式");
                self.bump();
                // 恢复：返回一个占位标识符。
                Expression::Identifier(
                    self.alloc(Ident::new(span, self.interner.intern("__error__"))),
                )
            }
        }
    }

    fn parse_import_expression(&mut self, lo: u32) -> Expression<'a> {
        self.bump(); // import
        // import.meta
        if self.at(TokenKind::Dot) {
            self.bump();
            let prop = self.parse_ident_name();
            return Expression::MetaProperty(self.alloc(MetaProperty {
                span: self.span_to(lo),
                meta: self.interner.intern("import"),
                property: prop.name,
            }));
        }
        // 动态 import(specifier)
        self.expect(TokenKind::LParen);
        let source = self.with_allow_in(true, |p| p.parse_assignment_expression());
        let options = if self.eat(TokenKind::Comma) && !self.at(TokenKind::RParen) {
            Some(self.parse_assignment_expression())
        } else {
            None
        };
        self.expect(TokenKind::RParen);
        // 依赖提取：字符串字面量的动态 import。
        if let Expression::StringLiteral(s) = source {
            self.record_dependency(Dependency {
                specifier: s.value,
                kind: DependencyKind::DynamicImport,
                span: self.span_to(lo),
            });
        }
        Expression::Import(self.alloc(ImportExpression {
            span: self.span_to(lo),
            source,
            options,
        }))
    }

    fn parse_array_expression(&mut self) -> Expression<'a> {
        let lo = self.start();
        // Raw assignment/for targets such as `[{a, ...rest}] = rhs` and
        // `for ([a, ...rest] of values)` have no surrounding cover parens. Scan only for this
        // array's matching `]` before parsing, so nested rest is not prematurely lowered as
        // expression spread.
        let previous_cover = self.in_cover_paren;
        if !previous_cover
            && self.delimited_expression_needs_cover(TokenKind::LBracket, TokenKind::RBracket)
        {
            self.in_cover_paren = true;
        }
        self.bump(); // [
        let mut elements = self.new_vec::<Option<Expression>>();
        while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::Comma) {
                self.bump(); // 空位 elision
                elements.push(None);
                continue;
            }
            if self.at(TokenKind::DotDotDot) {
                let slo = self.start();
                self.bump();
                let argument = self.parse_assignment_expression();
                elements.push(Some(Expression::Spread(self.alloc(SpreadElement {
                    span: self.span_to(slo),
                    argument,
                }))));
            } else {
                elements.push(Some(
                    self.with_allow_in(true, |p| p.parse_assignment_expression()),
                ));
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBracket);
        self.in_cover_paren = previous_cover;
        let array = self.alloc(ArrayExpression {
            span: self.span_to(lo),
            elements,
        });
        // A spread-shaped element before `=` / a for-head `in` / `of` is rest, not array spread.
        let destructuring_cover = self.at(TokenKind::Eq)
            || (self.in_for_head_init
                && (self.at_keyword(Keyword::In) || self.at_keyword(Keyword::Of)));
        if self.lowers(wake_ecma_transform::EcmaFeature::Spread)
            && !self.in_cover_paren
            && !destructuring_cover
            && array
                .elements
                .iter()
                .any(|element| matches!(element, Some(Expression::Spread(_))))
        {
            let helper = self.spread_helper_atom();
            wake_ecma_transform::lower_array_spread(
                self.arena,
                self.interner,
                helper,
                self.options.transform_features,
                array,
            )
        } else {
            Expression::Array(array)
        }
    }

    fn delimited_expression_needs_cover(&mut self, open: TokenKind, close: TokenKind) -> bool {
        let checkpoint = self.checkpoint();
        let mut depth = 0usize;
        loop {
            if self.cur.kind == open {
                depth += 1;
            } else if self.cur.kind == close {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                self.bump();
                if depth == 0 {
                    break;
                }
                continue;
            } else if self.at(TokenKind::Eof) {
                break;
            }
            self.bump();
        }
        let followed = depth == 0
            && (self.at(TokenKind::Eq)
                || (self.in_for_head_init
                    && (self.at_keyword(Keyword::In) || self.at_keyword(Keyword::Of))));
        self.rewind(checkpoint);
        followed
    }

    fn parse_object_expression(&mut self) -> Expression<'a> {
        let lo = self.start();
        let previous_cover = self.in_cover_paren;
        if !previous_cover
            && self.delimited_expression_needs_cover(TokenKind::LBrace, TokenKind::RBrace)
        {
            self.in_cover_paren = true;
        }
        self.bump(); // {
        let mut properties = self.new_vec::<ObjectMember>();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::DotDotDot) {
                let slo = self.start();
                self.bump();
                let argument = self.parse_assignment_expression();
                properties.push(ObjectMember::Spread(self.alloc(SpreadElement {
                    span: self.span_to(slo),
                    argument,
                })));
            } else {
                let prop = self.parse_object_property();
                properties.push(ObjectMember::Property(prop));
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace);
        self.in_cover_paren = previous_cover;
        let object = self.alloc(ObjectExpression {
            span: self.span_to(lo),
            properties,
        });
        let destructuring_cover = self.at(TokenKind::Eq)
            || (self.in_for_head_init
                && (self.at_keyword(Keyword::In) || self.at_keyword(Keyword::Of)));
        if self.lowers(wake_ecma_transform::EcmaFeature::ObjectRestSpread)
            && !self.in_cover_paren
            && !destructuring_cover
            && object
                .properties
                .iter()
                .any(|member| matches!(member, ObjectMember::Spread(_)))
        {
            let helper = self.object_spread_helper_atom();
            wake_ecma_transform::lower_object_spread(
                self.arena,
                self.interner,
                helper,
                self.options.transform_features,
                object,
            )
        } else {
            Expression::Object(object)
        }
    }

    fn parse_object_property(&mut self) -> &'a ObjectProperty<'a> {
        let lo = self.start();

        // getter / setter / async / generator 方法。
        let mut kind = PropertyKind::Init;
        let mut is_async = false;
        let mut is_generator = false;

        if self.at_keyword(Keyword::Get) && !self.peek_is_key_terminator() {
            self.bump();
            kind = PropertyKind::Get;
        } else if self.at_keyword(Keyword::Set) && !self.peek_is_key_terminator() {
            self.bump();
            kind = PropertyKind::Set;
        } else {
            if self.at_keyword(Keyword::Async)
                && !self.peek_is_key_terminator()
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

        // 方法（含 get/set/async/generator）。
        if self.at(TokenKind::LParen) || kind != PropertyKind::Init || is_async || is_generator {
            let func = self.parse_method_function(lo, is_async, is_generator);
            return self.alloc(ObjectProperty {
                span: self.span_to(lo),
                key,
                value: Expression::Function(func),
                kind,
                method: kind == PropertyKind::Init
                    && (!self.lowers(wake_ecma_transform::EcmaFeature::ShorthandProperties)
                        || wake_ecma_transform::object_method_uses_lexical_super(func)),
                shorthand: false,
                computed,
                prototype_setter: false,
            });
        }

        // `key: value`
        if self.eat(TokenKind::Colon) {
            let value = self.with_allow_in(true, |p| p.parse_assignment_expression());
            let prototype_setter = !computed
                && match key {
                    PropertyKey::Ident(ident) => self
                        .interner
                        .with_resolved(ident.name, |name| name == "__proto__"),
                    PropertyKey::String(string) => self
                        .interner
                        .with_resolved(string.value, |name| name == "__proto__"),
                    _ => false,
                };
            return self.alloc(ObjectProperty {
                span: self.span_to(lo),
                key,
                value,
                kind: PropertyKind::Init,
                method: false,
                shorthand: false,
                computed,
                prototype_setter,
            });
        }

        // 简写 `{ x }` 或 `{ x = default }`（后者仅解构合法，这里宽松接受）。
        let ident = match key {
            PropertyKey::Ident(id) => Expression::Identifier(self.alloc(id)),
            _ => {
                self.error(self.span_to(lo), "对象属性缺少值");
                Expression::NullLiteral(self.span_to(lo))
            }
        };
        let value = if self.eat(TokenKind::Eq) {
            let right = self.parse_assignment_expression();
            Expression::Assignment(self.alloc(AssignmentExpression {
                span: self.span_to(lo),
                operator: AssignmentOperator::Assign,
                left: ident,
                right,
            }))
        } else {
            ident
        };
        self.alloc(ObjectProperty {
            span: self.span_to(lo),
            key,
            value,
            kind: PropertyKind::Init,
            method: false,
            shorthand: !self.lowers(wake_ecma_transform::EcmaFeature::ShorthandProperties),
            computed,
            prototype_setter: false,
        })
    }

    /// 属性键：标识符名 / 字符串 / 数字 / `[computed]` / `#private`。
    pub(crate) fn parse_property_key(&mut self) -> (PropertyKey<'a>, bool) {
        let span = self.cur.span;
        match self.cur.kind {
            TokenKind::LBracket => {
                self.bump();
                let e = self.with_allow_in(true, |p| p.parse_assignment_expression());
                self.expect(TokenKind::RBracket);
                (PropertyKey::Computed(e), true)
            }
            TokenKind::Str => {
                let value = self.intern_string(span);
                self.bump();
                (
                    PropertyKey::String(self.alloc(StringLiteral { span, value })),
                    false,
                )
            }
            TokenKind::Number => {
                let value = self.lexer_number(span);
                self.bump();
                (
                    PropertyKey::Number(self.alloc(NumberLiteral { span, value })),
                    false,
                )
            }
            TokenKind::PrivateIdent => {
                self.bump();
                let name = self.intern_ident(Span::new(span.lo + 1, span.hi));
                (PropertyKey::Private(Ident::new(span, name)), false)
            }
            _ => {
                let id = self.parse_ident_name();
                (PropertyKey::Ident(id), false)
            }
        }
    }

    // ==================================================================
    // 模板
    // ==================================================================

    fn parse_template_literal(&mut self) -> Expression<'a> {
        let lo = self.start();
        let mut quasis = self.new_vec::<TemplateElement>();
        let mut expressions = self.new_vec::<Expression>();

        if self.at(TokenKind::TemplateNoSub) {
            let span = self.cur.span;
            quasis.push(self.template_element(span, TemplatePart::NoSub, true));
            self.bump();
        } else {
            // Head
            let span = self.cur.span;
            quasis.push(self.template_element(span, TemplatePart::Head, false));
            self.bump();
            loop {
                expressions.push(self.with_allow_in(true, |p| p.parse_expression()));
                match self.cur.kind {
                    TokenKind::TemplateMiddle => {
                        let s = self.cur.span;
                        quasis.push(self.template_element(s, TemplatePart::Middle, false));
                        self.bump();
                    }
                    TokenKind::TemplateTail => {
                        let s = self.cur.span;
                        quasis.push(self.template_element(s, TemplatePart::Tail, true));
                        self.bump();
                        break;
                    }
                    _ => {
                        self.error(self.cur.span, "未闭合的模板字符串");
                        break;
                    }
                }
            }
        }

        Expression::TemplateLiteral(self.alloc(TemplateLiteral {
            span: self.span_to(lo),
            quasis,
            expressions,
        }))
    }

    fn template_element(&self, span: Span, part: TemplatePart, tail: bool) -> TemplateElement {
        let raw_slice = self.slice(span);
        // 去掉定界符得到 raw 文本。
        let inner = match part {
            TemplatePart::NoSub => &raw_slice[1..raw_slice.len().saturating_sub(1)], // `...`
            TemplatePart::Head => &raw_slice[1..raw_slice.len().saturating_sub(2)],  // `...${
            TemplatePart::Middle => &raw_slice[1..raw_slice.len().saturating_sub(2)], // }...${
            TemplatePart::Tail => &raw_slice[1..raw_slice.len().saturating_sub(1)],  // }...`
        };
        let raw = self.interner.intern(inner);
        // cooked 的完整转义解码是后续精修项；此处以 raw 近似。
        TemplateElement {
            span,
            cooked: Some(raw),
            raw,
            tail,
        }
    }

    // ==================================================================
    // cover grammar / 箭头
    // ==================================================================

    fn parse_cover_paren(&mut self) -> CoverParen<'a> {
        self.expect(TokenKind::LParen);
        let previous_cover = self.in_cover_paren;
        self.in_cover_paren = true;
        // Until `=>` is seen these expressions may become parameter initializers. Keep their
        // temporary registrations transactional by using a disabled child scope; a failed cover
        // remains conservatively unlowered instead of leaking a binding into the parent scope.
        self.push_transform_temp_scope(false);
        let mut items = self.new_vec::<Expression>();
        let mut rest: Option<&'a RestElement<'a>> = None;
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::DotDotDot) {
                let rlo = self.start();
                self.bump();
                let argument = self.parse_binding_pattern();
                rest = Some(self.alloc(RestElement {
                    span: self.span_to(rlo),
                    argument,
                }));
                // TS：rest 参数类型注解 `...args: T[]` 擦除（若为箭头参数）。
                self.ts_type_annotation();
                break;
            }
            let item_lo = self.start();
            // 可选标识符参数必须在条件表达式之前识别，否则 `x?: T` 会被当作 `x ? ... : ...`。
            let optional_identifier = self.ts_optional_arrow_parameter_ahead();
            let mut item = if optional_identifier {
                let ident = self.parse_binding_ident();
                Expression::Identifier(self.alloc(ident))
            } else {
                self.with_allow_in(true, |p| p.parse_assignment_expression())
            };
            if optional_identifier
                || (self.ts && self.at(TokenKind::Question) && self.peek().kind == TokenKind::Colon)
            {
                self.bump(); // ?
            }
            // TS：箭头参数类型注解 `(x: T)` 擦除。cover grammar 把参数当表达式解析，
            // 表达式在 `:` 处自然停下，此处消费 `: T`（非空断言 `x!` 已在 tail 处理）。
            self.ts_type_annotation();
            // 类型注解位于绑定与默认值之间：`(x: T = initial) => ...`。表达式 parser
            // 无法跨过已擦除的类型来构造 Assignment，因此在 cover 中补建该节点。
            if self.ts && self.eat(TokenKind::Eq) {
                let previous_cover = self.in_cover_paren;
                self.in_cover_paren = false;
                let right = self.with_allow_in(true, |p| p.parse_assignment_expression());
                self.in_cover_paren = previous_cover;
                item = Expression::Assignment(self.alloc(AssignmentExpression {
                    span: self.span_to(item_lo),
                    operator: AssignmentOperator::Assign,
                    left: item,
                    right,
                }));
            }
            items.push(item);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen);
        let _ = self.pop_transform_temp_scope();
        self.in_cover_paren = previous_cover;
        CoverParen { items, rest }
    }

    fn ts_optional_arrow_parameter_ahead(&mut self) -> bool {
        if !self.ts || !self.at_ident_name() || self.peek().kind != TokenKind::Question {
            return false;
        }
        let checkpoint = self.checkpoint();
        self.bump(); // identifier
        self.bump(); // ?
        let is_optional_parameter = matches!(
            self.cur.kind,
            TokenKind::Colon | TokenKind::Comma | TokenKind::RParen | TokenKind::Eq
        );
        self.rewind(checkpoint);
        is_optional_parameter
    }

    /// TS：箭头返回类型注解 `(...): R =>` 擦除。仅当 `)` 后为 `: 类型`（或类型谓词）且紧跟 `=>`
    /// 时才消费；用 checkpoint 试探，避免误吃 `cond ? (x) : y` 里条件表达式的 `:`。
    fn skip_arrow_return_type_if_arrow(&mut self) {
        if !self.ts || !self.at(TokenKind::Colon) {
            return;
        }
        let cp = self.checkpoint();
        self.bump(); // :
        self.ts_type_or_predicate();
        // 只有确实是箭头（`=>` 紧随）才提交；否则回退，`:` 交回给条件表达式等。
        if !self.at(TokenKind::Arrow) || self.newline_before() {
            self.rewind(cp);
        }
    }

    fn cover_to_expression(&mut self, lo: u32, cover: CoverParen<'a>) -> Expression<'a> {
        if cover.rest.is_some() {
            self.error(self.span_to(lo), "`...` 只能出现在箭头函数参数中");
        }
        let n = cover.items.len();
        if n == 0 {
            self.error(self.span_to(lo), "空的括号表达式");
            return Expression::NullLiteral(self.span_to(lo));
        }
        if n == 1 {
            let expression = self.lower_deferred_cover_spread(cover.items[0]);
            if has_optional_chain(expression) {
                self.preserve_optional_chain_tail = true;
            }
            return expression;
        }
        let mut expressions = self.new_vec::<Expression>();
        expressions.extend(
            cover
                .items
                .iter()
                .copied()
                .map(|expression| self.lower_deferred_cover_spread(expression)),
        );
        Expression::Sequence(self.alloc(SequenceExpression {
            span: self.span_to(lo),
            expressions,
        }))
    }

    fn lower_deferred_cover_spread(&mut self, expression: Expression<'a>) -> Expression<'a> {
        if !LOWER {
            return expression;
        }
        // Parenthesized destructuring (`([a, ...rest] = rhs)` / `({a, ...rest} = rhs)`) passes
        // through cover grammar. At this point `=` is still unconsumed, so do not reinterpret rest
        // as expression spread before the assignment pass sees the target.
        if (self.at(TokenKind::Eq)
            || (self.in_for_head_init
                && (self.at_keyword(Keyword::In) || self.at_keyword(Keyword::Of))))
            && wake_ecma_transform::is_destructuring_assignment_target(expression)
        {
            return expression;
        }
        match expression {
            Expression::Assignment(assignment)
                if assignment.operator == AssignmentOperator::Assign
                    && wake_ecma_transform::destructuring_assignment_needs_lowering(
                        self.options.transform_features,
                        assignment.left,
                    ) =>
            {
                self.lower_destructuring_assignment_expression(
                    assignment.span,
                    assignment.left,
                    assignment.right,
                )
            }
            Expression::Array(array)
                if self.lowers(wake_ecma_transform::EcmaFeature::Spread)
                    && array
                        .elements
                        .iter()
                        .any(|element| matches!(element, Some(Expression::Spread(_)))) =>
            {
                let helper = self.spread_helper_atom();
                wake_ecma_transform::lower_array_spread(
                    self.arena,
                    self.interner,
                    helper,
                    self.options.transform_features,
                    array,
                )
            }
            Expression::Object(object)
                if self.lowers(wake_ecma_transform::EcmaFeature::ObjectRestSpread)
                    && object
                        .properties
                        .iter()
                        .any(|member| matches!(member, ObjectMember::Spread(_))) =>
            {
                let helper = self.object_spread_helper_atom();
                wake_ecma_transform::lower_object_spread(
                    self.arena,
                    self.interner,
                    helper,
                    self.options.transform_features,
                    object,
                )
            }
            _ => expression,
        }
    }

    pub(crate) fn lower_destructuring_assignment_expression(
        &mut self,
        span: Span,
        left: Expression<'a>,
        right: Expression<'a>,
    ) -> Expression<'a> {
        if !wake_ecma_transform::destructuring_assignment_needs_lowering(
            self.options.transform_features,
            left,
        ) {
            return Expression::Assignment(self.alloc(AssignmentExpression {
                span,
                operator: AssignmentOperator::Assign,
                left,
                right,
            }));
        }

        let temporary_count = wake_ecma_transform::destructuring_assignment_temporary_count(left);
        let Some(temporaries) = (0..temporary_count)
            .map(|_| self.fresh_scoped_transform_atom())
            .collect::<Option<Vec<_>>>()
        else {
            return Expression::Assignment(self.alloc(AssignmentExpression {
                span,
                operator: AssignmentOperator::Assign,
                left,
                right,
            }));
        };

        // Helpers are requested only after the entire scope-owned temporary set is available. This
        // keeps parameter/cover/class/async-arrow conservative paths free of unused runtime code.
        let iterator_helper = self.spread_helper_atom();
        let object_helper = if wake_ecma_transform::assignment_target_has_object_rest(left) {
            self.object_spread_helper_atom()
        } else {
            iterator_helper
        };
        wake_ecma_transform::lower_destructuring_assignment(
            self.arena,
            self.interner,
            iterator_helper,
            object_helper,
            self.options.transform_features,
            span,
            left,
            right,
            &temporaries,
        )
    }

    fn finish_arrow(&mut self, lo: u32, cover: CoverParen<'a>, is_async: bool) -> Expression<'a> {
        self.expect(TokenKind::Arrow);
        let mut params = self.new_vec::<Pattern>();
        for e in cover.items.iter() {
            params.push(self.expr_to_pattern(*e));
        }
        if let Some(rest) = cover.rest {
            params.push(Pattern::Rest(rest));
        }
        self.finish_arrow_body(lo, params, is_async)
    }

    fn finish_single_arrow(&mut self, lo: u32, id: Ident, is_async: bool) -> Expression<'a> {
        self.expect(TokenKind::Arrow);
        let mut params = self.new_vec::<Pattern>();
        params.push(Pattern::Ident(self.alloc(id)));
        self.finish_arrow_body(lo, params, is_async)
    }

    fn finish_arrow_body(
        &mut self,
        lo: u32,
        params: AVec<'a, Pattern<'a>>,
        is_async: bool,
    ) -> Expression<'a> {
        let saved_async = self.ctx.in_async;
        let saved_gen = self.ctx.in_generator;
        let saved_top = self.ctx.top_level;
        self.ctx.in_async = is_async;
        self.ctx.in_generator = false;
        self.ctx.top_level = false;

        let body = if self.at(TokenKind::LBrace) {
            ArrowBody::Block(self.parse_function_body_with_transform_temps(!is_async))
        } else {
            // A concise arrow is an independent execution scope even when it occurs while an
            // enclosing parenthesized cover is still being resolved. Clear only the inherited
            // cover flag while parsing its body; the arrow's own parameter cover has already
            // finished and remains conservative through its disabled transactional scope.
            let previous_cover = self.in_cover_paren;
            self.in_cover_paren = false;
            // Async-arrow lowering is not available yet. Give it a disabled child scope so a
            // temporary can neither be injected into nor leak out of the arrow.
            self.push_transform_temp_scope(!is_async);
            let expression = self.parse_assignment_expression();
            let transform_temps = self.pop_transform_temp_scope();
            self.in_cover_paren = previous_cover;
            if let Some(declaration) = self.transform_temp_declaration(&transform_temps) {
                let span = self.span_to(lo);
                let mut statements = self.new_vec::<Statement>();
                statements.push(declaration);
                statements.push(Statement::Return(self.alloc(ReturnStatement {
                    span,
                    argument: Some(expression),
                })));
                ArrowBody::Block(self.alloc(FunctionBody {
                    span,
                    statements,
                    strict: false,
                }))
            } else {
                ArrowBody::Expression(expression)
            }
        };

        self.ctx.in_async = saved_async;
        self.ctx.in_generator = saved_gen;
        self.ctx.top_level = saved_top;

        let arrow = self.alloc(ArrowFunction {
            span: self.span_to(lo),
            params,
            body,
            is_async,
        });
        let features = self.options.transform_features;
        if !LOWER {
            return Expression::Arrow(arrow);
        }
        let needs_binding_lowering = arrow
            .params
            .iter()
            .copied()
            .any(|param| wake_ecma_transform::binding_pattern_needs_lowering(features, param));
        let can_lower_async_binding_parameters = !arrow.is_async
            || (!features.contains(wake_ecma_transform::EcmaFeature::ArrowFunction)
                && !features.contains(wake_ecma_transform::EcmaFeature::AsyncAwait)
                && !features.contains(wake_ecma_transform::EcmaFeature::FunctionParameters));
        let needs_parameter_lowering = (features
            .contains(wake_ecma_transform::EcmaFeature::FunctionParameters)
            || needs_binding_lowering)
            && can_lower_async_binding_parameters;
        if needs_parameter_lowering
            && matches!(
                arrow.body,
                ArrowBody::Block(body)
                    if wake_ecma_transform::parameter_lowering_has_body_binding_conflict(
                        features,
                        &arrow.params,
                        body,
                    )
            )
        {
            // Lowering the arrow syntax as well would still move its parameter expressions into
            // the body. Keep the complete arrow before allocating parameter helpers/temps.
            return Expression::Arrow(arrow);
        }
        let temporary_count = if needs_parameter_lowering {
            wake_ecma_transform::complex_parameter_temporary_count_for_features(
                features,
                &arrow.params,
            )
        } else {
            0
        };
        let iterator_helper = if needs_parameter_lowering
            && wake_ecma_transform::complex_parameters_need_iterator_helper(features, &arrow.params)
        {
            self.spread_helper_atom()
        } else {
            self.interner.intern("__wake_unused_iterator_helper")
        };
        let object_helper = if needs_parameter_lowering
            && arrow
                .params
                .iter()
                .copied()
                .any(wake_ecma_transform::pattern_has_object_rest)
        {
            self.object_spread_helper_atom()
        } else {
            iterator_helper
        };
        let parameter_temporaries = (0..temporary_count)
            .map(|_| self.fresh_transform_atom())
            .collect::<Vec<_>>();
        wake_ecma_transform::lower_arrow(
            self.arena,
            self.interner,
            iterator_helper,
            object_helper,
            &parameter_temporaries,
            features,
            arrow,
        )
    }

    /// 把一个表达式重解释为绑定模式（cover grammar 转换）。
    fn expr_to_pattern(&mut self, expr: Expression<'a>) -> Pattern<'a> {
        match expr {
            Expression::Identifier(id) => Pattern::Ident(id),
            Expression::Assignment(a) if a.operator == AssignmentOperator::Assign => {
                let left = self.expr_to_pattern(a.left);
                Pattern::Assignment(self.alloc(AssignmentPattern {
                    span: a.span,
                    left,
                    right: a.right,
                }))
            }
            Expression::Array(arr) => {
                let mut elements = self.new_vec::<Option<Pattern>>();
                for el in arr.elements.iter() {
                    match el {
                        None => elements.push(None),
                        Some(Expression::Spread(s)) => {
                            let argument = self.expr_to_pattern(s.argument);
                            elements.push(Some(Pattern::Rest(self.alloc(RestElement {
                                span: s.span,
                                argument,
                            }))));
                        }
                        Some(e) => elements.push(Some(self.expr_to_pattern(*e))),
                    }
                }
                Pattern::Array(self.alloc(ArrayPattern {
                    span: arr.span,
                    elements,
                }))
            }
            Expression::Object(obj) => {
                let mut properties = self.new_vec::<ObjectPatternProperty>();
                let mut rest = None;
                for m in obj.properties.iter() {
                    match m {
                        ObjectMember::Property(p) => {
                            properties.push(ObjectPatternProperty {
                                span: p.span,
                                key: p.key,
                                value: self.expr_to_pattern(p.value),
                                shorthand: p.shorthand,
                                computed: p.computed,
                            });
                        }
                        ObjectMember::Spread(s) => {
                            let argument = self.expr_to_pattern(s.argument);
                            rest = Some(self.alloc(RestElement {
                                span: s.span,
                                argument,
                            }));
                        }
                    }
                }
                Pattern::Object(self.alloc(ObjectPattern {
                    span: obj.span,
                    properties,
                    rest,
                }))
            }
            other => {
                // 简单目标（成员表达式等）无法直接作绑定模式；报错并退化为占位。
                self.error(other.span(), "非法的箭头参数 / 绑定目标");
                Pattern::Ident(
                    self.alloc(Ident::new(other.span(), self.interner.intern("__error__"))),
                )
            }
        }
    }

    // ==================================================================
    // 值解码辅助
    // ==================================================================

    fn lexer_number(&self, span: Span) -> f64 {
        self.lexer.number_value(span)
    }

    fn intern_string(&self, span: Span) -> wake_common::Atom {
        self.interner.intern(&self.lexer.string_value(span))
    }

    fn split_regex(&self, span: Span) -> (wake_common::Atom, wake_common::Atom) {
        let s = self.slice(span);
        // s = /pattern/flags
        let last_slash = s.rfind('/').unwrap_or(0);
        let pattern = &s[1..last_slash.max(1)];
        let flags = if last_slash < s.len() {
            &s[last_slash + 1..]
        } else {
            ""
        };
        (self.interner.intern(pattern), self.interner.intern(flags))
    }

    /// 当前 token 是否是「表达式结束」标记（用于 yield/return 无参数判定）。
    fn at_expression_end(&self) -> bool {
        matches!(
            self.cur.kind,
            TokenKind::Semicolon
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::Comma
                | TokenKind::Colon
                | TokenKind::Eof
        )
    }

    /// 前瞻：get/set/async 后是否直接跟键结束标记（即它们本身是键，而非修饰符）。
    fn peek_is_key_terminator(&mut self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Colon
                | TokenKind::Comma
                | TokenKind::RBrace
                | TokenKind::LParen
                | TokenKind::Eq
        )
    }
}

enum TemplatePart {
    NoSub,
    Head,
    Middle,
    Tail,
}

#[derive(Clone, Copy)]
enum BinOp {
    Binary(BinaryOperator),
    Logical(LogicalOperator),
}

fn unary_op(kind: TokenKind) -> Option<UnaryOperator> {
    Some(match kind {
        TokenKind::Plus => UnaryOperator::Plus,
        TokenKind::Minus => UnaryOperator::Minus,
        TokenKind::Bang => UnaryOperator::LogicalNot,
        TokenKind::Tilde => UnaryOperator::BitwiseNot,
        TokenKind::Keyword(Keyword::Typeof) => UnaryOperator::Typeof,
        TokenKind::Keyword(Keyword::Void) => UnaryOperator::Void,
        TokenKind::Keyword(Keyword::Delete) => UnaryOperator::Delete,
        _ => return None,
    })
}

fn assignment_op(kind: TokenKind) -> Option<AssignmentOperator> {
    use AssignmentOperator as A;
    Some(match kind {
        TokenKind::Eq => A::Assign,
        TokenKind::PlusEq => A::Add,
        TokenKind::MinusEq => A::Sub,
        TokenKind::StarEq => A::Mul,
        TokenKind::SlashEq => A::Div,
        TokenKind::PercentEq => A::Rem,
        TokenKind::StarStarEq => A::Exp,
        TokenKind::ShlEq => A::Shl,
        TokenKind::ShrEq => A::Shr,
        TokenKind::UshrEq => A::Ushr,
        TokenKind::AmpEq => A::BitAnd,
        TokenKind::PipeEq => A::BitOr,
        TokenKind::CaretEq => A::BitXor,
        TokenKind::AmpAmpEq => A::And,
        TokenKind::PipePipeEq => A::Or,
        TokenKind::QuestionQuestionEq => A::Coalesce,
        _ => return None,
    })
}

fn is_ident_name_kind(kind: TokenKind) -> bool {
    match kind {
        TokenKind::Ident => true,
        TokenKind::Keyword(kw) => !kw.is_reserved(),
        _ => false,
    }
}
