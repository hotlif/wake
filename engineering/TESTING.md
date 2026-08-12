# Wake 测试与质量门禁

本文描述 v0.1.15 仓库实际执行的验证。命令应从仓库根运行；需要原生 Node 绑定的测试必须先执行 `npm run native:build` 并设置或生成当前平台 binding。

# 1. 本地最小门禁

Rust 修改：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Node/npm 修改：

```bash
npm ci --ignore-scripts
npm run versions:check
npm run native:build
npm run npm:test
npm run npm:typecheck
npm run npm:pack:check
```

文档修改：

```bash
npm run docs:check
npm run docs:build
cargo test -p wake_docs
```

`docs:check` 校验 Frontmatter、slug、站内路由和仓库文档链接；`docs:build` 验证真实 MDX/React 打包，而不只检查 Markdown 文本。

# 2. CI 矩阵

`.github/workflows/ci.yml` 当前包含：

| Job | 平台/工具链 | 目的 |
| --- | --- | --- |
| `fmt` | Ubuntu / Rust 1.95 | rustfmt 无差异 |
| `clippy` | Ubuntu / Rust 1.95 | workspace 全 target，warnings 视为错误 |
| `test` | Ubuntu、Windows / Rust 1.95 + Node 24 | 全 workspace 测试 |
| `miri` | Ubuntu / nightly | `wake_ecma_ast` 和 `wake_turbo` 手写 unsafe/内存模型 |
| `loom` | Ubuntu / Rust 1.95 | single-flight 线程交错 |
| `bench-smoke` | Ubuntu / Rust 1.95 | 全 benchmark 可编译 |
| `node` | Windows / Node 24、26 | 原生绑定、API、类型、启动与 npm pack |
| `docs` | Ubuntu / Rust 1.95 + Node 24 | 文档链接检查和生产构建 |

Node 包声明支持 `>=22.14 <27`，常规 CI 目前只覆盖 24 与 26；补齐最低版本覆盖列入路线图。

# 3. 回归矩阵

## 3.1 编译器

- lexer snapshot：字面量、模板、正则、ASI、类私有名和 JSX 边界；
- parser/semantic：JS、TS、JSX、TSX、scope、引用、依赖与顶层 await；
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
- `react-docs`：MDX、Demo、Props、主题和组件工作台；
- `2k-modules`：生成式压力与跨工具测量。

关键语义必须执行产物或在真实服务器中验证。仅检查 bundle 字符串适合局部形态断言，不能替代运行时回归。

## 3.4 Docs

- Frontmatter 必填字段、重复 slug、MDX 静态属性和 import；
- Demo glob、Preview、主题、Props/JSDoc 和 API 表；
- Components 模式的 Controls、默认值、显式 unset、hash round-trip、视口和错误恢复；
- `base_path`、404 外壳、public 资源和静态托管路径。

## 3.5 Node API

- ESM/CommonJS 导出一致；
- build、bundle、BuildContext、应用/文档服务器和事件；
- AbortSignal、关闭幂等、async dispose 和结构化错误；
- experimental 句柄 dispose、字符串便捷入口和 TypeScript 声明；
- 主包与五个平台包的版本、文件清单和可选依赖一致。

# 4. Miri 与 Loom 分工

Miri 用于 `ModuleAst` 自引用持有者、引擎线程本地裸指针等内存安全问题。涉及 crossbeam 的长时并发用例在 Miri 下按已知限制跳过，由普通并发测试和 Loom 接管。

Loom 使用 `RUSTFLAGS="--cfg loom"` 只编译引擎核心，穷举 single-flight 协议。普通 workspace test 不能替代 Loom，Loom 也不覆盖 resolver、bundler 或服务器端到端行为。

# 5. Benchmark 门禁

CI 当前执行：

```bash
cargo bench --workspace --no-run
```

它只保证 benchmark 可编译，不判断性能回归。提交性能数字或优化声明前按 [PERFORMANCE.md](PERFORMANCE.md) 运行实际测量并保存环境信息。

# 6. 发布门禁

发布 workflow 在 tag 上：

1. 运行 workspace test、clippy 和许可证检查；
2. 在 Windows、macOS x64/arm64、manylinux glibc 2.28 x64/arm64 构建；
3. 审计六个不可变 tarball 的版本、许可证、文件和体积；
4. 先发布平台包，最后发布主包；
5. 在 Node 24/26 和全部目标平台执行注册表干净安装与构建 smoke。

部分发布失败不能覆盖已有 npm 版本；修复后统一提升六个包和 workspace 版本。
