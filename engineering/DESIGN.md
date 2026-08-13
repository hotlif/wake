# Wake 设计文档

本文保留源码现有 `DESIGN §…` 引用所需的编号，但描述的是 v0.1.16 已实现设计。未来意向统一写入 [ROADMAP.md](ROADMAP.md)。

# 1. 目标与原则

Wake 将 JavaScript/TypeScript 编译、生产打包、开发服务器和 React 组件文档放在同一套 Rust 工具链中。设计优先级依次是正确性、可诊断性、跨平台一致性、增量工作规避和吞吐。

非目标包括执行任意 JavaScript 配置、公开稳定 Rust 插件 ABI，以及在 0.x 冻结实验编译器数据结构。

# 2. 产品边界

CLI 与 Node API 是两种调用外壳；构建行为由 `wake_app` 统一。Wake Docs 是同一编译管线上的产品层，不是独立的第二套打包器。

# 3. 总体架构

## 3.1 Workspace 与入口

25 个 crate 按基础、编译、解析/资源、执行/编排和产品边缘分层。完整列表和依赖方向见 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 3.2 构建阶段

应用构建采用 Scan → Link → Emit：Scan 解析依赖并建立记录，Link 计算活跃性与 chunk，Emit 生成 runtime、代码和资源。

## 3.3 核心抽象

编译核心通过 `FileSystem` 读取内容，通过 `Diagnostic` 报告问题；`ModuleAst` 持有 arena 生命周期；打包器只跨阶段传稳定记录和受控句柄。

# 4. ECMAScript 编译器

## 4.1 基础设施

热路径使用字节 Span，行列号在诊断或 Source Map 序列化时按需换算。`Atom` 是进程内句柄，禁止进入持久化缓存。

## 4.2 AST

AST 使用 arena 分配和紧凑枚举；`ModuleAst` 把源码、arena 与 Program 生命周期封装在不可克隆的自引用持有者中。手写 `unsafe` 由 Miri 常驻验证。

## 4.3 Lexer 与 JSX

Lexer 按字节扫描，token 只保存类型、Span 和 ASI 换行状态。parser 驱动正则/除号与 JSX 文本上下文；标识符只在 parser 需要时驻留。

## 4.4 Parser 与依赖

语句使用递归下降，表达式使用 Pratt 优先级解析。解析时同步提取静态/动态依赖和顶层 await 标志，并根据 SourceType 选择 JS、TS、JSX、TSX 或 CommonJS 语义。

## 4.5 Transform

TypeScript 擦除、JSX automatic runtime 和浏览器目标转换在明确上下文中运行。转换复用原 Span，避免丢失诊断与 Source Map 来源。

## 4.6 Codegen 与 Source Map

Codegen 直接写字符串并根据优先级补括号。Source Map 记录生成位置到源码字节偏移；精确 source map 构建关闭会改变映射结构的压缩和分包路径。

# 5. 解析与模块图

## 5.1 Resolver

Resolver 支持相对路径、node_modules、package exports、alias、workspace 和 Yarn PnP zip。Components 模式可配置按 issuer 包名、目标依赖和 provider issuer 限定的 PnP fallback；它只处理未声明依赖或未满足 peer，不绕过 alias、正常依赖、顶层 fallback 或 exports 错误。同一物理路径对应多个 PnP locator 时，依赖目标必须唯一或等价，否则报告歧义。缓存按解析上下文隔离；结构性文件变化需要清理相关命中与 miss。

## 5.2 Scan

增量路径按依赖层并行加载、解析和分析。共享模块通过内容身份与 single-flight 避免重复工作；诊断不因并行而丢失来源路径。

## 5.3 Tree Shaking

Link 阶段从入口计算模块、导出和绑定活跃性。缓存摘要保存恢复该分析所需的稳定字符串和标志，不保存 `Atom` ID。

# 6. 打包与产物

模块默认进入函数包装 runtime；满足条件的 ESM 可进入 concat/scope-hoist 块。循环、CJS `module.exports` 替换、导出冲突和不安全块语义会保守降级为独立 factory。

## 6.1 CJS/ESM 互操作

runtime 为 ESM namespace、CommonJS exports、循环模块和异步模块维持单次执行与缓存身份。

## 6.1.1 顶层 await

包含顶层 await 的模块及其同步导入方形成 async 子图。runtime 缓存执行 Promise，调用方等待同一生命周期，避免重复执行或捕获未完成导出。

## 6.2 Emit

Emit 生成 JS chunk、CSS、静态资源、HTML、manifest 和可选 Source Map。文件路径在跨平台输出前统一规范化。

## 6.3 Chunk 与动态导入

生产构建为动态 import 创建 async chunk，并通过 `public_path` 生成加载 URL。相对 `./` 用于 file URL/Electron；文档站独立使用 `base_path`。

# 7. Dev Server 与 HMR

开发服务器采用增量打包模式：监听变化、失效会话、重建受影响模块，再发送 HMR 消息。通知在 Windows 上会合并延迟事件；配置、根或入口的结构变化建议重启。

# 8. CSS 与静态资源

## 8.1 CSS

开发模式可注入样式，生产模式抽取 CSS；CSS Modules 生成局部类名。CSS-in-JS 位于独立 crate，保持编译核心依赖边界。Crab UI 包由 `package.json#name = @crab-dev/rc-*` 与受支持入口共同识别，存在 `css/index.css` 时由 loader 自动加入模块图；业务源码与 Components runtime 均不显式导入这些样式。该身份也参与持久缓存判定，使 CSS 的新增、删除及跨进程热缓存保持冷构建等价。

## 8.2 静态资源

小资源内联为 data URL，大资源使用内容哈希独立输出。`.raw` 作为文本导出，JSON 作为默认导出模块，CSS `url()` 与 HTML/manifest 使用统一公共路径。

# 9. 扩展边界

当前没有承诺稳定的公开 Rust/JavaScript 插件系统。组件扫描、Preview、主题 CSS 和声明式 TOML 是受支持的扩展点；实验编译器 API 不等于插件 ABI。

# 10. 增量与并发

## 10.1 工作规避

Wake 使用内容身份、resolver/load cache、任务依赖、持久化摘要和生成体缓存减少重复解析与 codegen。

## 10.2 并发模型

一次 revision 的变更写入是串行的，查询可并行；Web 构建依赖图仍由领域层组织，不把每个对象建模为 Actor。

## 10.3 wake_turbo

任务 ID 来自函数与参数指纹。slot 保存类型擦除值、输出指纹、依赖和 revision；浅校验、深校验、重算与早期截断组成红绿失效协议。per-task single-flight 锁保证同一任务只执行一次，线程本地上下文收集直接依赖。

## 10.4 Arena AST 生命周期

AST 持有者放入 `Arc` 管理的任务值，下游只在受控闭包中借用。不得把 AST 引用、指针或 `Atom` 跨进程持久化。

## 10.5 执行器

工作窃取执行器处理同层 parse/codegen 扇出。Loom 验证 single-flight 交错；循环依赖检测在同线程任务栈上拒绝递归环。

## 10.6 性能预算

预算以可复现 benchmark 而非设计目标数字为准。现有测量面包括 interner、lexer、parser、resolver、turbo 和 bundle；方法见 [PERFORMANCE.md](PERFORMANCE.md)。

## 10.7 缓存身份

目标浏览器、JSX、define、生产选项和源码内容进入必要的缓存身份。缓存命中必须与冷构建产物等价。

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

关键降级包括：增量引擎可关闭跨 revision 复用、危险 concat 回退独立 factory、Source Map 关闭压缩与分包、终端非 TTY 回退 plain 日志。任何性能优化都必须保留可对拍的正确路径。

# 14. 附录

## 14.1 依赖纪律

编译核心外部依赖白名单在 workspace `Cargo.toml` 维护；网络、序列化、配置、终端和 Node 依赖限制在边缘 crate。新增依赖同时接受许可证检查。
