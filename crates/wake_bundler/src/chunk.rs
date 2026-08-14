//! # Chunk 图（代码分割，DESIGN §6.3 / PLAN §6.5）
//!
//! 纯函数 [`compute_chunk_graph`]：从「每模块静态/动态依赖边」算出 chunk 划分。**无 wake_turbo
//! 任务、无 IO**——在驱动线程跑，不计入引擎 `exec_count`，输出确定（同输入逐字节一致）。
//!
//! ## 算法（「每 root 静态闭包 + owner 集分桶」）
//!
//! 1. **root** = 入口 + 每个动态 import 目标（async root）；
//! 2. 每 root 的**静态闭包**（只走静态边 import/export-from/require，不跨动态边）；
//! 3. 模块 `m` 的 **owner 集** = 静态可达它的 root 集合；
//! 4. 分桶：owner 含入口 → **entry chunk**（入口恒先加载、恒可用，吸收共享到入口的模块）；
//!    否则全是 async root——owner 数 ≥ `share_threshold` → **shared chunk**（抽取避免重复），
//!    否则 → 该 async root 独占的 **async chunk**；
//! 5. chunk id 按 `BucketKey` 有序分配（entry 恒 0），保证确定性；
//! 6. chunk 间**静态依赖边**（`m@cm` 静态引用 `t@ct`，`ct≠cm` 且 `ct≠0`）→ 运行时 `ensure` 先加载。
//!
//! ## DAG 性质（无环，`ensure` 递归必终止）
//!
//! 沿静态边 `m→n` 有 `owners(m) ⊆ owners(n)`（能静态到达 `m` 的 root 也到达 `n`）。同 chunk 内
//! owner 集相等；跨 chunk 静态边只从「小 owner 集」指向「大 owner 集」，不可能互为真子集 ⇒ chunk 图无环。
//!
//! 无 async chunk（项目无跨 chunk 动态 import）时返回 `None` ⇒ 打包走旧单包路径（产物逐字节不变）。

use std::collections::BTreeMap;

use wake_common::{FxHashMap, FxHashSet};

use crate::ChunkKind;

/// 一个模块的依赖边（供 chunk 划分）。
pub(crate) struct ModuleEdges {
    /// 静态依赖目标模块 id（import / export-from / require；去重排序）。
    pub static_targets: Vec<u32>,
    /// 动态 import 目标模块 id（去重排序）。
    pub dyn_targets: Vec<u32>,
    /// 命名用文件 stem（chunk 名）。
    pub stem: String,
}

/// 一个 chunk 的规划。
#[derive(Clone)]
pub(crate) struct ChunkPlan {
    pub id: u32,
    pub kind: ChunkKind,
    pub name: String,
    /// 承载的模块 id（升序）。
    pub modules: Vec<u32>,
}

/// chunk 图结果。
#[derive(Clone)]
pub(crate) struct ChunkGraph {
    /// 模块 id → chunk id。
    pub module_chunk: FxHashMap<u32, u32>,
    /// chunk id → 依赖的其它 chunk id（须先加载；升序，不含 entry=0）。
    pub chunk_deps: FxHashMap<u32, Vec<u32>>,
    /// 全部 chunk（按 id 升序）。
    pub chunks: Vec<ChunkPlan>,
}

/// 分桶 key。变体声明顺序即 `Ord` 顺序——`Entry` 最小 ⇒ chunk id 恒为 0。
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
enum BucketKey {
    Entry,
    Shared(Vec<u32>),
    Async(u32),
}

/// 计算 chunk 图。无 async chunk → `None`（走单包路径）。
pub(crate) fn compute_chunk_graph(
    edges: &FxHashMap<u32, ModuleEdges>,
    entry_id: u32,
    share_threshold: usize,
) -> Option<ChunkGraph> {
    // 1. async roots（升序去重，去掉入口自身）。
    let mut async_roots: Vec<u32> = edges
        .values()
        .flat_map(|e| e.dyn_targets.iter().copied())
        .collect();
    async_roots.sort_unstable();
    async_roots.dedup();
    async_roots.retain(|r| *r != entry_id);

    let mut roots: Vec<u32> = vec![entry_id];
    roots.extend(async_roots.iter().copied());

    // 2. 每 root 静态闭包。
    let closure = |r: u32| -> FxHashSet<u32> {
        let mut seen = FxHashSet::default();
        let mut stack = vec![r];
        while let Some(x) = stack.pop() {
            if !seen.insert(x) {
                continue;
            }
            if let Some(e) = edges.get(&x) {
                for &t in &e.static_targets {
                    if !seen.contains(&t) {
                        stack.push(t);
                    }
                }
            }
        }
        seen
    };
    let sc: FxHashMap<u32, FxHashSet<u32>> = roots.iter().map(|&r| (r, closure(r))).collect();

    // 3+4. owner 集 → 分桶。
    let mut buckets: BTreeMap<BucketKey, Vec<u32>> = BTreeMap::new();
    let mut all_mods: Vec<u32> = edges.keys().copied().collect();
    all_mods.sort_unstable();
    for &m in &all_mods {
        let owners: Vec<u32> = roots
            .iter()
            .copied()
            .filter(|r| sc[r].contains(&m))
            .collect();
        if owners.is_empty() {
            continue; // 不可达（正常不出现）
        }
        let key = if owners.contains(&entry_id) {
            BucketKey::Entry
        } else if owners.len() >= share_threshold {
            BucketKey::Shared(owners.clone())
        } else {
            BucketKey::Async(owners[0])
        };
        buckets.entry(key).or_default().push(m);
    }

    // 无 async/shared chunk（只有 entry）→ 单包路径。
    if buckets.len() <= 1 {
        return None;
    }

    // 5. chunk id 分配（BTreeMap 有序 ⇒ 确定；Entry 最小 ⇒ 0）。
    let mut chunk_id_of: BTreeMap<BucketKey, u32> = BTreeMap::new();
    for (next, key) in buckets.keys().enumerate() {
        chunk_id_of.insert(key.clone(), next as u32);
    }
    debug_assert_eq!(chunk_id_of.get(&BucketKey::Entry), Some(&0));

    let mut module_chunk: FxHashMap<u32, u32> = FxHashMap::default();
    for (key, mods) in &buckets {
        let cid = chunk_id_of[key];
        for &m in mods {
            module_chunk.insert(m, cid);
        }
    }

    // chunk plans（名字冲突追加 _<id>）。
    let mut used_names: FxHashSet<String> = FxHashSet::default();
    let mut chunks: Vec<ChunkPlan> = Vec::new();
    for (key, mods) in &buckets {
        let cid = chunk_id_of[key];
        let (kind, base) = match key {
            BucketKey::Entry => (ChunkKind::Initial, stem_of(edges, entry_id, "index")),
            BucketKey::Async(root) => (ChunkKind::Async, stem_of(edges, *root, "chunk")),
            BucketKey::Shared(_) => (ChunkKind::Shared, "shared".to_string()),
        };
        let mut name = base.clone();
        if !used_names.insert(name.clone()) {
            name = format!("{base}_{cid}");
            used_names.insert(name.clone());
        }
        let mut modules = mods.clone();
        modules.sort_unstable();
        chunks.push(ChunkPlan {
            id: cid,
            kind,
            name,
            modules,
        });
    }
    chunks.sort_by_key(|c| c.id);

    // 6. chunk 间静态依赖边。
    let mut chunk_deps: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
    for (&m, e) in edges {
        let Some(&cm) = module_chunk.get(&m) else {
            continue;
        };
        for &t in &e.static_targets {
            if let Some(&ct) = module_chunk.get(&t)
                && ct != cm
                && ct != 0
            {
                chunk_deps.entry(cm).or_default().push(ct);
            }
        }
    }
    for v in chunk_deps.values_mut() {
        v.sort_unstable();
        v.dedup();
    }

    Some(ChunkGraph {
        module_chunk,
        chunk_deps,
        chunks,
    })
}

fn stem_of(edges: &FxHashMap<u32, ModuleEdges>, id: u32, fallback: &str) -> String {
    edges
        .get(&id)
        .map(|e| e.stem.clone())
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(stat: &[u32], dyn_: &[u32], stem: &str) -> ModuleEdges {
        ModuleEdges {
            static_targets: stat.to_vec(),
            dyn_targets: dyn_.to_vec(),
            stem: stem.to_string(),
        }
    }

    fn graph(items: Vec<(u32, ModuleEdges)>) -> FxHashMap<u32, ModuleEdges> {
        items.into_iter().collect()
    }

    #[test]
    fn no_dynamic_import_returns_none() {
        // entry(0) 静态依赖 1；无动态 import → 单包。
        let g = graph(vec![
            (0, edge(&[1], &[], "index")),
            (1, edge(&[], &[], "util")),
        ]);
        assert!(compute_chunk_graph(&g, 0, 2).is_none());
    }

    #[test]
    fn dynamic_import_creates_async_chunk() {
        // entry(0) 动态 import 1（1 无依赖）。
        let g = graph(vec![
            (0, edge(&[], &[1], "index")),
            (1, edge(&[], &[], "lazy")),
        ]);
        let cg = compute_chunk_graph(&g, 0, 2).expect("应产生 async chunk");
        assert_eq!(cg.chunks.len(), 2);
        assert_eq!(cg.module_chunk[&0], 0, "entry 模块在 chunk 0");
        assert_ne!(cg.module_chunk[&1], 0, "lazy 模块不在 entry chunk");
        assert_eq!(cg.chunks[0].kind, ChunkKind::Initial);
        assert_eq!(cg.chunks[1].kind, ChunkKind::Async);
        assert_eq!(cg.chunks[1].name, "lazy");
    }

    #[test]
    fn shared_module_extracted_to_shared_chunk() {
        // entry 动态 import a(1) 与 b(2)；a、b 都静态依赖 shared(3)。
        let g = graph(vec![
            (0, edge(&[], &[1, 2], "index")),
            (1, edge(&[3], &[], "a")),
            (2, edge(&[3], &[], "b")),
            (3, edge(&[], &[], "shared")),
        ]);
        let cg = compute_chunk_graph(&g, 0, 2).expect("应产生 chunk 图");
        assert_eq!(cg.chunks.len(), 4, "entry + a + b + shared");
        let kinds: Vec<ChunkKind> = cg.chunks.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds.iter().filter(|k| **k == ChunkKind::Initial).count(),
            1
        );
        assert_eq!(kinds.iter().filter(|k| **k == ChunkKind::Async).count(), 2);
        assert_eq!(kinds.iter().filter(|k| **k == ChunkKind::Shared).count(), 1);
        // shared 模块单独成 chunk，两 async chunk 都不含它。
        let shared_cid = cg.module_chunk[&3];
        assert_ne!(shared_cid, cg.module_chunk[&1]);
        assert_ne!(shared_cid, cg.module_chunk[&2]);
        // 两 async chunk 都依赖 shared chunk（须先加载）。
        assert!(cg.chunk_deps[&cg.module_chunk[&1]].contains(&shared_cid));
        assert!(cg.chunk_deps[&cg.module_chunk[&2]].contains(&shared_cid));
    }

    #[test]
    fn dynamic_target_in_entry_closure_stays_in_entry() {
        // entry 既静态依赖 1，又动态 import 1 → 1 已在 entry 闭包 → 不分离。
        let g = graph(vec![
            (0, edge(&[1], &[1], "index")),
            (1, edge(&[], &[], "shared")),
        ]);
        // 只有 entry 一个桶 → None（退回单包）。
        assert!(compute_chunk_graph(&g, 0, 2).is_none());
    }

    #[test]
    fn deterministic_across_runs() {
        let g1 = graph(vec![
            (0, edge(&[], &[1, 2], "index")),
            (1, edge(&[3], &[], "a")),
            (2, edge(&[3], &[], "b")),
            (3, edge(&[], &[], "shared")),
        ]);
        let g2 = graph(vec![
            (0, edge(&[], &[1, 2], "index")),
            (1, edge(&[3], &[], "a")),
            (2, edge(&[3], &[], "b")),
            (3, edge(&[], &[], "shared")),
        ]);
        let a = compute_chunk_graph(&g1, 0, 2).unwrap();
        let b = compute_chunk_graph(&g2, 0, 2).unwrap();
        assert_eq!(a.module_chunk, b.module_chunk);
        let an: Vec<_> = a.chunks.iter().map(|c| (c.id, c.name.clone())).collect();
        let bn: Vec<_> = b.chunks.iter().map(|c| (c.id, c.name.clone())).collect();
        assert_eq!(an, bn);
    }

    #[test]
    fn high_threshold_folds_shared_into_async() {
        // share_threshold 很高 → shared 模块不抽取，归到第一个 async owner。
        let g = graph(vec![
            (0, edge(&[], &[1, 2], "index")),
            (1, edge(&[3], &[], "a")),
            (2, edge(&[3], &[], "b")),
            (3, edge(&[], &[], "shared")),
        ]);
        let cg = compute_chunk_graph(&g, 0, 99).unwrap();
        // 无 shared chunk：3 与 owners[0]=1 同 chunk。
        assert!(cg.chunks.iter().all(|c| c.kind != ChunkKind::Shared));
        assert_eq!(cg.module_chunk[&3], cg.module_chunk[&1]);
    }
}
