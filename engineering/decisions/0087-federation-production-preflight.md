# ADR 0087: Federation 生产资源预检策略与启动队列

- Status: accepted
- Date: 2026-11-20

## Context

生产文档站会在同一页面为多个 ShadowRoot 挂载同一个 isolated expose。浏览器的样式和脚本
加载顺序同时限制了预检顺序，静态托管也可能为成功的 HEAD 响应省略 Content-Length。

## Decision

浏览器运行时在接受目标生产 Manifest 和 expose 后，使用统一、最多并发六项的队列提前预检初始 CSS、
容器入口、同步 JavaScript 和 expose 入口。启动任务优先于队列中普通任务。懒加载按需验证。
生产 remote 可显式选择对已锁定的同源 HTTPS 资源直接进行 GET 字节验证。其他请求保持 HEAD。
自定义 transport 可提供纯资源预检能力，并继续通过现有方法加载资源。

## Invariants

- Manifest 摘要、发布锁和资源闭包先于资源预检。原生标签仍强制 SHA-384 SRI，且保持 CSS 和
  JavaScript 的加载顺序。
- 生产预检身份包含规范化 URL、SHA-384、资源种类、MIME、解码大小、字节上限和实际策略。
  共享进行中请求和成功结果。失败可以重试，最后一个等待方离开才中止共享操作。
- 直接 GET 不带凭据、不跟随重定向，验证 MIME、有界的解码流、精确大小和 SHA-384。
- 开发远程仍在实际加载前预检 HEAD，保留开发租约和 410 控制。

## Evidence

`crates/wake_app/assets/federation-runtime.mjs` 拥有浏览器加载顺序、资源验证和生产 Manifest
接受。Crab UI PR #75 记录了缺少 HEAD 长度的部署以及跨 ShadowRoot 的预检复用。

## Consequences

生产资源校验与执行可以重叠。选择直接校验的生产 remote 会在原生加载前读取解码后的响应，
浏览器仍按 HTTP 缓存规则处理原生标签的加载请求。

## Validation

- `corepack yarn npm:test:wake:federation`
- `cargo test --locked --offline -p wake_test --test federation_chromium -- --ignored --nocapture --test-threads=1`
- `corepack yarn architecture:test` 和 `corepack yarn architecture:check`

## Supersedes

None.

## Removal plan

None.
