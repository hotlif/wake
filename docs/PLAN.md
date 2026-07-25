# Wake 开发计划（Development Plan）

> 依据：docs/DESIGN.md v0.3
> 口径：单人全职估算；每个任务给出交付物（Deliverable）、完成定义（DoD）、依赖（Deps）。
> 时间单位：周（w）。里程碑用 🎯 标注，验收门禁（Gate）不过则不进入下一阶段。
> 阅读方式：本文件是「做什么、按什么顺序、做到什么程度算完」；技术方案细节看 DESIGN.md 对应章节号。

---

## 0. 总览与关键路径

### 0.1 里程碑地图

| 里程碑 | 阶段 | 累计周 | 交付物 | 门禁 |
|--------|------|--------|--------|------|
| M0 地基就绪 | P0 | 2w | workspace + 两个 spike 结论 | Gate-0 |
| M1 编译前端就绪 | P1+P2 | 10w | lexer + parser + AST（能 parse 真实 npm 包） | Gate-1 |
| M2 增量引擎就绪 | P2.5 | 14w | wake_turbo v1（对拍一致） | Gate-2 |
| 🎯 M3 能打包 | P3 | 18w | `wake build` 打包真实 React 应用 | Gate-3 |
| M4 生产级转换 | P4 | 22w | TS/JSX/SourceMap/旁路 | Gate-4 |
| 🎯 M5 Dev 体验 | P5 | 27w | `wake dev` + HMR p95<25ms | Gate-5 |
| M6 完整能力 | P6 | 33w | CSS/资源/分包/TreeShaking | Gate-6 |
| 🎯 M7 发布 0.1 | P7 | 持续 | store/daemon/SIMD/PGO + 性能达标 | Gate-7 |

### 0.2 关键路径（critical path）

```
P0-spike① (arena) ─┐
                    ├─► P1 lexer ─► P2 parser ─► P3 bundler ─► P4 transform ─► P5 dev/HMR ─► P6 ─► P7
P0-spike② (引擎)  ─┴─► P2.5 wake_turbo ──────────► (P3 全管线以 task 编写)
```

关键路径上的两个高风险节点是 **P0 的两个 spike**——它们证伪成本低、证伪代价高，必须先做。P2.5（引擎）与 P2 后半（parser）可并行，因为引擎不依赖 parser（用玩具计算图开发）。

### 0.3 并行机会

- P2.5 引擎 ∥ P2 parser 后半（不同 crate，接口约定好即可）。
- 各 transform pass（P4）彼此独立，可并行开发+并行测试。
- 测试基建（test262 接入、fixture 框架、bench 骨架）应在 P0 铺好，之后各阶段只填用例。

---

## 1. 贯穿全程的工程基线（Day 1 就位，之后只维护）

这些不是某个阶段的任务，是从 P0 起就必须持续满足的约束：

| 基线 | 要求 | 对应设计 |
|------|------|----------|
| CI | 每 PR：`cargo test` + `clippy -D warnings` + `fmt --check` + bench 回归检测（超 5% 红灯） | 10.8.8 |
| 测试门槛 | 编译器代码无 snapshot 覆盖不合入；引擎代码无对拍/loom 测试不合入 | 11 章 |
| 基准 | criterion 微基准 + 三档合成项目(100/1k/10k) + 一个真实开源项目，双值(底线/目标)展示 | 11 章 |
| 正确性防线 | `WAKE_VERIFY=1` 模式：每次增量构建后偷偷全量重算比对，进 CI nightly | 13 章风险表 |
| 依赖纪律 | 编译核心只允许白名单依赖（bumpalo/rustc-hash/memchr/simdutf8/xxhash-rust）；新增需 review | 14.1 |
| 可观测 | 所有阶段用 tracing 埋点，`--profile` 出 chrome tracing + 调度器指标 | 10.5 |

---

## 2. Phase 0 — 地基（2w）

**目标**：骨架能编译、能测试、能出诊断；两个高风险方案先证伪。

| # | 任务 | 交付物 | DoD | Deps |
|---|------|--------|-----|------|
| 0.1 | workspace 骨架 | 14 个 crate 的空实现 + 依赖方向约束 | `cargo build` 通过；crate 依赖图无环、方向符合 3.1 | — |
| 0.2 | wake_common | Span、Atom interner（分片锁）、Diagnostic + 终端渲染、FileSystem trait（含内存 FS） | 单测覆盖；能对假 span 渲染带源码上下文的彩色报错 | 0.1 |
| 0.3 | CLI 骨架 | clap 命令 `build/dev/parse/tokenize`（桩） | `wake build a.js` 读文件并打印一条诊断 | 0.2 |
| 0.4 | 测试/基准基建 | insta 接入、fixture 目录约定、criterion 骨架、CI 流水线 | CI 全绿；一个样例 bench 能跑出数字 | 0.1 |
| 0.5 | **Spike ① arena AST** | ~200 行：Bump + 自引用 `ModuleAst` 持有者 + `with_ast` 借用 + visitor demo | miri 跑通无 UB；结论文档（可行/调整点）写入 docs/spikes/ | 0.1 |
| 0.6 | **Spike ② 红绿引擎** | ~200 行：单线程 memo + 依赖记录 + 红绿失效 + 早期截断的玩具实现 | 随机变更下与全量重算对拍一致；结论文档写入 docs/spikes/ | 0.1 |

**🚦 Gate-0**：`cargo test` 全绿；两个 spike 均有可运行 demo 与书面结论。**若 spike① 的自引用方案不可行 → 退回「AST 索引化（id 代替引用）」备选，Gate-0 前必须定案。** 若 spike② 早期截断难以保证正确 → 引擎降级为「无早期截断的纯记忆化」，记录到风险台账。

---

## 3. Phase 1 — 词法分析器（2~3w）

**目标**：全量 ES2022 token，SWAR 层性能达底线。

| # | 任务 | 交付物 | DoD | Deps |
|---|------|--------|-----|------|
| 1.1 | token 定义 + 字节级扫描骨架 | Token 枚举、256 项首字节跳转表、Span 输出 | ASCII 快路径打通 | 0.2 |
| 1.2 | 数字/字符串/模板/正则 | 含 BigInt、转义解码（惰性）、模板模式栈、regex 模式 | 各类字面量单测 + snapshot | 1.1 |
| 1.3 | 标识符 + Unicode | XID_Start/Continue 分层区间表、私有字段 `#x` | 非 ASCII 标识符正确 | 1.1 |
| 1.4 | parser 驱动接口 | `next_token_regex_allowed/div_allowed`、换行标志位、ASI 信息 | 接口就绪供 P2 调用 | 1.2 |
| 1.5 | 错误恢复 + fuzz | `Token::Error` 恢复；cargo-fuzz target | fuzz 跑 1h 不 panic/OOM | 1.2,1.3 |
| 1.6 | SWAR 优化 + 基准 | memchr/SWAR 批量扫描空白/注释/字符串 | `wake tokenize` 可用；基准 ≥150MB/s（底线） | 1.2–1.4 |

**🚦 Gate-1a**：test262 语料 lexer 无误报；基准达底线 150MB/s；fuzz 干净。（显式 SIMD 目标档 400MB/s 留到 P7 回补，此处不卡。）

---

## 4. Phase 2 — 语法分析器 + AST（4~6w，最重）

**目标**：能 parse 真实 npm 包，一遍产出语义信息与依赖。

| # | 任务 | 交付物 | DoD | Deps |
|---|------|--------|-----|------|
| 2.1 | AST 全量节点 + visitor 宏 | ES2022 节点、`Visit/VisitMut` 宏生成、size 静态断言 | `size_of::<Expression>()<=16` 通过 | 0.5,1.x |
| 2.2 | 语句解析 | 声明/控制流/类/模块语法（import/export） | 语句 snapshot 集 | 2.1 |
| 2.3 | 表达式解析（Pratt） | 优先级爬升、结合性、全部运算符 | 表达式 snapshot 集 | 2.1 |
| 2.4 | Cover grammar + ASI | 箭头函数重解释、括号表达式、ASI 完整规则、上下文 bitflags | 边角用例集通过 | 2.2,2.3 |
| 2.5 | 作用域/符号/引用 | 一遍内联构建 scope 树 + symbol 表 + 引用记录 | 符号解析单测 | 2.2,2.3 |
| 2.6 | 依赖同步提取 | 解析时产出 `Dependency` 列表（含动态 import/require） | 依赖列表断言 | 2.2,2.5 |
| 2.7 | test262 接入 + 对拍 | test262-parser-tests 跑批、与 acorn 对拍脚本 | 通过率 >95%；React/lodash-es 无错 | 2.1–2.6 |

**🚦 Gate-1**（M1）：test262-parser-tests >95%；能 parse React、lodash-es、一个中型真实项目全量源码无错误；基准 ≥80MB/s（底线）；`wake parse --ast` 输出正确。

---

## 5. Phase 2.5 — 增量引擎 wake_turbo v1（3~4w，可与 P2 后半并行）

**目标**：函数级记忆化 + 失效 + 并发执行，对拍永远一致。

| # | 任务 | 交付物 | DoD | Deps |
|---|------|--------|-----|------|
| 2.5.1 | `#[wake::task]` 宏 | proc-macro：注册函数、生成 TaskId、包装调用 | 玩具任务能注册并执行 | 0.6 |
| 2.5.2 | Cell / slot 表 / 依赖记录 | `Vc<T>` 句柄、分片 slot 表、thread-local 依赖收集 | 依赖边自动记录正确 | 2.5.1 |
| 2.5.3 | 红绿失效 + 早期截断 | 输入 cell、脏标传播、按需自底向上校验、输出指纹截断 | 单线程对拍一致 | 2.5.2 |
| 2.5.4 | 动态依赖 + generation 取消 | 每次执行重记依赖；新一代到来旧代任务边界放弃 | 分支切换/连续变更用例通过 | 2.5.3 |
| 2.5.5 | 工作窃取执行器 | crossbeam-deque、每核 LIFO 本地 + FIFO 窃取、优先级车道 | 玩具负载 8 核加速比 >6x | 2.5.2 |
| 2.5.6 | 并发正确性 | loom 模型检查、乱序失效风暴压测 | loom 通过；压测无陈旧结果 | 2.5.3–2.5.5 |
| 2.5.7 | 引擎开销基准 | 任务调度微基准 | <2µs/任务 | 2.5.5 |

**🚦 Gate-2**（M2）：玩具流水线在随机变更+随机请求下与全量重算**永远一致**；调度开销 <2µs/任务；加速比 >6x；loom 干净。**引擎必须提供「无增量纯并行」降级开关**（风险表降级预案），此开关在 Gate-2 一并验收。

---

## 6. Phase 3 — MVP 打包器（3~4w）🎯 M3

**目标**：全管线以 task 编写，打包真实 React 应用跑起来。

| # | 任务 | 交付物 | DoD | Deps |
|---|------|--------|-----|------|
| 3.1 | resolver v1 | 相对路径 + node_modules 向上查找 + main/module 字段 + 结果缓存 | 解析单测；能定位 react | Gate-1 |
| 3.2 | Scan 任务化 | resolve/load/parse 各为 `#[wake::task]`，扇出建图 | 1k 合成项目并行建图 | Gate-2,3.1,2.6 |
| 3.3 | codegen v1 | AST→字符串、优先级补括号、无 sourcemap | 往返 parse→codegen→parse 语义等价 | Gate-1 |
| 3.4 | 函数包装 + runtime | `function(module,exports,require)` 包装、~1KB mini runtime、CJS/ESM 基础互操作 | 产物结构正确 | 3.3 |
| 3.5 | Emit + 写盘 | chunk 拼接、原子写、manifest.json | 产物落盘、可被 node/浏览器加载 | 3.4 |
| 3.6 | e2e fixture | hello-world React 工程 + 执行断言 | 浏览器跑起来、断言运行结果 | 3.1–3.5 |

**🚦 Gate-3**（M3）：`wake build src/index.js` 打包 import React 的 hello world，产物浏览器运行正确；1k 模块合成项目 <1.5s；**同命令跑两遍第二遍任务缓存命中率 100%**（引擎接入验证）。

---

## 7. Phase 4 — 转换管线 + SourceMap + 旁路（3~4w）M4

**目标**：TS/JSX 生产可用，恒等旁路大面积生效，SourceMap 正确。各 pass 可并行开发。

| # | 任务 | 交付物 | DoD | Deps |
|---|------|--------|-----|------|
| 4.1 | TS 擦除（span 置空白） | 类型注解/interface/type/import type 原位置空；isolatedModules 语义 | snapshot；行列与源码一致 | Gate-3 |
| 4.2 | TS enum/namespace | 值语义 AST 改写路径 | 语义对齐 tsc 输出 | 4.1 |
| 4.3 | JSX | automatic runtime（jsx/jsxs）+ classic 可配 | 对齐 Babel children/key/spread | Gate-3 |
| 4.4 | define + 死分支剪枝 | process.env/import.meta.env 替换 + `if(false)` 剪枝 | snapshot | Gate-3 |
| 4.5 | 恒等旁路 + span 补丁 | 无转换模块跳过 codegen，仅补 import/export span | tracing 确认 node_modules ≥90% 走旁路 | 3.3 |
| 4.6 | SourceMap（VLQ） | 生成映射 + dev 惰性端点 | 断点映射回 .tsx 正确（playwright+CDP） | 3.3 |
| 4.7 | resolver v2 | exports/imports 字段、tsconfig paths、browser 字段、symlink | pnpm 项目解析正确 | 3.1 |

**🚦 Gate-4**（M4）：打包真实 Vite 模板级 React+TS 项目并运行正确；断点调试映射准确；node_modules ≥90% 走旁路（tracing 证实）；resolver 通过 pnpm 场景。

---

## 8. Phase 5 — Dev Server + HMR（4~5w）🎯 M5

**目标**：dev 即引擎重执行，HMR 达 p95<25ms，React 状态保留热更。

| # | 任务 | 交付物 | DoD | Deps |
|---|------|--------|-----|------|
| 5.1 | 文件监听 + 输入 cell | notify + 20ms 防抖 + 更新输入 cell 触发传播 | 改文件触发精确失效 | Gate-4 |
| 5.2 | 依赖预扫描 | es-module-lexer 式轻扫，提前扇出 resolve/IO | 冷启动 IO 藏于 CPU（profile 证实） | 5.1,4.7 |
| 5.3 | 摘要防火墙任务 | export_signature 等窄腰 task，验证早期截断 | 只改函数体时 link/chunk 零重跑 | Gate-2,4.x |
| 5.4 | dev server | tokio+axum：内存产物服务、HTML 注入、SPA fallback、proxy、错误 overlay | 起服务、页面可访问、错误全屏展示 | Gate-4 |
| 5.5 | HMR 协议 + client runtime | WS 推送、~2KB client、`import.meta.hot` accept/dispose/data/invalidate、accept 边界查找 | 找不到边界整页刷新兜底 | 5.4 |
| 5.6 | React Fast Refresh | 组件模块自动注入 refresh 注册 pass | 改组件 state 保留 | 5.5,4.3 |
| 5.7 | 热路径预算断言 | 逐环节 tracing 埋点 + CI 断言 | 各环节达 10.6 预算表 | 5.1–5.6 |

**🚦 Gate-5**（M5）：`wake dev` 起服务；改组件 state 保留热更；HMR 端到端 **p95<25ms**；只改函数体时 tree-shaking/chunk 任务零重跑；断网/语法错误恢复正常。

---

## 9. Phase 6 — CSS / 资源 / 分包 / TreeShaking（4~6w）M6

| # | 任务 | 交付物 | DoD | Deps |
|---|------|--------|-----|------|
| 6.1 | CSS 依赖提取 | 极简 tokenizer：@import 提取 + url() 改写，其余透传 | @import 去重排序正确 | Gate-5 |
| 6.2 | CSS dev/prod | dev 注入 `<style>`（可 HMR）；prod 按 chunk 抽取 .css | CSS HMR 无闪烁 | 6.1 |
| 6.3 | CSS Modules | `.module.css` 类名 hash + 导出映射 | 选择器级改写正确 | 6.1 |
| 6.4 | 静态资源 | 内联阈值（4KB）、hash 拷贝、`?raw`/`?url`、JSON | import 返回正确 URL/内容 | Gate-5 |
| 6.5 | 代码分割 | entry/动态 import/共享 chunk 规则、内容 hash 文件名、hash 环处理 | 懒加载路由正确分包 | Gate-5 |
| 6.6 | TreeShaking v1 | 符号可达性、export* 展开、sideEffects、未用 export 移除 | 体积对比 esbuild 差距 <15% | 6.5,2.5 |

**🚦 Gate-6**（M6）：多页 + 懒加载路由项目正确分包；产物体积 vs esbuild <15%；CSS HMR 无闪烁；tree shaking 移除未用导出。

---

## 10. Phase 7 — 性能强化与发布 0.1（持续）🎯 M7

| # | 任务 | 交付物 | DoD | Deps |
|---|------|--------|-----|------|
| 7.1 | 任务图持久化 | 图骨架 + persistent 输出 rkyv 落盘；StableSerialize 约束 | 重启零重算未变部分；Atom 不落盘（编译期拦截） | Gate-6 |
| 7.2 | 全局跨项目 store | `~/.wake/store/` 内容寻址；项目缓存存指针 | 新 clone 冷启动大部分命中 | 7.1 |
| 7.3 | daemon 模式 | `wake daemon` + IPC；崩溃降级 | 增量构建毫秒级起步 | 7.1 |
| 7.4 | 显式 SIMD | AVX2/NEON + runtime 检测 + simdutf8 | lexer 达 400MB/s 目标档 | Gate-1a |
| 7.5 | scope hoisting | prod 纯 ESM 子图作用域提升 + 符号重命名 | 产物更小、语义正确 | Gate-6 |
| 7.6 | 内存 + 调度打磨 | arena 复用池、interner 竞争监控、长跑内存预算 | 1k dev<500MB、10k<3GB | Gate-5 |
| 7.7 | 发布工程 | fat LTO + codegen-units=1 + PGO + BOLT + 多档二进制 | 全局 5~15% 提升实测 | Gate-6 |
| 7.8 | 插件 API + 文档站 | Plugin trait 冻结、文档站、报错打磨 | 内置功能全部吃狗粮 | Gate-6 |
| 7.9 | （可选）minifier 立项 | 重命名（复用符号表）先行 | 独立里程碑，不卡 0.1 | 7.5 |

**🚦 Gate-7**（M7，发布 0.1）：10k 模块冷构建 <5s、热缓存 <500ms、daemon 毫秒级；lexer 400MB/s 目标档；长跑内存达标；`WAKE_VERIFY` nightly 长期干净。

### Phase 7 各任务与性能目标详情

Phase 7 的性能强化任务与 wake vs webpack/turbopack 实测数据、优化优先级排序见独立文档：

➡️ `docs/PERF-ANALYSIS.md`（第 3 节「超越路线图」）

---

## 11. 风险检查点（与设计 13 章对齐，按时点触发）

| 时点 | 检查 | 不通过的动作 |
|------|------|--------------|
| Gate-0 | spike① 自引用 AST 是否可行 | 切 AST 索引化备选 |
| Gate-0 | spike② 早期截断是否正确 | 引擎降级为纯记忆化 |
| Gate-2 | 引擎开销/复杂度是否失控 | 启用「无增量纯并行」降级，产品照发 |
| 每阶段 | test262 通过率是否爬升 | 接受渐进，锁定回归 |
| Gate-4 | 旁路命中率是否达标 | 排查转换判定过宽 |
| Gate-5 | HMR 预算是否达标 | 按热路径表逐环节归因 |
| 持续 | bench 回归 >5% | 红灯阻断合入 |

---

## 12. 建议的落地顺序摘要（TL;DR）

1. **先证伪**：P0 两个 spike（arena、引擎）——决定后续所有架构。
2. **前端打通**：P1 lexer → P2 parser，拿到能 parse 真实包的编译前端。
3. **引擎并行推进**：P2.5 与 P2 后半并行，独立用玩具图开发到对拍一致。
4. **第一个能跑的东西**：P3 MVP 打包器（M3，约第 18 周）——这是士气与验证的关键节点。
5. **生产可用**：P4 转换 + 旁路 → P5 dev/HMR（M5，最能体现「性能」卖点）。
6. **补齐能力**：P6 CSS/资源/分包/shaking。
7. **榨性能 + 发版**：P7 store/daemon/SIMD/PGO，发布 0.1。

审计里的正确性坑（Atom 不落盘）在 7.1 用类型系统兜底，但**编码规范从 P2.5 引擎落地时就要写明**，不要等到 P7 才想起。
