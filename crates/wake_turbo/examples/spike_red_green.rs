//! Spike ② 可运行 demo（PLAN §0.6）。
//!
//! 运行：`cargo run -p wake_turbo --example spike_red_green`
//!
//! 演示：① 随机变更 + 随机请求下与全量重算对拍一致；② 早期截断显著减少查询体执行次数。

use wake_turbo::spike::{Engine, Key, Rng, all_derived_keys, reference};

fn main() {
    println!("== Spike ② 红绿失效 + 早期截断 demo ==\n");

    // —— 演示 1：对拍一致性 ——
    let n = 10;
    let mut rng = Rng::new(0x1234_5678_9ABC_DEF0);
    let mut inputs: Vec<i64> = (0..n).map(|_| rng.below(200) as i64).collect();
    let engine = Engine::new(inputs.clone());
    let keys = all_derived_keys(n);

    let mut checks = 0u64;
    for _ in 0..2000 {
        if rng.next_u64() & 1 == 0 {
            let i = rng.below(n);
            let v = rng.below(200) as i64;
            inputs[i] = v;
            engine.set_input(i, v);
        }
        let key = keys[rng.below(keys.len())];
        assert_eq!(engine.request(key), reference(&inputs, key));
        checks += 1;
    }
    println!("对拍一致性：{checks} 次随机请求全部与全量重算一致 ✓");

    // —— 演示 2：早期截断收益 ——
    let big_n = 100;
    let engine = Engine::new(vec![0; big_n]); // 全部 < 阈值
    engine.request(Key::Report);
    let baseline = engine.exec_count();

    let rounds = 30;
    for r in 0..rounds {
        // 在阈值同侧小幅改一个输入（sign 不变）→ CountBig 应被早期截断。
        engine.set_input(r % big_n, (r as i64 % 90) + 1);
        engine.request(Key::Report);
    }
    let incremental = engine.exec_count() - baseline;
    let full_recompute_estimate = rounds as u64 * (big_n as u64 + 3);

    println!(
        "早期截断：{rounds} 轮增量共执行查询体 {incremental} 次；\
         全量重算需约 {full_recompute_estimate} 次 → 省去 {:.1}%",
        100.0 * (1.0 - incremental as f64 / full_recompute_estimate as f64)
    );
    println!("  （CountBig 因所有 Sign 未变而被截断，从不重算）");

    println!(
        "\n结论：单线程红绿 + 早期截断算法正确且截断有效。详见 docs/spikes/spike-02-red-green.md"
    );
}
