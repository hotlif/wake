# ADR 0023: Closure 风格压缩管线所有权

- Status: accepted
- Date: 2026-08-30
- Last updated: 2026-08-31

## Context

Wake 需要在普通 ESM/CommonJS 构建中同时解决固定点优化、Tree Shaking、模块实时绑定、确定性改名和
Source Map。冻结 parser AST 之外的 `Span` 决策侧表无法成为反复结构化改写后的唯一事实源，也容易让
minifier、codegen 与 bundler 分别拥有一部分优化语义。

Google Closure Compiler 的显式 pass 排序、统一变更跟踪和固定点迭代提供了适合 Wake 的调度思想。
本决策只借鉴这些架构原则：Wake 不实现 Closure `ADVANCED`、Closure modules、externs、类型驱动优化、
whole-world 属性 flattening 或逐字输出兼容。Wake 不复制、vendor 或下载 Closure Compiler 或其他第三方
压缩器的源码、测试、二进制和语料，也不为本重写增加 Cargo/Yarn 依赖。

无持久缓存的 2k modules 冷构建表明，固定点本身之外还有一类跨层固定成本：已验证 arena 被重复全量
校验或复制、exact linker 结论到达后仍建立不会被消费的 semantic model、单 generation 调用仍记录红绿
依赖，以及数千个短任务、解析器锁和重复文件探测。此类优化必须按调用生命周期和显式证明收窄；不能
以关闭增量语义、改变缓存身份、跳过诊断或为 benchmark 增加专用输出路径来换取冷构建数字。

## Decision

### 所有权

- `wake_ecma_minify` 独占拥有可变优化表示、当前树分析、结构化编辑、pass 调度、模块计划、属性改名和
  标识符改名。
- `wake_ecma_codegen` 只从最终 `TypedProgram` 发射合法 token、最短字面量和 Source Map；它不再决定
  DCE、内联、Tree Shaking、模块重写或名字。
- `wake_bundler` 只提供链接活跃性、模块解析结果和最终布局事实，消费优化器报告的保留依赖，并在
  retained graph 收敛后安排 chunk。

生产构建仍使用单一 `optimize(...) -> Result<OptimizedProgram, MinifyDiagnostic>` 边界。公开 CLI 与
Node API 继续只暴露 `minify`/`--minify` 和 `sourceMap`/`--sourcemap`，不新增 Advanced、externs 或 pass
配置，也不保留第二个名字优化开关。

### 拥有所有权的可变 IR

`crates/wake_ecma_minify/src/typed_ir.rs` 定义 `TypedProgram`。Parser AST 只作为入口 lowering owner；普通
路径所需的初始 semantic model 也只在该边界建立一次，exact semantic-free 路径则以更严格证明省略它。
此后优化器不通过 parser AST 或 source span 查询当前语义。

`TypedProgram` 以 append-only arena 持有全部可重发射语法，并使用稳定 `NodeId`、`ListId`、`NameId` 和
当前程序内的 `SymbolId` 表达结构与绑定。节点来源为 `Source`、`Derived` 或 `Synthetic`，可带最近的
source anchor。结构变更通过 typed node/list 操作提交，删除使用 tombstone。完整 `validate()` 属于所有权
边界：lowering/可信编辑后、消费式 module planning 结果、最终 seal/finalize 和测试入口验证 parent/child、
列表、节点类别与名字不变量。Typed mutator 在提交时维护局部 parent/list/grammar 不变量；同一已验证
revision 内部使用 `preorder_validated` 等入口，避免每次逻辑遍历都隐式追加一次全 arena 校验。结构变化
后必须进入下一验证或分析边界，不能把 validated epoch 跨 revision 复用。复杂语法可以保守保持原形，
但不能触发整模块切回另一条压缩路径。

Defines、CSS-in-JS 等可信表达式编辑和 binding/statement 删除由
`crates/wake_ecma_minify/src/typed_edits.rs` 解析为结构化 typed edit。编辑目标必须唯一且类型正确；冲突、
重叠、错误 owner 或不合法 removal 返回诊断，不能把任意源码字符串侧表交给 codegen。

### 当前树分析

`crates/wake_ecma_minify/src/typed_analysis.rs` 的 `TypedAnalysis::rebuild` 只读取当前 live `TypedProgram`，
对外入口先验证；拥有当前 validated epoch 的调度器使用 `rebuild_validated`，不在同一边界重复扫描 arena。
两者都完整重建当前代的：

- lexical scope、binding、读写引用与捕获关系；
- CFG 与确定初始化结果；
- 节点副作用、可能抛错、成员读取、未知调用、未解析访问与 suspend 摘要；
- symbol 逃逸、外部存储、参数传递、return/throw、别名与动态观察摘要。

Binding-sensitive pass 可以在 `TypedProgram::revision` 未变化时共享分析；结构 pass 报告真实变化后，
调度器必须在下一次 binding-sensitive pass 前重建，不能继续使用旧引用或控制流。固定点收敛时的当前
分析直接供动态作用域判断和最终改名使用。真正的 direct `eval` 与 `with` 只冻结其可见环境；其他作用域仍可使用当前树事实优化。未知调用、成员读取、
getter/Proxy、TDZ、未解析全局和用户强制转换默认可能抛错或有副作用。

### 显式 pass 与固定点

`crates/wake_ecma_minify/src/typed_pipeline.rs` 是统一调度器。一次性阶段按顺序应用可信编辑、物化
parser runtime helpers、降低装饰器、重建分析并建立 typed module plan。Minify 固定点每轮顺序固定为：

1. primitive 常量传播与折叠；
2. 条件和控制流简化；
3. 已证明封闭的函数及调用内联；
4. 单次使用变量内联；
5. 死控制流、赋值和声明清除，以及配置的 debugger/console 删除；
6. 声明和 sequence 合并；
7. late peephole。

每个结构 pass 返回实际变更数，调度器把它汇总到同一个 `OptimizeStats` change tracker。完整有序轮次
无变更才算收敛；100 轮仍变化时返回包含 pass 和轮数的 `DidNotConverge` 诊断。调度器不静默输出旧路径
或未压缩源码。最终阶段复用与收敛 revision 一致的分析，执行安全属性/私有名改名、无捕获且作用域可复用的变量槽分配和
确定性标识符改名；名字直接写回 owned name occurrences。

Statement merging 对同一 statement list 先建立一次不可变快照和互不重叠的 run 计划，再按原有从左到右
顺序提交 splice。禁止每成功合并一个短 run 就重新复制整张 list；该实现细节不改变 pass 顺序、变更计数或
固定点证明，只把长列表的规划工作约束为线性。

Bundled CommonJS 的严格 trivial-effect 证明是同一调度器内的零轮固定点结果，不是第二套 minifier。只有
module planning 已完成、可信编辑无变化、无绑定/作用域/调用/动态查找/挂起语法，且残余只包含已在紧凑
emitter normal form 中的全局副作用时，调度器才跳过 fixed-point、后续 semantic rebuild 和 mangle。任何
未列入 allow-list 的节点或证明失败都进入上述普通固定点；不能按源码长度、模块数量或 benchmark 名称
触发。

只有能证明值为 primitive 且转换不会调用用户代码的表达式才折叠。函数、变量及声明改写必须保留
`this`、`arguments`、`super`、`new.target`、默认参数、解构、await/yield、异常与求值顺序。`var` 只允许
按声明语义处理，初始化器留在原求值位置。以体积收益为条件的候选使用 typed token 成本比较，只有
不增长才提交；未获得证明的候选保持原形。

### 安全属性改名

`crates/wake_ecma_minify/src/typed_mangle.rs` 只改名类 `#private` 名称和分析证明封闭的局部对象形状。
局部形状必须由不可变 binding 持有，只存在允许的简单别名，不逃逸，并且全部访问都是静态属性名。
动态成员、spread/rest、枚举、反射、序列化、Proxy、delete、方法/accessor、未知别名、参数传递、
外部存储、export、return 或 throw 都会使该形状退出属性改名。公共类成员、`__proto__`、宿主属性与已知
协议名始终保留。

### 模块与链接

`crates/wake_ecma_minify/src/typed_modules.rs` 拥有 `PreserveEsm`、`PreserveCommonJs` 和
`BundledCommonJs` 三种内部模块模式。Bundler 只把稳定的 `(ModuleId, export names)` 活跃性传入
`OptimizeInput`。普通路径在唯一 `optimize` 边界建立一次 parser semantic model，用它同时把导出名解析为
当前顶层绑定的 `SymbolId` 并降低 `TypedProgram`；`SymbolId` 不跨 optimizer 边界，也不能写入模块图、指纹
或持久缓存。局部同名遮蔽不会因字符串相同被误根化。

Exact linker liveness 允许在降低前建立按顶层 ordinal 寻址的 `TypedLoweringPlan`。只有 bundled CommonJS
中已证明不活跃、没有别名或外部局部引用、没有 direct-eval-like 未解析访问、没有可信编辑重叠且 span
可靠的 export function 才不进入 owned arena；lowering 在原来源位置留下空 `export {}` marker，以保留
模块 ESM 身份。命名/default export 和自递归都必须按语义 symbol 证明，任一条件不明即保留声明。

更窄的 exact whole-module 路径先从 parser 语法建立 elision plan，再以 `semantic=None` 降低候选 owner。
只有 residual typed IR 通过 binding-free allow-list，且只含空 ESM marker、空语句和不包含 function/arrow/
class、call/new/tagged、await/yield/import、meta/this/super/with 或声明的表达式时，才采用 semantic-free owner
并由 `try_plan_owned_trivial_bundled_module` 直接建立 module plan。任何 live root、局部引用、alias、re-export、
import、动态作用域或新语法都拒绝该路径并回到普通 semantic + lowering；拒绝不是诊断，也不能产生部分
提交。

模块计划在固定点前把 import/export/require/dynamic-import 表达为结构化请求和绑定。生产调度器把
`TypedProgram` 移交给 `plan_owned_typed_modules`，由 planner 消费 owner 并返回完整验证的 program/plan，
不再为不可观察的 rollback 克隆整个 arena；借用式测试适配器仍以 clone-and-commit 验证失败原子性。
优化完成后 `seal_typed_module_plan` 从当前 live tree 收敛保留请求。

最终 chunk/链接事实确定后，普通 codegen 路径把 owned program/plan clone 交给消费式
`finalize_owned_typed_modules`，得到已完成最终不变量校验且不含 pending request sentinel 的
`FinalizedTypedProgram` 类型状态。若 trivial-effect 报告与 sealed revision、空 request、无顶层 await、
bundled CommonJS 和最终 `no_esmodule` 事实共同证明 finalization 为 no-op，codegen 可直接借用 sealed
`TypedProgram` 发射；调用方不能独立伪造该证明。未提供 linker liveness 的 preserved ESM 保留公开导出；
带显式空 liveness 的 linked module 可以删除全部未活跃导出。普通 ESM/CommonJS、循环依赖、顶层 await、
默认/namespace interop 和实时导出仍受 Wake 模块语义约束，不能使用 Closure whole-world 假设放宽。

### Typed codegen 与 Source Map

`crates/wake_ecma_codegen/src/typed.rs` 是优化产物的直接 emitter。Mapped 与 unmapped 入口执行同一个
token walk，mapping 只是可选 sink，因此启用 map 不得改变 JavaScript body。Source/Derived anchor 形成
源码映射；改名后的 name occurrence 把原名写入 V3 `names`；没有可靠来源的优化器/包装器标点保持
unmapped。可信结构化编辑只有在携带明确 anchor 时映射到该 anchor。

更细的来源承诺以回归测试为准。特别是常量折叠、函数体内联、实参替换和模块 wrapper 的每类
映射规则都必须分别有 generated position、source position 和 `names` 证据，不能仅凭 origin 枚举推断
已完整覆盖。Bundler 对 placement 的 token 对齐先建立 `(生成行, UTF-16 列) → 首个 token` 索引；每条
mapping 只能做精确位置或单列 separator 回退查询，禁止重新从模块首 token 线性扫描而形成二次复杂度。

### 一次性冷构建、并行与解析

CLI/library 的普通单次 `build` 显式使用 `BuildSession::new_one_shot` 和消费式 `build_once`；watch、dev
server 和任何会构建第二个 generation 的宿主使用 retained `BuildSession::new`。`BuildSession` 是产品侧
唯一 owner，底层 one-shot engine 只能执行一次 build；其 `Engine::new_one_shot` 在首个派生 query 后冻结
输入，保留 typed `Vc` 边界、并发 single-flight 和同次构建内的值共享，但不计算无后续 generation
消费者的 red/green 指纹，也不保存 recomputer 或依赖边。该公共生命周期边界由
[ADR 0027](0027-build-session-ownership-and-lifetime.md) 定义。
对应的 `optimize_one_shot` 只省略进程内 `OptimizedProgram` 的跨 generation fingerprint；owned program、
module plan、诊断、retained dependencies 和 codegen 输出必须与普通 `optimize` 相同。

在 one-shot 且不请求 Source Map 的 terminal body miss 上，bundler 直接把本轮已拥有的
`Arc<OptimizeArtifact>` 与 final linker facts 交给 `emit_body`，避免为了立刻消费同一值而重新进入 task
graph。它仍执行同一个 typed finalization/codegen 和诊断路径；启用 Source Map、持久 body 命中或长生命周期
session 时继续走既有 task/facts 边界。One-shot 是生命周期优化，不是关闭缓存的同义词，也不是第二套
构建语义。

所有 terminal body/map 请求 join 后，one-shot bundler 先消费而非 clone plan-owned optimizer/linker
artifact，并删除只服务下一 generation 的 cell/summary owner。随后把最后一个 `Arc<Engine>` 移交给
`Engine::release_one_shot`；该消费型边界用 `Arc::try_unwrap` 证明没有在飞 query，再把 input、memo、
recomputer 和 task-lock 的 128 个 shard 移交现有有界 executor 并行析构。Session engine 不进入该路径。
Wake CLI 使用 workspace 已批准的 mimalloc 作为进程全局分配器；library/Node 宿主不被 CLI 的 allocator
选择隐式接管。

CPU 型 parse/optimize/body 请求按输入顺序切成至多 `workers × 32` 个连续批次，再由 executor work stealing
调度；批内和展平结果保持原顺序。I/O 型 resolve/load 使用独立且更保守的 batch limit，不能套用 CPU
常量。`wake_turbo` 的 task memo、recomputer 和 task-lock 表使用 128 个 cache-padded shard，按稳定
`TaskId` 低位选择分片；任何方法都必须在递归 query 前释放 shard lock，128 只改变锁竞争，不改变任务
身份、single-flight、revision 或返回顺序。

Resolver 以两个互补缓存压缩 package-root 探测：`package_owners` 把文件目录映射到最近 package root，并
把一次向上搜索结果回填到访问过的祖先；`package_roots` 按 `(issuer directory, package name)` 保存向上
搜索到的全部 `node_modules` package roots。昂贵文件系统探测始终在锁外，缓存只保存规范解析的确定性
结果并随 resolver cache 清理。

当且仅当 one-shot、未启用持久缓存且未启用跨 generation load cache 时，显式 `./`/`../` 且带非空扩展名
的依赖可尝试 exact relative resolve+load 预取。External 先分类，Node browser-resource 禁止预读；候选
按规范化物理路径在同层聚合，读取成功后用同一 resolver 产生 module identity、共享同一 loaded owner，
并在后续 BFS 层复用该成功结果。不同物理安装路径即使逻辑 identity 相同也不合并；失败不进入成功表，
必须逐请求回到普通 resolver + loader，以保留 TypeScript twin、目录入口、alias、PnP 和规范诊断。

## Invariants

- IR lowering、编辑后的所有权交接、module planning 结果和 seal/finalize 以完整 `TypedProgram::validate`
  为边界；同一 validated revision 内部遍历与 analysis rebuild 不重复全量校验，typed mutator 必须维护
  局部结构/grammar 不变量。普通最终发射只接受 `FinalizedTypedProgram`；仅 optimizer-owned no-op 证明可
  允许 sealed trivial program 借用发射。
- 不合法 typed edit、IR 不变量失败和不收敛必须包含模块与 pass 上下文，不能
  静默退回源码或旧 emitter。
- 绑定敏感行为只用 `SymbolId`/`NameId`；`Span` 只表示来源与外部编辑定位，不作为活跃性或 rewrite key。
- Pre-lower 与 semantic-free 路径只接受 exact linker liveness 和显式 allow-list；证明拒绝必须无部分提交
  地回到普通 semantic 路径，不能放宽 live export、动态作用域、可信编辑或 ESM identity。
- One-shot 输入在首个 query 后不可变，消费式 `BuildSession::build_once` 只能执行一次；retained
  `BuildSession` 始终保留普通 engine 的 fingerprint、依赖、revision 与精确失效语义。
- CPU batching 和 128 shards 必须保持请求/输出确定顺序与 single-flight；resolver exact prefetch 失败必须
  回到规范解析，不能吞掉 TS twin、目录、alias、PnP 或诊断。
- 合成标点和 wrapper 不伪造源码位置；改名映射保留 original name。
- 供应链清单、锁文件、`.yarn`、vendor 路径和二进制制品不得因本重写新增内容。

## Evidence

- `crates/wake_ecma_minify/src/typed_ir.rs`：owned typed arena、稳定 ID、origin、结构 mutation、validated
  traversal、fingerprint 与验证。
- `crates/wake_ecma_minify/src/typed_analysis.rs`：从当前 live tree 重建 scope、reference、CFG、确定初始化、
  effect 与 escape facts，以及 validated epoch 内的无重复验证 rebuild。
- `crates/wake_ecma_minify/src/typed_edits.rs`、`typed_lowering.rs`、`typed_decorators.rs`：一次性结构化编辑
  与 lowering。
- `crates/wake_ecma_minify/src/typed_passes.rs`、`typed_inline.rs`、`typed_mangle.rs`、`typed_pipeline.rs`：
  固定 pass 顺序、100 轮上限、分析刷新、DCE/inline 和最终名字。
- `crates/wake_ecma_minify/src/typed_modules.rs`：消费式 module plan、binding-free plan、seal、retained
  requests 与 final facts。
- `crates/wake_ecma_codegen/src/typed.rs`、`crates/wake_ecma_codegen/src/lib.rs`：不依赖 parser AST/legacy
  tables 的 typed token、mapping 发射和 optimizer-owned no-op finalization 证明消费。
- `crates/wake_ecma_minify/src/owned_optimizer.rs`：稳定 export-name liveness 到当前 `SymbolId` 的解析、exact
  pre-lower、semantic-free owner、构建入口到 typed pipeline 的 owner/input 适配与诊断。
- `crates/wake_bundler/src/incremental.rs`：one-shot/session 生命周期选择、terminal emission、CPU/I/O batching、
  exact relative prefetch、稳定持久缓存边界和 2k timing 诊断。
- `crates/wake_turbo/src/engine.rs`：transient engine 冻结边界、single-flight 与 128 个 cache-padded shards。
- `crates/wake_resolver/src/lib.rs`：package owner 路径压缩、issuer/package roots 缓存及 cache clear。

## Consequences

优化语义只有一个拥有者和一份可变语法事实源。Structural pass 与分析通过稳定 ID 直接协作，codegen
无法重新解释 span 决策，bundler 也不再维护第二份名字或模块 rewrite 计划。代价是 `TypedProgram` 必须
完整覆盖 parser/lowering 的全部输出节点，任何新增语法都必须同时补 lowering、验证、分析保守语义、
typed emitter 与测试。

## Validation

完成声明必须由 locked/offline 验证支持，而不是由本 ADR 推定。最低矩阵为：

- `wake_ecma_minify`：所有节点无损 lowering/re-emit/reparse、每个 pass 正反例、同 revision 分析复用与
  结构变化后重建的 work-count、固定点终止、
  eval/with、TDZ、异常/副作用顺序、属性逃逸和确定性改名；
- `wake_ecma_codegen`：typed emitter 语法、最短 token、mapped/unmapped body 相同，以及各类 origin/name
  mapping；
- `wake_bundler`：普通 ESM/CJS、循环、顶层 await、代码分割、Tree Shaking、preserve/bundled module
  finalization、one-shot 与 session 逐字节/诊断等价、冷/热/持久缓存、exact-prefetch 成功/失败回退和不同
  worker 数；
- `wake_turbo`：one-shot 输入冻结、无依赖边读值、普通 engine red/green、并发 single-flight、128 shard
  竞争与 loom/并发门禁；
- `wake_resolver`：同目录/相邻目录 package-root 探测压缩、nested package、node_modules/scoped package、
  pnpm/PnP identity 和 clear 后重算；
- 同一 Wake 自有程序的未压缩/压缩 Node 差分，并对压缩产物重新解析；
- 自有体积语料逐例 `new <= legacy` 且聚合 `new < legacy`，比较时排除 map trailer 和必须保留的头部；
- `wake_app`、CLI、Node API 的 `minify + sourceMap` 回归、release performance invariant、bench 编译、
  architecture/docs check 与 `git diff --check`。

所有 Cargo 命令使用 `--locked --offline`。不得执行下载或安装命令。未运行或失败的矩阵必须在交付时
明确报告，不能写成已经验证。

无缓存 2k modules 的跨工具验证固定使用 `fixtures/2k-modules/run.mjs` 所描述的口径：同一生成输入、lock、
release binary、机器、电源状态和后台负载；Wake 与 Vite 都启用生产 minify、关闭 Source Map，并使用
runner 中冻结的各自产物格式/chunk 配置，Wake 不传 `--cache`。每个计时样本由直接 spawn 的新进程执行，
因此不存在前一 generation 的内存 session；先 warmup 并运行 bundle 校验
Northstar committed stdout oracle，再记录 5 个纯构建墙钟原始样本及 avg/min/max。峰值内存另用
`memwrap.ps1` 运行 2 次，只报告 WorkingSet，不把 PowerShell 启动
与轮询时间混入构建时间。产物同时记录 raw/gzip/brotli 和结构统计。

“无缓存”在此仅表示没有持久缓存命中和前代 session 复用；one-shot engine 在单次构建内部的 typed value
共享、single-flight 与批处理仍属于被测生产路径。报告必须确认未启用持久缓存，不能把预存 `.wake` 目录、
`WAKE_TIMING` 诊断开销或 memory wrapper 时间计入优势。Wake 快于 Vite 的结论只能来自同轮 5 次原始
样本的平均值，并以 checksum、重新解析/运行、产物大小和上述 crate 门禁同时通过为前提。

## Supersedes

None.

## Removal plan

本变更采用原子切换，不保留运行时 feature flag、兼容 emitter 或可执行旧压缩器。生产入口、bundler
调用方、typed module finalize 和 typed emitter 必须在同一迁移中切换；生产数据流统一为结构化
`OptimizeInput` → owned typed IR → typed emitter。外部 pass 拼装、第二套名字优化状态、4096 字节退让、
禁用压缩 Source Map 和停用 hoist 分支一并删除。旧实现只可作为审阅过的冻结字节数字存在，不能作为
测试时或运行时 fallback。

Validated/consuming、semantic-free、trivial-effect 和 no-op finalization 都是同一 typed pipeline 的证明化
入口；成功后不保留运行时 feature flag 或双实现。One-shot engine/terminal emission 与 retained engine
由 `BuildSession` 的显式构造器选择，共享同一任务定义和编译产物；session 不通过运行期探测“是否可能只
构建一次”切换生命周期或语义。

压缩器版本（当前 `wake-closure-minifier-v12`）、defines/drop flags、稳定的声明保留名/公开观察名/star 链接事实、可信编辑和保留名进入稳定指纹。版本提升使旧产物自然
miss，不做格式迁移。Map facts 可以独立缓存，但 map 开关不得改变 optimizer/body 身份或 JavaScript
body。本轮不改变持久缓存的 opt-in 默认、schema、阶段归属或 session invalidation：retained facts、body
和 mapping facts 仍由现有稳定 key 持久化，arena、`SymbolId`、`Vc` 与 transient engine 值仍不落盘。
One-shot 省略的只是没有下一 generation 消费者的进程内元数据；启用持久缓存时仍按既有 key 读写，长
生命周期 session 仍计算 fingerprint、依赖边和 revision。
