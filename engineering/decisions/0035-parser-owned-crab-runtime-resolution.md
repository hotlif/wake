# ADR 0035: Parser-owned Crab runtime resolution

- Status: accepted
- Date: 2026-09-02

## Context

`@crab-dev/css` 是 Wake 唯一公共 CSS-in-JS 契约，但仍受支持的一部分不可变
`@crab-dev/rc-*` 发布归档在公共 ESM/CJS 入口请求 `@linaria/core` 的 `cx` runtime。旧兼容路径在
loader 读取源码后对单双引号字符串执行 `.replace`。这会让 loader 同时拥有依赖识别、源码迁移和
样式发现，并会误改普通字符串；模板、注释、正则、import attributes 等同值文本也没有可靠的语义
边界。改写后的源码还让 parser、缓存、诊断和 source map 丢失归档中真实的 request identity。

PnP 进一步要求兼容目标不能变成隐式依赖供给。目标包必须从组件 issuer 的依赖清单可见；根 alias、
提升安装或 Components 私有 fallback 都不能覆盖 Yarn 的拒绝。

## Decision

1. loader 不再迁移 JavaScript 源码。它只读取原始字节，并保留既有的、独立的组件
   `css/index.css` 自动追加行为。
2. `crab_component_package_dir` 是组件公共入口身份的唯一判定：最近候选包根的
   `package.json#name` 必须精确匹配 `@crab-dev/rc-*`，文件必须是包根或直接 `esm/`、`cjs/` 下的
   `index.js|mjs|cjs`。物理目录名不参与身份。
3. parser 是依赖事实来源。仅当上述公共入口的 `Import`、`ExportFrom` 或 `Require` 节点的原始
   specifier 精确等于 `@linaria/core` 时，bundler 才把 resolver 的内部目标设为
   `@crab-dev/css`。`DynamicImport` 不映射。
4. `ParsedDep`、持久缓存摘要、诊断、`ModuleRequestKey`、linker 与 codegen request 始终保留原始
   specifier 和 dependency kind。内部目标不是第二个模块请求身份，也不跨 resolver 边界持久化。
5. `Resolver::resolve_internal_package_with_profile` 只接受精确的、无子路径 npm 包根。它跳过面向
   源码请求的 Wake alias，但仍按 issuer 执行最近 PnP manifest 的声明/peer/virtual/zip 规则；未受
   PnP 管理时才沿经典 `node_modules` 包树解析。
6. 应用源码、其他第三方包、组件内部文件、嵌套 `dist/esm/index`、近似包名和伪造目录布局继续按
   原始 `@linaria/core` 请求解析。Wake 编译器仍只识别源码中真正来自 `@crab-dev/css` 的 API 绑定。

## Invariants

- loader 输出除既有样式 import append 外与输入源码逐字节相同；不得恢复 `.replace` 或同类扫描器。
- 只有 parser 产出的依赖节点能触发兼容解析；字符串、模板、注释和正则中的同值文本没有依赖语义。
- ModuleRec、缓存和 emit 使用 `(original specifier, dependency kind)`；内部目标只决定本次解析出的
  `ModuleIdentity`。
- 静态 ESM 与 CJS 兼容结果在 readable/minified、cold/retained/persistent-warm 构建中一致。
- PnP 下组件或 `packageExtensions` 必须声明 `@crab-dev/css`；`PnpDependency::Undeclared` 产生
  `WAKE0301`，不能被根 alias 或 fallback 转成成功。
- 包身份在 classic `node_modules`、workspace、virtual、unplugged 和 zip 路径上由 manifest 决定，
  不从目录拼写猜测。

## Evidence

- `crates/wake_bundler/src/loader.rs`：单一公共入口判定、源码字节不变和样式 append。
- `crates/wake_bundler/src/incremental.rs`：parser-owned dependency kind、内部 resolution target 与
  原始 `ModuleRequestKey`。
- `crates/wake_bundler/src/tests.rs`：真实 ESM/export-from/CJS/Node、负例、动态 import、缓存与 PnP
  矩阵。
- `crates/wake_resolver/src/lib.rs`：exact internal package API、alias 隔离及 PnP declared/undeclared
  测试。
- `scripts/check-architecture.test.mjs`：禁止旧 helper/loader `.replace`，并固定原始 request identity。

## Consequences

兼容行为不再制造一份伪源码，诊断和 source map 可以继续指向发布归档的真实 request；增加依赖语法
不会要求扩充字符串替换表。resolver 多一个仅供 host-owned exact package target 使用的入口，该入口
刻意不共享源码 alias 语义。每个组件模块会做一次 manifest-backed 公共入口判定；依赖解析的并行和
确定顺序不变。

旧归档在 PnP 项目中仍需要安装所有者通过 `packageExtensions` 补齐 `@crab-dev/css`。这是显式元数据
修复，不是 Wake resolver fallback。动态 import 继续归 Linaria 自身所有；若未来要支持，必须另行以
parser/linker/runtime 的完整切片决策，不能扩宽本桥。

## Validation

- `cargo +1.95.0 test -p wake_resolver`
- `cargo +1.95.0 test -p wake_bundler`
- `cargo +1.95.0 clippy -p wake_resolver -p wake_bundler --all-targets -- -D warnings`
- `cargo +1.95.0 fmt --all -- --check`
- `corepack yarn architecture:test`
- `corepack yarn architecture:check`
- `git diff --check`

## Supersedes

[ADR 0006](0006-crab-css-public-contract.md).

## Removal plan

本桥由 `wake_bundler` 维护者负责，只覆盖仍在 Wake 支持范围内、尚未重新发布的
`@crab-dev/rc-*` 公共入口。所有受支持组件版本直接请求并声明 `@crab-dev/css` 后，删除内部目标映射、
对应 resolver API、`packageExtensions` 和迁移测试，同时保留普通组件样式发现与 PnP 隔离测试。若该
条件此前未满足，最晚在 Wake 0.2.0 移除旧归档支持，而不是把桥扩展为永久公共契约。
