//! 并发红绿引擎端到端测试（PLAN §2.5.5 并发整合 / Gate-2）。
//!
//! 验证 single-flight（多线程请求同一 task 只执行一次）与并发下失效传播的正确性。
//! 图刻意设「共享子任务」：`combine(leaf(a), leaf(b))`，让多个顶层请求竞争同一批任务。
//!
//! 用真实工作窃取执行器扇出（`Executor`）→ 依赖 crossbeam，miri 下 crossbeam 有已知假阳性
//! 且极慢，故整文件在 miri 下忽略（single-flight 协议的并发正确性由 `loom_single_flight.rs`
//! 用 loom 独立验证，引擎红绿语义由 `red_green.rs` 单线程 + miri 覆盖）。

use std::sync::Arc;

use wake_turbo::spike::Rng;
use wake_turbo::{Engine, Executor, Vc, task};

/// 叶子任务：平方（被多个顶层请求共享，是 single-flight 的观察点）。
#[task]
fn leaf(x: Vc<i64>) -> i64 {
    let v = *x.read();
    v * v
}

/// 二元聚合（两个 `Vc` 参数，顺带验证宏的多参数支持）。
#[task]
fn combine(a: Vc<i64>, b: Vc<i64>) -> i64 {
    *a.read() + *b.read()
}

#[test]
#[cfg_attr(miri, ignore = "用 crossbeam 执行器，miri 慢且有已知假阳性；见文件头")]
fn single_flight_shared_subtasks() {
    let engine = Arc::new(Engine::new());
    let x0 = engine.new_input(1i64);
    let x1 = engine.new_input(2i64);
    let exec = Executor::new(8);

    // M 个并发请求，全部请求同一个 combine(leaf(x0), leaf(x1))（同参 → 同 TaskId）。
    let m = 100;
    let requests: Vec<_> = (0..m)
        .map(|_| move || *combine(leaf(x0), leaf(x1)).read())
        .collect();
    let results = engine.par_request(&exec, requests);

    // 结果全对：x0² + x1² = 1 + 4 = 5。
    assert!(results.iter().all(|&r| r == 5), "并发请求结果错误");
    // single-flight：无论 100 个并发请求 / 8 worker，只执行 leaf(x0)+leaf(x1)+combine = 3 次。
    assert_eq!(
        engine.exec_count(),
        3,
        "single-flight 失效：共享任务被重复执行"
    );
}

#[test]
#[cfg_attr(miri, ignore = "用 crossbeam 执行器，miri 慢且有已知假阳性；见文件头")]
fn concurrent_matches_reference() {
    let engine = Arc::new(Engine::new());
    let n = 8usize;
    let mut vals: Vec<i64> = (0..n as i64).map(|i| i + 1).collect();
    let x: Vec<Vc<i64>> = vals.iter().map(|&v| engine.new_input(v)).collect();
    let exec = Executor::new(8);
    let mut rng = Rng::new(0x1234_5678_9ABC_DEF0);

    // 多轮：单线程改若干输入（revision 串行 bump）→ 多线程并行请求 → 逐对对拍全量重算。
    for _round in 0..50 {
        let changes = rng.below(n) + 1;
        for _ in 0..changes {
            let i = rng.below(n);
            let v = rng.below(50) as i64;
            vals[i] = v;
            engine.set_input(x[i], v);
        }

        // 每对相邻输入组一个 combine(leaf(i), leaf((i+1)%n))，并行请求全部。
        let pairs: Vec<(usize, usize)> = (0..n).map(|i| (i, (i + 1) % n)).collect();
        let reqs: Vec<_> = pairs
            .iter()
            .map(|&(i, j)| {
                let xi = x[i];
                let xj = x[j];
                move || *combine(leaf(xi), leaf(xj)).read()
            })
            .collect();
        let got = engine.par_request(&exec, reqs);

        for (k, &(i, j)) in pairs.iter().enumerate() {
            let want = vals[i] * vals[i] + vals[j] * vals[j];
            assert_eq!(got[k], want, "并发对拍不一致：pair ({i},{j})");
        }
    }
}
