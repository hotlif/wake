//! # wake_turbo — 函数级增量计算引擎
//!
//! DESIGN §10：记忆化并发任务图 = Salsa 红绿失效算法 × turbo-tasks 全并发执行 × 自研工作窃取执行器。
//!
//! **当前能力（历史阶段索引见 PLAN §2.5）**：
//! - [`spike`]：单线程红绿 + 早期截断的正确性证明（PLAN §0.6，历史留存）。
//! - [`engine`] + [`vc`]：正式引擎核心——`TaskId` / `Vc<T>` 句柄 / 类型擦除 slot 表 /
//!   thread-local 依赖收集 / 红绿失效 + 早期截断 / 分片并发 slot / single-flight。
//! - [`task`]：`#[wake::task]` 过程宏，把纯函数登记为增量任务。
//! - [`executor`]：自研工作窃取执行器（PLAN §2.5.5 / DESIGN §10.5），并行执行独立任务扇出。
//! - 并发协议由 Loom 验证，循环依赖检测与无增量纯并行降级由 Gate-2 测试覆盖。
//!
//! 产品层取消由 `wake_app::CancellationToken` / Node `AbortSignal` 在安全点协作完成；引擎本体
//! 尚未提供抢占任意正在执行任务的通用 generation 取消。
//!
//! ## 用法速览
//!
//! ```
//! use wake_turbo::{task, Engine, Vc};
//!
//! #[task]
//! fn double(x: Vc<i64>) -> i64 {
//!     *x.read() * 2
//! }
//!
//! let engine = Engine::new();
//! let x = engine.new_input(21i64);
//! let y = engine.enter(|| double(x));
//! assert_eq!(*engine.enter(|| y.read()), 42);
//! ```

pub mod engine;
// 执行器依赖 crossbeam，loom 验证不需要它（见 tests/loom_single_flight.rs）。
#[cfg(not(loom))]
pub mod executor;
pub mod spike;
pub mod vc;

pub use engine::{Engine, query, read};
#[cfg(not(loom))]
pub use executor::{Executor, global_executor};
pub use vc::{RawVc, Revision, TaskArg, TaskId, TaskOutput, Vc};

// 重导出过程宏，上层以 `#[wake_turbo::task]` 或 `use wake_turbo::task;` 使用。
pub use wake_turbo_macros::task;
