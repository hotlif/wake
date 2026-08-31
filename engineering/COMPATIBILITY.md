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

Source Map 与生产压缩、确定性名字优化和代码分割可以同时启用。优化后的 mapped/unmapped 发射由同一
typed token walk 完成，map 只是可选 sink，因此 JavaScript body 必须逐字节相同。`Source`/`Derived`
origin 提供源位置，改名 occurrence 通过 V3 `names` 保留原标识符；无可靠来源的合成标点和 wrapper
保持 unmapped。折叠、函数体内联、实参替换、可信 CSS 编辑和模块 wrapper 的细粒度映射分别以测试为
兼容证据，未覆盖的来源关系不能仅凭 origin anchor 推断。

## M4e — 压缩语义边界

`minify` 进入唯一的 Closure 风格显式 pass/固定点管线（当前 `wake-closure-minifier-v12`），不要求第二个
名字优化开关，也不因模块源码长度改变行为。这里的“Closure 风格”只表示 Wake 对 pass 排序、变更跟踪和收敛模型的架构选择；
不兼容 Closure `ADVANCED`、Closure modules、externs、类型优化、全局属性 flattening 或逐字输出。
普通 ESM/CommonJS、公开成员和宿主协议按 Wake 的局部可证明安全边界保留。实现只使用 workspace
现有依赖和 Wake 自有语料，不复制、vendor 或下载第三方压缩器代码。

“唯一管线”指生产调用只有 `optimize` 一个决策入口。Parser AST 与初始 semantic model 在入口一次性
降低成 `TypedProgram`；此后 owned typed arena 是结构、binding、名字和 origin 的唯一可变事实源。
`TypedAnalysis::rebuild` 从当前 live tree 重建 scope、读写、capture、CFG、确定初始化、effect 和 escape
facts。相同 program revision 上复用分析；结构变化后在下一次 binding-sensitive pass 前重建，收敛时的
当前分析供动态作用域判断与最终改名复用。typed codegen 也只接受 finalizer 验证后构造的
`FinalizedTypedProgram`。

固定点顺序为 primitive folding、控制流简化、封闭函数内联、单次变量内联、DCE、声明/sequence 合并和
late peephole；完整轮次无变化才收敛，100 轮仍变化返回构建诊断。Direct `eval`/`with` 只冻结可见环境。
若 decorated class 的 member 内 direct eval 会因 lowering wrapper 改变词法可见环境，只对该类返回明确
装饰器 lowering 诊断；decorator/key 表达式与无关作用域不因此整模块退出。
所有依赖“变短”才成立的局部候选使用 typed token cost 并要求不增长，未证明安全或更短的候选保留原形。
模块图用稳定名称分别保存声明绑定保留与公开 export-key 观察，并独立携带 star 转发事实；bundler 将这些
事实和 `ModuleId` 交给 optimizer，optimizer 只用 lowering 共用的 parser semantic model 把声明保留名解析为
当前 `SymbolId`，公开观察名直接控制 getter。`SymbolId` 不跨 optimizer 边界或
重新解析持久化。未提供 linker liveness 的
preserved ESM 保留公开导出的本地名；只有带已验证 liveness 的 linked output 才能压缩这些本地名，并保持
原 export key。

define 回归保证赋值/update/delete/for-in/for-of 目标不被常量替换，对象 shorthand 会展开以保留
属性 key。DCE 删空 if/loop/label/with 的必需 body 后仍生成合法空语句；export 前装饰器和 decorated
default export 的绑定语义也有 parser/codegen 回归。

Preserve ESM、Preserve CommonJS 与 bundled CommonJS 共用 typed module plan→seal→finalize 生命周期。
Optimizer 在固定点前建立 import/export/request 与 runtime binding，固定点后只保留 live request，最终
链接/chunk 事实到齐后再写入真实目标和 interop。Bundler/runtime 只选择 mode 并提供最终事实，codegen
不重建 namespace/live-binding side-plan。普通 ESM/CJS、循环、顶层 await、default/namespace interop 和
实时导出不能用 Closure whole-world 假设改写。

Scope-concat 的 bare-block 安全判断只由 bundler 私有分析拥有；检测到 `var` 或 `this` 会保守保留
IIFE。Codegen 不提供 concat policy，也不会因压缩重新解释 wrapper eligibility。

原子切换不提供兼容 emitter、旧压缩器 feature flag 或静默未压缩 fallback。IR 不变量、可信编辑、非法
linker liveness、模块 finalize 和不收敛错误都必须成为构建诊断。旧压缩器只允许以冻结体积数字存在。

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
