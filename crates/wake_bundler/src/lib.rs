//! # wake_bundler — 编排器（Scan / Link / Emit）
//!
//! DESIGN §6：三阶段——**Scan**（从入口递归 resolve+parse 建模块图）→ **Link**（每模块
//! ESM→CJS 改写，说明符映射到内部 id）→ **Emit**（`function(module, exports, __wake_require__)`
//! 包装 + ~1KB mini runtime + 拼接）。
//!
//! Phase 3 MVP：**直接执行**（无增量），全管线接入引擎（`#[wake::task]`）是 Phase 2.5 之后的事
//! （DESIGN §13 降级预案：引擎缺席时纯执行也能产出正确产物）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wake_common::{Atom, Diagnostic, FileSystem, FxHashMap, Interner, fs::normalize};
use wake_ecma_ast::{ModuleAst, SourceType};
use wake_ecma_codegen::{ModuleLinker, codegen_module};
use wake_ecma_parser::parse;
use wake_resolver::Resolver;

mod chunk;
pub mod incremental;
mod loader;
pub use incremental::IncrementalBundler;
// 供 CLI 组装别名而无需直接依赖 wake_resolver。
pub use wake_resolver::ResolveOptions;

/// 打包器。持有文件系统与全局 interner（跨模块共享 Atom，DESIGN §4.1）。
pub struct Bundler {
    fs: Arc<dyn FileSystem>,
    interner: Interner,
}

/// 打包产物。
pub struct BuildOutput {
    /// 入口 chunk 源码（= `chunks[entry_chunk].code`；向后兼容单产物调用方）。
    pub bundle: String,
    /// 模块数。
    pub module_count: usize,
    /// 诊断（读文件失败 / 解析错误 / 依赖解析失败）。
    pub diagnostics: Vec<Diagnostic>,
    /// 全部产物 chunk（至少 1 个 = entry）。代码分割（6.5）时含 async/shared chunk。
    pub chunks: Vec<OutputChunk>,
    /// 入口 chunk 在 `chunks` 的下标。
    pub entry_chunk: usize,
    /// 带外产物（非 JS chunk）：超阈值独立资源文件 + prod 抽取的 `.css`（CRUSTIFY-PARITY §M3）。
    /// 由 CLI 写盘；CSS 产物供 HTML `<link>` 注入。默认空（dev / 未开启抽取）。
    pub assets: Vec<OutputAsset>,
}

/// 一个带外产物（独立写盘的资源文件）。
pub struct OutputAsset {
    /// 写盘文件名（含内容 hash，如 `logo.a1b2c3d4.png` / `styles.e5f6g7h8.css`）。
    pub file_name: String,
    /// 文件字节。
    pub bytes: Vec<u8>,
    /// 是否为抽取的 CSS（`true` → HTML 注入 `<link>`；`false` → 二进制资源）。
    pub is_css: bool,
}

/// 一个产物 chunk（DESIGN §6.3）。
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
    };
    BuildOutput {
        bundle,
        module_count,
        diagnostics,
        chunks: vec![chunk],
        entry_chunk: 0,
        assets: Vec::new(),
    }
}

/// 已扫描的模块。
struct Module {
    id: u32,
    ast: ModuleAst,
    /// 原始说明符 → 内部模块 id（供 linker）。
    deps: Vec<(Atom, u32)>,
}

impl Bundler {
    pub fn new(fs: Arc<dyn FileSystem>) -> Bundler {
        Bundler {
            fs,
            interner: Interner::new(),
        }
    }

    /// 从 `entry` 打包，产出单 chunk bundle。
    pub fn build(&self, entry: &Path) -> BuildOutput {
        let resolver = Resolver::new(self.fs.clone());
        let mut diagnostics = Vec::new();

        // —— Scan：BFS 建模块图 ——
        let mut path_to_id: FxHashMap<PathBuf, u32> = FxHashMap::default();
        let mut worklist: Vec<(u32, PathBuf)> = Vec::new();
        let mut modules: Vec<Module> = Vec::new();
        let mut next_id: u32 = 0;

        let entry_norm = normalize(entry);
        let entry_id = intern_id(&mut path_to_id, &mut worklist, &mut next_id, entry_norm);

        while let Some((id, path)) = worklist.pop() {
            let source = match self.fs.read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    diagnostics.push(
                        Diagnostic::error(format!("无法读取模块 `{}`：{e}", path.display()))
                            .with_code("WAKE0300"),
                    );
                    continue;
                }
            };
            let out = parse(&source, &self.interner, SourceType::Module);
            diagnostics.extend(out.diagnostics);

            let from_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
            let mut deps: Vec<(Atom, u32)> = Vec::new();
            for dep in &out.dependencies {
                let spec = self.interner.resolve(dep.specifier);
                match resolver.resolve(&spec, &from_dir) {
                    Ok(resolved) => {
                        let dep_id =
                            intern_id(&mut path_to_id, &mut worklist, &mut next_id, resolved);
                        deps.push((dep.specifier, dep_id));
                    }
                    Err(_) => {
                        diagnostics.push(
                            Diagnostic::error(format!(
                                "无法从 `{}` 解析依赖 `{spec}`",
                                path.display()
                            ))
                            .with_code("WAKE0301")
                            .with_primary(dep.span, "此依赖"),
                        );
                    }
                }
            }
            modules.push(Module {
                id,
                ast: out.module,
                deps,
            });
        }

        // —— Link + Emit ——
        let bundle = self.emit(&modules, entry_id);
        let module_ids: Vec<u32> = modules.iter().map(|m| m.id).collect();
        single_chunk(bundle, modules.len(), diagnostics, module_ids)
    }

    /// 每模块 ESM→CJS 链接 + 函数包装 + 运行时拼接。
    fn emit(&self, modules: &[Module], entry_id: u32) -> String {
        let mut out = String::new();
        out.push_str(PRELUDE);
        out.push_str("var __wake_modules__ = {\n");

        for m in modules {
            let mut map: FxHashMap<String, u32> = FxHashMap::default();
            for (atom, id) in &m.deps {
                map.insert(self.interner.resolve(*atom), *id);
            }
            let linker = Linker {
                map,
                dyn_chunk: FxHashMap::default(),
            };
            let body = m
                .ast
                .with_ast(|program| codegen_module(program, &self.interner, &linker, false));

            out.push_str(&format!(
                "{}: function(module, exports, __wake_require__) {{\n",
                m.id
            ));
            for line in body.lines() {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("},\n");
        }

        out.push_str("};\n");
        out.push_str(&format!(
            "var __wake_entry__ = __wake_require__({entry_id});\n"
        ));
        out.push_str(POSTLUDE);
        out
    }
}

/// 分配/复用模块 id；新路径入队。
fn intern_id(
    path_to_id: &mut FxHashMap<PathBuf, u32>,
    worklist: &mut Vec<(u32, PathBuf)>,
    next_id: &mut u32,
    path: PathBuf,
) -> u32 {
    if let Some(&id) = path_to_id.get(&path) {
        return id;
    }
    let id = *next_id;
    *next_id += 1;
    path_to_id.insert(path.clone(), id);
    worklist.push((id, path));
    id
}

pub(crate) struct Linker {
    pub(crate) map: FxHashMap<String, u32>,
    /// 动态 import 说明符 → async/shared chunk id（代码分割，6.5）。空 = 不分割。
    pub(crate) dyn_chunk: FxHashMap<String, u32>,
}

impl ModuleLinker for Linker {
    fn module_id(&self, specifier: &str) -> Option<u32> {
        self.map.get(specifier).copied()
    }
    fn dynamic_chunk(&self, specifier: &str) -> Option<u32> {
        self.dyn_chunk.get(specifier).copied()
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
"#;

/// mini runtime 后半（入口执行之后，导出到 module.exports 或全局）。
pub(crate) const POSTLUDE: &str = r#"if (typeof module !== "undefined" && module.exports) module.exports = __wake_entry__;
else root.__wake_entry__ = __wake_entry__;
return __wake_entry__;
})(typeof globalThis !== "undefined" ? globalThis : this);
"#;

#[cfg(test)]
mod tests;
