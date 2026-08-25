# Wake 架构决策记录

本目录保存长期有效的架构决策。源码和测试仍是实现事实；ADR 用于说明为何选择某种架构、它保护哪些不变量，以及未来如何替换它。

## 生命周期

允许的状态值：

- `proposed`：正在验证；
- `accepted`：已采纳并反映在仓库中；
- `superseded`：已被更新的 ADR 替代；
- `rejected`：已评估但未采纳。

基于 `0000-template.md` 新建带编号的 ADR。上下文发生变化后，不要重写之前的决策。替代 ADR 应在 `Supersedes` 下列出旧记录；旧记录的状态改为 `superseded`，并添加 `Superseded by` 链接。

`npm run architecture:check` 会验证编号、状态、必需章节、链接，以及机器可读边界策略中的引用。
