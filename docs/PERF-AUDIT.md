# Wake DESIGN.md v0.2 性能审计报告

> 审计对象：docs/DESIGN.md v0.2
> 审计问题：当前设计是否已达性能上限？还有哪些优化点？
> 结论先行：**架构层已站在第一梯队的上限**（函数级增量 × 全并行 × 零拷贝，这个组合等于 Turbopack 的增量模型 + Oxc 的内存模型 + esbuild 的并行模型），但**「最高性能」还谈不上**——还有一批可观的优化没进文档，而且其中收益最大的几项不是「把活干得更快」，而是「压根不干活」（旁路、惰性、跨项目复用）。以下按预期收益排序。

---

## 一、高收益缺失项（建议进正文，标 ★ 的建议列为设计原则）

### A1. ★ 恒等旁路与跨度补丁（passthrough / span-patching）——可能是 dev 最大单项收益

现状：文档假设所有 JS 模块都走 `AST → codegen` 全管线。但真实项目里**大部分模块（尤其 node_modules，通常占模块数 70%+）不需要任何转换**——纯 JS、无 TS/JSX、define 不命中。对这些模块，唯一必须的改写是模块说明符/导入形式，这可以用「跨度补丁」完成：

- 保留原文，只在 parser 给出的 import/export span 处做字符串级替换拼接（magic-string 思路），**完全跳过 codegen**；
- 更激进：TS 类型擦除本身也可以做成「span 置空白」（ts-blank-space / Node.js amaro 的路线）——被删的类型区域用空格原位覆盖，产物行列与源码完全一致，**SourceMap 退化为恒等映射（几乎零成本）**，且不破坏「pass 不改 Span」纪律；
- AST 仍然要建（依赖提取、tree shaking 符号信息需要），但**codegen 这条最长的字符串生产线在多数模块上直接消失**。

判定：与函数级增量正交、叠加生效。建议写入 4.6/6.4，dev 模式默认开启，prod 下对「无转换命中」模块同样适用。

### A2. ★ 全局跨项目依赖缓存（machine-level content-addressed store）

现状：持久化缓存按项目（`.wake/cache/`）。但 `react@18.3.1` 的 parse/codegen 结果在同一台机器的 10 个项目里是完全相同的。建议：node_modules 内不可变包（有版本号 + 内容 hash）的任务产物写入**全机级内容寻址存储**（`~/.wake/store/`，pnpm store 思想），项目级缓存只存指针。效果：新项目/新 clone 的「冷启动」大部分是热的；CI 上可直接挂载 store。设计成本低（缓存键本来就是内容寻址），收益横跨所有项目。

### A3. ★ 常驻 daemon 模式

现状：每次 `wake build` 是独立进程，重启 = rkyv 反序列化任务图。更强的形态：`wake daemon`——任务图**热驻内存**的常驻进程，CLI/编辑器插件通过 IPC（unix socket / named pipe）提交构建请求。收益：① 零反序列化，增量构建从「秒起」变「毫秒起」；② watch 与一次性 build 共享同一个热图；③ 为未来 IDE 集成（保存即构建）铺路。Bazel/Gradle/watchman 均验证过此路线。注意点：daemon 生命周期管理与版本升级失效（binary hash 进缓存键已覆盖）。

### A4. 惰性 SourceMap

现状：4.6/6.4 把 sourcemap 当作 Emit 的同步产物。dev 模式下浏览器**只在 DevTools 打开且请求时才需要 map**。建议：dev 不生成 mappings，`//# sourceMappingURL` 指向 dev server 的按需端点，请求到达时才由（已缓存的）span 信息现算；node_modules 模块默认永不生成。HMR 热路径上砍掉一整段 VLQ 编码工作。Turbopack 同款策略。

### A5. 快速依赖预扫描（流水线化 IO 与 CPU）

现状：5.2 中 resolve 子模块要等父模块完整 parse 之后。可加一个 es-module-lexer 式的**轻量 import 扫描器**（只识别顶层 import/export 语法骨架，~10 倍于全量 parse 的速度）：文件一读进来先跑预扫描，把子依赖的 resolve + 文件读取**提前扇出**，让 IO 延迟藏在父模块的 parse/transform 之下。冷启动的关键路径从「串行深度 × (IO+CPU)」变为接近「串行深度 × CPU」。注意：预扫描结果只用于预取，正式依赖列表仍以 parser 为准（正确性不依赖它）。

### A6. 显式 SIMD（在 SWAR 之上再提一档）

现状：10.7 只写了 SWAR/memchr。字符串字面量、注释、空白、标识符连续段的扫描可用显式 SIMD（AVX2/NEON，simdjson 的结构字符检测思想）批量处理 16~32 字节；UTF-8 校验用 simdutf8（同为 SIMD）。Lexer 吞吐上限可从 ~150MB/s 档进入 500MB/s~1GB/s 档（Oxc/simdjson 已示范）。与「编译核心零依赖」的冲突需要澄清：建议例外清单放行 `memchr`/`simdutf8` 这两个无传染依赖，或 `std::arch` 手写并做 runtime feature 检测。

### A7. 编译 Wake 自身的构建配置（免费的全局 5~15%）

文档完全没提。发布产物应启用：`lto = "fat"`、`codegen-units = 1`、`panic = "abort"`、**PGO**（用宏基准项目采样）+ **BOLT/相似布局优化**、按 `x86-64-v3`/`aarch64` 分发多档二进制。对一个 CPU 密集的工具，这是不改一行源码的两位数收益，应写进 Phase 7 与 CI 发布流程。

---

## 二、中等收益 / 正确性相关

| # | 发现 | 建议 |
|---|------|------|
| B1 | **Interner 分片锁在高核数下会热**：Scan 阶段所有线程高频写同一个全局 interner，32 核+ 时分片锁是可测的竞争点 | 线程本地 intern 缓冲 + 批量合并；或无锁（leapfrog/追加式 slab）。至少把「interner 竞争」加进 10.5 的可观测指标 |
| B2 | **Atom id 跨进程不稳定 vs 持久化缓存**（正确性坑，文档未写）：持久层若序列化了 Atom 的 u32 id，重启后全错 | 明确规则：persistent 输出禁止含 Atom/TaskId 等进程内句柄，落盘一律字符串化/规范化。写进 10.3 持久化小节 |
| B3 | **Arena 复用池**：每模块新建 Bump，增量场景下反复 mmap/缺页 | 释放的 arena 回池复用（reset 而非 drop），HMR 热路径尤其受益 |
| B4 | **Hash 算法未指名**：内容指纹与任务参数指纹在热路径上 | 内存内指纹用 xxh3/rapidhash；持久化键用 blake3（SIMD、可并行、防碰撞等级够） |
| B5 | **写盘细节**：6.4 只写了原子 rename | 预估容量一次性 reserve、vectored write 聚合小段、（Linux）O_TMPFILE + linkat；产物 UTF-8 已知合法，避免任何重校验 |
| B6 | **HMR 目标偏保守**：函数级增量下 50ms 是模块级时代的目标 | 分解热路径预算到 µs 级（防抖 20ms 之外：读+parse ~2ms、cutoff 校验 <1ms、patch 生成 <1ms、推送 <1ms），把目标改为 **p95 < 25ms、理想 <10ms**，对齐 Turbopack 档位 |
| B7 | **Windows 现实问题**（开发者本机即 Windows，文档零提及）：NTFS stat 昂贵、Defender 实时扫描能吃掉 30%+ 构建时间、路径规范化（大小写/UNC/`\\?\`）开销 | 目录级批量 listing 缓存替代逐文件 stat；文档写明建议用户为项目目录加 Defender 排除；路径规范化结果缓存进 resolver |
| B8 | **长跑 dev 的内存无预算**：任务图 v1 无 GC（风险表已列），但没有量化护栏 | 加内存预算进验收（如 1k 模块 dev 常驻 < 500MB、10k < 3GB）；空闲期做 cell 压缩/不可达任务清理（挂进 10.5 的空闲利用清单） |
| B9 | **配置粒度失效**：缓存键含「相关配置切片 hash」，但若实现偷懒用整个 config hash，任意改配置 = 全量失效 | 把「配置切片化为独立输入 cell（define 一个、target 一个、alias 一个…）」写成明确设计，而非口头纪律 |

---

## 三、文档内部一致性问题（审计顺带发现）

1. **两套基准口径**：Phase 1 要求 lexer ≥150MB/s，而 A6 落地后应为 ≥400MB/s 档——建议 Phase 验收标注「底线/目标」双值，避免用底线值自我满足。
2. **10.6 预算表仍是「冷启动」单口径**：v0.2 的卖点是增量，却没有 HMR 热路径的预算表（B6），两张表应并列。
3. **「编译核心零第三方依赖（bumpalo 除外）」**与 A6 的 SIMD crate、B4 的 hash crate 冲突——需要改成明确的例外白名单（bumpalo、memchr、simdutf8、rustc-hash 级别的「无传染、可替换」依赖）。
4. **5.2 伪代码与 wake_turbo 的关系**已在 v0.2 补丁中说明，但 `seen` 集合与引擎任务去重是重复机制——正文应明确 `seen` 在引擎接入后删除，避免实现时做两遍。

---

## 四、明确判定为「不值得做」的（防止过度优化）

- 单文件内并行 parse（函数级切分 parse）：同步点开销 > 收益，除非出现 >10MB 的单文件病态场景；
- GPU / 异构计算：编译负载分支密集、不规则，无收益；
- 自研 malloc：mimalloc 之上再榨的空间 <2%，机会成本极高；
- 把 resolver 做成常驻 mmap 数据库：daemon（A3）已覆盖同样收益，更简单。

---

## 五、总结

**回答「性能是最高的吗」：架构骨架（增量模型 + 并发模型 + 内存模型）已经没有已知的更优范式可换——这一层是「最高」的。** 差距在策略层与实现层：A1~A7 合计的预期量级——dev 冷启动再降 30~50%（A2+A5+A3）、HMR 再降一半以上（A1+A4+B6）、全场景 5~15%（A7）、lexer 吞吐 2~4 倍（A6）。建议把 A1/A2/A3 提升为设计原则级条目（它们改变架构决策），A4~A7 与 B 类进入对应章节，形成 DESIGN v0.3。

优先落地顺序建议：**B2（正确性坑，先堵）→ A1 → A4 → A5 → A7 → A2 → A3 → A6**。

---

## 六、实测进展

实现进度与实测基准数据（2000 模块合成项目 wake vs webpack vs turbopack 对比）已移至独立文档：

➡️ `docs/PERF-ANALYSIS.md`

该文档持续更新，记录每个关键优化落地前后的 benchmark 变化，是「设计与现实的差距跟踪文件」。
