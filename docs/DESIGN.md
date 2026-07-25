# Wake — 高性能 Rust Web 构建器设计文档

> 版本：v0.3（2026-07）
> 状态：设计阶段
> v0.2 变更：将增量模型从「模块级」升级为「函数级增量计算引擎 wake_turbo」（Turbopack 式），并完成并发模型选型（Actor vs 工作窃取 vs 记忆化任务图），见第 10 章。
> v0.3 变更：合入性能审计（docs/PERF-AUDIT.md）结论——新增「工作规避」设计原则与 10.7 节（恒等旁路/span 补丁、惰性 SourceMap、依赖预扫描、全局跨项目缓存、daemon 模式）、显式 SIMD、PGO/BOLT、HMR 热路径预算表；堵住 Atom 持久化正确性坑；依赖策略改为白名单制。
> 定位：用 Rust 编写的下一代 Web 构建器，覆盖 webpack 的核心场景（打包、转换、Dev Server、HMR、代码分割），JS/TS 编译器（类 SWC 部分）**完全自研**，不依赖 swc / oxc。

---

## 目录

1. [愿景与目标](#1-愿景与目标)
2. [生态调研与差异化定位](#2-生态调研与差异化定位)
3. [总体架构](#3-总体架构)
4. [自研 ECMAScript 编译器（wake_ecma）](#4-自研-ecmascript-编译器wake_ecma)
5. [模块解析与模块图（Resolver & Module Graph）](#5-模块解析与模块图)
6. [打包与产物生成（Bundler）](#6-打包与产物生成)
7. [Dev Server 与 HMR](#7-dev-server-与-hmr)
8. [CSS 与静态资源](#8-css-与静态资源)
9. [插件系统](#9-插件系统)
10. [增量计算引擎与并发模型（wake_turbo）](#10-增量计算引擎与并发模型wake_turbo)
11. [测试与质量保障](#11-测试与质量保障)
12. [循序渐进的实施路线图](#12-循序渐进的实施路线图)
13. [风险与权衡](#13-风险与权衡)
14. [附录](#14-附录)

---

## 1. 愿景与目标

### 1.1 一句话定位

**Wake = webpack 的能力边界 + esbuild 级别的性能 + 完全自主可控的编译器内核。**

### 1.2 目标（Goals）

| 维度 | 目标 |
|------|------|
| 生产构建 | 10,000 模块规模项目冷构建 **< 5s**（不含 minify），单核性能对齐 esbuild 同量级 |
| Dev 冷启动 | 中型项目（~1,000 模块）**< 1s** |
| HMR | 单文件修改到浏览器更新 **p95 < 25ms、理想 < 10ms**（不含浏览器执行时间；函数级增量下 50ms 是旧口径） |
| 增量构建 | 二次构建命中持久化缓存 **< 500ms**；daemon 模式（10.7.5）下 **毫秒级** |
| 语言支持 | ES2015+ / ESM / CJS 互操作 / TypeScript（类型擦除）/ JSX |
| 产物 | ESM & IIFE 输出、代码分割、动态 import、Tree Shaking、SourceMap |
| 开发体验 | Dev Server、文件监听、增量编译、HMR（含 React Fast Refresh 协议对接） |
| 可扩展 | Rust 原生插件系统（先行），JS 插件桥接（远期） |

### 1.3 非目标（Non-Goals，至少 1.0 之前不做）

- **不做完整 TS 类型检查**——只做类型擦除，类型检查交给 `tsc --noEmit` / IDE（与 esbuild/swc 相同策略）。
- **不追求 webpack 插件生态兼容**——rspack 已证明这条路工程量巨大，Wake 定义自己的插件 API。
- **不做 ES5 全量降级**——目标浏览器基线为 ES2018+，语法降级只做少量高价值转换（可选、后置）。
- **Minifier 自研放到最后**——压缩器是独立的大工程，先预留接口，1.0 前可先不压缩或对接外部工具。

### 1.4 设计原则

1. **性能是架构问题，不是优化问题**：并行、增量、缓存必须在第一天的架构里，而不是后期补丁。
2. **一切皆增量**：整个构建被建模为「输入指纹 → 产物」的纯函数集合，任何阶段都可缓存、可失效、可并行。
3. **AST 只建一次**：解析、转换、依赖提取、代码生成共享同一棵 AST，杜绝多次 parse（webpack + babel 时代最大的性能浪费）。
4. **循序渐进，每个阶段都可运行、可验收**：先做窄而深的垂直切片（能真正打包一个 React 应用），再横向扩展能力。
5. **最快的工作是不做的工作（work avoidance）**：在「把活干得更快」（并行/SIMD/零拷贝）之上，优先设计「压根不干活」的路径——无转换模块走恒等旁路不进 codegen、SourceMap 惰性到被请求才生成、同版本依赖全机只编译一次、daemon 让任务图热驻内存。详见 10.7。

---

## 2. 生态调研与差异化定位

### 2.1 现有格局（2026）

| 工具 | 语言 | 编译器 | 特点 | 对 Wake 的启示 |
|------|------|--------|------|----------------|
| webpack | JS | babel/tsc 外挂 | 生态最全，性能天花板低 | 学习其能力模型（loader/plugin/chunk），不学实现 |
| esbuild | Go | 自研 | 单遍架构，极致性能标杆 | 学习其「lexer/parser/link 三阶段 + 全并行」设计 |
| Rspack | Rust | 依赖 SWC | webpack 兼容 + Rust 性能 | 证明 Rust 做 bundler 可行；但编译器不自主 |
| Rolldown（Vite 底层） | Rust | 依赖 Oxc | Rollup 兼容 API | 学习其 chunk 设计与 Vite 集成方式 |
| Turbopack | Rust | 依赖 SWC | 增量计算框架（Turbo engine）| **函数级增量思想被 Wake 采纳**，自研简化实现（wake_turbo，第 10 章），规避其历史包袱 |
| SWC / Oxc | Rust | 自研 | 编译器基础设施 | **自研部分的直接参照物**：arena AST、字节级 lexer、Pratt parser |

（生态现状参考：[JavaScript Bundlers in 2026](https://dev.to/thedailyagent/javascript-bundlers-in-2026-vite-rspack-turbopack-and-the-end-of-an-era-16hk)、[Rolldown vs Rspack vs Turbopack 2026](https://www.pkgpulse.com/guides/rolldown-vs-rspack-vs-turbopack-2026)、[Vite 6 (Rolldown) vs. Turbopack](https://medium.com/better-dev-nextjs-react/vite-6-rolldown-vs-turbopack-the-2026-truth-about-the-bundler-wars-f1f86e936d7e)）

### 2.2 Wake 的差异化

现有 Rust bundler（Rspack/Rolldown/Turbopack）全部**把编译器外包**给 SWC 或 Oxc。这带来两个问题：bundler 与编译器的 AST / 内存模型 / 并行模型是两套设计，边界上存在序列化与拷贝开销；且核心能力受制于上游。

Wake 的核心赌注：**编译器与打包器同构设计**——同一个 arena、同一套 span/interner、同一个并行调度器。依赖提取、Tree Shaking 的符号分析直接复用 parser 建立的 scope/symbol 信息，中间不落地、不拷贝。这是 esbuild 快的真正秘密，也是「自研 SWC 这一块」的价值所在。

---

## 3. 总体架构

### 3.1 Cargo Workspace 布局

```
wake/
├── Cargo.toml                 # workspace root
├── crates/
│   ├── wake_common/           # Span、诊断、字符串驻留(Atom)、FxHashMap 等基础设施
│   ├── wake_turbo/            # 函数级增量计算引擎：任务图、失效传播、调度器（第 10 章）
│   ├── wake_turbo_macros/     # #[wake::task] 过程宏
│   ├── wake_ecma_ast/         # AST 定义（arena 分配）
│   ├── wake_ecma_lexer/       # 词法分析器
│   ├── wake_ecma_parser/      # 语法分析器（含 scope/symbol 分析）
│   ├── wake_ecma_transform/   # 转换管线：TS 擦除、JSX、目标降级
│   ├── wake_ecma_codegen/     # 代码生成 + SourceMap
│   ├── wake_resolver/         # 模块解析（Node/exports/tsconfig paths）
│   ├── wake_graph/            # 模块图、chunk 图、Tree Shaking
│   ├── wake_bundler/          # 产物拼装、runtime 注入、代码分割
│   ├── wake_css/              # CSS 解析与打包
│   ├── wake_cache/            # wake_turbo 任务图与产物的持久化层
│   ├── wake_dev_server/       # HTTP 服务、文件监听、HMR
│   └── wake_cli/              # 命令行入口（bin: wake）
└── docs/
    ├── DESIGN.md              # 本文档
    └── PERF-AUDIT.md          # v0.2 性能审计报告（v0.3 结论来源）
```

分 crate 的意义：强制清晰的依赖方向（`ast ← lexer ← parser ← transform/codegen`，`graph` 只依赖 `ast` 的只读视图），编译期并行更好，且未来可单独发布 `wake_ecma_*` 作为独立编译器库。

### 3.2 数据流总览

```
                    ┌────────────────────────────────────────────────┐
                    │                  wake_cli                      │
                    │        wake build / wake dev / wake preview    │
                    └───────────────────────┬────────────────────────┘
                                            │ Config
                                            ▼
   ┌────────────────────────── Compiler（编排器，wake_bundler）─────────────────────────┐
   │                                                                                    │
   │  entry ──► [Resolve] ──► [Load] ──► [Transform] ──► [Parse] ──► [提取依赖] ──┐     │
   │              ▲            (fs读取)   (TS/JSX等)      (共享AST)               │     │
   │              └────────────────────────── 新发现的依赖，并行递归 ◄────────────┘     │
   │                                                                                    │
   │                     ▼ 全部模块就绪（Module Graph 冻结）                            │
   │                                                                                    │
   │  [符号级 Tree Shaking] ──► [Chunk 划分] ──► [并行 Codegen+SourceMap] ──► [写盘]    │
   └────────────────────────────────────────────────────────────────────────────────────┘
                    每一步的输入输出都经过 wake_cache 做内容寻址缓存
```

三大阶段与 esbuild 的经典划分一致：**Scan（发现并编译所有模块，天然全并行）→ Link（跨模块分析：tree shaking / chunk，需要全局视图）→ Emit（按 chunk 并行生成产物）**。

### 3.3 核心抽象

```rust
/// 模块的唯一标识：解析后的绝对路径 + query（如 ?raw）
struct ModuleId(Atom);

/// 一个已编译的模块（Scan 阶段的产物，不可变）
struct Module {
    id: ModuleId,
    module_type: ModuleType,        // Js | Ts | Jsx | Css | Asset | Json
    ast: Option<Program>,            // 共享 AST（arena 中）
    deps: Vec<Dependency>,           // import/export/require/动态import 记录
    symbols: SymbolTable,            // parser 顺带产出的作用域与符号
    side_effects: SideEffects,       // 副作用标记（tree shaking 用）
    source_hash: u64,                // 内容指纹（缓存键）
}

/// 编排器持有的全局状态
struct Compiler {
    options: NormalizedOptions,
    graph: ModuleGraph,              // DashMap<ModuleId, Module>
    plugins: PluginDriver,
    cache: CacheManager,
    fs: Arc<dyn FileSystem>,         // 抽象文件系统（测试可用内存FS）
}
```

关键决策：**Module 一旦进入图即不可变**。增量构建时不修改旧 Module，而是用新 Module 替换并让下游缓存失效——不可变数据 + 内容指纹是整个增量体系能保持简单正确的根基。

更进一步：整条管线（resolve/load/transform/parse/link/emit 中的每个函数）都注册为 **wake_turbo 任务**（第 10 章）。`wake build` 本质上只是执行根任务 `emit_all(config)`，「全量构建」与「增量构建」在代码上是同一条路径——前者是空缓存下的执行，后者是失效传播后的部分重执行。这消灭了「watch 模式专用逻辑」这一整类 bug 来源。

---

## 4. 自研 ECMAScript 编译器（wake_ecma）

这是整个项目技术含量最高、也最需要「循序渐进」的部分。设计参照 SWC / Oxc / esbuild 的公开经验，但从零实现。

### 4.1 基础设施（wake_common）

**Span**：所有位置信息用 `struct Span { lo: u32, hi: u32 }`（8 字节）表示源文件内字节偏移。行列号只在报错时通过预先构建的「换行符偏移表」二分还原，热路径上永不计算行列。

**字符串驻留（Atom）**：标识符、字符串字面量、模块路径全部驻留到全局 interner（分片锁 + FxHashMap），比较退化为 u32 比较，跨线程无拷贝。这是 parser 与 bundler 同构设计的第一块基石——模块图里的符号名与 AST 里的标识符是同一个 Atom。

**诊断系统**：`Diagnostic { level, code, span, message, labels }`，所有阶段统一产出，CLI 端负责渲染成带源码上下文的彩色报错（对标 Rust 编译器的报错体验，这是开发体验的重要卖点）。

### 4.2 AST 设计（wake_ecma_ast）

**核心决策：arena 分配 + 生命周期参数**（Oxc 路线，而非 SWC 的 Box 路线）：

```rust
// 每个模块一个 bump arena，模块编译完成后整块保留（AST 存活期 = 模块存活期）
pub struct Program<'a> {
    pub body: Vec<'a, Statement<'a>>,   // bumpalo::collections::Vec
    pub span: Span,
}

pub enum Expression<'a> {
    Identifier(&'a IdentRef),
    NumberLit(&'a NumberLit),
    Binary(&'a BinaryExpr<'a>),
    Call(&'a CallExpr<'a>),
    Arrow(&'a ArrowExpr<'a>),
    // ... 约 40 个变体，enum 本体控制在 16 字节（tag + 指针）
}
```

理由：JS AST 节点小而多（百万级），arena 让分配变成指针碰撞、释放变成整块 drop，缓存局部性远好于散落堆分配；实测是 Oxc 比 SWC parser 快约 2~3 倍的主要来源之一。代价是生命周期传染，通过「AST 只在编译管线内部流动、跨阶段只传只读引用」的纪律来控制复杂度。

**演进策略**：Phase 1 先只定义表达式/语句的最小子集跑通管线，之后随 parser 逐步补全到 ES2022 + TS + JSX 全量节点。AST 定义用一个内部宏统一生成 `Visit / VisitMut` trait，避免手写 200 个 visitor 方法。

### 4.3 词法分析器（wake_ecma_lexer）

- **字节级扫描**：输入按 `&[u8]` 处理，ASCII 快路径（标识符/数字/空白的首字节查 256 项跳转表），仅在遇到非 ASCII 时进入 UTF-8/Unicode 慢路径（`XID_Start/XID_Continue` 用分层区间表查询）。
- **零拷贝**：token 不携带字符串，只携带 `Span`；需要值时（标识符驻留、字符串转义解码）惰性进行。
- **JS 特有难点的处理**：
  - `/` 的二义性（除号 vs 正则）：lexer 不自作主张，由 parser 按语法上下文调用 `next_token_regex_allowed()` / `next_token_div_allowed()`——这是 esbuild/swc 共同采用的「parser 驱动 lexer 模式」。
  - 模板字符串：lexer 维护模式栈处理 `` `a${b}c` `` 的嵌套。
  - ASI（自动分号插入）：lexer 记录「本 token 前是否有换行」标志位，ASI 判断完全留给 parser。
- **错误恢复**：非法字符产出 `Token::Error` 并继续，保证 dev 模式下一个语法错误不会中断整个构建的诊断输出。

### 4.4 语法分析器（wake_ecma_parser）

- **手写递归下降 + Pratt 表达式解析**（运算符优先级爬升），不用 parser generator——JS 语法的上下文相关性（ASI、cover grammar、`let` 既是关键字又是标识符等）决定了手写是唯一务实选择，业界（V8/esbuild/swc/oxc）无一例外。
- **Cover Grammar**：`(a, b)` 在看到 `=>` 前既可能是括号表达式也可能是箭头函数参数。策略与 esbuild 一致：先按表达式解析，遇到 `=>` 时做「表达式 → 参数模式」的重解释转换，并在重解释时校验合法性。
- **上下文标志**：`in`（for-in 限制）、`yield`/`await`（生成器/异步上下文）、严格模式等用 bitflags 随递归传递。
- **一遍产出语义信息**：parser 内联构建 **作用域树 + 符号表 + 引用记录**（进入函数/块时 push scope，声明时登记 symbol，标识符使用时记录引用待 resolve）。这样 Tree Shaking 与未来的 minifier 不需要再遍历一次 AST——这是「编译器为 bundler 服务」的关键设计。
- **依赖同步提取**：解析到 `import/export/import()/require()` 时直接把 `Dependency { kind, specifier, span, imported_names }` 推入模块的 deps 列表，Scan 阶段无需额外遍历。

### 4.5 转换管线（wake_ecma_transform）

基于 `VisitMut` 的按序 pass 组合，Phase 内只做「必要转换」：

1. **TypeScript 类型擦除**：删 type 注解/`interface`/`type`/类型 only 的 import；`enum` 与 `namespace` 需要真实代码生成（值语义）；`import type` 与「仅用作类型的 import」需借助引用信息判断（`isolatedModules` 语义，与 esbuild 对齐）。**实现形态优先选「span 置空白」**（ts-blank-space / Node.js amaro 路线）：被擦除的类型区域用空格原位覆盖而非删除节点，产物行列与源码完全一致 → SourceMap 退化为恒等映射（近零成本），且天然满足「pass 不改 Span」纪律；仅 enum/namespace 等需要生成代码的场景回退到 AST 改写。
2. **JSX**：转换为 `jsx()/jsxs()`（automatic runtime，默认）或 `React.createElement`（classic，可配），children/key/spread 规则对齐 Babel 语义。
3. **常量折叠 + `process.env.NODE_ENV` / `import.meta.env` 替换**：define 替换后立刻做一次简单的死分支消除（`if (false) {}` 剪枝），这对产物体积与后续 tree shaking 影响巨大，实现成本低。
4. （后置，可选）**语法降级**：可选链/空值合并 → ES2018 等少量高价值转换。

原则：**每个 pass 都是独立、可单测的纯函数**，用 snapshot 测试（insta crate）逐个验收。

### 4.6 代码生成 + SourceMap（wake_ecma_codegen）

- 直接从 AST 写字符串（不经过中间 IR），维护「运算符优先级 + 结合性」表自动决定是否补括号；dev 产物带缩进，prod 产物紧凑（去空白，但不做重命名——那是 minifier 的职责）。
- SourceMap：codegen 过程中对每个有意义的 token 落点记录 `(生成位置, Span)` 映射，最后编码为 VLQ mappings（自研 VLQ 编码，很小）。多级 sourcemap 合并（transform 前后）通过「转换 pass 保持 Span 不变」的纪律来回避——所有 pass 复用原始 Span，一步到位映射回源码，这是 SWC 也在用的取巧但正确的方案。
- **恒等旁路（passthrough）与 span 补丁——codegen 的头号优化**：对「无需任何 AST 级改写」的模块（纯 JS、无 JSX、define 未命中——真实项目中 node_modules 通常占模块数 70%+，绝大多数命中此路径），**跳过整条 AST→字符串生产线**：保留源文本，仅在 parser 标记的 import/export span 处做字符串级替换（magic-string 思路），SourceMap 为恒等或简单偏移。配合 4.5 的 TS span 置空白，dev 模式下大多数模块从「parse + transform + codegen」缩为「parse + 若干 span 补丁」。AST 仍然要建（依赖提取与 tree shaking 需要符号信息），但最长的字符串生产线在多数模块上消失。
- **惰性 SourceMap（dev）**：dev 产物不内联生成 mappings，`//# sourceMappingURL` 指向 dev server 的按需端点，DevTools 实际请求时才由缓存的 span 信息现算；node_modules 模块默认不生成。HMR 热路径上不做任何 VLQ 编码。

---

## 5. 模块解析与模块图

### 5.1 Resolver（wake_resolver）

实现 Node.js 解析算法的现代完整版，这是「能打包真实 npm 项目」的门槛：

- 相对/绝对路径、目录 `index`、扩展名补全（`.tsx → .ts → .jsx → .js → .mjs → .json` 可配置）；
- `package.json`：`exports`（含条件导出 `import/require/browser/default`、子路径模式 `./*`）、`imports`（`#` 别名）、`main/module/browser` 字段优先级；
- `tsconfig.json` 的 `paths` / `baseUrl` 别名；
- symlink 处理（pnpm 场景必须正确）、`node_modules` 逐级向上查找；
- **缓存无处不在**：`(from_dir, specifier, conditions) → ModuleId` 结果缓存 + `package.json` 解析缓存 + 目录存在性缓存，watch 模式下按目录粒度失效；
- **Windows 现实优化**（主力开发环境即 Windows）：NTFS 逐文件 stat 昂贵，用目录级批量 listing 缓存替代；路径规范化（大小写折叠、`\\?\` 前缀、UNC）结果缓存；文档向用户明确建议为项目目录添加 Defender 排除（实测实时扫描可吃掉 30%+ 构建时间）。

### 5.2 模块图构建（Scan 阶段的并行模型）

```
调度器（工作窃取线程池）：
  seen: DashSet<ModuleId>          // 去重，保证每个模块只编译一次
  graph: DashMap<ModuleId, Module>

  task(specifier, importer):
    id = resolve(specifier, importer)         // 带缓存
    if !seen.insert(id) { return }            // 已在处理中
    source = load(id)                          // 文件读取（阻塞IO放独立池）
    module = compile(id, source)               // transform + parse + 提依赖（纯CPU）
    for dep in module.deps { spawn task(dep.specifier, id) }   // 递归扇出
    graph.insert(id, module)
```

要点：**CPU 密集工作（compile）跑在 wake_turbo 的工作窃取执行器上**（10.5 节），文件 IO 用少量专用线程（构建负载下 mmap/顺序读很快，不值得引入完整 async runtime 进编译核心；dev server 的网络层才用 tokio）。每个 compile 任务完全独立、无锁（写入 DashMap 分片锁粒度极小），扇出天然打满所有核。上面伪代码中的 `resolve/load/compile` 各自是独立的 `#[wake::task]`，因此「改一个文件」只失效 `load(该文件)` 及其下游，resolve 结果与兄弟模块的编译全部命中缓存。注意：伪代码里的 `seen` 集合在引擎接入后**删除**——同参任务全局唯一执行（10.3 的任务去重）已覆盖其职责，实现时不做两遍。

**依赖预扫描（流水线化 IO 与 CPU）**：文件读入后先跑一个 es-module-lexer 式的轻量 import 扫描器（只识别顶层 import/export 骨架，约为全量 parse 的 10 倍速度），把子依赖的 resolve 与文件读取**提前扇出**，让子模块 IO 延迟藏在父模块 parse/transform 之下——冷启动关键路径从「串行深度 × (IO+CPU)」逼近「串行深度 × CPU」。预扫描结果仅用于预取，正式依赖列表仍以 parser 为准，正确性不依赖它。

### 5.3 Tree Shaking（符号级，Link 阶段）

基于 parser 已产出的符号表做**跨模块符号可达性分析**：

1. 从入口的具名使用出发，`import { a } from 'x'` 建立 `本模块引用 → x 的导出符号` 边；
2. `export * from` 展开、re-export 链条打通；
3. 副作用判定：模块顶层语句逐条判断是否「可安全省略」（声明类语句安全；调用/赋值默认有副作用；`/*#__PURE__*/` 注释与 `package.json sideEffects: false` 参与判定）；
4. 标记-清除：不可达导出对应的声明语句在 Emit 阶段直接跳过。

CJS 模块不做 shaking（整体保留），与业界一致。第一版可以只做「未使用的 export 移除」，逐步演进到语句级细粒度。

---

## 6. 打包与产物生成

### 6.1 模块包装策略：两步走

**第一步（MVP，webpack 风格函数包装）**：每个模块包成 `function(module, exports, require)`，产物带一个 ~1KB 的迷你 runtime（`__wake_require__` + 模块注册表 + HMR 钩子位）。优点：实现直观、CJS/ESM 互操作简单、HMR 的模块替换天然容易做。

**第二步（优化期，scope hoisting）**：对「纯 ESM、单实例、无循环复杂性」的模块子图做 Rollup 式的作用域提升——把多个模块拼进同一函数作用域，冲突标识符用符号表统一重命名（Atom 使这一步很便宜）。产物更小、运行更快。dev 永远用函数包装（利于 HMR），prod 用 scope hoisting。

### 6.2 CJS/ESM 互操作

- ESM import CJS：生成 `interop` 辅助（`default` 指向 `module.exports`，具名属性尽力而为——与 esbuild 语义对齐并写成兼容性测试集）；
- CJS require ESM：dev 下允许并给 warning；
- `import()`：切分点，返回 Promise 包装的命名空间对象。

### 6.3 Chunk 划分与代码分割

```
ChunkGraph 构建规则（第一版，保持简单可预测）：
  1. 每个 entry → 一个 initial chunk
  2. 每个动态 import() → 一个 async chunk
  3. 被 ≥2 个 chunk 共享的模块 → 提取进共享 chunk（阈值可配）
  4. node_modules 可选整体切 vendor chunk
```

产物文件名带内容 hash（`[name].[hash8].js`），hash 参与依赖传递（chunk A 引 chunk B，B 的 hash 变化会改变 A 中的引用 → A 的 hash 也变化；用两轮 hash 或占位符替换解决环）。

### 6.4 Emit

按 chunk 并行：拼接模块产物（codegen 结果字符串级缓存，未变化模块直接复用上次字符串；恒等旁路模块直接引用源文本切片）+ 注入 runtime + 生成 sourcemap（各模块 map 做偏移合并；dev 走惰性路径）+ 原子写盘（先写临时文件再 rename）。写盘细节：按预估容量一次性 reserve、vectored write 聚合小段、Linux 下可用 O_TMPFILE + linkat；产物已知为合法 UTF-8，全程不做任何重校验。同时产出 `manifest.json`（chunk → 文件映射，供 HTML 注入与 SSR 用）。

---

## 7. Dev Server 与 HMR

### 7.1 形态选择：增量打包式 dev（rspack 路线），而非 unbundled（vite 路线）

理由：unbundled dev 在大项目下请求瀑布与首屏模块数是硬伤（Vite 自己也在用 Rolldown 走回「打包式 dev」）；而 Wake 的编译足够快，增量打包能同时给出「快」与「dev/prod 行为一致」。整个 dev 产物**驻留内存**（不写盘），HTTP 层直接从内存 map 服务。

### 7.2 增量构建模型：一切都是任务重执行

dev 模式没有专用的增量逻辑——它只是 wake_turbo 引擎（第 10 章）的一次次根任务重执行：

```
文件变更（notify 监听，20ms 防抖窗口聚合）
  → fs_content(path) 输入 cell 更新 → 引擎沿任务图传播「可能脏」标记
  → 重新请求根任务 emit_dev(config)
  → 引擎自底向上校验：内容 hash 未变的任务（如只动了 mtime）在 load 层被
    早期截断(early cutoff)，下游一个任务都不会重跑
  → 真正变化的路径上：parse(该模块) 重跑 → 若导出签名未变，link/tree-shaking
    任务同样被早期截断 → 只有该模块的 codegen 与所在 chunk 的拼接重跑
  → diff 新旧 deps：新增依赖自然触发新任务，移除依赖的任务变为不可达等待回收
  → 计算 HMR 更新范围并推送
```

函数级增量的价值在这里最直观：改一个函数体（不改 import/export），失效链是 `load → parse → codegen(本模块) → chunk 拼接`，全程不触碰 resolver、其他模块、tree shaking 全局分析——这是模块级增量做不到的（模块级方案里 link 阶段是整体重跑的）。<50ms 目标由此从「优化目标」变成「架构必然」。

### 7.3 HMR 协议与客户端 runtime

- WebSocket 推送 `{ type: "update", updates: [{ id, acceptedBy, newCode }] }`；
- 客户端 runtime（注入产物的 ~2KB JS）：维护模块注册表，收到更新后**沿依赖图向上找 accept 边界**（`import.meta.hot.accept` 标记的模块）；找到边界 → 重执行新模块代码并触发 accept 回调；找不到 → 整页刷新（保底正确性）；
- API 对齐 Vite 的 `import.meta.hot`（accept/dispose/data/invalidate）——这是事实标准，生态教程可直接复用；
- React Fast Refresh：以内置转换 pass 的形式支持（给组件模块自动注入 refresh 注册代码），Phase 5 的验收标准就是「改 React 组件保留 state 热更新」。

### 7.4 Dev Server 本体

tokio + hyper（或 axum）：静态服务（内存产物 + public 目录）、HTML 注入（script 标签 + HMR client）、SPA fallback、代理（`/api → 后端`）、错误遮罩层（编译错误全屏 overlay，展示 wake_common 的诊断渲染结果）。

---

## 8. CSS 与静态资源

### 8.1 CSS（wake_css）

第一版做「打包必需」的最小集，**不做完整 CSS 引擎**（lightningcss 级别的自研留给远期）：

- 极简 CSS tokenizer/解析：只需识别 `@import`（依赖提取，进模块图统一去重排序）和 `url()`（资源引用改写），其余内容原样透传；
- `import './a.css'`：dev 下转成「注入 <style> 的 JS 模块」（可 HMR：换 style 内容即可，天然无损热更）；prod 下按 chunk 聚合抽取为 `.css` 文件；
- CSS Modules（`.module.css`）：类名 hash 改写 + 导出映射对象——需要选择器级解析，放 Phase 6 后半；
- PostCSS/Sass 等预处理：不内置，通过插件钩子（transform 钩子对 `.scss` 调外部工具）留口。

### 8.2 静态资源

- 图片/字体等：小于阈值（默认 4KB）内联 base64，否则拷贝到产物目录带内容 hash，import 返回 URL 字符串；
- `?raw` / `?url` 查询后缀语义对齐 Vite；
- JSON：解析后作为 ESM 默认导出（支持具名导出 tree shaking 的优化后置）。

---

## 9. 插件系统

### 9.1 Rust 原生插件（Phase 3 起内建，公开 API 于 1.0 冻结）

钩子设计取 Rollup 系（正交、易组合），Wake 内部功能也**吃自己的狗粮**（CSS、asset、define 都实现为内置插件）：

```rust
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    // 解析期
    fn resolve_id(&self, ctx: &Ctx, specifier: &str, importer: Option<&ModuleId>) -> Option<Resolution>;
    fn load(&self, ctx: &Ctx, id: &ModuleId) -> Option<LoadResult>;          // 虚拟模块
    fn transform(&self, ctx: &Ctx, code: Source, id: &ModuleId) -> Option<Source>; // 源码级
    fn transform_ast(&self, ctx: &Ctx, ast: &mut Program) -> ();             // AST 级（高性能路径）
    // 打包期
    fn render_chunk(&self, ctx: &Ctx, chunk: &Chunk, code: Source) -> Option<Source>;
    fn generate_bundle(&self, ctx: &Ctx, bundle: &mut Bundle) -> ();
    // 生命周期
    fn build_start(&self, ctx: &Ctx); fn build_end(&self, ctx: &Ctx);
}
```

### 9.2 JS 插件桥接（远期，1.0 后）

预留方案而不预先实现：进程内嵌 JS runtime（deno_core）或 N-API 反向调用。设计上的准备只有一条：**所有钩子的入参/出参保持可序列化**，不把 arena 引用泄漏进插件 API 边界。

---

## 10. 增量计算引擎与并发模型（wake_turbo）

本章是 v0.2 的核心升级：Wake 的增量粒度从「模块级」提升到 **Turbopack 式的函数级**，并给出并发模型的正式选型结论。wake_turbo 是与 wake_ecma 并列的第二个自研核心。

### 10.1 为什么要函数级增量

模块级增量的天花板：模块是增量的最小单位，意味着 ① Link（tree shaking / chunk 划分）这类全局阶段每次都整体重跑，项目越大 HMR 越慢，10k 模块下仅 Link 就可能吃掉几十 ms；② 「改了文件但语义没变」（改注释、改格式、只改函数体不改导出）无法被识别，浪费级联重算。函数级增量把**每一次函数调用**（`parse(x)`、`export_signature(x)`、`chunk_of(m)`……）都变成可缓存、可精确失效的计算节点，失效只沿真实数据依赖传播，且在「输出没变」处提前截断。这是把 HMR 复杂度从 O(项目规模) 压到 O(变更影响面) 的唯一路线，也是 Turbopack 的核心遗产。

### 10.2 并发/计算模型选型：为什么不是 Actor

评估过的候选模型：

| 模型 | 代表 | 并行能力 | 增量能力 | 判定 |
|------|------|---------|---------|------|
| Actor（消息传递 + 状态隔离） | Erlang/OTP、actix | 好（但邮箱是串行化点） | **无**——记忆化/依赖追踪/失效传播全要自己另建 | ❌ 不用于计算核心；✅ 用于边缘编排 |
| 裸工作窃取池 | rayon | 极好 | 无 | 作为底层执行器，不作为模型 |
| 红绿查询系统 | rustc queries / Salsa | 中（revision 全局串行写，读并行） | **强**：函数级记忆化 + 按需校验 + 早期截断，久经 rust-analyzer 验证 | ✅ 采纳其**失效算法** |
| 全并发任务图 | turbo-tasks | 极好：任务即调度单元，写入并发 | 强，但实现极复杂（聚合树、恢复、GC） | ✅ 采纳其**执行模型**，砍掉过度设计 |
| 时序/微分数据流 | timely/differential-dataflow | 好 | 强（对集合运算） | ❌ 抽象与编译器的树/图形态错配，概念税太高 |

**对 Actor 模型的具体判定**：Actor 解决的是「并发访问可变状态」——通过把状态锁进 actor、用消息串行化访问来换安全。但编译核心在我们的架构里**根本没有共享可变状态**（Module 不可变、AST arena 只读、interner 无锁分片），Actor 的核心卖点在此落空；而它的代价照付：每次细粒度调用变成一次消息投递（入队/唤醒/出队，百 ns 到 µs 级），函数级粒度下每秒百万级调用会被邮箱开销吞噬；且 Actor 天然不提供记忆化与失效语义——用 Actor 实现 Turbopack 式增量，等于在 Actor 之上再造一个任务图引擎，两层抽象叠加。结论：**Actor 只用在天然是「串行状态机」的边缘组件**——文件监听聚合、HMR 会话管理、dev server 连接管理（tokio task + mpsc channel 就是轻量 actor，不引框架）。

**最终模型：记忆化并发任务图 = Salsa 的红绿失效算法 × turbo-tasks 的全并发执行 × 自研工作窃取执行器。**这比 Actor「更强大」的确切含义是：它同时给出并行（任务图天然暴露全部并行度）与增量（依赖追踪 + 最小重算），而 Actor 只给出前者的一半。

### 10.3 wake_turbo 引擎设计

**编程模型**——对上层代码，增量几乎是透明的：

```rust
#[wake::task]                       // 过程宏：注册任务函数
fn parse(source: Vc<Source>) -> Vc<ParseResult> { ... }

#[wake::task]
fn export_signature(m: Vc<ParseResult>) -> Vc<ExportSig> { ... }  // 从 ParseResult 提炼稳定摘要
```

- **任务标识**：`TaskId = fx_hash(函数指纹, 参数指纹)`，同参调用全局唯一执行（自动去重，替代 5.2 的手工 seen 集合）；
- **Cell（`Vc<T>`）**：任务输出存放在引擎的分片 slot 表中，任务间只传轻量句柄（u32 索引），读取即自动登记依赖边——依赖追踪靠执行时记录（thread-local 收集器），零手工声明；
- **失效传播（红绿算法，取自 rustc/Salsa）**：输入 cell（文件内容、配置项）变更时，不立即重算，只把直接依赖者标「红」、传递依赖者标「可能脏」；下次有人请求某任务时**自底向上按需校验**：先递归确认其依赖是否真变了，全没变 → 直接漂绿复用，任何一个变了 → 重执行；
- **早期截断（early cutoff，性能命门）**：任务重执行后，输出与缓存值做指纹比较，**相等则下游不失效**。配合刻意设计的「摘要任务」形成防火墙：`parse` 变了但 `export_signature` 没变 → 全部跨模块分析不重跑。管线中要有意识地多设这类窄腰任务（导出签名、依赖列表、chunk 成员集）；
- **动态依赖**：依赖边每次执行时重新记录，天然支持「本次执行走了不同分支/读了不同文件」，无需静态声明依赖图；
- **取消**：每轮构建持 generation token，新变更到来时旧一代在任务边界检查并放弃（连续快速保存不排队、不浪费核）；
- **任务粒度纪律（防止 Turbopack 式开销失控）**：引擎开销预算为每任务 <2µs（slot 查找 + 依赖记录 + 指纹比较）。纪律：单个任务体的工作量应 ≥100µs——lexer+parser 合并为一个 `parse` 任务，禁止 per-语句/per-token 任务；粒度只在「失效防火墙有价值处」加细（如 export_signature 这类摘要任务虽小但值得）。用 tracing 统计任务耗时直方图，持续校准。

**持久化（wake_cache 的新角色）**：任务图的节点元数据（TaskId、依赖边、输出指纹）+ 标记为 `persistent` 的任务输出（rkyv 序列化）落盘 `.wake/cache/`。重启进程 = 加载图骨架 + 输入指纹比对 + 全图「可能脏」校验，未变部分零重算——watch 重启秒起、CI 增量构建都由同一机制覆盖。AST 这类含 arena 引用的中间值**不持久化**（标 `transient`，miss 时重算），只持久化源无关的终端产物（codegen 字符串、sourcemap、解析结果摘要）。

两条持久化硬规则（正确性级别）：

- **进程内句柄禁止落盘**：Atom 的 u32 id、TaskId、slot 索引均在进程重启后失效，`persistent` 输出中出现它们即是 bug——落盘一律字符串化/规范化，反序列化时重新驻留。用类型系统强制：`persistent` 任务的输出类型必须实现 `StableSerialize`（Atom 不实现它）。
- **配置切片化为独立输入 cell**：`define`、`target`、`alias`、`jsx` 各是独立 cell，任务只依赖它实际读过的切片——改一个 alias 不会全量失效。禁止实现时偷懒把整个 config 的 hash 当缓存键。

### 10.4 与 arena AST 的内存模型整合（关键难点）

任务输出要求 `'static`，而 AST 带 arena 生命周期——这是 wake_ecma 与 wake_turbo 两大自研件的正面冲突，解法：

- `parse` 任务的输出是 `ModuleAst`：一个把 `Bump` arena 与指向其内的 `Program<'self>` 封在一起的自引用持有者（内部用一处经过 miri 验证的 unsafe 实现，对外只暴露 `fn with_ast<R>(&self, f: impl FnOnce(&Program) -> R)` 安全借用接口）；
- 引擎按 `Arc<ModuleAst>` 持有该 cell，下游任务借用读取，**arena 生命周期 = cell 生命周期**，模块被替换后旧 arena 随 Arc 归零整块释放；
- 纪律不变：arena 内部引用绝不逃出 `with_ast` 闭包，指纹用 AST 的结构 hash（parse 时顺手计算）而非指针。
- 这是 Phase 0 spike 必须首先验证的两件事之一（另一件是红绿校验的正确性）。

### 10.5 执行器：榨干 CPU 的调度层

- **自研工作窃取执行器**（crossbeam-deque 双端队列）：每核一个 worker、本地 LIFO slot（新派生任务优先本核执行，吃热缓存）、跨核窃取 FIFO（保证扇出广度）；不用 rayon 的原因是需要定制：任务优先级、generation 取消、依赖阻塞时的续体处理；
- **优先级车道**：HMR 关键路径（变更模块 → 推送）> 普通构建任务 > 后台任务（持久化落盘、预热编译），保证交互延迟不被吞吐挤压；
- **IO 隔离**：文件读取走专用小线程池（或 io_uring，Linux 后置优化项），CPU worker 永不阻塞在 IO 上；
- **微架构级细节**：热点计数器缓存行填充避免伪共享；slot 表分片 + 原子操作，热路径无互斥锁；worker 可选绑核（`--pin-threads`）；空闲 worker 指数退避自旋后 park，避免忙等偷电；
- **饱和度可观测**：`wake dev --profile` 输出每核占用时间线 + 任务耗时直方图 + 窃取/失败窃取计数 + **interner 分片竞争计数**（Scan 阶段全线程高频写全局 interner，32 核以上分片锁是可测热点——若指标恶化，切换为线程本地 intern 缓冲 + 批量合并）；目标：Scan 阶段核占用 >90%，HMR 路径无跨核迁移；
- **Arena 复用池**：模块被替换时其 Bump arena 不 drop 而是 reset 后回池，HMR 热路径避免反复 mmap/缺页；
- **空闲时间利用**：dev server 空闲时后台执行持久化写盘、低优先级预热（预 parse 最近变更过的兄弟文件）、以及**任务图整理**（不可达 cell 清理、内存压缩），把「等待用户改代码」的时间也用起来。长跑 dev 的内存有硬预算：1k 模块常驻 < 500MB、10k < 3GB，进 CI 验收。

### 10.6 性能预算

性能预算自顶向下拆解（以 1,000 模块 dev 冷启动 < 1s 为例，8 核机器）：

| 阶段 | 预算 | 手段 |
|------|------|------|
| 配置加载 + 启动 | < 30ms | CLI 二进制直接启动，无 JS runtime |
| Scan（1000 模块 resolve+读+编译） | < 600ms | 全并行：单模块编译 ~2-4ms × 1000 / 8核 |
| Link（tree shaking + chunk） | < 100ms | 符号表复用，图算法 O(E) |
| Emit（内存产物 + sourcemap） | < 150ms | 按 chunk 并行 + 字符串缓存 |
| 余量 | ~120ms | |

增量时代的主口径是**HMR 热路径预算**（改一个函数体，不改 import/export，目标 p95 < 25ms、理想 < 10ms）：

| 环节 | 预算 | 说明 |
|------|------|------|
| 变更聚合防抖 | 0~20ms（可配，激进模式 0） | notify 事件聚合窗口，不计入引擎耗时 |
| load + 内容 hash | < 1ms | 单文件读 + xxh3 |
| parse（单模块） | < 3ms | 典型 <50KB 源文件 |
| 红绿校验 + 早期截断 | < 1ms | export_signature 未变 → link 全线截断 |
| span 补丁 / codegen（单模块） | < 1ms | 恒等旁路命中时接近 0 |
| chunk 拼接（内存） | < 2ms | rope 局部替换，不生成 sourcemap（惰性） |
| WS 推送序列化 | < 1ms | 消息预序列化 |
| **合计（不含防抖）** | **< 10ms** | tracing 断言进 CI |

### 10.7 工作规避（work avoidance）——设计原则 5 的落地清单

排在所有「干得更快」手段之前，因为收益量级最大：

1. **恒等旁路 / span 补丁**（4.6）：无转换模块跳过 codegen，TS 擦除走 span 置空白 → 多数模块只剩 parse + 补丁。
2. **惰性 SourceMap**（4.6）：dev 不生成 mappings，按需端点现算；node_modules 不生成。
3. **依赖预扫描**（5.2）：轻量 import 扫描提前扇出 resolve/IO，藏 IO 于 CPU 之下。
4. **全局跨项目依赖缓存**：node_modules 内不可变包（名 + 版本 + 内容 hash）的任务产物写入**全机级内容寻址存储**（`~/.wake/store/`，pnpm store 思想），项目级 `.wake/cache/` 只存指针。react 同一版本全机只编译一次；新项目/新 clone 的冷启动大部分是热的；CI 可直接挂载 store。缓存键本就内容寻址，实现成本低。
5. **daemon 模式**：`wake daemon` 常驻进程持有热任务图，CLI/编辑器插件经 IPC（unix socket / named pipe）提交构建；零反序列化，增量构建毫秒级起步；watch 与一次性 build 共享同一热图；为 IDE「保存即构建」铺路。版本升级失效由「编译器版本进缓存键」覆盖；daemon 崩溃降级为普通进程模式，不影响正确性。

### 10.8 单机性能手段清单（引擎之外）

1. **并行**：全部并行度由任务图自然暴露（Scan 扇出、Emit 按 chunk）；lexer/parser 单模块内保持单线程（任务粒度并行已够，避免细粒度同步开销）。
2. **SIMD 分层**：第一层 SWAR/memchr（一次比较 8 字节找定界符）；第二层**显式 SIMD**（AVX2/NEON + runtime feature 检测）批扫字符串字面量、注释、空白、标识符连续段（simdjson 的结构字符检测思想），UTF-8 校验用 simdutf8。lexer 吞吐目标从 150MB/s 底线档提至 **400MB/s+ 目标档**（Oxc/simdjson 已示范可达）。
3. **内存**：arena AST（4.2）+ 复用池（10.5）；Atom 驻留（4.1）；`FxHashMap`（非加密 hash）；全局分配器换 `mimalloc`（实测对分配密集型 Rust 程序有 10-20% 整体收益）；AST enum 尺寸用静态断言钉死（`const _: () = assert!(size_of::<Expression>() <= 16)`）。
4. **零拷贝**：源文件读入后不复制（token/AST/诊断全用 Span 指回源文本）；产物拼接用 rope/分段写，不做中间大字符串拼接；持久缓存 rkyv 零拷贝反序列化。
5. **hash 选型**：内存内容指纹/任务参数指纹用 xxh3（或 rapidhash）；持久化缓存键用 blake3（SIMD、可并行、防碰撞等级足够）。
6. **缓存键纪律**：任务参数指纹必须包含源内容 hash、相关配置切片 cell、编译器版本号（升级自动全量失效）——引擎保证机制，人保证键的完备。
7. **编译 Wake 自身（免费的全局 5~15%）**：发布构建启用 `lto = "fat"`、`codegen-units = 1`、`panic = "abort"`、**PGO**（用宏基准项目采样）+ **BOLT** 布局优化；按 `x86-64-v3` / `aarch64` 分发多档二进制。写进 CI 发布流程，Phase 7 落地。
8. **profiling 常态化**：内置 `wake build --profile` 输出 chrome tracing 格式火焰图 + 10.5 的调度器指标；benchmark suite（criterion 微基准 + 真实项目宏基准）进 CI，性能回归超 5% 直接红灯。**Phase 0 就建 benchmark 骨架**——没有测量的性能目标是口号。

### 10.9 反面清单（明确不做的「负优化」）

- 不引入通用 async runtime 进编译核心（wake_turbo 的续体机制是专用的、无 waker/poll 通用税；tokio 只留在 dev server 网络层）；
- 函数级增量 ≠ 无限细分：严守 10.3 的任务粒度纪律，引擎开销占比 >5% 即回退合并任务；
- 不提前做分布式/远程缓存（全局 store 的内容寻址格式天然是未来远程缓存的接口，届时只加传输层）；
- 单文件内并行 parse：同步点开销 > 收益（>10MB 病态单文件再议）；
- GPU / 异构计算：编译负载分支密集、不规则，无收益；
- 自研内存分配器：mimalloc 之上可榨空间 <2%，机会成本极高。

---

## 11. 测试与质量保障

| 层 | 手段 |
|----|------|
| Lexer/Parser | ① 自建用例 + insta snapshot；② **test262-parser-tests** 全量跑（pass/fail/early 三类，目标通过率 >99%）；③ 对拍测试：随机抓 npm top 包源码，AST 与 esprima/acorn 语义对比 |
| Transform | 每个 pass 独立 snapshot 测试；TS/JSX 用例对齐 esbuild/swc 输出语义（不要求字节一致） |
| Bundler | 端到端 fixture 目录（输入项目 → 期望产物结构 + **产物在 node/浏览器中实际执行**断言运行结果） |
| HMR | 集成测试：headless 浏览器（playwright）改文件断言更新 |
| 模糊测试 | cargo-fuzz 打 lexer/parser（不 panic、不 OOM 即通过），长期跑 |
| 性能 | criterion 微基准 + 宏基准项目（三档：100/1k/10k 模块合成项目 + 一个真实开源项目）进 CI |

原则：**每个 Phase 的验收标准里必须包含测试指标**（见路线图），编译器代码没有 snapshot 覆盖不予合入。

---

## 12. 循序渐进的实施路线图

总原则：**每个 Phase 结束时都有一个能跑的东西 + 明确的验收命令**。时间按一个人全职估算，可并行处则标注。

### Phase 0 — 地基（1~2 周）

- workspace 骨架（3.1 的 crate 布局，空实现）；
- `wake_common`：Span、Atom interner、诊断结构 + 终端渲染、FileSystem 抽象；
- CLI 骨架（clap）：`wake build <entry>` 能读文件并打印诊断；
- benchmark / snapshot / CI 骨架；
- **两个高风险 spike（各 ~200 行，先证伪再投入）**：① arena AST + 自引用持有者 + visitor 的生命周期方案（10.4）；② 单线程版红绿失效 + 早期截断的最小正确实现（10.3）。
- **验收**：`cargo test` 全绿；`wake build a.js` 能报出带源码上下文的假错误；两个 spike 各有可运行 demo 与结论记录。

### Phase 1 — 词法分析器（2~3 周）

- 全量 ES2022 token（含模板串、regex 模式、BigInt、私有字段 `#x`、Unicode 标识符）；
- parser 驱动的 regex/div 二义性接口；换行标志位；错误 token 恢复；
- fuzz 跑通不 panic。
- **验收**：`wake tokenize file.js` 输出 token 流；对 test262 语料 lexer 无误报；criterion 基准：**底线 ≥ 150MB/s**（SWAR 层）/ **目标 ≥ 400MB/s**（10.8 显式 SIMD 落地后，Phase 7 回补）单核吞吐——验收用底线值，但基准报告始终双值展示，防止用底线自我满足。

### Phase 2 — 语法分析器 + AST（4~6 周，全项目最重的一段）

- AST 全量节点（ES2022 模块语法）+ visitor 宏；
- 递归下降 + Pratt；cover grammar；ASI；上下文标志；
- 一遍产出 scope/symbol/引用 + 依赖记录（4.4）；
- **验收**：test262-parser-tests 通过率 >95%（此阶段）；能 parse React、lodash-es 源码无错误；`wake parse file.js --ast` 输出 AST JSON；基准：底线 ≥ 80MB/s / 目标 ≥ 150MB/s 单核。

### Phase 2.5 — 增量计算引擎 wake_turbo v1（3~4 周）🎯 引擎里程碑

与 Phase 2 后半可部分并行（引擎不依赖 parser）：

- `#[wake::task]` 宏 + TaskId/Cell/slot 表 + 执行期依赖记录；
- 红绿失效 + 早期截断 + 动态依赖 + generation 取消（先单线程保证正确性）；
- 自研工作窃取执行器（crossbeam-deque），任务图并发执行；
- loom/断言级并发测试 + 「乱序失效风暴」压力测试。
- **验收**：用引擎实现一个玩具计算图（文件字数统计流水线），随机变更 + 随机请求下与全量重算结果永远一致；任务调度开销基准 < 2µs/任务；8 核下玩具负载加速比 > 6x。

### Phase 3 — MVP 打包器（3~4 周）🎯 第一个打包里程碑

- resolver（先做相对路径 + node_modules + main/module 字段），全管线以 `#[wake::task]` 编写；
- Scan 并行模块图 + 函数包装式 bundle + mini runtime + CJS/ESM 基础互操作；
- codegen 第一版（无 sourcemap）。
- **验收**：`wake build src/index.js` 打包「import React 的 hello world」，产物在浏览器里跑起来；打包 1000 模块合成项目 < 1.5s；同一命令跑两遍，第二遍任务缓存命中率 100%（引擎接入的直接验证）。

### Phase 4 — 转换管线 + SourceMap（3~4 周）

- TS 类型擦除（span 置空白路线为主，enum/namespace 走 AST 改写）、JSX（automatic runtime）、define 替换 + 死分支剪枝；
- **恒等旁路 + span 补丁路径**（4.6）落地，node_modules 大面积命中；
- codegen 补 sourcemap（VLQ）+ dev 惰性 sourcemap 端点；resolver 补 exports 字段 + tsconfig paths。
- **验收**：打包一个真实的 Vite 模板级 React+TS 项目；断点调试映射回 .tsx 源码正确；tracing 统计确认 node_modules 模块 ≥90% 走旁路（未进 codegen）。

### Phase 5 — Dev Server + HMR（4~5 周）🎯 第二个里程碑

- 内存增量构建（7.2，即引擎重执行）；notify 监听 + 防抖 + 输入 cell 更新；
- export_signature 等「摘要防火墙」任务落地，验证早期截断真实生效；
- dev server（静态 + HTML 注入 + proxy + 错误 overlay）；
- HMR 协议 + 客户端 runtime + `import.meta.hot`；React Fast Refresh pass。
- 依赖预扫描（5.2）接入，冷启动 IO/CPU 流水线化。
- **验收**：React+TS 项目 `wake dev` 起服务；改组件代码 state 保留热更；HMR 端到端 **p95 < 25ms**（按 10.6 热路径预算逐环节 tracing 断言）；**只改函数体时 tree-shaking/chunk 任务零重跑**；断网/语法错误恢复正常。

### Phase 6 — CSS、资源、代码分割、Tree Shaking（4~6 周）

- CSS 依赖提取 + dev 注入 + prod 抽取；静态资源 + 内联阈值；
- 动态 import 切 chunk + 共享 chunk + 内容 hash 文件名 + manifest；
- 符号级 tree shaking 第一版（未使用 export 移除 + sideEffects 支持）。
- **验收**：多页 + 懒加载路由的项目正确分包；产物体积对比 esbuild 差距 < 15%；CSS HMR 无闪烁。

### Phase 7 — 性能强化与工程化（持续）

- 任务图持久化（10.3：图骨架 + persistent 输出 rkyv 落盘）；**全局跨项目 store**（10.7.4）；**daemon 模式**（10.7.5）；
- scope hoisting；显式 SIMD 层（10.8.2）回补 lexer；arena 复用池；`wake build --profile`（含调度器饱和度视图）；优先级车道与空闲预热；
- 发布工程：PGO + BOLT + fat LTO 构建流水线，多档 CPU 二进制分发（10.8.7）；
- 插件 API 整理公开 + 文档站；错误信息打磨；
- （可选启动）自研 minifier 立项：重命名（复用符号表，成本低、收益大，先做）→ 压缩变换（长期）。
- **验收**：10k 模块冷构建 < 5s、热缓存 < 500ms、daemon 下毫秒级；lexer 达 400MB/s 目标档；长跑 dev 内存预算达标（10.5）；对外发布 0.1。

累计约 8~10 个月全职（相比 v0.1 增加的 1.5~2 个月即 wake_turbo 的成本，换来的是 dev 体验的代际差）。**最短可行路径是 Phase 0→3**（约 3.5~4 个月拿到能打包真实项目、且天然全增量的 MVP），这条主干走通后，4~7 可按兴趣与需求调整顺序。若中途发现引擎复杂度失控，降级预案见第 13 章。

---

## 13. 风险与权衡

| 风险 | 等级 | 缓解 |
|------|------|------|
| JS 语法边角（ASI、cover grammar、regex 二义、label、`let[a]`…）消耗远超预期 | 高 | test262 从 Phase 2 第一天就跑；参照 esbuild 源码的处理注释（其代码即文档）；接受 <100% 通过率渐进爬坡 |
| arena 生命周期在 Rust 中传染，API 设计反复 | 高 | Phase 0 spike ①验证 AST+自引用持有者+visitor 方案（miri 跑通）；纪律：arena 引用绝不跨出 `with_ast` 边界 |
| **wake_turbo 引擎正确性**（失效传播漏标/错标 → 产物陈旧这种最难查的 bug） | **高** | Phase 0 spike ②先单线程证明算法；「与全量重算对拍」做成常驻测试模式（`WAKE_VERIFY=1` 时每次增量构建后偷偷全量重算比对）；loom 并发模型检查 |
| **引擎复杂度失控**（Turbopack 前车之鉴：聚合树/GC/恢复机制越滚越大） | **高** | 明确砍掉的范围写进设计（不做分布式、不做任务图 GC 的第一版、粒度纪律）；**降级预案**：引擎 API 设计成也能以「无增量、纯并行执行」模式运行（等价于 v0.1 的模块级方案），引擎出问题时产品仍可发布 |
| 函数级任务开销吃掉增量收益 | 中 | 每任务 <2µs 开销预算进 CI 基准；粒度纪律（任务体 ≥100µs）；tracing 直方图持续校准 |
| SourceMap 正确性难以肉眼验证 | 中 | 自动化：产物断点位置断言（playwright + CDP）；「pass 不改 Span」纪律从第一天执行 |
| CJS/ESM 互操作语义黑洞 | 中 | 不发明语义，逐条对齐 esbuild 并固化为兼容性测试集 |
| HMR 边界与状态管理复杂 | 中 | 找不到 accept 边界一律整页刷新，先保正确再保体验 |
| 战线过长导致烂尾 | 高 | 严格执行 Phase 制；Phase 3 的 MVP 里程碑前不写任何 Phase 4+ 代码；每 Phase 有可演示物保持动力 |
| 一个人同时自研编译器 + bundler 工作量巨大 | 高 | 非目标清单（1.3）严格执行；minifier/降级/类型检查坚决不提前做 |

## 14. 附录

### 14.1 第三方 crate 选型（核心路径极简依赖）

| 用途 | crate | 说明 |
|------|-------|------|
| arena | bumpalo | AST 分配 |
| 并发容器 | dashmap | 模块图、slot 表分片 |
| 工作窃取队列 | crossbeam-deque | wake_turbo 自研执行器的底层队列 |
| 过程宏 | syn / quote / proc-macro2 | `#[wake::task]` |
| 并发测试 | loom | 引擎并发正确性模型检查 |
| hash | rustc-hash (FxHashMap) | 全部内部 map |
| 分配器 | mimalloc | 全局 |
| 文件监听 | notify | watch |
| 网络 | tokio + hyper/axum | 仅 dev server |
| WebSocket | tokio-tungstenite | HMR |
| 序列化 | rkyv / serde_json | 缓存 / manifest |
| SIMD 扫描 | memchr、simdutf8（或 std::arch 手写） | lexer 白名单依赖 |
| hash | xxhash-rust (xxh3)、blake3 | 内存指纹 / 持久键 |
| CLI | clap | |
| 测试 | insta、criterion、cargo-fuzz、playwright(JS侧) | |

编译核心（lexer/parser/transform/codegen）的依赖策略为**白名单制**：只允许「无传染、可随时替换、单一职责」的底层库进入——现白名单：bumpalo、rustc-hash、memchr、simdutf8、xxhash-rust。任何带框架性质或拉传递依赖树的库一律禁止。这既是性能纪律也是自主可控的题中之义。

### 14.2 参考资料

- esbuild 架构文档与源码注释（bundler 三阶段与并行模型的最佳教材）
- Oxc 的 AST/arena 设计、SWC 源码（parser 工程细节）
- rustc 查询系统（red-green 算法）源码文档 + Salsa 手册：失效算法的两份教科书
- turbo-tasks 源码（Turbopack 仓库）：全并发任务图的工程参照，也是「什么不该做」的参照
- Adapton / self-adjusting computation 论文（Acar）：函数级增量的理论根基
- ECMAScript 规范（tc39.es/ecma262）：lexer/parser 必须直接读规范而不是二手资料
- test262 / test262-parser-tests：正确性基准
- Vite HMR API 文档：`import.meta.hot` 事实标准
- [JavaScript Bundlers in 2026](https://dev.to/thedailyagent/javascript-bundlers-in-2026-vite-rspack-turbopack-and-the-end-of-an-era-16hk) / [Rolldown vs Rspack vs Turbopack 2026](https://www.pkgpulse.com/guides/rolldown-vs-rspack-vs-turbopack-2026)（生态现状）

---

*本文档为 Wake 项目的顶层设计，各子系统（parser、HMR、缓存）在进入对应 Phase 时应各自展开详细设计文档，存放于 docs/ 下。*
