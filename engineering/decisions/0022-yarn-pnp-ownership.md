# ADR 0022: Yarn PnP 权威与 npm 经典解析兼容

- Status: accepted
- Date: 2026-08-26

## Context

Wake 同时维护产品解析器、根 npm lock、编辑器独立 npm lock，以及 Components 私有 PnP fallback。
这些事实来源会在损坏清单、未声明依赖、watch 失效和 zip 压缩场景产生不同结果。尤其是损坏的
`.pnp.cjs` 曾静默命中 `node_modules`，而 Components fallback 和裸包 alias 又能覆盖 Yarn 的拒绝。
源码构建脚本还直接读取根 `node_modules`，因此仓库自身没有持续验证无物理安装树的产品契约。

Yarn PnP specification 和 Yarn 4.16 `pnpapi.resolveToUnqualified` 已定义 locator、依赖可见性、fallback、
ignore pattern、virtual package 与 zip 路径语义。Wake 不应在这些边界建立第二套选择规则。

## Decision

Wake 源码仓库以 Yarn 4.16 PnP 作为唯一 JavaScript 安装图；Wake 产品则同时支持 Yarn PnP 与
npm/经典 `node_modules` 用户项目。产品侧由 `wake_resolver::ResolutionEnvironment` 统一拥有基础
文件系统、PnP/zip 文件系统、按 issuer 发现的 registry、Resolver 和失效缓存，不提供需要用户选择的
解析模式开关。

`.pnp.cjs` 是 PnP 唯一激活标志；非内联 loader 读取其匹配的 `.pnp.data.json`，损坏或不支持的数据
返回结构化 PnP 诊断且不得回退。没有 `.pnp.cjs` 时，已安装的 `node_modules` 文件树与包内
`package.json` 是经典解析输入；`package-lock.json` 只表示 npm 安装可复现性和结构变更，不作为
Wake 的第二套运行时依赖求解图读取。

对有效 npm 裸包，受清单管理时 Yarn 成功或拒绝都是最终结果，并先于 Wake `resolve.alias`。命中
`ignorePatternData` 或不属于依赖树的 issuer 按 Yarn 指示执行经典 Node 解析；没有 PnP 根时保留普通
Wake alias 与 `node_modules` 行为。`@/`、`@@/`、`@@@/`、`@@wake/docs` 等非 npm 包名的 Wake
路径或私有生成 alias 不受影响。
Components 不再拥有 resolver fallback；旧发布包缺失的 CSS/Lucide 元数据由根 `.yarnrc.yml` 的
`packageExtensions` 显式声明。

源码仓库固定 `packageManager: yarn@4.16.0` 与 Corepack 0.34.6，根 `yarn.lock`、`.yarnrc.yml` 和 PnP
registry 是唯一安装图。所有 npm 包与 VS Code 编辑器均为根 workspace；不保留根 `file:` 平台包桥或
编辑器独立 lock。源码构建只读取现有 PnP 安装并通过 `pnpapi` 定位依赖，绝不联网。

## Invariants

- `.pnp.cjs` 缺失表示未启用 PnP；存在但损坏时产生包含清单路径和原因的最终错误。
- 没有 PnP 根时，npm workspace、嵌套/作用域包、subpath 与条件 exports 按经典 Node 文件树解析；
  缺失依赖返回 `NotFound`，不会产生 PnP 诊断。
- 有效裸包在受管理 issuer 中由 Yarn locator、fallback、peer/virtual/unplugged 与 ignore 语义决定。
- PnP 拒绝不能被 Wake alias、Components 桥或现存 `node_modules` 覆盖。
- zip 文件系统支持 Yarn 生成的 STORE 与 DEFLATE，并显式拒绝其他压缩算法。
- `.pnp.cjs`、`.pnp.data.json`、`yarn.lock` 任一变化都会清除清单、成功/失败解析和模块拓扑缓存。
- `package-lock.json` 变化会清除经典解析的成功/失败缓存和模块拓扑，但其内容不会被运行时解析。
- Bundler、Library/Token、Wake Test 与 CSS LSP 只通过 `ResolutionEnvironment` 访问 PnP 状态并传播诊断。
- 仓库只有一个 `yarn.lock`；工作区 locator、精确 npm resolution 与 checksum 由官方 `parseSyml` 校验。
- 安装使用 `yarn install --immutable --check-cache`，依赖 lifecycle scripts 禁用。

## Evidence

- `crates/wake_resolver/src/environment.rs` 与 `crates/wake_resolver/src/lib.rs`
- `crates/wake_common/src/zip.rs` 与 `crates/wake_resolver/src/pnpfs.rs`
- `scripts/check-yarn-lock.mjs`、`scripts/check-pnp-conformance.mjs`
- `.yarnrc.yml`、`yarn.lock` 与根 workspaces
- `crates/wake_js_runtime/build.rs`、`scripts/resolve-embedded-packages.mjs`
- Bundler、Library/Token、Wake Test、CSS LSP 的 PnP 回归测试
- `scripts/check-npm-consumer.mjs` 与 CI 的 npm artifact/consumer 矩阵

## Consequences

PnP 项目的未声明依赖会稳定显示 Yarn/PnP 诊断，损坏安装不会被物理目录掩盖；npm 项目继续使用物理
安装树并在缺失时返回普通解析诊断。两种项目的 lock/安装变化都会在下一 generation 重建模块拓扑。
仓库安装、编辑器和维护脚本统一使用 Yarn PnP，Windows 与 Linux 的干净 checkout 不再需要源码
`node_modules`。

发布 tarball 对 npm 与 Yarn PnP 消费项目均是一等兼容合同。普通 PR 在仓库外以本轮本地 tarball 生成
`package-lock.json` 并执行 `npm ci`；发布矩阵继续覆盖全部平台。Yarn 4 zip/virtual 路径仍属于产品输入，
不会泄露为公开路径格式承诺。

## Validation

- `yarn install --immutable --check-cache`
- `yarn yarn:lock:check`、`yarn pnp:conformance:check`、`yarn pnp:components:check`
- `cargo test -p wake_resolver -p wake_common` 及 Bundler、Library/Token、Wake Test、CSS LSP 聚焦测试
- `yarn architecture:test`、`yarn architecture:check`、`yarn release:check`
- `yarn npm:typecheck`、Docs、VSIX、npm pack 和独立 npm/Yarn PnP 消费测试
- Windows/Linux × Node 22.14/26 的仓库外 `npm ci`、CLI、Node API、workspace、CSS 与 Wake Test 门禁
- `git diff --check`

## Supersedes

- [ADR 0021](0021-local-platform-package-links.md) 的根 npm lock 与 `file:` 平台包链接策略。
- [ADR 0020](0020-react-browser-test-runtime.md) 中以 npm lock 拥有 JavaScript 源码依赖的部分。
- [ADR 0006](0006-crab-css-public-contract.md) 中 Components 私有 PnP fallback 的临时部分。

## Removal plan

原子删除根及编辑器 `package-lock.json`、`scripts/check-npm-lock.mjs`、根 `file:` optional bridge、
`PnpDependencyFallback` 和所有消费者接线。源码 CI 与维护脚本同批切换到 Yarn/PnP，不保留双 lock、
环境开关或静默回退路径；npm consumer 产生的临时 lock 只存在于仓库外测试项目。未来替换源码安装器或
任一产品解析后端时，必须由新的 accepted ADR 明确替换对应所有权。
