# ADR 0070: 构建归属、诊断上下文与步骤统计

- Status: accepted
- Date: 2026-09-19

## Context

ADR 0069 的共享活动会话不能区分交错构建，也不能解释大量短任务的累计耗时。编译任务运行于
共享 worker，持久的 Turbo recomputer 不应持有上一轮观察状态。

## Decision

产品执行边界创建独立构建上下文；内部阶段和模块继承构建编号与父操作，独立请求始终创建新编号。
`wake_common::progress` 拥有可克隆的上下文与线程内 RAII 进入/恢复作用域，执行器在提交任务时
捕获、在调用任务时进入。上下文只随短期工作传播，不写入任务输入、recomputer 或缓存。

所有活动构建共享一个采样线程，但分别持有活动、固定步骤分组统计及最慢十个模块操作。每轮结束
独立输出终端汇总；全部结束后停止采样。构建墙钟时长与包含子步骤的累计时间分别呈现。

## Invariants

- 重叠构建及同一 worker 后续任务不共享编号、父操作或统计；作用域异常退出恢复此前上下文。
- 默认关闭时不构造详情、上下文注册、计时或采样线程；不改变任务调度顺序与结果。
- 分组键仅为操作类别与静态步骤/pass 名；路径与迭代轮次作为详情，不按模块累积无界历史。
- 观察围绕真实执行边界；联合优化不虚构独立时间，步骤退出不表示成功。
- 每轮清理与其他活动构建独立；采样线程不拥有构建生命周期，避免引用环和自 join。
- 不新增构建选项、序列化结果、机器协议、报告文件或缓存身份字段。

## Evidence

- `crates/wake_common/src/progress.rs`
- `crates/wake_turbo/src/executor.rs`
- `crates/wake_bundler/tests/progress.rs`
- `crates/wake_ecma_minify/src/typed_pipeline.rs`
- `crates/wake_cli/tests/cli_output.rs`
- `crates/wake_app/src/lib.rs`、`crates/wake_dev_server/src/lib.rs`、`crates/wake_docs/src/lib.rs`

## Consequences

细粒度观察只在显式诊断时产生开销。并行累计时间可能超过墙钟总时长，各层统计不能相加；
固定统计组和有界慢模块榜不会保存完整事件历史。本轮仍保留同步 stderr，不提供 CPU 或线程栈采样。

## Validation

- `cargo test -p wake_common -p wake_ecma_minify -p wake_docs -p wake_app progress`：可控时钟、
  作用域恢复、禁用零登记、阻塞优化、Docs 路径和取消清理。
- `cargo test -p wake_bundler --test progress`：共享 worker 上交错构建、后续重建、冷/内存/磁盘缓存一致性。
- `cargo test -p wake_cli --test cli_output`；用 `corepack yarn native:build` 重新构建绑定后执行
  `npm/wake/test/cli.test.mjs`。
- 受影响 crate 的完整测试、Clippy；`cargo fmt --all --check`、`corepack yarn architecture:test`、
  `corepack yarn architecture:check`、`node scripts/check-docs.mjs`。

## Supersedes

None.

## Amends

- [ADR 0069](0069-live-build-progress-diagnostics.md): 仅修订构建关联、统计内存和采样生命周期；重叠构建分别登记，保存固定步骤统计与各自的 Top 10，各自最后的执行上下文退出时汇总，全部退出后停止共享采样线程。其余开关、输出与非干预契约继续有效。

## Removal plan

移除基于进程共享活动会话推断构建归属的路径；保留开关、stderr 和 WAKE_TIMING。
无过渡格式或持久化迁移；原共享会话路径已由构建上下文替换。
