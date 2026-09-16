# ADR 0061: 显式 lint 全局绑定

- Status: accepted
- Date: 2026-09-13
- Amended by: [ADR 0066](0066-lint-host-environments.md)

## Context

未声明名称规则需要区分源码绑定、标准语言全局与项目提供的宿主全局。执行 lint 的操作系统、
Node 进程或编辑器不能隐式改变结果；CLI、未保存文本、缓存和配置解释必须使用相同的输入。

## Decision

纯 lint 核心拥有版本化标准全局集合与显式全局模式验证，wake_config 只读取声明式
数据，应用层按路径合并根配置、匹配 override 和请求。模式为 readonly、writable、off；off
显式移除已有全局。全局名称必须是 parser 接受的单一未转义标识符，不能包含表达式或注释。

首个集合为冻结的 ES2024 标准全局。浏览器、Node 和 CommonJS 宿主集合另外版本化，不从运行
进程自动探测。显式 globals 可以描述项目注入的名字；类型服务与模块图不由这些名字代替。

配置解释返回全局模式和来源。缓存仅纳入最终有效全局绑定及其模式，不纳入来源或层叠历史；
语义相同的显式默认值复用相同身份，移除标准全局必须失效。规则仍消费同一原生源码值投影，
擦除或未表示的值名称保留不完整状态，不能误报为未定义。

本决策局部扩展 ADR 0052 的有效配置与 ADR 0055 的缓存身份，其余约束保持有效。

## Invariants

- 不读取宿主全局，不执行项目 JavaScript 配置；配置和壳层没有独立的名称解析器或标准表。
- 所有配置层在匹配前验证，包括 off、未匹配 override 和未启用相关规则的情况。
- 根、override、请求顺序和来源在分析、修复、watch、Node、CLI、配置解释中一致。
- 持久化结果的身份包括最终全局配置，不能因沿用旧规则键而命中错误诊断。

## Evidence

`wake_app/src/lint.rs` 的有效配置层叠和 `lint/cache.rs` 的物化规则身份是当前扩展入口。
`wake_app/tests/lint_globals.rs` 覆盖逐名合并、来源和所有层验证；缓存单元测试证明标准全局
移除失效及显式默认值等价。真实 Node addon 覆盖 CJS/ESM、持久上下文和缓存，CLI 测试验证
可重复赋值与参数拒绝。标准集合与名称验证由 `wake_lint_core/tests/globals.rs` 覆盖。

## Consequences

允许项目显式描述运行环境，避免对全局 API 的无根据假设。标准集合的新增需要版本变更，
完整宿主集合、模块解析和原始值语义继续按 LINT.md 的阶段目标验收。

## Validation

先验证核心名称/模式/标准集合、配置合并、未匹配拒绝和缓存隔离的失败测试；再运行聚焦规则、
应用/CLI/Node 一致性、类型检查、架构测试、架构检查及文档门禁。

## Supersedes

None.

## Amends

- [ADR 0052](0052-lint-effective-configuration.md): 新增逐名合并的 globals 及其配置解释字段。
- [ADR 0055](0055-content-addressed-lint-cache.md): 身份纳入最终有效全局名称及模式，不纳入来源。

## Removal plan

不移除已支持配置字段，不引入从宿主进程提取全局的临时路径。
