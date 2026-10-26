# ADR 0073: Windows 与 Linux 浏览器证据纳入已审查主版本 155

- Status: accepted
- Date: 2026-10-26

## Context

2026-10-22 的 CI 在 Windows 与 Linux x64 成功启动系统 Chrome 并取得 CDP 完整版本，
随后因主版本清单仍仅包含 151、152 而停止。实际版本分别为 155.0.8059.40 和 155.0.8059.39，
与该次 runner 对应的官方不可变镜像清单一致。该失败尚未执行后续浏览器一致性测试。

## Decision

Windows x64 与 Linux x64 的 `reviewed-major-conformance` 清单加入 155，保留 151、152 的
既有审查证据。CI、发布前消费验证和发布后浏览器冒烟同步使用这份范围。
每个新增主版本仍须先取得固定提交的官方 runner 清单，并通过完整浏览器门禁；不会自动跟随镜像升级。

Windows 新镜像同时包含 Edge 154，但本次 CI 固定使用 Chrome。新 Windows 证据只记录经过本次
准入审查的 Chrome 155，不因同镜像内存在 Edge 154 而扩大允许范围。

## Invariants

- 仅使用系统浏览器；CDP 完整版本、headless、目标平台与不可变官方清单校验保持阻塞。
- 未审查的 153、154、156 等主版本继续被拒绝；不下载浏览器，不增加 fallback。
- 版本准入只允许执行后续一致性检查，不能替代 CDP、React、截图、覆盖率和产品流程验收。
- stable readiness 仍要求五目标共享精确主版本 151，当前结果保持 blocked。
- macOS 与 Linux ARM64 的已审查策略、公共 API 和普通本地浏览器选择不变。

## Evidence

- [Windows 官方清单](https://github.com/actions/runner-images/blob/aee0669dd04510a11756664eb77f5630debabed3/images/windows/Windows2025-VS2026-Readme.md)：
  镜像 20261007.264.1，Chrome 155.0.8059.40；该次 [Windows CI](https://github.com/hotlif/wake/actions/runs/39772116516/job/119802465008)
  记录相同版本，并在旧主版本门禁失败。
- [Linux 官方清单](https://github.com/actions/runner-images/blob/7e7dc1bcc0ef464e5e315a2b4c33609590c6cffe/images/ubuntu/Ubuntu2404-Readme.md)：
  镜像 20261012.334.1，Chrome 155.0.8059.39、Chromium 155.0.8059.0、Edge 155.0.4283.45；
  该次 [Linux CI](https://github.com/hotlif/wake/actions/runs/39772116516/job/119802465158) 记录同一 Chrome 版本。
- `scripts/check-system-browser-conformance.test.mjs` 使用上述两份实际 CDP 版本，验证准入与未知主版本拒绝。

## Consequences

经过审查的托管镜像升级可以进入实验性浏览器一致性检查；现有版本证据仍可复核。
未来主版本升级继续需要显式审查和 CI 验收，本次清单更新不宣称后续测试已经通过。

## Validation

- `corepack yarn browser:conformance:test`、`corepack yarn release:check`。
- `corepack yarn architecture:test`、`corepack yarn architecture:check`。
- Windows/Linux 云端完整 browser-conformance 与干净 npm 消费项目的发布前后浏览器冒烟。
- `node scripts/check-system-browser-conformance.mjs --stable-readiness blocked`。

## Supersedes

None.

## Amends

- [ADR 0020](0020-react-browser-test-runtime.md): Linux x64 实验性浏览器证据允许的已审查主版本范围。
- [ADR 0048](0048-windows-browser-reviewed-majors.md): Windows x64 实验性浏览器证据允许的已审查主版本范围。

## Removal plan

无兼容桥。移除某个已审查主版本时，同步删除其证据并更新工作流矩阵与测试。
