//! 合成项目打包基准（PLAN §3 Gate-3：1k 模块 <1.5s）。
//!
//! 运行：`cargo bench -p wake_bundler --bench bundle`
//!
//! 合成图：`m0` 为入口，`mi` 二叉扇出 `m(2i+1)/m(2i+2)`（完全二叉树，有宽度也有深度），
//! 且每个模块都 import 公共 `util`（被全体模块引用 → 考验 single-flight 去重）。
//! 基准组：**bundle_1k**（1000 模块）与 **bundle_2k**（2000 模块），
//! 每组含 **cold**（新引擎全量构建）与 **incremental**（同实例、同内容第二遍，全缓存命中）。

use std::path::Path;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use wake_bundler::IncrementalBundler;
use wake_common::MemoryFileSystem;

/// 生成 N 模块合成项目：二叉树扇出 + 全体共享 `util`（= m{N-1}）。
fn gen_project(n: usize) -> MemoryFileSystem {
    let util = n - 1;
    let mut files: Vec<(String, String)> = Vec::with_capacity(n);
    for i in 0..n {
        let mut src = String::new();
        if i != util {
            src.push_str(&format!("import u from './m{util}.js';\n"));
        }
        let (l, r) = (2 * i + 1, 2 * i + 2);
        if l < util {
            src.push_str(&format!("import a from './m{l}.js';\n"));
        }
        if r < util {
            src.push_str(&format!("import b from './m{r}.js';\n"));
        }
        src.push_str(&format!("export default {i};\n"));
        files.push((format!("m{i}.js"), src));
    }
    MemoryFileSystem::from_files(files)
}

fn bench_bundle(c: &mut Criterion) {
    // ---- bundle_1k ----
    bench_n(c, "bundle_1k", 1000);

    // ---- bundle_2k ----
    bench_n(c, "bundle_2k", 2000);
}

fn bench_n(c: &mut Criterion, name: &str, n: usize) {
    // cold：每次新引擎全量构建（空缓存）。
    {
        let fs = Arc::new(gen_project(n));
        let mut group = c.benchmark_group(name);
        group.sample_size(15);
        group.bench_function("cold", |b| {
            b.iter(|| {
                let mut bundler = IncrementalBundler::new(fs.clone());
                let out = bundler.build(Path::new("m0.js"));
                assert!(!out.has_errors());
                assert_eq!(out.module_count, n);
            })
        });
        group.finish();
    }

    // incremental：同一 bundler 预热后，第二遍全缓存命中。
    {
        let fs = Arc::new(gen_project(n));
        let mut bundler = IncrementalBundler::new(fs);
        let _ = bundler.build(Path::new("m0.js")); // 预热
        let mut group = c.benchmark_group(name);
        group.sample_size(30);
        group.bench_function("incremental_cached", |b| {
            b.iter(|| {
                let out = bundler.build(Path::new("m0.js"));
                assert!(!out.has_errors());
            })
        });
        group.finish();
    }
}

criterion_group!(benches, bench_bundle);
criterion_main!(benches);
