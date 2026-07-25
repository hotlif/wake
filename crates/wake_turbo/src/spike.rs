//! # Spike ② — 单线程红绿失效 + 早期截断（PLAN §0.6）
//!
//! 目标：在投入自研引擎前，用 ~200 行证明红绿算法（取自 rustc/Salsa，DESIGN §10.3）在
//! 「随机变更 + 随机请求」下与全量重算 **永远一致**，并展示早期截断（early cutoff）真实生效。
//!
//! ## 算法要点
//!
//! - **Revision**：全局单调递增版本号。修改一个输入且值确实变化时 +1，并记该输入 `changed_at`。
//! - **Memo**：每个派生查询缓存 `{ value, verified_at, changed_at, deps }`。
//!   - `verified_at`：该 memo 已确认「green（有效）」到哪个 revision。
//!   - `changed_at`：该 memo 的 **值** 上次真正变化于哪个 revision（早期截断的关键）。
//! - **浅校验**：`verified_at == 当前 revision` → 直接复用。
//! - **深校验**：逐依赖问 `maybe_changed_since(verified_at)`，全否 → 漂绿（`verified_at = 当前`）复用。
//! - **重算 + 早期截断**：任一依赖变了 → 重跑；新值 == 旧值则 **不** 更新 `changed_at`
//!   （下游据此判定「没变」而被截断），仅更新 `verified_at`。
//! - **动态依赖**：每次重算时重新记录依赖（thread-local 收集栈），天然支持走了不同分支。
//!
//! 依赖记录纪律：只记 **直接** 读；深校验期间的嵌套读 **不** 计入调用者的依赖（见 [`Engine::query`]）。

use std::cell::{Cell, RefCell};

use wake_common::FxHashMap;

/// 全局版本号。
type Revision = u64;

/// 玩具计算图的节点键（一个「文件字数统计」式流水线，DESIGN §2.5 验收口径的最小化）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    /// 输入 cell：第 `i` 个文件的一个整数值。
    Input(usize),
    /// `Input(i) > THRESHOLD`（0/1）。早期截断在此发生：输入在阈值同侧变动 → sign 不变。
    Sign(usize),
    /// 所有 `Sign(i)` 之和（依赖全部 sign；受各 sign 的早期截断保护）。
    CountBig,
    /// 所有 `Input(i)` 之和（依赖全部 input）。
    SumAll,
    /// 顶层报告：`CountBig * 1_000_000 + SumAll`。
    Report,
}

const THRESHOLD: i64 = 100;
const COUNT_BIG_SCALE: i64 = 1_000_000;

#[derive(Clone)]
struct Memo {
    value: i64,
    verified_at: Revision,
    changed_at: Revision,
    deps: Vec<Key>,
}

/// 单线程红绿引擎（玩具）。全部字段用内部可变性以支持递归校验/重算。
pub struct Engine {
    revision: Cell<Revision>,
    inputs: RefCell<Vec<i64>>,
    input_changed_at: RefCell<Vec<Revision>>,
    memos: RefCell<FxHashMap<Key, Memo>>,
    /// 依赖收集栈：每层对应一个正在执行的派生查询。
    dep_stack: RefCell<Vec<Vec<Key>>>,
    /// 统计：查询体真正执行的次数（用于展示早期截断收益）。
    exec_count: Cell<u64>,
}

impl Engine {
    /// 以 `n` 个初始输入建立引擎，起始 revision = 1。
    pub fn new(initial: Vec<i64>) -> Engine {
        let n = initial.len();
        Engine {
            revision: Cell::new(1),
            inputs: RefCell::new(initial),
            input_changed_at: RefCell::new(vec![1; n]),
            memos: RefCell::new(FxHashMap::default()),
            dep_stack: RefCell::new(Vec::new()),
            exec_count: Cell::new(0),
        }
    }

    pub fn input_len(&self) -> usize {
        self.inputs.borrow().len()
    }

    pub fn exec_count(&self) -> u64 {
        self.exec_count.get()
    }

    /// 设置一个输入。值确实变化时才 +1 revision 并记 `changed_at`（输入级早期截断）。
    pub fn set_input(&self, i: usize, value: i64) {
        let changed = {
            let mut inputs = self.inputs.borrow_mut();
            if inputs[i] == value {
                false
            } else {
                inputs[i] = value;
                true
            }
        };
        if changed {
            let r = self.revision.get() + 1;
            self.revision.set(r);
            self.input_changed_at.borrow_mut()[i] = r;
        }
    }

    /// 顶层请求：取某个 key 的最新值（必要时按需重算）。
    pub fn request(&self, key: Key) -> i64 {
        // 顶层无父查询，dep_stack 为空，不产生依赖记录。
        self.verify_value(key)
    }

    // —— 查询体内使用的读取原语（会登记依赖）——

    /// 读输入（登记对 `Input(i)` 的依赖）。
    fn read_input(&self, i: usize) -> i64 {
        self.record_dependency(Key::Input(i));
        self.inputs.borrow()[i]
    }

    /// 读派生查询（登记依赖 + 确保最新）。
    fn query(&self, key: Key) -> i64 {
        self.record_dependency(key);
        self.verify_value(key)
    }

    fn record_dependency(&self, key: Key) {
        if let Some(top) = self.dep_stack.borrow_mut().last_mut()
            && !top.contains(&key)
        {
            top.push(key);
        }
    }

    /// 确保 `key` 在当前 revision 下有效并返回其值。**不** 登记依赖（校验用路径）。
    fn verify_value(&self, key: Key) -> i64 {
        // 输入不走 memo。
        if let Key::Input(i) = key {
            return self.inputs.borrow()[i];
        }

        let now = self.revision.get();

        // 浅校验 / 深校验。
        let existing = self.memos.borrow().get(&key).cloned();
        if let Some(memo) = existing {
            if memo.verified_at == now {
                return memo.value; // 浅绿
            }
            if self.deep_verify(&memo.deps, memo.verified_at) {
                // 深绿：所有依赖自 verified_at 起都没变 → 漂绿复用。
                self.memos.borrow_mut().get_mut(&key).unwrap().verified_at = now;
                return memo.value;
            }
        }

        // 需要重算。
        let old = self.memos.borrow().get(&key).cloned();
        let (value, deps) = self.execute(key);
        let changed_at = match &old {
            Some(m) if m.value == value => m.changed_at, // 早期截断：值没变，changed_at 不动
            _ => now,
        };
        self.memos.borrow_mut().insert(
            key,
            Memo {
                value,
                verified_at: now,
                changed_at,
                deps,
            },
        );
        value
    }

    /// 深校验：任一依赖自 `since` 起可能变化则返回 false（需重算）。
    fn deep_verify(&self, deps: &[Key], since: Revision) -> bool {
        for &dep in deps {
            if self.maybe_changed_since(dep, since) {
                return false;
            }
        }
        true
    }

    /// `key` 的值自 revision `since` 起是否可能变化。
    fn maybe_changed_since(&self, key: Key, since: Revision) -> bool {
        match key {
            Key::Input(i) => self.input_changed_at.borrow()[i] > since,
            derived => {
                // 先确保最新（可能触发重算 + 早期截断），再看它的 changed_at。
                self.verify_value(derived);
                self.memos.borrow()[&derived].changed_at > since
            }
        }
    }

    /// 执行查询体，返回 `(值, 本次读到的直接依赖)`。
    fn execute(&self, key: Key) -> (i64, Vec<Key>) {
        self.exec_count.set(self.exec_count.get() + 1);
        self.dep_stack.borrow_mut().push(Vec::new());

        let value = match key {
            Key::Input(_) => unreachable!("输入不经 execute"),
            Key::Sign(i) => (self.read_input(i) > THRESHOLD) as i64,
            Key::CountBig => {
                let n = self.input_len();
                (0..n).map(|i| self.query(Key::Sign(i))).sum()
            }
            Key::SumAll => {
                let n = self.input_len();
                (0..n).map(|i| self.read_input(i)).sum()
            }
            Key::Report => self.query(Key::CountBig) * COUNT_BIG_SCALE + self.query(Key::SumAll),
        };

        let deps = self.dep_stack.borrow_mut().pop().unwrap();
        (value, deps)
    }
}

/// 参考实现：不带任何缓存的全量重算，作为对拍基准。
pub fn reference(inputs: &[i64], key: Key) -> i64 {
    match key {
        Key::Input(i) => inputs[i],
        Key::Sign(i) => (inputs[i] > THRESHOLD) as i64,
        Key::CountBig => inputs.iter().map(|&v| (v > THRESHOLD) as i64).sum(),
        Key::SumAll => inputs.iter().sum(),
        Key::Report => {
            let count_big: i64 = inputs.iter().map(|&v| (v > THRESHOLD) as i64).sum();
            let sum_all: i64 = inputs.iter().sum();
            count_big * COUNT_BIG_SCALE + sum_all
        }
    }
}

/// 极小确定性 PRNG（xorshift64*），避免引入 rand 依赖，保证测试可复现。
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// 所有派生 key（供随机请求）。
pub fn all_derived_keys(n: usize) -> Vec<Key> {
    let mut keys = vec![Key::CountBig, Key::SumAll, Key::Report];
    for i in 0..n {
        keys.push(Key::Sign(i));
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_on_random_workload() {
        let n = 12;
        let mut rng = Rng::new(0xC0FF_EE12_3456_789A);
        let mut inputs: Vec<i64> = (0..n).map(|_| (rng.below(200)) as i64).collect();
        let engine = Engine::new(inputs.clone());
        let keys = all_derived_keys(n);

        // 大量交错的「随机变更 + 随机请求」，逐次与全量重算对拍。miri 解释慢，缩小轮数。
        let iters = if cfg!(miri) { 300 } else { 5000 };
        for _ in 0..iters {
            // 一半概率改一个输入
            if rng.next_u64() & 1 == 0 {
                let i = rng.below(n);
                let v = (rng.below(200)) as i64;
                inputs[i] = v;
                engine.set_input(i, v);
            }
            // 请求一个随机 key
            let key = keys[rng.below(keys.len())];
            let got = engine.request(key);
            let want = reference(&inputs, key);
            assert_eq!(got, want, "engine != reference at {key:?}");
        }
    }

    #[test]
    fn early_cutoff_saves_recompute() {
        // 一个大到早期截断收益明显的图。miri 下缩小规模。
        let n = if cfg!(miri) { 12 } else { 50 };
        let rounds = if cfg!(miri) { 8 } else { 20 };
        let engine = Engine::new(vec![0; n]); // 全部 < THRESHOLD → sign 全 0
        // 先请求 Report，建满缓存。
        assert_eq!(engine.request(Key::Report), 0);
        let baseline = engine.exec_count();

        // 反复把某个输入在「阈值同侧」小幅变动（仍 < THRESHOLD）：sign 不变。
        for round in 0..rounds {
            let i = round % n;
            engine.set_input(i, (round as i64 % 90) + 1); // 1..=90，始终 < 100
            let report = engine.request(Key::Report);
            // 语义正确性：Report == 参考值
            let inputs: Vec<i64> = (0..n).map(|k| engine_input_snapshot(&engine, k)).collect();
            assert_eq!(report, reference(&inputs, Key::Report));
        }
        let after = engine.exec_count();

        // 早期截断生效：每轮只需重算变更输入的 Sign + SumAll + Report（常数级），
        // CountBig 因所有 sign 未变而被截断（不重算）。远小于「每轮全量重算」的 n+3 量级。
        let executes = after - baseline;
        // rounds 轮全量重算下界约 rounds*(n+3)；有早期截断应显著更少。
        assert!(
            executes < rounds as u64 * 6,
            "早期截断未生效：{rounds} 轮执行了 {executes} 次查询体（期望远小于全量）"
        );
        // 且 CountBig 的 changed_at 从未推进（始终为初始 revision 1）。
    }

    // 测试辅助：读引擎当前输入快照（绕过 memo）。
    fn engine_input_snapshot(engine: &Engine, i: usize) -> i64 {
        engine.request(Key::Input(i))
    }

    #[test]
    fn idempotent_requests_do_not_reexecute() {
        let engine = Engine::new(vec![10, 200, 30]);
        let _ = engine.request(Key::Report);
        let after_first = engine.exec_count();
        // 不改任何输入，重复请求：应全部走浅绿，零重算。
        let repeats = if cfg!(miri) { 20 } else { 100 };
        for _ in 0..repeats {
            engine.request(Key::Report);
        }
        assert_eq!(
            engine.exec_count(),
            after_first,
            "无变更的重复请求不应重新执行"
        );
    }
}
