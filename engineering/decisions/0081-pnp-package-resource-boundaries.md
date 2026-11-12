# ADR 0081: PnP 包内静态资源边界

- Status: accepted
- Date: 2026-11-12

## Context

ADR 0078 的项目根边界拒绝了 Monaco 在项目外 Yarn 缓存中的包内静态 URL。
`fixtures/monaco-worker-yarn-pnp` 已通过真实 Monaco 复现 WAKE0304。

## Decision

保留项目根内资源行为。额外允许当前项目 PnP 清单管理的模块引用同一包内的静态资源与 Worker。
ResolutionEnvironment 独占包归属、逻辑路径及物理身份验证；bundler 不识别缓存目录名。
引用者与目标必须属于同一 locator，真实目标必须仍在真实包根内。读取与 Worker 子构建保留逻辑路径。
本次不扩大经典 node_modules 或任意根外源码的授权范围。

开发与 build-watch 在候选构建之前从项目 PnP 清单注册安装包的物理 archive 精确兴趣与非归档包的
all-files tree 兴趣。排除项目自身的包根，沿用现有项目兴趣。安装图控制文件变化重新推导覆盖，
沿用现有 coverage/Rescan 发布协议。包内资源变化与归档覆盖使下一构建读取新字节。

## Invariants

- 不允许跨包、符号链接逃逸或未管理的根外资源；失败保留 WAKE0304。
- 物理路径仅用于边界验证、监听和失效；virtual locator 身份不折叠。
- 不增加 Monaco 特判、缓存目录白名单或关闭边界的配置开关。
- 不在已发布构建之后补注册资源兴趣。

## Evidence

- `crates/wake_resolver/src/environment.rs`
- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_app/src/lib.rs`
- `fixtures/monaco-worker-yarn-pnp`

## Consequences

允许真实 PnP 依赖的包内资源；预先覆盖安装图会增加监听范围，但避免资源发现与发布之间的覆盖空窗。
静态 JavaScript URL 继续按资源发布，不把普通 URL 隐式改成 Worker 构建。

## Validation

resolver 的包身份与物理逃逸测试，bundler 的根外包内资源、跨包拒绝和归档失效测试，
app 的预注册兴趣测试，Monaco fixture 构建及产物字节检查；相关 crate 门禁和架构测试/检查。

## Supersedes

None.

## Amends

- [ADR 0078](0078-native-worker-resource-urls.md): 项目根边界增加由项目 PnP 清单证明的同包资源范围；保留静态 URL、独立 Worker 和发布事务语义。
- [ADR 0037](0037-typed-development-watch-and-candidate-generations.md): 默认候选监听计划预注册 PnP 安装包归档与目录；覆盖确认和 Rescan 协议不变。

## Removal plan

无临时桥接。
