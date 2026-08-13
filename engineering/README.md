# Wake 工程文档

本目录记录 Wake v0.1.16 的实现边界、设计依据和工程门禁。内容以源码、Cargo/npm 清单、测试与 CI 配置为事实来源，不作为尚未实现功能的承诺。

## 阅读顺序

1. [ARCHITECTURE.md](ARCHITECTURE.md)：25 个 workspace crate 的分层、依赖方向和构建数据流。
2. [DESIGN.md](DESIGN.md)：编译、解析、打包、文档与增量引擎的当前设计。
3. [COMPATIBILITY.md](COMPATIBILITY.md)：Crustify 迁移决策和 M1–M8 兼容里程碑。
4. [TESTING.md](TESTING.md)：本地、CI、Miri、Loom、Node 与发布门禁。
5. [PERFORMANCE.md](PERFORMANCE.md)：现有 benchmark、压力样例和测量纪律。
6. [AUDIT.md](AUDIT.md)：v0.1.14 的实证结论与开放风险。
7. [ROADMAP.md](ROADMAP.md)：后续工作及其验收条件。
8. [PLAN.md](PLAN.md)：供源码历史引用使用的 Phase/Gate 索引；不承担未来排期。

## 文档边界

- `docs/` 面向 Wake 使用者，会进入 Wake Docs 路由和搜索索引。
- `engineering/` 面向维护者，不参与产品文档站构建。
- crate 级 rustdoc 说明局部不变量；跨 crate 的约束在本目录维护。
- `README.md` 与 npm README 只保留安装、入口和稳定导航，不复制完整参考内容。

## 更新规则

- 修改公开 Node API 时同步更新 `npm/wake/index.d.ts`、测试和用户 API 参考。
- 修改 CLI 或配置字段时同步更新 CLI/配置参考并运行 `npm run docs:check`。
- 修改 crate 边界、缓存身份、并发协议或发布矩阵时同步更新对应工程文档。
- 性能结论必须包含命令、提交、工具链和环境；没有可复现数据时只描述方法。
- `AUDIT.md` 只记录可由源码、测试或命令复现的事实，推断必须显式标注。

当前文档基线：v0.1.16，复核日期 2026-08-13。
