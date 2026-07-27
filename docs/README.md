# Wake 文档

本文档集描述仓库在 2026-07-28 的实际状态，而不是远期愿景。结论来自源码、Cargo 清单和本地验证。

## 阅读顺序

1. [ARCHITECTURE.md](ARCHITECTURE.md)：当前系统边界、依赖方向和构建数据流。
2. [AUDIT.md](AUDIT.md)：已确认的问题、风险等级和证据。
3. [ROADMAP.md](ROADMAP.md)：按风险排序的架构演进方案。
4. [TESTING.md](TESTING.md)：本地与 CI 应执行的质量门禁。
5. [PERFORMANCE.md](PERFORMANCE.md)：可复现的构建性能和产物体积基线。

## 当前结论

Wake 已经不是纯设计原型。它具备从解析、转换、链接到输出、缓存和开发服务器的完整垂直切片，工作区测试全部通过。当前最需要解决的不是继续增加 crate，而是收敛执行模型：

- `Bundler` 与 `IncrementalBundler` 是两套编排路径，能力和正确性容易漂移。
- `IncrementalBundler` 已使用 `wake_turbo`，但不是所有阶段都被统一建模为任务。
- 持久化缓存保存摘要与产物，命中时会缺少活跃性及 concat 分析，导致优化结果依赖缓存冷热状态。
- `wake_bundler` 同时承担扫描、解析调度、图构建、优化、chunk、runtime 生成和缓存协调，边界过宽。
- 测试通过，但仓库声明的 `cargo clippy --workspace --all-targets -- -D warnings` 当前失败。

## 文档原则

- 以源码为事实来源；文档不承诺尚未实现的能力。
- 架构决策必须说明不变量、失败模式和验证方法。
- 性能目标必须配套可复现基准，不使用无测量依据的数字。
- 新设计先消除双路径和正确性分叉，再考虑微优化。
