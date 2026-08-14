# Fixtures — 端到端打包用例

> 端到端测试约定见 [工程测试规范](../engineering/TESTING.md#3-必补回归矩阵)。

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
| `react-docs` | React 19+ 组件文档 | Wake 原生 MDX、Demo、Props API 与主题运行时 |
| `react-components-yarn-pnp` | Yarn PnP Components | 隔离安装下的组件工作台、包产物与样式聚合验证 |
| `2k-modules` | 合成压力测试 | 2000 模块二叉树 + 共享 util，用于跨工具性能对比 |

### `2k-modules` — 使用方式

```bash
# 一键全自动：生成 + 构建(wake + webpack) + 计时对比
cd fixtures/2k-modules && npm run bench

# 分步执行
cd fixtures/2k-modules && npm run generate    # 生成 2000 个合成模块
npm run build:wake                            # wake 构建
npm run build:webpack                         # webpack 构建（需已安装）
npm run compare                               # 仅计时对比（不重新生成）

# Criterion 基准（内存 FS，无需磁盘 IO）
cd ../.. && cargo bench -p wake_bundler --bench bundle -- "bundle_2k"
```
