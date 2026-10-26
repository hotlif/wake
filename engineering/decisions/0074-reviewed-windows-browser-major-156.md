# ADR 0074: Windows 浏览器证据纳入已审查主版本 156

- Status: accepted
- Date: 2026-10-26

## Context

ADR 0073 为 Windows 与 Linux x64 加入已审查的 Chromium 主版本 155。
2026-10-26 的修复验证分配到更新的 Windows 镜像 20261021.278.1，系统 Chrome 已为
156.0.8078.48，CDP 身份检查因清单仍只允许 151、152、155 而停止。
该次 runner 日志明确引用的镜像 tag 和对应不可变提交是本次审查依据；其他时间取得的
`main` 清单不能替代正在运行的镜像证据。

## Decision

仅将 Windows x64 的 `reviewed-major-conformance` 范围扩展为 151、152、155、156，
保留已有审查证据，并新增 20261021.278.1 镜像的固定提交清单。
该清单记录 Chrome 156.0.8078.48 和已在准入范围内的 Edge 155.0.4283.62。
Windows CI、发布前消费验证和发布后浏览器冒烟同步使用这一范围。

## Invariants

- 仅使用系统浏览器，保留 CDP 完整版本、headless、平台及不可变清单校验，不下载或回退浏览器。
- 未审查的 153、154、157 等主版本继续被拒绝；Linux x64 保持 151、152、155，仍拒绝 156。
- 版本准入只允许执行后续门禁，不能替代 CDP、React、截图、覆盖率及产品流程验收。
- stable readiness 仍要求五目标共享精确主版本 151，当前结果保持 blocked。
- macOS、Linux ARM64、公共 API 和普通本地浏览器选择保持原有策略。

## Evidence

- [Windows 官方不可变清单](https://github.com/actions/runner-images/blob/ca1c9228ba0cdb8ac41a6f2b2ef44dfd6fe56b2b/images/windows/Windows2025-VS2026-Readme.md)：
  镜像 20261021.278.1，Chrome 156.0.8078.48、Edge 155.0.4283.62；对应官方 tag 为
  `win25-vs2026/20261021.278`。
- [Windows CI](https://github.com/hotlif/wake/actions/runs/40278776396/job/121427504853) 记录同一镜像与 Chrome CDP 版本。
- `scripts/check-system-browser-conformance.test.mjs` 使用实际 CDP 版本复现旧清单拒绝，
  并覆盖 Windows 156 准入、Linux 156 拒绝及两个平台的未知主版本拒绝。

## Consequences

Windows 新镜像可以进入完整实验性浏览器门禁，其通过结果仍须由 CI 验收。
平台间已审查范围可以不同，不从一个平台的镜像推断其他平台也支持同一主版本。
未来镜像滚动升级继续需要显式审查。

## Validation

- `corepack yarn browser:conformance:test`、`corepack yarn release:check`。
- `corepack yarn architecture:test`、`corepack yarn architecture:check`。
- Windows 云端完整 browser-conformance 与干净 npm 消费项目的发布前后浏览器冒烟。
- `node scripts/check-system-browser-conformance.mjs --stable-readiness blocked`。

## Supersedes

None.

## Amends

- [ADR 0073](0073-reviewed-browser-major-155.md): Windows x64 实验性浏览器证据允许的已审查主版本范围。

## Removal plan

无兼容桥。移除已审查主版本时，同步删除其证据并更新工作流矩阵与测试。
