//! wake_cache — wake_turbo 任务图与产物的**持久化层**（DESIGN §10.3 / PLAN §7.1）。
//!
//! 目标：让一个**全新进程**在读取并哈希真实源码后跳过未变模块的 parse、optimize 与 body emit——
//! 把四类可重建的派生数据落盘：
//!
//! - **模块摘要**（`ModuleSummary`）：`content_key → (deps, uses, 顶层 await 标志)`。`content_key = hash(源类型 ‖ 源文本)`。
//!   有它就能不 parse 直接建依赖图、算 Tree Shaking 保留集。
//! - **优化事实**（`retained_requests`）：以不含最终 chunk 编号的 optimizer key 存储稳定说明符；驱动先
//!   收敛这些边并重新规划 chunk，再形成 body key。
//! - **codegen 产物**（`body`）：`(content_key, optimizer_key, final_layout_key) → String`。
//! - **模块发射元数据**（`mappings`）：source-map 段、生成请求范围和运行时绑定名与 `body` 使用
//!   相同产物键但独立存取；body 发射始终记录并持久化它们。
//!
//! **健壮性**：值全是 `String`/`Vec`/整数——**绝不落 `ModuleAst`（自引用 arena）也绝不落 `Atom`**
//! （interner id 跨进程无意义；说明符已在此前解成 `String`）。这正是 PLAN「Atom 不落盘」的落地。
//! 持久层不保存路径、文件元数据或源码快照。每个新进程都从 loader 读取真实源码并计算
//! `content_key`，因此保留 mtime/size 的外部编辑也不会复用陈旧源码。
//!
//! **格式**：32-byte 小端 envelope（`MAGIC`、`SCHEMA`、payload length、XXH3-128 checksum）+
//! 有界手写 payload。schema 不符是正常 miss；当代 schema 的损坏、I/O 与存储事务失败由调用方以
//! 非致命缓存诊断呈现。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::NamedTempFile;
use xxhash_rust::xxh3::Xxh3;

/// 缓存文件魔数。
const MAGIC: &[u8; 4] = b"WKC1";
/// schema 版本：**wake 的 parse/codegen 输出语义变更时必须 +1**，否则可能取到陈旧产物。
const SCHEMA: u32 = 13;
const HEADER_LEN: usize = 32;

/// Persistent caches are an optimization, so bounded retention is preferable to allowing edited
/// content versions to grow without limit for the lifetime of a project. The limits are deliberately
/// generous enough for large applications while preventing a forgotten cache from consuming an
/// unbounded amount of memory and disk.
const MAX_CACHE_BYTES: usize = 512 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 200_000;
const MAX_CACHE_ITEMS: usize = 4_000_000;
const MAX_CACHE_OWNED_BYTES: usize = 512 * 1024 * 1024;
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheDecodeError {
    TruncatedHeader,
    InvalidMagic,
    PayloadLengthOverflow,
    PayloadTooLarge { declared: u64, maximum: usize },
    PayloadLengthMismatch { declared: u64, actual: usize },
    TrailingBytes,
    ChecksumMismatch,
    BudgetExceeded(&'static str),
    AllocationFailed(&'static str),
    InvalidUtf8,
    InvalidTag { field: &'static str, value: u8 },
    InvalidValue(&'static str),
    DuplicateKey(&'static str),
    TruncatedPayload,
}

impl fmt::Display for CacheDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for CacheDecodeError {}

#[derive(Debug)]
pub enum CacheLoadOutcome {
    Loaded(Box<BuildCache>),
    Missing,
    Incompatible { found_schema: u32 },
    Corrupt(CacheDecodeError),
    Io(io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStoreStage {
    CreateDirectory,
    OpenLock,
    Lock,
    Reload,
    Encode,
    CreateTemporary,
    WriteTemporary,
    FlushTemporary,
    SyncTemporary,
    Replace,
}

#[derive(Debug)]
pub enum CacheStoreError {
    Io {
        stage: CacheStoreStage,
        source: io::Error,
    },
    Encode(CacheEncodeError),
}

impl CacheStoreError {
    fn io(stage: CacheStoreStage, source: io::Error) -> Self {
        Self::Io { stage, source }
    }
}

impl fmt::Display for CacheStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { stage, source } => write!(formatter, "{stage:?}: {source}"),
            Self::Encode(error) => write!(formatter, "encode: {error}"),
        }
    }
}

impl std::error::Error for CacheStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Encode(source) => Some(source),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheEncodeError {
    BudgetExceeded(&'static str),
    LengthOverflow(&'static str),
    AllocationFailed,
    InvalidValue(&'static str),
}

impl fmt::Display for CacheEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for CacheEncodeError {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStoreReport {
    pub repaired_corrupt_latest: bool,
    pub dropped_conflicts: usize,
}

#[derive(Clone, Copy)]
struct CacheLimits {
    max_bytes: usize,
    max_entries: usize,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_CACHE_BYTES,
            max_entries: MAX_CACHE_ENTRIES,
        }
    }
}

/// 一条依赖（说明符 + 种类判别值 + 源码位置）。`kind` 用 `u8`（由调用方与 `DependencyKind` 互转），
/// 使本 crate 不依赖 AST 类型。
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct CachedDep {
    pub specifier: String,
    pub kind: u8,
    pub lo: u32,
    pub hi: u32,
}

/// 一条静态使用记录（Tree Shaking 用）。`all=true` 表示整体使用（namespace/export*），
/// `reexport=true` 且 `all=true` 表示 `export *`（仅当下游消费本模块导出时才传播）。
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct CachedUse {
    pub specifier: String,
    pub all: bool,
    pub reexport: bool,
    pub names: Vec<String>,
}

#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub struct CachedNamedImport {
    pub local: String,
    pub spec: String,
    pub imported: String,
}

/// 链接阶段需要的绑定活跃性。跨持久化边界只存字符串，不存进程内 `Atom`。
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub struct CachedLiveness {
    pub decls: Vec<(String, Vec<String>)>,
    pub root_refs: Vec<String>,
    pub named_imports: Vec<CachedNamedImport>,
    pub namespace_imports: Vec<(String, String)>,
    pub reexport_star: Vec<String>,
    pub ns_reexports: Vec<(String, String)>,
    pub reexport_named: Vec<(String, String, String)>,
    pub exports: Vec<(String, Option<String>)>,
}

/// 一个模块的摘要：依赖、静态使用及链接分析。足以建图、tree shaking 和 concat，无需重建 AST。
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub struct ModuleSummary {
    pub deps: Vec<CachedDep>,
    pub uses: Vec<CachedUse>,
    /// 模块顶层出现过 `await` / `for await` → 打包器把它包成 `async function`。
    pub has_top_level_await: bool,
    pub liveness: CachedLiveness,
    pub concat_is_esm: bool,
    pub concat_block_safe: bool,
    pub concat_observes_commonjs_bindings: bool,
}

/// One module-local source-map segment. Keeping only integers across the persistent-cache
/// boundary avoids coupling `wake_cache` to the ECMAScript code generator while still allowing
/// the bundler to reconstruct its `ModuleMappings` value without reparsing or regenerating code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CachedMapping {
    pub gen_line: u32,
    pub gen_col: u32,
    pub src_index: u32,
    pub src_offset: u32,
    /// `u32` index into [`CachedModuleMappings::names`] when the segment carries an original name.
    pub name_index: Option<u32>,
    /// Source Map V3 one-field generated-only segment.
    pub is_unmapped: bool,
}

/// Semantic use of a codegen-owned internal request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CachedModuleRequestRole {
    Value,
    DiscardedStatic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CachedModuleRequestKind {
    StaticImport,
    DynamicImport,
    Require,
}

impl CachedModuleRequestKind {
    const fn as_u8(self) -> u8 {
        match self {
            Self::StaticImport => 0,
            Self::DynamicImport => 1,
            Self::Require => 2,
        }
    }

    const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::StaticImport),
            1 => Some(Self::DynamicImport),
            2 => Some(Self::Require),
            _ => None,
        }
    }
}

/// Stable optimizer-retained request identity. Numeric graph IDs never cross this boundary.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CachedRetainedRequest {
    pub specifier: String,
    pub kind: CachedModuleRequestKind,
}

impl CachedModuleRequestRole {
    const fn as_u8(self) -> u8 {
        match self {
            Self::Value => 0,
            Self::DiscardedStatic => 1,
        }
    }

    const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Value),
            1 => Some(Self::DiscardedStatic),
            _ => None,
        }
    }
}

/// One proof-carrying generated target-literal range stored with its byte-identical module body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedModuleRequest {
    pub start: u32,
    pub end: u32,
    pub specifier: String,
    pub kind: CachedModuleRequestKind,
    pub role: CachedModuleRequestRole,
}

/// Stable emitted names for one module factory's runtime-owned bindings.
///
/// These names are codegen facts, not values that the cache may reconstruct from generated text.
/// They therefore travel with the byte-identical body and its other module-local metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CachedModuleRuntimeCapabilities {
    pub meta_url: bool,
    pub external_require: bool,
    pub promise_resolve: bool,
    pub object_assign: bool,
    pub object_keys: bool,
    pub object_define_property: bool,
    pub runtime_import: bool,
    pub shared: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedModuleRuntimeNames {
    pub module: String,
    pub exports: String,
    pub require: String,
    pub capabilities: CachedModuleRuntimeCapabilities,
}

impl Default for CachedModuleRuntimeNames {
    fn default() -> Self {
        Self {
            module: "module".into(),
            exports: "exports".into(),
            require: "__wake_require__".into(),
            capabilities: CachedModuleRuntimeCapabilities::default(),
        }
    }
}

/// Complete module-local emission metadata stored independently from the JavaScript body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CachedModuleMappings {
    pub mappings: Vec<CachedMapping>,
    pub names: Vec<String>,
    pub generated_module_requests: Vec<CachedModuleRequest>,
    pub runtime_names: CachedModuleRuntimeNames,
}

/// 持久化构建缓存：摘要表 + 产物表。
#[derive(Clone, Debug, Default)]
pub struct BuildCache {
    summaries: HashMap<u64, ModuleSummary>,
    /// codegen 产物体。存 `Arc<String>`：命中返回引用计数自增而非整体拷贝，
    /// 与 bundler 拼接侧的 `Arc<String>` 同构，消除全命中路径的两次全 bundle memcpy。
    bodies: HashMap<u128, Arc<String>>,
    /// Optimizer-reported internal dependency requests. Stable source specifiers, never one
    /// process's graph traversal IDs, cross the persistent boundary.
    retained_requests: HashMap<u128, Arc<Vec<CachedRetainedRequest>>>,
    /// Module-local emission facts are cached independently from JavaScript bodies. New body
    /// entries are committed atomically with their facts; schema 13 naturally misses legacy entries
    /// which lack generated request-range or runtime-binding metadata.
    mappings: HashMap<u128, Arc<CachedModuleMappings>>,
    /// 命中计数（诊断/测试用）。
    pub summary_hits: u64,
    pub body_hits: u64,
    pub retained_dependency_hits: u64,
    pub mapping_hits: u64,
    /// Session-local recency. These maps are intentionally not serialized: every entry used by a
    /// fresh build is touched before the next store, while untouched entries are precisely the best
    /// eviction candidates from the previous process.
    access_clock: u64,
    summary_access: HashMap<u64, u64>,
    body_access: HashMap<u128, u64>,
    retained_dependency_access: HashMap<u128, u64>,
    mapping_access: HashMap<u128, u64>,
    /// Keys authored by this process since load. Store merges only this overlay into the latest
    /// locked snapshot, so a stale writer cannot resurrect entries another writer evicted.
    authored_summaries: HashSet<u64>,
    authored_bodies: HashSet<u128>,
    authored_retained_requests: HashSet<u128>,
    authored_mappings: HashSet<u128>,
    /// 本次构建是否往缓存写过新条目。全命中（未变）时为 `false` → 跳过落盘，
    /// 免掉「重写 = 缓存体量」大小的磁盘 I/O（缓存文件常和 bundle 一样大）。
    dirty: bool,
}

impl BuildCache {
    /// 空缓存。
    pub fn new() -> BuildCache {
        BuildCache::default()
    }

    /// Load a bounded schema-13 cache without collapsing normal misses, corruption, and I/O.
    pub fn load(path: &Path) -> CacheLoadOutcome {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return CacheLoadOutcome::Missing;
            }
            Err(error) => return CacheLoadOutcome::Io(error),
        };
        load_open_file(file)
    }

    /// Merge with the latest cache under an OS lock and atomically replace the durable file.
    pub fn store(&mut self, path: &Path) -> Result<CacheStoreReport, CacheStoreError> {
        self.store_inner(path, LOCK_WAIT_TIMEOUT, || Ok(()))
    }

    fn store_inner(
        &mut self,
        path: &Path,
        lock_timeout: Duration,
        before_replace: impl FnOnce() -> io::Result<()>,
    ) -> Result<CacheStoreReport, CacheStoreError> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|error| CacheStoreError::io(CacheStoreStage::CreateDirectory, error))?;

        let lock_path = companion_lock_path(path);
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| CacheStoreError::io(CacheStoreStage::OpenLock, error))?;
        acquire_lock(&lock_file, lock_timeout)
            .map_err(|error| CacheStoreError::io(CacheStoreStage::Lock, error))?;

        let mut report = CacheStoreReport::default();
        let latest = match Self::load(path) {
            CacheLoadOutcome::Loaded(cache) => *cache,
            CacheLoadOutcome::Missing | CacheLoadOutcome::Incompatible { .. } => Self::new(),
            CacheLoadOutcome::Corrupt(_) => {
                report.repaired_corrupt_latest = true;
                Self::new()
            }
            CacheLoadOutcome::Io(error) => {
                return Err(CacheStoreError::io(CacheStoreStage::Reload, error));
            }
        };
        let (mut committed, dropped_conflicts) = self.merge_with_latest(latest);
        report.dropped_conflicts = dropped_conflicts;
        committed.compact(CacheLimits::default());
        let bytes = committed.encode().map_err(CacheStoreError::Encode)?;

        let mut temporary = NamedTempFile::new_in(parent)
            .map_err(|error| CacheStoreError::io(CacheStoreStage::CreateTemporary, error))?;
        temporary
            .write_all(&bytes)
            .map_err(|error| CacheStoreError::io(CacheStoreStage::WriteTemporary, error))?;
        temporary
            .flush()
            .map_err(|error| CacheStoreError::io(CacheStoreStage::FlushTemporary, error))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| CacheStoreError::io(CacheStoreStage::SyncTemporary, error))?;
        before_replace().map_err(|error| CacheStoreError::io(CacheStoreStage::Replace, error))?;
        temporary
            .persist(path)
            .map_err(|error| CacheStoreError::io(CacheStoreStage::Replace, error.error))?;

        committed.dirty = false;
        *self = committed;
        Ok(report)
    }

    /// 查模块摘要（命中计数 +1）。
    pub fn summary(&mut self, content_key: u64) -> Option<ModuleSummary> {
        let s = self.summaries.get(&content_key).cloned();
        if s.is_some() {
            self.summary_hits += 1;
            let access = self.next_access();
            self.summary_access.insert(content_key, access);
        }
        s
    }

    pub fn put_summary(&mut self, content_key: u64, summary: ModuleSummary) {
        let access = self.next_access();
        self.summary_access.insert(content_key, access);
        if self.summaries.get(&content_key) != Some(&summary) {
            self.summaries.insert(content_key, summary);
            self.authored_summaries.insert(content_key);
            self.dirty = true;
        }
    }

    /// 本次是否新增过条目——否则（全命中）无需落盘。
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 查 codegen 产物体（命中计数 +1）。返回 `Arc<String>`：命中为引用计数自增，非整体拷贝。
    pub fn body(&mut self, key: u128) -> Option<Arc<String>> {
        let b = self.bodies.get(&key).cloned();
        if b.is_some() {
            self.body_hits += 1;
            let access = self.next_access();
            self.body_access.insert(key, access);
        }
        b
    }

    #[cfg(test)]
    fn put_body(&mut self, key: u128, body: Arc<String>) {
        let access = self.next_access();
        self.body_access.insert(key, access);
        if self
            .bodies
            .get(&key)
            .is_none_or(|current| current.as_str() != body.as_str())
        {
            self.bodies.insert(key, body);
            self.authored_bodies.insert(key);
            self.dirty = true;
        }
    }

    /// Query optimizer-owned internal dependency edges by optimizer key (independent of body
    /// layout and source-map requests).
    pub fn retained_requests(&mut self, key: u128) -> Option<Arc<Vec<CachedRetainedRequest>>> {
        let requests = self.retained_requests.get(&key).cloned();
        if requests.is_some() {
            self.retained_dependency_hits += 1;
            let access = self.next_access();
            self.retained_dependency_access.insert(key, access);
        }
        requests
    }

    pub fn put_retained_requests(&mut self, key: u128, requests: Arc<Vec<CachedRetainedRequest>>) {
        debug_assert!({
            let mut seen = std::collections::HashSet::new();
            requests.iter().all(|request| {
                !request.specifier.is_empty()
                    && seen.insert((request.specifier.as_str(), request.kind))
            })
        });
        let access = self.next_access();
        self.retained_dependency_access.insert(key, access);
        if self
            .retained_requests
            .get(&key)
            .is_none_or(|current| current.as_ref() != requests.as_ref())
        {
            self.retained_requests.insert(key, requests);
            self.authored_retained_requests.insert(key);
            self.dirty = true;
        }
    }

    /// Query module-local source-map segments independently of the JavaScript body.
    pub fn mappings(&mut self, key: u128) -> Option<Arc<CachedModuleMappings>> {
        let mappings = self.mappings.get(&key).cloned();
        if mappings.is_some() {
            self.mapping_hits += 1;
            let access = self.next_access();
            self.mapping_access.insert(key, access);
        }
        mappings
    }

    #[cfg(test)]
    fn put_mappings(&mut self, key: u128, mappings: Arc<CachedModuleMappings>) {
        let access = self.next_access();
        self.mapping_access.insert(key, access);
        if self
            .mappings
            .get(&key)
            .is_none_or(|current| current.as_ref() != mappings.as_ref())
        {
            self.mappings.insert(key, mappings);
            self.authored_mappings.insert(key);
            self.dirty = true;
        }
    }

    /// Commit one generated body and its provenance metadata as a single authored cache fact.
    pub fn put_emission(
        &mut self,
        key: u128,
        body: Arc<String>,
        mappings: Arc<CachedModuleMappings>,
    ) {
        let access = self.next_access();
        self.body_access.insert(key, access);
        self.mapping_access.insert(key, access);
        let changed = self
            .bodies
            .get(&key)
            .is_none_or(|current| current.as_str() != body.as_str())
            || self
                .mappings
                .get(&key)
                .is_none_or(|current| current.as_ref() != mappings.as_ref());
        if changed {
            self.bodies.insert(key, body);
            self.mappings.insert(key, mappings);
            self.authored_bodies.insert(key);
            self.authored_mappings.insert(key);
            self.dirty = true;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.summaries.is_empty()
            && self.bodies.is_empty()
            && self.retained_requests.is_empty()
            && self.mappings.is_empty()
    }

    fn merge_with_latest(&self, mut latest: BuildCache) -> (BuildCache, usize) {
        let mut conflicts = 0;
        merge_map(
            &mut latest.summaries,
            &self.summaries,
            &self.authored_summaries,
            &mut conflicts,
        );
        merge_map(
            &mut latest.retained_requests,
            &self.retained_requests,
            &self.authored_retained_requests,
            &mut conflicts,
        );

        let mut body_keys = BTreeSet::new();
        body_keys.extend(self.authored_bodies.iter().copied());
        body_keys.extend(self.authored_mappings.iter().copied());
        for key in body_keys {
            match merge_body_group(
                latest.bodies.get(&key),
                latest.mappings.get(&key),
                self.authored_bodies
                    .contains(&key)
                    .then(|| self.bodies.get(&key))
                    .flatten(),
                self.authored_mappings
                    .contains(&key)
                    .then(|| self.mappings.get(&key))
                    .flatten(),
            ) {
                Some((body, mappings)) => {
                    if let Some(body) = body {
                        latest.bodies.insert(key, body);
                    } else {
                        latest.bodies.remove(&key);
                    }
                    if let Some(mappings) = mappings {
                        latest.mappings.insert(key, mappings);
                    } else {
                        latest.mappings.remove(&key);
                    }
                }
                None => {
                    latest.bodies.remove(&key);
                    latest.mappings.remove(&key);
                    conflicts += 1;
                }
            }
        }

        latest.access_clock = latest.access_clock.max(self.access_clock);
        merge_access(&mut latest.summary_access, &self.summary_access);
        merge_access(&mut latest.body_access, &self.body_access);
        merge_access(
            &mut latest.retained_dependency_access,
            &self.retained_dependency_access,
        );
        merge_access(&mut latest.mapping_access, &self.mapping_access);
        latest
            .summary_access
            .retain(|key, _| latest.summaries.contains_key(key));
        latest
            .body_access
            .retain(|key, _| latest.bodies.contains_key(key));
        latest
            .retained_dependency_access
            .retain(|key, _| latest.retained_requests.contains_key(key));
        latest
            .mapping_access
            .retain(|key, _| latest.mappings.contains_key(key));

        latest.summary_hits = self.summary_hits;
        latest.body_hits = self.body_hits;
        latest.retained_dependency_hits = self.retained_dependency_hits;
        latest.mapping_hits = self.mapping_hits;
        latest.authored_summaries.clear();
        latest.authored_bodies.clear();
        latest.authored_retained_requests.clear();
        latest.authored_mappings.clear();
        latest.dirty = true;
        (latest, conflicts)
    }

    fn next_access(&mut self) -> u64 {
        self.access_clock = self.access_clock.saturating_add(1);
        self.access_clock
    }

    fn estimated_size(&self) -> usize {
        let summaries = self.summaries.values().map(summary_estimated_size);
        let bodies = self.bodies.values().map(|body| body.len() + 32);
        let retained_requests = self.retained_requests.values().map(|requests| {
            requests
                .iter()
                .map(|request| {
                    request.specifier.len() + std::mem::size_of::<CachedModuleRequestKind>()
                })
                .sum::<usize>()
                + 32
        });
        let mappings = self
            .mappings
            .values()
            .map(|mappings| cached_mappings_estimated_size(mappings));
        summaries
            .chain(bodies)
            .chain(retained_requests)
            .chain(mappings)
            .sum::<usize>()
            + 64
    }

    /// Retain the most recently used entries under a combined entry and byte budget. Eviction only
    /// changes future cache hit rates; it never changes build semantics because every value is
    /// reproducible from source.
    fn compact(&mut self, limits: CacheLimits) {
        #[derive(Clone)]
        enum Key {
            Summary(u64),
            RetainedDependencies(u128),
            Emission(u128),
        }

        let mut emission_keys = BTreeSet::new();
        emission_keys.extend(self.bodies.keys().copied());
        emission_keys.extend(self.mappings.keys().copied());
        let mut candidates = Vec::with_capacity(
            self.summaries.len() + self.retained_requests.len() + emission_keys.len(),
        );
        candidates.extend(self.summaries.keys().map(|&key| {
            (
                self.summary_access.get(&key).copied().unwrap_or(0),
                Key::Summary(key),
            )
        }));
        candidates.extend(self.retained_requests.keys().map(|&key| {
            (
                self.retained_dependency_access
                    .get(&key)
                    .copied()
                    .unwrap_or(0),
                Key::RetainedDependencies(key),
            )
        }));
        candidates.extend(emission_keys.into_iter().map(|key| {
            let access = self
                .body_access
                .get(&key)
                .into_iter()
                .chain(self.mapping_access.get(&key))
                .copied()
                .max()
                .unwrap_or(0);
            (access, Key::Emission(key))
        }));
        // Oldest first. Tie-breakers do not affect correctness; stable key ordering makes retained
        // contents deterministic for a fixed cache state.
        candidates.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| match (&left.1, &right.1) {
                    (Key::Summary(a), Key::Summary(b)) => a.cmp(b),
                    (Key::RetainedDependencies(a), Key::RetainedDependencies(b)) => a.cmp(b),
                    (Key::Emission(a), Key::Emission(b)) => a.cmp(b),
                    (Key::Summary(_), _) => std::cmp::Ordering::Less,
                    (Key::RetainedDependencies(_), Key::Emission(_)) => std::cmp::Ordering::Less,
                    _ => std::cmp::Ordering::Greater,
                })
        });

        let mut entries = self
            .summaries
            .len()
            .saturating_add(self.bodies.len())
            .saturating_add(self.retained_requests.len())
            .saturating_add(self.mappings.len());
        let mut bytes = self.estimated_size();
        for (_, key) in candidates {
            if entries <= limits.max_entries && bytes <= limits.max_bytes {
                break;
            }
            let (removed_entries, removed_bytes) = match key {
                Key::Summary(key) => {
                    self.summary_access.remove(&key);
                    let bytes = self
                        .summaries
                        .remove(&key)
                        .map_or(0, |entry| summary_estimated_size(&entry));
                    (usize::from(bytes > 0), bytes)
                }
                Key::RetainedDependencies(key) => {
                    self.retained_dependency_access.remove(&key);
                    let bytes = self.retained_requests.remove(&key).map_or(0, |entry| {
                        entry
                            .iter()
                            .map(|request| {
                                request.specifier.len()
                                    + std::mem::size_of::<CachedModuleRequestKind>()
                            })
                            .sum::<usize>()
                            + 32
                    });
                    (usize::from(bytes > 0), bytes)
                }
                Key::Emission(key) => {
                    self.body_access.remove(&key);
                    self.mapping_access.remove(&key);
                    let body = self.bodies.remove(&key);
                    let mappings = self.mappings.remove(&key);
                    let removed_entries =
                        usize::from(body.is_some()) + usize::from(mappings.is_some());
                    let removed_bytes = body.map_or(0, |entry| entry.len() + 32)
                        + mappings.map_or(0, |entry| cached_mappings_estimated_size(&entry));
                    (removed_entries, removed_bytes)
                }
            };
            if removed_entries > 0 {
                entries = entries.saturating_sub(removed_entries);
                bytes = bytes.saturating_sub(removed_bytes);
                self.dirty = true;
            }
        }
    }

    // —— 编解码（手写小端二进制）——

    fn encode(&self) -> Result<Vec<u8>, CacheEncodeError> {
        let payload = self.encode_payload()?;
        let payload_len = u64::try_from(payload.len())
            .map_err(|_| CacheEncodeError::LengthOverflow("payload"))?;
        let mut prefix = [0_u8; 16];
        prefix[..4].copy_from_slice(MAGIC);
        prefix[4..8].copy_from_slice(&SCHEMA.to_le_bytes());
        prefix[8..16].copy_from_slice(&payload_len.to_le_bytes());
        let checksum = envelope_checksum(&prefix, &payload);

        let total = HEADER_LEN
            .checked_add(payload.len())
            .ok_or(CacheEncodeError::LengthOverflow("cache file"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(total)
            .map_err(|_| CacheEncodeError::AllocationFailed)?;
        bytes.extend_from_slice(&prefix);
        bytes.extend_from_slice(&checksum.to_le_bytes());
        bytes.extend_from_slice(&payload);
        Ok(bytes)
    }

    fn encode_payload(&self) -> Result<Vec<u8>, CacheEncodeError> {
        if self
            .bodies
            .keys()
            .any(|key| !self.mappings.contains_key(key))
            || self
                .mappings
                .keys()
                .any(|key| !self.bodies.contains_key(key))
        {
            return Err(CacheEncodeError::InvalidValue(
                "incomplete body and mappings provenance",
            ));
        }
        let top_entries = self
            .summaries
            .len()
            .checked_add(self.bodies.len())
            .and_then(|value| value.checked_add(self.retained_requests.len()))
            .and_then(|value| value.checked_add(self.mappings.len()))
            .ok_or(CacheEncodeError::LengthOverflow("top-level entries"))?;
        if top_entries > MAX_CACHE_ENTRIES {
            return Err(CacheEncodeError::BudgetExceeded("top-level entries"));
        }

        let mut b = Vec::new();
        b.try_reserve(4096)
            .map_err(|_| CacheEncodeError::AllocationFailed)?;
        let mut budget = EncodeBudget::default();

        let summary_keys = sorted_keys(&self.summaries)?;
        put_len(&mut b, summary_keys.len(), "summaries")?;
        for key in summary_keys {
            let summary = &self.summaries[&key];
            put_u64(&mut b, key)?;
            budget.claim_items(summary.deps.len())?;
            put_len(&mut b, summary.deps.len(), "dependencies")?;
            for dependency in &summary.deps {
                if dependency.kind > 3 || dependency.lo > dependency.hi {
                    return Err(CacheEncodeError::InvalidValue("dependency"));
                }
                put_str(&mut b, &dependency.specifier)?;
                put_u8(&mut b, dependency.kind)?;
                put_u32(&mut b, dependency.lo)?;
                put_u32(&mut b, dependency.hi)?;
            }
            budget.claim_items(summary.uses.len())?;
            put_len(&mut b, summary.uses.len(), "uses")?;
            for usage in &summary.uses {
                put_str(&mut b, &usage.specifier)?;
                put_bool(&mut b, usage.all)?;
                put_bool(&mut b, usage.reexport)?;
                budget.claim_items(usage.names.len())?;
                put_strings(&mut b, &usage.names)?;
            }
            put_bool(&mut b, summary.has_top_level_await)?;
            put_liveness(&mut b, &summary.liveness, &mut budget)?;
            put_bool(&mut b, summary.concat_is_esm)?;
            put_bool(&mut b, summary.concat_block_safe)?;
            put_bool(&mut b, summary.concat_observes_commonjs_bindings)?;
        }

        let body_keys = sorted_keys(&self.bodies)?;
        put_len(&mut b, body_keys.len(), "bodies")?;
        for key in body_keys {
            put_u128(&mut b, key)?;
            put_str(&mut b, &self.bodies[&key])?;
        }

        let retained_keys = sorted_keys(&self.retained_requests)?;
        put_len(&mut b, retained_keys.len(), "retained requests")?;
        for key in retained_keys {
            let requests = &self.retained_requests[&key];
            put_u128(&mut b, key)?;
            budget.claim_items(requests.len())?;
            put_len(&mut b, requests.len(), "retained request list")?;
            let mut seen = HashSet::new();
            seen.try_reserve(requests.len())
                .map_err(|_| CacheEncodeError::AllocationFailed)?;
            for request in requests.iter() {
                if request.specifier.is_empty()
                    || !seen.insert((request.specifier.as_str(), request.kind))
                {
                    return Err(CacheEncodeError::InvalidValue("retained request"));
                }
                put_str(&mut b, &request.specifier)?;
                put_u8(&mut b, request.kind.as_u8())?;
            }
        }

        let mapping_keys = sorted_keys(&self.mappings)?;
        put_len(&mut b, mapping_keys.len(), "mapping entries")?;
        for key in mapping_keys {
            let module = &self.mappings[&key];
            put_u128(&mut b, key)?;
            budget.claim_items(module.mappings.len())?;
            put_len(&mut b, module.mappings.len(), "mappings")?;
            for mapping in &module.mappings {
                if (!mapping.is_unmapped
                    && mapping
                        .name_index
                        .is_some_and(|index| index as usize >= module.names.len()))
                    || (mapping.is_unmapped && mapping.name_index.is_some())
                {
                    return Err(CacheEncodeError::InvalidValue("mapping name index"));
                }
                put_u32(&mut b, mapping.gen_line)?;
                put_u32(&mut b, mapping.gen_col)?;
                put_u32(&mut b, mapping.src_index)?;
                put_u32(&mut b, mapping.src_offset)?;
                put_u32(&mut b, mapping.name_index.unwrap_or(u32::MAX))?;
                put_bool(&mut b, mapping.is_unmapped)?;
            }
            budget.claim_items(module.names.len())?;
            put_strings(&mut b, &module.names)?;
            budget.claim_items(module.generated_module_requests.len())?;
            put_len(
                &mut b,
                module.generated_module_requests.len(),
                "generated module requests",
            )?;
            if module
                .generated_module_requests
                .windows(2)
                .any(|pair| pair[0].end > pair[1].start)
                || module
                    .generated_module_requests
                    .iter()
                    .any(|request| !valid_cached_module_request(request))
                || !valid_cached_module_runtime_names(&module.runtime_names)
                || !generated_requests_match_body(
                    self.bodies.get(&key).map(|body| body.as_str()),
                    &module.generated_module_requests,
                )
            {
                return Err(CacheEncodeError::InvalidValue("module mappings"));
            }
            for request in &module.generated_module_requests {
                put_u32(&mut b, request.start)?;
                put_u32(&mut b, request.end)?;
                put_str(&mut b, &request.specifier)?;
                put_u8(&mut b, request.kind.as_u8())?;
                put_u8(&mut b, request.role.as_u8())?;
            }
            put_str(&mut b, &module.runtime_names.module)?;
            put_str(&mut b, &module.runtime_names.exports)?;
            put_str(&mut b, &module.runtime_names.require)?;
            let capabilities = &module.runtime_names.capabilities;
            put_bool(&mut b, capabilities.meta_url)?;
            put_bool(&mut b, capabilities.external_require)?;
            put_bool(&mut b, capabilities.promise_resolve)?;
            put_bool(&mut b, capabilities.object_assign)?;
            put_bool(&mut b, capabilities.object_keys)?;
            put_bool(&mut b, capabilities.object_define_property)?;
            put_bool(&mut b, capabilities.runtime_import)?;
            put_bool(&mut b, capabilities.shared)?;
        }
        Ok(b)
    }

    fn decode_payload(bytes: &[u8]) -> Result<BuildCache, CacheDecodeError> {
        let mut c = Cursor::new(bytes);
        let mut cache = BuildCache::default();

        let summary_count = c.count()?;
        c.reserve_map(&mut cache.summaries, summary_count, "summaries")?;
        for _ in 0..summary_count {
            let key = c.u64()?;
            let dependency_count = c.count()?;
            let mut deps = c.vec_with_capacity(dependency_count, "dependencies")?;
            for _ in 0..dependency_count {
                let specifier = c.str()?;
                let kind = c.u8()?;
                if kind > 3 {
                    return Err(CacheDecodeError::InvalidTag {
                        field: "dependency kind",
                        value: kind,
                    });
                }
                let lo = c.u32()?;
                let hi = c.u32()?;
                if lo > hi {
                    return Err(CacheDecodeError::InvalidValue("dependency span"));
                }
                deps.push(CachedDep {
                    specifier,
                    kind,
                    lo,
                    hi,
                });
            }
            let use_count = c.count()?;
            let mut uses = c.vec_with_capacity(use_count, "uses")?;
            for _ in 0..use_count {
                uses.push(CachedUse {
                    specifier: c.str()?,
                    all: c.strict_bool("use all")?,
                    reexport: c.strict_bool("use reexport")?,
                    names: c.strings()?,
                });
            }
            let summary = ModuleSummary {
                deps,
                uses,
                has_top_level_await: c.strict_bool("top-level await")?,
                liveness: c.liveness()?,
                concat_is_esm: c.strict_bool("concat esm")?,
                concat_block_safe: c.strict_bool("concat block safety")?,
                concat_observes_commonjs_bindings: c.strict_bool("concat CommonJS observation")?,
            };
            if cache.summaries.insert(key, summary).is_some() {
                return Err(CacheDecodeError::DuplicateKey("summary"));
            }
        }

        let body_count = c.count()?;
        c.reserve_map(&mut cache.bodies, body_count, "bodies")?;
        for _ in 0..body_count {
            let key = c.u128()?;
            let body = Arc::new(c.str()?);
            if cache.bodies.insert(key, body).is_some() {
                return Err(CacheDecodeError::DuplicateKey("body"));
            }
        }

        let retained_count = c.count()?;
        c.reserve_map(
            &mut cache.retained_requests,
            retained_count,
            "retained request entries",
        )?;
        for _ in 0..retained_count {
            let key = c.u128()?;
            let request_count = c.count()?;
            let mut requests = c.vec_with_capacity(request_count, "retained requests")?;
            for _ in 0..request_count {
                let specifier = c.str()?;
                let value = c.u8()?;
                let kind = CachedModuleRequestKind::from_u8(value).ok_or(
                    CacheDecodeError::InvalidTag {
                        field: "retained request kind",
                        value,
                    },
                )?;
                requests.push(CachedRetainedRequest { specifier, kind });
            }
            let mut seen = c.set_with_capacity(request_count, "retained request set")?;
            if requests.iter().any(|request| {
                request.specifier.is_empty()
                    || !seen.insert((request.specifier.as_str(), request.kind))
            }) {
                return Err(CacheDecodeError::InvalidValue("retained request"));
            }
            if cache
                .retained_requests
                .insert(key, Arc::new(requests))
                .is_some()
            {
                return Err(CacheDecodeError::DuplicateKey("retained request"));
            }
        }

        let mapping_entry_count = c.count()?;
        c.reserve_map(&mut cache.mappings, mapping_entry_count, "mapping entries")?;
        for _ in 0..mapping_entry_count {
            let key = c.u128()?;
            let mapping_count = c.count()?;
            let mut mappings = c.vec_with_capacity(mapping_count, "mappings")?;
            for _ in 0..mapping_count {
                mappings.push(CachedMapping {
                    gen_line: c.u32()?,
                    gen_col: c.u32()?,
                    src_index: c.u32()?,
                    src_offset: c.u32()?,
                    name_index: match c.u32()? {
                        u32::MAX => None,
                        index => Some(index),
                    },
                    is_unmapped: c.strict_bool("unmapped mapping")?,
                });
            }
            let names = c.strings()?;
            let generated_count = c.count()?;
            let mut generated_module_requests =
                c.vec_with_capacity(generated_count, "generated module requests")?;
            for _ in 0..generated_count {
                let start = c.u32()?;
                let end = c.u32()?;
                let specifier = c.str()?;
                let kind_value = c.u8()?;
                let kind = CachedModuleRequestKind::from_u8(kind_value).ok_or(
                    CacheDecodeError::InvalidTag {
                        field: "generated request kind",
                        value: kind_value,
                    },
                )?;
                let role_value = c.u8()?;
                let role = CachedModuleRequestRole::from_u8(role_value).ok_or(
                    CacheDecodeError::InvalidTag {
                        field: "generated request role",
                        value: role_value,
                    },
                )?;
                generated_module_requests.push(CachedModuleRequest {
                    start,
                    end,
                    specifier,
                    kind,
                    role,
                });
            }
            let runtime_names = CachedModuleRuntimeNames {
                module: c.str()?,
                exports: c.str()?,
                require: c.str()?,
                capabilities: CachedModuleRuntimeCapabilities {
                    meta_url: c.strict_bool("meta URL capability")?,
                    external_require: c.strict_bool("external require capability")?,
                    promise_resolve: c.strict_bool("Promise.resolve capability")?,
                    object_assign: c.strict_bool("Object.assign capability")?,
                    object_keys: c.strict_bool("Object.keys capability")?,
                    object_define_property: c.strict_bool("Object.defineProperty capability")?,
                    runtime_import: c.strict_bool("runtime import capability")?,
                    shared: c.strict_bool("shared capability")?,
                },
            };
            if mappings.iter().any(|mapping| {
                (!mapping.is_unmapped
                    && mapping
                        .name_index
                        .is_some_and(|index| index as usize >= names.len()))
                    || (mapping.is_unmapped && mapping.name_index.is_some())
            }) {
                return Err(CacheDecodeError::InvalidValue("mapping name index"));
            }
            if generated_module_requests
                .windows(2)
                .any(|pair| pair[0].end > pair[1].start)
                || generated_module_requests
                    .iter()
                    .any(|request| !valid_cached_module_request(request))
                || !valid_cached_module_runtime_names(&runtime_names)
            {
                return Err(CacheDecodeError::InvalidValue("module mappings"));
            }
            if cache
                .mappings
                .insert(
                    key,
                    Arc::new(CachedModuleMappings {
                        mappings,
                        names,
                        generated_module_requests,
                        runtime_names,
                    }),
                )
                .is_some()
            {
                return Err(CacheDecodeError::DuplicateKey("mapping"));
            }
        }
        if !c.is_eof() {
            return Err(CacheDecodeError::TrailingBytes);
        }
        if cache
            .bodies
            .keys()
            .any(|key| !cache.mappings.contains_key(key))
            || cache.mappings.iter().any(|(key, mappings)| {
                !cache.bodies.contains_key(key)
                    || !generated_requests_match_body(
                        cache.bodies.get(key).map(|body| body.as_str()),
                        &mappings.generated_module_requests,
                    )
            })
        {
            return Err(CacheDecodeError::InvalidValue(
                "body and mappings provenance",
            ));
        }
        Ok(cache)
    }
}

fn load_open_file(mut file: File) -> CacheLoadOutcome {
    let mut prefix = [0_u8; 8];
    if let Err(error) = file.read_exact(&mut prefix) {
        return if error.kind() == io::ErrorKind::UnexpectedEof {
            CacheLoadOutcome::Corrupt(CacheDecodeError::TruncatedHeader)
        } else {
            CacheLoadOutcome::Io(error)
        };
    }
    if &prefix[..4] != MAGIC {
        return CacheLoadOutcome::Corrupt(CacheDecodeError::InvalidMagic);
    }
    let schema = u32::from_le_bytes(prefix[4..8].try_into().expect("fixed schema bytes"));
    if schema != SCHEMA {
        return CacheLoadOutcome::Incompatible {
            found_schema: schema,
        };
    }

    let mut tail = [0_u8; HEADER_LEN - 8];
    if let Err(error) = file.read_exact(&mut tail) {
        return if error.kind() == io::ErrorKind::UnexpectedEof {
            CacheLoadOutcome::Corrupt(CacheDecodeError::TruncatedHeader)
        } else {
            CacheLoadOutcome::Io(error)
        };
    }
    let declared = u64::from_le_bytes(tail[..8].try_into().expect("fixed payload length bytes"));
    if declared > MAX_CACHE_BYTES as u64 {
        return CacheLoadOutcome::Corrupt(CacheDecodeError::PayloadTooLarge {
            declared,
            maximum: MAX_CACHE_BYTES,
        });
    }
    let payload_len = match usize::try_from(declared) {
        Ok(length) => length,
        Err(_) => {
            return CacheLoadOutcome::Corrupt(CacheDecodeError::PayloadLengthOverflow);
        }
    };
    let read_limit = match declared.checked_add(1) {
        Some(limit) => limit,
        None => return CacheLoadOutcome::Corrupt(CacheDecodeError::PayloadLengthOverflow),
    };
    let mut payload = Vec::new();
    if payload
        .try_reserve_exact(payload_len.saturating_add(1))
        .is_err()
    {
        return CacheLoadOutcome::Corrupt(CacheDecodeError::AllocationFailed("payload"));
    }
    if let Err(error) = file.take(read_limit).read_to_end(&mut payload) {
        return CacheLoadOutcome::Io(error);
    }
    if payload.len() < payload_len {
        return CacheLoadOutcome::Corrupt(CacheDecodeError::PayloadLengthMismatch {
            declared,
            actual: payload.len(),
        });
    }
    if payload.len() > payload_len {
        return CacheLoadOutcome::Corrupt(CacheDecodeError::TrailingBytes);
    }

    let expected = u128::from_le_bytes(tail[8..].try_into().expect("fixed checksum bytes"));
    let mut checksum_prefix = [0_u8; 16];
    checksum_prefix[..8].copy_from_slice(&prefix);
    checksum_prefix[8..].copy_from_slice(&tail[..8]);
    if envelope_checksum(&checksum_prefix, &payload) != expected {
        return CacheLoadOutcome::Corrupt(CacheDecodeError::ChecksumMismatch);
    }
    match BuildCache::decode_payload(&payload) {
        Ok(cache) => CacheLoadOutcome::Loaded(Box::new(cache)),
        Err(error) => CacheLoadOutcome::Corrupt(error),
    }
}

fn envelope_checksum(prefix: &[u8; 16], payload: &[u8]) -> u128 {
    let mut hasher = Xxh3::new();
    hasher.update(prefix);
    hasher.update(payload);
    hasher.digest128()
}

fn companion_lock_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".lock");
    PathBuf::from(value)
}

fn acquire_lock(file: &File, timeout: Duration) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(error) => {
                let error: io::Error = error.into();
                if error.kind() != io::ErrorKind::WouldBlock {
                    return Err(error);
                }
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "timed out waiting for persistent cache lock",
                    ));
                }
                thread::sleep(remaining.min(LOCK_RETRY_INTERVAL));
            }
        }
    }
}

fn merge_map<K, V>(
    latest: &mut HashMap<K, V>,
    local: &HashMap<K, V>,
    authored: &HashSet<K>,
    conflicts: &mut usize,
) where
    K: Copy + Eq + std::hash::Hash,
    V: Clone + PartialEq,
{
    for &key in authored {
        let Some(local_value) = local.get(&key) else {
            continue;
        };
        match latest.get(&key) {
            Some(latest_value) if latest_value != local_value => {
                latest.remove(&key);
                *conflicts += 1;
            }
            Some(_) => {}
            None => {
                latest.insert(key, local_value.clone());
            }
        }
    }
}

fn merge_access<K>(latest: &mut HashMap<K, u64>, local: &HashMap<K, u64>)
where
    K: Copy + Eq + std::hash::Hash,
{
    for (&key, &access) in local {
        latest
            .entry(key)
            .and_modify(|current| *current = (*current).max(access))
            .or_insert(access);
    }
}

type BodyGroup = (Option<Arc<String>>, Option<Arc<CachedModuleMappings>>);

fn merge_body_group(
    latest_body: Option<&Arc<String>>,
    latest_mappings: Option<&Arc<CachedModuleMappings>>,
    local_body: Option<&Arc<String>>,
    local_mappings: Option<&Arc<CachedModuleMappings>>,
) -> Option<BodyGroup> {
    if latest_body
        .zip(local_body)
        .is_some_and(|(latest, local)| latest != local)
        || latest_mappings
            .zip(local_mappings)
            .is_some_and(|(latest, local)| latest != local)
    {
        return None;
    }

    let latest_complete = latest_body.is_some() && latest_mappings.is_some();
    let local_complete = local_body.is_some() && local_mappings.is_some();
    if latest_complete {
        return Some((latest_body.cloned(), latest_mappings.cloned()));
    }
    if local_complete {
        return Some((local_body.cloned(), local_mappings.cloned()));
    }

    let complementary = (latest_body.is_some()
        && latest_mappings.is_none()
        && local_body.is_none()
        && local_mappings.is_some())
        || (latest_body.is_none()
            && latest_mappings.is_some()
            && local_body.is_some()
            && local_mappings.is_none());
    if complementary {
        return None;
    }
    Some((
        local_body.cloned().or_else(|| latest_body.cloned()),
        local_mappings.cloned().or_else(|| latest_mappings.cloned()),
    ))
}

fn valid_cached_module_runtime_names(names: &CachedModuleRuntimeNames) -> bool {
    is_safe_ascii_js_binding_identifier(&names.module)
        && is_safe_ascii_js_binding_identifier(&names.exports)
        && is_safe_ascii_js_binding_identifier(&names.require)
        && names.module != names.exports
        && names.module != names.require
        && names.exports != names.require
}

fn is_safe_ascii_js_binding_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || matches!(first, b'_' | b'$'))
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
    {
        return false;
    }

    // Module factories may be async and their body may establish strict mode. Keep the persistent
    // boundary conservative by rejecting keywords and strict-mode restricted binding names.
    !matches!(
        name,
        "arguments"
            | "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "eval"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "implements"
            | "import"
            | "in"
            | "instanceof"
            | "interface"
            | "let"
            | "new"
            | "null"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "return"
            | "static"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
    )
}

fn generated_requests_match_body(body: Option<&str>, requests: &[CachedModuleRequest]) -> bool {
    if requests.is_empty() {
        return true;
    }
    let Some(body) = body else {
        return false;
    };
    let mut previous_end = 0usize;
    for request in requests {
        let start = request.start as usize;
        let end = request.end as usize;
        if start < previous_end || start >= end {
            return false;
        }
        let Some(literal) = body.get(start..end) else {
            return false;
        };
        let Some(target) = literal.parse::<u32>().ok() else {
            return false;
        };
        if literal != target.to_string() || request.specifier.is_empty() {
            return false;
        }
        previous_end = end;
    }
    true
}

fn valid_cached_module_request(request: &CachedModuleRequest) -> bool {
    request.start < request.end
        && !request.specifier.is_empty()
        && (request.role != CachedModuleRequestRole::DiscardedStatic
            || request.kind == CachedModuleRequestKind::StaticImport)
}

fn summary_estimated_size(summary: &ModuleSummary) -> usize {
    let deps = summary
        .deps
        .iter()
        .map(|dep| dep.specifier.len() + 16)
        .sum::<usize>();
    let uses = summary
        .uses
        .iter()
        .map(|usage| {
            usage.specifier.len()
                + usage.names.iter().map(String::len).sum::<usize>()
                + usage.names.len() * 4
                + 16
        })
        .sum::<usize>();
    let liveness = &summary.liveness;
    let liveness_strings = liveness
        .decls
        .iter()
        .map(|(name, refs)| name.len() + refs.iter().map(String::len).sum::<usize>())
        .sum::<usize>()
        + liveness.root_refs.iter().map(String::len).sum::<usize>()
        + liveness
            .named_imports
            .iter()
            .map(|import| import.local.len() + import.spec.len() + import.imported.len())
            .sum::<usize>()
        + liveness
            .namespace_imports
            .iter()
            .map(|(local, spec)| local.len() + spec.len())
            .sum::<usize>()
        + liveness
            .reexport_star
            .iter()
            .map(String::len)
            .sum::<usize>()
        + liveness
            .ns_reexports
            .iter()
            .map(|(name, spec)| name.len() + spec.len())
            .sum::<usize>()
        + liveness
            .reexport_named
            .iter()
            .map(|(name, spec, imported)| name.len() + spec.len() + imported.len())
            .sum::<usize>()
        + liveness
            .exports
            .iter()
            .map(|(name, local)| name.len() + local.as_ref().map_or(0, String::len))
            .sum::<usize>();
    deps + uses + liveness_strings + 128
}

fn cached_mappings_estimated_size(mappings: &CachedModuleMappings) -> usize {
    mappings.mappings.len() * std::mem::size_of::<CachedMapping>()
        + mappings.names.iter().map(String::len).sum::<usize>()
        + mappings.names.len() * std::mem::size_of::<String>()
        + mappings.generated_module_requests.len() * std::mem::size_of::<CachedModuleRequest>()
        + std::mem::size_of::<CachedModuleRuntimeNames>()
        + mappings.runtime_names.module.len()
        + mappings.runtime_names.exports.len()
        + mappings.runtime_names.require.len()
        + std::mem::size_of::<CachedModuleRuntimeCapabilities>()
        + 32
}

// —— 写原语 ——

#[derive(Default)]
struct EncodeBudget {
    items: usize,
}

impl EncodeBudget {
    fn claim_items(&mut self, count: usize) -> Result<(), CacheEncodeError> {
        self.items = self
            .items
            .checked_add(count)
            .ok_or(CacheEncodeError::LengthOverflow("nested items"))?;
        if self.items > MAX_CACHE_ITEMS {
            return Err(CacheEncodeError::BudgetExceeded("nested items"));
        }
        Ok(())
    }
}

fn sorted_keys<K, V>(map: &HashMap<K, V>) -> Result<Vec<K>, CacheEncodeError>
where
    K: Copy + Ord,
{
    let mut keys = Vec::new();
    keys.try_reserve_exact(map.len())
        .map_err(|_| CacheEncodeError::AllocationFailed)?;
    keys.extend(map.keys().copied());
    keys.sort_unstable();
    Ok(keys)
}

fn append_bytes(b: &mut Vec<u8>, bytes: &[u8]) -> Result<(), CacheEncodeError> {
    let next = b
        .len()
        .checked_add(bytes.len())
        .ok_or(CacheEncodeError::LengthOverflow("payload"))?;
    if next > MAX_CACHE_BYTES {
        return Err(CacheEncodeError::BudgetExceeded("payload bytes"));
    }
    b.try_reserve(bytes.len())
        .map_err(|_| CacheEncodeError::AllocationFailed)?;
    b.extend_from_slice(bytes);
    Ok(())
}

fn put_u8(b: &mut Vec<u8>, value: u8) -> Result<(), CacheEncodeError> {
    append_bytes(b, &[value])
}

fn put_bool(b: &mut Vec<u8>, value: bool) -> Result<(), CacheEncodeError> {
    put_u8(b, u8::from(value))
}

fn put_u32(b: &mut Vec<u8>, value: u32) -> Result<(), CacheEncodeError> {
    append_bytes(b, &value.to_le_bytes())
}

fn put_u64(b: &mut Vec<u8>, value: u64) -> Result<(), CacheEncodeError> {
    append_bytes(b, &value.to_le_bytes())
}

fn put_u128(b: &mut Vec<u8>, value: u128) -> Result<(), CacheEncodeError> {
    append_bytes(b, &value.to_le_bytes())
}

fn put_len(b: &mut Vec<u8>, length: usize, field: &'static str) -> Result<(), CacheEncodeError> {
    let value = u32::try_from(length).map_err(|_| CacheEncodeError::LengthOverflow(field))?;
    put_u32(b, value)
}

fn put_str(b: &mut Vec<u8>, value: &str) -> Result<(), CacheEncodeError> {
    put_len(b, value.len(), "string")?;
    append_bytes(b, value.as_bytes())
}

fn put_strings(b: &mut Vec<u8>, values: &[String]) -> Result<(), CacheEncodeError> {
    put_len(b, values.len(), "string list")?;
    for value in values {
        put_str(b, value)?;
    }
    Ok(())
}

fn put_liveness(
    b: &mut Vec<u8>,
    liveness: &CachedLiveness,
    budget: &mut EncodeBudget,
) -> Result<(), CacheEncodeError> {
    budget.claim_items(liveness.decls.len())?;
    put_len(b, liveness.decls.len(), "liveness declarations")?;
    for (name, references) in &liveness.decls {
        put_str(b, name)?;
        budget.claim_items(references.len())?;
        put_strings(b, references)?;
    }
    budget.claim_items(liveness.root_refs.len())?;
    put_strings(b, &liveness.root_refs)?;
    budget.claim_items(liveness.named_imports.len())?;
    put_len(b, liveness.named_imports.len(), "named imports")?;
    for import in &liveness.named_imports {
        put_str(b, &import.local)?;
        put_str(b, &import.spec)?;
        put_str(b, &import.imported)?;
    }
    budget.claim_items(liveness.namespace_imports.len())?;
    put_len(b, liveness.namespace_imports.len(), "namespace imports")?;
    for (local, specifier) in &liveness.namespace_imports {
        put_str(b, local)?;
        put_str(b, specifier)?;
    }
    budget.claim_items(liveness.reexport_star.len())?;
    put_strings(b, &liveness.reexport_star)?;
    budget.claim_items(liveness.ns_reexports.len())?;
    put_len(b, liveness.ns_reexports.len(), "namespace reexports")?;
    for (name, specifier) in &liveness.ns_reexports {
        put_str(b, name)?;
        put_str(b, specifier)?;
    }
    budget.claim_items(liveness.reexport_named.len())?;
    put_len(b, liveness.reexport_named.len(), "named reexports")?;
    for (name, specifier, imported) in &liveness.reexport_named {
        put_str(b, name)?;
        put_str(b, specifier)?;
        put_str(b, imported)?;
    }
    budget.claim_items(liveness.exports.len())?;
    put_len(b, liveness.exports.len(), "exports")?;
    for (name, local) in &liveness.exports {
        put_str(b, name)?;
        put_bool(b, local.is_some())?;
        if let Some(local) = local {
            put_str(b, local)?;
        }
    }
    Ok(())
}

// —— 读游标 ——

#[derive(Default)]
struct DecodeBudget {
    entries: usize,
    items: usize,
    owned_bytes: usize,
}

impl DecodeBudget {
    fn claim_entries(&mut self, count: usize) -> Result<(), CacheDecodeError> {
        self.entries = self
            .entries
            .checked_add(count)
            .ok_or(CacheDecodeError::BudgetExceeded("top-level entries"))?;
        if self.entries > MAX_CACHE_ENTRIES {
            return Err(CacheDecodeError::BudgetExceeded("top-level entries"));
        }
        Ok(())
    }

    fn claim_items(&mut self, count: usize) -> Result<(), CacheDecodeError> {
        self.items = self
            .items
            .checked_add(count)
            .ok_or(CacheDecodeError::BudgetExceeded("nested items"))?;
        if self.items > MAX_CACHE_ITEMS {
            return Err(CacheDecodeError::BudgetExceeded("nested items"));
        }
        Ok(())
    }

    fn claim_owned(&mut self, bytes: usize) -> Result<(), CacheDecodeError> {
        self.owned_bytes = self
            .owned_bytes
            .checked_add(bytes)
            .ok_or(CacheDecodeError::BudgetExceeded("owned bytes"))?;
        if self.owned_bytes > MAX_CACHE_OWNED_BYTES {
            return Err(CacheDecodeError::BudgetExceeded("owned bytes"));
        }
        Ok(())
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
    budget: DecodeBudget,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            budget: DecodeBudget::default(),
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], CacheDecodeError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(CacheDecodeError::TruncatedPayload)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(CacheDecodeError::TruncatedPayload)?;
        self.position = end;
        Ok(value)
    }

    fn is_eof(&self) -> bool {
        self.position == self.bytes.len()
    }

    fn u8(&mut self) -> Result<u8, CacheDecodeError> {
        Ok(self.take(1)?[0])
    }

    fn strict_bool(&mut self, field: &'static str) -> Result<bool, CacheDecodeError> {
        let value = self.u8()?;
        match value {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(CacheDecodeError::InvalidTag { field, value }),
        }
    }

    fn u32(&mut self) -> Result<u32, CacheDecodeError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| CacheDecodeError::TruncatedPayload)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, CacheDecodeError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| CacheDecodeError::TruncatedPayload)?,
        ))
    }

    fn u128(&mut self) -> Result<u128, CacheDecodeError> {
        Ok(u128::from_le_bytes(
            self.take(16)?
                .try_into()
                .map_err(|_| CacheDecodeError::TruncatedPayload)?,
        ))
    }

    fn count(&mut self) -> Result<usize, CacheDecodeError> {
        usize::try_from(self.u32()?).map_err(|_| CacheDecodeError::PayloadLengthOverflow)
    }

    fn str(&mut self) -> Result<String, CacheDecodeError> {
        let length = self.count()?;
        self.budget.claim_owned(length)?;
        let bytes = self.take(length)?;
        let value = std::str::from_utf8(bytes).map_err(|_| CacheDecodeError::InvalidUtf8)?;
        let mut owned = String::new();
        owned
            .try_reserve_exact(length)
            .map_err(|_| CacheDecodeError::AllocationFailed("string"))?;
        owned.push_str(value);
        Ok(owned)
    }

    fn vec_with_capacity<T>(
        &mut self,
        count: usize,
        field: &'static str,
    ) -> Result<Vec<T>, CacheDecodeError> {
        self.budget.claim_items(count)?;
        let bytes = count
            .checked_mul(std::mem::size_of::<T>())
            .ok_or(CacheDecodeError::BudgetExceeded("owned bytes"))?;
        self.budget.claim_owned(bytes)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| CacheDecodeError::AllocationFailed(field))?;
        Ok(values)
    }

    fn reserve_map<K, V>(
        &mut self,
        map: &mut HashMap<K, V>,
        count: usize,
        field: &'static str,
    ) -> Result<(), CacheDecodeError>
    where
        K: Eq + std::hash::Hash,
    {
        self.budget.claim_entries(count)?;
        let bytes = count
            .checked_mul(std::mem::size_of::<(K, V)>())
            .ok_or(CacheDecodeError::BudgetExceeded("owned bytes"))?;
        self.budget.claim_owned(bytes)?;
        map.try_reserve(count)
            .map_err(|_| CacheDecodeError::AllocationFailed(field))
    }

    fn set_with_capacity<T>(
        &mut self,
        count: usize,
        field: &'static str,
    ) -> Result<HashSet<T>, CacheDecodeError>
    where
        T: Eq + std::hash::Hash,
    {
        let bytes = count
            .checked_mul(std::mem::size_of::<T>())
            .ok_or(CacheDecodeError::BudgetExceeded("owned bytes"))?;
        self.budget.claim_owned(bytes)?;
        let mut values = HashSet::new();
        values
            .try_reserve(count)
            .map_err(|_| CacheDecodeError::AllocationFailed(field))?;
        Ok(values)
    }

    fn strings(&mut self) -> Result<Vec<String>, CacheDecodeError> {
        let count = self.count()?;
        let mut values = self.vec_with_capacity(count, "string list")?;
        for _ in 0..count {
            values.push(self.str()?);
        }
        Ok(values)
    }

    fn liveness(&mut self) -> Result<CachedLiveness, CacheDecodeError> {
        let declaration_count = self.count()?;
        let mut decls = self.vec_with_capacity(declaration_count, "liveness declarations")?;
        for _ in 0..declaration_count {
            decls.push((self.str()?, self.strings()?));
        }
        let root_refs = self.strings()?;
        let import_count = self.count()?;
        let mut named_imports = self.vec_with_capacity(import_count, "named imports")?;
        for _ in 0..import_count {
            named_imports.push(CachedNamedImport {
                local: self.str()?,
                spec: self.str()?,
                imported: self.str()?,
            });
        }
        let namespace_count = self.count()?;
        let mut namespace_imports = self.vec_with_capacity(namespace_count, "namespace imports")?;
        for _ in 0..namespace_count {
            namespace_imports.push((self.str()?, self.str()?));
        }
        let reexport_star = self.strings()?;
        let namespace_reexport_count = self.count()?;
        let mut ns_reexports =
            self.vec_with_capacity(namespace_reexport_count, "namespace reexports")?;
        for _ in 0..namespace_reexport_count {
            ns_reexports.push((self.str()?, self.str()?));
        }
        let named_reexport_count = self.count()?;
        let mut reexport_named = self.vec_with_capacity(named_reexport_count, "named reexports")?;
        for _ in 0..named_reexport_count {
            reexport_named.push((self.str()?, self.str()?, self.str()?));
        }
        let export_count = self.count()?;
        let mut exports = self.vec_with_capacity(export_count, "exports")?;
        for _ in 0..export_count {
            let name = self.str()?;
            let local = if self.strict_bool("export local")? {
                Some(self.str()?)
            } else {
                None
            };
            exports.push((name, local));
        }
        Ok(CachedLiveness {
            decls,
            root_refs,
            named_imports,
            namespace_imports,
            reexport_star,
            ns_reexports,
            reexport_named,
            exports,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(payload: &[u8]) -> Vec<u8> {
        let mut prefix = [0_u8; 16];
        prefix[..4].copy_from_slice(MAGIC);
        prefix[4..8].copy_from_slice(&SCHEMA.to_le_bytes());
        prefix[8..].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len());
        bytes.extend_from_slice(&prefix);
        bytes.extend_from_slice(&envelope_checksum(&prefix, payload).to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn load_bytes(bytes: &[u8]) -> CacheLoadOutcome {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.bin");
        std::fs::write(&path, bytes).unwrap();
        BuildCache::load(&path)
    }

    fn loaded(outcome: CacheLoadOutcome) -> BuildCache {
        match outcome {
            CacheLoadOutcome::Loaded(cache) => *cache,
            other => panic!("expected loaded cache, got {other:?}"),
        }
    }

    fn empty_payload() -> Vec<u8> {
        let mut payload = Vec::new();
        put_u32(&mut payload, 0).unwrap();
        put_u32(&mut payload, 0).unwrap();
        put_u32(&mut payload, 0).unwrap();
        put_u32(&mut payload, 0).unwrap();
        payload
    }

    fn put_empty_summary(payload: &mut Vec<u8>, key: u64) {
        put_u64(payload, key).unwrap();
        put_u32(payload, 0).unwrap();
        put_u32(payload, 0).unwrap();
        put_bool(payload, false).unwrap();
        put_liveness(
            payload,
            &CachedLiveness::default(),
            &mut EncodeBudget::default(),
        )
        .unwrap();
        put_bool(payload, false).unwrap();
        put_bool(payload, false).unwrap();
        put_bool(payload, false).unwrap();
    }

    fn retained(specifier: &str, kind: CachedModuleRequestKind) -> CachedRetainedRequest {
        CachedRetainedRequest {
            specifier: specifier.into(),
            kind,
        }
    }

    fn sample() -> BuildCache {
        let mut c = BuildCache::new();
        c.put_summary(
            0xDEAD_BEEF,
            ModuleSummary {
                deps: vec![CachedDep {
                    specifier: "./a.js".into(),
                    kind: 2,
                    lo: 3,
                    hi: 9,
                }],
                uses: vec![CachedUse {
                    specifier: "react".into(),
                    all: false,
                    reexport: false,
                    names: vec!["useState".into(), "default".into()],
                }],
                has_top_level_await: true,
                liveness: CachedLiveness {
                    root_refs: vec!["sideEffect".into()],
                    ..CachedLiveness::default()
                },
                concat_is_esm: true,
                concat_block_safe: true,
                concat_observes_commonjs_bindings: false,
            },
        );
        c.put_body(
            0x1234_5678_9ABC_DEF0_1111_2222_3333_4444,
            Arc::new("9;exports.x = 1;".to_string()),
        );
        c.put_retained_requests(
            0x1234_5678_9ABC_DEF0_1111_2222_3333_4444,
            Arc::new(vec![
                retained("./a.js", CachedModuleRequestKind::StaticImport),
                retained("./z.js", CachedModuleRequestKind::Require),
            ]),
        );
        c.put_mappings(
            0x1234_5678_9ABC_DEF0_1111_2222_3333_4444,
            Arc::new(CachedModuleMappings {
                mappings: vec![
                    CachedMapping {
                        gen_line: 1,
                        gen_col: 2,
                        src_index: 0,
                        src_offset: 7,
                        name_index: Some(0),
                        is_unmapped: false,
                    },
                    CachedMapping {
                        gen_line: 1,
                        gen_col: 9,
                        src_index: 0,
                        src_offset: 0,
                        name_index: None,
                        is_unmapped: true,
                    },
                ],
                names: vec!["descriptiveName".into()],
                generated_module_requests: vec![CachedModuleRequest {
                    start: 0,
                    end: 1,
                    specifier: "./dep.js".into(),
                    kind: CachedModuleRequestKind::StaticImport,
                    role: CachedModuleRequestRole::DiscardedStatic,
                }],
                runtime_names: CachedModuleRuntimeNames {
                    module: "$module".into(),
                    exports: "_exports2".into(),
                    require: "require$3".into(),
                    capabilities: CachedModuleRuntimeCapabilities {
                        meta_url: true,
                        ..CachedModuleRuntimeCapabilities::default()
                    },
                },
            }),
        );
        c
    }

    #[test]
    fn roundtrip_encode_decode() {
        let c = sample();
        let bytes = c.encode().unwrap();
        assert_eq!(&bytes[..4], MAGIC);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 13);
        assert_eq!(
            u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize,
            bytes.len() - HEADER_LEN
        );
        let mut back = loaded(load_bytes(&bytes));
        assert_eq!(
            back.summary(0xDEAD_BEEF).unwrap().deps[0].specifier,
            "./a.js"
        );
        assert_eq!(
            back.mappings(0x1234_5678_9ABC_DEF0_1111_2222_3333_4444)
                .expect("cached mappings")
                .mappings[1],
            CachedMapping {
                gen_line: 1,
                gen_col: 9,
                src_index: 0,
                src_offset: 0,
                name_index: None,
                is_unmapped: true,
            }
        );
        assert_eq!(back.summary(0xDEAD_BEEF).unwrap().uses[0].names.len(), 2);
        assert!(back.summary(0xDEAD_BEEF).unwrap().has_top_level_await);
        assert_eq!(
            back.summary(0xDEAD_BEEF).unwrap().liveness.root_refs,
            ["sideEffect"]
        );
        assert_eq!(
            back.body(0x1234_5678_9ABC_DEF0_1111_2222_3333_4444)
                .as_deref()
                .map(String::as_str),
            Some("9;exports.x = 1;")
        );
        assert!(back.body(0).is_none());
        assert_eq!(
            back.retained_requests(0x1234_5678_9ABC_DEF0_1111_2222_3333_4444)
                .expect("cached retained dependencies")
                .as_slice(),
            [
                retained("./a.js", CachedModuleRequestKind::StaticImport),
                retained("./z.js", CachedModuleRequestKind::Require),
            ]
        );
        assert!(back.retained_requests(0).is_none());
        assert_eq!(
            back.mappings(0x1234_5678_9ABC_DEF0_1111_2222_3333_4444)
                .expect("cached mappings")
                .mappings[0],
            CachedMapping {
                gen_line: 1,
                gen_col: 2,
                src_index: 0,
                src_offset: 7,
                name_index: Some(0),
                is_unmapped: false,
            }
        );
        assert_eq!(
            back.mappings(0x1234_5678_9ABC_DEF0_1111_2222_3333_4444)
                .expect("cached mappings")
                .names,
            ["descriptiveName"]
        );
        assert_eq!(
            back.mappings(0x1234_5678_9ABC_DEF0_1111_2222_3333_4444)
                .expect("cached mappings")
                .generated_module_requests,
            [CachedModuleRequest {
                start: 0,
                end: 1,
                specifier: "./dep.js".into(),
                kind: CachedModuleRequestKind::StaticImport,
                role: CachedModuleRequestRole::DiscardedStatic,
            }]
        );
        assert_eq!(
            back.mappings(0x1234_5678_9ABC_DEF0_1111_2222_3333_4444)
                .expect("cached mappings")
                .runtime_names,
            CachedModuleRuntimeNames {
                module: "$module".into(),
                exports: "_exports2".into(),
                require: "require$3".into(),
                capabilities: CachedModuleRuntimeCapabilities {
                    meta_url: true,
                    ..CachedModuleRuntimeCapabilities::default()
                },
            }
        );
        assert!(back.mappings(0).is_none());
    }

    #[test]
    fn bad_magic_and_schema_have_distinct_outcomes() {
        assert!(matches!(
            load_bytes(b"XXXX................."),
            CacheLoadOutcome::Corrupt(CacheDecodeError::InvalidMagic)
        ));
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&(SCHEMA + 1).to_le_bytes());
        assert!(matches!(
            load_bytes(&bytes),
            CacheLoadOutcome::Incompatible { found_schema } if found_schema == SCHEMA + 1
        ));
    }

    #[test]
    fn store_load_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("wake_cache_roundtrip_test.bin");
        let mut cache = sample();
        cache.store(&path).unwrap();
        let mut loaded = loaded(BuildCache::load(&path));
        assert_eq!(loaded.summary(0xDEAD_BEEF).unwrap().deps[0].kind, 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn successful_store_commits_dirty_state() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "wake_cache_commit_{}_{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut cache = sample();

        assert!(cache.is_dirty());
        cache.store(&path).unwrap();
        assert!(!cache.is_dirty());

        cache.put_body(99, Arc::new("changed".to_string()));
        cache.put_mappings(99, Arc::new(CachedModuleMappings::default()));
        assert!(cache.is_dirty());
        cache.store(&path).unwrap();
        assert!(!cache.is_dirty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn failed_store_preserves_dirty_state_for_retry() {
        let dir = std::env::temp_dir().join(format!(
            "wake_cache_store_failure_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cache = sample();

        assert!(cache.store(&dir).is_err());
        assert!(cache.is_dirty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn compaction_keeps_recent_entries_within_budget() {
        let mut cache = BuildCache::new();
        for key in 0..6u128 {
            cache.put_body(key, Arc::new(format!("body-{key}")));
        }
        // Refresh an old key so recency, not insertion order, owns retention.
        assert!(cache.body(0).is_some());
        cache.compact(CacheLimits {
            max_bytes: usize::MAX,
            max_entries: 2,
        });

        assert_eq!(cache.bodies.len(), 2);
        assert!(cache.bodies.contains_key(&0));
        assert!(cache.bodies.contains_key(&5));
    }

    #[test]
    fn body_dependency_edges_and_source_map_entries_are_independent() {
        let mut cache = BuildCache::new();
        cache.put_body(7, Arc::new("body".into()));
        assert!(cache.body(7).is_some());
        assert!(cache.retained_requests(7).is_none());
        assert!(cache.mappings(7).is_none());

        cache.put_retained_requests(
            7,
            Arc::new(vec![
                retained("./a.js", CachedModuleRequestKind::StaticImport),
                retained("./b.js", CachedModuleRequestKind::DynamicImport),
            ]),
        );
        assert_eq!(
            cache.retained_requests(7).expect("edges").as_slice(),
            [
                retained("./a.js", CachedModuleRequestKind::StaticImport),
                retained("./b.js", CachedModuleRequestKind::DynamicImport),
            ]
        );

        cache.put_mappings(
            7,
            Arc::new(CachedModuleMappings {
                mappings: vec![
                    CachedMapping {
                        gen_line: 0,
                        gen_col: 1,
                        src_index: 0,
                        src_offset: 2,
                        name_index: Some(0),
                        is_unmapped: false,
                    },
                    CachedMapping {
                        gen_line: 0,
                        gen_col: 4,
                        src_index: 0,
                        src_offset: 5,
                        name_index: None,
                        is_unmapped: true,
                    },
                ],
                names: vec!["original".into()],
                generated_module_requests: Vec::new(),
                runtime_names: CachedModuleRuntimeNames::default(),
            }),
        );
        assert!(cache.body(7).is_some());
        let mappings = cache.mappings(7).expect("map");
        assert_eq!(mappings.mappings.len(), 2);
        assert_eq!(mappings.mappings[1].name_index, None);
        assert_eq!(mappings.names, ["original"]);
    }

    #[test]
    fn mismatched_generated_request_metadata_evicts_the_body_pair() {
        let mut cache = BuildCache::new();
        cache.put_body(7, Arc::new("x".into()));
        cache.put_mappings(
            7,
            Arc::new(CachedModuleMappings {
                generated_module_requests: vec![CachedModuleRequest {
                    start: 0,
                    end: 1,
                    specifier: "./dep.js".into(),
                    kind: CachedModuleRequestKind::StaticImport,
                    role: CachedModuleRequestRole::Value,
                }],
                ..CachedModuleMappings::default()
            }),
        );

        assert!(matches!(
            cache.encode(),
            Err(CacheEncodeError::InvalidValue("module mappings"))
        ));
    }

    #[test]
    fn module_runtime_names_default_to_canonical_bindings() {
        assert_eq!(
            CachedModuleRuntimeNames::default(),
            CachedModuleRuntimeNames {
                module: "module".into(),
                exports: "exports".into(),
                require: "__wake_require__".into(),
                capabilities: CachedModuleRuntimeCapabilities::default(),
            }
        );
    }

    #[test]
    fn malformed_module_runtime_names_reject_the_persistent_cache() {
        let malformed = [
            CachedModuleRuntimeNames {
                module: String::new(),
                ..CachedModuleRuntimeNames::default()
            },
            CachedModuleRuntimeNames {
                exports: "module".into(),
                ..CachedModuleRuntimeNames::default()
            },
            CachedModuleRuntimeNames {
                module: "1module".into(),
                ..CachedModuleRuntimeNames::default()
            },
            CachedModuleRuntimeNames {
                exports: "wake-exports".into(),
                ..CachedModuleRuntimeNames::default()
            },
            CachedModuleRuntimeNames {
                require: "请求".into(),
                ..CachedModuleRuntimeNames::default()
            },
            CachedModuleRuntimeNames {
                module: "class".into(),
                ..CachedModuleRuntimeNames::default()
            },
        ];

        for runtime_names in malformed {
            let mut cache = BuildCache::new();
            cache.put_emission(
                7,
                Arc::new("body".into()),
                Arc::new(CachedModuleMappings {
                    runtime_names,
                    ..CachedModuleMappings::default()
                }),
            );
            assert!(matches!(
                cache.encode(),
                Err(CacheEncodeError::InvalidValue("module mappings"))
            ));
        }
    }

    #[test]
    fn discarded_static_role_requires_a_static_import_on_encode_and_decode() {
        for kind in [
            CachedModuleRequestKind::DynamicImport,
            CachedModuleRequestKind::Require,
        ] {
            let mut cache = BuildCache::new();
            cache.put_emission(
                7,
                Arc::new("0".into()),
                Arc::new(CachedModuleMappings {
                    generated_module_requests: vec![CachedModuleRequest {
                        start: 0,
                        end: 1,
                        specifier: "role-kind-sentinel".into(),
                        kind,
                        role: CachedModuleRequestRole::DiscardedStatic,
                    }],
                    ..CachedModuleMappings::default()
                }),
            );
            assert!(matches!(
                cache.encode(),
                Err(CacheEncodeError::InvalidValue("module mappings"))
            ));
        }

        let mut cache = BuildCache::new();
        cache.put_emission(
            7,
            Arc::new("0".into()),
            Arc::new(CachedModuleMappings {
                generated_module_requests: vec![CachedModuleRequest {
                    start: 0,
                    end: 1,
                    specifier: "role-kind-sentinel".into(),
                    kind: CachedModuleRequestKind::StaticImport,
                    role: CachedModuleRequestRole::DiscardedStatic,
                }],
                ..CachedModuleMappings::default()
            }),
        );
        let mut bytes = cache.encode().unwrap();
        let sentinel = b"role-kind-sentinel";
        let start = bytes
            .windows(sentinel.len())
            .position(|window| window == sentinel)
            .expect("encoded request specifier");
        let kind = start + sentinel.len();
        assert_eq!(bytes[kind], CachedModuleRequestKind::StaticImport.as_u8());
        assert_eq!(
            bytes[kind + 1],
            CachedModuleRequestRole::DiscardedStatic.as_u8()
        );
        bytes[kind] = CachedModuleRequestKind::DynamicImport.as_u8();
        let mut checksum_prefix = [0_u8; 16];
        checksum_prefix.copy_from_slice(&bytes[..16]);
        let checksum = envelope_checksum(&checksum_prefix, &bytes[HEADER_LEN..]);
        bytes[16..HEADER_LEN].copy_from_slice(&checksum.to_le_bytes());

        assert!(matches!(
            load_bytes(&bytes),
            CacheLoadOutcome::Corrupt(CacheDecodeError::InvalidValue("module mappings"))
        ));
    }

    #[test]
    fn runtime_capability_flags_use_strict_persistent_encodings() {
        let mut valid_false = Cursor::new(&[0]);
        assert_eq!(valid_false.strict_bool("test"), Ok(false));
        let mut valid_true = Cursor::new(&[1]);
        assert_eq!(valid_true.strict_bool("test"), Ok(true));
        let mut malformed_bool = Cursor::new(&[2]);
        assert!(matches!(
            malformed_bool.strict_bool("test"),
            Err(CacheDecodeError::InvalidTag {
                field: "test",
                value: 2
            })
        ));
    }

    #[test]
    fn checksum_rejects_a_valid_utf8_body_bit_flip() {
        let mut bytes = sample().encode().unwrap();
        let marker = b"exports.x";
        let offset = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("body marker");
        bytes[offset] = b'f';
        assert!(matches!(
            load_bytes(&bytes),
            CacheLoadOutcome::Corrupt(CacheDecodeError::ChecksumMismatch)
        ));
    }

    #[test]
    fn deterministic_encoding_ignores_hash_map_insertion_order() {
        fn build(order: &[u64]) -> BuildCache {
            let mut cache = BuildCache::new();
            for &key in order {
                cache.put_summary(
                    key,
                    ModuleSummary {
                        has_top_level_await: key % 2 == 0,
                        ..ModuleSummary::default()
                    },
                );
                cache.put_retained_requests(
                    key as u128,
                    Arc::new(vec![retained(
                        &format!("./{key}.js"),
                        CachedModuleRequestKind::StaticImport,
                    )]),
                );
                cache.put_emission(
                    key as u128,
                    Arc::new(format!("body-{key}")),
                    Arc::new(CachedModuleMappings::default()),
                );
            }
            cache
        }

        assert_eq!(
            build(&[3, 1, 2]).encode().unwrap(),
            build(&[2, 1, 3]).encode().unwrap()
        );
    }

    #[test]
    fn declared_and_semantic_trailing_bytes_are_rejected() {
        let mut declared_trailing = envelope(&empty_payload());
        declared_trailing.push(0);
        assert!(matches!(
            load_bytes(&declared_trailing),
            CacheLoadOutcome::Corrupt(CacheDecodeError::TrailingBytes)
        ));

        let mut payload = empty_payload();
        payload.push(0);
        assert!(matches!(
            load_bytes(&envelope(&payload)),
            CacheLoadOutcome::Corrupt(CacheDecodeError::TrailingBytes)
        ));
    }

    #[test]
    fn strict_decoder_rejects_invalid_bool_and_dependency_kind() {
        let mut invalid_bool = Vec::new();
        put_u32(&mut invalid_bool, 1).unwrap();
        put_u64(&mut invalid_bool, 7).unwrap();
        put_u32(&mut invalid_bool, 0).unwrap();
        put_u32(&mut invalid_bool, 0).unwrap();
        put_u8(&mut invalid_bool, 2).unwrap();
        assert!(matches!(
            load_bytes(&envelope(&invalid_bool)),
            CacheLoadOutcome::Corrupt(CacheDecodeError::InvalidTag {
                field: "top-level await",
                value: 2
            })
        ));

        let mut invalid_kind = Vec::new();
        put_u32(&mut invalid_kind, 1).unwrap();
        put_u64(&mut invalid_kind, 7).unwrap();
        put_u32(&mut invalid_kind, 1).unwrap();
        put_str(&mut invalid_kind, "./dep.js").unwrap();
        put_u8(&mut invalid_kind, 4).unwrap();
        put_u32(&mut invalid_kind, 0).unwrap();
        put_u32(&mut invalid_kind, 1).unwrap();
        assert!(matches!(
            load_bytes(&envelope(&invalid_kind)),
            CacheLoadOutcome::Corrupt(CacheDecodeError::InvalidTag {
                field: "dependency kind",
                value: 4
            })
        ));
    }

    #[test]
    fn duplicate_top_level_keys_are_corrupt() {
        let mut payload = Vec::new();
        put_u32(&mut payload, 2).unwrap();
        put_empty_summary(&mut payload, 9);
        put_empty_summary(&mut payload, 9);
        put_u32(&mut payload, 0).unwrap();
        put_u32(&mut payload, 0).unwrap();
        put_u32(&mut payload, 0).unwrap();
        assert!(matches!(
            load_bytes(&envelope(&payload)),
            CacheLoadOutcome::Corrupt(CacheDecodeError::DuplicateKey("summary"))
        ));
    }

    #[test]
    fn aggregate_budget_and_cursor_overflow_fail_without_allocating() {
        let mut payload = Vec::new();
        put_u32(&mut payload, u32::MAX).unwrap();
        assert!(matches!(
            load_bytes(&envelope(&payload)),
            CacheLoadOutcome::Corrupt(CacheDecodeError::BudgetExceeded("top-level entries"))
        ));

        let mut cursor = Cursor {
            bytes: &[],
            position: usize::MAX,
            budget: DecodeBudget::default(),
        };
        assert_eq!(cursor.take(1), Err(CacheDecodeError::TruncatedPayload));

        let mut nested = Vec::new();
        put_u32(&mut nested, 1).unwrap();
        put_u64(&mut nested, 1).unwrap();
        put_u32(&mut nested, u32::MAX).unwrap();
        assert!(matches!(
            load_bytes(&envelope(&nested)),
            CacheLoadOutcome::Corrupt(CacheDecodeError::BudgetExceeded("nested items"))
        ));
    }

    #[test]
    fn oversized_declared_payload_is_rejected_before_reading_it() {
        let mut prefix = [0_u8; HEADER_LEN];
        prefix[..4].copy_from_slice(MAGIC);
        prefix[4..8].copy_from_slice(&SCHEMA.to_le_bytes());
        prefix[8..16].copy_from_slice(&((MAX_CACHE_BYTES as u64) + 1).to_le_bytes());
        assert!(matches!(
            load_bytes(&prefix),
            CacheLoadOutcome::Corrupt(CacheDecodeError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn stale_writers_merge_only_authored_disjoint_keys() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.bin");
        let mut seed = BuildCache::new();
        seed.put_summary(1, ModuleSummary::default());
        seed.store(&path).unwrap();

        let mut first = loaded(BuildCache::load(&path));
        let mut second = loaded(BuildCache::load(&path));
        first.put_summary(
            2,
            ModuleSummary {
                has_top_level_await: true,
                ..ModuleSummary::default()
            },
        );
        second.put_summary(
            3,
            ModuleSummary {
                concat_is_esm: true,
                ..ModuleSummary::default()
            },
        );
        first.store(&path).unwrap();
        second.store(&path).unwrap();

        let mut merged = loaded(BuildCache::load(&path));
        assert!(merged.summary(1).is_some());
        assert!(merged.summary(2).is_some());
        assert!(merged.summary(3).is_some());
    }

    #[test]
    fn concurrent_writers_are_serialized_by_the_companion_lock() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.bin");
        let mut seed = BuildCache::new();
        seed.put_summary(1, ModuleSummary::default());
        seed.store(&path).unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let writers = [2_u64, 3].map(|key| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut cache = loaded(BuildCache::load(&path));
                cache.put_summary(
                    key,
                    ModuleSummary {
                        has_top_level_await: key == 2,
                        concat_is_esm: key == 3,
                        ..ModuleSummary::default()
                    },
                );
                barrier.wait();
                cache.store(&path).unwrap();
            })
        });
        for writer in writers {
            writer.join().unwrap();
        }
        let mut merged = loaded(BuildCache::load(&path));
        assert!(merged.summary(1).is_some());
        assert!(merged.summary(2).is_some());
        assert!(merged.summary(3).is_some());
    }

    #[test]
    fn held_companion_lock_times_out_without_replacing_or_committing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.bin");
        let mut seed = BuildCache::new();
        seed.put_summary(1, ModuleSummary::default());
        seed.store(&path).unwrap();
        let original = std::fs::read(&path).unwrap();

        let mut writer = loaded(BuildCache::load(&path));
        writer.put_summary(
            2,
            ModuleSummary {
                has_top_level_await: true,
                ..ModuleSummary::default()
            },
        );
        let held_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(companion_lock_path(&path))
            .unwrap();
        held_lock.lock().unwrap();

        let started = Instant::now();
        let error = writer
            .store_inner(&path, Duration::from_millis(40), || Ok(()))
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(matches!(
            error,
            CacheStoreError::Io {
                stage: CacheStoreStage::Lock,
                source,
            } if source.kind() == io::ErrorKind::WouldBlock
        ));
        assert!(writer.is_dirty());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn stale_snapshot_does_not_resurrect_a_conflict_removed_key() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.bin");
        let mut seed = BuildCache::new();
        seed.put_summary(1, ModuleSummary::default());
        seed.store(&path).unwrap();

        let mut remover = loaded(BuildCache::load(&path));
        let mut stale = loaded(BuildCache::load(&path));
        remover.put_summary(
            1,
            ModuleSummary {
                has_top_level_await: true,
                ..ModuleSummary::default()
            },
        );
        assert_eq!(remover.store(&path).unwrap().dropped_conflicts, 1);
        stale.put_summary(2, ModuleSummary::default());
        stale.store(&path).unwrap();

        let mut merged = loaded(BuildCache::load(&path));
        assert!(merged.summary(1).is_none());
        assert!(merged.summary(2).is_some());
    }

    #[test]
    fn body_and_mapping_conflicts_or_complementary_halves_drop_the_group() {
        let mut latest = BuildCache::new();
        latest.put_body(7, Arc::new("latest".into()));
        let mut local = BuildCache::new();
        local.put_mappings(7, Arc::new(CachedModuleMappings::default()));
        let (merged, conflicts) = local.merge_with_latest(latest);
        assert_eq!(conflicts, 1);
        assert!(!merged.bodies.contains_key(&7));
        assert!(!merged.mappings.contains_key(&7));

        let mut latest = BuildCache::new();
        latest.put_emission(
            8,
            Arc::new("one".into()),
            Arc::new(CachedModuleMappings::default()),
        );
        let mut local = BuildCache::new();
        local.put_emission(
            8,
            Arc::new("two".into()),
            Arc::new(CachedModuleMappings::default()),
        );
        let (merged, conflicts) = local.merge_with_latest(latest);
        assert_eq!(conflicts, 1);
        assert!(!merged.bodies.contains_key(&8));
        assert!(!merged.mappings.contains_key(&8));
    }

    #[test]
    fn corrupt_latest_is_repaired_by_atomic_store() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/cache.bin");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"corrupt").unwrap();
        let mut cache = sample();
        let report = cache.store(&path).unwrap();
        assert!(report.repaired_corrupt_latest);
        assert!(!cache.is_dirty());
        assert!(matches!(
            BuildCache::load(&path),
            CacheLoadOutcome::Loaded(_)
        ));
        assert!(companion_lock_path(&path).is_file());
    }

    #[test]
    fn encode_failure_preserves_old_file_and_dirty_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.bin");
        let mut baseline = sample();
        baseline.store(&path).unwrap();
        let old = std::fs::read(&path).unwrap();

        let mut invalid = loaded(BuildCache::load(&path));
        invalid.put_summary(
            99,
            ModuleSummary {
                deps: vec![CachedDep {
                    specifier: "./bad.js".into(),
                    kind: 9,
                    lo: 0,
                    hi: 1,
                }],
                ..ModuleSummary::default()
            },
        );
        assert!(matches!(
            invalid.store(&path),
            Err(CacheStoreError::Encode(CacheEncodeError::InvalidValue(
                "dependency"
            )))
        ));
        assert!(invalid.is_dirty());
        assert_eq!(std::fs::read(&path).unwrap(), old);
    }

    #[test]
    fn failure_after_sync_before_replace_preserves_old_file_and_dirty_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.bin");
        let mut baseline = sample();
        baseline.store(&path).unwrap();
        let old = std::fs::read(&path).unwrap();

        let mut changed = loaded(BuildCache::load(&path));
        changed.put_summary(99, ModuleSummary::default());
        let error = changed
            .store_inner(&path, LOCK_WAIT_TIMEOUT, || {
                Err(io::Error::other("injected before replace"))
            })
            .unwrap_err();
        assert!(matches!(
            error,
            CacheStoreError::Io {
                stage: CacheStoreStage::Replace,
                ..
            }
        ));
        assert!(changed.is_dirty());
        assert_eq!(std::fs::read(&path).unwrap(), old);
    }

    #[test]
    fn emission_compaction_is_atomic() {
        let mut cache = BuildCache::new();
        for key in 0..3 {
            cache.put_emission(
                key,
                Arc::new(format!("body-{key}")),
                Arc::new(CachedModuleMappings::default()),
            );
        }
        assert!(cache.body(0).is_some());
        assert!(cache.mappings(0).is_some());
        cache.compact(CacheLimits {
            max_bytes: usize::MAX,
            max_entries: 2,
        });
        assert_eq!(cache.bodies.len(), 1);
        assert_eq!(cache.mappings.len(), 1);
        assert_eq!(
            cache.bodies.keys().copied().collect::<BTreeSet<_>>(),
            cache.mappings.keys().copied().collect()
        );
        assert!(cache.bodies.contains_key(&0));
    }

    #[test]
    fn missing_file_is_empty() {
        assert!(matches!(
            BuildCache::load(Path::new("does/not/exist.bin")),
            CacheLoadOutcome::Missing
        ));
    }

    #[test]
    fn directory_load_is_an_io_outcome() {
        let directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            BuildCache::load(directory.path()),
            CacheLoadOutcome::Io(_)
        ));
    }
}
