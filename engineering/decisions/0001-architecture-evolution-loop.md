# ADR 0001: 将架构演进变为可执行闭环

- Status: superseded
- Date: 2026-08-14
- Superseded by: [ADR 0044](0044-architecture-decision-routing.md)

## Context

Wake 已记录其 25 个 crate 的分层、构建数据流、测试矩阵、CSS 设计和产品边界。但这些文档无法形成持久的决策历史，也不能自动拒绝新增的反向依赖。因此，架构工作可能退化为实现先行的补丁，或与仓库逐渐脱节的文字说明。

## Decision

采用一个架构演进闭环，其中包括仓库范围的 `architect-wake` Skill、版本化的架构简报、ADR、机器可读的 crate 边界策略和 CI 架构检查。

将架构视为可证伪的模型。实现期间可依据证据修订目标模型。实现最小但完整的纵向切片，删除被替代的路径，并重新审计收敛情况。除非兼容性是明确的产品需求，否则默认采用破坏性的原子切换。

## Invariants

- 对行为或结构的变更，架构决策先于实现细节。
- 当前代码是证据，并不意味着必须保留错误的边界。
- 确定性边界根据 Cargo 元数据进行机器检查。
- 长期决策被替代后仍保留历史。
- 完成的架构切片只留下一个所有者和一个事实来源。
- 破坏性数据操作及工作树中的无关变更不适用破坏性变更策略。

## Evidence

- `engineering/ARCHITECTURE.md` 定义了依赖方向，但此前没有脚本强制执行。
- `engineering/TESTING.md` 定义了按风险划分的门禁，但没有架构任务分类。
- `cargo metadata --no-deps` 提供当前工作区的 crate 和依赖图。
- 仓库级工作流的官方 Codex Skill 位置为 `.agents/skills`。

## Consequences

影响架构的工作现在必须具备可审查的决策、可证伪的假设、删除审计和 CI 反馈。新增或重新分类 crate 时，必须更新边界策略，并引用状态为 `proposed` 或 `accepted` 的 ADR。由于仓库中的所有使用方要一同切换，部分变更会有意扩大范围。

## Validation

- 使用标准 Skill 验证器验证该 Skill。
- 使用非法依赖、未知 crate、ADR 状态和替代关系夹具运行架构检查器测试。
- 对实际工作区运行 `npm run architecture:check`。
- 在 CI 中独立运行架构任务。

## Supersedes

None.

## Removal plan

不引入兼容桥接。如果要替换此架构工作流，新的 ADR 必须替代本记录，并同时移除 Skill、策略、检查器和 CI 任务。
