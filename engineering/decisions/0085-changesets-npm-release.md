# ADR 0085: 由 Changesets 管理 Wake npm 版本

- Status: accepted
- Date: 2026-11-18

## Context

主包精确依赖 CSS 和五个平台包，Rust workspace 与 npm 共用一个版本。过去维护者手工同步清单、锁文件并推送版本标签。

## Decision

贡献者为用户可见的变更提交 changeset。通过 CI 的 `canary` 推送驱动 Changesets 创建或更新 Release PR。八个公共 npm 包组成一个固定版本组，Release PR 同步 Cargo、Yarn 和变更日志。合并后，发布工作流从通过 CI 的提交构建、审计和发布，成功发布后记录 `v<version>` 标签。

## Invariants

- 只有本仓库 `canary` 的成功 push CI 可创建版本 PR 或启动 npm 发布。写入前复核已验证提交仍是分支头。手动运行只验证产物。
- 发布的版本来自已合并的 Changesets PR。平台包、CSS 和 SDK 在主包之前发布。重试只接受注册表中与审计产物 SHA-512 完全相同的已发布版本。
- VS Code 扩展继续由独立清单、标签和工作流发布。

## Evidence

`.changeset/config.json` 定义版本组。`.github/workflows/release-npm.yml` 连接 CI、版本 PR、产物矩阵和 npm 环境。

## Consequences

维护者通过合并 Release PR 批准升版。无待发版本的推送跳过发布矩阵。首次启用时已有版本标签仍可用于识别已处理的版本。

## Validation

`corepack yarn release:check`、`corepack yarn versions:check`、`corepack yarn architecture:test`、`corepack yarn architecture:check` 以及隔离副本中的 changeset 升版演练。

## Supersedes

None.

## Amends

- [ADR 0011](0011-release-automation.md): npm 版本生成、发布触发、重试和版本标签的所有权。

## Removal plan

移除 npm 标签推送触发器，由同一工作流继续消费已审计的 tarball。
