# 架构改进路线

路线按正确性风险排序，不按功能吸引力排序。

## 当前进度（2026-07-28）

- P0：已完成。fmt、check、Clippy `-D warnings` 与 workspace 测试通过。
- P1：已完成第一步。新增 `BuildOptions`、`BuildRequest`、`BuildSession`，旧 `Bundler`
  已作为兼容适配器转入同一增量管线。
- P2：已建立冷/热 Tree Shaking 产物一致门禁。完整分析 DTO 持久化仍待实现；当前在需要
  liveness/concat 信息时重新解析，以正确性优先。
- P3–P7：待后续迭代。

## P0：恢复可信门禁

完成标准：

- `cargo fmt --all -- --check` 通过；
- `cargo check --workspace --all-targets` 无非预期警告；
- `cargo clippy --workspace --all-targets -- -D warnings` 通过；
- `cargo test --workspace --no-fail-fast` 通过；
- 修正 `toml` 版本约束警告。

不要在此阶段做大规模重构；先建立可靠基线。

## P1：统一构建入口

引入内部 `BuildSession`，让一次性构建和增量构建都调用：

```rust
pub struct BuildRequest {
    pub entry: PathBuf,
    pub options: BuildOptions,
}

pub struct BuildSession { /* private services */ }

impl BuildSession {
    pub fn build(&self, request: BuildRequest) -> Result<BuildOutput, BuildFailure>;
}
```

迁移顺序：

1. 为 `Bundler` 与 `IncrementalBundler` 建立行为对照测试。
2. 把共同的纯函数阶段抽出，不复制状态。
3. 让 `Bundler` 成为创建临时 session 的薄适配器。
4. CLI/dev server 只依赖 `BuildSession`。
5. 对照测试稳定后删除旧编排实现。

完成标准：同一请求只有一条 Scan/Link/Emit 实现路径。

## P2：定义版本化任务键与缓存 DTO

为 resolve、parse、analyze、link、emit 分别定义可审查的 key，而不是散落的哈希拼接。顶层 fingerprint 至少组合：

```text
schema version
tool version / algorithm revision
normalized source identity and content
normalized build options
resolver environment and PnP identity
target platform and mode
```

缓存记录只使用拥有所有权、无 arena 生命周期、无裸 Atom 的 DTO。对记录增加 checksum，损坏时回退为 miss。

完成标准：

- 冷缓存与热缓存产物逐字节一致；
- 改变任一相关配置会使命中失效；
- 损坏和旧 schema 缓存不会导致构建失败；
- 多进程并发写不会产生部分记录。

## P3：把构建拆为显式阶段

建议的内部结果：

- `ScannedGraph`：稳定模块 ID、依赖边、解析/语义摘要；
- `LinkPlan`：活跃绑定、异步边界、chunk 约束；
- `EmissionPlan`：确定排序后的 chunk 与 asset 描述；
- `BuildOutput`：最终字节。

阶段间数据尽量不可变。ID 分配、排序和路径规范化只在一个位置完成。

完成标准：每个阶段可用内存输入做单元测试，且阶段 API 不暴露调度器、锁或文件写入。

## P4：统一并发与取消

- 只有 scheduler 决定执行并发度。
- 每个任务支持成功、领域错误、取消、panic 四种终态。
- single-flight 的所有等待者在任何终态都会被唤醒。
- 新 generation 能取消不再需要的旧工作。
- 不允许任务在持有图/缓存锁时执行用户扩展代码或阻塞 I/O。

使用 Loom 覆盖 cell 状态机；用真实线程测试取消、panic 和高争用。

## P5：开发服务器事务化

每批文件变化产生单调递增 generation：

```text
collect changes -> invalidate -> build snapshot -> commit outputs -> publish HMR
```

只有最新 generation 可以 commit。静态文件和 source map 在 commit 后一起可见。连续事件进行 debounce，但不能丢失最终状态。

## P6：可观测性与性能

先记录，再优化：

- 每阶段 wall time、CPU time 和任务数；
- cache hit/miss 及 miss 原因；
- 被失效模块数与重新 emit chunk 数；
- peak RSS、arena 字节与 interner 大小；
- tree-shaking 全保留的原因。

建立固定 fixture 的冷构建、无变化热构建、单叶修改、共享依赖修改和大范围修改基准。性能回归门槛基于多次采样和中位数，不使用单次结果。

## P7：扩展能力

完成 P0–P6 后再稳定插件 API、daemon 多项目共享缓存及更激进的 minify。插件 hook 必须声明它影响的任务键、是否确定、是否可并行以及缓存序列化策略。
