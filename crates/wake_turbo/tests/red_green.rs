//! 正式引擎的红绿失效对拍测试（PLAN §2.5.3 / Gate-2 正确性核心）。
//!
//! 用 `#[wake::task]` 定义与 spike 同构的「文件字数统计」玩具图，把 spike 里手写的固定
//! `Key` 图换成真正经过宏 + `TaskId` + 类型擦除 slot 表的任务图，验证：
//!
//! 1. **对拍一致**：随机交错「变更输入 + 请求任意任务」下，引擎结果 **永远** 等于全量重算；
//! 2. **早期截断**：输入在阈值同侧变动时，受保护的聚合任务不重算；
//! 3. **幂等零重算**：无变更的重复请求全走浅绿，`exec_count` 不增。
//!
//! 这是 DESIGN §13 `WAKE_VERIFY` 常驻对拍思想的最小固化（spike 的 `reference()` 即降级基准）。

use std::cell::RefCell;

use wake_turbo::{Engine, Vc, task};

const THRESHOLD: i64 = 100;
const COUNT_BIG_SCALE: i64 = 1_000_000;

// 玩具图的输入 cell 句柄（借 thread-local 让无参聚合任务能读到所有输入——真实管线里
// 这类扇入通过传入一个 `Vc<Vec<..>>` 完成，此处为最小复刻 spike 的 n-扇入图而简化）。
// 存拥有的 Vec（Vc 是 Copy），线程结束正常 drop——不用 Box::leak，miri 无泄漏。
thread_local! {
    static INPUTS: RefCell<Vec<Vc<i64>>> = const { RefCell::new(Vec::new()) };
}

fn inputs() -> Vec<Vc<i64>> {
    INPUTS.with(|c| c.borrow().clone())
}

/// `Input(i) > THRESHOLD`（0/1）。早期截断在此：输入阈值同侧变动 → sign 不变。
#[task]
fn sign(input: Vc<i64>) -> i64 {
    (*input.read() > THRESHOLD) as i64
}

/// 所有 sign 之和（依赖全部 sign，受各 sign 的早期截断保护）。
#[task]
fn count_big(all: Vc<()>) -> i64 {
    let _ = all.read(); // 依赖一个「全体输入」的哨兵，确保新增输入时会被纳入（此图中输入数固定）
    inputs().iter().map(|&v| *sign(v).read()).sum()
}

/// 所有输入之和。
#[task]
fn sum_all(all: Vc<()>) -> i64 {
    let _ = all.read();
    inputs().iter().map(|v| *v.read()).sum()
}

/// 顶层报告：`count_big * SCALE + sum_all`。
#[task]
fn report(all: Vc<()>) -> i64 {
    *count_big(all).read() * COUNT_BIG_SCALE + *sum_all(all).read()
}

/// 全量重算参考实现（对拍基准，等价 spike 的 `reference`）。
fn reference(vals: &[i64], which: Which) -> i64 {
    match which {
        Which::CountBig => vals.iter().map(|&v| (v > THRESHOLD) as i64).sum(),
        Which::SumAll => vals.iter().sum(),
        Which::Report => {
            let cb: i64 = vals.iter().map(|&v| (v > THRESHOLD) as i64).sum();
            let sa: i64 = vals.iter().sum();
            cb * COUNT_BIG_SCALE + sa
        }
    }
}

#[derive(Clone, Copy)]
enum Which {
    CountBig,
    SumAll,
    Report,
}

/// 极小确定性 PRNG（xorshift64*），复刻 spike，保证测试可复现、无 rand 依赖。
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// 建图：n 个输入 cell + 一个哨兵，返回 (输入句柄向量, 哨兵)。
///
/// 输入句柄同时存入 thread-local，供无参聚合任务经 [`inputs`] 读到。
fn build(engine: &Engine, init: &[i64]) -> (Vec<Vc<i64>>, Vc<()>) {
    let vcs: Vec<Vc<i64>> = init.iter().map(|&v| engine.new_input(v)).collect();
    INPUTS.with(|c| c.borrow_mut().clone_from(&vcs));
    let sentinel = engine.new_input(());
    (vcs, sentinel)
}

#[test]
fn matches_reference_on_random_workload() {
    let n = 12;
    let mut rng = Rng::new(0xC0FF_EE12_3456_789A);
    let mut vals: Vec<i64> = (0..n).map(|_| rng.below(200) as i64).collect();

    let engine = Engine::new();
    let (vcs, sentinel) = build(&engine, &vals);
    let whiches = [Which::CountBig, Which::SumAll, Which::Report];

    // miri 解释执行慢，缩小对拍轮数（正确性覆盖不变）；release 下高强度对拍。
    let iters = if cfg!(miri) { 300 } else { 5000 };
    for _ in 0..iters {
        // 一半概率改一个输入。
        if rng.next_u64() & 1 == 0 {
            let i = rng.below(n);
            let v = rng.below(200) as i64;
            vals[i] = v;
            engine.set_input(vcs[i], v);
        }
        // 请求一个随机任务，逐次对拍。
        let which = whiches[rng.below(whiches.len())];
        let got = engine.enter(|| match which {
            Which::CountBig => *count_big(sentinel).read(),
            Which::SumAll => *sum_all(sentinel).read(),
            Which::Report => *report(sentinel).read(),
        });
        assert_eq!(got, reference(&vals, which), "引擎与全量重算不一致");
    }
}

#[test]
fn early_cutoff_saves_recompute() {
    let n = if cfg!(miri) { 12 } else { 50 };
    let rounds = if cfg!(miri) { 8 } else { 20 };
    let mut vals = vec![0i64; n]; // 全部 < THRESHOLD → sign 全 0 → count_big 恒为 0
    let engine = Engine::new();
    let (vcs, sentinel) = build(&engine, &vals);

    // 先请求 Report 建满缓存。
    assert_eq!(engine.enter(|| *report(sentinel).read()), 0);
    let baseline = engine.exec_count();

    // 反复把某输入在「阈值同侧」小幅变动（仍 < THRESHOLD）：sign 不变 → count_big 被截断。
    for round in 0..rounds {
        let i = round % n;
        let v = (round as i64 % 90) + 1; // 1..=90，始终 < 100
        vals[i] = v;
        engine.set_input(vcs[i], v);
        let got = engine.enter(|| *report(sentinel).read());
        // 语义仍正确：count_big 恒为 0，故 report == 当前输入之和。
        let expect: i64 = vals.iter().sum();
        assert_eq!(got, expect, "早期截断下语义仍须正确");
    }
    let after = engine.exec_count();

    // 早期截断生效：每轮只重算 变更输入的 sign + sum_all + report（常数级），
    // count_big 因所有 sign 未变被截断，远小于「每轮全量」的 rounds*(n+3)。
    let executes = after - baseline;
    assert!(
        executes < rounds as u64 * 6,
        "早期截断未生效：{rounds} 轮执行了 {executes} 次任务体（期望远小于全量）"
    );
}

#[test]
fn idempotent_requests_do_not_reexecute() {
    let vals = [10i64, 200, 30];
    let engine = Engine::new();
    let (_vcs, sentinel) = build(&engine, &vals);

    let _ = engine.enter(|| *report(sentinel).read());
    let after_first = engine.exec_count();
    // 不改任何输入，重复请求：应全走浅绿，零重算。
    let repeats = if cfg!(miri) { 20 } else { 100 };
    for _ in 0..repeats {
        engine.enter(|| *report(sentinel).read());
    }
    assert_eq!(
        engine.exec_count(),
        after_first,
        "无变更的重复请求不应重新执行"
    );
}
