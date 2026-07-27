//! # wake_ecma_codegen — 代码生成（AST → JS 字符串）
//!
//! DESIGN §4.6：直接从 AST 写字符串，维护运算符优先级/结合性表自动补括号。Phase 3 先做无 sourcemap
//! 的可读输出；恒等旁路、VLQ sourcemap、prod 紧凑模式是 Phase 4 的增量。
//!
//! 入口：[`codegen`]（默认 dev 可读风格）。往返 `parse → codegen → parse` 语义等价（见测试）。

use std::fmt::Write as _;

use wake_common::{Atom, FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::*;
use wake_ecma_minify::{IfReturnCandidate, MinifyCtx, write_number_minified};

/// 编译期常量替换（DESIGN §4.4 的最小切片）：静态成员访问链 → 字面量源码。
/// [`codegen_module`] 默认应用，使 React 等库的 `process.env.NODE_ENV` 在浏览器无需
/// `process` shim（生产口径 → `"production"`），并为后续死分支剪枝创造条件。
const DEFAULT_DEFINE: &[(&str, &str)] = &[("process.env.NODE_ENV", "\"production\"")];

/// 预驻留 define 各键的**叶名**（`a.b.c` → `c`）为 Atom，供 [`Codegen::match_define`] 零分配快速否决。
/// 叶名若模块未用到，此处 intern 只多一条 Atom；用到则命中已存在的 Atom。
fn define_leaf_atoms(define: &[(&str, &str)], interner: &Interner) -> Vec<Atom> {
    define
        .iter()
        .map(|(k, _)| interner.intern(k.rsplit('.').next().unwrap_or(k)))
        .collect()
}

/// 把一个 Program 生成为 JS 源码字符串（保留 ESM，不链接）。
pub fn codegen(program: &Program, interner: &Interner) -> String {
    let mut cg = Codegen {
        out: String::new(),
        interner,
        indent: 0,
        linker: None,
        link_tmp: 0,
        define: &[],
        define_leaves: Vec::new(),
        shake: None,
        minify: false,
        rename: None,
        prop_rename: None,
        minify_ctx: None,
        no_esmodule: false,
        minify_names: false,
    };
    cg.emit_program(program);
    cg.out
}

/// 链接器：把模块说明符映射到内部模块 id（`__wake_require__` 的实参）。
///
/// 用于 [`codegen_module`]：把 ESM import/export 与 `import()`/`require()` 改写为 CJS，
/// 供 webpack 式函数包装打包（DESIGN §6.1）。
pub trait ModuleLinker {
    /// 说明符 → 内部模块 id；`None` 表示外部/未解析（MVP 保留原样）。
    fn module_id(&self, specifier: &str) -> Option<u32>;
    /// runtime require 函数名。
    fn require_fn(&self) -> &str {
        "__wake_require__"
    }
    /// 动态 `import(specifier)` 目标所属的 async/shared chunk id（代码分割，6.5）。
    /// `None` = 目标在入口闭包内 / 未启用分割 → 走旧内联（`Promise.resolve(require(id))`）。
    fn dynamic_chunk(&self, _specifier: &str) -> Option<u32> {
        None
    }
    /// 目标模块是否为 **async 模块**（自身含顶层 await，或静态导入了这类模块）。
    ///
    /// 为真时它的包装器是 `async function`，`__wake_require__(id)` 返回 Promise，
    /// 故**静态导入点**（`import` / `export ... from`）需写成 `(await __wake_require__(id))`。
    /// 由打包器在全图算出 async 子图后经 linker 传入（DESIGN §6.1.1）。
    fn is_async_module(&self, _id: u32) -> bool {
        false
    }
}

/// 生成 **已链接**（ESM→CJS）的模块体，供函数包装打包。
pub fn codegen_module(program: &Program, interner: &Interner, linker: &dyn ModuleLinker, minify_names: bool) -> String {
    codegen_module_shaken(program, interner, linker, None, minify_names)
}

/// 同 [`codegen_module`]，但可选启用 **Tree Shaking**（PLAN §6.6）。
///
/// `keep_exports`：
/// - `None` → 不 shake，保留全部导出（入口模块 / 被 `import *`·动态 import·require 整体使用的模块）；
/// - `Some(names)` → 只保留 `names` 中的导出（其余「未用 + 模块内未引用 + 无副作用」的导出声明移除，
///   否则仅移除其 `exports.x = ...` 绑定行）。`names` 里 `"default"` 表示默认导出。
pub fn codegen_module_shaken(
    program: &Program,
    interner: &Interner,
    linker: &dyn ModuleLinker,
    keep_exports: Option<&[String]>,
    minify_names: bool,
) -> String {
    codegen_module_shaken_with(
        program,
        interner,
        linker,
        keep_exports,
        DEFAULT_DEFINE,
        false,
        false,
        minify_names,
    )
}

/// 同 [`codegen_module_shaken`]，但可传入自定义 **define 表**（编译期常量替换）。
///
/// crustify 用 webpack `mode`/`DefinePlugin` 决定 `process.env.NODE_ENV` 等常量；wake 由此接入：
/// prod 传 `[("process.env.NODE_ENV", "\"production\"")]` + 用户 `[define]`，dev 传 `"development"`
/// （CRUSTIFY-PARITY §M3）。`define` 的每项为「静态成员链 → 字面量**源码**」（值含引号自便）。
pub fn codegen_module_shaken_with(
    program: &Program,
    interner: &Interner,
    linker: &dyn ModuleLinker,
    keep_exports: Option<&[String]>,
    define: &[(&str, &str)],
    minify: bool,
    no_esmodule: bool,
    minify_names: bool,
) -> String {
    codegen_module_shaken_mangled(
        program,
        interner,
        linker,
        keep_exports,
        define,
        minify,
        None,
        None,
        no_esmodule,
        minify_names,
    )
}

/// 同 [`codegen_module_shaken_with`]，但可传入 **mangling 侧表**（`span → 新名`）做标识符重命名，
/// 以及 `no_esmodule` 标记在单包模式下省略 `__esModule` 定义（bundler 静态处理 interop）。
///
/// `rename` 由 `wake_ecma_minify::plan_mangle` 构建（作用域安全、只重命名非模块作用域局部）。codegen
/// 属编译核心（DESIGN §14.1，只依赖 `wake_common`/`wake_ecma_ast`），故语义分析在外部完成、映射传入；
/// 此处仅在标识符发射点按 span 查表替换，并对**对象字面量/解构 shorthand** 在被重命名时展开为
/// `key: value` 以免改变属性名（CRUSTIFY-PARITY §M4）。`None` = 不重命名。
pub fn codegen_module_shaken_mangled(
    program: &Program,
    interner: &Interner,
    linker: &dyn ModuleLinker,
    keep_exports: Option<&[String]>,
    define: &[(&str, &str)],
    minify: bool,
    rename: Option<&FxHashMap<Span, Atom>>,
    minify_ctx: Option<&MinifyCtx>,
    no_esmodule: bool,
    minify_names: bool,
) -> String {
    let shake = keep_exports.map(|keep| ShakeCtx {
        // 外部已用导出名预驻留为 Atom（与导出名 Atom 同 interner，u32 相等 ⟺ 字符串相等）。
        used: keep.iter().map(|s| interner.intern(s)).collect(),
        internal_reads: collect_reads(program),
        default_atom: interner.intern("default"),
    });
    let prop_rename = minify_ctx.and_then(|ctx| ctx.prop_rename);
    let mut cg = Codegen {
        out: String::new(),
        interner,
        indent: 0,
        linker: Some(linker),
        link_tmp: 0,
        define,
        define_leaves: define_leaf_atoms(define, interner),
        shake,
        minify,
        rename,
        prop_rename,
        minify_ctx,
        no_esmodule,
        minify_names,
    };
    // ESM 模块（含 import/export 语法）标记 `__esModule`，供默认导入 interop 区分「转译 ESM」
    // 与「纯 CJS」。纯 CJS 模块（只有 `module.exports`/`require`）不标记，保持整体 exports 语义。
    // 单包模式下 `no_esmodule` 为 true 时省略此标记（bundler 静态处理 interop，见 emit）。
    if program_is_esm(program) && !cg.no_esmodule {
        cg.push("Object.defineProperty(exports, \"__esModule\", { value: true });");
        cg.newline();
    }
    cg.emit_program(program);
    cg.out
}

/// Tree Shaking 上下文：外部已用导出名 + 模块内被读取的标识符名（全部用 `Atom`，u32 比较，无分配）。
struct ShakeCtx {
    /// 外部（其它模块）真正 import 的导出名 Atom（含 `"default"`，见 [`ShakeCtx::default_atom`]）。
    used: FxHashSet<Atom>,
    /// 本模块内作为**读取**出现的标识符名 Atom（判断某导出声明是否还被内部引用）。
    internal_reads: FxHashSet<Atom>,
    /// 预驻留的 `"default"` Atom（默认导出的 used 判定用）。
    default_atom: Atom,
}

impl ShakeCtx {
    /// 某导出名是否应保留（外部已用）。
    fn is_used(&self, name: Atom) -> bool {
        self.used.contains(&name)
    }
    /// 某声明名是否被模块内部引用（读取）。
    fn is_read(&self, name: Atom) -> bool {
        self.internal_reads.contains(&name)
    }
    /// 从 internal_reads 中移除指定 atom。
    fn remove_read(&mut self, name: Atom) {
        self.internal_reads.remove(&name);
    }
}

/// 模块是否含 ESM 语法（据此决定是否标记 `__esModule`）。
fn program_is_esm(program: &Program) -> bool {
    program.body.iter().any(|s| {
        matches!(
            s,
            Statement::Import(_)
                | Statement::ExportNamed(_)
                | Statement::ExportDefault(_)
                | Statement::ExportAll(_)
        )
    })
}

struct Codegen<'i, 'l, 'd, 'm, 'mc> {
    out: String,
    interner: &'i Interner,
    indent: usize,
    linker: Option<&'l dyn ModuleLinker>,
    /// 链接时生成临时变量的计数器。
    link_tmp: u32,
    /// 编译期常量替换表（静态成员链 → 字面量源码）。见 [`DEFAULT_DEFINE`]。
    /// 借用自调用方（可为 [`DEFAULT_DEFINE`] 或运行期装配的 dev/用户 define），生命周期 `'d`
    /// 独立于 `&self`——`match_define` 返回的字面量须比 `&mut self`（`push`）活得久。
    define: &'d [(&'d str, &'d str)],
    /// [`define`] 各键叶名的预驻留 Atom（与 `define` 同序），用于零分配快速否决。
    define_leaves: Vec<Atom>,
    /// Tree Shaking 上下文（`None` = 不 shake）。见 [`codegen_module_shaken`]。
    shake: Option<ShakeCtx>,
    /// 紧凑（minify）输出：换行/缩进省略（语句均发显式 `;`/`}`，ASI 安全）。CRUSTIFY-PARITY §M4a。
    minify: bool,
    /// 跳过 `__esModule` 定义（用于单包模式，bundler 静态处理 interop）。
    no_esmodule: bool,
    /// 标识符 mangling 侧表（`span → 新名`，`None` = 不重命名）。由 `wake_ecma_minify::plan_mangle`
    /// 构建、经调用方传入（codegen 属编译核心，不能反向依赖 parser 的语义分析）。CRUSTIFY-PARITY §M4。
    /// 只在**变量引用/绑定**发射点（[`Codegen::push_ident`]）按 span 查表；属性名/成员名/导出名不查。
    rename: Option<&'m FxHashMap<Span, Atom>>,
    /// Property mangling side-table (span → new name).
    /// Built by `plan_prop_mangle`, consumed to shorten property names in
    /// member access expressions and object literal keys.
    prop_rename: Option<&'mc FxHashMap<Span, Atom>>,
    /// 来自 minifier 的分析上下文（常量折叠、纯性标注等）。`None` = 不使用 minify 引擎。
    minify_ctx: Option<&'mc MinifyCtx<'mc>>,
    /// Use short names for codegen output (e for exports, etc.) when true.
    minify_names: bool,
}

// 表达式优先级（越大绑定越紧）。用于自动补括号。
const P_SEQUENCE: u8 = 1;
const P_ASSIGN: u8 = 2; // 赋值 / 箭头 / yield
const P_CONDITIONAL: u8 = 3;
const P_COALESCE: u8 = 4;
const P_LOGICAL_OR: u8 = 5;
const P_LOGICAL_AND: u8 = 6;
const P_BIT_OR: u8 = 7;
const P_BIT_XOR: u8 = 8;
const P_BIT_AND: u8 = 9;
const P_EQUALITY: u8 = 10;
const P_RELATIONAL: u8 = 11;
const P_SHIFT: u8 = 12;
const P_ADDITIVE: u8 = 13;
const P_MULTIPLICATIVE: u8 = 14;
const P_EXPONENT: u8 = 15;
const P_UNARY: u8 = 16;
const P_POSTFIX: u8 = 17;
const P_CALL_MEMBER: u8 = 18;
const P_PRIMARY: u8 = 19;

impl<'i, 'l, 'd, 'm, 'mc> Codegen<'i, 'l, 'd, 'm, 'mc> {
    fn name(&self, atom: Atom) -> String {
        self.interner.resolve(atom)
    }

    fn push(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn push_name(&mut self, atom: Atom) {
        // 零分配：借用驻留切片直接拷进输出缓冲，省去 resolve 的临时 String。
        // 闭包只写 out、不回调 interner，无重入死锁风险。
        let interner = self.interner;
        let out = &mut self.out;
        interner.with_resolved(atom, |s| out.push_str(s));
    }

    /// 发射一个**变量引用/绑定**标识符：若 mangling 侧表命中该 span，写新名，否则写原名。
    /// 只用于会被 mangle 的位置（标识符表达式、绑定模式、函数/类名）；属性名/成员名/导出名
    /// 走 [`Codegen::push_name`]，永不查表。
    fn push_ident(&mut self, ident: &Ident) {
        if let Some(map) = self.rename
            && let Some(&nn) = map.get(&ident.span)
        {
            self.push_name(nn);
            return;
        }
        self.push_name(ident.name);
    }

    /// 该 span 处标识符是否被 mangle 重命名。
    fn is_renamed(&self, span: Span) -> bool {
        self.rename.is_some_and(|m| m.contains_key(&span))
    }

    /// 对象字面量 shorthand `{ x }` 的 value 标识符是否被重命名——若是，须展开为 `x: 新名`，
    /// 否则会把属性名也一起改掉。
    fn ex(&self) -> &str {
        if self.minify_names { "e" } else { "exports" }
    }

    fn value_ident_renamed(&self, e: &Expression) -> bool {
        matches!(e, Expression::Identifier(id) if self.is_renamed(id.span))
    }

    /// 解构 shorthand `{ x }` / `{ x = 1 }` 的绑定标识符是否被重命名——若是，须展开为
    /// `x: 新名` / `x: 新名 = 1`，否则会把从对象取的属性名也一起改掉。
    fn pattern_binding_renamed(&self, pat: &Pattern) -> bool {
        let span = match pat {
            Pattern::Ident(id) => id.span,
            Pattern::Assignment(a) => match &a.left {
                Pattern::Ident(id) => id.span,
                _ => return false,
            },
            _ => return false,
        };
        self.is_renamed(span)
    }

    fn newline(&mut self) {
        // 紧凑模式：省略换行与缩进。语句均以显式 `;`/`}` 收尾 → 拼接无 ASI 风险。
        if self.minify {
            return;
        }
        self.out.push('\n');
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
    }

    // ==================================================================
    // 程序 / 语句
    // ==================================================================

    fn emit_program(&mut self, program: &Program) {
        let stmts = &program.body[..];

        // Pre-pass: prune zombie internal_reads from declarations that will be dropped.
        // A "zombie read" is a read of variable A from inside declaration B, where B itself
        // is externally unused and will be removed. After B is removed, A's only reader is
        // gone and A becomes droppable. We iterate until fixpoint.
        if self.shake.is_some() {
            self.prune_zombie_reads(stmts);
        }

        let mut i = 0;
        while i < stmts.len() {
            if i > 0 {
                self.newline();
            }
            i = self.emit_merged_statement(stmts, i);
        }
    }

    /// Iteratively identify export declarations that can be dropped and remove their
    /// reads from `shake.internal_reads`. This breaks zombie chains where a pure
    /// declaration A reads variable B, and B reads nothing else. When A is dropped,
    /// B's only reader disappears and B becomes droppable.
    fn prune_zombie_reads(&mut self, stmts: &[Statement]) {
        // Collect export info first (needs immutable self).
        struct ExpInfo {
            names: Vec<Atom>,
            pure: bool,
            reads: FxHashSet<Atom>,
        }
        let mut exports: Vec<ExpInfo> = Vec::new();
        // Map: atom → set of export indices that READ this atom.
        let mut readers_of: FxHashMap<Atom, Vec<usize>> = FxHashMap::default();

        for stmt in stmts {
            if let Statement::ExportNamed(s) = stmt {
                if let Some(decl) = &s.declaration {
                    let names = self.decl_names(decl);
                    let all_unused = names.iter().all(|n| !self.shake.as_ref().unwrap().is_used(*n));
                    let pure = decl_is_pure(decl);

                    let mut reads = FxHashSet::default();
                    if all_unused && pure {
                        collect_reads_in_statement(decl, &mut reads);
                        for atom in &reads {
                            readers_of.entry(*atom).or_default().push(exports.len());
                        }
                    }

                    exports.push(ExpInfo { names, pure, reads });
                }
            }
        }

        if exports.is_empty() {
            return;
        }

        // Iteratively drop declarations whose internal reads are only from already-dropped decls.
        let mut dropped: FxHashSet<usize> = FxHashSet::default();
        let mut changed = true;

        while changed {
            changed = false;
            for (i, exp) in exports.iter().enumerate() {
                if !exp.pure || dropped.contains(&i) {
                    continue;
                }
                let all_unused = exp.names.iter().all(|n| !self.shake.as_ref().unwrap().is_used(*n));
                if !all_unused {
                    continue;
                }
                let all_readers_dropped = exp.names.iter().all(|n| {
                    readers_of.get(n).map_or(true, |readers| {
                        readers.iter().all(|r| dropped.contains(r))
                    })
                });
                if all_readers_dropped {
                    dropped.insert(i);
                    changed = true;
                }
            }
        }

        // Remove internal_reads entries that only come from dropped declarations.
        let shake = self.shake.as_mut().unwrap();
        for (i, exp) in exports.iter().enumerate() {
            if !dropped.contains(&i) {
                continue;
            }
            for atom in &exp.reads {
                let has_live_reader = readers_of.get(atom).map_or(false, |readers| {
                    readers.iter().any(|r| *r != i && !dropped.contains(r))
                });
                if !has_live_reader {
                    shake.remove_read(*atom);
                }
            }
        }
    }

    fn emit_statement(&mut self, stmt: &Statement) {
        // DCE: skip statements marked for removal by the DCE analysis.
        if let Some(ctx) = self.minify_ctx
            && ctx.remove_spans.contains(&stmt.span())
        {
            return;
        }
        match stmt {
            Statement::VariableDeclaration(d) => {
                if let Some(ctx) = self.minify_ctx {
                    self.emit_var_decl_elim(d, ctx);
                } else {
                    self.emit_var_decl(d);
                    self.push(";");
                }
            }
            Statement::FunctionDeclaration(f) => self.emit_function(f),
            Statement::ClassDeclaration(c) => self.emit_class(c),
            Statement::Block(b) => self.emit_block(&b.body),
            Statement::Empty(_) => self.push(";"),
            Statement::Expression(e) => {
                // 避免以 `{`/`function`/`class` 开头被误解析为块/声明。
                let needs_paren = starts_with_problematic(&e.expression);
                if needs_paren {
                    self.push("(");
                }
                self.emit_expr(&e.expression, P_SEQUENCE);
                if needs_paren {
                    self.push(")");
                }
                self.push(";");
            }
            Statement::If(s) => {
                // M4b: minify 下按常量 test 折叠死分支（decide-then-skip，不改 AST，Span 保持）。
                // 被丢弃分支含提升声明（var/函数）则不折叠——丢弃提升绑定会致 ReferenceError。
                if self.minify
                    && let Some(cond) = self.const_eval_bool(&s.test)
                {
                    let (kept, dropped): (Option<&Statement>, Option<&Statement>) = if cond {
                        (Some(&s.consequent), s.alternate.as_ref())
                    } else {
                        (s.alternate.as_ref(), Some(&s.consequent))
                    };
                    if dropped.is_none_or(|st| !has_hoisted_decl(st)) {
                        if let Some(st) = kept {
                            self.emit_statement(st);
                        }
                        // `if(false)` 无 else → 整条消除（不发任何东西）。
                        return;
                    }
                    // 被丢弃分支含提升声明 → 落到常规发射（保守不折叠）。
                }
                self.push("if (");
                self.emit_expr(&s.test, P_SEQUENCE);
                self.push(") ");
                self.emit_statement(&s.consequent);
                if let Some(alt) = &s.alternate {
                    self.push(" else ");
                    self.emit_statement(alt);
                }
            }
            Statement::For(s) => {
                self.push("for (");
                if let Some(init) = &s.init {
                    match init {
                        ForInit::Variable(d) => self.emit_var_decl(d),
                        ForInit::Expression(e) => self.emit_expr(e, P_SEQUENCE),
                    }
                }
                self.push("; ");
                if let Some(t) = &s.test {
                    self.emit_expr(t, P_SEQUENCE);
                }
                self.push("; ");
                if let Some(u) = &s.update {
                    self.emit_expr(u, P_SEQUENCE);
                }
                self.push(") ");
                self.emit_statement(&s.body);
            }
            Statement::ForIn(s) => {
                self.push("for (");
                self.emit_for_left(&s.left);
                self.push(" in ");
                self.emit_expr(&s.right, P_SEQUENCE);
                self.push(") ");
                self.emit_statement(&s.body);
            }
            Statement::ForOf(s) => {
                self.push(if s.is_await { "for await (" } else { "for (" });
                self.emit_for_left(&s.left);
                self.push(" of ");
                self.emit_expr(&s.right, P_ASSIGN);
                self.push(") ");
                self.emit_statement(&s.body);
            }
            Statement::While(s) => {
                self.push("while (");
                self.emit_expr(&s.test, P_SEQUENCE);
                self.push(") ");
                self.emit_statement(&s.body);
            }
            Statement::DoWhile(s) => {
                self.push("do ");
                self.emit_statement(&s.body);
                self.push(" while (");
                self.emit_expr(&s.test, P_SEQUENCE);
                self.push(");");
            }
            Statement::Switch(s) => {
                self.push("switch (");
                self.emit_expr(&s.discriminant, P_SEQUENCE);
                self.push(") {");
                self.indent += 1;
                for case in s.cases.iter() {
                    self.newline();
                    match &case.test {
                        Some(t) => {
                            self.push("case ");
                            self.emit_expr(t, P_SEQUENCE);
                            self.push(":");
                        }
                        None => self.push("default:"),
                    }
                    self.indent += 1;
                    for st in case.consequent.iter() {
                        self.newline();
                        self.emit_statement(st);
                    }
                    self.indent -= 1;
                }
                self.indent -= 1;
                self.newline();
                self.push("}");
            }
            Statement::Return(s) => {
                self.push("return");
                if let Some(a) = &s.argument {
                    self.push(" ");
                    self.emit_expr(a, P_SEQUENCE);
                }
                self.push(";");
            }
            Statement::Break(s) => {
                self.push("break");
                if let Some(l) = &s.label {
                    self.push(" ");
                    self.push_name(l.name);
                }
                self.push(";");
            }
            Statement::Continue(s) => {
                self.push("continue");
                if let Some(l) = &s.label {
                    self.push(" ");
                    self.push_name(l.name);
                }
                self.push(";");
            }
            Statement::Throw(s) => {
                self.push("throw ");
                self.emit_expr(&s.argument, P_SEQUENCE);
                self.push(";");
            }
            Statement::Try(s) => {
                self.push("try ");
                self.emit_block(&s.block.body);
                if let Some(h) = &s.handler {
                    self.push(" catch ");
                    if let Some(p) = &h.param {
                        self.push("(");
                        self.emit_pattern(p);
                        self.push(") ");
                    }
                    self.emit_block(&h.body.body);
                }
                if let Some(f) = &s.finalizer {
                    self.push(" finally ");
                    self.emit_block(&f.body);
                }
            }
            Statement::Labeled(s) => {
                self.push_name(s.label.name);
                self.push(": ");
                self.emit_statement(&s.body);
            }
            Statement::With(s) => {
                self.push("with (");
                self.emit_expr(&s.object, P_SEQUENCE);
                self.push(") ");
                self.emit_statement(&s.body);
            }
            Statement::Debugger(_) => self.push("debugger;"),
            Statement::Import(d) => {
                if self.linker.is_some() {
                    self.emit_import_linked(d);
                } else {
                    self.emit_import(d);
                }
            }
            Statement::ExportNamed(s) => {
                if self.linker.is_some() {
                    self.emit_export_named_linked(s);
                } else {
                    self.emit_export_named(s);
                }
            }
            Statement::ExportDefault(s) => {
                if self.linker.is_some() {
                    self.emit_export_default_linked(s);
                } else {
                    self.emit_export_default(s);
                }
            }
            Statement::ExportAll(s) => {
                if self.linker.is_some() {
                    self.emit_export_all_linked(s);
                } else {
                    self.emit_export_all(s);
                }
            }
        }
    }

    fn emit_block(&mut self, body: &AVec<Statement>) {
        self.push("{");
        if body.is_empty() {
            self.push("}");
            return;
        }
        self.indent += 1;
        let stmts = &body[..];
        let mut i = 0;
        while i < stmts.len() {
            self.newline();
            i = self.emit_merged_statement(stmts, i);
        }
        self.indent -= 1;
        self.newline();
        self.push("}");
    }

    fn emit_var_decl(&mut self, d: &VariableDeclaration) {
        self.push(d.kind.as_str());
        self.push(" ");
        for (i, decl) in d.declarations.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            self.emit_pattern(&decl.id);
            if let Some(init) = &decl.init {
                self.push(" = ");
                self.emit_expr(init, P_ASSIGN);
            }
        }
    }

    /// Emit a variable declaration with variable elimination: skip unused
    /// pure-bindings, emit only the initializer for unused impure bindings.
    fn emit_var_decl_elim(&mut self, d: &VariableDeclaration, ctx: &MinifyCtx) {
        let mut emitted = false;
        let mut in_var = false;

        for decl in d.declarations.iter() {
            let is_unused = match &decl.id {
                Pattern::Ident(id) => {
                    ctx.unused_vars.contains(&id.name)
                }
                _ => false,
            };

            if is_unused {
                if let Some(init) = &decl.init {
                    if !expr_is_pure(init) {
                        if emitted {
                            self.push(";");
                        }
                        self.emit_expr(init, P_SEQUENCE);
                        emitted = true;
                        in_var = false;
                    }
                }
                continue;
            }

            if in_var {
                self.push(", ");
            } else {
                if emitted {
                    self.push(";");
                }
                self.push(d.kind.as_str());
                self.push(" ");
                in_var = true;
            }

            self.emit_pattern(&decl.id);
            if let Some(init) = &decl.init {
                self.push(" = ");
                self.emit_expr(init, P_ASSIGN);
            }
            emitted = true;
        }

        if emitted {
            self.push(";");
        }
    }

    // ==================================================================
    // Phase 3 — Statement-level optimization emission
    // ==================================================================

    /// Emit one statement from a list, applying Phase 3 merges.
    /// Returns the index of the next statement to process.
    fn emit_merged_statement(&mut self, stmts: &[Statement], i: usize) -> usize {
        let stmt = &stmts[i];

        // Skip hoisted var declarations (already emitted at function top)
        if let Some(ctx) = self.minify_ctx {
            if let Statement::VariableDeclaration(d) = stmt {
                if ctx.hoist.var_hoist_flat.contains(&d.span) {
                    return i + 1;
                }
            }
        }

        if let Some(ctx) = self.minify_ctx {
            match stmt {
                Statement::VariableDeclaration(d) => {
                    if ctx.join_var_spans.iter().any(|(_, b)| *b == d.span) {
                        return i + 1;
                    }
                    if ctx.join_var_spans.iter().any(|(a, _)| *a == d.span) {
                        return self.emit_joined_vars(stmts, i);
                    }
                }
                Statement::Expression(e) => {
                    if ctx.sequence_spans.iter().any(|(_, b)| *b == e.span) {
                        return i + 1;
                    }
                    if ctx.sequence_spans.iter().any(|(a, _)| *a == e.span) {
                        return self.emit_joined_sequence(stmts, i);
                    }
                }
                Statement::If(s) => {
                    if self.minify && self.const_eval_bool(&s.test).is_some() {
                        // DCE folding takes precedence
                    } else if let Some(cand) =
                        ctx.if_return_spans.iter().find(|c| c.if_span == s.span)
                    {
                        return self.emit_optimized_if_return(s, cand, stmts, i);
                    }
                }
                _ => {}
            }
        }

        self.emit_statement(stmt);
        i + 1
    }

    /// Merge consecutive same-kind VariableDeclarations: `var a=1; var b=2;` → `var a=1, b=2;`
    fn emit_joined_vars(&mut self, stmts: &[Statement], i: usize) -> usize {
        let ctx = self.minify_ctx.unwrap();
        let Statement::VariableDeclaration(first) = &stmts[i] else { return i + 1 };
        let kind = first.kind;

        let mut emitted = false;
        let mut in_var = false;
        let mut j = i;

        while j < stmts.len() {
            let Statement::VariableDeclaration(next) = &stmts[j] else { break };
            if next.kind != kind {
                break;
            }
            if j > i {
                let prev_span = stmts[j - 1].span();
                if !ctx
                    .join_var_spans
                    .iter()
                    .any(|(a, b)| *a == prev_span && *b == next.span)
                {
                    break;
                }
            }

            for decl in next.declarations.iter() {
                let is_unused = match &decl.id {
                    Pattern::Ident(id) => ctx.unused_vars.contains(&id.name),
                    _ => false,
                };

                if is_unused {
                    if let Some(init) = &decl.init {
                        if !expr_is_pure(init) {
                            if emitted {
                                self.push(";");
                            }
                            self.emit_expr(init, P_SEQUENCE);
                            emitted = true;
                            in_var = false;
                        }
                    }
                    continue;
                }

                if in_var {
                    self.push(", ");
                } else {
                    if emitted {
                        self.push(";");
                    }
                    self.push(kind.as_str());
                    self.push(" ");
                    in_var = true;
                }

                self.emit_pattern(&decl.id);
                if let Some(init) = &decl.init {
                    self.push(" = ");
                    self.emit_expr(init, P_ASSIGN);
                }
                emitted = true;
            }

            j += 1;
        }

        if emitted {
            self.push(";");
        }
        j
    }

    /// Merge consecutive ExpressionStatements: `a(); b();` → `a(), b();`
    fn emit_joined_sequence(&mut self, stmts: &[Statement], i: usize) -> usize {
        let ctx = self.minify_ctx.unwrap();

        let needs_paren = if let Statement::Expression(e) = &stmts[i] {
            starts_with_problematic(&e.expression)
        } else {
            false
        };

        if needs_paren {
            self.push("(");
        }

        let mut first = true;
        let mut j = i;
        while j < stmts.len() {
            let Statement::Expression(next) = &stmts[j] else { break };
            if j > i {
                let prev_span = stmts[j - 1].span();
                if !ctx
                    .sequence_spans
                    .iter()
                    .any(|(a, b)| *a == prev_span && *b == next.span)
                {
                    break;
                }
            }
            if !first {
                self.push(", ");
            }
            first = false;
            self.emit_expr(&next.expression, P_ASSIGN);
            j += 1;
        }

        if needs_paren {
            self.push(")");
        }
        self.push(";");
        j
    }

    /// Optimize `if (cond) return a; return b;` → `return cond ? a : b;`
    fn emit_optimized_if_return(
        &mut self,
        s: &IfStatement,
        _cand: &IfReturnCandidate,
        _stmts: &[Statement],
        i: usize,
    ) -> usize {
        let cons_ret_expr = extract_return_expression(&s.consequent);
        let alt_ret_expr = s
            .alternate
            .as_ref()
            .and_then(|alt| extract_return_expression(alt));

        self.push("return ");
        self.emit_expr(&s.test, P_CONDITIONAL);
        self.push(" ? ");
        if let Some(arg) = cons_ret_expr {
            self.emit_expr(arg, P_ASSIGN);
        } else {
            self.push("void 0");
        }
        self.push(" : ");
        if let Some(arg) = alt_ret_expr {
            self.emit_expr(arg, P_ASSIGN);
        } else {
            self.push("void 0");
        }
        self.push(";");

        // Pattern 1 (no else): also skip the subsequent return statement
        if s.alternate.is_none() {
            i + 2
        } else {
            i + 1
        }
    }

    fn emit_for_left(&mut self, left: &ForLeft) {
        match left {
            ForLeft::Variable(d) => self.emit_var_decl(d),
            ForLeft::Target(e) => self.emit_expr(e, P_ASSIGN),
        }
    }

    // ==================================================================
    // 模块语句
    // ==================================================================

    fn emit_import(&mut self, d: &ImportDeclaration) {
        self.push("import ");
        let mut named = Vec::new();
        let mut wrote_leading = false;
        for spec in d.specifiers.iter() {
            match spec {
                ImportSpecifier::Default { local, .. } => {
                    if wrote_leading {
                        self.push(", ");
                    }
                    self.push_name(local.name);
                    wrote_leading = true;
                }
                ImportSpecifier::Namespace { local, .. } => {
                    if wrote_leading {
                        self.push(", ");
                    }
                    self.push("* as ");
                    self.push_name(local.name);
                    wrote_leading = true;
                }
                ImportSpecifier::Named {
                    imported, local, ..
                } => named.push((*imported, *local)),
            }
        }
        if !named.is_empty() {
            if wrote_leading {
                self.push(", ");
            }
            self.push("{ ");
            for (i, (imported, local)) in named.iter().enumerate() {
                if i > 0 {
                    self.push(", ");
                }
                self.emit_module_export_name(imported);
                if !same_name(imported, &ModuleExportName::Ident(*local)) {
                    self.push(" as ");
                    self.push_name(local.name);
                }
            }
            self.push(" }");
            wrote_leading = true;
        }
        if wrote_leading {
            self.push(" from ");
        }
        self.emit_string_atom(d.source);
        self.push(";");
    }

    fn emit_export_named(&mut self, s: &ExportNamedDeclaration) {
        self.push("export ");
        if let Some(decl) = &s.declaration {
            self.emit_statement(decl);
            return;
        }
        self.push("{ ");
        for (i, spec) in s.specifiers.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            self.emit_module_export_name(&spec.local);
            if !same_name(&spec.local, &spec.exported) {
                self.push(" as ");
                self.emit_module_export_name(&spec.exported);
            }
        }
        self.push(" }");
        if let Some(src) = s.source {
            self.push(" from ");
            self.emit_string_atom(src);
        }
        self.push(";");
    }

    fn emit_export_default(&mut self, s: &ExportDefaultDeclaration) {
        self.push("export default ");
        match s.declaration {
            ExportDefaultKind::Function(f) => self.emit_function(f),
            ExportDefaultKind::Class(c) => self.emit_class(c),
            ExportDefaultKind::Expression(e) => {
                self.emit_expr(&e, P_ASSIGN);
                self.push(";");
            }
        }
    }

    fn emit_export_all(&mut self, s: &ExportAllDeclaration) {
        self.push("export *");
        if let Some(ns) = &s.exported {
            self.push(" as ");
            self.emit_module_export_name(ns);
        }
        self.push(" from ");
        self.emit_string_atom(s.source);
        self.push(";");
    }

    fn emit_module_export_name(&mut self, n: &ModuleExportName) {
        match n {
            ModuleExportName::Ident(id) => self.push_name(id.name),
            ModuleExportName::String(a) => self.emit_string_atom(*a),
        }
    }

    // ==================================================================
    // 链接（ESM → CJS）：linker 存在时启用
    // ==================================================================

    fn next_tmp(&mut self) -> String {
        let t = format!("_wm{}", self.link_tmp);
        self.link_tmp += 1;
        t
    }

    /// `__wake_require__(id)` 或外部回退 `require("spec")`。
    fn require_expr(&self, specifier: &str) -> String {
        let linker = self.linker.unwrap();
        match linker.module_id(specifier) {
            Some(id) => format!("{}({})", linker.require_fn(), id),
            None => format!("require({specifier:?})"),
        }
    }

    /// **静态导入位置**的 require 表达式：目标是 async 模块（顶层 await）时 `__wake_require__`
    /// 返回 Promise，需 `await` 解包。此处的调用点只出现在模块体顶层，而导入了 async 模块的模块
    /// 本身也被打包器标为 async（包装器是 `async function`），故 `await` 合法。
    ///
    /// 只用于 `import` / `export ... from` 的降级；`require("x")` 改写点与动态 `import()`
    /// 不走这里（前者可能嵌在普通函数内，后者本就产出 Promise）。
    fn require_expr_static(&self, specifier: &str) -> String {
        let linker = self.linker.unwrap();
        match linker.module_id(specifier) {
            Some(id) if linker.is_async_module(id) => {
                format!("(await {}({}))", linker.require_fn(), id)
            }
            Some(id) => format!("{}({})", linker.require_fn(), id),
            None => format!("require({specifier:?})"),
        }
    }

    fn module_export_name_string(&self, n: &ModuleExportName) -> String {
        match n {
            ModuleExportName::Ident(id) => self.name(id.name),
            ModuleExportName::String(a) => self.name(*a),
        }
    }

    fn emit_import_linked(&mut self, d: &ImportDeclaration) {
        let src = self.name(d.source);
        let req = self.require_expr_static(&src);
        if d.specifiers.is_empty() {
            self.push(&req);
            self.push(";");
            return;
        }
        let tmp = self.next_tmp();
        self.push(&format!("const {tmp} = {req};"));
        for spec in d.specifiers.iter() {
            self.newline();
            match spec {
                ImportSpecifier::Default { local, .. } => {
                    let n = self.name(local.name);
                    // CJS interop：转译 ESM（有 __esModule）取 `.default`，纯 CJS 取整个 exports
                    //（如 `import React from 'react'`，react 是 `module.exports = {...}`）。
                    self.push(&format!("const {n} = __wake_interop_default({tmp});"));
                }
                ImportSpecifier::Namespace { local, .. } => {
                    let n = self.name(local.name);
                    // namespace interop：转译 ESM 原样；纯 CJS 复制属性并补 `default` = 整个 exports。
                    self.push(&format!("const {n} = __wake_interop_star({tmp});"));
                }
                ImportSpecifier::Named {
                    imported, local, ..
                } => {
                    let imp = self.module_export_name_string(imported);
                    let n = self.name(local.name);
                    self.push(&format!("const {n} = {tmp}[{imp:?}];"));
                }
            }
        }
    }

    fn emit_export_named_linked(&mut self, s: &ExportNamedDeclaration) {
        if let Some(decl) = &s.declaration {
            let names = self.decl_names(decl);
            let decl_span = self.decl_span(decl);
            if self.shake.is_some() {
                // 先在不可变借用下算好决策，再释放借用做可变发射（避免 borrowck 冲突）。
                let (drop_all, used_flags): (bool, Vec<bool>) = {
                    let shake = self.shake.as_ref().unwrap();
                    let all_unused = names.iter().all(|n| !shake.is_used(*n));
                    let none_read = names.iter().all(|n| !shake.is_read(*n));
                    let used_flags = names.iter().map(|n| shake.is_used(*n)).collect();
                    // 整条声明既无外部使用、模块内也未引用、且无副作用 → 安全移除（Tree Shaking）。
                    (all_unused && none_read && decl_is_pure(decl), used_flags)
                };
                if drop_all {
                    return;
                }
                // 否则保留声明，但仅为**已用**导出发绑定行（移除未用绑定永远安全）。
                self.emit_statement(decl);
                for (n, &used) in names.iter().zip(&used_flags) {
                    if used {
                        self.newline();
                        self.emit_export_binding(*n, decl_span);
                    }
                }
                return;
            }
            self.emit_statement(decl);
            for n in names {
                self.newline();
                self.emit_export_binding(n, decl_span);
            }
            return;
        }
        match s.source {
            Some(src) => {
                // re-export：`require` 始终保留（模块副作用），仅按 shake 过滤绑定行。
                let srcs = self.name(src);
                let req = self.require_expr_static(&srcs);
                let tmp = self.next_tmp();
                self.push(&format!("const {tmp} = {req};"));
                for spec in s.specifiers.iter() {
                    if self.is_shaken_out(module_export_name_atom(&spec.exported)) {
                        continue;
                    }
                    self.newline();
                    let exported = self.module_export_name_string(&spec.exported);
                    let local = self.module_export_name_string(&spec.local);
                    self.push(&format!("{ex}[{exported:?}] = {tmp}[{local:?}];", ex = self.ex()));
                }
            }
            None => {
                let mut first = true;
                for spec in s.specifiers.iter() {
                    if self.is_shaken_out(module_export_name_atom(&spec.exported)) {
                        continue;
                    }
                    if !first {
                        self.newline();
                    }
                    first = false;
                    let exported = self.module_export_name_string(&spec.exported);
                    let local = self.module_export_name_string(&spec.local);
                    self.push(&format!("exports[{exported:?}] = {local};"));
                }
            }
        }
    }

    /// shake 开启且该导出名未被外部使用 → 应移除其绑定行。
    fn is_shaken_out(&self, exported: Atom) -> bool {
        self.shake.as_ref().is_some_and(|sh| !sh.is_used(exported))
    }

    /// 发射一行 `exports["name"] = name;`：借用驻留切片直接写出，零分配。
    /// `{s:?}` 对 `&str` 与原 `String` 的 Debug 输出一致，字节不变。
    fn emit_export_binding(&mut self, name: Atom, _decl_span: Option<Span>) {
        let interner = self.interner;
        let out = &mut self.out;
        interner.with_resolved(name, |s| {
            let _ = write!(out, "exports[{s:?}] = {s};");
        });
    }

    fn emit_export_default_linked(&mut self, s: &ExportDefaultDeclaration) {
        let default_used = self
            .shake
            .as_ref()
            .is_none_or(|sh| sh.is_used(sh.default_atom));
        match s.declaration {
            ExportDefaultKind::Function(f) => match f.id {
                Some(id) => {
                    let n = self.name(id.name);
                    let read = self.shake.as_ref().is_some_and(|sh| sh.is_read(id.name));
                    // 命名默认函数：未用且内部未引用 → 整体移除；否则保留声明，按需发绑定。
                    if !default_used && !read {
                        return;
                    }
                    self.emit_function(f);
                    if default_used {
                        self.newline();
                        self.push(&format!("exports.default = {n};"));
                    }
                }
                None => {
                    if !default_used {
                        return; // 匿名默认函数（纯），未用即移除
                    }
                    self.push("exports.default = ");
                    self.emit_function(f);
                    self.push(";");
                }
            },
            ExportDefaultKind::Class(c) => match c.id {
                Some(id) => {
                    let n = self.name(id.name);
                    let read = self.shake.as_ref().is_some_and(|sh| sh.is_read(id.name));
                    if !default_used && !read && class_is_pure(c) {
                        return;
                    }
                    self.emit_class(c);
                    if default_used {
                        self.newline();
                        self.push(&format!("exports.default = {n};"));
                    }
                }
                None => {
                    if !default_used && class_is_pure(c) {
                        return;
                    }
                    self.push("exports.default = ");
                    self.emit_class(c);
                    self.push(";");
                }
            },
            ExportDefaultKind::Expression(e) => {
                // 未用且无副作用 → 移除；有副作用则保留求值。
                if !default_used && expr_is_pure(&e) {
                    return;
                }
                self.push("exports.default = ");
                self.emit_expr(&e, P_ASSIGN);
                self.push(";");
            }
        }
    }

    fn emit_export_all_linked(&mut self, s: &ExportAllDeclaration) {
        let srcs = self.name(s.source);
        let req = self.require_expr_static(&srcs);
        let tmp = self.next_tmp();
        self.push(&format!("const {tmp} = {req};"));
        self.newline();
        match &s.exported {
            Some(ns) => {
                let name = self.module_export_name_string(ns);
                self.push(&format!("exports[{name:?}] = {tmp};"));
            }
            None => {
                self.push(&format!(
                    "for (const _k in {tmp}) if (_k !== \"default\") exports[_k] = {tmp}[_k];"
                ));
            }
        }
    }

    /// 声明语句导出的名字 Atom（用于 `export const/function/class`）。
    fn decl_names(&self, stmt: &Statement) -> Vec<Atom> {
        let mut names = Vec::new();
        match stmt {
            Statement::VariableDeclaration(d) => {
                for decl in d.declarations.iter() {
                    collect_pattern_names(&decl.id, &mut names);
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

    /// 声明语句导出的名字 Span（用于 rename lookup）。
    fn decl_span(&self, stmt: &Statement) -> Option<Span> {
        match stmt {
            Statement::FunctionDeclaration(f) => f.id.as_ref().map(|id| id.span),
            Statement::ClassDeclaration(c) => c.id.as_ref().map(|id| id.span),
            _ => None,
        }
    }

    /// linker 存在且是 `require("literal")` 调用时改写为 `__wake_require__(id)`；返回是否已发射。
    fn emit_require_call(&mut self, c: &CallExpression) -> bool {
        if let Expression::Identifier(id) = &c.callee
            && c.arguments.len() == 1
            && let Expression::StringLiteral(s) = &c.arguments[0]
            && self.interner.with_resolved(id.name, |n| n == "require")
        {
            let spec = self.name(s.value);
            let req = self.require_expr(&spec);
            self.push(&req);
            return true;
        }
        false
    }

    // ==================================================================
    // 函数 / 类
    // ==================================================================

    fn emit_function(&mut self, f: &Function) {
        if f.is_async {
            self.push("async ");
        }
        self.push("function");
        if f.is_generator {
            self.push("*");
        }
        self.push(" ");
        if let Some(id) = f.id {
            self.push_ident(&id);
        }
        self.emit_params(&f.params);
        self.push(" ");
        match f.body {
            Some(body) => {
                if let Some(ctx) = self.minify_ctx
                    && ctx.minify
                    && ctx.hoist.var_hoist_spans.contains_key(&body.span)
                {
                    self.emit_function_body_hoisted(body);
                } else {
                    self.emit_block(&body.statements);
                }
            }
            None => self.push("{}"),
        }
    }

    /// Emit a function body with `var` declarations hoisted to the top.
    fn emit_function_body_hoisted(&mut self, body: &FunctionBody) {
        let ctx = self.minify_ctx.unwrap();
        let hoisted_flat = &ctx.hoist.var_hoist_flat;

        self.push("{");
        if body.statements.is_empty() {
            self.push("}");
            return;
        }
        self.indent += 1;

        // Collect and emit hoisted var declarations at the top
        let hoisted_decls = collect_hoisted_var_decls(&body.statements, hoisted_flat);
        if !hoisted_decls.is_empty() {
            self.emit_hoisted_var_group(&hoisted_decls);
        }

        // Emit remaining statements (hoisted vars skipped by emit_merged_statement)
        let stmts = &body.statements[..];
        let mut i = 0;
        while i < stmts.len() {
            self.newline();
            i = self.emit_merged_statement(stmts, i);
        }
        self.indent -= 1;
        self.newline();
        self.push("}");
    }

    /// Emit a group of `var` declarations as a single `var x = 1, y = 2;`.
    fn emit_hoisted_var_group(&mut self, decls: &[&VariableDeclaration]) {
        self.newline();
        self.push("var ");
        let mut first = true;
        for d in decls {
            for decl in d.declarations.iter() {
                if !first {
                    self.push(", ");
                }
                first = false;
                self.emit_pattern(&decl.id);
                if let Some(init) = &decl.init {
                    self.push(" = ");
                    self.emit_expr(init, P_ASSIGN);
                }
            }
        }
        self.push(";");
    }

    fn emit_params(&mut self, params: &AVec<Pattern>) {
        let end = if let Some(ctx) = self.minify_ctx
            && ctx.minify
        {
            let mut last_used = params.len();
            for (i, p) in params.iter().enumerate().rev() {
                if let Pattern::Ident(id) = p {
                    if ctx.unused_vars.contains(&id.name) {
                        last_used = i;
                        continue;
                    }
                }
                break;
            }
            last_used
        } else {
            params.len()
        };

        self.push("(");
        for i in 0..end {
            if i > 0 {
                self.push(", ");
            }
            self.emit_pattern(&params[i]);
        }
        self.push(")");
    }

    fn emit_class(&mut self, c: &Class) {
        self.push("class");
        if let Some(id) = c.id {
            self.push(" ");
            self.push_ident(&id);
        }
        if let Some(sc) = &c.super_class {
            self.push(" extends ");
            self.emit_expr(sc, P_CALL_MEMBER);
        }
        self.push(" {");
        self.indent += 1;
        for member in c.body.iter() {
            self.newline();
            self.emit_class_member(member);
        }
        self.indent -= 1;
        if !c.body.is_empty() {
            self.newline();
        }
        self.push("}");
    }

    fn emit_class_member(&mut self, member: &ClassMember) {
        match member {
            ClassMember::Method(m) => {
                if m.is_static {
                    self.push("static ");
                }
                if m.value.is_async {
                    self.push("async ");
                }
                if m.value.is_generator {
                    self.push("*");
                }
                match m.kind {
                    MethodKind::Get => self.push("get "),
                    MethodKind::Set => self.push("set "),
                    _ => {}
                }
                self.emit_property_key(&m.key, m.computed);
                self.emit_params(&m.value.params);
                self.push(" ");
                match m.value.body {
                    Some(b) => self.emit_block(&b.statements),
                    None => self.push("{}"),
                }
            }
            ClassMember::Property(p) => {
                if p.is_static {
                    self.push("static ");
                }
                self.emit_property_key(&p.key, p.computed);
                if let Some(v) = &p.value {
                    self.push(" = ");
                    self.emit_expr(v, P_ASSIGN);
                }
                self.push(";");
            }
            ClassMember::StaticBlock(b) => {
                self.push("static ");
                self.emit_block(&b.body);
            }
        }
    }

    fn emit_property_key(&mut self, key: &PropertyKey, computed: bool) {
        if computed {
            self.push("[");
            if let PropertyKey::Computed(e) = key {
                self.emit_expr(e, P_ASSIGN);
            }
            self.push("]");
            return;
        }
        match key {
            PropertyKey::Ident(id) => self.push_name(id.name),
            PropertyKey::String(s) => self.emit_string_atom(s.value),
            PropertyKey::Number(n) => write_number(&mut self.out, n.value),
            PropertyKey::Private(id) => {
                self.push("#");
                self.push_name(id.name);
            }
            PropertyKey::Computed(e) => {
                self.push("[");
                self.emit_expr(e, P_ASSIGN);
                self.push("]");
            }
        }
    }

    // ==================================================================
    // 模式
    // ==================================================================

    fn emit_pattern(&mut self, pat: &Pattern) {
        match pat {
            Pattern::Ident(id) => self.push_ident(id),
            Pattern::Array(a) => {
                self.push("[");
                for (i, el) in a.elements.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    if let Some(p) = el {
                        self.emit_pattern(p);
                    }
                }
                self.push("]");
            }
            Pattern::Object(o) => {
                self.push("{ ");
                let mut first = true;
                for p in o.properties.iter() {
                    if !first {
                        self.push(", ");
                    }
                    first = false;
                    // mangle 后绑定名改变时展开 shorthand，避免连带改掉属性名。
                    if p.shorthand && !p.computed && !self.pattern_binding_renamed(&p.value) {
                        self.emit_pattern(&p.value);
                    } else {
                        self.emit_property_key(&p.key, p.computed);
                        self.push(": ");
                        self.emit_pattern(&p.value);
                    }
                }
                if let Some(rest) = &o.rest {
                    if !first {
                        self.push(", ");
                    }
                    self.push("...");
                    self.emit_pattern(&rest.argument);
                }
                self.push(" }");
            }
            Pattern::Assignment(a) => {
                self.emit_pattern(&a.left);
                self.push(" = ");
                self.emit_expr(&a.right, P_ASSIGN);
            }
            Pattern::Rest(r) => {
                self.push("...");
                self.emit_pattern(&r.argument);
            }
        }
    }

    // ==================================================================
    // 表达式（优先级补括号）
    // ==================================================================

    fn emit_expr(&mut self, expr: &Expression, min_prec: u8) {
        // Variable inlining (Phase 2.4): if the identifier resolves to a single-use
        // pure variable, emit its initializer expression directly.
        if let Expression::Identifier(id) = expr {
            if let Some(ctx) = self.minify_ctx
                && let Some(inline_expr) = ctx.inline_vars.get(&id.name)
            {
                let prec = expr_precedence(inline_expr);
                let parens = prec < min_prec;
                if parens {
                    self.push("(");
                }
                self.emit_expr_inner(inline_expr);
                if parens {
                    self.push(")");
                }
                return;
            }
        }
        let prec = expr_precedence(expr);
        let parens = prec < min_prec;
        if parens {
            self.push("(");
        }
        self.emit_expr_inner(expr);
        if parens {
            self.push(")");
        }
    }

    fn emit_expr_inner(&mut self, expr: &Expression) {
        // 若 minify 引擎已确定该表达式的常量值，直接发射折叠结果。
        if let Some(ctx) = self.minify_ctx {
            if let Some(val) = ctx.constants.get(&expr.span()) {
                self.push(&val.to_source());
                return;
            }
            if let Some(replacement) = ctx.expression_replacements.get(&expr.span()) {
                self.push(replacement);
                return;
            }
        }
        match expr {
            Expression::NumberLiteral(n) => {
                if self.minify {
                    self.push(&write_number_minified(n.value));
                } else {
                    write_number(&mut self.out, n.value);
                }
            }
            Expression::StringLiteral(s) => self.emit_string_atom(s.value),
            Expression::BooleanLiteral(b) => {
                if self.minify {
                    self.push(if b.value { "!0" } else { "!1" });
                } else {
                    self.push(if b.value { "true" } else { "false" });
                }
            }
            Expression::NullLiteral(_) => self.push("null"),
            Expression::BigIntLiteral(b) => {
                self.push_name(b.raw);
                self.push("n");
            }
            Expression::RegExpLiteral(r) => {
                self.push("/");
                self.push_name(r.pattern);
                self.push("/");
                self.push_name(r.flags);
            }
            Expression::TemplateLiteral(t) => self.emit_template(t),
            Expression::Identifier(id) => {
                if self.minify
                    && let Some(ctx) = self.minify_ctx
                    && ctx.no_undefined_shadow
                    && self.interner.resolve(id.name) == "undefined"
                {
                    self.push("void 0");
                } else {
                    self.push_ident(id);
                }
            }
            Expression::This(_) => self.push("this"),
            Expression::Super(_) => self.push("super"),
            Expression::MetaProperty(m) => {
                self.push_name(m.meta);
                self.push(".");
                self.push_name(m.property);
            }
            Expression::Array(a) => {
                self.push("[");
                for (i, el) in a.elements.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    if let Some(e) = el {
                        self.emit_expr(e, P_ASSIGN);
                    }
                }
                self.push("]");
            }
            Expression::Object(o) => self.emit_object(o),
            Expression::Function(f) => self.emit_function(f),
            Expression::Arrow(a) => self.emit_arrow(a),
            Expression::Class(c) => self.emit_class(c),
            Expression::Unary(u) => {
                if let Some(ctx) = self.minify_ctx
                    && ctx.double_not_spans.contains(&u.span)
                {
                    if let Expression::Unary(inner) = &u.argument {
                        self.emit_expr(&inner.argument, P_UNARY);
                        return;
                    }
                }
                let op = u.operator.as_str();
                self.push(op);
                if op.len() > 1 {
                    self.push(" "); // typeof/void/delete
                }
                self.emit_expr(&u.argument, P_UNARY);
            }
            Expression::Update(u) => {
                if u.prefix {
                    self.push(u.operator.as_str());
                    self.emit_expr(&u.argument, P_UNARY);
                } else {
                    self.emit_expr(&u.argument, P_POSTFIX);
                    self.push(u.operator.as_str());
                }
            }
            Expression::Binary(b) => {
                let prec = binary_prec(b.operator);
                // `**` 右结合：左操作数同级需括号；其余左结合：右操作数同级需括号。
                let (left_min, right_min) = if b.operator == BinaryOperator::Exp {
                    (prec + 1, prec)
                } else {
                    (prec, prec + 1)
                };
                self.emit_expr(&b.left, left_min);
                self.push(" ");
                self.push(b.operator.as_str());
                self.push(" ");
                self.emit_expr(&b.right, right_min);
            }
            Expression::Logical(l) => {
                let prec = logical_prec(l.operator);
                self.emit_expr(&l.left, prec);
                self.push(" ");
                self.push(l.operator.as_str());
                self.push(" ");
                self.emit_expr(&l.right, prec + 1);
            }
            Expression::Assignment(a) => {
                self.emit_expr(&a.left, P_CALL_MEMBER);
                self.push(" ");
                self.push(a.operator.as_str());
                self.push(" ");
                self.emit_expr(&a.right, P_ASSIGN);
            }
            Expression::Conditional(c) => {
                self.emit_expr(&c.test, P_CONDITIONAL + 1);
                self.push(" ? ");
                self.emit_expr(&c.consequent, P_ASSIGN);
                self.push(" : ");
                self.emit_expr(&c.alternate, P_ASSIGN);
            }
            Expression::Call(c) => {
                if self.linker.is_some() && self.emit_require_call(c) {
                    // require("x") 已改写为 __wake_require__(id)
                } else {
                    self.emit_expr(&c.callee, P_CALL_MEMBER);
                    if c.optional {
                        self.push("?.");
                    }
                    self.emit_arguments(&c.arguments);
                }
            }
            Expression::New(n) => {
                self.push("new ");
                self.emit_expr(&n.callee, P_CALL_MEMBER);
                self.emit_arguments(&n.arguments);
            }
            Expression::Member(m) => match self.match_define(m) {
                // 命中 define（如 `process.env.NODE_ENV`）→ 直接发字面量（字面量是 primary，无需补括号）。
                Some(lit) => self.push(lit),
                None => self.emit_member(m),
            },
            Expression::Sequence(s) => {
                for (i, e) in s.expressions.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.emit_expr(e, P_ASSIGN);
                }
            }
            Expression::TaggedTemplate(t) => {
                self.emit_expr(&t.tag, P_CALL_MEMBER);
                self.emit_template(t.quasi);
            }
            Expression::Spread(s) => {
                self.push("...");
                self.emit_expr(&s.argument, P_ASSIGN);
            }
            Expression::Await(a) => {
                self.push("await ");
                self.emit_expr(&a.argument, P_UNARY);
            }
            Expression::Yield(y) => {
                self.push("yield");
                if y.delegate {
                    self.push("*");
                }
                if let Some(a) = &y.argument {
                    self.push(" ");
                    self.emit_expr(a, P_ASSIGN);
                }
            }
            Expression::Import(i) => {
                if let Some(linker) = self.linker
                    && let Expression::StringLiteral(s) = &i.source
                {
                    let spec = self.name(s.value);
                    if let Some(cid) = linker.dynamic_chunk(&spec) {
                        // 代码分割：目标在独立 async/shared chunk → 懒加载再取命名空间。
                        // 只发数字 chunkId 与模块 id（无文件名 → 内容 hash 无环）。
                        let id = linker
                            .module_id(&spec)
                            .expect("split dynamic import target must resolve to a module id");
                        let req_fn = linker.require_fn();
                        self.push(&format!("{req_fn}.import({cid}, {id})"));
                    } else {
                        // 未分割 / 外部：与既有实现逐字节一致。
                        let req = self.require_expr(&spec);
                        self.push("Promise.resolve(");
                        self.push(&req);
                        self.push(")");
                    }
                } else {
                    self.push("import(");
                    self.emit_expr(&i.source, P_ASSIGN);
                    if let Some(o) = &i.options {
                        self.push(", ");
                        self.emit_expr(o, P_ASSIGN);
                    }
                    self.push(")");
                }
            }
        }
    }

    /// 若成员表达式是一条命中 define 表的静态访问链（如 `process.env.NODE_ENV`），
    /// 返回其替换字面量源码；否则 None。
    fn match_define(&self, m: &MemberExpression) -> Option<&'d str> {
        if self.define.is_empty() || m.optional {
            return None;
        }
        let MemberProperty::Ident(prop) = &m.property else {
            return None;
        };
        // 零分配快速否决：叶名 Atom 不在 define 叶集合里（绝大多数成员访问在此返回，不建链、不 resolve）。
        if !self.define_leaves.contains(&prop.name) {
            return None;
        }
        let mut chain = String::new();
        if !self.build_static_chain(&m.object, &mut chain) {
            return None;
        }
        chain.push('.');
        self.interner
            .with_resolved(prop.name, |s| chain.push_str(s));
        self.define
            .iter()
            .find(|(k, _)| *k == chain)
            .map(|(_, v)| *v)
    }

    /// 编译期求值一个表达式的**真值性**（M4b if/条件折叠用）。
    /// 委托给 `wake_ecma_minify` 的统一引擎。无法确定 → `None`。
    fn const_eval_bool(&self, e: &Expression) -> Option<bool> {
        let ctx = wake_ecma_minify::const_eval::ConstCtx {
            defines: self.define,
            known_vars: &[],
            interner: Some(self.interner),
        };
        wake_ecma_minify::const_eval::const_eval_bool(e, &ctx)
    }

    /// 重建纯静态成员访问链（`a.b.c`）到 `out`；遇到 optional/computed/private/非标识符起点即失败。
    fn build_static_chain(&self, e: &Expression, out: &mut String) -> bool {
        match e {
            Expression::Identifier(id) => {
                self.interner.with_resolved(id.name, |s| out.push_str(s));
                true
            }
            Expression::Member(m) if !m.optional => match &m.property {
                MemberProperty::Ident(p) => {
                    if !self.build_static_chain(&m.object, out) {
                        return false;
                    }
                    out.push('.');
                    self.interner.with_resolved(p.name, |s| out.push_str(s));
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn emit_member(&mut self, m: &MemberExpression) {
        if !m.optional
            && let Some(ctx) = self.minify_ctx
            && let Some(dot_name) = ctx.bracket_to_dot.get(&m.span)
        {
            self.emit_expr(&m.object, P_CALL_MEMBER);
            self.push(".");
            self.push(dot_name);
            return;
        }
        self.emit_expr(&m.object, P_CALL_MEMBER);
        match &m.property {
            MemberProperty::Ident(id) => {
                self.push(if m.optional { "?." } else { "." });
                if !m.optional
                    && let Some(map) = self.prop_rename
                    && let Some(&nn) = map.get(&id.span)
                {
                    self.push_name(nn);
                } else {
                    self.push_name(id.name);
                }
            }
            MemberProperty::Private(id) => {
                self.push(if m.optional { "?.#" } else { ".#" });
                self.push_name(id.name);
            }
            MemberProperty::Computed(e) => {
                if m.optional {
                    self.push("?.");
                }
                self.push("[");
                self.emit_expr(e, P_SEQUENCE);
                self.push("]");
            }
        }
    }

    fn emit_arguments(&mut self, args: &AVec<Expression>) {
        self.push("(");
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            self.emit_expr(arg, P_ASSIGN);
        }
        self.push(")");
    }

    fn emit_object(&mut self, o: &ObjectExpression) {
        if o.properties.is_empty() {
            self.push("{}");
            return;
        }
        self.push("{ ");
        for (i, m) in o.properties.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            match m {
                ObjectMember::Spread(s) => {
                    self.push("...");
                    self.emit_expr(&s.argument, P_ASSIGN);
                }
                ObjectMember::Property(p) => self.emit_object_property(p),
            }
        }
        self.push(" }");
    }

    fn emit_object_property(&mut self, p: &ObjectProperty) {
        if (p.method || matches!(p.kind, PropertyKind::Get | PropertyKind::Set))
            && let Expression::Function(f) = &p.value
        {
            if f.is_async {
                self.push("async ");
            }
            if f.is_generator {
                self.push("*");
            }
            match p.kind {
                PropertyKind::Get => self.push("get "),
                PropertyKind::Set => self.push("set "),
                _ => {}
            }
            self.emit_property_key(&p.key, p.computed);
            self.emit_params(&f.params);
            self.push(" ");
            match f.body {
                Some(b) => self.emit_block(&b.statements),
                None => self.push("{}"),
            }
            return;
        }
        // mangle 后 value 名改变时展开 shorthand，避免连带改掉属性名。
        if p.shorthand && !p.computed && !self.value_ident_renamed(&p.value) {
            self.emit_expr(&p.value, P_ASSIGN);
        } else if !p.computed
            && !p.method
            && p.kind == PropertyKind::Init
            && !p.shorthand
            && let PropertyKey::Ident(id) = &p.key
            && let Some(map) = self.prop_rename
            && let Some(&nn) = map.get(&id.span)
        {
            self.push_name(nn);
            self.push(": ");
            self.emit_expr(&p.value, P_ASSIGN);
        } else {
            self.emit_property_key(&p.key, p.computed);
            self.push(": ");
            self.emit_expr(&p.value, P_ASSIGN);
        }
    }

    fn emit_arrow(&mut self, a: &ArrowFunction) {
        if a.is_async {
            self.push("async ");
        }
        self.emit_params(&a.params);
        self.push(" => ");
        match a.body {
            ArrowBody::Block(b) => self.emit_block(&b.statements),
            ArrowBody::Expression(e) => {
                let needs = matches!(e, Expression::Object(_));
                if needs {
                    self.push("(");
                }
                self.emit_expr(&e, P_ASSIGN);
                if needs {
                    self.push(")");
                }
            }
        }
    }

    fn emit_template(&mut self, t: &TemplateLiteral) {
        self.push("`");
        for (i, q) in t.quasis.iter().enumerate() {
            self.push_name(q.raw);
            if i < t.expressions.len() {
                self.push("${");
                self.emit_expr(&t.expressions[i], P_SEQUENCE);
                self.push("}");
            }
        }
        self.push("`");
    }

    fn emit_string_atom(&mut self, atom: Atom) {
        // 零分配：借用驻留切片，转义直接写进 out（闭包不回调 interner，无重入）。
        let interner = self.interner;
        let out = &mut self.out;
        out.push('"');
        interner.with_resolved(atom, |s| {
            for ch in s.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\u{2028}' => out.push_str("\\u2028"),
                    '\u{2029}' => out.push_str("\\u2029"),
                    c if (c as u32) < 0x20 => {
                        let _ = write!(out, "\\x{:02x}", c as u32);
                    }
                    c => out.push(c),
                }
            }
        });
        out.push('"');
    }
}

/// Extract the return value expression from a single-return statement (possibly wrapped in a block).
fn extract_return_expression<'a>(stmt: &'a Statement<'a>) -> Option<&'a Expression<'a>> {
    let ret = match stmt {
        Statement::Return(r) => r,
        Statement::Block(b) if b.body.len() == 1 => match &b.body[0] {
            Statement::Return(r) => r,
            _ => return None,
        },
        _ => return None,
    };
    ret.argument.as_ref()
}

/// 表达式起始是否会与语句上下文冲突（`{`→块、`function`/`class`→声明）。
fn starts_with_problematic(expr: &Expression) -> bool {
    match expr {
        Expression::Object(_) | Expression::Function(_) | Expression::Class(_) => true,
        Expression::Assignment(a) => starts_with_problematic(&a.left),
        Expression::Binary(b) => starts_with_problematic(&b.left),
        Expression::Logical(l) => starts_with_problematic(&l.left),
        Expression::Member(m) => starts_with_problematic(&m.object),
        Expression::Call(c) => starts_with_problematic(&c.callee),
        Expression::Conditional(c) => starts_with_problematic(&c.test),
        Expression::Sequence(s) => s.expressions.first().is_some_and(starts_with_problematic),
        Expression::TaggedTemplate(t) => starts_with_problematic(&t.tag),
        _ => false,
    }
}

/// 收集一个绑定模式里所有标识符名 Atom（用于 `export const {a, b} = ...` 的导出赋值）。
fn collect_pattern_names(pat: &Pattern, out: &mut Vec<Atom>) {
    match pat {
        Pattern::Ident(id) => out.push(id.name),
        Pattern::Array(a) => {
            for el in a.elements.iter().flatten() {
                collect_pattern_names(el, out);
            }
        }
        Pattern::Object(o) => {
            for p in o.properties.iter() {
                collect_pattern_names(&p.value, out);
            }
            if let Some(r) = &o.rest {
                collect_pattern_names(&r.argument, out);
            }
        }
        Pattern::Assignment(a) => collect_pattern_names(&a.left, out),
        Pattern::Rest(r) => collect_pattern_names(&r.argument, out),
    }
}

/// 取模块导出名的 Atom（`Ident` 或 `String` 形式都持有 Atom）。用于 shake 判定（无分配）。
fn module_export_name_atom(n: &ModuleExportName) -> Atom {
    match n {
        ModuleExportName::Ident(id) => id.name,
        ModuleExportName::String(a) => *a,
    }
}

fn same_name(a: &ModuleExportName, b: &ModuleExportName) -> bool {
    match (a, b) {
        (ModuleExportName::Ident(x), ModuleExportName::Ident(y)) => x.name == y.name,
        (ModuleExportName::String(x), ModuleExportName::String(y)) => x == y,
        _ => false,
    }
}

// ======================================================================
// Tree Shaking 辅助（PLAN §6.6）
// ======================================================================

/// 收集模块内**读取位置**出现的标识符名（不含绑定位置）。
///
/// 只在 `Expression::Identifier` 处采集——绑定（Pattern）走 `visit_ident` 默认空实现，不计入。
/// 用于判断「某导出声明是否还被模块内部引用」。会因遮蔽而**高估**（把内层同名局部也算引用），
/// 但高估只会让我们**多保留**，是安全方向。
struct ReadCollector {
    reads: FxHashSet<Atom>,
}

impl<'a> Visit<'a> for ReadCollector {
    fn visit_expression(&mut self, node: &Expression<'a>) {
        if let Expression::Identifier(id) = node {
            // 直接插 Atom：每个读取标识符省一次 interner 锁 + String 分配（此遍历是 shaken 构建的热点）。
            self.reads.insert(id.name);
        }
        walk_expression(self, node);
    }
}

fn collect_reads(program: &Program) -> FxHashSet<Atom> {
    let mut c = ReadCollector {
        reads: FxHashSet::default(),
    };
    c.visit_program(program);
    c.reads
}

/// Collect all identifier reads within a single statement (for per-declaration read tracking).
fn collect_reads_in_statement(stmt: &Statement, reads: &mut FxHashSet<Atom>) {
    struct StmtReadCollector<'a> {
        reads: &'a mut FxHashSet<Atom>,
    }
    impl<'a, 'ast> Visit<'ast> for StmtReadCollector<'a> {
        fn visit_expression(&mut self, node: &Expression<'ast>) {
            if let Expression::Identifier(id) = node {
                self.reads.insert(id.name);
            }
            walk_expression(self, node);
        }
    }
    let mut c = StmtReadCollector { reads };
    c.visit_statement(stmt);
}

/// 一条声明语句是否**无副作用**（移除它不改变程序行为）。
fn decl_is_pure(stmt: &Statement) -> bool {
    match stmt {
        // 函数声明不在定义时执行，永远无副作用。
        Statement::FunctionDeclaration(_) => true,
        Statement::ClassDeclaration(c) => class_is_pure(c),
        // var/let/const：所有初始化器都无副作用才可移除。
        Statement::VariableDeclaration(d) => d
            .declarations
            .iter()
            .all(|decl| decl.init.as_ref().is_none_or(expr_is_pure)),
        _ => false,
    }
}

/// 类**定义时**是否无副作用：无静态块、无静态字段初始化、无（含副作用的）计算键、父类纯。
fn class_is_pure(c: &Class) -> bool {
    if let Some(sc) = &c.super_class
        && !expr_is_pure(sc)
    {
        return false;
    }
    for member in c.body.iter() {
        match member {
            // 静态初始化块在定义时执行 → 视为有副作用。
            ClassMember::StaticBlock(_) => return false,
            ClassMember::Method(m) => {
                if m.computed && !key_is_pure(&m.key) {
                    return false;
                }
            }
            ClassMember::Property(p) => {
                if p.computed && !key_is_pure(&p.key) {
                    return false;
                }
                // 静态字段初始化器在定义时执行。
                if p.is_static
                    && let Some(v) = &p.value
                    && !expr_is_pure(v)
                {
                    return false;
                }
            }
        }
    }
    true
}

fn key_is_pure(key: &PropertyKey) -> bool {
    match key {
        PropertyKey::Computed(e) => expr_is_pure(e),
        _ => true,
    }
}



/// 语句（含嵌套块 / if）是否含**提升**声明（`var` / 函数声明）。M4b 折叠时用于守卫**被丢弃**分支：
/// 含提升声明则不折叠（保守但安全——丢弃提升绑定会致 ReferenceError）。
/// 不下钻函数/箭头表达式体（其内 `var` 不外提）；未精确处理的控制流（For/Switch/Try…）保守判 `true`。
fn has_hoisted_decl(stmt: &Statement) -> bool {
    match stmt {
        Statement::VariableDeclaration(d) => d.kind == VarKind::Var,
        Statement::FunctionDeclaration(_) => true,
        Statement::Block(b) => b.body.iter().any(has_hoisted_decl),
        Statement::If(s) => {
            has_hoisted_decl(&s.consequent) || s.alternate.as_ref().is_some_and(has_hoisted_decl)
        }
        // 不提升的叶子语句：安全折叠。
        Statement::Expression(_)
        | Statement::Empty(_)
        | Statement::Return(_)
        | Statement::Break(_)
        | Statement::Continue(_)
        | Statement::Throw(_)
        | Statement::Debugger(_)
        | Statement::ClassDeclaration(_) => false,
        // 其余控制流保守视为可能含提升声明 → 不折叠。
        _ => true,
    }
}

/// Collect all `VariableDeclaration` nodes whose span is in `hoisted_flat`,
/// recursing into containers (block, if, for, …) but stopping at nested
/// function / class boundaries.
fn collect_hoisted_var_decls<'a>(
    stmts: &'a [Statement<'a>],
    hoisted_flat: &FxHashSet<Span>,
) -> Vec<&'a VariableDeclaration<'a>> {
    let mut out = Vec::new();
    walk_collect_hoisted(stmts, hoisted_flat, &mut out);
    out
}

fn walk_collect_hoisted<'a>(
    stmts: &'a [Statement<'a>],
    hoisted_flat: &FxHashSet<Span>,
    out: &mut Vec<&'a VariableDeclaration<'a>>,
) {
    for stmt in stmts {
        match stmt {
            Statement::VariableDeclaration(d) if hoisted_flat.contains(&d.span) => {
                out.push(d);
            }
            // Descend into unconditional containers
            Statement::Block(b) => walk_collect_hoisted(&b.body, hoisted_flat, out),
            Statement::Labeled(s) => {
                walk_collect_hoisted(std::slice::from_ref(&s.body), hoisted_flat, out);
            }
            Statement::With(s) => {
                walk_collect_hoisted(std::slice::from_ref(&s.body), hoisted_flat, out);
            }
            // If branches / switch / try (conditional but hoisted by plan)
            Statement::If(s) => {
                walk_collect_hoisted(std::slice::from_ref(&s.consequent), hoisted_flat, out);
                if let Some(alt) = &s.alternate {
                    walk_collect_hoisted(std::slice::from_ref(alt), hoisted_flat, out);
                }
            }
            Statement::Switch(s) => {
                for case in s.cases.iter() {
                    walk_collect_hoisted(&case.consequent, hoisted_flat, out);
                }
            }
            Statement::Try(s) => {
                walk_collect_hoisted(&s.block.body, hoisted_flat, out);
                if let Some(h) = &s.handler {
                    walk_collect_hoisted(&h.body.body, hoisted_flat, out);
                }
                if let Some(f) = &s.finalizer {
                    walk_collect_hoisted(&f.body, hoisted_flat, out);
                }
            }
            // Loop bodies — consistent with plan_hoist (no vars collected from loops)
            Statement::For(_) | Statement::ForIn(_) | Statement::ForOf(_)
            | Statement::While(_) | Statement::DoWhile(_) => {}
            // Stop at scope boundaries
            Statement::FunctionDeclaration(_) | Statement::ClassDeclaration(_) => {}
            _ => {}
        }
    }
}

fn expr_is_pure(e: &Expression) -> bool {
    use Expression::*;
    match e {
        NumberLiteral(_) | StringLiteral(_) | BooleanLiteral(_) | NullLiteral(_)
        | BigIntLiteral(_) | RegExpLiteral(_) | Identifier(_) | This(_) | Super(_)
        | MetaProperty(_) | Function(_) | Arrow(_) => true,
        Class(c) => class_is_pure(c),
        TemplateLiteral(t) => t.expressions.iter().all(expr_is_pure),
        Array(a) => a.elements.iter().flatten().all(expr_is_pure),
        Object(o) => o.properties.iter().all(|m| match m {
            ObjectMember::Property(p) => key_is_pure(&p.key) && expr_is_pure(&p.value),
            // 展开可能触发迭代器 / getter → 视为有副作用。
            ObjectMember::Spread(_) => false,
        }),
        // delete 有副作用；其余一元（typeof/void/!/~/+/-）在参数纯时纯。
        Unary(u) => u.operator != UnaryOperator::Delete && expr_is_pure(&u.argument),
        Binary(b) => expr_is_pure(&b.left) && expr_is_pure(&b.right),
        Logical(l) => expr_is_pure(&l.left) && expr_is_pure(&l.right),
        Conditional(c) => {
            expr_is_pure(&c.test) && expr_is_pure(&c.consequent) && expr_is_pure(&c.alternate)
        }
        Sequence(s) => s.expressions.iter().all(expr_is_pure),
        // Member（可能触发 getter）、Call、New、Assignment、Update、Await、Yield、
        // Import、Spread、TaggedTemplate → 保守判为有副作用。
        _ => false,
    }
}

fn expr_precedence(expr: &Expression) -> u8 {
    match expr {
        Expression::Sequence(_) => P_SEQUENCE,
        Expression::Assignment(_) | Expression::Arrow(_) | Expression::Yield(_) => P_ASSIGN,
        Expression::Conditional(_) => P_CONDITIONAL,
        Expression::Logical(l) => logical_prec(l.operator),
        Expression::Binary(b) => binary_prec(b.operator),
        Expression::Unary(_) | Expression::Await(_) | Expression::Spread(_) => P_UNARY,
        Expression::Update(u) => {
            if u.prefix {
                P_UNARY
            } else {
                P_POSTFIX
            }
        }
        Expression::Call(_)
        | Expression::New(_)
        | Expression::Member(_)
        | Expression::TaggedTemplate(_) => P_CALL_MEMBER,
        _ => P_PRIMARY,
    }
}

fn binary_prec(op: BinaryOperator) -> u8 {
    use BinaryOperator::*;
    match op {
        BitOr => P_BIT_OR,
        BitXor => P_BIT_XOR,
        BitAnd => P_BIT_AND,
        Eq | NotEq | StrictEq | StrictNotEq => P_EQUALITY,
        Lt | Gt | LtEq | GtEq | In | Instanceof => P_RELATIONAL,
        Shl | Shr | Ushr => P_SHIFT,
        Add | Sub => P_ADDITIVE,
        Mul | Div | Rem => P_MULTIPLICATIVE,
        Exp => P_EXPONENT,
    }
}

fn logical_prec(op: LogicalOperator) -> u8 {
    match op {
        LogicalOperator::Or => P_LOGICAL_OR,
        LogicalOperator::And => P_LOGICAL_AND,
        LogicalOperator::Coalesce => P_COALESCE,
    }
}

/// 数字字面量格式化（f64 → 最短往返表示），直接写入输出缓冲，省去每个字面量一次临时 String。
fn write_number(out: &mut String, v: f64) {
    if v == v.trunc() && v.abs() < 1e21 && v.is_finite() {
        let _ = write!(out, "{}", v as i64);
    } else {
        let _ = write!(out, "{v}");
    }
}

#[cfg(test)]
mod tests;
