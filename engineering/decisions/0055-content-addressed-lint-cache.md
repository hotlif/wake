# ADR 0055: 按内容寻址的 lint 诊断缓存

- Status: accepted
- Date: 2026-09-13
- Amended by: [ADR 0056](0056-lint-suppression-baselines.md)
- Amended by: [ADR 0061](0061-explicit-lint-global-bindings.md)
- Amended by: [ADR 0063](0063-lint-module-graph-snapshots.md)

## Context

重复检查可以复用单文件诊断，但不能让缓存替代源码、绕过新配置或提供直接写回的修复文本。
既有 wake_cache 拥有派生产物持久化；其编译 body/mapping 格式不应用来存放 lint JSON。

## Decision

wake_cache 增加有界、按内容键寻址的 opaque blob 存储，负责目录、校验 envelope、跨进程锁、
原子替换及有界保留，不依赖 lint/parser/产品类型。wake_app 负责 lint key、诊断 schema 与验证；
核心保持无 I/O，并暴露分析身份及有效配置。该 blob 协议独立于既有编译缓存 schema 13。

`--cache` / Node `cache: true` 默认关闭。目录固定为项目 `.wake/lint/v1`；每次仍读取并哈希
真实源码。key 包含规范相对路径、源码、语言、物化后的规则参数/抑制等级和 parser/core pipeline
身份。只缓存无 parser 诊断的只读单文件检查结果。stdin、修复预览与写回均绕过缓存；未来模块、
类型、插件或 baseline 规则必须补入对应身份与依赖后才能缓存，不能套用当前单文件键。

读取返回 missing/incompatible/corrupt/I/O 等可区分状态。损坏、不可用或 DTO/范围验证失败均重新
分析；最多一条聚合缓存提示进入结果的 cache 元数据，不增加规则 warningCount，不改变退出码。
缓存不保存原始源码快照或修复后全文；写回命令总是从当前快照重新运行修复分析。

存储在 500 ms 有界锁内对当前批次新增键逐项提交。同键同值合并，不同值删除冲突项并报告，
不将载入但未新写的旧项复活。每项使用同目录唯一临时文件、同步与原子替换，无原地写入 fallback。
同一批次不承诺整体原子性。符号链接目录/条目拒绝使用；只清理存储命名规则内的缓存条目。
每项至多 4 MiB，最多 20,000 项/256 MiB；保留淘汰只影响命中率，不改变诊断。

## Invariants

- 源码始终是权威；保留 mtime/size 的编辑也会失效。
- CLI、Node 共用应用缓存路径；核心、parser、semantic 均不依赖磁盘缓存。
- blob 层不解析 lint DTO；应用校验消息身份、等级、范围和编辑边界后才接受命中。
- 缓存只保存 owned 可再生数据，不能保存 Atom、arena、原始源码或修复后全文。
- 取消后不开始缓存提交；任何缓存失败不触发源码写入，也不让本轮 lint 失败。

## Evidence

`wake_cache/tests/blobs.rs` 的持久化/校验/并发/保留测试；应用与 CLI/Node 的冷热、内容/配置失效、
修复绕过及损坏恢复用例。

## Consequences

冷进程仍读取文件，但有效命中可以跳过 parser 与规则。当前不缓存 parser 错误或修复迭代；
修改其他文件不会使纯单文件规则失效，项目级规则接入时必须改变这一策略。

## Validation

最小失败测试先行；wake_cache、lint core、应用、Rust CLI、真实 Node API 和 npm CLI 测试；
Clippy、架构及文档门禁。对并发写入、恶意长度/校验和及只读路径提供明确证据。

## Supersedes

None.

## Removal plan

无旧 lint 缓存格式或兼容读取器；schema 不兼容直接视为 miss。
