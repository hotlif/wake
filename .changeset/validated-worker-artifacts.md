---
"@crab-dev/wake": patch
---

开发服务在启动复扫及后续增量构建中复用输入保持一致的原生 Worker 产物，减少重复打包。

可通过 `wake dev --worker-cache` 或 Node API 的 `workerCache: true` 在开发服务重启时复用 Worker 模块编译结果。每个项目最多保留 256 MiB 快照，损坏的快照会在构建时自动修复。
