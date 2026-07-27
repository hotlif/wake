//! TypeScript **值语义**构造的降级（非纯擦除）——参照 tsc emit。
//!
//! 目前实现 `enum` / `const enum` → 单条 `var E = function (E) { ...; return E; }({})`：
//! - 数字成员：`E[E["A"] = 0] = "A"`（正向 + 反向映射，支持自增）；
//! - 字符串/表达式成员：`E["B"] = <expr>`（仅正向）。
//!
//! 函数在初始化位置（`=` 右侧）是函数表达式，无需额外括号。
//!
//! `namespace`/`module` 同样降级为 IIFE：`export` 成员改写为 `N.name = name`（并保留局部声明），
//! 非导出成员保持局部。**点分名** `A.B.C` 展开为嵌套 IIFE；实参用 `X || (X = {})`，
//! 即 TS 的**声明合并**——同名 enum/namespace 再次出现时复用已有对象。

use wake_common::{Atom, Span};
use wake_ecma_ast::*;
use wake_ecma_lexer::TokenKind;

use crate::Parser;

impl<'a, 'src> Parser<'a, 'src> {
    /// 解析 `enum E { .. }`（`const` 已由调用方消费；当前 token 为 `enum`）。
    pub(crate) fn parse_enum(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // enum
        let name = if self.at_ident_name() {
            self.parse_binding_ident()
        } else {
            self.error_expected("枚举名");
            Ident::new(self.cur.span, self.interner.intern("__enum__"))
        };
        let e_atom = name.name;
        self.expect(TokenKind::LBrace);

        let mut stmts = self.new_vec::<Statement>();
        let mut auto: f64 = 0.0;
        let mut auto_ok = true; // 下一个自增值是否仍为合法数字

        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            // 成员名：标识符名 或 字符串。
            let member_atom = if self.at(TokenKind::Str) {
                let a = self.enum_string_atom(self.cur.span);
                self.bump();
                a
            } else {
                let id = self.parse_ident_name();
                id.name
            };

            // 初始值。
            let (init, is_numeric) = if self.eat(TokenKind::Eq) {
                let e = self.parse_assignment_expression();
                match e {
                    Expression::NumberLiteral(n) => {
                        auto = n.value + 1.0;
                        auto_ok = true;
                        (e, true)
                    }
                    _ => {
                        auto_ok = false;
                        (e, false)
                    }
                }
            } else if auto_ok {
                let e = self.num_lit(auto);
                auto += 1.0;
                (e, true)
            } else {
                // 非数字成员之后的自增成员在 TS 中即报错；此处退化为 undefined。
                (self.undefined_expr(), false)
            };

            // E["member"]
            let forward = self.member_str(e_atom, member_atom);
            // E["member"] = init
            let assign1 = self.assign(forward, init);
            if is_numeric {
                // 反向映射：E[ (E["member"] = init) ] = "member"
                let outer = self.member_expr_computed(e_atom, assign1);
                let key_str = self.str_lit(member_atom);
                let assign2 = self.assign(outer, key_str);
                stmts.push(self.expr_stmt(assign2));
            } else {
                stmts.push(self.expr_stmt(assign1));
            }

            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace);

        // return E;
        stmts.push(Statement::Return(self.alloc(ReturnStatement {
            span: Span::DUMMY,
            argument: Some(self.ident_ref(e_atom)),
        })));

        // var E = function (E) { stmts }(E || (E = {}));
        // 用 `||` 而非 `{}`：TS 的 enum 同样支持跨声明合并。
        let init = self.or_assign_ident(e_atom);
        let decl_span = self.span_to(lo);
        self.build_value_iife(decl_span, name, stmts, init)
    }

    /// 解析 `namespace N { .. }` / `module N { .. }` → `var N = function (N) { ..; return N; }(N || (N = {}))`。
    ///
    /// `export` 成员改写为 `N.name = name`（同时保留局部声明，供内部引用）；非导出成员保持局部。
    /// 点分名 `A.B.C` 展开为嵌套 IIFE（成员挂最内层段）；`||` 初始化式支持跨声明合并。
    /// 字符串模块名（ambient `module "x" {}`）→ 擦除。
    pub(crate) fn parse_namespace(&mut self, lo: u32) -> Statement<'a> {
        self.bump(); // namespace / module

        // ambient 模块 `module "x" { .. }` → 擦除。
        if self.at(TokenKind::Str) {
            self.bump();
            if self.at(TokenKind::LBrace) {
                self.ts_skip_balanced();
            }
            return Statement::Empty(self.span_to(lo));
        }

        let name = if self.at_ident_name() {
            self.parse_binding_ident()
        } else {
            self.error_expected("命名空间名");
            Ident::new(self.cur.span, self.interner.intern("__ns__"))
        };
        // 点分名 `A.B.C` → 逐段收集，最终展开为嵌套 IIFE（对齐 tsc）。
        let mut segments: Vec<Ident> = vec![name];
        while self.at(TokenKind::Dot) {
            self.bump();
            if self.at_ident_name() {
                segments.push(self.parse_binding_ident());
            } else {
                break;
            }
        }
        // 成员挂载到**最内层**段上（`namespace A.B.C { export const x }` → `C.x`）。
        let inner_atom = segments[segments.len() - 1].name;
        self.expect(TokenKind::LBrace);

        let mut stmts = self.new_vec::<Statement>();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let before = self.cur.span.lo;
            let s = self.parse_statement();
            self.lower_namespace_member(s, inner_atom, &mut stmts);
            if self.cur.span.lo == before && !self.at(TokenKind::RBrace) {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);

        stmts.push(Statement::Return(self.alloc(ReturnStatement {
            span: Span::DUMMY,
            argument: Some(self.ident_ref(inner_atom)),
        })));

        // 由内向外逐层包裹：最内层的初始化实参是 `<父段>.<本段> || (<父段>.<本段> = {})`，
        // 最外层是 `A || (A = {})`——`||` 即 TS 的**声明合并**机制：同名 namespace 再次出现时
        // 复用已存在的对象，而非新建。
        let mut current = stmts;
        for depth in (0..segments.len()).rev() {
            let seg = segments[depth];
            let init = if depth == 0 {
                // 顶层：`A || (A = {})`
                self.or_assign_ident(seg.name)
            } else {
                // 嵌套：`Parent.Seg || (Parent.Seg = {})`
                self.or_assign_member(segments[depth - 1].name, seg.name)
            };
            // **每层必须用各自的 span**：压缩器的侧表以 span 为键，若各层共用同一 span，
            // 对某一层的「未用绑定消除」判定会连带命中其它层，把整个嵌套削平
            // （曾致 `namespace A.B.C` 的第二个声明块被整体消除、合并失效）。
            // 段标识符在源码中位置互不相同，天然可作唯一键；最外层用整条语句的 span。
            let decl_span = if depth == 0 {
                self.span_to(lo)
            } else {
                seg.span
            };
            let iife = self.build_value_iife(decl_span, seg, current, init);
            if depth == 0 {
                return iife;
            }
            // 外层函数体：`var Seg = <iife>;` + `return Parent;`
            let mut outer = self.new_vec::<Statement>();
            outer.push(iife);
            outer.push(Statement::Return(self.alloc(ReturnStatement {
                span: Span::DUMMY,
                argument: Some(self.ident_ref(segments[depth - 1].name)),
            })));
            current = outer;
        }
        unreachable!("segments 非空，depth==0 时已 return")
    }

    /// `X || (X = {})`——命名空间/枚举的**声明合并**初始化式。
    fn or_assign_ident(&self, name: Atom) -> Expression<'a> {
        let assign = Expression::Assignment(self.alloc(AssignmentExpression {
            span: Span::DUMMY,
            operator: AssignmentOperator::Assign,
            left: self.ident_ref(name),
            right: self.empty_object(),
        }));
        Expression::Logical(self.alloc(LogicalExpression {
            span: Span::DUMMY,
            operator: LogicalOperator::Or,
            left: self.ident_ref(name),
            right: assign,
        }))
    }

    /// `Obj.Prop || (Obj.Prop = {})`。
    fn or_assign_member(&self, obj: Atom, prop: Atom) -> Expression<'a> {
        let assign = Expression::Assignment(self.alloc(AssignmentExpression {
            span: Span::DUMMY,
            operator: AssignmentOperator::Assign,
            left: self.member_dot(obj, prop),
            right: self.empty_object(),
        }));
        Expression::Logical(self.alloc(LogicalExpression {
            span: Span::DUMMY,
            operator: LogicalOperator::Or,
            left: self.member_dot(obj, prop),
            right: assign,
        }))
    }

    fn empty_object(&self) -> Expression<'a> {
        Expression::Object(self.alloc(ObjectExpression {
            span: Span::DUMMY,
            properties: self.new_vec(),
        }))
    }

    /// 命名空间成员降级：`export` 声明 → 声明 + `N.name = name`；其余保持局部；空语句（类型）丢弃。
    fn lower_namespace_member(
        &self,
        s: Statement<'a>,
        n_atom: Atom,
        out: &mut wake_ecma_ast::AVec<'a, Statement<'a>>,
    ) {
        match s {
            Statement::ExportNamed(e) => {
                if let Some(decl) = e.declaration {
                    out.push(decl);
                    for name in declared_names(&decl) {
                        out.push(self.expr_stmt(self.dot_assign(n_atom, name)));
                    }
                } else {
                    // export { a, b as c } → N.c = a ...
                    for spec in e.specifiers.iter() {
                        if let (ModuleExportName::Ident(local), ModuleExportName::Ident(exported)) =
                            (spec.local, spec.exported)
                        {
                            let assign = Expression::Assignment(self.alloc(AssignmentExpression {
                                span: Span::DUMMY,
                                operator: AssignmentOperator::Assign,
                                left: self.member_dot(n_atom, exported.name),
                                right: self.ident_ref(local.name),
                            }));
                            out.push(self.expr_stmt(assign));
                        }
                    }
                }
            }
            Statement::Empty(_) => {} // 类型/接口等已擦除
            other => out.push(other),
        }
    }

    /// `N.name = name`。
    fn dot_assign(&self, obj: Atom, name: Atom) -> Expression<'a> {
        Expression::Assignment(self.alloc(AssignmentExpression {
            span: Span::DUMMY,
            operator: AssignmentOperator::Assign,
            left: self.member_dot(obj, name),
            right: self.ident_ref(name),
        }))
    }

    /// `obj.name`（非计算成员）。
    fn member_dot(&self, obj: Atom, name: Atom) -> Expression<'a> {
        Expression::Member(self.alloc(MemberExpression {
            span: Span::DUMMY,
            object: self.ident_ref(obj),
            property: MemberProperty::Ident(Ident::new(Span::DUMMY, name)),
            optional: false,
        }))
    }

    /// 构造 `var Name = function (Name) { <stmts> }(<init>);`（enum/namespace 共用）。
    ///
    /// `init` 通常是 `Name || (Name = {})`——即 TS 的**声明合并**：同名 enum/namespace 再次
    /// 出现时复用已有对象。传 `{}` 则每次新建（不合并）。
    fn build_value_iife(
        &self,
        decl_span: Span,
        name: Ident,
        stmts: wake_ecma_ast::AVec<'a, Statement<'a>>,
        init: Expression<'a>,
    ) -> Statement<'a> {
        let atom = name.name;
        let mut params = self.new_vec::<Pattern>();
        params.push(Pattern::Ident(self.alloc(Ident::new(Span::DUMMY, atom))));
        let body = self.alloc(FunctionBody {
            span: Span::DUMMY,
            statements: stmts,
            strict: false,
        });
        let func = self.alloc(Function {
            span: Span::DUMMY,
            id: None,
            params,
            body: Some(body),
            is_async: false,
            is_generator: false,
        });
        let mut args = self.new_vec::<Expression>();
        args.push(init);
        let iife = Expression::Call(self.alloc(CallExpression {
            span: Span::DUMMY,
            callee: Expression::Function(func),
            arguments: args,
            optional: false,
        }));
        let mut decls = self.new_vec::<VariableDeclarator>();
        decls.push(VariableDeclarator {
            span: decl_span,
            id: Pattern::Ident(self.alloc(name)),
            init: Some(iife),
        });
        Statement::VariableDeclaration(self.alloc(VariableDeclaration {
            span: decl_span,
            kind: VarKind::Var,
            declarations: decls,
        }))
    }

    /// TS `export = value;` 的降级：`module.exports = value;`（对齐 tsc 的 commonjs emit）。
    ///
    /// 必须逐字发射 `module.exports`：bundler 的 `compact_body_names` 以**文本**匹配这一串把它
    /// 改写成包装器形参 `m.exports`（见 incremental.rs 的注释），换成别的写法会静默丢导出。
    pub(crate) fn module_exports_assign(&self, span: Span, value: Expression<'a>) -> Statement<'a> {
        let target = Expression::Member(self.alloc(MemberExpression {
            span: Span::DUMMY,
            object: self.ident_ref(self.interner.intern("module")),
            property: MemberProperty::Ident(Ident::new(
                Span::DUMMY,
                self.interner.intern("exports"),
            )),
            optional: false,
        }));
        Statement::Expression(self.alloc(ExpressionStatement {
            span,
            expression: self.assign(target, value),
        }))
    }

    // —— 小构造器 ——

    fn enum_string_atom(&self, span: Span) -> Atom {
        self.interner.intern(&self.lexer.string_value(span))
    }

    fn ident_ref(&self, name: Atom) -> Expression<'a> {
        Expression::Identifier(self.alloc(Ident::new(Span::DUMMY, name)))
    }

    fn str_lit(&self, value: Atom) -> Expression<'a> {
        Expression::StringLiteral(self.alloc(StringLiteral {
            span: Span::DUMMY,
            value,
        }))
    }

    pub(crate) fn num_lit(&self, value: f64) -> Expression<'a> {
        Expression::NumberLiteral(self.alloc(NumberLiteral {
            span: Span::DUMMY,
            value,
        }))
    }

    pub(crate) fn undefined_expr(&self) -> Expression<'a> {
        self.ident_ref(self.interner.intern("undefined"))
    }

    /// `E["member"]`（计算成员，键为字符串字面量）。
    fn member_str(&self, obj: Atom, member: Atom) -> Expression<'a> {
        let key = self.str_lit(member);
        self.member_expr_computed(obj, key)
    }

    /// `E[<key_expr>]`（计算成员）。
    fn member_expr_computed(&self, obj: Atom, key: Expression<'a>) -> Expression<'a> {
        Expression::Member(self.alloc(MemberExpression {
            span: Span::DUMMY,
            object: self.ident_ref(obj),
            property: MemberProperty::Computed(key),
            optional: false,
        }))
    }

    fn assign(&self, left: Expression<'a>, right: Expression<'a>) -> Expression<'a> {
        Expression::Assignment(self.alloc(AssignmentExpression {
            span: Span::DUMMY,
            operator: AssignmentOperator::Assign,
            left,
            right,
        }))
    }

    fn expr_stmt(&self, expression: Expression<'a>) -> Statement<'a> {
        Statement::Expression(self.alloc(ExpressionStatement {
            span: Span::DUMMY,
            expression,
        }))
    }
}

/// 提取声明语句引入的名字（简单标识符）——用于命名空间 `export` 成员改写为 `N.name`。
fn declared_names(s: &Statement) -> Vec<Atom> {
    let mut names = Vec::new();
    match s {
        Statement::VariableDeclaration(d) => {
            for decl in d.declarations.iter() {
                if let Pattern::Ident(id) = &decl.id {
                    names.push(id.name);
                }
            }
        }
        Statement::FunctionDeclaration(f) => {
            if let Some(id) = f.id {
                names.push(id.name);
            }
        }
        Statement::ClassDeclaration(c) => {
            if let Some(id) = c.id {
                names.push(id.name);
            }
        }
        _ => {}
    }
    names
}
