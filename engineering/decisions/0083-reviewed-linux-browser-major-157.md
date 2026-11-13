# ADR 0083: Linux 157 浏览器镜像准入

- Status: accepted
- Date: 2026-11-13

## Context

0.1.51 候选发布的 Linux x64 消费门禁连续两次取得 Chrome 157.0.8103.50，
React 截图 smoke 成功，但 ADR 0080 的主版本策略拒绝该身份；npm 发布未执行。
运行器日志与官方不可变镜像清单一致。保留 v0.1.51 标签，以 0.1.52 继续发布。

## Decision

Linux x64 的 reviewed-major-conformance 增加 157，记录对应官方镜像证据，
同步 CI 与发布前后消费矩阵。准入后仍须通过完整 Linux browser-conformance 和发布消费检查。

## Invariants

- 仅使用系统浏览器，保留 CDP 完整版本、headless、平台、类型与不可变来源校验。
- Linux 继续拒绝 153、154、158 等未经审查的主版本；不从其他平台推断 Linux 准入。
- 准入不替代 React、CDP、截图、覆盖率和产品流程回归。
- stable readiness 仍要求五目标共享精确主版本 151，保持 blocked。
- Windows、macOS、Linux ARM64 与普通本地浏览器选择不变。

## Evidence

- [官方不可变镜像清单](https://github.com/actions/runner-images/blob/fb099341ddaf44a428fb17495341cb7c2300faf2/images/ubuntu/Ubuntu2404-Readme.md)：20261111.370.1，Chrome 157.0.8103.50、Chromium 157.0.8103.0、Edge 157.0.4341.46。
- [首次发布消费门禁](https://github.com/hotlif/wake/actions/runs/43380503838/job/131752802069)和[同门禁重试](https://github.com/hotlif/wake/actions/runs/43380503838/job/131753806704)记录相同镜像及 CDP 版本。
- 身份测试覆盖实际 Linux 157、未知主版本拒绝、缺失镜像证据拒绝和 stable readiness 不变。

## Consequences

新镜像可进入原有实验性门禁，完整功能结果由云端验证。后续镜像升级继续显式审查。
0.1.51 的未发布候选记录和标签保留，不改写已有标签。

## Validation

- `corepack yarn browser:conformance:test`、`corepack yarn release:check`。
- `corepack yarn architecture:test`、`corepack yarn architecture:check`。
- `node scripts/check-system-browser-conformance.mjs --stable-readiness blocked`。
- Linux Chrome 157 云端完整 browser-conformance，以及发布前后干净 npm 消费验证。

## Supersedes

None.

## Amends

- [ADR 0080](0080-reviewed-linux-156-macos-arm64-157.md): 仅修订 Linux x64 继续拒绝 157 的范围约束。

## Removal plan

无兼容桥。删除准入主版本时，同步删除镜像证据并更新矩阵和测试。
