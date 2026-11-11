# ADR 0079: 同步 ESM 库输出与源码映射组合

- Status: accepted
- Date: 2026-11-11

## Context

单文件库需要可被标准 `import()` 消费的导出，以及由调用方提供的 banner/footer。
现有 factory runtime 不能证明任意导出的 live binding；把 namespace 属性复制成 ESM 导出会
静默改变可变绑定语义。输出前缀还会移动每个生成位置，必须与 source map 同步。

## Decision

1. `wake_bundler` 拥有 `ModuleFormat::Esm`。第一版只接受同步、无外部依赖的模块图，入口
   直接导出的 const 标识符、未被赋值的函数/类和 default 表达式。可变绑定、解构导出、导出
   列表、再导出、CommonJS 全局、require、eval、import.meta 和顶层 await 均明确拒绝。
   判定只读取原始 AST、语义绑定和依赖事实，不从生成 JavaScript 反推事实。
2. ESM 外层仅将已证明不变的入口属性发布为原生导出，不写全局 namespace。现有 IIFE/CJS
   默认值与语义保持不变。Browser/Node 均可显式请求 ESM；ESM 工厂的顶层 `this` 为 undefined。
3. `wake_app::BundleOptions` 拥有字符串 `banner` / `footer`，Rust CLI、Node DTO 和 TypeScript
   契约直接传递。两者按独立行添加于最终代码前后，不参与模块编译缓存身份。
4. banner 的生成行数进入 source map 偏移；wrapper 和 footer 不映射到源码。JS/map 仍由
   ADR 0036 的完整集合事务发布，返回的内存代码和文件字节一致。

## Invariants

- 无法证明 ESM 导出稳定时构建失败；不得用 snapshot 冒充 live binding。
- 模块体的映射以结构化位置产生，banner 只平移生成行，不改来源位置或内容。
- 默认 CJS/IIFE 和精确输出发布所有权不变。

## Evidence

- `crates/wake_bundler/src/esm_bundle.rs`：原始 AST 与绑定写入验证。
- `crates/wake_bundler/src/incremental.rs`：格式诊断、结构化输出位置。
- `crates/wake_app/src/lib.rs`：输出组合、Node 实际执行与 source map 消费测试。
- `crates/wake_node/src/lib.rs`、`npm/wake/index.d.ts`、两套 CLI：跨层选项。

## Consequences

插件的静态 activate/deactivate 导出可以直接作为 ESM 加载；需要动态导出或宿主 external 的
调用方继续使用 CJS，直到原生 live binding / external linking 的独立设计获验证。

## Validation

`cargo test -p wake_app --lib bundle_`、`cargo test -p wake_node --lib bundle_`、
`cargo test -p wake_cli --bin wake bundle_cli_accepts_esm`；Node 执行 minify 开关和持久缓存两轮
ESM 产物，source map consumer 校验 LF/CRLF/CR/Unicode banner 后原始异常行。
构建当前 wake_node 后以 `WAKE_NATIVE_PATH` 指向该 addon，执行
`node --test --test-name-pattern "bundle ESM and affixes" npm/wake/test/api.test.mjs`，覆盖 API
与 npm CLI（PnP 工作区使用其 loader）。`corepack yarn npm:typecheck:wake`、
`corepack yarn architecture:test`、`corepack yarn architecture:check`。

## Supersedes

None.

## Amends

- [ADR 0008](0008-node-library-bundle.md): 扩展其仅 CJS 的首版 Node 格式范围，增加受限同步 ESM 和应用层 banner/footer 组合；平台解析、缓存身份、壳层和精确发布所有权不变。

## Removal plan

无临时桥接；未来扩展 ESM 子集必须先证明相应导出和宿主语义。
