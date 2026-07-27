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

### 1.2 实证复核（2026-07-27）——**本文档的 ✅ 不可全信**

对本文件与 [TS-SYNTAX-SUPPORT.md](TS-SYNTAX-SUPPORT.md) 的 **113 条声明**做了一次实证复核：每条都建真实项目、
用 `wake build`/`wake dev` 跑、检查产物 / 用 node 执行，**不靠读代码下结论**。最严重的 6 条另派独立复核者
「尽力驳回」，**无一被驳回**（多条反而被发现比初报更严重）。

| 裁决 | 条数 |
|---|---|
| CONFIRMED（属实） | 66 |
| PARTIAL（可用但有未记载的边界） | 18 |
| **BROKEN（声明不成立 / 产出错误结果）** | **27**（其中 high 19） |
| UNTESTED（CLI 无观测途径） | 2 |

**方法论教训（这是最该记住的一条）**：出问题的绝大多数不是「没做」，而是
**「做了、测试全绿、文档标 ✅，但产出静默错误结果」**。原因是历次验收大量采用
「grep 产物文本」而非「跑产物断言行为」，且验收 fixture 常常恰好绕开自己刚引入的回归
（例：M8 声称验收 `bundle/.map 均正常`，而所用的 react-ts-app 不含动态 `import()`，
分割根本没发生，正好躲过它自己引入的 dev sourcemap 失效）。
**后续任何验收都应以「node 跑产物 + 断言行为」为准。**

#### 需要优先处理的（high，全部实证复现）

| # | 症状 | 影响面 |
|---|---|---|
| 1 | **prod 单 bundle 把非入口模块的所有非 ASCII 字面量变成乱码**（`incremental.rs` `strip_standalone_requires` 里 `bytes[i] as char` 按 Latin-1 重建）| 任何中文/日文/emoji 文案、i18n、`.json`、`.raw`。react-ts-app 是纯 ASCII 才没暴露 |
| 2 | **`export { x }` / `export { x as y }` 的导出赋值不跟随 mangle** → 整包加载即 ReferenceError | 默认 prod 构建；`export const/function` 内联形式不受影响 |
| 3 | **配置驱动 `wake build`（无 entry）产物里 ESM 默认导入全部失效** —— 分包 chunk 运行时的 `interopDefault` 认 `__esModule`，而 minify 后的 chunk 体不再发该标记 | crustify 对齐的默认工作流；≥2 chunk 即中招 |
| 4 | **minify 在模板字面量 quasi 与 `${` 之间插空格**（`` `user${id}` `` → `` `user ${id}` ``）| 所有含模板字面量的 prod 产物；标签模板 `.raw` 与计算属性键同样被污染 |
| 5 | **常量传播把死分支内 `var x = 字面量` 的初值传播到分支外** → 错误运行时值（架空了 `has_hoisted_decl` 守卫）| 任意条件块，非 define 专属 |
| 6 | **enum 成员引用同 enum 的前序成员**（`enum E { A = 1, B = A * 3 }`）原样发裸名 → ReferenceError；**非纯数字字面量成员丢失反向映射** → 反查静默 undefined | 所有 TS enum |
| 7 | **`accessor` 字段原样发射**，而无任何引擎实现 → 整包 SyntaxError；带装饰器时装饰器被**静默丢弃**（非文档所称「原样发射、不产出错误语义」）| 用了 auto-accessor 的类；CLI 仍报「✓ 构建成功」 |
| 8 | **`wake dev` 不认 `root_dir`**：入口/public/HTML 用 CLI ROOT 参数，别名却用 `root_dir` → 同一次运行两个根 | 配了 `root_dir` 的项目；`wake build` 正常 |
| 9 | **dev sourcemap 一遇动态 `import()` 即整体失效**（`/bundle.js.map` 404）| M8 开启代码分割后引入的回归 |
| 10 | **`wake.config.toml` 任何语法错 → 整份配置静默回退默认值，构建仍 exit 0** | 别名/define/publicPath/模板全失效而无感知 |
| 11 | **namespace 体内的 `export namespace`/`export enum` 跨声明合并失效**（种子取内层局部变量而非父对象属性）| 嵌套 namespace |
| 12 | **组件扫描目录里有任何 `.md`/`.mdx` 即构建失败**（无条件发 `import()`，而 MDX loader 排在 M5），且诊断不含出错文件名 | M2 的 frontmatter 支持与 M5 的 MDX 待做自相矛盾 |
| 13 | **非压缩路径把对象解构的属性重命名 `{ q: renamed }` 发成 `{ renamed }`** → 静默读错属性 | `--sourcemap`、`wake dev`、`WAKE_NO_MANGLE=1`；默认 prod 因 mangle 写全名而侥幸正确 |
| 14 | **CSS-in-JS 插值失败在选择器/at-rule prelude 位置时留下不平衡花括号** → 后续规则被浏览器整条吞掉 | 声明值位置的降级是好的，非声明值位置不是 |

其余 medium/low：扩展名判定大小写敏感（`.CSS`/`.SCSS` 落进 JS parser）、配置未知键静默忽略、
`WAKE_NO_MANGLE=1` 下常量传播产出语法非法的 `{1,2}`、`.tsx` 中泛型箭头函数 `<T,>()=>` 硬解析失败、
jsxDEV 的 `fileName` 泄露 Windows `\\?\` 前缀。

> ⚠️ 27 条 BROKEN 中只有 6 条经过了对抗性复核（预算所限），其余 21 条为单一复核者的实证结论
> ——证据链完整（含复现步骤），但未经第二人独立复现。

📄 **完整清单（27 条 BROKEN + 18 条 PARTIAL，每条含实测输出与可复现步骤）见
[AUDIT-2026-07-27.md](AUDIT-2026-07-27.md)**，另附 6 条对抗性复核结论与 40 处文档矛盾。

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

**切片 2（✅ 完成）——资源与 CSS：**
- [x] asset **4KB 阈值** + 超阈值独立产物（hash 命名 `<stem>.<hash8>.<ext>` + CLI 写盘）
- [x] **`url()` 资源改写**（[loader.rs](../crates/wake_bundler/src/loader.rs) `prepare_css` / `rewrite_one_css_url`）：CSS 里的字体与图片（`@font-face` 的 `src`、`background-image`）按同一阈值内联 base64 或写为独立产物，url 改写为最终 URL（带 `publicPath`）。**dev/prod × 普通 CSS/CSS Modules 四条路径统一处理**。`data:`/`http(s):`/`//`/`#fragment`/`/absolute` 一律不改写；`?#iefix`、`#fontname` 这类查询串/片段解析时剥离、发 URL 时带回；读不到的引用原样保留（不让一处笔误炸掉整个构建）。
  - CSS Modules 路径：`transform_modules` 不记录 url 位置，故对其**输出**重跑一次 `analyze` 拿偏移（此时 `@import` 已移除，`analyze` 对余下文本恒等）。
  - 改写必须在**模块粒度**做——`CssUrl` 偏移相对单模块 `code`，进了 `split_css_imports`/跨模块聚合/`minify` 就全废。
- [x] prod **CSS 抽取为 `.css`** 独立产物 + HTML `<link>` 注入（`emit_html` 的 `styles` 填充）
- [x] **CSS 聚合按依赖后序**（`incremental.rs::css_emission_order`）：层叠顺序须与 dev 的 `<style>` 注入顺序（= 模块求值序，依赖先行）一致。此前按模块 id（BFS 发现序）排，被 `@import` 的基础样式会排到消费方**之后**，覆盖关系整个反过来。
- [x] manifest 增 `styles` / `assets` 字段（SSR / CDN 上传脚本需要 hash 后的真实文件名）
- [x] watcher 扩展名纳入图片与字体（此前改一张图不触发重建 → 陈旧产物）
- [x] **`publicPath` 贯穿 runtime chunk 加载**：entry chunk 在 prelude 后注入 `__wake__.publicPath = "<配置值>"`（JS 字符串转义走 `push_js_string`），运行时 `loadFile` 据此拼 `script.src`；写在 prelude 之后而非其对象字面量里，因 registry 可能已由同 token 的先前加载建好。node 加载分支（`W.nreq` + `__dirname`）不受影响。单测 `public_path_injected_into_chunk_loader`/`public_path_defaults_and_escapes` + vm 沙箱浏览器形态 e2e `public_path_chunk_url_loads_in_browser_like_env`。

**切片 3（✅ 完成）——dev server 网络层：**
- [x] **proxy**（context/target/pathRewrite(正则)/changeOrigin）：`ServeOptions`+`ProxyRule`；[wake_dev_server](../crates/wake_dev_server/src/lib.rs) 用 `awc` 转发，默认服务先试代理（任意方法）再回退 SPA；`CompiledProxy`（正则预编译）
- [x] **host**（`0.0.0.0` 局域网）/ **open**（跨平台开浏览器）；端口配置 `devServer.port` 优先
- [x] CLI `cmd_dev` 从 `config.dev_server` 装配 host/open/proxy/port
- **切片 3 验收通过**：live 烟测——`/api/ping.txt` 转发+pathRewrite 去前缀得后端响应、`/` 回退 SPA、后端 404 透传；`CompiledProxy` matches/rewrite 单测绿；fmt/clippy/全量测试全绿。
- ⏭ 待做：**https**（需 rustls+rcgen TLS，配了 `server="https"` 现告警回退 http）、**ws 代理**（配了 `ws=true` 现告警仅转 HTTP）。

> 📌 此处原有一份**重复的「切片 2」**（"bundler emit 手术"版）已删除：它与上面那份记述的是同一批工作，
> 但内容已过时且与实现相反——写着「driver 聚合（**模块 id 升序**）」而实现早已改为依赖后序，
> 「⏭ 待做：`url()` 资源改写」也已完成。旧快照留在文档里比没有更糟，会让后来者按错误前提动手。
> 其独有信息（带外产物基建 `BuildOutput.assets`、`LoadOptions` 三字段、`publicPath` 贯穿 chunk 加载）
> 已并入上面的条目。

### M3 小结（核心三切片完成）
- 切片 1 define/`.raw`/clean · 切片 2 asset 阈值/CSS 抽取/url() 改写/`publicPath` 贯穿 · 切片 3 dev proxy/host/open。
- **遗留（非阻塞）**：dev server **https**（rustls+rcgen）、**ws 代理**；`buffer`/`string_decoder` polyfill。

### M4 — prod 压缩 + sourcemap · XL（分阶段，按安全性排序）
> 设计经多-agent workflow 撑出（codegen 发射机制 + 语义模型 + 压缩器设计）：压缩器**作为 codegen 发射期决策**渐进落地（mangling 除外走独立 crate），每 pass 往返验证后再进下一 pass。
- [x] **M4a 紧凑输出**（✅）：codegen `minify` 字段，`newline()` 在 minify 下空操作（唯一换行/缩进来源；语句均发显式 `;`/`}` → **ASI 安全**）。`codegen_module_shaken_with` 加 `minify` 参数；bundler `enable_minify` + `MINIFY_SALT` 混入缓存键；CLI prod 启用。验收：react-ts-app **1.58→1.36 MB（-14%）**、往返 `wake parse` 成功、node 跑非 DOM fixture 语义正确（`5 - -3`=8 等）；codegen 61 + bundler 28 单测绿。
- [x] **M4b define 驱动死分支消除**（✅）：`ConstVal` + `const_eval`/`const_eval_bool`（字面量/define 成员/`!`/严格 `===`·`!==`/`&&`·`||` 短路，纯节点才求值 → 有副作用 test 自然拒绝）；`Statement::If` 在 minify 下 decide-then-skip 折叠（不改 AST、Span 保持）；**被丢弃分支含 `var`/函数提升声明则保守不折叠**（`has_hoisted_decl` 守卫，避免 ReferenceError）。验收：3 单测（dev 块剥离 / var 守卫 / dev 不折叠）；fixture node 运行 `result=42`（`if false` dev 块剥离、`if true` 折叠为 consequent）；react-ts-app **`if(false)` 残留 0、`process.env.NODE_ENV` 残留 0**（模块内死分支全部折叠）。
  - ⏭ 同类 pass：**四项里三项已完成**（不可达码、空语句、`?:` 折叠 —— 见 M6「死分支裁剪」），**仅 `debugger` 清理未做**。
  - ⚠️ **`has_hoisted_decl` 守卫被常量传播架空**（2026-07-27 实证）：守卫本身有效（分支不折叠），
    但死分支内 `var x = <字面量>` 的初值会被传播到分支外的使用点，产出**错误运行时值**。
    非 define 专属——任意条件块（如 `if (globalThis.__NEVER__)`）内的 `var v = 字面量` 都会被无条件传播。
- [x] **死模块消除（DME）**（✅ M4b 的兑现）：codegen DCE 剥离死 `require` 后，emit 前从 entry 按各模块体中**存活**的 `__wake_require__(N)` / `.import(cid,N)` 边重算可达（`live_modules`/`extract_referenced_ids`），丢弃不可达模块。边提取覆盖所有模块引用形式（静态/内联动态/split 动态/require）→ 无假阴性；误判只「多留」不「错删」。bundler `enable_dead_module_elimination`；CLI prod 启用。
  - **成果**：**react-ts-app 1.36 MB → 600 KB（-56%，19→14 模块）**，dev react 警告全消；**引用完整性验证：14 注册/14 引用/0 悬空**（无活模块被错删）；DME fixture node 运行 `greet=PROD`（dev 模块丢弃、prod 保留）；`dead_module_elimination_drops_unreachable` 单测绿。
  - 📊 **M4a+M4b+DME 累计：react-ts-app 1.58 MB → 600 KB（-62%）**。
- [x] **M4c CSS 压缩**（✅ `wake_css::minify`）：UTF-8 安全 push-slice 扫描——折叠空白为单空格、去注释、删 `{}`/`;`/`,` 相邻空白、删 `}` 前 `;`；字符串内原样。**安全子集刻意不删**：后代组合器空白（`.a .b`）、`calc(1px + 2px)` 值内空白、`>`/`+`/`~` 组合器与 `prop: value` 冒号后空白（删了会破坏语义）。bundler CSS 聚合处 prod 调用。验收：5 单测（含 descendant/calc/注释/字符串/UTF-8）；fixture 输出 `.nav .item{color: red;width: calc(100% - 20px);margin: 0}.a > .b{padding: 8px}`——压缩且语义完整。
- [x] **标识符 mangling**（独立 `wake_ecma_minify`）—— 此前误标为未完成。实际**早已落地且是 `wake build` 的默认行为**
  （`wake_cli/src/main.rs`：`enable_minify()` 后无条件 `enable_mangle()`，仅 `WAKE_NO_MANGLE=1` 可关），产物局部量全为单字母。
  - ⚠️ **但有两个会让整包加载即死的缺陷**（2026-07-27 实证）：① `export { local as name }` 形式的导出赋值
    **不跟随 mangle 重命名**（`$["renamed"] = renamedLocal` 里仍是旧名）→ ReferenceError；`export const/function/class`
    内联形式正常。② 用户源码里名为 `m`/`$`/`_r` 的**导入绑定**与单 bundle 包装器形参 `function(m,$,_r)` 撞名
    → 重复声明 SyntaxError（导入绑定不参与 mangle，故躲不掉）。
- [x] **M4d codegen sourcemap**（VLQ）+ dev devtool（✅ **dev/非压缩路径**）：
  - 新增 [wake_ecma_codegen/src/sourcemap.rs](../crates/wake_ecma_codegen/src/sourcemap.rs)：Base64 VLQ 编码 + Source Map V3 序列化。源侧只存**字节偏移**，行列换算推迟到序列化（DESIGN §4.1 热路径不算行列）。
  - `wake_common::SourceFile::location0_utf16`：0 基行 + **UTF-16 列**（sourcemap 规范口径，BMP 外字符占 2 码元）。
  - codegen 位置追踪：`push`/`push_name`/`newline`/`sp` 累计产物行列，直写点由 `sync_from` 兜底；`mark()` 在**语句**与**标识符**发射点记录映射。同一产物位置后写覆盖前写（被擦除的 `import` 不留下陈旧映射）。
  - **零破坏 API**：新增 `codegen_module_shaken_with_map`（返回 `(String, ModuleMappings)`），既有 `codegen_module_shaken_*` 签名与产物**逐字节不变**（快照测试全绿）。
  - bundler：`enable_sourcemap()`；`emit` 非 minify 分支回填「模块 id → 体首行」，`merge_bundle_map` 按 (行 +offset, 列 +2) 平移合并为整包 map；`OutputChunk.source_map`。源路径去 Windows `\\?\` 前缀 + 相对 cwd + 正斜杠（对齐 esbuild/Vite）。
  - 接线：`wake dev` 默认开（`/bundle.js.map` 路由 + `sourceMappingURL`）；`wake build --sourcemap`（与 minify/mangle/分割互斥并告警）。
  - **验收**：官方 `source-map` 库（DevTools 同款算法）跨 3 个源文件 **9/9 精确命中**；独立 Node VLQ 解码器校验 25 条映射 0 错位 0 越界；单测 codegen 9 + bundler 4；全量 576 测试绿、fmt/clippy 干净。
  - ⏭ **未覆盖**：minify 路径（scope-hoist + `strip_hoisted_requires_and_barrels` 改写模块体 → 行偏移法失效，需逐 token 平移 + `names` 字段）、代码分割多 chunk、产物磁盘缓存不存映射（开 sourcemap 时按未命中重算）。
- **验收**：prod bundle 体积与 esbuild `--minify` 同量级；DevTools 行号正确。

### M5 — 与 crustify 完全对齐的高级转换 · XL（全做，按难度递增）
- [ ] auto-import-style（M）：组件自动注入同名样式 import
- [ ] MDX（L）：`.md/.mdx` → JS（remark-gfm 子集 + frontmatter）；`wake_ecma_transform` 落地
- [x] **linaria / 零运行时 CSS-in-JS**（✅ `css` 标签，新增 [wake_css_in_js](../crates/wake_css_in_js/src/lib.rs)）
  - **范围由真实用量决定**（crab-dev 实测：442 处 `@linaria/core` import、1454 个 `css` 模板、**0 处 `styled`**、**0 处动态插值**、1529 处成员访问插值）→ 只做 `css` 标签 + 静态插值，不做 `styled`（需 React 运行时）与 CSS 变量动态插值。
  - **静态求值器**（[value.rs](../crates/wake_css_in_js/src/value.rs)）：Linaria 用 Node VM 真实执行模块，wake 无 JS 运行时 → 改为对**纯数据子集**递归求值（字面量/模板/对象/数组/标识符/成员访问，含 `a['b-c']` 括号键）。**跨模块**：先收各模块「静态导出常量」（`collect_static_exports`），再按 import 装配求值作用域——覆盖 design token 模式 `import token from './token.js'` + `${token.a.b}`。刻意只做一层，不递归被引用模块的 import（避免环与指数展开）。
  - **CSS 嵌套编译器**（[nesting.rs](../crates/wake_css_in_js/src/nesting.rs)）：`&:hover`→`.cls:hover`、裸选择器→后代、逗号分组逐项展开、`@media`/`@supports` 保留外壳递归、`@keyframes` 原样提升（其内非选择器上下文）。字符串/注释内的 `;{}` 不误判。
  - **类名**：`变量名_hash8`（hash 混入模块路径+序号+内容）→ 可读、跨模块不撞、同输入稳定。
  - **降级策略**：插值无法求值 → 发警告 + **丢弃该条声明**（其余保留），产物始终是合法 CSS；不中断构建、不产出非法 `${...}` 残留。
  - **管线接入**：`MinifyCtx` 构造从 `if mangle` 提出（原先只有 prod 才活），CSS-in-JS 无条件填 `expression_replacements`；`CodegenBody` 加 `css`/`diagnostics`；prod 汇入既有 `styles.<hash>.css` 聚合 + `<link>` 注入，dev 随模块体 `<style>` 注入。启用时绕过产物磁盘缓存（缓存只存 body，命中会丢样式）。
  - **零开销默认开启**：扫描后若全项目无人 import `@linaria/core` 则整体跳过（`cij_active`），故 CLI/dev 均默认启用。
  - **验收**：对 **crab-dev 真实 `token.ts`** 构建 → `${token.container.padding}` 求值出完整嵌套 `var(--empty-container-padding,var(--token-semantic-space-dialog-padding,var(--token-global-space-6,24px)))`；`&:hover`/`@media` 正确展开；bundle 内 `css\`` 残留 0、导出为纯类名字符串且与 CSS 规则一一对应（node 验证）。单测 wake_css_in_js 30 + wake_bundler 5；全量 611 测试绿、新 crate clippy 0 警告。
  - **完整对齐 `@linaria/core`（二次迭代）**：读真实包（v8.1.0）+ `@wyw-in-js/processor-utils` 源码定标，补齐 templateProcessor / toCSS 的全部插值语义：
    - **`css` 不支持动态插值是设计如此**——`CssProcessor.addInterpolation()` 直接 `throw`，类型签名的插值联合为 `string | number | CSSProperties | WYWEvalMeta`（无函数）。动态插值只存在于 `styled`（属 `@linaria/react`）。故本包"完整实现"= **不**做动态插值。
    - **css 互相引用**：类名改为**只由「模块路径 + 序号」决定、不含内容 hash**（对齐 Linaria slug），从而可在求值前确定，打破「求值需类名 / 类名需求值」的循环。`${X}` 求值得**裸类名**（与 Linaria 一致，当选择器须写 `.${X}`）。同模块与**跨模块**（类名并入静态导出）均支持。
    - **CSSProperties 对象/数组插值**：`toCSS` 语义——驼峰转连字符（`--x` 原样、`ms` → `-ms-`）、数字补 `px`（0 与 `unitless` 表内属性除外，含厂商前缀剥离）、falsy 值丢弃但数字 0 保留、值为对象时键当选择器、数组以换行连接。
    - **`undefined` / `""` 插值静默跳过**（不报警）；插值结果经 `stripLines` 折行为空格。
    - `cx` 无需编译支持（纯运行时，由真实包提供，含 `atm_` 原子类去重）。
  - **连带修复两个既有打包器缺陷**（由本功能暴露，与 CSS-in-JS 无关但影响所有 CJS 包）：
    1. `compact_body_names` 把 `module.exports` 改写成 `m.$`——`$` 是 exports 的**值**、`m` 才是 module，赋值落到无关属性上 → 任何 `module.exports = X` 的 CJS 包导出恒为空。改为 `m.exports`（用占位符隔离，避免被后续 `exports`→`$` 改回）。
    2. scope-hoist 的 concat 让被合并模块共享 `$`，而 `module.exports = X` 会**整体替换**导出对象 → 与其它模块写入的 `$` 分属两处必丢其一。改为把这类模块**排除出 concat、保留为独立注册模块**（`reassigns_module_exports`），并让 concat 的 `_r` 垫片对未合并 id 转发真实 require；同时把 minify 下的简化 interop 改为按「是否有 `default` 键」判定（`m.default` 对纯 CJS 恒为 undefined）。
  - **验收**：真实 crab-dev `token.ts` + 对象插值 + 跨模块 `.${icon}` + `cx` 组合构建 → CSS 与类名全部正确、node 运行 `cx` 输出两个类名；react-ts-app 产物 11 模块 214.1 KB，模块 1 `useState` / 模块 2 `createRoot` 均为可调用函数（验证 concat 改动未伤 React）。单测 wake_css_in_js 41 + wake_bundler 8；全量 625 绿、clippy 干净。
  - **第三次迭代（keyframes / `:global()` / 多层传播）**，语义对齐 `@wyw-in-js/transform` 的 stylis 中间件：
    - **`@keyframes` 名作用域化**：加 `-<父选择器去非字母数字>` 后缀（对齐 Linaria `elementToKeyframeSuffix`），并同步改写 `animation` / `animation-name`（含厂商前缀）中的引用。**只改写本块内已定义**的关键帧名——未定义的可能来自全局，乱改会断链；`@keyframes :global(name)` 与值里的 `:global(name)` 均不加后缀。
    - **`:global()` 逃逸**：真实用法 100% 是空括号块 `:global() { html, body { … } }`（crab-dev 实测 32 处全是此形态）→ 块内以**空父选择器**递归展开，内容不带类前缀；`:global(sel)` 形式取 `sel` 原样且不加前缀。全局作用域下的裸声明无选择器可挂，直接丢弃（写出来是非法 CSS）。
    - **跨模块多层常量传播**：`resolve_css_in_js_scopes` 改为按**依赖后序**推进——处理某模块前其依赖的导出已算好，于是导出常量可引用依赖的常量再供下游引用，`a → b → c` 链完整传播（原实现只做一层）。成环时该边跳过，环内模块用「当时已算出的部分」求值：可能少解出几个值，但绝不产出错误值。
    - **数字算术与字符串拼接**：补 `+ - * / % **` 与 JS `+` 拼接语义——`${UNIT * 2}px` 是 design token 的自然写法，缺了它多层传播基本用不上（一个属性求值失败会毒化整个对象）。比较/位运算/条件表达式仍拒绝求值（保持「不猜语义」边界）。
    - **验收**：三特性叠加真实构建 → `${space.sm}`→`8px`（穿 3 模块 + 算术）、`@keyframes fade-<cls>` 与 `animation` 引用一致、`html,body{margin: 0}` 无类前缀、crab-dev token 三层 `var()` 链不变；CSS 花括号平衡、类名与规则一一对应。单测 wake_css_in_js 51 + wake_bundler 10；全量 637 绿、clippy 干净。
  - ⏭ 未做：`styled` 组件工厂与动态插值（属 `@linaria/react`，非本包）、`&:global()` 的 Stylis v3 token 重排怪癖（真实项目零使用）、`:global` 的 DCE 保留规则（wake 从不丢弃 css 块，无需）。
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

### M6 — 对比表 ⚠️ 项收口（namespace / 装饰器 / JSX / 死分支）✅
四项均以 **tsc 参考产物**定标（`fixtures/react-ts-app/node_modules/.bin/tsc`，v7.0.2），而非凭记忆实现。

- **namespace / module → IIFE**：点分名 `A.B.C` 展开为**嵌套 IIFE**（成员挂最内层段）；实参改用 `X || (X = {})` 从而支持**跨声明合并**（enum 同步启用）；嵌套 `export namespace` 递归降级。
  - ⚠️ 踩坑记录：最初各嵌套层共用同一 `span`，而压缩器侧表以 span 为键 → 对某层的「未用绑定消除」连带命中其它层，把整个嵌套削平（非压缩产物正确、压缩产物静默错误）。改为每层取各自段的 span。
- **装饰器 → TC39 Stage-3**：`__esDecorate` + `__runInitializers` 运行时 + 每类 IIFE 包装 + `static{}` 编排。支持类/方法/静态方法/取值器/设值器/字段/静态字段装饰器、多装饰器倒序应用、装饰器返回值替换、`addInitializer`、继承、显式构造函数、字段 extraInitializers 串联。
  - `context.name` 用**源码原名**字面量而非 tsc 的 `_classThis.name`——后者在 mangler 重命名类绑定后会读到压缩名。
  - 新增 AST 字段必须同步 `visit.rs` **与** `semantic.rs` 两处遍历：漏掉会让 mangler 看不到装饰器引用（改名后 ReferenceError）、tree-shaking 误删装饰器函数。
  - ⏭ 未做：`accessor` auto-accessor 的装饰（含之则整类放弃转换、原样发射）；参数装饰器（Stage-3 本就没有）。
- **JSX**：命名空间名 `a:b` → 字符串类型；**dev runtime**（`jsxDEV` + `{fileName,lineNumber,columnNumber}` + `this`）；`jsxImportSource` 可配。dev server 已接线。
  - JSX 口径改变依赖说明符（`jsx-runtime` ↔ `jsx-dev-runtime`），已混入 `content_key`——否则 prod 缓存的模块摘要会在 dev 构建里带错依赖。
  - classic runtime / `@jsx` pragma **不在 crustify 对齐范围**（crustify 显式配 `runtime: "automatic"`）。
- **死分支裁剪**：**解除与 minify 的耦合**（折叠只依赖「条件可在构建期定为常量」，dev 同样受益）；扩展到常量三元 `?:` 与**不可达代码**（`return`/`throw`/`break`/`continue` 之后），后者复用 `has_hoisted_decl` 守卫。
- **验收**：装饰器三组用例与 tsc 产物在 node 中**逐字节同结果**（含 minify+mangle 路径），已固化为 `stage3_decorators_match_tsc_behavior`；namespace 端到端回归测试；react-ts-app 11 模块 214.3 KB、`useState`/`createRoot` 均正常。全量 653 绿、clippy 干净。

### M7 — 矩阵复核后的收口（root_dir 接线 / Sass·Less 明确拒绝）✅
两项均源自「可舍弃项里藏的真 bug」——舍弃的是**功能**，不是**正确性**。

- **`root_dir` 接上**：此前是死字段（`resolved_root()` 有实现但全代码库无调用方，`prepare_project` 恒用 `find_root` 结果）。现拆为 `config_dir`（配置文件所在目录，向上探测）与 `root`（= `config.resolved_root(config_dir)`），此后**一切基准都用 `root`**：别名 `@`→root/src、`@@`→root、组件扫描 cwd、`.wake/`、虚拟入口、HTML 模板。相对路径按配置目录解析、绝对路径原样取用；指向不存在目录时告警并回退到 `config_dir`。
  - 验收：fixture `root_dir = "app"` + `@/lib.js` → 解析到 `app/src/lib.ts` 并跑出正确值；**去掉 `root_dir` 后同一 fixture 构建失败**（证明确由该字段生效，非巧合）。单测 4 条（含「别名基准随 root_dir 平移」）。
- **`.scss`/`.sass`/`.less` 明确拒绝**：此前被并入 `is_css_path` 当普通 CSS **原样透传**——嵌套/变量/mixin/`@use` 直接落进产物，形成非法 CSS 或静默错误样式。**「看似构建成功」比构建失败更危险**。现 `is_css_path` 收窄为仅 `.css`，新增 `is_css_preprocessor_path`，在 `load_source` 以 `ErrorKind::Unsupported` 早退；bundler 侧区分该 kind 发 `WAKE0302`「不支持的文件类型」并附可操作提示（改用 `.css` 或先用 sass/less CLI 预编译）。**不实现预处理器**。
  - 连带修复：`print_diagnostics` 此前**只打印 message、从不打印 notes**——等于最有用的「怎么办」那半条信息一直不可见（CSS-in-JS 求值失败警告的「支持哪些表达式」提示同样隐形）。现统一渲染尾注。
- 全量 659 绿、clippy 干净。

### M8 — dev server 多 chunk / 静态资源服务 ✅（矩阵中唯一的阻断性缺口）
- **实测先纠正了一处判断**：此前记为「组件扫描+懒加载在 dev 跑不通」并不准确。dev 从未开启代码分割，动态 `import()` 被内联进单包——**功能可用，只是不真正懒加载**。真正坏掉的是下面两点。
- **`public/` 静态资源完全不可用**：任何未知 GET 都回退 SPA HTML，于是 `/note.txt`、`/logo.png` 一律拿到 `200 + text/html`——浏览器把 HTML 当图片渲染为空白、当 JS 执行则语法错误，且**看不出是 404**。
- **修复**：
  - dev 开启 `enable_code_splitting()`，与 prod 产物结构一致（避免 dev 掩盖分割相关问题）。
  - `BundleState` 增 `chunks`（文件名→源码）与 `assets`（文件名→字节），`rebuild` 每次写入。
  - `serve_default` 改为按序：代理 → **chunk** → **资源产物** → **`public/` 文件** → SPA 回退。
  - **SPA 回退仅对无扩展名路径**（前端路由）；形似文件的路径未命中即 **404**（对齐 webpack-dev-server 的 `disableDotRule: false`）。
  - `public/` 读取做**目录穿越防护**：规范化后必须仍在 `public/` 之内。
- **验收**：组件扫描 fixture（2 个 `.page.tsx`）→ dev 下入口不再内联页面、bundle 声明的 2 个 async chunk 均以 `application/javascript` 200 返回；`public/note.txt` 返回文本原文；`/nope.png` 404；`/users/1` 仍回退 HTML。真实 react-ts-app dev：17 模块 183ms，bundle/`.map` 均正常。单测 3 条（回退规则 / MIME / public 读取+穿越拒绝）。全量 662 绿、clippy 干净。
