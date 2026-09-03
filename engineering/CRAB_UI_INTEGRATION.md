# UI 组件工程集成

本文记录 Wake 文档站消费 `@crab-dev/rc-*` 发布包时的维护者约束。它不进入产品文档路由，
也不构成业务项目必须采用的目录结构。

## 集成边界

- `wake_docs` 生成器和通用运行时不依赖 UI 组件库。
- 当前仓库的 `docs/site` 可以通过 npm 使用组件，作为真实消费者验证发布质量。
- 组件源码仓库只用于组件开发和发布；Wake 文档不得导入本机绝对源码路径。
- React 和 React DOM 由文档项目提供，组件包必须把它们声明为 peer dependency。
- Wake 项目的 CSS-in-JS 唯一入口是 `@crab-dev/css`。

## 当前文档站依赖

文档站使用 Alert、Button、Card、Prose 和 Tag 构建首页、提示块及组件示例。版本由根
`package.json`、`.yarnrc.yml` 与 `yarn.lock` 锁定；本文不复制版本号，避免升级后形成第二份事实来源。

项目级包装集中在 `docs/site/components.tsx`，主题映射集中在 `docs/site/theme.css`，Demo
Provider 位于 `docs/site/preview.tsx`。MDX 页面应优先导入这些包装，而不是散布组件包依赖。

## 自动样式发现

Wake loader 只对满足以下条件的入口启用组件样式发现：

1. 最近包根的 `package.json#name` 匹配 `@crab-dev/rc-*`；
2. 文件是包根、`esm/` 或 `cjs/` 下受支持的公开 `index` 入口；
3. 同包存在 `css/index.css`。

loader 把 CSS 作为普通模块加入依赖图，因此开发注入、生产抽取、资源改写、顺序和缓存失效复用
标准 CSS 管线。内部实现文件、业务源码和其他第三方包不能触发这项行为。

## 已发布组件迁移桥

仓库自有源码、示例和清单只使用 `@crab-dev/css`。部分已发布 `@crab-dev/rc-*` 归档的公开
ESM/CJS 入口仍包含前代 CSS runtime 的 bare import，且没有完整声明新的 CSS runtime 与图标运行时
依赖。hoisted `node_modules` 会隐藏后一个问题，Yarn PnP 则会正确拒绝解析。

Wake 对无法原地修改的发布归档保留两条受限迁移措施：

- parser 只为已经验证包身份的 `@crab-dev/rc-*` 公开入口记录的静态 ESM/CJS 依赖选择内部
  `@crab-dev/css` 解析目标；loader 不改源码，ModuleRec/缓存/codegen 保留旧 runtime 的原始
  specifier/kind，业务源码、其他第三方包、组件内部文件和动态 import 不映射；
- Yarn `.yarnrc.yml#packageExtensions` 显式补齐已知 `@crab-dev/rc-*` 归档的 CSS、Lucide、React 与
  React DOM 元数据；这些条目进入 PnP 清单，Wake resolver 不再提供 Components 私有供给桥。

使用旧归档的 Yarn PnP 项目应迁移根仓库 `.yarnrc.yml` 中对应的 `packageExtensions`；不得通过
`pnpFallbackMode` 或隐式顶层 fallback 放宽可见性。当所有受支持组件版本都声明完整元数据后，应删除
这些扩展及 resolution bridge，但保留隔离 PnP fixture。详见
[ADR 0035](decisions/0035-parser-owned-crab-runtime-resolution.md)。

## 发布与验证

组件集成变更至少执行：

```powershell
corepack yarn docs:check
corepack yarn docs:build
corepack yarn pnp:components:check
```

PnP 门禁从本地 tarball 建立无 `node_modules` hoist 的隔离安装，验证：

- `@crab-dev/css`、Wake 主包和当前平台原生包可以解析；
- Components 模式可以完成生产构建；
- 入口和 CSS 文件带内容 hash；
- Lucide barrel 和组件运行时导出在真实 bundle 中可用；
- 当前文档站使用的组件样式前缀全部进入 CSS asset。

升级 UI 组件时还要人工检查浅色、暗色、键盘、移动端和 portal 组件，并同步更新公开的
“UI 组件库”指南。发布问题不应通过新增业务包名特例或绝对路径导入规避。
