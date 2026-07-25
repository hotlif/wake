# Spike ② — 单线程红绿失效 + 早期截断

> 对应：PLAN §0.6 / DESIGN §10.3
> 结论：**可行（GREEN）**，红绿算法在随机变更+随机请求下与全量重算永远一致，早期截断显著减负。
> 代码：[`crates/wake_turbo/src/spike.rs`](../../crates/wake_turbo/src/spike.rs)、
> demo：`cargo run -p wake_turbo --example spike_red_green`

## 1. 要验证什么

wake_turbo 引擎最难查的 bug 是 **失效传播漏标/错标 → 产物陈旧**（DESIGN §13 风险表，等级「高」）。
在投入正式引擎（Cell/slot 表/工作窃取执行器）之前，先用 ~200 行单线程实现证明 **红绿算法本身正确**：

1. 「随机变更输入 + 随机请求任意查询」下，结果 **永远** 等于全量重算（对拍）；
2. **早期截断**（early cutoff）真实生效——重算后输出未变则下游不失效。

若早期截断难以保证正确，退回：引擎降级为「无早期截断的纯记忆化」（记入风险台账）。

## 2. 算法（取自 rustc/Salsa）

- **Revision**：全局单调版本号。修改输入且值确实变化时 +1，并记该输入 `changed_at`（输入级截断）。
- **Memo**：每个派生查询缓存 `{ value, verified_at, changed_at, deps }`。
  - `verified_at`：已确认 green（有效）到哪个 revision。
  - `changed_at`：**值** 上次真正变化于哪个 revision（早期截断的关键量）。
- **取值流程**：
  1. **浅校验**：`verified_at == 当前 revision` → 直接复用。
  2. **深校验**：逐依赖问 `maybe_changed_since(verified_at)`；全否 → 漂绿（`verified_at = 当前`）复用。
  3. **重算 + 早期截断**：任一依赖变了 → 重跑；新值 `==` 旧值则 **不** 更新 `changed_at`
     （下游据此判「没变」被截断），仅更新 `verified_at`。
- **动态依赖**：每次重算用 thread-local 收集栈重新记录依赖，天然支持走了不同分支。
- **依赖记录纪律**：只记 **直接** 读；深校验期间的嵌套读不计入调用者依赖
  （`query` 先记依赖，`verify_value` 校验路径不记）。

## 3. 玩具计算图

「文件字数统计」式流水线（DESIGN §2.5 验收口径最小化）：

```
Input(i)  ── u32 输入 cell
Sign(i)   = Input(i) > 100        ← 早期截断在此：输入在阈值同侧变动 → sign 不变
CountBig  = Σ Sign(i)             ← 依赖全部 sign，受各 sign 早期截断保护
SumAll    = Σ Input(i)
Report    = CountBig*1e6 + SumAll ← 顶层查询
```

## 4. 验证结果

| 项 | 结果 |
|----|------|
| `matches_reference_on_random_workload` | 5000 次交错「随机变更+随机请求」全部与全量重算一致 ✓ |
| `early_cutoff_saves_recompute` | 20 轮阈值同侧变动，查询体执行 <120 次（全量约 1060） ✓ |
| `idempotent_requests_do_not_reexecute` | 无变更的 100 次重复请求零重算（全走浅绿） ✓ |
| demo | 2000 次随机对拍一致；30 轮增量执行 90 次 vs 全量约 3090 次 → **省去 97.1%** |

早期截断的确切表现：每轮改一个「阈值同侧」输入，只需重算该输入的 `Sign`（值不变 →
`changed_at` 不推进）+ `SumAll` + `Report`，共 3 次；`CountBig` 因所有 `Sign` 未变被截断，
从不重算。30 轮 = 90 次，与 demo 输出吻合。

## 5. 结论

单线程红绿 + 早期截断算法 **正确且截断有效**，成为 wake_turbo 正式引擎（Phase 2.5）的算法内核。
**不** 退回「无早期截断纯记忆化」。

Phase 2.5 在此算法之上叠加：`Vc<T>` 句柄 + 分片 slot 表、`#[wake::task]` 宏、generation 取消、
工作窃取并发执行、loom 并发正确性验证。并保留「与全量重算对拍」为常驻测试模式
（`WAKE_VERIFY=1`，DESIGN §13），把本 spike 的对拍思想固化为长期防线。

**降级开关**（DESIGN §13 预案）：正式引擎须提供「无增量纯并行」模式，本 spike 的 `reference()`
全量重算即其语义基准。
