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

# 7. 建立回归门禁的前置条件

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
