# ADR 0015: 将自动补全触发器限定在嵌入式 CSS 内

- Status: accepted
- Date: 2026-08-18

## Context

`wake_css_language` 已能为识别出的 `@crab-dev/css` 模板计算属性、值、at-rule 和伪选择器补全。
VS Code Extension Host 测试过去通过 `vscode.executeCompletionItemProvider` 请求这些项目，但未提供
触发字符；这等同于显式请求补全，无法证明正常输入体验。

语言服务器只声明 `:`、`@` 和 `-` 为补全触发器。嵌入式 CSS 位于 JavaScript 和 TypeScript 模板
字符串 token 内；对于属性首字母，VS Code 不会使用普通代码的快速建议。因此，在 Crab CSS 模板中
输入 `d` 不会自动打开已有的 `display` 补全。

把字母声明为 LSP 触发字符也不能解决问题。光标位于通常应使用快速建议的单词中时，VS Code 会有意
跳过触发字符提供器，随后又因该 token 是字符串而抑制快速建议请求。此外，字母触发器会作用于整个
TypeScript 文档，而不是嵌入语言区域。

## Decision

应用增量标识符插入后，`wake_css_lsp` 会询问更新后的 `LanguageDocument`：所得光标位置是否至少存在
一个 Crab CSS 补全。只有结果为肯定时，服务器才发出 `crabCss/triggerSuggest`，其中包含文档 URI、
版本和光标位置。客户端仅在三者仍与活动编辑器匹配时调用 `editor.action.triggerSuggest`。

标准 LSP 补全触发器仍为 `:`、`@` 和 `-`。它们直接处理标点，不会在整个 JavaScript 或 TypeScript
文档中占用普通字母。

当宿主位置不在语义识别的 Crab CSS 虚拟文档中，或处于插值孔洞内时，
`wake_css_language::LanguageDocument::completions` 返回 `None`，LSP 将其传输为该提供器不响应。
在已识别模板内则返回列表；即使提供器适用但无匹配项，也返回空列表。

扩展不会覆盖 `editor.quickSuggestions.strings`，因为该设置会影响 JavaScript 和 TypeScript 中的普通
字符串及所有补全提供器。客户端只负责拒绝过期通知和执行编辑器命令；不识别模板，也不决定补全是否适用。

VS Code 应用 `display: ` 等属性项时，所得增量替换通过与标识符输入相同的分析后通知路径处理。服务器
识别有界的 `property: ` 编辑形态，分析新文档版本，并仅在新值位置存在语义候选项时发出通知。这样可
避免在补全编辑同步前查询服务器。语言层只返回为当前属性声明的值，并按已输入的值前缀过滤。

VS Code 可能先发布补全编辑后的文档版本，再发布相应光标移动。因此客户端会立即检查通知；若只有
光标尚未跟上，则最多观察第一次后续编辑器选择事件。只有该事件到达精确的 URI、版本和位置时才触发；
任何其他移动或有界超时都会取消通知。

LSP 根据语义事实顺序分配稳定的 `sortText` 键。因此 VS Code 会让常用标准值排在旧版厂商值之前，
而不会按字母顺序把 `-moz-*` 提前。

## Invariants

- 语义绑定分析是判断宿主位置是否为 Crab CSS 的唯一权威。
- `:`、`@` 和 `-` 仍分别作为值、at-rule 和带前缀属性的触发器。
- 自动建议必须由更新后的服务器分析给出肯定结果。
- 过期文档版本、已移动光标以及已停止或替换的客户端不能触发建议。
- 补全引起的光标移动最多只能满足第一次后续选择事件；绝不轮询，也不在另一次移动后接受。
- 虚拟 CSS 文档外的位置不返回 Crab CSS 补全响应。
- 扩展绝不会为 JavaScript 或 TypeScript 字符串 token 全局启用建议。
- VS Code 客户端不拥有源码或模板识别器。
- 属性项替换只有在更新后的服务器完成分析后才请求后续值。
- 属性值候选项限定于当前属性，并按已输入前缀过滤。
- 补全排序保留确定性的语义事实顺序。
- 压缩必须保留每个 `void` 操作数的求值；客户端有意使用
  `void vscode.commands.executeCommand(...)` 执行即发即弃的编辑器命令。

## Evidence

- `crates/wake_css_language/src/lib.rs`
- `crates/wake_css_language/src/tests.rs`
- `crates/wake_css_lsp/src/lib.rs`
- `crates/wake_css_lsp/src/main.rs`
- `editors/vscode-css/src/extension.ts`
- `editors/vscode-css/test/manifest.test.mjs`
- `editors/vscode-css/test/suite/index.ts`
- `editors/vscode-css/README.md`
- `crates/wake_ecma_minify/src/const_eval.rs`
- `crates/wake_bundler/src/tests.rs`

## Consequences

在识别出的 Crab CSS 模板内输入 CSS 属性前缀时，即使宿主 token 是模板字符串，也会打开已有建议。
服务器只为存在候选项的 Crab 位置发送一条小型通知；普通 JavaScript 和 TypeScript 编辑不会引发额外
请求或通知。普通字符串建议和 TypeScript 导航行为保持不变。

Wake 常量求值器只有在 `expression` 本身为常量时，才将 `void expression` 视为可折叠。这可防止
压缩把 `void sideEffect()` 替换为 `undefined`；它属于通用打包器正确性规则，而非编辑器专用变通。

本决策完善 [ADR 0007](0007-css-language-intelligence.md) 建立的编辑器交互契约，同时保留
[ADR 0010](0010-shared-css-syntax-tree.md) 中的共享语法与语义绑定边界。

## Validation

- 测试模板内的属性、值、at-rule 和伪类补全，以及模板外返回 `None`。
- 在真实 VS Code Extension Host 中逐字符输入属性前缀，接受自动打开的建议，并断言编辑结果。
- 通过能力测试覆盖标准 LSP 标点触发器集合。
- 在启用压缩时打包由 `void` 包装的命令调用，断言其参数仍在输出中；检查编译后的扩展含有
  `editor.action.triggerSuggest`。
- 运行受影响的 Rust 测试、Clippy、扩展检查、架构检查和 VSIX 检查。

## Supersedes

None.

## Removal plan

在同一变更中替换仅显式触发的 Extension Host 断言和定时器驱动的 `hasCompletion` 请求。不得保留字母
LSP 触发器、全局快速建议覆盖或客户端侧识别器作为回退。
