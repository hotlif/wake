# ADR 0007: 在编辑器产品下层拥有 CSS 语言智能

- Status: superseded
- Date: 2026-08-14
- Superseded by: [ADR 0009](0009-semantic-css-highlighting.md)

## Context

`@crab-dev/css` 已由 `wake_css_in_js` 统一拥有构建时语义，但 Wake 尚无编辑器语言服务。TextMate
文法可以为常见标签拼写着色，却无法证明别名指向 `@crab-dev/css` 的导入，也无法区分遮蔽它的局部
绑定。将编辑器会话、工作区文件或 LSP 协议放入编译器或打包器，还会反转现有依赖方向。

## Decision

`wake_css_language` 拥有独立于文件系统的 Crab CSS 标签模板发现、虚拟 CSS 文档、宿主到虚拟源码映射
及 CSS 编辑分析。它使用现有解析器、语义模型和 `wake_css_in_js` 契约，但不依赖打包器、解析器、
LSP 或编辑器。

`wake_css_lsp` 是产品边缘，拥有 LSP 传输、文档版本、有界缓存、工作区解析和依赖感知的已保存文档
分析。精确的 Crab 编译器诊断来自 `wake_css_in_js`；语言服务不复制其静态求值规则。

`editors/vscode-css` 是轻量工作区扩展，提供声明式 TextMate 高亮并启动特定目标的 Rust 服务器。
JavaScript/TypeScript 的定义、引用和重命名行为仍由 TypeScript 拥有。

## Invariants

- 只有从 `@crab-dev/css` 导入的语义绑定才能激活 Crab CSS 行为。
- `wake_css_language` 不拥有文件系统、解析器、打包器、编辑器或协议。
- 构建时静态求值和 `CRAB_CSS_*` 诊断仍由 `wake_css_in_js` 拥有。
- 宿主编辑绝不跨越或修改 `${...}` 插值。
- LSP 位置使用从零开始的 UTF-16 单位，并保留宿主文档标识。
- 为旧文档版本计算的结果绝不发布到新版本。
- 运行时缓存有界，缓存标识包含文档版本、配置和已保存的依赖输入。
- 每个发布的 VSIX 恰好包含一个平台服务器二进制文件。

## Evidence

- `crates/wake_css_in_js/src/lib.rs`
- `crates/wake_common/src/source.rs`
- `engineering/CRAB_CSS.md`
- `engineering/CSS_LANGUAGE_SERVICE.md`
- `engineering/architecture-boundaries.json`
- `crates/wake_css_language/src/tests.rs`
- `crates/wake_css_lsp/src/lib.rs`
- `editors/vscode-css/test/`
- `.github/workflows/vscode-css.yml`

## Consequences

Wake 获得可复用的 CSS 分析层和原生编辑器产品，同时不让打包器依赖编辑器。Rust 服务器和生成的
CSS 事实数据扩大了构建与发布范围。TextMate 为规范拼写即时着色，语义 token 则在分析后提供精确、
感知别名的着色。

扩展版本独立于 Wake。`0.1.x` 支持 VS Code 1.96 或更高版本及
`@crab-dev/css >=0.1.0 <0.2.0`；不受支持的包版本仍保留语法着色，但会禁用精确编译器诊断并给出
可操作的警告。

## Validation

- 每次更改 crate 边界后运行 `npm run architecture:check`。
- 在 `wake_css_language` 中测试别名、遮蔽、不完整语法、插值映射、CRLF 和 UTF-16 位置。
- 在 `wake_css_lsp` 中测试 LSP 初始化、文档生命周期、过期结果抑制和协议功能。
- 为发布矩阵中的每个受支持目标构建并检查一个 VSIX。
- 运行工作区测试、Clippy、编辑器包检查和 `git diff --check`。

## Supersedes

None.

## Superseded by

[ADR 0009](0009-semantic-css-highlighting.md) 以 AST 和语义绑定驱动的 token 取代基于拼写的
TextMate 高亮，并将其作为唯一高亮权威。

## Removal plan

此前没有语言服务器。用于验证恢复能力的实验或临时解析器必须在接受本决策前删除；不得保留兼容
crate、旧编辑器目录或第二个 CSS 包说明符。
