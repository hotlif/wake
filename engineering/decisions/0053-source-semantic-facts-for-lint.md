# ADR 0053: lint 复用原生语义事实

- Status: accepted
- Date: 2026-09-13
- Amended by: [ADR 0067](0067-top-level-ambient-value-projection.md)
- Amended by: [ADR 0060](0060-source-type-symbol-identities.md)
- Amended by: [ADR 0063](0063-lint-module-graph-snapshots.md)

## Context

作用域规则需要区分读取、写入、声明和导出；现有 semantic 已拥有绑定解析，但引用没有访问类别。
直接在规则中重写绑定解析或根据源码名称猜测 Promise、console、React Hooks 会产生不一致。

## Decision

继续遵守 ADR 0003：语法由 parser 产生，语义由 wake_ecma_semantic 分析公共 AST 模型。语义模型
增加访问分类及供源码分析使用的事实；保留现有 analyze 入口的解析身份与顺序。赋值目标的写入
必须与其成员基对象/计算键的读取区分，解构默认值和 for-in/of 目标采用相同语义。
普通与源码分析使用调用方同一个 Interner 识别内建 arguments 名称，生成独立隐式绑定；
隐式绑定没有源码声明 occurrence。具名函数表达式名称位于参数环境外侧，不与同名体内 var 合并。

lint 核心按已启用规则的分析依赖调用 semantic，不能在 CLI、Node、配置层重复绑定或以文本匹配
代替符号身份。源码语法模型的中立数据放在 AST 层，由 parser 构建，semantic 不依赖 parser。
源码规则通过原始标识符 occurrence 与语义身份相交，排除合成绑定与引用。当前投影只支持值空间；
已用显式源码 export 事实解析值导出使用，独立于表达式读取，且不改变普通 analyze 的表与身份；
首个切片不把擦除的类型声明或类型查询宣称为已解析使用。后续扩展必须增加显式事实与测试，
缺失能力不靠名称猜测补齐。

## Invariants

- parser 不依赖或重新导出 semantic；semantic 只依赖 common 与 AST。
- 一条引用记录 Read、Write 或 ReadWrite；成员对象和计算键始终按读取分析。
- 类型和 JSX 的保留事实来自原始语法，不逆向扫描生成代码或合成 helper。
- 新事实不改变原有优化器的符号身份；必要的语义缺陷修复有独立失败用例和编译回归。
- 仅当作用域事实确实完整且规则正反例通过时，才登记其支持语言与默认等级。

## Evidence

`wake_ecma_semantic/tests/analysis.rs` 的赋值、解构、闭包、具名类及源码投影用例；
`wake_ecma_semantic/tests/annex_b.rs` 的非严格块级函数双绑定、词法阻挡与不伪造声明；
`wake_ecma_semantic/tests/source_exports.rs` 的 live binding、导出解构、默认声明、namespace
成员、重导出/类型导出排除及编译身份等价；`wake_ecma_parser/tests/source_exports.rs` 的原始导出事实；
`wake_ecma_parser/tests/source_identifiers.rs` 的源码身份与 speculation 回退用例；
`wake_ecma_parser/tests/source_functions.rs` 的原始函数范围与编译等价，以及
`wake_ecma_semantic/tests/source_type_queries.rs` 的不求值类型查询与不完整环境保护；
`wake_ecma_parser/tests/source_jsx_values.rs` 的原始 JSX 字面值、表达式范围与解码/编译等价；
`wake_lint_core/tests/a11y.rs` 的静态值、动态覆盖、祖先可见性与键盘/焦点边界；
`wake_lint_core/tests/scope.rs` 的 console、Promise 构造器及执行器返回用例。

## Consequences

原生规则可复用统一符号身份，内建规则不需要 JavaScript 插件宿主。类型服务及模块解析另行接入，
本决策不把只提供词法作用域的结果宣称为类型检查或控制流图。

## Validation

最小语义失败测试先行；运行 semantic/parser/lint 核心及受影响编译测试、Clippy、架构测试与检查。

## Supersedes

None.

## Amends

- [ADR 0050](0050-native-single-file-lint-core.md): 仅扩展纯核心依赖，允许并要求复用 wake_ecma_semantic；其它禁止边界保持不变。

## Removal plan

不引入旧 lint 兼容实现。原有 analyze 的消费者继续使用同一语义所有者。
