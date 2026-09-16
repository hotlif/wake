# ADR 0060: 原始类型符号身份

- Status: accepted
- Date: 2026-09-13

## Context

编译 AST 擦除接口、类型别名、类型导入与泛型，并将 namespace 降级为函数。原生值 SymbolId
不能为这些不存在于运行时树的声明提供身份；按名称把类型引用附到值符号会错误地标记使用。
ADR 0053 已要求 parser 提供中立源码事实、semantic 拥有解析，现有值投影和编译身份必须保留。

## Decision

在 wake_ecma_semantic 内增加独立的原始类型符号表。类型符号使用明确的新类型身份，
不转换成编译 SymbolId；有对应值声明时只能通过精确声明 occurrence 建立显式关联。parser
继续拥有声明、原始词法容器、类型参数可见范围和模块语法；semantic 不依赖 parser 或重扫源码。

类型表描述绑定与使用，不推断类型或判断赋值相容性。接口/namespace 等可合并声明保留每个
occurrence；局部参数、映射键和 infer 按各自可见范围解析。同一 infer 声明的约束和真分支
共用类型身份。未表示的环境保留不可解析状态，不能捕获外部同名类型，也不能作为 no-undef 证据。

当前实现保留独立全局层和模块层，合并显式导出及 ambient namespace 成员；字符串 ambient
module 的未知外部成员阻止外层查找。原生 `ts/consistent-type-imports` 以精确 import occurrence
联合这张表和既有值使用/类型查询，遇到不完整事实不猜测来源。

## Invariants

- 原生值分析、编译 SymbolId 与实验性 Node 值语义 ABI 不因类型表改变。
- 类型符号、作用域和解析只由 semantic 拥有；规则和壳层不能复制绑定器。
- 缺少模块图或类型服务时不宣称完成外部成员解析、类型检查或完整 TS 语义。
- 类型引用不生成运行时读取或 TDZ 访问，源码产生式的失败试探不留下声明。

## Evidence

- `wake_ecma_parser/tests/source_type_scopes.rs`：泛型、映射、infer、转义和回滚。
- `wake_ecma_parser/tests/source_type_declarations.rs`：擦除声明与原始词法/namespace 容器。
- `wake_ecma_semantic/tests/source_types.rs`：接口合并、局部遮蔽、重复 infer、namespace 和全局扩展。
- `wake_lint_core/tests/type_imports.rs`：双语义空间、值使用/导出与 ambient 未知环境。

## Consequences

源类型和编译值身份可以并存，保持擦除前后的边界。代价是必须明确关联和未表示状态，不能用
一个整数 ID 混合不同语义空间。完整 P1 和类型感知 P5 仍分别验收。

## Validation

先运行类型解析的最小失败测试，再验证 semantic/parser/lint/compiler 回归、Clippy、架构测试
和架构检查。证明重复声明、遮蔽、导入与 namespace 边界以及编译身份不变。

## Supersedes

None.

## Amends

- [ADR 0053](0053-source-semantic-facts-for-lint.md): 仅扩展原始类型空间的独立符号身份及其与值身份的精确 occurrence 关联；其余所有权和原生编译身份不变量保持不变。

## Removal plan

不替代普通 analyze，不引入第二个 TS parser 或临时 JS 绑定器。
