# ADR 0020: 拥有以 React 为先的浏览器测试运行时

- Status: proposed
- Product maturity: experimental
- Date: 2026-08-24

## Context

ADR 0019 选择了纯 Rust Boa VM 和 Jest 30.4 兼容目标。实现证据表明，这实际上合并了四个各自都很
庞大的产品：ECMAScript 引擎、Node/jsdom 宿主、Jest 兼容界面和测试编排器。覆盖率、行内快照、
Node-API、DOM 行为和上游兼容矩阵仍不完整，而兼容承诺又阻碍 Wake 专门为 React 应用选择 API 和
执行语义。

Wake 真正需要的是一个源码形态让 Jest 用户熟悉、但契约由 Wake 拥有的测试产品。React 组件正确性
还需要两种不同证据：适合多数快速反馈的隔离 DOM realm，以及用于布局、原生输入、hydration、截图和
浏览器专用 API 的真实浏览器。Deno 测试运行器及外部 Jest 或 Node 进程都不能拥有这些产品需求。

工作区已拥有 TypeScript/JSX 预处理、解析、源码映射、增量图、CLI/Node 应用收敛和隔离测试宿主进程。
在替换已放弃的兼容目标与引擎时，可以复用这些边界。

## Decision

Wake Test 是 Wake 原生、以 React 为先的框架。`@crab-dev/wake/test` 暴露显式导入且形态类似 Jest 的
`describe`、`test`、hook 和 `expect` 等原语；React 入口拥有渲染、异步 `act`、清理及面向用户的
DOM 辅助函数。API 熟悉不代表兼容 Jest 配置、运行器、报告器、插件、快照、mock、CLI 或 JSON。
Wake 不导出 `jest` 命名空间，也不执行 Deno、Node 或 Jest 测试运行器。

执行系统有七个明确所有者：

- `wake_ecma_vm` 是唯一可直接依赖 crates.io 中校验和锁定的 `deno_core` 包的 Wake crate；
  `deno_v8` 保持为来自 crates.io 的传递依赖。它拥有产品中立的嵌入式 V8 isolate/realm、Promise
  任务队列、终止及稳定诊断门面。Deno CLI 和 `deno test` 不是产品依赖。
- `wake_js_runtime` 拥有 Wake 模块标识、预处理、解析适配器、宿主操作及快速 DOM 环境生命周期。
  其 DOM 实现是固定版本、私有且经 Wake 适配的基础层，不承诺公开兼容 jsdom、Happy DOM 或浏览器。
- `wake_test_browser` 拥有系统 Chromium 系浏览器发现、版本报告、启动、CDP 传输、隔离
  BrowserContext、经认证的回环资源、真实输入、网络拦截、截图和精确 V8 覆盖率。它不拥有发现、
  断言、快照或报告器策略，且绝不依赖 `wake_test`。
- `wake_test_contract` 是可序列化测试选项、结果、诊断及版本化测试宿主线路的唯一所有者。它不依赖
  其他 Wake crate，也不含发现、调度、VM、DOM、浏览器或进程生命周期行为。
- `wake_test` 是发现、调度、创作 API、React 集成、函数与网络 mock、现代异步时钟、快照格式、
  覆盖率归一化、监视失效及结果构造的唯一所有者。它使用 `wake_test_contract`，并可组合
  `wake_js_runtime` 与 `wake_test_browser`。
- `wake_test_host` 是持久隔离与会话 IPC 的唯一所有者。它认证请求并检查版本，隔离 VM 或浏览器崩溃，
  传播取消并关闭资源；它使用 `wake_test_contract` 的线路协议，把所有测试语义委托给 `wake_test`。
- `wake_app` 仍是 CLI 与 Node 调用方配置、启动、观察、取消和关闭测试会话时共同经过的产品边界。
  它只链接 `wake_test_contract`，以独立进程启动测试宿主，绝不链接运行器、JavaScript 运行时或嵌入式 V8。

默认快速环境为每个套件创建全新的 V8 realm、模块注册表、Window 和 Document。DOM 基础层在 React 与
React DOM 求值前安装，保持同 realm 构造函数标识，设置 `IS_REACT_ACT_ENVIRONMENT`，将计时器和网络
委托给 Wake，并在套件结束后完全销毁。其兼容承诺仅限于 Wake 的版本化 React/DOM 一致性清单。

浏览器环境启动或附加到显式选择的 Chrome、Edge 或 Chromium 可执行文件。一个浏览器进程可以复用，
但每个套件都获得新的 BrowserContext 和页面。可执行文件标识与版本属于诊断、结果和缓存标识的一部分。
浏览器二进制文件不放入 Wake 现有平台包；CI 提供并固定用于发布证据的浏览器。
`engineering/system-browser-conformance.json` 架构 v2 将两类声明分开。实验性发布证据遵循已审查的
托管 runner 清单：Windows x64 和 Linux x64 对 Chromium 主版本 151 运行阻塞式精确主版本一致性；
macOS x64 对已审查的主版本 151、macOS arm64 对已审查的主版本 150 运行阻塞式功能浏览器冒烟测试，
并明确不视为稳定一致性；
Linux ARM64 记录机器可读的浏览器不可用状态，绝不下载替代品。稳定就绪策略更严格：五个目标必须在
同一 Chromium 系精确主版本上运行一致性测试。因此，当前清单的稳定就绪结果为 `ready: false`，并
列出各目标阻塞项。这些固定版本仅是 CI 与发布证据的准入策略；普通本地测试仍接受任何兼容的系统
Chrome、Edge 或 Chromium，并始终报告完整 CDP 标识。

同一 DOM 内的测试按顺序执行。不同套件可在隔离 realm 或 BrowserContext 中并行。运行时模块自动
mock、Jest 式提升、旧版伪计时器和行内快照重写不属于契约。模块替换必须显式声明，并在模块求值前解析；
浏览器网络 mock 在驱动边界执行。覆盖率使用 V8 范围，通过 Wake 源码映射转换为 Wake 自有架构，而非
Babel/Istanbul 插桩语义。

相关测试选择及监视失效使用 `wake_js_runtime` 编译出的精确自有模块图；不复用打包器分块图或
`wake_graph` 存活记录。运行时 sidecar 将逻辑模块 ID 与物理监视路径分离，且只含已排序的自有记录。
`wake_test` 拥有套件到模块及模块到套件索引。动态加载、当前 PnP 解析、解析失败和结构输入会使图变为
不透明；Wake 此时会选择全部候选套件并发出诊断，绝不冒漏选风险。完整 PnP 精度属于独立解析切片，
仍是发布门禁。

`--changed` 在 Wake 中只有一种确定含义：相对于 `HEAD` 比较已跟踪的暂存及未暂存路径，再加入未忽略的
未跟踪文件；禁用重命名检测，使删除和新增都可见；没有 `HEAD` 的新仓库使用索引加未跟踪文件。缺少
Git 或根目录不在工作树中时返回 `WAKE_TEST_DISCOVERY`；Wake 不猜测上游分支，也不模拟 Jest SCM
启发式。

宿主协议以原子方式升级为版本 3，其完整可序列化架构和帧编解码器都位于 `wake_test_contract`。
`StartWatch` 携带上下文的冻结选项，并创建唯一的递归原生监视器；`WatchControl` 拥有 `all`、
`failed`、`path`、`name`、`updateSnapshots` 和 `rerun` 转换。宿主合并变更、中断过时运行，并在
认证会话上发出有序的主动运行事件。JavaScript 不拥有文件监视器，也不合成测试结果事件。每次重跑都
在新 realm 或 BrowserContext 中使用编译图，同时只保留发现与已编译依赖产物。

## Invariants

- CLI、Node 和宿主中的 Wake Test 只有一个语义所有者 `wake_test`，可序列化契约只有一个所有者
  `wake_test_contract`。
- `wake_test_contract`、`wake_app`、`wake_cli` 和 `wake_node` 的传递依赖中不得包含测试运行器、
  JavaScript 运行时、VM、浏览器驱动、`deno_core`、`deno_v8`、`serde_v8` 或 `v8`；这些携带引擎
  的依赖必须终止于单独打包的 `wake_test_host`。
- 只有 `wake_ecma_vm` 可直接依赖 `deno_core`；引擎句柄绝不越过其公共边界。
- 第三方 Rust/JavaScript 源码或二进制文件绝不复制进仓库。Rust 依赖通过 `Cargo.lock` 从 crates.io
  解析；JavaScript 依赖通过 `package-lock.json` 从 npm 注册表解析。正式构建步骤在单独完成校验和验证
  的依赖或产物准备后必须锁定且离线。
- `wake_test_browser` 可被 `wake_test` 使用，但不能反向依赖它或实现框架语义。
- 每个套件拥有一个隔离 realm/模块注册表或 BrowserContext/页面。DOM 节点、V8 句柄、CDP 会话标识、
  进程内 atom 及 arena 引用都不得进入 IPC 或持久化。
- 快速 DOM 绝不解析项目模块、执行无中介网络 I/O、在 Wake 时钟之外捕获真实计时器函数，或通过第二个
  加载器求值脚本。
- 真实 Chromium 是布局、CSS 渲染、原生输入、焦点、导航、截图及浏览器敏感 hydration 的权威。
  快速 DOM 不支持的行为必须失败或转由浏览器验证，不能静默近似并当作浏览器证据。
- 每个浏览器页面都将 `prefers-reduced-motion` 固定为 `reduce`；同一值参与截图渲染配置哈希和产物
  元数据，避免宿主动画设置复用不兼容基线。
- React 渲染和用户交互通过异步 `act` 稳定；套件清理会移除根节点、DOM 状态、计时器、存储、拦截请求
  和待处理句柄。
- 源码位置和覆盖率映射回原始 JS、TS、JSX 或 TSX 源码。
- 发现、排序、种子、文本快照和归一化覆盖率保持确定。缓存标识包括编译器选项、框架/运行时版本、环境、
  引擎或浏览器版本、DOM 适配器版本及所有语义配置输入。
- 只有自有反向索引证明关系时，变更路径才能少选套件；拓扑变更、删除/新增路径、解析器输入及不透明边会
  重新发现或选择全部。
- `TestContext.run()`、`startWatch()` 和 `stopWatch()` 保持为无参数公共上下文方法；交互式选择属于
  内部协议命令，不扩展 npm API。
- 浏览器资源源和宿主会话必须位于本地、经过认证，并在成功、失败、超时、取消或子进程崩溃时关闭。
- 不受支持的 Jest、Node、Deno、DOM 或 Chromium 扩展行为产生结构化 Wake 诊断；不存在隐藏的外部
  运行器或第二 VM 后端。

## Evidence

- `engineering/ARCHITECTURE.md` 和可执行边界策略确定 `wake_app` 为共享 CLI/Node 应用边界。
- `wake_ecma_vm` 使用 crates.io 中校验和锁定的 `deno_core` 0.410.0，并暴露自有 Wake 值和诊断，
  而非 V8 句柄。
- `wake_test_browser` 具有独立的浏览器发现、CDP、BrowserContext、资源源、输入、截图和精确覆盖率
  接口，且不依赖任何 Wake 工作区 crate。
- `wake_resolver`、解析器和代码生成器已拥有包/Yarn PnP 解析，以及保留源码标识的
  JS/TS/JSX/TSX 预处理。
- React 19 要求异步 `act` 和 `IS_REACT_ACT_ENVIRONMENT`；React 已删除其他旧版
  `react-dom/test-utils` API，并建议采用面向用户的 DOM 测试。
- 模拟 DOM 实现不能提供权威布局、导航或渲染，因此浏览器敏感行为必须有实际执行的 Chromium 证据。

## Consequences

Wake 不再承诺实现 Jest 30.4。使用 `jest` 命名空间、Jest 专用配置或快照的现有实验性测试需要迁移到
Wake API 和格式。这是在测试产品稳定前有意进行的破坏性替换。

嵌入 V8 可提高 ECMAScript 与 React 运行时保真度，但会以由 Rust 门面包装的注册表原生引擎取代纯
Rust 引擎承诺。源码来源、许可证、锁文件校验和、工具链可复现性、受支持 libc 基线和二进制大小成为
发布门禁。仓库本地 fork 或 vendored 副本不能作为回退；必需的上游变更必须先通过批准的注册表源发布，
Wake 才能使用。

快速 DOM 提供短反馈，但其事实范围有意小于 Chromium。维护两种环境会增加差异测试和缓存输入，但不会
产生两套框架实现，因为它们共享 Wake 测试内核与结果模型。

浏览器模式需要兼容的本地 Chromium 系可执行文件。系统浏览器差异通过结果元数据显式呈现。CI、阻塞式
发布前冒烟和发布后注册表冒烟会依据目标已审查的实验策略验证每个可用浏览器的 CDP 后标识，并保留证据
产物。五平台非浏览器发布契约绝不变为可选：Linux ARM64 仍执行干净安装、`wake test`、`runTests()`
和 `TestContext` 冒烟，同时记录其已审查 runner 不提供浏览器。托管 runner 清单变化必须经过清单更新
审查，不能触发下载、回退浏览器或静默跳过。实验性发布可在明确不可用的情况下继续，但 ADR 接受和稳定
Wake Test 发布仍被阻塞，直到五个目标都通过共享精确主版本一致性策略。这不限制普通用户使用该主版本。
浏览器不计入当前 npm 平台包大小预算。

原生扩展托管、完整 Node 兼容、第三方 Jest 运行器/报告器/环境 ABI、旧版计时器、Babel 覆盖率及 Jest
黄金输出不再属于稳定发布负担。

## Validation

- 运行 `npm run architecture:test` 和 `npm run architecture:check`；拒绝 `wake_ecma_vm` 之外直接
  使用 `deno_core`、`wake_test_browser` 到 `wake_test` 的反向依赖、`wake_test_contract`、
  `wake_app`、`wake_cli` 或 `wake_node` 的全 feature/全目标 normal/build 依赖闭包中出现任何运行器、
  运行时或 V8 包，以及宿主闭包缺少权威契约与运行器。同时拒绝仓库本地第三方源码/二进制树、非注册表
  依赖、不完整锁文件来源及可联网的正式构建 hook。
- 固定引擎源码/校验和，并针对受支持 ECMAScript、模块、Promise、终止及源码位置行为运行选定的
  Test262 清单。
- 运行 React 19 夹具，覆盖 createRoot、hook/effect、portal、受控表单、Suspense、惰性模块、错误边界、
  异步 `act`、SSR 解析和 hydration 诊断。
- 运行快速 DOM 清单，覆盖 realm 标识、MutationObserver、焦点、选择、表单、清理、计时器和拦截网络；
  在 Chromium 中差异执行每个浏览器敏感夹具，并记录可接受的快速环境边界。
- 在隔离 BrowserContext 中执行真实键盘/指针/默认操作、CSS/布局、hydration、无障碍、截图、网络拦截
  和 V8 覆盖率。
- 通过 Wake 源码映射将运行时错误和覆盖率映射到原始 TSX，并比较冷、热及监视重跑的语义和确定性顺序。
- 在临时 Git 仓库中覆盖暂存、未暂存、忽略、未跟踪、重命名/删除、新生 `HEAD`、嵌套根、缺失 Git 及
  非仓库行为；测试直接、传递、共享、不透明和结构化反向依赖选择，确保无漏选。
- 测试协议 v3 的监视启动/停止/控制、文件系统防抖、过时运行取消、有序主动事件、同一 TCP 会话重复运行
  及全新 realm 状态。CLI PTY 测试覆盖 `a`、`f`、`p`、`t`、`u`、`r` 和 `q`；非 TTY 监视持续到
  收到信号或关闭。
- 测试超时、无限循环、未处理 rejection、浏览器/宿主崩溃、取消、畸形 IPC、资源源认证和幂等关闭，
  确保不泄漏子进程、端口、页面或配置目录。
- Windows x64 和 Linux x64 验证系统浏览器精确主版本 151，以及 CDP、React、截图与覆盖率一致性。
  macOS x64 验证已审查的精确主版本 151、macOS arm64 验证已审查的精确主版本 150 和相同功能冒烟，
  但标记为不满足稳定就绪一致性。
  Linux ARM64 执行所有非浏览器平台门禁并生成已审查的 `unavailable` 证据；绝不下载或选择回退浏览器。
- 独立评估稳定浏览器就绪情况：只有 Windows x64、Linux x64/arm64 和 macOS x64/arm64 全部针对同一
  精确主版本执行一致性测试，才可得到 `ready: true`。当前架构 v2 证据必须得到 `ready: false`，
  因此 ADR 接受仍被阻塞。
- 审计注册表许可证/SBOM、V8 产物、原生符号、GLIBC 基线、npm 打包允许列表及大小限制。浏览器可执行
  文件和第三方源码或二进制副本不得进入仓库或现有平台 tarball。

只要上述任一适用门禁缺失，公共入口就保持实验性。接受本 ADR 要求仓库中不存在活动的
Jest/Boa/jsdom/Node-API 兼容路径或过期发布承诺，要求每个受支持平台门禁都从干净安装产物通过，还要求
五目标共享精确主版本浏览器就绪结果为 `ready: true`。仅通过架构 v2 的实验性发布拆分，不足以接受
本 ADR 或标记为稳定。

## Supersedes

[ADR 0019](0019-native-test-runtime.md).

## Removal plan

替换 Boa 依赖与旧 Jest 运行时，不保留可选后端。删除 `jest` 命名空间、Jest 配置/CLI/JSON 映射、
Jest 快照头和行内重写契约、Babel 覆盖率、旧版伪计时器、手写 jsdom shim，以及 Node-API/原生扩展
一致性要求。重命名或替换仍暗示这些契约的测试、文档和诊断。

保留 `wake_ecma_vm`、`wake_js_runtime`、`wake_test`、`wake_test_host`、`wake_app`、`wake test`、
`runTests()` 和 `TestContext`；新增 `wake_test_contract` 作为唯一结果与线路所有者，并以原子方式将其
内部实现和实验性公共类型迁移至本决策所述所有权。删除 `wake_app -> wake_test` 直接依赖；应用与壳层
只通过契约值同单独打包的宿主交互。新增 `wake_test_browser` 作为唯一 Chromium/CDP 所有者。替换收敛
后，不保留弃用的 Jest 门面、外部运行器回退、第二套结果架构、第二个协议所有者、第二个 DOM 加载器或
第二个 VM 后端。
