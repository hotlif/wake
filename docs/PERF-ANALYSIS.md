# Wake 性能分析：实测基准与超越 Turbopack 路线图

> 版本：v0.1（2026-07）
> 审计对象：DESIGN.md v0.3 实现进度 & 2000 模块合成项目实测
> 前置阅读：docs/PERF-AUDIT.md（设计层优化清单）、docs/DESIGN.md（架构设计）

---

## 1. 合成项目实测基准

### 1.1 测试环境

| 维度 | 值 |
|------|-----|
| 硬件 | Windows x64, 8 核 CPU |
| wake 版本 | release build（`cargo build -r`, thin LTO） |
| webpack 版本 | 5.109.0 |
| 测试项目 | 2000 模块二叉树（二叉扇出 + 全模块共享 util，每模块含副作用防 tree-shaking） |

### 1.2 对比结果

| 指标 | wake | webpack | 倍数 |
|------|------|---------|------|
| 最小耗时 | **220ms** | 2306ms | **10.5x** |
| 平均耗时 | **226ms** | 2493ms | **11.0x** |
| 最大耗时 | **234ms** | 2792ms | **11.9x** |
| 产物大小 | 655.8KB | 84.0KB | 7.8x |

两者运行时输出一致（`0 2000`），语义正确。

### 1.3 结论

**wake（早期阶段，大量优化未落地）已实现约 11x vs webpack。** Turbopack 官方公布的基准约 13x webpack。考虑到 wake 尚处于实现早期、关键优化（恒等旁路、SIMD、全局缓存等）还未做，11x 数据说明架构方向正确，天花板更高。

---

## 2. 架构对比：wake vs Turbopack

### 2.1 同构性与独立性

| 维度 | Turbopack | wake |
|------|-----------|------|
| 语言 | Rust | Rust |
| 编译器 | **SWC**（外部依赖） | **自研**（lexer/parser/codegen 全自主） |
| 增量引擎 | turbo-tasks（Vercel 自研，~50k 行） | wake_turbo（自研，红绿算法 + 工作窃取） |
| CSS 引擎 | LightningCSS（外部依赖） | 自研（wake_css） |
| 绑定目标 | Next.js 深度集成 | webpack 兼容独立工具 |

### 2.2 关键差异

**wake 的核心差异化**是「编译器与打包器同构设计」（DESIGN §2.2）：

- Turbopack + SWC：两套独立的内存模型（SWC 的 arena AST / turbo 的任务图）、两套 interner/span、两套调度器。边界存在序列化和拷贝成本，跨组件传递需要（反）序列化中间表示。
- wake：同一个 arena、同一套 span/interner、同一个调度器。依赖提取、tree shaking 的符号分析直接复用 parser 建立的 scope/symbol 信息，中间不落地、不拷贝。

**理论上，完全的自研路线可以在延迟上低于 SWC + turbopack 的组合**——因为编译器与打包器的整合边界开销为零。

### 2.3 实现进度差距

wake 在实现进度上落后于 turbopack。以下表格列出 DESIGN.md 和 PERF-AUDIT.md 已识别但尚未落地的关键优化项：

| # | 优化项 | 状态 | 预期收益 | 对应设计 |
|---|--------|------|---------|----------|
| 1 | **恒等旁路与 span 补丁**：无转换模块跳过 codegen | ❌ 未实现 | **2-3x**（真实项目） | DESIGN §4.6, PERF-AUDIT A1 |
| 2 | **依赖预扫描**：轻量 import 扫描提前扇出 IO | ❌ 未实现 | **30-50%** 冷启动 | DESIGN §5.2, PERF-AUDIT A5 |
| 3 | **惰性 SourceMap**：dev 不生成 VLQ，按需端点 | ❌ 未实现 | HMR 减半 | PERF-AUDIT A4 |
| 4 | **SIMD lexer**：显式 AVX2/NEON 批量扫描 | ❌ 未实现 | **15-30%** parse | PERF-AUDIT A6 |
| 5 | **mimalloc 全局分配器** | ❌ 未配置 | **10-20%** 整体 | DESIGN §10.8 |
| 6 | **fat LTO + codegen-units=1 + panic="abort"** | ❌ 未启用 | **5-15%** 整体（不改一行代码） | PERF-AUDIT A7 |
| 7 | **全局跨项目缓存**：`~/.wake/store/` | ❌ 未实现 | CI 热启动 | PERF-AUDIT A2 |
| 8 | **daemon 模式**：任务图热驻内存，IPC 构建 | ❌ 未实现 | ms 级起步 | PERF-AUDIT A3 |
| 9 | **Arena 复用池**：Bump arena reset 回池 | ❌ 未实现 | HMR 稳定 | PERF-AUDIT B3 |
| 10 | **优先级车道 + 绑核**：executor 精细化调度 | ❌ 未实现 | 大项目稳定性 | DESIGN §10.5 |

---

## 3. 超越路线图

### 3.1 战略判断

wake 的架构上限不低于 turbopack，但**实现成熟度有 6-12 个月差距**。追赶策略应聚焦：

1. **先做收益最大、实现简单的**（旁路、构建配置 = 白送 15-30%）
2. **再做收益大、需要一定工程量的**（预扫描、SIMD、全局缓存）
3. **再做架构层面的大工程**（daemon、scope hoisting）

### 3.2 阶段划分

#### 第一阶段：低位果实（1-2 周，预期收益 35-55%）

| # | 任务 | 难度 | 收益 |
|---|------|------|------|
| 1 | 恒等旁路 + span 补丁：无转换模块跳过 codegen，纯字符串级 import 替换 | 中等 | **2-3x** |
| 2 | mimalloc 全局分配器 (`#[global_allocator]`) | 简单 | **10-20%** |
| 3 | release profile 调优：fat LTO + codegen-units=1 + panic="abort" | 简单 | **5-15%** |

**目标**：真实项目冷构建从 ~11x webpack 提升到 **~20x webpack**。

#### 第二阶段：IO/CPU 流水线（2-3 周，预期收益 30-50%）

| # | 任务 | 难度 | 收益 |
|---|------|------|------|
| 4 | 依赖预扫描器：es-module-lexer 式轻量 import 骨架扫描 | 中等 | 30-50% |
| 5 | 惰性 SourceMap：dev 模式 VLQ 延迟生成 | 中等 | HMR 减半 |

**目标**：冷构建延迟进一步降低，IO 延迟被 CPU 时间覆盖。

#### 第三阶段：全局基础设施（2-3 周）

| # | 任务 | 难度 | 收益 |
|---|------|------|------|
| 6 | 全局跨项目缓存：`~/.wake/store/` | 中等 | CI 热启动 |
| 7 | Arena 复用池：Bump arena reset 回池 | 简单 | 内存稳定 |
| 8 | executor 精细化：优先级车道 + 绑核 | 中等 | 大项目稳定 |

#### 第四阶段：极致单机榨取（持续）

| # | 任务 | 难度 | 收益 |
|---|------|------|------|
| 9 | SIMD lexer：AVX2/NEON + runtime 检测 | 较难 | 2-4x lexer |
| 10 | daemon 模式 + IPC | 较难 | ms 级起步 |
| 11 | scope hoisting（prod 作用域提升） | 较难 | 产物更小 |
| 12 | PGO + BOLT 发布流水线 | 中等 | 5-10% |

### 3.3 预期最终效果

所有优化落地后：

| 场景 | 当前 vs webpack | 目标 vs webpack | vs turbopack |
|------|----------------|-----------------|--------------|
| 冷构建（2000 模块） | ~11x | ~25-30x | 持平或略优 |
| 冷构建（真实项目） | ~5-8x（估） | ~15-20x | 持平 |
| HMR（单文件改函数体） | 未测试 | p95 < 10ms | 持平或略优 |
| 增量构建（缓存命中） | 未测试 | < 10ms | 持平 |

---

## 4. 持续基准方案

### 4.1 基准矩阵

| 项目 | 规模 | 类型 | 衡量指标 | 频率 |
|------|------|------|----------|------|
| 合成 1k 模块 | 1000 小文件 | 二叉树 | cold/warm 构建时间 | 每 PR |
| 合成 2k 模块 | 2000 小文件 | 二叉树 | cold/warm 构建时间 | 每 PR |
| 合成 10k 模块 | 10000 小文件 | 二叉树 | cold/warm 构建时间 | nightly |
| react-ts-app | ~500 模块 | 真实 React 19 | cold/warm 构建时间 | nightly |
| 空项目（noop） | 1 模块 | 最小 | 工具自身启动开销 | 每 PR |

### 4.2 回归门禁

- criterion 基准注册到 CI，回归 >5% 红灯（`critcmp` 对比 baseline）
- 每阶段结束更新本文件中的实测数据表格
- 重大优化提交时要求附带 `perf: before/after` 数据

### 4.3 本文件维护方式

每次 Phase 完成或关键优化落地后，更新第 1 节（实测基准）、第 3.3 节（预期 vs 实际对比），保持本文件反映最优状态。
