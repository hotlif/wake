//! # 引擎核心句柄与值类型（PLAN §2.5.2）
//!
//! - [`Revision`]：全局单调版本号（红绿算法的时间轴，取自 spike/Salsa）。
//! - [`TaskId`]：`fx_hash(函数指纹, 参数指纹)`——同参调用全局唯一（DESIGN §10.3）。
//! - [`RawVc`]：类型擦除的节点引用（输入 cell 或派生任务）。
//! - [`Vc<T>`]：带类型标记的轻量句柄，任务间只传它（u64 索引级），读取即登记依赖。
//! - [`TaskOutput`]：任务输出值的约束——`Send + Sync + 'static` + 内容指纹（早期截断的比较量）。
//! - [`TaskArg`]：任务参数的指纹来源。第一版约定参数均为 `Vc<T>`（DESIGN §10.3 示例即如此）。

use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use wake_common::Hash64;
use xxhash_rust::xxh3::Xxh3;

/// 全局单调递增版本号。修改输入且值确实变化时 +1。
pub type Revision = u64;

/// 派生任务的全局唯一标识：`fx_hash(函数指纹, 参数指纹...)`。
///
/// 同一函数以同一组参数（同 `RawVc`）调用得到相同 `TaskId`，从而在引擎里
/// 全局唯一执行（自动去重，替代手工 `seen` 集合，DESIGN §10.3）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TaskId(pub u64);

impl TaskId {
    /// 由「函数位置（`module` + `name`）」与「参数句柄」派生 id。
    ///
    /// 用 xxh3 混合：先喂模块路径与函数名，再依次喂每个参数的 [`RawVc`]。模块+名唯一确定
    /// 一个任务函数；参数句柄稳定（输入 cell 索引 / 上游 `TaskId` 均是进程内稳定量），
    /// 故同参调用 id 稳定。`module`/`name` 分开传是因 `concat!` 无法展开 `module_path!()`。
    pub fn of(module: &str, name: &str, args: &[RawVc]) -> TaskId {
        let mut h = Xxh3::new();
        h.write(module.as_bytes());
        h.write(b"::");
        h.write(name.as_bytes());
        for a in args {
            a.hash(&mut h);
        }
        TaskId(h.finish())
    }
}

/// 类型擦除的节点引用。派生任务与输入 cell 共用一个引用命名空间，
/// 依赖边、slot 读取都以它为键。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RawVc {
    /// 输入 cell（文件内容、配置切片……），由引擎递增分配。
    Input(u32),
    /// 派生任务的输出，由 [`TaskId`] 定位。
    Task(TaskId),
}

/// 带类型标记的轻量句柄：任务间只传它，读取时 downcast 回 `T`。
///
/// `Vc<T>` 只是一个 `RawVc`，本身不持有值——值在引擎的 slot 表里。故它
/// `Copy` 且 `Send + Sync`（与 `T` 是否 `Send`/`Sync` 无关，用 `fn() -> T` 标记规避）。
pub struct Vc<T> {
    raw: RawVc,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Vc<T> {
    pub(crate) fn from_raw(raw: RawVc) -> Vc<T> {
        Vc {
            raw,
            _marker: PhantomData,
        }
    }

    /// 底层类型擦除引用。
    pub fn raw(self) -> RawVc {
        self.raw
    }
}

// 手动实现，避免 derive 给 `T` 附加不必要的 `Clone`/`Copy` 约束（`Vc<T>` 永远 `Copy`）。
impl<T> Clone for Vc<T> {
    fn clone(&self) -> Vc<T> {
        *self
    }
}
impl<T> Copy for Vc<T> {}

impl<T> PartialEq for Vc<T> {
    fn eq(&self, other: &Vc<T>) -> bool {
        self.raw == other.raw
    }
}
impl<T> Eq for Vc<T> {}

impl<T> std::fmt::Debug for Vc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Vc({:?})", self.raw)
    }
}

/// 任务输出值的约束：可跨线程共享、`'static`，并能给出内容指纹用于早期截断。
///
/// 早期截断需要比较「重算后的新值是否等于旧值」。类型擦除下无法用 `PartialEq`，
/// 故改用内容指纹（xxh3）比较——这正是 DESIGN §10.3「输出与缓存值做指纹比较」的落地。
/// 为任意 `T: Hash` 提供 blanket 实现，玩具图与真实产物（可 `derive(Hash)`）都自动满足。
pub trait TaskOutput: Send + Sync + 'static {
    /// 该值的内容指纹（xxh3）。
    fn fingerprint(&self) -> Hash64;
}

impl<T: Hash + Send + Sync + 'static> TaskOutput for T {
    fn fingerprint(&self) -> Hash64 {
        let mut h = Xxh3::new();
        self.hash(&mut h);
        h.finish()
    }
}

/// 任务参数的指纹来源。第一版只支持 `Vc<T>` 参数（参与 `TaskId` 计算的是其句柄，
/// 而非其当前值——值变化通过红绿失效沿依赖边传播，不改变 `TaskId`）。
pub trait TaskArg {
    /// 参与 `TaskId` 计算的稳定句柄。
    fn arg_ref(&self) -> RawVc;
}

impl<T> TaskArg for Vc<T> {
    fn arg_ref(&self) -> RawVc {
        self.raw
    }
}
