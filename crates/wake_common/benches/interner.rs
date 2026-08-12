//! Atom interner 吞吐微基准（criterion）。
//!
//! 运行：`cargo bench -p wake_common`
//! 当前 CI 只执行 benchmark 编译冒烟；历史回归基线建立后再接入自动阈值。
//!
//! 单线程 `intern_1000_fresh`/`_hit` 之外，`intern_contended_*` 用多线程度量分片锁竞争——
//! 这是评估 tier-3 interner 改动（`Mutex`→`RwLock` 读快路径 / miss 路径单分配 + 单哈希）的
//! **关键基线**：现有单线程 hit 基准测不出并行 Scan 阶段的锁争用。

use std::hint::black_box;
use std::sync::Arc;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use wake_common::Interner;

fn bench_intern_fresh(c: &mut Criterion) {
    // 预生成一批标识符，度量驻留（含首次插入）吞吐。
    let idents: Vec<String> = (0..1000).map(|i| format!("identifier_{i}")).collect();
    c.bench_function("intern_1000_fresh", |b| {
        b.iter_batched(
            Interner::new,
            |it| {
                for s in &idents {
                    black_box(it.intern(s));
                }
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_intern_hit(c: &mut Criterion) {
    // 全部已驻留后的重复查询（命中路径）吞吐。
    let it = Interner::new();
    let idents: Vec<String> = (0..1000).map(|i| format!("identifier_{i}")).collect();
    for s in &idents {
        it.intern(s);
    }
    c.bench_function("intern_1000_hit", |b| {
        b.iter(|| {
            for s in &idents {
                black_box(it.intern(s));
            }
        });
    });
}

/// 多线程**命中**竞争：N 个线程在共享 interner 上重复驻留同一批已存在的 key。
/// 读命中在当前 `Mutex<Shard>` 下会串行化 → 衡量 `RwLock` 读快路径能挽回多少并行度。
fn bench_intern_contended_hits(c: &mut Criterion) {
    const KEYS: usize = 2000;
    const ROUNDS: usize = 10;
    let keys: Vec<String> = (0..KEYS).map(|i| format!("identifier_{i}")).collect();

    let mut group = c.benchmark_group("intern_contended_hits");
    for &threads in &[1usize, 2, 4, 8] {
        group.throughput(Throughput::Elements((threads * KEYS * ROUNDS) as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                b.iter_batched(
                    || {
                        // 每批新建并预热：所有 key 已驻留，线程走读命中路径。
                        let it = Arc::new(Interner::new());
                        for k in &keys {
                            it.intern(k);
                        }
                        it
                    },
                    |it| {
                        let keys: &[String] = &keys;
                        std::thread::scope(|s| {
                            for _ in 0..threads {
                                let it = Arc::clone(&it);
                                s.spawn(move || {
                                    for _ in 0..ROUNDS {
                                        for k in keys {
                                            black_box(it.intern(k));
                                        }
                                    }
                                });
                            }
                        });
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

/// 多线程**未命中**竞争：每线程驻留各自不相交的新 key（miss 路径）。
/// 衡量分片锁持锁期间的 `Box<str>` 双分配 + 全串哈希 → 评估「单分配 / raw_entry 单哈希」改动。
fn bench_intern_contended_fresh(c: &mut Criterion) {
    const PER_THREAD: usize = 2000;
    let mut group = c.benchmark_group("intern_contended_fresh");
    for &threads in &[1usize, 2, 4, 8] {
        // 预生成每线程不相交的 key 集，避免在计时区里 format!。
        let keysets: Vec<Vec<String>> = (0..threads)
            .map(|t| (0..PER_THREAD).map(|i| format!("t{t}_ident_{i}")).collect())
            .collect();
        group.throughput(Throughput::Elements((threads * PER_THREAD) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(threads), &threads, |b, _| {
            b.iter_batched(
                || Arc::new(Interner::new()),
                |it| {
                    // keysets 恰有 `threads` 个不相交子集，逐个交给一个线程。
                    let keysets: &[Vec<String>] = &keysets;
                    std::thread::scope(|s| {
                        for keyset in keysets {
                            let it = Arc::clone(&it);
                            s.spawn(move || {
                                for k in keyset {
                                    black_box(it.intern(k));
                                }
                            });
                        }
                    });
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_intern_fresh,
    bench_intern_hit,
    bench_intern_contended_hits,
    bench_intern_contended_fresh
);
criterion_main!(benches);
