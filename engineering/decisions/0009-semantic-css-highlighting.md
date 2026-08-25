# ADR 0009: 将语义绑定作为编辑器高亮的唯一权威

- Status: superseded
- Date: 2026-08-17
- Superseded by: [ADR 0010](0010-shared-css-syntax-tree.md)

## Context

VS Code 扩展注册了 TextMate 注入文法，把所有拼写为 `css`、`keyframes` 或 `globalStyle` 的标签模板
都视为 Crab CSS。TextMate 能匹配拼写，却无法证明标签的导入来源、跟踪别名或区分词法遮蔽。因此，
从 `@linaria/core` 等包导入的模板也会错误地获得 Crab CSS 高亮。

语言栈已将 JavaScript 和 TypeScript 解析为 AST。`wake_css_in_js` 把导入和引用解析为语义符号标识，
`wake_css_language` 只发现绑定到 `@crab-dev/css` 的模板，`wake_css_lsp` 则为这些模板范围发布语义
token。在每个已发现模板中，`wake_css_language` 将 `cssparser` 输出保存为具体语法树，其中含嵌套
块、解码 token、错误和虚拟文档范围。

## Decision

AST 解析和语义绑定标识是 Crab CSS 模板发现与高亮的唯一权威。
`wake_css_in_js::discover_css_templates` 负责识别受支持的绑定；`wake_css_language` 从发现结果派生
虚拟 CSS 和 token；`wake_css_lsp` 把 token 传输到编辑器。

CSS 语法敏感功能为每个虚拟文档使用一棵具体语法树。语义 token、诊断、悬停、颜色、补全、折叠和
格式化不得各自实现正则表达式或字节扫描识别器。

编辑器客户端不得注册基于拼写的 TextMate 文法或维护另一套模板识别器，也不得通过在文档文本或
依赖清单中搜索包名字符串来决定是否启动服务器。VS Code 扩展仍只是原生语言服务器的轻量启动器
和配置界面。

## Invariants

- 只有从 `@crab-dev/css` 导入的语义绑定才能激活 Crab CSS 行为。
- 识别导入别名，忽略词法遮蔽及其他包中的同名标签。
- 编译器和编辑器使用方的模板发现统一由一条 AST 与语义绑定路径拥有。
- 每个模板内的语法敏感语言功能统一由一棵 CSS 具体语法树拥有。
- 语义 token 保留宿主文档范围，并使用从零开始的 UTF-16 LSP 位置。
- 高亮绝不需要执行项目 JavaScript。
- 客户端激活不会从文本或清单拼写推断源码语义。
- 每个发布的 VSIX 恰好包含一个特定目标的语言服务器，且不含 Crab TextMate 文法。

## Evidence

- `crates/wake_css_in_js/src/lib.rs`
- `crates/wake_css_language/src/lib.rs`
- `crates/wake_css_language/src/tests.rs`
- `crates/wake_css_lsp/src/lib.rs`
- `editors/vscode-css/package.json`
- `editors/vscode-css/test/manifest.test.mjs`
- `editors/vscode-css/scripts/check-vsix.mjs`

## Consequences

Crab CSS 高亮依据语义标识而非标签拼写，既保留别名，又消除对 Linaria 等 CSS-in-JS 包的干扰。
着色在语言服务器分析文档后开始，而非由 TextMate 立即执行。服务器不可用时，扩展不会猜测并应用
可能错误的 Crab 高亮。

## Validation

- 在 `wake_css_language` 中测试 Crab 导入别名、词法遮蔽及其他包的同名导入。
- 在 `wake_css_lsp` 中测试语义 token 能力和 UTF-16 编码。
- 检查扩展清单和打包后的 VSIX 不含 Crab TextMate 文法。
- 运行编辑器包检查、受影响的 Rust 测试、架构检查和 `git diff --check`。

## Supersedes

[ADR 0007](0007-css-language-intelligence.md) 中将 TextMate 着色与语义 token 结合的决策。
其语言服务所有权和依赖方向仍然有效。

## Superseded by

[ADR 0010](0010-shared-css-syntax-tree.md) 将 CSS 语法所有权移至语言服务下层，使编译器、打包器和
编辑器使用同一棵 CST，同时保留仅基于语义的高亮。

## Removal plan

在同一变更中删除 `syntaxes/crab-css.injection.json`、其清单贡献、包允许列表项和打包断言。
不保留兼容文法或第二套识别器。
