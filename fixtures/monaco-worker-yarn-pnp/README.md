# Monaco 静态 Worker URL / Yarn PnP 回归

此独立 fixture 使用真实的 `monaco-editor@0.57.0`，验证依赖包中的
`new URL('../../common/services/editorWebWorkerMain.js', import.meta.url)`
位于项目根外的 Yarn 缓存时，Wake 能构建并发布正确的资源。

依据 [ADR 0081](../../engineering/decisions/0081-pnp-package-resource-boundaries.md)，
项目根外的 PnP 依赖可以引用自身包内资源，仍检查包归属与真实路径边界。

在本目录运行：

```sh
yarn install --immutable --check-cache
yarn build
yarn reproduce
```

`build` 使用当前仓库源码，通过 `cargo run --locked --offline` 构建 CLI。
首次运行需要已准备好仓库的 Rust 离线依赖和工具链。
预期构建退出码为零。`yarn reproduce` 保留原复现命令名称，现执行成功回归检查：
运行与 `yarn build` 相同的 Cargo 命令，检查资源字节与 Monaco 原文件一致、
bundle 引用已发布文件，且不泄漏 Yarn 缓存路径。

`.yarnrc.yml` 将缓存固定到 `../../.tmp/monaco-worker-yarn-pnp/cache`，
它位于仓库内但在本 fixture 项目根之外；不修改个人全局 Yarn 设置。
`yarn.lock` 将此目录隔离为独立 Yarn 项目，无需加入根 workspace。
入口直接导入 Monaco 的 Worker service 模块，以缩小依赖图；不复制或改写 Monaco 源码。

## 已验证结果

2026-11-12，Windows 上使用仓库 Wake 0.1.48 源码：

- `yarn install --immutable --check-cache` 通过。
- 修复前 `yarn build` 退出码 1，输出以下目标诊断；成功预期检查也以该错误失败。
- 修复后 `yarn build` / `yarn reproduce` 退出码 0，构建 147 个模块并通过资源产物检查。

```text
ERROR [WAKE0304]: cannot build resource URL `../../common/services/editorWebWorkerMain.js`: static resource resolves outside project root <repo>/fixtures/monaco-worker-yarn-pnp
 --> <repo>/.tmp/monaco-worker-yarn-pnp/cache/monaco-editor-<hash>.zip/node_modules/monaco-editor/esm/vs/editor/browser/services/editorWorkerService.js
```

上述历史诊断省略机器绝对路径和缓存哈希。此 fixture 验证构建与资源发布，未验证完整编辑器的浏览器运行。
普通 `new URL` 中的 JavaScript 保持静态资源语义；独立 Worker 打包由 `new Worker(new URL(...))` 触发。
