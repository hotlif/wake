# ADR 0090: 开发服务的可选 Worker 磁盘缓存

- Status: accepted
- Date: 2026-11-27

## Context

ADR 0089 允许复扫复用同一进程内的 Worker 产物。重新启动后，开发服务仍需要编译每个顶层 Worker 的依赖。

## Decision

`wake dev --worker-cache` 和应用 Node API 的 `workerCache: true` 选择持久化 Worker 模块编译结果，默认关闭。应用层从项目根派生 `.wake/worker-cache/`。bundler 按逻辑入口划分文件，在顶层 Worker 的一次性子构建中使用 ADR 0034 的模块缓存。
新进程重新读取内容、解析依赖并应用当前编译选项。模块摘要、保留的请求及模块体和映射按已有内容与编译身份复用。成功构建仍按 ADR 0089 参与当轮发布和同进程复扫。

## Invariants

- 开发服务在启动复扫完成后就绪。Worker 新开关仅影响原生 Worker 子构建的磁盘复用。
- 同一项目缓存目录最多保留 4096 个已发布的受管快照，含文件头合计最多 256 MiB。一个目录级进程锁串行化写入和淘汰，按写入时间从旧到新回收其他快照。读者只接收完整快照。
- 缺失及不兼容按冷缓存处理。损坏或 I/O 失败走原有可恢复缓存诊断，仍从源码构建。损坏条目在成功写回时原子替换。
- 文件系统查询结果及 Worker 最终产物的进程内复用记录限于相应会话。跨进程只使用已有模块缓存格式和经过验证的编译结果。

## Evidence

`wake_cache/src/workers.rs` 定义目录预算，其测试覆盖并发写入与淘汰。`wake_bundler` 的 `worker_disk_cache` 测试在新的父会话中统计模块发射次数并比对产物。开发服务的 `worker_disk_cache_corruption_warns_and_repairs_before_startup_ready` 测试逐次检查启动构建事件、缓存警告与 HTTP 响应。

## Consequences

首次启动需要填充缓存。已有缓存的重启仍支付文件读取、哈希及解析选路成本。超出预算的项目会重新编译已淘汰的数据。

## Validation

验证 CLI 和 Node 入口、默认关闭、成功命中、等长源码及 PnP 归档替换、损坏修复、目录预算和并发写入。用相同 Release 构建分别测量真实应用的无缓存启动、首次填充及热重启。

## Supersedes

None.

## Amends

- [ADR 0034](0034-transactional-persistent-cache-boundary.md): Worker 快照共用一个目录级写锁，在同一事务中执行容量回收和原子替换。
- [ADR 0089](0089-validated-worker-artifacts.md): 显式启用时，允许新会话按入口读取和验证持久化的模块编译结果，最终 Worker 产物仍由会话管理。

## Removal plan

None.
