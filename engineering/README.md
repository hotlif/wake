# Wake 工程文档

本目录面向 Wake 维护者，记录当前实现、目标契约、工程门禁和历史证据。实现状态以源码、Cargo/npm
清单、测试与 CI 配置为事实来源；目标、路线图和历史记录不代表当前已经支持的能力。

## 按任务查阅

以下是导航索引，不要求依次通读。先从任务涉及的源码、清单和测试定位，只查阅需要的主题；
验证范围见 [TESTING.md](TESTING.md#验证范围与结果复用)，持续任务与证据复用约定见 [AGENTS.md](../AGENTS.md#控制重复工作)。

| 要解决的问题 | 文档入口 | 内容边界 |
| --- | --- | --- |
| 功能归哪个 crate，依赖如何流动 | [ARCHITECTURE.md](ARCHITECTURE.md) | 当前分层、所有权和构建数据流 |
| 编译、打包、增量与缓存如何实现 | [DESIGN.md](DESIGN.md) | 实现机制与局部设计；分层归属回链架构文档 |
| 一项变更需要运行哪些检查 | [TESTING.md](TESTING.md) | 按影响范围选择本地、CI、Miri/Loom 与发布门禁 |
| 如何测量性能、比较优化结果 | [PERFORMANCE.md](PERFORMANCE.md) | 测量方法、工作量门禁及注明环境的历史样本 |
| Lint 做到哪一步、规则如何验收 | [LINT.md](LINT.md) | P0–P7 状态、规则契约与验证要求 |
| Crab CSS 如何编译与集成 | [CRAB_CSS.md](CRAB_CSS.md)、[CRAB_UI_INTEGRATION.md](CRAB_UI_INTEGRATION.md) | CSS API、静态求值、样式产物、组件迁移桥与 PnP |
| CSS 编辑器与语言服务如何工作 | [CSS_LANGUAGE_SERVICE.md](CSS_LANGUAGE_SERVICE.md) | 文档模型、协议、资源限制与分发 |
| 为什么采用某项架构约束 | [decisions/README.md](decisions/README.md) | 按领域检索 ADR 的状态、证据与替代关系 |
| 后续还有哪些工作、何时算完成 | [ROADMAP.md](ROADMAP.md) | 未完成事项、验收条件及已完成事项的持续门禁 |
| 如何理解源码中的迁移或阶段引用 | [COMPATIBILITY.md](COMPATIBILITY.md)、[PLAN.md](PLAN.md) | M1–M8 兼容决策和历史 Phase/Gate 索引 |
| 查找过去的审计、实施或发布结果 | [AUDIT.md](AUDIT.md)、[Lint 实施记录](history/LINT_IMPLEMENTATION.md)、[releases/](releases/) | 指定版本和代码状态下的证据，不作为当前能力清单 |

可执行 crate 边界以 [architecture-boundaries.json](architecture-boundaries.json) 为唯一事实来源，通过 `corepack yarn architecture:check` 校验；叙述性文档只解释边界意图，不复制规则。

其他机器输入按需查阅：[浏览器验证矩阵](system-browser-conformance.json)、[测试运行时来源](test-runtime-sources.json)、
[Test262 选择与来源](test262-es2024.json)。用途和运行方式统一见 [TESTING.md](TESTING.md)，避免在导航页复写版本与平台清单。

## 文档边界

- `docs/` 面向 Wake 使用者，会进入 Wake Docs 路由和搜索索引。
- `engineering/` 面向维护者，不参与产品文档站构建。
- crate 级 rustdoc 说明局部不变量；跨 crate 的约束在本目录维护。
- `README.md` 与 npm README 只保留安装、入口和稳定导航，不复制完整参考内容。
- `ROADMAP.md` 汇总待办与验收；专题文档维护详细契约，ADR 维护长期架构依据。`proposed` ADR 不作为当前边界。
- `AUDIT.md`、`history/`、`releases/` 和性能历史章节保留时点证据；当前状态变化时更新对应主文档，不改写旧记录以冒充新验证。

## 更新规则

- 所有可观察行为变更遵循 [文档、测试、实现顺序](TESTING.md#工作顺序)：先明确权威文档或契约，再以失败测试证明缺口，
  最后编写最小实现并运行风险匹配的门禁；代理执行约定见[根级 `AGENTS.md`](../AGENTS.md)。
- 修改公开 Node API 时同步更新 `npm/wake/index.d.ts`、测试和用户 API 参考。
- 修改 CLI 或配置字段时同步更新 CLI/配置参考并运行 `corepack yarn docs:check`。
- 修改 crate 边界、缓存身份、并发协议或发布矩阵时同步更新对应工程文档。
- 修改机器边界时必须引用一份已进入 decisions 索引的 `accepted` ADR，并运行 `corepack yarn architecture:check`。
- 性能结论必须包含命令、提交、工具链和环境；没有可复现数据时只描述方法。
- `AUDIT.md` 只记录可由源码、测试或命令复现的事实，推断必须显式标注。
- 新增专题文档时补齐本页入口；迁移内容时更新相对链接并保留仍被引用的章节锚点。纯文档检查见 [TESTING.md](TESTING.md#1-5-文档)。
