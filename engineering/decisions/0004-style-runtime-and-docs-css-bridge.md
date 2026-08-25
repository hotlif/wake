# ADR 0004: 确定样式运行时所有权并限定 Docs CSS 桥接

- Status: superseded
- Date: 2026-08-14
- Superseded by: [ADR 0005](0005-chunk-owned-style-artifacts.md)

## Context

开发环境的 CSS-in-JS 过去会嵌入由进程 ID、挂钟时间和进程内计数器派生的命名空间。这样虽能避免
两个 bundle 共用文档全局样式槽，却会让本应相同的构建输出因进程而异，也迫使 CSS 代码生成任务
的标识在每个冷启动打包器实例中改变。

生产环境的 Wake Docs 路由可能含有抽取出的全局或组件样式。Wake 当时还不能生成有序的
入口/分块到 CSS 清单，也不能在执行动态分块前加载 CSS。因此 Docs 为保证正确性禁用了代码拆分，
但该桥接没有长期所有者或移除里程碑。

## Decision

使用 bundle 运行时的 `__wake_require__` 函数对象作为开发样式所有者。将所有者注册表存入文档范围的
`WeakMap`，再通过确定性的模块 ID 定位同一所有者内的样式。构建输出不包含进程标识、时间或随机值。

保留生产环境 Wake Docs 的单分块桥接，直到打包器拥有原子的 `StyleArtifact` 流、有序的
入口/分块到 CSS 清单，以及能在动态模块执行前完成相关 CSS 加载的加载器。桥接仅限于
`wake_app::build_docs_with_mode`；普通应用的生产构建仍保留代码拆分。

`wake_bundler` 负责交付替代产物协议。该协议可用后，`wake_app` 负责删除桥接。桥接必须在
`@crab-dev/css` 0.2.0 发布前，或 Wake Docs 宣布支持页面级代码拆分前移除，以先到者为准。

## Invariants

- 相同的源码图和选项在不同进程中生成字节完全一致的开发 JavaScript。
- 独立 bundle 运行时不能覆盖同一文档中彼此的样式槽。
- 动态分块共享入口运行时的样式所有者。
- 重新执行模块时更新或删除其稳定槽，不累积匿名样式。
- 生产 Docs 只有在所有相关 CSS 产物生效后才执行路由分块。
- 桥接存在期间，生产 Docs 构建只生成一个带内容哈希的 JavaScript 分块。

## Evidence

- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_bundler/src/tests.rs`
- `crates/wake_app/src/lib.rs`
- `engineering/CRAB_CSS.md`
- 已执行针对两个独立 bundle 运行时的 Node DOM 模拟回归测试。

## Consequences

开发输出变得可复现，CSS 代码生成也不再携带进程级输入。运行时每执行一个带样式模块，会增加一次
文档 `WeakMap` 查询；当对应的 require 函数不可达时，所有者注册表即被释放。

正确性桥接存在期间，Wake Docs 会保留较大的初始 JavaScript 产物。这是明确且有界的成本，
并非目标性能架构。

## Validation

- 用独立打包器构建相同的开发输入并比较 JavaScript 字节。
- 在同一个文档模拟对象上执行两个模块 ID 相同的 bundle，并验证存在两个样式节点。
- 运行完整的 `wake_bundler` 测试，包括动态分块执行。
- 构建多路由 Docs 夹具，并断言仅有一个带内容哈希的 JavaScript 分块。
- 运行 `npm run docs:build`、`npm run docs:check`、CSS 包测试和类型检查。

## Supersedes

None.

## Superseded by

[ADR 0005](0005-chunk-owned-style-artifacts.md) 使用分块所有的样式产物替代生产环境的单分块桥接。

## Removal plan

引入包含 CSS、顺序和源码标识的可序列化 `StyleArtifact`；将产物接入分块规划；生成有序的
入口/分块到 CSS 清单；让浏览器加载和 HTML 生成激活所需 CSS；添加冷/热构建、动态导入、HMR
和浏览器层叠测试；随后从 Docs 删除 `disable_code_splitting()`，并用页面级分块/CSS 执行测试
替换单分块桥接测试。切换后不保留双 CSS 加载器。
