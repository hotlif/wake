---
name: architect-wake
description: 设计或实现 Wake 行为与架构变更；按风险分级，以证据驱动完整切片和验证。文案、格式及仓库外任务不隐式触发。
---

# Wake 架构演进

现有代码是可证伪证据，须服务目标架构。取舍顺序：正确性、诊断、跨平台确定性、增量复用、吞吐量。

## 分级

- **L0 局部维护**：文案、格式或机械式版本同步，无行为变化。声明 `无架构影响`；无需简报、ADR 或参考文件。
- **L1 行为变更**：改变单个子系统，但不转移所有权或改变跨层/公共契约。编辑前发布简明的 `架构简报 v1`：目标、当前证据、目标设计、不变量、验证。
- **L2 架构演进**：改变所有权、依赖、数据流、缓存标识、持久化格式、公共契约，或删除旧路径。读 [architecture-loop.md](references/architecture-loop.md) 并发布简报；长期决策须新增或更新 ADR。缓存、HMR、打包器运行时、Node/npm API、文档路由和 CSS 编译器契约默认属于 L2，除非证据证明只是机械式修改。

## 工作原则

- 分开列出事实、推断、决策和假设；用最小实验检验最高风险项。假设被证伪即修订简报与目标，禁止用兼容补丁掩盖。
- 实现最小完整垂直切片；共享契约的实现、调用方、类型、诊断、测试、文档和发布配置一并更新。
- 默认原子切换并删除被取代路径。只有明确要求兼容时才加桥接，并记录负责人、范围、移除条件和最晚里程碑。
- 保留无关工作树变更。仅在扩大批准范围、影响用户数据或需不可逆外部操作时请求新授权。

## 按需读取

从 `engineering/README.md` 定位源码、清单、测试。只读命中领域卡；跨领域读多个，不读无关文件：

- crate、编译器、打包器、运行时、并发或 unsafe：[rust-core.md](references/rust-core.md)
- 缓存或 HMR：[cache-hmr.md](references/cache-hmr.md)
- Node、npm 或发布：[node-release.md](references/node-release.md)
- Wake 文档：[docs.md](references/docs.md)
- Crab CSS：[crab-css.md](references/crab-css.md)
- 性能：[performance.md](references/performance.md)

## 完成条件

确认能力只有一个负责人和事实来源，依赖符合目标模型，旧路径、临时桥接和重复实现已移除。运行领域关卡和 `git diff --check`，报告证据、跳过项及未验证风险。
