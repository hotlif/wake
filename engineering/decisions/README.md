# Wake 架构决策记录

本目录保存长期有效的架构决策。源码和测试仍是实现事实；ADR 说明为何选择某种架构、它保护哪些
不变量，以及未来如何替换它。ADR 文件中的状态和关系是唯一事实源；下方索引只是由架构检查器验证的
路由视图，不另设 JSON 清单。

## 入口与读取方式

会改变 crate/产品所有权、依赖方向、缓存身份、并发或发布事务、公共编译/运行时契约的任务，自动使用
`.agents/skills/architect-wake/SKILL.md`。Skill 先读取本页，按领域定位 ADR，再读取命中的当前决策及其
直接 `Supersedes`/`Amends` 关系；不默认遍历全部历史记录。

每份 ADR 只属于一个主领域。领域用于路由，不表示代码目录，也不取代 ADR 内的证据与边界说明。

## 决策索引

<!-- ADR-INDEX:START -->
### governance

- [ADR 0001: 将架构演进变为可执行闭环](0001-architecture-evolution-loop.md) — `superseded`
- [ADR 0044: 以 Skill 与验证索引路由架构决策](0044-architecture-decision-routing.md) — `accepted`

### compiler

- [ADR 0003: 明确编译器阶段和壳层依赖](0003-compiler-and-shell-boundaries.md) — `accepted`
- [ADR 0023: Closure 风格压缩管线所有权](0023-closure-style-minifier-pipeline.md) — `accepted`
- [ADR 0024: Linker-proven barrel and trivial-module compaction](0024-linker-proven-barrel-compaction.md) — `accepted`
- [ADR 0033: Structured module emit provenance](0033-structured-module-emit-provenance.md) — `accepted`
- [ADR 0042: Linker-owned `export *` resolution](0042-linker-owned-export-star-resolution.md) — `accepted`
- [ADR 0043: React 单模块编译边界](0043-react-module-compiler-boundary.md) — `accepted`

### build

- [ADR 0008: 分离 Node 库打包与 Web 应用构建](0008-node-library-bundle.md) — `accepted`
- [ADR 0013: 新增原生组件库产品边界](0013-native-component-library.md) — `proposed`
- [ADR 0026: Wake-owned failure-atomic output publication](0026-owned-failure-atomic-output-publication.md) — `accepted`
- [ADR 0027: BuildSession owns product compilation and engine lifetime](0027-build-session-ownership-and-lifetime.md) — `accepted`
- [ADR 0028: BuildGeneration owns coherent product publication](0028-build-generation-ownership-and-observation-cache.md) — `accepted`
- [ADR 0030: 普通开发更新采用诚实的 Live Reload 能力边界](0030-live-reload-capability-boundary.md) — `accepted`
- [ADR 0034: Transactional persistent-cache boundary](0034-transactional-persistent-cache-boundary.md) — `accepted`
- [ADR 0036: Input-disjoint exact-output transactions](0036-input-disjoint-exact-output-transactions.md) — `accepted`
- [ADR 0037: Typed development watches and isolated candidate generations](0037-typed-development-watch-and-candidate-generations.md) — `accepted`
- [ADR 0039: Owned immutable generated-input overlay](0039-owned-immutable-filesystem-overlay.md) — `accepted`

### css-editor

- [ADR 0004: 确定样式运行时所有权并限定 Docs CSS 桥接](0004-style-runtime-and-docs-css-bridge.md) — `accepted`
- [ADR 0005: 将抽取样式变为分块所有的产物](0005-chunk-owned-style-artifacts.md) — `accepted`
- [ADR 0006: 将 Crab CSS 设为唯一公共 CSS-in-JS 契约](0006-crab-css-public-contract.md) — `accepted`
- [ADR 0007: 在编辑器产品下层拥有 CSS 语言智能](0007-css-language-intelligence.md) — `accepted`
- [ADR 0009: 将语义绑定作为编辑器高亮的唯一权威](0009-semantic-css-highlighting.md) — `accepted`
- [ADR 0010: 由 wake_css 统一拥有所有 CSS 语法](0010-shared-css-syntax-tree.md) — `accepted`
- [ADR 0012: 显式标记跨模块 token 结构为不可变](0012-explicit-immutable-token-values.md) — `accepted`
- [ADR 0014: 为嵌入式 CSS 值赋予主题感知的语义标识](0014-theme-aware-css-semantic-values.md) — `accepted`
- [ADR 0015: 将自动补全触发器限定在嵌入式 CSS 内](0015-scope-embedded-css-completion-triggers.md) — `accepted`
- [ADR 0035: Parser-owned Crab runtime resolution](0035-parser-owned-crab-runtime-resolution.md) — `accepted`

### docs

- [ADR 0002: 分离 Wake Docs 运行时界面并强制执行内容契约](0002-docs-runtime-and-content-contracts.md) — `accepted`
- [ADR 0018: 通过隔离的应用挂载聚合 Docs 工作区](0018-docs-workspace-aggregation.md) — `accepted`
- [ADR 0031: Wake Docs 统一页面身份、语法所有权与源码溯源](0031-docs-page-identity-and-source-provenance.md) — `accepted`
- [ADR 0038: Docs generation directory transaction](0038-docs-generation-transaction.md) — `accepted`
- [ADR 0041: Cross-process Docs generation transaction](0041-cross-process-docs-generation-transaction.md) — `accepted`

### federation

- [ADR 0025: Wake-native federation contract and identity boundary](0025-wake-native-federation-contract.md) — `accepted`
- [ADR 0029: Node contracts and Federation control stay product-owned](0029-node-contract-and-federation-control-ownership.md) — `accepted`
- [ADR 0032: Federation 开发快照使用浏览器租约与有界竞态窗口](0032-federation-development-snapshot-leases.md) — `accepted`
- [ADR 0040: Parser-owned frozen declaration graph](0040-parser-owned-frozen-declaration-graph.md) — `accepted`

### testing

- [ADR 0019: 拥有原生 JavaScript 测试运行时](0019-native-test-runtime.md) — `superseded`
- [ADR 0020: 拥有以 React 为先的浏览器测试运行时](0020-react-browser-test-runtime.md) — `accepted`

### node-release

- [ADR 0011: 由产品拥有自动包发布流程](0011-release-automation.md) — `accepted`
- [ADR 0021: 用根级本地链接拥有平台 optional 包](0021-local-platform-package-links.md) — `superseded`
- [ADR 0022: Yarn PnP 权威与 npm 经典解析兼容](0022-yarn-pnp-ownership.md) — `accepted`

### cli

- [ADR 0016: 交互式终端控制台契约](0016-interactive-terminal-console.md) — `accepted`
- [ADR 0017: 带源码位置的终端诊断](0017-source-located-terminal-diagnostics.md) — `accepted`
<!-- ADR-INDEX:END -->

索引必须覆盖除 `0000-template.md` 外的每份 ADR，且每份恰好出现一次。条目的编号、标题、文件名和状态
必须与 ADR 一致；领域内按编号升序排列。`superseded` 与 `rejected` 记录仍留在索引中，以保存可发现的
决策历史。

## 生命周期

允许的状态值：

- `proposed`：决策尚在验证，不能作为已实施边界的权威；
- `accepted`：决策已采纳并反映在仓库中；
- `superseded`：整份决策的当前权威已转移到后续 ADR；
- `rejected`：已评估但未采纳。

决策状态与产品成熟度分离。实验性产品只要其当前实现边界已被采纳，ADR 仍应为 `accepted`，并在正文
单独记录产品成熟度。`proposed` ADR 可以进入索引，但在转为 `accepted` 前不能激活 `Supersedes`、
`Amends` 或机器边界引用。

## 关系格式

`## Supersedes` 是必需章节，只表示完整替代。无替代时内容必须恰为 `None.`；有替代时逐行使用：

```md
- [ADR 0001](0001-architecture-evolution-loop.md)
```

被完整替代的旧 ADR 必须改为 `superseded`，并在头部添加唯一反向链接：

```md
- Superseded by: [ADR 0044](0044-architecture-decision-routing.md)
```

仍然有效、但局部约束被改变的 ADR 使用可选 `## Amends`；它必须位于 `## Supersedes` 与
`## Removal plan` 之间，每条关系必须写明非空范围：

```md
- [ADR 0025](0025-wake-native-federation-contract.md): decision 15 的开发快照保留边界
```

被修订的 ADR 保持 `accepted`，并在头部为每个修订者添加一条反向链接：

```md
- Amended by: [ADR 0032](0032-federation-development-snapshot-leases.md)
```

背景依赖、延伸或相关资料只放在正文，不创建 `Depends on`/`Related` 关系。`Supersedes` 与 `Amends`
以及头部反向链接必须互相一致；同一 ADR 对不能同时使用两种关系。

## 新建与更新规则

基于 `0000-template.md` 新建连续编号的 ADR。必需章节各出现一次，并按 `Context`、`Decision`、
`Invariants`、`Evidence`、`Consequences`、`Validation`、`Supersedes`、可选 `Amends`、`Removal plan`
排列。上下文变化后不重写旧决策的论证；用新的 ADR 完整替代或定点修订，并维护索引和双向关系。

`corepack yarn architecture:check` 验证编号、日期、状态、章节顺序、关系闭合、索引覆盖，以及机器边界策略
只引用 `accepted` ADR。
