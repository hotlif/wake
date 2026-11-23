# ADR 0088: macOS arm64 的 Chrome 158 镜像准入

- Status: accepted
- Date: 2026-11-23

## Context

Wake 0.1.58 的 CI 在 macOS arm64 运行器上通过 CDP 识别到 Chrome 158.0.8127.37。既有主版本清单在 React 功能测试开始前拒绝了这份镜像。

## Decision

在 `darwin-arm64` 的实验性浏览器冒烟清单中加入主版本 158。CI、发布前 tarball 消费和发布后 npm 消费使用相同的清单。

## Invariants

每次运行均需提供系统浏览器的实际 CDP 身份，匹配平台及已审查主版本，并通过原有功能测试。稳定就绪仍要求五个目标通过同一精确主版本的一致性测试。

## Evidence

GitHub 镜像 `20261118.0550.1` 的软件清单记录 Google Chrome 158.0.8127.37，与本次运行器的 CDP 结果一致。[不可变镜像清单](https://github.com/actions/runner-images/blob/186066b10e735549f10370f053c1891445b20e96/images/macos/macos-15-arm64-Readme.md) · [CI 身份检查](https://github.com/hotlif/wake/actions/runs/45120362618/job/137512797184)

## Consequences

使用这份镜像的 macOS arm64 运行器可进入实验性功能测试。后续主版本仍需单独审查。

## Validation

运行 `browser:conformance:test`、`release:check`、`architecture:test`、`architecture:check` 和 `--stable-readiness blocked` 检查。由 macOS arm64 托管运行器验证浏览器功能及发布前后的消费用例。

## Supersedes

None.

## Amends

- [ADR 0080](0080-reviewed-linux-156-macos-arm64-157.md): 将 macOS arm64 的 158 从拒绝范围移入已审查范围。

## Removal plan

退役这一主版本时，同步更新镜像清单、CI、发布矩阵与对应测试。
