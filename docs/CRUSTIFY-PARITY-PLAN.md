# Wake ← crustify 功能对齐计划（CRUSTIFY-PARITY）

> 目标：让 `wake_cli` **完整支持** `@crab-dev/crustify` 对用户暴露的能力。
> crustify 是基于 Webpack 5 + Babel + SWC 的零配置 React 19 构建器；wake 是**自研 Rust 内核、无 JS 运行时**。
> 因此"对齐"= 用 wake 原生能力**复刻用户可见能力**，而非移植 webpack loader。

## 0. 决策基线（已确认）

| # | 决策 | 选定 | 理由 |
|---|---|---|---|
| ① | 配置文件 | **声明式 `wake.config.toml`** | Rust 无 JS 运行时，不能执行 `.crustify.ts`；正则/枚举类字段用字符串或内置枚举表达 |
| ② | HTML 生成 | **静态 `index.html` 模板 + 资源注入** | `renderToString` 需真实 React；改 Vite 模式静态外壳，覆盖绝大多数场景 |
| ③ | 高级特性范围 | **完整对齐**（MDX / linaria / React Compiler 全做） | 排在主干之后的 M5，committed，不降级 |
| ④ | 插件系统 | **声明式配置 hook**（折叠进 `wake.config.toml`） | mods 是 JS 函数，Rust 无法加载；用声明式项覆盖常见用途 |

## 1. wake 现状核对（一手源码结论）

**已具备**（相对旧记忆的更正）：
- ✅ **JSX → automatic runtime**（parse 期降级 `react/jsx-runtime`，`wake_ecma_parser/src/jsx.rs`）
- ✅ **TS 擦除** + **enum/namespace → IIFE 值转换**（`wake_ecma_parser/src/stmt.rs`）
- ✅ 图片/svg/字体 → base64 内联、JSON、CSS(dev `<style>` 注入)、CSS Modules（`wake_bundler/src/loader.rs`）
- ✅ Tree Shaking / 代码分割 / contenthash / 持久化缓存 / CJS interop / Yarn PnP（`wake_bundler/src/incremental.rs`）
- ✅ dev server：HMR(live-reload) + SPA fallback + 错误 overlay + 文件监听（`wake_dev_server`）
- ✅ 解析：node_modules / main·module / ts-twin(`.js`→`.ts`)（`wake_resolver`）

**缺口**：配置加载、路径别名/tsconfig paths、组件扫描、入口/HTML 生成、build 产 HTML、JS 压缩、CSS prod 抽取/压缩、`.raw`/MDX/asset 阈值、dev proxy/https/host/open/多 chunk 服务、sourcemap、React Compiler/linaria/auto-import-style、buffer/string_decoder polyfill。

### 1.1 多 agent 差距综合的交叉校验补充
（workflow `wake-vs-crustify-gap` 结论，与一手阅读一致，评估 wake ≈ 30-40% 到位：快路径已成、框架层是空地。补充要点：）
- **`define` / `process.env.NODE_ENV`**：codegen 已有 `DEFAULT_DEFINE` 的 NODE_ENV 替换（部分），但缺分支裁剪 + 可配置 `define` 映射 + dev/prod 选择。**React prod 构建必需**（否则 React 走 dev 慢路径 + 保留 warning）。→ 纳入 M1 配置 + M3。
- **dev HMR 现为 live-reload（整页刷新），非 React Fast Refresh（状态保留）**。crustify "存盘即热更新" 依赖 react-refresh 保留组件 state。真正对齐需 Fast Refresh transform（`$RefreshReg$`/`$RefreshSig$`）+ 模块级更新协议。→ M5（DESIGN 5.6）。此前计划把 HMR 记为 ✓ 属乐观，更正为"live-reload ✓ / Fast Refresh ✗"。
- **preset-env 目标降级**：wake 做擦除不做语法降级到旧目标（现代浏览器通常无碍）。→ 归入 M5 `wake_ecma_transform` 落地，可选。
- **JSX 仅 automatic runtime**（无 classic pragma / `jsxImportSource` / `jsxDEV`）。多数场景够用。
- **建议新增 crate**（细化 M2–M5 的落点）：`wake_config`(M1) · `wake_scan`(M2) · `wake_html`(M2) · `wake_plugin`(声明式 hook，折叠进 config) · `wake_mdx`(M5) · `wake_css_in_js`(M5) · `wake_react_compiler`(M5)；就地扩展：codegen 加 sourcemap+minify、bundler 加虚拟模块/资源产物/publicPath/CSS 抽取、resolver 加 alias+polyfill、dev_server 加 `ServeOptions`。

## 2. Milestone 计划

### M1 — 配置 + 别名地基〔解锁一切〕· L · **硬前置** ✅ 完成
- [x] 新增 crate `wake_config`：`serde`+`toml` 反序列化 `wake.config.toml`，schema 对齐 crustify `Config`（含 `define`）
- [x] `wake_resolver`：`ResolveOptions` 增 `alias`；`resolve_uncached` 前置最长前缀 alias 匹配；`with_pnp_options`（PnP 不丢别名）
- [x] `wake_bundler::IncrementalBundler`：`set_resolve_options`；PnP 检测复用已配置选项；`pub use ResolveOptions`
- [x] `wake_cli`：`load_resolve_options` 读配置 → 组装别名 → build/dev 均接线；`wake_dev_server::serve` 加 `ResolveOptions` 参数
- [x] **验收通过**：fixture（`@`→src、`@@`→根、配置 `~`→lib 三种别名 + JSX + JSON）`wake build` 5 模块全解析、产物正确；单测 wake_config 4 / wake_resolver 20 / wake_bundler 58 / wake_dev_server 3 全绿。
- ⏭ 后续（非阻塞）：tsconfig `paths` 读取、`extensionAlias` 可配置、`define`/publicPath 贯穿构建（并入 M3）。

### M2 — 组件扫描 + 入口/HTML 生成〔crustify 招牌〕· L ✅ 完成
- [x] 新增 [wake_scan](../crates/wake_scan/src/lib.rs)：递归扫描 + include/exclude 正则 + `generate_source`（内联源码）
- [x] frontmatter：`.ts/.tsx/.js/.jsx` 首块注释 → TOML；`.md/.mdx` → `+++…+++` frontmatter；`toml::Value`→JSON
- [x] 生成 `@@@/{ns}` 懒加载模块（`const x=import("@@/rel"); {name,component,path,frontmatter,source}`）
- [x] 虚拟入口生成 [wake_cli `virtual_entry`]：`wake build`（无 entry）→ `.wake/entry.tsx` = `import("@@/src/entry.tsx")`，对齐 crustify `app:build`
- [x] 新增 [wake_html](../crates/wake_html/src/lib.rs)：模板（`config.html.template`/`public/index.html`/内置外壳）+ 入口 chunk `<script defer>` 注入 + `publicPath` 前缀
- [x] CLI：`prepare_project`（配置+扫描+别名一体）；build/watch/dev 均接线；`entry` 改 `Option`
- [x] **验收通过**：fixture（`component_scan` pages + include 正则筛 `.page.tsx` + TOML 块注释 frontmatter + 自定义模板 + publicPath `/app/`）→ `wake build`（无 entry）产 6 模块/5 chunk，每页独立 async chunk 懒加载，`.wake/scan/pages.ts` 仅含 2 页且 frontmatter 正确，`dist/index.html` 注入 `/app/entry.<hash>.js`。单测 wake_scan 7 / wake_html 7；全 workspace 测试全绿；fmt+clippy 干净。
- ⚠️ **用户须知**：`wake.config.toml` 里的正则（include/exclude）须用 **TOML 字面量字符串（单引号）**，如 `include = '\.page\.tsx$'`——双引号会把 `\.` 当非法转义。
- ⏭ 后续（非阻塞）：autoscan `tsconfig.json` 生成（IDE 提示）、`generateSourceCharacter` 走 `.raw`（并入 M3）、watch 模式下 scan 增量重跑、CSS `<link>` 注入（依赖 M3 prod 抽取）。

### M3 — 完整 dev/prod 产物 · L 〔进行中〕
**切片 1（✅ 完成）——构建侧内聚三件：**
- [x] **`define` / `process.env.NODE_ENV` 可配置**（CRUSTIFY-PARITY §M3 核心）：codegen `define` 由 `&'static` 改为可传入（新增 `codegen_module_shaken_with`，`'d` 生命周期使替换字面量不绑 `&self`）；`IncrementalBundler::set_define`（+ `define_hash` 混入产物缓存键，dev↔prod 精确失效）；CLI `build_define`（prod=`"production"` / dev=`"development"` + 用户 `[define]`，用户可覆盖）；build/watch=prod、`wake dev`=dev。
- [x] **`.raw` → asset/source**（[loader.rs](../crates/wake_bundler/src/loader.rs) `raw_to_js_module`，`export default "<文本>"`）。
- [x] **`clean` dist**（[wake_cli `clean_outdir`]，移除过期 hash chunk；watch 仅首次清）。
- **切片 1 验收通过**：fixture（`process.env.NODE_ENV` + 自定义 `[define] process.env.API` + `.raw` 导入）→ prod build 得 `"production"`/`"/api/v2"`/内联 raw、无残留；`set_define("development")` 单测绿；clean 后旧 hash chunk 消失。全 workspace 测试全绿、fmt/clippy 干净。

**切片 2（⏭ 待做）——资源与 CSS：**
- [ ] asset **4KB 阈值** + 超阈值独立产物（hash 命名 + 服务/拷贝）；`url()` 资源改写
- [ ] prod **CSS 抽取为 `.css`** 独立产物（`rewrite_urls` 钩子已在）+ HTML `<link>` 注入（`emit_html` 的 `styles` 填充）
- [ ] `publicPath` 贯穿 runtime chunk 加载

**切片 3（✅ 完成）——dev server 网络层：**
- [x] **proxy**（context/target/pathRewrite(正则)/changeOrigin）：`ServeOptions`+`ProxyRule`；[wake_dev_server](../crates/wake_dev_server/src/lib.rs) 用 `awc` 转发，默认服务先试代理（任意方法）再回退 SPA；`CompiledProxy`（正则预编译）
- [x] **host**（`0.0.0.0` 局域网）/ **open**（跨平台开浏览器）；端口配置 `devServer.port` 优先
- [x] CLI `cmd_dev` 从 `config.dev_server` 装配 host/open/proxy/port
- **切片 3 验收通过**：live 烟测——`/api/ping.txt` 转发+pathRewrite 去前缀得后端响应、`/` 回退 SPA、后端 404 透传；`CompiledProxy` matches/rewrite 单测绿；fmt/clippy/全量测试全绿。
- ⏭ 待做：**https**（需 rustls+rcgen TLS，配了 `server="https"` 现告警回退 http）、**ws 代理**（配了 `ws=true` 现告警仅转 HTTP）。

**切片 2（✅ 完成）——资源与 CSS（bundler emit 手术）：**
- [x] **带外产物基建**：`BuildOutput.assets: Vec<OutputAsset>`（[wake_bundler](../crates/wake_bundler/src/lib.rs)），CLI 写盘。
- [x] **asset 4KB 阈值**：[loader](../crates/wake_bundler/src/loader.rs) `LoadOptions`（extract_css/asset_inline_limit/public_path）；超阈值写 `<stem>.<hash>.<ext>` 独立产物 + 模块导出 `publicPath` URL，`≤` 阈值内联 base64。
- [x] **prod CSS 抽取为 `.css`**：`css_extract`/`css_module_extract`（不注入 `<style>`，CSS 文本带出），driver 聚合（模块 id 升序）为 `styles.<hash>.css`；`emit_html` 注入 `<link>`。
- [x] bundler `enable_css_extraction`/`set_asset_inline_limit`/`set_public_path`；CLI build/watch prod 开启。
- **切片 2 验收通过**：fixture（普通 CSS + CSS Module + 5KB/1KB 资源 + publicPath `/app/`）→ 8/8 校验：CSS 抽取（含作用域化 `.card_964e17`）、`<link>` 带 publicPath、bundle 不注入 `<style>`、大资源独立产物 URL、小资源内联、CSS Module 导出映射。`css_extraction_and_asset_threshold` 单测绿；真实 `react-ts-app` prod 全管线 19 模块无回归。
- ⏭ 待做（非阻塞）：`url()` 资源改写、CSS 抽取的运行时 eval 序（当前 BFS 发现序）、polyfill 决策。

### M3 小结 ✅（核心三切片完成）
- 切片 1 define/`.raw`/clean · 切片 2 asset 阈值/CSS 抽取 · 切片 3 dev proxy/host/open — 全部验证通过、CI-clean。
- **遗留（非阻塞）**：dev server **https**（rustls+rcgen）、**ws 代理**；`buffer`/`string_decoder` polyfill。

### M4 — prod 压缩 + sourcemap · XL（分阶段，按安全性排序）
> 设计经多-agent workflow 撑出（codegen 发射机制 + 语义模型 + 压缩器设计）：压缩器**作为 codegen 发射期决策**渐进落地（mangling 除外走独立 crate），每 pass 往返验证后再进下一 pass。
- [x] **M4a 紧凑输出**（✅）：codegen `minify` 字段，`newline()` 在 minify 下空操作（唯一换行/缩进来源；语句均发显式 `;`/`}` → **ASI 安全**）。`codegen_module_shaken_with` 加 `minify` 参数；bundler `enable_minify` + `MINIFY_SALT` 混入缓存键；CLI prod 启用。验收：react-ts-app **1.58→1.36 MB（-14%）**、往返 `wake parse` 成功、node 跑非 DOM fixture 语义正确（`5 - -3`=8 等）；codegen 61 + bundler 28 单测绿。
- [x] **M4b define 驱动死分支消除**（✅）：`ConstVal` + `const_eval`/`const_eval_bool`（字面量/define 成员/`!`/严格 `===`·`!==`/`&&`·`||` 短路，纯节点才求值 → 有副作用 test 自然拒绝）；`Statement::If` 在 minify 下 decide-then-skip 折叠（不改 AST、Span 保持）；**被丢弃分支含 `var`/函数提升声明则保守不折叠**（`has_hoisted_decl` 守卫，避免 ReferenceError）。验收：3 单测（dev 块剥离 / var 守卫 / dev 不折叠）；fixture node 运行 `result=42`（`if false` dev 块剥离、`if true` 折叠为 consequent）；react-ts-app **`if(false)` 残留 0、`process.env.NODE_ENV` 残留 0**（模块内死分支全部折叠）。
  - ⏭ 同类 pass 待做：return/throw 后不可达码、空语句、`debugger` 清理；条件表达式 `?:` 折叠。
- [x] **死模块消除（DME）**（✅ M4b 的兑现）：codegen DCE 剥离死 `require` 后，emit 前从 entry 按各模块体中**存活**的 `__wake_require__(N)` / `.import(cid,N)` 边重算可达（`live_modules`/`extract_referenced_ids`），丢弃不可达模块。边提取覆盖所有模块引用形式（静态/内联动态/split 动态/require）→ 无假阴性；误判只「多留」不「错删」。bundler `enable_dead_module_elimination`；CLI prod 启用。
  - **成果**：**react-ts-app 1.36 MB → 600 KB（-56%，19→14 模块）**，dev react 警告全消；**引用完整性验证：14 注册/14 引用/0 悬空**（无活模块被错删）；DME fixture node 运行 `greet=PROD`（dev 模块丢弃、prod 保留）；`dead_module_elimination_drops_unreachable` 单测绿。
  - 📊 **M4a+M4b+DME 累计：react-ts-app 1.58 MB → 600 KB（-62%）**。
- [x] **M4c CSS 压缩**（✅ `wake_css::minify`）：UTF-8 安全 push-slice 扫描——折叠空白为单空格、去注释、删 `{}`/`;`/`,` 相邻空白、删 `}` 前 `;`；字符串内原样。**安全子集刻意不删**：后代组合器空白（`.a .b`）、`calc(1px + 2px)` 值内空白、`>`/`+`/`~` 组合器与 `prop: value` 冒号后空白（删了会破坏语义）。bundler CSS 聚合处 prod 调用。验收：5 单测（含 descendant/calc/注释/字符串/UTF-8）；fixture 输出 `.nav .item{color: red;width: calc(100% - 20px);margin: 0}.a > .b{padding: 8px}`——压缩且语义完整。
- [ ] **标识符 mangling**（独立 `wake_ecma_minify`，需语义模型 scopes/symbols，XL 最高风险）
- [ ] **M4d codegen sourcemap**（VLQ）+ dev devtool
- **验收**：prod bundle 体积与 esbuild `--minify` 同量级；DevTools 行号正确。

### M5 — 与 crustify 完全对齐的高级转换 · XL（全做，按难度递增）
- [ ] auto-import-style（M）：组件自动注入同名样式 import
- [ ] MDX（L）：`.md/.mdx` → JS（remark-gfm 子集 + frontmatter）；`wake_ecma_transform` 落地
- [ ] linaria / 零运行时 CSS-in-JS（XL）：构建期求值 `css`/`styled` → 提取静态 CSS + 类名替换
- [ ] React Compiler（XL+）：自动记忆化编译，独立立项
- **验收**：crustify 示例项目逐特性对齐产出。

## 3. 依赖与并行度
- **M1 硬前置**（配置 + 别名解锁一切）
- M2、M3 依赖 M1；M4 可与 M2/M3 并行；M5 依赖 M2（扫描/入口）+ M3（CSS 抽取）

## 4. wake.config.toml 目标 schema（草案）
```toml
root_dir = "."
public_path = "/"

[alias]
"@" = "src"
"@@" = "."

[[component_scan]]
namespace = "pages"
cwd = "src/pages"
generate_source = false
include = "\\.page\\.tsx$"   # 正则字符串
# exclude = "..."

[dev_server]
server = "http"        # http | https
port = 5173
host = "127.0.0.1"
open = false

[[dev_server.proxy]]
context = ["/api"]
target = "http://localhost:8080"
ws = false
change_origin = false
# [dev_server.proxy.path_rewrite]  "^/api" = ""

[html]
template = "public/index.html"
entry = "src/entry.tsx"       # 虚拟入口目标（替代 modifyEntry）

[hooks]                        # 声明式，替代 mods
bootstrap_path = "src/bootstrap.tsx"
```
