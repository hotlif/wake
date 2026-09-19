# ADR 0069: 原生构建活动与卡顿诊断

- Status: accepted
- Date: 2026-09-19
- Amended by: [ADR 0070](0070-build-scoped-progress-context.md)

## Context

`WAKE_TIMING` 在阶段完成后汇总，无法观察尚未返回的读取、编译或发布步骤。Rust 与 npm
入口共享原生构建栈，构建 worker 的活动必须在其阻塞期间仍可读取。

## Decision

`wake_common::progress` 拥有仅用于诊断的进程内活动登记、RAII 生命周期和 stderr 文本输出。
产品与 bundler 在实际执行边界登记阶段和模块任务；不向任务输入、缓存身份或构建产物添加观察数据。
一个活动会话由仍存活的 guard 共同持有，以独立线程每秒采样；最后一个 guard 退出时停止并 join
采样线程，输出有界的慢任务记录。重叠构建共享会话、各操作有独立编号。

CLI 的 `--progress` 或进程启动时的 `WAKE_PROGRESS=1` 显式启用；CLI 选择 plain 呈现，避免
原生日志破坏备用屏幕。保留现有 `WAKE_TIMING`。基础层文本仅为显式启用的调试日志，终端交互、
输入和产品成功/失败呈现仍属于壳层，不引入进度回调或跨进程机器协议。

## Invariants

- 默认关闭；关闭时不分配活动详情、不创建线程。
- 原生 worker 不依赖 Node 事件循环或构建驱动线程来输出心跳。
- 每个并行任务独立登记；内存仅保存活动任务及最慢十条已结束任务。
- 不在持有活动登记锁时执行被观察的工作；日志 I/O 失败不改变构建结果。
- guard 退出、错误返回和 unwind 清理登记；最后退出停止采样，空闲 watch 无残留心跳。
- 不虚构百分比或成功状态；嵌套/并行耗时不相加，控制字符在单行日志中转义。
- 观察不进入缓存或 Turbo 指纹，不改变依赖、发布事务、产物与取消语义。

## Evidence

- `crates/wake_common/src/progress.rs`
- `crates/wake_bundler/tests/progress.rs`
- `crates/wake_cli/tests/cli_output.rs`
- `npm/wake/test/cli.test.mjs`
- `docs/reference/cli/build.mdx`

## Consequences

显式诊断会增加同步和日志成本。共享原生实现让 Rust、npm、Node API 和重建得到相同观察。
日志可定位到已登记操作，不能替代线程栈采样；完整进程或 OS 调度停顿可能同时阻止心跳。

## Validation

- `cargo test -p wake_common progress`
- `cargo test -p wake_bundler --test progress`
- `cargo test -p wake_cli --test cli_output`
- 用新原生绑定执行 `npm/wake/test/cli.test.mjs`。
- `corepack yarn architecture:test` 和 `corepack yarn architecture:check`。

## Supersedes

None.

## Removal plan

无废弃路径或兼容桥。未来结构化 profiler 应另行定义公开事件协议。
