# ADR 0077: 原生开发代理流与升级连接

- Status: accepted
- Date: 2026-11-11

## Context

crab-boot 为 SSE 和 Inspector WebSocket 使用外部 Node 代理。原生代理缓冲响应，配置 ws 未传入服务。

## Decision

wake_app 传递已有 ws 配置，wake_dev_server 拥有 HTTP 响应流、WebSocket 双向转发及连接关闭。
开发代理不新增子进程，不改变字符串前缀匹配或 path_rewrite 规则。HTTP 通过消费上游流实现背压；
WebSocket 使用有界通道，保留文本、二进制、控制帧与协商协议，服务停止结束桥接任务。
单帧上限为 16 MiB，以覆盖 Inspector 源码响应，同时保留明确的帧大小边界。

## Invariants

- SSE 首帧不等待上游 EOF；断开下游释放上游资源。
- ws 默认关闭，配置必须穿透生产者与消费者。
- 已升级连接不阻止服务关闭和端口复用。

## Evidence

- crates/wake_dev_server/src/lib.rs 中代理首帧及升级回归。
- 升级回归覆盖 ws=false、协议协商、路径及查询参数、256 KiB 文本、二进制、ping/pong、关闭码及原因、服务关闭与端口复用。
- HTTP 回归覆盖首帧、多值 Cookie 与 Connection 声明的逐跳头移除。
- crates/wake_app/src/lib.rs 中配置映射。

## Consequences

无需外部 Node 代理；已经发出响应头后的流错误只能结束连接，不能改写为 502。

## Validation

- cargo test -p wake_dev_server --lib
- cargo test -p wake_app --lib
- cargo clippy -p wake_dev_server -p wake_app --all-targets -- -D warnings
- corepack yarn architecture:test
- corepack yarn architecture:check

## Supersedes

None.

## Removal plan

验证原生路径后，消费者可自行移除外层代理；Wake 不保留第二套代理实现。
