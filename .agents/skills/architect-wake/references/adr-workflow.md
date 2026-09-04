# Wake ADR 工作流

仅在判断是否需要、创建、修改、替代或审查 ADR 时读取。

## 先查现有决策

读取 `engineering/decisions/README.md`，按能力、所有者、接口、协议和机器规则搜索现有 ADR，并沿
`Supersedes`、`Superseded by`、`Amends` 与 `Amended by` 关系找到当前决策。不要为已有决策创建同义 ADR。

## 何时需要 ADR

以下情况需要新增或调整 ADR：

- 改变子系统所有权、依赖方向或机器边界；
- 改变缓存身份、并发模型、发布事务、持久化格式、跨进程协议或长期兼容策略；
- 改变跨层数据流或公共契约的长期设计；
- 引入需要机器门禁保护的新不变量；
- 替代状态为 `accepted` 的现有决策。

恢复既有行为的缺陷修复、保持契约的局部重构、测试或文档同步，以及未采纳的实验，不创建 ADR。

## 生命周期

- 同一项仍在验证中的 `proposed` 决策可以补充证据和收敛设计，但不能激活替代/修订关系或作为机器
  边界引用。
- 新证据只改变 `accepted` 决策的局部约束时，新建下一编号 ADR，以带明确范围的 `Amends` 指向旧
  记录，并在旧记录头部添加 `Amended by`；旧记录保持 `accepted`。
- 新决策完整转移 `accepted` 决策的当前权威时，新建下一编号 ADR；旧记录改为 `superseded` 并添加
  `Superseded by`，新记录在 `Supersedes` 链接旧记录。
- `rejected` 和 `superseded` 保留为历史，不改写成当前结论。
- ADR 使用 `engineering/decisions/0000-template.md`，只记录长期决策、直接证据、后果、迁移与可执行验证，不复制完整实现说明。
- 临时兼容桥只在实际采用时记录范围、负责人、移除条件和目标里程碑。

## 机器边界

修改 `engineering/architecture-boundaries.json` 时，每项新增或变化的规则都必须引用真正定义该边界、
状态为 `accepted` 且已进入索引的 ADR。替代 ADR 前先迁移仍引用旧 ADR 的机器策略，随后运行验证索引中
当前定义的架构测试与检查。
