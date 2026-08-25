# ADR 0002: 分离 Wake Docs 运行时界面并强制执行内容契约

- Status: accepted
- Date: 2026-08-14

## Context

Wake Docs 通过同一个生成器提供两个产品：公共文档站点和组件工作台。此前生成的入口会无条件导入这两个产品，因此普通文档构建也会打包组件浏览器和所有已安装的 `@crab-dev/rc-*` 包。导航展开状态还混合了路由派生状态和用户偏好；内容检查则只要求存在标题，导致配置参考页可能逐渐偏离 `wake_config`。

作出本决策前，公共站点构建包含 2,306 个模块、约 2.60 MB JavaScript 和 179 KB CSS。浏览器检查还发现，访问页面会不断累积持久化的展开分区。

## Decision

生成特定于模式的运行时入口。站点模式只拥有文档应用及其基础样式；组件模式拥有组件工作台、组件状态和组件样式。共享代码仍放在 `app.tsx` 中，但两种模式互不导入对方的产品界面。

将当前导航分区视为路由派生状态。只持久化用户明确切换过的分区。分组概览页直接位于其导航分组下，不再重复作为子分区的第一页。

在 `scripts/check-docs.mjs` 中把页面 `kind` 变为可执行的内容契约。教程必须包含目标结果、可运行代码、验证、常见错误和下一步；指南必须包含验证或测量章节以及下一步；概览必须包含主要任务入口和多个面向任务的章节。

以公共 Rust 配置结构体作为参考文档覆盖范围的事实来源。文档检查从 `wake_config` 中提取公共字段；若指定的参考页遗漏任何字段，检查即失败。

## Invariants

- 站点模式不得生成或导入组件工作台运行时。
- 组件模式必须包含工作台及其状态和样式资源。
- 当前分区保持展开，但不写入用户的展开偏好。
- 导航顺序和层级只能来自 `docs/navigation.toml`。
- 页面种类决定其最低证据要求和完整结构。
- 每个公共 `wake_config` 字段都出现在唯一的权威配置参考页中。
- 非法配置必须以诊断失败；只有文件缺失时才使用默认值。

## Evidence

- `crates/wake_docs/runtime/site-entry.tsx`
- `crates/wake_docs/runtime/components-entry.tsx`
- `crates/wake_docs/src/lib.rs`
- `crates/wake_docs/runtime/app.tsx`
- `scripts/check-docs.mjs`
- `docs/reference/configuration/`
- 实现任务中记录的生产构建输出和浏览器检查。

## Consequences

两个运行时产品可以独立演进，公共文档不再承担组件目录的成本。新增页面或公共配置字段时，现在必须通过明确的检查。内容作者会更早收到失败反馈，代价是必须维持教程和指南的结构完整。

生成的入口文件名属于内部实现细节，会随模式变化。公共 Wake Docs 命令、路由、Frontmatter 和 MDX API 均不变。

## Validation

- `cargo test -p wake_docs`
- `npm run docs:check`
- `npm run docs:build`
- 检查站点构建清单和资源大小；任何 `@crab-dev/rc-*` 包都不得进入站点模式。
- 在真实浏览器中浏览多个分区，确认只有显式切换会持久化。
- 验证桌面端和移动端布局、深层链接、搜索及浏览器控制台。

## Supersedes

None.

## Removal plan

立即删除共享的 `runtime/entry.tsx` 路径。不保留兼容包装器、浏览器内双入口选择或旧版展开状态存储键。
