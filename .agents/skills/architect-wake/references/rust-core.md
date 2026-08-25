# Rust 核心与打包器

事实来源：`engineering/ARCHITECTURE.md`、`engineering/DESIGN.md`、`engineering/TESTING.md`、受影响的 `Cargo.toml`、源码和测试。

- 绑定敏感行为使用语义标识；arena 引用和进程本地 ID 不跨持久化边界；转换保留诊断与 source map 所需 span。
- Scan、Link、Emit 所有权明确；tree shaking 保留有意副作用；ESM/CJS 循环、顶层 await、concat 和动态 chunk 保持单次执行与导出标识。
- crate 边界变更须引用 `proposed` 或 `accepted` ADR，并运行 `npm run architecture:check`。

最低验证：受影响 crate 的测试和快照；运行生成产物验证运行时声明。按风险追加 workspace test、fuzz smoke、Miri、Loom、完整 `wake_bundler` 测试或 fixture 构建。
