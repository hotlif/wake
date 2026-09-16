# ADR 0052: lint 有效配置与规则参数所有权

- Status: accepted
- Amended by: [ADR 0061](0061-explicit-lint-global-bindings.md)
- Date: 2026-09-13

## Context

lint 需要让 CLI、Node、后续缓存与编辑器得到相同的规则参数和来源。配置语法由 wake_config
读取，规则身份和参数不能分散到各个前端，也不能通过执行 JavaScript 配置获得。

## Decision

沿用 ADR 0050 的纯核心边界，规则注册表同时拥有封闭参数 schema、参数默认值及版本化预设。
核心接受等级简写或完整 `{ level, options }` 设置，验证所有配置（含 off 和未匹配 override），
并物化包含默认参数的有效规则。应用层拥有按路径匹配、完整设置替换、请求覆盖和来源解释。

`recommended` 布尔值保留既有默认行为；显式 presets 按顺序叠加，再应用项目 rules、匹配
overrides 和请求 rules。简写覆盖会恢复该规则默认参数，不继承先前设置的参数。

`lint({ printConfig: filename })` 与 `wake lint --print-config filename` 调用同一应用入口，
仅加载配置和解释虚拟文件名，不读取目标源码或写入文件。结果包含版本化 schema、语言、
是否被忽略、规则有效等级/参数/来源以及未使用抑制等级。它和分析/修复选择互斥。

## Invariants

- wake_config 只描述数据，不依赖核心；CLI/Node 不持有预设表或参数 schema。
- 未知预设、字段、规则和参数类型必须失败；off 不绕过验证。
- 来源为默认、版本化预设、根配置 rules、按零起点编号的 override 或 request；稳定排序。
- 参数整体替换；简写、空参数和显式默认值物化出相同参数。
- 核心仍无配置文件读取、路径匹配或环境探测；本决策不启用缓存和插件宿主。

## Evidence

`wake_lint_core` 注册表/配置测试、`wake_config/tests/lint.rs`、`wake_app/tests/lint.rs`、
Rust CLI 与真实 Node addon 配置解释用例。

## Consequences

后续缓存可使用物化配置，迁移工具可引用同一注册表验证结果。预设升级必须改版本并验证变化，
不能将新增默认规则隐藏在已有版本内。当前规则参数仍按各条已验证契约提供。

## Validation

配置拒绝与合并测试先红后绿；规则正反例、CLI/Node 一致性、类型检查、架构测试和检查。

## Supersedes

None.

## Removal plan

不引入兼容宿主；保留已支持的等级简写与 recommended 配置。
