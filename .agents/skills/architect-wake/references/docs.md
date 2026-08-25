# Wake 文档

事实来源：`docs/navigation.toml`、`wake_docs`、`scripts/check-docs.mjs` 和公开页面。

- 文件路径决定路由，`navigation.toml` 决定可见层级与顺序，frontmatter 只决定页面元数据。
- MDX、Demo、Props 提取、运行时、搜索和静态外壳使用同一页面标识。
- 公开文档解释用户契约；实现、PnP 桥接和发布内部机制保留在 `engineering/`。

最低验证：`npm run docs:check`。运行时或视觉变更追加生产构建、控制台、DOM、无障碍、响应式、主题和减少动态效果检查；按风险运行 `cargo test -p wake_docs` 与 `npm run docs:build`。
