# ADR 0010: 由 wake_css 统一拥有所有 CSS 语法

- Status: accepted
- Date: 2026-08-17

## Context

Wake 有三个 CSS 使用方：`wake_css` 中的普通样式表打包、`wake_css_in_js` 中的构建时模板，以及
`wake_css_language` 中的编辑器智能。此前它们通过各自的字节扫描器或语言层专用解析器作出结构判断，
可能在注释、引号文本、转义、嵌套函数、at-rule 和声明边界上产生分歧。仅修复语义高亮，无法阻止
编译器和打包器路径继续保留第二套 CSS 解释。

仓库已通过解析器和语义 AST 标识发现 JavaScript/TypeScript 模板。CSS 也需要在所有使用方下层具备
同样的单一权威属性。

## Decision

`wake_css` 拥有公共的 `syntax::CssSyntaxTree` 具体语法树，包括已解码 token 种类、嵌套块结构、
文法上下文项、声明、语法错误、源码范围、token 载荷范围和 token 序列化类别。调用方显式选择
`Stylesheet`、`StyleBlock`、`Keyframes` 或 `ComponentValues` 入口上下文。它是工作区中唯一的
CSS 语法权威。

`wake_css` 用该树处理导入、URL、压缩和 CSS Modules；`wake_css_in_js` 用它处理嵌套、选择器、
关键帧、动画引用、限定范围的 at-rule 检查、URL 检查和声明删除；`wake_css_language` 从同一棵树
构建编辑器功能。使用方可以向已解码节点应用领域规则，并通过解析器拥有的范围编辑文本，但不得为
CSS 结构增加正则表达式、拼写搜索或字节扫描回退。

每份不可变 CSS 文本快照在每次使用方操作中只解析一次，所得语法树复用于所有语法敏感决策。若转换
生成不同的 CSS 文本，新快照可以作为新单元解析。JavaScript/TypeScript 模板发现仍由 ECMA 解析器
和语义绑定分析拥有。

## Invariants

- `wake_css` 位于编译器、语言服务和产品 crate 下层，不拥有产品行为。
- 注释和字符串不能激活导入、URL、全局转义、at-rule 或选择器。
- 转义 CSS 标识符根据解析器解码后的 token 解释，而非源码拼写。
- 嵌套函数和块通过子节点遍历，而非统计分隔符。
- 规则与声明通过共享上下文项区分，不能把每个花括号块或 `ident:` 对都视为相同结构。
- 删除注释时使用解析器的 token 序列化类别，不能合并相邻 token。
- 重写使用解析器拥有的源码范围，并保留未触及的源码字节。
- 编译器和编辑器模板发现只使用 ECMA AST 加语义绑定标识。
- TextMate 文法、正则识别器或手写字节扫描器都不能作为兼容回退。

## Evidence

- `crates/wake_css/src/syntax.rs`
- `crates/wake_css/src/lib.rs`
- `crates/wake_css_in_js/src/lib.rs`
- `crates/wake_css_in_js/src/nesting.rs`
- `crates/wake_css_language/src/lib.rs`
- `crates/wake_css_language/src/tests.rs`
- `editors/vscode-css/test/manifest.test.mjs`
- `engineering/architecture-boundaries.json`

## Consequences

构建和编辑器路径中的 CSS 行为现在只有一个依赖方向及一套转义/结构解释。解析器改进会惠及所有使用方，
测试也可覆盖拼写扫描器无法识别的转义语法。`wake_css` 暴露更大的稳定内部 API，每次 CSS 操作都需
分配 CST；使用方通过避免单次操作内重复解析来限制成本。

新增语法支持时，必须扩展共享节点模型，而非增加局部扫描。解析器节点确立语法边界后，仍允许进行
URL 协议策略、生成名称清理和输出序列化等纯领域文本处理。

## Validation

- 运行 `wake_css`、`wake_css_in_js`、`wake_css_language` 和 `wake_css_lsp` 测试。
- 运行聚焦的打包器 CSS 测试，以及拒绝警告的 Clippy。
- 以字符串/注释作为否定相似项，以转义标识符作为肯定解析案例进行测试。
- 检查 VS Code 包、架构策略、格式化和 `git diff --check`。
- 在受影响运行时源码中搜索已删除的扫描器/正则入口；仅测试断言和非语法领域匹配不算替代 CSS 解析器。

## Supersedes

[ADR 0009](0009-semantic-css-highlighting.md)。其中仅基于语义绑定的高亮决策及删除 TextMate 回退，
仍属于本次更广泛的共享语法决策。

## Removal plan

在同一次迁移中删除语言层拥有的语法树模块，以及所有 CSS 导入、URL、嵌套、at-rule、声明和类选择器
扫描器。不得保留独立解析结构的兼容包装器；唯一允许的包装器接收 CSS 文本后立即构造共享语法树。
