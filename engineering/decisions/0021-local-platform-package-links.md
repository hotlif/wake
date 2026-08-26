# ADR 0021: 用根级本地链接拥有平台 optional 包

- Status: superseded
- Date: 2026-08-26
- Superseded by: [ADR 0022](0022-yarn-pnp-ownership.md)

## Context

`@crab-dev/wake` 以精确版本 optionalDependencies 引用五个平台原生包。版本准备发生在这些包发布之前，
npm 11 因而曾在 `package-lock.json` 中为它们写入只有 `optional: true`、没有版本的嵌套占位项。该锁文件
在包尚不存在时可以通过部分安装路径；同版本平台包发布后，npm 11.17 的 `npm ci` 会解析空版本并报
`Invalid Version:`，使所有依赖 JavaScript 安装的 CI 作业同时失败。

把五个平台目录直接列为 npm workspaces 不能解决问题：npm 会在当前主机上对其他四个平台清单执行
`os`/`cpu` 检查并以 `EBADPLATFORM` 失败。锁中保留 registry 元数据也不能支持下一次尚未发布的版本。

## Decision

根 `wake-workspace` 清单以 optional `file:npm/wake-<target>` 依赖拥有五个内部平台目录。根清单是私有的，
这些定位符只定义仓库安装图，不进入公共 tarball。公开 `@crab-dev/wake` 清单继续以与平台清单一致的精确
版本引用 registry 包，因此外部消费者的安装契约不变。

`package-lock.json` 必须把每个平台包记录为根 `node_modules` 下指向相应内部目录的 link，并保存该目录
清单的精确名称、版本和 optional 状态。无版本的嵌套预发布占位项不再是合法锁形状。根清单中的这五个
受策略拥有的 `file:` optional 定位符是 registry-only 来源规则的唯一例外。

`scripts/check-architecture.mjs` 拥有锁图和来源不变量，并由 `scripts/check-npm-lock.mjs` 在 clean install
之前复用；`scripts/check-versions.mjs` 拥有版本与目录定位符同步。机器策略通过
`internalOptionalPackages` 列出唯一允许的名称到目录映射。CI 的 npm 作业依赖单一 lock preflight，
发布与 VS Code 验证也在各自首次 `npm ci` 前运行同一门禁。

## Invariants

- `@crab-dev/wake` 的五个平台 optional 版本与对应平台清单版本完全一致。
- 根私有清单恰好以 `file:` optional 定位符链接这五个平台目录；其他非 registry 定位符仍被拒绝。
- 每个根 lock link 的目标、内部 lock 清单名称、版本和 optional 状态与源码清单一致。
- `package-lock.json` 中任何非 link 的 `node_modules` 条目都必须具有精确 SemVer、官方 registry tarball
  和 SHA-512 integrity；无版本 optional 占位项一律失败。
- 平台目录不成为 npm workspaces，避免跨平台安装触发 `EBADPLATFORM`。
- 公共包清单和发布顺序保持由 [ADR 0011](0011-release-automation.md) 所有。

## Evidence

- GitHub CI `32881578128` 的 17 个作业在 npm 11.17.0 上以 `Invalid Version:` 失败。
- 旧锁文件包含五个仅有 `optional: true` 的 `npm/wake/node_modules/@crab-dev/wake-*` 条目。
- 把平台目录加入 workspaces 的实验在 Windows x64 上以 macOS ARM64 包 `EBADPLATFORM` 失败。
- 根 optional `file:` link 锁图通过 npm 11.17.0 的 Windows x64、Linux x64/ARM64 和 macOS
  x64/ARM64 `npm ci --dry-run` 平台矩阵。

## Consequences

源码安装不再依赖目标版本是否已在 npm registry 发布，版本准备和发布后 CI 使用同一锁图。当前平台的
根安装会链接仓库内平台目录；需要加载原生扩展的门禁仍必须先构建并通过既有 `WAKE_NATIVE_PATH` 或
发布 tarball 流程选择经过验证的二进制。

根清单新增五个内部 optional 定位符，但公开 `@crab-dev/wake` 及平台包的依赖、导出和 tarball 内容均
不变。

## Validation

- `npm run architecture:test`
- `npm run architecture:check`
- `npm run npm:lock:check`
- `npm run versions:check`
- npm 11.17.0 对五个目标执行 `npm ci --ignore-scripts --dry-run`
- Node 测试、TypeScript 检查和 `git diff --check`

## Supersedes

None.

本决策已由 [ADR 0022](0022-yarn-pnp-ownership.md) 替代；根 `file:` bridge 与 npm lock 已删除。

## Removal plan

删除 `internalOptionalPlaceholders` 策略、五个无版本嵌套锁条目及相关允许逻辑。原子切换到
`internalOptionalPackages` 和根级本地 links；不保留兼容开关或第二套锁图。
