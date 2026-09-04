# Wake 设计文档

本文保留源码现有 `DESIGN §…` 引用所需的编号，但描述的是仓库当前已实现设计。未来意向统一写入 [ROADMAP.md](ROADMAP.md)。

# 1. 目标与原则

Wake 将 JavaScript/TypeScript 编译、生产打包、开发服务器和 React 组件文档放在同一套 Rust 工具链中。设计优先级依次是正确性、可诊断性、跨平台一致性、增量工作规避和吞吐。

非目标包括执行任意 JavaScript 配置、公开稳定 Rust 插件 ABI，以及在 0.x 冻结实验编译器数据结构。

# 2. 产品边界

CLI 与 Node API 是两种调用外壳；构建行为由 `wake_app` 统一。Wake Docs 是同一编译管线上的产品层，不是独立的第二套打包器。

# 3. 总体架构

## 3.1 Workspace 与入口

workspace crate 按基础、编译、解析/资源、执行/编排和产品边缘分层。完整列表和依赖方向见 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 3.2 构建阶段

应用构建采用 Scan → Link/Chunk → Optimize/Codegen → Emit：Scan 解析依赖并建立记录，Link 以公开
export 名称计算稳定的模块/绑定活跃性；bundler 把 `(ModuleId, export names)` 交给 Optimize。Optimize
在单一入口建立 parser semantic model，用它解析本次程序的 `SymbolId` 并执行 typed lowering，随后报告
仍保留的模块依赖；bundler 再据此保守收敛可达集合并组装 runtime、chunk、代码和资源。`SymbolId` 只在
optimizer 内部属于当前 AST 生命周期，不是跨 crate 或持久缓存格式。

## 3.3 核心抽象

ECMAScript 编译核心不读取文件，只消费调用方提供的源码并返回 owned 诊断/依赖/发射事实；parser
创建的 `ModuleAst` 持有 arena 生命周期与精确 source，并记录创建其 `Atom` 的进程内 interner
identity。文件系统、路径解析、任务和缓存由 bundler/runtime 等调用方拥有，它们跨阶段只传稳定记录和
受控句柄。

## 3.4 BuildSession 所有权与生命周期

`BuildSession` 是所有产品构建进入 `wake_bundler` 的唯一公共 owner。调用方先把平台、解析、JSX、
CSS/资源、优化、缓存、entry/chunk 命名和 bundler-facing Federation plan 完整归一化为 owned
`BuildOptions`，再一次性创建 session；构建开始后不存在产品可用的 setter 路径。新增语义选项必须同时
进入 typed options、最近的复用身份和 retained/one-shot 等价矩阵，不能只给某个产品入口补配置。

`BuildSession::new` 创建 retained 生命周期，拥有 loader snapshots、generation 和 committed
entry/output；强制构建与 current-generation 构建通过同一 commit transition 更新状态。
`BuildSession::new_one_shot` 创建 transient 生命周期，只能由消费式 `build_once(self, request)` 执行一次，
不启用 retained load cache，也不为未来 generation 保存和 clone 完整输出。两种生命周期共享相同的
Scan/Link/Optimize/Emit 任务定义、诊断和 `BuildOutput` 契约；one-shot 只省略没有后续消费者的进程内状态。

底层 engine 与 mutable setter 只属于 `wake_bundler` crate 内部实现和 unit-level 证明。产品 crate、外部
integration tests 与 benchmark 只能使用 session API；兼容 façade 必须委托 typed session，不能重新暴露
configurator closure。长期边界见
[ADR 0027](decisions/0027-build-session-ownership-and-lifetime.md)。

## 3.5 BuildGeneration 与候选一致性

`BuildSession` 只拥有一次 typed compilation；application、Federation container、optional shared
provider、types 和 manifest 组成的产品候选由更外层的 `BuildGeneration` 唯一拥有。一次性 build/Docs
从 owner 创建 application one-shot view，再由同一 owner 创建 container/shared one-shot views。长生命周期
build context 同时保存 owner 与 retained application session；接受 watcher batch 后先推进 generation，再
失效 retained graph 并创建该代的 transient Federation views。产品 Federation 子构建不能另建 filesystem
或 session。

Owner 内的 filesystem proxy 按 `FileSystem` method family 和精确 `OsString` 路径拼写缓存首次完成结果，
包括可重放的 I/O failure。七个查询族互不推导；advance 会整体替换 observation epoch。因此它提供的是
lazy、query-scoped repeatability，不是时间点文件系统快照：未查询路径可能看到后续变化，先前
`exists(false)` 也不约束另一个 family 的首次 `read`。跨产物 identity 所需的 package/config/synthetic
source/type inputs 必须在候选准备阶段主动观察或冻结。

完整 application/Federation/声明/manifest/bootstrap/HTML/asset/hidden-map 候选构造成功后只发布一次。
声明图每个候选只通过 generation filesystem 读取、解析一次，产出与 `buildId` 无关的 canonical identity
和 `FrozenDeclarationGraph`；最终 `buildId` 确定后只通过纯 binder 渲染 types/ambient files，不重新读取
源文件。开发服务器采用另一种但同样唯一的 owner 形态：一个 retained session
编译 synthetic container、application、exposes 与 shared fallback 的 combined graph，成功后才安装新的
runtime snapshot。BuildGeneration 的长期边界和明确非保证见
[ADR 0028](decisions/0028-build-generation-ownership-and-observation-cache.md)，parser-owned frozen declaration
graph 及其纯绑定契约见 [ADR 0040](decisions/0040-parser-owned-frozen-declaration-graph.md)。

Node addon、CommonJS/ESM wrapper 与 npm CLI 只是产品适配器。Federation 初始化和 lock 生成调用
`wake_app` 的同一服务；`OutputFileKind` 与 Federation `ErrorCode` 使用闭合集合和跨语言相等门禁，
dev event adapter 对未知事件产生诊断。ESM Federation 子路径使用与 `.mjs` 配对的唯一 `.d.mts`
声明，tarball 外部 NodeNext 消费测试负责证明 export map 和声明解析。见
[ADR 0029](decisions/0029-node-contract-and-federation-control-ownership.md)。

# 4. ECMAScript 编译器

## 4.1 基础设施

热路径使用字节 Span，行列号在诊断或 Source Map 序列化时按需换算。`Atom` 是进程内句柄，禁止进入持久化缓存。

## 4.2 AST

AST 使用 arena 分配和紧凑枚举；`ModuleAst` 把源码、arena 与 Program 生命周期封装在不可克隆的自引用持有者中。手写 `unsafe` 由 Miri 常驻验证。

## 4.3 Lexer 与 JSX

Lexer 按字节扫描，token 只保存类型、Span 和 ASI 换行状态。parser 驱动正则/除号与 JSX 文本上下文；标识符只在 parser 需要时驻留。

## 4.4 Parser 与依赖

语句使用递归下降，表达式使用 Pratt 优先级解析。解析时同步提取静态/动态依赖和顶层 await 标志，并根据 SourceType 选择 JS、TS、JSX、TSX 或 CommonJS 语义。需要 cover grammar、词法作用域或临时变量所有权的浏览器 lowering 由 parser 编排，但规则与 AST 构造归 `wake_ecma_transform`；因此目标特性属于前端任务身份。

## 4.5 Transform

TypeScript 擦除、JSX automatic runtime 和浏览器目标转换在明确上下文中运行。`wake_ecma_transform` 不依赖 parser，只提供稳定的 lowering 规则与 AST helper；React runtime plan 统一决定 `jsx-runtime`/`jsx-dev-runtime` 路径、固定 helper import 集合和 runtime call ABI。parser 在仍掌握 cover grammar、作用域、collision-free 本地名、import 插入位置和依赖记录时调用这些 helper。转换复用原 Span，避免丢失诊断与 Source Map 来源。

### 4.5.1 Compiler core 与公开 façade

`wake_compiler_core::CompilerBackend` 在共享 `Interner` 上依次执行 `parse_module`、
`optimize_module` 和 `emit_module`。它只拥有
纯 CPU 编译能力；finalize 只在 emit 调用内部消费最终模块事实，不形成可观察的第四阶段或缓存身份。
Bundler 复用 backend，但继续拥有 retained/one-shot task、persistent/body cache、sealed-trivial fast
path、依赖图、Chunk、CSS/runtime 组装与最终 map 合并。

`wake_compiler::transpile_module` 是借用源码、返回 owned 结果的 React-only 单模块入口。首版支持
JavaScript/TypeScript Module、可选 React-compatible automatic JSX、Preserve ESM/CommonJS 与 detached
V3 Source Map，并固定不压缩。它不暴露 AST、Atom、Interner 或 typed IR。依赖 graph/runtime 才能可靠
转换的 CommonJS 构造必须 fail closed，错误结果不同时携带 code/map。Vue 和 AngularJS 未来通过独立
需求、ADR 与前端接入，不在当前 API 中预留 trait、枚举或字段。见
[ADR 0043](decisions/0043-react-module-compiler-boundary.md)。

## 4.6 Closure 风格优化管线

`wake_ecma_minify` 把 parser AST 与初始 semantic model 一次性降低为完整拥有所有权的 `TypedProgram`。
它以稳定 `NodeId`、`ListId`、`NameId` 和当前程序内 `SymbolId` 表达结构与绑定，每个节点携带 `Source`、
`Derived` 或 `Synthetic` origin。节点、child list、名字 spelling 和符号记录都由 typed arena 持有；
`replace_node`、`splice_list`、`tombstone_subtree` 等结构操作修改当前树，`validate()` 检查所有 parent/child、
节点类别和名字不变量。Parser AST 只提供一次 lowering 的输入；后续结构、名字和活跃性都以 owned
typed IR 为事实源，codegen 不按 Span 或外部计划拼装重写。

`OptimizeInput` 包含 parser 已验证的 defines、drop flags、保留名、当前程序上的
`LinkerExportLiveness`、源码有序的精确/opaque export-star plan、内部模块模式，以及 CSS-in-JS 等可信结构化编辑。`typed_edits.rs` 先把编辑解析为
typed target 和 owned expression；目标不唯一、类型错误、重叠或 owner 错配都返回诊断。`optimize` 返回
持有最终 `TypedProgram`、typed module plan、最终名字、保留依赖、稳定指纹和 pass 统计的
`OptimizedProgram`。内部 ESM 的 plain star 由 graph 按最终 binding identity 静态消歧；显式名不进入
star plan，同一 binding 的多路径只分配一次，循环使用静态名称 getter。只有 CommonJS、external 或
缺少完整分析的 surface 使用带显式排除名和 own-export guard 的运行时枚举。

一次性验证之后，固定点按“常量传播/折叠 → 条件与退出点简化 → 已封闭函数/调用内联 → 单次使用变量
内联 → 死控制流/赋值/声明清除 → 声明/sequence 合并 → late peephole”调度，统一 change tracker
决定是否继续；100 轮仍不收敛返回包含模块和最后变更 pass 的诊断。Binding-sensitive pass 通过
`TypedAnalysis::rebuild` 从当前 live tree 完整重建 lexical scope、引用读写、capture、CFG、确定初始化、
effect 和 escape facts；相同 program revision 上复用当前分析，结构变化后才在下一次 binding-sensitive
pass 前重建。固定点收敛时的当前分析继续供动态作用域判断、变量槽复用和最终符号改名使用。它不读取
parser AST、旧 revision snapshot 或活跃 Span 集合。`LatePeephole` 只有在当前树分析证明求值安全时才省略表达式。

所有“只有变短才值得做”的局部候选使用 typed token 成本，计入重复引用、名字、括号和 separator；
闭包内联、常量/控制流替换、语句合并、安全属性和标识符改名只有在不增长时才提交。纯删除、合法性
规范化与调用方显式请求的可信编辑不依赖收益判定。源码长度退让和旧运行时 fallback 不属于该架构。

属性改名只处理类私有名和可证明封闭的局部对象字面量数据属性。动态访问、逃逸、反射、枚举、
spread/rest、Proxy、delete、方法/accessor 或未知别名会使整个形状退出；公共类成员、DOM/React/Node
宿主属性、`__proto__` 和协议名保留。该模型不采用 Closure `ADVANCED` 的 whole-world、externs 或
类型假设，只对普通 ESM/CommonJS 做 Wake 自有的语义保持优化。

标识符改名的模块边界由 linker 证明决定：没有 `LinkerExportLiveness` 的 preserved ESM 保留公开导出；
有该契约的 linked output 可以删除或缩名未活跃本地 binding，同时保持原 export key。公开 export 名只在
optimizer 内通过同一 parser semantic model 解析为当前模块符号；不会通过 Span 或裸局部名称猜测遮蔽关系。

模块转换遵循 plan→optimize→seal→finalize。`plan_typed_modules` 在固定点前把 import/export、静态
`require` 和 dynamic import 转为 typed request 与 runtime binding；`seal_typed_module_plan` 只保留最终 live
tree 上的请求。最终链接/chunk 事实确定后，消费式 `finalize_owned_typed_modules` 写入真实目标和 interop，
完成全 arena 校验并构造没有 pending request sentinel 的 `FinalizedTypedProgram` 类型状态。Preserve ESM、
Preserve CJS 与 bundled CJS 共用这一结构化生命周期；
外层不能注入另一套 namespace/live-read side-plan。Direct `eval`/`with` 的冻结范围由当前树分析决定。

## 4.7 Codegen 与 Source Map

`wake_ecma_codegen::typed` 直接遍历 finalizer 验证后构造的 `FinalizedTypedProgram`，根据 typed node
类别与优先级写 token 和必要括号；它没有 parser AST 或 legacy optimization-table 输入，也不运行 DCE、
内联、模块规划或改名。这个类型状态避免 emitter 紧接 finalizer 再做一次全 arena 校验。
最短数字、字符串、空白和 token separator 由 emitter 负责。

Mapped 与 unmapped 入口执行同一个 token walk，mapping 只是可选 sink，因此 `minify` 与 Source Map
可以同时启用，且 map 收集不得改变 JavaScript body。`Source` 和带 anchor 的 `Derived` origin 映射回源；
改名 name occurrence 把 original spelling 写入 V3 `names`；无可靠来源的合成标点和 wrapper 写 unmapped
segment。折叠、内联、实参替换和模块 wrapper 的每类细粒度来源必须由独立测试证明，未验证的映射规则
不作为兼容承诺。

# 5. 解析与模块图

## 5.1 Resolver

`ResolutionEnvironment` 是唯一解析所有者，统一持有基础/PnP/zip 文件系统、按 issuer 发现的最近 PnP
根、经典 Node 后端、Resolver 与成功/失败缓存。`.pnp.cjs` 存在即启用 PnP；非内联 loader 读取匹配 data，损坏清单返回
`PnpManifest` 诊断且禁止命中 `node_modules`。有效 npm 裸包在受管理 issuer 中先于 Wake alias，并由
Yarn locator、alias dependency、peer/virtual/unplugged 与 fallback 的成功或拒绝最终裁决；非 npm 包名的
Wake 路径 alias 保留。ignore pattern 或 unmanaged issuer 执行 Yarn 指定的经典 Node 解析，无 PnP 根
时使用普通 Wake alias/node_modules 行为。npm 的 nested/scoped/workspace/subpath/exports 均由已安装
文件树解析；`package-lock.json` 不被求解，但其变化会失效解析和模块拓扑。zip 支持 STORE 与 DEFLATE；
逻辑（含 virtual）路径始终是模块身份，物理 archive 路径只用于 I/O、watch 与精确缓存 key。物理 zip
变化只失效对应 `ZipArchive`，并发中的旧 archive open 由 revision 丢弃；PnP 清单、data 或 lock 变化
则清除全部清单、解析和 zip 缓存。Components 不拥有 resolver fallback，旧归档元数据由 Yarn
`packageExtensions` 声明。Wake 内部兼容目标通过 strict package-root resolution 跳过用户源码 alias，
但不跳过 issuer 的 PnP 可见性；这类目标不能用来提供未声明依赖。

## 5.2 Scan

增量路径按依赖层并行加载、解析和分析。共享模块通过内容身份与 single-flight 避免重复工作；诊断不因并行而丢失来源路径。

## 5.3 Tree Shaking

Link 阶段从入口计算模块可达性，并分别保存稳定的声明绑定保留名、精确公开观察名和 plain
`export *` 转发事实。模块 parse 后，bundler 把这些事实与 `ModuleId` 一起传给 `OptimizeInput`；优化器使用
lowering 共用的 parser semantic model 将声明保留名解析为顶层 `SymbolId`，再按公开观察名生成 getter、
删除未根化导出并报告仍保留的依赖说明符。死 re-export 只移除 namespace/getter，源模块请求仍按原顺序执行。
Bundler 将保留依赖映射为排序去重的 `ModuleId`
用于可达性收敛。缓存摘要保存恢复分析所需的稳定字符串和标志，不持久化 `Atom`/`SymbolId` 或 AST 指针。

# 6. 打包与产物

模块默认进入函数包装 runtime；满足条件的 ESM 可进入 concat/scope-hoist 块。Bare-block eligibility 的
保守 AST 扫描由 `wake_bundler::concat` 私有拥有：存在 `var` 或任何 `this` 即退回 IIFE；codegen 不导出
这项分析。循环、CJS `module.exports` 替换、导出冲突和不安全块语义会保守降级为独立 factory。

## 6.1 CJS/ESM 互操作

runtime 为 ESM namespace、CommonJS exports、循环模块和异步模块维持单次执行与缓存身份。
Library preserve-modules 的每个 CommonJS 文件是独立执行单元，由 codegen 按实际引用在文件内
去重注入 default/namespace interop helper，不能依赖入口先执行或应用 bundler runtime。

为保留 ESM live binding，命名 import 的每次读取直接访问其 CommonJS namespace 属性，而不是从
初始化时 `const` snapshot 读取。用作 call/tag 时，前置 sequence value 去掉 namespace receiver，使 `this`
等同 ESM import binding 的值调用。Namespace import 则由 interop helper 物化一次并复用局部绑定，不在每次读取
重建 wrapper。每个 import statement 的 namespace 名由 optimizer 内部计划：首选基于 declaration Span
的稳定名，与用户绑定或自由引用冲突时按确定顺序加数字后缀。Library/runtime 只选择 preserve-CJS 模式，
codegen 从 `OptimizedProgram` 读取 namespace 与实时访问表达式，避免任何 bundler/codegen side-plan 分叉。

## 6.1.1 顶层 await

包含顶层 await 的模块及其同步导入方形成 async 子图。runtime 缓存执行 Promise，调用方等待同一生命周期，避免重复执行或捕获未完成导出。

## 6.2 Emit

Emit 生成 JS chunk、CSS、静态资源、HTML、manifest 和可选 Source Map。启用映射不会关闭压缩或
代码分割，也不会改变不含 `sourceMappingURL` trailer 的 JS payload。文件路径在跨平台输出前统一规范化。
Library emit 先生成确定性 staging 清单，再在稳定输出目录内跳过相同文件、逐文件原子替换变化、
删除 stale 文件并在失败时回滚；不得整体 rename 已存在的输出目录。

精确文件产物由 `wake_app::output` 单独拥有集合事务。Bundle 的 JavaScript 与启用时的 Source Map、
token 生成结果和 Docgen 结果在发布前携带 reader/resolver 给出的成功内容读取 provenance；目标先按
词法、canonical、符号/reparse 与文件身份和全部输入比较，再以同目录 unique temp 完整写入、flush、
sync、备份、安装和反向回滚。Bundle 未启用 Source Map 时不推断相邻旧 map 的所有权，也不删除它。
长期不变量见 [ADR 0036](decisions/0036-input-disjoint-exact-output-transactions.md)。

## 6.3 Chunk 与动态导入

生产构建为动态 import 创建 async chunk，并通过 `public_path` 生成加载 URL。抽取 CSS 归属到具体 chunk：入口样式由 HTML 激活，异步样式由 runtime 在对应 JavaScript 执行前加载；书面 manifest 保留同一 chunk/style 关系。相对 `./` 用于 file URL/Electron；文档站独立使用 `base_path`。

## 6.4 Wake Federation v1

`wake_federation_contract` 是 Federation 公共数据的唯一 Rust owner，固定
`wake.federation.manifest.v1` schema 与 `wake.federation.v1` browser ABI。配置、bundler、dev server、
Node 和浏览器适配只转换或消费这些 DTO；contract 不执行文件、网络、resolver、hashing、semver 或
JavaScript。Manifest 以有序 map 描述 container/build、remote entry、exposes、同步/异步 JS/CSS/Source
Map、shared offers/requirements、声明文件和开发更新端点。所有资源携带 content hash、SHA-384 SRI、
MIME 与 size，类型产物必须绑定同一 `buildId`。

生产 Federation 不是 application 之外的一组独立文件系统构建。`wake_app` 先冻结一个 generation 的
shared descriptors、synthetic entries 与 declaration graph，再由同一 `BuildGeneration` 编译 application、
container 和 optional shared provider。container/shared 可以使用不同 immutable `BuildOptions` profile，
但不能脱离同一 observation epoch。所有编译输出与单次冻结的类型 identity 共同决定一个 `buildId`；
manifest、bootstrap、types 和 hidden maps 完成后，整个候选才进入一次 failure-atomic publication。

跨构建身份是 `(container, buildId, expose, generation)`；现有数字 module ID 和 runtime namespace token
仍只在单一 container 内有效。Canonical build material 规范 set-like 顺序并排除部署 URL、dev metadata
和 `buildId` 自身，bundler 再负责实际 hash。Manifest 是控制面，异步 container `init/get` 是数据面；只
允许显式 remote dynamic import，静态 import、`require` 和跨 container 静态/TLA 循环 fail closed。

每个 Window 的 broker 以 single-flight 加载 container，按冻结 singleton/coherence group、宿主 provider、
已加载兼容 remote、当前 remote lazy fallback 的顺序求解显式 shared 白名单。相同 package version 只有
resolver 的 package/peer context 与 build variant 一致时才是同一 provider。React coherence group 同时
覆盖 `react`、JSX runtimes 与 `react-dom` entry；host-rendered 复用宿主 scope 且不创建 ShadowRoot，
isolated 使用非 default scope、自己的 root 与 open ShadowRoot，并只跨边界传递结构化 props/events/DOM
slots。长期所有权、失败语义和删除计划见
[ADR 0025](decisions/0025-wake-native-federation-contract.md)。

# 7. Dev Server 与 Live Reload

开发服务器采用增量打包模式：监听变化、失效会话并重建受影响模块；普通应用完整候选成功发布后，
通过 `/__wake_live_reload` 发送 typed `reload` frame，浏览器调用 `location.reload()`。服务端的增量复用
不构成浏览器模块热替换：`import.meta.hot` 恒为 `false`，没有 accept/dispose、React Fast Refresh 或
状态保留契约。

Federation dev 把 synthetic container、application、exposes 与 shared fallback 放进一个 retained
session 的 combined graph，不按生产形态创建 container/provider 子 session；只有完整 build 成功才安装
runtime snapshot 并发送独立的 types-only / isolated-remount / full-reload 更新。每个 browser socket
同时用 versioned lease 申领至多 8 个实际活跃 build；服务端仅为 lease refcount 或两代 manifest→entry
竞态窗口保留旧 build routes，断开后回收。cursor 变化即使 build set 不变也重新 lease；ack 必须精确
匹配页面已接受 cursor，漏代重连只刷新本页。每个 remote 独占 sender，多 mount container name 不得重复。
已裁剪 build 的 HEAD/GET 返回无副作用 typed 410，只有请求该资产、严格匹配控制身份且 generation 严格
推进的页面会刷新；reserved missing 直接 404，不进入 public/SPA。Production page cache 不参与这套裁剪，
development metadata 不能越过 registration mode 开启恢复行为。通知在 Windows 上会
合并延迟事件；配置、根或入口的结构变化建议重启。长期能力与生命周期边界见
[ADR 0030](decisions/0030-live-reload-capability-boundary.md) 与
[ADR 0032](decisions/0032-federation-development-snapshot-leases.md)。

Federation editor 类型同步属于 dev server 控制面：启动时由唯一的 fail-closed synchronizer 校验全部
remote，之后仅轮询 `dev_follow` Manifest 的 build/type revision。revision 未变不下载声明；变化后仍由
同一 synchronizer 先校验全体，再原子切换稳定 index。刷新失败不修改 index 并产生可重试诊断；pinned
remote 不轮询，启动同步必须满足生产 lock。monitor 与 `DevServer` 共享生命周期，关闭完成后不得继续写入。

# 8. CSS 与静态资源

## 8.1 CSS

开发模式可注入样式，生产模式抽取 CSS；CSS Modules 生成局部类名。CSS-in-JS 位于独立 crate，保持编译核心依赖边界。Crab UI 包由 `package.json#name = @crab-dev/rc-*` 与受支持入口共同识别，存在 `css/index.css` 时由 loader 自动加入模块图；业务源码与 Components runtime 均不显式导入这些样式。该身份使 CSS 的新增、删除及跨进程热缓存保持冷构建等价。

旧组件公共入口的 `@linaria/core` runtime 兼容不修改 loader 源码。parser 产出的精确
`Import`/`ExportFrom`/`Require` 依赖在 resolution-time 指向内部 `@crab-dev/css` 包目标；原始
specifier/kind 继续由 ModuleRec、缓存、诊断和 codegen request 拥有。动态 import、应用、第三方、
组件内部文件和嵌套入口不映射。目标跳过 source alias，但 Yarn PnP 仍按组件 issuer 的依赖声明
成功或拒绝。长期边界与删除条件见
[ADR 0035](decisions/0035-parser-owned-crab-runtime-resolution.md)。

## 8.2 静态资源

小资源内联为 data URL，大资源使用内容哈希独立输出。`.raw` 作为文本导出，JSON 作为默认导出模块，CSS `url()` 与 HTML/manifest 使用统一公共路径。

# 9. 扩展边界

当前没有承诺稳定的公开 Rust/JavaScript 插件系统。组件扫描、Preview、主题 CSS 和声明式 TOML 是受支持的扩展点；实验编译器 API 不等于插件 ABI。

## 9.1 Wake Docs 身份与源码溯源

Docs 页面编译是 typed content pipeline，不是字符串预处理器。每个源文件先构造一次 `PageIdentity`，
同时冻结 root-relative source/generated 路径和 decoded/canonical-encoded `RoutePath`；registry、静态 shell、
browser runtime 与文档 checker 只能转换或消费该身份。URL codec 逐段编码 UTF-8，只保留 RFC 3986
unreserved，percent hex 固定大写。非 UTF-8 段或 normal component 内反斜杠不允许 lossy 降级。

markdown AST 拥有 `MdxjsEsm` node/span，Wake ECMA parser/lexer 在 node 内按 typed dependency kind
定位 module specifier。标题先由单个 `HeadingPlan` 分配 ID，再供 metadata 和 renderer 使用。页面 writer
将 synthetic wrapper 留空映射，只为能证明由 MDX node/token 派生的 generated 位置建立 Source Map 段，
并用 generated-only 段终止同一行的映射。详细不变量及移除的旧路径见
[ADR 0031](decisions/0031-docs-page-identity-and-source-provenance.md)。

# 10. 增量与并发

## 10.1 工作规避

Wake 使用内容身份、resolver/load cache、任务依赖、持久化摘要和优化/生成体缓存减少重复解析与
codegen。依赖形状和绑定活跃性不变时复用上一 generation 的 link/chunk 规划；旧的内存摘要在成功
generation 后回收。新进程始终读取并哈希真实 loader 输出，持久层只复用由内容身份派生的摘要、优化
事实和生成体。持久缓存成功原子落盘才恢复 clean，并按访问新近度限制为 512 MiB/20 万条。

## 10.2 并发模型

一次 revision 的变更写入是串行的，查询可并行；Web 构建依赖图仍由领域层组织，不把每个对象建模为 Actor。生产 bundler 共享进程级工作窃取执行器，使多 BuildContext 的总 worker 数受机器并行度约束；显式执行器仅用于测试和隔离实验。

## 10.3 wake_turbo

任务 ID 来自函数与参数指纹。slot 保存类型擦除值、输出指纹、依赖和 revision；浅校验、深校验、重算与早期截断组成红绿失效协议。per-task single-flight 锁保证同一任务只执行一次，线程本地上下文收集直接依赖。

## 10.4 Arena AST 生命周期

AST 持有者放入 `Arc` 管理的任务值，下游只在受控闭包中借用。Parser owner 同时保存 exact source 和
interner identity；优化入口校验两者以阻止同长度错误 source 或另一张 interner 表与 AST 配对。不得把
AST 引用、指针、`Atom`、`SymbolId` 或 interner identity 跨进程持久化。

## 10.5 执行器

工作窃取执行器处理同层 parse、optimize 和 body 扇出；source-map facts 是只依赖 body 的下游任务。
Loom 验证 single-flight 交错；循环依赖检测在同线程任务栈上拒绝递归环。

## 10.6 性能预算

预算以可复现 benchmark 而非设计目标数字为准。现有测量面包括 interner、lexer、parser、resolver、turbo 和 bundle；方法见 [PERFORMANCE.md](PERFORMANCE.md)。

## 10.7 缓存身份

目标浏览器、JSX、源码内容、压缩器版本、defines/drop flags、图边界的声明保留名/公开观察名/star 事实、可信编辑和保留名
进入相关输入。optimizer key 不含最终 chunk 编号，持久层单独保存 retained request specifier，再映射为
本代 module IDs；这些边收敛并重新规划
chunk 后，final-layout key 才标识 JavaScript body。body 发射始终记录 mapping facts，二者在持久层作为
一个 provenance group 原子写入和合并；map 开关不进入 optimize/body 的输入、相等性或哈希。body metadata 同时保存 typed finalizer
证明的全部 internal request 的目标字面量区间、稳定 specifier/role，以及 typed symbol 表决定的三个
collision-free runtime 参数名和真实的 `metaUrl` runtime capability；default/star interop 已结构化
内联，不进入 compact runtime 注入；最终布局只能修改经整组校验的 typed
range，任一界外、重叠、非规范字面量或当代 target 不匹配都使整模块 no-op。它参与 body hash 并随
mapping facts 持久化。当前 `wake-closure-minifier-v15` 与 schema 13 使旧路径产物自然 miss，不做格式迁移；当前
AST 的 `SymbolId`/`NodeId` 只可进入本次 optimize/codegen 内部，不进入指纹或持久 key。缓存命中必须与
冷构建产物等价。SCC/拓扑/concat 只消费 retained `ModuleEdges`，wrapper 名称和 Federation expose 在 typed
codegen/body identity 中决定；非 canonical runtime 名称的 concat 候选保守保留独立 factory，禁止从生成
body 重建这些语义或通过 helper/metaUrl token 扫描决定 runtime 注入。见
[ADR 0033](decisions/0033-structured-module-emit-provenance.md)。持久文件的校验 envelope、有界解码、
并发 authored-key 合并和失败诊断见
[ADR 0034](decisions/0034-transactional-persistent-cache-boundary.md)。

最终 map 合并按 placement 为 generated token 位置建立一次索引；模块局部 mapping 只进行精确位置与
单列 separator 回退查询。禁止对每条 mapping 从 token 列表头重新扫描，否则大模块会退化为
`O(mapping × token)`，并直接拖慢启用 Source Map 的代码分割与 lazy workspace 首次构建。

## 10.8 单机实现纪律

### 10.8.3 哈希表

内部非安全边界使用 `FxHashMap`；来自不可信网络的输入不直接进入可控碰撞的热表。

### 10.8.5 数据布局

热路径优先紧凑结构和稳定 ID，但不得为尺寸牺牲生命周期或错误恢复正确性。

# 11. 测试与质量

单元、snapshot、fixture、Miri、Loom、Node API、类型、打包审计和平台 smoke 共同构成门禁，详见 [TESTING.md](TESTING.md)。字符串断言只能覆盖代码形态，关键 bundler 回归还需执行生成产物。

# 12. 发布

主包与五个平台包使用同一版本。发布先验证源码和许可证，再在目标平台构建、审计不可变 tarball，Windows 原生构建还用 Yarn 4.16 PnP fixture 验证 Components 样式；之后先发平台包、最后发主包，并执行注册表干净安装。版本与 tarball 名从清单和 tag 动态取得。

# 13. 风险与降级

关键降级包括：增量引擎可关闭跨 revision 复用、危险 concat 回退独立 factory、单个无法证明安全的
压缩候选保持原形、终端非 TTY 回退 plain 日志。Source Map
不再关闭压缩或分包；优化表示不变量、可信编辑冲突或固定点不收敛返回构建诊断，不能静默输出旧路径
或未压缩代码。Typed emitter 只消费最终 `TypedProgram`；不存在用于降级的 parser-AST/span emitter。

# 14. 附录

## 14.1 依赖纪律

编译核心外部依赖白名单在 workspace `Cargo.toml` 维护；网络、序列化、配置、终端和 Node 依赖限制在边缘 crate。Closure 风格压缩重写只使用 workspace 现有依赖和 Wake 自有测试语料，不复制或下载第三方实现、测试、二进制或 fixture。新增依赖同时接受许可证检查。
