# ADR 0043: React 单模块编译边界

- Status: accepted
- Date: 2026-09-03

## Context

Wake 原先由 `wake_bundler`、`LibraryGraph` 和 `wake_js_runtime` 分别直接组合 parser、optimizer 与
codegen。React automatic JSX runtime 的规则也部分留在 parser 的语法编排中。这样既没有一个可供
Rust 调用方复用的 Babel-like 单模块入口，也容易在抽离过程中形成第二条生产编译管线。

当前已实现能力只有 JavaScript、TypeScript、JSX、TSX 与 React-compatible automatic runtime。
Vue 和 AngularJS 尚无已验证的语法、组件模型、样式或运行时契约，因此现在建立统一 framework trait、
枚举或注册中心只会冻结猜测出来的抽象。

## Decision

1. parser 继续拥有 JSX grammar、作用域、Span、props/spread、开发 source 信息、collision-free 本地名、
   import 插入位置和依赖记录；`wake_ecma_transform` 拥有 React automatic-runtime 计划、runtime 路径、
   固定候选 binding 顺序、实际 helper usage 以及最终调用 ABI 的 AST helper。
2. 新增不发布的 `wake_compiler_core`，用同一个进程内 `Interner` 提供 `parse_module`、
   `optimize_module`、`emit_module` 三个纯 CPU
   阶段。它不拥有文件系统、配置发现、Turbo task、持久缓存、`BuildSession`、Chunk、CSS 或 runtime
   assembly。
3. finalize 是 `emit` 内部的瞬时步骤，不成为公开 API、独立 task 或缓存层。Bundler 继续拥有现有
   parse/optimize/body/map 的任务身份、one-shot/retained 生命周期和 sealed-trivial fast path。
4. 新增不发布的 `wake_compiler` 作为同步、线程安全的单模块 facade。输入借用源码；输出只包含 owned
   JavaScript、detached V3 Source Map、带来源的模块请求与非错误诊断，不暴露 AST、Atom、Interner、
   `SymbolId` 或 `NodeId`。
5. `transpile_module` 固定不压缩。未来压缩只能由语义明确的独立 `minify_module` API 引入，不能改变
   transpile 默认行为或增加模糊布尔开关。
6. Preserve ESM 与可靠的单模块 CommonJS lowering 走 fallible codegen。top-level await、import
   attributes、`import.meta` 以及需要 graph-owned 名称消歧的 `export *` 在公开 CommonJS 路径 fail
   closed；存在错误时不返回可执行 code 或 map。`export *` 的完整 Bundle 语义仍由
   [ADR 0042](0042-linker-owned-export-star-resolution.md) 的 linker-owned 计划负责。
7. Bundler、`LibraryGraph` 与 `wake_js_runtime` 迁移为消费 compiler core；compiler 不反向了解这些
   消费者。迁移不得增加二次 parse、完整 AST clone 或 optimized IR clone。
8. 当前不创建 Vue/AngularJS crate、占位 frontend、统一 `FrameworkAdapter`、`Framework` 枚举或插件
   注册中心。未来前端以独立 ADR 和独立入口产生普通 JS/CSS 模块，再复用 compiler/bundler。

## Invariants

- Source Map 开关不改变 JavaScript 输出字节；map 保持 detached，且 CRLF、emoji 与非 BMP 来源使用
  UTF-16 坐标。
- React production/development runtime、fragment、key、children、spread、自定义 import source 与
  collision-free helper 名称保持 parser/transform characterization tests 的现有语义；runtime import 只包含
  lowering 实际使用的 helper，但仍只有一条 runtime dependency。
- 每次 optimize 只消费同一个 backend/interner 所产生的 parsed owner；公开结果在 backend、interner
  和输入源码销毁后仍可读取。
- parse、optimize、emit 各自保留版本身份；不存在一个使全部缓存同时失效的全局 compiler version。
- Bundler 独占依赖图、缓存、Chunk、CSS 注入、concat、runtime assembly 与最终 Source Map 合并。
- `wake_compiler` 仅依赖 `wake_compiler_core`；两者都不依赖文件系统、配置或产品层。

## Evidence

- `crates/wake_ecma_parser/src/jsx.rs` 与 `crates/wake_ecma_transform/src/lib.rs` 分别实现语法上下文和
  React automatic-runtime plan/call builder。
- `crates/wake_compiler_core/src/lib.rs` 提供 owned 诊断/依赖事实、结构化 error kind 和 canonical
  `parse_module`/`optimize_module`/`emit_module` backend。
- `crates/wake_compiler/src/lib.rs` 提供 borrowed-input/owned-output 的 `transpile_module` facade。
- parser、transform、codegen、compiler core/facade 的单元与集成测试覆盖 JSX 等价、fallible emit、
  Source Map、并发 owned 输出和 CommonJS fail-closed 语义。
- `engineering/architecture-boundaries.json` 机器检查两个新 crate 的出边。

## Consequences

Rust 调用方可以在不创建 Bundle、Chunk 或文件系统 session 的情况下完成单模块 React/TypeScript
转译，三个现有生产消费者则复用同一个 core。代价是 core 需要维护稳定的 owned DTO seam，公开 facade
也会保守拒绝依赖图或宿主运行时才能正确实现的 CommonJS 语法。crate 在 API、性能和发布流程分别达到
门禁前保持 `publish = false`。

## Validation

- `cargo test -p wake_ecma_parser --lib`
- `cargo test -p wake_ecma_transform --lib`
- `cargo test -p wake_ecma_codegen --lib`
- `cargo test -p wake_compiler_core -p wake_compiler`
- `cargo test -p wake_bundler --lib`
- `cargo test -p wake_bundler --test one_shot`
- `cargo test -p wake_js_runtime --lib`
- `corepack yarn architecture:check`
- `cargo bench -p wake_ecma_parser --bench parser`
- `cargo bench -p wake_compiler --bench transpile`
- `cargo bench -p wake_bundler --bench bundle`
- `cargo fmt --all -- --check`
- `cargo clippy -p wake_compiler_core -p wake_compiler -p wake_bundler -p wake_js_runtime --all-targets -- -D warnings`

## Supersedes

None.

## Removal plan

删除 Bundler、`LibraryGraph` 与 `wake_js_runtime` 中直接组合 parser/optimizer/codegen 的生产路径；crate
内部测试 helper 只有在验证同一 backend seam 时才能保留。以后若实现 Vue 或 AngularJS，由新的需求和
ADR 决定其独立前端边界，不通过修改本 ADR 的 React 语义来扩展。
