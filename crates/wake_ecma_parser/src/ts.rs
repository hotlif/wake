//! 完整 TypeScript 类型语法**消费器**（擦除）——参照 typescript-go / tsc 的 parser 结构。
//!
//! 与旧的「bracket-depth 启发式」不同，这里按真实类型文法**结构化消费**类型语法（不建类型 AST，
//! 仅推进 token），从而在所有位置都能精确定位类型的起止、正确处理 `>>` 拆分、函数类型 `=>`、
//! 条件类型 `A extends B ? C : D`、`keyof`/`typeof`/`infer`、映射/对象/元组/模板字面量类型等。
//!
//! 类型擦除与事实收集共用结构化类型文法，嵌套模板的早期错误不能被平衡 token 跳过；
//! [`Parser::ts_skip_balanced`] 仅用于推测解析、错误恢复及不支持的声明属性恢复路径。
//! `<...>`（类型参数/实参）用 [`Parser::consume_type_gt`] 精确处理 `>`/`>>`/`>>>` 收尾；
//! 「扁平」文法（联合/交叉/条件/引用/前缀算子）按产生式递归消费。

use wake_common::Span;
use wake_ecma_ast::Expression;
use wake_ecma_lexer::{Keyword, TokenKind};

use crate::{DeclarationRequestRole, Parser, SourceNodeKind};

/// 类型参数列表在 TSX 表达式起始位置的消歧信息。
///
/// 单个无约束参数 `<T>` 与 JSX 起始标签有歧义；逗号、约束或默认类型则能明确表明
/// 这是 TypeScript 类型参数列表。`closed` 防止试探把未闭合的 JSX 标签头提交为泛型箭头。
#[derive(Clone, Copy, Default)]
pub(crate) struct TsTypeParametersInfo {
    pub(crate) closed: bool,
    pub(crate) jsx_unambiguous: bool,
    pub(crate) comma_disambiguates: bool,
    pub(crate) source_list: Option<usize>,
}

#[derive(Clone, Copy)]
pub(crate) enum TsTypeParameterContext {
    TypeDeclaration,
    FunctionLike,
    Class,
}

impl TsTypeParameterContext {
    fn allows_const(self) -> bool {
        matches!(self, Self::FunctionLike | Self::Class)
    }
}

#[derive(Clone, Copy)]
enum TsEntityReferenceNamespace {
    Type,
    Value,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TsDeclarationBracketMemberKind {
    Index,
    Mapped,
    Computed,
}

impl<'a, 'src, const LOWER: bool> Parser<'a, 'src, LOWER> {
    // ==================================================================
    // 对外集成入口
    // ==================================================================

    /// `: Type`（含类型谓词 `x is T` / `asserts x is T`）。仅 ts 且当前为 `:` 时消费。
    pub(crate) fn ts_type_annotation(&mut self) -> Option<Span> {
        if !self.ts || !self.at(TokenKind::Colon) {
            return None;
        }
        self.source_begin(SourceNodeKind::TsTypeAnnotation, self.cur.span.lo);
        self.bump(); // :
        let lo = self.start();
        self.ts_type_or_predicate();
        let span = Span::new(lo, self.prev_end.max(lo));
        self.declaration_record_type_annotation(span);
        self.source_end(self.prev_end);
        Some(span)
    }

    /// 类型或类型谓词（返回位置）。
    pub(crate) fn ts_type_or_predicate(&mut self) {
        // asserts x / asserts x is T
        if self.at_contextual("asserts")
            && matches!(
                self.peek().kind,
                TokenKind::Ident | TokenKind::Keyword(Keyword::This)
            )
        {
            self.bump(); // asserts
            self.bump(); // x / this
            if self.at_contextual("is") {
                self.bump();
                self.ts_type();
            }
            return;
        }
        // x is T
        if (self.at_ident_name() || self.at_keyword(Keyword::This)) && self.peek_contextual("is") {
            self.bump(); // x / this
            self.bump(); // is
            self.ts_type();
            return;
        }
        self.ts_type();
    }

    /// 类型参数声明 `<T extends U = D, ...>`（仅 ts 且当前 `<`）。
    pub(crate) fn ts_type_parameters(
        &mut self,
        context: TsTypeParameterContext,
    ) -> TsTypeParametersInfo {
        let mut info = TsTypeParametersInfo::default();
        if !self.ts || !self.at(TokenKind::Lt) {
            return info;
        }
        let source_list = self.source_list_start(wake_ecma_ast::SourceListKind::TypeParameters);
        let mut has_constraint_or_default = false;
        self.source_begin(SourceNodeKind::TsTypeParameters, self.cur.span.lo);
        let source_scope = self.source_type_scope_begin(
            wake_ecma_ast::SourceTypeScopeKind::TypeParameters,
            self.cur.span.lo,
        );
        let strict = self.declaration_requires_strict_type_syntax();
        let reference_mark = self.declaration_type_reference_mark();
        let mut bindings = Vec::new();
        let mut saw_parameter = false;
        let mut reported_missing_parameter = false;
        self.bump(); // <
        while !self.at_type_gt() && !self.at(TokenKind::Eof) {
            // 方差/const 修饰符（in / out / const）。
            while self.at_contextual("in")
                || self.at_contextual("out")
                || self.at_keyword(Keyword::In)
            {
                self.bump();
            }
            if self.at_keyword(Keyword::Const) {
                if strict && !context.allows_const() {
                    self.error(self.cur.span, "const 类型参数仅允许用于函数、方法或类");
                }
                self.bump();
            }
            if self.at_ident_name() {
                let binding = self.intern_ident(self.cur.span);
                self.source_type_parameter(source_scope, self.cur.span, binding);
                bindings.push(binding);
                self.bump(); // 参数名
                saw_parameter = true;
            } else if strict {
                self.error_expected("类型参数名");
                reported_missing_parameter = true;
            }
            if self.eat_keyword(Keyword::Extends) {
                has_constraint_or_default = true;
                info.jsx_unambiguous = true;
                self.ts_type();
            }
            if self.eat(TokenKind::Eq) {
                has_constraint_or_default = true;
                info.jsx_unambiguous = true;
                self.ts_type();
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
            info.jsx_unambiguous = true;
        }
        if strict && !saw_parameter && !reported_missing_parameter {
            self.error_expected("类型参数名");
        }
        info.closed = self.at_type_gt();
        info.comma_disambiguates = bindings.len() == 1 && !has_constraint_or_default;
        info.source_list = self.source_list_finish(source_list, true);
        self.consume_type_gt();
        self.declaration_activate_type_bindings_since(reference_mark, &bindings);
        self.source_end(self.prev_end);
        info
    }

    /// 类型实参 `<A, B>`（非试探，假设当前 `<`）。用于 `expr as Foo<T>`、`extends Base<T>` 等。
    pub(crate) fn ts_type_arguments(&mut self) {
        self.ts_type_arguments_inner(false);
    }

    fn ts_type_arguments_inner(&mut self, expression: bool) {
        if !self.at(TokenKind::Lt) {
            return;
        }
        self.source_begin(SourceNodeKind::TsTypeArguments, self.cur.span.lo);
        let strict = self.declaration_requires_strict_type_syntax();
        let mut saw_argument = false;
        let mut reported_missing_argument = false;
        self.bump(); // <
        while !self.at_type_gt() && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::Comma) {
                if strict {
                    self.error_expected("类型实参");
                    reported_missing_argument = true;
                }
                self.bump();
                continue;
            }
            self.ts_type();
            saw_argument = true;
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        if strict && !saw_argument && !reported_missing_argument {
            self.error_expected("类型实参");
        }
        if expression {
            if !self.at(TokenKind::Gt) {
                self.error_expected("类型实参闭合的 `>`");
            } else {
                // An instantiation is an expression value. Its following slash is division,
                // unlike a comparison's RHS; do not lex it with the generic `>` heuristic.
                self.source_token(self.cur, crate::SourceTokenContext::JavaScript);
                self.prev_end = self.cur.span.hi;
                self.cur = self.lexer.next_at(self.prev_end, false);
                self.lookahead = None;
            }
        } else {
            self.consume_type_gt();
        }
        self.source_end(self.prev_end);
    }

    /// Speculatively erase call, constructor, tag, or standalone instantiation type arguments.
    /// The outer closing token must be a complete `>`; splitting `>=` or `>>` here would steal a
    /// comparison/shift operator. Nested type lists may still split their own closing tokens.
    /// Keep the parse only with an unambiguous follower and no new diagnostics; otherwise rewind
    /// tokens, diagnostics, and declaration facts together, including an absent closing `>`.
    pub(crate) fn try_ts_type_arguments(&mut self) -> bool {
        self.try_ts_type_arguments_with_context(false)
    }

    /// `new C<T>` owns its type arguments even when the optional argument list is absent and
    /// followed by a comparison or unary-capable binary operator, e.g. `new C<T> + value`.
    pub(crate) fn try_ts_constructor_type_arguments(&mut self) -> bool {
        self.try_ts_type_arguments_with_context(true)
    }

    fn try_ts_type_arguments_with_context(&mut self, constructor: bool) -> bool {
        debug_assert!(self.at(TokenKind::Lt));
        let cp = self.checkpoint();
        self.ts_type_arguments_inner(true);
        if self.diagnostics.len() == cp.diag_len
            && (constructor || self.can_follow_type_args_in_expr())
        {
            if self.at(TokenKind::Dot)
                || (self.at(TokenKind::QuestionDot)
                    && !matches!(self.peek().kind, TokenKind::LParen | TokenKind::Lt))
            {
                self.error(self.cur.span, "类型实例化表达式必须加括号后才能访问属性");
            }
            true
        } else {
            self.rewind(cp);
            false
        }
    }

    /// A call/tag starts immediately; other followers may end an instantiation expression.
    /// Prefer a comparison when the following token can begin its RHS. In particular `+`/`-`
    /// remain unary RHS starters even across a newline, and `<`/`>` keep chained comparisons.
    fn can_follow_type_args_in_expr(&self) -> bool {
        match self.cur.kind {
            TokenKind::LParen | TokenKind::TemplateNoSub | TokenKind::TemplateHead => return true,
            TokenKind::Lt | TokenKind::Gt | TokenKind::Plus | TokenKind::Minus => return false,
            _ => {}
        }
        if self.newline_before() || self.at_keyword(Keyword::As) || self.at_contextual("satisfies")
        {
            return true;
        }
        let starts_expression = match self.cur.kind {
            TokenKind::Ident
            | TokenKind::PrivateIdent
            | TokenKind::Number
            | TokenKind::Str
            | TokenKind::BigInt
            | TokenKind::Regex
            | TokenKind::LBrace
            | TokenKind::LBracket
            | TokenKind::Bang
            | TokenKind::Tilde
            | TokenKind::PlusPlus
            | TokenKind::MinusMinus => true,
            TokenKind::Keyword(keyword) => {
                !keyword.is_reserved()
                    || matches!(
                        keyword,
                        Keyword::This
                            | Keyword::Super
                            | Keyword::New
                            | Keyword::Function
                            | Keyword::Class
                            | Keyword::Import
                            | Keyword::True
                            | Keyword::False
                            | Keyword::Null
                            | Keyword::Typeof
                            | Keyword::Void
                            | Keyword::Delete
                            | Keyword::Await
                            | Keyword::Yield
                    )
            }
            _ => false,
        };
        !starts_expression
    }

    // ==================================================================
    // 类型文法（消费）
    // ==================================================================

    /// 完整类型：函数/构造类型，或联合类型 + 可选条件类型。
    pub(crate) fn ts_type(&mut self) {
        self.source_begin(SourceNodeKind::TsType, self.cur.span.lo);
        self.declaration_begin_type();
        self.ts_type_inner();
        self.declaration_end_type();
        self.source_end(self.prev_end);
    }

    fn ts_type_inner(&mut self) {
        if self.ts_at_function_type_start() {
            self.ts_function_type();
            return;
        }
        self.ts_union();
        // 条件类型 `A extends B ? C : D`。
        if self.at_keyword(Keyword::Extends) && !self.newline_before() {
            self.declaration_begin_infer_scope();
            self.bump(); // extends
            // extends 类型：不再吃条件，避免贪婪。
            self.ts_union();
            let has_true = self.eat(TokenKind::Question);
            let infer_scope = self.declaration_activate_infer_scope();
            if has_true {
                self.ts_type();
                self.declaration_restore_type_scope(infer_scope);
                self.expect(TokenKind::Colon);
                self.ts_type();
            } else {
                self.declaration_restore_type_scope(infer_scope);
                if self.declaration_requires_strict_type_syntax() {
                    self.error_expected("条件类型的 `?`");
                }
            }
        }
    }

    fn ts_union(&mut self) {
        self.eat(TokenKind::Pipe); // 前导 `|`
        self.ts_intersection();
        while self.eat(TokenKind::Pipe) {
            self.ts_intersection();
        }
    }

    fn ts_intersection(&mut self) {
        self.eat(TokenKind::Amp); // 前导 `&`
        self.ts_type_operand();
        while self.eat(TokenKind::Amp) {
            self.ts_type_operand();
        }
    }

    /// 前缀算子（keyof/typeof/readonly/unique/infer）+ 后缀（数组/索引访问）。
    fn ts_type_operand(&mut self) {
        // typeof 类型查询：`typeof a.b.c` (+ 可选类型实参)。
        if self.at_keyword(Keyword::Typeof) {
            let lo = self.start();
            self.source_begin(SourceNodeKind::TsTypeOperator, lo);
            self.bump();
            if self.at_keyword(Keyword::Import) {
                self.ts_import_type();
            } else {
                self.ts_entity_name(TsEntityReferenceNamespace::Value);
            }
            if self.at(TokenKind::Lt) {
                self.ts_type_arguments();
            }
            // 类型查询同样可以继续索引：`typeof VALUE[keyof typeof VALUE]`。
            self.ts_postfix_suffixes(lo);
            self.source_end(self.prev_end);
            return;
        }
        // infer T (extends U)?
        if self.at_contextual("infer") {
            self.bump();
            let binding = if self.at_ident_name() {
                let span = self.cur.span;
                let binding = self.intern_ident(self.cur.span);
                self.source_infer_binding(span, binding);
                self.bump();
                Some((binding, span))
            } else {
                if self.declaration_requires_strict_type_syntax() {
                    self.error_expected("infer 类型参数名");
                }
                None
            };
            if self.at_keyword(Keyword::Extends) && !self.newline_before() {
                self.bump();
                let constraint_scope = self.declaration_type_scope_mark();
                if let Some((name, span)) = binding {
                    self.source_infer_constraint(span, name);
                }
                self.ts_type_operand();
                self.declaration_restore_type_scope(constraint_scope);
            }
            if let Some((binding, span)) = binding
                && !self.declaration_record_infer_binding(binding)
                && self.declaration_requires_strict_type_syntax()
            {
                self.error(span, "`infer` 仅允许出现在条件类型的 extends 模式中");
            }
            return;
        }
        // keyof / readonly / unique：算子 + 操作数。
        if self.at_contextual("keyof")
            || self.at_contextual("readonly")
            || self.at_contextual("unique")
        {
            self.source_begin(SourceNodeKind::TsTypeOperator, self.start());
            self.bump();
            self.ts_type_operand();
            self.source_end(self.prev_end);
            return;
        }
        self.ts_postfix();
    }

    fn ts_postfix(&mut self) {
        let lo = self.start();
        self.ts_primary();
        self.ts_postfix_suffixes(lo);
    }

    fn ts_postfix_suffixes(&mut self, lo: u32) {
        // 后缀数组/索引访问 `T[]` / `T[K]`（同行）。
        while !self.newline_before() && self.at(TokenKind::LBracket) {
            self.bump();
            let array = self.at(TokenKind::RBracket);
            if !array {
                self.ts_type();
            }
            self.expect(TokenKind::RBracket);
            self.source_leaf(
                if array {
                    SourceNodeKind::TsArrayType
                } else {
                    SourceNodeKind::TsIndexedAccessType
                },
                self.span_to(lo),
            );
        }
    }

    fn ts_primary(&mut self) {
        match self.cur.kind {
            TokenKind::LParen => {
                self.bump();
                self.ts_type();
                self.expect(TokenKind::RParen);
            }
            TokenKind::LBrace => {
                self.ts_declaration_type_literal();
            }
            TokenKind::LBracket => {
                self.ts_declaration_tuple_type();
            }
            // this / void / null / const 类型。
            // `const` 是 `as const` / `<const>` 断言的类型位置写法；它是保留字，
            // ts_entity_name 会拒收，故在此显式消费（否则整段解析在 `const` 处失步）。
            TokenKind::Keyword(Keyword::This)
            | TokenKind::Keyword(Keyword::Void)
            | TokenKind::Keyword(Keyword::Null)
            | TokenKind::Keyword(Keyword::True)
            | TokenKind::Keyword(Keyword::False)
            | TokenKind::Keyword(Keyword::Const) => {
                if self.at_keyword(Keyword::Const) {
                    self.declaration_record_const_assertion(self.cur.span);
                }
                self.bump();
            }
            // 字面量类型。
            TokenKind::Str | TokenKind::Number | TokenKind::BigInt => {
                self.bump();
            }
            TokenKind::Minus => {
                self.bump();
                if !matches!(self.cur.kind, TokenKind::Number | TokenKind::BigInt)
                    && self.declaration_requires_strict_type_syntax()
                {
                    self.error_expected("负数字面量类型");
                } else if matches!(self.cur.kind, TokenKind::Number | TokenKind::BigInt) {
                    self.bump();
                }
            }
            // 模板字面量类型 `` `a${T}b` ``。
            TokenKind::TemplateNoSub => {
                self.template_value(self.cur.span, true, false);
                self.bump();
            }
            TokenKind::TemplateHead => self.ts_template_literal_type(),
            // import 类型。
            TokenKind::Keyword(Keyword::Import) => {
                self.ts_import_type();
                if self.at(TokenKind::Lt) {
                    self.ts_type_arguments();
                }
            }
            // 类型引用：`A.B.C<Args>`（含 unique/keyof 之外的上下文关键字作名字）。
            TokenKind::Ident | TokenKind::Keyword(_) => {
                self.source_begin(SourceNodeKind::TsTypeReference, self.start());
                if self.at_contextual("any") {
                    self.declaration_record_any(self.cur.span);
                }
                self.ts_entity_name(TsEntityReferenceNamespace::Type);
                if self.at(TokenKind::Lt) {
                    self.ts_type_arguments();
                }
                self.source_end(self.prev_end);
            }
            _ => {
                // Recovery still consumes one token, but an absent/invalid type must not be
                // accepted as a declaration fact.
                self.error_expected("TypeScript 类型");
                self.bump();
            }
        }
    }

    /// `import("mod")` 类型（`.entity` 尾巴由调用方按需接类型实参）。
    fn ts_import_type(&mut self) {
        let lo = self.start();
        self.bump(); // import
        if !self.at(TokenKind::LParen) {
            self.error_expected("import 类型参数");
            return;
        }
        self.bump(); // (
        let mut source = None;
        if self.at(TokenKind::Str) {
            if self.source_syntax.is_some() {
                source = Some(wake_ecma_ast::SourceModuleSpecifier {
                    value: self.lexer.string_value(self.cur.span),
                    span: self.cur.span,
                });
            }
            self.declaration_record_request(
                self.cur.span,
                DeclarationRequestRole::ImportTypeExpression,
            );
            self.bump();
        } else if self.declaration_requires_strict_type_syntax() || self.source_syntax.is_some() {
            self.error_expected("import 类型的字符串模块名");
        }

        let mut attributes = Some(Vec::new());
        if self.declaration_requires_strict_type_syntax() || self.source_syntax.is_some() {
            if self.eat(TokenKind::Comma) {
                attributes = self.ts_declaration_import_type_options();
                self.eat(TokenKind::Comma);
            }
            self.expect(TokenKind::RParen);
        } else {
            self.ts_skip_balanced_tail(1);
        }
        self.source_type_import(Span::new(lo, self.prev_end), source, attributes);
        while self.at(TokenKind::Dot) {
            self.bump();
            if self.at_ident_name() {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn ts_declaration_import_type_options(
        &mut self,
    ) -> Option<Vec<wake_ecma_ast::SourceImportAttribute>> {
        if !self.at(TokenKind::LBrace) {
            self.error_expected("import 类型 attributes 对象");
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                self.bump();
            }
            return None;
        }
        self.bump();
        let mut saw_attributes = false;
        let mut known = true;
        let mut attributes = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let name;
            if matches!(
                self.cur.kind,
                TokenKind::Ident | TokenKind::Keyword(_) | TokenKind::Str
            ) {
                name = if self.at(TokenKind::Str) {
                    self.lexer.string_value(self.cur.span)
                } else {
                    self.lexer
                        .identifier_text(self.cur.span)
                        .into_owned()
                        .into()
                };
                self.bump();
            } else {
                self.error_expected("import 类型 attributes 属性名");
                self.ts_recover_declaration_import_attributes(TokenKind::RBrace);
                break;
            }
            self.expect(TokenKind::Colon);
            let entries = self.ts_declaration_import_attributes_bag();
            if name == "with" {
                if let Some(entries) = entries {
                    attributes = entries;
                } else {
                    known = false;
                }
            } else {
                known = false;
            }
            saw_attributes = true;
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        if !saw_attributes {
            self.error_expected("import 类型 attributes");
        }
        self.expect(TokenKind::RBrace);
        known.then_some(attributes)
    }

    fn ts_declaration_import_attributes_bag(
        &mut self,
    ) -> Option<Vec<wake_ecma_ast::SourceImportAttribute>> {
        if !self.at(TokenKind::LBrace) {
            self.error_expected("import 类型 attribute 映射");
            self.ts_recover_declaration_import_attributes(TokenKind::RBrace);
            return None;
        }
        self.bump();
        let mut attributes = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let lo = self.start();
            let name;
            if matches!(
                self.cur.kind,
                TokenKind::Ident | TokenKind::Keyword(_) | TokenKind::Str
            ) {
                name = if self.at(TokenKind::Str) {
                    self.lexer.string_value(self.cur.span)
                } else {
                    self.lexer
                        .identifier_text(self.cur.span)
                        .into_owned()
                        .into()
                };
                self.bump();
            } else {
                self.error_expected("import 类型 attribute 名");
                self.ts_recover_declaration_import_attributes(TokenKind::RBrace);
                break;
            }
            self.expect(TokenKind::Colon);
            if self.at(TokenKind::Str) {
                if self.source_syntax.is_some() {
                    attributes.push(wake_ecma_ast::SourceImportAttribute {
                        key: name,
                        value: self.lexer.string_value(self.cur.span),
                        span: Span::new(lo, self.cur.span.hi),
                    });
                }
                self.bump();
            } else {
                self.error_expected("import 类型 attribute 字符串值");
                self.ts_recover_declaration_import_attributes(TokenKind::RBrace);
                break;
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace);
        Some(attributes)
    }

    fn ts_recover_declaration_import_attributes(&mut self, boundary: TokenKind) {
        while !self.at(boundary) && !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            if matches!(
                self.cur.kind,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
            ) {
                self.ts_skip_balanced();
            } else {
                self.bump();
            }
        }
    }

    fn ts_template_literal_type(&mut self) {
        self.template_value(self.cur.span, false, false);
        self.bump(); // TemplateHead `` `..${ ``
        loop {
            self.ts_type();
            match self.cur.kind {
                TokenKind::TemplateMiddle => {
                    self.template_value(self.cur.span, false, false);
                    self.bump();
                }
                TokenKind::TemplateTail => {
                    self.template_value(self.cur.span, true, false);
                    self.bump();
                    break;
                }
                _ => break,
            }
        }
    }

    /// 实体名 `A.B.C`（用于类型引用/typeof）。
    fn ts_entity_name(&mut self, namespace: TsEntityReferenceNamespace) {
        if self.at_ident_name() {
            let binding = self
                .interner
                .intern(&self.lexer.identifier_text(self.cur.span));
            match namespace {
                TsEntityReferenceNamespace::Type => {
                    self.source_identifier(
                        self.cur.span,
                        binding,
                        wake_ecma_ast::SourceIdentifierRole::TypeReference,
                    );
                    self.declaration_record_type_reference(binding);
                }
                TsEntityReferenceNamespace::Value => {
                    self.source_identifier(
                        self.cur.span,
                        binding,
                        wake_ecma_ast::SourceIdentifierRole::TypeQuery,
                    );
                    self.declaration_record_value_reference(binding);
                }
            }
            self.bump();
        } else {
            return;
        }
        while self.at(TokenKind::Dot) {
            self.bump();
            if self.at_ident_name() {
                self.bump();
            } else {
                break;
            }
        }
    }

    // —— 函数/构造类型 ——

    fn ts_at_function_type_start(&mut self) -> bool {
        if self.at(TokenKind::Lt) {
            return true; // <T>() => R
        }
        if self.at_keyword(Keyword::New)
            && matches!(self.peek().kind, TokenKind::Lt | TokenKind::LParen)
        {
            return true; // new () => R
        }
        if self.at_contextual("abstract") && self.peek().kind == TokenKind::Keyword(Keyword::New) {
            return true; // abstract new () => R
        }
        if self.at(TokenKind::LParen) {
            return self.ts_paren_is_function_type();
        }
        false
    }

    /// 当前在 `(`：试探判断是函数类型 `(...) =>` 还是括号类型 `(T)`。
    fn ts_paren_is_function_type(&mut self) -> bool {
        let cp = self.checkpoint();
        self.ts_skip_balanced();
        let is_fn = self.at(TokenKind::Arrow);
        self.rewind(cp);
        is_fn
    }

    /// Function-type returns accept the same type predicates as return annotations, including
    /// `value is T`, `asserts value is T`, and `this` predicates. Signature parameter bindings
    /// remain active while collecting references from the return type.
    fn ts_function_type(&mut self) {
        self.source_begin(SourceNodeKind::TsSignature, self.cur.span.lo);
        let type_scope = self.declaration_type_scope_mark();
        let value_scope = self.declaration_value_scope_mark();
        if self.at_contextual("abstract") {
            self.bump();
        }
        self.eat_keyword(Keyword::New);
        if self.at(TokenKind::Lt) {
            self.ts_type_parameters(TsTypeParameterContext::FunctionLike);
        }
        if self.at(TokenKind::LParen) {
            self.ts_declaration_signature_parameters();
        }
        self.expect(TokenKind::Arrow);
        self.ts_type_or_predicate();
        self.declaration_restore_value_scope(value_scope);
        self.declaration_restore_type_scope(type_scope);
        self.source_end(self.prev_end);
    }

    /// Parse type members for lowering, source facts and strict declaration validation alike.
    /// Erasure must still validate template literals nested in member and signature types.
    pub(crate) fn ts_declaration_type_literal(&mut self) {
        self.ts_declaration_type_literal_with_mapped(true);
    }

    pub(crate) fn ts_declaration_interface_body(&mut self) {
        self.ts_declaration_type_literal_with_mapped(false);
    }

    fn ts_declaration_type_literal_with_mapped(&mut self, allow_mapped: bool) {
        self.source_begin(SourceNodeKind::TsObjectType, self.cur.span.lo);
        self.expect(TokenKind::LBrace);
        let mut member_count = 0usize;
        let mut saw_mapped = false;
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if self.eat(TokenKind::Semicolon) || self.eat(TokenKind::Comma) {
                continue;
            }

            let before = self.cur.span.lo;
            let mapped = self.ts_declaration_type_member();
            if mapped && (!allow_mapped || member_count != 0) {
                self.error(
                    Span::new(before, self.prev_end),
                    "映射类型不能与其他类型成员混合，也不能声明在 interface 中",
                );
            } else if !mapped && saw_mapped {
                self.error(
                    Span::new(before, self.prev_end),
                    "映射类型不能声明其他属性或方法",
                );
            }
            saw_mapped |= mapped;
            member_count += 1;
            if self.eat(TokenKind::Semicolon) || self.eat(TokenKind::Comma) {
                continue;
            }
            if self.at(TokenKind::RBrace) {
                break;
            }
            if !self.newline_before() {
                self.error_expected("类型成员分隔符");
                self.ts_recover_declaration_type_member();
            }
            if self.cur.span.lo == before && !self.at(TokenKind::RBrace) {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        self.source_end(self.prev_end);
    }

    /// `new` starts a construct signature only before a parameter or type-parameter list;
    /// otherwise it is an ordinary property name, including optional properties and methods.
    fn ts_declaration_type_member(&mut self) -> bool {
        self.source_begin(SourceNodeKind::TsTypeMember, self.cur.span.lo);
        let type_scope = self.declaration_type_scope_mark();
        let mapped = self.ts_declaration_type_member_inner();
        self.declaration_restore_type_scope(type_scope);
        self.source_end(self.prev_end);
        mapped
    }

    fn ts_declaration_type_member_inner(&mut self) -> bool {
        let mut signature_requires_return_type = true;
        let mut mapped_readonly_modifier = None;
        // `readonly` is a modifier unless it is itself the property name (`readonly: T`).
        if self.at_contextual("readonly")
            && !matches!(
                self.peek().kind,
                TokenKind::Colon
                    | TokenKind::Question
                    | TokenKind::LParen
                    | TokenKind::Lt
                    | TokenKind::Semicolon
                    | TokenKind::Comma
                    | TokenKind::RBrace
            )
        {
            self.bump();
        } else if matches!(self.cur.kind, TokenKind::Plus | TokenKind::Minus)
            && self.peek_contextual("readonly")
        {
            mapped_readonly_modifier = Some(self.cur.span);
            self.bump();
            self.bump();
        }

        // Call and construct signatures have no property name.
        if self.at(TokenKind::Lt) || self.at(TokenKind::LParen) {
            self.ts_declaration_member_signature(true);
            return false;
        }
        if self.at_keyword(Keyword::New)
            && matches!(self.peek().kind, TokenKind::LParen | TokenKind::Lt)
        {
            self.bump();
            self.ts_declaration_member_signature(true);
            return false;
        }
        if self.at_contextual("abstract") && self.peek().kind == TokenKind::Keyword(Keyword::New) {
            self.bump();
            self.bump();
            self.ts_declaration_member_signature(true);
            return false;
        }

        // Accessor keywords remain legal property names when immediately followed by punctuation.
        let at_get = self.at_contextual("get") || self.at_keyword(Keyword::Get);
        let at_set = self.at_contextual("set") || self.at_keyword(Keyword::Set);
        if (at_get || at_set)
            && !matches!(
                self.peek().kind,
                TokenKind::Colon
                    | TokenKind::Question
                    | TokenKind::LParen
                    | TokenKind::Lt
                    | TokenKind::Semicolon
                    | TokenKind::Comma
                    | TokenKind::RBrace
            )
        {
            signature_requires_return_type = !at_set;
            self.bump();
        }

        let member_name_span = self.cur.span;
        let bracket_member = if self.at(TokenKind::LBracket) {
            Some(self.ts_declaration_bracket_member_name())
        } else if matches!(
            self.cur.kind,
            TokenKind::Ident
                | TokenKind::Keyword(_)
                | TokenKind::Str
                | TokenKind::Number
                | TokenKind::BigInt
        ) {
            self.bump();
            None
        } else {
            self.error_expected("类型成员名或调用签名");
            self.ts_recover_declaration_type_member();
            return false;
        };

        if let Some(modifier_span) = mapped_readonly_modifier
            && bracket_member != Some(TsDeclarationBracketMemberKind::Mapped)
        {
            self.error(
                modifier_span,
                "`+readonly`/`-readonly` 仅允许用于映射类型成员",
            );
        }

        if bracket_member == Some(TsDeclarationBracketMemberKind::Mapped)
            && matches!(self.cur.kind, TokenKind::Plus | TokenKind::Minus)
        {
            self.bump();
            self.expect(TokenKind::Question);
        } else if bracket_member == Some(TsDeclarationBracketMemberKind::Index)
            && self.at(TokenKind::Question)
        {
            self.error(self.cur.span, "索引签名不能是可选成员");
            self.bump();
        } else {
            self.eat(TokenKind::Question);
        }
        if self.at(TokenKind::Lt) || self.at(TokenKind::LParen) {
            self.ts_declaration_member_signature(signature_requires_return_type);
        } else if self.eat(TokenKind::Colon) {
            self.ts_type_or_predicate();
        } else if self.at(TokenKind::Eq) || self.at(TokenKind::LBrace) {
            self.error(self.cur.span, "声明类型成员不能包含初始化器或方法实现");
            self.ts_recover_declaration_type_member();
        } else {
            self.declaration_record_implicit_any(member_name_span);
        }
        bracket_member == Some(TsDeclarationBracketMemberKind::Mapped)
    }

    fn ts_declaration_member_signature(&mut self, requires_return_type: bool) {
        self.source_begin(SourceNodeKind::TsSignature, self.cur.span.lo);
        self.ts_declaration_member_signature_inner(requires_return_type);
        self.source_end(self.prev_end);
    }

    fn ts_declaration_member_signature_inner(&mut self, requires_return_type: bool) {
        let type_scope = self.declaration_type_scope_mark();
        let value_scope = self.declaration_value_scope_mark();
        if self.at(TokenKind::Lt) {
            self.ts_type_parameters(TsTypeParameterContext::FunctionLike);
        }
        if self.at(TokenKind::LParen) {
            self.ts_declaration_signature_parameters();
        } else {
            self.error_expected("声明调用签名参数");
            self.declaration_restore_value_scope(value_scope);
            self.declaration_restore_type_scope(type_scope);
            return;
        }
        let return_type_anchor = self.cur.span;
        if self.eat(TokenKind::Colon) {
            self.ts_type_or_predicate();
        } else if requires_return_type {
            self.declaration_record_implicit_any(return_type_anchor);
        }
        if self.at(TokenKind::LBrace) {
            self.error(self.cur.span, "声明方法不能包含实现");
            self.ts_recover_declaration_type_member();
        }
        self.declaration_restore_value_scope(value_scope);
        self.declaration_restore_type_scope(type_scope);
    }

    fn ts_declaration_bracket_member_name(&mut self) -> TsDeclarationBracketMemberKind {
        self.expect(TokenKind::LBracket);
        let checkpoint = self.checkpoint();
        let has_binding = matches!(self.cur.kind, TokenKind::Ident | TokenKind::Keyword(_));
        if has_binding {
            self.bump();
        }
        let index_signature = has_binding && self.at(TokenKind::Colon);
        let mapped_signature =
            has_binding && (self.at_keyword(Keyword::In) || self.at_contextual("in"));
        self.rewind(checkpoint);

        if index_signature {
            self.bump();
            self.bump(); // `:`
            self.ts_type();
        } else if mapped_signature {
            let span = self.cur.span;
            let binding = self.intern_ident(span);
            self.bump();
            self.bump(); // `in`
            self.ts_type();
            // A mapped key is not in scope in its own constraint, but it is in scope in the
            // optional remapping clause and the mapped value type parsed by the caller.
            self.declaration_record_type_binding(binding);
            let source_scope = self.source_type_scope_begin(
                wake_ecma_ast::SourceTypeScopeKind::MappedType,
                self.cur.span.lo,
            );
            self.source_type_parameter(source_scope, span, binding);
            if self.eat_keyword(Keyword::As) || self.at_contextual("as") {
                if self.at_contextual("as") {
                    self.bump();
                }
                self.ts_type();
            }
        } else if matches!(
            self.cur.kind,
            TokenKind::Ident | TokenKind::Keyword(_) | TokenKind::Str | TokenKind::Number
        ) {
            self.bump();
            while self.eat(TokenKind::Dot) {
                if matches!(self.cur.kind, TokenKind::Ident | TokenKind::Keyword(_)) {
                    self.bump();
                } else {
                    self.error_expected("计算属性名");
                    break;
                }
            }
        } else {
            self.error_expected("索引、映射或计算属性名");
            while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                self.bump();
            }
        }
        self.expect(TokenKind::RBracket);
        if index_signature {
            TsDeclarationBracketMemberKind::Index
        } else if mapped_signature {
            TsDeclarationBracketMemberKind::Mapped
        } else {
            TsDeclarationBracketMemberKind::Computed
        }
    }

    pub(crate) fn ts_class_index_signature(&mut self) {
        self.ts_declaration_bracket_member_name();
    }

    fn ts_declaration_signature_parameters(&mut self) {
        let value_reference_mark = self.declaration_value_reference_mark();
        let source_list = self.source_list_start(wake_ecma_ast::SourceListKind::Parameters);
        let mut rest = false;
        let mut parameters = Vec::new();
        self.expect(TokenKind::LParen);
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            rest = self.eat(TokenKind::DotDotDot);
            while matches!(
                self.cur.kind,
                TokenKind::Keyword(Keyword::Public | Keyword::Private | Keyword::Protected)
            ) || self.at_contextual("readonly")
                || self.at_contextual("override")
            {
                let modifier_span = self.cur.span;
                self.bump();
                if self.declaration_requires_strict_type_syntax() {
                    self.error(modifier_span, "参数属性修饰符仅允许用于类构造函数参数");
                }
            }

            let parameter_span = self.cur.span;
            if self.at_keyword(Keyword::This) {
                self.bump();
            } else if self.at_ident_name()
                || self.at(TokenKind::LBrace)
                || self.at(TokenKind::LBracket)
            {
                let parameter = self.parse_binding_pattern();
                parameters.push(parameter);
            } else {
                self.error_expected("声明签名参数名");
                self.ts_recover_declaration_signature_parameter();
            }
            self.eat(TokenKind::Question);
            if self.ts_type_annotation().is_none() {
                self.declaration_record_implicit_any(parameter_span);
            }
            if self.at(TokenKind::Eq) {
                self.error(self.cur.span, "声明签名参数不能包含初始化器");
                self.ts_recover_declaration_signature_parameter();
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.source_list_finish(source_list, !rest);
        self.expect(TokenKind::RParen);
        self.declaration_activate_parameter_bindings(value_reference_mark, &parameters);
    }

    fn ts_declaration_tuple_type(&mut self) {
        let source_list = self.source_list_start(wake_ecma_ast::SourceListKind::Tuple);
        self.source_begin(SourceNodeKind::TsTupleType, self.cur.span.lo);
        self.expect(TokenKind::LBracket);
        while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
            self.eat(TokenKind::DotDotDot);
            if self.ts_tuple_label_ahead() {
                self.bump();
                self.eat(TokenKind::Question);
                self.expect(TokenKind::Colon);
            }
            self.ts_type();
            self.eat(TokenKind::Question);
            if self.at(TokenKind::Eq) {
                self.error(self.cur.span, "声明元组元素不能包含初始化器");
                self.ts_recover_declaration_tuple_element();
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.source_list_finish(source_list, true);
        self.expect(TokenKind::RBracket);
        self.source_end(self.prev_end);
    }

    fn ts_tuple_label_ahead(&mut self) -> bool {
        if !matches!(self.cur.kind, TokenKind::Ident | TokenKind::Keyword(_)) {
            return false;
        }
        let checkpoint = self.checkpoint();
        self.bump();
        self.eat(TokenKind::Question);
        let is_label = self.at(TokenKind::Colon);
        self.rewind(checkpoint);
        is_label
    }

    fn ts_recover_declaration_type_member(&mut self) {
        while !matches!(
            self.cur.kind,
            TokenKind::Semicolon | TokenKind::Comma | TokenKind::RBrace | TokenKind::Eof
        ) {
            if matches!(
                self.cur.kind,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
            ) {
                self.ts_skip_balanced();
            } else {
                self.bump();
            }
        }
    }

    fn ts_recover_declaration_signature_parameter(&mut self) {
        while !matches!(
            self.cur.kind,
            TokenKind::Comma | TokenKind::RParen | TokenKind::Eof
        ) {
            if matches!(
                self.cur.kind,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
            ) {
                self.ts_skip_balanced();
            } else {
                self.bump();
            }
        }
    }

    fn ts_recover_declaration_tuple_element(&mut self) {
        while !matches!(
            self.cur.kind,
            TokenKind::Comma | TokenKind::RBracket | TokenKind::Eof
        ) {
            if matches!(
                self.cur.kind,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
            ) {
                self.ts_skip_balanced();
            } else {
                self.bump();
            }
        }
    }

    // ==================================================================
    // 原语
    // ==================================================================

    /// 从开括号 `(`/`[`/`{` 消费到匹配的闭括号（含）。模板 `}` 由词法归为 Template*，不误计。
    pub(crate) fn ts_skip_balanced(&mut self) {
        if !matches!(
            self.cur.kind,
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
        ) {
            return;
        }
        self.bump();
        self.ts_skip_balanced_tail(1);
    }

    fn ts_skip_balanced_tail(&mut self, mut depth: i32) {
        loop {
            if self.declaration_in_type()
                && self.at_keyword(Keyword::Import)
                && self.peek().kind == TokenKind::LParen
            {
                self.ts_import_type();
                continue;
            }
            if self.declaration_in_type() && self.at_ident_name() {
                // Object/function member names are bindings/keys rather than type references.
                // Everything else is a parser-observed type-position reference; imported
                // bindings are filtered from declaration output using these atoms.
                let is_member_name = self.declaration_identifier_is_member_name();
                if self.at_contextual("any") && !is_member_name {
                    self.declaration_record_any(self.cur.span);
                }
                if !is_member_name {
                    let binding = self.intern_slice(self.cur.span);
                    self.declaration_record_type_reference(binding);
                }
            }
            match self.cur.kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => depth -= 1,
                TokenKind::Eof => {
                    self.error_expected("对应的 TypeScript 结束分隔符");
                    return;
                }
                _ => {}
            }
            self.bump();
            if depth == 0 {
                return;
            }
        }
    }

    fn declaration_identifier_is_member_name(&mut self) -> bool {
        match self.peek().kind {
            TokenKind::Colon | TokenKind::LParen => true,
            TokenKind::Question => {
                let checkpoint = self.checkpoint();
                self.bump(); // any
                self.bump(); // ?
                let is_member = matches!(self.cur.kind, TokenKind::Colon | TokenKind::LParen);
                self.rewind(checkpoint);
                is_member
            }
            _ => false,
        }
    }

    /// 当前 token 是否以 `>` 起始（`>`/`>>`/`>>>`/`>=`/`>>=`/`>>>=`）。
    fn at_type_gt(&self) -> bool {
        matches!(
            self.cur.kind,
            TokenKind::Gt
                | TokenKind::Shr
                | TokenKind::Ushr
                | TokenKind::GtEq
                | TokenKind::ShrEq
                | TokenKind::UshrEq
        )
    }

    /// 消费一个 `>` 收尾类型实参/参数：若当前是 `>>`/`>>>`/`>=` 等，则「拆」出一个 `>`，
    /// 余下部分从下一位置重新词法化（reScanGreaterToken）。
    pub(crate) fn consume_type_gt(&mut self) {
        match self.cur.kind {
            TokenKind::Gt => {
                self.bump();
            }
            TokenKind::Shr
            | TokenKind::Ushr
            | TokenKind::GtEq
            | TokenKind::ShrEq
            | TokenKind::UshrEq => {
                let lo = self.cur.span.lo;
                self.source_token(
                    wake_ecma_lexer::Token::new(
                        TokenKind::Gt,
                        Span::new(lo, lo + 1),
                        self.cur.newline_before,
                    ),
                    crate::SourceTokenContext::JavaScript,
                );
                self.prev_end = lo + 1;
                self.cur = self.lexer.next_at(lo + 1, false);
                self.lookahead = None;
            }
            _ => self.error_expected(">"),
        }
    }

    /// 前瞻 token 是否是上下文关键字 `name`（Ident 且文本相符）。
    pub(crate) fn peek_contextual(&mut self, name: &str) -> bool {
        let p = self.peek();
        p.kind == TokenKind::Ident && self.slice(p.span) == name
    }

    /// `namespace`/`module` 是否引导声明（后接名字或字符串模块名，且同行）。
    pub(crate) fn ts_namespace_is_declaration(&mut self) -> bool {
        let p = self.peek();
        if p.newline_before {
            return false;
        }
        matches!(p.kind, TokenKind::Ident | TokenKind::Str)
            || matches!(p.kind, TokenKind::Keyword(kw) if !kw.is_reserved())
    }

    /// 当前为 `type`，判断是否内联类型说明符 `{ type X }`（`type` 后接名字且不是 `as`）。
    /// 排除 `{ type as X }`（导入名为 `type`）。
    pub(crate) fn ts_inline_type_specifier_ahead(&mut self) -> bool {
        let p = self.peek();
        if matches!(p.kind, TokenKind::Keyword(Keyword::As)) {
            return false;
        }
        matches!(p.kind, TokenKind::Ident | TokenKind::Str)
            || matches!(p.kind, TokenKind::Keyword(kw) if !kw.is_reserved())
    }

    // ==================================================================
    // 声明级：declare 环境声明 / 参数修饰符
    // ==================================================================

    /// `declare` 是否引导一个声明（而非用作标识符）。要求同行紧跟声明起始 token。
    pub(crate) fn ts_declare_is_declaration(&mut self) -> bool {
        let p = self.peek();
        if p.newline_before {
            return false;
        }
        match p.kind {
            TokenKind::Keyword(
                Keyword::Const
                | Keyword::Let
                | Keyword::Var
                | Keyword::Function
                | Keyword::Class
                | Keyword::Enum
                | Keyword::Interface
                | Keyword::Async
                | Keyword::Export,
            ) => true,
            TokenKind::Ident => matches!(
                self.slice(p.span),
                "namespace" | "module" | "global" | "type" | "abstract"
            ),
            _ => false,
        }
    }

    /// 消费装饰器 `@expr`（一个或多个）。用于类/成员/参数装饰器，使被装饰代码可解析。
    /// **注意**：当前仅擦除装饰器，不生成其运行时语义（legacy `__decorate`/`__param` 转换属大型
    /// 独立切片，React TSX 基本不用装饰器）。装饰器表达式本身按 LHS 消费（`@a.b(c)` 等）。
    pub(crate) fn skip_decorators(&mut self) {
        let _ = self.parse_decorators();
    }

    /// 解析装饰器序列 `@a @b.c(d)`，返回其表达式（按源码序）。
    ///
    /// TC39 Stage-3 语义：装饰器表达式按**源码序求值**，但按**相反序应用**——
    /// 后者由 `__esDecorate` 内部倒序遍历实现（见 codegen 发射的运行时辅助）。
    pub(crate) fn parse_decorators(&mut self) -> wake_ecma_ast::AVec<'a, Expression<'a>> {
        let mut out = self.new_vec::<Expression>();
        while self.at(TokenKind::At) {
            self.bump(); // @
            out.push(self.parse_decorator_expression());
        }
        out
    }

    /// 消费参数属性/参数修饰符（public/private/protected/readonly/override）。
    /// 返回是否消费了任一修饰符（→ 该参数为「参数属性」，需在构造函数体注入 `this.x = x`）。
    pub(crate) fn ts_skip_param_modifiers(&mut self) -> bool {
        let mut any = false;
        loop {
            let is_mod = matches!(
                self.cur.kind,
                TokenKind::Keyword(Keyword::Public | Keyword::Private | Keyword::Protected)
            ) || self.at_contextual("readonly")
                || self.at_contextual("override");
            if !is_mod {
                break;
            }
            // 仅当其后是绑定起始（标识符/`{`/`[`/另一修饰符）时才当修饰符。
            match self.peek().kind {
                TokenKind::Ident | TokenKind::LBrace | TokenKind::LBracket => self.bump(),
                TokenKind::Keyword(kw) if !kw.is_reserved() => self.bump(),
                _ => break,
            };
            any = true;
        }
        any
    }
}
