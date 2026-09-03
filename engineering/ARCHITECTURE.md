# Wake 当前架构

## 1. 系统定位

Wake 是 Rust 原生的 Web 构建工具，同时提供 CLI、Node.js API、应用开发服务器、React 组件文档系统和实验性的 React 优先测试系统。工作区当前包含 36 个第一方 workspace crate；`deno_core`、`deno_v8` 等第三方 Rust 包来自 crates.io，JavaScript 包来自 npm registry。npm 主包通过五个平台包加载预编译 Node-API 模块和内部 test host，系统 Chromium 不进入这些平台包。

稳定入口不是各编译 crate 的任意组合，而是：

- `wake_cli`：命令行与终端交互；
- `wake_node`：Node-API 原生绑定；
- `wake_app`：两种入口共享的应用服务；
- `wake_docs`：文档项目生成器与运行时资源。
- `wake_compiler`：借用源码、返回 owned 产物的单模块 Rust 转译入口；初期不发布到 crates.io。

## 2. Crate 分层

### 2.1 基础与编译核心

- `wake_common`：Span、源码、诊断、文件系统抽象、字符串驻留和压缩包读取。
- `wake_ecma_ast`：arena AST、自引用持有者、访问器和结构指纹；parser 创建的 `ModuleAst` 同时拥有
  精确源码字节，并记录创建 AST `Atom` 的进程内 interner identity。
- `wake_ecma_lexer`、`wake_ecma_parser`：词法、语法、依赖提取，以及必须借助 cover grammar/作用域上下文完成的 lowering 编排。
- `wake_ecma_semantic`：独立的符号、作用域与引用分析；不通过 parser façade 暴露。
- `wake_ecma_transform`：可复用 lowering 规则与 AST helper。
- `wake_ecma_minify`：拥有 `TypedProgram` 可变语法树、稳定节点/列表/名字/符号身份、当前树分析、
  可信编辑、显式 pass 固定点、typed module plan 和属性/标识符改名。Parser AST 只在入口降低一次。
- `wake_ecma_codegen`：从最终 `TypedProgram` 直接发射合法 token、最短字面量与 Source Map；生产压缩
  路径不读取 parser AST 或外部优化计划，也不重新决定 DCE、内联、模块改写或名字。
- `wake_compiler_core`：共享 `Interner` 上的纯 `parse_module`/`optimize_module`/`emit_module` 编排；
  finalize 只在 emit 内瞬时存在，
  不拥有文件系统、任务、缓存、配置、Bundle、Chunk、CSS 或运行时组装。
- `wake_compiler`：React-compatible JSX 的 Babel-like 单模块 façade；输入借用，JavaScript、detached
  Source Map、模块请求与诊断全部 owned，且不暴露 AST/Atom/进程内 ID。

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
- `wake_cache`：稳定派生 DTO 的有界、校验、事务式持久化；不保存源码快照、路径元数据、进程内
  `Atom` 或 AST 指针。
- `wake_federation_contract`：Federation v1 配置、Manifest、lock reference、跨构建身份、共享策略、
  资源/类型元数据、稳定错误码与 dev update/lease 的唯一 owner；只拥有可序列化数据，不拥有 I/O、解析、
  hashing、semver 求解或运行时执行。
- `wake_bundler`：`BuildSession` 是单次产品编译的唯一公共 owner；它从一次性传入的 typed、owned
  `BuildOptions` 冻结构建语义，再编排 Scan、Link、chunk、runtime、资源与 emit。`BuildGeneration` 则拥有
  一个完整产品候选的 retained/one-shot 编译视图和 generation-scoped 文件观察。crate 内部私有拥有 engine
  setter 与 scope-concat 的保守 AST 安全扫描；codegen 不导出 concat eligibility 分析接口。
- `wake_config`：声明式 TOML 配置、项目根发现和 Browserslist 规范化。

### 2.4 产品边缘层

- `wake_dev_server`：HTTP、WebSocket、监听、Live Reload 和代理。普通应用只在成功增量重建后
  发送整页刷新；Federation 局部 remount 与 development snapshot lease 使用独立版本化协议。
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

parser + semantic + transform ─ minify ─ codegen
              \___________________________/
                           └─ compiler_core ─ compiler

common ─ resolver
common + ecma_ast ─ graph
common ─ css/html/scan/tsdoc/docs

compiler_core + resolver + graph + assets
  └─ bundler ─ dev_server

turbo + cache ─ bundler

federation_contract ─ config + bundler

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
15. JavaScript 优化只有 `wake_ecma_minify` 一个生产语义入口。模块图用稳定 export 名称分别保存
    “声明绑定必须保留”和“公开 key 确实被观察”，并按源码顺序解析 plain `export *` 的精确名称或 opaque
    fallback；bundler 以 `LinkerExportLiveness` 和结构化 star plan 交给优化器。优化器在 `optimize` 边界建立唯一的 parser semantic model，只把
    声明保留名称解析为本次程序的 `SymbolId`，公开观察名称保持稳定字符串，并同时降低
    `TypedProgram`；进程内 ID 不跨 crate 边界或重新解析持久化。优化器报告的依赖再映射为保留的
    `ModuleId`。未提供 linker liveness 的
    preserve ESM 保留公开导出的本地名，只有带已校验 liveness 的 linked 输出可以缩名。Codegen 的生产
    压缩入口只接收优化器拥有的最终 `TypedProgram`，不得另收 parser AST 或自行重建压缩、名字优化、
    hoist、属性改名和模块计划；内部转换只能通过 `OptimizeInput` 的结构化可信编辑进入 typed IR。
16. `minify` 是完整优化管线的唯一公开开关，并可与 Source Map 同时启用。是否生成 map 不得改变
    JavaScript payload；改名段通过 V3 `names`/第五 VLQ 字段携带原名。持久缓存独立保存模块映射与
    names 表。`want_map` 不进入 optimize 或 body 任务身份；body 的同一次 token walk 总是记录映射事实，
    独立 map 任务只消费这些事实，因此 mapped/unmapped 共用同一优化结果和 JS body。
17. 属性改名仅限类私有名和可证明未逃逸、无动态/反射访问的局部封闭对象形状；公共类成员、宿主属性、
    `__proto__` 与协议名始终保留。该边界不得通过 whole-world 或 externs 假设放宽。
18. 生产 `optimize` 必须同时校验 `OptimizeInput.source` 与 parser owner 的精确源码字节，以及传入
    `Interner` 与 owner 记录的 identity。Span、`SymbolId` 与 `Atom` 只属于同一解析结果；即使另一张
    interner 表碰巧能 resolve 同一数值也不得接受。该进程内 identity 不进入持久缓存。
19. `wake_federation_contract` 是 Federation wire DTO 的唯一事实来源且不依赖其他 Wake crate。
    `(container, buildId, expose, generation)` 是跨构建模块身份；数字 module ID 与现有 runtime token
    只在各自 container 内有效。URL/文件访问、内容 hashing、package resolution、semver 求解与 remote
    执行必须由 bundler 或产品边缘层拥有，长期边界见
    [ADR 0025](decisions/0025-wake-native-federation-contract.md)。
20. Federation-enabled 的一个生产候选只由一个 `BuildGeneration` 拥有。application retained/one-shot
    view 与 container/shared one-shot view 必须共享同一 observation epoch；生产子构建不得另建文件系统或
    session。该 epoch 只冻结已观察的 method/path 对，不承诺全局或跨方法文件系统快照，长期边界见
    [ADR 0028](decisions/0028-build-generation-ownership-and-observation-cache.md)。
21. Node/npm 公共边界不拥有第二份产品语义。Federation init/lock 由 `wake_app` 统一实现；输出 kind
    与 Federation 错误码必须在 Rust、Node runtime 和 TypeScript 间保持精确集合相等；原生事件不得
    在 JavaScript adapter 静默丢失；发布包必须以外部 NodeNext 消费方验证。长期决策见
    [ADR 0029](decisions/0029-node-contract-and-federation-control-ownership.md)。
22. Federation development 历史路由只能由活跃浏览器 build lease 或固定两代 grace window 保留；
    每连接最多 8 个 canonical buildId。HTTP unknown/pruned build 只读返回 typed 410，不能广播或申领
    lease；浏览器仅在 410 控制精确匹配本次 asset identity 且 generation 严格推进时刷新。reserved
    Federation missing route 不得落入 public/SPA。每个 remote 独占 broadcast sender，多 mount container
    name 必须唯一，lease ack 必须与浏览器已接受的 build/generation cursor 相等。Production 页面缓存不受
    该裁剪影响，是否启用开发恢复只由 registration mode 决定。
    长期生命周期见 [ADR 0032](decisions/0032-federation-development-snapshot-leases.md)。
23. React 单模块编译只有 `wake_compiler → wake_compiler_core → ecma phases` 一条向下依赖路径。
    core 不拥有文件系统、配置、任务、缓存或产品对象；Bundler、`LibraryGraph` 与 `wake_js_runtime`
    可以消费 core，但 compiler 不得反向依赖它们。公开 CommonJS 对需要 graph/runtime 语义的构造必须
    fail closed，错误时不返回 code/map。当前不建立 Vue/AngularJS 占位前端或统一 framework 抽象，
    长期边界见 [ADR 0043](decisions/0043-react-module-compiler-boundary.md)。

上述依赖方向与来源规则的唯一机器事实来源是
[architecture-boundaries.json](architecture-boundaries.json)；叙述文档不维护第二份 allowlist。

## 4. 应用构建数据流

```text
CLI / Node API
  → wake_app 解析 cwd、configPath 与取消信号
  → wake_config 发现并规范化项目
  → wake_app 为完整候选创建 BuildGeneration（shared observation epoch）
  → application BuildSession（typed immutable BuildOptions；retained 或 one-shot）
  → resolve + load + parse + transform
  → module graph retained-binding names + observed public names + source-ordered export-star plans
  → OptimizeInput with stable linker export facts
  → optimizer-owned semantic resolution + owned optimization state
  → explicit passes to fixed point + retained dependency convergence
  → recompute chunk ownership from retained edges
  → shared optimized program → body emission + independent source-map facts task
  → 同 generation 的 Federation container/shared one-shot views + 单次冻结的 type identity
  → JS/CSS/assets/HTML/manifest/bootstrap/types/hidden maps 完整候选
  → 内存返回，或完整物化到同盘 staging 后一次 failure-atomic 提交到 Wake-owned outdir
```

产品层把规范化 `FederationConfig` 投影为 typed `FederationBuildPlan`，再随同一份不可变
`BuildOptions` 创建 `BuildSession`；Federation 不增加第三条 bundler 路径。Host/remote 产出的
`wake-federation.json` 是控制面，浏览器 container ABI 是数据面；二者
分别固定为 `wake.federation.manifest.v1` 与 `wake.federation.v1`。Manifest 只携带稳定 owned DTO，
per-container 数字 module ID 不越过 `(container, buildId, expose, generation)` 边界。构建方基于
canonical identity material 选择 build hash；contract 自身不读取文件、请求 URL 或决定 hash 算法。

页面内唯一 Federation Broker 持有 ContainerRegistry、ShareRegistry、TypeRegistry 和
DevCoordinator。共享求解固定采用已冻结 singleton/coherence group、宿主兼容 provider、已加载兼容
remote、当前 remote lazy fallback 的优先级；不在 import 热路径请求中央依赖服务。生产 lock 固定
manifest/build/type/asset closure，失败不会静默执行旧缓存。React host-rendered 边界使用宿主 scope 且
无 ShadowRoot；不同 React major 通过 isolated、非 default scope 与 open ShadowRoot 共存。

Development DevCoordinator 通过 `wake.federation.dev-lease.v1` 为每个 remote socket 原子替换完整
active build 集合。服务端只完整持有 current snapshot，retired build 只保留 build-scoped routes；
refcount 为零且超过两代 grace 即回收。公开 HTTP build lookup 无副作用，裁剪后的 build 用 CORS-exposed
typed 410 让请求该资产的页面恢复，不影响其他 socket。lease replacement 在 cursor 变化但 build set
不变时仍重新确认；ack cursor gap 会终止该 socket 并刷新当前页。每个 remote 的 sender 与 sibling
隔离，`/@wake/federation/` missing 请求直接 404。Production asset context 固定关闭这些开发恢复语义。

`BuildSession::new` 显式创建 retained 会话：开发服务器持有其 generation、load cache 与 committed
output，文件变化经 watcher 合并后执行失效和重建，再通过 WebSocket 发送更新。
`BuildSession::new_one_shot` 则只允许消费式 `build_once`，用于 CLI、Node、Docs 与 Federation 的单次生产
构建；它不启用 retained load cache，也不把完成产物 clone 到 committed state。产品 crate 不直接配置
底层 engine，所有语义选项必须在 session 创建前完整进入 typed options。长期所有权和删除计划见
[ADR 0027](decisions/0027-build-session-ownership-and-lifetime.md)。

跨多个编译视图的产品候选由 `BuildGeneration` 拥有。普通 build/Docs 的 application 与 Federation
container/shared provider 都从同一 owner 创建 one-shot view；长生命周期 build context 则把 retained
application session 与 owner 一起保存，每个 watcher batch 先推进 observation epoch，再让 container/shared
从该 epoch 创建 one-shot view。开发服务器不拆出这些生产子构建，而是在一个 retained session 的合成
container graph 中同时编译 application、exposes 与 shared fallback。类型声明在候选内读取和生成一次，最终
`buildId` 只重绑定已冻结声明图；全部产物完成后才跨一次 staging/publication 边界。

Generation filesystem 仅对每个精确路径拼写的 `read_to_string`、`read`、`exists`、`is_file`、`is_dir`
和 `read_dir` 首次完成结果分别复用。未观察路径可以看到稍后的底层状态，不同方法族也可能观察不同状态；
它不是事务快照、路径 canonicalization 或 symlink identity 保证。需要参与跨产物 identity 的输入必须在候选
准备阶段主动冻结。所有权、非保证和验证见
[ADR 0028](decisions/0028-build-generation-ownership-and-observation-cache.md)。
压缩器版本、defines/drop flags、链接活跃性、可信编辑和包装器保留名参与优化身份；图、优化器指纹与持久任务
都用稳定的声明保留名、公开观察名、star specifier/ordinal、精确转发名和 opaque 排除名作为身份，解析得到的 `SymbolId` 只在当前 optimizer 调用内有效。optimizer key 不含最终
chunk 编号，retained edges 收敛后才形成 final-layout body key；map 开关不进入二者。当前
`wake-closure-minifier-v15` 与缓存 schema 13 使旧压缩缓存自然失效。持久层除 JavaScript 与 mapping
facts 外，只保存 codegen 生成的目标字面量字节区间、稳定 request specifier/role，以及与 body 配对的
collision-free runtime 参数名和真实的 `metaUrl` runtime capability；default/star interop 已由 typed
finalizer 内联，不存在 compact helper capability；optimizer
边也只保存稳定 retained specifier。恢复时必须映射到当代 module ID 并校验 body 中的精确十进制字面量；
不持久化 arena 指针、`NodeId`、`SymbolId`、进程内 module ID 或可变 IR。长期所有权、
表示边界和验证门禁见
[ADR 0023](decisions/0023-closure-style-minifier-pipeline.md)。该设计借鉴 Closure Compiler 的 pass
调度思想，不是 Closure `ADVANCED` 的类型/whole-world 兼容层，也不引入其源码或依赖。
精确公开名/声明活跃性拆分、空 barrel 的副作用请求保留和 binding-free 单遍规范化见
[ADR 0024](decisions/0024-linker-proven-barrel-compaction.md)。
显式导出优先、star 歧义/同绑定消解、循环静态名称与 opaque fallback 见
[ADR 0042](decisions/0042-linker-owned-export-star-resolution.md)。
生成 JavaScript 只是输出产物，不拥有 SCC/顺序/concat、wrapper 绑定或 Federation expose 语义；wrapper
逐模块消费 typed codegen 的 runtime 名称和 capability，helper 注入不得扫描 body；非 canonical concat
成员保守保留独立 factory。结构化
emit 所有权、cache DTO 和文本路径删除见
[ADR 0033](decisions/0033-structured-module-emit-provenance.md)。
缓存文件只保存真实源码内容身份的派生值；schema 13 envelope、严格预算、原子 body/mapping 组、
有界文件锁和 authored-key 并发合并见
[ADR 0034](decisions/0034-transactional-persistent-cache-boundary.md)。

### 4.1 压缩器所有权状态

`TypedProgram` 是优化阶段唯一的可变语法事实源。它完整拥有可发射节点、child lists、名字 spelling、
当前程序内的符号记录和 `Source`/`Derived`/`Synthetic` origin，并用稳定 `NodeId`、`ListId`、`NameId` 和
`SymbolId` 连接它们。Parser AST 与初始 semantic model 只服务一次 lowering；结构 pass 通过 typed
mutation 修改当前树，`validate()` 检查 parent/child、列表、节点类别和名字不变量。`Span` 只保留来源与
可信编辑定位，不再承担活跃性或 rewrite table key。

`TypedAnalysis::rebuild` 只从当前 live tree 重建 lexical scope、读写引用、capture、CFG、确定初始化、
effect 和 escape facts。同一 `TypedProgram::revision` 上的 binding-sensitive pass 共享分析；任一结构 pass
发生真实变化后，调度器在下一次绑定敏感工作前重建。固定点收敛时的当前分析直接供最终改名和动态作用域
判断复用，因此删除或替换的引用不会通过旧 snapshot 继续存活，也不会为未变化的轮次重复扫描。Direct `eval` 与 `with` 冻结其可见环境，
但不要求无关作用域或整模块退回另一条实现。

一次性可信编辑、runtime helper、装饰器和 typed module planning 完成后，`typed_pipeline.rs` 按常量折叠、
控制流简化、封闭函数内联、单次变量内联、DCE、声明/sequence 合并和 late peephole 的顺序运行全局固定
点。每个 pass 只报告真实结构变化；完整轮次无变化才收敛，100 轮仍变化返回诊断。最终名字直接写回
owned name occurrences。依赖体积收益的候选由 typed token 成本约束为不增长，未证明安全或更短的候选
保持原形。

模块转换也由 typed IR 拥有：优化前建立 `TypedModulePlan`，固定点后从 live tree seal 保留请求，最终
链接/chunk facts 到齐后调用 `finalize_typed_modules`，再交给 typed emitter。Preserve ESM、Preserve CJS
和 bundled CJS 共用这一 plan→seal→finalize 生命周期；bundler/codegen 不维护 namespace 或 live-read
side-plan。

优化产物的 mapped/unmapped codegen 都由 `wake_ecma_codegen::typed` 执行同一个 token walk，mapping
只是可选 sink。Source/Derived anchor 提供源位置，改名 occurrence 把原名写入 V3 `names`，无可靠来源的
合成标点和 wrapper 保持 unmapped。每类折叠、内联、参数替换和模块 wrapper 的细粒度映射仍必须由独立
回归证明，不能仅依据 origin 元数据宣称完整覆盖。

Scope-concat 是否能用 bare block 的保守扫描由 `wake_bundler::concat` 私有拥有，结果只服务 bundler
wrapper 策略；codegen 不分析 concat eligibility。

原子迁移后，生产路径只保留 `OptimizeInput` → owned typed IR → typed emitter；不保留兼容 emitter、
第二套名字优化状态、4096 字节退让或 Source Map 禁用分支。完成条件及尚未执行的验证必须按
[ADR 0023](decisions/0023-closure-style-minifier-pipeline.md)
报告，不能以文档描述代替测试证据。

Source-mapped coverage 对函数指标采用更严格的所有权边界：只有 V8 函数起点精确命中 codegen 在真实
源函数起点写入的 map anchor 才进入用户函数统计。Preserve-CJS export getter 等合成函数不拥有该锚点，
不会因为 floor mapping 继承前一源位置而被计数。

## 4.2 测试数据流

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
  → PageIdentity(id / source file / generated file / decoded+encoded RoutePath)
  → markdown AST owns frontmatter / HeadingPlan / MdxjsEsm spans
  → Wake ECMA parser dependencies select only real module specifier tokens
  → Demo 扫描与 Props/JSDoc 提取
  → provenance-aware writer 生成相对、诚实的页面 Source Map
  → 生成 .wake/docs 虚拟 React 项目与 typed route registry
  → 与 Wake Docs runtime、Preview、主题一起打包
  → site routes 或 components hash workbench
  → docs-dist 静态产物
```

Site 模式生成页面、导航、搜索索引和静态子路由外壳。Components 模式只要求 `.demo.tsx`，以 hash 保存 Demo、非默认 Args、显式 unset 字段和视口状态，因此不要求托管端重写深链接。

页面身份只从 `PageIdentity` 派生。文件系统 shell 使用 decoded route，registry、history 和 pathname
lookup 使用逐段 RFC 3986 canonical encoded route；共享 runtime codec 同时供 `scripts/check-docs.mjs`
消费，禁止各层再实现一套 slug 规则。非 UTF-8 页面段或 normal component 内反斜杠直接诊断，不能
lossy 转成分隔符。Heading metadata 与 renderer 只消费
同一个 `HeadingPlan`。ESM 只由 markdown `MdxjsEsm` node 和 Wake ECMA dependency/token span 重写，
页面 Source Map 只映射能证明来源的 node/token，synthetic wrapper 保持 unmapped。长期约束见
[ADR 0031](decisions/0031-docs-page-identity-and-source-provenance.md)。

聚合 Docs 由 `wake_app` 发现和编排：主站与每个 Components 工作台仍分别调用单项目
`wake_docs` 生成器并独立打包。生产输出先进入隔离 staging 树再逐文件事务提交；开发时
`wake_dev_server` 用一个最长前缀挂载注册表、一个监听线程和一个 Live Reload endpoint 持有多个
独立 `BuildSession`。lazy 工作台首次请求才创建会话，事件和整页浏览器刷新按挂载身份隔离。

## 6. 对外边界

`wake_app` 是 CLI 与 Node 绑定的稳定内部边界：

- 将底层诊断映射为 `WakeError` 和结构化 `DiagnosticInfo`；
- 为一次构建、增量上下文、应用服务器和文档服务器提供统一选项与 `BuildGeneration` 边界；
- 管理取消、关闭、物理输出路径验证、产品所有权标记与 staging/rollback 发布；目录产品遵循
  ADR 0026，精确文件集合还必须消费 reader provenance、拒绝输入物理 alias，并按 ADR 0036 一次提交；
- 隔离 Node-API JSON 传输和 CLI 终端表现。

测试路径中 `wake_app` 只依赖并重导出 `wake_test_contract` 的稳定 DTO，启动独立
`wake_test_host` 并处理 session lifecycle；它不链接 `wake_test`、`wake_js_runtime`、
`wake_ecma_vm` 或 V8。

JavaScript 类型以 `npm/wake/index.d.ts` 和 `experimental.d.ts` 为公开事实来源。

## 7. 构建 generation 与会话所有权

每个单次编译只有 `BuildSession` 一个状态机和事实来源。retained 与 one-shot 是同一 typed options/产物契约下
显式选择的生命周期，不是两套 Scan/Link/Emit 实现；全字段等价测试约束两者的输出、诊断、chunk、map、
CSS 与资源。底层 engine、setter 和 crate 内 unit test 可以留在 `wake_bundler::src`，但产品源码、外部
integration test 与 benchmark 不得导入该实现类型或使用迁移构造器。`Bundler` 若继续存在，只能是委托给
typed `BuildSession` 的兼容 façade。边界、缓存身份和移除条件以
[ADR 0027](decisions/0027-build-session-ownership-and-lifetime.md) 为准，并由架构静态门禁防止旧入口回流。

一个需要多个编译视图的完整生产候选则只有 `BuildGeneration` 一个 owner。它创建 application session 和
Federation transient children，并使已观察的文件事实只在该 generation 内共享；`wake_app` federation
生产代码不得绕开 owner 自建 `OsFileSystem` 或 `BuildSession`。这项约束不禁止 `#[cfg(test)]` 后的隔离
fixture，也不把 lazy observation cache 描述成全局快照。跨编译所有权以
[ADR 0028](decisions/0028-build-generation-ownership-and-observation-cache.md) 为准。

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
