//! # wake_bundler — 编排器（Scan / Link / Emit）
//!
//! DESIGN §6：三阶段——**Scan**（从入口递归 resolve+parse 建模块图）→ **Link**（每模块
//! ESM→CJS 改写，说明符映射到内部 id）→ **Emit**（`function(module, exports, __wake_require__)`
//! 包装 + ~1KB mini runtime + 拼接）。
//!
//! Phase 3 MVP：**直接执行**（无增量），全管线接入引擎（`#[wake::task]`）是 Phase 2.5 之后的事
//! （DESIGN §13 降级预案：引擎缺席时纯执行也能产出正确产物）。

use std::path::Path;
use std::sync::{Arc, Mutex};

use wake_common::{Diagnostic, FileSystem, FxHashMap};
use wake_ecma_codegen::ModuleLinker;

mod chunk;
pub mod incremental;
mod loader;
mod session;
pub use incremental::IncrementalBundler;
pub use session::{BuildOptions, BuildRequest, BuildSession};
// 供 CLI 组装别名而无需直接依赖 wake_resolver。
pub use wake_resolver::{PnpDependencyFallback, ResolveOptions};

/// 把宿主或外来平台路径转换成产物使用的正斜杠形式，并移除 Windows verbatim 前缀。
///
/// 不能依赖 Path::components 解析其它平台的路径：Unix 会把 Windows 反斜杠视为普通字符。
pub(crate) fn path_to_slash(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = value.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = value.strip_prefix("//?/") {
        rest.to_string()
    } else {
        value
    }
}

/// 打包器。持有文件系统与全局 interner（跨模块共享 Atom，DESIGN §4.1）。
pub struct Bundler {
    session: Mutex<BuildSession>,
}

/// 打包产物。
#[derive(Clone)]
pub struct BuildOutput {
    /// 入口 chunk 源码（= `chunks[entry_chunk].code`；向后兼容单产物调用方）。
    pub bundle: String,
    /// 模块数。
    pub module_count: usize,
    /// 本轮实际重新执行 codegen 的模块数。未改变产物的模块和缓存命中均不计入。
    /// 首次构建调用方通常展示 `module_count`，增量构建展示此字段。
    pub updated_module_count: usize,
    /// 本轮复用既有 codegen 产物的模块数（内存红绿缓存 + 持久化缓存）。
    pub cached_module_count: usize,
    /// 诊断（读文件失败 / 解析错误 / 依赖解析失败）。
    pub diagnostics: Vec<Diagnostic>,
    /// 全部产物 chunk（至少 1 个 = entry）。代码分割（6.5）时含 async/shared chunk。
    pub chunks: Vec<OutputChunk>,
    /// 入口 chunk 在 `chunks` 的下标。
    pub entry_chunk: usize,
    /// 带外产物（非 JS chunk）：超阈值独立资源文件 + prod 抽取的 `.css`（WAKE-COMPATIBILITY §M3）。
    /// 由 CLI 写盘；CSS 产物供 HTML `<link>` 注入。默认空（dev / 未开启抽取）。
    pub assets: Vec<OutputAsset>,
}

/// 一个带外产物（独立写盘的资源文件）。
#[derive(Clone)]
pub struct OutputAsset {
    /// 写盘文件名（含内容 hash，如 `logo.a1b2c3d4.png` / `styles.e5f6g7h8.css`）。
    pub file_name: String,
    /// 文件字节。
    pub bytes: Vec<u8>,
    /// 是否为抽取的 CSS（`true` → HTML 注入 `<link>`；`false` → 二进制资源）。
    pub is_css: bool,
}

/// 一个产物 chunk（DESIGN §6.3）。
#[derive(Clone)]
pub struct OutputChunk {
    /// chunk 名（entry 用入口 stem，async 用根模块 stem，shared 用 `shared`+序号）。
    pub name: String,
    /// 写盘文件名（含内容 hash，如 `index.a1b2c3d4.js`）。
    pub file_name: String,
    /// chunk 源码。
    pub code: String,
    /// chunk 类型。
    pub kind: ChunkKind,
    /// 是否为入口 chunk。
    pub is_entry: bool,
    /// 运行时数字 chunk id（entry 恒 0）。
    pub chunk_id: u32,
    /// 该 chunk 承载的模块 id（升序）。
    pub module_ids: Vec<u32>,
    /// 依赖的其它 chunk 文件名（须先加载；供 manifest）。
    pub imports: Vec<String>,
    /// Source Map V3 JSON（`None` = 未启用或该路径不支持）。WAKE-COMPATIBILITY §M4d。
    /// 由 CLI 写为 `<file_name>.map` 或经 dev server 提供；`code` 末尾对应追加 `sourceMappingURL`。
    pub source_map: Option<String>,
}

/// chunk 类型：初始（entry）/ 异步（动态 import 目标）/ 共享（多 async 共享）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChunkKind {
    Initial,
    Async,
    Shared,
}

impl ChunkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChunkKind::Initial => "initial",
            ChunkKind::Async => "async",
            ChunkKind::Shared => "shared",
        }
    }
}

impl BuildOutput {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.is_error())
    }

    /// 入口 chunk。
    pub fn entry(&self) -> &OutputChunk {
        &self.chunks[self.entry_chunk]
    }
}

/// 把单一 bundle 字符串包成单元素 `chunks`（未分割路径 / MVP Bundler 用）。
pub(crate) fn single_chunk(
    bundle: String,
    module_count: usize,
    diagnostics: Vec<Diagnostic>,
    module_ids: Vec<u32>,
) -> BuildOutput {
    let chunk = OutputChunk {
        name: "bundle".to_string(),
        file_name: "bundle.js".to_string(),
        code: bundle.clone(),
        kind: ChunkKind::Initial,
        is_entry: true,
        chunk_id: 0,
        module_ids,
        imports: Vec::new(),
        source_map: None, // 由调用方（IncrementalBundler）在启用 sourcemap 时回填
    };
    BuildOutput {
        bundle,
        module_count,
        updated_module_count: 0,
        cached_module_count: 0,
        diagnostics,
        chunks: vec![chunk],
        entry_chunk: 0,
        assets: Vec::new(),
    }
}

/// 已扫描的模块。
impl Bundler {
    pub fn new(fs: Arc<dyn FileSystem>) -> Bundler {
        Bundler {
            session: Mutex::new(BuildSession::new(fs, BuildOptions::default())),
        }
    }

    /// 从 `entry` 打包。兼容旧 API，内部统一通过 [`BuildSession`] 执行。
    pub fn build(&self, entry: &Path) -> BuildOutput {
        self.session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .build_entry(entry)
    }
}

const SMALL_LINKER_LOOKUP: usize = 8;

pub(crate) enum SpecifierLookup<'a> {
    Slice(&'a [(String, u32)]),
    Map(FxHashMap<&'a str, u32>),
}

impl<'a> SpecifierLookup<'a> {
    pub(crate) fn new(entries: &'a [(String, u32)]) -> Self {
        if entries.len() <= SMALL_LINKER_LOOKUP {
            Self::Slice(entries)
        } else {
            Self::Map(
                entries
                    .iter()
                    .map(|(specifier, id)| (specifier.as_str(), *id))
                    .collect(),
            )
        }
    }

    fn get(&self, specifier: &str) -> Option<u32> {
        match self {
            Self::Slice(entries) => entries
                .iter()
                .find_map(|(candidate, id)| (candidate == specifier).then_some(*id)),
            Self::Map(entries) => entries.get(specifier).copied(),
        }
    }
}

pub(crate) struct Linker<'a> {
    pub(crate) map: SpecifierLookup<'a>,
    /// 动态 import 说明符 → async/shared chunk id（代码分割，6.5）。空 = 不分割。
    pub(crate) dyn_chunk: SpecifierLookup<'a>,
    /// 本模块依赖里属于 async 子图（顶层 await 传染）的模块 id。空 = 无顶层 await。
    pub(crate) async_ids: &'a [u32],
}

impl ModuleLinker for Linker<'_> {
    fn module_id(&self, specifier: &str) -> Option<u32> {
        self.map.get(specifier)
    }
    fn dynamic_chunk(&self, specifier: &str) -> Option<u32> {
        self.dyn_chunk.get(specifier)
    }
    fn is_async_module(&self, id: u32) -> bool {
        self.async_ids.contains(&id)
    }
}

/// mini runtime 前半（模块注册表定义之前）。含 CJS interop helper（DESIGN §6.1）：
/// `__wake_interop_default` 让 `import X from 'cjs'` 对纯 CJS 取整个 exports、对转译 ESM 取 `.default`；
/// `__wake_interop_star` 为 `import * as ns` 提供 namespace（纯 CJS 补 `default` = 整个 exports）。
pub(crate) const PRELUDE: &str = r#"(function(root) {
var __wake_cache__ = {};
function __wake_require__(id) {
  var cached = __wake_cache__[id];
  if (cached) return cached.exports;
  var module = { exports: {} };
  __wake_cache__[id] = module;
  __wake_modules__[id].call(module.exports, module, module.exports, __wake_require__);
  return module.exports;
}
function __wake_interop_default(m) { return m && m.__esModule ? m.default : m; }
function __wake_interop_star(m) {
  if (m && m.__esModule) return m;
  var ns = {};
  if (m != null) { for (var k in m) if (Object.prototype.hasOwnProperty.call(m, k) && k !== "default") ns[k] = m[k]; }
  ns.default = m;
  return ns;
}
__wake_require__.metaUrl = function () {
  return typeof document !== "undefined" ? document.baseURI : "";
};
"#;

/// mini runtime 前半的 **async 变体**：产物含顶层 await 时启用（DESIGN §6.1.1）。
///
/// 与 [`PRELUDE`] 的唯一差别是 `__wake_require__`：async 模块的包装器是 `async function`，
/// `.call(...)` 返回 Promise → 缓存该 Promise（`module.p`）并返回它，使导入方 `await` 得到
/// 求值完毕的 `module.exports`。同步模块的返回值是 `undefined`（非 thenable），走原路径不受影响。
/// 循环依赖下先拿到的是**部分填充**的 exports，与同步路径语义一致（不会死锁）。
pub(crate) const PRELUDE_ASYNC: &str = r#"(function(root) {
var __wake_cache__ = {};
function __wake_require__(id) {
  var cached = __wake_cache__[id];
  if (cached) return cached.p || cached.exports;
  var module = { exports: {} };
  __wake_cache__[id] = module;
  var r = __wake_modules__[id].call(module.exports, module, module.exports, __wake_require__);
  if (r && typeof r.then === "function") {
    module.p = r.then(function () { return module.exports; });
    return module.p;
  }
  return module.exports;
}
function __wake_interop_default(m) { return m && m.__esModule ? m.default : m; }
function __wake_interop_star(m) {
  if (m && m.__esModule) return m;
  var ns = {};
  if (m != null) { for (var k in m) if (Object.prototype.hasOwnProperty.call(m, k) && k !== "default") ns[k] = m[k]; }
  ns.default = m;
  return ns;
}
__wake_require__.metaUrl = function () {
  return typeof document !== "undefined" ? document.baseURI : "";
};
"#;

/// mini runtime 后半（入口执行之后，导出到 module.exports 或全局）。
pub(crate) const POSTLUDE: &str = r#"if (typeof module !== "undefined" && module.exports) module.exports = __wake_entry__;
else root.__wake_entry__ = __wake_entry__;
return __wake_entry__;
})(typeof globalThis !== "undefined" ? globalThis : this);
"#;

#[cfg(test)]
mod tests;
