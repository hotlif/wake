# Wake 工程路线图

路线图从 [AUDIT.md](AUDIT.md) 的开放风险出发，不承诺日期。优先级表示先后关系；每项只有满足验收条件才算完成。

# R1 — BuildSession 构建所有权收敛（已完成，持续门禁）

结果：CLI、Node、dev、Docs 与 Federation 产品构建统一由 typed `BuildSession` 拥有；retained 与
one-shot 只区分显式生命周期，共享 Scan/Link/Optimize/Emit 与 `BuildOutput` 契约。底层 engine setter
降为 crate 内实现边界，长期决策见
[ADR 0027](decisions/0027-build-session-ownership-and-lifetime.md)。

持续验收条件：

- one-shot/retained 全字段产物、诊断和错误等价测试保持通过；
- 产品源码、外部 integration tests 与 benchmark 不得引用底层 engine 或迁移构造器；
- persistent cache、Tree Shaking、分包、CSS/资源、Federation 和顶层 await 不因生命周期不同而变化；
- 新增 bundler 语义必须先进入 immutable `BuildOptions` 及相应缓存身份，不得恢复 product setter；
- 兼容 façade 只能委托 `BuildSession`，删除底层 public re-export 时不得改变 npm API。

# R1.1 — BuildGeneration 完整候选一致性（已完成，持续门禁）

结果：一个 production candidate 由一个 `BuildGeneration` 拥有；application retained/one-shot view 与
Federation container/shared one-shot views 共享 generation-scoped observation cache。类型 identity 每代
只冻结一次，application/Federation/types/manifest/bootstrap/hidden maps 作为完整候选一次发布；dev
Federation 则由单一 retained session 编译 combined graph。长期决策见
[ADR 0028](decisions/0028-build-generation-ownership-and-observation-cache.md)。

## Node/npm 公共边界：已完成

Federation init/lock 已下沉为共享应用服务并同时接通 Rust CLI、Node API 与 npm CLI；输出 kind、
Federation 错误码和 dev 事件都有闭合契约。发布前会验证 Federation runtime、Wake 类型、完整
tarball 目标及 PnP 树外 NodeNext 消费。决策见
[ADR 0029](decisions/0029-node-contract-and-federation-control-ownership.md)。

持续验收条件：

- `wake_app` production federation 子构建不得硬编码 `OsFileSystem` 或直接构造 `BuildSession`；
- build context 必须把 retained application 与 generation owner 共同持有，并在 watcher batch 观察前 advance；
- generation cache 的六个 query family、failure replay、single-flight 和 advance 有行为测试；
- 文档和测试始终声明 lazy、query-scoped snapshot 的边界，不宣称未观察路径或跨方法事务一致性；
- 类型声明只读取一次，再重绑定到最终 `buildId`；失败候选保留 last-good output/runtime snapshot；
- dev application、synthetic container、exposes 与 shared fallback 继续由一个 retained combined session 拥有。

# R2 — 建立性能历史基线（P1）

目标：从“benchmark 可编译”发展为低噪声、可复跑的回归检测。

验收条件：

- 固定 runner、工具链和电源/负载策略；
- 保存 interner、lexer、parser、resolver、turbo、bundle 的原始历史样本；
- 按 benchmark 噪声设置阈值和人工复跑流程；
- 性能报告同时验证输出与诊断等价；
- CI 文档明确区分 compile smoke 和 regression gate。

# R3 — 覆盖声明的 Node 支持下界（已完成，持续门禁）

结果：常规 CI 以仓库外、非 PnP 的干净 npm consumer 在 Windows/Linux 覆盖精确下界
Node 22.14.0，并与 Node 24/26 的完整 Node job 共同验证主 API、类型、CLI 与 `build()`；发布前
local tarball 和发布后 registry smoke 都覆盖 22.14.0/24/26。发布门禁会拒绝丢失精确下界的矩阵。

持续验收条件：

- CI 至少在 Node 22.14、24、26 运行主 API 与类型测试；
- 发布后干净安装在最低版本验证 CLI 和 `build()`；
- 如果上游 Action/原生工具不再支持 22，则先调整并发布 engines/迁移说明，不能静默失配。

# R4 — 浏览器运行时端到端矩阵（P1）

目标：覆盖 Node 执行无法发现的 Live Reload、iframe、hash、CSS 和静态路由问题。

验收条件：

- 在至少一个主流浏览器启动 React fixture 和 docs fixture；
- 验证应用源码变化只触发一次整页刷新且页面状态重置、文档 Demo resize/theme、Components
  Controls/URL round-trip；
- 验证 `/` 与非根 `base_path` 的直达、刷新、404 外壳和资源加载；
- 失败保留截图、控制台和网络诊断。

# R5 — 工程文档持续一致性（P2）

目标：避免再次出现迁移目录缺失或实现状态与 rustdoc 相反。

验收条件：

- `npm run docs:check` 和生产文档构建为必需 CI；
- 改 CLI、配置、Node 类型或组件状态协议的 PR 同步更新对应参考页；
- DESIGN/PLAN/COMPATIBILITY 锚点检查通过；
- 每个发布系列至少复核一次 AUDIT，旧性能数字没有环境时不得升级为 SLA。

# 非路线图事项

以下内容没有当前承诺：稳定 Rust 插件 ABI、任意 JS 配置执行、完整 Sass/Less 内建链、冻结 experimental AST schema。提出这些能力前需单独设计、兼容性和安全评审。
