# ADR 0016: 交互式终端控制台契约

- Status: accepted
- Date: 2026-08-19

## Context

Wake 的 Rust CLI 和 npm CLI 过去各自实现全屏仪表板。两者都会进入原始模式和备用屏幕模式，却只识别
单键退出、清除和滚动控制，不具备可编辑输入、粘贴协议、屏幕选择或剪贴板所有权。这使 TUI 无法像
现代交互式控制台那样工作，也让两个公共 CLI 界面容易产生行为漂移。

构建和开发服务器生命周期已通过 `wake_app` 共享。终端呈现及宿主操作系统副作用属于产品边缘职责，
不得移入编译器、打包器或应用服务层。

## Decision

Rust 和 npm CLI 边缘拥有等价的终端交互状态机：Unicode 感知的单行编辑器、有界内存历史、基于单元格
的屏幕选择、剪贴板与 URL 打开器适配器，以及完整的终端模式恢复。支持的命令为 `help`、`clear`、
`open` 和 `quit`；开头的斜杠可省略，提交的 `q` 等同于 `quit`。

`fixtures/terminal-console-contract.json` 是两个测试套件共同使用的机器可读命令契约。服务事件与生命周期
仍由 `wake_app` 拥有；终端层只读取已经生成的端点，并请求现有停止路径。

## Invariants

- `--ui plain` 绝不捕获输入、鼠标事件、粘贴或剪贴板状态，也不输出 TUI 转义序列。
- Rust 和 npm 对共享命令夹具的解析完全一致。
- 选择坐标使用渲染后的终端单元格，包括宽字符及组合 Unicode 字素。
- `open` 只能使用当前 Wake 服务生成的端点，不能执行用户提供的 URL。
- 剪贴板和打开器失败属于诊断 UI 事件，不会改变构建或服务器状态。
- 每条退出路径都会恢复原始模式、备用屏幕、鼠标捕获、括号粘贴和光标可见性。
- 用户输入和命令历史仅存在于进程内，绝不持久化。

## Evidence

- `crates/wake_cli/src/console.rs` 和 `crates/wake_cli/src/dashboard.rs` 实现并测试 Rust 交互模型。
- `npm/wake/bin/console.mjs`、`npm/wake/bin/terminal.mjs` 及其测试实现 npm 模型与流式终端解码器。
- `fixtures/terminal-console-contract.json` 负责命令一致性门禁。

## Consequences

输入 `q` 或 `c` 不再立即执行操作；文本会在命令行中编辑，按 Enter 后才执行。Ctrl-C 仍会立即中断。
npm 包和 Rust CLI 新增平台剪贴板、Unicode 宽度及 URL 打开器依赖。Node API、Rust 应用 API、配置、
缓存和持久产物均不变。

## Validation

运行聚焦的 Rust 与 npm 终端测试、`npm run architecture:check`、npm 打包验证、工作区测试和
`git diff --check`。在 Windows Terminal 中实际测试选择、粘贴、命令、缩放和恢复；发布 CI 覆盖
受支持的 Linux 与 macOS 构建。

## Supersedes

None.

## Removal plan

在同一变更中删除旧的直接 `q`/`c` 处理器、页脚契约及完整分块式 npm 按键解析器。不保留兼容桥接
或重复交互路径。
