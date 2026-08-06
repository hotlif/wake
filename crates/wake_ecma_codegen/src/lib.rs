//! # wake_ecma_codegen — 代码生成（AST → JS 字符串）
//!
//! DESIGN §4.6：直接从 AST 写字符串，维护运算符优先级/结合性表自动补括号。
//!
//! SourceMap（[`sourcemap`] 模块）：[`codegen_module_shaken_with_map`] 在发射时采集
//! 「产物行列 ↔ 源字节偏移」映射；不请求时零开销，且产物逐字节不变。当前覆盖**非压缩**
//! 路径（压缩路径由 bundler 做 scope hoisting 并改写模块体文本，映射需另行平移，见 M4d）。
//!
//! 入口：[`codegen`]（默认 dev 可读风格）。往返 `parse → codegen → parse` 语义等价（见测试）。

use std::fmt::Write as _;

mod decorators;

use wake_common::{Atom, FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::*;
use wake_ecma_minify::{IfReturnCandidate, MinifyCtx, write_number_minified};

pub mod sourcemap;
pub use sourcemap::{Mapping, ModuleMappings, SourceMap};

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
        program_reads: collect_reads(program),
        minify: false,
        rename: None,
        module_renames: FxHashMap::default(),
        prop_rename: None,
        minify_ctx: None,
        no_esmodule: false,
        minify_names: false,
        skip_inline: false,
        smap: None,
        needs_decorator_helpers: std::cell::Cell::new(false),
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
pub fn codegen_module(
    program: &Program,
    interner: &Interner,
    linker: &dyn ModuleLinker,
    minify_names: bool,
) -> String {
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
/// 旧实现使用 webpack `mode`/`DefinePlugin` 决定 `process.env.NODE_ENV` 等常量；wake 由此接入：
/// prod 传 `[("process.env.NODE_ENV", "\"production\"")]` + 用户 `[define]`，dev 传 `"development"`
/// （WAKE-COMPATIBILITY §M3）。`define` 的每项为「静态成员链 → 字面量**源码**」（值含引号自便）。
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
/// `key: value` 以免改变属性名（WAKE-COMPATIBILITY §M4）。`None` = 不重命名。
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
    codegen_impl(
        program,
        interner,
        linker,
        keep_exports,
        define,
        minify,
        rename,
        minify_ctx,
        no_esmodule,
        minify_names,
        false,
    )
    .0
}

/// 同 [`codegen_module_shaken_mangled`]，但**同时产出模块级 SourceMap 映射**（WAKE-COMPATIBILITY §M4d）。
///
/// 返回 `(模块体源码, 映射)`。映射的产物坐标是**模块体内的局部坐标**（0 基行、0 基 UTF-16 列），
/// `src_index` 恒为 0——bundler 把模块体拼进 bundle 时按行偏移平移、并重写为真实源文件下标。
/// 源侧只记字节偏移（`Span::lo`），行列换算推迟到序列化（DESIGN §4.1：热路径不算行列）。
///
/// 与不带 map 的版本相比，仅多出游标累计与映射 push；`smap` 为 `None` 时零开销，故现有调用点不受影响。
#[allow(clippy::too_many_arguments)]
pub fn codegen_module_shaken_with_map(
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
) -> (String, ModuleMappings) {
    let (code, map) = codegen_impl(
        program,
        interner,
        linker,
        keep_exports,
        define,
        minify,
        rename,
        minify_ctx,
        no_esmodule,
        minify_names,
        true,
    );
    (code, map.unwrap_or_default())
}

/// [`codegen_module_shaken_mangled`] 与 [`codegen_module_shaken_with_map`] 的共同实现。
#[allow(clippy::too_many_arguments)]
fn codegen_impl(
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
    want_map: bool,
) -> (String, Option<ModuleMappings>) {
    let shake = keep_exports.map(|keep| {
        let used: FxHashSet<Atom> = keep.iter().map(|s| interner.intern(s)).collect();
        ShakeCtx {
            used_locals: collect_used_export_locals(program, &used),
            // 外部已用导出名预驻留为 Atom（与导出名 Atom 同 interner，u32 相等 ⟺ 字符串相等）。
            used,
            internal_reads: collect_reads(program),
            default_atom: interner.intern("default"),
        }
    });
    let prop_rename = minify_ctx.and_then(|ctx| ctx.prop_rename);
    let module_renames = collect_module_renames(program, rename);
    let mut cg = Codegen {
        out: String::new(),
        interner,
        indent: 0,
        linker: Some(linker),
        link_tmp: 0,
        define,
        define_leaves: define_leaf_atoms(define, interner),
        shake,
        program_reads: collect_reads(program),
        minify,
        rename,
        module_renames,
        prop_rename,
        minify_ctx,
        no_esmodule,
        minify_names,
        skip_inline: false,
        smap: want_map.then(|| SmapState {
            line: 0,
            col: 0,
            mappings: Vec::new(),
            last_src: None,
        }),
        needs_decorator_helpers: std::cell::Cell::new(false),
    };
    // ESM 模块（含 import/export 语法）标记 `__esModule`，供默认导入 interop 区分「转译 ESM」
    // 与「纯 CJS」。纯 CJS 模块（只有 `module.exports`/`require`）不标记，保持整体 exports 语义。
    // 单包模式下 `no_esmodule` 为 true 时省略此标记（bundler 静态处理 interop，见 emit）。
    if program_is_esm(program) && !cg.no_esmodule {
        cg.push("Object.defineProperty(exports, \"__esModule\", { value: true });");
        cg.newline();
    }
    cg.emit_program(program);
    let map = cg.smap.map(|sm| ModuleMappings {
        mappings: sm.mappings,
    });
    (cg.out, map)
}

/// Tree Shaking 上下文：外部已用导出名 + 模块内被读取的标识符名（全部用 `Atom`，u32 比较，无分配）。
struct ShakeCtx {
    /// 外部（其它模块）真正 import 的导出名 Atom（含 `"default"`，见 [`ShakeCtx::default_atom`]）。
    used: FxHashSet<Atom>,
    /// Local bindings behind a live `export { local as public }` specifier.
    used_locals: FxHashSet<Atom>,
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

    fn is_local_live(&self, name: Atom) -> bool {
        self.is_used(name) || self.used_locals.contains(&name) || self.is_read(name)
    }
}

/// 单包 concat 的块安全信息：模块能否用裸 `{}` 块（而非 IIFE）包裹。
#[derive(Clone, Copy, Debug)]
pub struct ConcatBlockInfo {
    /// 模块含 ESM 语法（import/export）——ESM 恒 strict-safe，是加 `"use strict"` 的前提。
    pub is_esm: bool,
    /// 块安全：ESM 且**无任何 `var`、无任何 `this`**。满足则可用 `{}` 块隔离（strict 下块级函数声明
    /// 亦为块作用域，避免跨模块顶层名碰撞）。过近似（函数体内的 var/this 也一并否决）→ 只多退回 IIFE，
    /// 绝不误用 `{}`：① `var` 会 hoist 出块致碰撞；② 顶层 `this` 在 `{}`（=module.exports）与 IIFE
    /// （strict 下 =undefined，符合 ESM）语义不同。
    pub block_safe: bool,
}

/// 扫描模块：是否 ESM、块是否安全（无 `var`、无 `this`）。
pub fn concat_block_info(program: &Program) -> ConcatBlockInfo {
    struct Scan {
        has_var: bool,
        has_this: bool,
    }
    impl<'a> Visit<'a> for Scan {
        fn visit_statement(&mut self, s: &Statement<'a>) {
            if let Statement::VariableDeclaration(d) = s
                && d.kind == VarKind::Var
            {
                self.has_var = true;
            }
            walk_statement(self, s);
        }
        fn visit_expression(&mut self, e: &Expression<'a>) {
            if matches!(e, Expression::This(_)) {
                self.has_this = true;
            }
            walk_expression(self, e);
        }
    }
    let is_esm = program_is_esm(program);
    let mut sc = Scan {
        has_var: false,
        has_this: false,
    };
    sc.visit_program(program);
    ConcatBlockInfo {
        is_esm,
        block_safe: is_esm && !sc.has_var && !sc.has_this,
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

/// minify 去空格后，相邻两 token 是否会错误粘连 → 需补一个空格。
/// 覆盖：标识符/关键字/数字相邻（`return`+`x`、`in`+`y`）；`++`/`--` 误合并；`//`、`/*` 误起注释。
#[inline]
fn need_sep(a: u8, b: u8) -> bool {
    #[inline]
    fn word(c: u8) -> bool {
        c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
    }
    (word(a) && word(b))
        || (a == b'+' && b == b'+')
        || (a == b'-' && b == b'-')
        || (a == b'/' && (b == b'/' || b == b'*'))
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
    /// All identifier reads in the module. This remains available when tree shaking is disabled,
    /// allowing codegen-only rewrites that must preserve locally referenced declaration names.
    program_reads: FxHashSet<Atom>,
    /// 紧凑（minify）输出：换行/缩进省略（语句均发显式 `;`/`}`，ASI 安全）。WAKE-COMPATIBILITY §M4a。
    minify: bool,
    /// 跳过 `__esModule` 定义（用于单包模式，bundler 静态处理 interop）。
    no_esmodule: bool,
    /// 标识符 mangling 侧表（`span → 新名`，`None` = 不重命名）。由 `wake_ecma_minify::plan_mangle`
    /// 构建、经调用方传入（codegen 属编译核心，不能反向依赖 parser 的语义分析）。WAKE-COMPATIBILITY §M4。
    /// 只在**变量引用/绑定**发射点（[`Codegen::push_ident`]）按 span 查表；属性名/成员名/导出名不查。
    rename: Option<&'m FxHashMap<Span, Atom>>,
    /// Module-scope binding rename fallback for synthetic export references whose span is DUMMY.
    module_renames: FxHashMap<Atom, Atom>,
    /// Property mangling side-table (span → new name).
    /// Built by `plan_prop_mangle`, consumed to shorten property names in
    /// member access expressions and object literal keys.
    prop_rename: Option<&'mc FxHashMap<Span, Atom>>,
    /// 来自 minifier 的分析上下文（常量折叠、纯性标注等）。`None` = 不使用 minify 引擎。
    minify_ctx: Option<&'mc MinifyCtx<'mc>>,
    /// Use short names for codegen output (e for exports, etc.) when true.
    minify_names: bool,
    /// 临时标记：当前 emit_expr 处于写位置（Update 参数 / Assignment 左侧），禁止变量内联。
    skip_inline: bool,
    /// 本模块是否发射了装饰器降级 → 需在模块顶部注入 `__esDecorate`/`__runInitializers`。
    needs_decorator_helpers: std::cell::Cell<bool>,
    /// SourceMap 采集（`None` = 不产 map，零开销）。WAKE-COMPATIBILITY §M4d。
    smap: Option<SmapState>,
}

/// codegen 期的产物位置游标 + 映射累积（仅在启用 sourcemap 时存在）。
struct SmapState {
    /// 当前产物行（0 基）。
    line: u32,
    /// 当前产物列（0 基，UTF-16 码元）。
    col: u32,
    /// 已记录的映射（按发射顺序，天然按产物位置递增）。
    mappings: Vec<Mapping>,
    /// 上一条映射的源字节偏移——相同源位置连续发射时去重，避免 mappings 膨胀。
    last_src: Option<u32>,
}

impl SmapState {
    /// 把一段已写入产物的文本计入行列游标（列按 UTF-16 码元）。
    fn advance(&mut self, s: &str) {
        // 按行拆分：最后一个换行之后的部分决定新列。
        match s.rfind('\n') {
            Some(nl) => {
                self.line += s.as_bytes().iter().filter(|&&b| b == b'\n').count() as u32;
                self.col = s[nl + 1..].chars().map(char::len_utf16).sum::<usize>() as u32;
            }
            None => {
                self.col += s.chars().map(char::len_utf16).sum::<usize>() as u32;
            }
        }
    }
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
        // minify 下相邻 token 去空格可能错误粘连（`in`+ident、`a`+`+`→`++`、`/`+`/`→注释）——
        // 在真正拼接前按前后字符补一个必要空格。pretty 模式不触发（末尾多为空格/换行）。
        if self.minify
            && let (Some(&a), Some(&b)) = (self.out.as_bytes().last(), s.as_bytes().first())
            && need_sep(a, b)
        {
            self.out.push(' ');
            if let Some(sm) = &mut self.smap {
                sm.col += 1;
            }
        }
        self.out.push_str(s);
        if let Some(sm) = &mut self.smap {
            sm.advance(s);
        }
    }

    fn push_name(&mut self, atom: Atom) {
        // 零分配：借用驻留切片直接拷进输出缓冲，省去 resolve 的临时 String。
        // 闭包只写 out、不回调 interner，无重入死锁风险。
        let minify = self.minify;
        let interner = self.interner;
        let out = &mut self.out;
        let smap = self.smap.as_mut();
        interner.with_resolved(atom, |s| {
            // 与 push 相同的 token 边界守卫：关键字/标识符紧邻标识符时补空格（`return`+`x`）。
            let mut pad = false;
            if minify
                && let (Some(&a), Some(&b)) = (out.as_bytes().last(), s.as_bytes().first())
                && need_sep(a, b)
            {
                out.push(' ');
                pad = true;
            }
            out.push_str(s);
            // 名字不含换行，直接按 UTF-16 码元累加列。
            if let Some(sm) = smap {
                sm.col += pad as u32 + s.chars().map(char::len_utf16).sum::<usize>() as u32;
            }
        });
    }

    /// 在当前产物位置记录一条指向 `span.lo` 的映射（源侧行列推迟到序列化换算）。
    ///
    /// 合成节点（`Span::DUMMY`）不记录——它不对应任何源码位置，映射过去会把调试器
    /// 指到文件开头。相同源偏移连续出现时去重，避免每个 token 都产生冗余段。
    #[inline]
    fn mark(&mut self, span: Span) {
        if span.is_dummy() {
            return;
        }
        let Some(sm) = &mut self.smap else { return };
        if sm.last_src == Some(span.lo) {
            return;
        }
        sm.last_src = Some(span.lo);
        // 同一产物位置只保留**最后**一条映射：被完全擦除的语句（如链接后消失的 `import`）会先在
        // 当前位置留下一条映射，而真正占据该位置的是其后的语句——后者必须覆盖前者，否则调试器
        // 会把该位置指回已消失的源语句。
        if let Some(last) = sm.mappings.last_mut()
            && last.gen_line == sm.line
            && last.gen_col == sm.col
        {
            last.src_offset = span.lo;
            return;
        }
        sm.mappings.push(Mapping {
            gen_line: sm.line,
            gen_col: sm.col,
            src_index: 0, // 模块内恒 0；bundler 合并时重写为真实源下标
            src_offset: span.lo,
        });
    }

    /// 可省的标点分隔符：pretty 原样发；minify 去掉**两端**空格（内部空格保留）。
    /// 仅用于纯标点分隔（`, ` `; ` ` = ` `{ ` ` }` `: ` ` ? ` ` : ` `) ` ` {` ` => ` 等）。
    /// 去空格后若与相邻 token 粘连，由 [`Codegen::push`] 的 [`need_sep`] 守卫兜底补回。
    fn punct(&mut self, pretty: &'static str) {
        if self.minify {
            self.push(pretty.trim_matches(' '));
        } else {
            self.push(pretty);
        }
    }

    /// 二元/逻辑/赋值运算符：pretty 两侧留空格；minify 下**词运算符**（`in`/`instanceof`）
    /// 仍两侧留空格，**标点运算符**裸发（`+ +`/`- -`/`/ /` 的 token 粘连由 push 守卫兜底）。
    fn binop(&mut self, op: &str) {
        if self.minify && !op.as_bytes()[0].is_ascii_alphabetic() {
            self.push(op);
        } else {
            self.push(" ");
            self.push(op);
            self.push(" ");
        }
    }

    /// 可省的单个空格：pretty 发一个空格，minify 省略。用于体块前 `) {` 之间等。
    fn sp(&mut self) {
        if !self.minify {
            self.out.push(' ');
            if let Some(sm) = &mut self.smap {
                sm.col += 1;
            }
        }
    }

    /// 发射一个**变量引用/绑定**标识符：若 mangling 侧表命中该 span，写新名，否则写原名。
    /// 只用于会被 mangle 的位置（标识符表达式、绑定模式、函数/类名）；属性名/成员名/导出名
    /// 走 [`Codegen::push_name`]，永不查表。
    fn push_ident(&mut self, ident: &Ident) {
        // SourceMap：标识符是列级定位的锚点（悬停求值、"跳转到定义"都依赖它）。
        self.mark(ident.span);
        // 合成节点（Span::DUMMY）永不参与 span 索引的侧表——DUMMY 是共享哨兵，
        // 用作身份键会跨节点碰撞（见 emit_expr_inner 常量表守卫）。
        if !ident.span.is_dummy()
            && let Some(map) = self.rename
            && let Some(&nn) = map.get(&ident.span)
        {
            self.push_name(nn);
            return;
        }
        if let Some(&nn) = self.module_renames.get(&ident.name) {
            self.push_name(nn);
            return;
        }
        self.push_name(ident.name);
    }

    /// 标识符最终发射出的名字（若被 mangle 则为新名）。
    fn ident_text(&self, ident: &Ident) -> String {
        if !ident.span.is_dummy()
            && let Some(map) = self.rename
            && let Some(&nn) = map.get(&ident.span)
        {
            return self.name(nn);
        }
        if let Some(&nn) = self.module_renames.get(&ident.name) {
            return self.name(nn);
        }
        self.name(ident.name)
    }

    /// 该 span 处标识符是否被 mangle 重命名。
    fn is_renamed(&self, span: Span) -> bool {
        !span.is_dummy() && self.rename.is_some_and(|m| m.contains_key(&span))
    }

    /// 对象字面量 shorthand `{ x }` 的 value 标识符是否被重命名——若是，须展开为 `x: 新名`，
    /// 否则会把属性名也一起改掉。
    #[allow(dead_code)] // 预留：非单包模式下用短名 `e` 指代 exports（当前单包路径统一走 `exports`→`$`）。
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
        if let Some(sm) = &mut self.smap {
            sm.line += 1;
            sm.col = self.indent as u32 * 2;
            // 换行后允许同一源位置再记一条映射（新行需要自己的段）。
            sm.last_src = None;
        }
    }

    // ==================================================================
    // 程序 / 语句
    // ==================================================================

    fn emit_program(&mut self, program: &Program) {
        let stmts = &program.body[..];
        let directive_count = stmts
            .iter()
            .take_while(|statement| {
                matches!(
                    statement,
                    Statement::Expression(expression)
                        if matches!(expression.expression, Expression::StringLiteral(_))
                )
            })
            .count();
        let needs_decorator_helpers = program_has_decorated_class(stmts);
        let has_runtime_helpers = program.spread_helper.is_some()
            || program.object_spread_helper.is_some()
            || program.for_of_helper.is_some()
            || needs_decorator_helpers;

        for (index, statement) in stmts[..directive_count].iter().enumerate() {
            if index > 0 {
                self.newline();
            }
            let Statement::Expression(directive) = statement else {
                unreachable!("directive prefix contains only string expression statements")
            };
            self.emit_directive(directive);
        }
        if directive_count > 0 && has_runtime_helpers {
            self.newline();
        }

        if let Some(helper) = program.spread_helper {
            self.push("function ");
            self.push_name(helper);
            self.push("(value, limit) {");
            self.newline();
            self.push("if (Array.isArray(value)) return limit === void 0 ? value.slice() : value.slice(0, limit);");
            self.newline();
            self.push(
                "if (value == null) throw new TypeError(\"Cannot spread null or undefined\");",
            );
            self.newline();
            self.push("var method = typeof Symbol !== \"undefined\" && value[Symbol.iterator];");
            self.newline();
            self.push("if (method) {");
            self.newline();
            self.push("var iterator = method.call(value), result = [], step, error, done = false;");
            self.newline();
            self.push("try { while (limit === void 0 || result.length < limit) { step = iterator.next(); if (step.done) { done = true; break; } result.push(step.value); } }");
            self.newline();
            self.push("catch (caught) { error = caught; }");
            self.newline();
            self.push("finally {");
            self.newline();
            self.push("try { if (!done && iterator.return) iterator.return(); }");
            self.newline();
            self.push("finally { if (error) throw error; }");
            self.newline();
            self.push("}");
            self.newline();
            self.push("return result;");
            self.newline();
            self.push("}");
            self.newline();
            self.push("if (typeof value === \"string\") {");
            self.newline();
            self.push("var chars = [], index = 0, first, second;");
            self.newline();
            self.push("while (index < value.length && (limit === void 0 || chars.length < limit)) { first = value.charCodeAt(index++); if (first >= 55296 && first <= 56319 && index < value.length) { second = value.charCodeAt(index); if (second >= 56320 && second <= 57343) { chars.push(value.slice(index - 1, ++index)); continue; } } chars.push(String.fromCharCode(first)); }");
            self.newline();
            self.push("return chars;");
            self.newline();
            self.push("}");
            self.newline();
            self.push("throw new TypeError(\"Value is not iterable\");");
            self.newline();
            self.push("}");
            self.newline();
        }

        if let Some(helper) = program.object_spread_helper {
            self.push("function ");
            self.push_name(helper);
            self.push("(target) {");
            self.newline();
            self.push("for (var sourceIndex = 1; sourceIndex < arguments.length; sourceIndex++) {");
            self.newline();
            self.push("var source = arguments[sourceIndex];");
            self.newline();
            self.push("if (source == null) continue;");
            self.newline();
            self.push("var keys = Object.keys(Object(source));");
            self.newline();
            self.push("if (typeof Object.getOwnPropertySymbols === \"function\") {");
            self.newline();
            self.push("var symbols = Object.getOwnPropertySymbols(source);");
            self.newline();
            self.push("for (var symbolIndex = 0; symbolIndex < symbols.length; symbolIndex++) if (Object.prototype.propertyIsEnumerable.call(source, symbols[symbolIndex])) keys.push(symbols[symbolIndex]);");
            self.newline();
            self.push("}");
            self.newline();
            self.push("for (var keyIndex = 0; keyIndex < keys.length; keyIndex++) { var key = keys[keyIndex]; Object.defineProperty(target, key, { value: source[key], enumerable: true, configurable: true, writable: true }); }");
            self.newline();
            self.push("}");
            self.newline();
            self.push("return target;");
            self.newline();
            self.push("}");
            self.newline();
            self.push_name(helper);
            self.push(".define = function(target, source) {");
            self.newline();
            self.push("var keys = Object.keys(source);");
            self.newline();
            self.push("if (typeof Object.getOwnPropertySymbols === \"function\") { var symbols = Object.getOwnPropertySymbols(source); for (var symbolIndex = 0; symbolIndex < symbols.length; symbolIndex++) if (Object.prototype.propertyIsEnumerable.call(source, symbols[symbolIndex])) keys.push(symbols[symbolIndex]); }");
            self.newline();
            self.push("for (var keyIndex = 0; keyIndex < keys.length; keyIndex++) {");
            self.newline();
            self.push("var key = keys[keyIndex], descriptor = Object.getOwnPropertyDescriptor(source, key);");
            self.newline();
            self.push("if (!(\"value\" in descriptor)) { var previous = Object.getOwnPropertyDescriptor(target, key); if (previous && !(\"value\" in previous)) { if (descriptor.get === void 0) descriptor.get = previous.get; if (descriptor.set === void 0) descriptor.set = previous.set; } }");
            self.newline();
            self.push("Object.defineProperty(target, key, descriptor);");
            self.newline();
            self.push("}");
            self.newline();
            self.push("return target;");
            self.newline();
            self.push("};");
            self.newline();
            self.push_name(helper);
            self.push(".proto = function(target, value) {");
            self.newline();
            self.push("var type = typeof value;");
            self.newline();
            self.push("if (value !== null && type !== \"object\" && type !== \"function\") return target;");
            self.newline();
            self.push("if (Object.setPrototypeOf) Object.setPrototypeOf(target, value);");
            self.newline();
            self.push("else { var descriptor = Object.getOwnPropertyDescriptor(Object.prototype, \"__proto__\"); if (descriptor && descriptor.set) descriptor.set.call(target, value); }");
            self.newline();
            self.push("return target;");
            self.newline();
            self.push("};");
            self.newline();
            self.push_name(helper);
            self.push(".rest = function(source, excluded) {");
            self.newline();
            self.push("if (source == null) throw new TypeError(\"Cannot destructure null or undefined\");");
            self.newline();
            self.push("var target = {}, keys = Object.keys(Object(source));");
            self.newline();
            self.push("if (typeof Object.getOwnPropertySymbols === \"function\") { var symbols = Object.getOwnPropertySymbols(source); for (var symbolIndex = 0; symbolIndex < symbols.length; symbolIndex++) if (Object.prototype.propertyIsEnumerable.call(source, symbols[symbolIndex])) keys.push(symbols[symbolIndex]); }");
            self.newline();
            self.push("for (var keyIndex = 0; keyIndex < keys.length; keyIndex++) { var key = keys[keyIndex], skip = false; for (var excludedIndex = 0; excludedIndex < excluded.length; excludedIndex++) if (excluded[excludedIndex] === key) { skip = true; break; } if (!skip) Object.defineProperty(target, key, { value: source[key], enumerable: true, configurable: true, writable: true }); }");
            self.newline();
            self.push("return target;");
            self.newline();
            self.push("};");
            self.newline();
        }

        if let Some(helper) = program.for_of_helper {
            self.push("function ");
            self.push_name(helper);
            self.push("(value) {");
            self.newline();
            self.push("var iterator, next, normal = true, error, hasError = false, state;");
            self.newline();
            self.push("state = {");
            self.newline();
            self.push("s: function() {");
            self.newline();
            self.push("if (value == null || typeof Symbol === \"undefined\") throw new TypeError(\"Value is not iterable\");");
            self.newline();
            self.push("var iteratorSymbol = Symbol.iterator;");
            self.newline();
            self.push(
                "if (iteratorSymbol == null) throw new TypeError(\"Value is not iterable\");",
            );
            self.newline();
            self.push("var method = value[iteratorSymbol];");
            self.newline();
            self.push("if (typeof method !== \"function\") throw new TypeError(\"Value is not iterable\");");
            self.newline();
            self.push("iterator = method.call(value);");
            self.newline();
            self.push("if (iterator == null || (typeof iterator !== \"object\" && typeof iterator !== \"function\")) throw new TypeError(\"Iterator is not an object\");");
            self.newline();
            self.push("next = iterator.next;");
            self.newline();
            self.push("if (typeof next !== \"function\") throw new TypeError(\"Iterator next is not callable\");");
            self.newline();
            self.push("},");
            self.newline();
            self.push("n: function() {");
            self.newline();
            // IteratorStep failures do not trigger IteratorClose. Mark the iteration normal
            // before invoking the captured `next`, then switch it back only after `done` is false.
            self.push("normal = true;");
            self.newline();
            self.push("var step = next.call(iterator);");
            self.newline();
            self.push("if (step == null || (typeof step !== \"object\" && typeof step !== \"function\")) throw new TypeError(\"Iterator result is not an object\");");
            self.newline();
            self.push("var done = step.done;");
            self.newline();
            self.push("if (done) return true;");
            self.newline();
            // IteratorValue failures mark the iterator record done and do not trigger
            // IteratorClose. Switch to an active loop body only after `value` was read.
            self.push("state.v = step.value;");
            self.newline();
            self.push("normal = false;");
            self.newline();
            self.push("return false;");
            self.newline();
            self.push("},");
            self.newline();
            self.push("e: function(caught) { hasError = true; error = caught; },");
            self.newline();
            self.push("f: function() {");
            self.newline();
            self.push("try {");
            self.newline();
            self.push("if (!normal) {");
            self.newline();
            self.push("var returnMethod = iterator.return;");
            self.newline();
            self.push("if (returnMethod != null) {");
            self.newline();
            self.push("if (typeof returnMethod !== \"function\") throw new TypeError(\"Iterator return is not callable\");");
            self.newline();
            self.push("var closeResult = returnMethod.call(iterator);");
            self.newline();
            self.push("if (closeResult == null || (typeof closeResult !== \"object\" && typeof closeResult !== \"function\")) throw new TypeError(\"Iterator return result is not an object\");");
            self.newline();
            self.push("}");
            self.newline();
            self.push("}");
            self.newline();
            self.push("} finally {");
            self.newline();
            // This finally is nested inside the transformed loop's finally. A saved body error
            // therefore wins even when GetMethod(return), return.call or validation also throws.
            self.push("if (hasError) throw error;");
            self.newline();
            self.push("}");
            self.newline();
            self.push("},");
            self.newline();
            self.push("v: void 0");
            self.newline();
            self.push("};");
            self.newline();
            self.push("return state;");
            self.newline();
            self.push("}");
            self.newline();
        }

        // 装饰器运行时辅助：先探测本模块是否含需降级的类，若有则在模块顶部注入
        // `__esDecorate` / `__runInitializers`（对齐 tsc 的 per-file helper）。
        if needs_decorator_helpers {
            self.push(crate::decorators::RUN_INITIALIZERS);
            self.push(crate::decorators::ES_DECORATE);
            self.newline();
        }

        // Pre-pass: prune zombie internal_reads from declarations that will be dropped.
        // A "zombie read" is a read of variable A from inside declaration B, where B itself
        // is externally unused and will be removed. After B is removed, A's only reader is
        // gone and A becomes droppable. We iterate until fixpoint.
        if self.shake.is_some() {
            self.prune_zombie_reads(stmts);
        }

        let mut i = directive_count;
        while i < stmts.len() {
            if i > directive_count
                || (i == directive_count && directive_count > 0 && !has_runtime_helpers)
            {
                self.newline();
            }
            if let Statement::VariableDeclaration(decl) = &stmts[i]
                && self.emit_shaken_top_level_var(decl)
            {
                i += 1;
                continue;
            }
            if let Statement::FunctionDeclaration(function) = &stmts[i]
                && let Some(id) = function.id
                && self
                    .shake
                    .as_ref()
                    .is_some_and(|shake| !shake.is_local_live(id.name))
            {
                i += 1;
                continue;
            }
            i = self.emit_merged_statement_from(stmts, i, directive_count);
        }
    }

    /// Emit a module-scope declaration after zombie-export pruning.
    ///
    /// Returns `true` when this method handled the declaration. Only simple identifiers are
    /// candidates; destructuring is left to the normal emitter because binding itself may invoke
    /// iterators/getters.
    fn emit_shaken_top_level_var(&mut self, decl: &VariableDeclaration) -> bool {
        let Some(shake) = &self.shake else {
            return false;
        };
        if decl.kind.is_using() {
            return false;
        }
        let removable: Vec<bool> = decl
            .declarations
            .iter()
            .map(|item| match &item.id {
                Pattern::Ident(id) => !shake.is_local_live(id.name),
                _ => false,
            })
            .collect();
        if !removable.iter().any(|remove| *remove) {
            return false;
        }

        let mut emitted = false;
        let mut in_declaration = false;
        for (item, remove) in decl.declarations.iter().zip(removable) {
            if remove {
                if let Some(init) = &item.init
                    && !expr_is_definitely_effect_free(init)
                {
                    if emitted {
                        self.push(";");
                    }
                    let needs_paren = starts_with_problematic(init);
                    if needs_paren {
                        self.push("(");
                    }
                    self.emit_expr(init, P_SEQUENCE);
                    if needs_paren {
                        self.push(")");
                    }
                    emitted = true;
                    in_declaration = false;
                }
                continue;
            }

            if in_declaration {
                self.punct(", ");
            } else {
                if emitted {
                    self.push(";");
                }
                self.push(decl.kind.as_str());
                self.sp();
                in_declaration = true;
            }
            self.emit_pattern(&item.id);
            if let Some(init) = &item.init {
                self.punct(" = ");
                self.emit_expr(init, P_ASSIGN);
            }
            emitted = true;
        }
        if emitted {
            self.push(";");
        }
        true
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
        let mut fixed_reads = FxHashSet::default();

        for stmt in stmts {
            if let Statement::ExportNamed(s) = stmt
                && let Some(decl) = &s.declaration
            {
                let names = self.decl_names(decl);
                let all_unused = names
                    .iter()
                    .all(|n| !self.shake.as_ref().unwrap().is_used(*n));
                let pure = decl_is_pure(decl);

                let mut reads = FxHashSet::default();
                if all_unused && pure {
                    collect_reads_in_statement(decl, &mut reads);
                    for atom in &reads {
                        readers_of.entry(*atom).or_default().push(exports.len());
                    }
                } else {
                    collect_reads_in_statement(decl, &mut fixed_reads);
                }

                exports.push(ExpInfo { names, pure, reads });
            } else {
                collect_reads_in_statement(stmt, &mut fixed_reads);
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
                let all_unused = exp
                    .names
                    .iter()
                    .all(|n| !self.shake.as_ref().unwrap().is_used(*n));
                if !all_unused {
                    continue;
                }
                let all_readers_dropped = exp.names.iter().all(|n| {
                    readers_of.get(n).is_none_or(|readers| {
                        readers.iter().all(|r| *r == i || dropped.contains(r))
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
                let has_live_reader = readers_of
                    .get(atom)
                    .is_some_and(|readers| readers.iter().any(|r| *r != i && !dropped.contains(r)))
                    || fixed_reads.contains(atom);
                if !has_live_reader {
                    shake.remove_read(*atom);
                }
            }
        }
    }

    fn emit_directive(&mut self, directive: &ExpressionStatement) {
        self.mark(directive.span);
        self.emit_expr(&directive.expression, P_SEQUENCE);
        self.push(";");
    }

    fn emit_statement(&mut self, stmt: &Statement) {
        // DCE: skip statements marked for removal by the DCE analysis.
        // 合成语句（Span::DUMMY）从不参与 span 索引侧表，避免 DUMMY 键碰撞误删
        // （如参数属性注入的 `this.x = x`）。
        if let Some(ctx) = self.minify_ctx
            && !stmt.span().is_dummy()
            && ctx.remove_spans.contains(&stmt.span())
        {
            return;
        }
        // SourceMap：每条语句在其产物起点记一条映射——这是调试器断点/栈帧定位的主要粒度。
        self.mark(stmt.span());
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
            Statement::ClassDeclaration(c) => {
                // 装饰器降级把类变成 IIFE **表达式**，声明形态须自行补出绑定与 `;`。
                if Self::class_needs_decorator_lowering(c) {
                    self.push("let ");
                    match c.id {
                        Some(id) => self.push_ident(&id),
                        None => self.push("_default"),
                    }
                    self.punct(" = ");
                    self.emit_decorated_class(c);
                    self.push(";");
                } else {
                    self.emit_class(c);
                }
            }
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
                // 按常量 test 折叠死分支（decide-then-skip，不改 AST，Span 保持）。
                // **不再与 minify 耦合**：折叠只依赖「条件可在构建期定为常量」，语义中性且
                // dev 同样受益（`process.env.NODE_ENV` 的死分支在 dev 产物里也应消失）。
                // 被丢弃分支含提升声明（var/函数）则不折叠——丢弃提升绑定会致 ReferenceError。
                if let Some(cond) = self.const_eval_bool(&s.test) {
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
                self.push(if self.minify { "if(" } else { "if (" });
                self.emit_expr(&s.test, P_SEQUENCE);
                self.punct(") ");
                self.emit_statement(&s.consequent);
                if let Some(alt) = &s.alternate {
                    self.push(" else ");
                    self.emit_statement(alt);
                }
            }
            Statement::For(s) => {
                self.push(if self.minify { "for(" } else { "for (" });
                if let Some(init) = &s.init {
                    match init {
                        ForInit::Variable(d) => self.emit_var_decl(d),
                        ForInit::Expression(e) => self.emit_expr(e, P_SEQUENCE),
                    }
                }
                self.punct("; ");
                if let Some(t) = &s.test {
                    self.emit_expr(t, P_SEQUENCE);
                }
                self.punct("; ");
                if let Some(u) = &s.update {
                    self.emit_expr(u, P_SEQUENCE);
                }
                self.punct(") ");
                self.emit_statement(&s.body);
            }
            Statement::ForIn(s) => {
                self.push(if self.minify { "for(" } else { "for (" });
                self.emit_for_left(&s.left);
                self.push(" in ");
                self.emit_expr(&s.right, P_SEQUENCE);
                self.punct(") ");
                self.emit_statement(&s.body);
            }
            Statement::ForOf(s) => {
                self.push(match (s.is_await, self.minify) {
                    (true, true) => "for await(",
                    (true, false) => "for await (",
                    (false, true) => "for(",
                    (false, false) => "for (",
                });
                self.emit_for_left(&s.left);
                self.push(" of ");
                self.emit_expr(&s.right, P_ASSIGN);
                self.punct(") ");
                self.emit_statement(&s.body);
            }
            Statement::While(s) => {
                self.push(if self.minify { "while(" } else { "while (" });
                self.emit_expr(&s.test, P_SEQUENCE);
                self.punct(") ");
                self.emit_statement(&s.body);
            }
            Statement::DoWhile(s) => {
                self.push("do ");
                self.emit_statement(&s.body);
                self.push(if self.minify { "while(" } else { " while (" });
                self.emit_expr(&s.test, P_SEQUENCE);
                self.push(");");
            }
            Statement::Switch(s) => {
                self.push(if self.minify { "switch(" } else { "switch (" });
                self.emit_expr(&s.discriminant, P_SEQUENCE);
                self.push(if self.minify { "){" } else { ") {" });
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
                        self.punct(") ");
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
                self.punct(": ");
                self.emit_statement(&s.body);
            }
            Statement::With(s) => {
                self.push(if self.minify { "with(" } else { "with (" });
                self.emit_expr(&s.object, P_SEQUENCE);
                self.punct(") ");
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
        // 不可达代码消除：`return`/`throw`/`break`/`continue` 之后的语句永不执行。
        // 但**提升声明仍生效**（`var`/函数声明会被提升），故其后若含提升声明则整体保留，
        // 与 if 折叠用同一条守卫（`has_hoisted_decl`）。
        let stmts = truncate_after_terminator(&body[..]);
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
        self.sp();
        for (i, decl) in d.declarations.iter().enumerate() {
            if i > 0 {
                self.punct(", ");
            }
            self.emit_pattern(&decl.id);
            if let Some(init) = &decl.init {
                self.punct(" = ");
                self.emit_expr(init, P_ASSIGN);
            }
        }
    }

    /// Emit a variable declaration with variable elimination: skip unused
    /// pure-bindings, emit only the initializer for unused impure bindings.
    fn emit_var_decl_elim(&mut self, d: &VariableDeclaration, ctx: &MinifyCtx) {
        // `using` / `await using`：绑定即使无引用也**不可删**——作用域结束时的 dispose 调用是
        // 可观测副作用。降级成裸初始化式（本函数对「不纯 init」的处理）会静默丢掉 dispose。
        if d.kind.is_using() {
            self.emit_var_decl(d);
            self.push(";");
            return;
        }
        let mut emitted = false;
        let mut in_var = false;

        for decl in d.declarations.iter() {
            let is_unused = match &decl.id {
                // 按声明 span 判断（非名字）：避免删掉同名但在别处仍被使用的绑定。
                Pattern::Ident(id) => ctx.unused_var_spans.contains(&id.span),
                _ => false,
            };

            if is_unused {
                if let Some(init) = &decl.init
                    && !expr_is_pure(init)
                {
                    if emitted {
                        self.push(";");
                    }
                    let needs_paren = starts_with_problematic(init);
                    if needs_paren {
                        self.push("(");
                    }
                    self.emit_expr(init, P_SEQUENCE);
                    if needs_paren {
                        self.push(")");
                    }
                    emitted = true;
                    in_var = false;
                }
                continue;
            }

            if in_var {
                self.punct(", ");
            } else {
                if emitted {
                    self.push(";");
                }
                self.push(d.kind.as_str());
                self.sp();
                in_var = true;
            }

            self.emit_pattern(&decl.id);
            if let Some(init) = &decl.init {
                self.punct(" = ");
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
        self.emit_merged_statement_from(stmts, i, 0)
    }

    /// As [`Self::emit_merged_statement`], but statements before `lower_bound` were emitted by a
    /// separate path and therefore cannot satisfy an incoming statement merge.
    fn emit_merged_statement_from(
        &mut self,
        stmts: &[Statement],
        i: usize,
        lower_bound: usize,
    ) -> usize {
        let stmt = &stmts[i];

        // 合成语句（Span::DUMMY，如 enum IIFE 内的成员赋值、参数属性注入）不参与任何
        // span 索引的语句级合并：DUMMY 是共享哨兵，会与其它 DUMMY 语句在 sequence/join
        // 侧表里互相碰撞（例如把 `E["A"]=0` 误判为已合并的后继而整条跳过）。逐条原样发射。
        if stmt.span().is_dummy() {
            self.emit_statement(stmt);
            return i + 1;
        }

        // Skip hoisted var declarations (already emitted at function top)
        if let Some(ctx) = self.minify_ctx
            && let Statement::VariableDeclaration(d) = stmt
            && ctx.hoist.var_hoist_flat.contains(&d.span)
        {
            return i + 1;
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
                    if i > lower_bound && ctx.sequence_spans.iter().any(|(_, b)| *b == e.span) {
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
        let Statement::VariableDeclaration(first) = &stmts[i] else {
            return i + 1;
        };
        let kind = first.kind;
        // `using` / `await using` 不参与合并，也不参与未用绑定消除（见 emit_var_decl_elim）。
        // 上游 statements.rs 已不产生此类 join 计划，这里再兜一层。
        if kind.is_using() {
            self.emit_var_decl(first);
            self.push(";");
            return i + 1;
        }

        let mut emitted = false;
        let mut in_var = false;
        let mut j = i;

        while j < stmts.len() {
            let Statement::VariableDeclaration(next) = &stmts[j] else {
                break;
            };
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
                    // 按声明 span 判断（非名字）——见 emit_var_decl_elim 同款注释。
                    Pattern::Ident(id) => ctx.unused_var_spans.contains(&id.span),
                    _ => false,
                };

                if is_unused {
                    if let Some(init) = &decl.init
                        && !expr_is_pure(init)
                    {
                        if emitted {
                            self.push(";");
                        }
                        let needs_paren = starts_with_problematic(init);
                        if needs_paren {
                            self.push("(");
                        }
                        self.emit_expr(init, P_SEQUENCE);
                        if needs_paren {
                            self.push(")");
                        }
                        emitted = true;
                        in_var = false;
                    }
                    continue;
                }

                if in_var {
                    self.punct(", ");
                } else {
                    if emitted {
                        self.push(";");
                    }
                    self.push(kind.as_str());
                    self.sp();
                    in_var = true;
                }

                self.emit_pattern(&decl.id);
                if let Some(init) = &decl.init {
                    self.punct(" = ");
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
            let Statement::Expression(next) = &stmts[j] else {
                break;
            };
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
                self.punct(", ");
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
        stmts: &[Statement],
        i: usize,
    ) -> usize {
        let cons_ret_expr = extract_return_expression(&s.consequent);
        // Pattern 2（`if (c) return a; else return b;`）从 else 分支取 alternate；
        // Pattern 1（`if (c) return a; return b;`，无 else）从紧邻的后继 return 语句取。
        // 此前 Pattern 1 只看 `s.alternate`（恒为 None）→ 误发 `void 0` 并把真正的
        // `return b` 整条跳过，丢失表达式（如 `av > bv ? sign : -sign` 变成 `void 0`）。
        let alt_ret_expr = match &s.alternate {
            Some(alt) => extract_return_expression(alt),
            None => stmts.get(i + 1).and_then(extract_return_expression),
        };

        self.push("return ");
        self.emit_expr(&s.test, P_CONDITIONAL);
        self.punct(" ? ");
        if let Some(arg) = cons_ret_expr {
            self.emit_expr(arg, P_ASSIGN);
        } else {
            self.push("void 0");
        }
        self.punct(" : ");
        if let Some(arg) = alt_ret_expr {
            self.emit_expr(arg, P_ASSIGN);
        } else {
            self.push("void 0");
        }
        self.push(";");

        // Pattern 1 (no else): also skip the subsequent return statement
        if s.alternate.is_none() { i + 2 } else { i + 1 }
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
                        self.punct(", ");
                    }
                    self.push_name(local.name);
                    wrote_leading = true;
                }
                ImportSpecifier::Namespace { local, .. } => {
                    if wrote_leading {
                        self.punct(", ");
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
                self.punct(", ");
            }
            self.punct("{ ");
            for (i, (imported, local)) in named.iter().enumerate() {
                if i > 0 {
                    self.punct(", ");
                }
                self.emit_module_export_name(imported);
                if !same_name(imported, &ModuleExportName::Ident(*local)) {
                    self.push(" as ");
                    self.push_name(local.name);
                }
            }
            self.punct(" }");
            wrote_leading = true;
        }
        if wrote_leading {
            self.push(" from ");
        }
        self.emit_string_atom(d.source);
        self.emit_import_attributes(d.attributes);
        self.push(";");
    }

    /// 引入属性子句 `with { type: "json" }`（跟在模块说明符之后）。
    ///
    /// 仅**非链接**路径发射：链接路径下目标模块已被内联进包，属性对运行时不再有意义
    /// （`.json` 的加载在 loader 层按扩展名完成，见 `wake_bundler::loader::json_to_js_module`）。
    fn emit_import_attributes(&mut self, attrs: Option<&ImportAttributes>) {
        let Some(a) = attrs else { return };
        self.sp();
        self.push(a.keyword.as_str());
        self.punct(" { ");
        for (i, item) in a.items.iter().enumerate() {
            if i > 0 {
                self.punct(", ");
            }
            self.emit_module_export_name(&item.key);
            self.punct(": ");
            self.emit_string_atom(item.value);
        }
        self.punct(" }");
    }

    fn emit_export_named(&mut self, s: &ExportNamedDeclaration) {
        self.push("export ");
        if let Some(decl) = &s.declaration {
            self.emit_statement(decl);
            return;
        }
        self.punct("{ ");
        for (i, spec) in s.specifiers.iter().enumerate() {
            if i > 0 {
                self.punct(", ");
            }
            self.emit_module_export_name(&spec.local);
            if !same_name(&spec.local, &spec.exported) {
                self.push(" as ");
                self.emit_module_export_name(&spec.exported);
            }
        }
        self.punct(" }");
        if let Some(src) = s.source {
            self.push(" from ");
            self.emit_string_atom(src);
            self.emit_import_attributes(s.attributes);
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
        self.emit_import_attributes(s.attributes);
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
        let unused_spans = self
            .minify_ctx
            .map(|ctx| ctx.unused_var_spans.clone())
            .unwrap_or_default();
        let is_unused = |spec: &ImportSpecifier| {
            let span = match spec {
                ImportSpecifier::Default { local, .. }
                | ImportSpecifier::Namespace { local, .. }
                | ImportSpecifier::Named { local, .. } => local.span,
            };
            unused_spans.contains(&span)
        };
        let has_live_specifier = d.specifiers.iter().any(|spec| !is_unused(spec));
        if !has_live_specifier {
            self.push(&req);
            self.push(";");
            return;
        }
        let tmp = self.next_tmp();
        self.push(&format!("const {tmp} = {req};"));
        for spec in d.specifiers.iter() {
            if is_unused(spec) {
                continue;
            }
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
                    self.push(&format!("const {n} = "));
                    self.emit_property_access(&tmp, &imp);
                    self.push(";");
                }
            }
        }
    }

    fn emit_export_named_linked(&mut self, s: &ExportNamedDeclaration) {
        if let Some(decl) = &s.declaration {
            // 每个绑定带自己的 span——导出赋值的**值**按 span 查 rename 表（mangle 后取新名，字符串键保留）。
            let name_spans = self.decl_name_spans(decl);
            if self.shake.is_some() {
                // 先在不可变借用下算好决策，再释放借用做可变发射（避免 borrowck 冲突）。
                let (drop_all, used_flags): (bool, Vec<bool>) = {
                    let shake = self.shake.as_ref().unwrap();
                    let all_unused = name_spans.iter().all(|(n, _)| !shake.is_used(*n));
                    let none_read = name_spans.iter().all(|(n, _)| !shake.is_read(*n));
                    let used_flags = name_spans.iter().map(|(n, _)| shake.is_used(*n)).collect();
                    // 整条声明既无外部使用、模块内也未引用、且无副作用 → 安全移除（Tree Shaking）。
                    (all_unused && none_read && decl_is_pure(decl), used_flags)
                };
                if drop_all {
                    return;
                }
                if used_flags.as_slice() == [true]
                    && let Statement::FunctionDeclaration(function) = decl
                    && let Some(id) = function.id
                    && self
                        .shake
                        .as_ref()
                        .is_some_and(|shake| !shake.is_read(id.name))
                {
                    let key = self.name(name_spans[0].0);
                    self.emit_property_access("exports", &key);
                    self.punct(" = ");
                    self.emit_function_with_name(function, false);
                    self.push(";");
                    return;
                }
                // 否则保留声明，但仅为**已用**导出发绑定行（移除未用绑定永远安全）。
                self.emit_statement(decl);
                for ((n, sp), &used) in name_spans.iter().zip(&used_flags) {
                    if used {
                        self.newline();
                        self.emit_export_binding(*n, Some(*sp));
                    }
                }
                return;
            }
            if name_spans.len() == 1
                && let Statement::FunctionDeclaration(function) = decl
                && let Some(id) = function.id
                && !self.program_reads.contains(&id.name)
            {
                let key = self.name(name_spans[0].0);
                self.emit_property_access("exports", &key);
                self.punct(" = ");
                self.emit_function_with_name(function, false);
                self.push(";");
                return;
            }
            self.emit_statement(decl);
            for (n, sp) in name_spans {
                self.newline();
                self.emit_export_binding(n, Some(sp));
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
                    // 用字面量 `exports`（与 emit_export_binding / 本地 re-export 一致）——单包模式
                    // 由 compact_body_names 统一转 `$`。曾误用 `self.ex()`（minify_names 下 = "e"）→
                    // 单包 wrapper 无 `e` 绑定 → 运行期 `e is not defined`。
                    self.emit_property_access("exports", &exported);
                    self.punct(" = ");
                    self.emit_property_access(&tmp, &local);
                    self.push(";");
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
                    // 值随 mangle：本地 re-export 的 local 可能被重命名（按其 span 查表）。
                    let local_atom = module_export_name_atom(&spec.local);
                    let local_val = match &spec.local {
                        ModuleExportName::Ident(_) => self
                            .module_renames
                            .get(&local_atom)
                            .copied()
                            .unwrap_or(local_atom),
                        ModuleExportName::String(_) => local_atom,
                    };
                    self.emit_property_access("exports", &exported);
                    self.punct(" = ");
                    self.push_name(local_val);
                    self.push(";");
                }
            }
        }
    }

    /// shake 开启且该导出名未被外部使用 → 应移除其绑定行。
    fn is_shaken_out(&self, exported: Atom) -> bool {
        self.shake.as_ref().is_some_and(|sh| !sh.is_used(exported))
    }

    /// 绑定 `name`（声明于 `span`）经 mangle 后的实际名——命中 rename 侧表则取新名，否则原名。
    /// 用于**导出赋值的值**：`exports["原名"] = 新名`（字符串键保留公开契约，值随 mangle）。
    fn renamed_or(&self, name: Atom, span: Span) -> Atom {
        if !span.is_dummy() {
            if let Some(map) = self.rename
                && let Some(&nn) = map.get(&span)
            {
                return nn;
            }
        } else if let Some(&nn) = self.module_renames.get(&name) {
            return nn;
        }
        if let Some(&nn) = self.module_renames.get(&name) {
            return nn;
        }
        name
    }

    /// 发射一行 `exports["name"] = value;`：键用导出名（原名，公开契约），值随 mangle（按绑定 span 查表）。
    fn emit_export_binding(&mut self, name: Atom, decl_span: Option<Span>) {
        let value = decl_span.map_or(name, |sp| self.renamed_or(name, sp));
        let key = self.interner.resolve(name);
        let val = self.interner.resolve(value);
        self.emit_property_access("exports", &key);
        self.punct(" = ");
        self.push(&val);
        self.push(";");
    }

    /// Emit a static property access. In compact output, identifier-like keys are shorter and
    /// equally precise in dot form (`exports.name`); arbitrary string export names retain brackets.
    fn emit_property_access(&mut self, base: &str, key: &str) {
        self.push(base);
        if self.minify && is_ascii_property_name(key) {
            self.push(".");
            self.push(key);
        } else {
            let before = self.out.len();
            let _ = write!(self.out, "[{key:?}]");
            self.sync_from(before);
        }
    }

    /// 同步 sourcemap 游标：把 `self.out[from..]`（绕过 [`Codegen::push`] 直写的部分）计入行列。
    /// 直写点（`write!`/`write_number`/字符串转义）用它兜底，保证游标不漂移。
    #[inline]
    fn sync_from(&mut self, from: usize) {
        if self.smap.is_none() {
            return;
        }
        // 借用分离：先取出待计文本的副本长度信息，再改游标。
        let tail = &self.out[from..];
        let (nl, last_line_units, total_units) = {
            let mut nl = 0u32;
            let mut units = 0usize;
            let mut since_nl = 0usize;
            for ch in tail.chars() {
                if ch == '\n' {
                    nl += 1;
                    since_nl = 0;
                } else {
                    since_nl += ch.len_utf16();
                }
                units += ch.len_utf16();
            }
            (nl, since_nl, units)
        };
        let sm = self.smap.as_mut().expect("checked above");
        if nl > 0 {
            sm.line += nl;
            sm.col = last_line_units as u32;
        } else {
            sm.col += total_units as u32;
        }
    }

    fn emit_export_default_linked(&mut self, s: &ExportDefaultDeclaration) {
        let default_used = self
            .shake
            .as_ref()
            .is_none_or(|sh| sh.is_used(sh.default_atom));
        match s.declaration {
            ExportDefaultKind::Function(f) => match f.id {
                Some(id) => {
                    let n = self.name(self.renamed_or(id.name, id.span));
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
                    let n = self.name(self.renamed_or(id.name, id.span));
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
                self.emit_property_access("exports", &name);
                self.punct(" = ");
                self.push(&tmp);
                self.push(";");
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

    /// 声明的 (绑定名, 绑定标识符 span) 列表——供导出赋值行按各自 span 查 rename 表发正确的值。
    /// 变量声明按声明符逐个（含解构里每个绑定），函数/类取其 id。
    fn decl_name_spans(&self, stmt: &Statement) -> Vec<(Atom, Span)> {
        let mut out = Vec::new();
        match stmt {
            Statement::FunctionDeclaration(f) => {
                if let Some(id) = f.id {
                    out.push((id.name, id.span));
                }
            }
            Statement::ClassDeclaration(c) => {
                if let Some(id) = c.id {
                    out.push((id.name, id.span));
                }
            }
            Statement::VariableDeclaration(d) => {
                for decl in d.declarations.iter() {
                    collect_pattern_name_spans(&decl.id, &mut out);
                }
            }
            _ => {}
        }
        out
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
        self.emit_function_with_name(f, true);
    }

    fn emit_function_with_name(&mut self, f: &Function, emit_name: bool) {
        if f.is_async {
            self.push("async ");
        }
        self.push("function");
        if f.is_generator {
            self.push("*");
        }
        if emit_name || !self.minify {
            self.sp();
        }
        if emit_name && let Some(id) = f.id {
            self.push_ident(&id);
        }
        self.emit_params(&f.params);
        self.sp();
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
                    self.punct(", ");
                }
                first = false;
                self.emit_pattern(&decl.id);
                if let Some(init) = &decl.init {
                    self.punct(" = ");
                    self.emit_expr(init, P_ASSIGN);
                }
            }
        }
        self.push(";");
    }

    fn emit_params(&mut self, params: &AVec<Pattern>) {
        // Preserve the complete parameter list even under minification. Removing trailing
        // parameters saves very little and is unsafe when an earlier transformation leaves a
        // live reference that is not represented by the variable-analysis side table. In that
        // case codegen used to emit a renamed reference without its binding (`f is not defined`).
        self.push("(");
        for i in 0..params.len() {
            if i > 0 {
                self.punct(", ");
            }
            self.emit_pattern(&params[i]);
        }
        self.push(")");
    }

    /// 该类是否需要装饰器降级（类自身或任一成员带装饰器）。
    ///
    /// `accessor` auto-accessor 字段的降级（私有存储 + get/set 对）未实现，含之则整体放弃转换
    /// ——宁可原样发射（运行时报错可见），也不产出**看似成功但语义错误**的代码。
    fn class_needs_decorator_lowering(c: &Class) -> bool {
        class_needs_decorator_lowering(c)
    }

    fn emit_class(&mut self, c: &Class) {
        if Self::class_needs_decorator_lowering(c) {
            self.emit_decorated_class(c);
            return;
        }
        self.push("class");
        if let Some(id) = c.id {
            self.sp();
            self.push_ident(&id);
        }
        if let Some(sc) = &c.super_class {
            self.push(" extends ");
            self.emit_expr(sc, P_CALL_MEMBER);
        }
        self.punct(" {");
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

    /// 发射装饰器降级后的类（TC39 Stage-3，对齐 tsc emit）。见 [`crate::decorators`]。
    fn emit_decorated_class(&mut self, c: &Class) {
        use crate::decorators::{DecoratedKind, sanitize};
        self.needs_decorator_helpers.set(true);

        // 收集被装饰成员：(内部变量前缀, 属性名, kind, 是否静态)
        struct Decorated<'x, 'a> {
            var: String,
            name: String,
            kind: DecoratedKind,
            is_static: bool,
            decorators: &'x AVec<'a, Expression<'a>>,
        }
        let mut items: Vec<Decorated> = Vec::new();
        for m in c.body.iter() {
            match m {
                ClassMember::Method(md) if !md.decorators.is_empty() => {
                    let Some(name) = self.static_key_name(&md.key) else {
                        continue;
                    };
                    let kind = match md.kind {
                        MethodKind::Get => DecoratedKind::Getter,
                        MethodKind::Set => DecoratedKind::Setter,
                        _ => DecoratedKind::Method,
                    };
                    items.push(Decorated {
                        var: format!(
                            "_{}{}",
                            if md.is_static { "static_" } else { "" },
                            sanitize(&name)
                        ),
                        name,
                        kind,
                        is_static: md.is_static,
                        decorators: &md.decorators,
                    });
                }
                ClassMember::Property(p) if !p.decorators.is_empty() => {
                    let Some(name) = self.static_key_name(&p.key) else {
                        continue;
                    };
                    items.push(Decorated {
                        var: format!(
                            "_{}{}",
                            if p.is_static { "static_" } else { "" },
                            sanitize(&name)
                        ),
                        name,
                        kind: DecoratedKind::Field,
                        is_static: p.is_static,
                        decorators: &p.decorators,
                    });
                }
                _ => {}
            }
        }

        let has_class_dec = !c.decorators.is_empty();
        // 非字段成员才产生 `_instance/_staticExtraInitializers`（字段走各自的 `_X_extraInitializers`）。
        let has_instance_nonfield = items
            .iter()
            .any(|i| !i.is_static && i.kind != DecoratedKind::Field);
        let has_static_nonfield = items
            .iter()
            .any(|i| i.is_static && i.kind != DecoratedKind::Field);
        let has_instance_any = items.iter().any(|i| !i.is_static);
        let has_static_any = items.iter().any(|i| i.is_static);
        let class_name =
            c.id.map(|id| self.ident_text(&id))
                .unwrap_or_else(|| "_default".to_string());
        // 静态侧的 `this`：类被装饰时须用 `_classThis`（装饰可能替换类本身）。
        let static_target = if has_class_dec { "_classThis" } else { "this" };

        // —— IIFE 头：声明全部内部变量 ——
        self.push("(()=>{");
        if has_instance_nonfield {
            self.push("let _instanceExtraInitializers=[];");
        }
        if has_static_nonfield {
            self.push("let _staticExtraInitializers=[];");
        }
        for it in &items {
            self.push(&format!("let {}_decorators;", it.var));
            if it.kind == DecoratedKind::Field {
                self.push(&format!(
                    "let {v}_initializers=[];let {v}_extraInitializers=[];",
                    v = it.var
                ));
            }
        }
        if has_class_dec {
            self.push("let _classDecorators;let _classDescriptor;let _classExtraInitializers=[];let _classThis;");
        }

        // —— 类本体 ——
        self.push(&format!("var {class_name}=class"));
        if let Some(sc) = &c.super_class {
            self.push(" extends ");
            self.emit_expr(sc, P_CALL_MEMBER);
        }
        self.push("{");
        if has_class_dec {
            self.push("static{_classThis=this;}");
        }

        // static 块 #1：填装饰器数组 → 逐元素 __esDecorate → 类装饰。
        self.push("static{");
        for it in &items {
            self.push(&format!("{}_decorators=[", it.var));
            for (i, d) in it.decorators.iter().enumerate() {
                if i > 0 {
                    self.push(",");
                }
                self.emit_expr(d, P_ASSIGN);
            }
            self.push("];");
        }
        if has_class_dec {
            self.push("_classDecorators=[");
            for (i, d) in c.decorators.iter().enumerate() {
                if i > 0 {
                    self.push(",");
                }
                self.emit_expr(d, P_ASSIGN);
            }
            self.push("];");
        }
        // 顺序对齐 tsc：非字段（静态→实例）→ 字段（静态→实例）。
        let ordered = |field: bool, stat: bool| {
            items
                .iter()
                .filter(move |i| (i.kind == DecoratedKind::Field) == field && i.is_static == stat)
        };
        for it in ordered(false, true)
            .chain(ordered(false, false))
            .chain(ordered(true, true))
            .chain(ordered(true, false))
        {
            self.emit_es_decorate_call(it.var.as_str(), &it.name, it.kind, it.is_static);
        }
        if has_class_dec {
            // `context.name` 用**源码原名**字面量，而非 tsc 的 `_classThis.name`——后者在
            // mangler 重命名类绑定后会变成压缩名，装饰器读到的名字就错了。
            let src_name = c.id.map(|id| self.name(id.name)).unwrap_or_default();
            self.push(&format!(
                "__esDecorate(null,_classDescriptor={{value:_classThis}},_classDecorators,{{kind:\"{k}\",name:{n:?}}},null,_classExtraInitializers);{class_name}=_classThis=_classDescriptor.value;",
                k = DecoratedKind::Class.as_str(),
                n = src_name
            ));
        }
        self.push("}");

        // —— 成员：被装饰字段的初值需串联 extraInitializers ——
        // 首个被装饰字段跑 `_{instance,static}ExtraInitializers`（若存在），其后每个跑**前一个**
        // 字段的 `_extraInitializers`；最后一个的 `_extraInitializers` 即该侧「尾部」。
        let mut inst_prev: Option<String> =
            has_instance_nonfield.then(|| "_instanceExtraInitializers".to_string());
        let mut stat_prev: Option<String> =
            has_static_nonfield.then(|| "_staticExtraInitializers".to_string());
        let mut ctor_emitted = false;

        for m in c.body.iter() {
            self.newline();
            match m {
                ClassMember::Property(p) if !p.decorators.is_empty() => {
                    let name = self.static_key_name(&p.key).unwrap_or_default();
                    let var = format!(
                        "_{}{}",
                        if p.is_static { "static_" } else { "" },
                        sanitize(&name)
                    );
                    let (target, prev) = if p.is_static {
                        (static_target, &mut stat_prev)
                    } else {
                        ("this", &mut inst_prev)
                    };
                    let pre = prev.clone();
                    if p.is_static {
                        self.push("static ");
                    }
                    self.emit_property_key(&p.key, p.computed);
                    self.push("=");
                    match &pre {
                        Some(pv) => self.push(&format!(
                            "(__runInitializers({target},{pv}),__runInitializers({target},{var}_initializers,"
                        )),
                        None => {
                            self.push(&format!("__runInitializers({target},{var}_initializers,"))
                        }
                    }
                    match &p.value {
                        Some(v) => self.emit_expr(v, P_ASSIGN),
                        None => self.push("void 0"),
                    }
                    self.push(if pre.is_some() { "));" } else { ");" });
                    *prev = Some(format!("{var}_extraInitializers"));
                }
                ClassMember::Method(md)
                    if md.kind == MethodKind::Constructor && has_instance_any =>
                {
                    // 显式构造函数：在体首插入实例侧 extraInitializers（对齐 tsc）。
                    ctor_emitted = true;
                    let tail = inst_prev
                        .clone()
                        .unwrap_or_else(|| "_instanceExtraInitializers".to_string());
                    // `emit_params` 自带括号，此处不可再补。
                    self.push("constructor");
                    self.emit_params(&md.value.params);
                    self.push("{");
                    self.push(&format!("__runInitializers(this,{tail});"));
                    if let Some(body) = &md.value.body {
                        for st in body.statements.iter() {
                            self.emit_statement(st);
                        }
                    }
                    self.push("}");
                }
                other => self.emit_class_member(other),
            }
        }

        // 无显式构造函数时合成一个，跑实例侧尾部。
        if has_instance_any && !ctor_emitted {
            let tail = inst_prev
                .clone()
                .unwrap_or_else(|| "_instanceExtraInitializers".to_string());
            self.newline();
            if c.super_class.is_some() {
                self.push(&format!(
                    "constructor(...args){{super(...args);__runInitializers(this,{tail});}}"
                ));
            } else {
                self.push(&format!("constructor(){{__runInitializers(this,{tail});}}"));
            }
        }

        // 尾部 static 块：静态侧尾部 + 类 extraInitializers（须在全部静态字段初始化之后）。
        if has_static_any || has_class_dec {
            self.newline();
            self.push("static{");
            if has_static_any {
                let tail = stat_prev
                    .clone()
                    .unwrap_or_else(|| "_staticExtraInitializers".to_string());
                self.push(&format!("__runInitializers({static_target},{tail});"));
            }
            if has_class_dec {
                self.push("__runInitializers(_classThis,_classExtraInitializers);");
            }
            self.push("}");
        }

        self.push("};");
        if has_class_dec {
            self.push(&format!("return {class_name}=_classThis;"));
        } else {
            self.push(&format!("return {class_name};"));
        }
        self.push("})()");
    }

    /// 发射一次 `__esDecorate(...)` 调用。
    fn emit_es_decorate_call(
        &mut self,
        var: &str,
        name: &str,
        kind: crate::decorators::DecoratedKind,
        is_static: bool,
    ) {
        use crate::decorators::DecoratedKind;
        // 名字作为 JS 字符串字面量嵌入（属性名可能含引号/反斜杠）。
        let key = format!("{name:?}");
        // field 的 target 为 null（值经 initializers 注入），其余挂到 ctor/prototype 上。
        let ctor = if kind == DecoratedKind::Field {
            "null"
        } else {
            "this"
        };
        let access = match kind {
            DecoratedKind::Field => {
                format!("{{has:o=>{key} in o,get:o=>o[{key}],set:(o,v)=>{{o[{key}]=v;}}}}")
            }
            DecoratedKind::Setter => format!("{{has:o=>{key} in o,set:(o,v)=>{{o[{key}]=v;}}}}"),
            _ => format!("{{has:o=>{key} in o,get:o=>o[{key}]}}"),
        };
        let (inits, extra) = if kind == DecoratedKind::Field {
            (
                format!("{var}_initializers"),
                format!("{var}_extraInitializers"),
            )
        } else {
            (
                "null".to_string(),
                if is_static {
                    "_staticExtraInitializers".to_string()
                } else {
                    "_instanceExtraInitializers".to_string()
                },
            )
        };
        self.push(&format!(
            "__esDecorate({ctor},null,{var}_decorators,{{kind:\"{k}\",name:{key},static:{is_static},private:false,access:{access}}},{inits},{extra});",
            k = kind.as_str()
        ));
    }

    /// 取静态可知的成员名（标识符/字符串/数字键）；计算键返回 `None`（不降级）。
    fn static_key_name(&self, key: &PropertyKey) -> Option<String> {
        match key {
            PropertyKey::Ident(id) => Some(self.name(id.name)),
            PropertyKey::String(s) => Some(self.name(s.value)),
            PropertyKey::Number(n) => Some(format!("{}", n.value)),
            _ => None,
        }
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
                self.sp();
                match m.value.body {
                    Some(b) => self.emit_block(&b.statements),
                    None => self.push("{}"),
                }
            }
            ClassMember::Property(p) => {
                if p.is_static {
                    self.push("static ");
                }
                // auto-accessor（`accessor x = 1`）：语义是私有存储 + 自动 get/set 对，
                // 与普通字段不同，修饰符必须原样发出。
                if p.accessor {
                    self.push("accessor ");
                }
                self.emit_property_key(&p.key, p.computed);
                if let Some(v) = &p.value {
                    self.punct(" = ");
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
            PropertyKey::Number(n) => {
                let before = self.out.len();
                write_number(&mut self.out, n.value);
                self.sync_from(before);
            }
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
                        self.punct(", ");
                    }
                    if let Some(p) = el {
                        self.emit_pattern(p);
                    }
                }
                self.push("]");
            }
            Pattern::Object(o) => {
                self.punct("{ ");
                let mut first = true;
                for p in o.properties.iter() {
                    if !first {
                        self.punct(", ");
                    }
                    first = false;
                    // mangle 后绑定名改变时展开 shorthand，避免连带改掉属性名。
                    if p.shorthand && !p.computed && !self.pattern_binding_renamed(&p.value) {
                        self.emit_pattern(&p.value);
                    } else {
                        self.emit_property_key(&p.key, p.computed);
                        self.punct(": ");
                        self.emit_pattern(&p.value);
                    }
                }
                if let Some(rest) = &o.rest {
                    if !first {
                        self.punct(", ");
                    }
                    self.push("...");
                    self.emit_pattern(&rest.argument);
                }
                self.punct(" }");
            }
            Pattern::Assignment(a) => {
                self.emit_pattern(&a.left);
                self.punct(" = ");
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
        // Skip inlining when in write-target position (Update argument / Assignment left)
        // to avoid producing invalid syntax like `(1 << bf) = 1`.
        if !self.skip_inline
            && let Expression::Identifier(id) = expr
        {
            // 按标识符**引用 span**（非名字）命中内联,只替换那唯一一次使用,
            // 不波及其它作用域的同名变量。合成节点(DUMMY)不参与。
            if let Some(ctx) = self.minify_ctx
                && !id.span.is_dummy()
                && let Some(inline_expr) = ctx.inline_vars.get(&id.span)
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
        // 合成表达式（Span::DUMMY）跳过：DUMMY 是共享哨兵，多个合成字面量会在
        // constants 表上碰撞（last-write-wins），曾导致整个 enum IIFE 被替换成最后一个
        // 成员名字符串（如 `var a = "Archived"`）。
        if let Some(ctx) = self.minify_ctx
            && !expr.span().is_dummy()
        {
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
                    let before = self.out.len();
                    write_number(&mut self.out, n.value);
                    self.sync_from(before);
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
                        self.punct(", ");
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
                    && !u.span.is_dummy()
                    && ctx.double_not_spans.contains(&u.span)
                    && let Expression::Unary(inner) = &u.argument
                {
                    self.emit_expr(&inner.argument, P_UNARY);
                    return;
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
                    self.skip_inline = true;
                    self.emit_expr(&u.argument, P_UNARY);
                    self.skip_inline = false;
                } else {
                    self.skip_inline = true;
                    self.emit_expr(&u.argument, P_POSTFIX);
                    self.skip_inline = false;
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
                self.binop(b.operator.as_str());
                self.emit_expr(&b.right, right_min);
            }
            Expression::Logical(l) => {
                let prec = logical_prec(l.operator);
                let group_left = l.operator == LogicalOperator::Coalesce
                    && self.is_and_or_logical_after_inline(&l.left);
                if group_left {
                    self.push("(");
                }
                self.emit_expr(&l.left, if group_left { P_ASSIGN } else { prec });
                if group_left {
                    self.push(")");
                }
                self.binop(l.operator.as_str());
                let group_right = l.operator == LogicalOperator::Coalesce
                    && self.is_and_or_logical_after_inline(&l.right);
                if group_right {
                    self.push("(");
                }
                self.emit_expr(&l.right, if group_right { P_ASSIGN } else { prec + 1 });
                if group_right {
                    self.push(")");
                }
            }
            Expression::Assignment(a) => {
                self.skip_inline = true;
                self.emit_expr(&a.left, P_CALL_MEMBER);
                self.skip_inline = false;
                self.binop(a.operator.as_str());
                self.emit_expr(&a.right, P_ASSIGN);
            }
            Expression::Conditional(c) => {
                // 常量条件的三元同样折叠——与 if 折叠同源（`const_eval_bool` 只对纯节点求值，
                // 有副作用的 test 会被拒绝，故直接取存活分支是安全的）。
                if let Some(cond) = self.const_eval_bool(&c.test) {
                    let kept = if cond { &c.consequent } else { &c.alternate };
                    self.emit_expr(kept, P_ASSIGN);
                    return;
                }
                self.emit_expr(&c.test, P_CONDITIONAL + 1);
                self.punct(" ? ");
                self.emit_expr(&c.consequent, P_ASSIGN);
                self.punct(" : ");
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
                        self.punct(", ");
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
                        self.punct(", ");
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
            Expression::MetaProperty(m) => {
                self.interner.with_resolved(m.meta, |s| out.push_str(s));
                out.push('.');
                self.interner.with_resolved(m.property, |s| out.push_str(s));
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

    fn is_and_or_logical_after_inline(&self, expr: &Expression) -> bool {
        let effective = if let Expression::Identifier(id) = expr
            && let Some(ctx) = self.minify_ctx
            && !id.span.is_dummy()
            && let Some(inline_expr) = ctx.inline_vars.get(&id.span)
        {
            inline_expr
        } else {
            expr
        };
        matches!(
            effective,
            Expression::Logical(logical)
                if logical.operator == LogicalOperator::And
                    || logical.operator == LogicalOperator::Or
        )
    }

    fn emit_member(&mut self, m: &MemberExpression) {
        if !m.optional
            && !m.span.is_dummy()
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
                    && !id.span.is_dummy()
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
                self.punct(", ");
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
        self.punct("{ ");
        for (i, m) in o.properties.iter().enumerate() {
            if i > 0 {
                self.punct(", ");
            }
            match m {
                ObjectMember::Spread(s) => {
                    self.push("...");
                    self.emit_expr(&s.argument, P_ASSIGN);
                }
                ObjectMember::Property(p) => self.emit_object_property(p),
            }
        }
        self.punct(" }");
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
            self.sp();
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
            && !p.prototype_setter
            && let PropertyKey::Ident(id) = &p.key
            && !id.span.is_dummy()
            && let Some(map) = self.prop_rename
            && let Some(&nn) = map.get(&id.span)
        {
            self.push_name(nn);
            self.punct(": ");
            self.emit_expr(&p.value, P_ASSIGN);
        } else {
            self.emit_property_key(&p.key, p.computed);
            self.punct(": ");
            self.emit_expr(&p.value, P_ASSIGN);
        }
    }

    fn emit_arrow(&mut self, a: &ArrowFunction) {
        if a.is_async {
            self.push("async ");
        }
        self.emit_params(&a.params);
        self.punct(" => ");
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
        let before = self.out.len();
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
        // 转义后的字面量已直写 out（可能含 `\n` 两字符转义、非 ASCII 原样字符）→ 统一回算游标。
        self.sync_from(before);
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

/// 同 [`collect_pattern_names`]，但带每个绑定标识符的 span（供导出赋值行查 rename 表）。
fn collect_pattern_name_spans(pat: &Pattern, out: &mut Vec<(Atom, Span)>) {
    match pat {
        Pattern::Ident(id) => out.push((id.name, id.span)),
        Pattern::Array(a) => {
            for el in a.elements.iter().flatten() {
                collect_pattern_name_spans(el, out);
            }
        }
        Pattern::Object(o) => {
            for p in o.properties.iter() {
                collect_pattern_name_spans(&p.value, out);
            }
            if let Some(r) = &o.rest {
                collect_pattern_name_spans(&r.argument, out);
            }
        }
        Pattern::Assignment(a) => collect_pattern_name_spans(&a.left, out),
        Pattern::Rest(r) => collect_pattern_name_spans(&r.argument, out),
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

fn is_ascii_property_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || matches!(first, b'_' | b'$'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
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

fn collect_pattern_renames(
    pattern: &Pattern,
    rename: &FxHashMap<Span, Atom>,
    out: &mut FxHashMap<Atom, Atom>,
) {
    match pattern {
        Pattern::Ident(id) => {
            if let Some(&new_name) = rename.get(&id.span) {
                out.insert(id.name, new_name);
            }
        }
        Pattern::Array(array) => {
            for element in array.elements.iter().flatten() {
                collect_pattern_renames(element, rename, out);
            }
        }
        Pattern::Object(object) => {
            for property in object.properties.iter() {
                collect_pattern_renames(&property.value, rename, out);
            }
            if let Some(rest) = &object.rest {
                collect_pattern_renames(&rest.argument, rename, out);
            }
        }
        Pattern::Assignment(assignment) => collect_pattern_renames(&assignment.left, rename, out),
        Pattern::Rest(rest) => collect_pattern_renames(&rest.argument, rename, out),
    }
}

fn collect_module_renames(
    program: &Program,
    rename: Option<&FxHashMap<Span, Atom>>,
) -> FxHashMap<Atom, Atom> {
    let Some(rename) = rename else {
        return FxHashMap::default();
    };
    let mut out = FxHashMap::default();
    let mut collect = |stmt: &Statement| match stmt {
        Statement::VariableDeclaration(decl) => {
            for item in decl.declarations.iter() {
                collect_pattern_renames(&item.id, rename, &mut out);
            }
        }
        Statement::FunctionDeclaration(function) => {
            if let Some(id) = function.id
                && let Some(&new_name) = rename.get(&id.span)
            {
                out.insert(id.name, new_name);
            }
        }
        Statement::ClassDeclaration(class) => {
            if let Some(id) = class.id
                && let Some(&new_name) = rename.get(&id.span)
            {
                out.insert(id.name, new_name);
            }
        }
        _ => {}
    };
    for stmt in program.body.iter() {
        if let Statement::ExportNamed(export) = stmt
            && let Some(decl) = &export.declaration
        {
            collect(decl);
        } else {
            collect(stmt);
        }
    }
    out
}

fn collect_used_export_locals(program: &Program, used: &FxHashSet<Atom>) -> FxHashSet<Atom> {
    let mut locals = FxHashSet::default();
    for stmt in program.body.iter() {
        if let Statement::ExportNamed(export) = stmt
            && export.declaration.is_none()
            && export.source.is_none()
        {
            for specifier in export.specifiers.iter() {
                if used.contains(&module_export_name_atom(&specifier.exported)) {
                    locals.insert(module_export_name_atom(&specifier.local));
                }
            }
        }
    }
    locals
}

/// Conservative proof that evaluating an expression cannot call user code, throw through an
/// unresolved lookup, mutate state, or invoke a getter/iterator.
fn expr_is_definitely_effect_free(expr: &Expression) -> bool {
    match expr {
        Expression::NumberLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::RegExpLiteral(_)
        | Expression::Function(_)
        | Expression::Arrow(_) => true,
        Expression::Array(array) => array.elements.iter().flatten().all(|element| {
            !matches!(element, Expression::Spread(_)) && expr_is_definitely_effect_free(element)
        }),
        Expression::Object(object) => object.properties.iter().all(|member| match member {
            ObjectMember::Property(property) => {
                !matches!(property.key, PropertyKey::Computed(_))
                    && expr_is_definitely_effect_free(&property.value)
            }
            ObjectMember::Spread(_) => false,
        }),
        // Class evaluation may execute computed keys, static fields/blocks, or `extends`.
        // Identifiers can throw in TDZ/unresolved cases; member reads can invoke getters/proxies.
        _ => false,
    }
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
/// 截断语句序列中「终止语句之后」的不可达部分。
///
/// `return`/`throw`/`break`/`continue` 之后的语句永不执行，可整体丢弃。**但**其中若含
/// 提升声明（`var` / 函数声明），这些绑定在进入作用域时就已生效、丢弃会致 ReferenceError，
/// 故此时保守保留全部（与 if 折叠同一条守卫）。
fn truncate_after_terminator<'s, 'a>(stmts: &'s [Statement<'a>]) -> &'s [Statement<'a>] {
    let Some(pos) = stmts.iter().position(is_terminator) else {
        return stmts;
    };
    let tail = &stmts[pos + 1..];
    if tail.is_empty()
        || tail.iter().any(has_hoisted_decl)
        || tail.iter().any(is_dormant_transform_lexical_binding)
    {
        return stmts;
    }
    &stmts[..=pos]
}

/// A transform may intentionally jump over a lexical declaration so its binding is instantiated
/// for the surrounding block but never initialized (the synchronous for-of RHS TDZ sentinel).
/// Unlike ordinary unreachable statements, removing this node changes name resolution and the
/// behavior of closures created before it. DUMMY declaration/binding spans plus missing
/// initializers form a private AST marker that source declarations cannot accidentally match.
fn is_dormant_transform_lexical_binding(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::VariableDeclaration(declaration)
            if declaration.span.is_dummy()
                && declaration.kind != VarKind::Var
                && !declaration.declarations.is_empty()
                && declaration.declarations.iter().all(|declarator| {
                    declarator.span.is_dummy()
                        && declarator.id.span().is_dummy()
                        && declarator.init.is_none()
                })
    )
}

/// 该语句是否使控制流离开当前语句序列。
fn is_terminator(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::Return(_) | Statement::Throw(_) | Statement::Break(_) | Statement::Continue(_)
    )
}

/// 该类是否需要装饰器降级。
///
/// `accessor` auto-accessor 字段的降级（私有存储 + get/set 对）未实现，含之则整体放弃转换
/// ——宁可原样发射（运行时报错可见），也不产出**看似成功但语义错误**的代码。
fn class_needs_decorator_lowering(c: &Class) -> bool {
    if c.body.iter().any(|m| match m {
        ClassMember::Property(p) => p.accessor && !p.decorators.is_empty(),
        _ => false,
    }) {
        return false;
    }
    !c.decorators.is_empty()
        || c.body.iter().any(|m| match m {
            ClassMember::Method(m) => !m.decorators.is_empty(),
            ClassMember::Property(p) => !p.decorators.is_empty(),
            ClassMember::StaticBlock(_) => false,
        })
}

/// 模块顶层是否存在需要装饰器降级的类（决定是否注入运行时辅助）。
///
/// 只扫顶层的类声明 / `export [default] class` / 顶层 `const X = class`——覆盖装饰器的
/// 合法出现位置（装饰器只能修饰类声明与类表达式的绑定形式）。
fn program_has_decorated_class(stmts: &[Statement]) -> bool {
    // 与实际降级判定同源：含未支持的 `accessor` 装饰时不降级，也就不该注入辅助。
    let class_decorated = class_needs_decorator_lowering;
    stmts.iter().any(|s| match s {
        Statement::ClassDeclaration(c) => class_decorated(c),
        Statement::ExportDefault(d) => match &d.declaration {
            ExportDefaultKind::Class(c) => class_decorated(c),
            _ => false,
        },
        Statement::ExportNamed(e) => match &e.declaration {
            Some(Statement::ClassDeclaration(c)) => class_decorated(c),
            Some(Statement::VariableDeclaration(d)) => d
                .declarations
                .iter()
                .any(|x| matches!(&x.init, Some(Expression::Class(c)) if class_decorated(c))),
            _ => false,
        },
        Statement::VariableDeclaration(d) => d
            .declarations
            .iter()
            .any(|x| matches!(&x.init, Some(Expression::Class(c)) if class_decorated(c))),
        _ => false,
    })
}

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
            Statement::For(_)
            | Statement::ForIn(_)
            | Statement::ForOf(_)
            | Statement::While(_)
            | Statement::DoWhile(_) => {}
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
