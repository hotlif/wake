# ADR 0005: 将抽取样式变为分块所有的产物

- Status: accepted
- Date: 2026-08-14

## Context

ADR 0004 让 Wake Docs 保持单个生产 JavaScript 分块，因为抽取出的 CSS 在分块图中没有所有者。
因此，加载惰性路由时可能先执行 JavaScript，后激活路由 CSS。该桥接保证了正确性，却增大了初始
JavaScript 产物，并迫使产品边缘弥补打包器契约的缺失。

## Decision

`wake_bundler` 将抽取样式作为分块产物拥有。每个输出分块都列出其 JavaScript 执行前必须生效的
CSS 文件。入口 HTML 只加载入口所有的样式。入口运行时序列化非入口分块到 CSS 的映射，先加载
依赖分块，再等待当前分块的样式表，最后才加载其 JavaScript。

写出的构建清单包含相同的分块/样式关系。Wake Docs 使用常规生产代码拆分，不再配置单分块例外。

## Invariants

- 分块内的 CSS 遵循静态依赖求值顺序。
- 依赖方分块的 CSS 在依赖分块 CSS 之后、依赖方 JavaScript 之前加载。
- 入口 HTML 不会提前加载仅由惰性分块拥有的样式。
- 样式表和脚本 URL 使用同一个规范化 `publicPath`。
- 服务端和 Node 分块加载不会尝试求值 CSS。
- 冷构建、热会话构建和持久缓存构建生成等价的分块/样式所有权。
- 分块和样式文件名保持内容寻址且确定。

## Evidence

- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_bundler/src/tests.rs`
- `crates/wake_bundler/src/lib.rs`
- `crates/wake_app/src/lib.rs`
- 浏览器形态的 VM 回归测试证明 CSS 加载完成先于惰性 JavaScript 执行。
- Wake 应用回归测试证明生产路由分块拥有其抽取样式。

## Consequences

Wake Docs 恢复页面级 JavaScript 拆分，惰性路由 CSS 不再包含于初始 HTML 中。运行时为每个惰性
CSS 产物增加一个去重后的样式表 Promise。构建使用方可以直接检查分块/样式所有权，而不必从扁平
资源列表中推断。

独立惰性根按请求顺序激活，因此其全局 CSS 效果也遵循激活顺序。每个已激活分块图内的静态依赖
顺序仍保持确定。

## Validation

- 运行打包器的浏览器形态动态 CSS 执行回归测试。
- 运行代码拆分、CSS 抽取、公共路径和持久缓存测试。
- 运行 Wake 应用和 Docs 生产构建检查。
- 比较冷构建与热构建输出的分块/样式清单。

## Supersedes

[ADR 0004](0004-style-runtime-and-docs-css-bridge.md) 的生产 Docs 单分块桥接。ADR 0004
关于开发样式所有者的决策仍然有效，不受本产物协议影响。

## Removal plan

已在采纳本决策的切片中完成：删除 `configure_docs_production_bundler`、其中的
`disable_code_splitting()` 调用和单分块桥接断言。不保留双 CSS 加载器。
