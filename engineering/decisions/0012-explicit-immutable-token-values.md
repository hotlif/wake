# ADR 0012: 显式标记跨模块 token 结构为不可变

- Status: accepted
- Date: 2026-08-17

## Context

Crab 组件 token 生成器导出嵌套对象，使调用方可编写 `token.primary.color` 和
`vars['ring.indicator-color']` 等易读路径。Wake 能在声明模块内安全求值这些结构，但过去会丢弃导入
的对象和数组，因为其他导入方可能在 CSS 模板读取前修改共享 JavaScript 值。修复 Yarn PnP 工作区
解析后，VS Code 语言服务器也正确显示了同一编译器错误。

把每个 token 叶节点扁平化为公共原始值导出可以保证安全，却会割裂生成的 API。允许所有导入对象则会
使抽取 CSS 与运行时修改不一致。只在编辑器中抑制错误会掩盖真实的构建失败。

## Decision

`@crab-dev/css` 暴露 `defineTokens(value)`。它只接受递归纯净的普通对象、数组及编译器现有的有限
原始值集合，返回深度只读类型，并在运行时深度冻结同一结构，且不调用 getter 或用户函数。

`wake_css_in_js` 只识别从 `@crab-dev/css` 以 `defineTokens` 导入的语义绑定，且仅限直接初始化顶层
`const`。它用现有允许列表求值器计算参数，并记录不可伪造的冻结静态值标识。冻结对象和数组可以跨越
ESM 导出/导入边；普通结构仍采用保守拒绝路径。打包器和语言服务器使用方直接传播共享静态值，
不复制标记规则。

生成的 Crab 组件 token 模块使用 `defineTokens` 包装导出结构。这是对重新生成源码的原子契约切换；
不引入源码重写、插件诊断豁免或第二个包入口。

## Invariants

- Wake 绝不为获取 token 值而执行项目模块、getter、方法、构造函数或任意函数。
- 只有来自 `@crab-dev/css` 的导入绑定标识能激活 `defineTokens`；拼写和类型断言不能伪造冻结来源。
- 普通导入对象和数组仍不安全，并产生现有编译器诊断。
- 运行时值被深度冻结，并拒绝访问器、函数、symbol、bigint、特殊原型和非有限数字。
- 对象属性和数组项的顺序在构建与编辑器路径中保持源码确定性。
- Wake 构建和 Crab CSS 语言服务器使用同一静态值标识及诊断实现。

## Evidence

- `npm/css/`
- `crates/wake_css_in_js/src/lib.rs`
- `crates/wake_css_in_js/src/value.rs`
- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_css_lsp/src/lib.rs`
- `engineering/CRAB_CSS.md`
- `engineering/CSS_LANGUAGE_SERVICE.md`

## Consequences

生成的 token API 保留嵌套形态，并可安全用于跨模块 CSS 插值。npm 运行时新增有界深度冻结遍历，
编译器静态值模型新增冻结来源变体。有意需要可变运行时对象的调用方必须继续使用普通导出，且不能
跨模块静态插值这些对象。

这扩展了 ADR 0006 的单一公共 Crab CSS 契约，并保持 ADR 0010 的编译器/编辑器共享所有权。

## Validation

- 在 `wake_css_in_js` 中测试语义别名、遮蔽、顶层位置、非法参数、普通对象拒绝及冻结对象跨模块传播。
- 构建导入冻结嵌套 token 的 bundle 并验证抽取 CSS；在两种 npm 模块格式中分别验证运行时深度冻结。
- 在 `npm/css` 中测试 ESM/CommonJS 运行时一致性和 TypeScript 深度只读推断。
- 在 `wake_css_lsp` 中测试 Yarn PnP 工作区传播，并构建真实的 `rc-button` 源码。
- 运行架构、Clippy、包、VS Code 扩展、格式化和差异门禁。

## Supersedes

None.

## Removal plan

在同一次迁移中重新生成组件 token 模块，并删除所有临时原始值导出实验。不得保留兼容包装器或仅编辑器诊断路径。
