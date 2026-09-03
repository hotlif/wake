//! # wake_ecma_codegen — 代码生成（AST → JS 字符串）
//!
//! DESIGN §4.6：直接从 AST 写字符串，维护运算符优先级/结合性表自动补括号。
//!
//! SourceMap（[`sourcemap`] 模块）：mapped 发射入口采集「产物行列 ↔ 源字节偏移」映射；
//! 不请求时零开销，且 mapped/unmapped 的 JavaScript 主体逐字节一致。普通 AST 入口使用
//! 纯 readable emitter；optimized-program 入口只发射优化器拥有的 typed IR。
//!
//! 入口：[`codegen`]（默认 dev 可读风格）。往返 `parse → codegen → parse` 语义等价（见测试）。

/// Stable emitter implementation identity for caller-owned cache keys.
pub const PIPELINE_VERSION: &str = "wake-ecma-codegen-v1";

use std::error::Error;
use std::fmt::{self, Write as _};

mod decorators;
mod typed;

use wake_common::{Atom, FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::*;
use wake_ecma_minify::OptimizedProgram;
use wake_ecma_minify::codegen_bridge::{
    TypedChunkId, TypedFinalModuleFacts, TypedFinalModuleTarget, TypedModuleError, TypedModuleId,
    TypedModuleMode, TypedModuleRequestKind, TypedModuleSpecifierRewrite, TypedResolvedModule,
    finalize_owned_typed_modules,
};
pub use wake_ecma_minify::{ConstVal, ModuleRequestKind, OptimizeInput, ValidatedDefine, optimize};

pub mod sourcemap;
pub use sourcemap::{Mapping, ModuleMappings, SourceMap};
use typed::{
    codegen_finalized_typed, codegen_finalized_typed_with_map,
    codegen_finalized_typed_with_requests, codegen_sealed_trivial_typed,
    codegen_sealed_trivial_typed_with_map,
};
#[cfg(test)]
use typed::{codegen_typed, codegen_typed_with_map};

/// 把一个 Program 生成为 JS 源码字符串（保留 ESM，不链接）。
pub fn codegen(program: &Program, interner: &Interner) -> String {
    let mut cg = Codegen {
        out: String::new(),
        interner,
        indent: 0,
        smap: None,
        needs_decorator_helpers: std::cell::Cell::new(false),
    };
    cg.emit_program(program);
    cg.out
}

/// 链接器：把模块说明符映射到内部模块 id（`__wake_require__` 的实参）。
///
/// 用于 [`codegen_optimized`]：把 ESM import/export 与 `import()`/`require()` 改写为 CJS，
/// 供 webpack 式函数包装打包（DESIGN §6.1）。
pub trait ModuleLinker {
    /// 说明符 → 内部模块 id；`None` 表示外部/未解析（MVP 保留原样）。
    fn module_id(&self, specifier: &str, kind: ModuleRequestKind) -> Option<u32>;
    /// 动态 `import(specifier)` 目标所属的 async/shared chunk id（代码分割，6.5）。
    /// `None` = 目标在入口闭包内 / 未启用分割 → 走旧内联（`Promise.resolve(require(id))`）。
    fn dynamic_chunk(&self, _specifier: &str) -> Option<u32> {
        None
    }
    /// Returns a normalized request when a literal dynamic import is owned by the embedding
    /// runtime instead of the local module graph.
    ///
    /// The emitted call starts as `__wake_require__.runtimeImport(request)`, with an optional expose
    /// argument supplied by [`ModuleLinker::runtime_dynamic_import_expose`]. Static imports and
    /// `require()` never consult this hook, which keeps asynchronous runtime boundaries explicit.
    fn runtime_dynamic_import(&self, _specifier: &str) -> Option<String> {
        None
    }
    /// Optional immutable expose identity for a runtime-owned dynamic request. This final-layout
    /// fact is emitted structurally as the second `runtimeImport` argument and participates in the
    /// caller's body-cache identity.
    fn runtime_dynamic_import_expose(&self, _specifier: &str) -> Option<String> {
        None
    }
    /// Returns a normalized share key when an initialized embedding runtime supplies this
    /// request synchronously. Product configuration owns the explicit allowlist.
    fn runtime_shared_module(
        &self,
        _specifier: &str,
        _kind: ModuleRequestKind,
    ) -> Option<(String, String)> {
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

/// Rewrites one module specifier while preserving the surrounding module syntax.
///
/// Library preserve-modules output uses this seam to map source requests such as `./button.js`
/// to the final `.mjs` or `.cjs` artifact. Returning `None` keeps a runtime external unchanged.
pub trait ModuleSpecifierRewriter {
    fn rewrite(&self, specifier: &str) -> Option<String>;

    /// Rewrites a request while retaining whether JavaScript will load it through ESM semantics
    /// or CommonJS `require`. Existing preserve-module callers that do not need conditional-export
    /// profiles inherit the original [`Self::rewrite`] behavior.
    fn rewrite_with_kind(&self, specifier: &str, _kind: ModuleSpecifierKind) -> Option<String> {
        self.rewrite(specifier)
    }

    /// Whether preserve-CommonJS output should lower literal `import()` through its synchronous
    /// graph loader. Artifact-oriented rewriters keep native `import()` by default; embedded graph
    /// runtimes opt in so the request stays inside their owned module registry.
    fn lower_dynamic_import_to_require(&self) -> bool {
        false
    }
}

/// Runtime loading semantics of a module request emitted by preserve-modules codegen.
///
/// Static and dynamic ESM imports share the `Import` profile; raw `require()` calls use
/// `Require`. Keeping this distinction at the emitter boundary lets a single graph resolver select
/// the correct conditional export even when one module loads the same package both ways.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleSpecifierKind {
    Import,
    Require,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreserveModuleFormat {
    EsModule,
    CommonJs,
}

/// A fallible optimized-codegen failure suitable for compiler facades.
///
/// Existing infallible entry points retain their assertion-based contract for trusted internal
/// callers. User-input-facing compiler APIs should use the `try_*` variants so malformed or
/// unsupported module plans never escape as a panic.
#[derive(Debug)]
pub enum CodegenError {
    ModuleModeMismatch {
        expected: PreserveModuleFormat,
        actual: String,
    },
    ModuleFinalization(TypedModuleError),
}

impl fmt::Display for CodegenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModuleModeMismatch { expected, actual } => write!(
                formatter,
                "optimized module mode {actual} cannot be emitted as {expected:?}"
            ),
            Self::ModuleFinalization(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for CodegenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ModuleModeMismatch { .. } => None,
            Self::ModuleFinalization(error) => Some(error),
        }
    }
}

impl From<TypedModuleError> for CodegenError {
    fn from(error: TypedModuleError) -> Self {
        Self::ModuleFinalization(error)
    }
}

/// Semantic use of an internal request, proved by typed module finalization.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum GeneratedModuleRequestRole {
    Value,
    DiscardedStatic,
}

/// Exact generated byte range of one compiler-owned internal request's numeric target literal.
/// The bundler may redirect this range only against the byte-identical body emitted in the same
/// codegen walk.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct GeneratedModuleRequest {
    pub start: u32,
    pub end: u32,
    pub target_module_id: u32,
    pub role: GeneratedModuleRequestRole,
    pub specifier: String,
    pub kind: ModuleRequestKind,
}

/// Exact collision-free parameter spellings expected by one generated module body.
///
/// These names come from the finalized typed symbol table and must travel with the byte-identical
/// body. Wrappers and persistent-cache restores consume this value; neither is allowed to infer
/// runtime bindings by searching generated JavaScript.
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub struct GeneratedModuleRuntimeCapabilities {
    /// Whether a symbol-bound internal-require `metaUrl` member survives typed optimization.
    pub meta_url: bool,
    pub external_require: bool,
    pub promise_resolve: bool,
    pub object_assign: bool,
    pub object_keys: bool,
    pub object_define_property: bool,
    pub runtime_import: bool,
    pub shared: bool,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct GeneratedModuleRuntimeNames {
    pub module: String,
    pub exports: String,
    pub require: String,
    pub capabilities: GeneratedModuleRuntimeCapabilities,
}

impl GeneratedModuleRuntimeNames {
    pub fn canonical() -> Self {
        Self {
            module: "module".into(),
            exports: "exports".into(),
            require: "__wake_require__".into(),
            capabilities: GeneratedModuleRuntimeCapabilities::default(),
        }
    }

    pub fn is_canonical(&self) -> bool {
        self.module == "module" && self.exports == "exports" && self.require == "__wake_require__"
    }
}

/// Emit a preserve-modules artifact from optimizer-owned trusted edits. This is the library-build
/// counterpart of [`codegen_optimized`]; callers cannot inject independent rewrite side tables.
pub fn codegen_preserved_optimized(
    optimized: &OptimizedProgram,
    _interner: &Interner,
    format: PreserveModuleFormat,
    rewriter: &dyn ModuleSpecifierRewriter,
) -> String {
    try_codegen_preserved_optimized(optimized, _interner, format, rewriter).expect(match format {
        PreserveModuleFormat::EsModule => {
            "preserve-ESM codegen requires an optimizer-owned ESM module plan"
        }
        PreserveModuleFormat::CommonJs => {
            "preserve-CommonJS codegen requires an optimizer-owned import plan"
        }
    })
}

/// Fallible counterpart of [`codegen_preserved_optimized`] for user-input-facing compiler APIs.
pub fn try_codegen_preserved_optimized(
    optimized: &OptimizedProgram,
    _interner: &Interner,
    format: PreserveModuleFormat,
    rewriter: &dyn ModuleSpecifierRewriter,
) -> Result<String, CodegenError> {
    validate_preserved_module_mode(optimized, format)?;
    let facts = preserved_module_facts(optimized, rewriter);
    Ok(try_emit_finalized_typed(optimized, &facts, false)?.0)
}

/// Mapped counterpart of [`codegen_preserved_optimized`]. Trusted edits, defines, and renames are
/// consumed exclusively from `optimized`; mapping collection is the only difference from the
/// unmapped entry, so both return byte-identical JavaScript.
pub fn codegen_preserved_optimized_with_map(
    optimized: &OptimizedProgram,
    _interner: &Interner,
    format: PreserveModuleFormat,
    rewriter: &dyn ModuleSpecifierRewriter,
) -> (String, ModuleMappings) {
    try_codegen_preserved_optimized_with_map(optimized, _interner, format, rewriter).expect(
        match format {
            PreserveModuleFormat::EsModule => {
                "preserve-ESM codegen requires an optimizer-owned ESM module plan"
            }
            PreserveModuleFormat::CommonJs => {
                "preserve-CommonJS codegen requires an optimizer-owned import plan"
            }
        },
    )
}

/// Fallible mapped counterpart of [`codegen_preserved_optimized_with_map`].
pub fn try_codegen_preserved_optimized_with_map(
    optimized: &OptimizedProgram,
    _interner: &Interner,
    format: PreserveModuleFormat,
    rewriter: &dyn ModuleSpecifierRewriter,
) -> Result<(String, ModuleMappings), CodegenError> {
    validate_preserved_module_mode(optimized, format)?;
    let facts = preserved_module_facts(optimized, rewriter);
    let (code, mappings) = try_emit_finalized_typed(optimized, &facts, true)?;
    Ok((code, mappings.unwrap_or_default()))
}

/// Emit the optimizer-owned program through the single production minification path.
/// Identifier and proof-backed property renames are taken from the same [`OptimizedProgram`], so
/// callers cannot pair decisions with a different AST or configuration.
pub fn codegen_optimized(
    optimized: &OptimizedProgram,
    _interner: &Interner,
    linker: &dyn ModuleLinker,
    no_esmodule: bool,
) -> String {
    assert_bundled_module_mode(optimized);
    let facts = bundled_module_facts(optimized, linker, no_esmodule);
    emit_finalized_typed(optimized, &facts, false).0
}

/// Mapped counterpart of [`codegen_optimized`]. Both functions use the same emitter and differ
/// only in mapping collection, guaranteeing byte-identical JavaScript bodies.
pub fn codegen_optimized_with_map(
    optimized: &OptimizedProgram,
    _interner: &Interner,
    linker: &dyn ModuleLinker,
    no_esmodule: bool,
) -> (String, ModuleMappings) {
    assert_bundled_module_mode(optimized);
    let facts = bundled_module_facts(optimized, linker, no_esmodule);
    let (code, mappings) = emit_finalized_typed(optimized, &facts, true);
    (code, mappings.unwrap_or_default())
}

/// Bundler emission counterpart which additionally exposes exact generated ranges for
/// proof-carrying discarded static requests. Public callers should normally use
/// [`codegen_optimized_with_map`]; this metadata is meaningful only together with a final bundle
/// execution layout.
pub fn codegen_optimized_with_map_and_requests(
    optimized: &OptimizedProgram,
    _interner: &Interner,
    linker: &dyn ModuleLinker,
    no_esmodule: bool,
) -> (
    String,
    ModuleMappings,
    Vec<GeneratedModuleRequest>,
    GeneratedModuleRuntimeNames,
) {
    assert_bundled_module_mode(optimized);
    let facts = bundled_module_facts(optimized, linker, no_esmodule);
    if optimized.can_emit_sealed_without_finalization(facts.no_esmodule) {
        let (code, mappings) =
            codegen_sealed_trivial_typed_with_map(optimized.typed_program(), optimized.minify());
        let runtime_names = typed::generated_module_runtime_names(
            optimized.typed_program(),
            optimized.typed_module_plan(),
        );
        return (code, mappings, Vec::new(), runtime_names);
    }
    let (program, _) = finalize_owned_typed_modules(
        optimized.typed_program().clone(),
        optimized.typed_module_plan().clone(),
        &facts,
    )
    .expect("optimizer-owned module requests must finalize before code generation");
    codegen_finalized_typed_with_requests(&program, optimized.minify())
}

fn validate_preserved_module_mode(
    optimized: &OptimizedProgram,
    format: PreserveModuleFormat,
) -> Result<(), CodegenError> {
    let expected = match format {
        PreserveModuleFormat::EsModule => TypedModuleMode::PreserveEsm,
        PreserveModuleFormat::CommonJs => TypedModuleMode::PreserveCommonJs,
    };
    let actual = optimized.typed_module_plan().mode();
    if actual == expected {
        Ok(())
    } else {
        Err(CodegenError::ModuleModeMismatch {
            expected: format,
            actual: format!("{actual:?}"),
        })
    }
}

fn assert_bundled_module_mode(optimized: &OptimizedProgram) {
    assert_eq!(
        optimized.typed_module_plan().mode(),
        TypedModuleMode::BundledCommonJs,
        "bundled codegen requires an optimizer-owned CommonJS module plan"
    );
}

fn emit_finalized_typed(
    optimized: &OptimizedProgram,
    facts: &TypedFinalModuleFacts,
    want_map: bool,
) -> (String, Option<ModuleMappings>) {
    try_emit_finalized_typed(optimized, facts, want_map)
        .expect("optimizer-owned module requests must finalize before code generation")
}

fn try_emit_finalized_typed(
    optimized: &OptimizedProgram,
    facts: &TypedFinalModuleFacts,
    want_map: bool,
) -> Result<(String, Option<ModuleMappings>), CodegenError> {
    if optimized.can_emit_sealed_without_finalization(facts.no_esmodule) {
        if want_map {
            let (code, mappings) = codegen_sealed_trivial_typed_with_map(
                optimized.typed_program(),
                optimized.minify(),
            );
            return Ok((code, Some(mappings)));
        }
        return Ok((
            codegen_sealed_trivial_typed(optimized.typed_program(), optimized.minify()),
            None,
        ));
    }
    let (program, _) = finalize_owned_typed_modules(
        optimized.typed_program().clone(),
        optimized.typed_module_plan().clone(),
        facts,
    )
    .map_err(CodegenError::ModuleFinalization)?;
    if want_map {
        let (code, mappings) = codegen_finalized_typed_with_map(&program, optimized.minify());
        Ok((code, Some(mappings)))
    } else {
        Ok((codegen_finalized_typed(&program, optimized.minify()), None))
    }
}

fn bundled_module_facts(
    optimized: &OptimizedProgram,
    linker: &dyn ModuleLinker,
    no_esmodule: bool,
) -> TypedFinalModuleFacts {
    let mut seen = FxHashSet::default();
    let mut modules = Vec::new();
    for request in optimized.typed_module_plan().requests() {
        if !seen.insert((request.specifier.clone(), request.kind)) {
            continue;
        }
        let runtime_request = (request.kind == TypedModuleRequestKind::DynamicImport)
            .then(|| linker.runtime_dynamic_import(&request.specifier))
            .flatten();
        let shared_request = linker.runtime_shared_module(&request.specifier, request.kind);
        let target = if let Some(runtime_request) = runtime_request {
            TypedFinalModuleTarget::RuntimeDynamic {
                request: runtime_request,
                expose: linker.runtime_dynamic_import_expose(&request.specifier),
            }
        } else if let Some((shared_request, scope)) = shared_request {
            TypedFinalModuleTarget::RuntimeShared {
                request: shared_request,
                scope,
            }
        } else if let Some(module_id) = linker.module_id(&request.specifier, request.kind) {
            TypedFinalModuleTarget::Internal {
                module_id: TypedModuleId(module_id),
                is_esm: optimized.dependency_target_is_esm(&request.specifier, request.kind),
                async_dependency: linker.is_async_module(module_id),
                dynamic_chunk: (request.kind == TypedModuleRequestKind::DynamicImport)
                    .then(|| linker.dynamic_chunk(&request.specifier).map(TypedChunkId))
                    .flatten(),
            }
        } else {
            TypedFinalModuleTarget::External {
                rewritten_specifier: request.specifier.clone(),
            }
        };
        modules.push(TypedResolvedModule {
            specifier: request.specifier.clone(),
            request_kind: request.kind,
            target,
        });
    }
    TypedFinalModuleFacts {
        modules,
        no_esmodule,
        ..TypedFinalModuleFacts::default()
    }
}

fn preserved_module_facts(
    optimized: &OptimizedProgram,
    rewriter: &dyn ModuleSpecifierRewriter,
) -> TypedFinalModuleFacts {
    let mut seen = FxHashSet::default();
    let mut request_rewrites = Vec::new();
    for request in optimized.typed_module_plan().requests() {
        if !seen.insert((request.specifier.clone(), request.kind)) {
            continue;
        }
        let kind = match request.kind {
            TypedModuleRequestKind::StaticImport | TypedModuleRequestKind::DynamicImport => {
                ModuleSpecifierKind::Import
            }
            TypedModuleRequestKind::Require => ModuleSpecifierKind::Require,
        };
        if let Some(rewritten_specifier) = rewriter.rewrite_with_kind(&request.specifier, kind) {
            request_rewrites.push(TypedModuleSpecifierRewrite {
                specifier: request.specifier.clone(),
                request_kind: request.kind,
                rewritten_specifier,
            });
        }
    }
    TypedFinalModuleFacts {
        request_rewrites,
        lower_external_dynamic_to_require: rewriter.lower_dynamic_import_to_require(),
        ..TypedFinalModuleFacts::default()
    }
}

struct Codegen<'i> {
    out: String,
    interner: &'i Interner,
    indent: usize,
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
    /// Original identifier names, interned deterministically in first-emission order.
    names: Vec<String>,
    name_indices: FxHashMap<Atom, u32>,
    /// 上一条映射的源字节偏移与可选原名——完全相同的连续映射才去重。
    last_src: Option<(u32, Option<u32>)>,
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

impl<'i> Codegen<'i> {
    fn name(&self, atom: Atom) -> String {
        self.interner.resolve(atom)
    }

    fn push(&mut self, s: &str) {
        self.out.push_str(s);
        if let Some(sm) = &mut self.smap {
            sm.advance(s);
        }
    }

    fn push_name(&mut self, atom: Atom) {
        // 零分配：借用驻留切片直接拷进输出缓冲，省去 resolve 的临时 String。
        // 闭包只写 out、不回调 interner，无重入死锁风险。
        let interner = self.interner;
        let out = &mut self.out;
        let smap = self.smap.as_mut();
        interner.with_resolved(atom, |s| {
            // 与 push 相同的 token 边界守卫：关键字/标识符紧邻标识符时补空格（`return`+`x`）。
            out.push_str(s);
            // 名字不含换行，直接按 UTF-16 码元累加列。
            if let Some(sm) = smap {
                sm.col += s.chars().map(char::len_utf16).sum::<usize>() as u32;
            }
        });
    }

    /// 在当前产物位置记录一条指向 `span.lo` 的映射（源侧行列推迟到序列化换算）。
    ///
    /// 合成节点（`Span::DUMMY`）不记录——它不对应任何源码位置，映射过去会把调试器
    /// 指到文件开头。相同源偏移连续出现时去重，避免每个 token 都产生冗余段。
    #[inline]
    fn mark(&mut self, span: Span) {
        self.mark_with_name(span, None);
    }

    fn mark_with_name(&mut self, span: Span, original_name: Option<Atom>) {
        if span.is_dummy() {
            return;
        }
        if self.smap.is_none() {
            return;
        }
        let existing_name_index = original_name.and_then(|name| {
            self.smap
                .as_ref()
                .and_then(|sm| sm.name_indices.get(&name).copied())
        });
        let original_text = match (original_name, existing_name_index) {
            (Some(name), None) => Some(self.name(name)),
            _ => None,
        };
        let Some(sm) = &mut self.smap else { return };
        let name_index = original_name.map(|name| {
            if let Some(index) = existing_name_index {
                index
            } else {
                let index = sm.names.len() as u32;
                sm.names
                    .push(original_text.expect("named mapping resolves its original name"));
                sm.name_indices.insert(name, index);
                index
            }
        });
        if sm.last_src == Some((span.lo, name_index)) {
            return;
        }
        sm.last_src = Some((span.lo, name_index));
        // 同一产物位置只保留**最后**一条映射：被完全擦除的语句（如链接后消失的 `import`）会先在
        // 当前位置留下一条映射，而真正占据该位置的是其后的语句——后者必须覆盖前者，否则调试器
        // 会把该位置指回已消失的源语句。
        if let Some(last) = sm.mappings.last_mut()
            && last.gen_line == sm.line
            && last.gen_col == sm.col
        {
            last.is_unmapped = false;
            last.src_index = 0;
            last.src_offset = span.lo;
            last.name_index = name_index;
            return;
        }
        sm.mappings.push(Mapping {
            gen_line: sm.line,
            gen_col: sm.col,
            src_index: 0, // 模块内恒 0；bundler 合并时重写为真实源下标
            src_offset: span.lo,
            name_index,
            is_unmapped: false,
        });
    }

    /// End the previous source association at the current generated position.
    ///
    /// Source Map V3 represents generated-only punctuation with a one-field segment. Recording
    /// that boundary is essential: without it DevTools extends the preceding source token across
    /// synthetic grouping, receiver stripping, or bundler glue until the next mapped token.
    #[inline]
    fn mark_unmapped(&mut self) {
        let Some(sm) = &mut self.smap else { return };
        sm.last_src = None;
        if let Some(last) = sm.mappings.last_mut()
            && last.gen_line == sm.line
            && last.gen_col == sm.col
        {
            *last = Mapping::unmapped(sm.line, sm.col);
            return;
        }
        if sm.mappings.last().is_some_and(|last| {
            last.is_unmapped && last.gen_line == sm.line && last.gen_col == sm.col
        }) {
            return;
        }
        sm.mappings.push(Mapping::unmapped(sm.line, sm.col));
    }

    /// 发射 readable 模式的标点及其空白。
    fn punct(&mut self, pretty: &'static str) {
        self.push(pretty);
    }

    /// 发射两侧带空格的二元、逻辑或赋值运算符。
    fn binop(&mut self, op: &str) {
        self.push(" ");
        self.push(op);
        self.push(" ");
    }

    /// 发射 readable 模式的单个空格。
    fn sp(&mut self) {
        self.out.push(' ');
        if let Some(sm) = &mut self.smap {
            sm.col += 1;
        }
    }

    /// 发射一个变量引用或绑定标识符。优化后的程序由 typed emitter 发射；普通 AST
    /// emitter 始终保留 AST 中的原名。
    fn push_ident(&mut self, ident: &Ident) {
        self.mark(ident.span);
        self.push_name(ident.name);
    }

    fn ident_text(&self, ident: &Ident) -> String {
        self.name(ident.name)
    }

    fn newline(&mut self) {
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

        let mut i = directive_count;
        while i < stmts.len() {
            if i > directive_count
                || (i == directive_count && directive_count > 0 && !has_runtime_helpers)
            {
                self.newline();
            }
            self.emit_statement(&stmts[i]);
            i += 1;
        }
    }

    fn emit_directive(&mut self, directive: &ExpressionStatement) {
        self.mark(directive.span);
        self.emit_expr(&directive.expression, P_SEQUENCE);
        self.push(";");
    }

    fn emit_required_statement(&mut self, stmt: &Statement) {
        self.emit_statement(stmt);
    }

    fn emit_statement(&mut self, stmt: &Statement) {
        self.mark(stmt.span());
        match stmt {
            Statement::VariableDeclaration(d) => {
                self.emit_var_decl(d);
                self.push(";");
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
                self.push("if (");
                self.emit_expr(&s.test, P_SEQUENCE);
                self.punct(") ");
                self.emit_required_statement(&s.consequent);
                if let Some(alt) = &s.alternate {
                    self.push(" else ");
                    self.emit_required_statement(alt);
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
                self.punct("; ");
                if let Some(t) = &s.test {
                    self.emit_expr(t, P_SEQUENCE);
                }
                self.punct("; ");
                if let Some(u) = &s.update {
                    self.emit_expr(u, P_SEQUENCE);
                }
                self.punct(") ");
                self.emit_required_statement(&s.body);
            }
            Statement::ForIn(s) => {
                self.push("for (");
                self.emit_for_left(&s.left);
                self.push(" in ");
                self.emit_expr(&s.right, P_SEQUENCE);
                self.punct(") ");
                self.emit_required_statement(&s.body);
            }
            Statement::ForOf(s) => {
                self.push(if s.is_await { "for await (" } else { "for (" });
                self.emit_for_left(&s.left);
                self.push(" of ");
                self.emit_expr(&s.right, P_ASSIGN);
                self.punct(") ");
                self.emit_required_statement(&s.body);
            }
            Statement::While(s) => {
                self.push("while (");
                self.emit_expr(&s.test, P_SEQUENCE);
                self.punct(") ");
                self.emit_required_statement(&s.body);
            }
            Statement::DoWhile(s) => {
                self.push("do ");
                self.emit_required_statement(&s.body);
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
                self.emit_required_statement(&s.body);
            }
            Statement::With(s) => {
                self.push("with (");
                self.emit_expr(&s.object, P_SEQUENCE);
                self.punct(") ");
                self.emit_required_statement(&s.body);
            }
            Statement::Debugger(_) => self.push("debugger;"),
            Statement::Import(d) => self.emit_import(d),
            Statement::ExportNamed(s) => self.emit_export_named(s),
            Statement::ExportDefault(s) => self.emit_export_default(s),
            Statement::ExportAll(s) => self.emit_export_all(s),
        }
    }

    fn emit_block(&mut self, body: &AVec<Statement>) {
        self.push("{");
        if body.is_empty() {
            self.push("}");
            return;
        }
        self.indent += 1;
        for statement in body.iter() {
            self.newline();
            self.emit_statement(statement);
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
        self.emit_module_specifier(d.source);
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
            self.emit_module_specifier(src);
            self.emit_import_attributes(s.attributes);
        }
        self.push(";");
    }

    fn emit_export_default(&mut self, s: &ExportDefaultDeclaration) {
        match s.declaration {
            ExportDefaultKind::Function(f) => {
                self.push("export default ");
                self.emit_function(f);
            }
            ExportDefaultKind::Class(c)
                if Self::class_needs_decorator_lowering(c) && c.id.is_some() =>
            {
                let id = c.id.expect("guarded named decorated class");
                // A named default class creates a module-local lexical binding. Decorator
                // lowering turns the class into an IIFE expression, so recreate that binding
                // explicitly before exporting its value.
                self.push("let ");
                self.push_ident(&id);
                self.punct(" = ");
                self.emit_decorated_class(c);
                self.push(";");
                self.newline();
                self.push("export default ");
                self.push_ident(&id);
                self.push(";");
            }
            ExportDefaultKind::Class(c) => {
                self.push("export default ");
                self.emit_class(c);
                if Self::class_needs_decorator_lowering(c) {
                    // The lowered anonymous class is an expression, not a declaration.
                    self.push(";");
                }
            }
            ExportDefaultKind::Expression(e) => {
                self.push("export default ");
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
        self.emit_module_specifier(s.source);
        self.emit_import_attributes(s.attributes);
        self.push(";");
    }

    fn emit_module_export_name(&mut self, n: &ModuleExportName) {
        match n {
            ModuleExportName::Ident(id) => self.push_name(id.name),
            ModuleExportName::String(a) => self.emit_string_atom(*a),
        }
    }

    fn emit_module_specifier(&mut self, atom: Atom) {
        self.emit_string_atom(atom);
    }

    // ==================================================================
    // 函数 / 类
    // ==================================================================

    fn emit_function(&mut self, f: &Function) {
        // A FunctionExpression's inner binding is observable even when it has no lexical reads:
        // `function descriptiveName() {}.name` exposes the spelling, and engines also use it in
        // stacks. Never turn a named function into an anonymous one here.
        self.emit_function_with_name(f, f.id.is_some());
    }

    fn emit_function_with_name(&mut self, f: &Function, emit_name: bool) {
        // Function coverage and debuggers need an owned anchor at the exact generated function
        // boundary. Synthetic wrappers (for example preserve-CJS export getters) are emitted as
        // raw code and deliberately never receive this marker.
        self.mark(f.span);
        if f.is_async {
            self.push("async ");
        }
        self.push("function");
        if f.is_generator {
            self.push("*");
        }
        self.sp();
        if emit_name && let Some(id) = f.id {
            self.push_ident(&id);
        }
        self.emit_params(&f.params);
        self.sp();
        match f.body {
            Some(body) => self.emit_block(&body.statements),
            None => self.push("{}"),
        }
    }

    fn emit_params(&mut self, params: &AVec<Pattern>) {
        // The plain emitter reproduces the complete AST parameter list.
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
    /// plain/experimental AST emitter 尚不降级 `accessor` auto-accessor 字段，因此只保留
    /// 原始语法。生产 build/optimize 路径在 owned IR 中完整 materialize 装饰器。
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
                self.mark(m.span);
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

    /// Keep the optional source-map cursor synchronized after direct writes to `out`.
    fn sync_from(&mut self, from: usize) {
        let Some(state) = &mut self.smap else {
            return;
        };
        let tail = &self.out[from..];
        if let Some(last_newline) = tail.rfind('\n') {
            state.line += tail.bytes().filter(|byte| *byte == b'\n').count() as u32;
            state.col = tail[last_newline + 1..]
                .chars()
                .map(char::len_utf16)
                .sum::<usize>() as u32;
        } else {
            state.col += tail.chars().map(char::len_utf16).sum::<usize>() as u32;
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
                    if p.shorthand && !p.computed {
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
        match expr {
            Expression::NumberLiteral(n) => {
                let before = self.out.len();
                write_number(&mut self.out, n.value);
                self.sync_from(before);
            }
            Expression::StringLiteral(s) => self.emit_string_atom(s.value),
            Expression::BooleanLiteral(b) => self.push(if b.value { "true" } else { "false" }),
            Expression::NullLiteral(_) => self.push("null"),
            Expression::BigIntLiteral(b) => {
                self.push_name(b.raw);
                // `n` is part of the BigInt token, not an adjacent identifier. Going through
                // `push()` would invoke the minified token-boundary guard and emit invalid
                // JavaScript such as `0 n`.
                self.out.push('n');
                if let Some(sm) = &mut self.smap {
                    sm.col += 1;
                }
            }
            Expression::RegExpLiteral(r) => {
                self.push("/");
                self.push_name(r.pattern);
                self.push("/");
                self.push_name(r.flags);
            }
            Expression::TemplateLiteral(t) => self.emit_template(t),
            Expression::Identifier(id) => self.push_ident(id),
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
                self.binop(b.operator.as_str());
                self.emit_expr(&b.right, right_min);
            }
            Expression::Logical(l) => {
                let prec = logical_prec(l.operator);
                let group_left =
                    l.operator == LogicalOperator::Coalesce && self.is_and_or_logical(&l.left);
                if group_left {
                    self.push("(");
                }
                self.emit_expr(&l.left, if group_left { P_ASSIGN } else { prec });
                if group_left {
                    self.push(")");
                }
                self.binop(l.operator.as_str());
                let group_right =
                    l.operator == LogicalOperator::Coalesce && self.is_and_or_logical(&l.right);
                if group_right {
                    self.push("(");
                }
                self.emit_expr(&l.right, if group_right { P_ASSIGN } else { prec + 1 });
                if group_right {
                    self.push(")");
                }
            }
            Expression::Assignment(a) => {
                self.emit_expr(&a.left, P_CALL_MEMBER);
                self.binop(a.operator.as_str());
                self.emit_expr(&a.right, P_ASSIGN);
            }
            Expression::Conditional(c) => {
                self.emit_expr(&c.test, P_CONDITIONAL + 1);
                self.punct(" ? ");
                self.emit_expr(&c.consequent, P_ASSIGN);
                self.punct(" : ");
                self.emit_expr(&c.alternate, P_ASSIGN);
            }
            Expression::Call(c) => {
                self.emit_expr(&c.callee, P_CALL_MEMBER);
                if c.optional {
                    self.push("?.");
                }
                self.emit_arguments(&c.arguments);
            }
            Expression::New(n) => {
                self.push("new ");
                self.emit_expr(&n.callee, P_CALL_MEMBER);
                self.emit_arguments(&n.arguments);
            }
            Expression::Member(m) => self.emit_member(m),
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

    fn is_and_or_logical(&self, expr: &Expression) -> bool {
        matches!(
            expr,
            Expression::Logical(logical)
                if logical.operator == LogicalOperator::And
                    || logical.operator == LogicalOperator::Or
        )
    }

    fn emit_member(&mut self, m: &MemberExpression) {
        self.emit_member_object(&m.object);
        match &m.property {
            MemberProperty::Ident(id) => {
                self.push(if m.optional { "?." } else { "." });
                self.push_name(id.name);
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

    fn emit_member_object(&mut self, object: &Expression) {
        if self.member_object_is_number(object) {
            // Decimal integer tokens cannot be followed by a single property dot (`1.toString`).
            // Grouping is valid for every number spelling.
            self.mark_unmapped();
            self.push("(");
            self.emit_expr(object, P_ASSIGN);
            self.mark_unmapped();
            self.push(")");
        } else {
            self.emit_expr(object, P_CALL_MEMBER);
        }
    }

    fn member_object_is_number(&self, object: &Expression) -> bool {
        matches!(object, Expression::NumberLiteral(_))
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
            self.mark(f.span);
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
        if p.shorthand && !p.computed {
            self.emit_expr(&p.value, P_ASSIGN);
        } else {
            self.emit_property_key(&p.key, p.computed);
            self.punct(": ");
            self.emit_expr(&p.value, P_ASSIGN);
        }
    }

    fn emit_arrow(&mut self, a: &ArrowFunction) {
        self.mark(a.span);
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

fn same_name(a: &ModuleExportName, b: &ModuleExportName) -> bool {
    match (a, b) {
        (ModuleExportName::Ident(x), ModuleExportName::Ident(y)) => x.name == y.name,
        (ModuleExportName::String(x), ModuleExportName::String(y)) => x == y,
        _ => false,
    }
}

/// 该类是否需要装饰器降级。
///
/// plain/experimental AST emitter 尚不降级 `accessor` auto-accessor 字段，因此只保留
/// 原始语法。生产 build/optimize 路径在 owned IR 中完整 materialize 装饰器。
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
    // Rust's float-to-int cast saturates. Guard the conversion explicitly: `1e20 as i64`
    // becomes `i64::MAX`, which is a different JavaScript number when parsed again. The upper
    // bound is exclusive because `i64::MAX as f64` rounds to exactly 2^63.
    const I64_MIN_AS_F64: f64 = -9_223_372_036_854_775_808.0;
    const I64_MAX_EXCLUSIVE_AS_F64: f64 = 9_223_372_036_854_775_808.0;
    if v == v.trunc() && (I64_MIN_AS_F64..I64_MAX_EXCLUSIVE_AS_F64).contains(&v) {
        let integer = v as i64;
        if integer as f64 == v {
            let _ = write!(out, "{integer}");
            return;
        }
    }
    // `Display` uses a shortest round-trip representation for finite f64 values and preserves
    // non-integral values without passing through a narrowing integer conversion.
    let _ = write!(out, "{v}");
}

#[cfg(test)]
mod tests;
