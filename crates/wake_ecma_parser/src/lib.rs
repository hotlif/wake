//! # wake_ecma_parser — 语法分析器
//!
//! DESIGN §4.4：手写递归下降 + Pratt 表达式解析。一遍产出 AST 与依赖列表（[`Dependency`]）。
//! cover grammar 处理箭头函数；ASI 处理自动分号；上下文用 bitflags 随递归传递。
//!
//! 入口：[`parse`]。产出 [`ParseOutput`]（自引用 [`ModuleAst`] + 依赖 + 诊断）。

mod expr;
mod jsx;
/// 兼容导出：语义分析已拆到独立 crate，parser 调用方无需立即迁移路径。
pub mod semantic {
    pub use wake_ecma_semantic::*;
}
mod stmt;
mod ts;
mod ts_value;

pub use wake_ecma_semantic::{SemanticModel, analyze};

use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};

use bumpalo::Bump;
use wake_common::{Atom, Diagnostic, FxHashMap, Interner, Span};
use wake_ecma_ast::{
    AVec, Dependency, Ident, ModuleAst, Pattern, Program, SourceType, Statement, VarKind,
    VariableDeclaration, VariableDeclarator,
};
use wake_ecma_lexer::{Keyword, Lexer, LexerCheckpoint, Token, TokenKind, regex_allowed_after};
use xxhash_rust::xxh3::{xxh3_64, xxh3_64_with_seed};

/// 解析结果。
pub struct ParseOutput {
    /// 自引用持有者（arena + AST）。
    pub module: ModuleAst,
    /// 解析时同步提取的依赖（DESIGN §4.4）。
    pub dependencies: Vec<Dependency>,
    /// 诊断（错误恢复模式下可能多条）。
    pub diagnostics: Vec<Diagnostic>,
    /// 模块**顶层**（不在任何函数/方法/箭头/`static {}` 内）出现过 `await` 或 `for await`。
    /// 打包器据此把该模块包成 `async function` 并让其导入方 `await`（DESIGN §6.1.1）。
    pub has_top_level_await: bool,
}

impl ParseOutput {
    /// 是否有错误级诊断。
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.is_error())
    }
}

/// 解析选项（JSX 运行时口径等）。默认对应 production automatic runtime。
#[derive(Clone, Copy, Debug)]
pub struct ParseOptions<'o> {
    /// JSX 运行时来源包，实际 import `<source>/jsx-runtime`（dev 下 `/jsx-dev-runtime`）。
    /// 对齐 Babel/TS 的 `jsxImportSource`；默认 `"react"`。
    pub jsx_import_source: &'o str,
    /// 使用 **dev runtime**：`jsxDEV(type, props, key, isStaticChildren, source, self)`，
    /// 额外携带 `{fileName,lineNumber,columnNumber}`，供 React DevTools 显示组件栈。
    pub jsx_dev: bool,
    /// dev runtime 的 `fileName`（源文件路径）。
    pub file_name: &'o str,
    /// 由 Browserslist 目标计算出的实际 lowering pass。
    pub transform_features: wake_ecma_transform::FeatureSet,
}

impl Default for ParseOptions<'_> {
    fn default() -> Self {
        ParseOptions {
            jsx_import_source: "react",
            jsx_dev: false,
            file_name: "",
            transform_features: wake_ecma_transform::FeatureSet::default(),
        }
    }
}

/// 解析源码为 AST + 依赖 + 诊断。`source` / `interner` 需在返回值使用期内存活。
pub fn parse(source: &str, interner: &Interner, source_type: SourceType) -> ParseOutput {
    parse_with(source, interner, source_type, ParseOptions::default())
}

/// 同 [`parse`]，但可指定 [`ParseOptions`]（JSX dev runtime / `jsxImportSource`）。
pub fn parse_with(
    source: &str,
    interner: &Interner,
    source_type: SourceType,
    options: ParseOptions<'_>,
) -> ParseOutput {
    if options.transform_features.is_empty() {
        parse_with_mode::<false>(source, interner, source_type, options)
    } else {
        parse_with_mode::<true>(source, interner, source_type, options)
    }
}

/// `LOWER=false` 是现代目标的整条专用路径；编译器会移除所有 lowering 分支。
fn parse_with_mode<const LOWER: bool>(
    source: &str,
    interner: &Interner,
    source_type: SourceType,
    options: ParseOptions<'_>,
) -> ParseOutput {
    let deps = RefCell::new(Vec::new());
    let diags = RefCell::new(Vec::new());
    let tla = Cell::new(false);

    // 源码指纹比构建后重新遍历 AST 更便宜；seed 覆盖所有会改变解析结果的配置输入。
    let source_hash = parse_fingerprint(source, source_type, options);
    let module = ModuleAst::from_builder_prehashed(source_hash, |arena| {
        let mut parser = Parser::<LOWER>::new(source, interner, arena, source_type, options);
        let program = parser.parse_program();
        *deps.borrow_mut() = std::mem::take(&mut parser.dependencies);
        *diags.borrow_mut() = std::mem::take(&mut parser.diagnostics);
        tla.set(parser.has_top_level_await);
        program
    });

    ParseOutput {
        module,
        dependencies: deps.into_inner(),
        diagnostics: diags.into_inner(),
        has_top_level_await: tla.get(),
    }
}

#[inline]
fn parse_fingerprint(source: &str, source_type: SourceType, options: ParseOptions<'_>) -> u64 {
    let source_type_seed = match source_type {
        SourceType::Module => 1_u64,
        SourceType::Script => 2,
        SourceType::TypeScript => 3,
        SourceType::Tsx => 4,
        SourceType::Jsx => 5,
    };
    let mut seed = source_type_seed
        ^ options.transform_features.bits().rotate_left(7)
        ^ xxh3_64(options.jsx_import_source.as_bytes()).rotate_left(23);
    if options.jsx_dev {
        seed ^= 0x9e37_79b9_7f4a_7c15;
        seed ^= xxh3_64(options.file_name.as_bytes()).rotate_left(41);
    }
    xxh3_64_with_seed(source.as_bytes(), seed)
}

/// 上下文标志，随递归传递并在特定语法位保存/恢复（DESIGN §4.4）。
#[derive(Clone, Copy)]
struct Context {
    /// 处于 async 函数体（`await` 是运算符）。模块顶层同样为真——顶层 await（ES2022）。
    in_async: bool,
    /// 处于 generator 函数体（`yield` 是运算符）。
    in_generator: bool,
    /// 仍在模块**顶层**（未进入任何函数/方法/箭头/`static {}` 体）。
    /// 与 `in_async` 一起区分「顶层 await」与「async 函数内 await」。
    top_level: bool,
    /// `in` 运算符是否允许（for-init 里禁止，避免与 for-in 歧义）。
    allow_in: bool,
    /// 严格模式。
    strict: bool,
}

impl Default for Context {
    fn default() -> Context {
        Context {
            in_async: false,
            in_generator: false,
            top_level: true,
            allow_in: true,
            strict: false,
        }
    }
}

/// parser + lexer 的位置快照，供试探解析回溯（见 [`Parser::checkpoint`]）。
struct ParserCheckpoint {
    cur: Token,
    lookahead: Option<Token>,
    prev_end: u32,
    lex: LexerCheckpoint,
    /// 回溯时丢弃试探期间累积的 parser 诊断（如类型实参试探失败）。
    diag_len: usize,
}

/// JSX 降级要反复驻留的编译期常量名的预驻留 Atom（见 [`Parser::jsx_atoms`]）。
#[derive(Clone, Copy)]
pub(crate) struct JsxAtoms {
    pub children: Atom,
    pub jsx: Atom,
    pub jsxs: Atom,
    pub fragment: Atom,
    /// dev runtime 的调用名 `_jsxDEV`（仅 `jsx_dev` 下使用）。
    pub jsx_dev: Atom,
    pub file_name: Atom,
    pub line_number: Atom,
    pub column_number: Atom,
}

pub(crate) struct Parser<'a, 'src, const LOWER: bool> {
    source: &'src str,
    lexer: Lexer<'src>,
    interner: &'src Interner,
    arena: &'a Bump,
    source_type: SourceType,

    /// 当前 token。
    cur: Token,
    /// 前瞻缓冲（1 token）。
    lookahead: Option<Token>,
    /// 上一个已消费 token 的结束偏移（span 计算用）。
    prev_end: u32,

    ctx: Context,
    /// TypeScript 模式：在类型位置跳过类型语法（DESIGN §4.1）。
    ts: bool,
    /// JSX 模式：表达式起始处的 `<` 解析为 JSX 元素（DESIGN §4.3）。
    jsx: bool,
    /// 本模块是否用到 JSX（用于按需注入 `react/jsx-runtime` 的 import）。
    used_jsx: bool,
    /// 解析选项（JSX 运行时口径）。
    options: ParseOptions<'src>,
    /// 换行偏移表，惰性构建——仅 JSX dev runtime 需要把 span 换算成行列。
    line_starts: OnceCell<Vec<u32>>,
    /// 本模块顶层是否出现过 `await` / `for await`（见 [`ParseOutput::has_top_level_await`]）。
    has_top_level_await: bool,
    diagnostics: Vec<Diagnostic>,
    dependencies: Vec<Dependency>,

    /// 标识符驻留缓存：源码切片 → Atom。真实代码里标识符高频重复，命中即跳过全局
    /// interner 的锁 + 二次哈希，大幅提升单核解析吞吐。`RefCell` 保持 `&self` 接口。
    ident_cache: RefCell<FxHashMap<&'src str, Atom>>,
    /// 预驻留的 `"require"` Atom：`require(...)` 依赖检测用 `id.name == require_atom`（u32 比较）
    /// 取代每次调用 `with_resolved` 锁分片 + 字符串比较。
    require_atom: Atom,
    /// JSX 常量名（`children`/`_jsx`/`_jsxs`/`_Fragment`）的惰性预驻留：非 JSX 模块永不触发，
    /// JSX 模块首个元素驻留一次后复用，省去每元素对固定分片的锁 + 哈希 + 查找。
    jsx_atoms: Cell<Option<JsxAtoms>>,
    /// 为 lowering IIFE 生成与源码标识符不冲突的局部参数名。
    transform_temp: u32,
    /// 正在解析的可注入 `var` 的词法作用域。`None` 是参数/class 等保守区，禁止把
    /// 临时绑定泄漏到外层；`Some` 收集该 Program/函数/同步箭头自身使用的名字。
    transform_temp_scopes: Vec<Option<Vec<Atom>>>,
    /// 首次 spread lowering 时分配；最终写入 Program 供 codegen 注入 iterator helper。
    spread_helper: Option<Atom>,
    object_spread_helper: Option<Atom>,
    /// Synchronous for-of lowering 的 collision-free per-module 状态 helper。
    for_of_helper: Option<Atom>,
    /// cover grammar 内的数组/对象可能稍后重解释为箭头参数，暂缓 spread lowering。
    in_cover_paren: bool,
    /// 单项 cover 括号里因没有安全 temp scope 而保留了 optional chain。外层 lhs tail
    /// 只消费一次此标记，避免离开 cover 后又把同一条链错误地注入到父作用域。
    preserve_optional_chain_tail: bool,
    /// `delete (chain?.member)` 必须等括号边界确定后才能决定是删除 member，还是先求出
    /// chain 的值再删除括号外的后缀。此标记让 cover 内先保留原 optional AST。
    suppress_optional_chain_in_delete_cover: bool,
    /// 正在解析 `for (...)` 的非声明初始化项。数组/对象后紧跟顶层 `in` / `of` 时，
    /// spread 形态是赋值 rest，而不是表达式 spread。
    in_for_head_init: bool,
}

impl<'a, 'src, const LOWER: bool> Parser<'a, 'src, LOWER> {
    fn new(
        source: &'src str,
        interner: &'src Interner,
        arena: &'a Bump,
        source_type: SourceType,
        options: ParseOptions<'src>,
    ) -> Parser<'a, 'src, LOWER> {
        let mut lexer = Lexer::new(source);
        // 表达式起始位置：首 token 允许正则。
        let cur = lexer.next(true);
        // 模块顶层即 async 上下文：`await` 是运算符（ES2022 顶层 await）。Script 保持旧语义。
        let ctx = Context {
            strict: source_type.is_module(),
            in_async: source_type.is_module(),
            ..Context::default()
        };
        Parser {
            source,
            options,
            line_starts: OnceCell::new(),
            lexer,
            interner,
            arena,
            source_type,
            cur,
            lookahead: None,
            prev_end: 0,
            ctx,
            ts: source_type.is_typescript(),
            jsx: source_type.is_jsx(),
            used_jsx: false,
            has_top_level_await: false,
            diagnostics: Vec::new(),
            dependencies: Vec::new(),
            ident_cache: RefCell::new(FxHashMap::default()),
            require_atom: interner.intern("require"),
            jsx_atoms: Cell::new(None),
            transform_temp: 0,
            transform_temp_scopes: Vec::new(),
            spread_helper: None,
            object_spread_helper: None,
            for_of_helper: None,
            in_cover_paren: false,
            preserve_optional_chain_tail: false,
            suppress_optional_chain_in_delete_cover: false,
            in_for_head_init: false,
        }
    }

    #[inline(always)]
    fn lowers(&self, feature: wake_ecma_transform::EcmaFeature) -> bool {
        LOWER && self.options.transform_features.contains(feature)
    }

    fn fresh_transform_atom(&mut self) -> Atom {
        loop {
            let candidate = format!("__wake_t{}", self.transform_temp);
            self.transform_temp += 1;
            // 标识符文本完全不出现在源码中，必然不会捕获用户绑定或自由引用。
            if !self.source.contains(&candidate) {
                return self.interner.intern(&candidate);
            }
        }
    }

    fn push_transform_temp_scope(&mut self, enabled: bool) {
        if !LOWER {
            return;
        }
        self.transform_temp_scopes
            .push(enabled.then(Vec::<Atom>::new));
    }

    fn pop_transform_temp_scope(&mut self) -> Vec<Atom> {
        if !LOWER {
            return Vec::new();
        }
        self.transform_temp_scopes
            .pop()
            .expect("transform temp scopes are balanced")
            .unwrap_or_default()
    }

    /// 分配并登记一个属于当前 Program/函数/同步箭头的 `var` 临时名。
    /// cover grammar 和显式禁用区返回 `None`，调用方必须保留原语法。
    fn fresh_scoped_transform_atom(&mut self) -> Option<Atom> {
        if self.in_cover_paren || !matches!(self.transform_temp_scopes.last(), Some(Some(_))) {
            return None;
        }
        let atom = self.fresh_transform_atom();
        self.transform_temp_scopes
            .last_mut()
            .and_then(Option::as_mut)
            .expect("enabled transform temp scope exists")
            .push(atom);
        Some(atom)
    }

    fn has_scoped_transform_temp_scope(&self) -> bool {
        !self.in_cover_paren && matches!(self.transform_temp_scopes.last(), Some(Some(_)))
    }

    pub(crate) fn transform_temp_declaration(&self, temps: &[Atom]) -> Option<Statement<'a>> {
        if temps.is_empty() {
            return None;
        }
        let mut declarations = self.new_vec::<VariableDeclarator>();
        declarations.extend(temps.iter().copied().map(|atom| VariableDeclarator {
            span: Span::DUMMY,
            id: Pattern::Ident(self.alloc(Ident::new(Span::DUMMY, atom))),
            init: None,
        }));
        Some(Statement::VariableDeclaration(self.alloc(
            VariableDeclaration {
                span: Span::DUMMY,
                kind: VarKind::Var,
                declarations,
            },
        )))
    }

    pub(crate) fn inject_transform_temp_declaration(
        &self,
        mut statements: AVec<'a, Statement<'a>>,
        temps: &[Atom],
    ) -> AVec<'a, Statement<'a>> {
        let Some(declaration) = self.transform_temp_declaration(temps) else {
            return statements;
        };
        let directive_count = statements
            .iter()
            .take_while(|statement| {
                matches!(
                    statement,
                    Statement::Expression(expression)
                        if matches!(expression.expression, wake_ecma_ast::Expression::StringLiteral(_))
                )
            })
            .count();
        statements.insert(directive_count, declaration);
        statements
    }

    fn spread_helper_atom(&mut self) -> Atom {
        if let Some(atom) = self.spread_helper {
            return atom;
        }
        let atom = self.fresh_transform_atom();
        self.spread_helper = Some(atom);
        atom
    }

    fn object_spread_helper_atom(&mut self) -> Atom {
        if let Some(atom) = self.object_spread_helper {
            return atom;
        }
        let atom = self.fresh_transform_atom();
        self.object_spread_helper = Some(atom);
        atom
    }

    /// Allocate the synchronous for-of state helper lazily.
    fn for_of_helper_atom(&mut self) -> Atom {
        if let Some(atom) = self.for_of_helper {
            return atom;
        }
        let atom = self.fresh_transform_atom();
        self.for_of_helper = Some(atom);
        atom
    }

    // ==================================================================
    // token 管理
    // ==================================================================

    #[inline]
    fn at(&self, kind: TokenKind) -> bool {
        self.cur.kind == kind
    }

    #[inline]
    fn at_keyword(&self, kw: Keyword) -> bool {
        self.cur.kind == TokenKind::Keyword(kw)
    }

    /// 当前是否是标识符或「可作标识符的上下文关键字」（如 `async`/`of`/`let`）。
    fn at_ident_name(&self) -> bool {
        match self.cur.kind {
            TokenKind::Ident => true,
            TokenKind::Keyword(kw) => !kw.is_reserved(),
            _ => false,
        }
    }

    #[inline]
    fn newline_before(&self) -> bool {
        self.cur.newline_before
    }

    /// 前进一个 token，返回被消费的（旧 cur）。regex/div 上下文用启发式（见 lexer）。
    fn bump(&mut self) -> Token {
        let prev = self.cur;
        self.prev_end = prev.span.hi;
        self.cur = match self.lookahead.take() {
            Some(t) => t,
            None => self.lexer.next(regex_allowed_after(Some(prev.kind))),
        };
        prev
    }

    /// 前瞻下一个 token（不消费）。
    fn peek(&mut self) -> Token {
        if self.lookahead.is_none() {
            let allowed = regex_allowed_after(Some(self.cur.kind));
            self.lookahead = Some(self.lexer.next(allowed));
        }
        self.lookahead.unwrap()
    }

    /// 记录 parser + lexer 的完整位置，供试探解析（如 TS 调用类型实参 `f<T>()`）失败后回溯。
    fn checkpoint(&self) -> ParserCheckpoint {
        ParserCheckpoint {
            cur: self.cur,
            lookahead: self.lookahead,
            prev_end: self.prev_end,
            lex: self.lexer.checkpoint(),
            diag_len: self.diagnostics.len(),
        }
    }

    /// 回退到 `checkpoint` 记录的位置（含丢弃试探期间的 parser 诊断）。
    fn rewind(&mut self, cp: ParserCheckpoint) {
        self.cur = cp.cur;
        self.lookahead = cp.lookahead;
        self.prev_end = cp.prev_end;
        self.lexer.rewind(cp.lex);
        self.diagnostics.truncate(cp.diag_len);
    }

    /// 若当前 token 是 `kind` 则消费并返回 true。
    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_keyword(&mut self, kw: Keyword) -> bool {
        self.eat(TokenKind::Keyword(kw))
    }

    /// 期望某 token；不匹配则报错（不消费，交由错误恢复）。
    fn expect(&mut self, kind: TokenKind) {
        if self.at(kind) {
            self.bump();
        } else {
            self.error_expected(kind.describe());
        }
    }

    // ==================================================================
    // span / 分配 / 驻留
    // ==================================================================

    #[inline]
    fn start(&self) -> u32 {
        self.cur.span.lo
    }

    /// 从 `lo` 到上一个已消费 token 结束的 span。
    #[inline]
    fn span_to(&self, lo: u32) -> Span {
        Span::new(lo, self.prev_end)
    }

    #[inline]
    fn alloc<T>(&self, value: T) -> &'a T {
        self.arena.alloc(value)
    }

    #[inline]
    fn new_vec<T>(&self) -> AVec<'a, T> {
        AVec::new_in(self.arena)
    }

    /// 驻留指定 span 的标识符文本（含 `\u` 转义解码）。无转义时经本地缓存跳过全局 interner。
    fn intern_ident(&self, span: Span) -> Atom {
        match self.lexer.identifier_text(span) {
            Cow::Borrowed(s) => {
                if let Some(&atom) = self.ident_cache.borrow().get(s) {
                    return atom;
                }
                let atom = self.interner.intern(s);
                self.ident_cache.borrow_mut().insert(s, atom);
                atom
            }
            Cow::Owned(s) => self.interner.intern(&s),
        }
    }

    /// 驻留一个源码切片。
    fn intern_slice(&self, span: Span) -> Atom {
        self.interner
            .intern(&self.source[span.lo as usize..span.hi as usize])
    }

    /// 取源码切片。
    fn slice(&self, span: Span) -> &'src str {
        &self.source[span.lo as usize..span.hi as usize]
    }

    // ==================================================================
    // 诊断 / 错误恢复
    // ==================================================================

    fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.diagnostics.push(
            Diagnostic::error(msg)
                .with_code("WAKE0200")
                .with_primary(span, "此处"),
        );
    }

    fn error_expected(&mut self, what: &str) {
        let found = self.cur.kind.describe();
        let span = self.cur.span;
        self.error(span, format!("期望 {what}，但遇到 `{found}`"));
    }

    fn record_dependency(&mut self, dep: Dependency) {
        self.dependencies.push(dep);
    }

    // ==================================================================
    // 顶层
    // ==================================================================

    fn parse_program(&mut self) -> Program<'a> {
        let mut program = Program::new_in(self.arena, self.source_type);
        let lo = self.start();
        self.push_transform_temp_scope(true);

        // 指令序言（"use strict"）：脚本据此进入严格模式。
        let strict_directive = self.parse_directive_prologue(&mut program.body);
        if strict_directive {
            self.ctx.strict = true;
            program.strict = true;
        }

        while !self.at(TokenKind::Eof) {
            let before = self.cur.span.lo;
            let stmt = self.parse_statement();
            program.body.push(stmt);
            // 错误恢复：若一轮没有推进，强制吃一个 token 防死循环。
            if self.cur.span.lo == before && !self.at(TokenKind::Eof) {
                self.bump();
            }
        }

        let transform_temps = self.pop_transform_temp_scope();
        program.body = self.inject_transform_temp_declaration(program.body, &transform_temps);

        // 用到 JSX：在模块顶部注入 automatic runtime 的 import，并记录依赖（DESIGN §4.3）。
        // 降级产出的 `_jsx`/`_jsxs`/`_Fragment` 由此 import 绑定；codegen 与依赖扇出全走现有机制。
        if self.used_jsx {
            let import = self.build_jsx_runtime_import();
            // React/Next.js 等工具把 `"use client"` / `"use server"` 当真正的 Directive
            // Prologue。运行时 import 必须位于完整的连续字符串指令之后，否则会悄悄改变
            // 文件语义。transform temp 声明也在指令之后，因此 import 会自然排在它之前。
            let directive_count = program
                .body
                .iter()
                .take_while(|statement| {
                    matches!(
                        statement,
                        Statement::Expression(expression)
                            if matches!(
                                expression.expression,
                                wake_ecma_ast::Expression::StringLiteral(_)
                            )
                    )
                })
                .count();
            program.body.insert(directive_count, import);
            self.record_jsx_runtime_dependency();
        }

        program.spread_helper = self.spread_helper;
        program.object_spread_helper = self.object_spread_helper;
        program.for_of_helper = self.for_of_helper;
        program.span = self.span_to(lo);
        let lex_diags = self.lexer.take_diagnostics();
        self.diagnostics.extend(lex_diags);
        program
    }

    /// 解析开头的字符串指令序言，返回是否含 `"use strict"`。
    fn parse_directive_prologue(&mut self, body: &mut AVec<'a, Statement<'a>>) -> bool {
        let mut has_use_strict = false;
        while self.at(TokenKind::Str) {
            // 指令的原始文本（含引号）。
            let raw = self.slice(self.cur.span);
            let inner = &raw[1..raw.len().saturating_sub(1)];
            if inner == "use strict" {
                has_use_strict = true;
            }
            let stmt = self.parse_statement();
            body.push(stmt);
        }
        has_use_strict
    }

    /// 在保存/恢复 `allow_in` 下执行 `f`。
    fn with_allow_in<R>(&mut self, allow: bool, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.ctx.allow_in;
        self.ctx.allow_in = allow;
        let r = f(self);
        self.ctx.allow_in = saved;
        r
    }
}

#[cfg(test)]
mod semantic_tests;
#[cfg(test)]
mod tests;
