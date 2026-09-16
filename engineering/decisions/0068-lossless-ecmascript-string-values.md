# ADR 0068: 无损 ECMAScript 字符串值

- Status: proposed
- Date: 2026-09-16

## Context

ROADMAP R6 和 lint P1 要求保留任意 UTF-16 码元。当前 lexer 的字符串解码、AST 的 Atom、
优化 IR 的 String 都要求 Unicode 标量；孤立代理项被拒绝，移除诊断会造成静默丢值。
标识符、文件路径和源码字节仍有不同的合法性约束，不能为了运行时字符串放宽这些边界。

## Decision

拟由 `wake_common::JsString` 表示 owned ECMAScript 字符串值，保留 UTF-8 的常见路径，
仅含孤立代理项时使用 UTF-16 存储。构造规范化保证同一码元序列具有同一内容身份；比较、
哈希、长度和拼接以码元为准。UTF-8 转换必须可失败，不提供隐式替换、丢弃或占位字符转换。

lexer 增加无损解码入口；随后将 AST 字符串值和模板 cooked 值迁移到独立的驻留句柄，
优化器使用 owned 值，emitter 对孤立代理项输出 Unicode 转义。标识符和模板 raw 继续使用
UTF-8 Atom。模块路径、源事实和持久化消费者逐个采用明确的转换或编码契约；不得假定
运行时字符串总能转换为文件系统路径。句柄不得进入持久缓存或跨进程协议。

此 ADR 保持 proposed，直到 lexer、AST、优化器、输出和缓存消费者完成迁移及运行时验收。
import attributes 的键与模块导出名分开建模：标识符拼写仍为 Atom，字符串键和值为 JsAtom；
源码事实和 owned 模块请求采用 JsString。优化 IR 的字符串属性键使用既有 StringLiteral 值节点，
不得进入仅支持 Unicode 标量的名称表。模块解析仍由原解析环境拥有，不将属性值当文件路径。
源码模块事实与冻结声明请求同样使用 JsString，原始引用范围及声明模板仍为 UTF-8 源码。
声明渲染回调携带 parser-owned 请求事实以读取无损值，tsdoc 不直接依赖 common 或自行扫描。
外部字面量未重写时保留原文，相对请求必须在文件系统边界通过
显式 UTF-8 检查。lint 已知字面量无法进入解析环境时产生 unresolved，不伪造动态未知请求。
运行时值链路已接通；静态模块元数据和原生类型服务字符串协议尚需逐项完成迁移/验收，
不能由基础类型或运行时局部证据推导完整字符串兼容。

## Invariants

- 任意 u16 序列可往返，包括孤立高/低代理项、逆序代理项、NUL 和非 BMP 字符。
- 代理对、直接非 BMP 字符和等价转义具有相同值；真实替换字符不能与孤立代理项相等。
- 拼接可以跨边界形成代理对，不能改变码元顺序；长度是 UTF-16 码元数。
- 源码 Span 继续以 UTF-8 字节计量，字符串码元长度不用于源码编辑坐标。
- 标识符验证与字符串码元验证分离，不能接受代理项作为标识符。
- 各消费者只能接受其可无损表示的输入；未迁移的边界明确诊断或保持事实未知，不静默转换。

## Evidence

旧 `crates/wake_ecma_lexer/src/lexer.rs` 的 char 解码路径无法保留孤立代理项，已由码元值解码替换。
`crates/wake_common/tests/js_string.rs` 与 lexer 无损解码回归验证基础值契约；
`crates/wake_ecma_codegen/tests/typed_pipeline_acceptance.rs` 覆盖 AST、拼接、属性键、模板
raw/cooked、装饰器、enum/JSX 和普通/压缩/模板降级；
`crates/wake_ecma_parser/tests/template_escapes.rs` 验证 tagged 非法转义的缺失 cooked、
无标签与类型模板的早期错误，以及嵌套/推测解析；对应 Node 对照验证 raw 和 undefined。
`crates/wake_bundler/tests/utf16_cache.rs` 对照冷缓存、全新会话命中与内容失效的运行时结果；
`crates/wake_lint_core/tests/utf16.rs` 覆盖重复项、truthiness 和 ARIA 的码元边界。
`crates/wake_lint_core/tests/typed_strings.rs` 验证源值与类型字面量之间的码元身份，
原生类型服务测试覆盖 JSON 替换造成的不同代理项碰撞、实际替换字符、跨文件长字面量，
并验证尚未恢复的枚举值/属性名称会在不正确等价证明前失败。
`crates/wake_ecma_parser/tests/utf16_attributes.rs` 验证原始属性事实、同值键与导出名称约束；
codegen 的 Node 模块链接对照覆盖普通/优化/映射 ESM 产物的实际属性码元，模块请求与项目
回归覆盖动态属性、重复导入、缓存命中和属性值变化后的失效。
`crates/wake_ecma_parser/tests/utf16_type_modules.rs` 覆盖 ambient/类型导入、声明事实及擦除等价；
semantic 验证无损模块值控制类型和值声明合并。tsdoc 验证外部引用保留和重写不再读盘，
相对无损值在无法表示为 UTF-8 路径时于探测前失败；lint 项目验证该限制产生 unresolved。

## Consequences

普通 UTF-8 值继续使用紧凑共享存储；含孤立代理项的值需要单独的慢路径。消费运行时
字符串的 Rust API 必须明确处理无法表示为 UTF-8 的情况，不能对这些值使用显示文本身份。
迁移会改变 parser/optimizer 输入契约，正式启用时更新相关 pipeline 和缓存身份。

## Validation

- `cargo test -p wake_common --test js_string --locked --offline`
- `cargo test -p wake_ecma_lexer --test lossless_strings --locked --offline`
- lexer/parser/transform/minify/codegen/compiler 和 lint 聚焦回归及 Node 运行时等价测试。
- `corepack yarn architecture:check` 与 `corepack yarn architecture:test`。

## Supersedes

None.

## Removal plan

UTF-8-only 字符串解码桥已移除；标识符保留独立 Unicode 标量验证。静态模块元数据的
显式 UTF-8 转换由 parser 拥有，迁移完成后移除此限制，不宣称超出当前边界的完整支持。
