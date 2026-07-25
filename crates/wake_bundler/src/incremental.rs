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
use std::sync::Arc;

use wake_cache::{BuildCache, CachedDep, CachedUse, ModuleSummary};
use wake_common::{Diagnostic, FileSystem, FxHashMap, FxHashSet, Interner, Span, fs::normalize};
use wake_ecma_ast::{DependencyKind, ExportDefaultKind, ModuleAst, ModuleExportName, Pattern, Program, SourceType, Statement};
use wake_ecma_codegen::{codegen_module_shaken_mangled, codegen_module_shaken_with};
use wake_ecma_minify::{MinifyCtx, SimplifyAction, analyze_dce, analyze_vars, collect_init_map, is_undefined_shadowed, plan_simplifications};
use wake_ecma_parser::parse;
use wake_graph::{ImportUse, Used, collect_static_uses};
use wake_resolver::{ResolveOptions, Resolver};
use wake_turbo::{Engine, Executor, TaskArg, TaskId, Vc, query};
use xxhash_rust::xxh3::xxh3_64_with_seed;

use crate::chunk::{ChunkGraph, ModuleEdges, compute_chunk_graph};
use crate::loader::{LoadOptions, Loaded, load_source};
use crate::{BuildOutput, ChunkKind, Linker, OutputAsset, OutputChunk, POSTLUDE, PRELUDE};

/// 内容输入 cell 的类型：文件源码文本（`Arc<str>`，指纹 = 内容 hash）。
type Content = Arc<str>;

/// 「说明符 → 内部模块 id」映射（dep 顺序确定，指纹稳定）。
type DepIds = Vec<(String, u32)>;

/// scan 阶段单模块的解析结果：(依赖, 静态使用, 已 parse 的 AST 持有者, parse 任务句柄)。
/// 摘要命中时后两者为 `None`（不 parse）；未命中时携带新 parse 的 AST 与句柄。
type ScanParsed = (
    Vec<ParsedDep>,
    Vec<(String, ImportUse)>,
    Option<Arc<ParsedModule>>,
    Option<Vc<ParsedModule>>,
);

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
}

/// `parse` 任务的输出：AST 持有者 + 源文本 + 依赖（说明符已解为 `String`）+ 诊断。
/// 作为引擎 cell 值，须 `Send + Sync + 'static`（`ModuleAst` 已具备）+ 指纹。
pub struct ParsedModule {
    pub ast: ModuleAst,
    /// 原始源文本（minify 简化器需要读取 span 对应的源码文本）。
    pub source: Arc<str>,
    pub deps: Vec<ParsedDep>,
    pub diagnostics: Vec<Diagnostic>,
}

/// 一条依赖：说明符文本 + 种类 + 源码位置。
#[derive(Clone, Hash)]
pub struct ParsedDep {
    pub specifier: String,
    pub kind: DependencyKind,
    pub span: Span,
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
    resolver: Resolver,
    /// 解析选项（含别名）。跨构建保留——PnP 检测切换解析器时用它重建，避免丢别名。
    resolve_options: ResolveOptions,
    /// 规范化路径 → 内容输入 cell（跨构建保留）。
    content_cells: FxHashMap<PathBuf, Vc<Content>>,
    /// 规范化路径 → linker 输入 cell（跨构建保留）。
    linker_cells: FxHashMap<PathBuf, Vc<LinkerData>>,
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
    /// [`set_define`](Self::set_define) 覆盖（CRUSTIFY-PARITY §M3）。
    define: Arc<[(String, String)]>,
    /// `define` 的稳定指纹，混入产物缓存键——define 变（如 dev↔prod）→ 精确失效产物缓存。
    define_hash: u64,
    /// prod CSS 抽取（默认关；prod build 开）。开启后 CSS 不注入 `<style>`，聚合为独立 `.css` 产物。
    extract_css: bool,
    /// 资源内联字节上限（默认 `usize::MAX` = 全内联）。prod 设 4096：超阈值资源写独立产物。
    asset_inline_limit: usize,
    /// 资源 URL 的 `publicPath` 前缀（默认 `/`）。
    public_path: String,
    /// 紧凑（minify）codegen（默认关；prod build 开）。省换行/缩进（CRUSTIFY-PARITY §M4a）。
    minify: bool,
    /// 死模块消除（默认关；prod build 开）：codegen DCE 剥离死 `require` 后，从 entry 重算可达模块、
    /// 丢弃不可达者（如 `if(false)` 里 `require('…development')` 拉进图但已不可达的 dev 包）。§M4b 后续。
    dead_module_elimination: bool,
    /// 标识符 mangling（默认关；prod build 开）：作用域安全地把非模块作用域局部/参数重命名为短名。
    /// 每模块 `wake_ecma_minify::plan_mangle` 构建 `span→新名` 侧表传入 codegen（CRUSTIFY-PARITY §M4）。
    /// 影响产物缓存键（[`MANGLE_SALT`]）。
    mangle: bool,
    /// 移除 `console.*` 调用（默认关；prod build 可选开）。
    drop_console: bool,
    /// 移除 `debugger` 语句（默认关；prod build 可选开）。
    drop_debugger: bool,
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
    /// 静态使用（Tree Shaking 用；来自 parse 或缓存摘要；仅在需要时填充）。
    uses: Vec<(String, ImportUse)>,
    dep_ids: DepIds,
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
    cached: Option<ModuleSummary>,
}

impl IncrementalBundler {
    pub fn new(fs: Arc<dyn FileSystem>) -> IncrementalBundler {
        // 默认 define：`process.env.NODE_ENV → "production"`（与旧 codegen 硬编码默认逐字节一致）。
        let default_define: Arc<[(String, String)]> = Arc::from(vec![(
            "process.env.NODE_ENV".to_string(),
            "\"production\"".to_string(),
        )]);
        let define_hash = hash_define(&default_define);
        IncrementalBundler {
            resolver: Resolver::new(fs.clone()),
            resolve_options: ResolveOptions::default(),
            fs,
            interner: Arc::new(Interner::new()),
            engine: Arc::new(Engine::new()),
            exec: Executor::with_default_threads(),
            define: default_define,
            define_hash,
            content_cells: FxHashMap::default(),
            linker_cells: FxHashMap::default(),
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
        }
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
                self.resolver =
                    Resolver::with_pnp_options(wrapped, manifest, self.resolve_options.clone());
                true
            }
            None => false,
        };
        self.pnp_detected = Some(enabled);
        enabled
    }

    /// 设置解析选项（含路径别名 `@`/`@@`/`@@@`）。须在首次 `build()` 前调用（CLI 读配置后）。
    /// 重建解析器；跨构建保留选项，供 PnP 检测切换解析器时复用（不丢别名）。
    /// 对齐 crustify `resolve.alias`（CRUSTIFY-PARITY §M1/§H）。
    pub fn set_resolve_options(&mut self, options: ResolveOptions) -> &mut Self {
        self.resolver = Resolver::with_options(self.fs.clone(), options.clone());
        self.resolve_options = options;
        self
    }

    /// 设置编译期 define 表（`静态成员链 → 字面量源码`），如
    /// `[("process.env.NODE_ENV", "\"development\"")]`。指纹混入产物缓存键，
    /// dev↔prod 切换自动失效旧产物。CRUSTIFY-PARITY §M3。
    pub fn set_define(&mut self, define: Vec<(String, String)>) -> &mut Self {
        self.define = Arc::from(define);
        self.define_hash = hash_define(&self.define);
        self
    }

    /// 启用 prod CSS 抽取（CRUSTIFY-PARITY §M3）：CSS 不注入 `<style>`，聚合为独立 `.css` 产物
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

    /// 资源 URL / chunk 加载的 `publicPath` 前缀（如 `/app/`）。
    pub fn set_public_path(&mut self, public_path: impl Into<String>) -> &mut Self {
        self.public_path = public_path.into();
        self
    }

    /// 启用紧凑 codegen（省换行/缩进，CRUSTIFY-PARITY §M4a）。prod build 用；影响产物缓存键。
    pub fn enable_minify(&mut self) -> &mut Self {
        self.minify = true;
        self
    }

    /// 启用死模块消除（CRUSTIFY-PARITY §M4b 后续）：emit 前从 entry 按存活 `require` 边重算可达模块，
    /// 丢弃不可达者。与 `enable_minify` 搭配（DCE 剥离死 `require` 后才有不可达模块可删）。安全：
    /// 边提取误判只会「多留」不会「错删」。
    pub fn enable_dead_module_elimination(&mut self) -> &mut Self {
        self.dead_module_elimination = true;
        self
    }

    /// 启用标识符 mangling（CRUSTIFY-PARITY §M4）：每模块规划作用域安全的短名重命名（只动非模块
    /// 作用域局部/参数，模块顶层与 import/export 名保留）。须与 [`enable_minify`](Self::enable_minify)
    /// 搭配（mangle 侧表只在紧凑 codegen 路径生效）；影响产物缓存键。
    pub fn enable_mangle(&mut self) -> &mut Self {
        self.mangle = true;
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

    /// 从 `entry` 增量 + 并行打包。
    pub fn build(&mut self, entry: &Path) -> BuildOutput {
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
        let mut path_to_id: FxHashMap<PathBuf, u32> = FxHashMap::default();
        let mut next_id: u32 = 0;
        let mut modules: FxHashMap<u32, ModuleRec> = FxHashMap::default();

        let entry_norm = normalize(entry);
        let entry_id = assign_id(&mut path_to_id, &mut next_id, entry_norm.clone());
        let mut frontier: Vec<(u32, PathBuf)> = vec![(entry_id, entry_norm)];

        // Tree Shaking 或缓存启用时才需算 uses（否则 keep 全 None，白算）。
        let need_uses = self.tree_shaking || self.cache.is_some();

        // 加载选项（CSS 抽取 / 资源阈值 / publicPath）+ 带外产物收集（CRUSTIFY-PARITY §M3）。
        let load_opts = LoadOptions {
            extract_css: self.extract_css,
            asset_inline_limit: self.asset_inline_limit,
            public_path: self.public_path.clone(),
        };
        let mut collected_assets: Vec<(String, Vec<u8>)> = Vec::new();
        let mut collected_css: Vec<(u32, String)> = Vec::new();

        // —— 分层 BFS：每层并行 parse（命中缓存摘要的模块跳过 parse）——
        while !frontier.is_empty() {
            // 1. 驱动层：读文件 + 建内容 cell + 算 content_key；查缓存摘要决定是否需 parse。
            let mut layer: Vec<LayerItem> = Vec::new();
            for (id, path) in frontier.drain(..) {
                // 非 JS 资源（CSS/JSON/图片/字体）在此翻译成等价 JS 模块源码，再走统一管线（DESIGN §8）。
                // 计时仅在 WAKE_TIMING 下取——`Instant::now()`（QPC）在部分 Windows 上 ~1µs，
                // 每模块/每依赖各调会给热路径叠出可观开销（1000 模块实测 ~2×），故 gated。
                let tr = timing.then(std::time::Instant::now);
                let loaded = load_source(self.fs.as_ref(), &path, &load_opts);
                if let Some(t) = tr {
                    read_time += t.elapsed();
                }
                match loaded {
                    Ok(Loaded {
                        source: src,
                        source_type: st,
                        asset,
                        css,
                    }) => {
                        // 带外产物：超阈值资源文件 + prod 抽取的 CSS 文本（按模块 id 记序）。
                        if let Some(a) = asset {
                            collected_assets.push(a);
                        }
                        if let Some(text) = css {
                            collected_css.push((id, text));
                        }
                        // content_key 仅缓存启用时需要（缓存主键）；否则跳过 xxh3。
                        let content_key = if self.cache.is_some() {
                            content_key_of(&src, st)
                        } else {
                            0
                        };
                        let content_vc = self.content_cell(&path, &src);
                        let cached = self.cache.as_mut().and_then(|c| c.summary(content_key));
                        layer.push(LayerItem {
                            id,
                            path,
                            content_vc,
                            source_type: st,
                            content_key,
                            cached,
                        });
                    }
                    Err(e) => diagnostics.push(
                        Diagnostic::error(format!("无法读取模块 `{}`：{e}", path.display()))
                            .with_code("WAKE0300"),
                    ),
                }
            }
            if layer.is_empty() {
                break;
            }

            // 2. 并行 parse 仅「未命中缓存摘要」的模块（工作窃取执行器扇出）。
            let to_parse: Vec<usize> = layer
                .iter()
                .enumerate()
                .filter(|(_, it)| it.cached.is_none())
                .map(|(i, _)| i)
                .collect();
            let requests: Vec<_> = to_parse
                .iter()
                .map(|&i| {
                    let cell = layer[i].content_vc;
                    let st = layer[i].source_type;
                    let interner = self.interner.clone();
                    move || parse_request(cell, interner, st)
                })
                .collect();
            let engine = Arc::clone(&self.engine);
            let parsed_results = engine.par_request(&self.exec, requests);
            let mut parsed_by_idx: FxHashMap<usize, (Vc<ParsedModule>, Arc<ParsedModule>)> =
                FxHashMap::default();
            for (&i, res) in to_parse.iter().zip(parsed_results) {
                parsed_by_idx.insert(i, res);
            }

            // 3. 驱动层：取 deps/uses（parse 或缓存）+ resolve 依赖 + 下一层（BFS 去重处理循环）。
            let mut next: Vec<(u32, PathBuf)> = Vec::new();
            for (i, it) in layer.into_iter().enumerate() {
                let LayerItem {
                    id,
                    path,
                    content_vc,
                    source_type,
                    content_key,
                    cached,
                } = it;
                let (deps, uses, parsed_opt, parse_vc_opt): ScanParsed = match cached {
                    Some(sum) => (
                        sum.deps.iter().map(cached_dep_to_parsed).collect(),
                        sum.uses.iter().map(cached_use_to_import).collect(),
                        None,
                        None,
                    ),
                    None => {
                        let (parse_vc, parsed) =
                            parsed_by_idx.remove(&i).expect("miss 模块应已 parse");
                        diagnostics.extend(parsed.diagnostics.iter().cloned());
                        let deps = parsed.deps.clone();
                        let uses = if need_uses {
                            parsed
                                .ast
                                .with_ast(|p| collect_static_uses(p, self.interner.as_ref()))
                        } else {
                            Vec::new()
                        };
                        // 存缓存摘要——仅无诊断的干净模块（否则会吞掉告警）。
                        if parsed.diagnostics.is_empty()
                            && let Some(c) = self.cache.as_mut()
                        {
                            c.put_summary(
                                content_key,
                                ModuleSummary {
                                    deps: deps.iter().map(parsed_dep_to_cached).collect(),
                                    uses: uses.iter().map(import_use_to_cached).collect(),
                                },
                            );
                        }
                        (deps, uses, Some(parsed), Some(parse_vc))
                    }
                };

                let from_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
                let mut dep_ids: DepIds = Vec::new();
                for dep in &deps {
                    let tr = timing.then(std::time::Instant::now);
                    let resolved = self.resolver.resolve(&dep.specifier, &from_dir);
                    if let Some(t) = tr {
                        resolve_time += t.elapsed();
                    }
                    match resolved {
                        Ok(resolved) => {
                            let known = path_to_id.contains_key(&resolved);
                            let did = assign_id(&mut path_to_id, &mut next_id, resolved.clone());
                            if !known {
                                next.push((did, resolved));
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
                        uses,
                        dep_ids,
                        parse_vc: parse_vc_opt,
                        parsed: parsed_opt,
                    },
                );
            }
            frontier = next;
        }

        let t_scan = t0.elapsed();

        // —— Link 阶段：Tree Shaking——算每个模块的「保留导出名」（DESIGN §5.3 / PLAN §6.6）——
        let keep = self.compute_keep_exports(&modules, entry_id, next_id);

        // —— Link 阶段：代码分割——算 chunk 图（DESIGN §6.3 / PLAN §6.5）。None = 单包路径 ——
        let chunk_graph = if self.code_splitting {
            let edges = build_module_edges(&modules);
            compute_chunk_graph(&edges, entry_id, self.share_threshold)
        } else {
            None
        };

        // —— codegen 阶段：设 linker cell（驱动）+ 查产物缓存 + 并行 codegen 未命中者 ——
        let ordered: Vec<u32> = (0..next_id).filter(|id| modules.contains_key(id)).collect();

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
            let data = LinkerData {
                deps: dep_ids,
                keep_exports: keep.get(&id).cloned().flatten(),
                dyn_chunks,
            };
            // body_key 与查缓存仅在启用缓存时做——`hash_linker`（SipHash 全依赖）无缓存时纯浪费。
            // define + minify 指纹混入低 64 位：dev↔prod / minify 开关变化 → 缓存精确失效。
            let (body_key, cached_body) = if self.cache.is_some() {
                let minify_salt = if self.minify { MINIFY_SALT } else { 0 };
                let mangle_salt = if self.mangle { MANGLE_SALT } else { 0 };
                let low = hash_linker(&data) ^ self.define_hash ^ minify_salt ^ mangle_salt;
                let bk = ((content_key as u128) << 64) | (low as u128);
                let cb = self.cache.as_mut().and_then(|c| c.body(bk));
                (bk, cb)
            } else {
                (0u128, None)
            };
            let linker_vc = self.linker_cell(&path, data);
            plans.push(CgPlan {
                id,
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
                    move || parse_request(cell, interner, st)
                })
                .collect();
            let engine = Arc::clone(&self.engine);
            let results = engine.par_request(&self.exec, reqs);
            for (&i, (pvc, parsed)) in need_parse.iter().zip(results) {
                let rec = modules.get_mut(&plans[i].id).unwrap();
                rec.parse_vc = Some(pvc);
                rec.parsed = Some(parsed);
            }
        }

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
                move || codegen_request(parse_vc, linker_vc, interner, define, minify, mangle, no_esmodule, minify_names, drop_console, drop_debugger)
            })
            .collect();
        let engine = Arc::clone(&self.engine);
        let miss_bodies = engine.par_request(&self.exec, requests);

        // 3d. 新算的 body 写回缓存；汇总所有 body（命中 + 新算），按 `ordered` 出序。
        let mut body_of: FxHashMap<u32, Arc<String>> = FxHashMap::default();
        for (&i, body) in miss.iter().zip(miss_bodies) {
            if let Some(c) = self.cache.as_mut() {
                // Arc clone：写回缓存不再深拷贝整段 body。
                c.put_body(plans[i].body_key, body.clone());
            }
            body_of.insert(plans[i].id, body);
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

        if timing {
            let t_cg = t0.elapsed();
            eprintln!(
                "[wake-timing] 模块={} | scan(parse+resolve)={:.1?} 其中 read={:.1?} resolve={:.1?} | link+codegen={:.1?} | 总={:.1?}",
                ordered.len(),
                t_scan,
                read_time,
                resolve_time,
                t_cg - t_scan,
                t_cg,
            );
        }

        // —— Emit：双路（无 async chunk → 旧单包，逐字节不变；有 → 多 chunk 全局 registry）——
        // 模块 id / 数量用 `live_ids`（DME 后；未启用 DME 时 = 全量 `ordered`）。
        let mut output = match &chunk_graph {
            None => {
                let bundle = emit(&bodies, entry_id, self.minify, self.minify);
                crate::single_chunk(bundle, live_ids.len(), diagnostics, live_ids.clone())
            }
            Some(g) => {
                let token = build_token(&normalize(entry), live_ids.len());
                let (chunks, entry_chunk) =
                    emit_chunks(&bodies, g, entry_id, &token, self.content_hash);
                let bundle = chunks[entry_chunk].code.clone();
                BuildOutput {
                    bundle,
                    module_count: live_ids.len(),
                    diagnostics,
                    chunks,
                    entry_chunk,
                    assets: Vec::new(),
                }
            }
        };

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
            collected_css.sort_by_key(|(id, _)| *id);
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

        let n = next_id as usize;
        let mut used: Vec<Used> = (0..next_id).map(|_| Used::None).collect();
        if (entry_id as usize) < n {
            used[entry_id as usize] = Used::All;
        }

        // 预计算 spec→id 映射（模块的去重依赖）
        let spec_to_id: Vec<FxHashMap<&str, u32>> = (0..next_id)
            .map(|i| {
                modules
                    .get(&(i as u32))
                    .map(|rec| {
                        rec.dep_ids.iter().map(|(s, id)| (s.as_str(), *id)).collect()
                    })
                    .unwrap_or_default()
            })
            .collect();

        // 收集每个模块的 uses（含静态 import/export-from 与动态 import/require）。
        // uses_of[i] = Vec<(target_id, ImportUse)>
        let mut uses_of: Vec<Vec<(u32, ImportUse)>> = (0..next_id).map(|_| Vec::new()).collect();
        for (&id, rec) in modules.iter() {
            let i = id as usize;
            let map = &spec_to_id[i];
            for (spec, u) in &rec.uses {
                if let Some(&tid) = map.get(spec.as_str()) {
                    uses_of[i].push((tid, u.clone()));
                }
            }
            for dep in &rec.deps {
                if matches!(dep.kind, DependencyKind::DynamicImport | DependencyKind::Require)
                    && let Some(&tid) = map.get(dep.specifier.as_str())
                {
                    uses_of[i].push((tid, ImportUse::All));
                }
            }
        }

        // Worklist 传播：仅从导出被消费的模块向下传播。
        let mut worklist: Vec<u32> = Vec::new();
        let mut in_queue: Vec<bool> = vec![false; n];
        worklist.push(entry_id);
        in_queue[entry_id as usize] = true;

        while let Some(id) = worklist.pop() {
            in_queue[id as usize] = false;
            let module_used = used[id as usize].clone();

            // 模块的导出无消费 → 不传播其 `export *` (ReexportAll)。
            // 但其具名 import/动态 import 仍需传播（模块体内实际使用了目标导出）。
            for (tid, u) in &uses_of[id as usize] {
                let before = used[*tid as usize].clone();

                match u {
                    ImportUse::Names(ns) => {
                        // 具名 import：始终传播（模块代码实际使用了这些导出名）。
                        used[*tid as usize].merge(&ImportUse::Names(ns.clone()));
                    }
                    ImportUse::All => {
                        // import * / 动态 import / require：始终传播 All。
                        used[*tid as usize].merge(&ImportUse::All);
                    }
                    ImportUse::ReexportAll => {
                        // export *：仅当本模块导出被消费时才传播。
                        match &module_used {
                            Used::All => {
                                // 本模块全部导出被消费 → 传播 All。
                                used[*tid as usize].merge(&ImportUse::All);
                            }
                            Used::Names(ns) if !ns.is_empty() => {
                                // 本模块部分导出被消费 → 传播这些具名（宁多保留，安全）。
                                used[*tid as usize].merge(&ImportUse::Names(
                                    ns.iter().cloned().collect(),
                                ));
                            }
                            _ => {
                                // 本模块导出未被消费 → 不传播 barrel 的 export *。
                            }
                        }
                    }
                }

                if used[*tid as usize] != before && !in_queue[*tid as usize] {
                    worklist.push(*tid);
                    in_queue[*tid as usize] = true;
                }
            }
        }

        for &id in modules.keys() {
            keep.insert(id, used[id as usize].to_keep_list());
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
    body_key: u128,
    linker_vc: Vc<LinkerData>,
    cached_body: Option<Arc<String>>,
}

/// 内容键：`hash(源类型 ‖ 源文本)`。源类型作 seed——同字节不同源类型（.ts vs .js）解析不同，须区分。
fn content_key_of(src: &str, st: SourceType) -> u64 {
    let seed = match st {
        SourceType::Module => 1,
        SourceType::Script => 2,
        SourceType::TypeScript => 3,
        SourceType::Tsx => 4,
        SourceType::Jsx => 5,
    };
    xxh3_64_with_seed(src.as_bytes(), seed)
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
fn cached_use_to_import(c: &CachedUse) -> (String, ImportUse) {
    let u = if c.all {
        if c.reexport {
            ImportUse::ReexportAll
        } else {
            ImportUse::All
        }
    } else {
        ImportUse::Names(c.names.clone())
    };
    (c.specifier.clone(), u)
}

/// parse 请求（在 worker 线程的 `enter` 上下文内执行）：登记 parse 任务、返回句柄 + 结果。
fn parse_request(
    cell: Vc<Content>,
    interner: Arc<Interner>,
    source_type: SourceType,
) -> (Vc<ParsedModule>, Arc<ParsedModule>) {
    let id = TaskId::of("wake_bundler", "parse", &[cell.arg_ref()]);
    let vc = query(id, move || parse_module(cell, &interner, source_type));
    let arc = vc.read();
    (vc, arc)
}

/// codegen 请求（在 worker 线程的 `enter` 上下文内执行）：登记 codegen 任务、返回模块体。

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
) -> Arc<String> {
    let id = TaskId::of(
        "wake_bundler",
        "codegen",
        &[parse_vc.arg_ref(), linker_vc.arg_ref()],
    );
    let vc = query(id, move || {
        let parsed = parse_vc.read();
        let data = linker_vc.read();
        let map: FxHashMap<String, u32> = data.deps.iter().cloned().collect();
        let dyn_chunk: FxHashMap<String, u32> = data.dyn_chunks.iter().cloned().collect();
        let linker = Linker { map, dyn_chunk };
        let keep = data.keep_exports.as_deref();
        // define / minify / mangle 是每个 bundler 的常量（TaskId 未纳入——同一引擎内不变；
        // 跨引擎无共享内存缓存）。产物磁盘缓存键则由 body_key 混入 define/minify/mangle 指纹区分。
        let dv: Vec<(&str, &str)> = define
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        parsed.ast.with_ast(|program| {
            if mangle {
                // 每模块规划作用域安全的短名重命名，得 span→新名 侧表传入 codegen。
                let plan = wake_ecma_minify::plan_mangle(program, &interner);
                // 每模块规划属性名缩短，得 span→新名 侧表传入 codegen。
                let prop_plan = wake_ecma_minify::plan_prop_mangle(program, &interner);

                // ── 全量 minify 上下文（简化/DCE/变量消除） ──
                let mut minify_ctx = MinifyCtx {
                    defines: &dv,
                    prop_rename: Some(prop_plan.table()),
                    ..MinifyCtx::default()
                };
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
                                minify_ctx.expression_replacements.insert(*span, text.clone());
                            }
                        }
                    }
                    minify_ctx.constants = sp.constants;

                    // 2) DCE
                    let dce = analyze_dce(program, &interner, drop_debugger, drop_console);
                    minify_ctx.remove_spans = dce.remove_spans;

                    // 3) 变量使用分析（未引用变量消除 + 变量内联）
                    let va = analyze_vars(program, &interner);
                    minify_ctx.unused_vars = va.unused_vars;

                    // 3b) Tree shaking 整合：导出被移除时，标记对应声明 span。
                    // Span 匹配确保作用域安全（不同作用域的同名变量不同 span）。
                    if let Some(keep_names) = keep {
                        let keep_set: FxHashSet<String> = keep_names.iter().cloned().collect();
                        for (export_name, _var_name, decl_span) in collect_export_var_pairs(program, &interner) {
                            if !keep_set.contains(&export_name) {
                                minify_ctx.removed_export_spans.insert(decl_span);
                            }
                        }
                    }

                    // 4) 变量内联（Phase 2.4）：将单次使用纯变量的初始化表达式注入 inline_vars
                    if !va.inline_candidates.is_empty() {
                        let init_map = collect_init_map(program);
                        for (&name, &decl_span) in &va.inline_candidates {
                            if let Some(init) = init_map.get(&decl_span) {
                                minify_ctx.inline_vars.insert(name, *init);
                            }
                        }
                    }

                    // 5) Phase 3: statement-level optimizations
                    let if_return = wake_ecma_minify::analyze_if_return(program);
                    let join_vars = wake_ecma_minify::analyze_join_vars(program);
                    let seq = wake_ecma_minify::analyze_sequences(program);
                    minify_ctx.populate_stmts(if_return, join_vars, seq);

                    // 6) Scope hoisting (Phase 3.5): lift var declarations to function top
                    let hoist_plan = wake_ecma_minify::plan_hoist(program);
                    minify_ctx.hoist = hoist_plan;

                    // 7) Check if undefined is safe to replace with void 0
                    minify_ctx.no_undefined_shadow = !is_undefined_shadowed(program, &interner);

                    minify_ctx.minify = true;
                }

                codegen_module_shaken_mangled(
                    program,
                    &interner,
                    &linker,
                    keep,
                    &dv,
                    minify,
                    Some(plan.table()),
                    Some(&minify_ctx),
                    no_esmodule,
                    minify_names,
                )
            } else {
                codegen_module_shaken_with(
                    program,
                    &interner,
                    &linker,
                    keep,
                    &dv,
                    minify,
                    no_esmodule,
                    minify_names,
                )
            }
        })
    });
    vc.read()
}

/// parse 任务体：读内容 cell（登记依赖）→ 解析（TS 模式跳过类型）→ 依赖句柄解为字符串。
fn parse_module(cell: Vc<Content>, interner: &Interner, source_type: SourceType) -> ParsedModule {
    let src = cell.read(); // Arc<Content>；读取即登记对内容 cell 的依赖
    let text: &str = &src;
    let out = parse(text, interner, source_type);
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

/// 移除对 hoisted 模块的引用：
/// 1) 独立 `__wake_require__(N);` 调用
/// 2) barrel re-export 整行：`const _wmX = __wake_require__(N);for (..._wmX...)`
fn strip_hoisted_requires_and_barrels(body: &str, hoisted: &FxHashSet<u32>) -> String {
    let mut result = body.to_string();

    for &id in hoisted {
        // Pattern 1: barrel re-export — remove the entire line FIRST
        // const _wmN = __wake_require__(ID);for (const _k in _wmN) if (...) exports[_k] = _wmN[_k];
        let require_part = format!(" = __wake_require__({id});");
        let mut search_from = 0;
        while let Some(pos) = result[search_from..].find(&require_part) {
            let abs = search_from + pos;
            if let Some(rev_pos) = result[..abs].rfind("const _wm") {
                let mid_start = rev_pos + 9;
                let mut mid = mid_start;
                let rbytes = result.as_bytes();
                while mid < abs && rbytes[mid].is_ascii_digit() { mid += 1; }
                if mid > mid_start {
                    let var_name = &result[rev_pos + 6..mid];
                    let mut check = mid;
                    while check < result.len() && rbytes[check].is_ascii_whitespace() { check += 1; }
                    if check < result.len() && rbytes[check] == b'=' {
                        let after_req = abs + require_part.len();
                        let for_pat = format!(
                            "for (const _k in {var_name}) if (_k !== \"default\") exports[_k] = {var_name}[_k];"
                        );
                        if let Some(fp) = result[after_req..].find(&for_pat) {
                            let gap = &result[after_req..after_req + fp];
                            if gap.trim_start().is_empty() {
                                let full_end = after_req + fp + for_pat.len();
                                result.replace_range(rev_pos..full_end, "");
                                search_from = rev_pos;
                                continue;
                            }
                        }
                    }
                }
            }
            search_from = abs + require_part.len();
        }

        // Pattern 2: standalone __wake_require__(ID);
        let standalone = format!("__wake_require__({id});");
        result = result.replace(&standalone, "");
    }
    result
}

/// 检查 body 中是否包含对 `id` 的表达式内 require（如 `const x = __wake_require__(id)`）。
fn has_non_standalone_ref(body: &str, id: u32) -> bool {
    let mut start = 0;
    let needle = &format!("__wake_require__({id})");
    while let Some(pos) = body[start..].find(needle.as_str()) {
        let abs = start + pos;
        let is_standalone = abs == 0
            || body.as_bytes()[abs - 1] == b';'
            || body.as_bytes()[abs - 1] == b'{';
        if !is_standalone {
            return true;
        }
        start = abs + needle.len();
    }
    false
}

/// 紧凑 __reg body 格式：去空格、逗号分隔 → webpack 风格
fn compact_reg_body(body: &str) -> String {
    let mut s = body.replace(" || (", "||(")
                    .replace(" = {});", "={});");
    s = s.replace(";globalThis.__reg.", ",globalThis.__reg.");
    s = s.replace(" = ", "=");
    s
}

/// 紧凑模块 body 中的运行时引用：`__wake_require__`→`_r`，`exports`→`$`
fn compact_body_names(body: &str) -> String {
    body.replace("module.exports", "m.$")
        .replace("__wake_require__(", "_r(")
        .replace("exports", "$")
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
                while j < n && bytes[j].is_ascii_digit() { j += 1; }
                if j > i + 3 {
                    let mut k = j;
                    while k < n && bytes[k].is_ascii_whitespace() { k += 1; }
                    if k < n && bytes[k] == b')' { k += 1; }
                    while k < n && bytes[k].is_ascii_whitespace() { k += 1; }
                    if k < n && bytes[k] == b';' { k += 1; }
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

/// 拼接各模块（已 codegen 的）函数体 + mini runtime。
fn emit(bodies: &[(u32, Arc<String>)], entry_id: u32, minify: bool, no_esmodule: bool) -> String {
    let mut keep_bodies: Vec<(u32, Arc<String>)> = Vec::new();

    if minify {
        let mut candidates: FxHashMap<u32, Arc<String>> = FxHashMap::default();
        for (id, body) in bodies {
            if is_pure_reg_body(body) {
                candidates.insert(*id, body.clone());
            } else {
                keep_bodies.push((*id, body.clone()));
            }
        }

        // 预剥离：candidates 中的模块无导出 → 任何引用它们的 barrel re-export 行都是空操作。
        // 在检查非独立引用前先剥离，避免 candidates 因 barrel 行被错误回退。
        let pre_hoisted: FxHashSet<u32> = candidates.keys().copied().collect();
        let stripped_bodies: Vec<(u32, String)> = keep_bodies
            .iter()
            .map(|(id, body)| {
                if pre_hoisted.is_empty() {
                    (*id, (*body).to_string())
                } else {
                    (*id, strip_hoisted_requires_and_barrels(body, &pre_hoisted))
                }
            })
            .collect();

        // 检查剥离后的模块中是否还有非独立引用（如 `const x = require(N)` 形式的直接导入）。
        let mut ref_ids: FxHashSet<u32> = FxHashSet::default();
        for (_, body) in &stripped_bodies {
            for &cid in candidates.keys() {
                if !ref_ids.contains(&cid) && has_non_standalone_ref(body, cid) {
                    ref_ids.insert(cid);
                }
            }
        }

        // 再次剥离 standalone requires（指向已确认的 candidates + 可能新增的 hoist）。
        let hoisted: FxHashSet<u32> = candidates.keys().copied().collect();
        let mut filtered: Vec<(u32, String)> = stripped_bodies
            .into_iter()
            .map(|(id, body)| {
                if hoisted.is_empty() {
                    (id, body)
                } else {
                    (id, strip_hoisted_requires_and_barrels(&body, &hoisted))
                }
            })
            .collect();

        // 收集 inline __reg：所有 candidates 的 body 直接内联
        let inline_regs: Vec<String> = candidates.values().map(|b| (**b).clone()).collect();

        // —— 模块合并：将所有非 hoist 模块体拼接到一个闭包，block-scoped 避免命名冲突 ——
        let concat_id = entry_id + (filtered.len() as u32) + 1000;
        let mut concat_body = String::new();
        concat_body.push_str("_r=function(){return $};");
        // 按 id 升序（近似 BFS 序 = 依赖序）保证前置模块的 exports 先于消费方设置
        filtered.sort_by_key(|(id, _)| *id);
        for (id, body) in &filtered {
            if *id == entry_id { continue; }
            let b = strip_standalone_requires(&compact_body_names(body));
            concat_body.push_str("{");
            concat_body.push_str(&b);
            concat_body.push_str("}");
        }

        // 构建最终模块表：仅 module 0 + stubs + 合并模块
        let mut final_modules: Vec<(u32, String)> = Vec::new();
        for (id, body) in &filtered {
            if *id == entry_id {
                let nb = compact_body_names(body)
                    .replace("_r(1);", &format!("_r({concat_id});"));
                final_modules.push((entry_id, nb));
                break;
            }
        }
        for &sid in &ref_ids {
            final_modules.push((sid, String::new()));
        }
        final_modules.push((concat_id, concat_body));
        final_modules.sort_by_key(|(id, _)| *id);

        let mut out = String::new();

        let final_bodies: Vec<&str> = final_modules.iter().map(|(_, b)| b.as_str()).collect();
        let needs_interop_default = final_bodies.iter().any(|b| b.contains("__wake_interop_default"));
        let needs_interop_star = final_bodies.iter().any(|b| b.contains("__wake_interop_star"));

        let interop_default = if no_esmodule {
            "function __wake_interop_default(m){return m.default}"
        } else {
            "function __wake_interop_default(m){return m&&m.__esModule?m.default:m}"
        };
        let interop_star = if no_esmodule {
            "function __wake_interop_star(m){return m}"
        } else {
            "function __wake_interop_star(m){if(m&&m.__esModule)return m;var ns={};if(m!=null){for(var k in m)if(Object.prototype.hasOwnProperty.call(m,k)&&k!='default')ns[k]=m[k]}return ns}"
        };
        out.push_str("(function(root){var __wake_cache__={};function __wake_require__(id){var cached=__wake_cache__[id];if(cached)return cached.exports;var module={exports:{}};__wake_cache__[id]=module;__wake_modules__[id].call(module.exports,module,module.exports,__wake_require__);return module.exports}");

        if !inline_regs.is_empty() {
            out.push_str(";");
            for reg in &inline_regs {
                out.push_str(&compact_reg_body(reg));
            }
        }

        if needs_interop_default {
            out.push_str(interop_default);
        }
        if needs_interop_star {
            out.push_str(interop_star);
        }
        out.push_str("var __wake_modules__={");
        for (id, body) in &final_modules {
            if body.is_empty() {
                out.push_str(&format!("{}:function(){{}},", id));
            } else {
                out.push_str(&format!("{}:function(m,$,_r){{", id));
                out.push_str(body);
                out.push_str("},");
            }
        }
        out.push_str("};");
        out.push_str(&format!("var __wake_entry__=__wake_require__({});", entry_id));
        out.push_str("if(typeof module!=='undefined'&&module.exports)module.exports=__wake_entry__;else root.__wake_entry__=__wake_entry__;return __wake_entry__;})(typeof globalThis!=='undefined'?globalThis:this);");
        out
    } else {
        // 非 minify 模式：不 do scope hoisting（无 tree-shaking，所有模块都有 exports/requires）。
        let filtered: Vec<(u32, String)> = bodies
            .iter()
            .map(|(id, body)| (*id, (*body).to_string()))
            .collect();

        let mut out = String::new();
        out.push_str(PRELUDE);
        out.push_str("var __wake_modules__ = {\n");
        for (id, body) in &filtered {
            out.push_str(&format!(
                "{id}: function(module, exports, __wake_require__) {{\n"
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

// ======================================================================
// 代码分割 emit（多产物，DESIGN §6.3 / PLAN §6.5）
// ======================================================================

/// 从模块记录提取 chunk 划分所需的依赖边。
fn build_module_edges(modules: &FxHashMap<u32, ModuleRec>) -> FxHashMap<u32, ModuleEdges> {
    let mut edges = FxHashMap::default();
    for (&id, rec) in modules {
        let spec2id: FxHashMap<&str, u32> =
            rec.dep_ids.iter().map(|(s, i)| (s.as_str(), *i)).collect();
        let mut st = Vec::new();
        let mut dy = Vec::new();
        for dep in &rec.deps {
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
fn render_module_entries(module_ids: &[u32], body_of: &FxHashMap<u32, &Arc<String>>) -> String {
    let mut out = String::new();
    for &id in module_ids {
        if let Some(body) = body_of.get(&id) {
            out.push_str(&format!(
                "{id}: function(module, exports, __wake_require__) {{\n"
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
    hashed: bool,
) -> (Vec<OutputChunk>, usize) {
    let body_of: FxHashMap<u32, &Arc<String>> = bodies.iter().map(|(id, b)| (*id, b)).collect();

    // 1. 非 entry chunk 先行渲染 + hash（chunk 间只引用数字 id，互相独立）。
    let mut file_of: BTreeMap<u32, String> = BTreeMap::new();
    let mut nonentry: Vec<OutputChunk> = Vec::new();
    for plan in &g.chunks {
        if plan.id == 0 {
            continue;
        }
        let entries = render_module_entries(&plan.modules, &body_of);
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
    let entries = render_module_entries(&entry_plan.modules, &body_of);
    let f_map = json_file_map(&file_of);
    let d_map = json_deps_map(&g.chunk_deps);
    let code = render_entry_chunk(token, entry_id, &f_map, &d_map, &entries);
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
    };

    // 3. 按 chunk id 升序组装。
    let mut chunks = vec![entry];
    chunks.extend(nonentry);
    chunks.sort_by_key(|c| c.chunk_id);
    let entry_chunk = chunks.iter().position(|c| c.chunk_id == 0).unwrap();
    (chunks, entry_chunk)
}

/// entry chunk：全局 registry bootstrap + f/d 映射 + register 模块 + 运行入口 + 导出。
fn render_entry_chunk(
    token: &str,
    entry_id: u32,
    f_map: &str,
    d_map: &str,
    entries: &str,
) -> String {
    let mut out = RUNTIME_ENTRY_PRELUDE.replace("__WAKE_NS__", token);
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
    let mut s = entry_norm.to_string_lossy().into_owned();
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
    if (hit) return hit.exports;
    var module = { exports: {} };
    cache[id] = module;
    var fac = modules[id];
    if (!fac) throw new Error("wake: module " + id + " not registered");
    fac.call(module.exports, module, module.exports, require);
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
  function dynImport(cid, id) {
    if (cid == null) return Promise.resolve(interopStar(require(id)));
    return ensure(cid).then(function () { return interopStar(require(id)); });
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
