# 当前架构

## 1. 系统定位

Wake 是 Rust workspace 形式的 Web 构建工具。当前代码覆盖 ECMAScript/TypeScript/JSX 的词法、语法、转换、代码生成，Node 风格解析，模块图、tree shaking、chunk、缓存、开发服务器与 CLI。

顶层数据流是：

```text
CLI / Dev Server
       |
       v
IncrementalBundler
       |
       +-- Resolve + Load
       +-- Parse + dependency extraction
       +-- Module graph + liveness
       +-- Link + transform + minify
       +-- Chunk + runtime + assets
       +-- Memory task cache / persistent cache
       |
       v
BuildOutput
```

仓库还保留同步 `Bundler` 路径。这是当前最重要的架构分叉，不能被视为稳定的长期边界。

## 2. Crate 分层

### 基础层

- `wake_common`：Span、诊断、源码、文件系统抽象、Atom/interner 和通用集合别名。
- `wake_ecma_ast`：AST、访问器及自引用 AST holder。
- `wake_ecma_lexer`：字节级词法分析。

### 编译层

- `wake_ecma_parser`：语法解析、依赖提取和语义信息。
- `wake_ecma_transform`：转换入口。
- `wake_ecma_codegen`：输出、链接改写和 source map。
- `wake_ecma_minify`：常量折叠、DCE、mangle 等压缩阶段。

### 构建层

- `wake_resolver`：文件、node_modules、alias 和 Yarn PnP 解析。
- `wake_graph`：模块图工具、导出使用分析和绑定活跃性。
- `wake_css`、`wake_css_in_js`、`wake_html`、`wake_scan`：非 JS 内容与项目扫描。
- `wake_turbo`：记忆化任务、依赖跟踪、single-flight 和执行器。
- `wake_cache`：持久化记录的编码与文件存储。
- `wake_bundler`：Scan/Link/Emit 编排与最终产物生成。

### 边缘层

- `wake_config`：配置读取与归一化。
- `wake_dev_server`：HTTP、WebSocket、监听和代理。
- `wake_cli`：命令入口、输出和进程生命周期。

## 3. 必须维持的依赖方向

```text
common
  └─> ast
       ├─> parser ─> transform
       └───────────> codegen ─> minify

common ─> resolver
ast/common ─> graph

以上能力 ─> bundler ─> dev_server / cli
turbo + cache ───────> bundler
```

规则：

1. 编译层不得依赖 CLI、dev server 或 bundler。
2. `wake_graph` 不负责文件读取、路径解析或产物写入。
3. `wake_cache` 不认识 AST 生命周期，只存稳定、带版本的 DTO。
4. `wake_turbo` 不认识 Web 构建领域类型，通过任务输入/输出工作。
5. CLI 和 dev server 只能调用统一的构建会话 API，不能复制构建逻辑。

## 4. 生命周期与所有权

每个模块通过 `ModuleAst`/holder 管理 arena 与 AST 的自引用关系，并通过闭包式访问阻止 AST 引用逃逸。该设计能保持 arena 性能，但要求跨线程、跨缓存边界只传递拥有所有权的摘要或 `Arc` 包装结果。

应明确区分三类数据：

- 输入事实：规范化路径、源文本、配置、环境目标。
- 派生事实：解析摘要、依赖、语义/活跃性、转换结果。
- 最终产物：chunk、source map、CSS 与静态资源。

任何缓存键必须覆盖会改变对应派生事实的全部输入。不能只用源内容哈希来代表受配置、resolver 条件或工具版本影响的结果。

## 5. 增量模型

当前 `IncrementalBundler` 持有 resolver、interner、engine、executor、输入 cell、linker cell 和持久化缓存，并按 BFS 层次扫描模块。层内解析与 resolve 可并行，ID 分配和记账保持确定性。

目标模型应只有一个入口：

```text
BuildSession::build(request)
  -> task(resolve)
  -> task(load)
  -> task(parse)
  -> task(analyze)
  -> task(link)
  -> task(chunk)
  -> task(emit)
```

冷构建、热构建、watch 和 dev server 应共享这条路径，只在缓存状态和请求选项上不同。

## 6. 错误模型

用户输入错误使用 `Diagnostic` 返回；内部不变量破坏才允许 panic。并行任务必须把 panic、取消和普通错误区分开，防止等待者永久阻塞。输出写盘应由边缘层负责，并采用临时文件加原子替换，避免失败时留下半成品。

## 7. 确定性

相同的规范化输入、配置和工具版本必须生成字节一致的产物。所有来自 hash map、并行完成顺序和文件系统枚举的集合，在分配模块 ID、chunk ID、文件名或序列化前必须显式排序。现有 chunk 确定性测试应扩展到整个构建结果和持久化缓存冷热两种状态。
