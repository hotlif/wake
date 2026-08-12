//! 工作窃取执行器并行扇出 demo（PLAN §2.5.5）；实际加速比必须连同机器与负载测量。
//!
//! 运行：`cargo run --release -p wake_turbo --example work_stealing`
//!
//! 用一批 CPU 密集的独立任务对比「串行基线」与「工作窃取池」的墙钟时间，打印加速比。
//! 加速比对机器核数/负载敏感，故作为 demo 观察（而非硬 assert 的单测，避免 CI flaky）。

use std::time::Instant;

use wake_turbo::Executor;

/// 一个故意偏重的纯 CPU 任务（混合哈希，防止被优化掉）。
fn heavy(seed: u64) -> u64 {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for _ in 0..2_000_000 {
        x ^= x >> 12;
        x = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        x ^= x << 25;
        x ^= x >> 27;
    }
    x
}

fn main() {
    let n_jobs = 256;

    // 串行基线。
    let t0 = Instant::now();
    let seq: u64 = (0..n_jobs as u64).map(heavy).fold(0, u64::wrapping_add);
    let seq_time = t0.elapsed();

    // 工作窃取池。
    let exec = Executor::with_default_threads();
    let threads = exec.num_threads();
    let t1 = Instant::now();
    let results = exec.parallel((0..n_jobs as u64).map(|s| move || heavy(s)).collect());
    let par_time = t1.elapsed();
    let par: u64 = results.into_iter().fold(0, u64::wrapping_add);

    assert_eq!(seq, par, "并行结果须与串行一致");

    let speedup = seq_time.as_secs_f64() / par_time.as_secs_f64();
    println!("worker 线程数    : {threads}");
    println!("任务数           : {n_jobs}");
    println!("串行基线         : {seq_time:?}");
    println!("工作窃取池        : {par_time:?}");
    println!("加速比           : {speedup:.2}x  (理想上限 ≈ {threads}x)");
    println!(
        "并行效率         : {:.0}%",
        speedup / threads as f64 * 100.0
    );
}
