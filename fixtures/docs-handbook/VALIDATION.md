# 用户文档重写验证记录

本轮在 Windows x64、Node.js 22.15.0、TypeScript 6.0.2 和仓库锁定依赖环境中验证。
日期：2026-09-10。CLI 与测试 host 使用当前源码构建出的本机二进制；Node API
使用仓库现有本机 addon。没有发布 npm 包、站点或公网 Federation remote。

## 文档与兼容路径

- 文档检查通过：80 个路由、全部导航项、Frontmatter、页面结构、公开配置字段与站内链接。
- 9 项搜索测试通过。
- 原有 53 个 MDX 文件和对应构建 HTML 全部保留；新增 27 页。旧文档正文没有带 fragment 的站内链接；当前所有站内链接与锚点通过文档检查。
- 完整文档站构建通过：120 个模块、188 个文件、80 个路由，包含代码高亮、Demo 和资源。
- 完整架构检查通过；未调整架构决策、验证器或门禁来适配文案。
- 逐页处理与九章归属见 [重写映射](../../docs/REWRITE-MAP.md)。

## 可执行路径

| 路径 | 已执行检查与结果 |
| --- | --- |
| 新建 React 应用 | fixture 严格类型检查、开发启动、生产构建通过；生产页面按钮可递增 |
| Live Reload | 开发页面计数为 1 后修改标题，页面刷新并显示新标题，计数恢复 0；已还原验证修改 |
| 别名 | `@/Counter` 的 TypeScript paths 检查与单文件 bundle 通过；说明了 PnP 对包形态请求的优先解析 |
| API 代理 | 本地 Node 后端与 Wake 开发服务器同时运行，`/api/health` 返回 `{"path":"/health","ok":true}` |
| 样式 | StaticCard bundle 成功，生成规则包含 `padding: 8px`；文档中的完整样式 Demo 随站点构建通过 |
| 测试 | 7 个测试文件、9 项测试通过，含函数 Mock、模块替换、异步、时钟、网络和 React |
| React 浏览器环境 | Counter 测试在真实浏览器环境通过，1 项测试 |
| 库构建 | ESM、CJS、声明生成通过；本地 npm pack 并安装到独立消费目录，import/require 调用及 TypeScript 消费检查通过 |
| 最小文档站 | 构建通过；浏览器点击 iframe 计数递增，源码面板可展开 |
| Docs UI | 默认、局部和完整覆盖的生产构建均通过；教程新增 TSX 代码通过严格类型检查 |
| Node API | `tools/rebuild.mjs` 实际执行两轮重建；`verify-node.mjs` 验证重建一致性、结构化错误、HTTP 响应和资源关闭 |
| Federation | 本地 remote/host 开发启动及类型初始化成功，两个项目类型检查通过；浏览器动态调用显示 `Hello, Wake` |
| Federation lock | 现有 `wake_app` 中 `federation_lock` 聚焦测试通过，12 项 |

核心源码位于本目录。UI 使用相邻 [docs-ui fixture](../docs-ui/README.md)，完整 UI 功能的既有验证记录
见 [docs-ui/VALIDATION.md](../docs-ui/VALIDATION.md)。本轮没有重新执行其全部 Rust 与发布平台矩阵。

## 浏览器检查

使用真实 Chromium 检查桌面 1280px 和移动 390px 宽度：

- 首页与九章导航、快速开始深层链接、长篇代码块、Docs UI 参考表格可阅读；文档没有横向溢出，长代码和表格在自身区域滚动。
- 浅色与深色页面均检查。移动导航可进入新增 UI 教程，搜索 Mock 命中新教程，Enter 选择后关闭弹层并将焦点移到目标 H1。
- 最小站点 Demo 可交互并展开源码。完整覆盖的 Demo 全屏时只有一个 iframe，Escape 关闭后仍只有一个；API 筛选保留匹配的 disabled 属性。
- 局部覆盖仍显示默认导航；局部和完整覆盖均在 MDX 中显示 Root 提供的 `shared-context`。
- 完整覆盖的内联与 Portal 探针保留自有 monospace 字体和颜色，私有背景变量为空。局部覆盖的内联探针保持自有字体，但仍能继承默认 Page 的私有背景变量；Portal 不继承该变量。
- 以上正常页面交互未观察到浏览器错误日志。

## 明确的验证边界

CSS 选择器边界不阻断所有 CSS 自定义属性的继承。该限制已写入主题教程和 Docs UI 参考；
本轮没有把已有实现改写成 Shadow DOM 隔离，也不宣称全部变量污染验收通过。

安装示例使用仓库已有锁定依赖验证，未重新从空缓存下载每个公开 npm 包。新 Docs UI 入口
使用当前源码验证，不能据此认为此前发布的所有版本均包含该入口。Node API 验证没有重新构建 addon。

没有执行公网 HTTPS Federation 发布/lock 流程、跨平台安装矩阵、全量 crate 回归、整套浏览器
conformance 或覆盖率发布门禁。对应页面保留 experimental/Beta 状态；本轮新增文档不改变功能等级。

## 复现入口

仓库依赖安装完成后，从根目录执行：

```bash
corepack yarn docs:check
corepack yarn docs:build
corepack yarn architecture:check
corepack yarn exec pnpify tsc -p fixtures/docs-handbook/app/tsconfig.json
cargo test -p wake_app --lib federation_lock
```

其余 CLI 命令与运行目录见 [README.md](README.md)。Node 脚本从已安装 Wake 的 app 目录运行；
仓库 PnP 验证可以从根目录使用 `node --require ./.pnp.cjs --loader ./.pnp.loader.mjs fixtures/docs-handbook/verify-node.mjs`。
如本机 addon 与平台包不同步，按仓库测试指南选择已构建的本机 addon。

临时构建、消费目录和日志统一位于根 `.tmp/docs-rewrite`，不是示例运行依赖。
