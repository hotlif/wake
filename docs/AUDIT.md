# 架构与代码审计

审计日期：2026-07-28。

## 1. 验证结果

- `cargo test --workspace --no-fail-fast`：通过。
- `cargo check --workspace --all-targets`：可编译；Cargo 对 `toml = "1.1.3+spec-1.1.0"` 给出元数据被忽略的警告。
- `cargo clippy --workspace --all-targets -- -D warnings`：失败。
- 工作区审计前已有用户修改：`crates/wake_bundler/src/incremental.rs`、`crates/wake_ecma_codegen/src/lib.rs`；本次未修改。

测试覆盖数量较多，包含 bundler 端到端、Node 运行、缓存、动态 import、TLA、tree shaking、解析器、minifier 和 turbo 并发测试。这说明核心垂直链路具备较强基础，但“测试绿”不等于架构边界已经稳定。

## 2. 高优先级问题

### A1：存在两套打包编排路径

`wake_bundler` 同时公开 `Bundler` 和 `IncrementalBundler`。前者是直接执行的 Scan/Link/Emit，后者维护 engine、executor、cells 和持久化缓存。

风险：

- 新功能需要实现两次或只落在一条路径。
- 两条路径在 tree shaking、code splitting、TLA、CSS、source map 等行为上逐渐不同。
- 测试矩阵翻倍，缓存路径可能掩盖冷路径缺陷。

建议：将 `Bundler` 降为 `BuildSession` 的一次性适配器，内部始终走同一任务图。完成迁移后删除重复实现。

### A2：缓存冷热状态可能改变优化质量

`ModuleRec` 中的 `liveness` 和 `block_info` 在持久化摘要命中时可以是 `None`，调用方采用“全保留/IIFE”的保守退化。

这不会直接误删代码，但会造成：

- 冷缓存与热缓存的产物大小和结构不同。
- 性能基准不可比较，内容哈希和下游缓存命中受影响。
- “第二次构建更快”与“第二次构建优化更差”同时发生。

建议：把 link 所需语义摘要设计为稳定、带 schema 版本的缓存 DTO；或把其重建成本纳入独立任务。增加冷/热缓存产物逐字节一致测试。

### A3：缓存键缺少集中定义的不变量

实现中存在内容键、define hash、linker cells 和持久化记录，但缺少一个统一的 `TaskKey`/`BuildFingerprint` 规范，明确包含：

- 源内容与 source type；
- 规范化配置及 define；
- resolver 条件、alias、PnP 状态；
- 目标平台、开发/生产模式、minify/tree-shaking 选项；
- Wake 版本、缓存 schema、相关算法版本。

遗漏任一项都可能复用语义上过期的结果。建议让每类任务声明自己的显式 key DTO，禁止临时拼 hash。

### A4：编排器职责过重

`IncrementalBundler` 同时拥有解析调度、resolver、模块 ID、图构建、活跃性、CSS-in-JS、chunk、缓存和产物策略。大量配置 setter 与内部 cell map 使其成为“上帝对象”。

建议拆为：

- `BuildSession`：生命周期与请求入口；
- `ScanService`：resolve/load/parse；
- `LinkPlan`：图冻结后的纯分析结果；
- `Emitter`：chunk/runtime/source map/assets；
- `CacheStore`：版本化持久化边界。

拆分时保持内部 DTO 私有，避免只是把同一复杂度搬到更多 crate。

## 3. 中优先级问题

### A5：并发抽象有重叠

`wake_turbo` 同时包含任务 engine 和独立 executor；bundler 又按 BFS 层次组织并行。三层调度容易出现过度并行、线程利用不稳定及取消传播困难。

建议由一个 scheduler 拥有并发预算。图遍历表达依赖关系，不直接决定线程策略。加入并发度为 1、默认并发度和高并发度下的等价测试。

### A6：全局 interner 的隔离与回收策略不清晰

Atom 提供快速比较，但必须明确作用域。若 interner 跨 daemon 项目永久存活，字符串只增不减；若不同 interner 的 Atom 混用，数值相同不代表字符串相同。

建议让 Atom 与 `BuildSession`/daemon generation 绑定，跨持久化边界存字符串，不存裸 Atom。通过类型或 API 阻止不同 interner 域混用。

### A7：持久化格式与文件提交协议需要强化

缓存已有 magic/schema 检查，但还应记录工具版本、目标三元组（若格式相关）、配置 fingerprint 与校验和。读取损坏记录应视为 miss；写入应采用临时文件、flush 和原子 rename。多进程同时写同一 key 需要锁或 content-addressed immutable 文件。

### A8：依赖图与活跃性算法的保守策略缺少集中说明

`export *`、namespace import、dynamic import、require 和缓存缺分析时采用不同程度的 `All`。保守是正确方向，但规则散落会造成体积回归且难以定位。

建议输出可选的优化解释信息，例如“模块 X 因 namespace import 被全保留”，并对每种保守升级建立快照测试。

### A9：开发服务器与构建事务边界不够显式

文件事件可能合并、乱序或在构建中再次发生。需要 generation/revision：每次构建基于输入快照，旧 generation 的结果不得覆盖新结果；HMR 广播只发生在完整产物提交后。

### A10：诊断与失败策略仍需统一

解析/resolve 错误主要进入 `Diagnostic`，但内部仍存在 `unwrap`/`expect`。应分类为：

- 用户错误：结构化诊断，可继续收集；
- I/O/缓存错误：带上下文的可恢复错误或 cache miss；
- 内部不变量：panic，并在任务边界转换为构建失败，唤醒所有等待者。

## 4. 工程门禁问题

### A11：声明的 Clippy 门禁不成立

`-D warnings` 当前在多个 crate 失败。已观察到：

- `wake_graph`：`map_entry`、`collapsible_if`、测试中的 `single_match`；
- `wake_css_in_js`：`needless_borrow`；
- `wake_ecma_minify`：多项 `collapsible_if`、`manual_strip`、`items_after_test_module`、`approx_constant` 等。

这些大多不是功能 bug，但会让 CI 门禁失去可信度。要么修复并保持零警告，要么在 workspace lint 中对有意保留的规则作最小、带理由的 allow。

### A12：依赖版本字符串有误导性

`toml` 的 `+spec-1.1.0` 是 SemVer build metadata，Cargo 在版本匹配时忽略它并给出警告。应改成实际需要的 crate 版本约束，并通过解析行为测试保证 TOML 规范需求。

## 5. 做得好的部分

- crate 大方向合理，编译核心没有直接耦合网络/CLI。
- 文件系统抽象与内存 FS 提高了可测试性。
- arena AST holder 通过闭包访问限制引用逃逸，并有相应测试。
- chunk 确定性、缓存失效、并发 single-flight、端到端 Node 执行均已有测试。
- 解析失败与未解析依赖能够形成诊断，而不是简单崩溃。
- 对未知活跃性采用多保留而非误删，正确性取向合理。

## 6. 总体评价

当前架构适合继续演进，但还不适合把“函数级统一增量架构”视为已完成。核心风险是同一语义存在多条执行路径和多种摘要形态。下一阶段应优先统一构建入口、缓存键和缓存 DTO，再扩展插件或更多语法能力。

## 7. 本轮修复状态

2026-07-28 已完成：

- 修复 workspace Clippy 警告并恢复 `-D warnings` 门禁；
- 修正 TOML 依赖版本警告；
- 增加普通/增量构建产物等价测试；
- 固定旧非增量路径的模块输出顺序；
- 增加持久化缓存下 Tree Shaking 冷热产物一致测试；
- 热缓存缺少语义分析时重新解析，避免优化结果随缓存状态变化；
- 引入统一 `BuildSession`，旧 `Bundler` 改为兼容适配器。

仍未完成：

- 把 liveness/concat 分析结果序列化为版本化缓存 DTO；
- CLI 与 Dev Server 从直接使用 `IncrementalBundler` 迁移到 session API；
- 统一 executor 与任务引擎的并发预算；
- Dev Server generation、取消与原子发布。
