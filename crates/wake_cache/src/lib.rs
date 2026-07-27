//! wake_cache — wake_turbo 任务图与产物的**持久化层**（DESIGN §10.3 / PLAN §7.1）。
//!
//! 目标：让一个**全新进程**的冷构建跳过未变模块的 parse + codegen——把两类纯函数输出落盘：
//!
//! - **模块摘要**（`ModuleSummary`）：`content_key → (deps, uses, 顶层 await 标志)`。`content_key = hash(源类型 ‖ 源文本)`。
//!   有它就能不 parse 直接建依赖图、算 Tree Shaking 保留集。
//! - **codegen 产物**（`body`）：`(content_key, linker_key) → String`。`linker_key = hash(依赖 id 映射 ‖ keep ‖ dyn_chunks)`。
//!   两键都命中 → 该模块 parse 与 codegen 全跳过，直接取缓存体拼接。
//!
//! **健壮性**：值全是 `String`/`Vec`/整数——**绝不落 `ModuleAst`（自引用 arena）也绝不落 `Atom`**
//! （interner id 跨进程无意义；说明符已在此前解成 `String`）。这正是 PLAN「Atom 不落盘」的落地。
//! 任一字节变化 → `content_key` 变 → 未命中 → 重算，故**不可能产出陈旧结果**。
//!
//! **格式**：手写小端二进制 + `MAGIC`+`SCHEMA` 头；schema 不符（wake 自身的 parse/codegen 语义
//! 变更时应 bump `SCHEMA`）直接当空缓存忽略。rkyv 的零拷贝是更大规模时的优化，可后续替换（API 不变）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// 缓存文件魔数。
const MAGIC: &[u8; 4] = b"WKC1";
/// schema 版本：**wake 的 parse/codegen 输出语义变更时必须 +1**，否则可能取到陈旧产物。
const SCHEMA: u32 = 2;

/// 一条依赖（说明符 + 种类判别值 + 源码位置）。`kind` 用 `u8`（由调用方与 `DependencyKind` 互转），
/// 使本 crate 不依赖 AST 类型。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedDep {
    pub specifier: String,
    pub kind: u8,
    pub lo: u32,
    pub hi: u32,
}

/// 一条静态使用记录（Tree Shaking 用）。`all=true` 表示整体使用（namespace/export*），
/// `reexport=true` 且 `all=true` 表示 `export *`（仅当下游消费本模块导出时才传播）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedUse {
    pub specifier: String,
    pub all: bool,
    pub reexport: bool,
    pub names: Vec<String>,
}

/// 一个模块的摘要：依赖 + 静态使用 + 是否含顶层 await。足以建图 + 算 keep + 算 async 子图，无需 parse。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleSummary {
    pub deps: Vec<CachedDep>,
    pub uses: Vec<CachedUse>,
    /// 模块顶层出现过 `await` / `for await` → 打包器把它包成 `async function`。
    pub has_top_level_await: bool,
}

/// 持久化构建缓存：摘要表 + 产物表。
#[derive(Default)]
pub struct BuildCache {
    summaries: HashMap<u64, ModuleSummary>,
    /// codegen 产物体。存 `Arc<String>`：命中返回引用计数自增而非整体拷贝，
    /// 与 bundler 拼接侧的 `Arc<String>` 同构，消除全命中路径的两次全 bundle memcpy。
    bodies: HashMap<u128, Arc<String>>,
    /// 命中计数（诊断/测试用）。
    pub summary_hits: u64,
    pub body_hits: u64,
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
            Ok(bytes) => Self::decode(&bytes).unwrap_or_default(),
            Err(_) => BuildCache::default(),
        }
    }

    /// 原子落盘（临时文件 + rename，避开 Windows Defender 扫描锁；同 CLI 写产物的手法）。
    pub fn store(&self, path: &Path) -> std::io::Result<()> {
        let bytes = self.encode();
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                std::fs::write(path, &bytes).map_err(|_| e)
            }
        }
    }

    /// 查模块摘要（命中计数 +1）。
    pub fn summary(&mut self, content_key: u64) -> Option<ModuleSummary> {
        let s = self.summaries.get(&content_key).cloned();
        if s.is_some() {
            self.summary_hits += 1;
        }
        s
    }

    pub fn put_summary(&mut self, content_key: u64, summary: ModuleSummary) {
        self.summaries.insert(content_key, summary);
        self.dirty = true;
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
        }
        b
    }

    pub fn put_body(&mut self, key: u128, body: Arc<String>) {
        self.bodies.insert(key, body);
        self.dirty = true;
    }

    pub fn is_empty(&self) -> bool {
        self.summaries.is_empty() && self.bodies.is_empty()
    }

    // —— 编解码（手写小端二进制）——

    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(4096);
        b.extend_from_slice(MAGIC);
        put_u32(&mut b, SCHEMA);
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
        }
        // bodies
        put_u32(&mut b, self.bodies.len() as u32);
        for (k, body) in &self.bodies {
            put_u128(&mut b, *k);
            put_str(&mut b, body);
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
            cache.summaries.insert(
                key,
                ModuleSummary {
                    deps,
                    uses,
                    has_top_level_await,
                },
            );
        }
        let n_bodies = c.u32()?;
        for _ in 0..n_bodies {
            let key = c.u128()?;
            let body = c.str()?;
            cache.bodies.insert(key, Arc::new(body));
        }
        Some(cache)
    }
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
            },
        );
        c.put_body(
            0x1234_5678_9ABC_DEF0_1111_2222_3333_4444,
            Arc::new("exports.x = 1;".to_string()),
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
        assert_eq!(back.summary(0xDEAD_BEEF).unwrap().uses[0].names.len(), 2);
        assert!(back.summary(0xDEAD_BEEF).unwrap().has_top_level_await);
        assert_eq!(
            back.body(0x1234_5678_9ABC_DEF0_1111_2222_3333_4444)
                .as_deref()
                .map(String::as_str),
            Some("exports.x = 1;")
        );
        assert!(back.body(0).is_none());
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
        sample().store(&path).unwrap();
        let mut loaded = BuildCache::load(&path);
        assert_eq!(loaded.summary(0xDEAD_BEEF).unwrap().deps[0].kind, 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_empty() {
        let loaded = BuildCache::load(Path::new("does/not/exist.bin"));
        assert!(loaded.is_empty());
    }
}
