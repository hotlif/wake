# ADR 0018: 通过隔离的应用挂载聚合 Docs 工作区

- Status: accepted
- Date: 2026-08-21

## Context

Wake Docs 每次调用只生成一个站点或一个组件工作台。因此，组件仓库需要外部进程和路由层，才能在同一
文档源下呈现多个包工作台。把每个包都合入站点 bundle 会违反 ADR 0002 建立的运行时隔离，造成跨包
演示标识符，并使开发启动时间与整个仓库规模成正比。

生产输出还使用先清理后重写的应用发射器。通过该路径发布多个独立打包的应用，可能暴露部分更新的目录树；
在 Windows 上，若生成文件仍处于打开状态，还可能失败。

## Decision

`wake_config` 只拥有声明式 `[[docs.workspace]]` 发现规则。`wake_app` 解析直接子目录、加载各子配置、
验证名称和挂载路径，并编排生产或开发应用。`wake_docs` 仍是单项目生成器。

父站点和每个组件工作台分别拥有生成的项目、解析器配置、`BuildSession`、bundle 状态、公共目录、
公共路径及 HTML 壳层。共享的 `wake_dev_server` 拥有一个 HTTP 端口、一个监视器编排线程、一个最长
前缀挂载注册表和一个 HMR 端点。立即挂载项在启动时构建；惰性挂载项在首次请求其 HTML、分块或资源
时通过 single-flight 转换构建。HMR 消息携带挂载标识，使页面只响应自身应用。

生产环境将每个挂载项构建到隔离的暂存子树中。所有构建和冲突检查成功后，事务性逐文件提交器会跳过
相同文件、安装变更文件、删除过期文件，并在后续操作失败时恢复备份。根清单引用按确定顺序排序的子清单。
公共构建结果仍将站点 `routes` 与 `demos` 分开，并通过新的 `workspaces` 数组报告工作区。

## Invariants

- 站点 `base_path`（包括 CLI 或 Node 覆盖值）为每个工作区挂载添加前缀。
- 工作区规则只发现直接子目录；名称是区分大小写且 URL 安全的路径段。
- `*` 和 `?` 可匹配名称，但绝不匹配路径分隔符。
- 工作区挂载必须唯一且互不重叠，不能替换父路由或输出文件，并按最长基础路径优先于站点匹配。
- 除非精确匹配配置的工作台挂载，否则 `/components/<name>/` 仍是站点路由。
- 请求会拒绝解码后的 `.`/`..`、反斜杠、编码路径穿越及公共目录符号链接逃逸。
- 缺失的文件型请求返回 404；首次构建失败的惰性挂载返回 503，且不影响其他挂载。
- 更改工作区拓扑需要重启开发服务器；普通子配置变更只重新生成已加载的对应挂载。

## 展示方式

聚合组件工作台默认为 `embedded`。嵌入式展示只渲染预览界面，同时保留演示 hash、Args、主题传播、
iframe 隔离和运行时诊断。`standalone` 保留完整目录、工具栏、控件、对话框和抽屉。直接使用
`--mode components` 时仍默认 standalone。

## Evidence

- `crates/wake_app/src/lib.rs`
- `crates/wake_dev_server/src/lib.rs`
- `crates/wake_docs/src/lib.rs`
- `fixtures/react-docs-workspaces/`
- `fixtures/react-components-yarn-pnp/`
- 实现任务中记录的聚焦 Rust、Node API、生产夹具、惰性挂载和 HTTP 路由检查。

## Consequences

开发服务器成为通用多挂载所有者；普通应用和单 Docs 服务器则是同一引擎的单挂载用法。不新增 crate
依赖边。准备已配置的惰性工作区时会生成其小型虚拟源码树，但在请求该挂载前，不会创建解析器、模块图
或 bundle 会话。

Node `DocsBuildResult` 新增必需的 `workspaces` 数组。开发事件新增可选的 `workspace`、`basePath`
字段和 `workspaceState`。现有单项目使用方收到空数组，事件中不含工作区字段。

## Validation

使用真实的一站点/两工作区夹具、Yarn 4.16 PnP 聚合门禁、51 个惰性挂载启动测试、聚焦 Rust 测试、
Node 类型与测试、文档检查、架构检查、工作区 Clippy/测试、生产 Docs 构建及 `git diff --check`。

## Supersedes

None.

## Removal plan

在同一变更中删除原先对单 bundle HTTP 路由的假设。没有并行聚合路由器或兼容服务器需要日后删除。
