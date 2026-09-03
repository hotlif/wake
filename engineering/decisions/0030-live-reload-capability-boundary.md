# ADR 0030: 普通开发更新采用诚实的 Live Reload 能力边界

- Status: accepted
- Date: 2026-09-02

## Context

Wake 的开发构建会长期保留 `BuildSession`，文件变化后只重新解析和生成受影响的模块；但浏览器更新
路径从未拥有模块工厂替换、依赖接受边界、`accept` / `dispose` 生命周期、React Fast Refresh 状态槽
或失败恢复机制。普通应用成功重建后，服务端发送 `reload` 字段，客户端只调用
`location.reload()`。同时，构建配置将 `import.meta.hot` 降为 `false`。

旧的端点、终端标签和文档把这种行为称为 HMR，并宣称组件状态可以保留。该名称把服务端增量编译
错误等同于浏览器模块热替换。配置解析还会静默忽略 `hmr`、`hot` 等未知字段，用户可能以为一个无效
开关已生效。Federation 另有版本化的 `types-only`、`isolated-remount` 和 `full-reload` 更新协议，
不能用来证明普通应用具备模块 HMR。

## Decision

普通应用开发能力统一命名为 **Live Reload**：服务端可以增量重建，但浏览器在成功发布候选后重新
加载整个文档，页面内存状态重新创建。

`wake_dev_server` 是普通浏览器更新协议的唯一所有者。它以封闭的 `LiveReloadMessage` 枚举编码
`ready`、`reload` 和 `error` 帧，并只在 `/__wake_live_reload` 暴露 WebSocket。客户端从同一个常量
生成端点，`reload` 的唯一普通行为是清除诊断并调用 `location.reload()`。旧 `/__wake_hmr` 路径和
双路径兼容层在同一切片移除。

`import.meta.hot` 是保留能力标记，所有 Wake 应用构建都把它固定为 `false`；TOML 配置不能覆盖它，
程序化配置即使构造同名 define 也会被忽略。`[dev_server]` 和 Node `DevServerOptions` 采用封闭字段集，
伪造的 `hmr`、`hot`、`live_reload` / `liveReload` 选项必须失败，而不是静默接受。

Federation 更新继续使用 `wake.federation.dev-update.v1` 的专用类型和动作。它可在隔离边界内 remount，
或显式要求 full reload；该协议不扩展普通应用的 Live Reload 消息集合，也不公开 `import.meta.hot`。

## Invariants

- 普通应用每次成功发布一个新代后，目标挂载收到一个 `reload` 帧并执行一次整页刷新。
- 失败候选不替换最后成功产物、不发送普通 `reload`；客户端显示 `error` overlay。
- 普通帧只能由 `LiveReloadMessage` 编码，不能用手写 JSON 构造第二套形状。
- 产品源码只注册 `/__wake_live_reload`；不存在 `/__wake_hmr` 路由或兼容桥。
- `import.meta.hot` 恒为 `false`，不存在 accept/dispose 或状态保持承诺。
- Federation 的 types-only、isolated-remount 和 full-reload 保持其版本化专用契约。
- CLI、Node/npm、Docs 和工程现状文档都使用 Live Reload，并明确浏览器状态会重建。

## Evidence

- `crates/wake_dev_server/src/lib.rs` 拥有端点、`LiveReloadMessage`、浏览器客户端和 Federation 分流。
- `crates/wake_app/src/lib.rs` 固定并保护 `import.meta.hot = false`。
- `crates/wake_config/src/lib.rs` 与 `crates/wake_node/src/lib.rs` 拒绝伪 HMR 字段。
- `npm/wake/test/api.test.mjs` 读取真实 WebSocket 帧，并执行服务端提供的客户端来证明
  `location.reload()` 行为。
- `docs/app/live-reload.mdx`、README、CLI/npm 终端和工程文档公开同一能力边界。
- `scripts/check-architecture.test.mjs` 静态锁定所有权、端点、保留 define 和公开表述。

## Consequences

开发构建仍保留增量性能，但普通浏览器状态不会跨成功重建保留。`/__wake_hmr` 是未公开的旧内部路径，
此次原子改名会中断直接连接该路径的非契约客户端；旧 `/app/hmr` 文档路由也不保留重定向。未知开发
服务器字段从静默忽略变为配置错误，使错误的能力假设尽早失败。

若未来实现真正 HMR，必须先设计模块身份与工厂替换、接受/失效传播、dispose 数据、删除 prune、
CSS 生命周期、失败恢复、React 状态边界以及真实浏览器状态保持测试，并以新的 ADR 原子替代本决策；
仅增加消息名称或 `import.meta.hot` 对象不足以改变能力等级。

## Validation

- Rust 单测证明 typed 普通帧、唯一端点、客户端整页刷新源码和不可覆盖的 `import.meta.hot=false`。
- 配置与 Node 单测证明 `hmr`、`hot`、`liveReload` 等伪选项失败。
- Node 行为测试连接真实 WebSocket，断言 `ready` / `reload` 帧，并在隔离 VM 中执行真实客户端。
- TypeScript 负向用例拒绝公共 `hmr` 选项。
- Docs、架构静态门禁、Rust fmt/clippy 和定向测试在同一切片执行。

## Supersedes

None.

## Removal plan

旧 `/__wake_hmr` 端点、`/app/hmr` 文档页和 HMR 状态标签在本切片直接删除，不保留并行实现或兼容
桥。未来只有在新 ADR 和上述模块生命周期/浏览器行为证据同时落地时，才可用真正的 HMR 合同替换
Live Reload；替换时必须删除本协议而不是叠加第二个普通更新所有者。
