# ADR 0044: 以 Skill 与验证索引路由架构决策

- Status: accepted
- Date: 2026-09-04

## Context

Wake 的 ADR 已覆盖编译器、构建、CSS 编辑器、Docs、Federation、测试运行时、Node 发布与 CLI。
原有流程要求架构任务留下决策和验证证据，却没有提供按问题定位当前 ADR 的稳定入口。随着历史记录
增加，逐份扫描既放大上下文，也容易把已替代决策、产品成熟度和架构采纳状态混为一谈。

仓库已有 `architect-wake` Skill、ADR 目录和架构检查器。治理缺口不需要新的注册表或目录迁移，而需要
让 Skill 自动进入、让 README 承担简洁路由，并让检查器证明索引与 ADR 文件一致。

## Decision

1. 会改变所有权、依赖方向、缓存身份、并发或发布事务、公共编译/运行时契约的任务，自动使用
   `.agents/skills/architect-wake/SKILL.md`。文案、格式和不影响架构边界的局部修改不触发。
2. Skill 先读取 `engineering/decisions/README.md`，按九个主领域定位决策，再读取命中的当前 ADR 及其
   直接替代或修订关系。只有跨域变更才扩展到其他领域，不默认加载全部历史记录。
3. ADR 文件继续保存状态、日期、决策正文和关系，是唯一事实源。README 在固定标记之间保存全量、
   单领域、按编号排序的状态索引；检查器验证该投影，不增加 JSON 或第二份关系清单。
4. 文件编号、文件名和历史路径保持稳定。完整权威转移使用 `Supersedes`；旧 ADR 改为 `superseded`
   并记录 `Superseded by`。局部改变仍有效的决策使用带范围的 `Amends`；目标保持 `accepted` 并记录
   `Amended by`。普通背景引用不进入关系图。
5. `proposed` 表示决策尚未成为仓库边界，`accepted` 表示当前实现已采纳。产品自身的 experimental、
   beta 或 stable 成熟度在正文单独记录，不能借用 ADR 状态表达。
6. 新的长期架构选择仍以新 ADR 记录；已有决策发生局部或完整变化时，不回写其历史论证，而由新 ADR
   说明变化并维护双向关系。

## Invariants

- 架构相关任务有一个自动触发的 Skill 入口和一个决策路由入口。
- 除模板外，每份 ADR 在索引中恰好出现一次，标题、路径和状态与文件一致。
- 每份 ADR 只有一个主领域；跨域影响由正文链接表达，不复制索引条目。
- 历史 ADR 不移动、不改号，完整替代链和局部修订链都可从任一端追踪。
- ADR 文件是状态与关系的唯一事实源；README 是可验证投影，不存在独立 JSON 清单。
- `accepted` ADR 才能作为已实施机器边界的权威引用。
- `root`、`graph` 与 `turbo` 尚无专属 ADR 的基础机器边界由本决策承接；出现专属决策后再通过
  `Amends` 或 `Supersedes` 转移权威。

## Evidence

- `.agents/skills/architect-wake/SKILL.md`
- `engineering/decisions/README.md`
- `engineering/decisions/0000-template.md`
- `scripts/check-skills.mjs`
- `scripts/check-architecture.mjs`
- `scripts/check-architecture.test.mjs`
- `engineering/architecture-boundaries.json`

## Consequences

维护者可从任务类型进入 Skill，再从九域索引定位少量相关 ADR，同时仍能追溯完整历史。增加或改变 ADR
时必须同步一条索引投影和必要的双向关系；这是明确的维护成本，但由检查器阻止遗漏和漂移。

现有路径、外部链接和源码历史不需要迁移。README 不复制决策摘要、依赖图或实现规则，避免形成第二
事实源。产品仍可保持实验性，同时用 accepted ADR 固定其当前实现边界。

## Validation

- `corepack yarn architecture:test`
- `corepack yarn architecture:check`
- `git diff --check`

## Supersedes

- [ADR 0001](0001-architecture-evolution-loop.md)

## Removal plan

本治理切换不引入兼容桥。采用本决策时删除 Skill 中重复的仓库事实和全量 ADR 读取要求，只保留触发、
路由与验证判断；以后若替换本流程，新的 accepted ADR 必须完整替代本记录。
