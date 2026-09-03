# ADR 0031: Wake Docs 统一页面身份、语法所有权与源码溯源

- Status: accepted
- Date: 2026-09-02

## Context

Wake Docs 曾在扫描、registry、静态 route shell 和浏览器运行时分别从文件名推导 `id`、`file` 与
`slug`。文件名包含空格、百分号或 Unicode 时，磁盘路径、URL pathname 与客户端查找会采用不同的
编码状态；非 UTF-8 路径还可能经 lossy 转换碰撞。标题 metadata 与 renderer 也各自分配 heading ID，
重复标题因而出现不同锚点。

MDX 的 ESM 过去由逐行启发式提取，再用正则替换所有看似相对路径的字符串。这既无法正确拥有多行
import/export，也可能修改普通字符串、模板或注释。页面 Source Map 则把 synthetic wrapper 的每一行
粗略映射到 MDX，并写入绝对 checkout 路径和固定的 `page.tsx` 文件名；这些映射无法作为诊断证据，
相同内容在两个 checkout 下也不会产生相同字节。

本决策扩展 [ADR 0002](0002-docs-runtime-and-content-contracts.md) 的 Docs 产品边界，不替代其运行时
模式与内容契约。

## Decision

每个 MDX 文件只构造一次 typed `PageIdentity`。它拥有 root-relative source file、generated module/map、
页面 ID 与 typed `RoutePath`。`RoutePath` 同时保存 decoded 文件系统表示和 canonical encoded URL 表示；
URL 每段只保留 RFC 3986 unreserved 字节，其余 UTF-8 字节用大写十六进制 percent encoding。`/` 只作
段分隔。非 UTF-8 页面段或 normal component 内的平台相关反斜杠直接产生诊断。Registry、静态 route
shell、Docs checker 与浏览器 runtime
消费该身份或共享 codec，不再各自派生。

启用 markdown parser 的 `mdx_esm` construct，由 `MdxjsEsm` node、position 与 stops 拥有 ESM 范围。
Wake ECMA parser 提取 typed dependency；重写器按 dependency kind 和 token 语法位置选择 side-effect
import、import-from、export-from 或字面量 dynamic import 的 module specifier。普通字符串、模板、注释、
import attribute value 与非字面量 dynamic import 不参与重写。

一次 `HeadingPlan` 顺序分配所有 heading ID，metadata 与 renderer 只消费该计划。页面生成使用 provenance-
aware writer：synthetic wrapper、metadata、indent 和 trailer 不映射；原样 ESM 片段按 MDX stops 映射，
specifier replacement 与渲染节点只在可证明的源 token/node 起点建立低分辨率映射，并在同一 generated
line 用 generated-only segment 终止。Source Map 的 `file` 和 `sources` 取自 `PageIdentity` 的相对路径。

## Invariants

- `PageIdentity` 是 `id`、source file、generated file、decoded route 与 encoded route 的唯一派生点。
- 非 UTF-8 route segment 或组件内容中的反斜杠必须失败；禁止 lossy 身份或路径碰撞。
- canonical URL 每段只含 unreserved 字符或大写 `%XX`；调用方不得对 canonical route 二次编码。
- 静态 shell 使用 decoded segment，浏览器 history/lookup 使用 encoded route；两者来自同一 `RoutePath`。
- 只有 typed import/export/dynamic-import module specifier token 可被重写；同值诱饵不得改变。
- heading metadata、DOM `id` 与 fragment lookup 使用同一 allocator 结果。
- synthetic generated 区间没有伪 source mapping；任何 mapped segment 都能指回具体 MDX node/token。
- checkout 根路径不得进入页面模块、registry 或 Source Map，等价 checkout 产物必须字节一致。

## Evidence

- `crates/wake_docs/src/lib.rs` 中的 `PageIdentity`、`RoutePath`、`HeadingPlan`、typed ESM rewrite 与
  `ModuleWriter`。
- `crates/wake_docs/runtime/routes.mjs` 被生成 runtime 与 `scripts/check-docs.mjs` 共同消费。
- `crates/wake_docs/runtime/app.tsx` 只通过共享 route codec 生成 href、解析 pathname 和查找页面。
- `crates/wake_docs/src/lib.rs` 的 multiline ESM、同值诱饵、重复 heading、Unicode/space/hash/percent、
  non-UTF-8 与跨 checkout Source Map 回归测试。
- `crates/wake_docs/runtime/search.test.mjs` 的 direct refresh、client navigation 与 no-double-encoding 测试。

## Consequences

`RouteInfo.slug` 现在是 canonical encoded URL；registry 额外携带 typed `routePath`，但现有公开
`RouteInfo` 字段形状保持不变。静态托管仍在 decoded 文件名下写 shell，因此用户可直接刷新 Unicode
或空格页面。`wake_docs` 增加对 Wake ECMA parser/lexer/AST/common crates 的内部依赖，以换取语法所有权。

Source Map 有意采用可证明的低分辨率；renderer 变换后的整段 JSX不会伪装成逐 token 精确映射。若未来
需要更高精度，必须从 typed MDX/ECMA provenance 增量扩展，不得恢复逐行猜测或全局文本 lexer。

## Validation

- `cargo test -p wake_docs`
- `cargo clippy -p wake_docs --all-targets -- -D warnings`
- `node --test crates/wake_docs/runtime/search.test.mjs`
- `corepack yarn docs:check`
- `corepack yarn docs:build`
- `corepack yarn architecture:check`

## Supersedes

None.

## Removal plan

同一切片删除逐行 `extract_esm`、全局 regex import rewrite、重复 heading allocator、绝对路径/固定文件名
Source Map 和 runtime/checker 的独立 route 归一化。不存在双路径兼容层；替代本决策必须同时迁移生成器、
静态 shell、浏览器 runtime、Docs checker 与验证。
