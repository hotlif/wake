# ADR 0011: 由产品拥有自动包发布流程

- Status: accepted
- Date: 2026-08-17

## Context

Wake 有两个独立的公共包系列。七个 npm 注册表包由 `.github/workflows/release-npm.yml` 根据
`v<workspace-version>` 标签构建、审计和发布。Crab CSS VS Code 扩展是独立的多平台产品：其工作流
会构建五个特定目标的 VSIX 文件，但只将其保存为临时工作流产物；归档和工作流产物名称还重复包含
当前扩展版本。

仓库需要以可执行方式回答“哪些包会自动发布”，以免新增清单时静默产生未发布的公共包。

## Decision

每个公共产品边缘恰好拥有一个发布工作流：

- `.github/workflows/release-npm.yml` 拥有在 `npm/*` 下发现的所有非私有包，并根据
  `v<workspace-version>` 标签发布；
- `.github/workflows/vscode-css.yml` 拥有五个特定目标的 Crab CSS 扩展包，并根据
  `vscode-css-v<extension-version>` 标签将其发布为 GitHub Release 资源；
- npm 清单和 `editors/vscode-css/package.json` 分别是各自版本的事实来源，发布标签必须与其完全一致；
- `scripts/check-release-coverage.mjs` 从清单发现 npm 发布候选项，并验证两个工作流仍保留其构建、
  审计、目标、凭据和发布契约。

VS Code 工作流将已审计的 VSIX 产物附加到 GitHub Release；发布任务不会重新构建，也不发布到扩展市场。

## Invariants

- 拉取请求、分支推送和手动验证绝不向外部发布。
- npm 与 VSIX 发布标签的命名空间互不相交，扩展标签不能触发 npm 发布。
- 特定平台产物在发布前必须完整且版本一致。
- npm 平台包先于 `@crab-dev/wake` 发布；所有公共 npm 清单都参与发布审计。
- VS Code 扩展在 npm 中标记为 `private`，仅通过 GitHub Releases 分发。
- 发布任务使用预先构建的产物，且只获得实际使用的权限。
- 重新运行已有发布具有幂等性：GitHub Release 资源会被已审计的产物集替换。

## Evidence

- `npm/*/package.json`：当前七个公共 npm 包清单。
- `.github/workflows/release-npm.yml`：原生/JavaScript tarball 构建、不可变审计、有序发布及干净注册表冒烟测试。
- `.github/workflows/vscode-css.yml`：五个目标矩阵、VSIX 检查、来源证明及 GitHub Release 附加。
- `editors/vscode-css/scripts/package-vsix.mjs`：从扩展清单派生归档版本。
- `scripts/check-release-coverage.mjs`：由清单驱动的发布覆盖门禁。

## Consequences

维护者使用 `vX.Y.Z` 发布 npm 包，使用 `vscode-css-vX.Y.Z` 发布扩展。GitHub Actions 使用仓库范围的
`contents: write` 权限创建 Release；无需扩展市场账户、环境或发布密钥。Open VSX 和 VS Code
Marketplace 不属于本契约。

## Validation

- `npm run release:check`
- `npm run versions:check`
- `npm run vscode:css:check`
- 解析每个工作流的 YAML
- 在本地运行特定目标的 `npm run package:vsix` 冒烟测试
- `npm run architecture:check`
- `git diff --check`

## Supersedes

None.

## Removal plan

本变更删除包名和产物名中硬编码的 VSIX 版本，同时删除市场发布、相关凭据和迁移桥接。不保留临时
桥接或重复发布路径。
