# Wake 工程路线图

路线图从 [AUDIT.md](AUDIT.md) 的开放风险出发，不承诺日期。优先级表示先后关系；每项只有满足验收条件才算完成。

# R1 — 收敛 Bundler 编排路径（P0）

目标：让同步构建成为统一会话实现的薄入口，或明确降为内部测试工具，避免 Scan/Link/Emit 逻辑双写。

验收条件：

- 建立同步/增量路径在代表性 fixture 上的产物、诊断和错误等价测试；
- CLI、Node build/bundle、dev 与 docs 都只经一个应用层构建会话入口；
- persistent cache、Tree Shaking、分包、CSS/资源和顶层 await 不因入口不同而变化；
- 删除或降级 public 类型前记录内部调用迁移，不破坏 npm API。

# R2 — 建立性能历史基线（P1）

目标：从“benchmark 可编译”发展为低噪声、可复跑的回归检测。

验收条件：

- 固定 runner、工具链和电源/负载策略；
- 保存 interner、lexer、parser、resolver、turbo、bundle 的原始历史样本；
- 按 benchmark 噪声设置阈值和人工复跑流程；
- 性能报告同时验证输出与诊断等价；
- CI 文档明确区分 compile smoke 和 regression gate。

# R3 — 覆盖声明的 Node 支持下界（P1）

目标：让 `engines >=22.14 <27` 与自动验证一致。

验收条件：

- CI 至少在 Node 22.14、24、26 运行主 API 与类型测试；
- 发布后干净安装在最低版本验证 CLI 和 `build()`；
- 如果上游 Action/原生工具不再支持 22，则先调整并发布 engines/迁移说明，不能静默失配。

# R4 — 浏览器运行时端到端矩阵（P1）

目标：覆盖 Node 执行无法发现的 HMR、iframe、hash、CSS 和静态路由问题。

验收条件：

- 在至少一个主流浏览器启动 React fixture 和 docs fixture；
- 验证应用 HMR、文档 Demo resize/theme、Components Controls/URL round-trip；
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
