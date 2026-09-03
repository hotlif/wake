# Wake 历史 Phase 与 Gate 索引

本文件为源码中的 `PLAN §…`、Phase 和 Gate 引用提供稳定解释。它记录已经落地的工程阶段，不是当前排期；未完成事项只在 [ROADMAP.md](ROADMAP.md) 维护。

# 0. 原型与关键路径

## 0.5 Arena AST spike

验证自引用 `ModuleAst` 能安全进入长生命周期任务值。实现保留在 `wake_ecma_ast` 示例和 holder 中，Miri 为常驻门禁。

## 0.6 红绿失效 spike

以单线程参考模型验证 revision、深校验和早期截断，保留在 `wake_turbo::spike` 作为并发引擎的对拍基准。

# 1. 工程基线与 Gate-1

## 1.2 Token snapshot

覆盖字面量、模板、运算符、ASI、类私有名、正则和 JSX 边界。

## 1.5 Fuzz

常驻随机 smoke 保证 lexer 不 panic；长时间 cargo-fuzz 作为人工深度验证，不在每次 CI 中运行。

## 1.6 Lexer benchmark

Criterion 测量词法吞吐。旧设计数字不作为当前发布门禁，基线策略见性能文档。

Gate-1 当前含义：编译核心可解析目标语法、错误可恢复、snapshot/fuzz smoke 通过且 benchmark 可编译。

# 2. 编译器阶段

Phase 2 覆盖 AST、parser、semantic、transform 与 codegen。源码中的 “Phase 2.x” 表示历史落地顺序，不代表功能仍处于原型状态。

## 2.5 增量引擎

### 2.5.1 任务宏

`#[wake::task]` 把纯函数登记为可记忆任务。

### 2.5.2 任务句柄与 slot

`TaskId`、`Vc<T>`、类型擦除 slot 和重算注册表构成引擎核心。

### 2.5.3 红绿校验

浅校验、深校验、重算和输出指纹比较实现跨 revision 早期截断。

### 2.5.4 Generation 与取消

引擎本体没有通用 generation 抢占；产品层通过 `CancellationToken`/`AbortSignal` 在安全点协作取消。未来若引入抢占必须保持资源释放和缓存事务边界。

### 2.5.5 并发整合

分片 slot、per-task single-flight 与工作窃取扇出已经实现并用于增量打包扫描。

### 2.5.6 Loom 与循环

Loom 穷举 single-flight 交错；任务栈检测直接循环依赖并拒绝执行。跨线程动态环仍需依赖领域 DAG 约束和回归测试。

### 2.5.7 调度 benchmark

Criterion 测量任务调度与命中成本；旧的固定微秒目标不作为无环境说明的门禁。

Gate-2 当前含义：红绿对拍、循环检测、并发压力和 Loom 通过，并保留无增量纯并行降级模式。

# 3. 打包阶段与 Gate-3

## 3.2 增量打包接入

`IncrementalBundler` 把并行 scan、摘要、codegen 体和失效接入构建会话。第二次无变化构建、单文件变化和共享依赖均有回归测试。

Gate-3 当前含义：同步与增量路径能构建 fixture，缓存命中不改变产物语义，生成结果可执行。

# 4. Transform 与 Source Map

TypeScript、JSX、浏览器目标转换和精确 Source Map 已进入应用层。`--sourcemap` 可与生产压缩和代码
分割同时启用；映射采集不改变不含 trailer 的 JavaScript payload。

# 5. Dev Server 与 Live Reload

开发服务器、文件监听、代理、Live Reload WebSocket、重建指标和 TTY/plain 终端界面已经接入 CLI 与
Node API。当前浏览器能力是成功重建后的整页刷新，不是模块热替换。

# 6. 生产优化

## 6.3 CSS 与资源

覆盖 CSS 抽取、CSS Modules、内联/独立资源、HTML 和 manifest。

## 6.5 代码分割

动态 import 生成 async chunk，并传播 public path 与顶层 await 生命周期。

## 6.6 Tree Shaking

从模块导出可达性发展为绑定级活跃性，并为 persistent cache 保存恢复所需摘要。

# 7. 工程化

## 7.1 持久化缓存

缓存不保存源码、路径或文件 stamp；新进程先读取并哈希真实 loader 输出，再按内容身份复用
依赖/活跃性/concat 摘要、优化事实和原子 body/mapping 组。配置指纹防止跨选项错误复用，schema 13
事务边界与诊断契约见 [ADR 0034](decisions/0034-transactional-persistent-cache-boundary.md)。

## 7.2 输出发布

Wake-owned 目录产品通过 ownership marker 与 staging/rollback 发布；精确文件产品另以 reader
provenance 保证输入/输出物理分离，并把 bundle JavaScript 与启用时的 map 作为一个事务集合提交。
边界见 [ADR 0026](decisions/0026-owned-failure-atomic-output-publication.md) 与
[ADR 0036](decisions/0036-input-disjoint-exact-output-transactions.md)。

Phase 7 还包括 Node 发布、平台包、终端体验、文档系统和性能工作。当前状态与后续验收分别见 [AUDIT.md](AUDIT.md) 和 [ROADMAP.md](ROADMAP.md)。
