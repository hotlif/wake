# ADR 0071: Docs Demo 浏览器视觉回归

- Status: accepted
- Product maturity: experimental
- Date: 2026-09-23

## Context

Docs 已拥有 Demo 发现、Preview 和生产 CSS/资源构建；Wake Test 已拥有浏览器隔离、截图基线和
差异产物。用户需要直接验证文档中的真实 Demo，而不再维护另一份渲染入口。

## Decision

新增 `wake docs test` 与 Node `testDocs()`。`wake_app` 验证请求并构建当前单站到调用期间持有的
临时目录，保留配置的 base path。它把只含自有路径、Demo 标识和主题/视口的计划通过
`wake_test_contract` 传给隔离测试宿主。`wake_test` 是矩阵执行、结果、截图及报告产物的唯一所有者；
浏览器与回环资源仍由 `wake_test_browser` 拥有。CLI/Node 不链接浏览器或执行测试策略。

首版覆盖单站自动发现的全部 Demo，默认 light/dark × 1280×720/390×844；请求可覆盖两轴。
每个组合新建 BrowserContext，加载实际生产 Demo 页面及其 Preview、主题 CSS、动态分块和资源。
默认缺失基线失败，显式 `--update-snapshots` / `updateSnapshots: true` 才新增或更新。
不承诺交互步骤、工作区聚合、watch、覆盖率或变更筛选；这些能力需独立验收。

## Invariants

- 发现与渲染复用 Docs，不能扫描源码模拟 React/CSS，不能截图加载占位或错误界面获得成功。
- 必须收到当前 Demo 的 ready，并等待字体、图片和布局；加载/渲染错误或超时导致失败。
- 每个组合隔离页面状态；主题既控制 Docs data-theme，也控制浏览器 color scheme。
- 回环 HTTP 服务不提供项目字节；只有宿主的 CDP 会话可通过 Fetch 拦截从本次不可变资源表
  提供页面与资源。测试页面只访问本次资源，外部网络请求失败并进入诊断。
- 临时目录和服务在成功、失败、取消时关闭；基线与差异产物位于源码 Demo 邻近的截图目录，
  标识不包含临时路径、随机端口或认证 token。
- 复用 Wake 截图 profile 和 `wake.test.v1` 结果；不创造第二套截图格式或比较算法。
- 普通测试请求不携带 Docs 计划；新计划具有 schema 标识，旧宿主必须明确拒绝未知请求。

## Evidence

- `crates/wake_docs/src/lib.rs` 和 `runtime/app.tsx`：Demo 描述及 ready/error 协议。
- `crates/wake_app/src/docs_test.rs`：请求、构建与测试宿主的产品编排。
- `crates/wake_test_contract/src/lib.rs`：可序列化计划。
- `crates/wake_test/src/docs.rs`：真实页面矩阵与既有截图策略。
- `crates/wake_cli/tests/docs_test.rs`：系统 Chromium 的四组合基线、CSS、Preview、本地图片、非根 base path、渲染错误及外部请求失败。
- `npm/wake/test/api.test.mjs`：真实 Node ESM/CommonJS API、JavaScript CLI、产物返回和请求拒绝。
- `crates/wake_test_contract/tests/docs_plan.rs`：计划 round-trip、普通请求省略计划和未知字段拒绝。

## Consequences

Demo 无需额外测试文件即可建立视觉基线。首版需要完整单站生产构建和系统 Chromium，按组合串行
运行以简化资源及失败归属；API 保持 experimental。

## Validation

- 契约 round-trip/未知字段、矩阵验证、CLI/Node 请求与类型测试。
- 真实浏览器覆盖缺失基线、创建、重跑匹配、CSS 差异、Demo 错误和隔离 profile。
- 受影响 crate 测试与 Clippy、npm API/CLI/类型检查、Docs 与架构门禁。

## Supersedes

None.

## Removal plan

无旧路径替换或临时兼容桥。
