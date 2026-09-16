# ADR 0049: Wake 原生 lint 产品边界

- Status: proposed
- Date: 2026-09-12

## Context

Wake 原生 lint 需要原始 JS/TS/JSX 语法、规则、配置、修复、编辑器和扩展能力。当前 parser 在
解析期间擦除 TS 并降级 JSX，Semantic 只拥有作用域/符号/引用。现有构建 AST 不是完整源码模型。
用户允许规则和配置迁移，不要求兼容 ESLint 插件 ABI。详细目标与实施状态见
[LINT.md](../LINT.md)。本决策是候选架构，不代表产品入口或类型能力已经实现。

## Decision

1. 语法继续由 Wake lexer/parser 拥有，以可选采集方式先保留注释和源码位置，再验证原始语法模型。
   不另写一个 lint JS/TS parser，不把 JSX helper 当作源码引用，不从编译产物反推原始语法。
2. 候选 `wake_lint_core` 拥有只读规则分析和候选编辑，`wake_lint` 拥有文件/调度/缓存/写入。
   `wake_config` 拥有声明式配置，`wake_app` 是 CLI、Node 和 LSP 的共同项目入口。
   完成最小端到端切片前不创建空 crate 或激活机器边界。
3. 编译路径继续遵守 ADR 0003 和 ADR 0043。若原始语法实验需要改变已接受的 lowering 所有权，
   另行明确局部修订范围并提供编译回归，不能通过此 proposed 决策隐式改变有效约束。
4. 规则输出绑定 immutable source snapshot；修复由产品层验证并按文件发布。并发、增量、缓存
   与单次检查产生同样的有序诊断。跨文件写入不承诺整体原子事务。
5. 类型服务和动态插件宿主先做能力实验，再分别记录版本/线路/隔离决策；不将第三方内部类型或
   arena AST 暴露为稳定 SDK。普通原生规则不依赖动态宿主。

## Invariants

- 未显式启用的源码采集不分配注释/语法集合，不新增第二次完整解析。
- 注释由实际词法上下文识别，试探和重新词法分析不得泄漏错误注释。
- 原始字节范围和源码快照一致；Unicode 和 CRLF 坐标有明确测试。
- lint 核心不读取磁盘、不执行配置、不写源文件；产品操作不绕过共同应用入口。
- proposed 不能被机器策略引用；完整产品以 LINT.md 的有限范围逐项验收。

## Evidence

- `crates/wake_ecma_parser/src/ts.rs`：类型语法消费与擦除。
- `crates/wake_ecma_parser/src/jsx.rs`：JSX grammar 与直接 lowering。
- `crates/wake_ecma_lexer/src/lexer.rs`：注释跳过、checkpoint 和 JSX relex。
- `crates/wake_ecma_semantic/src/lib.rs`：当前作用域、符号和引用模型。
- `engineering/LINT.md`：产品契约、规则基线、阶段状态与验证要求。

## Consequences

可以复用既有语法权威和产品入口；代价是原始语法采集、类型服务与扩展宿主均需独立验证。
当前不改变构建行为、公共编译器结果或已接受 ADR，不冻结尚未实现的 AST/插件 ABI。

## Validation

- 首先运行 lexer/parser 聚焦失败测试，再实现源码保留；验证 JSX 文本、正则、模板中的伪注释，
  speculation rewind、relex、CRLF、Unicode 和错误恢复。
- 原始语法修改必须保留编译 AST、依赖和诊断等价证据；lowering 修改追加 compiler 回归。
- `corepack yarn architecture:test`、`corepack yarn architecture:check`、`git diff --check`。
- 类型服务与插件协议没有实验和两端测试时保持 proposed。

## Supersedes

None.

## Removal plan

当前没有需移除的旧 lint 路径。现有编译路径仅在替代方案通过完整回归且记录有效决策后才可移除。
