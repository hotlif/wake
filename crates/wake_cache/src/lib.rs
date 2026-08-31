//! wake_cache — wake_turbo 任务图与产物的**持久化层**（DESIGN §10.3 / PLAN §7.1）。
//!
//! 目标：让一个**全新进程**的冷构建跳过未变模块的源码读取、parse、optimize 与 body emit——把五类数据落盘：
//!
//! - **路径快照**：`path → (mtime, size, content_key, source)`，元数据命中时只 stat 一次。
//!   源码从单个缓存文件恢复，避免 Windows 对数千个小文件逐个触盘。
//! - **模块摘要**（`ModuleSummary`）：`content_key → (deps, uses, 顶层 await 标志)`。`content_key = hash(源类型 ‖ 源文本)`。
//!   有它就能不 parse 直接建依赖图、算 Tree Shaking 保留集。
//! - **优化事实**（`retained_module_ids`）：以不含最终 chunk 编号的 optimizer key 存储；驱动先
//!   收敛这些边并重新规划 chunk，再形成 body key。
//! - **codegen 产物**（`body`）：`(content_key, optimizer_key, final_layout_key) → String`。
//! - **source-map 映射事实**（`mappings`）：与 `body` 使用相同产物键但独立存取；body 发射始终
//!   记录并持久化它们，因此之后启用 source map 不需要重新 parse、optimize 或 emit body。
//!
//! **健壮性**：值全是 `String`/`Vec`/整数——**绝不落 `ModuleAst`（自引用 arena）也绝不落 `Atom`**
//! （interner id 跨进程无意义；说明符已在此前解成 `String`）。这正是 PLAN「Atom 不落盘」的落地。
//! 常规文件变化会改变 mtime 或 size，从而重新读取并由 `content_key` 做内容级失效。若外部工具刻意
//! 保留同一 mtime 与 size 却替换内容，需要清理缓存；这是用一次 stat 换取跨进程免读源码的明确取舍。
//! CSS、JSON、资源和依赖邻接文件系统状态的 loader 不使用路径快照。
//!
//! **格式**：手写小端二进制 + `MAGIC`+`SCHEMA` 头；schema 不符（wake 自身的 parse/codegen 语义
//! 变更时应 bump `SCHEMA`）直接当空缓存忽略。rkyv 的零拷贝是更大规模时的优化，可后续替换（API 不变）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 缓存文件魔数。
const MAGIC: &[u8; 4] = b"WKC1";
/// schema 版本：**wake 的 parse/codegen 输出语义变更时必须 +1**，否则可能取到陈旧产物。
const SCHEMA: u32 = 10;

/// Persistent caches are an optimization, so bounded retention is preferable to allowing edited
/// content versions to grow without limit for the lifetime of a project. The limits are deliberately
/// generous enough for large applications while preventing a forgotten cache from consuming an
/// unbounded amount of memory and disk.
const MAX_CACHE_BYTES: usize = 512 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 200_000;

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
}

/// 原文件元数据。命中时无需重新读取该小文件；`content_key` 仍负责内容级缓存寻址。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub size: u64,
    pub modified_ns: u128,
}

/// 路径索引命中项。源码随单个缓存文件顺序读取，避免 Windows 对大量源文件逐个触盘。
#[derive(Clone, Debug)]
pub struct CachedSource {
    pub stamp: FileStamp,
    pub content_key: u64,
    pub source: Arc<str>,
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

/// One proof-carrying generated request range stored with its byte-identical module body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CachedDiscardedStaticRequest {
    pub start: u32,
    pub end: u32,
    pub target_module_id: u32,
}

/// Complete module-local emission metadata stored independently from the JavaScript body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CachedModuleMappings {
    pub mappings: Vec<CachedMapping>,
    pub names: Vec<String>,
    pub discarded_static_requests: Vec<CachedDiscardedStaticRequest>,
}

#[derive(Clone, Debug)]
struct PathEntry {
    variant: u64,
    cached: CachedSource,
}

/// 持久化构建缓存：摘要表 + 产物表。
#[derive(Default)]
pub struct BuildCache {
    summaries: HashMap<u64, ModuleSummary>,
    /// 规范路径 → 文件元数据、配置变体、内容键与源码快照。
    paths: HashMap<PathBuf, PathEntry>,
    /// codegen 产物体。存 `Arc<String>`：命中返回引用计数自增而非整体拷贝，
    /// 与 bundler 拼接侧的 `Arc<String>` 同构，消除全命中路径的两次全 bundle memcpy。
    bodies: HashMap<u128, Arc<String>>,
    /// Optimizer-reported internal dependency targets. These stable numeric IDs are sorted and
    /// deduplicated by the bundler before crossing the persistent boundary.
    retained_module_ids: HashMap<u128, Arc<Vec<u32>>>,
    /// Source-map mapping facts are cached independently from JavaScript bodies. New body entries
    /// are committed atomically with their facts; schema 10 naturally misses legacy entries which
    /// lack generated request-range metadata.
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
    path_access: HashMap<PathBuf, u64>,
    summary_access: HashMap<u64, u64>,
    body_access: HashMap<u128, u64>,
    retained_dependency_access: HashMap<u128, u64>,
    mapping_access: HashMap<u128, u64>,
    /// 本次构建是否往缓存写过新条目。全命中（未变）时为 `false` → 跳过落盘，
    /// 免掉「重写 = 缓存体量」大小的磁盘 I/O（缓存文件常和 bundle 一样大）。
    dirty: bool,
}

impl BuildCache {
    /// 空缓存。
    pub fn new() -> BuildCache {
        BuildCache::default()
    }

    /// 从磁盘加载。文件不存在 / 损坏 / schema 不符 → 返回空缓存（缓存永远可重建，容错优先）。
    pub fn load(path: &Path) -> BuildCache {
        match std::fs::read(path) {
            Ok(bytes) => {
                let mut cache = Self::decode(&bytes).unwrap_or_default();
                // Older Wake versions could leave an arbitrarily large monolithic file. Compact
                // immediately so the next successful build commits it back under the current
                // budget; cache misses remain a correctness-neutral fallback.
                cache.compact(CacheLimits::default());
                cache
            }
            Err(_) => BuildCache::default(),
        }
    }

    /// 原子落盘（临时文件 + rename，避开 Windows Defender 扫描锁；同 CLI 写产物的手法）。
    pub fn store(&mut self, path: &Path) -> std::io::Result<()> {
        self.compact(CacheLimits::default());
        let bytes = self.encode();
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        let result = match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                std::fs::write(path, &bytes).map_err(|_| e)
            }
        };
        if result.is_ok() {
            // `dirty` describes mutations since the last durable commit. Keeping it set after a
            // successful store makes every later cache-hit build rewrite the complete cache file.
            self.dirty = false;
        }
        result
    }

    /// 查询路径源码快照；配置变体不一致时视为 miss。
    pub fn cached_source(&mut self, path: &Path, variant: u64) -> Option<CachedSource> {
        let cached = self
            .paths
            .get(path)
            .filter(|entry| entry.variant == variant)
            .map(|entry| entry.cached.clone());
        if cached.is_some() {
            let access = self.next_access();
            self.path_access.insert(path.to_path_buf(), access);
        }
        cached
    }

    /// 更新路径索引。内容完全相同时不置 dirty，避免全命中构建重写缓存文件。
    pub fn put_source(
        &mut self,
        path: &Path,
        stamp: FileStamp,
        variant: u64,
        content_key: u64,
        source: &str,
    ) {
        let access = self.next_access();
        self.path_access.insert(path.to_path_buf(), access);
        let unchanged = self.paths.get(path).is_some_and(|entry| {
            entry.variant == variant
                && entry.cached.stamp == stamp
                && entry.cached.content_key == content_key
                && entry.cached.source.as_ref() == source
        });
        if unchanged {
            return;
        }
        self.paths.insert(
            path.to_path_buf(),
            PathEntry {
                variant,
                cached: CachedSource {
                    stamp,
                    content_key,
                    source: Arc::from(source),
                },
            },
        );
        self.dirty = true;
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

    pub fn put_body(&mut self, key: u128, body: Arc<String>) {
        let access = self.next_access();
        self.body_access.insert(key, access);
        if self
            .bodies
            .get(&key)
            .is_none_or(|current| current.as_str() != body.as_str())
        {
            self.bodies.insert(key, body);
            self.dirty = true;
        }
    }

    /// Query optimizer-owned internal dependency edges by optimizer key (independent of body
    /// layout and source-map requests).
    pub fn retained_module_ids(&mut self, key: u128) -> Option<Arc<Vec<u32>>> {
        let ids = self.retained_module_ids.get(&key).cloned();
        if ids.is_some() {
            self.retained_dependency_hits += 1;
            let access = self.next_access();
            self.retained_dependency_access.insert(key, access);
        }
        ids
    }

    pub fn put_retained_module_ids(&mut self, key: u128, ids: Arc<Vec<u32>>) {
        debug_assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        let access = self.next_access();
        self.retained_dependency_access.insert(key, access);
        if self
            .retained_module_ids
            .get(&key)
            .is_none_or(|current| current.as_ref() != ids.as_ref())
        {
            self.retained_module_ids.insert(key, ids);
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

    pub fn put_mappings(&mut self, key: u128, mappings: Arc<CachedModuleMappings>) {
        let access = self.next_access();
        self.mapping_access.insert(key, access);
        if self
            .mappings
            .get(&key)
            .is_none_or(|current| current.as_ref() != mappings.as_ref())
        {
            self.mappings.insert(key, mappings);
            self.dirty = true;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
            && self.summaries.is_empty()
            && self.bodies.is_empty()
            && self.retained_module_ids.is_empty()
            && self.mappings.is_empty()
    }

    fn next_access(&mut self) -> u64 {
        self.access_clock = self.access_clock.saturating_add(1);
        self.access_clock
    }

    fn estimated_size(&self) -> usize {
        let paths = self
            .paths
            .iter()
            .map(|(path, entry)| path.to_string_lossy().len() + entry.cached.source.len() + 64);
        let summaries = self.summaries.values().map(summary_estimated_size);
        let bodies = self.bodies.values().map(|body| body.len() + 32);
        let retained_module_ids = self
            .retained_module_ids
            .values()
            .map(|ids| ids.len() * std::mem::size_of::<u32>() + 32);
        let mappings = self
            .mappings
            .values()
            .map(|mappings| cached_mappings_estimated_size(mappings));
        paths
            .chain(summaries)
            .chain(bodies)
            .chain(retained_module_ids)
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
            Path(PathBuf),
            Summary(u64),
            Body(u128),
            RetainedDependencies(u128),
            Mapping(u128),
        }

        let mut candidates = Vec::with_capacity(
            self.paths.len()
                + self.summaries.len()
                + self.bodies.len()
                + self.retained_module_ids.len()
                + self.mappings.len(),
        );
        candidates.extend(self.paths.keys().map(|key| {
            (
                self.path_access.get(key).copied().unwrap_or(0),
                Key::Path(key.clone()),
            )
        }));
        candidates.extend(self.summaries.keys().map(|&key| {
            (
                self.summary_access.get(&key).copied().unwrap_or(0),
                Key::Summary(key),
            )
        }));
        candidates.extend(self.bodies.keys().map(|&key| {
            (
                self.body_access.get(&key).copied().unwrap_or(0),
                Key::Body(key),
            )
        }));
        candidates.extend(self.retained_module_ids.keys().map(|&key| {
            (
                self.retained_dependency_access
                    .get(&key)
                    .copied()
                    .unwrap_or(0),
                Key::RetainedDependencies(key),
            )
        }));
        candidates.extend(self.mappings.keys().map(|&key| {
            (
                self.mapping_access.get(&key).copied().unwrap_or(0),
                Key::Mapping(key),
            )
        }));
        // Oldest first. Tie-breakers do not affect correctness; stable key ordering makes retained
        // contents deterministic for a fixed cache state.
        candidates.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| match (&left.1, &right.1) {
                    (Key::Path(a), Key::Path(b)) => a.cmp(b),
                    (Key::Summary(a), Key::Summary(b)) => a.cmp(b),
                    (Key::Body(a), Key::Body(b)) => a.cmp(b),
                    (Key::RetainedDependencies(a), Key::RetainedDependencies(b)) => a.cmp(b),
                    (Key::Mapping(a), Key::Mapping(b)) => a.cmp(b),
                    (Key::Path(_), _) => std::cmp::Ordering::Less,
                    (Key::Summary(_), _) => std::cmp::Ordering::Less,
                    (Key::Body(_), Key::RetainedDependencies(_) | Key::Mapping(_)) => {
                        std::cmp::Ordering::Less
                    }
                    (Key::RetainedDependencies(_), Key::Mapping(_)) => std::cmp::Ordering::Less,
                    _ => std::cmp::Ordering::Greater,
                })
        });

        let mut entries = candidates.len();
        let mut bytes = self.estimated_size();
        for (_, key) in candidates {
            if entries <= limits.max_entries && bytes <= limits.max_bytes {
                break;
            }
            let removed = match key {
                Key::Path(key) => {
                    self.path_access.remove(&key);
                    self.paths
                        .remove(&key)
                        .map(|entry| key.to_string_lossy().len() + entry.cached.source.len() + 64)
                }
                Key::Summary(key) => {
                    self.summary_access.remove(&key);
                    self.summaries
                        .remove(&key)
                        .map(|entry| summary_estimated_size(&entry))
                }
                Key::Body(key) => {
                    self.body_access.remove(&key);
                    self.bodies.remove(&key).map(|entry| entry.len() + 32)
                }
                Key::RetainedDependencies(key) => {
                    self.retained_dependency_access.remove(&key);
                    self.retained_module_ids
                        .remove(&key)
                        .map(|entry| entry.len() * std::mem::size_of::<u32>() + 32)
                }
                Key::Mapping(key) => {
                    self.mapping_access.remove(&key);
                    self.mappings
                        .remove(&key)
                        .map(|entry| cached_mappings_estimated_size(&entry))
                }
            };
            if let Some(removed) = removed {
                entries -= 1;
                bytes = bytes.saturating_sub(removed);
                self.dirty = true;
            }
        }
    }

    // —— 编解码（手写小端二进制）——

    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(4096);
        b.extend_from_slice(MAGIC);
        put_u32(&mut b, SCHEMA);
        // path index
        put_u32(&mut b, self.paths.len() as u32);
        for (path, entry) in &self.paths {
            put_str(&mut b, &path.to_string_lossy());
            put_u64(&mut b, entry.variant);
            put_u64(&mut b, entry.cached.stamp.size);
            put_u128(&mut b, entry.cached.stamp.modified_ns);
            put_u64(&mut b, entry.cached.content_key);
            put_str(&mut b, &entry.cached.source);
        }
        // summaries
        put_u32(&mut b, self.summaries.len() as u32);
        for (k, s) in &self.summaries {
            put_u64(&mut b, *k);
            put_u32(&mut b, s.deps.len() as u32);
            for d in &s.deps {
                put_str(&mut b, &d.specifier);
                b.push(d.kind);
                put_u32(&mut b, d.lo);
                put_u32(&mut b, d.hi);
            }
            put_u32(&mut b, s.uses.len() as u32);
            for u in &s.uses {
                put_str(&mut b, &u.specifier);
                b.push(u.all as u8);
                b.push(u.reexport as u8);
                put_u32(&mut b, u.names.len() as u32);
                for n in &u.names {
                    put_str(&mut b, n);
                }
            }
            b.push(s.has_top_level_await as u8);
            put_liveness(&mut b, &s.liveness);
            b.push(s.concat_is_esm as u8);
            b.push(s.concat_block_safe as u8);
        }
        // bodies
        put_u32(&mut b, self.bodies.len() as u32);
        for (k, body) in &self.bodies {
            put_u128(&mut b, *k);
            put_str(&mut b, body);
        }
        // optimizer-owned retained internal dependency targets
        put_u32(&mut b, self.retained_module_ids.len() as u32);
        for (key, ids) in &self.retained_module_ids {
            put_u128(&mut b, *key);
            put_u32(&mut b, ids.len() as u32);
            for id in ids.iter() {
                put_u32(&mut b, *id);
            }
        }
        // source maps (kept separate so body-only builds do not require map entries)
        put_u32(&mut b, self.mappings.len() as u32);
        for (key, mappings) in &self.mappings {
            put_u128(&mut b, *key);
            put_u32(&mut b, mappings.mappings.len() as u32);
            for mapping in &mappings.mappings {
                put_u32(&mut b, mapping.gen_line);
                put_u32(&mut b, mapping.gen_col);
                put_u32(&mut b, mapping.src_index);
                put_u32(&mut b, mapping.src_offset);
                put_u32(&mut b, mapping.name_index.unwrap_or(u32::MAX));
                b.push(mapping.is_unmapped as u8);
            }
            put_u32(&mut b, mappings.names.len() as u32);
            for name in &mappings.names {
                put_str(&mut b, name);
            }
            put_u32(&mut b, mappings.discarded_static_requests.len() as u32);
            for request in &mappings.discarded_static_requests {
                put_u32(&mut b, request.start);
                put_u32(&mut b, request.end);
                put_u32(&mut b, request.target_module_id);
            }
        }
        b
    }

    fn decode(bytes: &[u8]) -> Option<BuildCache> {
        let mut c = Cursor { b: bytes, pos: 0 };
        if c.take(4)? != MAGIC {
            return None;
        }
        if c.u32()? != SCHEMA {
            return None; // schema 变更 → 忽略旧缓存
        }
        let mut cache = BuildCache::default();
        let n_paths = c.u32()?;
        for _ in 0..n_paths {
            let path = PathBuf::from(c.str()?);
            let variant = c.u64()?;
            let stamp = FileStamp {
                size: c.u64()?,
                modified_ns: c.u128()?,
            };
            let content_key = c.u64()?;
            let source: Arc<str> = Arc::from(c.str()?);
            cache.paths.insert(
                path,
                PathEntry {
                    variant,
                    cached: CachedSource {
                        stamp,
                        content_key,
                        source,
                    },
                },
            );
        }
        let n_sum = c.u32()?;
        for _ in 0..n_sum {
            let key = c.u64()?;
            let n_deps = c.u32()?;
            let mut deps = Vec::with_capacity(n_deps as usize);
            for _ in 0..n_deps {
                let specifier = c.str()?;
                let kind = c.u8()?;
                let lo = c.u32()?;
                let hi = c.u32()?;
                deps.push(CachedDep {
                    specifier,
                    kind,
                    lo,
                    hi,
                });
            }
            let n_uses = c.u32()?;
            let mut uses = Vec::with_capacity(n_uses as usize);
            for _ in 0..n_uses {
                let specifier = c.str()?;
                let all = c.u8()? != 0;
                let reexport = c.u8()? != 0;
                let n_names = c.u32()?;
                let mut names = Vec::with_capacity(n_names as usize);
                for _ in 0..n_names {
                    names.push(c.str()?);
                }
                uses.push(CachedUse {
                    specifier,
                    all,
                    reexport,
                    names,
                });
            }
            let has_top_level_await = c.u8()? != 0;
            let liveness = c.liveness()?;
            let concat_is_esm = c.u8()? != 0;
            let concat_block_safe = c.u8()? != 0;
            cache.summaries.insert(
                key,
                ModuleSummary {
                    deps,
                    uses,
                    has_top_level_await,
                    liveness,
                    concat_is_esm,
                    concat_block_safe,
                },
            );
        }
        let n_bodies = c.u32()?;
        for _ in 0..n_bodies {
            let key = c.u128()?;
            let body = c.str()?;
            cache.bodies.insert(key, Arc::new(body));
        }
        let n_retained_dependency_entries = c.u32()?;
        for _ in 0..n_retained_dependency_entries {
            let key = c.u128()?;
            let count = c.u32()?;
            let mut ids = Vec::with_capacity(count as usize);
            for _ in 0..count {
                ids.push(c.u32()?);
            }
            if !ids.windows(2).all(|pair| pair[0] < pair[1]) {
                return None;
            }
            cache.retained_module_ids.insert(key, Arc::new(ids));
        }
        let n_mapping_entries = c.u32()?;
        for _ in 0..n_mapping_entries {
            let key = c.u128()?;
            let count = c.u32()?;
            let mut mappings = Vec::with_capacity(count as usize);
            for _ in 0..count {
                mappings.push(CachedMapping {
                    gen_line: c.u32()?,
                    gen_col: c.u32()?,
                    src_index: c.u32()?,
                    src_offset: c.u32()?,
                    name_index: match c.u32()? {
                        u32::MAX => None,
                        index => Some(index),
                    },
                    is_unmapped: c.u8()? != 0,
                });
            }
            let name_count = c.u32()?;
            let mut names = Vec::with_capacity(name_count as usize);
            for _ in 0..name_count {
                names.push(c.str()?);
            }
            let request_count = c.u32()?;
            let mut discarded_static_requests = Vec::with_capacity(request_count as usize);
            for _ in 0..request_count {
                discarded_static_requests.push(CachedDiscardedStaticRequest {
                    start: c.u32()?,
                    end: c.u32()?,
                    target_module_id: c.u32()?,
                });
            }
            if mappings.iter().any(|mapping| {
                (!mapping.is_unmapped
                    && mapping
                        .name_index
                        .is_some_and(|index| index as usize >= names.len()))
                    || (mapping.is_unmapped && mapping.name_index.is_some())
            }) {
                return None;
            }
            if discarded_static_requests
                .windows(2)
                .any(|pair| pair[0].end > pair[1].start)
                || discarded_static_requests
                    .iter()
                    .any(|request| request.start >= request.end)
            {
                return None;
            }
            cache.mappings.insert(
                key,
                Arc::new(CachedModuleMappings {
                    mappings,
                    names,
                    discarded_static_requests,
                }),
            );
        }
        Some(cache)
    }
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
        + mappings.discarded_static_requests.len()
            * std::mem::size_of::<CachedDiscardedStaticRequest>()
        + 32
}

// —— 写原语 ——
fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_u128(b: &mut Vec<u8>, v: u128) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_str(b: &mut Vec<u8>, s: &str) {
    put_u32(b, s.len() as u32);
    b.extend_from_slice(s.as_bytes());
}

fn put_strings(b: &mut Vec<u8>, values: &[String]) {
    put_u32(b, values.len() as u32);
    for value in values {
        put_str(b, value);
    }
}

fn put_liveness(b: &mut Vec<u8>, l: &CachedLiveness) {
    put_u32(b, l.decls.len() as u32);
    for (name, refs) in &l.decls {
        put_str(b, name);
        put_strings(b, refs);
    }
    put_strings(b, &l.root_refs);
    put_u32(b, l.named_imports.len() as u32);
    for import in &l.named_imports {
        put_str(b, &import.local);
        put_str(b, &import.spec);
        put_str(b, &import.imported);
    }
    put_u32(b, l.namespace_imports.len() as u32);
    for (local, spec) in &l.namespace_imports {
        put_str(b, local);
        put_str(b, spec);
    }
    put_strings(b, &l.reexport_star);
    put_u32(b, l.ns_reexports.len() as u32);
    for (name, spec) in &l.ns_reexports {
        put_str(b, name);
        put_str(b, spec);
    }
    put_u32(b, l.reexport_named.len() as u32);
    for (name, spec, imported) in &l.reexport_named {
        put_str(b, name);
        put_str(b, spec);
        put_str(b, imported);
    }
    put_u32(b, l.exports.len() as u32);
    for (name, local) in &l.exports {
        put_str(b, name);
        b.push(local.is_some() as u8);
        if let Some(local) = local {
            put_str(b, local);
        }
    }
}

// —— 读游标 ——
struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn u128(&mut self) -> Option<u128> {
        Some(u128::from_le_bytes(self.take(16)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        let s = self.take(n)?;
        String::from_utf8(s.to_vec()).ok()
    }
    fn strings(&mut self) -> Option<Vec<String>> {
        let n = self.u32()? as usize;
        (0..n).map(|_| self.str()).collect()
    }
    fn liveness(&mut self) -> Option<CachedLiveness> {
        let decl_count = self.u32()? as usize;
        let mut decls = Vec::with_capacity(decl_count);
        for _ in 0..decl_count {
            decls.push((self.str()?, self.strings()?));
        }
        let root_refs = self.strings()?;
        let import_count = self.u32()? as usize;
        let mut named_imports = Vec::with_capacity(import_count);
        for _ in 0..import_count {
            named_imports.push(CachedNamedImport {
                local: self.str()?,
                spec: self.str()?,
                imported: self.str()?,
            });
        }
        let namespace_count = self.u32()? as usize;
        let mut namespace_imports = Vec::with_capacity(namespace_count);
        for _ in 0..namespace_count {
            namespace_imports.push((self.str()?, self.str()?));
        }
        let reexport_star = self.strings()?;
        let ns_count = self.u32()? as usize;
        let mut ns_reexports = Vec::with_capacity(ns_count);
        for _ in 0..ns_count {
            ns_reexports.push((self.str()?, self.str()?));
        }
        let named_count = self.u32()? as usize;
        let mut reexport_named = Vec::with_capacity(named_count);
        for _ in 0..named_count {
            reexport_named.push((self.str()?, self.str()?, self.str()?));
        }
        let export_count = self.u32()? as usize;
        let mut exports = Vec::with_capacity(export_count);
        for _ in 0..export_count {
            let name = self.str()?;
            let local = if self.u8()? != 0 {
                Some(self.str()?)
            } else {
                None
            };
            exports.push((name, local));
        }
        Some(CachedLiveness {
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
            },
        );
        c.put_body(
            0x1234_5678_9ABC_DEF0_1111_2222_3333_4444,
            Arc::new("exports.x = 1;".to_string()),
        );
        c.put_retained_module_ids(
            0x1234_5678_9ABC_DEF0_1111_2222_3333_4444,
            Arc::new(vec![2, 9]),
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
                discarded_static_requests: vec![CachedDiscardedStaticRequest {
                    start: 0,
                    end: 7,
                    target_module_id: 9,
                }],
            }),
        );
        c.put_source(
            Path::new("src/index.js"),
            FileStamp {
                size: 19,
                modified_ns: 42,
            },
            7,
            0xDEAD_BEEF,
            "export const x=1;",
        );
        c
    }

    #[test]
    fn roundtrip_encode_decode() {
        let c = sample();
        let bytes = c.encode();
        let mut back = BuildCache::decode(&bytes).expect("decode");
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
            Some("exports.x = 1;")
        );
        assert!(back.body(0).is_none());
        assert_eq!(
            back.retained_module_ids(0x1234_5678_9ABC_DEF0_1111_2222_3333_4444)
                .expect("cached retained dependencies")
                .as_slice(),
            [2, 9]
        );
        assert!(back.retained_module_ids(0).is_none());
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
                .discarded_static_requests,
            [CachedDiscardedStaticRequest {
                start: 0,
                end: 7,
                target_module_id: 9,
            }]
        );
        assert!(back.mappings(0).is_none());
        let source = back
            .cached_source(Path::new("src/index.js"), 7)
            .expect("path source");
        assert_eq!(source.stamp.modified_ns, 42);
        assert_eq!(source.content_key, 0xDEAD_BEEF);
        assert_eq!(source.source.as_ref(), "export const x=1;");
        assert!(back.cached_source(Path::new("src/index.js"), 8).is_none());
    }

    #[test]
    fn bad_magic_or_schema_is_empty() {
        assert!(BuildCache::decode(b"XXXX....").is_none());
        // 正确 magic 但错 schema。
        let mut bytes = MAGIC.to_vec();
        put_u32(&mut bytes, SCHEMA + 1);
        assert!(BuildCache::decode(&bytes).is_none());
    }

    #[test]
    fn store_load_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("wake_cache_roundtrip_test.bin");
        let mut cache = sample();
        cache.store(&path).unwrap();
        let mut loaded = BuildCache::load(&path);
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
        assert!(cache.retained_module_ids(7).is_none());
        assert!(cache.mappings(7).is_none());

        cache.put_retained_module_ids(7, Arc::new(vec![2, 11]));
        assert_eq!(
            cache.retained_module_ids(7).expect("edges").as_slice(),
            [2, 11]
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
                discarded_static_requests: Vec::new(),
            }),
        );
        assert!(cache.body(7).is_some());
        let mappings = cache.mappings(7).expect("map");
        assert_eq!(mappings.mappings.len(), 2);
        assert_eq!(mappings.mappings[1].name_index, None);
        assert_eq!(mappings.names, ["original"]);
    }

    #[test]
    fn missing_file_is_empty() {
        let loaded = BuildCache::load(Path::new("does/not/exist.bin"));
        assert!(loaded.is_empty());
    }
}
