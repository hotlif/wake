//! loom 并发模型检查：single-flight 协议（PLAN §2.5.6）。
//!
//! 仅在 `--cfg loom` 下编译运行：
//! ```bash
//! RUSTFLAGS="--cfg loom" cargo test -p wake_turbo --test loom_single_flight
//! ```
//!
//! loom 穷举所有线程交错，验证与 [`Engine::ensure_green`] 同构的 single-flight 核心协议
//! （fast-path → per-task 执行锁 → double-check → compute）在 **任何交错** 下：
//! ① 任务体只执行一次；② 所有线程看到同一结果。
//!
//! 这里刻意抽出不依赖分片表/thread-local/executor 的最小模型——loom 无法 instrument 那些
//! （状态爆炸 + 裸指针），而 single-flight 的正确性只取决于「执行锁 + 双检」这几步。
//! Engine 的生产实现（`engine.rs`）逐字复刻此协议，端到端语义另由并发对拍压测覆盖。

#![cfg(loom)]

use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};

/// single-flight 最小模型：镜像 `ensure_green` 的 fast-path → exec_lock → double-check → compute。
struct SingleFlight {
    /// per-task 执行锁：序列化对同一 task 的重算。
    exec_lock: Mutex<()>,
    /// 记忆结果（模拟 memo）。
    memo: Mutex<Option<u64>>,
    /// 任务体执行次数（single-flight 的观察点）。
    exec_count: AtomicUsize,
}

impl SingleFlight {
    fn new() -> SingleFlight {
        SingleFlight {
            exec_lock: Mutex::new(()),
            memo: Mutex::new(None),
            exec_count: AtomicUsize::new(0),
        }
    }

    /// 获取结果：命中直接返回；否则持执行锁、双检、执行一次并记忆。
    fn get(&self, compute: impl Fn() -> u64) -> u64 {
        // fast path：无锁（此处仍走 memo 锁，但不持执行锁）浅命中。
        if let Some(v) = *self.memo.lock().unwrap() {
            return v;
        }
        // single-flight：持 per-task 执行锁。
        let _guard = self.exec_lock.lock().unwrap();
        // double-check：别的线程可能刚算完。
        if let Some(v) = *self.memo.lock().unwrap() {
            return v;
        }
        // 执行一次并记忆。
        self.exec_count.fetch_add(1, Ordering::Relaxed);
        let v = compute();
        *self.memo.lock().unwrap() = Some(v);
        v
    }
}

#[test]
fn single_flight_executes_once_under_all_interleavings() {
    loom::model(|| {
        let sf = Arc::new(SingleFlight::new());

        // 两个线程并发请求同一 task（loom 用 2 线程避免状态爆炸；协议对称，2 足以暴露双检竞争）。
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let sf = Arc::clone(&sf);
                loom::thread::spawn(move || sf.get(|| 42))
            })
            .collect();

        let mut results = Vec::new();
        for h in handles {
            results.push(h.join().unwrap());
        }

        // ① 所有线程看到同一结果。
        assert!(results.iter().all(|&v| v == 42), "single-flight 结果不一致");
        // ② 任何交错下任务体只执行一次。
        assert_eq!(
            sf.exec_count.load(Ordering::Relaxed),
            1,
            "single-flight 失效：某交错下任务被执行多次"
        );
    });
}
