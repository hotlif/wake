# Wake 兼容性决策

本文为源码中的 `WAKE-COMPATIBILITY` 与 M1–M8 引用提供当前解释，并记录从 Crustify 迁移时形成的历史产品决策。公共文档不再维护旧工具迁移教程。

# 0. 决策基线

1. 配置使用声明式 `wake.config.toml`，不执行 TypeScript/JavaScript 配置代码。
2. 应用使用静态 HTML 外壳，不提供旧工具的运行时 SSR 配置能力。
3. CLI、Node API 和文档构建共享 `wake_app`，避免兼容行为分叉。
4. 无法声明化的回调型 `mods` 不直接兼容；有限扩展通过 `[hooks]`、alias、component scan、Preview 和主题表达。

# M1 — 配置与 alias

- 从项目起点向上发现 `wake.config.toml` 或 `package.json`；
- `root_dir` 改变项目根；
- 内置 `@` 指向 `src`、`@@` 指向根，自定义 alias 叠加；
- Browserslist、TypeScript、React 和 define 使用声明字段。

# M2 — 扫描、入口与 HTML

- `[[component_scan]]` 用目录、正则和 namespace 生成 `@@@/<namespace>` 模块；
- `[html].entry` 定义虚拟入口；
- `[html].template`、`public/index.html`、内置外壳按顺序选择；
- `[hooks].bootstrap_path` 只保留声明数据，当前不替代普通入口。

# M3 — Dev/Prod 产物

- dev 注入 development define、样式和 HMR；
- prod 开启 Tree Shaking、压缩、分包、CSS 抽取和内容哈希；
- `[dev_server]` 提供 host、port、open 和 HTTP/WS 代理；
- 小资源内联、大资源独立输出，HTML 与 manifest 使用统一路径。

# M4 — 转换与 Source Map

- TypeScript 类型擦除与 React automatic runtime 为默认能力；
- 浏览器目标从显式配置、Browserslist 文件、package.json 或固定现代基线解析；
- `transforms.include/exclude` 只覆盖已登记转换，不执行任意插件。

## M4d — 精确 Source Map

Source Map 构建关闭压缩、mangle 和代码分割，以保证映射来源可解释。它面向问题排查，不等同于最终生产优化配置。

# M5 — 高级语法

parser/transform 支持项目测试覆盖的现代 JavaScript、TypeScript、JSX/TSX、装饰器相关语法和顶层 await。支持范围以测试与实际诊断为准，不使用“完全兼容所有 Babel 插件”的表述。

# M6 — 边界收口

组件 namespace、JSX、define 和死分支进入统一构建链。无法安全转换的语法应产生带路径和 Span 的诊断，不静默改变语义。

# M7 — 根与不支持项

`root_dir`、alias、入口、扫描、`.wake` 和输出均使用明确路径基准。Sass/Less 等未实现预处理器不伪装支持；项目需先生成 CSS 或接入上游步骤。

# M8 — 多 chunk 与静态资源

开发与生产路径都能服务构建输出和静态资源；生产动态 import 使用 async chunk。文档站的 `base_path` 与普通应用的 `public_path` 独立，避免部署语义串线。

# 兼容纪律

- 兼容结论必须由 fixture、测试或迁移示例支持。
- 旧工具的配置字段不因为名称相似就自动视为兼容。
- 破坏性变化在 0.x 中仍需 changelog、迁移说明和明确诊断。
- 新项目只依赖 Wake 文档中的稳定入口，不依赖本文件的历史里程碑编号。
