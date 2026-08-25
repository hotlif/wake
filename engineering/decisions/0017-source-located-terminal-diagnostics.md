# ADR 0017: 带源码位置的终端诊断

- Status: accepted
- Date: 2026-08-19

## Context

Wake 编译器诊断已保留源码路径、字节范围和标签，但 `wake_app` 只序列化主字节范围。开发服务器事件
路径会把整个失败构建缩减为消息字符串，因此 Rust 和 npm 终端边缘无法一致显示源码行或行号。

## Decision

`wake_app` 拥有公共、可序列化的诊断位置。它依据已准备的项目根目录解析诊断路径，每个诊断批次中
每条源码路径最多读取一次，并把从一开始的行/列范围、精确源码行和主标签附加到 `DiagnosticInfo`。

`wake_dev_server` 以结构化批次转发编译器诊断。`wake_app` 在暴露单个 Rust 或 Node 开发服务器诊断
事件前将每个批次实体化。Rust 和 npm 终端边缘根据共享的
`fixtures/terminal-diagnostic-contract.json` 契约渲染所得 DTO。浏览器覆盖层仍是独立文本使用方，
不属于终端事件协议。

## Invariants

- 现有 `start` 和 `end` 字节偏移保持不变。
- `line`、`column`、`endLine` 和 `endColumn` 从一开始；结束位置为开区间。
- 非法范围、缺失文件、无路径诊断和非源码失败应省略 `location`，而非虚构行号。
- JSON 和纯文本输出不含 ANSI 转义；彩色输出去色后与纯文本形式完全一致。
- Rust 和 npm 代码框使用相同的 Unicode 宽度和四列制表符展开规则。
- 静态、增量、开发及生产构建诊断使用同一 DTO。

## Evidence

- `wake_app` 测试执行失败的静态和开发构建，并断言源码位置在第 3 行。
- Rust 与 npm 终端测试使用同一机器可读代码框夹具。
- CLI 集成测试执行失败构建，并断言带编号源码行及脱字符。

## Consequences

Node `Diagnostic` 接口新增可选 `location` 对象。序列化的原生开发服务器事件现在包含 `diagnostic`，
而非仅消息载荷；公共 Node `diagnostic` 监听器仍接收一个 `Diagnostic` 参数，但数据更丰富。只有在
诊断实体化时才读取源码快照，且每个批次中按路径去重，因此新增工作量有界。

## Validation

运行聚焦的 `wake_app`、`wake_dev_server` 和 `wake_cli` 测试；npm 终端测试与类型检查；架构检查；
工作区测试；打包验证；以及 `git diff --check`。

## Supersedes

None.

## Removal plan

在同一变更中删除仅消息的服务器事件和 Node 侧合成的 `WAKE_BUILD` 诊断。不保留兼容桥接或第二套终端诊断协议。
