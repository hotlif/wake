//! # 增量 + 并行打包（引擎接入，PLAN §3.2 / DESIGN §10.3、§10.6）
//!
//! 把 `parse` 与 `codegen` 两个最贵环节接入 wake_turbo 引擎，实现：
//! - **第二遍构建全管线缓存命中**（内容/依赖未变 → parse/codegen 浅绿命中，零重执行）；
//! - **Scan 全并行**（DESIGN §10.6）：**分层 BFS**，每层用工作窃取执行器 `par_request` 并行 parse；
//!   codegen 阶段同样并行。
//!
//! ## cycle-safe 说明
//!
//! parse/codegen 任务**互不递归请求**（parse 只依赖自己的内容 cell）——任务图是「从内容 cell
//! 发散的星形」，**无环**。模块*依赖*图的循环由驱动层 BFS 的 `path_to_id` 去重集合处理，
//! 不会造成任务图环 / single-flight 死锁。故并行 scan **不需要 SCC 成组**（那是「full scan-as-tasks」
//! 递归请求路线才需要的，此处务实绕开）。
//!
//! ## 缓存与并行的确定性
//!
//! id 分配走确定性 BFS（层序 + 依赖序），`par_request` 保序返回 → 两遍相同构建 id 相同 →
//! linker cell 稳定 → 缓存命中不受并行影响。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_utils::CachePadded;
use wake_cache::{
    BuildCache, CachedDep, CachedLiveness, CachedNamedImport, CachedUse, FileStamp, ModuleSummary,
};
use wake_common::{
    Diagnostic, FileSystem, FxHashMap, FxHashSet, Interner, SourceFile, Span, fs::normalize,
};
use wake_ecma_ast::{
    DependencyKind, ExportDefaultKind, ModuleAst, ModuleExportName, Pattern, Program, SourceType,
    Statement,
};
use wake_ecma_codegen::{
    ConcatBlockInfo, Mapping, ModuleMappings, SourceMap, codegen_module_shaken_mangled,
    codegen_module_shaken_with_map, concat_block_info,
};
use wake_ecma_minify::{
    MinifyCtx, SimplifyAction, analyze_dce, analyze_vars_with_model, collect_init_map, has_hazard,
    is_undefined_shadowed, plan_mangle_with_model_and_protected, plan_simplifications,
};
use wake_ecma_parser::{analyze, parse_with};
use wake_ecma_transform::{FeatureSet, TargetEnv};
use wake_graph::{
    ImportUse, LiveResult, ModuleLiveness, NamedImport, collect_module_liveness,
    collect_static_uses, compute_live_keep,
};
use wake_resolver::{ResolveOptions, Resolver};
use wake_turbo::{Engine, Executor, TaskArg, TaskId, Vc, query};
use xxhash_rust::xxh3::xxh3_64_with_seed;

use crate::chunk::{ChunkGraph, ModuleEdges, compute_chunk_graph};
use crate::loader::{LoadOptions, Loaded, cached_source_type, load_source, push_js_string};
use crate::{
    BuildOutput, ChunkKind, Linker, OutputAsset, OutputChunk, POSTLUDE, PRELUDE, PRELUDE_ASYNC,
    SpecifierLookup, path_to_slash,
};

/// 内容输入 cell 的类型：文件源码文本（`Arc<str>`，指纹 = 内容 hash）。
type Content = Arc<str>;

/// 「说明符 → 内部模块 id」映射（dep 顺序确定，指纹稳定）。
type DepIds = Vec<(String, u32)>;
type LoadedResult = (
    u32,
    PathBuf,
    std::io::Result<Arc<Loaded>>,
    Option<FileStamp>,
    Option<u64>,
);
type ResolveResult = Result<PathBuf, wake_resolver::ResolveError>;
type CodegenExecCounts = Arc<[CachePadded<AtomicU64>]>;

/// I/O 小任务的目标批次数：既给工作窃取留出余量，又避免为数千个小文件逐个分配
/// `Job`、逐个经 channel 回传。最终顺序仍由调用方的索引槽位恢复。
const IO_BATCHES_PER_WORKER: usize = 4;
const CODEGEN_COUNTER_SHARDS: usize = 64;
fn file_stamp(path: &Path) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified_ns = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(FileStamp {
        size: metadata.len(),
        modified_ns,
    })
}

fn persistent_source_variant(jsx_salt: u64, target_fingerprint: u64) -> u64 {
    target_fingerprint ^ jsx_salt.rotate_left(23) ^ 0x7061_7468_2d76_3200
}

fn new_codegen_exec_counts() -> CodegenExecCounts {
    (0..CODEGEN_COUNTER_SHARDS)
        .map(|_| CachePadded::new(AtomicU64::new(0)))
        .collect::<Vec<_>>()
        .into()
}

fn codegen_exec_count(counts: &[CachePadded<AtomicU64>]) -> u64 {
    counts
        .iter()
        .map(|count| count.load(Ordering::Relaxed))
        .sum()
}

fn io_batch_limit(exec: &Executor) -> usize {
    if cfg!(windows) {
        exec.num_threads().min(8)
    } else {
        exec.num_threads().saturating_mul(IO_BATCHES_PER_WORKER)
    }
}

fn into_bounded_batches<T>(items: Vec<T>, max_batches: usize) -> Vec<Vec<T>> {
    if items.is_empty() {
        return Vec::new();
    }
    let batch_count = items.len().min(max_batches.max(1));
    let batch_size = items.len().div_ceil(batch_count);
    let mut items = items.into_iter();
    std::iter::from_fn(|| {
        let batch: Vec<T> = items.by_ref().take(batch_size).collect();
        (!batch.is_empty()).then_some(batch)
    })
    .collect()
}

/// 小模块的 parse/codegen 本身很快，逐模块穿过 `Executor` 的 boxed job + mpsc 会放大固定开销。
/// 外层每个任务处理一个连续小批次；`Engine::enter` 仍覆盖批内全部 query，结果按批次与批内顺序展平。
fn par_request_batched<T, F>(engine: &Arc<Engine>, exec: &Executor, requests: Vec<F>) -> Vec<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let max_batches = exec.num_threads().saturating_mul(IO_BATCHES_PER_WORKER);
    let batches: Vec<_> = into_bounded_batches(requests, max_batches)
        .into_iter()
        .map(|batch| {
            move || {
                batch
                    .into_iter()
                    .map(|request| request())
                    .collect::<Vec<_>>()
            }
        })
        .collect();
    engine
        .par_request(exec, batches)
        .into_iter()
        .flatten()
        .collect()
}

#[cfg(test)]
mod batching_tests {
    use super::*;

    #[test]
    fn bounded_batches_preserve_order_and_limit_task_count() {
        for (len, max_batches) in [(0, 0), (1, 0), (7, 3), (257, 16)] {
            let batches = into_bounded_batches((0..len).collect::<Vec<_>>(), max_batches);
            assert!(batches.len() <= len.min(max_batches.max(1)));
            assert_eq!(
                batches.into_iter().flatten().collect::<Vec<_>>(),
                (0..len).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn batched_engine_requests_preserve_order() {
        let engine = Arc::new(Engine::new());
        let exec = Executor::new(2);
        let requests: Vec<_> = (0..257usize).map(|value| move || value).collect();
        assert_eq!(
            par_request_batched(&engine, &exec, requests),
            (0..257).collect::<Vec<_>>()
        );
    }
}
#[derive(Clone)]
struct MemoryScanSummary {
    persisted: Arc<ModuleSummary>,
    deps: Vec<ParsedDep>,
    liveness: Arc<ModuleLiveness>,
    block_info: ConcatBlockInfo,
}

struct StableModuleGraph {
    entry: PathBuf,
    next_id: u32,
    modules: FxHashMap<u32, ModuleRec>,
}

/// scan 阶段单模块的解析结果：(依赖, 顶层 await 标志, 已 parse 的 AST 持有者, parse 任务句柄)。
/// 摘要命中时后两者为 `None`（不 parse）；未命中时携带新 parse 的 AST 与句柄。
type ScanParsed = (
    Vec<ParsedDep>,
    bool,
    Option<Arc<ParsedModule>>,
    Option<Vc<ParsedModule>>,
    Option<Arc<ModuleLiveness>>,
    Option<ConcatBlockInfo>,
);

/// parse miss 的工作线程结果。解析后的只读 AST 分析与 parse 同批并行完成，驱动层只做
/// 确定性的缓存/图记账，不再逐模块串行扫描 AST。
struct ParsedLayerResult {
    parse_vc: Vc<ParsedModule>,
    parsed: Arc<ParsedModule>,
    uses: Vec<(String, ImportUse)>,
    liveness: Option<Arc<ModuleLiveness>>,
    block_info: Option<ConcatBlockInfo>,
}

/// linker 输入 cell 的类型：依赖映射 + Tree Shaking 保留集 + 动态 import 的 chunk 落点。
///
/// `keep_exports`：`None` = 不 shake（入口 / 被整体使用）；`Some(已排序名单)` = 只保留这些导出。
/// `dyn_chunks`：本模块每个跨 chunk 动态 import 的 (说明符, 目标 chunk id)（排序去重）。
/// 三者都进 cell 指纹——任一变化精确触发 codegen 重跑；chunk **归属/文件名**不进指纹（纯 emit 期）。
#[derive(Clone, Hash, PartialEq, Eq, Default)]
struct LinkerData {
    deps: DepIds,
    keep_exports: Option<Vec<String>>,
    dyn_chunks: Vec<(String, u32)>,
    /// 本模块依赖里属于 **async 子图**（顶层 await 传染）的模块 id（升序去重）。
    /// codegen 据此把静态导入点写成 `(await __wake_require__(id))`。进指纹 → async 归属变化精确重跑。
    async_deps: Vec<u32>,
}

/// `parse` 任务的输出：AST 持有者 + 源文本 + 依赖（说明符已解为 `String`）+ 诊断。
/// 作为引擎 cell 值，须 `Send + Sync + 'static`（`ModuleAst` 已具备）+ 指纹。
pub struct ParsedModule {
    pub ast: ModuleAst,
    /// 原始源文本（minify 简化器需要读取 span 对应的源码文本）。
    pub source: Arc<str>,
    pub deps: Vec<ParsedDep>,
    pub diagnostics: Vec<Diagnostic>,
    /// 模块顶层含 `await` / `for await` → 该模块须包成 `async function`（见 [`async_module_ids`]）。
    pub has_top_level_await: bool,
}

/// 一条依赖：说明符文本 + 种类 + 源码位置。
#[derive(Clone, Hash)]
pub struct ParsedDep {
    pub specifier: String,
    pub kind: DependencyKind,
    pub span: Span,
}

fn same_dependency_shape(old: &[ParsedDep], new: &[ParsedDep]) -> bool {
    old.len() == new.len()
        && old
            .iter()
            .zip(new)
            .all(|(a, b)| a.specifier == b.specifier && a.kind == b.kind)
}

// 指纹只取 AST 结构 hash + 依赖（诊断是内容的函数，内容变则 AST 变、指纹变，无需入指纹）。
impl std::hash::Hash for ParsedModule {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ast.hash(state);
        self.deps.hash(state);
    }
}

/// 增量 + 并行打包器。持有引擎、工作窃取执行器与跨构建保留的输入 cell 表。
pub struct IncrementalBundler {
    fs: Arc<dyn FileSystem>,
    interner: Arc<Interner>,
    engine: Arc<Engine>,
    exec: Executor,
    /// `Arc` 包裹：每层依赖 resolve 经工作窃取执行器**并行**（`Resolver` 现为 `Sync`，见其 `cache` 注释）。
    resolver: Arc<Resolver>,
    /// 解析选项（含别名）。跨构建保留——PnP 检测切换解析器时用它重建，避免丢别名。
    resolve_options: ResolveOptions,
    /// 规范化路径 → 内容输入 cell（跨构建保留）。
    content_cells: FxHashMap<PathBuf, Vc<Content>>,
    /// 规范化路径 → linker 输入 cell（跨构建保留）。
    linker_cells: FxHashMap<PathBuf, Vc<LinkerData>>,
    /// codegen 任务体真正执行的累计次数。按构建前后差值形成准确的更新模块数。
    /// 计数器由已注册的重算闭包长期持有，因此跨 generation 仍能正确归属到同一 bundler。
    codegen_exec_counts: CodegenExecCounts,
    /// watch generation 间复用的 loader 结果。文件事件按路径精确移除，未变模块不再触碰磁盘。
    load_cache: Arc<Mutex<FxHashMap<PathBuf, Arc<Loaded>>>>,
    load_exec_count: Arc<AtomicU64>,
    load_cache_enabled: bool,
    resolve_exec_count: Arc<AtomicU64>,
    /// generation-aware session 的拥有型 scan 摘要。未变模块无需再次请求 parse 或重算活跃性。
    memory_summaries: FxHashMap<u64, MemoryScanSummary>,
    memory_parse_vcs: FxHashMap<u64, Vc<ParsedModule>>,
    /// 上一成功 generation 的完整模块图。普通内容编辑在依赖形状不变时直接复用稳定 id/边。
    stable_graph: Option<StableModuleGraph>,
    topology_invalidated: AtomicBool,
    topology_reuse_count: AtomicU64,
    last_module_count: usize,
    /// 是否启用 Tree Shaking（默认关闭；prod build 开启）。DESIGN §5.3 / PLAN §6.6。
    tree_shaking: bool,
    /// 是否启用代码分割（默认关闭；prod build 开启）。DESIGN §6.3 / PLAN §6.5。
    code_splitting: bool,
    /// 产物文件名是否带内容 hash（默认开；dev 关以稳定 URL）。
    content_hash: bool,
    /// 共享 chunk 抽取阈值（模块被 ≥N 个 async root 共享则抽取，默认 2）。
    share_threshold: usize,
    /// 持久化构建缓存（opt-in，PLAN §7.1）。`Some` 时：命中摘要跳 parse、命中产物跳 parse+codegen。
    /// 落盘路径见 `cache_path`。默认 `None`——默认构建路径与产物逐字节不变、性能不受影响。
    cache: Option<BuildCache>,
    cache_path: Option<PathBuf>,
    /// Yarn PnP 检测状态：`None` = 首次 build 前未检测；`Some(b)` = 已检测（`b`=是否为 PnP 项目）。
    /// 首次 build 时按入口向上探 `.pnp.cjs`，命中则包裹 zip 感知 fs + 切 PnP 解析器。
    pnp_detected: Option<bool>,
    /// 编译期常量替换表（`静态成员链 → 字面量源码`）。默认 `process.env.NODE_ENV → "production"`
    /// （对齐既有 codegen 默认，prod 口径）；dev/prod + 用户 `[define]` 由 CLI/dev-server 经
    /// [`set_define`](Self::set_define) 覆盖（WAKE-COMPATIBILITY §M3）。
    define: Arc<[(String, String)]>,
    /// `define` 的稳定指纹，混入产物缓存键——define 变（如 dev↔prod）→ 精确失效产物缓存。
    define_hash: u64,
    /// prod CSS 抽取（默认关；prod build 开）。开启后 CSS 不注入 `<style>`，聚合为独立 `.css` 产物。
    extract_css: bool,
    /// 资源内联字节上限（默认 `usize::MAX` = 全内联）。prod 设 4096：超阈值资源写独立产物。
    asset_inline_limit: usize,
    /// 资源 URL 的 `publicPath` 前缀（默认 `/`）。
    public_path: String,
    /// 紧凑（minify）codegen（默认关；prod build 开）。省换行/缩进（WAKE-COMPATIBILITY §M4a）。
    minify: bool,
    /// 死模块消除（默认关；prod build 开）：codegen DCE 剥离死 `require` 后，从 entry 重算可达模块、
    /// 丢弃不可达者（如 `if(false)` 里 `require('…development')` 拉进图但已不可达的 dev 包）。§M4b 后续。
    dead_module_elimination: bool,
    /// 标识符 mangling（默认关；prod build 开）：作用域安全地把非模块作用域局部/参数重命名为短名。
    /// 每模块 `wake_ecma_minify::plan_mangle` 构建 `span→新名` 侧表传入 codegen（WAKE-COMPATIBILITY §M4）。
    /// 影响产物缓存键（[`MANGLE_SALT`]）。
    mangle: bool,
    /// 移除 `console.*` 调用（默认关；prod build 可选开）。
    drop_console: bool,
    /// 移除 `debugger` 语句（默认关；prod build 可选开）。
    drop_debugger: bool,
    /// 产出 Source Map（WAKE-COMPATIBILITY §M4d）。当前仅支持 **非 minify 单包**路径：
    /// 该路径下模块体逐行原样拼接（仅前缀 2 空格缩进），行偏移法精确成立。
    /// minify 路径会 scope-hoist + 改写模块体文本，映射会错位，故 [`IncrementalBundler::build`]
    /// 在 minify 时不产 map（见 emit 处的守卫）。
    sourcemap: bool,
    /// 零运行时 CSS-in-JS（Linaria 子集）：构建期把 `` css`...` `` 抽取为静态 CSS
    /// （WAKE-COMPATIBILITY §M5）。见 [`IncrementalBundler::enable_css_in_js`]。
    css_in_js: bool,
    /// JSX 运行时口径（dev runtime / jsxImportSource）。见 [`IncrementalBundler::set_jsx_runtime`]。
    jsx: JsxRuntimeOptions,
    /// 目标环境指纹。即使 pass 尚未覆盖某语法，目标变化也不能复用旧转换缓存。
    target_fingerprint: u64,
    transform_features: FeatureSet,
}

/// 扫描完成的一个模块记录。
struct ModuleRec {
    path: PathBuf,
    /// 内容键 `hash(源类型 ‖ 源文本)`——跨进程稳定，作缓存主键。
    content_key: u64,
    source_type: SourceType,
    /// 内容输入 cell——codegen 阶段若需补 parse（产物未命中缓存）时用。
    content_vc: Vc<Content>,
    /// 依赖（来自 parse 或缓存摘要）。
    deps: Vec<ParsedDep>,
    dep_ids: DepIds,
    /// 绑定级活跃性（Tree Shaking 用；仅 prod + 新 parse 的模块有；缓存摘要命中 → `None` → 保守全保留）。
    liveness: Option<Arc<ModuleLiveness>>,
    /// 单包 concat 块安全信息（`{}` vs IIFE；缓存摘要命中 → `None` → 保守走 IIFE + 不加 strict）。
    block_info: Option<ConcatBlockInfo>,
    /// 模块顶层含 `await`（来自 parse 或缓存摘要）。async 子图的种子。
    has_top_level_await: bool,
    /// parse 结果——缓存命中摘要时为 `None`（延迟到产物未命中才 parse）。
    parse_vc: Option<Vc<ParsedModule>>,
    parsed: Option<Arc<ParsedModule>>,
}

/// BFS 一层内的一个待处理模块（读文件后、resolve 前）。
struct LayerItem {
    id: u32,
    path: PathBuf,
    content_vc: Vc<Content>,
    source_type: SourceType,
    content_key: u64,
    /// 缓存摘要命中则为 `Some`（该模块跳过 parse）。
    cached: Option<Arc<ModuleSummary>>,
    memory_liveness: Option<Arc<ModuleLiveness>>,
    memory_block_info: Option<ConcatBlockInfo>,
    memory_deps: Option<Vec<ParsedDep>>,
}

/// 驱动层两遍处理的中转态：Pass 1（串行）算好 deps/liveness/block_info 并收集 resolve 请求，
/// Pass P（并行）resolve 全部请求，Pass 2（串行）按序消费结果填 `dep_ids` + assign_id + 建 `ModuleRec`。
/// 拆两遍是为把昂贵的 resolve（FS 探测）从串行的 id 分配/建图记账中剥出来并行——而 id 分配顺序
/// （= 模块序 × 依赖序）在 Pass 2 严格复刻原单层循环，故产物逐字节不变。
struct PendingModule {
    id: u32,
    path: PathBuf,
    content_key: u64,
    source_type: SourceType,
    content_vc: Vc<Content>,
    deps: Vec<ParsedDep>,
    liveness: Option<Arc<ModuleLiveness>>,
    block_info: Option<ConcatBlockInfo>,
    has_top_level_await: bool,
    parse_vc_opt: Option<Vc<ParsedModule>>,
    parsed_opt: Option<Arc<ParsedModule>>,
}

impl IncrementalBundler {
    pub fn new(fs: Arc<dyn FileSystem>) -> IncrementalBundler {
        // 默认 define：`process.env.NODE_ENV → "production"`（与旧 codegen 硬编码默认逐字节一致）。
        let default_define: Arc<[(String, String)]> = Arc::from(vec![
            (
                "process.env.NODE_ENV".to_string(),
                "\"production\"".to_string(),
            ),
            ("import.meta.hot".to_string(), "false".to_string()),
            (
                "import.meta.url".to_string(),
                "__wake_require__.metaUrl()".to_string(),
            ),
        ]);
        let define_hash = hash_define(&default_define);
        let default_target = TargetEnv::default();
        IncrementalBundler {
            resolver: Arc::new(Resolver::new(fs.clone())),
            resolve_options: ResolveOptions::default(),
            fs,
            interner: Arc::new(Interner::new()),
            engine: Arc::new(Engine::new()),
            exec: Executor::with_default_threads(),
            define: default_define,
            define_hash,
            content_cells: FxHashMap::default(),
            linker_cells: FxHashMap::default(),
            codegen_exec_counts: new_codegen_exec_counts(),
            load_cache: Arc::new(Mutex::new(FxHashMap::default())),
            load_exec_count: Arc::new(AtomicU64::new(0)),
            load_cache_enabled: false,
            resolve_exec_count: Arc::new(AtomicU64::new(0)),
            memory_summaries: FxHashMap::default(),
            memory_parse_vcs: FxHashMap::default(),
            stable_graph: None,
            topology_invalidated: AtomicBool::new(true),
            topology_reuse_count: AtomicU64::new(0),
            last_module_count: 0,
            tree_shaking: false,
            code_splitting: false,
            content_hash: true,
            share_threshold: 2,
            cache: None,
            cache_path: None,
            pnp_detected: None,
            extract_css: false,
            asset_inline_limit: usize::MAX,
            public_path: "/".to_string(),
            minify: false,
            dead_module_elimination: false,
            mangle: false,
            drop_console: false,
            drop_debugger: false,
            sourcemap: false,
            css_in_js: false,
            jsx: JsxRuntimeOptions::default(),
            target_fingerprint: default_target.fingerprint(),
            transform_features: default_target.required_features(),
        }
    }

    /// 设置已规范化目标环境，并将其稳定指纹混入 parse/transform 内容键。
    pub fn set_target_env(&mut self, target: TargetEnv) -> &mut Self {
        let fingerprint = target.fingerprint();
        if fingerprint != self.target_fingerprint {
            // Parse tasks close over the selected transform set. A target change on a long-lived
            // bundler must not reuse task nodes created under the old target. Configuration
            // changes are rare, so rebuilding this in-memory graph is simpler and safer than
            // keeping a second configuration input on every parse edge.
            self.reset_parse_graph();
        }
        self.target_fingerprint = fingerprint;
        self.transform_features = target.required_features();
        self
    }

    fn reset_parse_graph(&mut self) {
        self.engine = Arc::new(Engine::new());
        self.content_cells.clear();
        self.linker_cells.clear();
        self.memory_summaries.clear();
        self.memory_parse_vcs.clear();
        self.stable_graph = None;
        self.topology_invalidated.store(true, Ordering::Release);
    }

    /// 若入口所在项目根（含祖先）存在 `.pnp.cjs`，启用 Yarn PnP：
    /// 用 [`PnpFileSystem`](wake_resolver::PnpFileSystem) 包裹文件系统（虚拟路径 + zip 内读取），
    /// 并把解析器切到 PnP 依赖图模式。返回是否检测到并启用 PnP。
    ///
    /// 幂等 + 惰性：`build()` 首次调用会自动触发；显式调用可提前决定日志/行为。
    pub fn enable_pnp(&mut self, start_dir: &Path) -> bool {
        if let Some(detected) = self.pnp_detected {
            return detected;
        }
        let enabled = match wake_resolver::PnpManifest::discover(self.fs.as_ref(), start_dir) {
            Some(manifest) => {
                let manifest = Arc::new(manifest);
                let wrapped: Arc<dyn FileSystem> =
                    Arc::new(wake_resolver::PnpFileSystem::new(self.fs.clone()));
                self.fs = wrapped.clone();
                // 带上已配置的别名，否则切 PnP 后会退回默认丢掉 @/@@/@@@。
                self.resolver = Arc::new(Resolver::with_pnp_options(
                    wrapped,
                    manifest,
                    self.resolve_options.clone(),
                ));
                true
            }
            None => false,
        };
        self.pnp_detected = Some(enabled);
        enabled
    }

    /// 设置解析选项（含路径别名 `@`/`@@`/`@@@`）。须在首次 `build()` 前调用（CLI 读配置后）。
    /// 重建解析器；跨构建保留选项，供 PnP 检测切换解析器时复用（不丢别名）。
    /// 保持既定行为 `resolve.alias`（WAKE-COMPATIBILITY §M1/§H）。
    pub fn set_resolve_options(&mut self, options: ResolveOptions) -> &mut Self {
        self.resolver = Arc::new(Resolver::with_options(self.fs.clone(), options.clone()));
        self.resolve_options = options;
        self
    }

    /// 设置编译期 define 表（`静态成员链 → 字面量源码`），如
    /// `[("process.env.NODE_ENV", "\"development\"")]`。指纹混入产物缓存键，
    /// dev↔prod 切换自动失效旧产物。WAKE-COMPATIBILITY §M3。
    pub fn set_define(&mut self, define: Vec<(String, String)>) -> &mut Self {
        self.define = Arc::from(define);
        self.define_hash = hash_define(&self.define);
        self
    }

    /// 启用 prod CSS 抽取（WAKE-COMPATIBILITY §M3）：CSS 不注入 `<style>`，聚合为独立 `.css` 产物
    /// （见 [`BuildOutput::assets`]）。dev 保持关闭（运行时 `<style>` 注入利于 HMR）。
    pub fn enable_css_extraction(&mut self) -> &mut Self {
        self.extract_css = true;
        self
    }

    /// 资源内联字节上限（超阈值写独立产物）。prod 设 4096；dev/默认 `usize::MAX`（全内联）。
    pub fn set_asset_inline_limit(&mut self, limit: usize) -> &mut Self {
        self.asset_inline_limit = limit;
        self
    }

    /// 资源 URL / chunk 加载的 `publicPath` 前缀（如 `/app/`）。前者由 loader 拼进模块导出的 URL，
    /// 后者由 entry chunk 注入运行时 `__wake__.publicPath`（`loadFile` 拼 async chunk 的 `script.src`）。
    pub fn set_public_path(&mut self, public_path: impl Into<String>) -> &mut Self {
        self.public_path = public_path.into();
        self
    }

    /// 启用紧凑 codegen（省换行/缩进，WAKE-COMPATIBILITY §M4a）。prod build 用；影响产物缓存键。
    pub fn enable_minify(&mut self) -> &mut Self {
        self.minify = true;
        self
    }

    /// 启用死模块消除（WAKE-COMPATIBILITY §M4b 后续）：emit 前从 entry 按存活 `require` 边重算可达模块，
    /// 丢弃不可达者。与 `enable_minify` 搭配（DCE 剥离死 `require` 后才有不可达模块可删）。安全：
    /// 边提取误判只会「多留」不会「错删」。
    pub fn enable_dead_module_elimination(&mut self) -> &mut Self {
        self.dead_module_elimination = true;
        self
    }

    /// 启用标识符 mangling（WAKE-COMPATIBILITY §M4）：每模块规划作用域安全的短名重命名（只动非模块
    /// 作用域局部/参数，模块顶层与 import/export 名保留）。须与 [`enable_minify`](Self::enable_minify)
    /// 搭配（mangle 侧表只在紧凑 codegen 路径生效）；影响产物缓存键。
    pub fn enable_mangle(&mut self) -> &mut Self {
        self.mangle = true;
        self
    }

    /// 启用 **Source Map** 产出（WAKE-COMPATIBILITY §M4d）。
    ///
    /// 产物 chunk 的 [`OutputChunk::source_map`](crate::OutputChunk::source_map) 将带上 V3 JSON，
    /// 由调用方决定写盘（`<chunk>.js.map`）或经 dev server 提供。
    ///
    /// **限制**：仅在**非 minify 单包**路径生效。minify 路径做 scope hoisting 且会改写模块体文本
    /// （`strip_hoisted_requires_and_barrels`/`compact_reg_body`），行偏移法失效，此时不产 map
    /// （宁可没有，也不给出错位的映射——错位的 sourcemap 比没有更误导）。
    pub fn enable_sourcemap(&mut self) -> &mut Self {
        self.sourcemap = true;
        self
    }

    /// 为每个模块解析 CSS-in-JS 插值可见的 import 绑定静态值。
    ///
    /// 按**依赖拓扑序**推进，实现跨模块**多层**常量传播：处理某模块前，其依赖的导出已算好，
    /// 于是该模块的导出常量可以引用依赖的常量，再供下游引用——`a → b → c` 的链可完整传播。
    ///
    /// 环的处理：DFS 后序遍历，遇到正在访问中的节点直接跳过（该边不参与传播）。环内模块仍会
    /// 用「当时已算出的部分」求值——可能少解析出几个值，但绝不会产出错误值。
    fn resolve_css_in_js_scopes(
        &self,
        modules: &FxHashMap<u32, ModuleRec>,
        ordered: &[u32],
    ) -> FxHashMap<u32, Arc<wake_css_in_js::value::Scope>> {
        use wake_css_in_js::value::{Scope, collect_imports};

        // 依赖后序：保证每个模块被处理时，其依赖（若非环）已处理完。
        let mut order: Vec<u32> = Vec::with_capacity(ordered.len());
        let mut state: FxHashMap<u32, u8> = FxHashMap::default(); // 0=访问中 1=已完成
        for &root in ordered {
            let mut stack = vec![(root, false)];
            while let Some((id, expanded)) = stack.pop() {
                if expanded {
                    if state.insert(id, 1) != Some(1) {
                        order.push(id);
                    }
                    continue;
                }
                match state.get(&id) {
                    Some(_) => continue, // 已完成 或 访问中（成环）→ 跳过该边
                    None => {
                        state.insert(id, 0);
                        stack.push((id, true));
                        if let Some(rec) = modules.get(&id) {
                            for (_, dep) in rec.dep_ids.iter() {
                                if !state.contains_key(dep) {
                                    stack.push((*dep, false));
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut exports_of: FxHashMap<u32, wake_css_in_js::StaticExports> = FxHashMap::default();
        let mut scopes: FxHashMap<u32, Scope> = FxHashMap::default();

        for &id in &order {
            let Some(rec) = modules.get(&id) else {
                continue;
            };
            // 未 parse 的模块（缓存摘要命中）没有 AST 可求值，跳过。
            let Some(parsed) = &rec.parsed else { continue };

            // ① 用**已算好的依赖导出**装配本模块的 import 作用域。
            let imports = parsed.ast.with_ast(|p| collect_imports(p, &self.interner));
            let mut scope = Scope::default();
            for (local, specifier, imported_name) in imports {
                // 说明符 → 依赖模块 id（linker 已解析过的同一张表）。
                let Some(dep_id) = rec
                    .dep_ids
                    .iter()
                    .find(|(s, _)| *s == specifier)
                    .map(|(_, d)| *d)
                else {
                    continue;
                };
                let Some(ex) = exports_of.get(&dep_id) else {
                    continue;
                };
                if imported_name == "*" {
                    // `import * as ns` → 命名空间对象（按名排序保证产物确定性）
                    let mut v: Vec<_> =
                        ex.iter().map(|(k, val)| (k.clone(), val.clone())).collect();
                    v.sort_by(|a, b| a.0.cmp(&b.0));
                    scope.insert(local, wake_css_in_js::StaticValue::Obj(v));
                } else if let Some(v) = ex.get(&imported_name) {
                    scope.insert(local, v.clone());
                }
            }

            // ② 带着该作用域算本模块的导出，供下游模块引用（多层传播的关键）。
            // seed 须与 codegen 期 `transform` 用的完全一致，否则跨模块引用到的类名会对不上。
            let seed = path_to_slash(&rec.path);
            let ex = parsed.ast.with_ast(|p| {
                wake_css_in_js::collect_static_exports_with(p, &self.interner, &seed, &scope)
            });
            if !ex.is_empty() {
                exports_of.insert(id, ex);
            }
            scopes.insert(id, scope);
        }

        // 没有可用 import 的模块也要有空作用域：其自身顶层 const 仍可求值。
        ordered
            .iter()
            .map(|&id| (id, Arc::new(scopes.remove(&id).unwrap_or_default())))
            .collect()
    }

    /// 设置 JSX 运行时口径。
    ///
    /// - `dev = true` → 用 **dev runtime**（`jsxDEV` + `{fileName,lineNumber,columnNumber}`），
    ///   React DevTools 借此显示组件栈；`wake dev` 应开启。
    /// - `import_source` → `jsxImportSource`（默认 `"react"`，可指向 `preact` 等）。
    ///
    /// 该口径会**改变解析出的依赖说明符**，故已混入 `content_key`（见 [`content_key_of`]）——
    /// dev 与 prod 的模块摘要缓存彼此隔离，不会交叉复用。
    pub fn set_jsx_runtime(&mut self, dev: bool, import_source: &'static str) -> &mut Self {
        let next = JsxRuntimeOptions { dev, import_source };
        if self.jsx != next {
            self.reset_parse_graph();
            // loader 会在 React production 下为错误发布的 jsxDEV 依赖生成兼容入口；
            // 长生命周期 bundler 切换 dev/prod 后不能复用另一口径的已加载源码。
            self.load_cache.lock().unwrap().clear();
        }
        self.jsx = next;
        self
    }

    /// 启用**零运行时 CSS-in-JS**（Linaria / wyw-in-js 子集，WAKE-COMPATIBILITY §M5）。
    ///
    /// 从 `@linaria/core` 等 import 的 `` css`...` `` 标签模板在构建期求值并抽取为静态 CSS，
    /// 表达式替换为类名字符串——运行时零样式计算。插值支持字面量/模板/对象/数组/成员访问，
    /// 以及它们引用的顶层 `const`（含跨模块 import 的静态导出）；无法求值者报警并跳过该条声明。
    ///
    /// prod（配合 [`enable_css_extraction`](Self::enable_css_extraction)）汇入
    /// `styles.<hash>.css`；dev 则随模块体 `<style>` 注入。
    pub fn enable_css_in_js(&mut self) -> &mut Self {
        self.css_in_js = true;
        self
    }

    /// 启用 `console.*` 调用移除（prod build 可选）。
    pub fn enable_drop_console(&mut self) -> &mut Self {
        self.drop_console = true;
        self
    }

    /// 启用 `debugger` 语句移除（prod build 可选）。
    pub fn enable_drop_debugger(&mut self) -> &mut Self {
        self.drop_debugger = true;
        self
    }

    /// 是否已启用 Yarn PnP（供 CLI 日志用；须在 `build()`/`enable_pnp` 之后查询）。
    pub fn is_pnp(&self) -> bool {
        self.pnp_detected == Some(true)
    }

    /// 启用持久化构建缓存（PLAN §7.1）：从 `path` 载入既有缓存；构建结束 `store` 回盘。
    /// 让全新进程的冷构建跳过未变模块的 parse + codegen（「冷启动首跑毫秒级」）。opt-in。
    pub fn enable_persistent_cache(&mut self, path: PathBuf) -> &mut Self {
        self.cache = Some(BuildCache::load(&path));
        self.cache_path = Some(path);
        self
    }

    /// 启用 Tree Shaking（移除未用导出，PLAN §6.6）。prod build 用；dev 保持关闭利于 HMR。
    pub fn enable_tree_shaking(&mut self) -> &mut Self {
        self.tree_shaking = true;
        self
    }

    /// 启用代码分割（动态 import 切 async chunk，PLAN §6.5）。prod build 用；dev 保持单包利于 HMR。
    pub fn enable_code_splitting(&mut self) -> &mut Self {
        self.code_splitting = true;
        self
    }

    /// 设置产物文件名是否带内容 hash（默认开）。dev 若开分割宜关（稳定 URL）。
    pub fn set_content_hash(&mut self, on: bool) -> &mut Self {
        self.content_hash = on;
        self
    }

    /// 引擎累计任务执行次数（parse + codegen）。第二遍构建应不增 → 全管线缓存命中。
    pub fn task_exec_count(&self) -> u64 {
        self.engine.exec_count()
    }

    /// 实际执行 loader/文件读取的累计次数；load snapshot 命中不增加。
    pub fn load_exec_count(&self) -> u64 {
        self.load_exec_count.load(Ordering::Relaxed)
    }

    pub fn resolve_exec_count(&self) -> u64 {
        self.resolve_exec_count.load(Ordering::Relaxed)
    }

    pub fn topology_reuse_count(&self) -> u64 {
        self.topology_reuse_count.load(Ordering::Relaxed)
    }

    /// 由 generation-aware `BuildSession` 启用。直接打包器保持每次读取文件的兼容语义。
    pub(crate) fn enable_load_cache(&mut self) {
        self.load_cache_enabled = true;
    }

    /// 通知文件系统 generation 已变化。内容 cell 会在下一次扫描时按文本精确更新；
    /// resolver 的成功/失败路径缓存必须立即清空，以识别新增、删除和重命名文件。
    pub fn invalidate_filesystem(&self) {
        self.resolver.clear_cache();
        self.load_cache.lock().unwrap().clear();
        self.topology_invalidated.store(true, Ordering::Release);
    }

    /// 精确失效 watcher 报告的路径。创建/删除/重命名还会使路径解析结果失效；
    /// 普通内容修改保留 resolver cache。
    pub fn invalidate_paths(&self, paths: &[PathBuf], structural: bool) {
        let normalized: FxHashSet<PathBuf> = paths.iter().map(|p| normalize(p)).collect();
        let resolution_metadata_changed = normalized.iter().any(|p| {
            p.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    matches!(
                        name,
                        "package.json"
                            | "wake.toml"
                            | ".pnp.cjs"
                            | "yarn.lock"
                            | "package-lock.json"
                            | "pnpm-lock.yaml"
                    )
                })
        });
        let asset_changed = normalized.iter().any(|p| {
            p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                matches!(
                    e,
                    "png"
                        | "jpg"
                        | "jpeg"
                        | "gif"
                        | "svg"
                        | "webp"
                        | "avif"
                        | "ico"
                        | "bmp"
                        | "woff"
                        | "woff2"
                        | "ttf"
                        | "otf"
                        | "eot"
                )
            })
        });
        self.load_cache.lock().unwrap().retain(|path, _| {
            !(normalized.contains(path)
                || asset_changed
                    && path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("css")))
        });
        if structural || resolution_metadata_changed {
            self.resolver.clear_cache();
            self.topology_invalidated.store(true, Ordering::Release);
        }
    }

    /// 刷新上一 generation 中 loader snapshot 已失效的模块。
    ///
    /// 只有依赖的说明符、种类及顺序都保持不变时才提交更新；任一模块改变依赖形状便返回
    /// `None`，调用方回退确定性的全量 BFS。这样普通实现编辑可复用稳定 id/解析边，而新增、
    /// 删除、重命名及 import 图变化仍走完整发现流程。
    fn refresh_stable_graph(
        &mut self,
        graph: &mut StableModuleGraph,
        load_opts: Arc<LoadOptions>,
        need_uses: bool,
        timing: bool,
        read_time: &mut std::time::Duration,
    ) -> Option<()> {
        let dirty: Vec<(u32, PathBuf)> = {
            let cache = self.load_cache.lock().unwrap();
            (0..graph.next_id)
                .filter_map(|id| {
                    let rec = graph.modules.get(&id)?;
                    (!cache.contains_key(&rec.path)).then(|| (id, rec.path.clone()))
                })
                .collect()
        };
        if dirty.is_empty() {
            self.topology_reuse_count.fetch_add(1, Ordering::Relaxed);
            return Some(());
        }

        let jobs: Vec<_> = dirty
            .into_iter()
            .map(|(id, path)| {
                let fs = self.fs.clone();
                let opts = load_opts.clone();
                let load_cache = self.load_cache.clone();
                let load_exec_count = self.load_exec_count.clone();
                move || {
                    load_exec_count.fetch_add(1, Ordering::Relaxed);
                    let loaded = load_source(fs.as_ref(), &path, opts.as_ref()).map(|loaded| {
                        let loaded = Arc::new(loaded);
                        load_cache
                            .lock()
                            .unwrap()
                            .insert(path.clone(), loaded.clone());
                        loaded
                    });
                    (id, path, loaded)
                }
            })
            .collect();
        let tr = timing.then(std::time::Instant::now);
        let loaded_results = self.exec.parallel(jobs);
        if let Some(t) = tr {
            *read_time += t.elapsed();
        }
        if loaded_results.iter().any(|(_, _, loaded)| loaded.is_err()) {
            return None;
        }

        struct RefreshItem {
            id: u32,
            path: PathBuf,
            loaded: Arc<Loaded>,
            content_key: u64,
            content_vc: Vc<Content>,
        }

        let mut items = Vec::with_capacity(loaded_results.len());
        for (id, path, loaded) in loaded_results {
            let loaded = loaded.ok()?;
            let content_key = content_key_of(
                &loaded.source,
                loaded.source_type,
                self.jsx,
                self.target_fingerprint,
                &path,
            );
            let content_vc = self.content_cell(&path, &loaded.source);
            items.push(RefreshItem {
                id,
                path,
                loaded,
                content_key,
                content_vc,
            });
        }

        let requests: Vec<_> = items
            .iter()
            .map(|item| {
                let cell = item.content_vc;
                let st = item.loaded.source_type;
                let interner = self.interner.clone();
                let jsx = self.jsx;
                let transform_features = self.transform_features;
                let file_name = jsx.dev.then(|| Arc::<str>::from(path_to_slash(&item.path)));
                move || parse_request(cell, interner, st, jsx, transform_features, file_name)
            })
            .collect();
        let engine = Arc::clone(&self.engine);
        let parsed_results = par_request_batched(&engine, &self.exec, requests);

        // 先验证全部依赖形状，再修改持久图。失败时整个候选图被调用方丢弃并走全量扫描。
        for (item, (_, parsed)) in items.iter().zip(&parsed_results) {
            let old = graph.modules.get(&item.id)?;
            if old.source_type != item.loaded.source_type
                || !same_dependency_shape(&old.deps, &parsed.deps)
            {
                return None;
            }
        }

        for (item, (parse_vc, parsed)) in items.into_iter().zip(parsed_results) {
            let deps = parsed.deps.clone();
            let uses = if need_uses {
                parsed
                    .ast
                    .with_ast(|p| collect_static_uses(p, self.interner.as_ref()))
            } else {
                Vec::new()
            };
            let liveness = (self.cache.is_some() || self.load_cache_enabled || self.tree_shaking)
                .then(|| {
                    Arc::new(
                        parsed
                            .ast
                            .with_ast(|p| collect_module_liveness(p, self.interner.as_ref())),
                    )
                });
            let block_info = (self.cache.is_some() || self.load_cache_enabled || self.minify)
                .then(|| parsed.ast.with_ast(concat_block_info));

            self.memory_parse_vcs.insert(item.content_key, parse_vc);
            if parsed.diagnostics.is_empty() && (self.cache.is_some() || self.load_cache_enabled) {
                let live = liveness
                    .as_ref()
                    .expect("cache summary always includes liveness");
                let block = block_info.expect("cache summary always includes concat info");
                let summary = Arc::new(ModuleSummary {
                    deps: deps.iter().map(parsed_dep_to_cached).collect(),
                    uses: uses.iter().map(import_use_to_cached).collect(),
                    has_top_level_await: parsed.has_top_level_await,
                    liveness: runtime_liveness_to_cached(live, &self.interner),
                    concat_is_esm: block.is_esm,
                    concat_block_safe: block.block_safe,
                });
                self.memory_summaries.insert(
                    item.content_key,
                    MemoryScanSummary {
                        persisted: summary.clone(),
                        deps: deps.clone(),
                        liveness: live.clone(),
                        block_info: block,
                    },
                );
                if let Some(cache) = self.cache.as_mut() {
                    cache.put_summary(item.content_key, (*summary).clone());
                }
            }

            let rec = graph.modules.get_mut(&item.id)?;
            rec.content_key = item.content_key;
            rec.source_type = item.loaded.source_type;
            rec.content_vc = item.content_vc;
            rec.deps = deps;
            rec.liveness = self.tree_shaking.then_some(liveness).flatten();
            rec.block_info = self.minify.then_some(block_info).flatten();
            rec.has_top_level_await = parsed.has_top_level_await;
            rec.parse_vc = Some(parse_vc);
            rec.parsed = Some(parsed);
        }

        self.topology_reuse_count.fetch_add(1, Ordering::Relaxed);
        Some(())
    }

    /// 从 `entry` 增量 + 并行打包。
    pub fn build(&mut self, entry: &Path) -> BuildOutput {
        let codegen_exec_before = codegen_exec_count(&self.codegen_exec_counts);
        // 首次 build 前惰性探测 Yarn PnP（从入口目录向上找 `.pnp.cjs`）。命中则 fs 变 zip 感知、
        // 解析器切 PnP 模式；非 PnP 项目无副作用。dev/watch 复用同一 bundler，只探一次。
        if self.pnp_detected.is_none() {
            let entry_norm = normalize(entry);
            let start_dir = entry_norm.parent().unwrap_or(Path::new("")).to_path_buf();
            self.enable_pnp(&start_dir);
        }
        // 分阶段计时（仅 `WAKE_TIMING` 环境变量存在时打印，用于定位性能热点）。
        let timing = std::env::var_os("WAKE_TIMING").is_some();
        let t0 = std::time::Instant::now();
        let mut resolve_time = std::time::Duration::ZERO;
        let mut read_time = std::time::Duration::ZERO;

        let mut diagnostics = Vec::new();
        let entry_norm = normalize(entry);

        // uses 只进缓存摘要（Tree Shaking 的 keep 集走绑定级 liveness，不读 uses），无缓存则白算。
        let need_uses = self.cache.is_some() || self.load_cache_enabled;

        // 加载选项（CSS 抽取 / 资源阈值 / publicPath）+ 带外产物收集（WAKE-COMPATIBILITY §M3）。
        // Arc 包裹：每层并行读取时克隆句柄进 worker 闭包（`'static` 约束）。
        let load_opts = Arc::new(LoadOptions {
            extract_css: self.extract_css,
            asset_inline_limit: self.asset_inline_limit,
            public_path: self.public_path.clone(),
            jsx_dev: self.jsx.dev,
            jsx_import_source: self.jsx.import_source,
        });

        let mut path_to_id: FxHashMap<PathBuf, u32> =
            FxHashMap::with_capacity_and_hasher(self.last_module_count, Default::default());
        let mut next_id: u32 = 0;
        let mut modules: FxHashMap<u32, ModuleRec> =
            FxHashMap::with_capacity_and_hasher(self.last_module_count, Default::default());
        let entry_id = assign_id(&mut path_to_id, &mut next_id, entry_norm.clone());
        let mut frontier: Vec<(u32, PathBuf)> = vec![(entry_id, entry_norm.clone())];
        let mut collected_assets: Vec<(String, Vec<u8>)> = Vec::new();
        let mut collected_css: Vec<(u32, String)> = Vec::new();

        // 普通内容编辑：取出上一成功 generation 的图，只刷新 loader snapshot 缺失的模块。
        // 依赖形状不变则 frontier 清空，后续直接进入 link/codegen；否则丢弃候选并走下方完整 BFS。
        if self.load_cache_enabled
            && !self.topology_invalidated.load(Ordering::Acquire)
            && let Some(mut graph) = self.stable_graph.take()
            && graph.entry == entry_norm
            && self
                .refresh_stable_graph(
                    &mut graph,
                    load_opts.clone(),
                    need_uses,
                    timing,
                    &mut read_time,
                )
                .is_some()
        {
            next_id = graph.next_id;
            modules = graph.modules;
            frontier.clear();

            // 全量扫描时带外产物在 loader 消费处收集；复用图时从仍有效的 snapshot 按稳定 id 重放。
            let cache = self.load_cache.lock().unwrap();
            for id in 0..next_id {
                let Some(rec) = modules.get(&id) else {
                    continue;
                };
                if let Some(parsed) = &rec.parsed {
                    extend_module_diagnostics(&mut diagnostics, &parsed.diagnostics, &rec.path);
                }
                let Some(loaded) = cache.get(&rec.path) else {
                    continue;
                };
                collected_assets.extend(loaded.assets.iter().cloned());
                if let Some(css) = &loaded.css {
                    collected_css.push((id, css.clone()));
                }
            }
        } else {
            // 候选图不匹配/已失效时不得留到下一次 generation。
            self.stable_graph = None;
        }

        // —— 分层 BFS：每层并行 parse（命中缓存摘要的模块跳过 parse）——
        while !frontier.is_empty() {
            // 1. 驱动层：读文件 + 建内容 cell + 算 content_key；查缓存摘要决定是否需 parse。
            //
            // **并行读取**：本层所有文件经工作窃取执行器并发 `load_source`（I/O 密集——Windows
            // CreateFile + Defender 逐文件同步开销大，串行读 2015 文件实测 ~115ms，并行后 ~25ms）。
            // 结果按输入顺序返回（`Executor::parallel` 保序）→ 后续串行建 cell/查缓存的顺序、
            // assign_id 与产物收集序完全不变 → 产物逐字节一致。读完再做 `&mut self` 的 cell/缓存
            // 记账（这些依赖共享可变状态，本就该串行；它们很便宜）。
            let frontier_items: Vec<(u32, PathBuf)> = std::mem::take(&mut frontier);
            let persistent_variant =
                persistent_source_variant(self.jsx.salt(), self.target_fingerprint);
            let tr = timing.then(std::time::Instant::now);
            let loaded_results: Vec<LoadedResult> = {
                let mut slots: Vec<Option<LoadedResult>> = std::iter::repeat_with(|| None)
                    .take(frontier_items.len())
                    .collect();
                let mut misses = Vec::new();
                {
                    let cache = self.load_cache.lock().unwrap();
                    for (index, (id, path)) in frontier_items.into_iter().enumerate() {
                        if self.load_cache_enabled
                            && let Some(loaded) = cache.get(&path).cloned()
                        {
                            slots[index] = Some((id, path, Ok(loaded), None, None));
                        } else {
                            let persistent = self.cache.as_ref().and_then(|persistent_cache| {
                                cached_source_type(&path).map(|source_type| {
                                    let cached =
                                        persistent_cache.cached_source(&path, persistent_variant);
                                    (source_type, cached)
                                })
                            });
                            misses.push((index, id, path, persistent));
                        }
                    }
                }

                // 2k 小模块场景中，逐文件提交任务的调度成本会高于读取本身。将 miss 限制为
                // 每 worker 少量批次；批内仍逐项计数，并用原始 index 恢复确定性顺序。
                let max_batches = io_batch_limit(&self.exec);
                let jobs: Vec<_> = into_bounded_batches(misses, max_batches)
                    .into_iter()
                    .map(|batch| {
                        let fs = self.fs.clone();
                        let opts = load_opts.clone();
                        let load_exec_count = self.load_exec_count.clone();
                        move || {
                            batch
                                .into_iter()
                                .map(|(index, id, path, persistent)| {
                                    let stamp = persistent.as_ref().and_then(|_| file_stamp(&path));
                                    let restored = persistent.and_then(|(source_type, cached)| {
                                        cached.filter(|cached| Some(cached.stamp) == stamp).map(
                                            |cached| {
                                                (
                                                    Arc::new(Loaded {
                                                        source: cached.source.as_ref().to_owned(),
                                                        source_type,
                                                        assets: Vec::new(),
                                                        css: None,
                                                    }),
                                                    cached.content_key,
                                                )
                                            },
                                        )
                                    });
                                    let (loaded, cached_content_key) =
                                        if let Some((restored, key)) = restored {
                                            (Ok(restored), Some(key))
                                        } else {
                                            load_exec_count.fetch_add(1, Ordering::Relaxed);
                                            (
                                                load_source(fs.as_ref(), &path, opts.as_ref())
                                                    .map(Arc::new),
                                                None,
                                            )
                                        };
                                    (index, id, path, loaded, stamp, cached_content_key)
                                })
                                .collect::<Vec<_>>()
                        }
                    })
                    .collect();

                // 聚合后一次持锁回填缓存，避免每个小文件各抢一次全局 mutex。
                let batches = self.exec.parallel(jobs);
                let mut cache = self
                    .load_cache_enabled
                    .then(|| self.load_cache.lock().unwrap());
                for batch in batches {
                    for (index, id, path, loaded, stamp, cached_content_key) in batch {
                        if let Some(cache) = cache.as_mut()
                            && let Ok(value) = &loaded
                        {
                            cache.insert(path.clone(), value.clone());
                        }
                        slots[index] = Some((id, path, loaded, stamp, cached_content_key));
                    }
                }
                slots
                    .into_iter()
                    .map(|slot| slot.expect("every load slot is filled"))
                    .collect()
            };
            if let Some(t) = tr {
                read_time += t.elapsed();
            }

            let mut layer: Vec<LayerItem> = Vec::new();
            for (id, path, loaded, stamp, cached_content_key) in loaded_results {
                match loaded {
                    Ok(loaded) => {
                        let src = loaded.source.as_str();
                        let st = loaded.source_type;
                        let module_assets = loaded.assets.clone();
                        let css = loaded.css.clone();
                        // 带外产物：超阈值资源文件（JS import 的资源 + CSS `url()` 引出的
                        // 字体/图片）+ prod 抽取的 CSS 文本（按模块 id 记序）。
                        collected_assets.extend(module_assets);
                        if let Some(text) = css {
                            collected_css.push((id, text));
                        }
                        // content_key 仅缓存启用时需要（缓存主键）；否则跳过 xxh3。
                        let content_key = cached_content_key.unwrap_or_else(|| {
                            if self.cache.is_some() || self.load_cache_enabled {
                                content_key_of(src, st, self.jsx, self.target_fingerprint, &path)
                            } else {
                                0
                            }
                        });
                        if let (Some(stamp), Some(cache)) = (stamp, self.cache.as_mut()) {
                            cache.put_source(&path, stamp, persistent_variant, content_key, src);
                        }

                        let content_vc = self.content_cell(&path, src);
                        let disk_cached = self
                            .cache
                            .as_mut()
                            .and_then(|c| c.summary(content_key))
                            .map(Arc::new);
                        let memory_cached = self.memory_summaries.get(&content_key).cloned();
                        let cached = disk_cached
                            .or_else(|| memory_cached.as_ref().map(|m| m.persisted.clone()));
                        layer.push(LayerItem {
                            id,
                            path,
                            content_vc,
                            source_type: st,
                            content_key,
                            cached,
                            memory_liveness: memory_cached.as_ref().map(|m| m.liveness.clone()),
                            memory_block_info: memory_cached.map(|m| m.block_info),
                            memory_deps: self
                                .memory_summaries
                                .get(&content_key)
                                .map(|m| m.deps.clone()),
                        });
                    }
                    // `Unsupported` 是 loader 对「识别得出、但 wake 有意不支持」的文件类型
                    // （如 `.scss`/`.less`）的信号——与真正的读取失败区分，避免误导为 I/O 问题。
                    Err(e) if e.kind() == std::io::ErrorKind::Unsupported => diagnostics.push(
                        Diagnostic::error(format!("不支持的文件类型 `{}`", path.display()))
                            .with_code("WAKE0302")
                            .with_path(path.to_string_lossy().into_owned())
                            .with_note(e.to_string()),
                    ),
                    Err(e) => diagnostics.push(
                        Diagnostic::error(format!("无法读取模块 `{}`：{e}", path.display()))
                            .with_code("WAKE0300")
                            .with_path(path.to_string_lossy().into_owned()),
                    ),
                }
            }
            if layer.is_empty() {
                break;
            }

            // 2. 并行 parse 缓存未命中的模块。摘要持久化了链接所需的活跃性与 concat 信息，
            // 因此 tree shaking/minify 的跨进程热构建也可跳过 AST 重建。
            let to_parse: Vec<usize> = layer
                .iter()
                .enumerate()
                .filter(|(_, it)| it.cached.is_none())
                .map(|(i, _)| i)
                .collect();
            let analyze_liveness =
                self.cache.is_some() || self.load_cache_enabled || self.tree_shaking;
            let analyze_block_info = self.cache.is_some() || self.load_cache_enabled || self.minify;
            let requests: Vec<_> = to_parse
                .iter()
                .map(|&i| {
                    let cell = layer[i].content_vc;
                    let st = layer[i].source_type;
                    let interner = self.interner.clone();
                    let jsx = self.jsx;
                    let transform_features = self.transform_features;
                    // dev runtime 的 `fileName`：统一正斜杠，避免 Windows 反斜杠进入产物。
                    let file_name = jsx
                        .dev
                        .then(|| Arc::<str>::from(path_to_slash(&layer[i].path)));
                    move || {
                        let (parse_vc, parsed) = parse_request(
                            cell,
                            interner.clone(),
                            st,
                            jsx,
                            transform_features,
                            file_name,
                        );
                        // 三项只读分析共享一次 AST holder 访问，并留在 parse worker 上并行执行。
                        let (uses, liveness, block_info) = parsed.ast.with_ast(|program| {
                            let uses = if need_uses {
                                collect_static_uses(program, interner.as_ref())
                            } else {
                                Vec::new()
                            };
                            let liveness = analyze_liveness.then(|| {
                                Arc::new(collect_module_liveness(program, interner.as_ref()))
                            });
                            let block_info = analyze_block_info.then(|| concat_block_info(program));
                            (uses, liveness, block_info)
                        });
                        ParsedLayerResult {
                            parse_vc,
                            parsed,
                            uses,
                            liveness,
                            block_info,
                        }
                    }
                })
                .collect();
            let engine = Arc::clone(&self.engine);
            let parsed_results = par_request_batched(&engine, &self.exec, requests);
            // layer 索引天然稠密，直接槽位寻址比为每个模块建立哈希表更便宜。
            let mut parsed_by_idx: Vec<Option<ParsedLayerResult>> =
                std::iter::repeat_with(|| None).take(layer.len()).collect();
            for (&i, res) in to_parse.iter().zip(parsed_results) {
                if self.load_cache_enabled {
                    self.memory_parse_vcs
                        .insert(layer[i].content_key, res.parse_vc);
                }
                parsed_by_idx[i] = Some(res);
            }
            // 3. 驱动层：取 deps（parse 或缓存）+ resolve 依赖 + 下一层（BFS 去重处理循环）。
            //
            // 拆三步：Pass 1 串行取预分析结果并做缓存记账（AST 分析已随 parse 并行完成）
            // 并收集扁平 resolve 请求；Pass P 并行 resolve（FS 探测密集）；Pass 2 串行按「模块序×依赖序」
            // 消费结果做 assign_id/建图——**顺序严格复刻原单层循环**，产物逐字节不变。

            // —— Pass 1：串行取预分析结果 + 缓存记账 + 收集 resolve 请求 ——
            let mut pending: Vec<PendingModule> = Vec::with_capacity(layer.len());
            let mut resolve_reqs: Vec<(String, PathBuf)> = Vec::new();
            for (i, it) in layer.into_iter().enumerate() {
                let LayerItem {
                    id,
                    path,
                    content_vc,
                    source_type,
                    content_key,
                    cached,
                    memory_liveness,
                    memory_block_info,
                    memory_deps,
                } = it;
                let (
                    deps,
                    has_tla,
                    parsed_opt,
                    parse_vc_opt,
                    cached_liveness,
                    cached_block_info,
                ): ScanParsed = match cached {
                    Some(sum) => {
                        let parsed = parsed_by_idx[i]
                            .take()
                            .map(|result| (result.parse_vc, result.parsed));
                        let memory_parse_vc =
                            self.memory_parse_vcs.get(&content_key).copied();
                        if let Some((_, parsed)) = &parsed {
                            extend_module_diagnostics(
                                &mut diagnostics,
                                &parsed.diagnostics,
                                &path,
                            );
                        }
                        let liveness = self.tree_shaking.then(|| {
                            memory_liveness.unwrap_or_else(|| {
                                Arc::new(cached_liveness_to_runtime(
                                    &sum.liveness,
                                    &self.interner,
                                ))
                            })
                        });
                        let block_info = self.minify.then_some(
                            memory_block_info.unwrap_or(ConcatBlockInfo {
                                is_esm: sum.concat_is_esm,
                                block_safe: sum.concat_block_safe,
                            }),
                        );
                        (
                            memory_deps.unwrap_or_else(|| {
                                sum.deps.iter().map(cached_dep_to_parsed).collect()
                            }),
                            sum.has_top_level_await,
                            parsed.as_ref().map(|(_, parsed)| Arc::clone(parsed)),
                            parsed
                                .map(|(parse_vc, _)| parse_vc)
                                .or(memory_parse_vc),
                            liveness,
                            block_info,
                        )
                    }
                    None => {
                        let ParsedLayerResult {
                            parse_vc,
                            parsed,
                            uses,
                            liveness,
                            block_info,
                        } = parsed_by_idx[i].take().expect("miss 模块应已 parse");
                        extend_module_diagnostics(&mut diagnostics, &parsed.diagnostics, &path);
                        let deps = parsed.deps.clone();
                        // 存拥有型 scan 摘要——仅无诊断的干净模块（否则会吞掉告警）。
                        if parsed.diagnostics.is_empty()
                            && (self.cache.is_some() || self.load_cache_enabled)
                        {
                            let live = liveness
                                .as_ref()
                                .expect("cache summary always includes liveness");
                            let block =
                                block_info.expect("cache summary always includes concat info");
                            let summary = Arc::new(ModuleSummary {
                                deps: deps.iter().map(parsed_dep_to_cached).collect(),
                                uses: uses.iter().map(import_use_to_cached).collect(),
                                has_top_level_await: parsed.has_top_level_await,
                                liveness: runtime_liveness_to_cached(live, &self.interner),
                                concat_is_esm: block.is_esm,
                                concat_block_safe: block.block_safe,
                            });
                            self.memory_summaries.insert(
                                content_key,
                                MemoryScanSummary {
                                    persisted: summary.clone(),
                                    deps: deps.clone(),
                                    liveness: live.clone(),
                                    block_info: block,
                                },
                            );
                            if let Some(c) = self.cache.as_mut() {
                                c.put_summary(content_key, (*summary).clone());
                            }
                        }
                        let has_tla = parsed.has_top_level_await;
                        (
                            deps,
                            has_tla,
                            Some(parsed),
                            Some(parse_vc),
                            self.tree_shaking.then_some(liveness).flatten(),
                            self.minify.then_some(block_info).flatten(),
                        )
                    }
                };

                let from_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
                // resolve 请求按依赖序压入（Pass 2 按同序消费）。
                for dep in deps.iter() {
                    resolve_reqs.push((dep.specifier.clone(), from_dir.clone()));
                }
                // 绑定级活跃性（仅 tree-shaking 且本模块有新 parse 的 AST 时；缓存摘要命中 → None → 保守全保留）。
                let liveness = cached_liveness.or_else(|| {
                    if self.tree_shaking {
                        parsed_opt.as_ref().map(|p| {
                            Arc::new(p.ast.with_ast(|prog| {
                                collect_module_liveness(prog, self.interner.as_ref())
                            }))
                        })
                    } else {
                        None
                    }
                });
                // 单包 concat 块安全（仅 minify + 有新 parse AST 时；缓存命中 → None → 保守 IIFE）。
                let block_info = cached_block_info.or_else(|| {
                    if self.minify {
                        parsed_opt
                            .as_ref()
                            .map(|p| p.ast.with_ast(concat_block_info))
                    } else {
                        None
                    }
                });
                pending.push(PendingModule {
                    id,
                    path,
                    content_key,
                    source_type,
                    content_vc,
                    deps,
                    liveness,
                    block_info,
                    has_top_level_await: has_tla,
                    parse_vc_opt,
                    parsed_opt,
                });
            }

            // —— Pass P：并行 resolve 全部请求（保序）。FS 探测（is_file/扩展名补全）密集，串行 ~77ms ——
            let resolved_flat: Vec<ResolveResult> = if resolve_reqs.is_empty() {
                Vec::new()
            } else {
                let tr = timing.then(std::time::Instant::now);
                let mut slots: Vec<Option<ResolveResult>> = std::iter::repeat_with(|| None)
                    .take(resolve_reqs.len())
                    .collect();
                let indexed = resolve_reqs.into_iter().enumerate().collect::<Vec<_>>();
                let jobs: Vec<_> = into_bounded_batches(indexed, io_batch_limit(&self.exec))
                    .into_iter()
                    .map(|batch| {
                        let resolver = Arc::clone(&self.resolver);
                        let resolve_exec_count = self.resolve_exec_count.clone();
                        move || {
                            batch
                                .into_iter()
                                .map(|(index, (specifier, from_dir))| {
                                    resolve_exec_count.fetch_add(1, Ordering::Relaxed);
                                    (index, resolver.resolve(&specifier, &from_dir))
                                })
                                .collect::<Vec<_>>()
                        }
                    })
                    .collect();

                for batch in self.exec.parallel(jobs) {
                    for (index, resolved) in batch {
                        slots[index] = Some(resolved);
                    }
                }
                let out = slots
                    .into_iter()
                    .map(|slot| slot.expect("every resolve slot is filled"))
                    .collect();
                if let Some(t) = tr {
                    resolve_time += t.elapsed();
                }
                out
            };
            // —— Pass 2：串行按「模块序×依赖序」消费 resolve 结果，assign_id + 建图 + 建 ModuleRec ——
            let mut next: Vec<(u32, PathBuf)> = Vec::new();
            let mut flat_idx = 0usize;
            for pm in pending {
                let PendingModule {
                    id,
                    path,
                    content_key,
                    source_type,
                    content_vc,
                    deps,
                    liveness,
                    block_info,
                    has_top_level_await,
                    parse_vc_opt,
                    parsed_opt,
                } = pm;
                let mut dep_ids: DepIds = Vec::new();
                for dep in deps.iter() {
                    let resolved = &resolved_flat[flat_idx];
                    flat_idx += 1;
                    match resolved {
                        Ok(resolved) => {
                            let known = path_to_id.contains_key(resolved);
                            let did = assign_id(&mut path_to_id, &mut next_id, resolved.clone());
                            if !known {
                                next.push((did, resolved.clone()));
                            }
                            dep_ids.push((dep.specifier.clone(), did));
                        }
                        Err(_) if is_node_builtin(&dep.specifier) => {
                            // Node 内置模块（fs/stream/util/crypto/...）外部化：不加入模块图，
                            // codegen 的 require_expr 走 external 回退保留 `require("...")`，
                            // 在 node 运行时由宿主提供（等价 esbuild --platform=node）。
                            // 浏览器目标若拉入 Node 内置则天然无法运行，需改用浏览器版依赖。
                        }
                        Err(_) => diagnostics.push(
                            Diagnostic::error(format!(
                                "无法从 `{}` 解析依赖 `{}`",
                                path.display(),
                                dep.specifier
                            ))
                            .with_code("WAKE0301")
                            .with_path(path.to_string_lossy().into_owned())
                            .with_primary(dep.span, "此依赖"),
                        ),
                    }
                }
                modules.insert(
                    id,
                    ModuleRec {
                        path,
                        content_key,
                        source_type,
                        content_vc,
                        deps,
                        dep_ids,
                        liveness,
                        block_info,
                        has_top_level_await,
                        parse_vc: parse_vc_opt,
                        parsed: parsed_opt,
                    },
                );
            }
            frontier = next;
        }

        let t_scan = t0.elapsed();
        let t_link_start = timing.then(std::time::Instant::now);

        // —— Link 阶段：Tree Shaking——算每个模块的「保留导出名」（DESIGN §5.3 / PLAN §6.6）——
        let keep = self.compute_keep_exports(&modules, entry_id, next_id);

        // —— Link 阶段：代码分割——算 chunk 图（DESIGN §6.3 / PLAN §6.5）。None = 单包路径 ——
        let chunk_graph = if self.code_splitting {
            let edges = build_module_edges(&modules);
            compute_chunk_graph(&edges, entry_id, self.share_threshold)
        } else {
            None
        };

        // —— Link 阶段：顶层 await——算 async 子图（含 TLA 的模块 + 静态导入它们的模块）——
        let async_ids = async_module_ids(&modules);
        let link_time = t_link_start.map_or(std::time::Duration::ZERO, |t| t.elapsed());
        let t_codegen_start = timing.then(std::time::Instant::now);

        // —— codegen 阶段：设 linker cell（驱动）+ 查产物缓存 + 并行 codegen 未命中者 ——
        let ordered: Vec<u32> = (0..next_id).filter(|id| modules.contains_key(id)).collect();
        self.last_module_count = ordered.len();

        // CSS-in-JS 是否真的用得上：全项目无人 import `@linaria/core` 时整体跳过——
        // 既省掉静态导出求值，也让产物磁盘缓存照常命中（见下方缓存守卫），
        // 从而使本功能对非 Linaria 项目零开销、可安全默认开启。
        let cij_active = self.css_in_js
            && modules.values().any(|r| {
                r.deps
                    .iter()
                    .any(|d| wake_css_in_js::is_css_in_js_source(&d.specifier))
            });

        // 3a. 为每个模块设 linker cell、算 body_key、查产物缓存。
        let mut plans: Vec<CgPlan> = Vec::with_capacity(ordered.len());
        for &id in &ordered {
            let (path, dep_ids, dyn_chunks, content_key) = {
                let rec = &modules[&id];
                let dyn_chunks = dyn_chunks_of(rec, chunk_graph.as_ref());
                (
                    rec.path.clone(),
                    rec.dep_ids.clone(),
                    dyn_chunks,
                    rec.content_key,
                )
            };
            // 本模块依赖中落在 async 子图内的（升序去重）——codegen 据此给静态导入点加 `await`。
            let mut async_deps: Vec<u32> = dep_ids
                .iter()
                .map(|(_, tid)| *tid)
                .filter(|tid| async_ids.contains(tid))
                .collect();
            async_deps.sort_unstable();
            async_deps.dedup();
            let data = LinkerData {
                deps: dep_ids,
                keep_exports: keep.get(&id).cloned().flatten(),
                dyn_chunks,
                async_deps,
            };
            // body_key 与查缓存仅在启用缓存时做——`hash_linker`（SipHash 全依赖）无缓存时纯浪费。
            // define + minify 指纹混入低 64 位：dev↔prod / minify 开关变化 → 缓存精确失效。
            let (body_key, cached_body) = if self.cache.is_some() {
                let minify_salt = if self.minify { MINIFY_SALT } else { 0 };
                let mangle_salt = if self.mangle { MANGLE_SALT } else { 0 };
                let low = hash_linker(&data) ^ self.define_hash ^ minify_salt ^ mangle_salt;
                let bk = ((content_key as u128) << 64) | (low as u128);
                // 产物磁盘缓存只存模块体（schema v1），不存映射与 CSS-in-JS 抽取的样式。
                // 启用其一时视为未命中并重算，否则缓存命中的模块会丢失 map / 丢失 CSS
                // （后者更危险：产物 JS 正确但样式凭空消失）。
                let cb = if self.sourcemap || cij_active {
                    None
                } else {
                    self.cache.as_mut().and_then(|c| c.body(bk))
                };
                (bk, cb)
            } else {
                (0u128, None)
            };
            let linker_vc = self.linker_cell(&path, data);
            plans.push(CgPlan {
                id,
                path,
                body_key,
                linker_vc,
                cached_body,
            });
        }

        // 3b. 未命中产物缓存的模块里，摘要曾命中（parse 被延迟）的现在补 parse。
        let need_parse: Vec<usize> = plans
            .iter()
            .enumerate()
            .filter(|(_, p)| p.cached_body.is_none() && modules[&p.id].parse_vc.is_none())
            .map(|(i, _)| i)
            .collect();
        if !need_parse.is_empty() {
            let reqs: Vec<_> = need_parse
                .iter()
                .map(|&i| {
                    let rec = &modules[&plans[i].id];
                    let cell = rec.content_vc;
                    let st = rec.source_type;
                    let interner = self.interner.clone();
                    let jsx = self.jsx;
                    let transform_features = self.transform_features;
                    // dev runtime 的 `fileName`：统一正斜杠，避免 Windows 反斜杠进入产物。
                    let file_name = jsx.dev.then(|| Arc::<str>::from(path_to_slash(&rec.path)));
                    move || parse_request(cell, interner, st, jsx, transform_features, file_name)
                })
                .collect();
            let engine = Arc::clone(&self.engine);
            let results = par_request_batched(&engine, &self.exec, reqs);
            for (&i, (pvc, parsed)) in need_parse.iter().zip(results) {
                let rec = modules.get_mut(&plans[i].id).unwrap();
                extend_module_diagnostics(&mut diagnostics, &parsed.diagnostics, &rec.path);
                rec.parse_vc = Some(pvc);
                rec.parsed = Some(parsed);
            }
        }

        // 3b′. CSS-in-JS：先算各模块的「静态导出常量」，再为每个模块把它的 import 绑定
        // 解析成可求值的静态值（`token` → 那个模块 default 导出的对象）。
        // 只做**一层**：被引用模块自身的求值仅用其模块内信息（design token 文件正是此形态）。
        let cij_scopes: FxHashMap<u32, Arc<wake_css_in_js::value::Scope>> = if cij_active {
            self.resolve_css_in_js_scopes(&modules, &ordered)
        } else {
            FxHashMap::default()
        };

        // 3c. 并行 codegen 所有未命中产物缓存的模块。
        let miss: Vec<usize> = plans
            .iter()
            .enumerate()
            .filter(|(_, p)| p.cached_body.is_none())
            .map(|(i, _)| i)
            .collect();
        let requests: Vec<_> = miss
            .iter()
            .map(|&i| {
                let parse_vc = modules[&plans[i].id]
                    .parse_vc
                    .expect("未命中模块此时必已 parse");
                let linker_vc = plans[i].linker_vc;
                let interner = self.interner.clone();
                let define = self.define.clone();
                let minify = self.minify;
                let mangle = self.mangle;
                let no_esmodule = self.minify;
                let minify_names = self.minify;
                let drop_console = self.drop_console;
                let drop_debugger = self.drop_debugger;
                let want_map = self.sourcemap;
                let id = plans[i].id;
                let codegen_exec_counts = self.codegen_exec_counts.clone();
                let codegen_counter_shard = id as usize & (CODEGEN_COUNTER_SHARDS - 1);
                let cij = cij_scopes.get(&id).cloned();
                let cij_seed = cij
                    .as_ref()
                    .map(|_| Arc::<str>::from(path_to_slash(&modules[&id].path)));
                // dev（未开抽取）时把 CSS 以 `<style>` 注入模块体；prod 带出聚合。
                let inject_style = !self.extract_css;
                move || {
                    codegen_request(
                        parse_vc,
                        linker_vc,
                        interner,
                        define,
                        minify,
                        mangle,
                        no_esmodule,
                        minify_names,
                        drop_console,
                        drop_debugger,
                        want_map,
                        codegen_exec_counts,
                        codegen_counter_shard,
                        cij,
                        cij_seed,
                        inject_style,
                    )
                }
            })
            .collect();
        let engine = Arc::clone(&self.engine);
        let miss_bodies = par_request_batched(&engine, &self.exec, requests);

        // 3d. 新算的 body 写回缓存；汇总所有 body（命中 + 新算），按 `ordered` 出序。
        let mut body_of: FxHashMap<u32, Arc<String>> = FxHashMap::default();
        // 模块局部映射（仅 sourcemap 启用时非空）——emit 拼接时按行偏移平移合并。
        let mut map_of: FxHashMap<u32, Arc<ModuleMappings>> = FxHashMap::default();
        for (&i, out) in miss.iter().zip(miss_bodies) {
            if let Some(c) = self.cache.as_mut() {
                // Arc clone：写回缓存不再深拷贝整段 body。
                c.put_body(plans[i].body_key, out.code.clone());
            }
            if let Some(m) = &out.map {
                map_of.insert(plans[i].id, m.clone());
            }
            // CSS-in-JS 抽取的样式汇入与 `.css` 模块同一条聚合通道（prod）。
            if let Some(c) = &out.css {
                collected_css.push((plans[i].id, (**c).clone()));
            }
            extend_module_diagnostics(&mut diagnostics, &out.diagnostics, &plans[i].path);
            body_of.insert(plans[i].id, out.code.clone());
        }
        for p in &plans {
            if let Some(cb) = &p.cached_body {
                // 命中缓存：Arc clone（引用计数自增），消除整段 body 的第二次 memcpy。
                body_of.insert(p.id, cb.clone());
            }
        }
        let bodies: Vec<(u32, Arc<String>)> = ordered
            .iter()
            .map(|&id| (id, body_of[&id].clone()))
            .collect();

        // —— 死模块消除：从 entry 按存活 `require` 边（codegen DCE 后）重算可达，丢弃不可达模块 ——
        let bodies = if self.dead_module_elimination {
            let live = live_modules(&bodies, entry_id);
            bodies
                .into_iter()
                .filter(|(id, _)| live.contains(id))
                .collect::<Vec<_>>()
        } else {
            bodies
        };
        // 存活模块 id（升序，= 过滤后的 `ordered`）——供 module_count 与单包 module_ids。
        let live_ids: Vec<u32> = bodies.iter().map(|(id, _)| *id).collect();
        let codegen_time = t_codegen_start.map_or(std::time::Duration::ZERO, |t| t.elapsed());
        let t_emit_start = timing.then(std::time::Instant::now);

        // —— Emit：双路（无 async chunk → 旧单包，逐字节不变；有 → 多 chunk 全局 registry）——
        // 模块 id / 数量用 `live_ids`（DME 后；未启用 DME 时 = 全量 `ordered`）。
        // concat 块安全信息（单包路径用）：id → ConcatBlockInfo（缺分析 → 保守缺省）。
        let block_infos: FxHashMap<u32, ConcatBlockInfo> = modules
            .iter()
            .filter_map(|(&id, rec)| rec.block_info.map(|bi| (id, bi)))
            .collect();

        // SourceMap 仅在「启用 + 非 minify」时产出：minify 分支会 scope-hoist 并改写模块体文本，
        // 行偏移法失效（错位的 map 比没有更误导），故此时不采集。
        let want_map = self.sourcemap && !self.minify;
        let mut body_starts: Vec<(u32, u32)> = Vec::new();

        let mut output = match &chunk_graph {
            None => {
                let bundle = emit(
                    &bodies,
                    entry_id,
                    self.minify,
                    self.minify,
                    &block_infos,
                    &async_ids,
                    &self.exec,
                    want_map.then_some(&mut body_starts),
                );
                let mut o =
                    crate::single_chunk(bundle, live_ids.len(), diagnostics, live_ids.clone());
                if want_map {
                    // 源文件名 + 源文本：文本取自 parse 结果；缓存命中未 parse 的模块只带路径
                    // （sourcesContent 为 null，浏览器按路径自取）。
                    let mut sources: FxHashMap<u32, (String, Option<String>)> =
                        FxHashMap::default();
                    let cwd = std::env::current_dir().ok();
                    for (id, _) in &body_starts {
                        if let Some(rec) = modules.get(id) {
                            let name = map_source_name(&rec.path, cwd.as_deref());
                            let content = rec.parsed.as_ref().map(|p| p.source.to_string());
                            sources.insert(*id, (name, content));
                        }
                    }
                    let file = o.chunks[o.entry_chunk].file_name.clone();
                    let sm = merge_bundle_map(&body_starts, &map_of, &sources, Some(file));
                    let json = serialize_map(&sm, &sources);
                    o.chunks[o.entry_chunk].source_map = Some(json);
                }
                o
            }
            Some(g) => {
                let token = build_token(&normalize(entry), live_ids.len());
                let (chunks, entry_chunk) = emit_chunks(
                    &bodies,
                    g,
                    entry_id,
                    &token,
                    &self.public_path,
                    self.content_hash,
                    &async_ids,
                );
                let bundle = chunks[entry_chunk].code.clone();
                BuildOutput {
                    bundle,
                    module_count: live_ids.len(),
                    updated_module_count: 0,
                    cached_module_count: 0,
                    diagnostics,
                    chunks,
                    entry_chunk,
                    assets: Vec::new(),
                }
            }
        };

        output.updated_module_count = codegen_exec_count(&self.codegen_exec_counts)
            .saturating_sub(codegen_exec_before) as usize;
        output.cached_module_count = plans.len().saturating_sub(output.updated_module_count);

        // —— 带外产物：超阈值资源（按文件名去重）+ prod 聚合 CSS（模块 id 升序 = BFS 发现序）——
        let mut assets: Vec<OutputAsset> = Vec::new();
        let mut seen: FxHashSet<String> = FxHashSet::default();
        for (name, bytes) in collected_assets {
            if seen.insert(name.clone()) {
                assets.push(OutputAsset {
                    file_name: name,
                    bytes,
                    is_css: false,
                });
            }
        }
        if self.extract_css && !collected_css.is_empty() {
            // **按依赖后序聚合**，不是按模块 id。CSS 的层叠靠顺序定胜负，而模块 id 是 BFS
            // 发现序：`index → styles.css --@import--> base.css` 会得到 id 0/1/2，于是
            // `base.css` 被排在 `styles.css` **之后**，覆盖关系整个反过来——与 dev 下
            // `<style>` 注入（= 模块求值序，依赖先行）恰好相反。后序遍历即 ESM 求值序，
            // 使 prod 与 dev 的层叠结果一致。
            let order = css_emission_order(&modules, entry_id);
            let fallback = u32::MAX;
            collected_css.sort_by_key(|(id, _)| (*order.get(id).unwrap_or(&fallback), *id));
            let mut css = String::new();
            for (_, text) in &collected_css {
                css.push_str(text);
                if !text.ends_with('\n') {
                    css.push('\n');
                }
            }
            // prod 压缩 CSS（安全子集：空白折叠 / 去注释 / 删 `}` 前 `;`）。§M4c
            let css = if self.minify {
                wake_css::minify(&css)
            } else {
                css
            };
            let name = format!("styles.{}.css", hash8(&css));
            assets.push(OutputAsset {
                file_name: name,
                bytes: css.into_bytes(),
                is_css: true,
            });
        }
        output.assets = assets;

        // 持久化缓存落盘（opt-in）：仅在无错误 **且本次新增过条目**（dirty）时写。
        // 全命中（未变）时缓存内容没变，跳过落盘——缓存文件常和 bundle 一样大，
        // 每次白写会让 `--cache` 的 I/O 反超它省下的 parse（实测小项目会更慢）。
        if !output.has_errors()
            && let (Some(cache), Some(path)) = (&self.cache, &self.cache_path)
            && cache.is_dirty()
        {
            let _ = cache.store(path);
        }

        if timing {
            let total = t0.elapsed();
            let emit_time = t_emit_start.map_or(std::time::Duration::ZERO, |t| t.elapsed());
            eprintln!(
                "[wake-timing] 模块={} | scan={:.1?} (read={:.1?} resolve={:.1?}) | link={:.1?} | codegen={:.1?} | emit={:.1?} | 总={:.1?}",
                ordered.len(),
                t_scan,
                read_time,
                resolve_time,
                link_time,
                codegen_time,
                emit_time,
                total,
            );
        }

        if output.has_errors() || !self.load_cache_enabled {
            self.stable_graph = None;
            self.topology_invalidated.store(true, Ordering::Release);
        } else {
            self.stable_graph = Some(StableModuleGraph {
                entry: entry_norm,
                next_id,
                modules,
            });
            self.topology_invalidated.store(false, Ordering::Release);
        }

        output
    }

    /// 取/建某路径的内容 cell 并写入最新文本（未变则 `set_input` 不推进 revision）。
    fn content_cell(&mut self, path: &Path, text: &str) -> Vc<Content> {
        let content: Content = Arc::from(text);
        if let Some(&cell) = self.content_cells.get(path) {
            self.engine.set_input(cell, content);
            cell
        } else {
            let cell = self.engine.new_input(content);
            self.content_cells.insert(path.to_path_buf(), cell);
            cell
        }
    }

    /// 取/建某路径的 linker cell 并写入最新数据（deps + keep 未变则 no-op）。
    fn linker_cell(&mut self, path: &Path, data: LinkerData) -> Vc<LinkerData> {
        if let Some(&cell) = self.linker_cells.get(path) {
            self.engine.set_input(cell, data);
            cell
        } else {
            let cell = self.engine.new_input(data);
            self.linker_cells.insert(path.to_path_buf(), cell);
            cell
        }
    }

    /// Link 阶段：算每个模块的「保留导出名」（`None` = 不 shake，全保留）。
    ///
    /// 从入口出发累计跨模块导出使用（DESIGN §5.3 / PLAN §6.6）：入口全保留；`import *` /
    /// 动态 `import()` / `require()` 目标全保留；具名 import 累加具体名。
    /// `export *` (ReexportAll) 仅在下游消费本模块导出时才传播至目标——避免 barrel 文件
    /// 无条件把整棵 re-export 子树全部标记为 Used::All。
    /// 保守（宁多保留），安全性由 codegen 侧的「纯 + 内部未引用」二次判定兜底。
    fn compute_keep_exports(
        &self,
        modules: &FxHashMap<u32, ModuleRec>,
        entry_id: u32,
        next_id: u32,
    ) -> FxHashMap<u32, Option<Vec<String>>> {
        let mut keep: FxHashMap<u32, Option<Vec<String>>> = FxHashMap::default();
        if !self.tree_shaking {
            for &id in modules.keys() {
                keep.insert(id, None);
            }
            return keep;
        }

        let _ = next_id;
        // —— 绑定级全程序活跃性（PLAN §6.6 增量）：把 shake 从「模块 + import 名」细化到「绑定」，
        //    识别「被 import 但引用它的代码本身是死代码」的传递性死亡导出（对齐 webpack）——

        // 每模块的 spec→id 解析表（借 dep_ids）。
        let spec_to_id: FxHashMap<u32, FxHashMap<String, u32>> = modules
            .iter()
            .map(|(&id, rec)| {
                (
                    id,
                    rec.dep_ids
                        .iter()
                        .map(|(s, t)| (s.clone(), *t))
                        .collect::<FxHashMap<String, u32>>(),
                )
            })
            .collect();
        let resolve = |m: u32, spec: &str| -> Option<u32> {
            spec_to_id.get(&m).and_then(|mm| mm.get(spec)).copied()
        };

        // 有绑定分析的模块（prod 冷构建通常全覆盖）。
        let live_mods: FxHashMap<u32, &ModuleLiveness> = modules
            .iter()
            .filter_map(|(&id, rec)| rec.liveness.as_ref().map(|l| (id, l.as_ref())))
            .collect();

        // force_all：① 动态 import / require 目标（不可 shake）；② **缺绑定分析**的模块（缓存摘要命中）
        //   ——其 import 关系未知，须把它自身与全部依赖目标保守全保留，绝不误删它可能用到的导出。
        let mut force_all: FxHashSet<u32> = FxHashSet::default();
        for (&id, rec) in modules.iter() {
            let has_live = rec.liveness.is_some();
            if !has_live {
                force_all.insert(id);
            }
            for dep in rec.deps.iter() {
                let dyn_or_req = matches!(
                    dep.kind,
                    DependencyKind::DynamicImport | DependencyKind::Require
                );
                if (dyn_or_req || !has_live)
                    && let Some(tid) = resolve(id, &dep.specifier)
                {
                    force_all.insert(tid);
                }
            }
        }

        let live_keep = compute_live_keep(&live_mods, &resolve, entry_id, &force_all);

        for &id in modules.keys() {
            let k = match live_keep.get(&id) {
                Some(LiveResult::Names(atoms)) => {
                    let mut v: Vec<String> =
                        atoms.iter().map(|a| self.interner.resolve(*a)).collect();
                    v.sort_unstable();
                    Some(v)
                }
                // `All` 或不在结果里（缺绑定分析）→ 保守全保留。
                _ => None,
            };
            keep.insert(id, k);
        }
        keep
    }
}

/// 是否为 Node.js 内置模块（含 `node:` 前缀与 `fs/promises`、`stream/web` 等子路径）。
/// 这些模块不打进 bundle，保留为运行时 `require(...)`（node 目标）。
fn is_node_builtin(spec: &str) -> bool {
    if spec.starts_with("node:") {
        return true;
    }
    // 取第一段（`fs/promises` → `fs`）。
    let head = spec.split('/').next().unwrap_or(spec);
    matches!(
        head,
        "assert"
            | "async_hooks"
            | "buffer"
            | "child_process"
            | "cluster"
            | "console"
            | "constants"
            | "crypto"
            | "dgram"
            | "diagnostics_channel"
            | "dns"
            | "domain"
            | "events"
            | "fs"
            | "http"
            | "http2"
            | "https"
            | "inspector"
            | "module"
            | "net"
            | "os"
            | "path"
            | "perf_hooks"
            | "process"
            | "punycode"
            | "querystring"
            | "readline"
            | "repl"
            | "stream"
            | "string_decoder"
            | "sys"
            | "timers"
            | "tls"
            | "trace_events"
            | "tty"
            | "url"
            | "util"
            | "v8"
            | "vm"
            | "wasi"
            | "worker_threads"
            | "zlib"
    )
}

/// codegen 阶段一个模块的计划：产物键 + linker 句柄 + 命中的缓存体（`None` = 需 codegen）。
struct CgPlan {
    id: u32,
    path: PathBuf,
    body_key: u128,
    linker_vc: Vc<LinkerData>,
    cached_body: Option<Arc<String>>,
}

fn extend_module_diagnostics(
    target: &mut Vec<Diagnostic>,
    diagnostics: &[Diagnostic],
    path: &Path,
) {
    let path = path.to_string_lossy().into_owned();
    target.extend(
        diagnostics
            .iter()
            .cloned()
            .map(|diagnostic| diagnostic.with_path(path.clone())),
    );
}

/// 内容键：源码、源类型及所有影响 parse/codegen 的配置。JSX dev 还必须隔离文件路径，
/// 否则两个内容相同的组件会错误复用带有另一方 `fileName` 的 codegen 产物。
fn content_key_of(
    src: &str,
    st: SourceType,
    jsx: JsxRuntimeOptions,
    target_fingerprint: u64,
    path: &Path,
) -> u64 {
    let mut seed = match st {
        SourceType::Module => 1,
        SourceType::Script => 2,
        SourceType::TypeScript => 3,
        SourceType::Tsx => 4,
        SourceType::Jsx => 5,
    };
    // JSX 口径改变解析产出的依赖（`react/jsx-runtime` ↔ `react/jsx-dev-runtime`），
    // 必须参与主键，否则跨 dev/prod 复用摘要会带错依赖。
    seed ^= jsx.salt() ^ target_fingerprint;
    if jsx.dev {
        seed ^= xxh3_64_with_seed(path_to_slash(path).as_bytes(), 0x6a73_782d_6669_6c65);
    }
    xxh3_64_with_seed(src.as_bytes(), seed)
}

/// JSX 运行时口径（随 bundler 恒定，传入 parse 任务）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JsxRuntimeOptions {
    pub(crate) dev: bool,
    /// `jsxImportSource`；`'static` 是因为其取值来自配置且在构建期恒定（`Box::leak` 或字面量）。
    pub(crate) import_source: &'static str,
}

impl Default for JsxRuntimeOptions {
    fn default() -> Self {
        JsxRuntimeOptions {
            dev: false,
            import_source: "react",
        }
    }
}

impl JsxRuntimeOptions {
    /// 参与 `content_key` 的盐。
    fn salt(&self) -> u64 {
        let mut h = if self.dev { 0x9E37_79B9_7F4A_7C15 } else { 0 };
        h ^= xxh3_64_with_seed(self.import_source.as_bytes(), 0x4A53_5800_0000_0001);
        h
    }
}

/// linker 键：对 `LinkerData`（依赖 id 映射 + keep + dyn_chunks）取稳定 hash（固定种子 SipHash，跨进程稳定）。
fn hash_linker(data: &LinkerData) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    data.hash(&mut h);
    h.finish()
}

/// minify 开关的缓存键盐（黄金比例常量）——minify 变化时与 define 指纹一并改变产物键。
const MINIFY_SALT: u64 = 0x9E37_79B9_7F4A_7C15;

/// mangle 开关的缓存键盐——mangle 变化（开/关）时改变产物键，避免命中另一口径的旧产物。
/// Covers both identifier mangling and property mangling.
const MANGLE_SALT: u64 = 0xC2B2_AE3D_27D4_EB4F;

/// define 表指纹（固定种子 SipHash，跨进程稳定）——混入产物缓存键，使 define 变化精确失效缓存。
fn hash_define(define: &[(String, String)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    define.hash(&mut h);
    h.finish()
}

fn dep_kind_to_u8(k: DependencyKind) -> u8 {
    match k {
        DependencyKind::Import => 0,
        DependencyKind::ExportFrom => 1,
        DependencyKind::DynamicImport => 2,
        DependencyKind::Require => 3,
    }
}
fn u8_to_dep_kind(v: u8) -> DependencyKind {
    match v {
        0 => DependencyKind::Import,
        1 => DependencyKind::ExportFrom,
        2 => DependencyKind::DynamicImport,
        _ => DependencyKind::Require,
    }
}

fn parsed_dep_to_cached(d: &ParsedDep) -> CachedDep {
    CachedDep {
        specifier: d.specifier.clone(),
        kind: dep_kind_to_u8(d.kind),
        lo: d.span.lo,
        hi: d.span.hi,
    }
}
fn cached_dep_to_parsed(d: &CachedDep) -> ParsedDep {
    ParsedDep {
        specifier: d.specifier.clone(),
        kind: u8_to_dep_kind(d.kind),
        span: Span::new(d.lo, d.hi),
    }
}

fn import_use_to_cached(u: &(String, ImportUse)) -> CachedUse {
    match &u.1 {
        ImportUse::All => CachedUse {
            specifier: u.0.clone(),
            all: true,
            reexport: false,
            names: Vec::new(),
        },
        ImportUse::ReexportAll => CachedUse {
            specifier: u.0.clone(),
            all: true,
            reexport: true,
            names: Vec::new(),
        },
        ImportUse::Names(ns) => CachedUse {
            specifier: u.0.clone(),
            all: false,
            reexport: false,
            names: ns.clone(),
        },
    }
}

fn runtime_liveness_to_cached(l: &ModuleLiveness, interner: &Interner) -> CachedLiveness {
    let resolve = |atom| interner.resolve(atom);
    let sorted_refs = |refs: &FxHashSet<_>| {
        let mut values: Vec<String> = refs.iter().map(|&atom| resolve(atom)).collect();
        values.sort();
        values
    };
    CachedLiveness {
        decls: l
            .decls
            .iter()
            .map(|(name, refs)| (resolve(*name), sorted_refs(refs)))
            .collect(),
        root_refs: sorted_refs(&l.root_refs),
        named_imports: l
            .named_imports
            .iter()
            .map(|import| CachedNamedImport {
                local: resolve(import.local),
                spec: import.spec.clone(),
                imported: resolve(import.imported),
            })
            .collect(),
        namespace_imports: l
            .namespace_imports
            .iter()
            .map(|(local, spec)| (resolve(*local), spec.clone()))
            .collect(),
        reexport_star: l.reexport_star.clone(),
        ns_reexports: l
            .ns_reexports
            .iter()
            .map(|(name, spec)| (resolve(*name), spec.clone()))
            .collect(),
        reexport_named: l
            .reexport_named
            .iter()
            .map(|(name, spec, imported)| (resolve(*name), spec.clone(), resolve(*imported)))
            .collect(),
        exports: l
            .exports
            .iter()
            .map(|(name, local)| (resolve(*name), local.map(resolve)))
            .collect(),
    }
}

fn cached_liveness_to_runtime(l: &CachedLiveness, interner: &Interner) -> ModuleLiveness {
    let intern = |name: &str| interner.intern(name);
    ModuleLiveness {
        decls: l
            .decls
            .iter()
            .map(|(name, refs)| (intern(name), refs.iter().map(|name| intern(name)).collect()))
            .collect(),
        root_refs: l.root_refs.iter().map(|name| intern(name)).collect(),
        named_imports: l
            .named_imports
            .iter()
            .map(|import| NamedImport {
                local: intern(&import.local),
                spec: import.spec.clone(),
                imported: intern(&import.imported),
            })
            .collect(),
        namespace_imports: l
            .namespace_imports
            .iter()
            .map(|(local, spec)| (intern(local), spec.clone()))
            .collect(),
        reexport_star: l.reexport_star.clone(),
        ns_reexports: l
            .ns_reexports
            .iter()
            .map(|(name, spec)| (intern(name), spec.clone()))
            .collect(),
        reexport_named: l
            .reexport_named
            .iter()
            .map(|(name, spec, imported)| (intern(name), spec.clone(), intern(imported)))
            .collect(),
        exports: l
            .exports
            .iter()
            .map(|(name, local)| (intern(name), local.as_deref().map(intern)))
            .collect(),
    }
}

/// parse 请求（在 worker 线程的 `enter` 上下文内执行）：登记 parse 任务、返回句柄 + 结果。
fn parse_request(
    cell: Vc<Content>,
    interner: Arc<Interner>,
    source_type: SourceType,
    jsx: JsxRuntimeOptions,
    transform_features: FeatureSet,
    file_name: Option<Arc<str>>,
) -> (Vc<ParsedModule>, Arc<ParsedModule>) {
    // Target/JSX changes rebuild the in-memory parse graph in their setters. JSX and target
    // fingerprints also enter `content_key`, preventing cross-configuration disk-cache reuse.
    let id = TaskId::of("wake_bundler", "parse", &[cell.arg_ref()]);
    let vc = query(id, move || {
        parse_module(
            cell,
            &interner,
            source_type,
            jsx,
            transform_features,
            file_name.clone(),
        )
    });
    let arc = vc.read();
    (vc, arc)
}

/// 收集模块中所有 (export_name, variable_name, declarator_span) 三元组。
/// declarator_span 用于 span 级别精确匹配，避免同名字段在不同作用域被误判。
fn collect_export_var_pairs(program: &Program, interner: &Interner) -> Vec<(String, String, Span)> {
    let mut pairs = Vec::new();
    for stmt in program.body.iter() {
        match stmt {
            Statement::ExportNamed(s) => {
                if let Some(decl) = &s.declaration {
                    match decl {
                        Statement::VariableDeclaration(d) => {
                            for decl in d.declarations.iter() {
                                if let Pattern::Ident(id) = &decl.id {
                                    let name = interner.resolve(id.name);
                                    pairs.push((name.clone(), name, decl.span));
                                }
                            }
                        }
                        Statement::FunctionDeclaration(f) => {
                            if let Some(id) = f.id {
                                let name = interner.resolve(id.name);
                                pairs.push((name.clone(), name, f.span));
                            }
                        }
                        Statement::ClassDeclaration(c) => {
                            if let Some(id) = c.id {
                                let name = interner.resolve(id.name);
                                pairs.push((name.clone(), name, c.span));
                            }
                        }
                        _ => {}
                    }
                }
                for spec in s.specifiers.iter() {
                    let exported = match &spec.exported {
                        ModuleExportName::Ident(id) => interner.resolve(id.name),
                        ModuleExportName::String(a) => interner.resolve(*a),
                    };
                    let local = match &spec.local {
                        ModuleExportName::Ident(id) => interner.resolve(id.name),
                        ModuleExportName::String(a) => interner.resolve(*a),
                    };
                    pairs.push((exported, local, spec.span));
                }
            }
            Statement::ExportDefault(s) => {
                let (var_name, span) = match &s.declaration {
                    ExportDefaultKind::Function(f) => {
                        (f.id.map(|id| interner.resolve(id.name)), f.span)
                    }
                    ExportDefaultKind::Class(c) => {
                        (c.id.map(|id| interner.resolve(id.name)), c.span)
                    }
                    _ => (None, s.span),
                };
                if let Some(name) = var_name {
                    pairs.push(("default".to_string(), name, span));
                }
            }
            _ => {}
        }
    }
    pairs
}

/// codegen 任务产物：模块体 + 可选的 SourceMap 映射（WAKE-COMPATIBILITY §M4d）。
///
/// `code` 用 `Arc<String>` 内嵌（而非裸 `String`），使下游 `bodies` 与产物磁盘缓存得以
/// 沿用 `Arc<String>` 类型、取出时只做引用计数自增，接入 sourcemap 无需改动既有拼接与缓存路径。
pub(crate) struct CodegenBody {
    pub(crate) code: Arc<String>,
    /// 模块内局部坐标的映射（`None` = 未请求 sourcemap）。
    pub(crate) map: Option<Arc<ModuleMappings>>,
    /// CSS-in-JS 抽取出的样式（`None` = 未启用 / 无样式 / 已内联为 `<style>` 注入）。
    pub(crate) css: Option<Arc<String>>,
    /// CSS-in-JS 求值诊断（警告级）。
    pub(crate) diagnostics: Vec<Diagnostic>,
}

// 指纹只取产物代码：map / css 都是同一次 codegen 对同一 AST 的纯函数产物，代码相同则二者
// 必然相同（同 `ParsedModule` 只取 AST+deps 的做法）。
impl std::hash::Hash for CodegenBody {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.code.hash(state);
    }
}

#[allow(clippy::too_many_arguments)]
fn codegen_request(
    parse_vc: Vc<ParsedModule>,
    linker_vc: Vc<LinkerData>,
    interner: Arc<Interner>,
    define: Arc<[(String, String)]>,
    minify: bool,
    mangle: bool,
    no_esmodule: bool,
    minify_names: bool,
    drop_console: bool,
    drop_debugger: bool,
    want_map: bool,
    codegen_exec_counts: CodegenExecCounts,
    codegen_counter_shard: usize,
    // CSS-in-JS：`css_in_js` = 本模块 import 绑定已解析出的静态值（None = 未启用）；
    // `cij_seed` = 类名 hash 种子（模块路径）；`inject_style` = dev 路径改为 `<style>` 注入。
    css_in_js: Option<Arc<wake_css_in_js::value::Scope>>,
    cij_seed: Option<Arc<str>>,
    inject_style: bool,
) -> Arc<CodegenBody> {
    let id = TaskId::of(
        "wake_bundler",
        "codegen",
        &[parse_vc.arg_ref(), linker_vc.arg_ref()],
    );
    let vc = query(id, move || {
        codegen_exec_counts[codegen_counter_shard].fetch_add(1, Ordering::Relaxed);
        let parsed = parse_vc.read();
        // Large generated modules expose span-keyed rename/liveness divergence between source
        // declarations and linker-generated reads. Keep their names/exports stable until the
        // optimizer is keyed end-to-end by SymbolId; small modules retain the normal fast path.
        let compatibility_mode = mangle && parsed.source.len() >= 4096;
        let mangle = mangle && !compatibility_mode;
        let minify_names = minify_names && !compatibility_mode;
        let data = linker_vc.read();
        let linker = Linker {
            map: SpecifierLookup::new(&data.deps),
            dyn_chunk: SpecifierLookup::new(&data.dyn_chunks),
            async_ids: &data.async_deps,
        };
        // Cross-module export shaking is disabled until declaration removal and synthesized
        // export writes share one SymbolId-based liveness model. Unreachable modules are still
        // removed by the graph phase.
        let keep: Option<&[String]> = if compatibility_mode {
            None
        } else {
            data.keep_exports.as_deref()
        };
        // define / minify / mangle 是每个 bundler 的常量（TaskId 未纳入——同一引擎内不变；
        // 跨引擎无共享内存缓存）。产物磁盘缓存键则由 body_key 混入 define/minify/mangle 指纹区分。
        let dv: Vec<(&str, &str)> = define
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        parsed.ast.with_ast(|program| {
            // —— CSS-in-JS（Linaria 子集）：与 mangle/minify 无关，先跑 ——
            // 产出「标签模板 span → 类名字面量」替换 + 本模块抽取的 CSS。
            let cij = css_in_js
                .as_ref()
                .zip(cij_seed.as_deref())
                .map(|(imported, seed)| {
                    wake_css_in_js::transform(program, &interner, &parsed.source, seed, imported)
                });

            // emit 把每个模块包成 `function(m,$,_r){…}`（`m`=module、`$`=exports、`_r`=require 的
            // 压缩名）。声明为保留名，mangler 不会把局部压成它们而与包装器参数撞车（曾致 React
            // 产物 `class m{}` 与参数 `m` 重复声明 → SyntaxError）。
            let export_pairs = collect_export_var_pairs(program, &interner);
            let mut protected_names = vec!["m", "$", "_r"];
            protected_names.extend(
                export_pairs
                    .iter()
                    .map(|(_, local_name, _)| local_name.as_str()),
            );
            let semantic = (minify || mangle).then(|| analyze(program));
            let semantic_safe = !has_hazard(program, &interner);
            let plan = (mangle && semantic_safe).then(|| {
                plan_mangle_with_model_and_protected(
                    program,
                    &interner,
                    &protected_names,
                    semantic.as_ref().expect("semantic model was requested"),
                    cij.as_ref()
                        .map(|c| c.verbatim_replacement_spans.as_slice())
                        .unwrap_or_default(),
                )
            });
            // 属性名混淆默认关闭：逐模块、基于名字频率的属性重命名从根本上不健全——
            // 它无法看到跨模块 / 宿主（React style、DOM）对属性的读取，也无法处理计算成员
            // `obj[expr]` 与运行时字符串键，会破坏 enum 成员访问、内联 style、查表等。
            // 对齐 esbuild（不混淆属性）/ Terser（默认关闭）。TODO：改为命名约定 opt-in。
            let _ = wake_ecma_minify::plan_prop_mangle;
            {
                let mut minify_ctx = MinifyCtx {
                    defines: &dv,
                    prop_rename: None,
                    ..MinifyCtx::default()
                };
                // CSS-in-JS 的替换先于 minify 填充：二者 span 不相交（前者是 TaggedTemplate，
                // 后者来自常量折叠），顺序不影响结果。
                if let Some(c) = &cij {
                    for (span, text) in &c.replacements {
                        minify_ctx
                            .expression_replacements
                            .insert(*span, text.clone());
                    }
                }
                if minify {
                    let source: &str = &parsed.source;

                    // 1) 表达式简化 + 常量折叠
                    let sp = plan_simplifications(program, source, &interner);
                    for (span, action) in &sp.actions {
                        match action {
                            SimplifyAction::RemoveDoubleNot => {
                                minify_ctx.double_not_spans.insert(*span);
                            }
                            SimplifyAction::BracketToDot => {
                                if let Some(name) = sp.bracket_names.get(span) {
                                    minify_ctx.bracket_to_dot.insert(*span, name.clone());
                                }
                            }
                            SimplifyAction::ReplaceWith(text) => {
                                minify_ctx
                                    .expression_replacements
                                    .insert(*span, text.clone());
                            }
                        }
                    }
                    minify_ctx.constants = sp.constants;

                    // 2) DCE
                    let dce = analyze_dce(program, &interner, drop_debugger, drop_console);

                    // 3) 变量使用分析（未引用变量消除 + 变量内联）。被 tree shaking 删除的
                    // export 不再人为保活其本地声明；同一本地仍由另一个活跃导出引用时除外。
                    let keep_set: Option<FxHashSet<&str>> =
                        keep.map(|keep_names| keep_names.iter().map(String::as_str).collect());
                    let export_pairs = keep_set
                        .as_ref()
                        .map(|_| collect_export_var_pairs(program, &interner));
                    let removable_export_locals =
                        if let (Some(keep_set), Some(pairs)) = (&keep_set, &export_pairs) {
                            let live_locals: FxHashSet<&str> = pairs
                                .iter()
                                .filter(|(export, _, _)| keep_set.contains(export.as_str()))
                                .map(|(_, local, _)| local.as_str())
                                .collect();
                            pairs
                                .iter()
                                .filter(|(export, local, _)| {
                                    !keep_set.contains(export.as_str())
                                        && !live_locals.contains(local.as_str())
                                })
                                .map(|(_, local, _)| interner.intern(local))
                                .collect()
                        } else {
                            FxHashSet::default()
                        };
                    let va = if semantic_safe {
                        analyze_vars_with_model(
                            program,
                            semantic.as_ref().expect("semantic model was requested"),
                            &dce.remove_spans,
                            &removable_export_locals,
                        )
                    } else {
                        Default::default()
                    };
                    minify_ctx.remove_spans = dce.remove_spans;
                    minify_ctx.unused_vars = va.unused_vars;
                    minify_ctx.unused_var_spans = va.unused_var_spans;

                    // 3b) Tree shaking 整合：导出被移除时，标记对应声明 span。
                    // Span 匹配确保作用域安全（不同作用域的同名变量不同 span）。
                    if let (Some(keep_set), Some(pairs)) = (&keep_set, &export_pairs) {
                        for (export_name, _var_name, decl_span) in pairs {
                            if !keep_set.contains(export_name.as_str()) {
                                minify_ctx.removed_export_spans.insert(*decl_span);
                            }
                        }
                    }

                    // 4) 变量内联（Phase 2.4）：将单次使用纯变量的初始化表达式注入 inline_vars。
                    // 按该变量**唯一一次使用**的引用 span 索引（非名字）——否则会把其它作用域里
                    // 同名变量的引用也一并替换（曾致 react-dom 局部 root/lane 被换成模块级同名变量）。
                    if !va.inline_candidates.is_empty() {
                        let init_map = collect_init_map(program);
                        for (name, &decl_span) in &va.inline_candidates {
                            if let (Some(&ref_span), Some(init)) =
                                (va.inline_ref_spans.get(name), init_map.get(&decl_span))
                            {
                                minify_ctx.inline_vars.insert(ref_span, *init);
                            }
                        }
                    }

                    // 5) Phase 3: statement-level optimizations
                    let if_return = wake_ecma_minify::analyze_if_return(program);
                    let join_vars = wake_ecma_minify::analyze_join_vars(program);
                    let seq = wake_ecma_minify::analyze_sequences(program);
                    minify_ctx.populate_stmts(if_return, join_vars, seq);

                    // 6) Scope hoisting：默认关闭。原实现把嵌套（含条件分支 if/switch/try）里的
                    // `var x = <init>` 连同**初始化器**提到函数顶部，改变了求值时机与顺序——当 init
                    // 有副作用或会抛（如守卫内 `var w = a.b` 中 a 在守卫外为 null）时直接改变语义
                    // （曾致 react-dom `flushMutationEffects` 读 null.flags 崩溃）。安全的作用域提升需
                    // 完整的「不抛 + 无副作用 + 提升后不改变可观察值」分析，暂未实现；先关闭保正确性。
                    // let hoist_plan = wake_ecma_minify::plan_hoist(program);
                    // minify_ctx.hoist = hoist_plan;
                    let _ = wake_ecma_minify::plan_hoist;

                    // 7) Check if undefined is safe to replace with void 0
                    minify_ctx.no_undefined_shadow = !is_undefined_shadowed(program, &interner);

                    minify_ctx.minify = true;
                }
                if let Some(c) = &cij {
                    minify_ctx
                        .remove_spans
                        .extend(c.removable_import_spans.iter().copied());
                }

                let rename = plan.as_ref().map(|p| p.table());
                // 仅在确有侧表要用时传 ctx：传 `Some(空 ctx)` 会让 codegen 走
                // `emit_var_decl_elim` 等分支，改变无 minify 场景的既有产物。
                let ctx_ref = (minify || mangle || cij.is_some()).then_some(&minify_ctx);

                let (code, map) = if want_map {
                    let (c, m) = codegen_module_shaken_with_map(
                        program,
                        &interner,
                        &linker,
                        keep,
                        &dv,
                        minify,
                        rename,
                        ctx_ref,
                        no_esmodule,
                        minify_names,
                    );
                    (c, Some(Arc::new(m)))
                } else {
                    (
                        codegen_module_shaken_mangled(
                            program,
                            &interner,
                            &linker,
                            keep,
                            &dv,
                            minify,
                            rename,
                            ctx_ref,
                            no_esmodule,
                            minify_names,
                        ),
                        None,
                    )
                };

                // dev（不抽取 CSS）时把样式随模块体注入 `<style>`，与 `.css` 模块的 dev 行为一致；
                // prod 则把 CSS 带出，由 driver 聚合进 `styles.<hash>.css`。
                let css = cij.as_ref().filter(|c| !c.css.is_empty()).map(|c| &c.css);
                let code = match css {
                    Some(css) if inject_style => {
                        let mut s = code;
                        append_style_injection(&mut s, css);
                        s
                    }
                    _ => code,
                };
                CodegenBody {
                    code: Arc::new(code),
                    map,
                    css: if inject_style {
                        None
                    } else {
                        css.map(|c| Arc::new(c.clone()))
                    },
                    diagnostics: cij.map(|c| c.diagnostics).unwrap_or_default(),
                }
            }
        })
    });
    vc.read()
}

/// 给模块体追加运行时 `<style>` 注入（dev 路径；对齐 `loader.rs` 的 CSS dev 行为）。
///
/// `typeof document` 守卫使 SSR / node 下静默跳过。
fn append_style_injection(js: &mut String, css: &str) {
    js.push_str("\nif (typeof document !== \"undefined\") {\n");
    js.push_str("  var __wake_cij__ = document.createElement(\"style\");\n");
    js.push_str("  __wake_cij__.textContent = ");
    crate::loader::push_js_string(js, css);
    js.push_str(";\n  document.head.appendChild(__wake_cij__);\n}\n");
}

/// parse 任务体：读内容 cell（登记依赖）→ 解析（TS 模式跳过类型）→ 依赖句柄解为字符串。
fn parse_module(
    cell: Vc<Content>,
    interner: &Interner,
    source_type: SourceType,
    jsx: JsxRuntimeOptions,
    transform_features: FeatureSet,
    file_name: Option<Arc<str>>,
) -> ParsedModule {
    let src = cell.read(); // Arc<Content>；读取即登记对内容 cell 的依赖
    let text: &str = &src;
    let out = parse_with(
        text,
        interner,
        source_type,
        wake_ecma_parser::ParseOptions {
            jsx_import_source: jsx.import_source,
            jsx_dev: jsx.dev,
            file_name: file_name.as_deref().unwrap_or(""),
            transform_features,
        },
    );
    let source: Arc<str> = (*src).clone();
    let deps = out
        .dependencies
        .iter()
        .map(|d| ParsedDep {
            specifier: interner.resolve(d.specifier),
            kind: d.kind,
            span: d.span,
        })
        .collect();
    ParsedModule {
        ast: out.module,
        source,
        deps,
        diagnostics: out.diagnostics,
        has_top_level_await: out.has_top_level_await,
    }
}

/// 死模块消除：从 `entry_id` 出发，按各模块体中**存活**的 `__wake_require__(id)` / `.import(cid,id)`
/// 调用（codegen DCE 已剥离死 `require`）重算可达模块集。误判只会「多留」不会「错删」。
fn live_modules(bodies: &[(u32, Arc<String>)], entry_id: u32) -> FxHashSet<u32> {
    let mut edges: FxHashMap<u32, FxHashSet<u32>> = FxHashMap::default();
    for (id, body) in bodies {
        let mut refs = FxHashSet::default();
        extract_referenced_ids(body, &mut refs);
        edges.insert(*id, refs);
    }
    let mut live: FxHashSet<u32> = FxHashSet::default();
    let mut stack = vec![entry_id];
    while let Some(id) = stack.pop() {
        if !live.insert(id) {
            continue;
        }
        if let Some(refs) = edges.get(&id) {
            for &r in refs {
                if !live.contains(&r) {
                    stack.push(r);
                }
            }
        }
    }
    live
}

/// 从一段模块体提取其引用的内部模块 id：`__wake_require__(<id>)`（静态/内联动态）与
/// `__wake_require__.import(<cid>, <id>)`（跨 chunk 动态，取第二参 = 模块 id）。
fn extract_referenced_ids(body: &str, out: &mut FxHashSet<u32>) {
    const NEEDLE: &str = "__wake_require__";
    let mut rest = body;
    while let Some(pos) = rest.find(NEEDLE) {
        let after = &rest[pos + NEEDLE.len()..];
        if let Some(a) = after.strip_prefix('(') {
            if let Some(id) = parse_leading_u32(a) {
                out.insert(id);
            }
        } else if let Some(a) = after.strip_prefix(".import(") {
            // 第一参是 chunk id，第二参是模块 id。
            let a = skip_u32(a.trim_start());
            let a = a.trim_start().strip_prefix(',').unwrap_or(a).trim_start();
            if let Some(id) = parse_leading_u32(a) {
                out.insert(id);
            }
        }
        rest = after;
    }
}

/// 解析字符串前缀的十进制 u32（无前导数字 → `None`）。
fn parse_leading_u32(s: &str) -> Option<u32> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        None
    } else {
        s[..end].parse().ok()
    }
}

/// 跳过字符串前缀的十进制数字，返回剩余。
fn skip_u32(s: &str) -> &str {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    &s[end..]
}

/// 检测模块 body 是否仅含 `__reg` 副作用（无 exports、无 require、无局部声明）。
fn is_pure_reg_body(body: &str) -> bool {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return true;
    }
    if trimmed.contains("exports[") || trimmed.contains("exports.") {
        return false;
    }
    if trimmed.contains("__wake_require__") {
        return false;
    }
    if !trimmed.contains("globalThis.__reg") {
        return false;
    }
    // 不可含任何局部声明——内联到同一作用域会导致 Identifier 重复声明错误。
    if trimmed.contains("let ") || trimmed.contains("const ") || trimmed.contains("var ") {
        return false;
    }
    if trimmed.contains("function ") || trimmed.contains("async ") || trimmed.contains("class ") {
        return false;
    }
    true
}

/// 遍历 body 中每个 `__wake_require__(N)` 调用，回调 `(require 起始字节偏移 abs, id, 右括号后一位 call_end)`。
/// 只匹配 `(` 后为纯十进制数字且紧跟 `)` 的形式（codegen 产出的 require 调用恒如此）。
/// 单遍 O(body) 扫描——替代「对全部候选 id 各扫一遍」的 O(id 数 × body) 反模式。
fn for_each_require<F: FnMut(usize, u32, usize)>(body: &str, mut f: F) {
    const NEEDLE: &str = "__wake_require__(";
    let bytes = body.as_bytes();
    let mut search = 0;
    while let Some(rel) = body[search..].find(NEEDLE) {
        let abs = search + rel;
        let after = abs + NEEDLE.len();
        let mut end = after;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > after
            && end < bytes.len()
            && bytes[end] == b')'
            && let Ok(id) = body[after..end].parse::<u32>()
        {
            f(abs, id, end + 1); // end 指向 ')'，call_end = 其后一位
        }
        search = after;
    }
}

/// 尝试把 `abs` 处的 require 识别为 barrel re-export 并返回待删区间 `[const 起点, for 循环结束)`。
/// 匹配：`const _wm{VAR} = __wake_require__(id);for (const _k in _wm{VAR}) if (_k !== "default") exports[_k] = _wm{VAR}[_k];`
/// 全为**局部**（有界回看/前看）检测，故整趟 strip 保持 O(body)。逐字节复刻原逐-id 版的判定。
fn try_barrel(body: &str, bytes: &[u8], abs: usize, call_end: usize) -> Option<(usize, usize)> {
    // require 前必须是 ` = `（空格=空格）。
    if abs < 3 || &body[abs - 3..abs] != " = " {
        return None;
    }
    // ` = ` 之前是 VAR 数字。
    let var_end = abs - 3;
    let mut ds = var_end;
    while ds > 0 && bytes[ds - 1].is_ascii_digit() {
        ds -= 1;
    }
    if ds == var_end {
        return None; // 无数字
    }
    // VAR 数字前应是 `_wm`，再前 `const `。
    if ds < 3 || &body[ds - 3..ds] != "_wm" {
        return None;
    }
    let wm_start = ds - 3;
    if wm_start < 6 || &body[wm_start - 6..wm_start] != "const " {
        return None;
    }
    let const_start = wm_start - 6;
    let var_name = &body[wm_start..var_end]; // `_wm{digits}`
    // require 后需 `;`，随后（可跳前导空白）紧跟精确的 for 展开。
    if call_end >= bytes.len() || bytes[call_end] != b';' {
        return None;
    }
    let mut fstart = call_end + 1;
    while fstart < bytes.len() && bytes[fstart].is_ascii_whitespace() {
        fstart += 1;
    }
    let for_pat = format!(
        "for (const _k in {var_name}) if (_k !== \"default\") exports[_k] = {var_name}[_k];"
    );
    if body[fstart..].starts_with(&for_pat) {
        Some((const_start, fstart + for_pat.len()))
    } else {
        None
    }
}

/// 移除对 hoisted 模块的引用（单遍 O(body)）：
/// 1) barrel re-export 整段：`const _wmX = __wake_require__(N);for (..._wmX...)`
/// 2) 独立 `__wake_require__(N);` 语句（前置为 `;`/`{`/起始）
///
/// 原实现对每个 hoisted id（可达上千）各 `format!`+全串 `find`/重建一遍——O(id 数 × body)，且被少数
/// 引用极多候选的巨型 barrel/聚合模块放大到几十毫秒。此版单遍扫描每个 require 调用，就地按局部上下文
/// 分类（barrel / 独立 / 非独立保留），收集**互不相交**的删除区间后一次性重建。与原版逐字节等价：
/// 各 id 的删除区间互不相交 → 处理顺序无关；barrel/独立的判定条件逐字节复刻。
fn strip_hoisted_requires_and_barrels(body: &str, hoisted: &FxHashSet<u32>) -> String {
    let bytes = body.as_bytes();
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    for_each_require(body, |abs, id, call_end| {
        if !hoisted.contains(&id) {
            return;
        }
        // barrel 优先（其 require 前置为 ` = `，天然非独立）。
        if let Some(span) = try_barrel(body, bytes, abs, call_end) {
            cuts.push(span);
            return;
        }
        // 独立语句：前置 `;`/`{`/起始，且 require 后紧跟 `;`。
        let prev_ok = abs == 0 || bytes[abs - 1] == b';' || bytes[abs - 1] == b'{';
        if prev_ok && call_end < bytes.len() && bytes[call_end] == b';' {
            cuts.push((abs, call_end + 1)); // 含末尾 `;`
        }
    });
    if cuts.is_empty() {
        return body.to_string();
    }
    // 按起点排序并跳过重叠（区间本应互不相交，合并仅为稳健）。一次性重建。
    cuts.sort_unstable_by_key(|c| c.0);
    let mut out = String::with_capacity(body.len());
    let mut pos = 0;
    for (s, e) in cuts {
        if s > pos {
            out.push_str(&body[pos..s]);
            pos = e;
        } else if e > pos {
            pos = e;
        }
    }
    out.push_str(&body[pos..]);
    out
}

/// 紧凑 __reg body 格式：去空格、逗号分隔 → webpack 风格
fn compact_reg_body(body: &str) -> String {
    let mut s = body.replace(" || (", "||(").replace(" = {});", "={});");
    s = s.replace(";globalThis.__reg.", ",globalThis.__reg.");
    s = s.replace(" = ", "=");
    s
}

/// If `body` is exactly the generated registry bootstrap followed by one numeric property
/// assignment, return the `property=number` tail. Callers may only cache the registry object across
/// exact bodies in the same uninterrupted run.
fn exact_reg_assignment(body: &str) -> Option<&str> {
    const PREFIX: &str = "globalThis.__reg||(globalThis.__reg={}),";
    let assignment = body.trim_end_matches(';').strip_prefix(PREFIX)?;
    let value = assignment.strip_prefix("globalThis.__reg.")?;
    let (property, number) = value.split_once('=')?;
    if property.is_empty()
        || !property
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
        || number.is_empty()
        || !number
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-' | b'e' | b'E'))
    {
        return None;
    }
    Some(value)
}

/// 输出非空的内联 registry body，并仅在实际输出项之间添加逗号。
///
/// hoist 候选模块的 body 在剥离 require/barrel 语句后可能为空。如果按原候选下标添加
/// 分隔符，会生成 `;,,,;` 这样的非法 JavaScript。
fn append_inline_regs(out: &mut String, inline_regs: &[String]) {
    let mut emitted = false;
    let mut registry_ready = false;

    for reg in inline_regs {
        let compact = compact_reg_body(reg);
        let compact = compact.trim().trim_end_matches(';').trim_end();
        if compact.is_empty() {
            continue;
        }

        if emitted {
            out.push(',');
        } else {
            out.push(';');
            emitted = true;
        }

        if let Some(assignment) = exact_reg_assignment(compact) {
            if registry_ready {
                out.push_str("q.");
                out.push_str(assignment);
            } else {
                out.push_str("q=globalThis.__reg||(globalThis.__reg={}),q.");
                out.push_str(assignment);
                registry_ready = true;
            }
        } else {
            out.push_str(compact);
            registry_ready = false;
        }
    }

    if emitted {
        out.push(';');
    }
}

#[cfg(test)]
mod inline_reg_tests {
    use super::append_inline_regs;

    #[test]
    fn skips_empty_registry_bodies_without_leaving_commas() {
        let regs = vec![
            String::new(),
            " ;;;".to_string(),
            "globalThis.__reg || (globalThis.__reg = {});globalThis.__reg.a = 1;".to_string(),
            "  ".to_string(),
            "globalThis.__reg || (globalThis.__reg = {});globalThis.__reg.b = 2;".to_string(),
        ];
        let mut out = "runtime".to_string();

        append_inline_regs(&mut out, &regs);

        assert_eq!(
            out,
            "runtime;q=globalThis.__reg||(globalThis.__reg={}),q.a=1,q.b=2;"
        );
        assert!(!out.contains(",,"));
    }

    #[test]
    fn emits_nothing_when_all_registry_bodies_are_empty() {
        let mut out = "runtime".to_string();
        append_inline_regs(&mut out, &[String::new(), " ; ".to_string()]);
        assert_eq!(out, "runtime");
    }
}

/// 紧凑模块 body 中的运行时引用：`__wake_require__`→`_r`，自由变量 `exports`→`$`。
///
/// **`module.exports` 必须改写成 `m.exports`，不能是 `m.$`**：包装器
/// `function(m,$,_r)` 由 `.call(module.exports, module, module.exports, __wake_require__)`
/// 调用，`$` 是 exports 对象的**值**、`m` 才是 module 本身。写成 `m.$` 只是在 module 上挂了个
/// 名为 `$` 的无关属性，`module.exports` 从未被重新赋值 → 该模块导出恒为空对象
/// （曾致任何 `module.exports = X` 形态的 CJS 包整包失效，如 `@linaria/core` 的 `cx`）。
///
/// 用占位符隔离，避免后一步的 `exports`→`$` 把刚写好的 `m.exports` 又改回 `m.$`。
#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimeNames {
    module: String,
    exports: String,
    require: String,
}

impl RuntimeNames {
    fn for_bodies<'a>(bodies: impl IntoIterator<Item = &'a str>) -> Self {
        let bodies: Vec<&str> = bodies.into_iter().collect();
        let mut used = FxHashSet::default();
        let mut pick = |preferred: &str| {
            for suffix in 0u32.. {
                let candidate = if suffix == 0 {
                    preferred.to_string()
                } else {
                    format!("{preferred}{suffix}")
                };
                if !used.contains(&candidate)
                    && !bodies
                        .iter()
                        .any(|body| contains_identifier(body, &candidate))
                {
                    used.insert(candidate.clone());
                    return candidate;
                }
            }
            unreachable!()
        };
        Self {
            module: pick("m"),
            exports: pick("$"),
            require: pick("_r"),
        }
    }
}

fn contains_identifier(source: &str, needle: &str) -> bool {
    fn ident_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$') || b >= 0x80
    }
    let bytes = source.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || needle.len() > bytes.len() {
        return false;
    }
    bytes.windows(needle.len()).enumerate().any(|(i, window)| {
        window == needle
            && (i == 0 || !ident_byte(bytes[i - 1]))
            && (i + needle.len() == bytes.len() || !ident_byte(bytes[i + needle.len()]))
    })
}

fn compact_body_names(body: &str, names: &RuntimeNames) -> String {
    // NUL 不可能出现在 JS 源码里，可安全作占位。占位符自身**不得含 `exports` 子串**，
    // 否则会被下一步的 `exports`→`$` 改坏而无法还原。
    const MODULE_EXPORTS: &str = "\u{0}wakeME\u{0}";
    body.replace("module.exports", MODULE_EXPORTS)
        .replace("__wake_require__(", &format!("{}(", names.require))
        .replace("exports", &names.exports)
        .replace(MODULE_EXPORTS, &format!("{}.exports", names.module))
}

/// 从 entry 出发的**依赖后序**编号（模块 id → 序号），即 ESM 的求值顺序：依赖先于消费方，
/// 同一模块的多个依赖按源码中的出现顺序。
///
/// 供 prod CSS 聚合排序用——层叠顺序必须与 dev 的 `<style>` 注入顺序（模块求值序）一致。
/// 深度优先按 `deps` 原序展开；不区分静态/动态依赖，保证每个可达模块都拿到序号。
fn css_emission_order(modules: &FxHashMap<u32, ModuleRec>, entry_id: u32) -> FxHashMap<u32, u32> {
    let mut order: FxHashMap<u32, u32> = FxHashMap::default();
    let mut seen: FxHashSet<u32> = FxHashSet::default();
    let mut next = 0u32;
    if !modules.contains_key(&entry_id) {
        return order;
    }
    seen.insert(entry_id);
    // 显式栈的后序 DFS：`(模块 id, 下一个待展开的依赖下标)`。
    let mut stack: Vec<(u32, usize)> = vec![(entry_id, 0)];
    while let Some((id, ci)) = stack.pop() {
        let Some(rec) = modules.get(&id) else {
            order.entry(id).or_insert_with(|| {
                let n = next;
                next += 1;
                n
            });
            continue;
        };
        if ci < rec.deps.len() {
            stack.push((id, ci + 1));
            let spec = rec.deps[ci].specifier.as_str();
            if let Some(&(_, child)) = rec.dep_ids.iter().find(|(s, _)| s == spec)
                && seen.insert(child)
            {
                stack.push((child, 0));
            }
        } else {
            order.insert(id, next);
            next += 1;
        }
    }
    order
}

/// 模块体写入 exports 对象的**导出名**集合。
///
/// 只认 codegen 实际发射的两种赋值形态——`exports["name"] = …`（[`emit_export_binding`] /
/// re-export）与 `exports.name = …`（默认导出）；`module.exports` 显式排除（那条路由
/// [`reassigns_module_exports`] 处理）。要求其后紧跟 `=` 且非 `==`/`=>`，避免把读取
/// （`exports.foo` 作右值）当成导出。
///
/// 字符串字面量里若恰好含 `exports["x"]=` 会被误记 → 该模块多降级一次（不进 concat），
/// **偏向安全方向**：只损失一点体积优化，不影响正确性。
///
/// [`emit_export_binding`]: wake_ecma_codegen
fn exported_names(body: &str) -> FxHashSet<String> {
    let mut out = FxHashSet::default();
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while let Some(pos) = body[i..].find("exports") {
        let at = i + pos;
        let after = at + "exports".len();
        i = after;
        // `module.exports` 不是这里的目标。
        if at >= "module.".len() && &body[at - "module.".len()..at] == "module." {
            continue;
        }
        // `exports` 必须是完整词（前一个字符不能是标识符字符）。
        if at > 0 {
            let p = bytes[at - 1];
            if p.is_ascii_alphanumeric() || p == b'_' || p == b'$' || p == b'.' {
                continue;
            }
        }
        let rest = &body[after..];
        let (name, tail) = if let Some(r) = rest.strip_prefix('[') {
            // `exports["name"]` / `exports['name']`
            let r = r.trim_start();
            let Some(quote) = r.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                continue;
            };
            let inner = &r[quote.len_utf8()..];
            let Some(end) = inner.find(quote) else {
                continue;
            };
            let Some(t) = inner[end + quote.len_utf8()..]
                .trim_start()
                .strip_prefix(']')
            else {
                continue;
            };
            (inner[..end].to_string(), t)
        } else if let Some(r) = rest.strip_prefix('.') {
            // `exports.name`
            let end = r
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
                .unwrap_or(r.len());
            if end == 0 {
                continue;
            }
            (r[..end].to_string(), &r[end..])
        } else {
            continue;
        };
        // 只算**赋值**，不算读取。
        let t = tail.trim_start();
        if let Some(t2) = t.strip_prefix('=')
            && !t2.starts_with('=')
            && !t2.starts_with('>')
        {
            out.insert(name);
        }
    }
    out
}

/// 模块体是否**整体重新赋值** `module.exports`（`module.exports = X`），而非只挂属性
/// （`module.exports.foo = X`）。
///
/// 这类 CJS 模块**不能**并入 scope-hoist 的 concat：concat 让所有被合并模块共享同一个
/// exports 对象 `$`，而整体赋值会把 `module.exports` 换成**另一个**对象——此后
/// 该模块的导出与其它模块写入的 `$` 分属两个对象，必丢其一
/// （曾致 `@linaria/core` 并入后 `cx` 与同组 ESM 模块的导出互相覆盖丢失）。
fn reassigns_module_exports(body: &str) -> bool {
    let mut rest = body;
    while let Some(pos) = rest.find("module.exports") {
        let after = &rest[pos + "module.exports".len()..];
        let trimmed = after.trim_start();
        // `=` 且非 `==`/`===`/`=>`：是整体赋值。`.foo =` / `[x] =` 只是挂属性，不算。
        if let Some(t) = trimmed.strip_prefix('=')
            && !t.starts_with('=')
            && !t.starts_with('>')
        {
            return true;
        }
        rest = after;
    }
    false
}

/// 移除独立的 `_r(N);` 调用（合并模块内不再需要）
fn strip_standalone_requires(body: &str) -> String {
    let mut result = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    let n = bytes.len();
    while i < n {
        if i + 3 <= n && &bytes[i..i + 3] == b"_r(" {
            let prev_ok = i == 0 || bytes[i - 1] == b';' || bytes[i - 1] == b'{';
            if prev_ok {
                let mut j = i + 3;
                while j < n && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j > i + 3 {
                    let mut k = j;
                    while k < n && bytes[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    if k < n && bytes[k] == b')' {
                        k += 1;
                    }
                    while k < n && bytes[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    if k < n && bytes[k] == b';' {
                        k += 1;
                    }
                    i = k;
                    continue;
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 拓扑排序：按依赖顺序排列模块，确保依赖方先于被依赖方执行。
/// 解析各模块 body 中的 `_r(N)` 调用构建依赖图，忽略对自身和不在集合内的引用。
fn topo_sort_modules(modules: &[(u32, String)]) -> Vec<(u32, String)> {
    let id_set: FxHashSet<u32> = modules.iter().map(|(id, _)| *id).collect();

    // 解析每个模块的内部依赖
    let mut deps: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
    for (id, body) in modules {
        let mut module_deps = Vec::new();
        for_each_require(body, |_, n, _| {
            if id_set.contains(&n) && n != *id {
                module_deps.push(n);
            }
        });
        deps.insert(*id, module_deps);
    }

    // DFS 拓扑排序（无环则严格有序，有环按访问序保证无死循环）
    let mut sorted: Vec<u32> = Vec::with_capacity(modules.len());
    let mut visited: FxHashSet<u32> = FxHashSet::default();
    let mut visiting: FxHashSet<u32> = FxHashSet::default();

    fn dfs(
        id: u32,
        deps: &FxHashMap<u32, Vec<u32>>,
        visited: &mut FxHashSet<u32>,
        visiting: &mut FxHashSet<u32>,
        sorted: &mut Vec<u32>,
    ) {
        if visited.contains(&id) || visiting.contains(&id) {
            return;
        }
        visiting.insert(id);
        if let Some(d) = deps.get(&id) {
            for &dep_id in d {
                dfs(dep_id, deps, visited, visiting, sorted);
            }
        }
        visiting.remove(&id);
        visited.insert(id);
        sorted.push(id);
    }

    for (id, _) in modules {
        dfs(*id, &deps, &mut visited, &mut visiting, &mut sorted);
    }

    // 按 sorted 顺序重排 modules
    let pos: FxHashMap<u32, usize> = sorted.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let mut result = modules.to_vec();
    result.sort_by_key(|(id, _)| pos.get(id).copied().unwrap_or(usize::MAX));
    result
}

/// 拼接各模块（已 codegen 的）函数体 + mini runtime。
///
/// `async_ids`：async 子图（顶层 await）。非空时包装器改 `async function`、runtime 换 Promise 感知版、
/// 且**关闭模块合并**；**为空时逐字节等同于改造前的产物**（无顶层 await 的项目零影响）。
///
/// `body_starts`：非 `None` 时，回填「模块 id → 体首行在 bundle 中的 0 基行号」，供
/// [`merge_bundle_map`] 平移模块内映射。**仅非 minify 分支回填**（minify 分支会重排/改写
/// 模块体，行偏移法不成立）。
fn emit(
    bodies: &[(u32, Arc<String>)],
    entry_id: u32,
    minify: bool,
    no_esmodule: bool,
    block_infos: &FxHashMap<u32, ConcatBlockInfo>,
    async_ids: &FxHashSet<u32>,
    exec: &Executor,
    body_starts: Option<&mut Vec<(u32, u32)>>,
) -> String {
    let mut keep_bodies: Vec<(u32, Arc<String>)> = Vec::new();

    if minify {
        let mut candidates: FxHashMap<u32, Arc<String>> = FxHashMap::default();
        let runtime_names = RuntimeNames::for_bodies(bodies.iter().map(|(_, body)| body.as_str()));

        for (id, body) in bodies {
            if is_pure_reg_body(body) {
                candidates.insert(*id, body.clone());
            } else {
                keep_bodies.push((*id, body.clone()));
            }
        }

        // 预剥离：candidates 中的模块无导出 → 任何引用它们的 barrel re-export 行都是空操作。
        // 在检查非独立引用前先剥离，避免 candidates 因 barrel 行被错误回退。
        // **并行**：各 keep body 的 strip 互相独立，经执行器扇出（保序 → 输出不变）。少数大 barrel
        // 模块引用了大量候选，串行 strip 是 emit 的剩余大头。
        let pre_hoisted: Arc<FxHashSet<u32>> = Arc::new(candidates.keys().copied().collect());
        let stripped_bodies: Vec<(u32, String)> = if pre_hoisted.is_empty() {
            keep_bodies
                .iter()
                .map(|(id, body)| (*id, (**body).clone()))
                .collect()
        } else {
            let jobs: Vec<_> = keep_bodies
                .iter()
                .map(|(id, body)| {
                    let id = *id;
                    let body = body.clone();
                    let ph = pre_hoisted.clone();
                    move || (id, strip_hoisted_requires_and_barrels(&body, &ph))
                })
                .collect();
            exec.parallel(jobs)
        };

        // 检查剥离后的模块中是否还有非独立引用（如 `const x = require(N)` 形式的直接导入）。
        // 单遍扫每个 body 的 `__wake_require__(N)`：N 属候选且该处为非独立引用（前置非 `;`/`{`/起始）
        // 则记入 ref_ids。等价于原「对每个候选 × 每个 body 调 has_non_standalone_ref」但从
        // O(候选数 × body) 降到 O(body)。
        let mut ref_ids: FxHashSet<u32> = FxHashSet::default();
        for (_, body) in &stripped_bodies {
            let bytes = body.as_bytes();
            for_each_require(body, |abs, id, _| {
                if candidates.contains_key(&id) {
                    let is_standalone =
                        abs == 0 || bytes[abs - 1] == b';' || bytes[abs - 1] == b'{';
                    if !is_standalone {
                        ref_ids.insert(id);
                    }
                }
            });
        }

        // 原此处有第二遍 `strip_hoisted_requires_and_barrels`：其 `hoisted` = `pre_hoisted`（同为
        // `candidates.keys()`），在已被第一遍剥净同一集合的 body 上重跑 → 可证明 no-op（删除只移除文本、
        // 不新增 require，同集合二次扫描无可删）。故直接复用第一遍结果，省一整趟 O(候选数 × body)。
        let mut filtered: Vec<(u32, String)> = stripped_bodies;

        // 收集 inline __reg：所有 candidates 的 body 直接内联
        // FxHashMap 的迭代顺序既不稳定，也会把路径/模块编号相近的注册语句随机打散，
        // 显著破坏 Gzip/Brotli 的局部重复匹配。按稳定 module id 输出。
        let mut inline_reg_entries: Vec<_> = candidates.iter().collect();
        inline_reg_entries.sort_unstable_by_key(|(id, _)| **id);
        let inline_regs: Vec<String> = inline_reg_entries
            .into_iter()
            .map(|(_, body)| (**body).clone())
            .collect();

        // 构建最终模块表。含顶层 await 时**不做模块合并**：合并闭包是单个函数，无法表达
        // 「其中一部分模块是 async、且彼此有 await 依赖顺序」；退回逐模块注册表（仍是紧凑产物）。
        let mut final_modules: Vec<(u32, String)> = Vec::new();
        if async_ids.is_empty() {
            // —— 模块合并：将所有非 hoist 模块体拼接到一个闭包，避免命名冲突 ——
            let concat_id = entry_id + (filtered.len() as u32) + 1000;
            // 被合并模块是否全为 ESM（恒 strict-safe）→ 给 concat 函数加 `"use strict"`，使**块级函数声明
            // 变块作用域**，从而可用裸 `{}` 块替代 IIFE 包裹（省 `(function(){`+`})();` 开销）而不发生
            // 顶层名跨模块碰撞。任一非 ESM（可能依赖 sloppy 语义）→ 全走 IIFE 且不加 strict（保守安全）。
            let all_esm = filtered
                .iter()
                .filter(|(id, _)| *id != entry_id)
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            let all_esm = !all_esm.is_empty()
                && all_esm
                    .iter()
                    .all(|id| block_infos.get(id).is_some_and(|bi| bi.is_esm));
            let mut concat_body = String::new();
            if all_esm {
                concat_body.push_str("\"use strict\";");
            }
            // 拓扑排序：确保依赖模块先于消费方执行，避免 _r(N) 返回空的 $
            filtered = topo_sort_modules(&filtered);

            // 整体重新赋值 `module.exports` 的 CJS 模块必须留作**独立注册模块**：它们会把
            // `module.exports` 换成新对象，与 concat 共享的 `$` 分裂成两个导出对象。
            let mut standalone: FxHashSet<u32> = filtered
                .iter()
                .filter(|(id, body)| *id != entry_id && reassigns_module_exports(body))
                .map(|(id, _)| *id)
                .collect();

            // **导出名冲突**同理必须降级：concat 让所有成员共享同一个 exports 对象 `$`，两个模块
            // 写同一个名字就是后者覆盖前者、静默丢值。`default` 尤其必撞——`export default` 是
            // 最常见的写法，而**每个资源模块恰好都是 `export default "<url>"`**，所以只要有两个
            // 图片/字体被 import，它们的 URL 就会串（实测 `import a from './a.js'` +
            // `import b from './b.js'` 各自 `export default` 曾产出 `"BB"` 而非 `"AB"`）。
            //
            // 按 id 序「先到先得」：首个占用某导出名的模块留在 concat，之后与之冲突的降级为独立
            // 注册模块（有自己的 exports 对象）。保留尽可能多的 scope-hoist 收益。
            {
                let mut claimed: FxHashSet<String> = FxHashSet::default();
                for (id, body) in &filtered {
                    if *id == entry_id || standalone.contains(id) {
                        continue;
                    }
                    let names = exported_names(body);
                    if names.iter().any(|n| claimed.contains(n)) {
                        standalone.insert(*id);
                    } else {
                        claimed.extend(names);
                    }
                }
            }

            // 真正并入 concat 的模块 id（供 `_r` 垫片判定）。
            let concat_member_ids: Vec<u32> = filtered
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| *id != entry_id && !standalone.contains(id))
                .collect();

            // `_r` 垫片：并入 concat 的模块共享 `$`，其余 id **转发真实 require**
            // （独立模块有自己的 module.exports，不能返回 `$`）。
            {
                let set = concat_member_ids
                    .iter()
                    .map(|i| format!("{i}:1"))
                    .collect::<Vec<_>>()
                    .join(",");
                concat_body.push_str(&format!(
                    "{require}=function(_o){{var _m={{{set}}};return function(i){{return _m[i]?{exports}:_o(i)}}}}({require});", require = runtime_names.require, exports = runtime_names.exports,
                ));
            }

            for (id, body) in &filtered {
                if *id == entry_id || standalone.contains(id) {
                    continue;
                }
                let b = strip_standalone_requires(&compact_body_names(body, &runtime_names));
                // 块安全（ESM 且无 `var`/`this`）+ 整组 strict → 用裸 `{}` 块（strict 下块级函数声明块作用域，
                // let/const 本就块作用域 → 顶层名不跨块碰撞）。否则用 IIFE 建立真正函数作用域隔离
                // （`var` 会 hoist 出块、sloppy 下块级函数亦 hoist；曾致 React Symbol 覆盖 scheduler 计数器）。
                let has_scope_binding = ["const ", "let ", "var ", "class ", "function "]
                    .iter()
                    .any(|token| b.contains(token));
                let block_safe = all_esm && block_infos.get(id).is_some_and(|bi| bi.block_safe);
                if !has_scope_binding {
                    // 经过 tree shaking 后只剩表达式的模块不再需要隔离作用域。
                    concat_body.push_str(&b);
                } else if block_safe {
                    concat_body.push('{');
                    concat_body.push_str(&b);
                    concat_body.push('}');
                } else {
                    concat_body.push_str("(function(){");
                    concat_body.push_str(&b);
                    concat_body.push_str("})();");
                }
            }

            // 收集所有被合并到 concat 模块的原始模块 ID（非入口 + 非 stub + 非独立 CJS）
            let merged_ids: FxHashSet<u32> = filtered
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| *id != entry_id && !ref_ids.contains(id) && !standalone.contains(id))
                .collect();

            // 把对已合并模块的 `_r(M)` 重定向到 concat 模块（独立模块保持自己的 id）。
            let redirect = |body: &str| -> String {
                let mut nb = compact_body_names(body, &runtime_names);
                for &mid in &merged_ids {
                    nb = nb.replace(
                        &format!("{}({mid})", runtime_names.require),
                        &format!("{}({concat_id})", runtime_names.require),
                    );
                }
                nb
            };

            // 构建最终模块表：入口 + stubs + 独立 CJS 模块 + 合并模块
            for (id, body) in &filtered {
                if *id == entry_id {
                    final_modules.push((entry_id, redirect(body)));
                    break;
                }
            }
            for &sid in &ref_ids {
                final_modules.push((sid, String::new()));
            }
            for (id, body) in &filtered {
                if standalone.contains(id) {
                    final_modules.push((*id, redirect(body)));
                }
            }
            final_modules.push((concat_id, concat_body));
        } else {
            // async 子图存在 → 逐模块注册（`_r` 返回 Promise，由导入点 `await`）。
            for (id, body) in &filtered {
                final_modules.push((*id, compact_body_names(body, &runtime_names)));
            }
            for &sid in &ref_ids {
                if !final_modules.iter().any(|(i, _)| *i == sid) {
                    final_modules.push((sid, String::new()));
                }
            }
        }
        // 空 stub 的 require 语义只是“创建并缓存独立的空 exports”。运行时对缺省表项执行
        // no-op 即可保持该语义，无需为每个 id 输出重复的 `function(){}`。
        final_modules.retain(|(_, body)| !body.is_empty());
        final_modules.sort_by_key(|(id, _)| *id);

        let mut out = String::new();

        let final_bodies: Vec<&str> = final_modules.iter().map(|(_, b)| b.as_str()).collect();
        let needs_interop_default = final_bodies
            .iter()
            .any(|b| b.contains("__wake_interop_default"));
        let needs_interop_star = final_bodies
            .iter()
            .any(|b| b.contains("__wake_interop_star"));
        // minify 下省略 `__esModule` 标记，故不能用它区分「转译 ESM」与「纯 CJS」，改按
        // **是否存在 `default` 键**判定：转译 ESM 必定写了 `exports.default`，而
        // `module.exports = {…}` 的纯 CJS 通常没有。
        //
        // 不能简化为恒取 `m.default`：真 CJS 模块（如 `module.exports = api`）没有 `default`，
        // 取之得 `undefined`。这类模块现在会作为独立注册模块存在（见 `reassigns_module_exports`），
        // 简化版会让 `import pkg from 'cjs-pkg'` 直接拿到 undefined。
        let interop_default = if no_esmodule {
            "function __wake_interop_default(m){return m&&(typeof m==='object'||typeof m==='function')&&'default' in m?m.default:m}"
        } else {
            "function __wake_interop_default(m){return m&&m.__esModule?m.default:m}"
        };
        let interop_star = if no_esmodule {
            "function __wake_interop_star(m){return m}"
        } else {
            "function __wake_interop_star(m){if(m&&m.__esModule)return m;var ns={};if(m!=null){for(var k in m)if(Object.prototype.hasOwnProperty.call(m,k)&&k!='default')ns[k]=m[k]}return ns}"
        };
        // async 变体：async 模块的包装器返回 Promise → 缓存并返回它，导入方 `await` 得到最终 exports。
        out.push_str(if async_ids.is_empty() {
            "(function(g){var c={},q;function r(i){var x=c[i];if(x)return x.exports;var m={exports:{}};c[i]=m;var f=t[i];f&&f.call(m.exports,m,m.exports,r);return m.exports}"
        } else {
            "(function(g){var c={},q;function r(i){var x=c[i];if(x)return x.p||x.exports;var m={exports:{}};c[i]=m;var f=t[i],p=f&&f.call(m.exports,m,m.exports,r);if(p&&typeof p.then==='function')return m.p=p.then(function(){return m.exports});return m.exports}"
        });

        append_inline_regs(&mut out, &inline_regs);

        if needs_interop_default {
            out.push_str(interop_default);
        }
        if needs_interop_star {
            out.push_str(interop_star);
        }
        out.push_str("var t={");
        for (id, body) in &final_modules {
            if body.is_empty() {
                out.push_str(&format!("{}:function(){{}},", id));
            } else {
                let kw = if async_ids.contains(id) {
                    "async function"
                } else {
                    "function"
                };
                out.push_str(&format!(
                    "{}:{kw}({},{},{}){{",
                    id, runtime_names.module, runtime_names.exports, runtime_names.require
                ));
                out.push_str(body);
                out.push_str("},");
            }
        }
        out.push_str("};r.m=t;r.c=c;");
        out.push_str(&format!("var e=r({});", entry_id));
        out.push_str("if(typeof module!=='undefined'&&module.exports)module.exports=e;else g.__wake_entry__=e;return e;})(typeof globalThis!=='undefined'?globalThis:this);");
        out
    } else {
        // 非 minify 模式：不 do scope hoisting（无 tree-shaking，所有模块都有 exports/requires）。
        let filtered: Vec<(u32, String)> = bodies
            .iter()
            .map(|(id, body)| (*id, (*body).to_string()))
            .collect();

        let mut out = String::new();
        out.push_str(if async_ids.is_empty() {
            PRELUDE
        } else {
            PRELUDE_ASYNC
        });
        out.push_str("var __wake_modules__ = {\n");
        // SourceMap：逐模块记录其体在 bundle 中的起始行，供调用方平移模块内局部映射。
        // 本分支模块体**逐行原样**拼接（仅前缀 2 空格），故映射平移是 (行 +offset, 列 +2)。
        let mut starts: Vec<(u32, u32)> = Vec::with_capacity(filtered.len());
        let mut line = count_lines(&out);
        for (id, body) in &filtered {
            // async 子图成员的包装器必须是 `async function`：其体内静态导入点被写成
            // `(await __wake_require__(id))`。
            let kw = if async_ids.contains(id) {
                "async function"
            } else {
                "function"
            };
            let head = format!("{id}: {kw}(module, exports, __wake_require__) {{\n");
            out.push_str(&head);
            line += 1; // 包装头占 1 行
            starts.push((*id, line));
            for l in body.lines() {
                out.push_str("  ");
                out.push_str(l);
                out.push('\n');
                line += 1;
            }
            out.push_str("},\n");
            line += 1;
        }
        out.push_str(
            "};\n__wake_require__.m = __wake_modules__;\n__wake_require__.c = __wake_cache__;\n",
        );
        out.push_str(&format!(
            "var __wake_entry__ = __wake_require__({entry_id});\n"
        ));
        out.push_str(POSTLUDE);
        if let Some(slot) = body_starts {
            *slot = starts;
        }
        out
    }
}

/// 统计字符串中的换行数（= 其后续内容所处的 0 基行号）。
fn count_lines(s: &str) -> u32 {
    s.as_bytes().iter().filter(|&&b| b == b'\n').count() as u32
}

/// 模块路径 → SourceMap `sources` 条目名。
///
/// 三步规整（对齐 esbuild/Vite 的产出）：① 去掉 Windows 的 `\\?\` 扩展长度前缀——它会
/// 泄漏成 `//?/C:/…` 且让 DevTools 无法定位；② 尽量取相对 `cwd` 的路径，避免把构建机的
/// 目录结构写死进 map；③ 统一用正斜杠（sourcemap 规范的路径分隔符）。
pub(crate) fn map_source_name(path: &Path, cwd: Option<&Path>) -> String {
    /// 先转成 SourceMap 使用的 `/`，再去掉 Windows verbatim 前缀。
    ///
    /// 这里刻意不借助 [`Path::components`]：在 Unix 上，`C:\foo` 的反斜杠只是普通字符，
    /// 用宿主路径语义处理其它平台的路径会导致相对化结果随 CI runner 改变。
    fn normalize(p: &Path) -> String {
        path_to_slash(p)
    }

    /// 仅在完整路径段边界上相对化，避免 `/proj` 误匹配 `/project`。
    fn strip_base<'a>(path: &'a str, base: &str) -> Option<&'a str> {
        let base = if base == "/" {
            base
        } else {
            base.trim_end_matches('/')
        };
        if base.is_empty() {
            return None;
        }
        if path == base {
            return Some("");
        }
        if base == "/" {
            return path.strip_prefix('/');
        }
        path.strip_prefix(base)?.strip_prefix('/')
    }

    let clean = normalize(path);
    cwd.map(normalize)
        .and_then(|base| strip_base(&clean, &base).map(str::to_string))
        .unwrap_or(clean)
}

/// 序列化 [`SourceMap`] 为 V3 JSON：把各映射的**源字节偏移**换算为 0 基行 + UTF-16 列。
///
/// 每个源文件建一次换行表（[`SourceFile`]）即可对该文件的全部映射做 O(log n) 二分，
/// 避免逐条映射重复扫描源文本。缺源文本的模块无法换算，其映射被丢弃（宁缺毋错）。
fn serialize_map(sm: &SourceMap, sources: &FxHashMap<u32, (String, Option<String>)>) -> String {
    // sm.sources 的下标序 = merge_bundle_map 的登记序；按同序建换行表。
    let by_name: FxHashMap<&str, &str> = sources
        .values()
        .filter_map(|(n, c)| c.as_deref().map(|c| (n.as_str(), c)))
        .collect();
    let files: Vec<Option<SourceFile>> = sm
        .sources
        .iter()
        .map(|n| by_name.get(n.as_str()).map(|c| SourceFile::new(n, *c)))
        .collect();
    sm.to_json(|src_index, offset| {
        files
            .get(src_index as usize)
            .and_then(|f| f.as_ref())
            .map_or((0, 0), |f| f.location0_utf16(offset))
    })
}

/// 把各模块的局部映射按 bundle 行偏移平移、合并为整包 [`SourceMap`]。
///
/// `body_starts`：模块 id → 其体首行在 bundle 中的 0 基行号（由 [`emit`] 采集）。
/// `sources`：模块 id → (源文件名, 源文本)。缺失源文本的模块（缓存命中未 parse）以 `None` 带出，
/// 浏览器会退回按 `sources` 路径自行拉取。
///
/// 平移规则来自 emit 的非 minify 拼接：行 `+start`、列 `+2`（每行固定 2 空格缩进）。
fn merge_bundle_map(
    body_starts: &[(u32, u32)],
    module_maps: &FxHashMap<u32, Arc<ModuleMappings>>,
    sources: &FxHashMap<u32, (String, Option<String>)>,
    file: Option<String>,
) -> SourceMap {
    let mut sm = SourceMap {
        file,
        ..SourceMap::new()
    };
    // 源文件按模块 id 升序登记，保证产物稳定（同输入 → 同 map）。
    let mut ids: Vec<u32> = body_starts.iter().map(|(id, _)| *id).collect();
    ids.sort_unstable();
    let mut src_index: FxHashMap<u32, u32> = FxHashMap::default();
    for id in &ids {
        if let Some((name, content)) = sources.get(id) {
            let idx = sm.add_source(name.clone(), content.clone());
            src_index.insert(*id, idx);
        }
    }
    for (id, start) in body_starts {
        let (Some(mm), Some(&si)) = (module_maps.get(id), src_index.get(id)) else {
            continue;
        };
        for m in &mm.mappings {
            sm.mappings.push(Mapping {
                gen_line: m.gen_line + start,
                gen_col: m.gen_col + 2, // 每行前缀 2 空格缩进
                src_index: si,
                src_offset: m.src_offset,
            });
        }
    }
    sm.mappings.sort_by_key(|m| (m.gen_line, m.gen_col));
    sm
}

// ======================================================================
// 代码分割 emit（多产物，DESIGN §6.3 / PLAN §6.5）
// ======================================================================

/// 顶层 await 的 **async 子图**：自身含顶层 await 的模块，加上（传递地）**静态 ESM 导入**了
/// 这类模块的模块。
///
/// 静态导入点由 codegen 写成 `(await __wake_require__(id))`，故导入方本身也必须是 `async function`
/// ——这就是传染的来源，与 esbuild/Rollup 的做法一致。两类边**不**传染：
/// - 动态 `import()`：本就产出 Promise，`Promise.resolve(...)` 会把 async 模块的 Promise 展平；
/// - CJS `require()`：调用点可能嵌在普通函数体内，插不进 `await`（同步 require 一个 async 模块
///   在任何打包器里都无解，见 `docs/TS-SYNTAX-SUPPORT.md` §11）。
fn async_module_ids(modules: &FxHashMap<u32, ModuleRec>) -> FxHashSet<u32> {
    // 反向边：被导入者 → 静态 ESM 导入它的模块。
    let mut importers: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
    for (&id, rec) in modules {
        let spec2id: FxHashMap<&str, u32> =
            rec.dep_ids.iter().map(|(s, i)| (s.as_str(), *i)).collect();
        for dep in rec.deps.iter() {
            if !matches!(
                dep.kind,
                DependencyKind::Import | DependencyKind::ExportFrom
            ) {
                continue;
            }
            if let Some(&tid) = spec2id.get(dep.specifier.as_str()) {
                importers.entry(tid).or_default().push(id);
            }
        }
    }
    let mut set: FxHashSet<u32> = FxHashSet::default();
    let mut stack: Vec<u32> = Vec::new();
    for (&id, rec) in modules {
        if rec.has_top_level_await && set.insert(id) {
            stack.push(id);
        }
    }
    while let Some(id) = stack.pop() {
        let Some(list) = importers.get(&id) else {
            continue;
        };
        for &imp in list {
            if set.insert(imp) {
                stack.push(imp);
            }
        }
    }
    set
}

/// 从模块记录提取 chunk 划分所需的依赖边。
fn build_module_edges(modules: &FxHashMap<u32, ModuleRec>) -> FxHashMap<u32, ModuleEdges> {
    let mut edges = FxHashMap::default();
    for (&id, rec) in modules {
        let spec2id: FxHashMap<&str, u32> =
            rec.dep_ids.iter().map(|(s, i)| (s.as_str(), *i)).collect();
        let mut st = Vec::new();
        let mut dy = Vec::new();
        for dep in rec.deps.iter() {
            if let Some(&tid) = spec2id.get(dep.specifier.as_str()) {
                if dep.kind == DependencyKind::DynamicImport {
                    dy.push(tid);
                } else {
                    st.push(tid);
                }
            }
        }
        st.sort_unstable();
        st.dedup();
        dy.sort_unstable();
        dy.dedup();
        let stem = rec
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("chunk")
            .to_string();
        edges.insert(
            id,
            ModuleEdges {
                static_targets: st,
                dyn_targets: dy,
                stem,
            },
        );
    }
    edges
}

/// 本模块每个「跨 chunk」动态 import 的 (说明符, 目标 chunk id)（排序去重；目标落 entry=0 者不入表）。
fn dyn_chunks_of(rec: &ModuleRec, chunk_graph: Option<&ChunkGraph>) -> Vec<(String, u32)> {
    let Some(g) = chunk_graph else {
        return Vec::new();
    };
    let spec2id: FxHashMap<&str, u32> = rec.dep_ids.iter().map(|(s, i)| (s.as_str(), *i)).collect();
    let mut v: Vec<(String, u32)> = rec
        .deps
        .iter()
        .filter(|d| d.kind == DependencyKind::DynamicImport)
        .filter_map(|d| {
            let tid = *spec2id.get(d.specifier.as_str())?;
            let c = *g.module_chunk.get(&tid)?;
            (c != 0).then_some((d.specifier.clone(), c))
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// 渲染一组模块为 `<id>: function(module, exports, __wake_require__) { <body> },` 条目。
/// `async_ids` 中的模块（顶层 await 子图）改用 `async function`。
fn render_module_entries(
    module_ids: &[u32],
    body_of: &FxHashMap<u32, &Arc<String>>,
    async_ids: &FxHashSet<u32>,
) -> String {
    let mut out = String::new();
    for &id in module_ids {
        if let Some(body) = body_of.get(&id) {
            let kw = if async_ids.contains(&id) {
                "async function"
            } else {
                "function"
            };
            out.push_str(&format!(
                "{id}: {kw}(module, exports, __wake_require__) {{\n"
            ));
            for line in body.lines() {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("},\n");
        }
    }
    out
}

/// 多产物 emit：先渲染并 hash 非 entry chunk（体内无文件名 → hash 无环），再渲染内嵌 f/d 映射的 entry。
fn emit_chunks(
    bodies: &[(u32, Arc<String>)],
    g: &ChunkGraph,
    entry_id: u32,
    token: &str,
    public_path: &str,
    hashed: bool,
    async_ids: &FxHashSet<u32>,
) -> (Vec<OutputChunk>, usize) {
    let body_of: FxHashMap<u32, &Arc<String>> = bodies.iter().map(|(id, b)| (*id, b)).collect();

    // 1. 非 entry chunk 先行渲染 + hash（chunk 间只引用数字 id，互相独立）。
    let mut file_of: BTreeMap<u32, String> = BTreeMap::new();
    let mut nonentry: Vec<OutputChunk> = Vec::new();
    for plan in &g.chunks {
        if plan.id == 0 {
            continue;
        }
        let entries = render_module_entries(&plan.modules, &body_of, async_ids);
        let code = render_async_chunk(token, plan.id, &entries);
        let file = chunk_filename(&plan.name, &code, hashed);
        file_of.insert(plan.id, file.clone());
        nonentry.push(OutputChunk {
            name: plan.name.clone(),
            file_name: file,
            code,
            kind: plan.kind,
            is_entry: false,
            chunk_id: plan.id,
            module_ids: plan.modules.clone(),
            imports: Vec::new(), // 回填于下
            source_map: None,    // 代码分割路径暂不产 map（M4d 首期只覆盖单包）
        });
    }
    // 回填非 entry chunk 的静态依赖文件名。
    for c in &mut nonentry {
        if let Some(deps) = g.chunk_deps.get(&c.chunk_id) {
            c.imports = deps
                .iter()
                .filter_map(|d| file_of.get(d).cloned())
                .collect();
        }
    }

    // 2. 渲染 entry chunk（内嵌 f/d 映射，引用非 entry 的文件名）。
    let entry_plan = g.chunks.iter().find(|c| c.id == 0).expect("entry chunk");
    let entries = render_module_entries(&entry_plan.modules, &body_of, async_ids);
    let f_map = json_file_map(&file_of);
    let d_map = json_deps_map(&g.chunk_deps);
    let code = render_entry_chunk(token, entry_id, public_path, &f_map, &d_map, &entries);
    let file = chunk_filename(&entry_plan.name, &code, hashed);
    let entry = OutputChunk {
        name: entry_plan.name.clone(),
        file_name: file,
        code,
        kind: ChunkKind::Initial,
        is_entry: true,
        chunk_id: 0,
        module_ids: entry_plan.modules.clone(),
        imports: Vec::new(),
        source_map: None, // 同上：代码分割路径暂不产 map
    };

    // 3. 按 chunk id 升序组装。
    let mut chunks = vec![entry];
    chunks.extend(nonentry);
    chunks.sort_by_key(|c| c.chunk_id);
    let entry_chunk = chunks.iter().position(|c| c.chunk_id == 0).unwrap();
    (chunks, entry_chunk)
}

/// entry chunk：全局 registry bootstrap + publicPath + f/d 映射 + register 模块 + 运行入口 + 导出。
fn render_entry_chunk(
    token: &str,
    entry_id: u32,
    public_path: &str,
    f_map: &str,
    d_map: &str,
    entries: &str,
) -> String {
    let mut out = RUNTIME_ENTRY_PRELUDE.replace("__WAKE_NS__", token);
    // 配置的 `publicPath` 注入运行时（`loadFile` 用它拼 chunk URL）：子路径部署下动态 import()
    // 才不会按当前页面 URL 相对解析而 404。写在 prelude 之后而非其对象字面量里——registry 可能
    // 已由同 token 的先前加载建好（`g.__WAKE_NS__ || (...)`），字面量那次不会再跑。
    out.push_str("__wake__.publicPath = ");
    push_js_string(&mut out, public_path);
    out.push_str(";\n");
    out.push_str(&format!("__wake__.f = {f_map};\n"));
    out.push_str(&format!("Object.assign(__wake__.d, {d_map});\n"));
    out.push_str("__wake__.markLoaded(0);\n");
    out.push_str("__wake__.register({\n");
    out.push_str(entries);
    out.push_str("});\n");
    out.push_str(&format!(
        "var __wake_entry__ = __wake__.require({entry_id});\n"
    ));
    out.push_str(
        "if (typeof module !== \"undefined\" && module.exports) module.exports = __wake_entry__;\n",
    );
    out.push_str("else g.__wake_entry__ = __wake_entry__;\n");
    out.push_str("})();\n");
    out
}

/// async/shared chunk：接入已建 registry + register 模块 + markLoaded。
fn render_async_chunk(token: &str, this_chunk_id: u32, entries: &str) -> String {
    let mut out = RUNTIME_ASYNC_PRELUDE.replace("__WAKE_NS__", token);
    out.push_str("__wake__.register({\n");
    out.push_str(entries);
    out.push_str("});\n");
    out.push_str(&format!("__wake__.markLoaded({this_chunk_id});\n"));
    out.push_str("})();\n");
    out
}

/// `{ 1: "lazy.abcd.js", 2: "shared.efgh.js" }`（chunk id 升序）。
fn json_file_map(file_of: &BTreeMap<u32, String>) -> String {
    let mut s = String::from("{");
    for (i, (cid, file)) in file_of.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(" {cid}: {file:?}"));
    }
    s.push_str(" }");
    s
}

/// `{ 2: [1], 3: [1] }`（chunk id 升序，仅含非空依赖的非 entry chunk）。
fn json_deps_map(chunk_deps: &FxHashMap<u32, Vec<u32>>) -> String {
    let sorted: BTreeMap<u32, &Vec<u32>> = chunk_deps
        .iter()
        .filter(|(cid, deps)| **cid != 0 && !deps.is_empty())
        .map(|(c, d)| (*c, d))
        .collect();
    let mut s = String::from("{");
    for (i, (cid, deps)) in sorted.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let arr: Vec<String> = deps.iter().map(|d| d.to_string()).collect();
        s.push_str(&format!(" {cid}: [{}]", arr.join(", ")));
    }
    s.push_str(" }");
    s
}

/// 内容 hash 文件名 `[name].[hash8].js`（关闭 hash 则 `[name].js`）。
fn chunk_filename(name: &str, code: &str, hashed: bool) -> String {
    if hashed {
        format!("{name}.{}.js", hash8(code))
    } else {
        format!("{name}.js")
    }
}

/// 内容 hash：xxh3 低 32 位 → 8 hex。
fn hash8(s: &str) -> String {
    format!("{:08x}", xxhash_rust::xxh3::xxh3_64(s.as_bytes()) as u32)
}

/// 构建命名空间 token（隔离同进程多 bundle 的全局 registry）：`__wake_<hash8(入口路径#模块数)>__`。
fn build_token(entry_norm: &Path, n: usize) -> String {
    let mut s = path_to_slash(entry_norm);
    s.push('#');
    s.push_str(&n.to_string());
    format!("__wake_{}__", hash8(&s))
}

/// entry chunk 运行时前半（`__WAKE_NS__` 为 emit 期替换的构建 token）。开放函数体，由 tail 收尾。
const RUNTIME_ENTRY_PRELUDE: &str = r#"(function () {
"use strict";
var g = typeof globalThis !== "undefined" ? globalThis
      : typeof self !== "undefined" ? self
      : typeof window !== "undefined" ? window : this;
var __wake__ = g.__WAKE_NS__ || (g.__WAKE_NS__ = (function () {
  var modules = {}, cache = {}, chunkPromises = {};
  var W = { m: modules, c: cache, p: chunkPromises, f: {}, d: {},
            publicPath: "", nreq: null, ndir: ".", npath: null };
  function require(id) {
    var hit = cache[id];
    if (hit) return hit.p || hit.exports;
    var module = { exports: {} };
    cache[id] = module;
    var fac = modules[id];
    if (!fac) throw new Error("wake: module " + id + " not registered");
    var r = fac.call(module.exports, module, module.exports, require);
    // 顶层 await：async 模块的工厂返回 Promise → 缓存并返回它，导入方 await 后得到最终 exports。
    if (r && typeof r.then === "function") {
      module.p = r.then(function () { return module.exports; });
      return module.p;
    }
    return module.exports;
  }
  function interopDefault(m) { return m && m.__esModule ? m.default : m; }
  function interopStar(m) {
    if (m && m.__esModule) return m;
    var ns = {};
    if (m != null) for (var k in m)
      if (Object.prototype.hasOwnProperty.call(m, k) && k !== "default") ns[k] = m[k];
    ns.default = m;
    return ns;
  }
  function register(mods) { for (var k in mods) if (!modules[k]) modules[k] = mods[k]; }
  function markLoaded(cid) { if (!chunkPromises[cid]) chunkPromises[cid] = Promise.resolve(); }
  function loadFile(file) {
    if (W.nreq) {
      return new Promise(function (res, rej) {
        try { W.nreq(W.npath.resolve(W.ndir, file)); res(); } catch (e) { rej(e); }
      });
    }
    return new Promise(function (res, rej) {
      var s = document.createElement("script");
      s.src = W.publicPath + file; s.async = true;
      s.onload = function () { res(); };
      s.onerror = function () { rej(new Error("wake: failed to load chunk " + file)); };
      (document.head || document.getElementsByTagName("head")[0] || document.documentElement).appendChild(s);
    });
  }
  function ensure(cid) {
    if (chunkPromises[cid]) return chunkPromises[cid];
    var deps = W.d[cid] || [];
    var p = Promise.all(deps.map(ensure)).then(function () {
      var file = W.f[cid];
      if (file == null) throw new Error("wake: unknown chunk " + cid);
      return loadFile(file);
    });
    chunkPromises[cid] = p;
    p.catch(function () { if (chunkPromises[cid] === p) delete chunkPromises[cid]; });
    return p;
  }
  // async 模块的 require 返回 Promise，需先解包再取命名空间（同步模块保持原时序）。
  function nsOf(m) { return m && typeof m.then === "function" ? m.then(interopStar) : interopStar(m); }
  function dynImport(cid, id) {
    if (cid == null) return Promise.resolve(nsOf(require(id)));
    return ensure(cid).then(function () { return nsOf(require(id)); });
  }
  require.import = dynImport;
  W.require = require; W.register = register; W.markLoaded = markLoaded;
  W.ensure = ensure; W.interopDefault = interopDefault; W.interopStar = interopStar;
  return W;
})());
if (typeof process !== "undefined" && process.versions && process.versions.node && typeof require !== "undefined") {
  __wake__.nreq = require;
  __wake__.ndir = (typeof __dirname !== "undefined") ? __dirname : ".";
  __wake__.npath = require("path");
}
var __wake_require__ = __wake__.require;
__wake_require__.metaUrl = function () {
  return typeof document !== "undefined" ? new URL(__wake__.publicPath || ".", document.baseURI).href : "";
};
var __wake_interop_default = __wake__.interopDefault;
var __wake_interop_star = __wake__.interopStar;
"#;

/// async/shared chunk 运行时前半（接入已建 registry）。开放函数体，由 render 收尾。
const RUNTIME_ASYNC_PRELUDE: &str = r#"(function () {
"use strict";
var g = typeof globalThis !== "undefined" ? globalThis
      : typeof self !== "undefined" ? self
      : typeof window !== "undefined" ? window : this;
var __wake__ = g.__WAKE_NS__;
if (!__wake__) throw new Error("wake: runtime not initialized (entry chunk must load first)");
var __wake_require__ = __wake__.require;
var __wake_interop_default = __wake__.interopDefault;
var __wake_interop_star = __wake__.interopStar;
"#;

/// 分配/复用模块 id（无 worklist；纯 id 记账）。
fn assign_id(path_to_id: &mut FxHashMap<PathBuf, u32>, next_id: &mut u32, path: PathBuf) -> u32 {
    if let Some(&id) = path_to_id.get(&path) {
        return id;
    }
    let id = *next_id;
    *next_id += 1;
    path_to_id.insert(path, id);
    id
}
