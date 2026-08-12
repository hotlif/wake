# Wake 性能测量

本文件记录可复现方法和现有测量面，不给出脱离机器、提交和工具链的目标数字。CI 当前只编译 benchmark，尚未启用“回归超过固定百分比即失败”的历史基线门禁。

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
| `wake_resolver` | `resolve` | 包、目录和缓存解析 |
| `wake_turbo` | `engine` | 调度、命中、失效与扇出 |
| `wake_bundler` | `bundle` | 合成模块冷构建与重复构建 |

编译全部基准：

```bash
cargo bench --workspace --no-run
```

运行单项：

```bash
cargo bench -p wake_ecma_lexer --bench lexer
cargo bench -p wake_ecma_parser --bench parser
cargo bench -p wake_resolver --bench resolve
cargo bench -p wake_turbo --bench engine
cargo bench -p wake_bundler --bench bundle
```

优化前后至少各运行两轮；首轮用于发现频率爬升、杀毒扫描或缓存预热造成的异常。Criterion 的统计目录是生成物，不提交仓库。

# 3. 2k modules 压力样例

`fixtures/2k-modules` 生成约 2000 个模块的二叉依赖树和共享工具模块，用于观察完整磁盘 I/O、解析、链接、emit 与进程启动：

```bash
cd fixtures/2k-modules
npm install
npm run generate
npm run bench
```

`npm run compare` 可重复比较已生成输入。跨工具比较必须确认双方使用相同模式、source map、minify、缓存和输出清理策略；否则只能作为探索数据。

# 4. 启动与 npm 开销

Node 包启动 smoke：

```bash
npm run native:build
node scripts/check-startup.mjs
```

该脚本用于发现加载器、平台包选择和 CLI 启动的大幅退化，不是稳定的毫秒级 SLA。安装验证使用 `npm run npm:pack:check` 和发布后的干净 registry smoke，关注是否触发源码编译或 postinstall。

# 5. 冷、热与增量口径

- 冷构建：新进程、无内存会话；是否保留 `.wake/cache.bin` 必须说明。
- 持久化缓存构建：新进程但保留 cache，验证跳过 source read/parse/codegen 的程度。
- 热重建：同一 `BuildSession`/`BuildContext`，无变化或指定 changed paths。
- Dev HMR：包含 watcher 合并、构建和消息发送；浏览器应用时间应单独测量。

性能提升必须同时验证冷/热产物等价、诊断一致和缓存失效。只减少 `durationMs` 但改变代码、chunk 或错误不是有效优化。

# 6. 建立回归门禁的前置条件

接入自动阈值前需要：

1. 选择固定 runner 或可校准的专用机器；
2. 连续保存足够历史样本；
3. 为高噪声与低噪声 benchmark 分别设置阈值；
4. 允许人工复跑并保存原始 Criterion 输出；
5. 将性能红灯与正确性门禁分开，避免重试掩盖功能失败。

在这些条件满足前，CI 保持 `--no-run` 编译门禁，性能 PR 在说明中附可复现结果。
