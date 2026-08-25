# ADR 0008: 分离 Node 库打包与 Web 应用构建

- Status: accepted
- Date: 2026-08-17

## Context

Wake 的应用构建拥有 HTML、清单、浏览器目标、CSS 抽取和代码拆分。VS Code CSS 扩展需要的则是
单个 Node 20 CommonJS 文件、由宿主提供的 `vscode` 导入和精确输出路径。此前的 esbuild 路径在
Wake 之外重复拥有 TypeScript 打包职责。

## Decision

`wake_bundler` 拥有平台感知的依赖解析、显式外部依赖和入口模块格式。`wake_app::bundle` 拥有库契约
和精确文件写入；Rust CLI、npm CLI 与 Node-API 绑定只是该服务的壳层。Node 依赖边会激活 `node`
及该边的 `import` 或 `require` 条件，首个匹配导出由包作者的声明顺序决定。Node 包回退先选 `main`
再选 `module`；浏览器回退顺序相反。Web 应用构建行为保持独立且不变。

首个稳定 Node 契约是单个 CommonJS 文件。Node 内置模块自动视为外部依赖；配置的裸包名也会将其
子路径外部化。打包选项是调用输入，不属于 `wake.config.toml` 项目配置。

## Invariants

- 平台、格式、目标、外部包和依赖条件均参与缓存标识。
- 未解析的依赖绝不会被静默视为外部依赖。
- Node 与浏览器解析，以及 import 与 require 条件，不能共享缓存结果。
- 精确文件写入不会清理或替换同级产物。
- 精确的 JavaScript 和源码映射文件使用同目录唯一临时文件并原子替换。
- CommonJS 输出只把入口导出赋给 `module.exports`；同步入口图拒绝顶层 await，但动态导入之后允许。
- CLI 和 Node 行为通过 `wake_app` 收敛。
- 打包默认值和选项语义验证只由 `wake_app` 拥有。

## Evidence

- `crates/wake_resolver/src/lib.rs`
- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_app/src/lib.rs`
- `editors/vscode-css/scripts/build.mjs`
- 解析器、打包器、应用和 Extension Host 测试

## Consequences

Wake 可以构建 Node 托管工具而不生成 Web 应用产物。公共打包 API 增加 platform、format、target、
external、minify 和 outfile 选项。Node ESM、多入口、插件及任意路径外部依赖不属于首版契约。

## Validation

- 执行解析器条件和缓存隔离测试。
- 使用外部 Node 依赖执行生成的 CommonJS 输出。
- 运行 npm API/类型测试、架构检查、vscode-css 检查和 Extension Host 测试。
- 打包并检查所有受支持的 VSIX 目标。

## Supersedes

None.

## Removal plan

在采纳本决策的变更中删除 esbuild、其锁文件依赖图及 vscode-css 构建调用。不保留双打包路径或兼容标志。
