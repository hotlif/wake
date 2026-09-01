//! # 增量 + 并行打包（引擎接入，PLAN §3.2 / DESIGN §10.3、§10.6）
//!
//! 把 `parse`、`optimize`、`emit_body` 与 `source_map_facts` 接入 wake_turbo 引擎，实现：
//! - **第二遍构建全管线缓存命中**（内容/依赖未变 → 各阶段浅绿命中，零重执行）；
//! - **Scan 全并行**（DESIGN §10.6）：**分层 BFS**，每层用工作窃取执行器 `par_request` 并行 parse；
//!   optimize/body 阶段同样并行。
//!
//! ## cycle-safe 说明
//!
//! 任务按 parse → optimize → body → optional map 单向依赖，**无环**。模块*依赖*图的循环由
//! 驱动层 BFS 的逻辑模块身份去重集合处理，
//! 不会造成任务图环 / single-flight 死锁。故并行 scan **不需要 SCC 成组**（那是「full scan-as-tasks」
//! 递归请求路线才需要的，此处务实绕开）。
//!
//! ## 缓存与并行的确定性
//!
//! id 分配走确定性 BFS（层序 + 依赖序），`par_request` 保序返回 → 两遍相同构建 id 相同 →
//! linker cell 稳定 → 缓存命中不受并行影响。

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_utils::CachePadded;
use wake_cache::{
    BuildCache, CachedDep, CachedDiscardedStaticRequest, CachedLiveness, CachedMapping,
    CachedModuleMappings, CachedNamedImport, CachedUse, FileStamp, ModuleSummary,
};
use wake_common::{
    Diagnostic, FileSystem, FxHashMap, FxHashSet, Interner, SourceFile, Span, fs::normalize,
};
use wake_ecma_ast::{
    DependencyKind, Expression, MemberProperty, ModuleAst, Program, SourceType, Statement,
    UnaryOperator,
};
use wake_ecma_codegen::{
    GeneratedDiscardedStaticRequest, Mapping, ModuleMappings, SourceMap,
    codegen_optimized_with_map_and_requests,
};
use wake_ecma_minify::{
    ConstVal, LinkerExportLiveness, NodeOrigin, OptimizeDependency, OptimizeInput,
    OptimizedProgram, PIPELINE_VERSION, SyntheticReason, TrustedExpression, TrustedExpressionEdit,
    ValidatedDefine, optimize, optimize_one_shot,
};
use wake_ecma_parser::{parse, parse_with};
use wake_ecma_transform::{FeatureSet, TargetEnv};
use wake_graph::{
    ImportUse, LiveResult, ModuleLiveness, NamedImport, collect_module_liveness,
    collect_static_uses, compute_live_keep,
};
use wake_resolver::{
    ModuleIdentity, ResolutionEnvironment, ResolutionProfile, ResolveOptions, ResolvedModule,
    Resolver,
};
use wake_turbo::{Engine, Executor, TaskArg, TaskId, Vc, global_executor, query};
use xxhash_rust::xxh3::xxh3_64_with_seed;

use crate::chunk::{ChunkGraph, ModuleEdges, compute_chunk_graph};
use crate::concat::{ConcatBlockInfo, scan_concat_block_info};
use crate::loader::{
    LoadOptions, Loaded, cached_source_type, is_asset_path, load_source, push_js_string,
};
use crate::{
    BuildOutput, BuildPlatform, ChunkKind, Linker, ModuleFormat, OutputAsset, OutputChunk,
    POSTLUDE, POSTLUDE_COMMONJS, PRELUDE, PRELUDE_ASYNC, SpecifierLookup, path_to_slash,
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
enum ResolveResult {
    Internal {
        module: ResolvedModule,
        /// One-shot cold builds may prove an exact relative file while reading it. Carry that
        /// immutable loader result into the next BFS layer instead of reopening the same file.
        prefetched: Option<Arc<Loaded>>,
    },
    External(String),
    Error,
}
type CodegenExecCounts = Arc<[CachePadded<AtomicU64>]>;

/// I/O 小任务的目标批次数：既给工作窃取留出余量，又避免为数千个小文件逐个分配
/// `Job`、逐个经 channel 回传。最终顺序仍由调用方的索引槽位恢复。
const IO_BATCHES_PER_WORKER: usize = 4;
const CPU_BATCHES_PER_WORKER: usize = 32;
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

/// Return the literal candidate for the narrow resolve+load fusion fast path.
///
/// Relative specifiers are resolved before aliases in `wake_resolver`; requiring an explicit,
/// non-empty extension excludes extension completion and directory entry selection. A failed
/// read is never authoritative: the caller must fall back to the regular resolver so TS twins,
/// directory indexes and canonical diagnostics retain their existing behavior.
fn exact_relative_file_candidate(specifier: &str, from_dir: &Path) -> Option<PathBuf> {
    let path = Path::new(specifier);
    let explicit_relative = specifier.starts_with("./") || specifier.starts_with("../");
    (explicit_relative
        && path
            .extension()
            .is_some_and(|extension| !extension.is_empty()))
    .then(|| normalize(&from_dir.join(path)))
}

/// 小模块的 parse/codegen 本身很快，逐模块穿过 `Executor` 的 boxed job + mpsc 会放大固定开销。
/// 外层每个任务处理一个连续小批次；`Engine::enter` 仍覆盖批内全部 query，结果按批次与批内顺序展平。
fn par_request_batched<T, F>(engine: &Arc<Engine>, exec: &Executor, requests: Vec<F>) -> Vec<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let max_batches = exec.num_threads().saturating_mul(CPU_BATCHES_PER_WORKER);
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

#[cfg(test)]
mod exact_relative_prefetch_tests {
    use std::io;

    use wake_common::MemoryFileSystem;

    use super::*;

    #[derive(Default)]
    struct CountingFileSystem {
        inner: MemoryFileSystem,
        reads: Mutex<FxHashMap<PathBuf, usize>>,
        file_probes: Mutex<FxHashMap<PathBuf, usize>>,
    }

    impl CountingFileSystem {
        fn insert(&self, path: impl AsRef<Path>, contents: impl Into<Vec<u8>>) {
            self.inner.insert(path, contents);
        }

        fn reads(&self, path: &str) -> usize {
            self.reads
                .lock()
                .unwrap()
                .get(&normalize(Path::new(path)))
                .copied()
                .unwrap_or(0)
        }

        fn file_probes(&self, path: &str) -> usize {
            self.file_probes
                .lock()
                .unwrap()
                .get(&normalize(Path::new(path)))
                .copied()
                .unwrap_or(0)
        }

        fn record(counter: &Mutex<FxHashMap<PathBuf, usize>>, path: &Path) {
            *counter.lock().unwrap().entry(normalize(path)).or_default() += 1;
        }
    }

    impl FileSystem for CountingFileSystem {
        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            Self::record(&self.reads, path);
            self.inner.read_to_string(path)
        }

        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            Self::record(&self.reads, path);
            self.inner.read(path)
        }

        fn exists(&self, path: &Path) -> bool {
            self.inner.exists(path)
        }

        fn is_file(&self, path: &Path) -> bool {
            Self::record(&self.file_probes, path);
            self.inner.is_file(path)
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.inner.is_dir(path)
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            self.inner.read_dir(path)
        }
    }

    fn exact_fixture() -> Arc<CountingFileSystem> {
        let fs = Arc::new(CountingFileSystem::default());
        fs.insert(
            "src/index.js",
            "import { value } from './dep.js'; export { value };",
        );
        fs.insert("src/dep.js", "export const value = 1;");
        fs
    }

    #[test]
    fn exact_relative_candidate_has_the_proven_narrow_shape() {
        assert_eq!(
            exact_relative_file_candidate("./dep.js", Path::new("src/nested")),
            Some(PathBuf::from("src/nested/dep.js"))
        );
        assert_eq!(
            exact_relative_file_candidate("../dep.ts", Path::new("src/nested")),
            Some(PathBuf::from("src/dep.ts"))
        );
        for specifier in ["dep.js", "./dep", "../dep", "/dep.js", "./.js"] {
            assert_eq!(
                exact_relative_file_candidate(specifier, Path::new("src")),
                None,
                "{specifier} must use the canonical resolver"
            );
        }
    }

    #[test]
    fn one_shot_exact_relative_prefetch_reads_the_dependency_once_without_stat() {
        let fs = exact_fixture();
        let mut bundler = IncrementalBundler::new_one_shot(fs.clone());
        let output = bundler.build(Path::new("src/index.js"));

        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        assert_eq!(output.module_count, 2);
        assert_eq!(fs.reads("src/dep.js"), 1);
        assert_eq!(fs.file_probes("src/dep.js"), 0);
        assert_eq!(bundler.load_exec_count(), 2);
    }

    #[test]
    fn duplicate_exact_edges_share_one_normalized_candidate_load() {
        let fs = Arc::new(CountingFileSystem::default());
        fs.insert(
            "src/index.js",
            "import { value as first } from './dep.js';\
             import { value as second } from './nested/../dep.js';\
             export const total = first + second;",
        );
        fs.insert("src/dep.js", "export const value = 1;");
        let mut bundler = IncrementalBundler::new_one_shot(fs.clone());
        let output = bundler.build(Path::new("src/index.js"));

        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        assert_eq!(output.module_count, 2);
        assert_eq!(fs.reads("src/dep.js"), 1);
        assert_eq!(fs.file_probes("src/dep.js"), 0);
        assert_eq!(bundler.load_exec_count(), 2);
        assert_eq!(bundler.resolve_exec_count(), 2);
    }

    #[test]
    fn diamond_and_back_edge_share_successful_exact_loads_across_bfs_layers() {
        let mut baseline = None;
        for workers in [1, 4] {
            let fs = Arc::new(CountingFileSystem::default());
            fs.insert(
                "src/index.js",
                "import { left } from './left.js';\
                 import { right } from './right.js';\
                 export const total = left + right;",
            );
            fs.insert(
                "src/left.js",
                "import { shared } from './shared.js'; export const left = shared + 1;",
            );
            fs.insert(
                "src/right.js",
                "import { shared } from './shared.js'; export const right = shared + 2;",
            );
            fs.insert(
                "src/shared.js",
                "import './index.js'; export const shared = 10;",
            );
            let mut bundler = IncrementalBundler::new_one_shot(fs.clone());
            bundler.set_test_thread_count(workers);
            let output = bundler.build(Path::new("src/index.js"));

            assert!(!output.has_errors(), "{:?}", output.diagnostics);
            assert_eq!(output.module_count, 4);
            assert_eq!(fs.reads("src/index.js"), 1, "workers={workers}");
            assert_eq!(fs.reads("src/left.js"), 1, "workers={workers}");
            assert_eq!(fs.reads("src/right.js"), 1, "workers={workers}");
            assert_eq!(fs.reads("src/shared.js"), 1, "workers={workers}");
            assert_eq!(bundler.load_exec_count(), 4, "workers={workers}");
            assert_eq!(bundler.resolve_exec_count(), 5, "workers={workers}");
            if let Some(expected) = &baseline {
                assert_eq!(&output.bundle, expected, "workers={workers}");
            } else {
                baseline = Some(output.bundle);
            }
        }
    }

    #[test]
    fn non_one_shot_and_load_cached_builds_keep_the_canonical_resolver_path() {
        let regular_fs = exact_fixture();
        let mut regular = IncrementalBundler::new(regular_fs.clone());
        let regular_output = regular.build(Path::new("src/index.js"));
        assert!(
            !regular_output.has_errors(),
            "{:?}",
            regular_output.diagnostics
        );
        assert!(regular_fs.file_probes("src/dep.js") > 0);

        let cached_fs = exact_fixture();
        let mut cached = IncrementalBundler::new_one_shot(cached_fs.clone());
        cached.enable_load_cache();
        let cached_output = cached.build(Path::new("src/index.js"));
        assert!(
            !cached_output.has_errors(),
            "{:?}",
            cached_output.diagnostics
        );
        assert!(cached_fs.file_probes("src/dep.js") > 0);
    }

    #[test]
    fn failed_exact_prefetch_falls_back_to_the_typescript_twin() {
        let fs = Arc::new(CountingFileSystem::default());
        fs.insert(
            "src/index.js",
            "import { value } from './dep.js'; export { value };",
        );
        fs.insert("src/dep.ts", "export const value: number = 1;");
        let mut bundler = IncrementalBundler::new_one_shot(fs.clone());
        let output = bundler.build(Path::new("src/index.js"));

        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        assert_eq!(output.module_count, 2);
        assert_eq!(
            fs.reads("src/dep.js"),
            1,
            "the failed probe is attempted once"
        );
        assert_eq!(
            fs.reads("src/dep.ts"),
            1,
            "the resolved twin is loaded once"
        );
        assert!(fs.file_probes("src/dep.js") > 0);
    }

    #[test]
    fn failed_exact_prefetch_falls_back_to_directory_resolution() {
        let fs = Arc::new(CountingFileSystem::default());
        fs.insert("src/index.js", "export { value } from './dir.js';");
        fs.insert("src/dir.js/index.js", "export const value = 1;");
        let mut bundler = IncrementalBundler::new_one_shot(fs.clone());
        let output = bundler.build(Path::new("src/index.js"));

        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        assert_eq!(output.module_count, 2);
        assert_eq!(fs.reads("src/dir.js"), 1);
        assert_eq!(fs.reads("src/dir.js/index.js"), 1);
    }

    #[test]
    fn explicit_external_is_classified_before_prefetch() {
        let fs = exact_fixture();
        let mut bundler = IncrementalBundler::new_one_shot(fs.clone());
        bundler.set_external_packages(vec!["./dep.js".into()]);
        let output = bundler.build(Path::new("src/index.js"));

        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        assert_eq!(output.module_count, 1);
        assert_eq!(fs.reads("src/dep.js"), 0);
        assert_eq!(fs.file_probes("src/dep.js"), 0);
    }

    #[test]
    fn node_browser_resource_is_not_read_before_its_canonical_diagnostic() {
        let fs = Arc::new(CountingFileSystem::default());
        fs.insert("src/index.js", "import './style.css';");
        fs.insert("src/style.css", "body { color: red; }");
        let mut bundler = IncrementalBundler::new_one_shot(fs.clone());
        bundler
            .set_platform(BuildPlatform::Node)
            .set_module_format(ModuleFormat::CommonJs);
        let output = bundler.build(Path::new("src/index.js"));

        assert!(output.has_errors());
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("WAKE0303"))
        );
        assert_eq!(fs.reads("src/style.css"), 0);
        assert!(fs.file_probes("src/style.css") > 0);
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

#[derive(Clone)]
struct LinkPlan {
    fingerprint: u64,
    keep: FxHashMap<u32, Option<ExportKeep>>,
}

/// Stable linker facts crossing into one optimizer task. Declaration retention and public-name
/// observation are deliberately separate: a locally used export binding need not expose a getter.
#[derive(Clone, Hash, PartialEq, Eq, Default)]
struct ExportKeep {
    retained_export_names: Vec<String>,
    observed_export_names: Vec<String>,
    preserve_export_star: bool,
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

/// Optimizer-owned link facts. Final chunk ids are deliberately absent: retained dependency
/// convergence can therefore re-plan chunks without invalidating semantic optimization.
#[derive(Clone, Hash, PartialEq, Eq, Default)]
struct OptimizeLinkerData {
    /// Deterministic BFS module identity. Combined with parser-local `SymbolId` values only inside
    /// the codegen task; it is never persisted as a semantic identifier.
    module_id: u32,
    deps: DepIds,
    /// Source specifiers whose resolved internal target contains ESM syntax. The optimizer turns
    /// these stable link facts into parser-generation SymbolId replacements; SymbolIds themselves
    /// never enter this persisted/hashable boundary.
    internal_esm_deps: Vec<String>,
    export_keep: Option<ExportKeep>,
}

/// Final body-emission link facts, created only after optimizer-retained edges have converged and
/// the chunk graph has been recomputed from that converged graph.
#[derive(Clone, Hash, PartialEq, Eq, Default)]
struct EmitLinkerData {
    deps: DepIds,
    dyn_chunks: Vec<(String, u32)>,
    /// 本模块依赖里属于 **async 子图**（顶层 await 传染）的模块 id（升序去重）。
    /// codegen 据此把静态导入点写成 `(await __wake_require__(id))`。进指纹 → async 归属变化精确重跑。
    async_deps: Vec<u32>,
    /// 单包 minify 可省略 ESM marker；代码分割 runtime 必须保留 marker，才能可靠区分
    /// 转译 ESM 与含 `default` 自有字段的纯 CJS。字段进入 linker/body 缓存身份。
    no_esmodule: bool,
}

#[derive(Clone, Debug, Default)]
struct CssCodegenInput {
    /// Imported static values in stable key order. Empty with `seed=None` means that this
    /// module has no compiler marker and should skip the CSS transform entirely.
    scope: Vec<(String, wake_css_in_js::StaticValue)>,
    seed: Option<String>,
    inject_style: bool,
}

impl PartialEq for CssCodegenInput {
    fn eq(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.seed == other.seed
            && self.inject_style == other.inject_style
    }
}

impl Eq for CssCodegenInput {}

impl std::hash::Hash for CssCodegenInput {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.scope.hash(state);
        self.seed.hash(state);
        self.inject_style.hash(state);
    }
}

/// Every non-Vc semantic option captured by optimization. Source-map enablement is intentionally
/// absent because mapping collection is a downstream consumer of body mapping facts.
#[derive(Clone, Debug)]
struct OptimizeOptionsInput {
    define: Vec<(String, String)>,
    /// Parser-validated form of `define`. This is a deterministic function of `define`; custom
    /// equality/hash below deliberately use the raw strings as the stable Turbo identity.
    validated_define: Result<Vec<ValidatedDefine>, String>,
    minify: bool,
    drop_console: bool,
    drop_debugger: bool,
    module_name: String,
    one_shot: bool,
}

impl PartialEq for OptimizeOptionsInput {
    fn eq(&self, other: &Self) -> bool {
        self.define == other.define
            && self.minify == other.minify
            && self.drop_console == other.drop_console
            && self.drop_debugger == other.drop_debugger
            && self.module_name == other.module_name
            && self.one_shot == other.one_shot
    }
}

impl Eq for OptimizeOptionsInput {}

impl Hash for OptimizeOptionsInput {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.define.hash(state);
        self.minify.hash(state);
        self.drop_console.hash(state);
        self.drop_debugger.hash(state);
        self.module_name.hash(state);
        self.one_shot.hash(state);
    }
}

/// `parse` 任务的输出：AST 持有者 + 源文本 + 依赖（说明符已解为 `String`）+ 诊断。
/// 作为引擎 cell 值，须 `Send + Sync + 'static`（`ModuleAst` 已具备）+ 指纹。
pub struct ParsedModule {
    pub ast: Arc<ModuleAst>,
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
    resolution_environment: Arc<ResolutionEnvironment>,
    interner: Arc<Interner>,
    engine: Arc<Engine>,
    /// CLI/library one-shot builds never advance the in-memory task graph to a second generation.
    /// The transient engine therefore stores cross-stage values without red/green fingerprints.
    one_shot: bool,
    one_shot_built: bool,
    /// Task executions retained as an observable counter after a one-shot engine is consumed.
    released_task_exec_count: u64,
    exec: Arc<Executor>,
    /// `Arc` 包裹：每层依赖 resolve 经工作窃取执行器**并行**（`Resolver` 现为 `Sync`，见其 `cache` 注释）。
    resolver: Arc<Resolver>,
    /// 解析选项（含别名）。跨构建保留——PnP 检测切换解析器时用它重建，避免丢别名。
    resolve_options: ResolveOptions,
    /// 宿主平台、入口格式与显式 external 都属于稳定构建身份。
    platform: BuildPlatform,
    module_format: ModuleFormat,
    external_packages: Arc<[String]>,
    /// 规范化路径 → 内容输入 cell（跨构建保留）。
    content_cells: FxHashMap<PathBuf, Vc<Content>>,
    /// 规范化路径 → linker 输入 cell（跨构建保留）。
    optimize_linker_cells: FxHashMap<PathBuf, Vc<OptimizeLinkerData>>,
    /// Final-layout linker facts are isolated from optimizer inputs so retained-edge re-planning
    /// invalidates only body emission.
    emit_linker_cells: FxHashMap<PathBuf, Vc<EmitLinkerData>>,
    /// 规范化路径 → CSS codegen 输入。跨模块 token、名称 seed 或 dev/prod 注入模式变化时，
    /// 即使本模块源码/linker 数据不变，也必须让 codegen 任务失效。
    css_codegen_cells: FxHashMap<PathBuf, Vc<CssCodegenInput>>,
    /// 规范化路径 → define/minify/DCE 优化输入。sourceMap 不属于此 cell。
    optimize_options_cells: FxHashMap<PathBuf, Vc<OptimizeOptionsInput>>,
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
    /// Export liveness is a pure function of semantic scan summaries and link options. Chunk
    /// planning is intentionally not cached here: it is recomputed from optimizer-retained edges.
    link_plan: Option<LinkPlan>,
    link_plan_reuse_count: AtomicU64,
    topology_invalidated: AtomicBool,
    topology_reuse_count: AtomicU64,
    last_module_count: usize,
    /// 是否启用 Tree Shaking（默认关闭；prod build 开启）。DESIGN §5.3 / PLAN §6.6。
    tree_shaking: bool,
    /// 是否启用代码分割（默认关闭；prod build 开启）。DESIGN §6.3 / PLAN §6.5。
    code_splitting: bool,
    /// 单 chunk 路径是否使用 `entry.<content-hash>.js`（默认关闭，保留历史 `bundle.js`）。
    /// 供主动关闭代码分割、但仍需生产缓存失效语义的宿主使用。
    hash_single_chunk_entry: bool,
    /// 宿主指定的入口 chunk 逻辑名。Docs 用它让单包和分包都稳定产出 `entry.<hash>.js`。
    entry_chunk_name: Option<Arc<str>>,
    /// 产物文件名是否带内容 hash（默认开；dev 关以稳定 URL）。
    content_hash: bool,
    /// 共享 chunk 抽取阈值（模块被 ≥N 个 async root 共享则抽取，默认 2）。
    share_threshold: usize,
    /// 持久化构建缓存（opt-in，PLAN §7.1）。命中摘要跳 parse；retained facts、body 与 mapping
    /// facts 各自按阶段命中。
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
    /// 移除 `console.*` 调用（默认关；prod build 可选开）。
    drop_console: bool,
    /// 移除 `debugger` 语句（默认关；prod build 可选开）。
    drop_debugger: bool,
    /// 产出 Source Map（WAKE-COMPATIBILITY §M4d），支持普通及 minify、单包及代码分割路径。
    sourcemap: bool,
    /// `@crab-dev/css` 零运行时编译：构建期把 `` css`...` `` 抽取为静态 CSS
    /// （WAKE-COMPATIBILITY §M5）。见 [`IncrementalBundler::enable_css_in_js`]。
    css_in_js: bool,
    /// CSS 生成名使用的项目根。设置后模块 identity 取 project-relative 路径，避免 checkout
    /// 绝对路径、Windows 盘符与路径分隔符进入产物身份。
    project_root: Option<PathBuf>,
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
    /// 已明确外置的宿主依赖。它们不进入模块图，但属于 link 与缓存身份。
    external_deps: Vec<String>,
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
        Self::new_with_lifetime(fs, false)
    }

    /// Construct a bundler for exactly one build invocation.
    ///
    /// This keeps the same typed `Vc` stage boundaries but omits in-memory incremental metadata
    /// that cannot be reused before the process-level operation returns. Watch/dev sessions must
    /// use [`IncrementalBundler::new`].
    #[doc(hidden)]
    pub fn new_one_shot(fs: Arc<dyn FileSystem>) -> IncrementalBundler {
        Self::new_with_lifetime(fs, true)
    }

    fn new_with_lifetime(fs: Arc<dyn FileSystem>, one_shot: bool) -> IncrementalBundler {
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
        let resolution_environment = Arc::new(ResolutionEnvironment::new(fs));
        IncrementalBundler {
            resolver: resolution_environment.resolver(),
            resolve_options: ResolveOptions::default(),
            platform: BuildPlatform::Browser,
            module_format: ModuleFormat::Iife,
            external_packages: Arc::from(Vec::<String>::new()),
            fs: resolution_environment.file_system(),
            resolution_environment,
            interner: Arc::new(Interner::new()),
            engine: Arc::new(if one_shot {
                Engine::new_one_shot()
            } else {
                Engine::new()
            }),
            one_shot,
            one_shot_built: false,
            released_task_exec_count: 0,
            exec: global_executor(),
            define: default_define,
            define_hash,
            content_cells: FxHashMap::default(),
            optimize_linker_cells: FxHashMap::default(),
            emit_linker_cells: FxHashMap::default(),
            css_codegen_cells: FxHashMap::default(),
            optimize_options_cells: FxHashMap::default(),
            codegen_exec_counts: new_codegen_exec_counts(),
            load_cache: Arc::new(Mutex::new(FxHashMap::default())),
            load_exec_count: Arc::new(AtomicU64::new(0)),
            load_cache_enabled: false,
            resolve_exec_count: Arc::new(AtomicU64::new(0)),
            memory_summaries: FxHashMap::default(),
            memory_parse_vcs: FxHashMap::default(),
            stable_graph: None,
            link_plan: None,
            link_plan_reuse_count: AtomicU64::new(0),
            topology_invalidated: AtomicBool::new(true),
            topology_reuse_count: AtomicU64::new(0),
            last_module_count: 0,
            tree_shaking: false,
            code_splitting: false,
            hash_single_chunk_entry: false,
            entry_chunk_name: None,
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
            drop_console: false,
            drop_debugger: false,
            sourcemap: false,
            css_in_js: false,
            project_root: None,
            jsx: JsxRuntimeOptions::default(),
            target_fingerprint: default_target.fingerprint(),
            transform_features: default_target.required_features(),
        }
    }

    /// Replace the shared production executor before a test build so determinism can be checked
    /// across different worker counts. This intentionally stays test-only: production contexts
    /// share one process-wide pool to bound total thread ownership.
    #[cfg(test)]
    pub(crate) fn set_test_thread_count(&mut self, threads: usize) -> &mut Self {
        self.exec = Arc::new(Executor::new(threads));
        self
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

    /// 设置 bundle 宿主平台。平台改变条件导出与 Node builtin 的处理。
    pub fn set_platform(&mut self, platform: BuildPlatform) -> &mut Self {
        if self.platform != platform {
            self.platform = platform;
            self.reset_parse_graph();
        }
        self
    }

    /// 设置入口模块输出格式。
    pub fn set_module_format(&mut self, format: ModuleFormat) -> &mut Self {
        if self.module_format != format {
            self.module_format = format;
            self.link_plan = None;
        }
        self
    }

    /// 设置由宿主运行时提供的裸 npm 包。包名同时匹配其子路径。
    pub fn set_external_packages(&mut self, mut packages: Vec<String>) -> &mut Self {
        packages.sort();
        packages.dedup();
        if self.external_packages.as_ref() != packages.as_slice() {
            self.external_packages = packages.into();
            self.reset_parse_graph();
        }
        self
    }

    fn reset_parse_graph(&mut self) {
        self.engine = Arc::new(if self.one_shot {
            Engine::new_one_shot()
        } else {
            Engine::new()
        });
        self.released_task_exec_count = 0;
        self.content_cells.clear();
        self.optimize_linker_cells.clear();
        self.emit_linker_cells.clear();
        self.css_codegen_cells.clear();
        self.optimize_options_cells.clear();
        self.memory_summaries.clear();
        self.memory_parse_vcs.clear();
        self.stable_graph = None;
        self.link_plan = None;
        self.topology_invalidated.store(true, Ordering::Release);
    }

    /// 若入口所在项目根（含祖先）存在 `.pnp.cjs`，由统一解析环境启用 Yarn PnP。
    ///
    /// 幂等 + 惰性：`build()` 首次调用会自动触发；显式调用可提前决定日志/行为。
    pub fn enable_pnp(&mut self, start_dir: &Path) -> bool {
        if let Some(detected) = self.pnp_detected {
            return detected;
        }
        let enabled = self.resolution_environment.has_pnp_root(start_dir);
        self.pnp_detected = Some(enabled);
        enabled
    }

    /// 设置解析选项（含路径别名 `@`/`@@`/`@@@`）。须在首次 `build()` 前调用（CLI 读配置后）。
    /// 重建解析器；跨构建保留选项，供 PnP 检测切换解析器时复用（不丢别名）。
    /// 保持既定行为 `resolve.alias`（WAKE-COMPATIBILITY §M1/§H）。
    pub fn set_resolve_options(&mut self, options: ResolveOptions) -> &mut Self {
        self.resolution_environment = Arc::new(ResolutionEnvironment::with_options(
            self.resolution_environment.base_file_system(),
            options.clone(),
        ));
        self.fs = self.resolution_environment.file_system();
        self.resolver = self.resolution_environment.resolver();
        self.pnp_detected = None;
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

    /// 设置 CSS 编译产物的路径身份根。应用层应传已规范化的项目根；测试或虚拟文件系统使用
    /// 相对模块路径时可省略。
    pub fn set_project_root(&mut self, root: impl Into<PathBuf>) -> &mut Self {
        self.project_root = Some(root.into());
        self
    }

    fn css_module_seed(&self, path: &Path) -> String {
        map_source_name(path, self.project_root.as_deref())
    }

    /// 启用完整生产优化管线：语义优化、紧凑 codegen 与作用域安全的标识符改名。
    ///
    /// `minify` 是压缩行为的唯一开关；调用方不应再额外启用 name mangling。
    pub fn enable_minify(&mut self) -> &mut Self {
        self.minify = true;
        self
    }

    /// 启用死模块消除：emit 前从 entry 按优化器报告的结构化保留依赖边重算可达集，
    /// 丢弃不可达模块。发射后 JavaScript 字符串不再参与该判定。
    pub fn enable_dead_module_elimination(&mut self) -> &mut Self {
        self.dead_module_elimination = true;
        self
    }

    /// 启用 **Source Map** 产出（WAKE-COMPATIBILITY §M4d）。
    ///
    /// 产物 chunk 的 [`OutputChunk::source_map`](crate::OutputChunk::source_map) 将带上 V3 JSON，
    /// 由调用方决定写盘（`<chunk>.js.map`）或经 dev server 提供。
    ///
    /// mapped 与 unmapped 构建共用同一发射器，开启映射不会改变 JavaScript 主体字节。
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
        use wake_css_in_js::value::{Scope, collect_imports, collect_static_reexports};

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
            let seed = self.css_module_seed(&rec.path);
            let mut ex = parsed.ast.with_ast(|p| {
                wake_css_in_js::collect_static_exports_with(p, &self.interner, &seed, &scope)
            });
            let reexports = parsed
                .ast
                .with_ast(|p| collect_static_reexports(p, &self.interner));
            for reexport in reexports {
                let Some(dep_id) = rec
                    .dep_ids
                    .iter()
                    .find(|(specifier, _)| *specifier == reexport.specifier)
                    .map(|(_, dep_id)| *dep_id)
                else {
                    continue;
                };
                let Some(dependency_exports) = exports_of.get(&dep_id) else {
                    continue;
                };
                match (reexport.imported, reexport.exported) {
                    (Some(imported), Some(exported)) => {
                        if let Some(value) = dependency_exports.get(&imported) {
                            ex.insert(exported, value.clone());
                        }
                    }
                    (None, Some(exported)) => {
                        let mut namespace: Vec<_> = dependency_exports
                            .iter()
                            .map(|(name, value)| (name.clone(), value.clone()))
                            .collect();
                        namespace.sort_by(|left, right| left.0.cmp(&right.0));
                        ex.insert(exported, wake_css_in_js::StaticValue::Obj(namespace));
                    }
                    (None, None) => {
                        for (name, value) in dependency_exports {
                            if name != "default" {
                                ex.entry(name.clone()).or_insert_with(|| value.clone());
                            }
                        }
                    }
                    (Some(_), None) => unreachable!("named re-export always has an export name"),
                }
            }
            if !ex.is_empty() {
                exports_of.insert(id, ex);
            }
            scopes.insert(id, scope);
        }

        // 只有直接 import 编译期 marker 的模块需要进入 transform/codegen 侧表。依赖模块的
        // 静态导出已经在上面的后序遍历中求值；把所有模块都放进侧表会让一个 CSS import
        // 导致整张图走 CSS codegen 分支，并在 dev bundle 中产生无意义的空 registry 脚本。
        ordered
            .iter()
            .filter_map(|&id| {
                let direct_marker_import = modules.get(&id).is_some_and(|module| {
                    module
                        .deps
                        .iter()
                        .any(|dep| wake_css_in_js::is_css_in_js_source(&dep.specifier))
                });
                direct_marker_import.then(|| (id, Arc::new(scopes.remove(&id).unwrap_or_default())))
            })
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

    /// 启用 `@crab-dev/css` 的**零运行时 CSS-in-JS** 编译（WAKE-COMPATIBILITY §M5）。
    ///
    /// 从 `@crab-dev/css` import 的 `` css`...` `` 标签模板在构建期求值并抽取为静态 CSS，
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

    /// 启用代码分割（动态 import 切 async chunk，PLAN §6.5）。
    pub fn enable_code_splitting(&mut self) -> &mut Self {
        self.code_splitting = true;
        self
    }

    /// Force a single production chunk when a caller explicitly prefers one JavaScript artifact.
    pub fn disable_code_splitting(&mut self) -> &mut Self {
        self.code_splitting = false;
        self
    }

    /// 让未分割的生产入口使用 `entry.<content-hash>.js`。
    ///
    /// 默认关闭，因此普通单包构建仍输出历史文件名 `bundle.js`。该选项仍服从
    /// [`set_content_hash`](Self::set_content_hash)：全局关闭内容 hash 时输出 `entry.js`。
    pub fn enable_single_chunk_content_hash(&mut self) -> &mut Self {
        self.hash_single_chunk_entry = true;
        self
    }

    /// 设置入口 chunk 的逻辑文件名（不含扩展名与 hash），同时适用于单包和代码分割。
    pub fn set_entry_chunk_name(&mut self, name: impl Into<String>) -> &mut Self {
        let name: Arc<str> = Arc::from(name.into());
        if self.entry_chunk_name.as_deref() != Some(name.as_ref()) {
            self.entry_chunk_name = Some(name);
            self.reset_parse_graph();
        }
        self
    }

    /// 设置产物文件名是否带内容 hash（默认开）。dev 若开分割宜关（稳定 URL）。
    pub fn set_content_hash(&mut self, on: bool) -> &mut Self {
        self.content_hash = on;
        self
    }

    /// 引擎累计任务执行次数（parse + codegen）。第二遍构建应不增 → 全管线缓存命中。
    pub fn task_exec_count(&self) -> u64 {
        self.released_task_exec_count
            .saturating_add(self.engine.exec_count())
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

    /// Number of generations that reused the previous semantic link/chunk plan.
    pub fn link_plan_reuse_count(&self) -> u64 {
        self.link_plan_reuse_count.load(Ordering::Relaxed)
    }

    /// 由 generation-aware `BuildSession` 启用。直接打包器保持每次读取文件的兼容语义。
    pub(crate) fn enable_load_cache(&mut self) {
        self.load_cache_enabled = true;
    }

    /// 通知文件系统 generation 已变化。内容 cell 会在下一次扫描时按文本精确更新；
    /// resolver 的成功/失败路径缓存必须立即清空，以识别新增、删除和重命名文件。
    pub fn invalidate_filesystem(&self) {
        self.resolution_environment.invalidate_all();
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
                            | ".pnp.data.json"
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
            self.resolution_environment
                .invalidate_paths(normalized.iter().map(PathBuf::as_path));
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
                self.css_in_js,
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
            let liveness = (self.cache.is_some()
                || self.load_cache_enabled
                || self.tree_shaking
                || self.minify)
                .then(|| {
                    Arc::new(parsed.ast.with_ast(|p| {
                        collect_module_liveness_with_css(p, self.interner.as_ref(), self.css_in_js)
                    }))
                });
            // Target-kind facts are required by linked import semantics in readable and minified
            // builds alike, not just by minified scope concatenation.
            let block_info = parsed.ast.with_ast(scan_concat_block_info);

            self.memory_parse_vcs.insert(item.content_key, parse_vc);
            if parsed.diagnostics.is_empty() && (self.cache.is_some() || self.load_cache_enabled) {
                let live = liveness
                    .as_ref()
                    .expect("cache summary always includes liveness");
                let summary = Arc::new(ModuleSummary {
                    deps: deps.iter().map(parsed_dep_to_cached).collect(),
                    uses: uses.iter().map(import_use_to_cached).collect(),
                    has_top_level_await: parsed.has_top_level_await,
                    liveness: runtime_liveness_to_cached(live, &self.interner),
                    concat_is_esm: block_info.is_esm,
                    concat_block_safe: block_info.block_safe,
                });
                self.memory_summaries.insert(
                    item.content_key,
                    MemoryScanSummary {
                        persisted: summary.clone(),
                        deps: deps.clone(),
                        liveness: live.clone(),
                        block_info,
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
            // Minified scope concatenation also consumes namespace-identity facts from this
            // parser generation. Retain an analysis already paid for by the generation cache even
            // during a readable build, so switching that stable graph to minify cannot lose it.
            rec.liveness = liveness;
            rec.block_info = Some(block_info);
            rec.has_top_level_await = parsed.has_top_level_await;
            rec.parse_vc = Some(parse_vc);
            rec.parsed = Some(parsed);
        }

        self.topology_reuse_count.fetch_add(1, Ordering::Relaxed);
        Some(())
    }

    /// 从 `entry` 增量 + 并行打包。
    pub fn build(&mut self, entry: &Path) -> BuildOutput {
        assert!(
            !self.one_shot || !self.one_shot_built,
            "one-shot IncrementalBundler may only build once"
        );
        self.one_shot_built = true;
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
        // Persistent and generation load caches have their own source/stamp ownership. Fuse exact
        // resolution with loading only when this process can consume the source exactly once and
        // no later generation needs a resolver/cache record.
        let prefetch_exact_relative =
            self.one_shot && self.cache.is_none() && !self.load_cache_enabled;

        let mut module_to_id: FxHashMap<ModuleIdentity, u32> =
            FxHashMap::with_capacity_and_hasher(self.last_module_count, Default::default());
        // A successful source read proves that this exact physical candidate is available for the
        // remainder of the one-shot build. Keep the logical identity alongside the normalized path:
        // package identities may intentionally collapse different install locations, while loader
        // failures and their canonical fallback diagnostics remain physical-path specific.
        let mut successfully_loaded_candidates: FxHashMap<PathBuf, ModuleIdentity> =
            if prefetch_exact_relative {
                FxHashMap::with_capacity_and_hasher(self.last_module_count, Default::default())
            } else {
                FxHashMap::default()
            };
        let mut next_id: u32 = 0;
        let mut modules: FxHashMap<u32, ModuleRec> =
            FxHashMap::with_capacity_and_hasher(self.last_module_count, Default::default());
        let entry_identity = self.resolver.module_identity(&entry_norm);
        let entry_id = assign_id(&mut module_to_id, &mut next_id, entry_identity);
        let mut frontier: Vec<(u32, PathBuf, Option<Arc<Loaded>>)> =
            vec![(entry_id, entry_norm.clone(), None)];
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
            let frontier_items: Vec<(u32, PathBuf, Option<Arc<Loaded>>)> =
                std::mem::take(&mut frontier);
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
                    for (index, (id, path, prefetched)) in frontier_items.into_iter().enumerate() {
                        if let Some(loaded) = prefetched {
                            slots[index] = Some((id, path, Ok(loaded), None, None));
                        } else if self.load_cache_enabled
                            && let Some(loaded) = cache.get(&path).cloned()
                        {
                            slots[index] = Some((id, path, Ok(loaded), None, None));
                        } else {
                            let persistent = self.cache.as_mut().and_then(|persistent_cache| {
                                cached_source_type(self.fs.as_ref(), &path).map(|source_type| {
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
                        if prefetch_exact_relative {
                            successfully_loaded_candidates
                                .entry(path.clone())
                                .or_insert_with(|| self.resolver.module_identity(&path));
                        }
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
                                content_key_of(
                                    src,
                                    st,
                                    self.jsx,
                                    self.target_fingerprint,
                                    self.css_in_js,
                                    &path,
                                )
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
                self.cache.is_some() || self.load_cache_enabled || self.tree_shaking || self.minify;
            let requests: Vec<_> = to_parse
                .iter()
                .map(|&i| {
                    let cell = layer[i].content_vc;
                    let st = layer[i].source_type;
                    let interner = self.interner.clone();
                    let jsx = self.jsx;
                    let css_in_js = self.css_in_js;
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
                                Arc::new(collect_module_liveness_with_css(
                                    program,
                                    interner.as_ref(),
                                    css_in_js,
                                ))
                            });
                            let block_info = Some(scan_concat_block_info(program));
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
            drop(engine);
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
            let mut resolve_reqs: Vec<(String, PathBuf, DependencyKind)> = Vec::new();
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
                        let liveness = Some(memory_liveness.unwrap_or_else(|| {
                            Arc::new(cached_liveness_to_runtime(&sum.liveness, &self.interner))
                        }));
                        let block_info = Some(memory_block_info.unwrap_or(ConcatBlockInfo {
                            is_esm: sum.concat_is_esm,
                            block_safe: sum.concat_block_safe,
                        }));
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
                            liveness,
                            block_info,
                        )
                    }
                };

                let from_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
                // resolve 请求按依赖序压入（Pass 2 按同序消费）。
                for dep in deps.iter() {
                    resolve_reqs.push((dep.specifier.clone(), from_dir.clone(), dep.kind));
                }
                // 绑定级活跃性同时承载 minified concat 的 namespace identity 事实；
                // SymbolId 只在当前 parse 世代内解析，持久化边界仍只存公开名与说明符。
                let liveness = cached_liveness.or_else(|| {
                    if self.tree_shaking || self.minify {
                        parsed_opt.as_ref().map(|p| {
                            Arc::new(p.ast.with_ast(|prog| {
                                collect_module_liveness_with_css(
                                    prog,
                                    self.interner.as_ref(),
                                    self.css_in_js,
                                )
                            }))
                        })
                    } else {
                        None
                    }
                });
                // ESM target kind is required by import interop in every linked mode; block safety
                // is consumed only by minified concat but is cheap to retain from the same scan.
                let block_info = cached_block_info.or_else(|| {
                    parsed_opt
                        .as_ref()
                        .map(|p| p.ast.with_ast(scan_concat_block_info))
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
                self.resolve_exec_count
                    .fetch_add(resolve_reqs.len() as u64, Ordering::Relaxed);

                struct IndexedResolveRequest {
                    index: usize,
                    specifier: String,
                    from_dir: PathBuf,
                    kind: DependencyKind,
                }

                enum ResolveWork {
                    Normal(IndexedResolveRequest),
                    Exact {
                        candidate: PathBuf,
                        requests: Vec<IndexedResolveRequest>,
                    },
                }

                struct ResolveWorkOutput {
                    results: Vec<(usize, ResolveResult)>,
                    successful_exact: Option<ResolvedModule>,
                }

                let mut work = Vec::<ResolveWork>::new();
                // A normalized physical path has one deterministic ModuleIdentity for this
                // resolver lifetime. Indexing the first `(identity, path)` work item by path avoids
                // repeating package-identity lookup for duplicate edges without merging distinct
                // physical package installations that share one logical identity.
                let mut exact_work_by_path: FxHashMap<PathBuf, usize> = FxHashMap::default();
                for (index, (specifier, from_dir, kind)) in resolve_reqs.into_iter().enumerate() {
                    if is_external_specifier(&specifier, self.platform, &self.external_packages) {
                        slots[index] = Some(ResolveResult::External(specifier));
                        continue;
                    }

                    let candidate = prefetch_exact_relative
                        .then(|| exact_relative_file_candidate(&specifier, &from_dir))
                        .flatten()
                        // Node rejects browser resources immediately after resolve; do not
                        // read/transform one merely to emit that same diagnostic.
                        .filter(|path| {
                            self.platform != BuildPlatform::Node || !is_node_browser_resource(path)
                        });
                    let request = IndexedResolveRequest {
                        index,
                        specifier,
                        from_dir,
                        kind,
                    };
                    let Some(candidate) = candidate else {
                        work.push(ResolveWork::Normal(request));
                        continue;
                    };

                    if let Some(identity) = successfully_loaded_candidates.get(&candidate) {
                        // A prior BFS layer already consumed this exact file successfully. Its
                        // graph identity is known, so reopening it cannot add source ownership.
                        slots[index] = Some(ResolveResult::Internal {
                            module: ResolvedModule {
                                identity: identity.clone(),
                                path: candidate,
                            },
                            prefetched: None,
                        });
                        continue;
                    }

                    if let Some(&work_index) = exact_work_by_path.get(&candidate) {
                        match &mut work[work_index] {
                            ResolveWork::Exact { requests, .. } => requests.push(request),
                            ResolveWork::Normal(_) => {
                                unreachable!("exact work index must be exact")
                            }
                        }
                    } else {
                        let work_index = work.len();
                        exact_work_by_path.insert(candidate.clone(), work_index);
                        work.push(ResolveWork::Exact {
                            candidate,
                            requests: vec![request],
                        });
                    }
                }

                let jobs: Vec<_> = into_bounded_batches(work, io_batch_limit(&self.exec))
                    .into_iter()
                    .map(|batch| {
                        let resolver = Arc::clone(&self.resolver);
                        let load_exec_count = self.load_exec_count.clone();
                        let fs = Arc::clone(&self.fs);
                        let load_opts = Arc::clone(&load_opts);
                        let platform = self.platform;
                        move || {
                            batch
                                .into_iter()
                                .map(|work| {
                                    let resolve_normally = |request: IndexedResolveRequest| {
                                        let profile = resolution_profile(platform, request.kind);
                                        let resolved = match resolver.resolve_module_with_profile(
                                            &request.specifier,
                                            &request.from_dir,
                                            &profile,
                                        ) {
                                            Ok(module) => ResolveResult::Internal {
                                                module,
                                                prefetched: None,
                                            },
                                            Err(_) => ResolveResult::Error,
                                        };
                                        (request.index, resolved)
                                    };

                                    match work {
                                        ResolveWork::Normal(request) => ResolveWorkOutput {
                                            results: vec![resolve_normally(request)],
                                            successful_exact: None,
                                        },
                                        ResolveWork::Exact {
                                            candidate,
                                            requests,
                                        } => {
                                            load_exec_count.fetch_add(1, Ordering::Relaxed);
                                            match load_source(
                                                fs.as_ref(),
                                                &candidate,
                                                load_opts.as_ref(),
                                            ) {
                                                Ok(loaded) => {
                                                    // Keep package ownership discovery parallel
                                                    // with the successful physical read. The
                                                    // driver only groups normalized paths; it
                                                    // must not serialize filesystem-backed
                                                    // identity work for every unique module.
                                                    let module = ResolvedModule {
                                                        identity: resolver
                                                            .module_identity(&candidate),
                                                        path: candidate,
                                                    };
                                                    let loaded = Arc::new(loaded);
                                                    let results = requests
                                                        .into_iter()
                                                        .map(|request| {
                                                            (
                                                                request.index,
                                                                ResolveResult::Internal {
                                                                    module: module.clone(),
                                                                    prefetched: Some(Arc::clone(
                                                                        &loaded,
                                                                    )),
                                                                },
                                                            )
                                                        })
                                                        .collect();
                                                    ResolveWorkOutput {
                                                        results,
                                                        successful_exact: Some(module),
                                                    }
                                                }
                                                // A speculative failure proves nothing. The
                                                // canonical resolver+loader path owns TS twins,
                                                // directories and all user-visible diagnostics.
                                                Err(_) => ResolveWorkOutput {
                                                    results: requests
                                                        .into_iter()
                                                        .map(resolve_normally)
                                                        .collect(),
                                                    successful_exact: None,
                                                },
                                            }
                                        }
                                    }
                                })
                                .collect::<Vec<_>>()
                        }
                    })
                    .collect();

                for batch in self.exec.parallel(jobs) {
                    for output in batch {
                        if let Some(module) = output.successful_exact {
                            successfully_loaded_candidates.insert(module.path, module.identity);
                        }
                        for (index, resolved) in output.results {
                            slots[index] = Some(resolved);
                        }
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
            let mut next: Vec<(u32, PathBuf, Option<Arc<Loaded>>)> = Vec::new();
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
                let mut external_deps = Vec::new();
                for dep in deps.iter() {
                    let resolved = &resolved_flat[flat_idx];
                    flat_idx += 1;
                    match resolved {
                        ResolveResult::Internal {
                            module: resolved,
                            prefetched,
                        } => {
                            if self.platform == BuildPlatform::Node
                                && is_node_browser_resource(&resolved.path)
                            {
                                diagnostics.push(
                                    Diagnostic::error(format!(
                                        "Node bundle 不支持浏览器资源模块 `{}`",
                                        resolved.path.display()
                                    ))
                                    .with_code("WAKE0303")
                                    .with_path(path.to_string_lossy().into_owned())
                                    .with_primary(dep.span, "此资源导入不适用于 Node bundle")
                                    .with_note("请移除此导入，或改用 browser+iife 构建"),
                                );
                                continue;
                            }
                            let known = module_to_id.contains_key(&resolved.identity);
                            let did = assign_id(
                                &mut module_to_id,
                                &mut next_id,
                                resolved.identity.clone(),
                            );
                            if !known {
                                next.push((
                                    did,
                                    resolved.path.clone(),
                                    prefetched.as_ref().map(Arc::clone),
                                ));
                            }
                            dep_ids.push((dep.specifier.clone(), did));
                        }
                        ResolveResult::External(specifier) => {
                            external_deps.push(specifier.clone());
                        }
                        ResolveResult::Error => diagnostics.push(
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
                if self.platform == BuildPlatform::Node && is_node_browser_resource(&path) {
                    diagnostics.push(
                        Diagnostic::error(format!(
                            "Node bundle 不支持浏览器资源模块 `{}`",
                            path.display()
                        ))
                        .with_code("WAKE0303")
                        .with_path(path.to_string_lossy().into_owned())
                        .with_note("请从 Node 入口移除此资源导入，或改用 browser+iife 构建"),
                    );
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
                        external_deps,
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

        let link_fingerprint = self.link_plan_fingerprint(&modules, entry_id, next_id);
        let keep = if let Some(plan) = self
            .link_plan
            .as_ref()
            .filter(|plan| plan.fingerprint == link_fingerprint)
        {
            self.link_plan_reuse_count.fetch_add(1, Ordering::Relaxed);
            plan.keep.clone()
        } else {
            // —— Link：Tree Shaking. Top-level-await propagation and chunk ownership are computed
            // only after optimizer-retained dependency edges have converged. ——
            let keep = self.compute_keep_exports(&modules, entry_id, next_id);
            self.link_plan = Some(LinkPlan {
                fingerprint: link_fingerprint,
                keep: keep.clone(),
            });
            keep
        };
        let link_time = t_link_start.map_or(std::time::Duration::ZERO, |t| t.elapsed());
        let t_codegen_start = timing.then(std::time::Instant::now);
        let mut optimize_time = std::time::Duration::ZERO;
        let mut body_time = std::time::Duration::ZERO;

        // —— codegen 阶段：设 linker cell（驱动）+ 查产物缓存 + 并行 codegen 未命中者 ——
        let ordered: Vec<u32> = (0..next_id).filter(|id| modules.contains_key(id)).collect();
        self.last_module_count = ordered.len();

        // CSS-in-JS 是否真的用得上：全项目无人 import `@crab-dev/css` 时整体跳过——
        // 既省掉静态导出求值，也让产物磁盘缓存照常命中（见下方缓存守卫），
        // 从而使本功能对未使用 Crab CSS 的项目零开销、可安全默认开启。
        let cij_active = self.css_in_js
            && modules.values().any(|r| {
                r.deps
                    .iter()
                    .any(|d| wake_css_in_js::is_css_in_js_source(&d.specifier))
            });

        // 3a. Build optimizer identities and read optimizer-owned retained facts. Neither final
        // chunk numbering nor source-map enablement participates in this stage.
        let optimizer_salt =
            optimizer_config_salt(self.minify, self.drop_console, self.drop_debugger);
        let mut plans: Vec<CgPlan> = Vec::with_capacity(ordered.len());
        for &id in &ordered {
            let (path, dep_ids, content_key) = {
                let rec = &modules[&id];
                (rec.path.clone(), rec.dep_ids.clone(), rec.content_key)
            };
            let mut internal_esm_deps: Vec<String> = dep_ids
                .iter()
                .filter(|(_, target)| {
                    modules
                        .get(target)
                        .and_then(|module| module.block_info)
                        .is_some_and(|info| info.is_esm)
                })
                .map(|(specifier, _)| specifier.clone())
                .collect();
            internal_esm_deps.sort_unstable();
            internal_esm_deps.dedup();
            let data = OptimizeLinkerData {
                module_id: id,
                deps: dep_ids,
                internal_esm_deps,
                export_keep: keep.get(&id).cloned().flatten(),
            };
            let optimize_linker_hash = hash_optimize_linker(&data);
            let optimize_key = ((content_key as u128) << 64)
                | ((optimize_linker_hash ^ self.define_hash ^ optimizer_salt) as u128);
            let cached_retained_module_ids = if cij_active {
                None
            } else {
                self.cache
                    .as_mut()
                    .and_then(|cache| cache.retained_module_ids(optimize_key))
            };
            let optimize_linker_vc = self.optimize_linker_cell(&path, data);
            plans.push(CgPlan {
                id,
                path,
                content_key,
                optimize_key,
                optimize_linker_hash,
                optimize_linker_vc,
                optimized_vc: None,
                optimized_artifact: None,
                retained_module_ids: cached_retained_module_ids,
                body_key: 0,
                emit_linker_vc: None,
                emit_linker_data: None,
                body_vc: None,
                cached_body: None,
                cached_map: None,
                cached_discarded_static_requests: None,
            });
        }

        // 3b. Retained-fact misses must optimize before chunk planning. Parse only those modules;
        // a complete persistent hit keeps the parser and optimizer cold.
        let need_parse: Vec<usize> = plans
            .iter()
            .enumerate()
            .filter(|(_, p)| p.retained_module_ids.is_none() && modules[&p.id].parse_vc.is_none())
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
            drop(engine);
            for (&i, (pvc, parsed)) in need_parse.iter().zip(results) {
                let rec = modules.get_mut(&plans[i].id).unwrap();
                extend_module_diagnostics(&mut diagnostics, &parsed.diagnostics, &rec.path);
                rec.parse_vc = Some(pvc);
                rec.parsed = Some(parsed);
            }
        }

        // 3b'. CSS-in-JS：先算各模块的「静态导出常量」，再为每个模块把它的 import 绑定
        // 解析成可求值的静态值（`token` → 那个模块 default 导出的对象）。
        // 只做**一层**：被引用模块自身的求值仅用其模块内信息（design token 文件正是此形态）。
        let cij_scopes: FxHashMap<u32, Arc<wake_css_in_js::value::Scope>> = if cij_active {
            self.resolve_css_in_js_scopes(&modules, &ordered)
        } else {
            FxHashMap::default()
        };

        // 3c. Optimize retained-fact misses in parallel.
        let optimize_miss: Vec<usize> = plans
            .iter()
            .enumerate()
            .filter(|(_, p)| p.retained_module_ids.is_none())
            .map(|(i, _)| i)
            .collect();
        // User-provided define text is parsed once for this build, never once per module. The raw
        // form remains in the Vc identity; the validated form is carried in the same input cell so
        // a long-lived task cannot retain stale trusted edits after configuration changes.
        let validated_define = validate_defines(&self.define);
        let mut requests = Vec::with_capacity(optimize_miss.len());
        for &i in &optimize_miss {
            let parse_vc = modules[&plans[i].id]
                .parse_vc
                .expect("optimizer miss must be parsed");
            let linker_vc = plans[i].optimize_linker_vc;
            let interner = self.interner.clone();
            let id = plans[i].id;
            let cij = cij_scopes.get(&id).cloned();
            // dev（未开抽取）时把 CSS 以 `<style>` 注入模块体；prod 带出聚合。
            let inject_style = !self.extract_css;
            let css_input = CssCodegenInput {
                scope: cij
                    .as_deref()
                    .map(|scope| {
                        let mut entries: Vec<_> = scope
                            .iter()
                            .map(|(name, value)| (name.clone(), value.clone()))
                            .collect();
                        entries.sort_by(|left, right| left.0.cmp(&right.0));
                        entries
                    })
                    .unwrap_or_default(),
                seed: cij
                    .as_ref()
                    .map(|_| self.css_module_seed(&modules[&id].path)),
                inject_style,
            };
            let css_input_vc = self.css_codegen_cell(&plans[i].path, css_input);
            let options_input_vc = self.optimize_options_cell(
                &plans[i].path,
                OptimizeOptionsInput {
                    define: self.define.iter().cloned().collect(),
                    validated_define: validated_define.clone(),
                    minify: self.minify,
                    drop_console: self.drop_console,
                    drop_debugger: self.drop_debugger,
                    module_name: path_to_slash(&plans[i].path),
                    one_shot: self.one_shot,
                },
            );
            requests.push(move || {
                optimize_request(
                    parse_vc,
                    linker_vc,
                    css_input_vc,
                    options_input_vc,
                    interner,
                )
            });
        }
        let engine = Arc::clone(&self.engine);
        let t_optimize_start = timing.then(std::time::Instant::now);
        let optimized_results = par_request_batched(&engine, &self.exec, requests);
        drop(engine);
        optimize_time +=
            t_optimize_start.map_or(std::time::Duration::ZERO, |started| started.elapsed());
        let mut timing_optimizer_modules = 0_usize;
        let mut timing_optimizer_iterations = 0_usize;
        let mut timing_optimizer_passes = Vec::<(&'static str, usize, usize)>::new();
        for (&i, (optimized_vc, out)) in optimize_miss.iter().zip(optimized_results) {
            if timing && let Some(optimized) = &out.optimized {
                timing_optimizer_modules += 1;
                timing_optimizer_iterations += optimized.stats().iterations;
                for pass in &optimized.stats().passes {
                    if let Some((_, runs, changes)) = timing_optimizer_passes
                        .iter_mut()
                        .find(|(name, _, _)| *name == pass.pass.name())
                    {
                        *runs += pass.runs;
                        *changes += pass.changes;
                    } else {
                        timing_optimizer_passes.push((pass.pass.name(), pass.runs, pass.changes));
                    }
                }
            }
            if out.optimized.is_some()
                && let Some(c) = self.cache.as_mut()
            {
                c.put_retained_module_ids(plans[i].optimize_key, out.retained_module_ids.clone());
            }
            extend_module_diagnostics(&mut diagnostics, &out.diagnostics, &plans[i].path);
            plans[i].retained_module_ids = Some(out.retained_module_ids.clone());
            plans[i].optimized_vc = Some(optimized_vc);
            if self.one_shot && !self.sourcemap {
                plans[i].optimized_artifact = Some(out);
            }
        }
        if timing && timing_optimizer_modules != 0 {
            let passes = timing_optimizer_passes
                .iter()
                .map(|(name, runs, changes)| format!("{name}:{runs}/{changes}"))
                .collect::<Vec<_>>()
                .join(" ");
            eprintln!(
                "[wake-optimizer] modules={} avg-iterations={:.2} passes(run/change)={passes}",
                timing_optimizer_modules,
                timing_optimizer_iterations as f64 / timing_optimizer_modules as f64,
            );
        }
        let retained_edges: Vec<(u32, Arc<Vec<u32>>)> = plans
            .iter()
            .map(|plan| {
                (
                    plan.id,
                    plan.retained_module_ids
                        .as_ref()
                        .expect("retained facts")
                        .clone(),
                )
            })
            .collect();

        // —— Retained-edge convergence and fresh chunk planning. ——
        let (live, final_edges) = if self.dead_module_elimination {
            let live = live_modules(&retained_edges, entry_id);
            let edges = retained_module_edges(&modules, &retained_edges, &live);
            (live, edges)
        } else {
            (
                ordered.iter().copied().collect::<FxHashSet<_>>(),
                build_module_edges(&modules),
            )
        };
        let chunk_graph = if self.code_splitting {
            compute_chunk_graph(&final_edges, entry_id, self.share_threshold)
        } else {
            None
        };
        let async_ids = async_module_ids_with_edges(&modules, &final_edges);
        if self.module_format == ModuleFormat::CommonJs {
            if async_ids.contains(&entry_id) {
                let source = modules
                    .iter()
                    .filter(|(id, _)| live.contains(id))
                    .find(|(_, module)| module.has_top_level_await)
                    .map(|(_, module)| path_to_slash(&module.path))
                    .unwrap_or_else(|| path_to_slash(&entry_norm));
                diagnostics.push(
                    Diagnostic::error("CommonJS 入口的同步依赖图不支持顶层 await")
                        .with_code("WAKE0304")
                        .with_path(source),
                );
            } else {
                for (&module_id, module) in modules.iter().filter(|(id, _)| live.contains(id)) {
                    let retained_targets = final_edges
                        .get(&module_id)
                        .map(|edges| edges.static_targets.as_slice())
                        .unwrap_or_default();
                    let targets = module
                        .dep_ids
                        .iter()
                        .map(|(specifier, id)| (specifier.as_str(), *id))
                        .collect::<FxHashMap<_, _>>();
                    for dependency in &module.deps {
                        if dependency.kind == DependencyKind::Require
                            && targets
                                .get(dependency.specifier.as_str())
                                .is_some_and(|target| {
                                    retained_targets.contains(target) && async_ids.contains(target)
                                })
                        {
                            diagnostics.push(
                                Diagnostic::error(
                                    "CommonJS require() 不能同步加载包含顶层 await 的模块",
                                )
                                .with_code("WAKE0304")
                                .with_path(path_to_slash(&module.path))
                                .with_primary(dependency.span, "此 require() 指向异步模块"),
                            );
                        }
                    }
                }
            }
        }
        // Single-package minify can omit the ESM marker. A split runtime needs it for ESM/CJS
        // interop; this final-layout fact is intentionally downstream of optimization.
        let no_esmodule = self.minify && chunk_graph.is_none();
        let live_ids: Vec<u32> = ordered
            .iter()
            .copied()
            .filter(|id| live.contains(id))
            .collect();
        let live_id_set: FxHashSet<u32> = live_ids.iter().copied().collect();

        // 3d. Final body identities and persistent hits. Every body is persisted together with
        // mapping facts, even for an unmapped request, so later map enablement never re-emits JS.
        for plan in plans.iter_mut().filter(|plan| live.contains(&plan.id)) {
            let rec = &modules[&plan.id];
            let mut async_deps: Vec<u32> = rec
                .dep_ids
                .iter()
                .map(|(_, target)| *target)
                .filter(|target| async_ids.contains(target))
                .collect();
            async_deps.sort_unstable();
            async_deps.dedup();
            let emit_data = EmitLinkerData {
                deps: rec.dep_ids.clone(),
                dyn_chunks: dyn_chunks_of(rec, chunk_graph.as_ref()),
                async_deps,
                no_esmodule,
            };
            let emit_hash = hash_emit_linker(&emit_data);
            plan.body_key = ((plan.content_key as u128) << 64)
                | ((plan.optimize_linker_hash ^ self.define_hash ^ optimizer_salt ^ emit_hash)
                    as u128);
            if self.one_shot && !self.sourcemap {
                plan.emit_linker_data = Some(Arc::new(emit_data.clone()));
            }
            plan.emit_linker_vc = Some(self.emit_linker_cell(&plan.path, emit_data));
            if !cij_active && let Some(cache) = self.cache.as_mut() {
                let body = cache.body(plan.body_key);
                let mappings = cache.mappings(plan.body_key);
                if body.is_some() && mappings.is_some() {
                    plan.cached_body = body;
                    if let Some(mappings) = mappings {
                        let (map, requests) = module_metadata_from_cache(mappings);
                        plan.cached_map = Some(map);
                        plan.cached_discarded_static_requests = requests;
                    }
                }
            }
        }

        // A retained-fact hit can still have a missing body (for example after bounded cache
        // eviction). Materialize its optimizer value now, without affecting final chunk planning.
        let late_optimize: Vec<usize> = plans
            .iter()
            .enumerate()
            .filter(|(_, plan)| {
                live.contains(&plan.id) && plan.cached_body.is_none() && plan.optimized_vc.is_none()
            })
            .map(|(index, _)| index)
            .collect();
        let late_need_parse: Vec<usize> = late_optimize
            .iter()
            .copied()
            .filter(|index| modules[&plans[*index].id].parse_vc.is_none())
            .collect();
        if !late_need_parse.is_empty() {
            let reqs: Vec<_> = late_need_parse
                .iter()
                .map(|&i| {
                    let rec = &modules[&plans[i].id];
                    let cell = rec.content_vc;
                    let st = rec.source_type;
                    let interner = self.interner.clone();
                    let jsx = self.jsx;
                    let transform_features = self.transform_features;
                    let file_name = jsx.dev.then(|| Arc::<str>::from(path_to_slash(&rec.path)));
                    move || parse_request(cell, interner, st, jsx, transform_features, file_name)
                })
                .collect();
            let engine = Arc::clone(&self.engine);
            let results = par_request_batched(&engine, &self.exec, reqs);
            drop(engine);
            for (&i, (parse_vc, parsed)) in late_need_parse.iter().zip(results) {
                let rec = modules.get_mut(&plans[i].id).expect("late parse module");
                extend_module_diagnostics(&mut diagnostics, &parsed.diagnostics, &rec.path);
                rec.parse_vc = Some(parse_vc);
                rec.parsed = Some(parsed);
            }
        }
        let mut late_requests = Vec::with_capacity(late_optimize.len());
        for &i in &late_optimize {
            let parse_vc = modules[&plans[i].id]
                .parse_vc
                .expect("late optimizer parse");
            let linker_vc = plans[i].optimize_linker_vc;
            let interner = self.interner.clone();
            let css_input_vc = self.css_codegen_cell(&plans[i].path, CssCodegenInput::default());
            let options_input_vc = self.optimize_options_cell(
                &plans[i].path,
                OptimizeOptionsInput {
                    define: self.define.iter().cloned().collect(),
                    validated_define: validated_define.clone(),
                    minify: self.minify,
                    drop_console: self.drop_console,
                    drop_debugger: self.drop_debugger,
                    module_name: path_to_slash(&plans[i].path),
                    one_shot: self.one_shot,
                },
            );
            late_requests.push(move || {
                optimize_request(
                    parse_vc,
                    linker_vc,
                    css_input_vc,
                    options_input_vc,
                    interner,
                )
            });
        }
        let engine = Arc::clone(&self.engine);
        let t_late_optimize_start = timing.then(std::time::Instant::now);
        let late_results = par_request_batched(&engine, &self.exec, late_requests);
        drop(engine);
        optimize_time +=
            t_late_optimize_start.map_or(std::time::Duration::ZERO, |started| started.elapsed());
        for (&i, (optimized_vc, out)) in late_optimize.iter().zip(late_results) {
            debug_assert_eq!(
                plans[i].retained_module_ids.as_deref().map(Vec::as_slice),
                Some(out.retained_module_ids.as_slice()),
                "persisted retained facts must match recomputed optimizer output"
            );
            extend_module_diagnostics(&mut diagnostics, &out.diagnostics, &plans[i].path);
            plans[i].optimized_vc = Some(optimized_vc);
            if self.one_shot && !self.sourcemap {
                plans[i].optimized_artifact = Some(out);
            }
        }

        // 3e. Emit JS bodies independently of source-map requests. Mapping facts are collected in
        // the same token walk and handed to the downstream source-map task.
        let body_miss: Vec<usize> = plans
            .iter()
            .enumerate()
            .filter(|(_, plan)| live.contains(&plan.id) && plan.cached_body.is_none())
            .map(|(index, _)| index)
            .collect();
        let mut body_requests = Vec::with_capacity(body_miss.len());
        for &i in &body_miss {
            let direct = if self.one_shot && !self.sourcemap {
                Some((
                    plans[i]
                        .optimized_artifact
                        .take()
                        .expect("one-shot body miss optimizer value"),
                    plans[i]
                        .emit_linker_data
                        .take()
                        .expect("one-shot final linker value"),
                ))
            } else {
                None
            };
            let optimized_vc = plans[i].optimized_vc;
            let emit_linker_vc = plans[i].emit_linker_vc;
            let interner = self.interner.clone();
            let codegen_exec_counts = self.codegen_exec_counts.clone();
            let codegen_counter_shard = plans[i].id as usize & (CODEGEN_COUNTER_SHARDS - 1);
            body_requests.push(move || {
                if let Some((artifact, data)) = direct {
                    (
                        None,
                        Arc::new(emit_body(
                            &artifact,
                            &data,
                            &interner,
                            &codegen_exec_counts,
                            codegen_counter_shard,
                        )),
                    )
                } else {
                    let (body_vc, body) = emit_body_request(
                        optimized_vc.expect("body miss optimizer value"),
                        emit_linker_vc.expect("final linker value"),
                        interner,
                        codegen_exec_counts,
                        codegen_counter_shard,
                    );
                    (Some(body_vc), body)
                }
            });
        }
        let engine = Arc::clone(&self.engine);
        let t_body_start = timing.then(std::time::Instant::now);
        let emitted = par_request_batched(&engine, &self.exec, body_requests);
        drop(engine);
        body_time += t_body_start.map_or(std::time::Duration::ZERO, |started| started.elapsed());
        let mut timing_trivial_bodies = 0_usize;
        let mut timing_trivial_body_nanos = 0_u64;
        let mut timing_full_body_nanos = 0_u64;
        let mut body_of: FxHashMap<u32, Arc<String>> = FxHashMap::default();
        let mut map_of: FxHashMap<u32, Arc<ModuleMappings>> = FxHashMap::default();
        let mut discarded_static_requests_of: FxHashMap<
            u32,
            Arc<Vec<GeneratedDiscardedStaticRequest>>,
        > = FxHashMap::default();
        for (&i, (body_vc, out)) in body_miss.iter().zip(emitted) {
            if timing {
                if out.sealed_trivial {
                    timing_trivial_bodies += 1;
                    timing_trivial_body_nanos =
                        timing_trivial_body_nanos.saturating_add(out.codegen_nanos);
                } else {
                    timing_full_body_nanos =
                        timing_full_body_nanos.saturating_add(out.codegen_nanos);
                }
            }
            if out.cacheable
                && let Some(cache) = self.cache.as_mut()
            {
                cache.put_body(plans[i].body_key, out.code.clone());
                cache.put_mappings(
                    plans[i].body_key,
                    Arc::new(cached_mappings_from_module(
                        &out.mapping_facts,
                        &out.discarded_static_requests,
                    )),
                );
            }
            if let Some(css) = &out.css {
                collected_css.push((plans[i].id, (**css).clone()));
            }
            plans[i].body_vc = body_vc;
            body_of.insert(plans[i].id, out.code.clone());
            if !out.discarded_static_requests.is_empty() {
                discarded_static_requests_of
                    .insert(plans[i].id, out.discarded_static_requests.clone());
            }
        }
        if timing && !body_miss.is_empty() {
            eprintln!(
                "[wake-body] trivial={}/{} codegen-cpu(trivial/full)={:.1}/{:.1}ms body-wall={:.1}ms",
                timing_trivial_bodies,
                body_miss.len(),
                timing_trivial_body_nanos as f64 / 1_000_000.0,
                timing_full_body_nanos as f64 / 1_000_000.0,
                body_time.as_secs_f64() * 1000.0,
            );
        }
        for plan in plans.iter().filter(|plan| live.contains(&plan.id)) {
            if let Some(body) = &plan.cached_body {
                body_of.insert(plan.id, body.clone());
            }
            if let Some(requests) = &plan.cached_discarded_static_requests {
                discarded_static_requests_of.insert(plan.id, requests.clone());
            }
            if self.sourcemap
                && let Some(map) = &plan.cached_map
            {
                map_of.insert(plan.id, map.clone());
            }
        }
        if self.sourcemap {
            let mut map_requests = Vec::with_capacity(body_miss.len());
            for &i in &body_miss {
                let body_vc = plans[i].body_vc.expect("new body task value");
                map_requests.push(move || source_map_facts_request(body_vc));
            }
            let engine = Arc::clone(&self.engine);
            let maps = par_request_batched(&engine, &self.exec, map_requests);
            drop(engine);
            for (&i, map) in body_miss.iter().zip(maps) {
                map_of.insert(plans[i].id, map);
            }
        }
        let bodies: Vec<(u32, Arc<String>)> = live_ids
            .iter()
            .map(|id| (*id, body_of[id].clone()))
            .collect();
        // Terminal body/map consumers have taken every one-shot optimized artifact they need.
        // Release plan-owned Arc clones before draining Engine memos so the parallel release owns
        // the final deep AST drops instead of deferring them to this function's serial epilogue.
        drop(plans);

        if self.extract_css && !collected_css.is_empty() {
            collected_css.retain(|(id, _)| live_id_set.contains(id));
        }
        let (style_assets, style_files) = if self.extract_css {
            build_style_artifacts(
                &modules,
                entry_id,
                chunk_graph.as_ref(),
                &mut collected_css,
                self.minify,
            )
        } else {
            (Vec::new(), BTreeMap::new())
        };
        let codegen_time = t_codegen_start.map_or(std::time::Duration::ZERO, |t| t.elapsed());
        let t_emit_start = timing.then(std::time::Instant::now);

        // —— Emit：双路（无 async chunk → 旧单包，逐字节不变；有 → 多 chunk 全局 registry）——
        // 模块 id / 数量用 `live_ids`（DME 后；未启用 DME 时 = 全量 `ordered`）。
        // concat 块安全信息（单包路径用）：id → ConcatBlockInfo（缺分析 → 保守缺省）。
        let block_infos: FxHashMap<u32, ConcatBlockInfo> = modules
            .iter()
            .filter_map(|(&id, rec)| rec.block_info.map(|bi| (id, bi)))
            .collect();
        let namespace_identity_ids = namespace_identity_module_ids(&modules);

        // Source maps are orthogonal to optimization. The emitter records the exact final byte
        // placement of every surviving source body fragment; merge_bundle_map then conservatively
        // aligns module-local mappings through minify's string rewrites and scope concatenation.
        // Mapping collection must never participate in output decisions.
        let want_map = self.sourcemap;
        let mut body_placements = Vec::new();

        let mut output = match &chunk_graph {
            None => {
                let bundle = emit(
                    &bodies,
                    entry_id,
                    self.minify,
                    no_esmodule,
                    &block_infos,
                    &namespace_identity_ids,
                    &async_ids,
                    &discarded_static_requests_of,
                    &self.exec,
                    self.module_format,
                    want_map.then_some(&mut body_placements),
                );
                let mut o =
                    crate::single_chunk(bundle, live_ids.len(), diagnostics, live_ids.clone());
                if let Some(name) = self
                    .entry_chunk_name
                    .as_deref()
                    .or(self.hash_single_chunk_entry.then_some("entry"))
                {
                    let entry = &mut o.chunks[o.entry_chunk];
                    entry.name = name.to_string();
                    entry.file_name = chunk_filename(name, &entry.code, self.content_hash);
                }
                if want_map {
                    // 源文件名 + 源文本：优先复用 parse 结果；全缓存命中而未 parse 时直接读内容
                    // input cell，使冷热缓存的 sourcesContent 与最终 map 逐字节一致。
                    let mut sources: FxHashMap<u32, (String, Option<String>)> =
                        FxHashMap::default();
                    let cwd = std::env::current_dir().ok();
                    for placement in &body_placements {
                        let id = placement.module_id;
                        if let Some(rec) = modules.get(&id) {
                            let name = map_source_name(&rec.path, cwd.as_deref());
                            let content = Some(
                                rec.parsed
                                    .as_ref()
                                    .map(|p| p.source.to_string())
                                    .unwrap_or_else(|| {
                                        self.engine.enter(|| rec.content_vc.read().to_string())
                                    }),
                            );
                            sources.insert(id, (name, content));
                        }
                    }
                    let file = o.chunks[o.entry_chunk].file_name.clone();
                    let sm = merge_bundle_map(
                        &o.chunks[o.entry_chunk].code,
                        &body_placements,
                        &bodies,
                        &map_of,
                        &sources,
                        Some(file),
                    );
                    let json = serialize_map(&sm, &sources);
                    o.chunks[o.entry_chunk].source_map = Some(json);
                }
                o
            }
            Some(g) => {
                let token = build_token(&normalize(entry), live_ids.len());
                let mut chunk_placements: FxHashMap<u32, Vec<BodyPlacement>> = FxHashMap::default();
                let (mut chunks, entry_chunk) = emit_chunks(
                    &bodies,
                    g,
                    entry_id,
                    &token,
                    &self.public_path,
                    self.content_hash,
                    &async_ids,
                    &style_files,
                    want_map.then_some(&mut chunk_placements),
                );
                if let Some(name) = self.entry_chunk_name.as_deref() {
                    let entry = &mut chunks[entry_chunk];
                    entry.name = name.to_string();
                    entry.file_name = chunk_filename(name, &entry.code, self.content_hash);
                }
                if want_map {
                    let cwd = std::env::current_dir().ok();
                    for chunk in &mut chunks {
                        let Some(placements) = chunk_placements.get(&chunk.chunk_id) else {
                            continue;
                        };
                        let mut sources: FxHashMap<u32, (String, Option<String>)> =
                            FxHashMap::default();
                        for placement in placements {
                            let id = placement.module_id;
                            if let Some(rec) = modules.get(&id) {
                                sources.entry(id).or_insert_with(|| {
                                    (
                                        map_source_name(&rec.path, cwd.as_deref()),
                                        Some(
                                            rec.parsed
                                                .as_ref()
                                                .map(|p| p.source.to_string())
                                                .unwrap_or_else(|| {
                                                    self.engine
                                                        .enter(|| rec.content_vc.read().to_string())
                                                }),
                                        ),
                                    )
                                });
                            }
                        }
                        let sm = merge_bundle_map(
                            &chunk.code,
                            placements,
                            &bodies,
                            &map_of,
                            &sources,
                            Some(chunk.file_name.clone()),
                        );
                        chunk.source_map = Some(serialize_map(&sm, &sources));
                    }
                }
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
        for chunk in &mut output.chunks {
            chunk.styles = style_files
                .get(&chunk.chunk_id)
                .cloned()
                .unwrap_or_default();
        }

        output.updated_module_count = codegen_exec_count(&self.codegen_exec_counts)
            .saturating_sub(codegen_exec_before) as usize;
        output.cached_module_count = live_ids.len().saturating_sub(output.updated_module_count);

        // —— 带外产物：超阈值资源（按文件名去重）+ prod 聚合 CSS（模块 id 升序 = BFS 发现序）——
        let mut assets: Vec<OutputAsset> = style_assets;
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
        output.assets = assets;

        // Final output owns every string/map it needs. Remove local Arc owners before the
        // one-shot Engine is drained so its shard jobs perform the final deep drops in parallel.
        drop(bodies);
        drop(body_of);
        drop(map_of);
        drop(discarded_static_requests_of);
        drop(body_placements);
        drop(block_infos);
        drop(namespace_identity_ids);
        drop(style_files);

        // 持久化缓存落盘（opt-in）：仅在无错误 **且本次新增过条目**（dirty）时写。
        // 全命中（未变）时缓存内容没变，跳过落盘——缓存文件常和 bundle 一样大，
        // 每次白写会让 `--cache` 的 I/O 反超它省下的 parse（实测小项目会更慢）。
        if !output.has_errors()
            && let (Some(cache), Some(path)) = (&mut self.cache, &self.cache_path)
            && cache.is_dirty()
        {
            let _ = cache.store(path);
        }

        let pre_release_total = t0.elapsed();
        let emit_time = t_emit_start.map_or(std::time::Duration::ZERO, |t| t.elapsed());
        if timing {
            eprintln!(
                "[wake-timing] 模块={} workers={} lifetime={} | scan={:.1?} (read={:.1?} resolve={:.1?}) | link={:.1?} | codegen={:.1?} (optimize={:.1?} body={:.1?}) | emit={:.1?} | pre-release={:.1?}",
                ordered.len(),
                self.exec.num_threads(),
                if self.one_shot { "one-shot" } else { "session" },
                t_scan,
                read_time,
                resolve_time,
                link_time,
                codegen_time,
                optimize_time,
                body_time,
                emit_time,
                pre_release_total,
            );
        }

        if self.one_shot {
            let release_started = std::time::Instant::now();
            // These maps only describe a possible later generation. A one-shot bundler rejects a
            // second build, so retaining them would merely serialize their destruction on the
            // caller's return path.
            self.content_cells.clear();
            self.optimize_linker_cells.clear();
            self.emit_linker_cells.clear();
            self.css_codegen_cells.clear();
            self.optimize_options_cells.clear();
            self.memory_summaries.clear();
            self.memory_parse_vcs.clear();
            self.link_plan = None;
            self.stable_graph = None;
            self.topology_invalidated.store(true, Ordering::Release);
            drop(modules);

            // `build` is the terminal owner in one-shot mode. Replace the field with an empty
            // inert engine so `release_one_shot` can consume the unique task graph Arc and spread
            // its 128 shard drops across the existing bounded executor.
            let engine = std::mem::replace(&mut self.engine, Arc::new(Engine::new_one_shot()));
            self.released_task_exec_count = self
                .released_task_exec_count
                .saturating_add(engine.exec_count());
            let release_stats = engine.release_one_shot(&self.exec);
            let release_elapsed = release_started.elapsed();
            if timing {
                eprintln!(
                    "[wake-release] inputs={} memos={} recomputers={} locks={} batches={} elapsed={release_elapsed:.1?} total={:.1?}",
                    release_stats.input_cells,
                    release_stats.memo_entries,
                    release_stats.recomputer_entries,
                    release_stats.task_locks,
                    release_stats.drop_batches,
                    t0.elapsed(),
                );
            }
        } else {
            if !output.has_errors() {
                let live_content: FxHashSet<u64> =
                    modules.values().map(|module| module.content_key).collect();
                self.memory_summaries
                    .retain(|content_key, _| live_content.contains(content_key));
                self.memory_parse_vcs
                    .retain(|content_key, _| live_content.contains(content_key));
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

    /// Get/update optimizer-owned linker facts. Final chunk numbering never enters this cell.
    fn optimize_linker_cell(
        &mut self,
        path: &Path,
        data: OptimizeLinkerData,
    ) -> Vc<OptimizeLinkerData> {
        if let Some(&cell) = self.optimize_linker_cells.get(path) {
            self.engine.set_input(cell, data);
            cell
        } else {
            let cell = self.engine.new_input(data);
            self.optimize_linker_cells.insert(path.to_path_buf(), cell);
            cell
        }
    }

    fn emit_linker_cell(&mut self, path: &Path, data: EmitLinkerData) -> Vc<EmitLinkerData> {
        if let Some(&cell) = self.emit_linker_cells.get(path) {
            self.engine.set_input(cell, data);
            cell
        } else {
            let cell = self.engine.new_input(data);
            self.emit_linker_cells.insert(path.to_path_buf(), cell);
            cell
        }
    }

    fn css_codegen_cell(&mut self, path: &Path, data: CssCodegenInput) -> Vc<CssCodegenInput> {
        if let Some(&cell) = self.css_codegen_cells.get(path) {
            self.engine.set_input(cell, data);
            cell
        } else {
            let cell = self.engine.new_input(data);
            self.css_codegen_cells.insert(path.to_path_buf(), cell);
            cell
        }
    }

    fn optimize_options_cell(
        &mut self,
        path: &Path,
        data: OptimizeOptionsInput,
    ) -> Vc<OptimizeOptionsInput> {
        if let Some(&cell) = self.optimize_options_cells.get(path) {
            self.engine.set_input(cell, data);
            cell
        } else {
            let cell = self.engine.new_input(data);
            self.optimize_options_cells.insert(path.to_path_buf(), cell);
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
    ) -> FxHashMap<u32, Option<ExportKeep>> {
        let mut keep: FxHashMap<u32, Option<ExportKeep>> = FxHashMap::default();
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
                Some(LiveResult::Names {
                    retained,
                    observed,
                    preserve_export_star,
                }) => {
                    let resolve_names = |atoms: &FxHashSet<_>| {
                        let mut names: Vec<String> = atoms
                            .iter()
                            .map(|atom| self.interner.resolve(*atom))
                            .collect();
                        names.sort_unstable();
                        names
                    };
                    Some(ExportKeep {
                        retained_export_names: resolve_names(retained),
                        observed_export_names: resolve_names(observed),
                        preserve_export_star: *preserve_export_star,
                    })
                }
                // `All` 或不在结果里（缺绑定分析）→ 保守全保留。
                _ => None,
            };
            keep.insert(id, k);
        }
        keep
    }

    fn link_plan_fingerprint(
        &self,
        modules: &FxHashMap<u32, ModuleRec>,
        entry_id: u32,
        next_id: u32,
    ) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        entry_id.hash(&mut hasher);
        next_id.hash(&mut hasher);
        self.tree_shaking.hash(&mut hasher);
        self.code_splitting.hash(&mut hasher);
        self.share_threshold.hash(&mut hasher);
        self.minify.hash(&mut hasher);
        self.platform.hash(&mut hasher);
        self.module_format.hash(&mut hasher);
        self.external_packages.hash(&mut hasher);

        let mut ids = modules.keys().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        for id in ids {
            let module = &modules[&id];
            id.hash(&mut hasher);
            path_to_slash(&module.path).hash(&mut hasher);
            module.dep_ids.hash(&mut hasher);
            module.external_deps.hash(&mut hasher);
            module.has_top_level_await.hash(&mut hasher);
            // The owned summary contains dependency kinds and binding liveness without retaining
            // AST/arena state. Equal semantic summaries deliberately reuse the plan even when a
            // string literal changed and therefore produced a different source content key.
            if let Some(summary) = self.memory_summaries.get(&module.content_key) {
                summary.persisted.hash(&mut hasher);
            } else {
                module.content_key.hash(&mut hasher);
                for dependency in &module.deps {
                    dependency.specifier.hash(&mut hasher);
                    dependency.kind.hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }
}

fn resolution_profile(platform: BuildPlatform, kind: DependencyKind) -> ResolutionProfile {
    let require = matches!(kind, DependencyKind::Require);
    let conditions = match (platform, require) {
        (BuildPlatform::Node, true) => vec!["node".into(), "require".into()],
        (BuildPlatform::Node, false) => vec!["node".into(), "import".into()],
        (BuildPlatform::Browser, true) => vec!["browser".into(), "require".into()],
        (BuildPlatform::Browser, false) => {
            vec!["browser".into(), "import".into(), "module".into()]
        }
    };
    let main_fields = match platform {
        BuildPlatform::Browser => vec!["module".into(), "main".into()],
        BuildPlatform::Node => vec!["main".into(), "module".into()],
    };
    ResolutionProfile {
        conditions,
        main_fields,
    }
}

fn is_external_specifier(specifier: &str, platform: BuildPlatform, packages: &[String]) -> bool {
    (platform == BuildPlatform::Node && is_node_builtin(specifier))
        || packages.iter().any(|package| {
            specifier == package
                || specifier
                    .strip_prefix(package)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

fn is_node_browser_resource(path: &Path) -> bool {
    is_asset_path(path)
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "css" | "html" | "htm"
                )
            })
}

/// 是否为 Node.js 内置模块（含 `node:` 前缀与 `fs/promises`、`stream/web` 等子路径）。
fn is_node_builtin(spec: &str) -> bool {
    if spec.starts_with("node:") {
        return true;
    }
    // 取第一段（`fs/promises` → `fs`）。
    let head = spec.split('/').next().unwrap_or(spec);
    matches!(
        head,
        "_http_agent"
            | "_http_client"
            | "_http_common"
            | "_http_incoming"
            | "_http_outgoing"
            | "_http_server"
            | "_stream_duplex"
            | "_stream_passthrough"
            | "_stream_readable"
            | "_stream_transform"
            | "_stream_wrap"
            | "_stream_writable"
            | "_tls_common"
            | "_tls_wrap"
            | "assert"
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

/// Per-module staged optimizer/body plan. Process-local Vc values never cross the persistent
/// boundary; stable retained targets, JavaScript, and mapping facts use separate cache entries.
struct CgPlan {
    id: u32,
    path: PathBuf,
    content_key: u64,
    optimize_key: u128,
    optimize_linker_hash: u64,
    optimize_linker_vc: Vc<OptimizeLinkerData>,
    optimized_vc: Option<Vc<OptimizeArtifact>>,
    /// One-shot terminal emission consumes this Arc directly instead of re-entering the task graph.
    optimized_artifact: Option<Arc<OptimizeArtifact>>,
    retained_module_ids: Option<Arc<Vec<u32>>>,
    body_key: u128,
    emit_linker_vc: Option<Vc<EmitLinkerData>>,
    emit_linker_data: Option<Arc<EmitLinkerData>>,
    body_vc: Option<Vc<EmittedBody>>,
    cached_body: Option<Arc<String>>,
    cached_map: Option<Arc<ModuleMappings>>,
    cached_discarded_static_requests: Option<Arc<Vec<GeneratedDiscardedStaticRequest>>>,
}

fn cached_mappings_from_module(
    mappings: &ModuleMappings,
    discarded_static_requests: &[GeneratedDiscardedStaticRequest],
) -> CachedModuleMappings {
    CachedModuleMappings {
        mappings: mappings
            .mappings
            .iter()
            .map(|mapping| CachedMapping {
                gen_line: mapping.gen_line,
                gen_col: mapping.gen_col,
                src_index: mapping.src_index,
                src_offset: mapping.src_offset,
                name_index: mapping.name_index,
                is_unmapped: mapping.is_unmapped,
            })
            .collect(),
        names: mappings.names.clone(),
        discarded_static_requests: discarded_static_requests
            .iter()
            .map(|request| CachedDiscardedStaticRequest {
                start: request.start,
                end: request.end,
                target_module_id: request.target_module_id,
            })
            .collect(),
    }
}

fn module_metadata_from_cache(
    mappings: Arc<CachedModuleMappings>,
) -> (
    Arc<ModuleMappings>,
    Option<Arc<Vec<GeneratedDiscardedStaticRequest>>>,
) {
    let module_mappings = Arc::new(ModuleMappings {
        mappings: mappings
            .mappings
            .iter()
            .map(|mapping| Mapping {
                gen_line: mapping.gen_line,
                gen_col: mapping.gen_col,
                src_index: mapping.src_index,
                src_offset: mapping.src_offset,
                name_index: mapping.name_index,
                is_unmapped: mapping.is_unmapped,
            })
            .collect(),
        names: mappings.names.clone(),
    });
    let requests = (!mappings.discarded_static_requests.is_empty()).then(|| {
        Arc::new(
            mappings
                .discarded_static_requests
                .iter()
                .map(|request| GeneratedDiscardedStaticRequest {
                    start: request.start,
                    end: request.end,
                    target_module_id: request.target_module_id,
                })
                .collect(),
        )
    });
    (module_mappings, requests)
}

fn extend_module_diagnostics(
    target: &mut Vec<Diagnostic>,
    diagnostics: &[Diagnostic],
    path: &Path,
) {
    let path = path_to_slash(path);
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
    css_in_js: bool,
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
    if css_in_js {
        // CSS marker imports change the liveness summary even though they do not change parsing.
        // Keep persistent summaries from CSS-enabled and CSS-disabled builds isolated.
        seed ^= 0x6372_6162_2d63_7373;
    }
    if jsx.dev {
        seed ^= xxh3_64_with_seed(path_to_slash(path).as_bytes(), 0x6a73_782d_6669_6c65);
    }
    xxh3_64_with_seed(src.as_bytes(), seed)
}

fn collect_module_liveness_with_css(
    program: &Program,
    interner: &Interner,
    css_in_js: bool,
) -> ModuleLiveness {
    let mut liveness = collect_module_liveness(program, interner);
    if css_in_js {
        let consumed = wake_css_in_js::compiler_consumed_imports(program, interner);
        liveness
            .named_imports
            .retain(|import| !consumed.contains(&import.local));
    }
    liveness
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

/// Stable optimizer identity. It deliberately excludes final chunk ids and map enablement.
fn hash_optimize_linker(data: &OptimizeLinkerData) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    data.hash(&mut h);
    h.finish()
}

/// Stable final-layout body identity. This is downstream of retained-edge convergence.
fn hash_emit_linker(data: &EmitLinkerData) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    data.hash(&mut h);
    h.finish()
}

/// Complete optimizer configuration identity for persistent module-body and source-map cache
/// invalidation. The pipeline version applies even to an unminified request because the optimizer
/// still validates and applies trusted edits before emission.
fn optimizer_config_salt(minify: bool, drop_console: bool, drop_debugger: bool) -> u64 {
    let version = xxh3_64_with_seed(PIPELINE_VERSION.as_bytes(), 0x9E37_79B9_7F4A_7C15);
    xxh3_64_with_seed(
        &[
            u8::from(minify),
            u8::from(drop_console),
            u8::from(drop_debugger),
        ],
        version,
    )
}

/// define 表指纹（固定种子 SipHash，跨进程稳定）——混入产物缓存键，使 define 变化精确失效缓存。
fn hash_define(define: &[(String, String)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    define.hash(&mut h);
    h.finish()
}

/// Parse configuration-provided define values once per build before they enter the optimizer.
/// The parser wrapper proves the complete value is one expression, so statement/comment injection
/// and trailing tokens become diagnostics. The optimizer owns the eventual grouping bytes;
/// syntax-level primitives alone enter constant folding.
fn validate_defines(define: &[(String, String)]) -> Result<Vec<ValidatedDefine>, String> {
    let interner = Interner::new();
    let mut seen = FxHashSet::default();
    let mut validated = Vec::with_capacity(define.len());

    for (key, value) in define {
        if key.is_empty() || key.trim() != key || !seen.insert(key.clone()) {
            return Err(format!(
                "define key `{key}` must be a unique, non-empty static member chain"
            ));
        }

        let key_source = format!("const __wake_define_key__=({key});");
        let parsed_key = parse(&key_source, &interner, SourceType::Module);
        if parsed_key.has_errors()
            || !parsed_key
                .module
                .with_ast(|program| define_initializer(program).is_some_and(is_static_define_key))
        {
            return Err(format!(
                "define key `{key}` must be an identifier, meta-property, or dot member chain{}",
                parse_error_suffix(&parsed_key.diagnostics)
            ));
        }

        if value.trim().is_empty() {
            return Err(format!("define `{key}` has an empty expression"));
        }
        let value_source = format!("const __wake_define_value__=({value});");
        let parsed_value = parse(&value_source, &interner, SourceType::Script);
        if parsed_value.has_errors() {
            return Err(format!(
                "define `{key}` is not one valid JavaScript expression{}",
                parse_error_suffix(&parsed_value.diagnostics)
            ));
        }
        let Some(primitive) = parsed_value.module.with_ast(|program| {
            define_initializer(program)
                .map(|expression| primitive_define_value(expression, &interner))
        }) else {
            return Err(format!(
                "define `{key}` is not one valid JavaScript expression"
            ));
        };

        let entry = if let Some(primitive) = primitive {
            ValidatedDefine::primitive(key.clone(), primitive)
        } else {
            // `TrustedExpression` deliberately accepts only an expression program, never the
            // temporary variable declaration used above to classify primitive values. Grouping
            // keeps object/class/function expressions unambiguous while the parser still proves
            // that all configured bytes belong to one expression.
            let expression_source = format!("({value})");
            let parsed_expression = parse(&expression_source, &interner, SourceType::Script);
            let expression =
                TrustedExpression::from_parsed_program(&parsed_expression.module, &interner);
            if !expression.is_valid() {
                return Err(format!(
                    "define `{key}` could not be lowered into one owned expression"
                ));
            }
            ValidatedDefine::expression(key.clone(), expression)
        };
        validated.push(entry);
    }

    Ok(validated)
}

fn define_initializer<'ast>(program: &'ast Program<'ast>) -> Option<Expression<'ast>> {
    if program.body.len() != 1 {
        return None;
    }
    let Statement::VariableDeclaration(declaration) = program.body[0] else {
        return None;
    };
    if declaration.declarations.len() != 1 {
        return None;
    }
    declaration.declarations[0].init
}

fn is_static_define_key(expression: Expression<'_>) -> bool {
    match expression {
        Expression::Identifier(_) | Expression::MetaProperty(_) => true,
        Expression::Member(member) if !member.optional => {
            matches!(member.property, MemberProperty::Ident(_))
                && is_static_define_key(member.object)
        }
        _ => false,
    }
}

fn primitive_define_value(expression: Expression<'_>, interner: &Interner) -> Option<ConstVal> {
    match expression {
        Expression::NumberLiteral(literal) => Some(ConstVal::Num(literal.value)),
        Expression::StringLiteral(literal) => Some(ConstVal::Str(interner.resolve(literal.value))),
        Expression::BooleanLiteral(literal) => Some(ConstVal::Bool(literal.value)),
        Expression::NullLiteral(_) => Some(ConstVal::Null),
        Expression::Identifier(identifier) => match interner.resolve(identifier.name).as_str() {
            "undefined" => Some(ConstVal::Undefined),
            "NaN" => Some(ConstVal::Num(f64::NAN)),
            "Infinity" => Some(ConstVal::Num(f64::INFINITY)),
            _ => None,
        },
        Expression::Unary(unary) => {
            let argument = primitive_define_value(unary.argument, interner)?;
            match (unary.operator, argument) {
                (UnaryOperator::Minus, ConstVal::Num(value)) => Some(ConstVal::Num(-value)),
                (UnaryOperator::Plus, ConstVal::Num(value)) => Some(ConstVal::Num(value)),
                (UnaryOperator::LogicalNot, value) => Some(ConstVal::Bool(!value.truthy())),
                (UnaryOperator::Void, _) => Some(ConstVal::Undefined),
                _ => None,
            }
        }
        _ => None,
    }
}

fn parse_error_suffix(diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .find(|diagnostic| diagnostic.is_error())
        .map(|diagnostic| format!(": {}", diagnostic.message))
        .unwrap_or_default()
}

#[cfg(test)]
mod define_validation_tests {
    use wake_ecma_minify::ValidatedDefineValue;

    use super::*;

    #[test]
    fn define_values_are_parsed_and_primitives_are_typed() {
        let values = validate_defines(&[
            ("DEBUG".into(), "false".into()),
            ("process.env.MODE".into(), "'production'".into()),
            ("CONFIG".into(), "{answer: 42}".into()),
        ])
        .expect("valid definitions");

        assert!(matches!(
            values[0].value,
            ValidatedDefineValue::Primitive(ConstVal::Bool(false))
        ));
        assert!(matches!(
            &values[1].value,
            ValidatedDefineValue::Primitive(ConstVal::Str(value)) if value == "production"
        ));
        assert!(matches!(
            &values[2].value,
            ValidatedDefineValue::Expression(value) if value.source() == "{answer: 42}"
        ));
    }

    #[test]
    fn define_validation_rejects_non_static_keys_and_statement_injection() {
        assert!(validate_defines(&[("config[key]".into(), "1".into())]).is_err());
        assert!(
            validate_defines(&[("DEBUG".into(), "false);globalThis.injected=true".into())])
                .is_err()
        );
        assert!(
            validate_defines(&[
                ("DEBUG".into(), "true".into()),
                ("DEBUG".into(), "false".into())
            ])
            .is_err()
        );
    }
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

/// Optimizer task output. The owned program is process-local; only stable retained target ids are
/// persisted. Hashing uses the optimizer's stable fingerprint rather than arena addresses.
struct OptimizeArtifact {
    optimized: Option<Arc<OptimizedProgram>>,
    retained_module_ids: Arc<Vec<u32>>,
    css: Option<Arc<String>>,
    inject_style: bool,
    style_seed: Option<String>,
    diagnostics: Vec<Diagnostic>,
}

impl Hash for OptimizeArtifact {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.optimized
            .as_ref()
            .map(|program| program.fingerprint())
            .hash(state);
        self.retained_module_ids.hash(state);
        self.css.hash(state);
        self.inject_style.hash(state);
        self.style_seed.hash(state);
        for diagnostic in &self.diagnostics {
            format!("{diagnostic:?}").hash(state);
        }
    }
}

/// Byte-identical body plus module-local mapping facts produced by the same token walk. Mapping
/// facts are always present, making source-map enablement a downstream-only operation.
pub(crate) struct EmittedBody {
    pub(crate) code: Arc<String>,
    pub(crate) mapping_facts: Arc<ModuleMappings>,
    pub(crate) discarded_static_requests: Arc<Vec<GeneratedDiscardedStaticRequest>>,
    pub(crate) css: Option<Arc<String>>,
    cacheable: bool,
    /// WAKE_TIMING-only aggregation payload; deliberately excluded from the task fingerprint.
    sealed_trivial: bool,
    codegen_nanos: u64,
}

fn hash_module_mappings<H: Hasher>(map: &ModuleMappings, state: &mut H) {
    map.mappings.len().hash(state);
    for mapping in &map.mappings {
        mapping.gen_line.hash(state);
        mapping.gen_col.hash(state);
        mapping.src_index.hash(state);
        mapping.src_offset.hash(state);
        mapping.name_index.hash(state);
        mapping.is_unmapped.hash(state);
    }
    map.names.hash(state);
}

impl Hash for EmittedBody {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.code.hash(state);
        hash_module_mappings(&self.mapping_facts, state);
        self.discarded_static_requests.hash(state);
        self.css.hash(state);
        self.cacheable.hash(state);
    }
}

struct SourceMapFacts(Arc<ModuleMappings>);

impl Hash for SourceMapFacts {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_module_mappings(&self.0, state);
    }
}

#[cfg(test)]
mod emitted_body_hash_tests {
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::sync::Arc;

    use wake_ecma_codegen::{GeneratedDiscardedStaticRequest, Mapping, ModuleMappings};

    use super::EmittedBody;

    fn fingerprint(body: &EmittedBody) -> u64 {
        let mut hasher = DefaultHasher::new();
        body.hash(&mut hasher);
        hasher.finish()
    }

    fn body(name_index: Option<u32>, name: &str) -> EmittedBody {
        EmittedBody {
            code: Arc::new("let a=1;".into()),
            mapping_facts: Arc::new(ModuleMappings {
                mappings: vec![Mapping {
                    gen_line: 0,
                    gen_col: 4,
                    src_index: 0,
                    src_offset: 4,
                    name_index,
                    is_unmapped: false,
                }],
                names: vec![name.into()],
            }),
            discarded_static_requests: Arc::new(Vec::new()),
            css: None,
            cacheable: true,
            sealed_trivial: false,
            codegen_nanos: 0,
        }
    }

    #[test]
    fn mapping_names_and_name_index_participate_in_the_body_fingerprint() {
        let baseline = fingerprint(&body(Some(0), "descriptiveName"));
        assert_ne!(baseline, fingerprint(&body(None, "descriptiveName")));
        assert_ne!(baseline, fingerprint(&body(Some(0), "differentName")));
    }

    #[test]
    fn unmapped_segments_participate_in_the_body_fingerprint() {
        let baseline = body(None, "descriptiveName");
        let mut changed = body(None, "descriptiveName");
        Arc::make_mut(&mut changed.mapping_facts).mappings[0].is_unmapped = true;
        assert_ne!(fingerprint(&baseline), fingerprint(&changed));
    }

    #[test]
    fn discarded_static_request_ranges_participate_in_the_body_fingerprint() {
        let baseline = body(None, "descriptiveName");
        let mut changed = body(None, "descriptiveName");
        Arc::make_mut(&mut changed.discarded_static_requests).push(
            GeneratedDiscardedStaticRequest {
                start: 0,
                end: 3,
                target_module_id: 7,
            },
        );
        assert_ne!(fingerprint(&baseline), fingerprint(&changed));
    }
}

fn optimize_request(
    parse_vc: Vc<ParsedModule>,
    linker_vc: Vc<OptimizeLinkerData>,
    css_input_vc: Vc<CssCodegenInput>,
    options_input_vc: Vc<OptimizeOptionsInput>,
    interner: Arc<Interner>,
) -> (Vc<OptimizeArtifact>, Arc<OptimizeArtifact>) {
    let id = TaskId::of(
        "wake_bundler",
        "optimize",
        &[
            parse_vc.arg_ref(),
            linker_vc.arg_ref(),
            css_input_vc.arg_ref(),
            options_input_vc.arg_ref(),
        ],
    );
    let vc = query(id, move || {
        let parsed = parse_vc.read();
        let css_input = css_input_vc.read();
        let options = options_input_vc.read();
        let minify = options.minify;
        let drop_console = options.drop_console;
        let drop_debugger = options.drop_debugger;
        let module_name = &options.module_name;
        let one_shot = options.one_shot;
        let define = match &options.validated_define {
            Ok(define) => define,
            Err(message) => {
                return OptimizeArtifact {
                    optimized: None,
                    retained_module_ids: Arc::new(Vec::new()),
                    css: None,
                    inject_style: css_input.inject_style,
                    style_seed: css_input.seed.clone(),
                    diagnostics: vec![Diagnostic::error(format!(
                        "invalid define configuration: {message}"
                    ))],
                };
            }
        };
        let data = linker_vc.read();
        // The graph communicates its stable, persisted export-name keep set at the module
        // boundary. The optimizer resolves those module-local names to semantic SymbolIds and
        // computes the rooted declaration closure before it commits binding removals; codegen
        // only consumes those proofs.
        let keep = data.export_keep.as_ref();
        // 所有 codegen 配置都来自 Vc input；同一 bundler 实例在 build 之间切换 dev/prod 时会
        // 精确失效模块体，持久缓存仍由 body_key 的对应盐隔离。
        parsed.ast.with_ast(|program| {
            // —— `@crab-dev/css` 静态编译：先生成可信结构化编辑，再交给 optimizer ——
            // 产出「标签模板 span → 类名字面量」替换 + 本模块抽取的 CSS。
            let imported: wake_css_in_js::value::Scope = css_input.scope.iter().cloned().collect();
            let cij = css_input.seed.as_deref().map(|seed| {
                wake_css_in_js::transform(program, &interner, &parsed.source, seed, &imported)
            });

            let mut optimize_input = OptimizeInput::new(&parsed.source);
            optimize_input.minify = minify;
            // Bundled ESM is lowered through the same CommonJS-shaped registry as preserved CJS
            // artifacts. Let the optimizer own import namespace allocation and rewrite named /
            // default reads by semantic SymbolId so they remain live across module cycles instead
            // of becoming initialization-time property snapshots.
            optimize_input.set_bundled_commonjs(true);
            optimize_input
                .set_bundled_internal_esm_dependencies(data.internal_esm_deps.iter().cloned());
            optimize_input.defines = define.clone();
            optimize_input.drop_console = drop_console;
            optimize_input.drop_debugger = drop_debugger;
            optimize_input.module_name = Some(module_name.clone());
            optimize_input.dependencies = parsed
                .deps
                .iter()
                .map(|dependency| OptimizeDependency {
                    specifier: dependency.specifier.clone(),
                    origin: if dependency.span.is_dummy() {
                        NodeOrigin::Synthetic {
                            anchor: None,
                            reason: SyntheticReason::LoweringGenerated,
                        }
                    } else {
                        NodeOrigin::Source(dependency.span)
                    },
                })
                .collect();
            optimize_input.reserved_names = vec!["m".into(), "$".into(), "_r".into()];
            // `None` is the optimizer's explicit "preserve every public export" contract. Exact
            // linker sets cross the boundary only as stable public names; the optimizer resolves
            // them to parser-generation SymbolIds through the same semantic model used by owned
            // lowering, so no duplicate parser analysis or process-local ID escapes this task.
            optimize_input.linker_liveness = keep.map(|keep| {
                LinkerExportLiveness::from_parts(
                    data.module_id,
                    keep.retained_export_names.iter().cloned(),
                    keep.observed_export_names.iter().cloned(),
                    keep.preserve_export_star,
                )
            });
            if let Some(result) = &cij {
                for (&span, replacement) in &result.replacements {
                    let parsed_replacement = parse(replacement, &interner, SourceType::Module);
                    optimize_input.add_expression_edit(TrustedExpressionEdit::from_parsed_program(
                        span,
                        &parsed_replacement.module,
                        &interner,
                    ));
                }
                optimize_input
                    .extend_statement_removals(result.removable_import_spans.iter().copied());
                optimize_input
                    .extend_binding_removals(result.removable_import_binding_spans.iter().copied());
            }

            let optimized = match if one_shot {
                optimize_one_shot(parsed.ast.clone(), &interner, &optimize_input)
            } else {
                optimize(parsed.ast.clone(), &interner, &optimize_input)
            } {
                Ok(optimized) => optimized,
                Err(error) => {
                    let mut diagnostics = cij
                        .as_ref()
                        .map(|result| result.diagnostics.clone())
                        .unwrap_or_default();
                    diagnostics.push(Diagnostic::error(error.to_string()));
                    return OptimizeArtifact {
                        optimized: None,
                        retained_module_ids: Arc::new(Vec::new()),
                        css: None,
                        inject_style: css_input.inject_style,
                        style_seed: css_input.seed.clone(),
                        diagnostics,
                    };
                }
            };
            let mut retained_module_ids: Vec<u32> = optimized
                .retained_dependencies()
                .iter()
                .filter_map(|dependency| {
                    data.deps.iter().find_map(|(specifier, id)| {
                        (specifier == &dependency.specifier).then_some(*id)
                    })
                })
                .collect();
            retained_module_ids.sort_unstable();
            retained_module_ids.dedup();

            let css = cij
                .as_ref()
                .filter(|result| !result.css.is_empty())
                .map(|result| Arc::new(result.css.clone()));
            OptimizeArtifact {
                optimized: Some(Arc::new(optimized)),
                retained_module_ids: Arc::new(retained_module_ids),
                css,
                inject_style: css_input.inject_style,
                style_seed: css_input.seed.clone(),
                diagnostics: cij.map(|result| result.diagnostics).unwrap_or_default(),
            }
        })
    });
    let value = vc.read();
    (vc, value)
}

fn emit_body_request(
    optimized_vc: Vc<OptimizeArtifact>,
    linker_vc: Vc<EmitLinkerData>,
    interner: Arc<Interner>,
    codegen_exec_counts: CodegenExecCounts,
    codegen_counter_shard: usize,
) -> (Vc<EmittedBody>, Arc<EmittedBody>) {
    let id = TaskId::of(
        "wake_bundler",
        "emit_body",
        &[optimized_vc.arg_ref(), linker_vc.arg_ref()],
    );
    let vc = query(id, move || {
        let artifact = optimized_vc.read();
        let data = linker_vc.read();
        emit_body(
            &artifact,
            &data,
            &interner,
            &codegen_exec_counts,
            codegen_counter_shard,
        )
    });
    let value = vc.read();
    (vc, value)
}

fn emit_body(
    artifact: &OptimizeArtifact,
    data: &EmitLinkerData,
    interner: &Interner,
    codegen_exec_counts: &[CachePadded<AtomicU64>],
    codegen_counter_shard: usize,
) -> EmittedBody {
    codegen_exec_counts[codegen_counter_shard].fetch_add(1, Ordering::Relaxed);
    let Some(optimized) = artifact.optimized.as_ref() else {
        return EmittedBody {
            code: Arc::new(String::new()),
            mapping_facts: Arc::new(ModuleMappings::default()),
            discarded_static_requests: Arc::new(Vec::new()),
            css: None,
            cacheable: false,
            sealed_trivial: false,
            codegen_nanos: 0,
        };
    };
    let linker = Linker {
        map: SpecifierLookup::new(&data.deps),
        dyn_chunk: SpecifierLookup::new(&data.dyn_chunks),
        async_ids: &data.async_deps,
    };
    let sealed_trivial = optimized.can_emit_sealed_without_finalization(data.no_esmodule);
    let codegen_started = std::time::Instant::now();
    let (mut code, mappings, discarded_static_requests) =
        codegen_optimized_with_map_and_requests(optimized, interner, &linker, data.no_esmodule);
    let codegen_nanos = codegen_started
        .elapsed()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64;
    if artifact.inject_style
        && let Some(css) = &artifact.css
    {
        append_style_injection(
            &mut code,
            css,
            artifact.style_seed.as_deref().unwrap_or("module"),
        );
    }
    EmittedBody {
        code: Arc::new(code),
        mapping_facts: Arc::new(mappings),
        discarded_static_requests: Arc::new(discarded_static_requests),
        css: (!artifact.inject_style)
            .then(|| artifact.css.clone())
            .flatten(),
        cacheable: true,
        sealed_trivial,
        codegen_nanos,
    }
}

fn source_map_facts_request(body_vc: Vc<EmittedBody>) -> Arc<ModuleMappings> {
    let id = TaskId::of("wake_bundler", "source_map_facts", &[body_vc.arg_ref()]);
    let vc = query(id, move || {
        SourceMapFacts(body_vc.read().mapping_facts.clone())
    });
    vc.read().0.clone()
}

/// 给模块体追加带稳定 module id 的 `<style>` upsert/remove（dev 路径）。每个 bundle runtime
/// 以其 `__wake_require__` 函数对象作为 WeakMap owner：生成代码保持确定，独立 bundle 即使包含
/// 相同模块路径也不会互相覆盖，动态 chunk 则自然复用入口 runtime 的 owner。
///
/// `typeof document` 守卫使 SSR / node 下静默跳过。
fn append_style_injection(js: &mut String, css: &str, module_id: &str) {
    let style_id = format!("crab-css-{:016x}", stable_text_hash(module_id));
    js.push_str("\nif (typeof document !== \"undefined\") {\n");
    js.push_str("  var __wake_cij_owners__ = document.__wake_css_styles__ || (document.__wake_css_styles__ = new WeakMap());\n");
    js.push_str("  var __wake_cij_registry__ = __wake_cij_owners__.get(__wake_require__);\n");
    js.push_str("  if (!__wake_cij_registry__) { __wake_cij_registry__ = {}; __wake_cij_owners__.set(__wake_require__, __wake_cij_registry__); }\n");
    js.push_str("  var __wake_cij_id__ = ");
    crate::loader::push_js_string(js, &style_id);
    js.push_str(";\n  var __wake_cij__ = __wake_cij_registry__[__wake_cij_id__];\n");
    if css.is_empty() {
        js.push_str("  if (__wake_cij__) { if (__wake_cij__.remove) __wake_cij__.remove(); delete __wake_cij_registry__[__wake_cij_id__]; }\n");
    } else {
        js.push_str("  if (!__wake_cij__) { __wake_cij__ = document.createElement(\"style\"); __wake_cij_registry__[__wake_cij_id__] = __wake_cij__; document.head.appendChild(__wake_cij__); }\n");
        js.push_str("  __wake_cij__.textContent = ");
        crate::loader::push_js_string(js, css);
        js.push_str(";\n");
    }
    js.push_str("}\n");
}

fn stable_text_hash(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
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

/// 死模块消除：从 `entry_id` 出发，沿优化器报告的保留依赖边做单调 BFS。
///
/// 边是在源 AST/IR 上按 span 判活后产生的结构化数据；发射后代码的字符串、注释或字面量
/// 不参与模块可达性判定。
fn live_modules(edges: &[(u32, Arc<Vec<u32>>)], entry_id: u32) -> FxHashSet<u32> {
    let edges: FxHashMap<u32, &[u32]> = edges
        .iter()
        .map(|(id, targets)| (*id, targets.as_slice()))
        .collect();
    let mut live: FxHashSet<u32> = FxHashSet::default();
    let mut stack = vec![entry_id];
    while let Some(id) = stack.pop() {
        if !live.insert(id) {
            continue;
        }
        if let Some(refs) = edges.get(&id) {
            for &r in *refs {
                if !live.contains(&r) {
                    stack.push(r);
                }
            }
        }
    }
    live
}

#[cfg(test)]
mod retained_dependency_liveness_tests {
    use std::sync::Arc;

    use wake_common::FxHashSet;

    use super::live_modules;

    #[test]
    fn follows_only_structured_optimizer_edges() {
        let edges = vec![
            (0, Arc::new(vec![1, 2])),
            (1, Arc::new(vec![3])),
            (2, Arc::new(Vec::new())),
            (3, Arc::new(vec![2])),
            // A generated body could contain text resembling a require for this module, but no
            // structured edge reaches it, so it must remain dead.
            (99, Arc::new(vec![0])),
        ];

        assert_eq!(live_modules(&edges, 0), FxHashSet::from_iter([0, 1, 2, 3]));
    }

    #[test]
    fn cycles_terminate_and_keep_each_reachable_module_once() {
        let edges = vec![(4, Arc::new(vec![5])), (5, Arc::new(vec![4]))];
        assert_eq!(live_modules(&edges, 4), FxHashSet::from_iter([4, 5]));
    }
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

/// Redirect exact generated `require_name(N)` calls whose canonical decimal IDs are in
/// `redirected` to one target ID. This preserves the old textual rewrite contract, but performs
/// one scan and one allocation instead of one full `String::replace` per merged module ID.
fn redirect_require_targets(
    body: &str,
    require_name: &str,
    redirected: &FxHashSet<u32>,
    target: u32,
) -> String {
    if redirected.is_empty() {
        return body.to_string();
    }

    let needle = format!("{require_name}(");
    let target = target.to_string();
    let bytes = body.as_bytes();
    let mut search = 0;
    let mut copied = 0;
    let mut rewritten = None::<String>;
    while let Some(relative) = body[search..].find(&needle) {
        let start = search + relative;
        let digits_start = start + needle.len();
        let mut digits_end = digits_start;
        while digits_end < bytes.len() && bytes[digits_end].is_ascii_digit() {
            digits_end += 1;
        }
        let digits = &body[digits_start..digits_end];
        let canonical_decimal = !digits.is_empty()
            && (digits.len() == 1 || !digits.starts_with('0'))
            && digits_end < bytes.len()
            && bytes[digits_end] == b')';
        let redirect = canonical_decimal
            .then(|| digits.parse::<u32>().ok())
            .flatten()
            .is_some_and(|id| redirected.contains(&id));
        if redirect {
            let output = rewritten.get_or_insert_with(|| String::with_capacity(body.len()));
            output.push_str(&body[copied..digits_start]);
            output.push_str(&target);
            output.push(')');
            copied = digits_end + 1;
            search = copied;
        } else {
            search = digits_start;
        }
    }

    let Some(mut output) = rewritten else {
        return body.to_string();
    };
    output.push_str(&body[copied..]);
    output
}

/// 尝试把 `abs` 处的 require 识别为 barrel re-export 并返回待删区间 `[const 起点, for 循环结束)`。
/// 匹配：`const _wm{VAR} = __wake_require__(id);for (const _k in _wm{VAR}) if (_k !== "default") exports[_k] = _wm{VAR}[_k];`
/// 全为**局部**（有界回看/前看）检测，故整趟 strip 保持 O(body)。逐字节复刻原逐-id 版的判定。
fn try_barrel(body: &str, bytes: &[u8], abs: usize, call_end: usize) -> Option<(usize, usize)> {
    // require 前必须是 ` = `（空格=空格）。
    if abs < 3 || bytes.get(abs - 3..abs) != Some(b" = ") {
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
    if ds < 3 || bytes.get(ds - 3..ds) != Some(b"_wm") {
        return None;
    }
    let wm_start = ds - 3;
    if wm_start < 6 || bytes.get(wm_start - 6..wm_start) != Some(b"const ") {
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

/// 移除对 hoisted 模块的 generated barrel re-export 整段：
/// `const _wmX = __wake_require__(N);for (..._wmX...)`。
///
/// 独立 static request 的删除已由 typed finalizer + generated byte range 拥有；不能在这里
/// 从文本猜测，否则 `";__wake_require__(N);"` 之类的用户字符串会被误删。
///
/// 原实现对每个 hoisted id（可达上千）各 `format!`+全串 `find`/重建一遍——O(id 数 × body)，且被少数
/// 引用极多候选的巨型 barrel/聚合模块放大到几十毫秒。此版单遍扫描每个 require 调用，就地按局部上下文
/// 分类 generated barrel，收集**互不相交**的删除区间后一次性重建。
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

fn inline_regs_need_registry_temp(inline_regs: &[String]) -> bool {
    inline_regs.iter().any(|reg| {
        let compact = compact_reg_body(reg);
        let compact = compact.trim().trim_end_matches(';').trim_end();
        exact_reg_assignment(compact).is_some()
    })
}

#[cfg(test)]
mod inline_reg_tests {
    use super::{
        append_inline_regs, exported_names, inline_regs_need_registry_temp,
        redirect_require_targets, replace_eager_discarded_static_requests,
        strip_hoisted_requires_and_barrels, strip_standalone_requires,
    };
    use wake_common::FxHashSet;
    use wake_ecma_codegen::GeneratedDiscardedStaticRequest;

    #[test]
    fn replaces_only_proven_requests_to_eager_targets_without_rewriting_sequence_grammar() {
        let body = "__wake_require__(1),value,__wake_require__(2);";
        let first_end = "__wake_require__(1)".len();
        let second_start = body.find("__wake_require__(2)").unwrap();
        let requests = [
            GeneratedDiscardedStaticRequest {
                start: 0,
                end: first_end as u32,
                target_module_id: 1,
            },
            GeneratedDiscardedStaticRequest {
                start: second_start as u32,
                end: (second_start + "__wake_require__(2)".len()) as u32,
                target_module_id: 2,
            },
        ];
        let eager = FxHashSet::from_iter([1]);

        assert_eq!(
            replace_eager_discarded_static_requests(body, &requests, &eager),
            "0,value,__wake_require__(2);"
        );
        assert_eq!(
            replace_eager_discarded_static_requests(
                body,
                &[GeneratedDiscardedStaticRequest {
                    start: 20,
                    end: 10,
                    target_module_id: 1,
                }],
                &eager,
            ),
            body,
            "malformed cached metadata must make the optimization a no-op"
        );
    }

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
        assert!(!inline_regs_need_registry_temp(&[
            String::new(),
            " ; ".to_string()
        ]));
    }

    #[test]
    fn declares_the_registry_temp_when_an_inlined_registry_assignment_needs_it() {
        assert!(inline_regs_need_registry_temp(&[String::from(
            "globalThis.__reg || (globalThis.__reg = {});globalThis.__reg.a = 1;"
        )]));
    }

    #[test]
    fn compact_body_scanners_preserve_utf8_and_never_slice_inside_a_character() {
        let names = exported_names("const label=\"中文件exports\";exports[\"ok\"] = 1;");
        assert!(names.contains("ok"));

        let concat_members = FxHashSet::from_iter([1]);
        assert_eq!(
            strip_standalone_requires(
                "const label=\"中文\";_r(1);exports.value = label;",
                &concat_members,
            ),
            "const label=\"中文\";exports.value = label;"
        );
        assert_eq!(
            strip_standalone_requires("_r(1),globalThis.effect=1;", &concat_members),
            "_r(1),globalThis.effect=1;",
            "a require at the head of a sequence is not a standalone statement"
        );
        assert_eq!(
            strip_standalone_requires("_r(2);globalThis.effect=1;", &concat_members),
            "_r(2);globalThis.effect=1;",
            "a standalone factory dependency must execute when it is outside this concat"
        );

        let mut hoisted = FxHashSet::default();
        hoisted.insert(1);
        let embedded = "const label=\"😀__wake_require__(1)\";";
        assert_eq!(
            strip_hoisted_requires_and_barrels(embedded, &hoisted),
            embedded
        );
        let standalone_text = "globalThis.label=\";__wake_require__(1);\";";
        assert_eq!(
            strip_hoisted_requires_and_barrels(standalone_text, &hoisted),
            standalone_text,
            "standalone request-shaped text is not a structural deletion proof"
        );
    }

    #[test]
    fn redirects_generated_require_ids_in_one_exact_textual_pass() {
        let redirected = FxHashSet::from_iter([1, 42]);
        let body = "_r(1);_r(11);const label=\"中文_r(42)\";_r(42);_r(01);_r(x);";

        assert_eq!(
            redirect_require_targets(body, "_r", &redirected, 2002),
            "_r(2002);_r(11);const label=\"中文_r(2002)\";_r(2002);_r(01);_r(x);"
        );
        assert_eq!(
            redirect_require_targets(body, "_r", &FxHashSet::default(), 2002),
            body
        );
    }
}

#[test]
fn detects_only_modules_inside_dependency_cycles() {
    let modules = vec![
        (1, "const a = __wake_require__(2);".to_string()),
        (2, "const b = __wake_require__(1);".to_string()),
        (3, "const c = __wake_require__(2);".to_string()),
        (4, "const d = __wake_require__(4);".to_string()),
    ];
    let cyclic = cyclic_module_ids(&modules);
    assert!(cyclic.contains(&1));
    assert!(cyclic.contains(&2));
    assert!(!cyclic.contains(&3));
    assert!(cyclic.contains(&4));
}

#[test]
fn detects_cycles_created_only_by_collapsing_distant_modules_into_concat() {
    // 原图无环：barrel -> icon -> create -> helper。icon/create 已因其它安全约束独立；若
    // barrel/helper 被折叠为同一个 concat，就会凭空形成 concat -> icon -> create -> concat。
    let modules = vec![
        (0, "const barrel = __wake_require__(1);".to_string()),
        (1, "const icon = __wake_require__(2);".to_string()),
        (2, "const create = __wake_require__(3);".to_string()),
        (3, "const helper = __wake_require__(4);".to_string()),
        (4, "exports.helper = 1;".to_string()),
        (5, "exports.unrelated = 1;".to_string()),
    ];
    assert!(cyclic_module_ids(&modules).is_empty());

    let standalone = FxHashSet::from_iter([2, 3]);
    let demoted = concat_cycle_source_ids(&modules, 0, &standalone);

    assert_eq!(demoted, FxHashSet::from_iter([1]));
    assert!(!demoted.contains(&4), "下游 helper 仍可安全留在 concat");
    assert!(!demoted.contains(&5), "无关模块不应被降级");
}

/// 紧凑模块 body 中的运行时引用：`__wake_require__`→`_r`，自由变量 `exports`→`$`。
///
/// **`module.exports` 必须改写成 `m.exports`，不能是 `m.$`**：包装器
/// `function(m,$,_r)` 由 `.call(module.exports, module, module.exports, __wake_require__)`
/// 调用，`$` 是 exports 对象的**值**、`m` 才是 module 本身。写成 `m.$` 只是在 module 上挂了个
/// 名为 `$` 的无关属性，`module.exports` 从未被重新赋值 → 该模块导出恒为空对象
/// （曾致任何 `module.exports = X` 形态的 CJS 包整包失效）。
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
        // Runtime property calls (for example the trusted `import.meta.url` lowering to
        // `__wake_require__.metaUrl()`) are emitted as an opaque expression today. The compact
        // body rewrite only shortens direct `__wake_require__(...)` calls, so shortening the
        // wrapper parameter here would leave that property read unbound. Keep the canonical
        // parameter locally until all such compiler-generated reads are structural IR nodes.
        let requires_runtime_properties =
            bodies.iter().any(|body| body.contains("__wake_require__."));
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
        let module = pick("m");
        let exports = pick("$");
        let require = if requires_runtime_properties {
            "__wake_require__".to_string()
        } else {
            pick("_r")
        };
        Self {
            module,
            exports,
            require,
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
/// 深度优先按静态 `deps` 原序展开。动态 import 并不参与入口求值；把它先遍历会让 lazy
/// 子图提前占据 shared module 的 `seen` 槽位，从而颠倒 entry CSS 的 cascade 顺序。
fn css_emission_order(modules: &FxHashMap<u32, ModuleRec>, entry_id: u32) -> FxHashMap<u32, u32> {
    fn visit_static_postorder(
        root: u32,
        modules: &FxHashMap<u32, ModuleRec>,
        seen: &mut FxHashSet<u32>,
        order: &mut FxHashMap<u32, u32>,
        next: &mut u32,
    ) {
        if !seen.insert(root) {
            return;
        }
        let mut stack: Vec<(u32, usize)> = vec![(root, 0)];
        while let Some((id, dependency_index)) = stack.pop() {
            let Some(module) = modules.get(&id) else {
                order.insert(id, *next);
                *next += 1;
                continue;
            };
            if dependency_index < module.deps.len() {
                stack.push((id, dependency_index + 1));
                let dependency = &module.deps[dependency_index];
                if dependency.kind == DependencyKind::DynamicImport {
                    continue;
                }
                if let Some(&(_, child)) = module
                    .dep_ids
                    .iter()
                    .find(|(specifier, _)| specifier == &dependency.specifier)
                    && seen.insert(child)
                {
                    stack.push((child, 0));
                }
            } else {
                order.insert(id, *next);
                *next += 1;
            }
        }
    }

    let mut order: FxHashMap<u32, u32> = FxHashMap::default();
    let mut seen: FxHashSet<u32> = FxHashSet::default();
    let mut next = 0u32;
    if modules.contains_key(&entry_id) {
        visit_static_postorder(entry_id, modules, &mut seen, &mut order, &mut next);
    }

    // Lazy roots are not part of entry evaluation, but their own static dependency order matters
    // once activated. Visit every remaining component in deterministic module-id order.
    let mut remaining = modules.keys().copied().collect::<Vec<_>>();
    remaining.sort_unstable();
    for id in remaining {
        if !seen.contains(&id) {
            visit_static_postorder(id, modules, &mut seen, &mut order, &mut next);
        }
    }
    order
}

/// Turn extracted module styles into chunk-owned artifacts. The global dependency postorder is
/// preserved inside every chunk; cross-chunk order follows the JavaScript chunk dependency DAG,
/// which the runtime resolves before loading the dependent chunk's own styles.
fn build_style_artifacts(
    modules: &FxHashMap<u32, ModuleRec>,
    entry_id: u32,
    chunk_graph: Option<&ChunkGraph>,
    collected_css: &mut [(u32, String)],
    minify: bool,
) -> (Vec<OutputAsset>, BTreeMap<u32, Vec<String>>) {
    if collected_css.is_empty() {
        return (Vec::new(), BTreeMap::new());
    }
    let order = css_emission_order(modules, entry_id);
    let fallback = u32::MAX;
    collected_css.sort_by_key(|(id, _)| (*order.get(id).unwrap_or(&fallback), *id));

    let mut css_by_chunk: BTreeMap<u32, String> = BTreeMap::new();
    for (module_id, text) in collected_css.iter() {
        let chunk_id = chunk_graph
            .and_then(|graph| graph.module_chunk.get(module_id).copied())
            .unwrap_or(0);
        let output = css_by_chunk.entry(chunk_id).or_default();
        output.push_str(text);
        if !text.ends_with('\n') {
            output.push('\n');
        }
    }

    let mut assets = Vec::with_capacity(css_by_chunk.len());
    let mut files = BTreeMap::new();
    for (chunk_id, css) in css_by_chunk {
        let css = if minify { wake_css::minify(&css) } else { css };
        // Entry CSS keeps the stable `styles.<hash>.css` public contract even when the
        // JavaScript graph is split and its source entry has a project-specific stem.
        let chunk_name = if chunk_id == 0 {
            "styles"
        } else {
            chunk_graph
                .and_then(|graph| graph.chunks.iter().find(|chunk| chunk.id == chunk_id))
                .map(|chunk| chunk.name.as_str())
                .unwrap_or("styles")
        };
        let file_name = format!("{chunk_name}.{}.css", hash8(&css));
        files.insert(chunk_id, vec![file_name.clone()]);
        assets.push(OutputAsset {
            file_name,
            bytes: css.into_bytes(),
            is_css: true,
        });
    }
    (assets, files)
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
    // Mutable ESM bindings and bundled re-exports use getter-backed properties. Compare the
    // emitted literal spelling: equal public names have equal escaped spellings, while a partial
    // parse can only conservatively create an extra conflict/standalone module.
    let mut getter_rest = body;
    const GETTER: &str = "Object.defineProperty(exports,";
    while let Some(position) = getter_rest.find(GETTER) {
        getter_rest = &getter_rest[position + GETTER.len()..];
        let literal = getter_rest.trim_start();
        let Some(quote) = literal
            .chars()
            .next()
            .filter(|quote| matches!(quote, '\'' | '"'))
        else {
            continue;
        };
        let value = &literal[quote.len_utf8()..];
        let Some(end) = value.find(quote) else {
            break;
        };
        out.insert(value[..end].to_string());
        getter_rest = &value[end + quote.len_utf8()..];
    }
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while let Some(pos) = body[i..].find("exports") {
        let at = i + pos;
        let after = at + "exports".len();
        i = after;
        // `module.exports` 不是这里的目标。
        if at >= "module.".len() && bytes.get(at - "module.".len()..at) == Some(b"module.") {
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

/// Internal modules whose exported namespace object is observed as an object, rather than only
/// through one statically named property.
///
/// Scope concatenation normally lets several ESM factories share one compact `exports` object.
/// That is valid for target-aware named/default reads, but it is observably wrong for namespace
/// imports, star re-exports, dynamic import, and `require`: the shared object would expose exports
/// belonging to unrelated concat members. These facts come from the current parser generation's
/// liveness model and resolved graph ids; no `SymbolId` crosses the cache boundary.
fn namespace_identity_module_ids(modules: &FxHashMap<u32, ModuleRec>) -> FxHashSet<u32> {
    let mut targets = FxHashSet::default();
    for module in modules.values() {
        let resolved = module
            .dep_ids
            .iter()
            .map(|(specifier, id)| (specifier.as_str(), *id))
            .collect::<FxHashMap<_, _>>();
        let mut retain = |specifier: &str| {
            if let Some(id) = resolved.get(specifier) {
                targets.insert(*id);
            }
        };

        if let Some(liveness) = &module.liveness {
            for (_, specifier) in &liveness.namespace_imports {
                retain(specifier);
            }
            for specifier in &liveness.reexport_star {
                retain(specifier);
            }
            for (_, specifier) in &liveness.ns_reexports {
                retain(specifier);
            }
        }

        // Both forms return the target's namespace object at runtime. Keeping their targets
        // factory-owned is also the conservative fallback for modules lacking liveness facts.
        for dependency in &module.deps {
            if matches!(
                dependency.kind,
                DependencyKind::DynamicImport | DependencyKind::Require
            ) {
                retain(&dependency.specifier);
            }
        }
    }
    targets
}

/// 模块体是否**整体重新赋值** `module.exports`（`module.exports = X`），而非只挂属性
/// （`module.exports.foo = X`）。
///
/// 这类 CJS 模块**不能**并入 scope-hoist 的 concat：concat 让所有被合并模块共享同一个
/// exports 对象 `$`，而整体赋值会把 `module.exports` 换成**另一个**对象——此后
/// 该模块的导出与其它模块写入的 `$` 分属两个对象，必丢其一
/// （曾致 CJS 与同组 ESM 模块的导出互相覆盖丢失）。
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

/// 移除指向同一 concat 成员的独立 `_r(N);` 调用（成员 body 已在 concat 中按序执行）。
/// 指向独立 factory 的请求必须保留，否则它的顶层副作用永远不会执行。
fn strip_standalone_requires(body: &str, concat_members: &FxHashSet<u32>) -> String {
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
                    let target = body[i + 3..j].parse::<u32>().ok();
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
                    if k < n
                        && bytes[k] == b';'
                        && target.is_some_and(|target| concat_members.contains(&target))
                    {
                        i = k + 1;
                        continue;
                    }
                }
            }
        }
        let ch = body[i..]
            .chars()
            .next()
            .expect("i remains on a UTF-8 character boundary");
        result.push(ch);
        i += ch.len_utf8();
    }
    result
}

/// 返回处于依赖环中的模块 id。循环模块必须保留独立 factory，不能共享 concat exports。
fn cyclic_module_ids(modules: &[(u32, String)]) -> FxHashSet<u32> {
    let ids: FxHashSet<u32> = modules.iter().map(|(id, _)| *id).collect();
    let mut edges: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
    let mut reverse: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
    for (id, body) in modules {
        let mut targets = Vec::new();
        for_each_require(body, |_, target, _| {
            if ids.contains(&target) {
                targets.push(target);
            }
        });
        targets.sort_unstable();
        targets.dedup();
        for target in &targets {
            reverse.entry(*target).or_default().push(*id);
        }
        edges.insert(*id, targets);
    }

    fn visit(
        id: u32,
        graph: &FxHashMap<u32, Vec<u32>>,
        seen: &mut FxHashSet<u32>,
        order: &mut Vec<u32>,
    ) {
        if !seen.insert(id) {
            return;
        }
        if let Some(next) = graph.get(&id) {
            for target in next {
                visit(*target, graph, seen, order);
            }
        }
        order.push(id);
    }

    fn collect(
        id: u32,
        graph: &FxHashMap<u32, Vec<u32>>,
        seen: &mut FxHashSet<u32>,
        component: &mut Vec<u32>,
    ) {
        if !seen.insert(id) {
            return;
        }
        component.push(id);
        if let Some(next) = graph.get(&id) {
            for target in next {
                collect(*target, graph, seen, component);
            }
        }
    }

    let mut order = Vec::with_capacity(modules.len());
    let mut seen = FxHashSet::default();
    let mut sorted_ids = ids.iter().copied().collect::<Vec<_>>();
    sorted_ids.sort_unstable();
    for id in sorted_ids {
        visit(id, &edges, &mut seen, &mut order);
    }

    let mut cyclic = FxHashSet::default();
    seen.clear();
    while let Some(id) = order.pop() {
        if seen.contains(&id) {
            continue;
        }
        let mut component = Vec::new();
        collect(id, &reverse, &mut seen, &mut component);
        let self_cycle = component.len() == 1
            && edges
                .get(&component[0])
                .is_some_and(|targets| targets.contains(&component[0]));
        if component.len() > 1 || self_cycle {
            cyclic.extend(component);
        }
    }
    cyclic
}

/// 返回会因「把所有可合并模块折叠成同一个 concat factory」而**新造出运行时环**的
/// concat 成员。
///
/// 原始模块图可以是无环的，例如：
///
/// ```text
/// barrel (merge) -> icon (standalone) -> create (standalone) -> helper (merge)
/// ```
///
/// 若把 `barrel` 与 `helper` 合为一个 concat factory，图就变成
/// `concat -> icon -> create -> concat`。从 `create` 开始求值时，它先 require concat；concat
/// 又会急切初始化 barrel/icon，并在 `create` 尚未写出导出时拿到缓存里的空 exports。
///
/// 这里精确降级这类路径中位于 standalone **上游**的 merge 成员（例中的 barrel），保留下游
/// helper 的合并收益。设边方向为「模块 → 依赖」：
/// 1. 反向遍历所有 merge 成员，找出仍能走到 merge 的 standalone 边界；
/// 2. 再反向遍历这些边界，其上游 merge 成员就是折叠后闭环的来源。
fn concat_cycle_source_ids(
    modules: &[(u32, String)],
    entry_id: u32,
    standalone: &FxHashSet<u32>,
) -> FxHashSet<u32> {
    let ids: FxHashSet<u32> = modules.iter().map(|(id, _)| *id).collect();
    let merged: FxHashSet<u32> = ids
        .iter()
        .copied()
        .filter(|id| *id != entry_id && !standalone.contains(id))
        .collect();
    if merged.is_empty() {
        return FxHashSet::default();
    }

    // 反向边：dependency -> importers。
    let mut reverse: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
    for (id, body) in modules {
        for_each_require(body, |_, target, _| {
            if ids.contains(&target) && target != *id {
                reverse.entry(target).or_default().push(*id);
            }
        });
    }
    for importers in reverse.values_mut() {
        importers.sort_unstable();
        importers.dedup();
    }

    fn reverse_reachable(
        seeds: impl IntoIterator<Item = u32>,
        reverse: &FxHashMap<u32, Vec<u32>>,
    ) -> FxHashSet<u32> {
        let mut seen = FxHashSet::default();
        let mut stack = Vec::new();
        for seed in seeds {
            if seen.insert(seed) {
                stack.push(seed);
            }
        }
        while let Some(id) = stack.pop() {
            if let Some(importers) = reverse.get(&id) {
                for importer in importers {
                    if seen.insert(*importer) {
                        stack.push(*importer);
                    }
                }
            }
        }
        seen
    }

    // standalone（入口也永远是独立 factory）必须同时满足「它能到达某个 merge 成员」，
    // 才可能成为 concat 折叠环的中间边界。
    let can_reach_merge = reverse_reachable(merged.iter().copied(), &reverse);
    let bridge_standalones = ids
        .iter()
        .copied()
        .filter(|id| (*id == entry_id || standalone.contains(id)) && can_reach_merge.contains(id))
        .collect::<Vec<_>>();
    if bridge_standalones.is_empty() {
        return FxHashSet::default();
    }

    let can_reach_bridge = reverse_reachable(bridge_standalones, &reverse);
    merged
        .into_iter()
        .filter(|id| can_reach_bridge.contains(id))
        .collect()
}
/// 拓扑排序：按依赖顺序排列模块，确保依赖方先于被依赖方执行。
/// 解析各模块 body 中的 require 调用构建依赖图，忽略对自身和不在集合内的引用。
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

/// 生成 default import interop helper。minify 下 codegen 省略 `__esModule`，
/// 因此须改用转译 ESM 必然具有的 `default` 导出键判定。
fn interop_default_helper(name: &str, no_esmodule: bool) -> String {
    if no_esmodule {
        format!(
            "function {name}(m){{return m&&(typeof m==='object'||typeof m==='function')&&'default' in m?m.default:m}}"
        )
    } else {
        format!("function {name}(m){{return m&&m.__esModule?m.default:m}}")
    }
}

/// Generate namespace-import interop for CJS or external/unknown targets. Internal ESM imports
/// bypass this helper using the target-kind fact carried by the optimizer, so the compact
/// no-marker runtime must still build a standards-shaped CJS namespace with `.default`.
fn interop_star_helper(name: &str, _no_esmodule: bool) -> String {
    format!(
        "function {name}(m){{if(m&&m.__esModule)return m;var ns={{}};if(m!=null){{for(var k in m)if(Object.prototype.hasOwnProperty.call(m,k)&&k!='default')ns[k]=m[k]}}ns.default=m;return ns}}"
    )
}

/// Replace only finalizer-proven, generated request expressions whose target has already been
/// selected for eager inline execution. `0` deliberately preserves the surrounding typed
/// statement/sequence grammar; later minified text passes therefore never have to infer or repair
/// commas, parentheses, or semicolons.
fn replace_eager_discarded_static_requests(
    body: &str,
    requests: &[GeneratedDiscardedStaticRequest],
    eager_targets: &FxHashSet<u32>,
) -> String {
    if requests.is_empty()
        || !requests
            .iter()
            .any(|request| eager_targets.contains(&request.target_module_id))
    {
        return body.to_owned();
    }

    // Persistent metadata is untrusted input. Any malformed/overlapping/out-of-bounds range turns
    // the optimization into a conservative no-op instead of allowing a byte splice to corrupt JS.
    let mut previous_end = 0_usize;
    for request in requests {
        let start = request.start as usize;
        let end = request.end as usize;
        if start < previous_end
            || start >= end
            || end > body.len()
            || !body.is_char_boundary(start)
            || !body.is_char_boundary(end)
        {
            return body.to_owned();
        }
        previous_end = end;
    }

    let removed_bytes = requests
        .iter()
        .filter(|request| eager_targets.contains(&request.target_module_id))
        .map(|request| (request.end - request.start).saturating_sub(1) as usize)
        .sum::<usize>();
    let mut out = String::with_capacity(body.len().saturating_sub(removed_bytes));
    let mut cursor = 0_usize;
    for request in requests {
        if !eager_targets.contains(&request.target_module_id) {
            continue;
        }
        let start = request.start as usize;
        let end = request.end as usize;
        out.push_str(&body[cursor..start]);
        out.push('0');
        cursor = end;
    }
    out.push_str(&body[cursor..]);
    out
}

/// 拼接各模块（已 codegen 的）函数体 + mini runtime。
///
/// `async_ids`：async 子图（顶层 await）。非空时包装器改 `async function`、runtime 换 Promise 感知版、
/// 且**关闭模块合并**；**为空时逐字节等同于改造前的产物**（无顶层 await 的项目零影响）。
///
#[derive(Clone, Debug)]
struct BodyPlacement {
    module_id: u32,
    /// Byte offset of `emitted` in the final chunk.
    generated_offset: usize,
    /// Exact final text derived from this module. Synthetic runtime wrappers are excluded.
    emitted: String,
}

#[derive(Clone, Debug)]
struct RelativeBodyPlacement {
    module_id: u32,
    offset: usize,
    emitted: String,
}

/// `body_placements`：非 `None` 时，回填每段存活模块文本在最终 bundle 中的精确字节位置。
/// minify 的 scope concat 可让一个最终 factory 包含多个来源模块，因此同一 factory 可以产生
/// 多条 placement；合成 wrapper 不登记来源。
fn emit(
    bodies: &[(u32, Arc<String>)],
    entry_id: u32,
    minify: bool,
    no_esmodule: bool,
    block_infos: &FxHashMap<u32, ConcatBlockInfo>,
    namespace_identity_ids: &FxHashSet<u32>,
    async_ids: &FxHashSet<u32>,
    discarded_static_requests: &FxHashMap<u32, Arc<Vec<GeneratedDiscardedStaticRequest>>>,
    exec: &Executor,
    module_format: ModuleFormat,
    mut body_placements: Option<&mut Vec<BodyPlacement>>,
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
                    let requests = discarded_static_requests
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| Arc::new(Vec::new()));
                    move || {
                        let body = replace_eager_discarded_static_requests(&body, &requests, &ph);
                        (id, strip_hoisted_requires_and_barrels(&body, &ph))
                    }
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
        let mut module_fragments: FxHashMap<u32, Vec<RelativeBodyPlacement>> = FxHashMap::default();
        if async_ids.is_empty() {
            // —— 模块合并：将所有非 hoist 模块体拼接到一个闭包，避免命名冲突 ——
            // Synthetic factories occupy an ID outside the complete real-module domain. The old
            // `entry + filtered + 1000` formula could collide with a live source module (observed
            // at real id 1032 in the 2k fixture), silently redirecting that source request to the
            // concat factory.
            let concat_id = bodies
                .iter()
                .map(|(id, _)| *id)
                .max()
                .and_then(|id| id.checked_add(1))
                .expect("bundle module id space exhausted before concat factory allocation");
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
            let cyclic = cyclic_module_ids(&filtered);
            let mut standalone: FxHashSet<u32> = filtered
                .iter()
                .filter(|(id, body)| {
                    *id != entry_id
                        && (reassigns_module_exports(body)
                            || cyclic.contains(id)
                            || namespace_identity_ids.contains(id)
                            || !block_infos.get(id).is_some_and(|info| info.is_esm))
                })
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

            // 把任意 merge 成员全部折叠到一个 eager concat factory，可能在**原本无环**的图上
            // 新造出 `concat -> standalone -> concat`。典型例子是 Lucide：barrel（可合并）初始化
            // icon（因重复 default 导出而独立），icon 再依赖 createLucideIcon（独立），后者又依赖
            // concat 内的 helpers。从 createLucideIcon 开始 require 时，循环缓存会暴露尚未初始化的
            // 空 exports。只把 standalone 上游、会闭合该路径的 concat 成员降级即可保留其余合并。
            standalone.extend(concat_cycle_source_ids(&filtered, entry_id, &standalone));

            // 真正并入 concat 的模块 id（供 `_r` 垫片判定）。
            let concat_member_ids: Vec<u32> = filtered
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| *id != entry_id && !standalone.contains(id))
                .collect();
            let concat_member_set = concat_member_ids.iter().copied().collect::<FxHashSet<_>>();

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

            let mut concat_fragments = Vec::new();
            for (id, body) in &filtered {
                if *id == entry_id || standalone.contains(id) {
                    continue;
                }
                let b = strip_standalone_requires(
                    &compact_body_names(body, &runtime_names),
                    &concat_member_set,
                );
                // 块安全（ESM 且无 `var`/`this`）+ 整组 strict → 用裸 `{}` 块（strict 下块级函数声明块作用域，
                // let/const 本就块作用域 → 顶层名不跨块碰撞）。否则用 IIFE 建立真正函数作用域隔离
                // （`var` 会 hoist 出块、sloppy 下块级函数亦 hoist；曾致 React Symbol 覆盖 scheduler 计数器）。
                let has_scope_binding = ["const ", "let ", "var ", "class ", "function "]
                    .iter()
                    .any(|token| b.contains(token));
                let block_safe = all_esm && block_infos.get(id).is_some_and(|bi| bi.block_safe);
                if !has_scope_binding {
                    // 经过 tree shaking 后只剩表达式的模块不再需要隔离作用域。
                    let offset = concat_body.len();
                    concat_body.push_str(&b);
                    concat_fragments.push(RelativeBodyPlacement {
                        module_id: *id,
                        offset,
                        emitted: b,
                    });
                } else if block_safe {
                    concat_body.push('{');
                    let offset = concat_body.len();
                    concat_body.push_str(&b);
                    concat_body.push('}');
                    concat_fragments.push(RelativeBodyPlacement {
                        module_id: *id,
                        offset,
                        emitted: b,
                    });
                } else {
                    concat_body.push_str("(function(){");
                    let offset = concat_body.len();
                    concat_body.push_str(&b);
                    concat_body.push_str("})();");
                    concat_fragments.push(RelativeBodyPlacement {
                        module_id: *id,
                        offset,
                        emitted: b,
                    });
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
                let compact = compact_body_names(body, &runtime_names);
                redirect_require_targets(&compact, &runtime_names.require, &merged_ids, concat_id)
            };

            // 构建最终模块表：入口 + stubs + 独立 CJS 模块 + 合并模块
            for (id, body) in &filtered {
                if *id == entry_id {
                    let emitted = redirect(body);
                    module_fragments.insert(
                        entry_id,
                        vec![RelativeBodyPlacement {
                            module_id: entry_id,
                            offset: 0,
                            emitted: emitted.clone(),
                        }],
                    );
                    final_modules.push((entry_id, emitted));
                    break;
                }
            }
            for &sid in &ref_ids {
                final_modules.push((sid, String::new()));
            }
            for (id, body) in &filtered {
                if standalone.contains(id) {
                    let emitted = redirect(body);
                    module_fragments.insert(
                        *id,
                        vec![RelativeBodyPlacement {
                            module_id: *id,
                            offset: 0,
                            emitted: emitted.clone(),
                        }],
                    );
                    final_modules.push((*id, emitted));
                }
            }
            if !concat_member_ids.is_empty() {
                module_fragments.insert(concat_id, concat_fragments);
                final_modules.push((concat_id, concat_body));
            }
        } else {
            // async 子图存在 → 逐模块注册（`_r` 返回 Promise，由导入点 `await`）。
            for (id, body) in &filtered {
                let emitted = compact_body_names(body, &runtime_names);
                module_fragments.insert(
                    *id,
                    vec![RelativeBodyPlacement {
                        module_id: *id,
                        offset: 0,
                        emitted: emitted.clone(),
                    }],
                );
                final_modules.push((*id, emitted));
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
        let needs_meta_url = final_bodies
            .iter()
            .any(|body| body.contains("__wake_require__.metaUrl"));
        // minify 下省略 `__esModule` 标记，故不能用它区分「转译 ESM」与「纯 CJS」，改按
        // **是否存在 `default` 键**判定：转译 ESM 必定写了 `exports.default`，而
        // `module.exports = {…}` 的纯 CJS 通常没有。
        //
        // 不能简化为恒取 `m.default`：真 CJS 模块（如 `module.exports = api`）没有 `default`，
        // 取之得 `undefined`。这类模块现在会作为独立注册模块存在（见 `reassigns_module_exports`），
        // 简化版会让 `import pkg from 'cjs-pkg'` 直接拿到 undefined。
        let interop_default = interop_default_helper("__wake_interop_default", no_esmodule);
        let interop_star = interop_star_helper("__wake_interop_star", no_esmodule);
        // async 变体：async 模块的包装器返回 Promise → 缓存并返回它，导入方 `await` 得到最终 exports。
        let needs_registry_temp = inline_regs_need_registry_temp(&inline_regs);
        out.push_str(match (async_ids.is_empty(), needs_registry_temp) {
            (true, false) => {
                "(function(g){var c={};function r(i){var x=c[i];if(x)return x.exports;var m={exports:{}};c[i]=m;var f=t[i];f&&f.call(m.exports,m,m.exports,r);return m.exports}"
            }
            (true, true) => {
                "(function(g){var c={},q;function r(i){var x=c[i];if(x)return x.exports;var m={exports:{}};c[i]=m;var f=t[i];f&&f.call(m.exports,m,m.exports,r);return m.exports}"
            }
            (false, false) => {
                "(function(g){var c={};function r(i){var x=c[i];if(x)return x.p||x.exports;var m={exports:{}};c[i]=m;var f=t[i],p=f&&f.call(m.exports,m,m.exports,r);if(p&&typeof p.then==='function')return m.p=p.then(function(){return m.exports});return m.exports}"
            }
            (false, true) => {
                "(function(g){var c={},q;function r(i){var x=c[i];if(x)return x.p||x.exports;var m={exports:{}};c[i]=m;var f=t[i],p=f&&f.call(m.exports,m,m.exports,r);if(p&&typeof p.then==='function')return m.p=p.then(function(){return m.exports});return m.exports}"
            }
        });

        if needs_meta_url {
            // `import.meta.url` is lowered by a trusted edit to this runtime property. The
            // readable runtime always installs it; the compact runtime does so on demand to keep
            // unrelated bundles byte-identical and avoid an unbound/undefined helper call.
            out.push_str(
                "r.metaUrl=function(){return typeof document!='undefined'?document.baseURI:''};",
            );
        }

        append_inline_regs(&mut out, &inline_regs);

        if needs_interop_default {
            out.push_str(&interop_default);
        }
        if needs_interop_star {
            out.push_str(&interop_star);
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
                let body_offset = out.len();
                out.push_str(body);
                if let Some(slot) = body_placements.as_deref_mut()
                    && let Some(fragments) = module_fragments.get(id)
                {
                    slot.extend(fragments.iter().map(|fragment| BodyPlacement {
                        module_id: fragment.module_id,
                        generated_offset: body_offset + fragment.offset,
                        emitted: fragment.emitted.clone(),
                    }));
                }
                out.push_str("},");
            }
        }
        out.push_str("};r.m=t;r.c=c;");
        out.push_str(&format!("var e=r({});", entry_id));
        if module_format == ModuleFormat::CommonJs {
            out.push_str(
                "module.exports=e;return e;})(typeof globalThis!='undefined'?globalThis:this);",
            );
        } else {
            out.push_str("if(typeof module!='undefined'&&module.exports)module.exports=e;else g.__wake_entry__=e;return e;})(typeof globalThis!='undefined'?globalThis:this);");
        }
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
        // SourceMap：记录实际写入的带缩进模块片段；merge 阶段按 token 对齐到原始模块体。
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
            let generated_offset = out.len();
            let mut emitted = String::with_capacity(body.len() + body.lines().count() * 3);
            for l in body.lines() {
                emitted.push_str("  ");
                emitted.push_str(l);
                emitted.push('\n');
            }
            out.push_str(&emitted);
            if let Some(slot) = body_placements.as_deref_mut() {
                slot.push(BodyPlacement {
                    module_id: *id,
                    generated_offset,
                    emitted,
                });
            }
            out.push_str("},\n");
        }
        out.push_str(
            "};\n__wake_require__.m = __wake_modules__;\n__wake_require__.c = __wake_cache__;\n",
        );
        out.push_str(&format!(
            "var __wake_entry__ = __wake_require__({entry_id});\n"
        ));
        out.push_str(if module_format == ModuleFormat::CommonJs {
            POSTLUDE_COMMONJS
        } else {
            POSTLUDE
        });
        out
    }
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

#[derive(Clone, Copy, Debug)]
struct GeneratedToken<'a> {
    text: &'a str,
    line: u32,
    col: u32,
}

/// Tokenize generated JavaScript just far enough to align codegen output through bundler-owned
/// textual rewrites. This is not a parser: quoted literals are kept whole, identifier/number runs
/// are grouped, and punctuation is a single token. The alignment is deliberately conservative.
fn generated_tokens(code: &str) -> Vec<GeneratedToken<'_>> {
    fn is_word(c: char) -> bool {
        c == '_' || c == '$' || c.is_alphanumeric() || !c.is_ascii()
    }

    let mut tokens = Vec::new();
    let mut byte = 0;
    let mut line = 0u32;
    let mut col = 0u32;
    while byte < code.len() {
        let ch = code[byte..].chars().next().expect("valid UTF-8 suffix");
        if ch.is_whitespace() {
            if ch == '\n' {
                line += 1;
                col = 0;
            } else if ch != '\r' {
                col += ch.len_utf16() as u32;
            }
            byte += ch.len_utf8();
            continue;
        }

        let start = byte;
        let start_line = line;
        let start_col = col;
        let end = if is_word(ch) {
            let mut end = byte + ch.len_utf8();
            let rest_start = end;
            for (relative, next) in code[rest_start..].char_indices() {
                if !is_word(next) {
                    break;
                }
                end = rest_start + relative + next.len_utf8();
            }
            end
        } else if matches!(ch, '\'' | '"' | '`') {
            let quote = ch;
            let mut escaped = false;
            let mut end = byte + ch.len_utf8();
            for next in code[end..].chars() {
                end += next.len_utf8();
                if escaped {
                    escaped = false;
                } else if next == '\\' {
                    escaped = true;
                } else if next == quote {
                    break;
                }
            }
            end
        } else {
            byte + ch.len_utf8()
        };

        let text = &code[start..end];
        tokens.push(GeneratedToken {
            text,
            line: start_line,
            col: start_col,
        });
        for emitted in text.chars() {
            if emitted == '\n' {
                line += 1;
                col = 0;
            } else if emitted != '\r' {
                col += emitted.len_utf16() as u32;
            }
        }
        byte = end;
    }
    tokens
}

/// Patience-style token alignment. A segment is mapped wholesale when it is identical; otherwise
/// only tokens unique on both sides become anchors and recursion proves the gaps independently.
/// Ambiguous repeated/deleted text is left unmapped rather than guessed.
fn align_generated_tokens(
    old: &[GeneratedToken<'_>],
    new: &[GeneratedToken<'_>],
) -> Vec<Option<usize>> {
    fn recurse(
        old: &[GeneratedToken<'_>],
        new: &[GeneratedToken<'_>],
        old_range: std::ops::Range<usize>,
        new_range: std::ops::Range<usize>,
        aligned: &mut [Option<usize>],
    ) {
        if old_range.is_empty() || new_range.is_empty() {
            return;
        }
        if old_range.len() == new_range.len()
            && old[old_range.clone()]
                .iter()
                .zip(&new[new_range.clone()])
                .all(|(left, right)| left.text == right.text)
        {
            for (old_index, new_index) in old_range.zip(new_range) {
                aligned[old_index] = Some(new_index);
            }
            return;
        }

        let mut old_counts: FxHashMap<&str, (usize, usize)> = FxHashMap::default();
        for index in old_range.clone() {
            let entry = old_counts.entry(old[index].text).or_insert((0, index));
            entry.0 += 1;
            entry.1 = index;
        }
        let mut new_counts: FxHashMap<&str, (usize, usize)> = FxHashMap::default();
        for index in new_range.clone() {
            let entry = new_counts.entry(new[index].text).or_insert((0, index));
            entry.0 += 1;
            entry.1 = index;
        }

        let mut anchors = Vec::new();
        let mut last_new = None;
        for old_index in old_range.clone() {
            let text = old[old_index].text;
            let Some(&(1, new_index)) = new_counts.get(text) else {
                continue;
            };
            if old_counts.get(text).is_some_and(|entry| entry.0 == 1)
                && last_new.is_none_or(|last| new_index > last)
            {
                anchors.push((old_index, new_index));
                last_new = Some(new_index);
            }
        }
        if anchors.is_empty() {
            return;
        }

        let mut old_start = old_range.start;
        let mut new_start = new_range.start;
        for (old_anchor, new_anchor) in anchors {
            recurse(
                old,
                new,
                old_start..old_anchor,
                new_start..new_anchor,
                aligned,
            );
            aligned[old_anchor] = Some(new_anchor);
            old_start = old_anchor + 1;
            new_start = new_anchor + 1;
        }
        recurse(
            old,
            new,
            old_start..old_range.end,
            new_start..new_range.end,
            aligned,
        );
    }

    let mut aligned = vec![None; old.len()];
    recurse(old, new, 0..old.len(), 0..new.len(), &mut aligned);
    aligned
}

fn generated_positions(code: &str, placements: &[BodyPlacement]) -> Vec<(u32, u32)> {
    let mut order: Vec<usize> = (0..placements.len()).collect();
    order.sort_unstable_by_key(|index| placements[*index].generated_offset);
    let mut positions = vec![(0, 0); placements.len()];
    let mut byte = 0usize;
    let mut line = 0u32;
    let mut col = 0u32;
    for index in order {
        let target = placements[index].generated_offset.min(code.len());
        debug_assert!(
            target >= byte,
            "placements must point at ordered UTF-8 boundaries"
        );
        for ch in code[byte..target].chars() {
            if ch == '\n' {
                line += 1;
                col = 0;
            } else if ch != '\r' {
                col += ch.len_utf16() as u32;
            }
        }
        byte = target;
        positions[index] = (line, col);
    }
    positions
}

fn advance_generated_position((mut line, mut col): (u32, u32), generated: &str) -> (u32, u32) {
    for ch in generated.chars() {
        if ch == '\n' {
            line += 1;
            col = 0;
        } else if ch != '\r' {
            col += ch.len_utf16() as u32;
        }
    }
    (line, col)
}

struct MappingTokenIndex {
    first_by_position: FxHashMap<(u32, u32), usize>,
}

impl MappingTokenIndex {
    fn new(tokens: &[GeneratedToken<'_>]) -> Self {
        let mut first_by_position = FxHashMap::default();
        for (index, token) in tokens.iter().enumerate() {
            first_by_position
                .entry((token.line, token.col))
                .or_insert(index);
        }
        Self { first_by_position }
    }

    fn find(&self, mapping: &Mapping) -> Option<usize> {
        let exact = self
            .first_by_position
            .get(&(mapping.gen_line, mapping.gen_col))
            .copied();
        // `push` can insert one separator after `mark` to prevent token merging. Preserve the
        // old linear search's first-match semantics even for duplicate or non-canonical token
        // slices by selecting the lower index across the exact and one-column positions.
        let next = mapping.gen_col.checked_add(1).and_then(|column| {
            self.first_by_position
                .get(&(mapping.gen_line, column))
                .copied()
        });
        match (exact, next) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(index), None) | (None, Some(index)) => Some(index),
            (None, None) => None,
        }
    }
}

#[cfg(test)]
mod mapping_token_index_tests {
    use super::*;

    fn mapping(line: u32, col: u32) -> Mapping {
        Mapping::unmapped(line, col)
    }

    #[test]
    fn position_index_matches_exact_and_one_column_fallback() {
        let tokens = generated_tokens("alpha + beta");
        let index = MappingTokenIndex::new(&tokens);

        assert_eq!(index.find(&mapping(0, 0)), Some(0));
        assert_eq!(index.find(&mapping(0, 5)), Some(1));
    }

    #[test]
    fn position_index_retains_the_first_duplicate_position() {
        let tokens = [
            GeneratedToken {
                text: "fallback-first",
                line: 2,
                col: 8,
            },
            GeneratedToken {
                text: "exact-first",
                line: 2,
                col: 7,
            },
            GeneratedToken {
                text: "exact-duplicate",
                line: 2,
                col: 7,
            },
        ];
        let index = MappingTokenIndex::new(&tokens);

        assert_eq!(index.find(&mapping(2, 7)), Some(0));
        assert_eq!(index.find(&mapping(2, 6)), Some(1));
    }

    #[test]
    fn position_index_uses_utf16_columns_and_rejects_misses() {
        let tokens = generated_tokens("😀 x");
        let index = MappingTokenIndex::new(&tokens);

        assert_eq!(tokens[1].text, "x");
        assert_eq!(tokens[1].col, 3);
        assert_eq!(index.find(&mapping(0, 2)), Some(1));
        assert_eq!(index.find(&mapping(0, 1)), None);
        assert_eq!(index.find(&mapping(1, 3)), None);
    }

    #[test]
    fn position_index_is_equivalent_to_linear_first_match() {
        let tokens = [
            GeneratedToken {
                text: "later-line",
                line: 3,
                col: 2,
            },
            GeneratedToken {
                text: "fallback-before-exact",
                line: 1,
                col: 5,
            },
            GeneratedToken {
                text: "exact",
                line: 1,
                col: 4,
            },
            GeneratedToken {
                text: "exact-duplicate",
                line: 1,
                col: 4,
            },
            GeneratedToken {
                text: "origin",
                line: 0,
                col: 0,
            },
        ];
        let index = MappingTokenIndex::new(&tokens);

        for line in 0..=4 {
            for col in 0..=8 {
                let mapping = mapping(line, col);
                let linear = tokens.iter().position(|token| {
                    token.line == mapping.gen_line
                        && (token.col == mapping.gen_col
                            || mapping
                                .gen_col
                                .checked_add(1)
                                .is_some_and(|next| token.col == next))
                });
                assert_eq!(
                    index.find(&mapping),
                    linear,
                    "indexed lookup changed linear first-match semantics at ({line}, {col})"
                );
            }
        }
    }
}

/// Merge module-local mappings into a final bundle map. Each placement carries the exact emitted
/// fragment and final byte position. Local mappings survive only when token alignment proves that
/// the corresponding generated token remains present and in order after bundler rewrites.
fn merge_bundle_map(
    bundle: &str,
    body_placements: &[BodyPlacement],
    bodies: &[(u32, Arc<String>)],
    module_maps: &FxHashMap<u32, Arc<ModuleMappings>>,
    sources: &FxHashMap<u32, (String, Option<String>)>,
    file: Option<String>,
) -> SourceMap {
    let mut sm = SourceMap {
        file,
        ..SourceMap::new()
    };
    let mut name_indices: FxHashMap<String, u32> = FxHashMap::default();
    // 源文件按模块 id 升序登记，保证产物稳定（同输入 → 同 map）。
    let mut ids: Vec<u32> = body_placements
        .iter()
        .map(|placement| placement.module_id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let mut src_index: FxHashMap<u32, u32> = FxHashMap::default();
    for id in &ids {
        if let Some((name, content)) = sources.get(id) {
            let idx = sm.add_source(name.clone(), content.clone());
            src_index.insert(*id, idx);
        }
    }
    let body_of: FxHashMap<u32, &str> = bodies
        .iter()
        .map(|(id, body)| (*id, body.as_str()))
        .collect();
    let base_positions = generated_positions(bundle, body_placements);
    // Everything before the first module body is bundler-owned wrapper code. A one-field segment
    // at the file origin prevents that prologue from inheriting a source association.
    sm.mappings.push(Mapping::unmapped(0, 0));
    for (placement_index, placement) in body_placements.iter().enumerate() {
        let id = placement.module_id;
        let (base_line, base_col) = base_positions[placement_index];
        // Each placement is an island of module-owned code inside wrapper/concat glue. Fence both
        // edges even when token alignment later drops every local source anchor.
        sm.mappings.push(Mapping::unmapped(base_line, base_col));
        let (Some(mm), Some(&si), Some(original)) =
            (module_maps.get(&id), src_index.get(&id), body_of.get(&id))
        else {
            let (end_line, end_col) =
                advance_generated_position((base_line, base_col), &placement.emitted);
            sm.mappings.push(Mapping::unmapped(end_line, end_col));
            continue;
        };
        debug_assert_eq!(
            bundle.get(
                placement.generated_offset..placement.generated_offset + placement.emitted.len()
            ),
            Some(placement.emitted.as_str())
        );
        let original_tokens = generated_tokens(original);
        let emitted_tokens = generated_tokens(&placement.emitted);
        let aligned = align_generated_tokens(&original_tokens, &emitted_tokens);
        let original_token_index = MappingTokenIndex::new(&original_tokens);
        for m in &mm.mappings {
            let Some(old_token) = original_token_index.find(m) else {
                continue;
            };
            let Some(new_token) = aligned[old_token] else {
                continue;
            };
            let token = emitted_tokens[new_token];
            let gen_line = base_line + token.line;
            let gen_col = if token.line == 0 {
                base_col + token.col
            } else {
                token.col
            };
            if m.is_unmapped {
                sm.mappings.push(Mapping::unmapped(gen_line, gen_col));
            } else {
                let name_index = m
                    .name_index
                    .and_then(|index| mm.names.get(index as usize))
                    .map(|name| {
                        if let Some(index) = name_indices.get(name) {
                            *index
                        } else {
                            let index = sm.names.len() as u32;
                            sm.names.push(name.clone());
                            name_indices.insert(name.clone(), index);
                            index
                        }
                    });
                sm.mappings.push(Mapping {
                    gen_line,
                    gen_col,
                    src_index: si,
                    src_offset: m.src_offset,
                    name_index,
                    is_unmapped: false,
                });
            }
        }
        let (end_line, end_col) =
            advance_generated_position((base_line, base_col), &placement.emitted);
        sm.mappings.push(Mapping::unmapped(end_line, end_col));
    }
    sm.mappings.sort_by_key(|m| (m.gen_line, m.gen_col));
    let mut deduplicated: Vec<Mapping> = Vec::with_capacity(sm.mappings.len());
    for mapping in sm.mappings.drain(..) {
        if let Some(previous) = deduplicated.last_mut()
            && previous.gen_line == mapping.gen_line
            && previous.gen_col == mapping.gen_col
        {
            // Stable insertion order models ownership boundaries: a module mapping replaces its
            // leading wrapper fence, while the trailing fence replaces a mapping at body end.
            // For two source anchors, retain the richer named segment as before.
            if previous.is_unmapped != mapping.is_unmapped
                || (!mapping.is_unmapped
                    && previous.name_index.is_none()
                    && mapping.name_index.is_some())
            {
                *previous = mapping;
            }
        } else {
            deduplicated.push(mapping);
        }
    }
    sm.mappings = deduplicated;
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
///
/// Recompute top-level-await propagation from the optimizer-retained static graph. This prevents
/// a removed conditional import from keeping an otherwise synchronous module body async.
fn async_module_ids_with_edges(
    modules: &FxHashMap<u32, ModuleRec>,
    retained_edges: &FxHashMap<u32, ModuleEdges>,
) -> FxHashSet<u32> {
    // 反向边：被导入者 → 静态 ESM 导入它的模块。
    let mut importers: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
    for (&id, rec) in modules {
        let retained = retained_edges
            .get(&id)
            .map(|edges| {
                edges
                    .static_targets
                    .iter()
                    .copied()
                    .collect::<FxHashSet<_>>()
            })
            .unwrap_or_default();
        let spec2id: FxHashMap<&str, u32> =
            rec.dep_ids.iter().map(|(s, i)| (s.as_str(), *i)).collect();
        for dep in rec.deps.iter() {
            if !matches!(
                dep.kind,
                DependencyKind::Import | DependencyKind::ExportFrom
            ) {
                continue;
            }
            if let Some(&tid) = spec2id.get(dep.specifier.as_str())
                && retained.contains(&tid)
            {
                importers.entry(tid).or_default().push(id);
            }
        }
    }
    let mut set: FxHashSet<u32> = FxHashSet::default();
    let mut stack: Vec<u32> = Vec::new();
    for (&id, rec) in modules {
        if retained_edges.contains_key(&id) && rec.has_top_level_await && set.insert(id) {
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

/// Restrict parser-discovered edge kinds to the exact dependency targets retained by each
/// optimizer result. `live` is already the monotonic entry closure, so this produces the sole
/// graph consumed by final chunk pruning and emit. The target ids are process-local BFS module
/// identities; parser-local `SymbolId` values have already done their job inside optimization and
/// never cross this boundary or enter persistent graph state.
fn retained_module_edges(
    modules: &FxHashMap<u32, ModuleRec>,
    retained: &[(u32, Arc<Vec<u32>>)],
    live: &FxHashSet<u32>,
) -> FxHashMap<u32, ModuleEdges> {
    let retained_by_module = retained
        .iter()
        .map(|(module, targets)| (*module, targets.iter().copied().collect::<FxHashSet<u32>>()))
        .collect::<FxHashMap<_, _>>();
    let mut edges = build_module_edges(modules);
    edges.retain(|module, _| live.contains(module));
    for (&module, module_edges) in &mut edges {
        let targets = retained_by_module.get(&module);
        module_edges.static_targets.retain(|target| {
            live.contains(target) && targets.is_some_and(|targets| targets.contains(target))
        });
        module_edges.dyn_targets.retain(|target| {
            live.contains(target) && targets.is_some_and(|targets| targets.contains(target))
        });
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
) -> (String, Vec<BodyPlacement>) {
    let mut out = String::new();
    let mut placements = Vec::new();
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
            let generated_offset = out.len();
            let mut emitted = String::with_capacity(body.len() + body.lines().count() * 3);
            for line in body.lines() {
                emitted.push_str("  ");
                emitted.push_str(line);
                emitted.push('\n');
            }
            out.push_str(&emitted);
            placements.push(BodyPlacement {
                module_id: id,
                generated_offset,
                emitted,
            });
            out.push_str("},\n");
        }
    }
    (out, placements)
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
    style_files: &BTreeMap<u32, Vec<String>>,
    mut chunk_placements: Option<&mut FxHashMap<u32, Vec<BodyPlacement>>>,
) -> (Vec<OutputChunk>, usize) {
    let body_of: FxHashMap<u32, &Arc<String>> = bodies.iter().map(|(id, b)| (*id, b)).collect();

    // 1. 非 entry chunk 先行渲染 + hash（chunk 间只引用数字 id，互相独立）。
    let mut file_of: BTreeMap<u32, String> = BTreeMap::new();
    let mut nonentry: Vec<OutputChunk> = Vec::new();
    for plan in &g.chunks {
        if plan.id == 0 {
            continue;
        }
        let (entries, mut placements) = render_module_entries(&plan.modules, &body_of, async_ids);
        let (code, entries_offset) = render_async_chunk(token, plan.id, &entries);
        if let Some(slot) = chunk_placements.as_deref_mut() {
            for placement in &mut placements {
                placement.generated_offset += entries_offset;
            }
            slot.insert(plan.id, placements);
        }
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
            styles: style_files.get(&plan.id).cloned().unwrap_or_default(),
            source_map: None, // requested maps are merged after every chunk has its final file name
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
    let (entries, mut placements) = render_module_entries(&entry_plan.modules, &body_of, async_ids);
    let f_map = json_file_map(&file_of);
    let d_map = json_deps_map(&g.chunk_deps);
    let s_map = json_styles_map(style_files);
    let (code, entries_offset) = render_entry_chunk(
        token,
        entry_id,
        public_path,
        &f_map,
        &d_map,
        &s_map,
        &entries,
    );
    if let Some(slot) = chunk_placements {
        for placement in &mut placements {
            placement.generated_offset += entries_offset;
        }
        slot.insert(0, placements);
    }
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
        styles: style_files.get(&0).cloned().unwrap_or_default(),
        source_map: None, // requested maps are merged after every chunk has its final file name
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
    s_map: &str,
    entries: &str,
) -> (String, usize) {
    // 分割 runtime 跨 chunk 处理未知模块形态，必须使用 codegen 保留的 ESM marker；不能用
    // `default` 自有键猜测，因为合法 CJS 也可能导出 `{ default: value }`。
    let interop_default = interop_default_helper("interopDefault", false);
    let interop_star = interop_star_helper("interopStar", false);
    let mut out = RUNTIME_ENTRY_PRELUDE
        .replace("__WAKE_NS__", token)
        .replace("__WAKE_INTEROP_DEFAULT__", &interop_default)
        .replace("__WAKE_INTEROP_STAR__", &interop_star);
    // 配置的 `publicPath` 注入运行时（`loadFile` 用它拼 chunk URL）：子路径部署下动态 import()
    // 才不会按当前页面 URL 相对解析而 404。写在 prelude 之后而非其对象字面量里——registry 可能
    // 已由同 token 的先前加载建好（`g.__WAKE_NS__ || (...)`），字面量那次不会再跑。
    out.push_str("__wake__.publicPath = ");
    push_js_string(&mut out, public_path);
    out.push_str(";\n");
    out.push_str(&format!("__wake__.f = {f_map};\n"));
    out.push_str(&format!("Object.assign(__wake__.d, {d_map});\n"));
    out.push_str(&format!("Object.assign(__wake__.s, {s_map});\n"));
    out.push_str("__wake__.markLoaded(0);\n");
    out.push_str("__wake__.register({\n");
    let entries_offset = out.len();
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
    (out, entries_offset)
}

/// async/shared chunk：接入已建 registry + register 模块 + markLoaded。
fn render_async_chunk(token: &str, this_chunk_id: u32, entries: &str) -> (String, usize) {
    let mut out = RUNTIME_ASYNC_PRELUDE.replace("__WAKE_NS__", token);
    out.push_str("__wake__.register({\n");
    let entries_offset = out.len();
    out.push_str(entries);
    out.push_str("});\n");
    out.push_str(&format!("__wake__.markLoaded({this_chunk_id});\n"));
    out.push_str("})();\n");
    (out, entries_offset)
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

/// `{ 1: ["lazy.abcd.css"] }` (chunk id ascending; entry CSS is loaded by HTML).
fn json_styles_map(style_files: &BTreeMap<u32, Vec<String>>) -> String {
    let mut output = String::from("{");
    let mut first = true;
    for (chunk_id, files) in style_files.iter().filter(|(chunk_id, _)| **chunk_id != 0) {
        if !first {
            output.push(',');
        }
        first = false;
        let files = files
            .iter()
            .map(|file| format!("{file:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        output.push_str(&format!(" {chunk_id}: [{files}]"));
    }
    output.push_str(" }");
    output
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
  var modules = {}, cache = {}, chunkPromises = {}, stylePromises = {};
  var W = { m: modules, c: cache, p: chunkPromises, f: {}, d: {}, s: {},
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
  __WAKE_INTEROP_DEFAULT__
  __WAKE_INTEROP_STAR__
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
  function loadStyle(file) {
    if (stylePromises[file]) return stylePromises[file];
    if (W.nreq || typeof document === "undefined") return Promise.resolve();
    var p = new Promise(function (res, rej) {
      var link = document.createElement("link");
      link.rel = "stylesheet"; link.href = W.publicPath + file;
      link.onload = function () { res(); };
      link.onerror = function () { rej(new Error("wake: failed to load style " + file)); };
      (document.head || document.getElementsByTagName("head")[0] || document.documentElement).appendChild(link);
    });
    stylePromises[file] = p;
    p.catch(function () { if (stylePromises[file] === p) delete stylePromises[file]; });
    return p;
  }
  function ensure(cid) {
    if (chunkPromises[cid]) return chunkPromises[cid];
    var deps = W.d[cid] || [];
    var p = Promise.all(deps.map(ensure)).then(function () {
      return Promise.all((W.s[cid] || []).map(loadStyle));
    }).then(function () {
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
fn assign_id(
    module_to_id: &mut FxHashMap<ModuleIdentity, u32>,
    next_id: &mut u32,
    identity: ModuleIdentity,
) -> u32 {
    if let Some(&id) = module_to_id.get(&identity) {
        return id;
    }
    let id = *next_id;
    *next_id += 1;
    module_to_id.insert(identity, id);
    id
}
