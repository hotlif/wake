# ADR 0014: 为嵌入式 CSS 值赋予主题感知的语义标识

- Status: accepted
- Date: 2026-08-18

## Context

`wake_css_language` 过去会把所有不是声明名的 CSS 标识符归类为标准语义 token `keyword`。因此，
在 TypeScript 和 TSX 文档中，VS Code 会将 `inline-flex`、`center`、`unset` 等 CSS 值，与
`import`、`from`、`const` 等宿主语言关键字应用相同的主题规则。

共享的 `wake_css::syntax::CssSyntaxTree` 已记录精确的声明名和值范围，因此语言服务无需另一套解析器
或拼写启发式即可区分声明值。VS Code 扩展可以声明自定义语义 token 类型，并为未显式定义语义规则的
主题映射到既有 TextMate scope。

## Decision

`wake_css_language` 将声明解析器所拥有的 `value_span` 内普通标识符归类为 `SemanticKind::Value`。
`wake_css_lsp` 将该种类作为自定义语义 token 类型 `crabCssValue` 传输。VS Code 扩展清单拥有公共
token 声明，并将其映射到 `support.constant.property-value.css` 作为主题回退。

`crabCssValue` 没有 `keyword` 超类型。扩展不硬编码前景色。声明名仍使用标准 `property` 类型，
at-keyword 仍为 `keyword`，数字、字符串和函数保留其标准语义类型。模板插值中的 TypeScript 表达式
不属于虚拟 CSS 文档，只由宿主服务着色。

## Invariants

- 共享 CSS CST 是声明名和值边界的唯一权威。
- CSS 声明值不能仅因其为标识符就获得宿主语言关键字标识。
- 语义 token 绝不覆盖 TypeScript 或 JavaScript 插值孔洞。
- LSP 图例、编码后的 token 索引和 VS Code 清单使用同一稳定 token 标识符。
- 颜色由主题拥有；扩展只提供标准 CSS TextMate 回退。
- 语义绑定分析仍是发现 Crab CSS 模板的唯一权威。

## Evidence

- `crates/wake_css/src/syntax.rs`
- `crates/wake_css_language/src/lib.rs`
- `crates/wake_css_language/src/tests.rs`
- `crates/wake_css_lsp/src/lib.rs`
- `editors/vscode-css/package.json`
- `editors/vscode-css/test/manifest.test.mjs`
- `editors/vscode-css/scripts/check-vsix.mjs`

## Consequences

CSS 声明值可独立于 TypeScript 关键字设置样式，同时继续遵循用户主题。主题作者和用户可以直接针对
`crabCssValue`。已识别标准 CSS TextMate scope 的主题无需 Crab 专用规则即可获得合适回退。语义
图例新增一个自定义类型，因此客户端必须使用图例，不能假定硬编码索引。

本决策细化 [ADR 0010](0010-shared-css-syntax-tree.md) 保留的纯语义高亮路径。

## Validation

- 在 `wake_css_language` 中测试声明名、嵌套声明值、插值孔洞和非声明 CSS 标识符。
- 在 `wake_css_lsp` 中测试自定义图例项及编码后的值 token。
- 测试清单 token 声明、不存在 keyword 超类型，以及 CSS scope 映射。
- 运行扩展检查、受影响的 Rust 测试、Clippy、架构检查、格式化和 VSIX 检查。

## Supersedes

None.

## Removal plan

在同一变更中删除把声明值标识符归类为 `Keyword` 的旧分支。不保留兼容 token、重复分类器或硬编码主题颜色。
