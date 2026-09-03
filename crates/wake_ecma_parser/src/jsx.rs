//! JSX 解析 + 降级（DESIGN §4.3，M4）。
//!
//! 路线：**解析时直接降级**为 automatic runtime 调用，产出标准 AST（`_jsx`/`_jsxs`/`_Fragment`
//! 调用 + 对象/数组字面量），**codegen 无需任何 JSX 逻辑**。模块顶部只注入实际调用的
//! automatic-runtime binding，依赖与 CJS 互操作全走现有机制。
//!
//! 词法配合：`>`/`}` 之后的子节点文本由 [`wake_ecma_lexer::Lexer::next_jsx_child_token`] 扫描；
//! 标签内部（名字/属性/`/`/`>`）用普通词法 + 少量原始字节判断（避开 `/` 的 regex 歧义、支持连字符名）。
//!
//! 覆盖：元素/片段、intrinsic（小写→字符串）与组件（大写/成员 `A.B`）、**命名空间名 `a:b`**
//! （整体作字符串类型，同 tsc）、属性（字符串/`{表达式}`/布尔简写/`{...spread}`）、连字符属性名
//! （`data-*`/`aria-*`）、`key`（提到第 3 参）、子节点（文本/`{表达式}`/嵌套元素）、自闭合、
//! HTML 实体解码。
//!
//! **runtime 口径**由 [`crate::ParseOptions`] 决定：
//! - `jsx_import_source`（默认 `"react"`）→ 导入 `<source>/jsx-runtime`；
//! - `jsx_dev` → 改用 **dev runtime**：`_jsxDEV(type, props, key, isStaticChildren,
//!   {fileName,lineNumber,columnNumber}, this)`（对齐 tsc `--jsx react-jsxdev`），
//!   供 React DevTools 显示组件栈。
//!
//! 未覆盖：classic runtime / `@jsx` pragma——**不在 legacy tool 对齐范围**（legacy tool 显式配置
//! `@babel/preset-react` 的 `runtime: "automatic"`）；多行属性字符串中的 JS 转义。

use wake_common::{Atom, Span};
use wake_ecma_ast::{
    ArrayExpression, Dependency, DependencyKind, Expression, Ident, ImportDeclaration,
    ImportSpecifier, MemberExpression, MemberProperty, ModuleExportName, ObjectExpression,
    ObjectMember, ObjectProperty, PropertyKey, PropertyKind, SpreadElement, Statement,
    StringLiteral,
};
use wake_ecma_lexer::{Lexer, TokenKind};
use wake_ecma_transform::{
    AutomaticJsxBinding, AutomaticJsxCall, AutomaticJsxCallKind, AutomaticJsxRuntime,
    lower_automatic_jsx_call,
};

use crate::Parser;

impl<'a, 'src, const LOWER: bool> Parser<'a, 'src, LOWER> {
    /// 驻留任意字符串为 Atom。
    fn intern_str(&self, s: &str) -> Atom {
        self.interner.intern(s)
    }

    /// 重新以「普通词法」从 `from` 取 token 到 `cur`（regex 关闭，用于 JSX 标签内部/恢复）。
    fn jsx_relex(&mut self, from: u32) {
        self.cur = self.lexer.next_at(from, false);
        self.lookahead = None;
        self.prev_end = from;
    }

    /// 重新以「表达式起始」从 `from` 取 token 到 `cur`（regex 允许，用于 `{表达式}`）。
    fn jsx_relex_expr(&mut self, from: u32) {
        self.cur = self.lexer.next_at(from, true);
        self.lookahead = None;
        self.prev_end = from;
    }

    fn ident_expr_atom(&self, span: Span, name: Atom) -> Expression<'a> {
        Expression::Identifier(self.alloc(Ident::new(span, name)))
    }

    fn automatic_jsx_runtime(&self) -> AutomaticJsxRuntime {
        if self.options.jsx_dev {
            AutomaticJsxRuntime::Development
        } else {
            AutomaticJsxRuntime::Production
        }
    }

    /// Keep historical aliases when they cannot capture source identifiers. A source occurrence
    /// selects a deterministic Wake-owned fallback; the conservative text check also protects
    /// unresolved references which would otherwise become captured by the synthetic import.
    fn automatic_jsx_alias(&self, binding: AutomaticJsxBinding) -> Atom {
        let preferred = binding.preferred_local();
        if !self.automatic_jsx_alias_taken(preferred) {
            return self.interner.intern(preferred);
        }

        let fallback = match binding {
            AutomaticJsxBinding::Jsx => "__wake_jsx",
            AutomaticJsxBinding::Jsxs => "__wake_jsxs",
            AutomaticJsxBinding::Fragment => "__wake_fragment",
            AutomaticJsxBinding::JsxDev => "__wake_jsx_dev",
        };
        for suffix in 0_u32.. {
            let candidate = if suffix == 0 {
                fallback.to_string()
            } else {
                format!("{fallback}{suffix}")
            };
            if !self.automatic_jsx_alias_taken(&candidate) {
                return self.interner.intern(&candidate);
            }
        }
        unreachable!("the finite source cannot contain every generated JSX alias")
    }

    fn automatic_jsx_alias_taken(&self, candidate: &str) -> bool {
        if self.source.contains(candidate) {
            return true;
        }
        if !self.source.contains('\\') {
            return false;
        }
        self.jsx_source_identifiers
            .get_or_init(|| {
                let mut identifiers = wake_common::FxHashSet::default();
                let mut lexer = Lexer::new(self.source);
                loop {
                    let token = lexer.next(false);
                    match token.kind {
                        TokenKind::Ident => {
                            identifiers.insert(lexer.identifier_text(token.span).into_owned());
                        }
                        TokenKind::Eof => break,
                        _ => {}
                    }
                }
                identifiers
            })
            .contains(candidate)
    }

    /// JSX 常量名的惰性预驻留：首次调用按固定顺序驻留 props/source key 与全部候选 alias，
    /// 之后复用（Atom 为 `Copy`），省去每个 JSX 元素对固定分片的锁 + 哈希 + 查找。
    fn jsx_atoms(&self) -> crate::JsxAtoms {
        if let Some(a) = self.jsx_atoms.get() {
            return a;
        }
        let a = crate::JsxAtoms {
            children: self.interner.intern("children"),
            jsx: self.automatic_jsx_alias(AutomaticJsxBinding::Jsx),
            jsxs: self.automatic_jsx_alias(AutomaticJsxBinding::Jsxs),
            fragment: self.automatic_jsx_alias(AutomaticJsxBinding::Fragment),
            jsx_dev: self.automatic_jsx_alias(AutomaticJsxBinding::JsxDev),
            file_name: self.interner.intern("fileName"),
            line_number: self.interner.intern("lineNumber"),
            column_number: self.interner.intern("columnNumber"),
        };
        self.jsx_atoms.set(Some(a));
        a
    }

    /// JSX 入口（`parse_primary_expression` 在 jsx 模式遇到 `<` 时调用，`cur == <`）。
    /// 解析整个元素/片段并降级；结束后恢复普通词法。
    pub(crate) fn parse_jsx_root(&mut self) -> Expression<'a> {
        let el = self.parse_jsx_element_or_fragment();
        // JSX 元素是一个值：其后 `/` 视为除号（regex 关闭）。
        self.cur = self.lexer.next_at(self.prev_end, false);
        self.lookahead = None;
        el
    }

    /// 解析一个 JSX 元素或片段（`cur == <`）。返回降级后的表达式；
    /// 返回时 `self.prev_end` = 末尾 `>` 之后的字节位置，`cur` 已失效（由调用方决定后续取词方式）。
    fn parse_jsx_element_or_fragment(&mut self) -> Expression<'a> {
        let lo = self.cur.span.lo;
        let after_lt = self.cur.span.hi;
        // 取 `<` 之后的标签 token（名字，或片段的 `>`）。
        self.jsx_relex(after_lt);

        // 片段 `<> ... </>`。
        if self.at(TokenKind::Gt) {
            self.jsx_runtime_usage.insert(AutomaticJsxBinding::Fragment);
            self.prev_end = self.cur.span.hi;
            let children = self.parse_jsx_children();
            self.parse_jsx_closing();
            let frag = self.ident_expr_atom(Span::new(lo, lo), self.jsx_atoms().fragment);
            return self.build_jsx_call(lo, frag, None, self.new_vec(), children);
        }

        // 元素名 → intrinsic 字符串 或 组件标识符/成员。
        let name = self.parse_jsx_element_name();
        // TSX JSX 类型实参只参与类型检查，运行时直接擦除：`<Form<T> ... />`。
        if self.ts && self.at(TokenKind::Lt) {
            self.ts_type_arguments();
        }
        // 属性 → 对象成员 + key。
        let (members, key) = self.parse_jsx_attributes();

        if self.at(TokenKind::Slash) {
            // 自闭合 `/>`。
            let after = self.cur.span.hi;
            self.jsx_relex(after);
            if !self.at(TokenKind::Gt) {
                self.error_expected(">");
            }
            self.prev_end = self.cur.span.hi;
            return self.build_jsx_call(lo, name, key, members, self.new_vec());
        }

        // `>`：开标签结束。
        if !self.at(TokenKind::Gt) {
            self.error_expected(">");
        }
        self.prev_end = self.cur.span.hi;
        let children = self.parse_jsx_children();
        self.parse_jsx_closing();
        self.build_jsx_call(lo, name, key, members, children)
    }

    /// 元素名：连字符/成员链。小写简单名 → intrinsic 字符串字面量；否则组件标识符/成员表达式。
    fn parse_jsx_element_name(&mut self) -> Expression<'a> {
        let lo = self.cur.span.lo;
        let first = self.jsx_read_name_raw();

        if self.at(TokenKind::Dot) {
            // 成员组件 `Foo.Bar.Baz`。
            let mut expr = self.jsx_name_ident(first);
            while self.at(TokenKind::Dot) {
                let after_dot = self.cur.span.hi;
                self.jsx_relex(after_dot);
                let seg = self.jsx_read_name_raw();
                let prop = Ident::new(seg, self.intern_slice(seg));
                expr = Expression::Member(self.alloc(MemberExpression {
                    span: Span::new(lo, seg.hi),
                    object: expr,
                    property: MemberProperty::Ident(prop),
                    optional: false,
                }));
            }
            return expr;
        }

        // 命名空间名 `a:b`（SVG/XML 场景，如 `<xlink:href>`）：整体作为**字符串类型**，
        // 与 tsc 一致（`<a:b/>` → `_jsx("a:b", …)`）——冒号不是合法标识符字符，不可能是组件。
        if self.at(TokenKind::Colon) {
            let after_colon = self.cur.span.hi;
            self.jsx_relex(after_colon);
            let second = self.jsx_read_name_raw();
            let full = Span::new(first.lo, second.hi);
            return Expression::StringLiteral(self.alloc(StringLiteral {
                span: full,
                value: self.intern_slice(full),
            }));
        }

        let raw = self.slice(first);
        if is_intrinsic_name(raw) {
            Expression::StringLiteral(self.alloc(StringLiteral {
                span: first,
                value: self.intern_slice(first),
            }))
        } else {
            self.jsx_name_ident(first)
        }
    }

    fn jsx_name_ident(&self, span: Span) -> Expression<'a> {
        Expression::Identifier(self.alloc(Ident::new(span, self.intern_slice(span))))
    }

    /// 从 `cur`（名字段首 token）起，按 JSX 名字规则把连字符/后续名字符纳入同一 span，
    /// 然后把 `cur` 前移到名字之后的下一个标签 token。返回名字 span。
    fn jsx_read_name_raw(&mut self) -> Span {
        let lo = self.cur.span.lo;
        let bytes = self.source.as_bytes();
        let mut end = self.cur.span.hi as usize;
        while end < bytes.len() {
            let c = bytes[end];
            if c == b'-' || c.is_ascii_alphanumeric() || c == b'_' || c == b'$' {
                end += 1;
            } else {
                break;
            }
        }
        self.jsx_relex(end as u32);
        Span::new(lo, end as u32)
    }

    /// 属性列表 → (对象成员, key)。`cur` 停在开标签的 `/` 或 `>`。
    fn parse_jsx_attributes(
        &mut self,
    ) -> (
        wake_ecma_ast::AVec<'a, ObjectMember<'a>>,
        Option<Expression<'a>>,
    ) {
        let mut members = self.new_vec::<ObjectMember>();
        let mut key: Option<Expression<'a>> = None;
        loop {
            match self.cur.kind {
                TokenKind::Gt | TokenKind::Slash | TokenKind::Eof => break,
                TokenKind::LBrace => {
                    // 展开属性 `{...expr}`。
                    let after = self.cur.span.hi;
                    self.jsx_relex_expr(after);
                    if !self.at(TokenKind::DotDotDot) {
                        self.error_expected("...");
                        break;
                    }
                    let after_dots = self.cur.span.hi;
                    self.jsx_relex_expr(after_dots);
                    let e = self.with_allow_in(true, |p| p.parse_assignment_expression());
                    self.jsx_expect_rbrace();
                    members.push(ObjectMember::Spread(self.alloc(SpreadElement {
                        span: e.span(),
                        argument: e,
                    })));
                }
                _ => {
                    // 属性名（可含连字符）。`slice` 返回 `&'src str`（绑源码而非 `&self`），
                    // 可跨后续 `&mut self` 调用存活，无需 `to_string`。
                    let name_span = self.jsx_read_name_raw();
                    let name_raw = self.slice(name_span);

                    let value = if self.at(TokenKind::Eq) {
                        let after_eq = self.cur.span.hi;
                        self.jsx_relex(after_eq);
                        match self.cur.kind {
                            TokenKind::Str => {
                                let s_span = self.cur.span;
                                let val = self.jsx_decode_attr_string(s_span);
                                self.jsx_relex(s_span.hi);
                                Expression::StringLiteral(self.alloc(StringLiteral {
                                    span: s_span,
                                    value: val,
                                }))
                            }
                            TokenKind::LBrace => {
                                let after_lb = self.cur.span.hi;
                                self.jsx_relex_expr(after_lb);
                                let e =
                                    self.with_allow_in(true, |p| p.parse_assignment_expression());
                                self.jsx_expect_rbrace();
                                e
                            }
                            _ => {
                                self.error_expected("属性值");
                                self.jsx_true(name_span)
                            }
                        }
                    } else {
                        // 布尔简写：`<input disabled />` → `disabled: true`。
                        self.jsx_true(name_span)
                    };

                    if name_raw == "key" {
                        key = Some(value);
                    } else {
                        let key_node = self.jsx_prop_key(name_span, name_raw);
                        let computed = matches!(key_node, PropertyKey::Computed(_));
                        let prop = self.alloc(ObjectProperty {
                            span: name_span,
                            key: key_node,
                            value,
                            kind: PropertyKind::Init,
                            method: false,
                            shorthand: false,
                            computed,
                            prototype_setter: false,
                        });
                        members.push(ObjectMember::Property(prop));
                    }
                }
            }
        }
        (members, key)
    }

    /// 属性键：合法标识符用 Ident，含连字符等用字符串键（`"data-foo"`）。
    fn jsx_prop_key(&self, span: Span, raw: &str) -> PropertyKey<'a> {
        if is_plain_ident(raw) {
            PropertyKey::Ident(Ident::new(span, self.intern_str(raw)))
        } else {
            PropertyKey::String(self.alloc(StringLiteral {
                span,
                value: self.intern_str(raw),
            }))
        }
    }

    fn jsx_true(&self, span: Span) -> Expression<'a> {
        Expression::BooleanLiteral(self.alloc(wake_ecma_ast::BooleanLiteral { span, value: true }))
    }

    /// 校验 `cur == }` 并前移（用于 JSX 内嵌表达式后，按标签词法取下一 token）。
    fn jsx_expect_rbrace(&mut self) {
        if !self.at(TokenKind::RBrace) {
            self.error_expected("}");
            return;
        }
        let after = self.cur.span.hi;
        self.jsx_relex(after);
    }

    /// 解码 JSX 属性字符串（引号内原始文本 + HTML 实体，不做 JS 转义）。
    fn jsx_decode_attr_string(&self, span: Span) -> Atom {
        let inner = &self.source[(span.lo + 1) as usize..(span.hi.saturating_sub(1)) as usize];
        self.intern_str(&decode_entities(inner))
    }

    /// 子节点列表。停在闭合 `</` 处（`cur == <`）。
    fn parse_jsx_children(&mut self) -> wake_ecma_ast::AVec<'a, Expression<'a>> {
        let mut children = self.new_vec::<Expression>();
        let mut from = self.prev_end;
        loop {
            self.cur = self.lexer.next_jsx_child_token(from);
            self.lookahead = None;
            self.prev_end = from;
            match self.cur.kind {
                TokenKind::Eof => break,
                TokenKind::JsxText => {
                    if let Some(s) = clean_jsx_text(self.slice(self.cur.span)) {
                        let atom = self.intern_str(&s);
                        children.push(Expression::StringLiteral(self.alloc(StringLiteral {
                            span: self.cur.span,
                            value: atom,
                        })));
                    }
                    from = self.cur.span.hi;
                }
                TokenKind::LBrace => {
                    // `{表达式}` 子节点（`{}` / `{/*注释*/}` 跳过）。
                    let after = self.cur.span.hi;
                    self.jsx_relex_expr(after);
                    if !self.at(TokenKind::RBrace) {
                        if self.at(TokenKind::DotDotDot) {
                            let ad = self.cur.span.hi;
                            self.jsx_relex_expr(ad);
                        }
                        let e = self.with_allow_in(true, |p| p.parse_assignment_expression());
                        children.push(e);
                    }
                    if !self.at(TokenKind::RBrace) {
                        self.error_expected("}");
                        break;
                    }
                    from = self.cur.span.hi;
                }
                TokenKind::Lt => {
                    // `</` → 闭合标签，交回调用方。
                    if self.lexer.byte_at(self.cur.span.hi) == Some(b'/') {
                        break;
                    }
                    let el = self.parse_jsx_element_or_fragment();
                    children.push(el);
                    from = self.prev_end;
                }
                _ => break,
            }
        }
        children
    }

    /// 消费闭合标签 `</name>` 或 `</>`（`cur == <`，其后为 `/`）。
    fn parse_jsx_closing(&mut self) {
        if !self.at(TokenKind::Lt) {
            self.error_expected("</");
            return;
        }
        let slash_pos = self.cur.span.hi; // `/` 的位置
        self.jsx_relex(slash_pos + 1); // 跳过 `/`，取名字或 `>`
        if !self.at(TokenKind::Gt) {
            // 闭合名（可为成员链）——不校验是否匹配，直接消费。
            let _ = self.jsx_read_name_raw();
            while self.at(TokenKind::Dot) {
                let ad = self.cur.span.hi;
                self.jsx_relex(ad);
                let _ = self.jsx_read_name_raw();
            }
        }
        if !self.at(TokenKind::Gt) {
            self.error_expected(">");
        }
        self.prev_end = self.cur.span.hi;
    }

    /// 降级为 automatic runtime 调用：
    /// - children 归入 props 的 `children`（1 个直接放，≥2 个放数组用 `_jsxs`）；
    /// - `key` 作为第 3 个实参。
    fn build_jsx_call(
        &mut self,
        lo: u32,
        type_expr: Expression<'a>,
        key: Option<Expression<'a>>,
        mut members: wake_ecma_ast::AVec<'a, ObjectMember<'a>>,
        children: wake_ecma_ast::AVec<'a, Expression<'a>>,
    ) -> Expression<'a> {
        let span = self.span_to(lo);
        let use_jsxs = children.len() >= 2;

        if !children.is_empty() {
            let children_value = if children.len() == 1 {
                children[0]
            } else {
                let mut elems = self.new_vec::<Option<Expression>>();
                for c in children.iter() {
                    elems.push(Some(*c));
                }
                Expression::Array(self.alloc(ArrayExpression {
                    span,
                    elements: elems,
                }))
            };
            let prop = self.alloc(ObjectProperty {
                span,
                key: PropertyKey::Ident(Ident::new(span, self.jsx_atoms().children)),
                value: children_value,
                kind: PropertyKind::Init,
                method: false,
                shorthand: false,
                computed: false,
                prototype_setter: false,
            });
            members.push(ObjectMember::Property(prop));
        }

        let props_object = self.alloc(ObjectExpression {
            span,
            properties: members,
        });
        let props = if self.lowers(wake_ecma_transform::EcmaFeature::ObjectRestSpread)
            && props_object
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
                props_object,
            )
        } else {
            Expression::Object(props_object)
        };

        let atoms = self.jsx_atoms();
        let kind = match self.automatic_jsx_runtime() {
            AutomaticJsxRuntime::Production => AutomaticJsxCallKind::Production {
                jsx: atoms.jsx,
                jsxs: atoms.jsxs,
            },
            AutomaticJsxRuntime::Development => AutomaticJsxCallKind::Development {
                jsx_dev: atoms.jsx_dev,
                source: self.jsx_dev_source(span, lo),
            },
        };
        let (expression, binding) = lower_automatic_jsx_call(
            self.arena,
            self.interner,
            kind,
            AutomaticJsxCall {
                span,
                element: type_expr,
                props,
                key,
                static_children: use_jsxs,
            },
        );
        self.jsx_runtime_usage.insert(binding);
        expression
    }

    /// dev runtime 的第 5 参：`{ fileName, lineNumber, columnNumber }`（行列均 1 基，
    /// 列指向元素起始的 `<`——与 tsc 一致）。
    fn jsx_dev_source(&self, span: Span, lo: u32) -> Expression<'a> {
        let atoms = self.jsx_atoms();
        let (line, column) = self.line_col_1based(lo);
        let mut props = self.new_vec::<ObjectMember>();
        let mut push = |key: Atom, value: Expression<'a>| {
            props.push(ObjectMember::Property(self.alloc(ObjectProperty {
                span,
                key: PropertyKey::Ident(Ident::new(span, key)),
                value,
                kind: PropertyKind::Init,
                method: false,
                shorthand: false,
                computed: false,
                prototype_setter: false,
            })));
        };
        push(
            atoms.file_name,
            Expression::StringLiteral(self.alloc(StringLiteral {
                span,
                value: self.intern_str(self.options.file_name),
            })),
        );
        push(atoms.line_number, self.num_lit(line as f64));
        push(atoms.column_number, self.num_lit(column as f64));
        Expression::Object(self.alloc(ObjectExpression {
            span,
            properties: props,
        }))
    }

    /// 字节偏移 → (行, 列)，均 **1 基**；列按 UTF-16 code unit 计。惰性构建换行表并缓存。
    fn line_col_1based(&self, offset: u32) -> (u32, u32) {
        let starts = self.line_starts.get_or_init(|| {
            let mut v = Vec::with_capacity(self.source.len() / 32 + 1);
            v.push(0u32);
            for (i, b) in self.source.bytes().enumerate() {
                if b == b'\n' {
                    v.push(i as u32 + 1);
                }
            }
            v
        });
        let idx = starts.partition_point(|&s| s <= offset).saturating_sub(1);
        let line_start = starts[idx] as usize;
        let col = self.source[line_start..offset as usize]
            .encode_utf16()
            .count()
            + 1;
        (idx as u32 + 1, col as u32)
    }

    /// 构造并返回按需注入的 `react/jsx-runtime` import（供 `parse_program` 在用到 JSX 时插入 body[0]）。
    pub(crate) fn build_jsx_runtime_import(&self) -> (Statement<'a>, Dependency) {
        let runtime = self.automatic_jsx_runtime();
        let source = self.intern_str(&runtime.specifier(self.options.jsx_import_source));
        let atoms = self.jsx_atoms();
        let mut specs = self.new_vec::<ImportSpecifier>();
        for binding in runtime.used_imports(self.jsx_runtime_usage) {
            let local = match binding {
                AutomaticJsxBinding::Jsx => atoms.jsx,
                AutomaticJsxBinding::Jsxs => atoms.jsxs,
                AutomaticJsxBinding::Fragment => atoms.fragment,
                AutomaticJsxBinding::JsxDev => atoms.jsx_dev,
            };
            specs.push(ImportSpecifier::Named {
                span: Span::DUMMY,
                imported: ModuleExportName::Ident(Ident::new(
                    Span::DUMMY,
                    self.intern_str(binding.imported()),
                )),
                local: Ident::new(Span::DUMMY, local),
            });
        }
        let statement = Statement::Import(self.alloc(ImportDeclaration {
            span: Span::DUMMY,
            specifiers: specs,
            source,
            attributes: None,
        }));
        let dependency = Dependency {
            specifier: source,
            kind: DependencyKind::Import,
            span: Span::DUMMY,
        };
        (statement, dependency)
    }
}

/// intrinsic 元素：首字符小写 ASCII 字母（`div`、`my-element`）→ 字符串；否则组件。
fn is_intrinsic_name(raw: &str) -> bool {
    raw.as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_lowercase())
}

/// 是否是可直接作对象键的普通标识符（无连字符等）。
fn is_plain_ident(raw: &str) -> bool {
    let mut bytes = raw.bytes();
    match bytes.next() {
        Some(b) if b == b'_' || b == b'$' || b.is_ascii_alphabetic() => {}
        _ => return false,
    }
    bytes.all(|b| b == b'_' || b == b'$' || b.is_ascii_alphanumeric())
}

/// JSX 子节点文本的空白折叠（对齐 Babel `cleanJSXElementLiteralChild`）+ 实体解码。
/// 返回 `None` 表示该文本折叠后为空（如仅含换行缩进的空白）。
fn clean_jsx_text(raw: &str) -> Option<String> {
    let lines: Vec<&str> = raw.split('\n').collect();
    let mut last_nonempty = 0usize;
    let mut any = false;
    for (i, line) in lines.iter().enumerate() {
        if line.bytes().any(|b| b != b' ' && b != b'\t' && b != b'\r') {
            last_nonempty = i;
            any = true;
        }
    }
    if !any {
        // 纯空白：含换行 → 丢弃；单行空白（行内元素间）→ 保留。
        return if raw.contains('\n') {
            None
        } else {
            Some(decode_entities(raw))
        };
    }
    let n = lines.len();
    let mut result = String::new();
    for (i, line) in lines.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i == n - 1;
        let is_last_nonempty = i == last_nonempty;
        let mut trimmed = line.replace('\t', " ").replace('\r', "");
        if !is_first {
            trimmed = trimmed.trim_start_matches(' ').to_string();
        }
        if !is_last {
            trimmed = trimmed.trim_end_matches(' ').to_string();
        }
        if !trimmed.is_empty() {
            if !is_last_nonempty {
                trimmed.push(' ');
            }
            result.push_str(&trimmed);
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(decode_entities(&result))
    }
}

/// 解码 HTML 实体：五个 XML 实体 + `nbsp/copy/reg/mdash/ndash/hellip` + 数值实体（`&#..;`/`&#x..;`）。
/// 其余未知实体原样保留。
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';')
            && let Some(ch) = decode_one_entity(&after[..semi])
        {
            out.push(ch);
            rest = &after[semi + 1..];
            continue;
        }
        // 非实体：原样保留 `&`。
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn decode_one_entity(e: &str) -> Option<char> {
    match e {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        "copy" => Some('\u{a9}'),
        "reg" => Some('\u{ae}'),
        "mdash" => Some('\u{2014}'),
        "ndash" => Some('\u{2013}'),
        "hellip" => Some('\u{2026}'),
        _ => {
            if let Some(num) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
                u32::from_str_radix(num, 16).ok().and_then(char::from_u32)
            } else if let Some(num) = e.strip_prefix('#') {
                num.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                None
            }
        }
    }
}
