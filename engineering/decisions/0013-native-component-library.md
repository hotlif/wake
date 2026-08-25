# ADR 0013: 新增原生组件库产品边界

- Status: proposed
- Date: 2026-08-18

## Context

Crab 组件包目前使用 Packify 完成四项不同工作：库 JavaScript 与 CSS、TypeScript 声明、设计 token
源码生成，以及 react-docgen JSON。Wake 的应用构建拥有浏览器 HTML、清单和 IIFE 运行时。ADR 0008
中的独立 `wake bundle` 契约拥有精确文件的浏览器 IIFE 与 Node CommonJS bundle，但没有真正的 ESM
渲染器、声明图、token 生成器或兼容 react-docgen 的输出。

现有打包器链接计划不是格式中立的库 IR：在计划记录 tree-shaking 和分块位置前，每个模块都会转换为
CommonJS 主体。将此输出重新包装为 ES 模块只会保留 Wake 运行时，而非生成真正的库模块，因此不能
作为可接受的兼容实现。

## Decision

Wake 将新增独立的 `library` 产品边界。`wake_app` 拥有公共操作契约、项目解析、取消和事务性产物写入。
`wake_bundler` 拥有格式中立的模块图及 ESM/CommonJS 渲染器；`wake_ecma_codegen` 则通过向每个
preserve-modules CommonJS 文件注入其引用的所有互操作辅助函数，使文件自包含。声明与 docgen 生成
共享的类型语法图将独立于运行时 AST；后者会有意擦除 TypeScript 类型。

库产物提交在 `esm`、`cjs`、`declarations` 和 `css` 目录包含有效输出时保持其目录标识稳定。
`wake_app` 将确定性的暂存清单与当前清单比较，跳过字节相同的文件，备份每个已变更或过期文件，
原子替换变更文件，删除过期文件，并在失败后回滚已完成操作。目录级重命名不属于契约，因为 Windows
目录监视器可能会拒绝它。

公共命令为 `wake library build`、`wake library token` 和 `wake library docgen`。npm API 暴露
`buildLibrary`、`generateCssToken` 和 `generateDocgen`。只有原生实现及语义测试存在后才发布相应
能力；尤其不能通过用 ESM 包装 CommonJS 或 IIFE bundle 来实现库构建。

库 JavaScript 将所有裸包和 Node 内置模块视为运行时外部依赖。外部边还可具有供静态 CSS 求值使用的
分析目标；分析目标绝不进入运行时分块。CSS 编译使用 `@crab-dev/css` 及可静态证明的值。现有
Linaria 包按源码逐一迁移，而非向编译器加入任意 JavaScript 执行能力。

Token 生成是首个独立纵向切片。它通过 Wake 的 PnP 感知文件系统读取 `token.toml`，递归跟踪包导入，
验证每个 `$ref`、检测循环、转义生成的 TypeScript，并仅原子替换配置的输出文件。

## Invariants

- 现有应用构建和 ADR 0008 打包行为保持不变。
- ESM 库输出不含 Wake IIFE、模块表或浏览器加载器。
- 每个生成的 CommonJS 文件自行解析其运行时辅助函数；不依赖未声明的进程全局变量，也不依赖包入口先执行。
- 裸运行时依赖绝不打包，包括为静态分析而加载的依赖。
- CSS 静态分析失败即报错，绝不静默省略样式。
- 库输出先暂存再事务性替换；失败时保留最后一份有效输出。
- 字节相同的重建不会重写输出文件；输出提交不会重命名已有内容的顶层输出目录。
- 类型声明生成绝不使用 `any` 替代不受支持的公共推断。
- CLI、Node-API 和 npm 入口都通过 `wake_app` 收敛。
- 生成器输出不含绝对检出路径或非确定性元数据。

## Evidence

- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_ecma_codegen/src/lib.rs`
- `crates/wake_ecma_codegen/src/tests.rs`
- `crates/wake_bundler/src/library.rs`
- `crates/wake_ecma_parser/src/stmt.rs`
- `crates/wake_css_in_js/src/lib.rs`
- `crates/wake_app/src/lib.rs`
- crab-dev 中的 `toolbox/packify/src/index.ts`
- crab-dev 中的 `toolbox/packify/src/generateCssToken.ts`

## Consequences

这项工作是编译器扩展，而不是 CLI 别名。链接 IR、声明图和仓库迁移是可分别测试的里程碑。
React Compiler 19 记忆化和通用 WYW 执行不属于首版原生契约；仍需保证 React 运行时语义及 Crab
实际使用的静态 CSS 形式。

## Validation

- 运行 token 文本黄金测试、递归导入、循环、缺失引用、PnP 和原子写入测试。
- 针对 Node 运行 ESM 实时绑定/循环/重新导出/TLA 和 CommonJS 互操作测试。
- 在两次库构建期间持有一个不共享删除权限的 Windows 目录句柄，并注入后续文件替换失败，以证明回滚和诊断上下文。
- 运行 CSS 跨包静态值和稳定类名夹具。
- 从真实使用方项目对生成的声明执行类型检查。
- 将 docgen JSON 与 Packify 参照物做结构比较。
- 运行架构门禁及所有现有应用、打包、Node API 和 npm 回归测试。
- 构建每个 Crab 组件并验证 tarball，然后才移除 Packify。

### 实现检查点（2026-08-18）

首条原生纵向路径已实现：格式中立的运行时/分析边、真正的 preserve-module ESM 与 CommonJS 渲染器、
严格的静态 CSS 抽取、稳定的包前缀类名、保留源码的声明模块图，以及对四个 Packify 输出目录的
暂存/备份替换。Rust CLI 和 Node/npm 已通过 `wake_app` 暴露 build、token 与 docgen。

候选发布审计还确认：互操作能力由独立 CommonJS 模块拥有，而不只由应用打包器运行时拥有。
Preserve-module 代码生成现在会在每个文件引用时注入一次 default 与 namespace 辅助函数。产物提交
现在会在稳定输出目录下按内容逐文件替换并支持回滚，其中包括 Windows 回归：连续构建期间保持
`declarations` 打开且不共享删除权限。

当前 crab-dev 组件工作区的隔离副本在不改源码的情况下成功构建了 51 个包入口中的 43 个。其余八个
失败会按设计关闭：四种公共声明推断形式和四种不受支持的复杂 TSX 解析形式。声明图跟踪公共重新导出
和类型边，而非只用于实现的运行时导入，避免内部常量造成虚假的公共类型失败。
复合扩展名声明导入和工作区分析链接已有回归/探针夹具覆盖；其余类别仍是迁移门禁。当工作区分析依赖
可用时，代表性的 `rc-alert`、`rc-divider` 和 `rc-button` 构建通过，且这些样例中每个生成的声明都
通过真实 TypeScript 语法门禁。

声明输出目前把内部模块保存在 `declarations/_wake` 下，并通过稳定的 `declarations/index.d.ts`
入口暴露。符号扁平化、对全部 51 个包执行完整使用方类型检查、magic/legal 注释保留、tarball 验证和
两个样例发布周期仍须完成，才能停用 Packify。

## Supersedes

None.

## Removal plan

将 Packify 保留为显式弃用的 shim，直到原生 build、token 和 docgen 契约通过两个样例发布周期。
随后迁移包脚本与依赖，移除 Packify 专用 Turbo 排序，并删除 shim。不存在对 Node 实现的静默回退。
