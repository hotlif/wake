# Wake 当前架构

## 1. 系统定位

Wake 是 Rust 原生的 Web 构建工具，同时提供 CLI、Node.js API、应用开发服务器和 React 组件文档系统。工作区当前包含 25 个 crate，npm 主包通过五个平台包加载预编译 Node-API 模块。

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

- `wake_resolver`：Node 风格包解析、alias、package exports、Yarn PnP 与 zip 文件系统。
- `wake_graph`：模块和绑定活跃性、Tree Shaking 所需图分析。
- `wake_css`、`wake_css_in_js`、`wake_html`：非 JavaScript 内容和 HTML 外壳。
- `wake_css_language`：`@crab-dev/css` 模板发现、虚拟 CSS、宿主映射和无文件系统编辑分析；构建诊断仍由 `wake_css_in_js` 拥有。
- `wake_scan`、`wake_tsdoc`：组件扫描、Demo/Props 类型和 JSDoc 提取。

### 2.3 执行与编排层

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
```

必须维持的约束：

1. 编译核心不得依赖 CLI、Node、dev server 或 bundler；基础、AST、lexer、semantic、transform 与 parser 的允许出边由机器规则逐层限定。
2. `wake_graph` 不读取文件、不解析路径、不写产物。
3. `wake_cache` 只保存跨进程稳定数据，不保存 arena/Atom 句柄。
4. `wake_turbo` 不认识 Web 构建领域对象。
5. CLI 和 Node 的配置、错误、构建、Docs 与服务生命周期经 `wake_app` 共享；显式 compiler/experimental API 可直达 compiler crates，但不得直达 bundler 或其他产品层。

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
