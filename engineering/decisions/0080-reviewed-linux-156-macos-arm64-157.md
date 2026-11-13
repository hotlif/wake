# ADR 0080: Linux 156 与 macOS arm64 157 浏览器镜像准入

- Status: accepted
- Date: 2026-11-11
- Amended by: [ADR 0083](0083-reviewed-linux-browser-major-157.md)

## Context

0.1.47 候选的 CI 从系统浏览器取得 Linux Chrome 156.0.8078.79 和 macOS arm64
Chrome 157.0.8103.39。两者因旧主版本清单被拒绝，尚未执行后续功能测试。
运行器日志中的镜像版本与官方不可变清单一致，候选发布已在 npm 发布前取消。

## Decision

Linux x64 的 reviewed-major-conformance 增加 156，macOS arm64 的 reviewed-major-smoke
增加 157。保存对应镜像的不可变来源，同步 CI 与发布前后消费矩阵，不从其他平台推断准入。

## Invariants

- 仅使用系统浏览器；CDP 完整版本、headless、平台、浏览器类型及不可变证据检查保持生效。
- Linux 仍拒绝 153、154、157；macOS arm64 仍拒绝 151、153 至 156 及 158。
- 准入只允许执行后续门禁，不能替代 React、CDP、截图、覆盖率与产品流程测试。
- stable readiness 仍要求五目标共享精确主版本 151，当前保持 blocked。
- Windows、macOS x64、Linux ARM64 与普通本地浏览器选择不变。

## Evidence

- [Linux 官方镜像清单](https://github.com/actions/runner-images/blob/add4380b0d6685806e9192c33c37359ee0aa4023/images/ubuntu/Ubuntu2404-Readme.md)：20261101.360.1，Chrome 156.0.8078.79。
- [macOS arm64 官方镜像清单](https://github.com/actions/runner-images/blob/4a3efbb98bd2d06646222d4cbab17be441c0863b/images/macos/macos-15-arm64-Readme.md)：20261104.0514.1，Chrome 157.0.8103.39。
- [Linux CI 身份门禁](https://github.com/hotlif/wake/actions/runs/42942008588/job/130266202778)与 [macOS arm64 CI 身份门禁](https://github.com/hotlif/wake/actions/runs/42942008588/job/130266202843)记录相同镜像和 CDP 版本。
- `scripts/check-system-browser-conformance.test.mjs` 覆盖实际版本准入、缺失审查证据和未知主版本拒绝。

## Consequences

新镜像可进入原有实验性门禁；其完整功能结果仍须由云端验证。未来镜像升级继续显式审查，
不扩大 stable readiness 或其他平台承诺。

## Validation

- `corepack yarn browser:conformance:test`、`corepack yarn release:check`。
- `corepack yarn architecture:test`、`corepack yarn architecture:check`。
- `node scripts/check-system-browser-conformance.mjs --stable-readiness blocked`。
- Linux 云端完整 browser-conformance、macOS arm64 功能 smoke，以及干净 npm 消费项目的发布前后浏览器检查。

## Supersedes

None.

## Amends

- [ADR 0020](0020-react-browser-test-runtime.md): macOS arm64 实验性浏览器已审查主版本范围。
- [ADR 0073](0073-reviewed-browser-major-155.md): Linux x64 实验性浏览器已审查主版本范围。
- [ADR 0074](0074-reviewed-windows-browser-major-156.md): 不变量中 Linux x64 继续拒绝 156 的范围约束。

## Removal plan

无兼容桥。删除准入主版本时，同步删除镜像证据并更新矩阵和测试。
