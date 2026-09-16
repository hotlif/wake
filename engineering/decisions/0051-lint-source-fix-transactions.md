# ADR 0051: lint 文本修复与源码替换事务

- Status: accepted
- Date: 2026-09-13

## Context

原生 lint 已有单文件核心和共享项目入口。安全修复需要可验证的文本编辑及受控源码替换；
ADR 0036 的生成产物必须与输入分离，不能以空输入集合绕过该约束来写回源码。

## Decision

`wake_lint_core` 拥有 UTF-8 编辑集合校验、冲突选择及最多十轮重新分析。单条诊断的 fix 不可拆分，
按诊断稳定顺序选择非冲突集合；内部重叠、非法范围或字符边界直接失败。零宽插入与占用其位置的
编辑（含端点）冲突。每轮只解析一次；语法错误不修复，新语法错误、循环或未收敛拒绝整个结果。

`wake_app::lint` 拥有文件快照、选项与结果；`wake_app::output::replace_lint_source` 是源码替换
的唯一写入所有者，区别于生成产物事务。它持有现有共享 output commit lock，校验规范路径、
open-file identity、原始字节、普通文件和可写权限，拒绝多硬链接源码与符号链接。完成同目录
临时文件写入、权限复制、flush/sync 后再次验证快照，最后原子替换。检查身份及原始字节的快照
在读取阶段创建，不能在修复计算之后补造。
身份句柄持有到最后一次校验，随后关闭以允许 Windows 替换；发布锁持有到整个操作返回。

项目先完成全部发现、读取和修复计算，再进入逐文件 cancellation commit fence。每个文件是独立
事务；后续失败不回滚之前成功的文件。dry-run 和 stdin 不创建锁/暂存文件，不写入磁盘；stdin
配合 write 模式被拒绝。Node 请求是闭合的 `fix: off | dry-run | write`，CLI 映射对应两个互斥 flag。

## Invariants

- 核心不访问文件，CLI/Node 不自行应用编辑或写回文件。
- 抑制后的诊断才参与修复，返回的诊断、计数和坐标来自最终文本。
- 所有候选文件的配置/解析/修复执行错误在首个写入之前返回；原始语法错误是诊断，文件保持原文。
- 原始内容/身份漂移、只读或多硬链接文件拒绝写入；失败暂存文件自动清理。
- 写回使用 ADR 0026 的同一个进程及 OS 锁，不另设互不协调的 lint 锁。
- cancellation 在 fence 前无写入，fence 内替换完成后才响应取消。

## Evidence

- `crates/wake_lint_core/src/fix.rs` 和 `tests/fixes.rs`：编辑集合、修复迭代及规则正反例。
- `crates/wake_app/src/output.rs`：源码快照与重复验证的原子替换，冲突和权限测试。
- `crates/wake_app/src/lint.rs`、CLI 和 Node：同一修复入口及模式映射。

## Consequences

这是一项实验性能力，不保证跨文件整批原子性。共享锁只约束参与该锁的 Wake 进程；外部编辑器
不受锁约束，最终复核与原子替换之间仍存在 OS 路径竞态窗口，不宣称通用 compare-and-swap。
保留标准文件权限位；不承诺复制平台扩展属性、ACL、备用数据流或文件硬链接关系。拒绝多硬链接
避免修复一个别名却保留另一个旧版本。没有使用这些能力的普通检查不改变行为。

## Validation

- `cargo test -p wake_lint_core`、`cargo test -p wake_app lint`、`cargo test -p wake_cli --test lint`。
- Node addon 修复请求/结果测试、npm CLI 测试和 `corepack yarn npm:typecheck:wake`。
- `corepack yarn architecture:test`、`corepack yarn architecture:check`。
- 对应 crate 的 fmt 和 clippy；失败测试先于实现。

## Supersedes

None.

## Amends

- [ADR 0050](0050-native-single-file-lint-core.md): 在纯核心增加文本修复迭代；单次解析约束适用于每次检查或每轮修复，源文件写入仍属于应用层。

## Removal plan

无旧修复入口或兼容桥。新增规则只提供校验过的文本编辑，复用同一核心和源码替换所有者。
