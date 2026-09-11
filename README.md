# Wake

Wake 为 JavaScript、TypeScript 和 React 项目提供编译、构建和开发服务器，也提供配套的样式、测试、
组件包构建和文档站能力。可以使用 CLI，也可以通过带 TypeScript 类型的 Node.js API 集成到脚本。

项目当前为 0.1.x Beta。CSS-in-JS、Wake Test、库构建和低层实验 API 有各自的实验性边界，
团队项目请锁定版本，并验证自己的依赖、类型检查和生产构建。

## 从任务开始

- [创建 React 应用](docs/start/create-react-app.mdx)：完整入口、配置、样式和类型检查。
- [接入现有项目](docs/start/existing-project.mdx)：核对插件、别名、资源和代理。
- [编写样式](docs/styles/overview.mdx)：普通 CSS 或构建期 CSS-in-JS。
- [编写测试](docs/testing/overview.mdx)：逻辑断言、React 交互和浏览器环境。
- [构建组件库](docs/app/library.mdx)：ESM、CJS、声明和可选样式。
- [建设文档站](docs/wake-docs/overview.mdx)：MDX、Demo、Props API 和自定义展示组件。
- [进阶集成](docs/integration/overview.mdx)：Node API 与浏览器 Federation。

## 安装与运行

运行发布包需要 Node.js `>=22.14 <27`，React 教程使用 React 19。普通使用无需安装 Rust。

```bash
npm install react@19 react-dom@19
npm install --save-dev --save-exact @crab-dev/wake@0.1.34
npm install --save-dev typescript @types/react@19 @types/react-dom@19
```

按[快速开始](docs/start/create-react-app.mdx)创建配置和入口后运行：

```bash
npx wake dev .
npx wake build --outdir dist
```

开发成功重建后使用整页 Live Reload；生产输出交给自己的静态服务器。类型检查由 `tsc --noEmit`
单独执行。npm node_modules 与 Yarn 4 PnP 的选择见[安装说明](docs/start/install.mdx)。

## 文档与源码贡献

用户文档从 [Wake 首页](docs/index.mdx)阅读。贡献者先阅读 [工程入口](engineering/README.md)
和 [测试与门禁](engineering/TESTING.md)，再运行与变更对应的检查。

```bash
corepack yarn docs:check
corepack yarn docs:build
```

文档保留已有公开 URL；示例使用当前工作树实现，实验性能力不因示例通过而变为稳定接口。

## License

MIT OR Apache-2.0。见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)。
