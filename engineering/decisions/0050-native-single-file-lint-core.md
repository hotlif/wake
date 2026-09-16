# ADR 0050: 原生单文件 lint 核心

- Status: accepted
- Date: 2026-09-13
- Amended by: [ADR 0051](0051-lint-source-fix-transactions.md)
- Amended by: [ADR 0053](0053-source-semantic-facts-for-lint.md)
- Amended by: [ADR 0056](0056-lint-suppression-baselines.md)

## Context

ADR 0049 的完整产品仍处于 proposed。parser 已能在同一 pass 暴露源码注释、JSX 原始范围和部分
TS 范围；首批 JS 规则可消费普通编译 AST，React 属性规则必须消费原始 JSX 范围。需要一个纯 CPU
入口验证规则、等级、诊断和抑制，不让文件、CLI 或编辑器各建一套规则实现。

## Decision

建立不发布的 `wake_lint_core`。它只依赖 common、ECMA AST/parser 和序列化依赖，借用源码并返回
owned 结果；每次分析创建自己的 Interner，不把 Atom/arena/节点引用带出结果。它拥有注册表、规则
等级与无效规则配置校验、诊断排序、源注释抑制，以及 parser 错误到 lint 结果的转换。

首批只实现 [LINT.md](../LINT.md) 的单文件规则切片。parser 有 error 时返回原始诊断并跳过所有
规则。`off` 仍校验规则 ID；未启用规则不产出结果。无磁盘、配置发现、插件执行、缓存或写入。
后续产品入口通过应用层调用此核心；本决策不接受 ADR 0049 尚未实现的产品/类型/插件边界。

## Invariants

- `wake_lint_core` 不能依赖配置、resolver、bundler、app、CLI、Node、LSP 或 JS 运行时。
- 返回值持有字符串/字节位置，不持有 AST、Atom 或调用者源码引用。
- 每次只解析一次；规则不能重扫源码识别注释或从 helper 反推 JSX 属性。
- 语法错误不能被 lint-disable 抑制；结果按范围、规则、消息稳定排序。
- 单文件源码分析不宣称项目解析、类型检查、文件缓存、修复写入或完整 ESLint 兼容。

## Evidence

- `crates/wake_ecma_parser/src/source.rs`：源注释、JSX 和 TS 范围采集。
- `crates/wake_lint_core/src/lib.rs`、`tests/rules.rs`、`tests/jsx.rs`：六条首批 JS 规则与四条可选
  JSX 规则、等级、owned 结果和抑制测试。
- `scripts/check-architecture.test.mjs`、`engineering/architecture-boundaries.json`：允许边界和越界负例。

## Consequences

为后续 CLI/Node/LSP 提供共享规则入口。首批规则只承诺各自已验证的参数和语言范围；完整 TS
语法、控制流、模块图和类型服务继续受 ADR 0049 的阶段清单约束。

## Validation

- 规则测试先因缺少入口/目标行为失败，再运行 `cargo test -p wake_lint_core`。
- 正反例覆盖嵌套作用域、literal/JSX 伪指令、等级、禁用区间、Unicode、语法错误与重复键。
- `cargo clippy -p wake_lint_core --all-targets -- -D warnings`。
- `corepack yarn architecture:test`、`corepack yarn architecture:check`。

## Supersedes

None.

## Removal plan

没有旧 lint 实现或兼容桥。后续扩展在这个入口增加能力，不另建壳层规则引擎。
