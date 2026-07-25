//! # wake_ecma_parser — 语法分析器
//!
//! DESIGN §4.4：手写递归下降 + Pratt 表达式解析。一遍产出 AST 与依赖列表（[`Dependency`]）。
//! cover grammar 处理箭头函数；ASI 处理自动分号；上下文用 bitflags 随递归传递。
//!
//! 入口：[`parse`]。产出 [`ParseOutput`]（自引用 [`ModuleAst`] + 依赖 + 诊断）。

mod expr;
mod jsx;
pub mod semantic;
mod stmt;
mod ts;
mod ts_value;

pub use semantic::{SemanticModel, analyze};

use std::borrow::Cow;
use std::cell::{Cell, RefCell};

use bumpalo::Bump;
use wake_common::{Atom, Diagnostic, FxHashMap, Interner, Span};
use wake_ecma_ast::{AVec, Dependency, ModuleAst, Program, SourceType, Statement};
use wake_ecma_lexer::{Keyword, Lexer, LexerCheckpoint, Token, TokenKind, regex_allowed_after};

/// 解析结果。
pub struct ParseOutput {
    /// 自引用持有者（arena + AST）。
    pub module: ModuleAst,
    /// 解析时同步提取的依赖（DESIGN §4.4）。
    pub dependencies: Vec<Dependency>,
    /// 诊断（错误恢复模式下可能多条）。
    pub diagnostics: Vec<Diagnostic>,
}

impl ParseOutput {
    /// 是否有错误级诊断。
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.is_error())
    }
}

/// 解析源码为 AST + 依赖 + 诊断。`source` / `interner` 需在返回值使用期内存活。
pub fn parse(source: &str, interner: &Interner, source_type: SourceType) -> ParseOutput {
    let deps = RefCell::new(Vec::new());
    let diags = RefCell::new(Vec::new());

    let module = ModuleAst::from_builder(|arena| {
        let mut parser = Parser::new(source, interner, arena, source_type);
        let program = parser.parse_program();
        *deps.borrow_mut() = std::mem::take(&mut parser.dependencies);
        *diags.borrow_mut() = std::mem::take(&mut parser.diagnostics);
        program
    });

    ParseOutput {
        module,
        dependencies: deps.into_inner(),
        diagnostics: diags.into_inner(),
    }
}

/// 上下文标志，随递归传递并在特定语法位保存/恢复（DESIGN §4.4）。
#[derive(Clone, Copy)]
struct Context {
    /// 处于 async 函数体（`await` 是运算符）。
    in_async: bool,
    /// 处于 generator 函数体（`yield` 是运算符）。
    in_generator: bool,
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
}

pub(crate) struct Parser<'a, 'src> {
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
}

impl<'a, 'src> Parser<'a, 'src> {
    fn new(
        source: &'src str,
        interner: &'src Interner,
        arena: &'a Bump,
        source_type: SourceType,
    ) -> Parser<'a, 'src> {
        let mut lexer = Lexer::new(source);
        // 表达式起始位置：首 token 允许正则。
        let cur = lexer.next(true);
        let ctx = Context {
            strict: source_type.is_module(),
            ..Context::default()
        };
        Parser {
            source,
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
            diagnostics: Vec::new(),
            dependencies: Vec::new(),
            ident_cache: RefCell::new(FxHashMap::default()),
            require_atom: interner.intern("require"),
            jsx_atoms: Cell::new(None),
        }
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

        // 用到 JSX：在模块顶部注入 automatic runtime 的 import，并记录依赖（DESIGN §4.3）。
        // 降级产出的 `_jsx`/`_jsxs`/`_Fragment` 由此 import 绑定；codegen 与依赖扇出全走现有机制。
        if self.used_jsx {
            let import = self.build_jsx_runtime_import();
            program.body.insert(0, import);
            self.record_jsx_runtime_dependency();
        }

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
mod tests;
