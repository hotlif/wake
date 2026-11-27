# ADR 0091: Linux x64 的 Chrome 158 镜像准入

- Status: accepted
- Date: 2026-11-27

## Context

Ubuntu 24.04 托管运行器的系统浏览器通过 CDP 报告 Chrome 158.0.8127.36。ADR 0083 的 Linux 主版本清单在功能测试开始前拒绝了这一身份。

## Decision

将 158 加入 Linux x64 的实验性 `reviewed-major-conformance` 清单。CI、发布前 tarball 消费和发布后 npm 消费使用相同的 Linux 清单。

## Invariants

Linux 每次运行都必须核验系统浏览器路径、实际 CDP 身份和 headless 状态，并通过原有功能测试。稳定就绪要求全部五个目标通过同一精确主版本的一致性测试。

## Evidence

Ubuntu 24.04 镜像 `20261122.23.1` 的官方软件清单记录 Google Chrome 158.0.8127.36，与 Linux CI 实际读取的 CDP 版本一致。[不可变镜像清单](https://github.com/actions/runner-images/blob/710e8ff2142ab0ff47974681dba1ae59b2ac4982/images/ubuntu/Ubuntu2404-Readme.md) · [CI 身份检查](https://github.com/hotlif/wake/actions/runs/45939236652/job/140309861043)

## Consequences

使用这份镜像的 Linux 运行器可进入实验性浏览器功能门禁。后续主版本继续单独审查。

## Validation

运行 `browser:conformance:test`、`release:check`、`architecture:test`、`architecture:check` 和 `--stable-readiness blocked`。Linux 托管运行器执行浏览器完整功能检查及发布前后的消费用例。

## Supersedes

None.

## Amends

- [ADR 0083](0083-reviewed-linux-browser-major-157.md): 将 Linux x64 的 158 从拒绝范围移入已审查范围。

## Removal plan

退役这一主版本时同步更新镜像清单、CI、发布矩阵和对应测试。
