# ADR 0048: Windows 发布浏览器采用已审查主版本清单

- Status: accepted
- Date: 2026-09-11

## Context

Windows 托管镜像已从 Chromium 151 滚动升级到 152。0.1.36 发布诊断中，系统 Chrome
152.0.7977.83 成功执行 React 渲染与截图，但被旧的主版本 151 准入策略拒绝。

## Decision

Windows x64 的实验性浏览器证据采用 `reviewed-major-conformance`，明确允许主版本 151 和 152。
每个主版本必须具备固定提交的官方 runner 清单证据，并执行现有完整一致性检查。
Windows 官方清单路径仅允许 Windows2025 与 Windows2025-VS2026 两种已审查镜像。
后续主版本升级仍须审查并更新证据，不自动扩大允许范围。

## Invariants

- 只使用系统浏览器，保留 CDP 后完整版本和 headless 检查；不下载或回退浏览器。
- CI 的 CDP、React、输入、源码映射、截图、Federation 和覆盖率检查，以及发布前后冒烟继续阻塞失败。
- 多主版本证据不满足稳定一致性。五目标共享精确主版本 151 的稳定就绪策略保持不变，结果仍为 blocked。
- 普通本地测试的兼容浏览器选择、运行时与公共 API 不变。

## Evidence

- [官方 Windows 镜像清单](https://github.com/actions/runner-images/blob/c240f76fa0dd523af7376dbe8480964a3cb0af47/images/windows/Windows2025-VS2026-Readme.md)：
  镜像 20260907.229.1，Chrome 152.0.7977.83、Edge 152.0.4191.66。
- [发布诊断](https://github.com/hotlif/wake/actions/runs/34556149161)：实际浏览器测试成功，旧版本准入检查失败。
- `engineering/system-browser-conformance.json` 保留 151 的原始证据并新增 152 清单。

## Consequences

实验性发布可以覆盖已审查的 Windows 镜像滚动升级；Windows 不再产生稳定一致性通过声明。
未审查主版本、错误平台清单、可变来源和不完整证据仍被拒绝。

## Validation

- `corepack yarn browser:conformance:test`、`corepack yarn release:check`。
- `corepack yarn architecture:test`、`corepack yarn architecture:check`。
- Windows 云端完整 browser-conformance 与干净 npm 消费项目的发布前后浏览器冒烟。
- `node scripts/check-system-browser-conformance.mjs --stable-readiness blocked`。

## Supersedes

None.

## Amends

- [ADR 0020](0020-react-browser-test-runtime.md): Windows x64 实验性浏览器证据的主版本准入与稳定一致性声明。

## Removal plan

无兼容桥。移除某个已审查主版本时，同步删除其证据并更新工作流矩阵与测试。
