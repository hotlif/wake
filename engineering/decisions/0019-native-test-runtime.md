# ADR 0019: 拥有原生 JavaScript 测试运行时

- Status: superseded
- Date: 2026-08-21
- Superseded by: [ADR 0020](0020-react-browser-test-runtime.md)

## Context

Wake 已通过 `wake_app` 暴露构建、打包、开发服务器和文档行为，但没有 JavaScript 执行引擎或面向用户
的测试运行器。所需测试契约新增 `wake test`、Node API 和仅测试 npm 模块，同时要求内置 Jest 30.4
语义、声明式 Wake 配置，并且不得把产品行为委托给 Node 或 Jest。

编译器 AST 由 arena 拥有，不能作为运行时或缓存值保留。测试模块模拟与隔离还需要运行时模块注册表；
把每个测试套件打成一个产物会抹去这些 API 所控制的模块边界。

## Decision

新增四个明确所有者。`wake_ecma_vm` 拥有稳定、产品中立的 ECMAScript 执行门面，并将固定版本的纯
Rust Boa 引擎作为私有的字节码、值和垃圾回收内核。`wake_js_runtime` 拥有 Wake 解析/代码生成预处理、
模块加载和宿主能力。`wake_test` 拥有测试发现、兼容 Jest 的 API、执行策略和结构化结果。
`wake_test_host` 是用于隔离与原生扩展托管的内部可执行壳层。

测试源码在执行前通过 Wake 解析器和代码生成器，因此 TypeScript、JSX、诊断和源码标识仍属于 Wake
契约。生成的 JavaScript 立即编译为 VM 字节码；arena AST 与进程内 atom 不进入 VM 状态或持久化。
Boa API 不越过 `wake_ecma_vm` 的公共边界。

CLI 和 Node 调用方继续通过 `wake_app` 收敛。最终宿主传输采用版本化、带认证和长度分帧的本地协议。
首版实现可在验证协议期间于进程内执行同一服务，但必须在接受本 ADR 及发布稳定 npm 契约前删除该桥接。

## Invariants

- `wake_ecma_vm` 不依赖文件系统、解析器、测试框架、壳层或产品。
- `wake_js_runtime` 拥有运行时模块标识，绝不调用 Web 打包器。
- 每个测试套件获得隔离的 realm 和模块注册表。
- AST 引用、atom、VM 值、垃圾回收器句柄和 Node-API 句柄绝不序列化或持久化。
- 源码位置在预处理后仍指向原测试或依赖。
- 测试发现、结果排序、种子、快照和覆盖率产物在受支持平台上保持确定。
- CLI 与 Node 的选项、取消、结果和失败通过 `wake_app` 收敛。
- 不受支持的宿主或扩展行为以结构化诊断失败；不存在隐藏的 Node 或 Jest 回退。
- 原生扩展仅限 Node-API 1 至 8，且不得导入 V8、NAN、`node.h` 或直接 libuv ABI。

## Evidence

- `engineering/ARCHITECTURE.md` 将 `wake_app` 定义为共享应用边界。
- `crates/wake_ecma_ast` 封装 arena 支持的 `ModuleAst` 值。
- `crates/wake_ecma_parser` 和 `crates/wake_ecma_codegen` 已能转换 JS、TS、JSX 和 TSX 并保留范围。
- `crates/wake_resolver` 已拥有 Node 包、工作区及 Yarn PnP 解析。
- `crates/wake_node` 使用 napi-rs 的 `napi8` feature 构建。
- Boa 0.21.1 提供纯 Rust 字节码 VM、垃圾回收器、realm、模块和可嵌入上下文中的 Promise 任务队列。

## Consequences

工作区获得大型执行与测试产品，但测试语义不会移入打包器或壳层。平台产物会增加内部测试宿主，发布检查
必须同时审计 Node 绑定和宿主。VM 一致性成为新的正确性门禁。

内部引擎仍可替换，但更换时必须重放 ECMAScript 与 Jest 一致性矩阵。Wake 解析器和 VM 内核都会解析
生成的 JavaScript；只有 Wake 预处理结果是公共契约，差异测试必须拒绝语法或位置分歧。

## Validation

- 每次所有权变更都运行架构检查及聚焦 crate 测试。
- 执行有代表性的 JS、TS、JSX、异步及模块程序，而非只断言生成文本。
- 固定并运行适用的 Test262、Jest 30.4 和 Node-API v8 一致性清单。
- 测试冷/热测试发现、套件隔离、取消和确定性排序。
- 在每个发布目标上运行 Node API、TypeScript 声明、npm 打包和干净安装测试。
- 实现落地后，使用 Miri 检查 VM/原生句柄生命周期，使用 Loom 检查宿主/TSFN 关闭协议。

## 当前接受状态

本决策被替代时，所有权边界、回环宿主协议、显式测试模块、CLI/Node 入口、隔离套件 realm、核心断言
与模拟、伪计时器、外部快照、发现、项目、分片和套件 worker 已实现。但稳定发布门禁尚未满足，仍缺少：

- 固定的 Test262 ES2024 与 Jest 30.4 差异矩阵；
- babel/v8 覆盖率插桩、报告器及阈值；
- 对照 Jest 黄金夹具的原子行内快照源码重写；
- Node-API v1-v8 扩展 C ABI 及其五平台一致性矩阵；
- 完整 Node/jsdom 宿主清单、完整 Jest CLI 选项矩阵、开放句柄报告、缓存失效及真正的套件内并发调度；
- 指定的 Miri、Loom、模糊测试、OOM、损坏 IPC 和跨平台发布门禁。

覆盖率请求及缺失的行内快照重写会显式失败，而不会返回空或误导性的成功数据。原生 `.node` 导入在
ABI 门禁实现前返回 `WAKE_TEST_UNSUPPORTED`。

## Supersedes

None.

## Removal plan

接受本 ADR 前删除临时进程内宿主桥接。启用稳定运行器时，在同一次仓库级切换中把现有 JavaScript
测试从 `node:test` 迁出并删除该工具。不保留 Node/Jest 执行回退或第二套测试结果架构。

`npm/wake/test/api.test.mjs` 在测试 Node-API 插件、Worker 清理和真实 socket 生命周期期间，仍作为
开发专用的 `node:test` 一致性门禁，而非产品回退。只有测试宿主实现并通过固定的 Node-API v1-v8 ABI
矩阵后，才删除此例外并将文件迁移到 `@crab-dev/wake/test`。
