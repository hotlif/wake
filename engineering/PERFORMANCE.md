# Wake 性能测量

本文件记录可复现方法和现有测量面，不给出脱离机器、提交和工具链的目标数字。CI 对增量路径执行确定性的 work-count 门禁并编译 benchmark；尚未启用“回归超过固定百分比即失败”的历史耗时基线。

# 1. 测量纪律

每份结果至少记录：

```text
commit/tag:
date/timezone:
OS + kernel/build:
CPU + logical cores:
RAM:
Rust toolchain:
Node/npm:
power mode / background load:
command:
sample size / warmup:
```

同一比较必须使用相同源码、依赖锁、工具链、fixture 和电源状态。不要把 Windows Debug 构建与 Linux Release 构建直接比较。

# 2. Criterion 基准

工作区包含以下 benchmark：

| Crate | Bench | 测量内容 |
| --- | --- | --- |
| `wake_common` | `interner` | 字符串驻留和并发热点 |
| `wake_ecma_lexer` | `lexer` | 词法吞吐 |
| `wake_ecma_parser` | `parser` | 解析吞吐与语法模式 |
| `wake_compiler` | `transpile` | TSX 单模块 parse/lower/optimize/emit 端到端吞吐 |
| `wake_resolver` | `resolve` | 包、目录和缓存解析 |
| `wake_turbo` | `engine` | 调度、命中、失效与扇出 |
| `wake_bundler` | `bundle` | 合成模块冷构建与重复构建 |

编译全部基准：

```bash
cargo bench --workspace --no-run
```

增量架构门禁：

```bash
cargo test -p wake_bundler --test performance_invariants --release
```

该门禁固定检查 edit-one 只读取并 optimize/codegen 一个模块、复用 resolver 拓扑和 link/chunk 规划。它不使用共享 runner 上不稳定的绝对毫秒数。

运行单项：

```bash
cargo bench -p wake_ecma_lexer --bench lexer
cargo bench -p wake_ecma_parser --bench parser
cargo bench -p wake_compiler --bench transpile
cargo bench -p wake_resolver --bench resolve
cargo bench -p wake_turbo --bench engine
cargo bench -p wake_bundler --bench bundle
```

优化前后至少各运行两轮；首轮用于发现频率爬升、杀毒扫描或缓存预热造成的异常。Criterion 的统计目录是生成物，不提交仓库。

# 3. 2k modules 压力样例

`fixtures/2k-modules` 生成确定性的 Northstar 商务控制台项目。约 2000 个模块通过领域模型、规则、
指标、组件、本地化和页面聚合形成自然可达的静态依赖图，用于观察完整磁盘 I/O、解析、链接、
tree shaking、minify、emit 与进程启动：

```bash
cargo build --release -p wake_cli
cd fixtures/2k-modules
npm ci
npm run bench
```

`npm run bench` 和 `npm run compare` 共用同一严格 runner。每次测量前都强制执行生成器和
`verify-project.mjs`，从 `expected/project.json` 读取模块数、类别和源码规模，并先执行源码得到精确
stdout oracle。Wake、Vite 和 webpack 都面向 Chrome/Edge 120、Firefox 121、Safari/iOS 17.2，使用
browser IIFE、生产 minify、无 Source Map、无持久缓存的单 JavaScript 产物；每个样本在计时开始前删除
自己的输出目录，并关闭工具自身的重复清理。构建结束后要求唯一 `.js` 产物的 stdout 与源码逐字节相同；
任何额外 JavaScript chunk 或 `.map` 都直接失败。

普通 `generate`、`verify` 和 benchmark 只允许匹配 committed oracle，不得自动改写它；只有审阅业务、
图摘要和全类别 digest 的变化后，才使用 `npm run generate:update` 显式更新 `expected/`。

时间与内存分开测量：一轮预热后记录 5 个直接子进程墙钟样本；峰值常驻内存另测 2 次，Windows 使用
PowerShell WorkingSet 采样，Linux 使用 `/usr/bin/time -v`，macOS 使用 `/usr/bin/time -l`，包装器墙钟
不进入构建时间。最终唯一 JavaScript 产物按精确字节报告 raw、gzip level 9 和 Brotli quality 11。
完整合同见 [`../fixtures/2k-modules/README.md`](../fixtures/2k-modules/README.md)。

Northstar 替换了旧的全量 side-effect 注册语料，其依赖图、运行时 oracle、源码规模与可压缩性均已变化；
ADR 0023/0024 中记录的旧 `modules=2013` 数字只保留为历史证据，不能与 Northstar 结果作前后性能或体积比较。

# 4. 压缩器体积与工作量门禁

Closure 风格管线只使用 Wake 完全自有的源码语料和冻结旧数字，不复制或下载 Closure Compiler 或
其他第三方压缩器的源码、测试、二进制和语料，也不增加 Cargo/Yarn 依赖。自动门禁由两层组成：
`typed_pipeline_acceptance` 直接测量不含 runtime/header 与 map trailer 的 optimized-program JavaScript
payload；bundler 自有语料再比较最终 bundle。局部 size-contingent rewrite 由 `typed_passes.rs`、
`typed_inline.rs` 和 `typed_mangle.rs` 的 typed token cost 单元与反例测试约束。

体积门禁按以下顺序执行：

1. 生成代码必须重新解析，并通过未压缩/压缩 Node 运行时差分；
2. 规范化出不含 runtime/header 和 source-map trailer 的 JS payload；每例必须 `new <= legacy`，
   全部语料聚合必须 `new < legacy`；
3. payload 不包含 `sourceMappingURL` trailer 与必须保留的产物头，mapped/unmapped 的 payload 必须相同；
4. 记录固定点轮数、各 pass 变更计数和最终字节数；100 轮不收敛属于正确性失败，不作为性能样本；
5. `BuildSemanticModel` work-count 必须证明无结构变化的 minify 只建立一次 typed analysis，结构变化后才在
   下一次 binding-sensitive pass 前重建；
6. 2k modules 与 edit-one 门禁验证优化任务的工作局部性，不使用源码长度退让换取吞吐。

旧压缩器只以审阅过的数字基线存在，不保留第二条可执行路径。Typed primitive folding 使用实际最短
literal/operator token cost；封闭函数与 primitive specialization 计入声明、调用次数、结果和必要括号；
标识符/属性改名计入全部 live occurrences、保留名和 export/runtime 额外成本。只有不增长候选才提交，
属性改名要求严格缩小。纯删除、合法性规范化和可信配置编辑不属于可选体积候选。

局部 cost 函数与完整 emitter payload 门禁必须同时保留：新增候选必须有 precedence、separator、重复
引用和改名交互反例。任何体积收益必须与语义、Source Map 和冷/热缓存证据一起报告；跨线程确定性结论
必须来自实际 worker 矩阵，不能从稳定 ID 或 fingerprint 的存在推断。

# 5. 启动与 npm 开销

Node 包启动 smoke：

```bash
npm run native:build
node scripts/check-startup.mjs
```

该脚本用于发现加载器、平台包选择和 CLI 启动的大幅退化，不是稳定的毫秒级 SLA。安装验证使用 `npm run npm:pack:check` 和发布后的干净 registry smoke，关注是否触发源码编译或 postinstall。

# 6. 冷、热与增量口径

- 冷构建：新进程、无内存会话；是否保留 `.wake/cache.bin` 必须说明。
- 持久化缓存构建：新进程但保留 cache，验证跳过 source read/parse/optimize/codegen 的程度。
- 热重建：同一 `BuildSession`/`BuildContext`，无变化或指定 changed paths。
- Dev Live Reload：包含 watcher 合并、构建和 reload frame 发送；整页浏览器刷新时间应单独测量。

性能提升必须同时验证冷/热产物等价、诊断一致和缓存失效。压缩器版本（当前
`wake-closure-minifier-v13`）、defines/drop flags、图/缓存中稳定的声明保留名、公开观察名与 star 事实、可信编辑和保留名参与
相关身份；optimizer 内部解析的 `SymbolId` 以及 parser owner 的 interner identity 只对本次 AST 有效，
不作为持久性能缓存身份。retained facts、final-layout JavaScript body 和 mapping facts 分阶段缓存；
`want_map` 不进入 optimize/body 任务 key。启用 map 不得改变 JS payload 或重跑这两个阶段；只减少
`durationMs` 但改变代码、chunk、map 或错误不是有效优化。

Source Map 合并必须对每个 module placement 只索引一次 generated token 位置；局部 mapping 随后按
`(line, UTF-16 column)` 做精确或单列 separator 回退查询。不得让每条 mapping 从 token 列表头重新
扫描，否则 React 等大型模块会形成 `O(mapping × token)`，使 mapped code-split/lazy 构建退化。

Source Map 序列化也必须批量计算源坐标：按每个源文件的所需字节偏移排序并去重，单次推进 UTF-16
行列位置，避免为压成一行的依赖反复从行首扫描。该优化保持原有 CRLF、Unicode、越界夹取与映射
JSON 字节语义；不通过关闭 Source Map 缩短开发构建。

# 7. 建立回归门禁的前置条件

Docs 开发候选的工作量门禁见 `wake_bundler/tests/performance_invariants.rs` 与
`wake_app` 的 `docs_dev_candidates_reuse_compilation_and_retry_all_uncommitted_sources`。
它们检查无修改 Rescan 的零重复代码生成、叶子修改的局部失效、候选失败隔离和冷构建产物等价。
Rescan 仍须重读真实输入；不能以生成文件差异为空替代权威复查。

`WAKE_TIMING=1` 输出 `[wake-docs] materialize`、`[wake-candidate] prepare`，以及已有的
`[wake-timing]` scan/read/resolve/link/optimize/body/emit 分项。日志仅用于诊断，不是稳定机器协议；
计时日志开启与关闭的样本分开保存，`Rebuilt` 的模块复用计数只描述 codegen 工作。

真实 Docs 项目可使用 `cargo run --release -p wake_app --example docs_dev_startup -- <project>`
测量公开服务 API 的启动边界；追加项目内 MDX 路径可测量一次临时正文编辑。该入口恢复原始字节，
若发现并发编辑则拒绝覆盖，完成后关闭服务。反馈时间含最多约 10 ms 的事件轮询误差，不表示浏览器绘制。

接入自动阈值前需要：

1. 选择固定 runner 或可校准的专用机器；
2. 连续保存足够历史样本；
3. 为高噪声与低噪声 benchmark 分别设置阈值；
4. 允许人工复跑并保存原始 Criterion 输出；
5. 将性能红灯与正确性门禁分开，避免重试掩盖功能失败。

在这些条件满足前，CI 保持 work-count + `--no-run` 门禁，性能 PR 在说明中附可复现耗时结果。

# 8. 2026-08-31 Northstar one-shot 优化记录

本次测量基于提交 `e896cff` 加当前 one-shot/scan/emit/allocator 工作树切片；环境为
Windows NT 10.0.26200 x64、Intel i9-10900K（20 logical cores）、16 GiB RAM、
`rustc 1.95.0 (59807616e)`、Node `v22.15.0`。电源模式与后台负载未由 runner 固定，因此这些数字是
同一交互会话中的 A/B 证据，不是 CI 毫秒阈值。

构建与正式对比命令：

```powershell
cargo +1.95.0 build --locked --offline --release -p wake_cli
cd fixtures/2k-modules
npm run bench
```

优化前正式 runner：Wake `568ms (550–595)` / `241.0MB`，Vite `480ms (469–493)`；优化后正式 runner：
Wake `258ms (250–263)` / `235.0MB`，Vite `431ms (420–449)`。两轮均先 warmup，记录 5 个时间样本和
2 个独立 RSS 样本。优化后 Wake 原始五次为 `252, 261, 263, 250, 263ms`；同轮 Vite 为
`435, 423, 449, 431, 420ms`。runner 在计时外重新执行每个产物并与 committed source oracle 逐字节比较。

另以 Wake/Vite 交替顺序各运行 10 次，优化后 Wake 为
`249.2, 248.0, 258.2, 252.0, 258.4, 251.5, 250.0, 246.0, 250.2, 248.0ms`
（mean `251.1ms`，median `250.1ms`）；Vite 为
`438.6, 449.6, 458.3, 439.6, 426.7, 444.6, 433.4, 443.7, 433.9, 414.4ms`
（mean `438.3ms`，median `439.1ms`）。该交错实验不替代正式 runner 的 runtime oracle/RSS 门禁。

最终产物保持 `1200124 / 49576 / 23835 B`（raw / gzip-9 / Brotli-11），与优化前完全相同。
`WAKE_TIMING=1` 的热样本把 scan 从约 `89–91ms` 降到 `46–52ms`、emit 从约 `34–40ms` 降到
`18–21ms`，one-shot release 从隐式约 `125–160ms` 的尾部变为显式约 `10–11ms`；正式性能结论只使用
未设置 `WAKE_TIMING` 的 runner 样本。
保留 one-shot `task_exec_count()` 可观察语义并隔离并行析构 panic 后，最终 release 复测为
Wake `257ms (248–269)` / `235.5MB`，Vite `436ms (426–442)` / `209.0MB`，产物字节仍完全不变。

# 9. 2026-09-03 React compiler 抽离后基线

该记录用于给 `wake_compiler_core`/`wake_compiler` 抽离及 React helper import 裁剪建立后续可比较的
Criterion 基线，不是迁移前后 A/B 结论：开始本切片时工作树已包含大量未提交的 parser、optimizer 和
Bundler 改动，没有可归因的同机迁移前快照，因此不能伪造“优化百分比”。环境为 Windows NT
10.0.26200 x64、Intel Family 6 Model 165、`rustc 1.95.0 (59807616e)`、Node `v22.15.0`；电源模式和
后台负载未固定。命令均使用 Criterion `--quick`，只作为功能与数量级冒烟：

```powershell
cargo +1.95.0 bench -p wake_ecma_parser --bench parser -- --quick
cargo +1.95.0 bench -p wake_compiler --bench transpile -- --quick
cargo +1.95.0 bench -p wake_bundler --bench bundle -- --quick
```

| 基准 | 本次区间 | 中值/中间估计 |
| --- | --- | --- |
| parser 256 KiB module | 4.8153–4.8321 ms | 4.8186 ms / 51.933 MiB/s |
| compiler TSX module | 41.128–42.547 ms | 42.263 ms / 983.86 KiB/s |
| bundle 1k cold | 73.261–77.219 ms | 74.052 ms |
| bundle 1k incremental cached | 7.4804–7.5304 ms | 7.5204 ms |
| bundle 1k one-shot | 50.689–51.166 ms | 51.070 ms |
| bundle 1k edit-one | 9.2633–9.6183 ms | 9.5473 ms |
| bundle 2k cold | 110.91–115.60 ms | 111.85 ms |
| bundle 2k incremental cached | 14.945–14.949 ms | 14.948 ms |
| bundle 2k one-shot | 93.692–96.725 ms | 94.299 ms |
| bundle 2k edit-one | 16.495–16.836 ms | 16.768 ms |

`generation_cached` 的 1k/2k quick 样本分别约 51.9 ns/59.3 ns。后续性能改动必须在相同 fixture、工具链
和机器上运行正式 Criterion 样本，并以本节为“抽离后”起点；本轮正确性门禁另外证明没有额外 parse、
完整 AST clone 或 optimized IR clone。

React helper import 裁剪另有可重复的输出体积证据：简单 production JSX fixture 从固定 helper 集合的
153 B 降至 115 B（-38 B），development fixture 从 238 B 降至 215 B（-23 B）。差异只来自删除未使用的
runtime helper import；production/development golden 和运行时行为测试共同锁定其余代码语义。

# 10. 2026-09-08 Docs 候选与 Source Map 优化

基于 `c69a16a4f0c0fe714ccc592a02d0f38626b92e76` 的优化工作树，使用 release 公开服务 API 测量
Crab 文档站：3705 模块、620 分块，保留页面内 Demo、React Compiler、Crab CSS 和 Source Map。
环境、精确数值与限制见[测量数据](performance/docs-dev-2026-09-08.json)。

| 场景 | 原报告 v0.1.30 样本 | 本次优化样本 |
| --- | --- | --- |
| 服务 API 就绪 | 85.323 s | 22.145 / 22.255 s |
| 启动权威复查 | 42.040 s，3705 updated / 0 cached | 1.526–1.559 s，0 updated / 3705 cached |
| 单段正文修改到重建事件 | 43.074 s | 1.735 s |
| 正文修改构建 | 42.337 s，3705 updated / 0 cached | 1.366 s，2 updated / 3703 cached |

同轮分阶段试验中，仅复用候选编译仍需约 65.878 s 就绪：输出组装每轮仍占约 22 s。源码坐标的
UTF-16 换算对长单行依赖反复扫描行首，是这部分的算法性重复工作；按源文件批量计算坐标后，
输出阶段降至约 0.83–0.84 s。转换阶段首次仍约 16.7 s，后续复查可复用其结果。

编辑对象为 `rc-tree-select.mdx`，测量后恢复原始字节并通过 SHA256 一致性检查。历史对比不是同轮
同版本 A/B；计时开启/关闭的样本分列，文件系统缓存未清空，后台负载未完全控制，不能外推 P95。
启动复查仍存在且仍重读权威输入；本次没有实现按访问路由编译或测量浏览器绘制时间。

# 11. 大模块控制流分析的工作量约束

开发构建仍需维护变量初始化与 TDZ（暂时性死区）事实。优化这些内部分析时，应保持每个读取的
`None`/未初始化/已初始化结果、异常和循环边以及函数间保守性不变，不通过关闭分析换取速度。
函数入口的已初始化绑定按函数边界一次归组；互不连通的控制流区域分别求解，状态位图只索引本区域
实际读取的符号。不同区域中相同的外层符号仍各自求解，不能传递调用时尚未证明的初始化事实。
无入口块、不可达环、多入口汇合也保留原有不动点语义。

`wake_ecma_minify::typed_analysis` 的测试以全模块状态求解器为对照，检查分支、循环、异常、捕获和
不连通控制流的逐读取结果；合成多函数图另检查状态容量随局部工作量增长，不设不稳定的耗时断言。

## 2026-09-08 启动 22.3 秒的继续排查

同一 Crab 工作负载的临时任务探针定位到 `elkjs/lib/elk.bundled.js` 与 `elk-worker.min.js`：
单模块优化分别约 16.57 s 和 14.27 s，二者并行，不应将耗时相加。前者经
`components/rc-flow-diagram/src/hooks/useElkLayout.ts` 引入。CSS 处理在这两个模块上不足 0.01 ms。
探针仅用于排查，已从最终源码移除；下面最终样本使用重新编译的 release 二进制。

`elk.bundled.js` 的首轮内部分析包含 870502 节点、42210 符号、11119 个控制流入口和 125285 个块。
原算法每个入口扫描全部符号，并给每块分配全模块位图。仅 incoming/outgoing 位图的理论有效载荷为
`2 × 125285 × ceil(42210 / 64) × 8 = 1323009600` 字节（约 1.32 GB，非进程 RSS 实测）；
模块规划和结构改写还会触发重复分析。按入口归组与局部控制流区域求解后，探针测得初始化求解由
每轮约 1.8–2.2 s 降至约 0.12–0.14 s；ELK 两文件总优化耗时分别降至约 7.64 s 和 5.34 s。

| 阶段 | 上轮最终样本（计时关闭） | 本轮最终样本（计时关闭 / 开启） |
| --- | --- | --- |
| 服务 API 就绪 | 22.255 s | 13.110 / 13.342 s |
| 首次全量构建 | 20.098 s | 10.866 / 11.029 s |
| 启动权威复查 | 1.526 s | 1.613 / 1.672 s |

模块/分块仍为 3705/620，复查仍为 0 updated / 3705 cached。最终计时样本的首次 optimize 约
7.6 s、body 约 1.35 s、emit 约 0.91 s。没有以关闭 Demo、CSS、Source Map 或启动复查换取速度。
本轮两次最终启动测量未编辑 Crab 文件；沿用第 10 节环境和边界，不代表冷文件系统或 P95。

验证：minify/core/codegen 共 363 项单元测试、29 项 bundler minifier acceptance、4 项增量性能
不变量全部通过；minify 全 target Clippy 与格式检查通过。对照求解器仅编译进测试。

剩余工作：全站依赖图仍在首次访问前编译，ELK 仍主导优化阶段。其 Browserify 内部
`require('./elk-worker.min.js')` 也被当前语法依赖收集纳入候选；提前排除局部绑定的 `require`
需要专项验证依赖、缓存摘要与诊断语义。本阶段尚未改变，后续已在第 12 节实现与验证。

# 12. 局部加载器不产生外部 CommonJS 依赖

Bundler 将 parser 的语法依赖候选转成文件依赖时，应排除有明确局部绑定的 `require` 调用，包括
函数参数、局部声明和预打包模块的内置加载器。它们的调用代码与运行时语义保持不变；真正未绑定的
CommonJS `require` 仍需解析，目标缺失仍报错。该过滤由 Bundler 的依赖图所有者完成，不修改 parser
的语法候选契约，也不增加第二次 parse 或 AST clone；无法证明局部绑定的调用继续保守处理。
缓存内容键包含依赖扫描版本，确保旧的未过滤摘要不能跨版本复用。测试覆盖 readable/minified、
tree-shaking 开关、局部与真实调用混合、增量绑定变化、持久缓存重开及缺失依赖诊断。

大模块的内部遍历与校验保持节点顺序、所有权和错误诊断不变：节点的固定结构边使用栈内存，
子节点直接追加到复用的遍历栈；节点 ID 与 arena 位置已经校验后，可达性使用稠密标记，避免
对每个节点反复分配小数组、构造哈希项。列表长度仍不设固定上限，完整语法和环/重复检查仍执行。
逃逸与 CFG 读取查询只消费同一函数边界的名称事实；在已建立节点作用域之后，可在不同函数边界处
停止下降。函数名、参数默认值、类的计算属性及初始化表达式仍按原作用域事实决定，不靠源码形状猜测。

## 2026-09-08 继续排查 13 秒启动

局部 `require` 回归在修改前稳定报 `WAKE0301`（不存在的内置加载器目标被送入 resolver），修改后
正确保留局部调用而不加载目标文件。真实 Crab 工作负载从 3705 模块降至 3702，仍为 620 分块；
仅此修复的探针样本为 13.289 s 就绪，主 ELK 的优化长尾仍在，不能把少做的并行工作等同于墙钟收益。

随后收窄 IR 遍历/校验的内存分配：固定结构边改为栈内数组、子节点直接追加到遍历栈，可达性改为
按 arena ID 寻址的标记数组，样本降至 9.044 s。再按已建立的函数边界剪枝，避免查询外层引用时
遍历整份嵌套 ELK 引擎，得到以下最终结果；最终二进制已移除所有临时探针。

| 阶段 | 上轮最终样本（计时关闭） | 本轮最终样本（计时关闭 / 开启） |
| --- | --- | --- |
| 服务 API 就绪 | 13.110 s | 7.932 / 8.009 s |
| 首次全量构建 | 10.866 s | 5.740 / 5.726 s |
| 启动权威复查 | 1.613 s | 1.583 / 1.653 s |

最终计时样本的首次 scan 约 0.65 s、optimize 约 2.9 s、body 约 0.92 s、emit 约 0.88 s；启动复查仍
为 0 updated / 3702 cached，emit 约 0.96 s。复查会重新验证输入并组装输出，仍没有整份最终产物复用。
因此剩余时间同时包含首次全站编译和启动复查，不能继续全部归因于 ELK 或文件读取。

本轮 715 项 minify/core/codegen/bundler 单元测试、30 项 minifier acceptance、6 项增量性能不变量、
2 项 one-shot 测试通过，合计 753 项；minify/bundler 全 target Clippy、格式与文档检查通过。
新增测试验证局部与外部加载器的运行时区别、持久缓存重开、局部绑定被编辑移除后的依赖恢复，以及
剪枝查询与原全树查询的逐节点等价。release CLI 和测量 example 已重建，npm 包没有发布或替换。

两次最终测量沿用前述环境与公开服务 API 边界，持久缓存关闭，没有修改 Crab 源文件；页面内 Demo、
CSS、Source Map 和启动复查仍开启。没有清空文件系统缓存、测量浏览器绘制或建立 P95 分布。

# 13. Docs 开发按访问编译依赖（2026-09-09）

用户选择首页优先，允许页面依赖错误在访问时报告。独立 Site 开发服务器现在先编译外壳、导航与
元数据；页面及演示的 JS/TS 依赖由首次请求触发。MDX 解析和内容契约仍在启动执行，生产构建仍检查
全站。设计与适用范围见 [ADR 0046](decisions/0046-docs-development-demand-bundles.md)。

| 指标 | 上轮全站编译 | 最终按需样本 1 / 2 |
| --- | --- | --- |
| 服务 API 就绪 | 7.932 / 8.009 s | **1.894 / 1.960 s** |
| 启动图模块数 | 3702 | **29** |
| 首页正文首次 HTTP 请求 | 已包含在全站构建 | **299.7 / 308.8 ms** |
| 首页正文重复 HTTP 请求 | — | **0.62 / 0.57 ms** |

同一进程从启动到取得首页正文的串行 API/HTTP 总和约为 2.20 / 2.28 s，不能把 ready 当成浏览器绘制
时间。两次样本均为最终 release CLI/example 对应代码，计时开启、持久缓存关闭、未清空文件系统缓存，
没有修改 Crab 源码或替换其 npm 安装包，且未与本任务的编译/测试并发测量。原有组件构建及元数据
准备脚本仍不计入 API 边界。只记录两次观测，不据此宣称 P95。

真实 Chrome 152.0.7977.76 的独立检查中，服务器已就绪后，首次导航到首页正文可见为 590 ms；
从首页点击 Button 导航到组件页面可见并完成 network-idle 等待为 2359 ms。实际点击演示的普通按钮，
计数从 0 变为 1，SPA 导航与 Hooks 正常。该浏览器只加载首页和 Button 两个入口；首页 bundle 为
5 模块，Button 为 2523 模块。完整 CDP 版本保存在机器记录中。此为本机功能 smoke，不替代
`TESTING.md` 的 reviewed-browser / 跨平台 conformance 门禁。

这轮排查还暴露并修复了两处不能靠模块计数发现的问题：

- 同一目录被 73 个入口重复核对。监听声明先去重；未访问 Docs 入口使用 `OnRequest` 验证策略，
  首次请求才探测最新配置，随后注册候选监听并物化。外壳仍完成启动与恢复 Rescan。
- PnP 不允许普通 bare-package alias 覆盖依赖声明，初版因此出现第二份 React。现在原始请求先
  正常解析和校验，再把与外壳共享入口相同的结果精确映射到生成适配模块。PnP 幽灵依赖仍报错；
  映射纳入不可变 session 配置。普通 alias 和内部包入口的既有优先级不变。

页面错误由独立错误边界显示，导航仍可使用；失败加载可重试，已请求入口的成功更新会通知浏览器
刷新。开发悬停预加载关闭。会话中新页面暂时 eager 编译，重启后进入延迟清单；聚合站、Components、
自定义 Preview 或 JSX runtime 暂时保持原有完整图，以保护任意 Provider/Context 的共享语义。

剩余成本并未消失：Flow Diagram（包含 ELK）首次请求仍为 **3909 ms**。Button 中的图标 barrel
仍带入大量模块，多个已访问页面仍分别编译非共享依赖，且每次页面物化仍扫描文档元数据。这些是
后续优化方向，不能把本轮启动收益解释成全站总编译工作降到了 29 模块。

原始阶段与 HTTP 数据、浏览器版本及运行边界追加在
`engineering/performance/docs-dev-2026-09-08.json` 的 `onDemandFollowup` 中。

验证：741 项相关 Rust 单元测试通过（另有 2 项原有 ignored），38 项 bundler 集成回归、2 项 Docs
加载器测试、55 项架构测试通过；resolver/bundler/docs/app/dev-server 全 target Clippy、架构检查、
文档检查、格式检查及全 workspace `cargo check` 通过。release CLI 与测量 example 已重建；
未发布或替换 Crab 安装的 npm 包。
