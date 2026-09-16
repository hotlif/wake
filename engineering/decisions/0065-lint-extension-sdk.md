# ADR 0065: Wake lint 独立扩展 SDK

- Status: accepted
- Date: 2026-09-14

## Context

Wake 原生规则必须保持在 Rust 核心中，并且不能把 parser arena、编译 AST 或 TypeScript
临时句柄暴露给第三方。P6 仍需要一个可独立安装、测试和升级的扩展包，让规则作者可以在
受限边界内消费源码事实并返回诊断。该能力不要求执行 ESLint 配置，也不承诺兼容 ESLint
插件 ABI。

## Decision

新增 `@crab-dev/wake-lint-sdk`，协议版本固定为 `wake.lint.extension.v1`。SDK 只暴露冻结的
源码快照、路径、语言、可选的 JSON 语法事实、规则选项和 UTF-8 字节编辑；规则通过显式
`report` 返回诊断。加载、规则执行和编辑验证失败均被包边界转换为扩展错误，不污染宿主
lint 结果。规则 ID、消息、级别和修复能力在包加载时闭合校验。

SDK 不启动进程、不执行配置文件、不持有 Wake 内部对象。协议版本和 SDK 主版本不匹配时
拒绝加载；升级由包作者发布新版本并由兼容性检查显式确认。宿主是否启用某个扩展仍属于
后续产品入口，不改变内建规则的默认结果。

## Invariants

- 扩展只能消费冻结快照和公开协议值，不能取得 parser arena、编译 AST 或类型服务句柄。
- 扩展加载、规则执行和编辑验证失败必须隔离在扩展错误中，不得发布部分扩展诊断或源码写入。
- 协议主版本、SDK 主版本、规则 ID 和消息契约必须显式匹配，未知字段不能静默接受。
- 扩展不会执行 Wake 配置、启动子进程或改变内建规则的默认启用集合。

## Evidence

`npm/wake-lint-sdk/index.mjs` 和 `index.d.ts` 定义冻结的扩展边界；
`npm/wake-lint-sdk/test/index.test.mjs` 覆盖安装导出、规则 manifest、UTF-8 编辑边界、
异常隔离、确定性诊断排序和协议/版本拒绝。

## Consequences

规则作者可以独立发布和测试扩展，不需要依赖 Wake 内部 Rust 类型；扩展不能直接复用 ESLint
插件 ABI，宿主自动发现、配置迁移和第三方规则启用仍需另行定义。协议升级需要同步发布新 SDK
主版本和兼容性检查。

## Validation

- 包安装导出、规则/插件 manifest、UTF-8 边界和冻结输入有单测。
- 规则抛异常时只产生 `WAKE_LINT_EXTENSION`，后续规则可继续运行。
- 相同快照和规则产生稳定排序；修复编辑必须在源快照的字符边界内。
- 协议主版本升级、未知字段和不可兼容 SDK 主版本均拒绝。

## Supersedes

None.

## Removal plan

不移除当前 v1 协议；后续不兼容变更通过新增协议版本和 ADR 迁移，保留旧版本解析直到兼容
窗口结束。
