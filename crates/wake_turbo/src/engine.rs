//! # wake_turbo 引擎核心（PLAN §2.5.2–§2.5.3 + §2.5.5 并发整合）
//!
//! 把 [`spike`](crate::spike) 已对拍验证的红绿算法泛化成 **并发** 任务图：
//!
//! - **算法同构 spike**：`revision` / `Memo{value,fingerprint,verified_at,changed_at,deps}` /
//!   浅校验 → 深校验 → 重算+早期截断，逐条对应 spike 的 `verify_value`。
//! - **值类型擦除**：slot 存 `Arc<dyn Any + Send + Sync>` + [`Hash64`] 指纹（早期截断的比较量）。
//! - **深校验递归**改为「调注册表 `recomputers[TaskId]` 的闭包」，故依赖只需 `TaskId` 即可重算。
//!
//! ## 并发模型（DESIGN §10.3「revision 串行写、读并行」）
//!
//! - **自研分片 slot 表** [`Sharded`]：按 `TaskId` 分 128 片，每片一把 cache-padded
//!   `Mutex<FxHashMap>`。
//!   所有分片操作 **lock → 取值 → 立即释放**，绝不持分片锁递归（否则同片重入死锁）。
//! - **single-flight** = per-task 执行锁（`Arc<Mutex<()>>`）：多线程请求同一 task 时序列化，
//!   只执行一次；深校验递归时「持父锁 → 拿子锁」，**无环依赖下遵循 DAG 偏序，不死锁**。
//! - **同一 revision 内多查询并行**：`set_input`（串行 bump revision）与 query 不并发——
//!   dev 实际就是「一批变更 → 重算」分阶段，故校验期间 revision 固定，正确性大幅简化。
//! - **顶层扇出** [`Engine::par_request`]：各请求在 worker 线程 `enter` 执行其子树，
//!   共享子任务靠 single-flight 去重。
//!
//! 依赖记录纪律（同 spike）：只记直接读（[`record_dependency`]）；深校验路径不登记调用者依赖。
//! 任务体内的子任务调用与 `Vc::read` 经 thread-local「当前引擎」定位，须在 [`Engine::enter`] 内。
//!
//! 同线程任务栈上的循环依赖会被检测并拒绝；single-flight 协议的并发正确性由 loom 独立验证
//! （`tests/loom_single_flight.rs`），端到端语义由并发对拍压测覆盖。引擎本体不提供抢占任意
//! 正在执行任务的通用 generation 取消，产品层在构建安全点协作取消。

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crossbeam_utils::CachePadded;
use wake_common::{FxHashMap, Hash64};

#[cfg(not(loom))]
use crate::executor::Executor;
use crate::vc::{RawVc, Revision, TaskId, TaskOutput, Vc};

/// 类型擦除的任务输出值。
type AnyValue = Arc<dyn Any + Send + Sync>;

/// 重算器：无参闭包，产出 `(擦除值, 指纹)`。深校验递归到某依赖需重算时调用它。
type Recompute = Arc<dyn Fn() -> (AnyValue, Hash64) + Send + Sync>;

/// 分片数（2 的幂，便于按位取模）。
const SHARD_COUNT: usize = 128;
type Shard<V> = CachePadded<Mutex<FxHashMap<TaskId, V>>>;

/// 自研分片并发表：按 `TaskId` 分片，每片独立 `Mutex`。热路径无全局锁。
///
/// **纪律**：每个方法内 lock → 操作 → 返回前释放，绝不在持分片锁时递归到本表
/// （同片重入 = 死锁）。跨 task 的递归串行化交给 per-task 执行锁（见 [`Engine`]）。
struct Sharded<V> {
    shards: Box<[Shard<V>]>,
}

impl<V> Sharded<V> {
    fn new() -> Sharded<V> {
        let shards = (0..SHARD_COUNT)
            .map(|_| CachePadded::new(Mutex::new(FxHashMap::default())))
            .collect();
        Sharded { shards }
    }

    fn shard(&self, id: TaskId) -> &Mutex<FxHashMap<TaskId, V>> {
        // TaskId.0 是 xxh3 输出，分布良好，取低位即可。
        &self.shards[(id.0 as usize) & (SHARD_COUNT - 1)]
    }

    fn get_cloned(&self, id: TaskId) -> Option<V>
    where
        V: Clone,
    {
        self.shard(id).lock().unwrap().get(&id).cloned()
    }

    fn insert(&self, id: TaskId, value: V) {
        self.shard(id).lock().unwrap().insert(id, value);
    }

    /// 缺失才插入（幂等注册）。
    fn ensure_with(&self, id: TaskId, f: impl FnOnce() -> V) {
        self.shard(id).lock().unwrap().entry(id).or_insert_with(f);
    }

    /// 缺失则插入并返回其克隆（用于 per-task 执行锁的获取）。
    fn get_or_insert_with(&self, id: TaskId, f: impl FnOnce() -> V) -> V
    where
        V: Clone,
    {
        self.shard(id)
            .lock()
            .unwrap()
            .entry(id)
            .or_insert_with(f)
            .clone()
    }

    /// 只读访问某字段（不克隆整个值）。闭包内 **不得** 递归到本表。
    fn with<R>(&self, id: TaskId, f: impl FnOnce(Option<&V>) -> R) -> R {
        let guard = self.shard(id).lock().unwrap();
        f(guard.get(&id))
    }

    /// 就地更新（存在才改）。闭包内 **不得** 递归到本表。
    fn update(&self, id: TaskId, f: impl FnOnce(&mut V)) {
        if let Some(v) = self.shard(id).lock().unwrap().get_mut(&id) {
            f(v);
        }
    }

    /// 把每个分片当前拥有的 map 移出，原分片留作空表。
    ///
    /// 只允许在调用方独占整个 [`Engine`] 时使用；这样既没有并发访问，也没有仍持有
    /// per-task 锁的 single-flight。即使此前某个查询 panic 导致分片锁中毒，清理仍应取得
    /// map 的所有权并完成析构，而不是把整棵瞬态任务图留给主线程串行 drop。
    fn take_maps(&mut self) -> Vec<FxHashMap<TaskId, V>> {
        self.shards
            .iter_mut()
            .map(|shard| {
                let mut map = shard
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                std::mem::take(&mut *map)
            })
            .collect()
    }
}

/// 输入 cell：文件内容、配置切片……由引擎递增分配、外部显式写入。
struct InputCell {
    value: AnyValue,
    fingerprint: Hash64,
    /// 该输入的值上次真正变化于哪个 revision（输入级早期截断）。
    changed_at: Revision,
}

/// 派生任务的记忆条目。`changed_at` 是早期截断的关键量：重算后指纹未变则不推进它。
#[derive(Clone)]
struct Memo {
    value: AnyValue,
    fingerprint: Hash64,
    verified_at: Revision,
    changed_at: Revision,
    deps: Vec<RawVc>,
}

/// [`Engine::release_one_shot`] 已转移并析构的瞬态状态数量。
///
/// 计数用于阶段级性能诊断，也让调用方能验证预期的任务图已经真正释放。`drop_batches`
/// 是提交到 [`Executor`] 的非空析构批次数；它至多为引擎分片数。
#[cfg(not(loom))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OneShotReleaseStats {
    pub input_cells: usize,
    pub memo_entries: usize,
    pub recomputer_entries: usize,
    pub task_locks: usize,
    pub drop_batches: usize,
}

#[cfg(not(loom))]
struct OneShotDropBatch {
    inputs: Vec<InputCell>,
    memos: FxHashMap<TaskId, Memo>,
    recomputers: FxHashMap<TaskId, Recompute>,
    task_locks: FxHashMap<TaskId, Arc<Mutex<()>>>,
}

#[cfg(not(loom))]
impl OneShotDropBatch {
    fn is_empty(&self) -> bool {
        self.inputs.is_empty()
            && self.memos.is_empty()
            && self.recomputers.is_empty()
            && self.task_locks.is_empty()
    }
}

/// 并发红绿增量引擎。共享只读（`&self` / `Arc<Self>`），内部状态经原子/锁/分片表管理。
pub struct Engine {
    revision: AtomicU64,
    inputs: RwLock<Vec<InputCell>>,
    memos: Sharded<Memo>,
    /// 每个 `TaskId` 的重算闭包（首次 query 幂等注册）。
    recomputers: Sharded<Recompute>,
    /// per-task 执行锁：single-flight 的核心，序列化对同一 task 的校验/重算。
    task_locks: Sharded<Arc<Mutex<()>>>,
    /// 任务体真正执行的累计次数。
    exec_count: AtomicU64,
    /// 增量开关：false = 「无增量纯并行」降级模式（禁跨 revision 复用，每次变更全量重算，
    /// 但保留本 revision 内 single-flight 去重 + 并行）。DESIGN §6 的降级预案，Gate-2 验收项。
    incremental: AtomicBool,
    /// 单次构建模式：任务值仍通过 `Vc` 槽位跨阶段传递，但进程内不会发生 input 更新或第二个
    /// generation，因此跳过输出指纹、重算器和依赖边。per-task 锁仍保留，重复并发请求继续
    /// single-flight。该模式必须在创建引擎时确定；第一条派生 query 发布后输入即被冻结。
    one_shot: bool,
    /// One-shot 输入可在任务图启动前完成最后一次组装；第一条派生 query 发布此标志后，
    /// [`Engine::set_input`] 必须拒绝更新，否则已物化且没有依赖边的任务会静默陈旧。
    query_started: AtomicBool,
}

thread_local! {
    /// 当前线程绑定的引擎（`Engine::enter` 期间有效）。
    static CURRENT: Cell<*const Engine> = const { Cell::new(std::ptr::null()) };
    /// 依赖收集栈：每层对应本线程正在执行的一个任务体。每 worker 线程一份。
    static DEP_STACK: RefCell<Vec<Vec<RawVc>>> = const { RefCell::new(Vec::new()) };
    /// 本线程当前正在处理（持其 per-task 锁、深校验/重算中）的任务栈，
    /// 用于同线程循环依赖检测（并发模型下每个请求在单线程内 DFS 展开子树，环必在同线程出现）。
    static ACTIVE: RefCell<Vec<TaskId>> = const { RefCell::new(Vec::new()) };
}

/// RAII：任务处理结束时把它从本线程 [`ACTIVE`] 栈弹出（含 panic 展开）。
struct ActiveGuard;

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE.with(|a| {
            a.borrow_mut().pop();
        });
    }
}

/// RAII：在 `enter` 结束（含 panic 展开）时恢复上一个上下文。
struct ContextGuard(*const Engine);

impl Drop for ContextGuard {
    fn drop(&mut self) {
        CURRENT.with(|c| c.set(self.0));
    }
}

fn with_current<R>(f: impl FnOnce(&Engine) -> R) -> R {
    let ptr = CURRENT.with(|c| c.get());
    assert!(
        !ptr.is_null(),
        "wake_turbo: 任务读取/调用必须在 Engine::enter(...) 作用域内进行"
    );
    // SAFETY: `enter` 在 `f` 执行期间保证 `ptr` 指向存活的 `Engine`（单线程 enter 由 `&self`
    // 借用担保；并发 par_request 由 `Arc<Engine>` 在整个阻塞期间存活担保）。Engine 内部状态
    // 全部 Send+Sync（原子/锁/分片表），跨 worker 线程共享只读安全。
    let engine = unsafe { &*ptr };
    f(engine)
}

/// 读取一个 `Vc<T>` 的当前值（登记依赖 + 按需校验/重算 + downcast）。任务体内调用。
pub fn read<T: TaskOutput>(vc: Vc<T>) -> Arc<T> {
    with_current(|e| e.read_vc(vc))
}

/// 调用一个派生任务：登记依赖、幂等注册重算器、确保 green，返回其输出句柄。
pub fn query<T: TaskOutput>(id: TaskId, compute: impl Fn() -> T + Send + Sync + 'static) -> Vc<T> {
    with_current(|e| e.query_derived(id, compute))
}

fn record_dependency(raw: RawVc) {
    DEP_STACK.with(|s| {
        if let Some(top) = s.borrow_mut().last_mut()
            && !top.contains(&raw)
        {
            top.push(raw);
        }
    });
}

impl Engine {
    pub fn new() -> Engine {
        Self::with_mode(false)
    }

    fn with_mode(one_shot: bool) -> Engine {
        Engine {
            revision: AtomicU64::new(1),
            inputs: RwLock::new(Vec::new()),
            memos: Sharded::new(),
            recomputers: Sharded::new(),
            task_locks: Sharded::new(),
            exec_count: AtomicU64::new(0),
            incremental: AtomicBool::new(!one_shot),
            one_shot,
            query_started: AtomicBool::new(false),
        }
    }

    /// 创建单 generation 的瞬态引擎。
    ///
    /// 适合 CLI 冷构建这类“输入在首条 query 前完成组装、所有输出消费完即退出”的调用方。它保留 `Vc`
    /// 类型边界与并发 single-flight，但不计算没有消费者的红绿指纹，也不保存重算闭包/依赖边。
    /// watch、dev server 或任何会在 query 后调用 [`Engine::set_input`] 的宿主必须使用
    /// [`Engine::new`]。
    pub fn new_one_shot() -> Engine {
        Self::with_mode(true)
    }

    /// 终结瞬态引擎，并在现有工作窃取执行器上并行析构其任务图。
    ///
    /// 这是一个**消费型安全点**：调用方必须已经 join 所有 query，并把最后一个
    /// `Arc<Engine>` 的所有权传入。内部的 [`Arc::try_unwrap`] 会原子验证这一前置条件；
    /// 任一 worker、宿主或并发请求仍持有强引用时，本方法会在移动任何状态前 panic。
    /// 普通 red-green/session 引擎也会被拒绝，因其 memo 与 input 必须跨 generation 保留。
    ///
    /// 成功后先把 input 与 128 个 memo/recomputer/task-lock 分片移成独立批次，再由
    /// `executor` 并行 drop。方法会等待全部批次完成后返回，因此返回即代表瞬态状态已经
    /// 释放；消费掉 Engine 也使旧 [`Vc`] 无法再被误读。
    #[cfg(not(loom))]
    pub fn release_one_shot(self: Arc<Self>, executor: &Executor) -> OneShotReleaseStats {
        assert!(
            self.one_shot,
            "wake_turbo: release_one_shot 只能用于 one-shot 引擎"
        );
        let mut engine = match Arc::try_unwrap(self) {
            Ok(engine) => engine,
            Err(_) => panic!(
                "wake_turbo: release_one_shot 要求最后一个 Engine 强引用；必须先 join 所有 query"
            ),
        };

        let inputs = std::mem::take(
            engine
                .inputs
                .get_mut()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        let memo_shards = engine.memos.take_maps();
        let recomputer_shards = engine.recomputers.take_maps();
        let task_lock_shards = engine.task_locks.take_maps();

        debug_assert_eq!(memo_shards.len(), SHARD_COUNT);
        debug_assert_eq!(recomputer_shards.len(), SHARD_COUNT);
        debug_assert_eq!(task_lock_shards.len(), SHARD_COUNT);

        let mut stats = OneShotReleaseStats {
            input_cells: inputs.len(),
            memo_entries: memo_shards.iter().map(FxHashMap::len).sum(),
            recomputer_entries: recomputer_shards.iter().map(FxHashMap::len).sum(),
            task_locks: task_lock_shards.iter().map(FxHashMap::len).sum(),
            drop_batches: 0,
        };

        // 输入按连续块分散到相同的 128 个析构批次；memo/lock 本身已经按 TaskId 均匀分片。
        // 移动 InputCell 不克隆其 Arc 值，主线程只承担 O(n) 的轻量所有权转移。
        let input_batch_capacity = inputs.len().div_ceil(SHARD_COUNT);
        let mut inputs = inputs.into_iter();
        let mut batches = memo_shards
            .into_iter()
            .zip(recomputer_shards)
            .zip(task_lock_shards)
            .map(|((memos, recomputers), task_locks)| OneShotDropBatch {
                inputs: inputs.by_ref().take(input_batch_capacity).collect(),
                memos,
                recomputers,
                task_locks,
            })
            .filter(|batch| !batch.is_empty())
            .collect::<Vec<_>>();
        debug_assert!(inputs.next().is_none());

        // 此处 Engine 只剩空容器；在提交重析构任务前先结束它，避免返回路径再次串行遍历。
        drop(engine);
        stats.drop_batches = batches.len();
        let jobs = batches
            .drain(..)
            .map(|batch| {
                move || std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(batch)))
            })
            .collect::<Vec<_>>();
        let results = executor.parallel(jobs);
        if let Some(payload) = results.into_iter().find_map(Result::err) {
            // A cached user value may implement a panicking Drop. Keep that panic from escaping a
            // worker (which would otherwise lose the completion message and strand `parallel`),
            // but preserve normal Rust propagation after every independent batch has completed.
            std::panic::resume_unwind(payload);
        }
        stats
    }

    /// 「无增量纯并行」降级模式的引擎（DESIGN §6 降级预案）。等价 [`Engine::new`] 后
    /// 调 [`set_incremental(false)`](Engine::set_incremental)。
    pub fn new_pure_parallel() -> Engine {
        let engine = Engine::new();
        engine.set_incremental(false);
        engine
    }

    /// 运行时切换增量模式。`false` 进入「无增量纯并行」（引擎出问题时的降级开关）：
    /// 禁用跨 revision 复用与早期截断，每次变更全量重算，但仍保留 single-flight 去重 + 并行。
    pub fn set_incremental(&self, on: bool) {
        assert!(!self.one_shot, "wake_turbo: one-shot 引擎不能切换增量模式");
        self.incremental.store(on, Ordering::Relaxed);
    }

    /// 当前是否处于增量模式。
    pub fn is_incremental(&self) -> bool {
        self.incremental.load(Ordering::Relaxed)
    }

    /// 任务体真正执行的累计次数。
    pub fn exec_count(&self) -> u64 {
        self.exec_count.load(Ordering::Relaxed)
    }

    /// 当前全局 revision。
    pub fn revision(&self) -> Revision {
        self.revision.load(Ordering::Acquire)
    }

    // —— 上下文 ——

    /// 绑定本引擎为当前线程上下文并执行 `f`。任务调用与 `read` 必须在其内进行。
    pub fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        let prev = CURRENT.with(|c| c.replace(self as *const Engine));
        let _guard = ContextGuard(prev);
        f()
    }

    /// 顶层并行请求：每个闭包在一个 worker 线程内 `enter` 执行其子树，按序返回结果。
    /// 共享子任务由 single-flight 保证只执行一次。须以 `Arc<Engine>` 调用（供跨线程共享）。
    #[cfg(not(loom))]
    pub fn par_request<T, F>(self: &Arc<Self>, exec: &Executor, requests: Vec<F>) -> Vec<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let jobs: Vec<_> = requests
            .into_iter()
            .map(|req| {
                let engine = Arc::clone(self);
                move || engine.enter(req)
            })
            .collect();
        exec.parallel(jobs)
    }

    // —— 输入 cell ——

    /// 新建一个输入 cell，返回其句柄。
    pub fn new_input<T: TaskOutput>(&self, value: T) -> Vc<T> {
        let fingerprint = if self.one_shot {
            0
        } else {
            value.fingerprint()
        };
        let now = self.revision.load(Ordering::Acquire);
        let mut inputs = self.inputs.write().unwrap();
        let idx = inputs.len() as u32;
        inputs.push(InputCell {
            value: Arc::new(value),
            fingerprint,
            changed_at: now,
        });
        Vc::from_raw(RawVc::Input(idx))
    }

    /// 写入一个输入 cell。普通引擎仅在**指纹确实变化时**才 +1 revision 并记 `changed_at`
    /// （输入级早期截断）。One-shot 引擎允许在第一条派生 query 前组装输入，随后永久冻结。
    /// 并发模型下应在「无并行 query」阶段调用（一批变更后再统一重算）。
    pub fn set_input<T: TaskOutput>(&self, vc: Vc<T>, value: T) {
        let RawVc::Input(i) = vc.raw() else {
            panic!("wake_turbo: set_input 只能用于输入 cell");
        };
        let i = i as usize;
        if self.one_shot {
            if self.query_started.load(Ordering::Acquire) {
                panic!("wake_turbo: one-shot 引擎在派生 query 开始后不可更新输入");
            }
            let mut inputs = self.inputs.write().unwrap();
            // 与 query 发布形成冻结边界：若两者竞争，query 要么看到本次更新，要么本次更新
            // 明确失败，不能在没有依赖边的情况下留下陈旧任务值。
            if self.query_started.load(Ordering::Acquire) {
                drop(inputs);
                panic!("wake_turbo: one-shot 引擎在派生 query 开始后不可更新输入");
            }
            inputs[i].value = Arc::new(value);
            return;
        }
        let fingerprint = value.fingerprint();
        let mut inputs = self.inputs.write().unwrap();
        if inputs[i].fingerprint != fingerprint {
            inputs[i].value = Arc::new(value);
            inputs[i].fingerprint = fingerprint;
            let r = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
            inputs[i].changed_at = r;
        }
    }

    // —— 查询体内使用的原语（会登记依赖）——

    fn read_vc<T: TaskOutput>(&self, vc: Vc<T>) -> Arc<T> {
        let raw = vc.raw();
        if self.one_shot {
            return self
                .value_of(raw)
                .downcast::<T>()
                .expect("wake_turbo: Vc 读取类型与存储值不匹配");
        }
        record_dependency(raw);
        self.ensure(raw);
        self.value_of(raw)
            .downcast::<T>()
            .expect("wake_turbo: Vc 读取类型与存储值不匹配")
    }

    fn query_derived<T: TaskOutput>(
        &self,
        id: TaskId,
        compute: impl Fn() -> T + Send + Sync + 'static,
    ) -> Vc<T> {
        if self.one_shot {
            if !self.query_started.load(Ordering::Acquire) {
                // Holding the input read lock makes publication atomic with respect to
                // `set_input`'s write-locked recheck. Whichever side acquires the lock first wins:
                // the query observes the completed update, or the setter observes the freeze and
                // fails before changing the slot. Later queries take only the atomic fast path.
                let inputs = self.inputs.read().unwrap();
                self.query_started.store(true, Ordering::Release);
                drop(inputs);
            }
            return self.query_one_shot(id, compute);
        }
        record_dependency(RawVc::Task(id));
        // 幂等注册重算器：同一 TaskId 计算逻辑恒定（TaskId = 函数+参数指纹），只注册一次。
        self.recomputers.ensure_with(id, || {
            Arc::new(move || {
                let value = compute();
                let fingerprint = value.fingerprint();
                (Arc::new(value) as AnyValue, fingerprint)
            })
        });
        self.ensure_green(id);
        Vc::from_raw(RawVc::Task(id))
    }

    /// 瞬态任务只物化一次值。没有后续 generation，故 `fingerprint/deps/recomputer` 均无消费者；
    /// 保留与普通路径相同的循环检测和 per-task single-flight，确保并发语义不分叉。
    fn query_one_shot<T: TaskOutput>(
        &self,
        id: TaskId,
        compute: impl Fn() -> T + Send + Sync + 'static,
    ) -> Vc<T> {
        let vc = Vc::from_raw(RawVc::Task(id));
        if self.memos.with(id, |memo| memo.is_some()) {
            return vc;
        }
        if ACTIVE.with(|active| active.borrow().contains(&id)) {
            let path = ACTIVE.with(|active| active.borrow().clone());
            panic!("wake_turbo: 检测到循环依赖——任务 {id:?} 在本线程处理链 {path:?} 中被再次请求");
        }

        let lock = self
            .task_locks
            .get_or_insert_with(id, || Arc::new(Mutex::new(())));
        let _lock = lock.lock().unwrap();
        if self.memos.with(id, |memo| memo.is_some()) {
            return vc;
        }

        ACTIVE.with(|active| active.borrow_mut().push(id));
        let _active = ActiveGuard;
        self.exec_count.fetch_add(1, Ordering::Relaxed);
        let value = Arc::new(compute()) as AnyValue;
        let now = self.revision.load(Ordering::Acquire);
        self.memos.insert(
            id,
            Memo {
                value,
                fingerprint: 0,
                verified_at: now,
                changed_at: now,
                deps: Vec::new(),
            },
        );
        vc
    }

    // —— 红绿校验（对齐 spike `verify_value`，加 single-flight）——

    /// 确保 `id` 在当前 revision 下 green（必要时按需重算）。**不** 登记依赖。
    fn ensure_green(&self, id: TaskId) {
        // 快路径：无 per-task 锁的浅绿判定（只读 verified_at，不克隆整个 Memo）。
        let now = self.revision.load(Ordering::Acquire);
        if self.memos.with(id, |m| m.map(|m| m.verified_at)) == Some(now) {
            return; // 浅绿
        }

        // 循环依赖检测：本线程已在处理 id（其 per-task 锁被本线程持有）→ 依赖成环。
        // 必须在获取锁 **前** 判定，否则同线程重入非重入 Mutex 会死锁。
        if ACTIVE.with(|a| a.borrow().contains(&id)) {
            let path = ACTIVE.with(|a| a.borrow().clone());
            panic!("wake_turbo: 检测到循环依赖——任务 {id:?} 在本线程处理链 {path:?} 中被再次请求");
        }

        // single-flight：获取 per-task 执行锁，序列化对同一 task 的校验/重算。
        let lock = self
            .task_locks
            .get_or_insert_with(id, || Arc::new(Mutex::new(())));
        let _guard = lock.lock().unwrap();

        // 持锁后以最新 revision double-check 浅绿（别的线程可能刚算完）。
        let now = self.revision.load(Ordering::Acquire);
        let snapshot = self.memos.get_cloned(id);
        if let Some(memo) = &snapshot
            && memo.verified_at == now
        {
            return; // 浅绿（double-check）
        }

        // 标记本线程正在处理 id：深校验与重算都会递归子任务，环检测据此生效。
        ACTIVE.with(|a| a.borrow_mut().push(id));
        let _active = ActiveGuard;

        // 深校验：所有依赖自 verified_at 起都没变 → 漂绿复用（降级模式下 deep_verify 恒 false）。
        if let Some(memo) = &snapshot
            && self.deep_verify(&memo.deps, memo.verified_at)
        {
            self.memos.update(id, |m| m.verified_at = now);
            return;
        }

        // 重算 + 早期截断。
        let (value, fingerprint, deps) = self.execute(id);
        let changed_at = match &snapshot {
            // 早期截断：指纹未变，changed_at 不推进（下游据此判「没变」被截断）。
            Some(m) if m.fingerprint == fingerprint => m.changed_at,
            _ => now,
        };
        self.memos.insert(
            id,
            Memo {
                value,
                fingerprint,
                verified_at: now,
                changed_at,
                deps,
            },
        );
    }

    /// 执行任务体，返回 `(擦除值, 指纹, 本次读到的直接依赖)`。
    fn execute(&self, id: TaskId) -> (AnyValue, Hash64, Vec<RawVc>) {
        self.exec_count.fetch_add(1, Ordering::Relaxed);
        let recompute = self
            .recomputers
            .get_cloned(id)
            .expect("wake_turbo: 任务重算器未注册（ensure_green 前必先 query 注册）");
        DEP_STACK.with(|s| s.borrow_mut().push(Vec::new()));
        let (value, fingerprint) = recompute();
        let deps = DEP_STACK.with(|s| s.borrow_mut().pop().unwrap());
        (value, fingerprint, deps)
    }

    /// 深校验：任一依赖自 `since` 起可能变化则返回 false（需重算）。
    /// 降级模式（`!incremental`）下恒返回 false——禁跨 revision 复用，强制全量重算。
    fn deep_verify(&self, deps: &[RawVc], since: Revision) -> bool {
        if !self.incremental.load(Ordering::Relaxed) {
            return false;
        }
        for &dep in deps {
            if self.maybe_changed_since(dep, since) {
                return false;
            }
        }
        true
    }

    /// `raw` 的值自 revision `since` 起是否可能变化。
    fn maybe_changed_since(&self, raw: RawVc, since: Revision) -> bool {
        match raw {
            RawVc::Input(i) => self.inputs.read().unwrap()[i as usize].changed_at > since,
            RawVc::Task(id) => {
                // 先确保最新（可能触发重算 + 早期截断），再看它的 changed_at。
                self.ensure_green(id);
                self.memos
                    .with(id, |m| m.expect("已 ensure_green 必存在").changed_at)
                    > since
            }
        }
    }

    // —— 读取辅助 ——

    fn ensure(&self, raw: RawVc) {
        match raw {
            RawVc::Input(_) => {} // 输入 cell 总是最新
            RawVc::Task(id) => self.ensure_green(id),
        }
    }

    fn value_of(&self, raw: RawVc) -> AnyValue {
        match raw {
            RawVc::Input(i) => self.inputs.read().unwrap()[i as usize].value.clone(),
            RawVc::Task(id) => self
                .memos
                .with(id, |m| m.expect("已 ensure 必存在").value.clone()),
        }
    }
}

impl Default for Engine {
    fn default() -> Engine {
        Engine::new()
    }
}

impl<T: TaskOutput> Vc<T> {
    /// 读取当前值（登记依赖 + 按需校验）。等价于自由函数 [`read`]，须在 [`Engine::enter`] 内。
    pub fn read(self) -> Arc<T> {
        read(self)
    }
}
