# Wake v0.1.14 工程审计

基线：tag `v0.1.14` / commit `cd1c447`，复核日期 2026-08-12。结论来自仓库结构、清单、实现、测试和 workflow；没有运行数据支持的判断标记为“风险”而不是缺陷。

# 1. 已验证事实

- workspace 有 25 个 crate，编译核心、构建编排和产品边缘层已有明确依赖方向。
- CLI 与 Node API 通过 `wake_app` 共享 build、bundle、BuildContext、应用服务器和文档服务。
- npm 主包同时发布 ESM、CommonJS、TypeScript 类型和隔离的 experimental 子路径。
- Wake Docs 支持 15 个既有站点路由、MDX、Demo、Props/JSDoc、site/components 两种模式；本次补充两个 API 路由。
- persistent cache 保存 liveness/concat 摘要与生成体，并有 Tree Shaking、代码分割和顶层 await 回归。
- CI 包含 fmt、clippy、Linux/Windows test、Miri、Loom、benchmark 编译和 Node 24/26 门禁。
- 发布构建覆盖 Windows x64、macOS x64/arm64、glibc 2.28 Linux x64/arm64，共六个 npm 包。

# 2. 本次修复的文档缺口

- `docs/README.md`、根 Cargo 清单和 fixtures 已引用 `engineering/`，但历史迁移没有提交该目录。
- 主 README 只概述 Node API，缺少稳定/实验接口的完整生命周期与错误参考。
- crate 注释仍引用 DESIGN、PLAN 和 WAKE-COMPATIBILITY 章节，但对应文档不存在。
- 文档站生产构建此前没有独立 CI job，也没有 slug/链接静态检查。
- `wake_turbo` 顶层 rustdoc 把已经有实现和测试的 single-flight、并发整合、Loom 与循环检测写成“尚未落地”。

以上项目由本轮工程文档、API 页面、注释校正和 `docs:check` 门禁解决。

# 3. 当时的开放风险

## A1 — 双 Bundler 路径

`wake_bundler` 同时公开 `Bundler` 与 `IncrementalBundler`/`BuildSession`。主应用路径已经偏向会话实现，但两套编排会增加优化、runtime 和诊断行为漂移风险。

证据：两种结构均为 public，且同步与增量实现分别维护构建逻辑。处理条件见路线图 R1。

## A2 — 性能没有历史回归门禁

CI 的 benchmark job 使用 `cargo bench --workspace --no-run`，只能发现编译失败。源码注释中保留的旧目标数字没有固定 runner 和历史数据支撑，不应视为当前 SLA。

这是流程风险，不表示当前性能不达标。

## A3 — Node 最低版本未在常规 CI 覆盖

`engines` 声明 `>=22.14 <27`，CI 和发布 smoke 覆盖 Node 24、26，没有执行 Node 22.14。语法或 Node-API 行为可能在最低版本发生回归。

## A4 — 工程注释存在历史术语

大量局部注释使用 Phase、Gate、M 编号。新增 DESIGN/PLAN/COMPATIBILITY 后引用不再悬空，但维护者仍需避免把历史阶段当作当前完成度。明显错误的“尚未实现”说明应随实现修改立即更新。

## A5 — 浏览器端端到端覆盖有限

多数 bundler 回归通过 Rust 测试、Node 执行和静态输出断言覆盖；文档运行时与 Live Reload 的真实浏览器交互没有在当前 CI workflow 中形成独立跨浏览器矩阵。

这是根据 workflow 和测试形态得出的覆盖风险，不等同于已知运行时错误。

# 4. 已有强项

- arena/线程本地 unsafe 分别有 Miri 目标；single-flight 有 Loom，而不是只靠压力测试。
- Windows 与 Linux 同时运行 workspace test，Windows watcher 还有专门回归。
- 发布 tarball 在发布前检查版本、许可证、原生文件数量、体积和 glibc 基线。
- 平台包先发布、主包最后发布，降低主包指向不存在 optional dependency 的窗口。
- 文档组件工作台把 Args 状态放入 hash，适配无服务端路由重写的静态托管。

# 5. 审计纪律

- 新结论必须带源码位置、测试名或可复现命令。
- “通过 CI”只表示 workflow 中实际存在并运行的矩阵。
- 已修复项从开放风险移到变更记录，不反复保留为当前缺陷。
- 未来复核以新 tag/commit 新建审计段落，不改写旧基线日期。

# 6. v0.1.26 工作树复核（2026-09-02）

本节复核当前工作树，不改写上面的 v0.1.14 历史基线。当前 workspace 为 34 个 crate；Docs 有
53 个 canonical route、106 个 Markdown 文件与 6 个 fixture page；npm 自动发布覆盖 7/7 个包
（`@crab-dev/wake`、`@crab-dev/css` 与五个平台包）。

状态变化：

- A1 已关闭：产品构建统一由 typed `BuildSession`/`BuildGeneration` 所有，底层 engine 只保留在
  bundler 实现与单元测试边界；见 ADR 0027、0028。
- A2 已缓解但未关闭：CI 已运行 deterministic avoided-work 性能不变量和 benchmark compile smoke，
  仍缺固定 runner 上的历史样本、噪声阈值与人工复跑流程；继续由路线图 R2 跟踪。
- A3 已关闭：常规 CI、发布前 local tarball 与发布后 registry smoke 均覆盖精确下界 Node 22.14.0，
  并验证 TypeScript、CLI 和 `build()`；发布门禁静态锁定 22.14.0/24/26 矩阵。
- A4 仍为 P2 维护风险：历史 Phase/Gate/M 编号由 PLAN/COMPATIBILITY 提供稳定索引，但新增注释仍须
  引用当前契约或 ADR，不能借历史阶段暗示完成度。
- A5 已缓解但未关闭：CI 和发布前/后已有五目标 system-browser evidence、React/screenshot 与
  Federation Chromium E2E；共享 exact-major stable readiness 仍被显式标记为 blocked，Docs/Live Reload
  的完整浏览器行为矩阵继续由路线图 R4 跟踪。

本轮架构修复的长期边界记录在 ADR 0026—0035；当前仍开放的架构工作只有 R2、R4 与持续性的 R5。
