# ADR 0066: lint 宿主环境集合与版本身份

- Status: accepted
- Date: 2026-09-14

## Context

ADR 0061 已确定宿主全局不能从执行进程隐式探测，浏览器和 Node 集合需要独立版本化。
本轮实现把这项边界接入配置、应用、CLI 和 Node API；因此需要明确集合的所有权、选择方式、
缓存身份和解释结果，避免壳层各自维护一份全局表。

## Decision

`wake_lint_core` 拥有冻结的 `browser@1` 和 `node@1` 全局集合，并负责环境名称验证、确定性
排序和有效绑定生成。`wake_config` 只保存声明式的 `environments` 数组；应用、CLI 和 Node
绑定只负责传递请求，不能扩展或重写集合。未知环境在配置加载和请求入口均失败关闭。

环境集合先合并，再应用逐名 `globals`；显式 `off`、`readonly` 或 `writable` 始终覆盖环境
提供的同名绑定。配置解释输出每个绑定的来源以及最终环境数组，环境来源带集合版本；缓存身份
只使用最终有效绑定和版本化语义，不使用层叠来源文本。宿主进程、操作系统和编辑器运行时不
参与环境选择。

## Invariants

- 当前只允许 `browser` 和 `node`；集合版本改变必须更新核心版本和缓存身份。
- 所有入口使用同一个核心验证器和同一份集合；无规则配置也必须拒绝未知环境。
- 根配置、override、请求参数和 Node API 的环境合并顺序确定，显式 globals 最后生效。
- 相同最终绑定产生相同缓存身份；环境来源变化但最终绑定不变时不改变身份。
- `explain`、`print-config`、CLI 和 Node API 不能报告与实际规则执行不同的有效环境。

## Evidence

核心集合、版本和失败关闭行为由 `crates/wake_lint_core/tests/globals.rs` 覆盖；应用层配置
解释、层级覆盖顺序和规则执行由 `crates/wake_app/tests/lint.rs` 覆盖；CLI 参数冲突和入口验证由
`crates/wake_cli/tests/lint.rs` 覆盖；Node API 契约由 `npm/wake/test/api.test.mjs` 覆盖。
真实项目 smoke 矩阵同时验证 browser/Node 配置与 npm、workspace、PnP 项目根；
`crates/wake_app/src/lint/cache.rs` 验证不同环境集合不会复用错误缓存身份。

## Consequences

项目可以明确声明浏览器或 Node 运行环境，`no-undef` 等规则不会依赖当前机器。新增或修改
宿主 API 需要更新冻结集合、版本、缓存契约和兼容文档；CommonJS、Deno 等未列出的环境必须
通过显式 globals 或后续独立 ADR 接入。

## Validation

运行核心、应用、CLI、Node 的聚焦测试，`cargo fmt --all --check`、相关 clippy、文档门禁和
真实项目 smoke。架构检查还必须确认本 ADR 已进入索引，并与 ADR 0061 的 `Amends` 关系闭合。

## Supersedes

None.

## Amends

- [ADR 0061](0061-explicit-lint-global-bindings.md): 明确 browser/node 宿主集合的所有权、版本和配置/缓存身份边界。

## Removal plan

不移除 `environments` 配置字段；若未来替换集合协议，新增 ADR 并保留旧版本解析和缓存失效
规则，直到兼容窗口结束。
