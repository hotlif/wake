# Wake 中文文档

`docs/` 是 Wake 面向使用者的中文文档源码。它按任务组织内容，页面路径就是公开 URL，导航顺序只由 [`navigation.toml`](navigation.toml) 管理。

## 内容边界

- `start/`：安装、创建应用和接入现有项目。
- `app/`：日常开发、构建与部署。
- `styles/`：普通 CSS 与 `@crab-dev/css`。
- `wake-docs/`：使用 Wake Docs 建设技术文档。
- `reference/`：配置、CLI、Node API、错误与术语。
- `site/`、`examples/`：本站运行组件和可执行示例，不进入公共导航。

UI 组件库拥有独立的发布节奏和文档系统，本目录不维护 `@crab-dev/rc-*` 组件参考。Wake 的编译器设计、测试、性能和发布流程统一放在 [`engineering/`](../engineering/README.md)。

## 页面规则

每个 `.mdx` 页面只声明自身信息：

```toml
+++
title = "动态样式"
description = "使用 CSS 自定义属性连接 React 状态与静态 CSS"
kind = "tutorial"
status = "experimental"
+++
```

路由由文件路径生成，例如 `styles/dynamic-values.mdx` 对应 `/styles/dynamic-values`。不要添加 `slug`、`group`、`group_order` 或 `order`；构建器会直接拒绝这些旧字段。新增页面后必须把页面 ID 加入 `navigation.toml`，除非页面明确声明 `hidden = true`。

## 本地验证

```powershell
npm run docs:check
npm run docs:build
cargo test -p wake_docs
```

文档检查会验证 Frontmatter、导航完整性、路由唯一性和内部链接。生产构建默认输出到 `docs-dist/`。
