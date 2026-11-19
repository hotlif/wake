# ADR 0086: Windows 157 与 macOS Intel 浏览器运行器修复

- Status: accepted
- Date: 2026-11-19

## Context

Wake 0.1.56 的合并验证在 Windows x64 镜像 20261116.315.1 上取得 Chrome 157.0.8103.79，旧准入范围在功能测试前拒绝了该身份。同一轮 macOS Intel 镜像 20260824.0482.1 的 Chrome 在验证期间从 151.0.7922.174 变为 157.0.8103.79，React 输入用例在后一版本超时。

## Decision

Windows x64 在实验性门禁中加入主版本 157。macOS Intel 的 CI、发布前和发布后冒烟统一选择运行器镜像预装的 Chrome for Testing，避免测试期间更换浏览器版本。macOS Intel 仍按 150、151 审查实际 CDP 身份。

## Invariants

- 使用运行器预装浏览器。路径缺失或 CDP 身份超出审查范围时，门禁失败。
- 原有浏览器功能断言、超时和 stable readiness 策略继续生效。

## Evidence

- [Windows CI](https://github.com/hotlif/wake/actions/runs/44444096321/job/135292324091) 和 [Windows 官方镜像清单](https://github.com/actions/runner-images/blob/9aa6d32300c1b8caaeb4f01efdcd619261422fd4/images/windows/Windows2025-VS2026-Readme.md) 记录了同一镜像与 Chrome 版本。
- [macOS Intel CI](https://github.com/hotlif/wake/actions/runs/44444096321/job/135292324286) 记录了一次运行中的两个 Chrome 版本。[官方镜像清单](https://github.com/actions/runner-images/blob/f10516542b8f2fef89b652a8d5f5de63aa543b77/images/macos/macos-15-Readme.md) 记录预装的 Chrome for Testing 151.0.7922.138。[镜像构建脚本](https://github.com/actions/runner-images/blob/f10516542b8f2fef89b652a8d5f5de63aa543b77/images/macos/scripts/build/install-chrome.sh) 给出其安装位置。

## Consequences

新版 Windows 镜像进入原有浏览器一致性门禁。macOS Intel 在各阶段从显式路径启动镜像预装的测试版 Chrome，功能结果仍由托管运行器验证。

## Validation

`corepack yarn browser:conformance:test`、`corepack yarn release:check`、`corepack yarn architecture:test`、`corepack yarn architecture:check`。在 Windows x64 和 macOS Intel 托管运行器复核 CDP 身份及功能门禁。

## Supersedes

None.

## Amends

- [ADR 0020](0020-react-browser-test-runtime.md): macOS Intel 浏览器证据选择镜像预装的 Chrome for Testing。
- [ADR 0074](0074-reviewed-windows-browser-major-156.md): Windows x64 的实验性浏览器准入新增已审查的主版本 157。

## Removal plan

后续更新运行器时，同步复核路径、CDP 版本和官方镜像清单。
