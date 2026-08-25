# ADR 0006: 将 Crab CSS 设为唯一公共 CSS-in-JS 契约

- Status: accepted
- Date: 2026-08-14

## Context

Wake 过去通过应用清单和已发布的 UI 组件使用第三方 CSS 运行时。仓库现在已拥有带类型的构建时
CSS API、编译器契约、抽取样式产物和发布包；但若继续为旧包重写源码，就会留下两个事实上的公共
入口。部分已发布的 `@crab-dev/rc-*` 版本使用了新运行时，却未完整声明；提升式安装会掩盖此问题，
Yarn PnP 则会将其暴露。

## Decision

`@crab-dev/css` 是 Wake 唯一识别的公共 CSS-in-JS 包。仓库源码、演示、夹具、清单、声明、文档和
发布冒烟测试都直接使用该包。编译器不识别别名。

仅对不可变的已发布组件归档：当源码是经验证的 `@crab-dev/rc-*` 包公共 ESM 或 CommonJS 入口时，
加载器才把旧运行时说明符重写为 `@crab-dev/css`。应用源码、其他第三方包和组件内部永不重写。

只有当某个 `@crab-dev/rc-*` 发起方的常规 Yarn PnP 解析因未声明依赖或未满足 peer 而失败时，
组件模式才能从 Wake 包提供 `@crab-dev/css` 和 `lucide-react`。发起方声明的依赖、Yarn 顶层回退和
用户别名仍有更高优先级。此桥接用于修复不完整元数据，而不是增加第二套 CSS API。

## Invariants

- 编译器只识别绑定到 `@crab-dev/css` 的导入。
- 面向用户的示例和包清单不包含旧 CSS 依赖。
- 加载器重写仅限于经验证的公共 `@crab-dev/rc-*` 入口。
- 动态值通过已记录的 CSS 自定义属性边界传递。
- 抽取出的 CSS 仍由输出分块拥有，并在其惰性 JavaScript 之前加载。
- PnP 回退受发起方、依赖和错误范围约束，不能影响应用源码。
- 包、锁文件、版本、打包和发布门禁均包含 `@crab-dev/css`。

## Evidence

- `npm/css/`
- `crates/wake_css_in_js/src/lib.rs`
- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_resolver/src/lib.rs`
- `crates/wake_app/src/lib.rs`
- `scripts/check-versions.mjs`
- `scripts/check-components-pnp.mjs`
- `docs/styles/`

## Consequences

使用方只获得一个带类型的运行时和一个编译时契约。使用旧导入的应用项目会按常规解析失败，或仍作为
普通 JavaScript 处理，而不会被静默迁移。有界的加载器和 PnP 桥接增加了临时组件集成逻辑，但会保留
声明正确的组件所选择的依赖版本。

## Validation

- 运行 CSS 包运行时和类型测试，以及 `wake_css_in_js` 测试。
- 运行打包器 CSS 抽取及动态分块执行测试。
- 运行 `npm run versions:check`、`npm run npm:pack:check` 和 `npm run pnp:components:check`。
- 运行 `npm run docs:check`、`npm run docs:build` 并检查演示输出。
- 扫描受跟踪的源码和清单，查找旧包说明符。

## Supersedes

None.

## Removal plan

重新发布所有受支持的 `@crab-dev/rc-*` 包，使其直接导入 `@crab-dev/css`，并完整声明
`@crab-dev/css` 和 `lucide-react` 元数据。常规及隔离 PnP 夹具无需迁移即可成功后，删除
`migrate_crab_component_css_runtime`、`PnpDependencyFallback`、
`components_pnp_dependency_fallbacks`、相关测试及本决策的临时部分。单一公共 CSS 包决策继续有效。
