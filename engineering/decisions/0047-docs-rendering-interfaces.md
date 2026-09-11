# ADR 0047: Docs 能力与可覆盖展示接口

- Status: accepted
- Date: 2026-09-10

## Context

站点路由、搜索、导航、主题和 Demo 能力与默认 JSX 混合，使用者只能覆盖 CSS 或 Demo 包装器。
Crab UI 站点需要复用 Wake 能力并提供自己的组件，不能复制内部运行时或继承默认全局主题。

## Decision

`docs.ui` 显式指向项目模块，默认导出类型化组件注册表。Wake 拥有能力控制器和生成内容，组件通过
props 接收数据、状态、操作和内容节点。缺省组件使用默认实现；无效注册或组件错误必须报告。
公共 npm 子路径 `@crab-dev/wake/docs` 拥有注册辅助函数与类型，不暴露内部 registry 或公共状态 hooks。

站点默认展示样式仅作用于默认元素，不能向应用根注入 token 或匹配自定义组件。工作台保持独立。
自定义 UI 的开发构建使用完整单图，保留任意应用 Provider 与页面的共享语义；无 UI 配置继续沿用
ADR 0046 的按访问编译。`docs.preview` 仍独立拥有 iframe 内 Demo 包装，不被 Demo 展示接口替代。

## Invariants

- 所有覆盖路径复用 Wake 路由、搜索、导航偏好、主题和内容加载能力。
- 组件注册是可选的；错误不会触发静默默认回退。
- 默认样式不通过选择器、继承或全局变量污染自定义组件及其 Portal。
- 元数据、页面完成通知、错误捕获和 iframe 通信不交给展示组件重新实现。
- 独立工作台不继承父站点 UI；Components 模式拒绝直接配置 UI。
- 配置与 UI 输入沿用生成事务、监听及候选失败保留旧产物的边界。

## Evidence

- `crates/wake_docs/runtime/`：能力控制器、默认展示与覆盖接线。
- `crates/wake_docs/src/lib.rs`、`crates/wake_app/src/lib.rs`：生成、监听与构建选择。
- `npm/wake/docs.mjs`、`npm/wake/docs.d.ts`：公共注册契约。
- 自定义 UI fixture、运行时行为测试和浏览器样式验证。
- 验证结果与验证范围见 `fixtures/docs-ui/VALIDATION.md`。

## Consequences

使用者可逐步替换整个站点展示。默认站点保留原有行为；自定义 UI 暂时承担完整开发构建成本。
旧的内部 CSS 选择器不作为公共 API；显式 `theme_css` 继续在默认样式之后加载。

## Validation

- 受影响的 wake_config、wake_docs、wake_app 测试。
- 注册表、类型、默认/部分/全部覆盖与真实浏览器行为及样式测试。
- 默认与自定义 fixture 开发、生产构建，以及 npm 打包消费检查。
- `corepack yarn docs:check`、`corepack yarn architecture:test`、`corepack yarn architecture:check`。

## Supersedes

None.

## Amends

- [ADR 0002](0002-docs-runtime-and-content-contracts.md): 站点继续禁止隐式引入工作台；使用者的 UI 或内容可显式导入 Crab UI 包，默认界面不依赖这些包。
- [ADR 0046](0046-docs-development-demand-bundles.md): 配置 `docs.ui` 的开发站点加入完整单图编译范围，以保证 Root Provider 与页面共享 Context。

## Removal plan

移除站点入口的全局默认 CSS 和强制默认 UI 依赖 Crab UI 的未完成要求。完整单图继续使用既有构建
路径；按访问编译支持任意共享 Context 前保留自定义 UI 回退，并在未来决策中验证后替换。
