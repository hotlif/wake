# ADR 0078: 原生 Worker 与静态资源 URL

- Status: accepted
- Date: 2026-11-11

## Context

应用使用 `new Worker(new URL('./worker.js', import.meta.url))` 和
`new URL('./image.svg', import.meta.url)`。这些引用不属于普通模块 import，不能将 Worker
合并进页面模块图，也不能通过正则替换源文本绕过 ADR 0033 的结构化输出约束。

## Decision

编译器 AST 识别未被局部绑定遮蔽的全局 `Worker` 和 `URL` 构造器。支持相对路径字符串与
`import.meta.url` 的静态组合；动态表达式、远程 URL 和局部同名构造器保持原语义。
bundler 负责读取资源、按内容生成文件名，并通过带原始 span 的 typed finalization 替换 URL
字符串。输出 URL 使用 `public_path`，查询与 fragment 后缀保留。文件路径按 UTF-8 百分号编码解码，
输出文件名编码为 URL 路径。缺失文件为带来源路径和 span 的构建错误。

Worker 使用独立的一次性构建会话顺序构建，自带运行时和依赖闭包。递归 Worker 引用必须检测循环。
Worker 及资源均归属引用它们的模块并进入现有输出事务；源读取沿用同一 FileSystem。每次构建重新观察
引用内容，生成 URL 进入 body linker identity，资源变化不允许复用旧 body。

首版静态 URL/Worker 限定为项目根内的文件，使用 FileSystem canonicalize 验证目标身份；跨根引用与
根内链接指向根外的目标以 WAKE0304 显式拒绝。开发与 build-watch
在构建前注册项目根 all-files tree，排除 `.wake`、`.git`、`.tmp`、`.yarn`、`node_modules`、
`target`、`dist`、`docs-dist` 和实际写入输出目录。保留现有 alias/source 与精确控制文件兴趣。
该范围覆盖 `scripts/` Worker、二进制资源与尚不存在的引用，直接复用现有候选提交、coverage 和 Rescan
协议；不在构建后悄悄增加未覆盖的依赖。代价是项目根内其他非排除文件编辑也可能触发构建。

## Invariants

- 资源引用由语法和绑定事实识别，禁止扫描生成文本或正则改写源码。
- Worker 不依赖页面的全局模块表，不继承 federation 页面上下文。
- 子构建顺序执行并在完成后释放临时编译状态。
- 开发和生产共享引用识别、构建与发布路径。

## Evidence

- `crates/wake_bundler/src/resource_refs.rs`
- `crates/wake_bundler/src/tests.rs` 的 `resource_url_` 与 `native_worker_` 测试。
- `crates/wake_ecma_minify/src/typed_modules.rs` 的 typed finalization。

## Consequences

支持标准静态 URL 写法，无需应用手写 Worker 构建脚本。引用模块在摘要缓存命中时可能需要恢复
parse 结果以获得引用事实；暂不扩展持久化 wire schema。仅 browser 构建提供资源发布语义。

## Validation

运行 wake_bundler 的资源、Worker 聚焦测试与所属 crate 门禁，以及编译器 typed finalization 测试。
覆盖遮蔽、缺失资源、资源变更、Worker 独立执行与循环诊断。
`wake_dev_server::tests::native_worker_watch_updates_and_recovers_outside_src` 验证真实 HTTP 服务中
scripts Worker 删除与恢复；`resource_watch_plan_covers_scripts_and_excludes_custom_output` 验证
build-watch 的默认覆盖和实际输出排除。ProjectFiles 过滤器在任意深度排除生成与依赖组件，普通
all-files public 兴趣仍保留原语义。

## Supersedes

None.

## Amends

- [ADR 0037](0037-typed-development-watch-and-candidate-generations.md): 默认 source tree 扩展为项目根 ProjectFiles tree，以在候选构建前覆盖静态 URL 和 Worker；精确控制文件、显式 alias/source 兴趣与 coverage/Rescan/候选发布协议保持既定所有权。

## Removal plan

无临时桥接。
