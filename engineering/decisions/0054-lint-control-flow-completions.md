# ADR 0054: lint 控制流 completion 事实

- Status: accepted
- Date: 2026-09-13

## Context

返回一致性、不可达语句、switch 穿透和 finally 跳转必须共享对分支、循环、标签与异常完成方式的
理解。规则各自遍历关键字会错误处理嵌套函数、break/continue 的目标及 finally 覆盖行为。

## Decision

在既有 semantic 所有者中提供纯控制流 completion 分析，借用 AST，返回 owned 源码范围和事实。
按函数/模块/静态块独立分析 Normal、ReturnValue、ReturnVoid、Throw、Break 和 Continue；序列
仅从 Normal 继续，分支合并，循环和标签消费自己的跳转，finally 的 abrupt completion 覆盖进入值。
异常路径保守保留，不把调用推断为不会抛出，也不推断类型或跨函数执行。

lint 注册表增加按需控制流依赖；核心消费同一分析入口，不在 CLI/Node 重复分析。原始函数体身份
由 parser 记录，用于排除 TS namespace 等合成函数的返回诊断。本次不冻结外部 CFG/插件 ABI，
也不将 completion 摘要当成已经支持 Hooks 支配关系或类型流细化的完整图。

## Invariants

- semantic 仍只依赖 common/AST，parser 不导入 semantic。
- 函数边界隔离 completion；类方法与静态块不向外层函数贡献 return。
- finally 中已由内部循环/标签/catch 消费的跳转不视为逃逸；逃逸 completion 保留原始范围。
- 诊断与配置、修复和抑制仍归 lint；semantic 不拥有规则 ID 或文件 I/O。

## Evidence

`wake_ecma_semantic/tests/control_flow.rs`、`wake_lint_core/tests/control_flow.rs` 及 parser 的
原始函数体身份测试。

## Consequences

四条控制流规则共用结构分析，后续完整图可扩展同一所有者。数据流、模块/类型关系及 Hooks 分析
仍须单独实现和验证。

## Validation

最小失败测试先行，覆盖循环/标签、try/catch/finally、嵌套函数和 TS 合成节点；运行 semantic、
parser、lint 与编译器回归、Clippy 和架构门禁。

## Supersedes

None.

## Removal plan

无兼容桥或重复控制流所有者。
