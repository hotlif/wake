//! Gate-2 收尾项测试：循环依赖检测 + 「无增量纯并行」降级开关。

use wake_turbo::{Engine, Vc, task};

// —— 循环依赖：ping ↔ pong 互相请求同参（同 TaskId）构成环 ——

#[task]
fn ping(x: Vc<i64>) -> i64 {
    *pong(x).read() + 1
}

#[task]
fn pong(x: Vc<i64>) -> i64 {
    *ping(x).read() + 1
}

#[test]
#[should_panic(expected = "循环依赖")]
fn detects_direct_cycle() {
    let engine = Engine::new();
    let x = engine.new_input(0i64);
    // ping(x) → pong(x) → ping(x)：同线程 DFS 递归回 ping(x)，环检测 panic。
    engine.enter(|| {
        let _ = ping(x).read();
    });
}

// —— 「无增量纯并行」降级开关：语义仍等价全量重算，但禁早期截断 ——

#[task]
fn sign(x: Vc<i64>) -> i64 {
    (*x.read() > 100) as i64
}

#[task]
fn plus_one(x: Vc<i64>) -> i64 {
    *sign(x).read() + 1
}

#[test]
fn pure_parallel_mode_is_correct_but_uncached() {
    let engine = Engine::new_pure_parallel();
    assert!(!engine.is_incremental());
    let x = engine.new_input(50i64);

    // 首次请求：sign + plus_one 各执行一次。
    assert_eq!(engine.enter(|| *plus_one(x).read()), 1); // sign(50)=0, +1=1
    let after_first = engine.exec_count();

    // 阈值同侧变动（50→60，仍 < 100）：增量模式下 sign 早期截断、plus_one 不重算；
    // 降级模式下 deep_verify 恒 false → 每次全量重算（sign + plus_one 都重跑）。
    engine.set_input(x, 60);
    assert_eq!(engine.enter(|| *plus_one(x).read()), 1); // 语义仍正确
    let delta = engine.exec_count() - after_first;
    assert_eq!(delta, 2, "降级模式应全量重算 sign + plus_one（无早期截断）");
}

#[test]
fn incremental_mode_early_cutoff_still_works() {
    // 对照组：默认增量模式下，阈值同侧变动应触发早期截断，plus_one 不重算。
    let engine = Engine::new();
    let x = engine.new_input(50i64);
    assert_eq!(engine.enter(|| *plus_one(x).read()), 1);
    let after_first = engine.exec_count();

    engine.set_input(x, 60); // 仍 < 100，sign 不变
    assert_eq!(engine.enter(|| *plus_one(x).read()), 1);
    let delta = engine.exec_count() - after_first;
    // 只重算 sign（值不变 → 早期截断），plus_one 被截断不重跑。
    assert_eq!(delta, 1, "增量模式应早期截断：只重算 sign，plus_one 不重跑");
}

#[test]
fn runtime_toggle_between_modes() {
    let engine = Engine::new();
    let x = engine.new_input(50i64);
    let _ = engine.enter(|| *plus_one(x).read());

    // 切到降级模式，行为随之改变（全量重算）。
    engine.set_incremental(false);
    assert!(!engine.is_incremental());
    engine.set_input(x, 70);
    let before = engine.exec_count();
    assert_eq!(engine.enter(|| *plus_one(x).read()), 1);
    assert_eq!(engine.exec_count() - before, 2, "降级后应全量重算");
}
