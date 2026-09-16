# ADR 0062: 原生调用执行路径事实

- Status: accepted
- Date: 2026-09-14

## Context

ADR 0054 的 completion 摘要服务返回、不可达、穿透与 finally 跳转规则，不提供调用支配关系。
Hooks 需要区分提前返回、短路、可选调用、循环以及函数默认参数，不能由规则扫描关键字猜测。
既有 ADR 0053 已规定绑定与语义所有权；这里为新的调用路径事实定义具体契约。

## Decision

在 wake_ecma_semantic 内按模块、函数/箭头、类初始化区域构建内部执行图，返回 owned 调用范围、
区域身份、可达性、是否位于正常完成路径、是否每条正常完成路径必经、重复执行与语法上下文事实。
普通返回属于正常完成，未捕获 throw 不属于；catch 和 finally 保留自己的异常/跳转语义。
yield 恢复包含 next/throw/return 三种转移，外部 return 仍须执行当前 finally；await 拒绝走异常边。
默认值、短路、可选链与循环条件由原生 AST 决定，嵌套函数不会成为外层函数的执行路径。
finally 可以按待恢复的控制转移构建内部副本，但输出必须按同一源码调用聚合，不能漏掉调用身份。

图与算法不是外部插件 ABI，不持久化、不重写 AST、不替代现有 completion 消费者。lint 按规则
依赖请求调用路径与作用域事实；规则层负责 React 导入/绑定身份、组件/Hook 上下文和诊断。
目录新增组合分析依赖是显式公共目录契约，CLI、Node、配置解释及缓存须保持同一规则身份。

每次源码分析的图节点、边与算法工作量具有明确上限。超限返回分析错误，不能伪造空诊断、继续
以名字猜测或缓存部分结果。限制随 lint 分析身份管理，预算证明覆盖生成的大函数与异常嵌套。

## Invariants

- semantic 仅依赖 common/AST；React、规则 ID、配置、磁盘及 Node 宿主不进入 semantic。
- 调用执行事实和源码绑定身份由原生所有者提供，不从生成代码或源码名称重新构造控制流。
- 函数参数/体、类初始化与嵌套函数边界明确，异常完成不能被当作正常返回。
- 分析不更改普通 analyze 的符号身份、编译 AST 或优化器行为。
- 不完整语义与资源超限不能成为“检查通过”的证据。

## Evidence

`wake_ecma_semantic/tests/call_execution.rs` 八组测试覆盖调用路径、finally 多入口聚合、
生成器恢复和三种预算。`wake_lint_core/tests/hooks.rs` 六组验证符号、所有者、路径及按需分析。
`wake_app/tests/lint.rs` 与 `lint_context.rs` 验证分析失败不写源码/缓存以及新文档版本恢复；
CLI 退出码和 Node/CJS/ESM 错误码均由实际产物测试验证。核心/semantic/app 回归、CLI 11 项、
Node 30 项、公共类型、Clippy、文档与架构门禁通过。此决策只接受调用路径与已实现调用规则，
不代表完整 Hooks 依赖检查或 lint 产品计划已完成。

## Consequences

Hooks 的路径判断具有单一原生所有者，也为后续数据流消费者提供可演进的内部事实。分析按需
执行并保留错误传播，代价是增加图构建与生命周期验证；不引入 ESLint 插件运行时。

## Validation

先运行调用路径最小失败测试，覆盖分支、提前返回/抛出、所有循环与标签、switch、try/catch/finally、
短路/可选链、解构默认值、函数/类区域、不可达及预算边界。再通过 Hooks 正反例、作用域遮蔽、
原始 TS/JSX、Node/CLI/缓存/上下文和编译等价回归，运行 Clippy、公开类型及架构门禁。

## Supersedes

None.

## Removal plan

无兼容桥。现有 completion 分析继续拥有原有四条控制流规则的事实；内部图只有经过相同契约
验证后才可替换算法，不将候选设计写入当前机器策略。
