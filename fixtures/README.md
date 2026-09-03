# Fixtures — 端到端打包用例

> 端到端测试约定见 [工程测试规范](../engineering/TESTING.md#3-回归矩阵)。

## 布局

```
fixtures/
├── README.md                 # 本文件
└── <case-name>/              # 一个 fixture = 一个最小可打包项目
    ├── input/                # 输入项目（entry + 源码 + package.json + 可选 tsconfig）
    ├── expected/             # 期望产物结构（可选，用于结构断言）
    └── assert.mjs            # 产物在 node/浏览器执行后的运行结果断言
```

## 用法

每个 fixture 是一次完整的 `wake build fixtures/<case>/input/<entry>`：
1. 打包产物落到临时目录；
2. 校验产物结构（chunk/manifest）；
3. **实际执行产物**（Node.js 加载或浏览器验证）并断言运行结果，而不只比较生成字符串。

哪些变更必须补 fixture，以及本地与 CI 的验证组合，见[工程测试规范](../engineering/TESTING.md)。

## 现有 fixture

| case | 类型 | 说明 |
|------|------|------|
| `hello-esm` | 最小 ESM | 3 模块 ESM import/export 打包验证 |
| `react-ts-app` | React 19 + TypeScript | 真实 React+TS 项目（npm 依赖） |
| `react-ts-app-yarn-pnp` | Yarn PnP | 同上，使用 Yarn Plug'n'Play zip 依赖 |
| `typescript-7` | TypeScript 7 兼容矩阵 | TS7 CLI 类型基准与 Wake parser/codegen/Bundler 回归输入 |
| `react-docs` | React 19+ 组件文档 | Wake 原生 MDX、Demo、Props API 与主题运行时 |
| `react-docs-workspaces` | Docs 聚合站 | 一个主站 + 两个隔离工作台，覆盖 embedded/standalone 与 lazy/eager |
| `react-components-yarn-pnp` | Yarn PnP Components | 隔离安装下的组件工作台、包产物与样式聚合验证 |
| `2k-modules` | Northstar 业务压力项目 | 约 2000 个自然可达的商务控制台模块，用于 Wake、Vite、webpack 的无缓存生产构建对比 |

### `2k-modules` — 使用方式

```bash
# 首次准备：安装锁定依赖并构建本轮 Wake release 二进制
cargo build --release -p wake_cli
cd fixtures/2k-modules
npm ci

# 一键执行：强制重新生成、校验项目，再构建 Wake + Vite + webpack
npm run bench

# 分步执行
npm run generate
npm run generate:update    # 仅在审阅后显式更新 committed oracle
npm run verify
npm run build:wake
npm run build:vite
npm run build:webpack

# compare 是同一严格 runner 的别名，也会重新生成和校验输入
npm run compare

# Criterion 基准（内存 FS，无需磁盘 IO）
cd ../.. && cargo bench -p wake_bundler --bench bundle -- "bundle_2k"
```

完整的数据集、正确性 oracle、单 bundle 公平性约束和测量口径见
[`2k-modules/README.md`](2k-modules/README.md)。
