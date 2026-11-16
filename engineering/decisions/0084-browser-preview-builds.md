# ADR 0084: 浏览器内存构建与预览发布

- Status: accepted
- Date: 2026-11-16

## Context

嵌入式编辑器需要在浏览器中编译前端文件快照，并更新宿主内的预览区域。现有 Node API 加载原生
模块；`BuildSession` 已统一编译和缓存语义，但执行器、计时和磁盘缓存依赖原生环境。Federation
已有包身份、React 共享组和可替换 transport，现有开发更新则包含整页刷新和服务器租约语义。

## Decision

1. 提供独立浏览器产品入口，以 Worker 中的 WASM 运行现有 `BuildSession`。编译器、解析器、CSS
   转换、依赖图和分块继续由现有 crate 拥有；浏览器产品只拥有输入契约、构建生命周期和内存产物。
2. 输入是完整、不可变的虚拟项目快照，包括依赖包文件、文本和二进制内容。路径使用经过验证的
   相对 POSIX 路径；配置直接传对象。首版不读取磁盘、安装依赖、执行安装脚本或主动下载依赖。
   包解析沿用 Wake resolver，包上下文和条件导出参与身份，宿主负责供应固定版本的依赖内容。
   外部共享模块同时携带宿主 provider 的真实包身份；虚拟包名称、版本和上下文匹配后，requirement
   绑定宿主提供的 buildVariant，不按预览编译选项猜测或改写宿主已执行模块的身份。
3. 浏览器执行器在编译 Worker 内串行执行现有任务，不要求共享内存或浏览器多线程。原生执行器
   保留既有并行方式。浏览器没有磁盘缓存；原生计时不应成为浏览器构建的隐式系统调用。
4. 每次成功构建返回完整的 JS、CSS、二进制资源、分块、source map 和 Wake Federation manifest。
   共享同一编译路径与容器协议，保留 host-rendered React 五成员组的版本和身份校验。
   `wake_federation_build` 拥有原生与浏览器共用的纯产物装配、摘要和容器代码，不拥有 OS、HTTP、
   配置发现或会话生命周期。`wake_browser` 只依赖 bundler、resolver、common 和这两个 Federation
   所有者；编译/CSS/图/缓存/解析/任务层不得反向依赖这两个产品层 crate。
5. 本地产物适配器保持可寻址的相对资源图及完整性；新快照完整就绪后才切换预览，旧快照在仍有
   活跃消费者时可加载延迟资源。更新不触发 IDE 整页刷新。取消、迟到结果、失败与释放具有显式
   生命周期；组件状态保持/Fast Refresh 不属于首版保证。
6. 宿主拥有源码草稿、入口生成、依赖供应、挂载/卸载回调、页面上下文、模拟数据与业务 API。
   Wake 不理解工作区、业务分支或后端预览会话。本能力不替代正式发布构建与检查。

## Invariants

- 源码和依赖准备后，构建无需 Node、原生扩展、本地开发服务器或后端编译请求。
- 修改、增加、删除源码或依赖后，增量结果与同配置的冷构建一致；失败不能替换成功产物。
- Manifest 与实际执行/加载字节一致；延迟分块、CSS URL 和图片不依赖偶然的页面相对路径。
- 连续更新只能激活最新请求；取消或销毁后不得提交结果。旧产物的释放不破坏活跃旧页面。
- 宿主 React 共享契约保持有效；预览更新不刷新编辑器文档。
- 不支持的输入明确拒绝，不退回服务器构建，不悄悄更换编译器。

## Evidence

- `crates/wake_bundler/src/session.rs`、`generation.rs`：统一构建与候选会话。
- `crates/wake_common/src/fs.rs`：虚拟文件输入能力。
- `crates/wake_turbo/src/executor.rs`：当前原生线程依赖。
- `crates/wake_app/src/federation.rs`、`assets/federation-runtime.mjs`：现有容器与更新协议。
- `crates/wake_browser/tests/project.rs`：10 项快照、诊断恢复、依赖身份、路径和产物身份测试。
- `crates/wake_browser/tests/browser.rs`：真实浏览器中的原生/WASM 逐字节对照、12 次 runtime 更新、
  Worker 取消/恢复、本地 CSS/图片/旧延迟分块、失败回滚，以及真实 React Hooks/TSX 共享。
- `npm/wake/browser*.mjs`、`browser.d.ts`：Worker API、原子视图交接和按 client/session 隔离的资源存储。
- `crates/wake_turbo/src/executor_wasm.rs`、`wake_bundler/src/timer.rs`：串行 WASM 执行和平台计时。
- 原生 Federation 的 21 个测试与现有 npm Federation 的 61 个测试保持通过。

## Consequences

复用现有编译语义，客户端承担编译器下载与 CPU/内存成本。需维护 WASM 产物发布、Worker 通信、
虚拟输入验证和本地产物生命周期。首次加载开销及重建性能以实际 fixture 测量，不预先承诺数值。
WASM 随主 npm 包发布并由宿主部署；首个 release 产物约 6.8 MB（未压缩），主包压缩后约 2.1 MB。
主包 packed 上限调整为 4 MiB，独立 WASM 上限为 10 MiB；原平台包预算不变。

## Validation

- 原生与 WASM 对同一虚拟项目的行为对照：TSX、CSS、图片、动态导入、诊断和共享依赖。
- 编辑、删除、失败恢复、快速更新、取消和释放的契约测试。
- 真实浏览器中本地加载产物并更新预览，校验无后端构建、无 IDE 刷新及旧分块可用性。
- `cargo test --locked --offline -p wake_browser --test project`。
- `corepack yarn browser:build` 后设置 `WAKE_BROWSER_WASM_PATH` 指向
  `npm/wake/browser/wake_browser.wasm`，执行
  `cargo test --locked --offline -p wake_browser --test browser -- --ignored --nocapture --test-threads=1`。
- `cargo test --locked --offline -p wake_app --lib federation::tests`、
  `corepack yarn npm:test:wake:federation`、`corepack yarn npm:typecheck:wake`。
- `corepack yarn architecture:test`、`corepack yarn architecture:check`、`corepack yarn release:check`，
  以及 `WAKE_PACK_TARGETS=npm/wake corepack yarn npm:pack:check`。

## Supersedes

None.

## Removal plan

无临时兼容桥。现有原生构建与服务器 Federation 开发路径继续服务原有调用方。
