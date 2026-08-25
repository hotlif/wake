# Wake 测试与质量门禁

本文描述 v0.1.21 仓库实际执行的验证。命令应从仓库根运行；需要原生 Node 绑定或 JavaScript 测试 host 的测试必须先执行 `npm run native:build`。

# 1. 本地最小门禁

Rust 修改：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

架构或 crate 依赖修改：

```bash
npm run architecture:test
npm run architecture:check
```

`architecture:check` 还执行 [architecture-boundaries.json](architecture-boundaries.json) 的依赖
来源合同：tracked tree 不得包含第三方 source/binary vendor；workspace 外 Rust path dependency、
非 crates.io lock source 或缺失 checksum 会失败；npm lock 的外部 entry 必须来自 npm registry 且
带 SHA-512 integrity，声明为 conformance source 的少数 npm 包还必须精确 pin。Cargo build 与 npm
lifecycle 下载被禁止，依赖和 Rusty V8 归档只能在独立、校验 checksum 的准备步骤获取。

Node/npm 修改：

```bash
npm ci --ignore-scripts
npm run versions:check
npm run native:build
npm run npm:test
npm run npm:typecheck
npm run typescript:7:check
npm run npm:pack:check
npm run pnp:components:check
```

JavaScript 测试统一显式导入 `@crab-dev/wake/test`，由 `wake test` 执行；不得新增
`node:test`、Jest/Deno runner 或调用官方 Node/Deno 的产品回退路径。测试 API 采用熟悉的
`describe`/`test`/`expect` 形态，但门禁验证 Wake 自有契约而不是 Jest 兼容性。测试运行时的
分层最小门禁为：

```bash
cargo test -p wake_ecma_vm -p wake_js_runtime -p wake_test_browser -p wake_test -p wake_test_host
npm run test262:es2024
wake test scripts/check-architecture.test.mjs
wake test npm/wake/test
```

唯一保留的 Node runner 例外是 `npm run npm:test:wake:addon`。它加载真实 `.node` 绑定并验证
Node Worker、socket 与 cleanup-hook 生命周期，不能由 Wake 自身的测试 realm 证明；它不执行
Wake Test 语义，也不是产品运行时的回退路径。普通 JavaScript、CLI 和包测试不得加入该例外。

V8/handle、fast DOM、Chromium/CDP、协议并发与 React 差分 fixture 分别进入普通单元测试、
固定版本 conformance、真实浏览器 smoke 和 Loom 模型。系统浏览器测试必须记录可执行文件与
CDP 完整版本；CI 与发布门禁还必须通过 `engineering/system-browser-conformance.json` 的
exact-major admission。major 不匹配直接失败，不下载、不 vendor、也不选择 fallback 浏览器；
普通用户仍可运行任意 Wake 可识别的兼容系统浏览器。没有浏览器的普通 Rust 单元测试不得伪装成
browser gate。

`engineering/test262-es2024.json` 固定 Test262 官方提交、源码归档 SHA-256 与精确的 ES2024
选择目录。runner 对每个脚本分别执行非严格与严格变体并创建新 V8 realm；选中测试若要求尚未
实现的 module/agent/host 合同会直接失败，禁止以 skip 或更新 expected failure 掩盖。

PnP 门禁通过根工作区精确锁定的 `corepack@0.34.6` 启动 Yarn，避免依赖或覆盖 CI
镜像中的全局 `yarn` shim。

文档修改：

```bash
npm run docs:check
npm run docs:build
cargo test -p wake_docs
```

`docs:check` 校验 Frontmatter、slug、站内路由和仓库文档链接；`docs:build` 验证真实 MDX/React 打包，而不只检查 Markdown 文本。

Crab CSS 编辑器修改：

```bash
npm ci --ignore-scripts --prefix editors/vscode-css
npm run vscode:css:check
cargo test -p wake_css_language -p wake_css_lsp
```

本平台先构建 `wake_css_lsp` release 二进制，再用 `editors/vscode-css` 的 `package:vsix`
脚本生成 VSIX。脚本会检查归档只包含一个目标二进制、无开发目录且不超过 15 MiB。
Manifest 与语言服务测试还必须验证自定义 semantic token 的名称、legend 索引和 CSS TextMate
scope 回退一致，避免嵌入式 CSS 值重新继承宿主 TypeScript 的 `keyword` 主题规则。补全测试必须
在真实 Extension Host 中键入属性前缀、接受自动弹出的候选并断言编辑结果，同时验证模板外位置
返回不适用；不能只覆盖等同于 `Ctrl+Space` 的显式请求。

# 2. CI 矩阵

`.github/workflows/ci.yml` 当前包含：

| Job | 平台/工具链 | 目的 |
| --- | --- | --- |
| `fmt` | Ubuntu / Rust 1.95 | rustfmt 无差异 |
| `clippy` | Ubuntu / Rust 1.95 | workspace 全 target，warnings 视为错误 |
| `test` | Ubuntu、Windows / Rust 1.95 + Node 24 | 全 workspace 测试 |
| `typescript-7` | Ubuntu、Windows / Node 24 + TypeScript 7 | TS7 CLI 版本、严格类型与 TS/TSX 兼容 fixture |
| `miri` | Ubuntu / nightly | `wake_ecma_ast` 和 `wake_turbo` 手写 unsafe/内存模型 |
| `loom` | Ubuntu / Rust 1.95 | single-flight 线程交错 |
| `bench-smoke` | Ubuntu / Rust 1.95 | 全 benchmark 可编译 |
| `node` | Windows / Node 24、26 | 原生绑定、API、类型、启动与 npm pack；Node 24 另跑 Yarn 4.16 PnP Components 门禁 |
| `docs` | Ubuntu / Rust 1.95 + Node 24 | 文档链接检查和生产构建 |
| `vscode-css` | Windows、macOS x64/arm64、manylinux glibc 2.28 x64/arm64 | 语言核心、Extension Host、五个平台 VSIX 和归档白名单 |

Node 包声明支持 `>=22.14 <27`，常规 CI 目前只覆盖 24 与 26；补齐最低版本覆盖列入路线图。

# 3. 回归矩阵

## 3.1 编译器

- lexer snapshot：字面量、模板、正则、ASI、类私有名和 JSX 边界；
- parser/semantic：JS、TS、JSX、TSX、scope、引用、依赖与顶层 await；
- TypeScript 7 compatibility fixture：高级类型、类/函数、模块、TSX、值语义及严格类型负例；
- codegen/minify：优先级括号、重命名、DCE、导出和 Source Map；
- fuzz smoke：随机输入不 panic；深度 fuzz 使用 `cargo +nightly fuzz run lex` 人工执行。

## 3.2 增量与缓存

- 同内容二次构建应命中；
- 单文件变化只失效受影响记录；
- resolver miss、结构性文件和配置身份正确失效；
- persistent cache 冷/热路径的 Tree Shaking、concat、顶层 await 和代码分割产物等价；
- `wake_turbo` 红绿参考、并发压力、循环检测和纯并行降级通过。

## 3.3 Bundler fixture

`fixtures/` 覆盖：

- `hello-esm`：最小 ESM；
- `react-ts-app`：React 19 + TypeScript；
- `react-ts-app-yarn-pnp`：Yarn PnP/zip 包；
- `react-components-yarn-pnp`：只声明 Wake、React 和 React DOM 的 Components PnP 发布包门禁；
- `react-docs`：MDX、Demo、Props、主题和组件工作台；
- `react-docs-workspaces`：一个主站与两个隔离工作台的聚合生产构建；
- `2k-modules`：生成式压力与跨工具测量。

关键语义必须执行产物或在真实服务器中验证。仅检查 bundle 字符串适合局部形态断言，不能替代运行时回归。
Library CommonJS 回归必须由 Node 加载最终入口和内部 preserve-module 文件；Windows 重复构建测试
必须覆盖被 watcher 持有的输出目录、变化声明替换和中途失败回滚。

## 3.4 Docs

- Frontmatter 必填字段、重复 slug、MDX 静态属性和 import；
- Demo glob、Preview、主题、Props/JSDoc 和 API 表；
- Components 模式的 Controls、默认值、显式 unset、hash round-trip、视口和错误恢复；
- `base_path`、404 外壳、public 资源和静态托管路径。
- 聚合挂载冲突、lazy single-flight、作用域 HMR、事务回滚和根/子 manifest。

## 3.5 Node API

- ESM/CommonJS 导出一致；
- build、bundle、BuildContext、应用/文档服务器和事件；
- AbortSignal、关闭幂等、async dispose 和结构化错误；
- experimental 句柄 dispose、字符串便捷入口和 TypeScript 声明；
- 主包与五个平台包的版本、文件清单和可选依赖一致。

## 3.6 React 优先测试系统

- `wake_ecma_vm` 的 V8 realm、Promise job、异常位置、终止和 handle 生命周期；除该 crate 外
  任意 workspace crate 直接依赖 `deno_core` 都必须被架构门禁拒绝；
- fast DOM 在 React/React DOM 求值前完成 same-realm 安装，满足
  `globalThis === window === self`、`document.defaultView === globalThis` 和
  `IS_REACT_ACT_ENVIRONMENT`，并在每个 suite 后清除 root、DOM、timer、storage 与网络状态；
- React 19 的 createRoot、hooks/effects、portal、controlled form、Suspense、lazy、error
  boundary、async `act`、SSR 解析与 hydration 诊断；
- Chromium BrowserContext/page 隔离、真实 keyboard/pointer/default action、focus/selection、
  CSS/layout、导航、accessibility、hydration、截图、network interception 与 V8 coverage；
- fast DOM 与 Chromium 对 Wake conformance manifest 的差分结果；layout、原生输入、截图和
  browser-sensitive hydration 只接受真实 Chromium 证据；
- 显式测试 API、hooks、focus/skip/todo/each、assertion count、function mock/spy、网络 mock、
  现代 async clock、外部 value/DOM/accessibility/visual snapshot；不得把 Jest automock、mock
  hoisting、legacy timer、inline snapshot 或 Babel coverage 当作 Wake 能力；
- V8 range coverage 经 Source Map 回映原始 JS/TS/JSX/TSX，并在 cold、warm 与 watch rerun 中
  产生同一规范化 Wake schema；
- reverse-dependency watch 只重跑受影响 suite，但每轮获得干净 realm 或 BrowserContext；浏览器
  进程可以复用，页面、module registry 与 DOM 状态不能复用；
- 无限循环、host/browser panic 或 crash、协议损坏、资源 origin 伪造、取消和关闭不泄漏调用
  进程、端口、profile、page 或 V8 handle；
- CLI、Node `runTests()` 和 `TestContext` 经过唯一持久 host session，返回同一稳定结果与事件模型；
- Windows x64、macOS x64/arm64、glibc 2.28 Linux x64/arm64 都执行显式 browser path 或系统
  browser discovery、版本记录、CDP lifecycle 和 React smoke；浏览器二进制不得进入平台包。

# 4. Miri 与 Loom 分工

Miri 用于 `ModuleAst` 自引用持有者等 Rust 自有 unsafe 内存模型。V8 是外部原生引擎，不能用
Miri 结果替代 isolate/handle/termination 的真实进程测试；这些生命周期必须由普通测试、崩溃
fixture 和适用平台的 sanitizer/leak gate 覆盖。涉及 crossbeam 的长时并发用例在 Miri 下按已知
限制跳过，由普通并发测试和 Loom 接管。

Loom 使用 `RUSTFLAGS="--cfg loom"` 穷举 single-flight 与可建模的 test-host session/关闭协议。
普通 workspace test 不能替代 Loom，Loom 也不覆盖 V8、Chromium、resolver、bundler 或服务器
端到端行为。

# 5. Benchmark 门禁

CI 当前执行：

```bash
cargo test -p wake_bundler --test performance_invariants --release
cargo bench --workspace --no-run
```

第一条用稳定 work-count 检查 edit-one 的 loader、resolver、link/chunk 与 codegen 工作局部性；第二条保证 benchmark 可编译。两者都不判断机器相关的耗时回归。提交性能数字或优化声明前按 [PERFORMANCE.md](PERFORMANCE.md) 运行实际测量并保存环境信息。

# 6. 发布门禁

发布 workflow 在 tag 上：

1. 运行 workspace test、clippy 和许可证检查；
2. 在 Windows、macOS x64/arm64、manylinux glibc 2.28 x64/arm64 构建；
3. 审计七个不可变 tarball 的版本、许可证、文件和体积；五个平台包必须包含唯一 Node binding、
   `test-host/wake-test-host[.exe]`、可复算 checksum/build ID 的 native manifest、SPDX SBOM 与第三方
   许可证清单；独立准备步骤中的 Rusty V8 target 归档必须先匹配固定 SHA-256，再以本地 archive
   输入 locked/offline Cargo build；仓库和平台包不得包含第三方源码、浏览器、下载器、install
   script 或该归档；
4. 在 Windows 原生发布 leg 中把主包和五个平台包打成本地 tarball，以 Yarn 4.16 PnP 构建 Components fixture，并检查 internal runtime、Lucide 运行时导出、hashed CSS link 和 18 个组件前缀；
5. 发布前在五个目标平台和 Node 22.14/24/26 的 15 个组合中，以本次构建的主包、CSS 包和 matching
   platform package 本地 tarball 执行 `--ignore-scripts` clean install；外部已发布依赖可以来自 registry，
   但三个待发布包必须保持可验证的 `file:` 来源。每个组合执行 `wake test`、`runTests()` 和长期
   `TestContext` lifecycle；Node 24 还必须用显式发现的系统 Chrome、Edge 或 Chromium 执行 React
   browser/screenshot smoke，并从 `wake.test.v1` 结果校验仓库固定的 browser family/major。包括
   Linux ARM 在内，找不到浏览器或版本不匹配必须阻止发布，不得 skip、放宽范围或下载自愈；
6. 只有上述本地 tarball matrix 全绿后，才先发布平台包、最后发布主包；
7. 发布后继续在 Node 24/26 和全部目标平台执行注册表干净安装与构建 smoke。平台包的硬上限为 64 MiB
   packed / 192 MiB unpacked，56 MiB packed 起发布 warning；运行期不得下载 host 或浏览器。

部分发布失败不能覆盖已有 npm 版本；修复后统一提升七个包和 workspace 版本。

Wake Test 仍是 experimental。除上述通用发布门禁外，转为 stable 前还必须同时满足：从 registry
lock 记录固定并审计 Deno/V8 与私有 DOM adapter 的来源、checksum、许可证和 SBOM；在五个平台执行 fast DOM
与 Chromium React matrix；验证 browser/host crash 与取消；证明 source-mapped coverage、snapshot
和 watch 冷热等价；确认平台包没有浏览器制品且仍满足 GLIBC 2.28、白名单和体积上限；删除活动
代码、类型、配置和文档中的 Jest、Boa、jsdom 与 Node-API test-runtime 兼容路径。门禁缺一项时，
CLI、Node API 和 npm test entry 都不得标记 stable。
