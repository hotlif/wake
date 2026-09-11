# ADR 0046: Docs 开发按访问构建页面与演示

- Status: accepted
- Date: 2026-09-08
- Amended by: [ADR 0047](0047-docs-rendering-interfaces.md)

## Context

Crab 文档站启动仍编译 3702 个模块；动态 import 只推迟浏览器执行，未推迟服务端编译。
用户选择优先首页可用，并接受页面依赖错误在访问时报告。

## Decision

独立 Docs Site 开发服务器启动只构建文档外壳。wake_docs 生成页面、演示及源码的受控入口清单；
wake_app 将这些入口交给 wake_dev_server 已有的 deferred mounts，沿用 watcher 注册、请求合并、
候选发布与失败恢复。Bundler 的模块、chunk 与 generation 所有权不变。

Docs 未访问入口使用显式 `OnRequest` 验证策略：启动及控制文件通知只保留监听覆盖、清除失效的
待处理候选；首次请求重新探测权威配置，并在物化前注册候选监听。默认工作区策略仍按控制通知
探测，避免同一 Docs 配置被几十个未访问入口重复读取，同时保留外壳的启动复查。

浏览器使用 Docs 私有加载器请求这些 bundle，保持 SPA 导航。外壳显式提供 React、React DOM、
JSX runtime 与 Docs 渲染组件；延迟 bundle 用生成的 CommonJS 适配模块引用同一对象，禁止生成
第二份 React renderer。私有通道以 Docs base path 隔离，不借用 Federation broker 或全局 require。
共享适配采用已解析入口的精确重定向。原始请求先完成 PnP 声明、条件导出与文件存在性校验，只有
成功解析到外壳实际共享入口时才转向生成适配文件；普通 alias 的 PnP 优先级保持不变。映射属于
不可变 ResolveOptions/BuildOptions 身份，不能在持有中的 session 内切换，也不能掩盖幽灵依赖。

启动清单固定 URL 所有权。会话中新增的页面/演示暂时使用原有 eager import 路径，下一次启动纳入
延迟清单；删除项目不再出现在导航，但已分配的 URL 不得被另一个入口复用。生产构建、聚合站点及
Components 模式保留完整构建路径。MDX 结构、导航和元数据仍在启动时验证；JS/TS 依赖错误按访问报告。
自定义 Preview 或 JSX import source 可能引入任意共享 Context，因此继续使用完整的单图编译；
不能仅共享 React 就宣称任意 Provider 跨 bundle 等价。

## Invariants

- 首页依赖图不包含未访问页面和演示的静态依赖；不能通过提前发送 ready 跳过 watcher fence。
- 一个浏览器文档中的 React、React DOM 和 JSX runtime 使用相同对象；多个 Docs base path 独立。
- 同一入口的并发请求只触发一次编译；加载失败不保留成功缓存，修复源码后可以重试。
- 路径由生成清单决定，不接受任意文件路径；非 ASCII 名称的编码稳定且不碰撞。
- 候选失败不得替换已发布输出；未访问入口不因普通源文件修改而提前编译。
- 生产产物不含开发加载器或开发 URL。

## Evidence

- `wake_docs/src/dev.rs` 与 `runtime/dev-loader.mjs` 生成受控入口及共享适配，运行时保持导航与失败重试。
- `wake_app` 的 Docs refresh 与 `wake_dev_server` 的 `DeferredValidation::OnRequest` 沿用既有
  候选事务；真实 HTTP 回归验证未访问依赖、失败不替换外壳、配置更新、修复重试与自定义 Preview 回退。
- `wake_resolver::ResolveOptions::resolved_redirects` 的 PnP 回归验证声明成功后才替换入口，以及
  已声明/未声明请求的缓存一致性；原有 PnP alias 优先级测试继续通过。
- 真实 Crab release 样本为 1.894 / 1.960 s 就绪，启动 29 模块；Chrome 导航与 Button Hooks
  计数交互通过。完整环境及阶段结果见 `engineering/PERFORMANCE.md` 第 13 节与对应机器记录。

## Consequences

启动成本由全部演示依赖缩小到外壳及元数据；首次访问重型演示仍承担其编译成本。
不同延迟 bundle 暂时分别编译非共享依赖，访问大量演示后内存和累计工作仍有进一步优化空间。

## Validation

- `cargo +1.95.0 test --offline --locked -p wake_docs -p wake_app -p wake_dev_server --lib`
- `wake test crates/wake_docs/runtime/dev-loader.test.mjs --serial` 验证请求合并、失败重试及共享对象；
  CI 的 architecture job 在构建本轮 CLI 与 test host 后执行该门禁。
- 真实 Crab 工作负载的启动、首次页面访问与演示访问测量。
- 架构检查、文档检查、格式检查及受影响 crate 的 Clippy。

## Supersedes

None.

## Removal plan

无临时编译器或第二套发布机制；原有 eager 路径继续用于生产及尚未接入的 Docs 模式。
