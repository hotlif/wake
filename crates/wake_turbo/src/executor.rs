//! # 工作窃取执行器（PLAN §2.5.5 / DESIGN §10.5）
//!
//! 自研工作窃取线程池，是 wake_turbo 榨干 CPU 的调度层。不用 rayon 是因为后续要定制：
//! 任务优先级、generation 取消、依赖阻塞时的续体处理（§10.5）——这些在 rayon 上无法干净实现。
//!
//! **本版范围**：干净可复用的工作窃取池 + 批量 `parallel` API，用于并行执行 **独立** 任务
//! （典型是 Scan 阶段各模块 parse 的扇出，DESIGN §10.6 预算表里的大头）。
//!
//! - 每 worker 一个 [`Worker`] 本地 **LIFO** 队列（新派生任务优先本核执行，吃热缓存）；
//! - 跨核 **FIFO** 窃取（[`Stealer`]，保证扇出广度）；
//! - 全局 [`Injector`] 承接外部提交，worker 批量搬运到本地以降争用；
//! - 空闲 worker 经 condvar 休眠（不忙等偷电），提交时唤醒；带 1ms 兜底轮询防丢唤醒。
//!
//! **尚未做**（下一轮，与引擎并发整合一起）：优先级车道、generation 取消、依赖阻塞续体、
//! 绑核、指数退避 park 的精细化（当前 condvar+超时够用且正确）。`parallel` 要求任务 `'static`
//! （借用栈数据的 `scope` API 待后续）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};

/// 类型擦除的任务体。
type Job = Box<dyn FnOnce() + Send + 'static>;

/// 线程池共享状态（worker 与提交方共同持有）。
struct Shared {
    injector: Injector<Job>,
    stealers: Vec<Stealer<Job>>,
    shutdown: AtomicBool,
    /// 空闲 worker 的休眠协调。
    idle_lock: Mutex<()>,
    idle_cv: Condvar,
}

impl Shared {
    /// 找一个可执行任务：本地 LIFO → 全局 injector 批量搬运 → 跨核 FIFO 窃取。
    fn find(&self, local: &Worker<Job>) -> Option<Job> {
        if let Some(job) = local.pop() {
            return Some(job);
        }
        loop {
            match self.injector.steal_batch_and_pop(local) {
                Steal::Success(job) => return Some(job),
                Steal::Empty => break,
                Steal::Retry => continue,
            }
        }
        for stealer in &self.stealers {
            loop {
                match stealer.steal() {
                    Steal::Success(job) => return Some(job),
                    Steal::Empty => break,
                    Steal::Retry => continue,
                }
            }
        }
        None
    }

    /// 唤醒所有空闲 worker（提交任务后调用）。
    fn wake_all(&self) {
        let _guard = self.idle_lock.lock().unwrap();
        self.idle_cv.notify_all();
    }
}

/// 单个 worker 线程主循环。
fn worker_loop(shared: Arc<Shared>, local: Worker<Job>) {
    loop {
        if let Some(job) = shared.find(&local) {
            job();
            continue;
        }
        if shared.shutdown.load(Ordering::Acquire) {
            break;
        }
        // 无活可干：休眠等待唤醒。带 1ms 超时兜底，避免任何丢唤醒导致挂死。
        let guard = shared.idle_lock.lock().unwrap();
        // 上锁后复检：提交方可能在我们上锁前刚 push 并 notify。
        if !shared.injector.is_empty() || shared.shutdown.load(Ordering::Acquire) {
            continue;
        }
        let _ = shared
            .idle_cv
            .wait_timeout(guard, Duration::from_millis(1))
            .unwrap();
    }
}

/// 自研工作窃取线程池。`Drop` 时优雅关闭并 join 全部 worker。
pub struct Executor {
    shared: Arc<Shared>,
    handles: Vec<JoinHandle<()>>,
}

/// Process-wide execution capacity used by production bundlers. A Wake process may host multiple
/// build contexts; sharing this pool keeps total worker ownership bounded by machine parallelism
/// instead of multiplying it by the number of contexts.
pub fn global_executor() -> Arc<Executor> {
    static GLOBAL: OnceLock<Arc<Executor>> = OnceLock::new();
    Arc::clone(GLOBAL.get_or_init(|| Arc::new(Executor::with_default_threads())))
}

impl Executor {
    /// 起 `num_threads` 个 worker（至少 1）。
    pub fn new(num_threads: usize) -> Executor {
        let num_threads = num_threads.max(1);
        let workers: Vec<Worker<Job>> = (0..num_threads).map(|_| Worker::new_lifo()).collect();
        let stealers = workers.iter().map(|w| w.stealer()).collect();
        let shared = Arc::new(Shared {
            injector: Injector::new(),
            stealers,
            shutdown: AtomicBool::new(false),
            idle_lock: Mutex::new(()),
            idle_cv: Condvar::new(),
        });
        let mut handles = Vec::with_capacity(num_threads);
        for (idx, local) in workers.into_iter().enumerate() {
            let shared = shared.clone();
            let handle = thread::Builder::new()
                .name(format!("wake-worker-{idx}"))
                .spawn(move || worker_loop(shared, local))
                .expect("无法创建 worker 线程");
            handles.push(handle);
        }
        Executor { shared, handles }
    }

    /// 用机器可用并行度（`available_parallelism`）创建，回退 4。
    pub fn with_default_threads() -> Executor {
        let n = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Executor::new(n)
    }

    /// worker 线程数。
    pub fn num_threads(&self) -> usize {
        self.handles.len()
    }

    /// 并行执行一批 **独立** 任务，按输入顺序返回结果。提交后阻塞直至全部完成
    /// （主线程经 channel 休眠等待，不占核，保证与串行基线对比公平）。
    ///
    /// 任务须 `'static`（不借用调用栈）。空批直接返回空。
    pub fn parallel<T, F>(&self, jobs: Vec<F>) -> Vec<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let n = jobs.len();
        if n == 0 {
            return Vec::new();
        }
        let (tx, rx) = mpsc::channel::<(usize, T)>();
        for (i, job) in jobs.into_iter().enumerate() {
            let tx = tx.clone();
            self.shared.injector.push(Box::new(move || {
                let result = job();
                // 接收端存活期间发送必成功；失败仅当提交方已放弃（进程退出），忽略。
                let _ = tx.send((i, result));
            }));
        }
        drop(tx);
        self.shared.wake_all();

        // 主线程休眠收集 n 个结果，按索引归位。
        let mut out: Vec<Option<T>> = (0..n).map(|_| None).collect();
        for _ in 0..n {
            let (i, result) = rx.recv().expect("worker 在返回结果前异常退出");
            out[i] = Some(result);
        }
        out.into_iter()
            .map(|slot| slot.expect("每个索引都应收到结果"))
            .collect()
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.wake_all();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn global_executor_has_one_process_owner() {
        let first = global_executor();
        let second = global_executor();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.num_threads() >= 1);
    }

    #[test]
    fn global_executor_supports_concurrent_submitters() {
        let submitters = (0..4)
            .map(|owner| {
                let executor = global_executor();
                std::thread::spawn(move || {
                    let jobs = (0..128)
                        .map(|value| move || owner * 1_000 + value)
                        .collect::<Vec<_>>();
                    executor.parallel(jobs)
                })
            })
            .collect::<Vec<_>>();
        for (owner, submitter) in submitters.into_iter().enumerate() {
            let output = submitter.join().expect("submitter");
            assert_eq!(output[0], owner * 1_000);
            assert_eq!(output[127], owner * 1_000 + 127);
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "crossbeam 无锁结构在 miri 慢且有已知假阳性；见模块文档"
    )]
    fn parallel_preserves_order_and_runs_all() {
        let exec = Executor::new(4);
        // miri 下线程解释慢，缩小规模。
        let n = if cfg!(miri) { 40 } else { 1000 };
        let jobs: Vec<_> = (0..n).map(|i| move || i as u64 * i as u64).collect();
        let results = exec.parallel(jobs);
        assert_eq!(results.len(), n);
        for (i, &r) in results.iter().enumerate() {
            assert_eq!(r, i as u64 * i as u64, "结果错位或缺失");
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "crossbeam 无锁结构在 miri 慢且有已知假阳性；见模块文档"
    )]
    fn every_job_executes_exactly_once() {
        let exec = Executor::new(4);
        let n = if cfg!(miri) { 40 } else { 500 };
        let counter = Arc::new(AtomicUsize::new(0));
        let jobs: Vec<_> = (0..n)
            .map(|_| {
                let counter = counter.clone();
                move || {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            })
            .collect();
        let out = exec.parallel(jobs);
        assert_eq!(out.len(), n);
        assert_eq!(counter.load(Ordering::Relaxed), n, "任务执行次数应恰为 n");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "crossbeam 无锁结构在 miri 慢且有已知假阳性；见模块文档"
    )]
    fn reused_across_batches() {
        let exec = Executor::new(3);
        for round in 0..5u64 {
            let jobs: Vec<_> = (0..50u64).map(|i| move || i + round).collect();
            let results = exec.parallel(jobs);
            let expect: u64 = (0..50u64).map(|i| i + round).sum();
            assert_eq!(results.iter().sum::<u64>(), expect);
        }
    }
}
