//! 引擎调度开销微基准（PLAN §2.5.7 / Gate-2：<2µs/任务）。
//!
//! 「每任务开销」= slot 查找 + 依赖记录 + 指纹比较等引擎自身成本（不含任务体真实工作）。
//! 玩具任务体极轻（平方），故墙钟 / 任务数 ≈ 纯引擎开销。两组：
//! - **shallow_green**：无变更重复请求 N 个已记忆任务，走浅绿快路径（记忆化命中的稳态开销）；
//! - **incremental_verify**：改一个输入后请求 N 个任务，走深校验 + 早期截断（增量重算开销）。

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use wake_turbo::{Engine, Vc, task};

#[task]
fn sq(x: Vc<i64>) -> i64 {
    let v = *x.read();
    v * v
}

const N: usize = 1000;

fn bench_engine(c: &mut Criterion) {
    // shallow_green：稳态记忆化命中开销。
    {
        let engine = Engine::new();
        let xs: Vec<Vc<i64>> = (0..N).map(|i| engine.new_input(i as i64)).collect();
        engine.enter(|| {
            for &x in &xs {
                let _ = sq(x).read();
            }
        }); // 预热建满缓存

        let mut group = c.benchmark_group("shallow_green");
        group.throughput(criterion::Throughput::Elements(N as u64));
        group.bench_function("request_1000_memoized", |b| {
            b.iter(|| {
                engine.enter(|| {
                    for &x in &xs {
                        black_box(*sq(x).read());
                    }
                })
            })
        });
        group.finish();
    }

    // incremental_verify：改一个输入后请求全部，走深校验 + 早期截断。
    {
        let engine = Engine::new();
        let xs: Vec<Vc<i64>> = (0..N).map(|i| engine.new_input(i as i64)).collect();
        engine.enter(|| {
            for &x in &xs {
                let _ = sq(x).read();
            }
        });

        let mut group = c.benchmark_group("incremental_verify");
        group.throughput(criterion::Throughput::Elements(N as u64));
        let mut v = 0i64;
        group.bench_function("change_1_request_1000", |b| {
            b.iter(|| {
                v += 1;
                engine.set_input(xs[0], v); // 每次改一个输入 → 新 revision
                engine.enter(|| {
                    for &x in &xs {
                        black_box(*sq(x).read());
                    }
                })
            })
        });
        group.finish();
    }
}

criterion_group!(benches, bench_engine);
criterion_main!(benches);
