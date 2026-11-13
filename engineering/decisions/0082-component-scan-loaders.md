# ADR 0082: 组件扫描导出按需加载函数

- Status: accepted
- Date: 2026-11-13

## Context

0.1.50 的 `wake_scan` 在生成模块顶层执行所有 `import()`，条目的 `component` 因而是已经启动的
Promise。仅读取路由或菜单元数据也会加载页面，外层 lazy 回调无法推迟这些导入。

## Decision

`wake_scan` 将 `component` 生成为无参数加载函数，调用时才执行动态导入并返回模块命名空间的
Promise。`wake_app` 的开发服务和生产构建继续共享同一扫描生成入口；分块及 CSS 所有权继续由
打包器按 ADR 0005 管理。扫描器不增加运行时加载器、Promise 缓存或兼容模式。

## Invariants

- 导入扫描模块、读取元数据、读取或解构加载函数均不触发页面导入。
- 调用加载函数仅激活目标页面及必要依赖；未访问页面独占的 JS、CSS 不提前加载。
- `name`、路径排序、根相对 `path`、TOML `frontmatter` 和 `generate_source` 语义保持不变。
- 重复调用服从动态导入的模块缓存语义；不承诺 Promise 对象身份。
- 开发服务新增、删除页面和修改 frontmatter 后，通过现有 Live Reload 发布一致扫描结果。

## Evidence

- `crates/wake_scan/src/lib.rs` 的生成与元数据测试。
- `crates/wake_app/src/lib.rs` 的 `prepare_aliases_and_scans_in` 共享生成入口。
- `crates/wake_test/tests/app_browser_conformance.rs` 的扫描页面网络、执行、CSS 与监听验收。

## Consequences

这是扫描结果 `component` 字段的显式契约变更。旧调用方需将等待 Promise 改为先调用函数，并
同步应用类型与 lazy 回调。用户已要求直接改为加载函数，不保留提前导入的旧 Promise。
其他条目字段与扫描配置不变；`generate_source` 仍会增大元数据模块的文本体积。

## Validation

- `cargo test -p wake_scan`
- `cargo test -p wake_test --test app_browser_conformance component_scan -- --ignored --nocapture --test-threads=1`
- `cargo test -p wake_app component_scan`
- `corepack yarn architecture:test` 与 `corepack yarn architecture:check`

## Supersedes

None.

## Removal plan

直接移除顶层 eager import 生成形式；没有临时兼容桥。
