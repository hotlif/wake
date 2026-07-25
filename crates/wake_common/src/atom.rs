//! 字符串驻留（Atom）：标识符 / 字符串字面量 / 模块路径统一驻留为 `u32`。
//!
//! 比较退化为 `u32 == u32`，跨线程无拷贝——parser 与 bundler 同构设计的第一块基石
//! （DESIGN §4.1）。分片锁 + `FxHashMap` 降低 Scan 阶段全线程高频写的竞争。
//!
//! **正确性纪律（DESIGN §10.3）**：`Atom` 是进程内句柄，**禁止落盘**——
//! 持久化前必须还原为字符串。因此这里刻意 **不** 为 `Atom` 实现 `Serialize`/`rkyv`。

use std::sync::Mutex;

use rustc_hash::FxHashMap;

/// 分片数（2 的幂）。Atom 高 [`SHARD_BITS`] 位编码分片号，低位编码片内序号。
const SHARD_COUNT: usize = 16;
const SHARD_BITS: u32 = 4; // log2(SHARD_COUNT)
const INDEX_BITS: u32 = u32::BITS - SHARD_BITS; // 28
const INDEX_MASK: u32 = (1u32 << INDEX_BITS) - 1;

/// 驻留后的字符串句柄。`Copy`、`u32` 大小，比较为常数时间。
///
/// 仅在同一个 [`Interner`] 内有意义；跨 interner / 跨进程无效。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Atom(u32);

impl Atom {
    #[inline]
    fn encode(shard: usize, index: u32) -> Atom {
        debug_assert!(index <= INDEX_MASK, "单分片驻留字符串超过 2^28 上限");
        Atom(((shard as u32) << INDEX_BITS) | index)
    }

    #[inline]
    fn shard(self) -> usize {
        (self.0 >> INDEX_BITS) as usize
    }

    #[inline]
    fn index(self) -> u32 {
        self.0 & INDEX_MASK
    }

    /// 原始 `u32` 表示（调试 / 稳定排序用，勿落盘）。
    #[inline]
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl std::fmt::Debug for Atom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Atom(#{})", self.0)
    }
}

#[derive(Default)]
struct Shard {
    /// 去重：`&str`（借自 `store`）→ 片内序号。用 `Box<str>` 拥有所有权。
    map: FxHashMap<Box<str>, u32>,
    /// 片内序号 → 驻留字符串。`Box<str>` 一经插入地址稳定。
    store: Vec<Box<str>>,
}

/// 字符串驻留表。分片锁，`intern` 无需全局锁；`resolve` 只锁对应分片。
///
/// 通常整个进程共享一个 `Interner`（放进编译上下文里以 `&` 传递）。
pub struct Interner {
    shards: Box<[Mutex<Shard>]>,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    pub fn new() -> Interner {
        let shards = (0..SHARD_COUNT)
            .map(|_| Mutex::new(Shard::default()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Interner { shards }
    }

    #[inline]
    fn shard_of(&self, s: &str) -> usize {
        // 用 FxHash 选片：并行 Scan 阶段全线程高频写全局 interner，分片分布要均匀以降低竞争。
        use std::hash::{BuildHasher, BuildHasherDefault};
        let h = BuildHasherDefault::<rustc_hash::FxHasher>::default().hash_one(s);
        (h as usize) & (SHARD_COUNT - 1)
    }

    /// 驻留一个字符串，返回其 [`Atom`]。同一字符串多次调用返回相同 Atom。
    pub fn intern(&self, s: &str) -> Atom {
        let shard_idx = self.shard_of(s);
        let mut shard = self.shards[shard_idx].lock().unwrap();
        if let Some(&index) = shard.map.get(s) {
            return Atom::encode(shard_idx, index);
        }
        let index = shard.store.len() as u32;
        let owned: Box<str> = s.into();
        shard.store.push(owned.clone());
        shard.map.insert(owned, index);
        Atom::encode(shard_idx, index)
    }

    /// 还原 Atom 为字符串。返回拥有所有权的副本，避免持锁跨越调用边界。
    ///
    /// 热路径不应调用此函数（比较用 Atom 即可）；仅诊断 / codegen / 落盘前使用。
    pub fn resolve(&self, atom: Atom) -> String {
        let shard = self.shards[atom.shard()].lock().unwrap();
        shard.store[atom.index() as usize].as_ref().to_owned()
    }

    /// 以闭包借用形式还原，避免分配（诊断渲染热路径用）。
    pub fn with_resolved<R>(&self, atom: Atom, f: impl FnOnce(&str) -> R) -> R {
        let shard = self.shards[atom.shard()].lock().unwrap();
        f(shard.store[atom.index() as usize].as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_string_same_atom() {
        let it = Interner::new();
        let a = it.intern("foo");
        let b = it.intern("foo");
        let c = it.intern("bar");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(it.resolve(a), "foo");
        assert_eq!(it.resolve(c), "bar");
    }

    #[test]
    fn atom_is_u32_sized() {
        assert_eq!(std::mem::size_of::<Atom>(), 4);
    }

    #[test]
    fn many_strings_roundtrip() {
        let it = Interner::new();
        let atoms: Vec<(Atom, String)> = (0..5000)
            .map(|i| {
                let s = format!("ident_{i}");
                (it.intern(&s), s)
            })
            .collect();
        // 重复驻留必须命中同一 Atom
        for (atom, s) in &atoms {
            assert_eq!(it.intern(s), *atom);
            assert_eq!(&it.resolve(*atom), s);
        }
    }

    #[test]
    fn concurrent_intern_is_consistent() {
        use std::sync::Arc;
        use std::thread;

        let it = Arc::new(Interner::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let it = Arc::clone(&it);
            handles.push(thread::spawn(move || {
                (0..1000)
                    .map(|i| it.intern(&format!("k{}", i % 200)))
                    .collect::<Vec<_>>()
            }));
        }
        let results: Vec<Vec<Atom>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        // 所有线程对同一字符串必须得到相同 Atom。
        let baseline = &results[0];
        for other in &results[1..] {
            assert_eq!(baseline, other);
        }
        // 且相同键值的 Atom 全局唯一。
        for row in &results {
            for (i, atom) in row.iter().enumerate() {
                assert_eq!(it.resolve(*atom), format!("k{}", i % 200));
            }
        }
    }

    #[test]
    fn with_resolved_no_alloc() {
        let it = Interner::new();
        let a = it.intern("hello");
        let len = it.with_resolved(a, |s| s.len());
        assert_eq!(len, 5);
    }
}
