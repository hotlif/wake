# 测试与基准约定

> 对应：DESIGN §11、PLAN §1 / §0.4。各 Phase 只往这套骨架里填用例。

## 门禁（每 PR，CI 强制）

| 门 | 命令 | 说明 |
|----|------|------|
| 格式 | `cargo fmt --all --check` | rustfmt 统一风格 |
| lint | `cargo clippy --workspace --all-targets -- -D warnings` | 警告即失败 |
| 测试 | `cargo test --workspace` | 单测 + 集成 + 快照 + doctest |
| miri | `cargo miri test -p wake_ecma_ast` + `cargo miri test -p wake_turbo` | 手写 `unsafe` 无 UB（自引用 AST §10.4 + 引擎 thread-local 裸指针 §10.3） |
| loom | `RUSTFLAGS=--cfg loom cargo test -p wake_turbo --test loom_single_flight` | single-flight 协议在所有线程交错下正确（§10.3 / §2.5.6） |
| bench 冒烟 | `cargo bench --workspace --no-run` | bench 骨架可编译 |

**编译器代码无 snapshot 覆盖不合入；引擎代码无对拍/loom 测试不合入**（PLAN §1）。

## 1. 快照测试（insta）

用于 lexer/parser/transform/codegen/诊断的输出验收。示例见
[`crates/wake_common/tests/render_snapshot.rs`](../crates/wake_common/tests/render_snapshot.rs)。

- 快照文件：`<crate>/tests/snapshots/*.snap`，随代码提交。
- 新增/变更后审查：`cargo insta review`（或一次性 `INSTA_UPDATE=always cargo test -p <crate>`）。
- 快照应用 **plain**（无 ANSI）形态，保证可读、可 diff。

## 2. 基准（criterion）

- 微基准：`<crate>/benches/*.rs`，`harness = false` + `criterion_main!`。示例见
  [`crates/wake_common/benches/interner.rs`](../crates/wake_common/benches/interner.rs)。
- 运行：`cargo bench -p <crate>`；快速冒烟：`-- --measurement-time 1 --sample-size 10`。
- 宏基准（三档合成项目 100/1k/10k + 一个真实开源项目）在 P1+ 随管线建立。
- **双值展示**：底线值与目标值并列（PLAN §1），防止用底线自我满足。
- **回归门禁**：超 5% 红灯（PLAN §1）。需基线历史，用 critcmp/bencher 在有数据后接入 CI。

## 3. Fixture 约定（e2e 打包）

见 [`fixtures/README.md`](../fixtures/README.md)。端到端打包 fixture 从 P3 起填充：
输入项目目录 → 期望产物结构 + **产物在 node/浏览器实际执行** 的断言。

## 4. 正确性防线

- **对拍**（引擎）：随机变更 + 随机请求与全量重算比对。Spike ② 已建雏形
  （[`crates/wake_turbo/src/spike.rs`](../crates/wake_turbo/src/spike.rs)），P2.5 固化为
  `WAKE_VERIFY=1` 常驻模式（每次增量构建后偷偷全量重算比对，进 CI nightly）。
- **对拍**（parser）：test262-parser-tests 全量 + 随机 npm 包源码与 acorn 语义对比（P2）。
- **fuzz**：cargo-fuzz 打 lexer/parser，长期跑不 panic/OOM（P1 起）。
- **loom**：引擎并发正确性模型检查（P2.5）。

## 5. miri

所有 **手写 unsafe** 必须常驻 miri 防回归（默认 Stacked Borrows）：
- `wake_ecma_ast`：自引用 AST 持有者（DESIGN §10.4）；
- `wake_turbo` 引擎核心：`engine.rs` 的 thread-local 「当前引擎」裸指针（DESIGN §10.3）。

```bash
rustup toolchain install nightly --component miri
MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test -p wake_ecma_ast
MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test -p wake_turbo
```

**用 crossbeam 的测试（`executor` 单测 + `concurrent` 集成测试）标了 `#[cfg_attr(miri, ignore)]`，miri 自动跳过**：
它们无自有 `unsafe`，仅依赖 crossbeam-deque/epoch 无锁结构——这类结构在 Stacked Borrows 下有已知 retag 假阳性，
须 `-Zmiri-tree-borrows`；而 Tree Borrows 下又会把 crossbeam-epoch 的延迟回收报为 `memory leaked`
（需再加 `-Zmiri-ignore-leaks`），且单跑约 11 分钟。其并发正确性由 **loom**（§10.3 single-flight 协议）
与并发对拍压测覆盖，更对口。本地取证命令（非 CI）：

```bash
MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-tree-borrows -Zmiri-ignore-leaks" \
  cargo +nightly miri test -p wake_turbo --lib executor

# loom（穷举线程交错验证 single-flight 协议）
RUSTFLAGS="--cfg loom" cargo test -p wake_turbo --test loom_single_flight
```
