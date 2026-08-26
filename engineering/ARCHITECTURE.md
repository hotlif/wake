# Wake 当前架构

## 1. 系统定位

Wake 是 Rust 原生的 Web 构建工具，同时提供 CLI、Node.js API、应用开发服务器、React 组件文档系统和实验性的 React 优先测试系统。工作区当前包含 33 个第一方 workspace crate；`deno_core`、`deno_v8` 等第三方 Rust 包来自 crates.io，JavaScript 包来自 npm registry。npm 主包通过五个平台包加载预编译 Node-API 模块和内部 test host，系统 Chromium 不进入这些平台包。

稳定入口不是各编译 crate 的任意组合，而是：

- `wake_cli`：命令行与终端交互；
- `wake_node`：Node-API 原生绑定；
- `wake_app`：两种入口共享的应用服务；
- `wake_docs`：文档项目生成器与运行时资源。

## 2. Crate 分层

### 2.1 基础与编译核心

- `wake_common`：Span、源码、诊断、文件系统抽象、字符串驻留和压缩包读取。
- `wake_ecma_ast`：arena AST、自引用持有者、访问器和结构指纹。
- `wake_ecma_lexer`、`wake_ecma_parser`：词法、语法、依赖提取，以及必须借助 cover grammar/作用域上下文完成的 lowering 编排。
- `wake_ecma_semantic`：独立的符号、作用域与引用分析；不通过 parser façade 暴露。
- `wake_ecma_transform`、`wake_ecma_codegen`、`wake_ecma_minify`：可复用 lowering 规则、生成、Source Map 和生产压缩。

编译核心只依赖 workspace 白名单中的少量通用库，不依赖网络、CLI 或 Node-API。

### 2.2 解析、图与资源层

- `wake_resolver`：`ResolutionEnvironment` 统一拥有经典 Node/`node_modules`/alias/package exports 与
  按 issuer 的 Yarn PnP registry、virtual/unplugged 路径、STORE/DEFLATE zip 文件系统和结构失效缓存。
- `wake_graph`：模块和绑定活跃性、Tree Shaking 所需图分析。
- `wake_css`、`wake_css_in_js`、`wake_html`：非 JavaScript 内容和 HTML 外壳。
- `wake_css_language`：`@crab-dev/css` 模板发现、虚拟 CSS、宿主映射和无文件系统编辑分析；构建诊断仍由 `wake_css_in_js` 拥有。
- `wake_scan`、`wake_tsdoc`：组件扫描、Demo/Props 类型和 JSDoc 提取。

### 2.3 执行与编排层

- `deno_core`、`deno_v8`：由 crates.io 与 `Cargo.lock` 固定的第三方嵌入式 V8 内核；只有 `wake_ecma_vm` 可以直接依赖 `deno_core`，`deno_v8` 保持其传递依赖。
- `wake_ecma_vm`：产品无关的 V8 isolate/realm、Promise job queue、中断与稳定诊断 façade；不公开 V8 handle 或 Deno API。
- `wake_js_runtime`：Wake 模块预处理、ESM/CommonJS/JSON 图、宿主操作和 fast DOM realm 生命周期；DOM 内核是固定版本的私有适配，不是 jsdom/Happy DOM/浏览器兼容承诺。
- `wake_test_browser`：系统 Chrome、Edge 或 Chromium 的发现与版本记录、CDP、BrowserContext、真实输入、网络拦截、截图、V8 coverage 和本机资源 origin；不拥有测试语义。
- `wake_test_contract`：唯一可序列化测试 options、result、diagnostic 与 test-host protocol v3 的 owner；不拥有发现、调度、VM、DOM、浏览器或进程生命周期。
- `wake_test`：Wake 原生的发现、调度、显式测试 API、React adapter、mock、异步 clock、snapshot、coverage 规范化、watch 与结果构造；消费 `wake_test_contract`，仅保留 Jest 风格的熟悉书写形式，不兼容 Jest。
- `wake_test_host`：唯一持久测试 session、IPC transport、取消、进程/浏览器崩溃隔离和关闭 owner；消费 `wake_test_contract` 并调用 `wake_test`，不拥有断言或 React 语义。
- `wake_turbo`、`wake_turbo_macros`：红绿失效、single-flight、任务宏和工作窃取执行器。
- `wake_cache`：稳定 DTO 的持久化编码；不保存进程内 `Atom` 或 AST 指针。
- `wake_bundler`：Scan、Link、chunk、runtime、资源与 emit 编排。
- `wake_config`：声明式 TOML 配置、项目根发现和 Browserslist 规范化。

### 2.4 产品边缘层

- `wake_dev_server`：HTTP、WebSocket、监听、HMR 和代理。
- `wake_docs`：MDX、Demo、API 表、组件工作台和静态路由生成。
- `wake_app`：配置、构建会话、文档构建、错误和取消的统一应用层。
- `wake_cli`、`wake_node`：面向用户的 Rust CLI 与 JavaScript 绑定。
- `wake_css_lsp`：增量 LSP 文档、受限依赖缓存和保存时静态分析；`editors/vscode-css` 是启动该服务的薄 VS Code 客户端。

## 3. 依赖方向

```text
common ─ ecma_ast
   ├─ lexer
   ├─ semantic
   └─ transform ─ parser（消费 lexer + lowering helpers）

parser + semantic + transform ─ codegen/minify

common ─ resolver
common + ecma_ast ─ graph
common ─ css/html/scan/tsdoc/docs

compiler + resolver + graph + assets
  └─ bundler ─ dev_server

turbo + cache ─ bundler

config + bundler + dev_server + docs
  └─ app ─ cli
         └─ node binding

deno_v8 (crates.io transitive) → deno_core (crates.io) → wake_ecma_vm → wake_js_runtime
wake_test_contract → wake_test
wake_js_runtime + wake_test_browser → wake_test
wake_test_contract + wake_test → wake_test_host
wake_test_contract → wake_app → cli / node binding
```

必须维持的约束：

1. 编译核心不得依赖 CLI、Node、dev server 或 bundler；基础、AST、lexer、semantic、transform 与 parser 的允许出边由机器规则逐层限定。
2. `wake_graph` 不读取文件、不解析路径、不写产物。
3. `wake_cache` 只保存跨进程稳定数据，不保存 arena/Atom 句柄。
4. `wake_turbo` 不认识 Web 构建领域对象。
5. CLI 和 Node 的配置、错误、构建、Docs 与服务生命周期经 `wake_app` 共享；显式 compiler/experimental API 可直达 compiler crates，但不得直达 bundler 或其他产品层。
6. `wake_ecma_vm` 是 Wake 对 `deno_core` 的唯一直接依赖者；它不读取文件、不解析包，也不认识 DOM 或测试框架。AST、Atom、V8 handle 和 DOM 节点不进入持久缓存或 IPC。
7. `wake_js_runtime` 拥有 fast DOM 的创建与销毁，但项目解析、timer、network 和脚本求值仍分别经 Wake resolver、clock、host 与 module loader；私有 DOM 内核不得形成第二条执行路径。
8. `wake_test_contract` 是测试 DTO 与 protocol v3 的唯一 owner，不依赖其他 Wake crate，也不包含执行、文件系统或进程行为。`wake_test` 消费 contract 并拥有全部测试语义；host 同时消费 contract 与 runner。
9. `wake_test_contract`、`wake_app`、`wake_cli` 与 `wake_node` 的 normal/build 传递依赖闭包不得出现 runner、JavaScript runtime、VM、browser driver 或 V8；这些依赖只终止在独立 test host 中。
10. `wake_test_browser` 只适配 Chromium/CDP；允许 `wake_test` 依赖并编排它，禁止 browser driver 反向依赖测试框架。
11. 每个测试 suite 独占 V8 realm/module registry/Window/Document，或独占 Chromium BrowserContext/page。测试在同一 DOM 内顺序执行，suite 才能跨隔离环境并行。
12. CLI 和 Node 请求经 `wake_app` 进入唯一持久 `wake_test_host` session；protocol v3 schema 与 frame codec 由 `wake_test_contract` 拥有，host 拥有 transport。协议必须带随机令牌，并在完成、失败、取消或崩溃后关闭 VM、页面、端口和子进程。
13. 仓库不保存第三方源码、原生二进制或归档副本。Rust 第三方依赖只由 crates.io 与
    `Cargo.lock` 固定；JavaScript 第三方依赖只由 Yarn 4.16 `yarn.lock` locator/checksum 与 PnP
    registry 固定。源码安装使用 `yarn install --immutable --check-cache`，不生成 `node_modules`；
    正式 Cargo build 使用 locked/offline 输入，`build.rs` 与依赖 lifecycle 不下载。
14. `.pnp.cjs` 是 PnP 唯一激活标志。受管理的有效裸包由 Yarn 成功或拒绝最终裁决，Wake alias 与
    Components 不能覆盖；ignore/unmanaged issuer 按 Yarn 指示执行经典 Node 解析。所有消费者只通过
    `ResolutionEnvironment` 访问解析状态并传播结构化诊断。没有 PnP 根的 npm 项目以实际
    `node_modules` 文件树为权威；`package-lock.json` 只触发结构失效，不参与运行时依赖求解。

上述依赖方向与来源规则的唯一机器事实来源是
[architecture-boundaries.json](architecture-boundaries.json)；叙述文档不维护第二份 allowlist。

## 4. 应用构建数据流

```text
CLI / Node API
  → wake_app 解析 cwd、configPath 与取消信号
  → wake_config 发现并规范化项目
  → BuildSession / IncrementalBundler
  → resolve + load + parse
  → module graph + liveness + chunk graph
  → transform + codegen + minify
  → JS/CSS/assets/HTML/manifest
  → 内存返回或原子写入 outdir
```

开发服务器持有 `BuildSession`，文件变化经 watcher 合并后执行失效和重建，再通过 WebSocket 发送更新。一次性生产构建会绕过不需要的常驻服务资源。

## 4.1 测试数据流

```text
wake test / runTests() / TestContext
  → wake_app 解析 cwd、wake.config.toml、取消信号与 test-host 路径
  → wake_test_contract 编码随机令牌 + protocol v3 握手、options 与事件/result frames
  → 持久 wake_test_host session 解码 contract 并调用 wake_test runner
  → wake_test 发现 suite、构建反向依赖、调度、snapshot、coverage 与结果事件
  → fast DOM：wake_js_runtime 为 suite 创建 module registry 与 DOM realm
      → wake_ecma_vm → crates.io-resolved deno_core/V8 执行并清空 Promise jobs
  → browser：wake_test_browser 启动/连接系统 Chromium
      → token 化本机资源 origin → 独立 BrowserContext/page → CDP 事件与 artifacts
  → wake_test_contract 的可序列化 TestRunResult 返回 CLI 或 Node
```

测试代码必须显式导入 `@crab-dev/wake/test`。普通 Node/Wake 运行上下文加载该入口会以
`WAKE_TEST_CONTEXT` 失败。公开测试语义只由 Wake 拥有；产品路径不调用 Node、Jest、Deno CLI
或 `deno test`。fast DOM 只承诺 Wake React conformance manifest，layout、原生输入、导航、截图
与浏览器敏感 hydration 以 Chromium 结果为权威。长期决策与稳定门禁见
[ADR 0020](decisions/0020-react-browser-test-runtime.md)。

## 5. 文档构建数据流

```text
docs source_dir
  → MDX frontmatter / headings / imports
  → Demo 扫描与 Props/JSDoc 提取
  → 生成 .wake/docs 虚拟 React 项目
  → 与 Wake Docs runtime、Preview、主题一起打包
  → site routes 或 components hash workbench
  → docs-dist 静态产物
```

Site 模式生成页面、导航、搜索索引和静态子路由外壳。Components 模式只要求 `.demo.tsx`，以 hash 保存 Demo、非默认 Args、显式 unset 字段和视口状态，因此不要求托管端重写深链接。

聚合 Docs 由 `wake_app` 发现和编排：主站与每个 Components 工作台仍分别调用单项目
`wake_docs` 生成器并独立打包。生产输出先进入隔离 staging 树再逐文件事务提交；开发时
`wake_dev_server` 用一个最长前缀挂载注册表、一个监听线程和一个 HMR endpoint 持有多个
独立 `BuildSession`。lazy 工作台首次请求才创建会话，事件和浏览器刷新按挂载身份隔离。

## 6. 对外边界

`wake_app` 是 CLI 与 Node 绑定的稳定内部边界：

- 将底层诊断映射为 `WakeError` 和结构化 `DiagnosticInfo`；
- 为一次构建、增量上下文、应用服务器和文档服务器提供统一选项；
- 管理取消、关闭与输出目录安全检查；
- 隔离 Node-API JSON 传输和 CLI 终端表现。

测试路径中 `wake_app` 只依赖并重导出 `wake_test_contract` 的稳定 DTO，启动独立
`wake_test_host` 并处理 session lifecycle；它不链接 `wake_test`、`wake_js_runtime`、
`wake_ecma_vm` 或 V8。

JavaScript 类型以 `npm/wake/index.d.ts` 和 `experimental.d.ts` 为公开事实来源。

## 7. 当前架构分叉

`wake_bundler` 同时公开同步 `Bundler` 与基于 `IncrementalBundler` 的 `BuildSession`。应用层主流程使用会话/增量路径，但同步路径仍被测试和部分内部调用覆盖。

这不是新的功能入口。后续应先建立两条路径的产物等价测试，再决定把同步路径降为薄适配器或内部测试工具；在此之前不得删除其回归用例。验收条件见 [ROADMAP.md](ROADMAP.md)。

## 8. 发布边界

npm registry 包与 Crab CSS VS Code 扩展是两个独立产品边缘：前者只由
`release-npm.yml` 发布，后者只由 `vscode-css.yml` 发布。各自的 package manifest 是版本事实
来源，标签必须与 manifest 精确一致；发布 job 只消费已经构建和审计的不可变制品。可执行的
覆盖规则由 `scripts/check-release-coverage.mjs` 持有，长期决策见
[ADR 0011](decisions/0011-release-automation.md)。

五个平台包各自只携带一个 Node binding、`test-host/wake-test-host[.exe]`、原生 manifest、SPDX
SBOM 和许可证清单；manifest 的 artifact checksum 与跨平台 build ID 必须能从 tarball 内容复算。
平台包不得包含浏览器、下载器或 install script，测试运行时只连接显式路径或系统发现的 Chromium。
发布准备步骤必须先校验各 target 固定的 Rusty V8 归档 SHA-256，再通过本地
`RUSTY_V8_ARCHIVE` 交给 locked/offline 的正式构建消费；仓库、npm 包和 Cargo build script
均不得保存或下载该归档。平台包预算为 64 MiB packed、192 MiB unpacked，超过 56 MiB packed
即产生发布 warning。
