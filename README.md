# Wake

Wake 是一套 Rust 原生的 Web 构建工具，将 JavaScript / TypeScript 编译、打包、开发服务器和 React 组件文档整合到同一条工具链中。它既可以通过 `wake` CLI 使用，也提供 ESM、CommonJS 和 TypeScript 类型完备的 Node.js API。

> [!IMPORTANT]
> Wake 当前处于 `0.1.x` Beta 阶段。用于团队或生产项目时，请锁定具体版本、在 CI 中同时验证应用与文档构建，并保留可回退的构建方案。

## 主要能力

| 能力 | 说明 |
| --- | --- |
| 编译 | 原生解析和转换 JavaScript、TypeScript、JSX 与 TSX |
| 生产构建 | Tree Shaking、无效模块消除、压缩、动态导入分包、CSS 抽取及内容哈希 |
| 开发服务器 | 常驻增量构建、文件监听、HMR 和 API 代理 |
| 资源管线 | 小资源内联、大资源独立输出，并生成 HTML 与 `manifest.json` |
| Wake Docs | 从 MDX、React Demo、Props 类型和 JSDoc 构建静态组件文档站 |
| 多种入口 | 原生 CLI、Node.js API，以及隔离在 `experimental` 子路径下的编译器原语 |

## 快速开始

### 环境要求

- Node.js `>=22.14 <27`；
- npm；
- React 项目和 Wake Docs 使用 React 19；
- 只有从源码构建 Wake 时才需要 Rust 1.95 或更高版本。

安装 Wake 和一个最小 React 应用所需的依赖：

```bash
npm install react react-dom
npm install --save-dev @crab-dev/wake typescript @types/react @types/react-dom
```

创建 `wake.config.toml`：

```toml
[html]
entry = "src/entry.tsx"

[react]
enabled = true
jsx_import_source = "react"

[dev_server]
host = "127.0.0.1"
port = 5173
open = false
```

创建 `src/entry.tsx`：

```tsx
import React from "react";
import { createRoot } from "react-dom/client";

function App() {
  return <h1>Hello, Wake!</h1>;
}

const root = document.getElementById("root");
if (!root) throw new Error("Missing #root element");

createRoot(root).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
```

Wake 在没有自定义 HTML 模板时会生成包含 `<div id="root"></div>` 的默认页面外壳。现在可以启动开发服务器：

```bash
npx wake dev .
```

创建生产构建：

```bash
npx wake build --outdir dist
```

建议将命令写入项目的 `package.json`：

```json
{
  "scripts": {
    "dev": "wake dev .",
    "build": "wake build --outdir dist",
    "docs:dev": "wake docs dev .",
    "docs:build": "wake docs build ."
  }
}
```

完整入门教程见[快速开始](docs/getting-started/quick-start.mdx)，所有配置项见[配置参考](docs/guide/configuration.mdx)。

## CLI

```text
wake [--no-color] <COMMAND>
```

| 命令 | 用途 |
| --- | --- |
| `wake build [ENTRY]` | 创建生产构建，默认输出到 `dist` |
| `wake dev [ROOT]` | 启动应用开发服务器和 HMR |
| `wake docs dev [ROOT]` | 启动组件文档开发服务器 |
| `wake docs build [ROOT]` | 创建可部署的静态文档站 |
| `wake parse <FILE>` | 解析源码并打印 AST |
| `wake tokenize <FILE>` | 对源码执行词法分析并打印 token 流 |

使用 `wake <COMMAND> --help` 查看当前版本支持的参数。更详细的组合、默认值和退出行为见 [CLI 参考](docs/reference/cli.mdx)。

## Node.js API

`@crab-dev/wake` 同时支持 ESM 和 CommonJS。下面的 ESM 示例分别执行一次生产构建并启动开发服务器：

```js
import { build, startDevServer } from "@crab-dev/wake";

const result = await build({
  cwd: process.cwd(),
  outdir: "dist",
});

console.log(`Built ${result.moduleCount} modules`);

const server = await startDevServer({
  cwd: process.cwd(),
  port: 5173,
});

console.log(`Wake is running at ${server.url}`);
await server.waitUntilClosed();
```

公开 API 还包括：

- `bundle()`：在内存中生成 bundle；
- `createBuildContext()`：创建可重复增量构建的上下文；
- `buildDocs()` 和 `startDocsDevServer()`：构建或开发文档站；
- `WakeError`：提供稳定错误码、路径和结构化诊断。

词法分析、解析、转换和语义分析 API 位于 `@crab-dev/wake/experimental`。其中 `ParsedModule` 是需要显式释放的原生句柄，不能克隆、持久化或传入 Worker。

## Wake Docs

Wake Docs 将文档页面、组件示例和类型信息编译为静态站点。它支持：

- 带 Frontmatter 的 MDX 页面与分组导航；
- 隔离运行的 React Demo；
- 从 TypeScript Props 和 JSDoc 生成 API 表格；
- Preview 包装器、自定义主题、搜索索引和静态子路由；
- 开发时增量更新，以及带 `base_path` 的生产部署。

在 `wake.config.toml` 中添加文档配置：

```toml
[docs]
source_dir = "docs"
title = "My UI"
description = "React 19 component documentation"
locale = "zh-CN"
base_path = "/"
```

```bash
npx wake docs dev .
npx wake docs build . --outdir docs-dist
```

从零搭建文档站请阅读[第一个文档站](docs/getting-started/first-docs-site.mdx)，MDX、Demo 和 Props API 的详细说明位于 [`docs/docs-system`](docs/docs-system/overview.mdx)。

## 安装包与平台

npm 主包会通过可选依赖选择对应的预编译原生模块，安装过程不会编译 Rust，也没有 `postinstall` 脚本。

| 操作系统 | 架构 | 运行时 |
| --- | --- | --- |
| Windows | x64 | MSVC |
| Linux | x64、arm64 | glibc |
| macOS | x64、arm64 | Darwin |

其他平台可以从源码构建 CLI，但不在当前 npm 预编译包矩阵内。

## 从源码构建

```bash
git clone https://github.com/hotlif/wake.git
cd wake
cargo build --release -p wake_cli
```

生成的可执行文件位于：

- Linux / macOS：`target/release/wake`；
- Windows：`target/release/wake.exe`。

运行仓库自带的中文文档站：

```bash
npm install
npm run docs:dev
```

## 项目结构

```text
crates/
├─ wake_ecma_*       # AST、词法、解析、语义、转换、生成与压缩
├─ wake_resolver     # 模块解析与 Yarn Plug'n'Play 支持
├─ wake_graph        # 模块依赖图
├─ wake_bundler      # Bundle、chunk 和增量构建会话
├─ wake_turbo        # 并发增量计算引擎
├─ wake_dev_server   # HTTP 开发服务器、监听与 HMR
├─ wake_docs         # MDX、Demo、Props API 和静态文档生成
├─ wake_app          # CLI 与 Node 共用的应用层
├─ wake_cli          # wake 命令行程序
└─ wake_node         # Node-API 原生绑定
docs/                # 中文使用文档与示例
fixtures/            # 应用、文档和压力测试项目
npm/                 # npm 主包与各平台原生包
scripts/             # 版本、打包和启动时间检查
```

编译核心与 CLI / 服务器边缘层保持分离；`wake_app` 负责让 CLI 和 Node.js API 共享同一套构建、配置和诊断行为。

## 开发与验证

提交 Rust 修改前运行：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

修改 Node.js 绑定或 npm 包时运行：

```bash
npm install
npm run versions:check
npm run native:build
npm run npm:test
npm run npm:typecheck
npm run npm:pack:check
```

CI 还会在 Linux 和 Windows 上测试 workspace，并使用 Miri 验证手写 `unsafe`、使用 Loom 验证并发 single-flight 协议，以及编译全部 benchmark。

## 已知边界

- `--sourcemap` 面向生产问题排查；为保证映射准确，它会关闭压缩、mangle 和代码分割，不应直接作为最终发布配置；
- `.wake`、`dist` 和 `docs-dist` 都是可再生成目录，不应存放手写文件；
- Wake Docs 要求项目直接声明 React 19 和 `react-dom` 19；
- 配置文件、入口或监听根发生结构变化后，建议重启开发服务器。

遇到问题时先查看[故障排查](docs/reference/troubleshooting.mdx)。从 Crustify 迁移的项目请参考[迁移指南](docs/migrations/crustify-to-wake.md)。

## 许可证

Wake 采用 MIT 或 Apache-2.0 双重许可。详见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)。
