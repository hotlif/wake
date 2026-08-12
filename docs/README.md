# Wake 中文文档

`docs/` 只保存由 `wake docs` 构建的中文使用文档及其运行资源。Wake 内部架构、性能和测试资料统一放在 [`engineering/`](../engineering/README.md)，不会进入文档路由或搜索索引。

## 新手从哪里开始

建议严格按照下面的顺序阅读。每一阶段都包含可以直接复制运行的代码、检查方法和练习。

1. [学习路线](getting-started/learning-path.mdx)：先认识 Wake、Wake Docs 和完整学习目标。
2. [快速开始](getting-started/quick-start.mdx)：从零创建第一个 React 19 应用。
3. [第一个文档站](getting-started/first-docs-site.mdx)：完成组件、Demo、API 表格和主题配置。
4. [开发模式](guide/development.mdx)：掌握热更新、代理、别名和文件监听。
5. [配置参考](guide/configuration.mdx)：理解 `wake.config.toml` 的全部常用字段。
6. [MDX 写作](docs-system/mdx.mdx)：掌握 Frontmatter、代码高亮、JSX 和静态资源。
7. [Demo 与 API](docs-system/demo-api.mdx)：编写隔离预览和 Props 文档。
8. [Button 完整教程](docs-system/button-tutorial.mdx)：独立完成一个组件的整套文档。
9. [生产构建](guide/production.mdx)：配置部署路径、缓存、静态路由和 CI。
10. [排错手册](reference/troubleshooting.mdx)：按构建阶段定位常见问题。

需要查命令时阅读 [CLI 参考](reference/cli.mdx)，从 JavaScript 调用 Wake 时阅读 [Node.js API](reference/node-api.mdx)，低层编译器原语见[实验 API](reference/experimental-api.mdx)。不理解术语时阅读 [术语表](reference/glossary.mdx)。Crab UI 的安装、导入、样式与版本升级见 [Crab UI 组件指南](guide/crab-components.mdx)。

## 本地运行文档站

环境要求：

- Rust 1.95 或更高版本；
- Node.js 与 npm；
- 文档项目使用 React 19。

在仓库根目录执行：

```powershell
npm install
npm run docs:dev
```

浏览器打开终端显示的地址。开发模式会监听 MDX、Demo、组件 Props 和主题文件的变化。

构建生产站点：

```powershell
npm run docs:build
```

默认产物位于 `docs-dist/`。提交前建议执行：

```powershell
cargo test -p wake_docs
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
npm run docs:build
```

## 文档目录

```text
docs/
├─ index.mdx                 # 文档站首页
├─ getting-started/          # 入门路线和零基础教程
├─ guide/                    # 开发、配置、生产和组件使用
├─ docs-system/              # MDX、Demo、API 和完整实战
├─ reference/                # CLI、Node API、排错和术语表
├─ examples/                 # Demo 与 Props 提取使用的真实源码
└─ site/                     # 文档站 React 组件、Preview 与主题样式
```

新增页面时必须提供 Frontmatter，并确保 `slug` 唯一。代码示例应能够运行；涉及组件 Props 时优先使用 JSDoc 说明、`@default` 和 `@deprecated`，以便 `<API>` 自动生成准确内容。

只把面向 Wake 使用者、会参与文档站构建的内容放进本目录。编译器设计、基准数据、测试策略和阶段性结论写入 [`engineering/`](../engineering/README.md)，避免站点源码再次混入内部记录。
