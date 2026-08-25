# ADR 0003: 明确编译器阶段和壳层依赖

- Status: accepted
- Date: 2026-08-14

## Context

Wake 将 `wake_app` 记为 CLI 和 Node 构建行为的共同所有者，但可执行边界策略只禁止内部 crate
依赖这两个壳层。因此，壳层仍可直接依赖 `wake_bundler` 并通过架构检查。编译器组内部也没有依赖约束。

此外，`wake_ecma_parser` 还把 `wake_ecma_semantic` 重新导出为开放式兼容路径。与此同时，
上下文相关的浏览器降级转换需要有意协同，而覆盖文法和词法作用域信息仍由解析器拥有。

## Decision

使编译器依赖图可执行。`wake_common` 是工作区基础；`wake_ecma_ast` 和 `wake_ecma_lexer`
只能使用该基础；`wake_ecma_semantic` 和 `wake_ecma_transform` 只能使用公共 AST 模型；
`wake_ecma_parser` 可以使用词法分析器及 transform 的降级转换原语，但不能依赖 Semantic。

删除 `wake_ecma_parser` 对 Semantic 的重新导出。执行分析的调用方应直接依赖
`wake_ecma_semantic`。将解析时降级转换视为有意融合的前端操作：解析器拥有语法上下文，
transform crate 拥有可复用的降级规则和 AST 构造辅助函数。

允许 CLI 和 Node 壳层为显式面向编译器的命令及实验性 API 直接依赖编译器 crate。所有构建、
配置、服务器、Docs 和生命周期行为都必须经由 `wake_app`；壳层不得直接依赖编排层或其他产品 crate。

## Invariants

- `wake_common` 不依赖工作区中的其他 crate。
- Parser 不拥有或重新导出 Semantic 分析。
- Transform 辅助函数不依赖 Parser、编排层或产品层。
- CLI 和 Node 构建行为通过 `wake_app` 进入构建栈。
- 实验性编译器 API 不得创建第二套打包器、解析器、缓存或服务器路径。
- 上述每条规则都有会失败的架构检查夹具。

## Evidence

- `crates/wake_ecma_parser/Cargo.toml`
- `crates/wake_ecma_parser/src/lib.rs`
- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_cli/src/main.rs`
- `engineering/architecture-boundaries.json`
- `scripts/check-architecture.test.mjs`

## Consequences

编译器使用方应声明实际所有者，而非依赖 Parser 门面。架构策略因此更严格，未来的壳层功能必须
明确选择 `wake_app` 行为或仅编译器行为。上下文相关的降级转换仍与解析融合，因此浏览器目标变更
可能使前端任务失效；这是有意的正确性权衡，而不是未记录的阶段倒置。

## Validation

- 运行 `npm run architecture:test` 和 `npm run architecture:check`。
- 运行 Parser、Semantic、Bundler、CLI 和 Node 的检查/测试。
- 验证夹具会拒绝 `wake_common -> wake_css`、`wake_ecma_parser -> wake_ecma_semantic` 和
  `wake_cli -> wake_bundler`。
- 运行 `cargo metadata --no-deps` 并检查生成的工作区依赖图。

## Supersedes

None.

## Removal plan

以原子方式删除 Parser Semantic 门面及依赖，不保留兼容包装器。若以后拆分解析时降级转换，
必须用新 ADR 替代本决策，并在改变依赖方向前保证覆盖文法、作用域临时状态和源码映射的正确性。
