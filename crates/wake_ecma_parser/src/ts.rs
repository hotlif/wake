//! 完整 TypeScript 类型语法**消费器**（擦除）——参照 typescript-go / tsc 的 parser 结构。
//!
//! 与旧的「bracket-depth 启发式」不同，这里按真实类型文法**结构化消费**类型语法（不建类型 AST，
//! 仅推进 token），从而在所有位置都能精确定位类型的起止、正确处理 `>>` 拆分、函数类型 `=>`、
//! 条件类型 `A extends B ? C : D`、`keyof`/`typeof`/`infer`、映射/对象/元组/模板字面量类型等。
//!
//! 括号包裹的构造（`(...)`/`[...]`/`{...}`）本身自平衡，用 [`Parser::ts_skip_balanced`] 消费即完整；
//! `<...>`（类型参数/实参）用 [`Parser::consume_type_gt`] 精确处理 `>`/`>>`/`>>>` 收尾；
//! 「扁平」文法（联合/交叉/条件/引用/前缀算子）按产生式递归消费。

use wake_ecma_ast::Expression;
use wake_ecma_lexer::{Keyword, TokenKind};

use crate::Parser;

impl<'a, 'src, const LOWER: bool> Parser<'a, 'src, LOWER> {
    // ==================================================================
    // 对外集成入口
    // ==================================================================

    /// `: Type`（含类型谓词 `x is T` / `asserts x is T`）。仅 ts 且当前为 `:` 时消费。
    pub(crate) fn ts_type_annotation(&mut self) {
        if self.ts && self.eat(TokenKind::Colon) {
            self.ts_type_or_predicate();
        }
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
    pub(crate) fn ts_type_parameters(&mut self) {
        if !self.ts || !self.at(TokenKind::Lt) {
            return;
        }
        self.bump(); // <
        while !self.at_type_gt() && !self.at(TokenKind::Eof) {
            // 方差/const 修饰符（in / out / const）。
            while self.at_contextual("in")
                || self.at_contextual("out")
                || self.at_keyword(Keyword::In)
                || self.at_keyword(Keyword::Const)
            {
                self.bump();
            }
            if self.at_ident_name() {
                self.bump(); // 参数名
            }
            if self.eat_keyword(Keyword::Extends) {
                self.ts_type();
            }
            if self.eat(TokenKind::Eq) {
                self.ts_type();
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.consume_type_gt();
    }

    /// 类型实参 `<A, B>`（非试探，假设当前 `<`）。用于 `expr as Foo<T>`、`extends Base<T>` 等。
    pub(crate) fn ts_type_arguments(&mut self) {
        if !self.at(TokenKind::Lt) {
            return;
        }
        self.bump(); // <
        while !self.at_type_gt() && !self.at(TokenKind::Eof) {
            self.ts_type();
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.consume_type_gt();
    }

    /// 试探消费调用/`new`/标签模板处的类型实参 `<A, B>`。仅当其后紧跟 `(`/模板等
    /// 「可跟在类型实参后的表达式 token」时才认定并保留消费，否则回溯（把 `<` 交回二元小于）。
    pub(crate) fn try_ts_type_arguments(&mut self) -> bool {
        debug_assert!(self.at(TokenKind::Lt));
        let cp = self.checkpoint();
        self.ts_type_arguments();
        if self.can_follow_type_args_in_expr() {
            true
        } else {
            self.rewind(cp);
            false
        }
    }

    /// 类型实参之后、能表明这是「带类型实参的调用/标签模板」的 token。
    fn can_follow_type_args_in_expr(&self) -> bool {
        matches!(
            self.cur.kind,
            TokenKind::LParen | TokenKind::TemplateNoSub | TokenKind::TemplateHead
        )
    }

    // ==================================================================
    // 类型文法（消费）
    // ==================================================================

    /// 完整类型：函数/构造类型，或联合类型 + 可选条件类型。
    pub(crate) fn ts_type(&mut self) {
        if self.ts_at_function_type_start() {
            self.ts_function_type();
            return;
        }
        self.ts_union();
        // 条件类型 `A extends B ? C : D`。
        if self.at_keyword(Keyword::Extends) && !self.newline_before() {
            self.bump(); // extends
            // extends 类型：不再吃条件，避免贪婪。
            self.ts_union();
            if self.eat(TokenKind::Question) {
                self.ts_type();
                self.expect(TokenKind::Colon);
                self.ts_type();
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
            self.bump();
            if self.at_keyword(Keyword::Import) {
                self.ts_import_type();
            } else {
                self.ts_entity_name();
            }
            if self.at(TokenKind::Lt) {
                self.ts_type_arguments();
            }
            return;
        }
        // infer T (extends U)?
        if self.at_contextual("infer") {
            self.bump();
            if self.at_ident_name() {
                self.bump();
            }
            if self.at_keyword(Keyword::Extends) && !self.newline_before() {
                self.bump();
                self.ts_type_operand();
            }
            return;
        }
        // keyof / readonly / unique：算子 + 操作数。
        if self.at_contextual("keyof")
            || self.at_contextual("readonly")
            || self.at_contextual("unique")
        {
            self.bump();
            self.ts_type_operand();
            return;
        }
        self.ts_postfix();
    }

    fn ts_postfix(&mut self) {
        self.ts_primary();
        // 后缀数组/索引访问 `T[]` / `T[K]`（同行）。
        while !self.newline_before() && self.at(TokenKind::LBracket) {
            self.ts_skip_balanced();
        }
    }

    fn ts_primary(&mut self) {
        match self.cur.kind {
            // 括号/对象(映射)/元组类型：自平衡，整体消费。
            TokenKind::LParen | TokenKind::LBrace | TokenKind::LBracket => self.ts_skip_balanced(),
            // this / void / null / const 类型。
            // `const` 是 `as const` / `<const>` 断言的类型位置写法；它是保留字，
            // ts_entity_name 会拒收，故在此显式消费（否则整段解析在 `const` 处失步）。
            TokenKind::Keyword(Keyword::This)
            | TokenKind::Keyword(Keyword::Void)
            | TokenKind::Keyword(Keyword::Null)
            | TokenKind::Keyword(Keyword::True)
            | TokenKind::Keyword(Keyword::False)
            | TokenKind::Keyword(Keyword::Const) => {
                self.bump();
            }
            // 字面量类型。
            TokenKind::Str | TokenKind::Number | TokenKind::BigInt => {
                self.bump();
            }
            TokenKind::Minus => {
                self.bump();
                self.eat(TokenKind::Number);
            }
            // 模板字面量类型 `` `a${T}b` ``。
            TokenKind::TemplateNoSub => {
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
                self.ts_entity_name();
                if self.at(TokenKind::Lt) {
                    self.ts_type_arguments();
                }
            }
            _ => {
                // 兜底：吃一个 token，避免死循环。
                self.bump();
            }
        }
    }

    /// `import("mod")` 类型（`.entity` 尾巴由调用方按需接类型实参）。
    fn ts_import_type(&mut self) {
        self.bump(); // import
        if self.at(TokenKind::LParen) {
            self.ts_skip_balanced();
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

    fn ts_template_literal_type(&mut self) {
        self.bump(); // TemplateHead `` `..${ ``
        loop {
            self.ts_type();
            match self.cur.kind {
                TokenKind::TemplateMiddle => {
                    self.bump();
                }
                TokenKind::TemplateTail => {
                    self.bump();
                    break;
                }
                _ => break,
            }
        }
    }

    /// 实体名 `A.B.C`（用于类型引用/typeof）。
    fn ts_entity_name(&mut self) {
        if self.at_ident_name() {
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
        if self.at_keyword(Keyword::New) {
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

    fn ts_function_type(&mut self) {
        if self.at_contextual("abstract") {
            self.bump();
        }
        self.eat_keyword(Keyword::New);
        if self.at(TokenKind::Lt) {
            self.ts_type_parameters();
        }
        if self.at(TokenKind::LParen) {
            self.ts_skip_balanced();
        }
        self.expect(TokenKind::Arrow);
        self.ts_type();
    }

    // ==================================================================
    // 原语
    // ==================================================================

    /// 从开括号 `(`/`[`/`{` 消费到匹配的闭括号（含）。模板 `}` 由词法归为 Template*，不误计。
    pub(crate) fn ts_skip_balanced(&mut self) {
        let mut depth: i32 = 0;
        loop {
            match self.cur.kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => depth -= 1,
                TokenKind::Eof => return,
                _ => {}
            }
            self.bump();
            if depth == 0 {
                return;
            }
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

    /// 消费一个「环境声明」（`declare` 之后）用于整体擦除：遇 depth-0 的 `{` 体则整体消费；
    /// 否则到 depth-0 的 `;`/换行/`}`/EOF 结束。括号内容保持平衡消费。
    pub(crate) fn ts_skip_ambient(&mut self) {
        let mut started = false;
        loop {
            match self.cur.kind {
                TokenKind::LBrace => {
                    self.ts_skip_balanced();
                    return;
                }
                TokenKind::Semicolon => {
                    self.bump();
                    return;
                }
                TokenKind::RBrace | TokenKind::Eof => return,
                TokenKind::LParen | TokenKind::LBracket => {
                    self.ts_skip_balanced();
                    started = true;
                    continue;
                }
                _ => {}
            }
            if started && self.newline_before() {
                return;
            }
            self.bump();
            started = true;
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
            out.push(self.parse_lhs_expression());
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
