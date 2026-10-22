# Wake Lint 实施记录

本文件由 [LINT.md](../LINT.md#实施日志与门禁) 的实施日志迁入，保留原有顺序和记录时的结论。
未注明日期的条目不推定发生时间；规则数量、pipeline 版本、测试结果及“当前”“尚未完成”均指当时的代码状态。
当前契约与 P0–P7 完成度见 [Lint 阶段表](../LINT.md#范围与完成定义)，待办见 [ROADMAP.md](../ROADMAP.md)。

## 实施记录

- 0.1.40 候选发布汇总：77 条基线规则、CLI/Node 项目流程、LSP 和独立 SDK 已拆分提交。
  parser v41 / 核心 v74 保留类型导入、ambient 模块、声明请求及动态 import/require 的 UTF-16
  字符串身份；解析器无法接收的路径明确报告 unresolved，不把已知字面量误作动态未知请求。
  最新应用、核心、声明生成和 LSP 回归已通过。静态运行时模块说明符、完整高级类型语义、
  第三方规则宿主与完整 P7 验收仍未完成，本次发布不代表 P0–P7 全部完成。

- `ts/restrict-template-expressions` 已通过类别/参数、联合/交叉/约束、缺失事实/抑制三组核心
  测试、真实编译器及未保存依赖测试，Node 边界 33 项、核心全量、应用 lint 单元 43 项、
  Clippy 和文档检查通过。泛型实例的声明目标关系已补齐，两条类型规则的泛型继承正反例通过。
  已实现 69/77 条规则，核心 v30；不安全成员访问继续实施，P5 及完整 P0–P7 尚未完成。

- `ts/no-unsafe-call` 已通过四组核心类型事实/规则测试、真实原生编译器符号与签名验证、
  五组项目/基线/恢复/监听集成、13 项 CLI、33 项 Node 边界和 76 项 Wake 门禁。
  核心/config 全量、42 项应用 lint 单元、38 项既有应用集成、Clippy、架构、公开类型与文档检查通过。
  当前实现 68/77 条规则，parser v24、核心 v29、压缩管线 v20。P5 剩余九条类型规则，
  P1 原始 ambient 值绑定及 P6/P7 尚未完成；不能将首条类型规则接入视为全计划完成。

- 2026-09-14：`ts/await-thenable` 完成 parser-owned `await` 操作数范围、标准 Promise 身份及
  派生/联合/交叉类型证明；`any`、`unknown`、error 与缺失类型事实保持 fail-closed。核心三项
  正反例、原生 TypeScript 项目查询和 Unicode/未保存源码路径通过；类型规则计数更新为 70/77，
  P5 尚余六条规则，P6/P7 与平台/tarball/真实项目验收仍未完成。

- 2026-09-14：`ts/no-unnecessary-type-assertion` 完成 `as`、角括号和 `satisfies` 断言的原始
  范围及操作数/断言类型事实绑定；非空断言仍由语法规则负责，any/unknown/error 与缺失事实
  fail-closed。核心类别/结构正反例和原生 TypeScript 项目查询通过；P5 尚余五条类型规则，P6/P7
  与真实项目、平台和 tarball 验收仍未完成。

- 2026-09-14：`ts/no-floating-promises` 完成表达式语句边界、调用返回类型身份和标准 Promise
  证明；await、赋值、return 与显式 void 消费不报告，缺失返回事实 fail-closed。核心两项及
  原生 TypeScript 项目查询通过；P5 尚余四条类型规则，P6/P7 与真实项目、平台和 tarball
  验收仍未完成。

- 2026-09-14：`ts/no-unsafe-assignment`、`ts/no-unsafe-return`、`ts/no-misused-promises` 与
  `ts/switch-exhaustiveness-check` 完成 parser-owned 初始化/赋值、return、同步条件和 switch
  判别式事实；四条规则均默认关闭、无修复，缺失类型事实 fail-closed。核心正反例和原生
  TypeScript 项目查询通过，P5 十条规则均已接入；字面量覆盖证明、LSP/SDK、真实项目矩阵、
  平台和 tarball 验收继续在 P6/P7。

- 2026-09-14：新增 `wake_lint_lsp` stdio 服务，支持 workspace root、打开/全量变更/保存/关闭文档、
  取消旧诊断、UTF-16 坐标发布和安全修复的 quick-fix code action；新增 `editors/vscode-lint`
  客户端，使用 `wakeLint.serverPath` 启动同一服务。Node SDK 的 `lint`、`LintContext` 和 watcher
  入口已复用同一项目快照契约。
  LSP 坐标单测、Rust clippy 与扩展 manifest 检查通过。

- 2026-09-14：新增 `scripts/lint-migration-report.mjs`，把全部已登记原生规则、未命名空间规则和
  `@typescript-eslint/*` 别名及已知 preset 映射为原生配置，并对未知规则、parser/plugin/processor
  等顶层配置生成稳定的 `wake.lint.migration.v1` 报告；迁移单测和 `npm/css`、`npm/wake` dry-run
  tarball 检查通过，并以规则注册表 77 个 ID 做全量映射回归。
  平台原生包验证仍需在对应构建机刷新二进制和 manifest 后完成。

- 2026-09-14：新增 `wake_lint_core` Criterion lint 基线（16 KiB/128 KiB 源码，覆盖 parser-owned
  facts 与 recommended 规则），并接入 `scripts/run-performance.mjs` 的 key/full 比较矩阵；基准
  `--no-run` 编译通过。VS Code 扩展构建脚本改用 `fileURLToPath`，Windows 路径不再依赖 URL pathname
  的 POSIX 形态。

- 2026-09-14：P5 十条类型规则均已有规则注册、核心正反例、parser-owned 查询事实、TypeScript
  原生项目查询和 fail-closed 缺失事实校验；规则目录现含 77 个稳定 ID。LSP quick-fix 单测、
  lint Criterion 实际运行（约 22 MiB/s，16 KiB；约 24 MiB/s，128 KiB）与迁移 preset/顶层配置
  报告单测通过。

- 2026-09-14：以真实 stdio JSON-RPC 客户端完成 LSP 协议 smoke：workspace 初始化、TSX 打开后的
  诊断发布、quick-fix `WorkspaceEdit` 及 shutdown/exit 全链路通过；stdin 虚拟文件名现在按 workspace
  root 生成相对路径，配置匹配和未保存文档规则保持一致。
  该流程固化为 `scripts/lint-lsp-smoke.mjs`，可在构建 `wake-lint-language-server` 后重复运行。

- 2026-09-14：接入声明式 `[lint.processors]` 内建 Markdown 处理器。处理器只暴露带 `js`、
  `javascript`、`mjs` 或 `cjs` 标签的 fenced 代码，按原文 UTF-8 字节范围发布诊断；非代码块内容
  以等长空白遮蔽，缓存绕过处理器虚拟源码，`--fix`/写回在无法证明可逆映射时返回
  `WAKE_LINT_CONFIG`。配置解析、字节偏移、诊断和宿主文件不变性测试通过。

- 2026-09-14：新增 `@crab-dev/wake-lint-sdk` 独立扩展包和 `wake.lint.extension.v1` 协议。规则
  manifest、只读源码事实、UTF-8 编辑边界、确定性排序、异常隔离和 SDK 主版本升级检查均有
  安装级 Node 测试；该包不执行 Wake 配置、不暴露内部 AST，也不自动改变内建 lint 结果。

- 2026-09-14：扩展真实 CLI 项目 smoke 为 JS/MJS/CJS/JSX/TS/MTS/CTS/TSX/Markdown 矩阵，覆盖规则诊断、
  React 修复预览、Markdown fenced 诊断、`--print-config` 处理器回显和处理器修复拒绝；另以模块
  项目配置覆盖 JS `import/no-unresolved` 与 `import/no-cycle` 缺失/环依赖；在构建后的
  `target/debug/wake` 上通过。

- 2026-09-14：真实 CLI smoke 增加 npm package、workspace monorepo 和 PnP 风格 manifest 三类独立
  项目根 fixture，分别验证 package 边界、跨 workspace 文件发现、依赖声明和 PnP manifest 读取；
  三类 fixture 与既有多语言/模块项目矩阵均在构建后的 `target/debug/wake` 上通过，真实第三方
  仓库仍需在目标平台继续验证。

- 2026-09-14：CI 新增 Windows/Linux `lint-product` 矩阵，构建同一 CLI 与 LSP，执行核心/应用/LSP
  测试、真实项目矩阵、协议 smoke、迁移报告和独立 SDK 测试；本机已复跑对应命令，目标 runner
  的正式证据随 CI 保留。

- 2026-09-14：恢复 Yarn PnP 归档后，架构检查与 59 项架构行为测试通过；锁文件门禁验证 10 个
  workspace，CSS、Wake 和 `wake-lint-sdk` 定向 npm 打包通过。全目标平台打包仍等待匹配当前
  workspace 版本的 Windows 原生包构建。

- 2026-09-14：`ts/switch-exhaustiveness-check` 增加 parser-owned 原始 case 值和类型服务字面量事实，
  对字符串、数字、布尔、`null`、`undefined` 联合完成无 `default` 的覆盖证明；不完整字面量和
  动态 case 仍报告，核心与原生 TypeScript 项目正反例通过；lint core/parser cache identity
  分别推进至 v38/v32。随后接入 `case Enum.Member` 的 CaseClause 类型查询，枚举成员缺失/完整
  覆盖的核心与原生项目验收通过。

- 2026-09-14：补充版本化 `browser`/`node` lint environments。宿主集合由核心统一展开为只读
  globals，显式 globals 最后覆盖，未知环境在配置、CLI、Node API 和 `LintContext` 中均 fail-closed；
  `--print-config`、缓存身份、Rust/Node CLI 和 ESM/CJS API 均有正反例，核心流水线推进至 v39。

- 2026-09-14：修正 `--print-config` 的环境层叠顺序，使 override/request 的显式 globals 在宿主
  集合之后生效，并以应用规则执行回归测试证明解释结果与实际诊断一致；契约补入 ADR 0066。

- 2026-09-14：在当前 Windows TypeScript 7.0.2 原生包上运行完整 `lint::type_service` 忽略测试，
  23/23 通过（含项目、PnP/zip 文件系统、stdio transport、backend 生命周期，以及数字/`const enum`
  switch 覆盖）；P5 的本机类型服务证据不再依赖跳过项。

- 2026-09-14：新增跨 Windows/Linux runner 的 `scripts/find-native-typescript.mjs`，CI 在运行
  `lint::type_service -- --ignored` 前写入 `WAKE_LINT_TYPESCRIPT_EXE`；本机路径发现与 23/23
  原生类型服务测试通过，避免 CI 将显式原生门禁误当成跳过；新增脚本的 `--print`、GitHub
  环境写入和缺少环境变量失败契约测试。
- 2026-09-14：在同一 Windows TypeScript 7.0.2 原生编译器上运行 `typescript:7:check`，高级类型、
  类/函数、模块、TSX、值语义和严格类型负例兼容 fixture 通过。

- 2026-09-14：默认文件发现忽略 Yarn PnP 生成的 `.pnp.cjs`、`.pnp.mjs` 和 `.pnp.loader.mjs`，
  避免把第三方生成运行时当作用户源码；新增回归测试，并将 `react-ts-app`、`react-docs`、
  `react-components-yarn-pnp`、`react-ts-app-yarn-pnp` 和两个独立 Docs workspace 仓库 fixture 接入真实 CLI smoke，
  验证 React/TypeScript、Docs（含 `import/no-unresolved` 与依赖声明规则）、workspace 与 zip/PnP 项目根均只输出预期源码文件。外部第三方
  仓库与目标平台发布运行仍待验收。

- 2026-09-14：发布版本门禁将独立的 `@crab-dev/wake-lint-sdk` 从原生平台包集合中分离，
  `versions:check`、架构和定向 CSS/Wake/SDK tarball 门禁通过；全目标平台 tarball 仍需在
  对应构建机生成当前 workspace 版本的原生包。

- 2026-09-14：使用当前 Windows debug addon 与 test-host 完成 Node 原生 lint API 的 29 项
  消费测试（模块图、类型服务、globals/environments、修复、watch、基线、缓存、上下文与
  取消）；完整 Node API 运行仍需目标平台发布 addon/test-host 对齐后复跑。

- 2026-09-14：执行 `node scripts/run-performance.mjs --mode key --output .tmp/lint-performance`
  生成 Windows Criterion 对照报告。lint 16 KiB/128 KiB 冷基线分别为约 0.805 ms/5.457 ms；
  报告因基准文件在当前工作树新增而按契约标为 report-only，lexer 初测回归经重测降为 +5.4%，
  未触发确认性性能阻塞。

- 2026-09-14：获取并校验 pinned Windows Rusty V8 归档，构建当前 release addon/test-host，
  刷新 `@crab-dev/wake-win32-x64-msvc` manifest，并通过 native manifest 校验、Wake CLI/API
  及原生 lint Node 消费门禁（37 项 CLI/组件/终端与 61 项 API）。当前可构建目标的 tarball 检查已通过
  CSS、Wake、SDK 和 Windows 包，当前剩余阻塞是 Linux/macOS 平台包没有本机交叉构建产物，
  需对应目标 runner 生成并验证。

- 2026-09-14：使用当前 CSS、Wake、SDK、Windows 原生包及其他四个锁文件元数据包，在仓库外的
  经典 npm workspace 完成 consumer 门禁。`npm install`/`npm ci --ignore-scripts`、TypeScript
  类型检查、Wake build、跨 workspace 链接、CSS/Wake Test 入口均通过；Windows consumer
  证据已固化到 `scripts/check-npm-consumer.mjs` 的输出。Linux/macOS 原生包仍需对应 runner
  生成真实二进制后重复同一门禁。

- 2026-09-14：在真实外部仓库快照上补跑 lint 矩阵：Preact `8101ff8` 的 `src`/`hooks`/`compat`
  39 个文件及 p-map `bc8380d` 的纯 ESM 入口均由构建后的 Wake CLI 完成项目发现和 JSON 输出，
  显式关闭推荐规则后 `js/no-debugger` 均为零诊断并以退出码 0 完成；Preact 同时以
  `js/no-undef` 运行并产生预期的环境缺失诊断，证明规则诊断和退出码可用。外部仓库源码只在
  本轮临时目录使用，不进入仓库或发布包。

- 2026-09-14：使用官方 Zig 0.15.2 临时工具链交叉构建 Linux x64/ARM64 `wake_node`、
  `wake-test-host` 和 Wake CLI；为两个 Linux 目标下载并校验锁定的 Rusty V8 归档，生成
  `wake.linux-x64-gnu.node`（ELF machine `0x3e`）和 `wake.linux-arm64-gnu.node`（ELF machine
  `0xb7`），随后刷新两个平台包的 manifest/SBOM/第三方许可证并通过
  `verify-native-package.mjs`。Windows 本机 npm pack 对无扩展名 Unix test-host 只能报告
  `0644` 文件模式，故完整 `check-npm-packs.mjs` 的可执行位门禁仍保留到 Linux runner；
  macOS 交叉链接因缺少 Apple SDK（CoreFoundation/iconv）保持由 macOS runner 验收，不能用
  非 macOS 伪造二进制替代。

- 2026-09-14：`cargo test -p wake_lint_core --locked --offline` 全部规则、配置、模块图、类型、
  修复和基线测试通过，应用 lint/LSP 聚焦测试也通过；完整 `cargo test --workspace` 在本机
  因当前环境未提供 V8 预编译归档而停止于 `v8` 构建下载，不能把该失败归因于 lint。

- 2026-09-14：补齐并锁定 Windows Rusty V8 归档后，重新运行 `cargo test --workspace --locked --offline`，
  应用、lint、Node、测试宿主、协议、解析器、解析器语义、文档测试和所有 doctest 全部通过；
  结果为 221 个 `wake_app` 单元测试通过、其余 workspace 测试均无失败，依赖系统 Chromium 的测试继续按
  既有契约保持显式 ignored。

- 2026-09-14：收紧 P0 类型服务帧边界。stdio 读取器现在只接受单个大小写不敏感的
  `Content-Length` 头，带未知头的帧即使长度合法也会失败关闭；新增失败先行测试覆盖未知头、重复头、
  截断和超限输入，避免把非协议输出误当作可恢复的类型事实。

- 2026-09-14：`node scripts/check-release-coverage.mjs` 验证 npm 原生 lint 发布覆盖 8/8，
  VS Code 发布覆盖 5/5；真实平台二进制仍由 release workflow 的 Windows、Linux 与 macOS
  runner 生成并执行。

- 2026-09-14：将 `lint-product` CI 矩阵扩展为 Ubuntu、Windows、macOS，三者均发现对应
  TypeScript 原生宿主并执行相同的核心、应用、LSP、项目 smoke、协议 smoke、迁移和 SDK 门禁；
  本机仅能复跑 Windows，另外两个 runner 的结果由 CI 保留。

- 原生 TypeScript 7.0.2 stdio JSON-RPC 实验通过：直接文件系统回调、AST wire v5 节点查询、
  未保存依赖改变类型且旧快照仍保持原类型。候选边界进入 ADR 0064；尚未把类型规则计入
  67/77。模块规则的架构/文档/公开类型检查、Clippy 和 Wake 自有 76 项门禁通过。

- 六条模块规则已接入项目、stdin、修复、基线、版本化上下文与监听，当前 67/77 条规则；
  parser v23、核心 v28、压缩管线 v20。模块入口 7 项、监听 6 项、应用 lint 单元 24 项、
  CLI 12 项、真实 Node 32 项及配置/缓存/基线/上下文回归通过。PnP 根外缓存、嵌套权威和
  共享 ZIP 的不同虚拟 peer 保持独立图身份；更新物理 archive 与缺失依赖可自动恢复。
  ADR 0063 已接受；继续类型服务、十条类型规则与 P6/P7 验收，不把规则数量视为全计划完成。

- 模块核心图与六条规则的九组测试、图预算测试及核心全量回归通过，核心为 v28；公开
  `module-graph` 与数组/区域参数 schema 类型检查通过。稀疏文件系统快照四组测试通过。
  目前项目解析/CLI/Node/上下文尚未接通这些事实，因此完整交付计数仍为 61/77，不能把
  已注册的模块规则视为产品完成。ADR 0063 继续为 proposed，当前在接入解析权威和项目调度。

- 模块请求投影五组与原始 import-type 三组测试通过；parser v23、核心 v27，规则数仍为
  61/77。原始属性、字符串范围、类型边、遮蔽和推测回滚均有证据，parser/core/compiler/
  codegen 回归、Clippy、CLI 11 项、Node 31 项及文档/架构检查通过。模块解析快照和六条
  import 规则继续实施，ADR 0063 仍为 proposed。

- 两条 Hooks 规则已接入原生规则引擎，当前 61/77 条规则，parser v22、核心 v27、压缩管线 v20。
  调用路径八组、Hooks 调用位置七组、依赖检查十组及预算失败测试通过；核心全量回归、应用
  lint/config/cache/baseline/context/watch 回归、CLI 11 项、真实 Node 31 项、公开类型、Clippy、
  文档与架构检查通过。原始编译/codegen 差分回归保持通过；P4 剩余六条模块图规则，P5–P7
  和真实项目/平台验收继续执行。

- `style/indent` 的七项聚焦测试、原始 JSX 属性字符串两项 lexer/三项 parser 测试已通过。
  lexer/parser/semantic/lint/compiler/codegen/app 完整回归、Clippy、Node 边界 29 项、CLI 10 项、
  架构/CLI 联合 76 项、公开 TypeScript 参数 schema 与文档检查通过。当前 59/77 条规则，
  parser v22、核心 v25、压缩管线 v20；P4 剩余两条 Hooks 和六条模块图规则。

- `style/comma-dangle` 的六项核心测试、原始列表与 TSX 推测回滚测试、数字后 Unicode 空白
  词法回归均已通过。lexer/parser/semantic/lint/compiler/codegen、Clippy、Node 边界 28 项、
  架构与文档检查通过。当前 58/77 条规则，parser v21、核心 v24、压缩管线 v20。

- `style/semi` 的六项规则测试与三项 parser 原始终止位置测试已通过；公共 Node 入口 27 项、
  parser/lint/compiler/codegen 回归和 Clippy 通过。当前 57/77 条规则，parser v20、核心 v23、
  压缩管线 v20。类字段成员前缀分隔具有独立失败与修复证据，尾逗号规则继续实现。

- `js/no-unused-vars` 与 `react/jsx-uses-vars` 已通过原始值/类型身份、参数位置、正则过滤、
  导出、隐式复制、递归与动态访问的九项聚焦测试；类型消费者共享单次类型分析，重复声明区间
  使用索引查找。当前 56/77 条规则，parser v19、核心 v22、压缩管线 v20；核心回归和 Clippy
  已通过，公共入口 26 项、类型 schema、应用回归、架构与文档检查也已通过。尚未完成 P4
  其余规则或 P5–P7。

- `js/prefer-const` 已通过初始化、延后赋值、解构分组、循环、闭包写入、动态名称访问和安全
  修复的六项聚焦测试；当前 54/77 条规则，parser v19、核心 v21、压缩管线 v20。
  Unicode/私有标识符原始与转义一致性、lexer/parser/semantic/lint/compiler/codegen 回归和
  Clippy 已通过；新规则的公共入口和完整回归继续执行。

- lexer 标识符分类已由 std 字母/数字近似替换为锁定 Unicode 17.0.0 ID 属性，复用现有
  registry 包；所有权沿用 ADR 0003，依赖方向与机器策略不变。合法组合字符被拒绝、非法字母
  标记起始被接受的最小测试已从失败转为通过；parser 身份更新为 v19，完整回归继续执行。

- 显式 globals 已按 [ADR 0061](../decisions/0061-explicit-lint-global-bindings.md) 接入根配置、override、
  请求、配置解释、缓存、上下文及 Rust/npm CLI。默认标准集合冻结为 es2024@1；浏览器/Node
  集合仍待提供。`js/no-undef` 与 `react/jsx-no-undef` 的原始引用、typeof、TS/JSX 排除及抑制
  测试从失败转为通过，当前为 53/77 条规则，parser v18、核心 v20、压缩管线 v20。
  核心 v19 的应用回归、Rust CLI 10 项、真实 Node 23 项、类型检查及 CLI/架构测试 76 项通过；
  新规则的完整回归与公共入口验证继续执行。

- `js/no-redeclare`、`js/no-use-before-define` 和 `js/no-shadow` 的聚焦失败到通过已完成，
  源码声明身份、TS 合并、参数环境与隐式复制分别覆盖。当前为 51/77 条规则，parser v18、
  核心 v18、压缩管线 v20；完整回归和公共入口门禁继续执行。原始值语义、P5–P7 仍未完成。
- `ts/consistent-type-exports` 已接入双空间绑定事实；源码值声明的缺失名称由 semantic 集中
  记录，不能将擦除后的空值解析误认成纯类型导出。聚焦 import/export 测试通过；当前规则
  为 48/77，parser v18、核心 v15、压缩管线 v20。上一轮核心 v14 的 Node 21 项、CLI 10 项
  测试通过；本轮公共入口和回归门禁继续执行。
- 原始类型参数/映射/infer 范围、接口/别名/类/enum 声明与 namespace 容器已接入 parser；
  semantic 独立类型身份支持遮蔽、接口合并、namespace 导出和全局扩展，保留外部模块未知状态。
  `ts/consistent-type-imports` 消费类型与值身份，当前规则为 47/77，parser 为 v18、核心为 v14、
  压缩管线仍为 v20。聚焦失败到通过、parser/semantic/lint/compiler/codegen 回归、Clippy、
  架构与文档检查已通过；原始值声明/类型导出、剩余规则和 P5–P7 继续实施。
- 非严格块级普通函数的词法绑定、可选外层 var 和声明执行时复制已分开，覆盖参数/词法阻挡、
  if/case/catch/循环、重复函数与动态 arguments 目标。非法声明位置产生 parser 诊断。
  压缩器从当前 IR 识别兼容性结构并保留声明和控制流；`.cjs` 的构建及库入口使用 Script。
  parser/semantic/minifier/codegen/compiler/lint 回归、完整 bundler 回归与真实 CommonJS 运行、
  Clippy、CLI 10 项、原生 Node 20 项通过。箭头源码身份在实际降级前后保留，合成 arguments
  不能成为原始类型查询目标。当前 parser 为 v17、核心为 v13、压缩管线为 v20，规则数为 46/77；
  原始 TS 声明、类型空间及完整产品阶段仍继续实施。
- 类型查询值绑定切片：函数范围由原始产生式保存，semantic 附加环境区间后使用单次有序扫描
  解析查询；缺失的 TS 环境保留 Unavailable。没有类型查询时不构建该索引；普通编译的符号、
  声明和引用表保持相同。parser 为 v15，核心为 v12，压缩管线仍为 v19；规则数仍为 46/77。
  parser、semantic、compiler、lint crate 回归通过；完整类型空间及 Annex B 仍待补齐。
- React 列表 key 与数组索引规则已实现，当前 46/77 条规则。原始数组元素在实际 spread
  降级前后保持一致，不包含 JSX 注入的 children 数组；绑定模式仍须结合最终 AST 分类。
  覆盖 Fragment、括号/TS 断言、条件分支、包装调用排除、回调返回、同名遮蔽、动态 spread
  和真实注释抑制。回调参数身份建立一次索引，避免逐回调扫描整张绑定表。parser 为 v14，
  核心为 v11；压缩管线仍为 v19。
  parser/semantic/compiler/lint 回归、Clippy、CLI 10 项、项目配置 11 项及真实 Node 20 项通过；
  文档和差异格式检查通过。

- 隐式 arguments 已由同一个 semantic 所有者处理，普通与源码分析都显式接收当前 Interner。
  覆盖箭头捕获、独立普通函数、参数/函数/词法遮蔽和体内 var 的隐式初值；原始源码没有合成
  declaration occurrence。压缩分析保留 arguments 拼写/声明，调用参数特化识别其可观察性。
  语义 27 项及真实 Node 差分用例通过，涵盖 mapped 参数、索引/别名写入和 arguments.length。
  Rust 回归、相关 crate Clippy、bundler require 场景、CLI 10 项、前端/架构 75 项和原生 Node
  19 项通过。核心为 v10，压缩管线为 v19；解析器仍为 v13。

- 具名函数表达式已具有独立自引用环境，semantic 与当前 IR 的重建分析保持对应。两个失败
  语义用例转绿，默认参数闭包、体内同名 var/let、参数遮蔽与严格/非严格自引用赋值通过真实
  Node 差分验收。semantic/minifier/codegen/compiler/lint 回归和 Clippy 通过；核心为 v9，
  压缩管线为 v18，旧缓存自然失效。

- 九条无障碍规则实施时达到 44/77 条规则。JSX 原始值、解码文本、空子节点与表达式范围来自
  parser v13；lint 核心为 v8。规则覆盖顺序、动态值、隐藏祖先、装饰图片、原生键盘交互、
  programmatic focus 和角色后备均有先失败后通过的用例。53 项 ARIA 属性与 88 个核心具体角色
  已和固定规范逐项核对；不复制第三方实现或将静态结果宣称为运行时可访问性证明。
  核心/解析/语义/编译回归、Clippy、CLI 10 项、项目配置 11 项及真实 Node 18 项均通过；
  文档、架构与差异格式检查通过。
- `ts/array-type` 实施时达到 35/77 条规则。支持 array/generic 两种书写模式、原始 TS/TSX
  范围、嵌套类型、类型查询数组后缀、注释抑制和关闭状态参数校验；排除元组、索引键、值调用及
  heritage 基类型。parser 新增类型引用、算子、数组/索引与 heritage 事实，版本为 v12；核心为 v7。
  原始语法与规则用例先失败后通过，parser、semantic、compiler 与 lint 全量回归通过。

- P1 原始 export 记录与源码值导出使用已接通：包含被 TS 擦除的导出、引入属性和 namespace
  所有权；值导出符号与普通编译分析使用相同身份，live binding 导出单独建模，不当成求值读取。
  parser 版本为 v11。parser/semantic 聚焦用例先失败后通过，编译与 lint 回归通过；类型空间与
  erased ambient 值声明仍待完成，规则计数仍为 34。
- P1 值空间修复了严格模式块级函数泄漏与类静态块 var 环境缺失。模块、函数/箭头的 strict 指令、
  类成员、switch 与 try/catch/finally 块有独立回归，静态块提前绑定 var/function/lexical 声明。
  压缩版本升为 v17、lint 核心升为 v6，使旧分析结果自然失效；最终生成代码的 Node 差分用例
  同时验证块内函数、类静态块、原始/压缩/mapping 产物的行为。
- P1 参数与 var/function 的绑定关系、含参数表达式的独立函数体环境及隐式初值复制已修复。
  最小失败用例同时复现了语义身份错误与压缩后参数值丢失；修复后普通/解构/rest 参数、默认值、
  计算解构键、箭头和参数闭包的语义及生成代码差分验证通过。compiler、semantic、minify、codegen、
  lint 回归与 Clippy 已通过。

- P0 原生类型后端实验已覆盖锁定的 TypeScript 7.0.2 `unstable/async` API：原始 AST 位置为
  UTF-16（emoji 前缀示例 40→42 UTF-8）、内存依赖从 Promise 改为原始类型、旧快照隔离、两个
  project-reference 根及仅一根的未保存更新。直接使用原生 FS 时 PnP 导入出现 TS2307；委托已安装
  PnP 文件系统后两个项目均为 97 个源文件且无语义/配置错误。关闭 API 会拒绝未完成请求，已观察
  子进程正常退出并成功重建服务。当前没有逐请求取消 API；产品宿主必须自己拥有关闭/退出确认与
  超时终止，不能把 close Promise 当成进程回收证据。原型与 JSON 在本地 `.tmp/lint-validation/`
  的 `type-api-native-probe`、`type-projects-probe`、`type-api-close-probe`，未冻结产品协议。

- 原始 import 事实已覆盖具名/默认/namespace/type/equals 形式、本地绑定角色、解码模块和属性，
  保留 ambient 所有权，排除动态 import 与文字诱饵。源码捕获与普通编译的 AST、依赖等价；
  parser、semantic、compiler 与核心回归及 Clippy/架构通过。新增 `js/no-duplicate-imports`，
  总数 34；可允许纯类型导入单独存在，默认关闭，无修复。parser/core 分析身份分别为 v10/v5。

- 已实现 `js/no-constant-binary-expression`，规则总数 33。覆盖短路、空值合并、严格类型不相交、
  字面量和新建引用比较；反例覆盖对象转换、BigInt、赋值别名、类静态逃逸、未知全局与 TS/JSX
  原始表达式及注释抑制。默认关闭，无修复；未改动既有版本化预设。核心全量、应用二十七项、
  原生 Node 十六项和 Clippy 通过；核心分析身份更新为 v4。

- 已实现 [ADR 0059](../decisions/0059-bounded-lint-file-execution.md)：独立文件复用进程共享线程池，
  单批最多 min(32, workers) 个文件，全进程最多两个准备/执行批次；等待与已入队任务均可取消。
  单文件 panic 被隔离，缓存/基线按路径汇总，发布仍在协调线程执行。真实单线程/四线程矩阵的
  诊断、修复全文、写入文件、缓存统计与基线完全一致。应用二十七项、内部十一项、Rust CLI 十项、
  真实 Node 十六项、真实 npm CLI 与 watch 冒烟及 Clippy/架构/文档门禁通过。当前未公布性能数字。

- 已实现 [ADR 0058](../decisions/0058-lint-watch-lifecycle.md) 的共享文件监听：CLI 逐行 JSON 事件、
  Node 事件与重复启动/停止、配置错误恢复、目录删除/重建、未保存文档更新和显式基线变化。
  根目录被符号链接或 junction 重定向时拒绝复用上下文；普通目录重建可恢复。真实 Windows
  监听四项、后端代次/缓存循环两项、Node 十六项、CLI 十项、CLI/架构七十五项及类型/文档检查通过。
  平台矩阵仍待运行；Windows 子进程强制退出测试不作为 Unix 信号优雅退出的证据。

- 已实现 [ADR 0057](../decisions/0057-versioned-lint-project-context.md) 的手动 `LintContext`：
  固定项目根、多个未保存源码、严格安全整数版本和关闭墓碑、同步请求代次、单分析执行与取消。
  检查仍复用项目入口和配置发现，内存文档绕过缓存；Node ESM/CJS 提供 create/check/update/close。
  已复现并修复 Worker 退出期间 N-API Promise 完成回调导致的原生崩溃；上下文及单次 lint 均通过
  owned 结果槽主动读取，不从后台调用已退出的 JS 环境，环境清理会取消并回收线程。
  应用三项上下文与两项确定性并发/关闭测试、原有二十项 lint/cache/baseline 回归、真实 Node
  十四项、Rust CLI 九项和 CLI/架构 Wake Test 七十五项通过；npm 类型、真实 npm CLI 冒烟、
  Clippy、架构及文档检查通过。手动上下文尚无自动文件监听，也未宣称 AST/类型增量完成。

- 已实现 [ADR 0056](../decisions/0056-lint-suppression-baselines.md) 的显式抑制基线：CLI 的生成、
  检查、清理和 Node 等价入口共用核心唯一上下文匹配。SHA-256 身份不依赖绝对行号；重复上下文
  全部报告。修复每轮先应用基线，缓存键包含当前文件基线条目，parser/指令错误不能被基线化。
  文件 schema、规范路径、尺寸/数量和旧内容均校验；output owner 原子替换或不覆盖创建。
  清理不添加条目，保留解析失败及被忽略的现存文件。核心三项、应用六项、publication 六项、
  Rust CLI 九项、真实 Node 十项与真实 npm CLI 冒烟通过；应用既有十四项、核心全量回归、
  Clippy、npm 类型、架构和文档检查通过。CLI/架构 Wake Test 两个 suite 全部通过。

- P0 类型服务候选实验：仓库锁定 TypeScript 6.0.2 的 LanguageService 已验证内存源码覆盖、
  import 依赖从 Promise<number> 改为 number 后的失效、PnP 工作区类型导入、UTF-16→UTF-8 映射、
  请求取消及取消后的继续检查。实验读取 97 个类型源文件，无项目语义错误；证据保存在本地
  `.tmp/lint-validation/type-api-probe.mjs` 与 JSON 报告。后续 TS7 实验已补齐 project references/
  多根项目；这些实验尚未建立跨进程产品协议，不能据此将 P5 标记为完成。

- 已接入 `--list-rules` / Node `listRules` 的 `wake.lint.rules.v1` 目录，注册表统一提供类别、支持
  语言、默认等级、分析依赖、参数 schema、消息 ID、文档路径和修复能力。目录按 ID 排序，不读取
  项目或源码，与分析选项互斥。该批次为 32 条规则；Node ESM/CJS、真实 npm CLI 与 Rust CLI 已验证。
  当前批次通过：原生 Node 8 项、Rust CLI 6 项、CLI/架构 Wake Test 72 项，TypeScript 类型与架构
  检查、文档检查（85 routes / 159 Markdown）及 parser/semantic/compiler/core 的回归。
  缓存已按 [ADR 0055](../decisions/0055-content-addressed-lint-cache.md) 接入 CLI/Node；默认关闭，只读
  文件检查可复用，无 parser 诊断才持久化。源码和有效规则/抑制设置进入 key，缓存错误为不影响
  规则计数/退出码的元数据提示。已验证冷热、同 mtime/size 编辑、配置失效、损坏恢复、并发进程、
  有界保留/锁等待与失败替换；stdin 和修复均绕过缓存。Unix symlink 用例未在本机运行。
  缓存命中前，核心验证已登记规则/消息、当前等级、UTF-8 诊断范围与完整 fix 编辑集合；
  只有与当前规范配置及分析身份一致的缓存键可以送入验证。修复命令绕过持久化结果。

- 风格与结构规则切片：默认关闭。`style/quotes` 参数 `quote = "single"` 可选 `double`，
  检查 JS/TS 字符串 token，排除 JSX 属性字符串、模板和注释；修复保留字符串值、原始其他转义及
  指令语义。`style/no-trailing-spaces` 检查换行或 EOF 前的空格和 tab；若空白位于模板、JSX 文本
  或其他字符串 token 内，只报告，不提供修复；其他尾空白可删除。空白行也检查，支持所有 ES 换行。
  `js/no-sparse-arrays` 检查数组表达式中的空槽（含多逗号），不检查数组解构空位；
  `js/valid-typeof` 检查 typeof 与静态字符串的等/不等比较，接受八种标准结果字符串；
  `js/no-cond-assign` 拒绝 if、while、do、for、条件表达式的测试中直接或嵌套的赋值，括号不豁免，
  但不进入测试中的函数/类体；`js/no-var` 报告语句和 for 声明中的 var，不自动改变作用域。
  这些 JS 规则无参数、无自动修复。新增规则不加入任何旧版本预设。
  作用域规则按需复用 semantic 的原始引用投影：`js/no-console` 检查未被本地绑定遮蔽的
  console 成员访问；`js/no-async-promise-executor` 检查全局 Promise 构造器的 async 函数/箭头实参；
  `js/no-promise-executor-return` 检查该 executor 的带值 return 或隐式返回表达式（显式 void 除外），
  不进入嵌套函数。这三条默认 off、无参数、无修复，不根据名称误判本地同名绑定。
  `js/no-self-assign` 检查直接标识符的自赋值及数组/对象字面量解构中的对应自赋值；不推断
  成员 getter 或动态 spread。`js/no-self-compare` 检查比较两侧相同标识符或相同原始字面量，
  不把重复调用/成员读取当成常量。`js/use-isnan` 检查与未遮蔽 NaN / Number.NaN 的相等、关系
  比较，以及 switch 判别值与 case；不检查同名本地绑定。这三条默认 off、无参数、无修复。
  控制流规则统一消费 semantic completion 事实，默认 off、无参数、无修复。
  `js/no-unreachable` 检查必然离开后的语句，允许提升的函数声明、无初始化器 var 和空语句；
  不按常量 if 条件裁剪分支。`js/consistent-return` 检查同一原始函数中带值返回与无值返回/自然
  结束并存；显式 void 返回视为无值，throw 和不终止路径不算无值返回。
  `js/no-fallthrough` 检查非空 case 可继续进入下一 case 的路径；空 case 允许共享分支，
  最后语句与下一 case 之间的真实注释正文为 `fallthrough`、`fall through` 或 `falls through`
  时允许穿透（忽略大小写和首尾空白）。`js/no-unsafe-finally` 检查逃逸 finally 的 return、throw、
  break/continue；内部循环/标签消费的跳转和内部 catch 捕获的 throw 不报告。

- 配置切片见 [ADR 0052](../decisions/0052-lint-effective-configuration.md)：等级简写和 `{ level, options }` 整体替换；核心统一校验参数并填充默认值。
  `js/eqeqeq` 的 `allow_null = false` 可允许一侧 null 的宽松比较；`js/no-empty` 的
  `allow_catch = false` 可允许空 catch；`js/no-constant-condition` 的 `check_loops = true`
  可关闭循环条件检查；`style/eol-last` 的 `linebreak = "lf"` 可选 `"crlf"`，只决定缺失时追加的文本。
  其他当前规则只接受空参数对象。所有规则即使 off 也必须校验；非布尔/错误枚举/未知字段失败。
  预设为 `recommended@1`（既有六规则）、`react@1`（四条已实施 JSX 规则）、`style@1`
  （eol-last）和 `all@1`（首批十一规则），无版本别名解析到当前版本。默认 recommended
  可通过原有布尔字段关闭；显式预设按顺序追加。新规则不得静默加入已版本化预设。
  配置解释通过 `--print-config <file>` 或 Node `printConfig` 输出有效参数、等级和最后来源；
  文件名可不存在，也可被忽略，不解析源码。`--rule 'id=error'` 或 `--rule 'id={"level":"warn",
  "options":{...}}'` 可重复，后者覆盖前者；Node `rules` 提供等价的请求覆盖。

- JSX 规则切片：四条可选规则消费 parser-owned source nodes，默认 off。
  `jsx-no-duplicate-props` 比较开标签下的显式属性（含 lowering 移出的 key），不猜测 spread。
  `no-danger`、`no-children-prop` 检查显式属性，`self-closing-comp` 只检查没有任何子源码的元素。
  规则不从合成调用反推语法；参数及其余 React 分析仍待实现。
- 修复切片见 [ADR 0051](../decisions/0051-lint-source-fix-transactions.md)。当前安全修复仅覆盖
  `react/self-closing-comp` 和 `style/eol-last`。闭标签改写范围有真实注释时只报告，不删除注释。
  核心验证编辑集合、稳定选择冲突并进行最多十轮重新分析；CLI 和 Node 共用 off/dry-run/write
  模式，返回最终文本和诊断。源码写回复用 output owner 的共享锁、快照复核及取消 fence。
  保留标准权限位，拒绝只读和多硬链接源码；不承诺跨文件原子性或外部编辑器参与锁。
  本轮 Windows 验证：核心 19 项、publication 12 项、项目 lint 8 项、Rust CLI 4 项、真实 Node
  addon 6 项、CLI/架构 70 项测试通过；真实 npm CLI 预览/写回/stdin 冒烟、类型、Clippy、架构、
  文档检查和站点构建通过。Unix 权限/符号链接测试已加入但未在本机执行；发布平台矩阵仍未运行。

- 单文件规则切片：`wake_lint_core::lint_text` 借用源码，返回 owned 诊断。默认 recommended：
  `js/no-debugger`、`js/no-empty`、`js/no-duplicate-case`、`js/no-dupe-keys` 为 error；
  `js/eqeqeq`、`js/no-constant-condition` 为 warn。显式规则等级覆盖默认，无未知规则容忍。
  参数支持见上方配置切片；未登记参数不能静默忽略。
  `eqeqeq` 检查所有 `==`/`!=`（含 null），不自动改变比较语义；`no-empty` 允许含真实注释的空块，
  不检查函数体。重复 case 比较可证明相等的原始字符串/number/bool/null；不猜测动态表达式。
  重复对象键比较静态名称与字面量（包括静态 computed），允许同名 getter/setter 配对，
  跳过 JSX lowering 合成的 props 对象。常量条件检查 if/while/do/for/条件表达式中可证明恒定的
  字面量、对象/函数、逻辑与否定表达式，不代替一般常量求值或控制流分析。
  parser error 时跳过规则。抑制只消费真实注释，语法诊断不可被抑制。
  未使用抑制按规则项记录，默认 warn；重复 disable 不替换先前生效项。多行 next-line 按注释末行
  定位，disable-line 不接受跨行注释。具体配置见 lint 命令参考。
- 项目入口切片契约见 `docs/reference/cli/lint.mdx`。Rust CLI 和 Node 共享 `wake_app::lint_project`，
  已覆盖文件发现、覆盖顺序、未保存文本、未知规则、退出码和 ECMAScript 换行坐标。
  项目编排先放入现有 `wake_app`；不为尚未实现的缓存/watch
  创建空的产品 crate。默认 ignore 在此预览版不可取消，符号链接均不跟随。
- 源码结构切片：`parse_source` 在同一 parser pass 采集 JSX 元素/片段、开闭标签、名称、属性、
  属性值、表达式容器和原始文本。节点按前序编号，parent 指向包含节点；范围为原始 UTF-8 半开
  区间，保留重复 key、空表达式、实体拼写和空白文本。编译 AST 仍执行原有 lowering。
  这不是完整 JS/TS 原始 AST，也不是公共插件 ABI；未完成的语法在阶段表中继续保持待实现。
  语法错误时节点只是恢复产物，消费者须先检查诊断，不能据此直接修复。
  token 采集继续由同一 parser 的实际消费路径产生，区分 JavaScript/TypeScript、JSX 标签和 JSX 文本。
  不含注释、空白、EOF 或合成 helper；JSX 原始名称保留完整范围，类型闭合 `>>` 拆为已消费的 `>`，
  表达式移位仍保留 `>>`。失败的语法试探回滚 token；lookahead 不提前进入最终结果。
  普通 `parse` 和仅注释入口不分配 token 集合。token 与 comment 可结合源码完整重建文本；
  不能把 token 流宣称为完整类型 AST。
  供语义分析消费的标识符事实保存 cooked 名称、原始范围和绑定/值引用/类型引用/类型查询角色；
  它们由产生式记录并跟随 checkpoint 回滚。属性名不当成引用，JSX 只记录组件根，合成 helper
  不进入事实。cover grammar 的候选表达式仍须与最终 AST 的绑定/引用身份联合解释。
  类型实参实例化表达式（如 `fn<T> / 2`）的闭合 `>` 后恢复表达式尾部词法，`/` 必须作为除号，
  不得留下试探阶段产生的正则错误；普通 `a > /x/` 的右侧仍允许正则表达式。
  parser 的表达式起始上下文拥有 regex/div 最终判定；`if (x) /test/.exec(x)` 等控制头之后的
  正则语句不能被词法启发式误判为除号，普通调用、括号值和二元除法保持原有意义。
  已修复 JSX 开闭名称不匹配仍被接受的问题，覆盖普通/成员/命名空间/片段标签；parser pipeline
  身份更新为 v8，既有持久化编译缓存不复用旧语法结果。
- TS 源码范围切片：保留类型注解（包含冒号）、类型参数/实参（包含尖括号）与类型产生式的原始
  范围和嵌套；失败的 `<...>` 表达式试探不能留下类型节点。平衡跳过的类型内部尚无完整结构，
  不据此宣称类型 AST、类型命名空间或类型感知规则已实现。
  已复用现有声明语法中的对象类型、元组和签名产生式，为源码采集保留其结构及嵌套类型；
  不执行第二个 TS parser。`any` 只在类型产生式消费时登记，属性名、普通标识符和 `typeof any`
  不算显式 any。非空断言只在表达式后缀登记，不混淆逻辑非或声明的 definite-assignment `!`。
  `ts/no-explicit-any`、`ts/no-non-null-assertion` 默认关闭、无参数、无自动修复；只基于上述 parser
  事实报告。无类型信息需求，不推断名字和表达式类型。
  `ts/no-namespace` 检查具名 namespace/module 声明（含 declare），允许字符串 ambient module。
  `ts/no-empty-interface` 检查无任何成员的接口（即使有 extends），注释不算成员；不自动转换类型别名。
  二者默认 off、无参数、无修复，依赖 parser 保留的原始声明节点。
  `ts/ban-ts-comment` 默认 off，拒绝真实注释行开头的 @ts-ignore 与 @ts-nocheck，允许 @ts-check；
  @ts-expect-error 需要至少三个非首尾空白字符的说明（可用冒号分隔）。只报告、不移除注释；
  支持 JS/TS 源码和块注释的星号前缀，不把普通文本中间的提及或字符串当成指令。
- 2026-09-12：建立目标契约和 proposed ADR。确认现有 TS 擦除、JSX 直接降级、lexer 丢弃注释，
  首个实现切片为保留 lexer 的真实注释并通过 parser 暴露源码快照。它不等于原始 AST 完成。
- 2026-09-13：已验证本轮基础入口与十条规则。lexer/parser、transform/codegen、compiler/core、
  common、config 和 lint core 的 crate 测试通过；应用层六项 lint 测试与 Rust CLI 三项集成测试通过。
  CLI/架构共 68 项 Wake Test、五项真实 Node addon 契约测试、真实 Node CLI stdin/退出码冒烟、
  Node 请求 DTO 测试、npm 类型检查及架构检查通过。文档检查和实际文档站构建通过（85 routes）。
  尚未执行完整发布平台矩阵、tarball 消费、性能/ESLint 对照；本轮不构成完整产品或发布就绪声明。
- 2026-09-14：修复 PnP 模块监听覆盖范围过宽的问题。快照为保持身份而记录的项目根祖先目录不再被单独注册为 watcher；
  监听只覆盖项目根、根父目录和真实外部依赖父目录，避免 Windows 受限环境中的 `PathNotFound` 循环，
  PnP 虚拟 peer 共享物理归档回归测试及完整 `cargo test --workspace --locked --offline`（本地 Rusty V8 归档）通过。
  `lint_watch` 六项、相关 Clippy、rustfmt、diff、架构检查和文档检查均通过；CI lint-product 矩阵现包含 Ubuntu、Windows、macOS。
- 2026-09-14：P1 原始值语义补充顶层 `declare`/`declare global` 值绑定恢复。擦除声明现在进入源码 semantic
  的模块值环境，`typeof` 查询和对应运行时引用按原始声明身份解析；块内声明、ambient module/namespace
  成员及函数头的不完整环境仍 fail-closed。新增 semantic、`no-undef` 与 `no-unused-vars` 回归并通过。
- 2026-09-14：补齐顶层 ambient 函数签名的边界。`declare global` 中无函数体签名的首个值绑定可恢复，
  参数绑定继续留在不完整签名环境，不再被投影到模块作用域；新增函数/类/枚举与参数泄漏回归，
  semantic 聚焦套件通过；随后带本地 Rusty V8 归档的完整 `cargo test --workspace --locked --offline`
  也通过。
- 2026-09-14：P1 ambient namespace 值作用域完成一块。非字符串 `declare namespace` 的根绑定、
  直接值成员和嵌套 namespace 现在由 semantic 建立独立源码 scope，`typeof` 查询与 namespace 根
  引用按原始身份解析；字符串 ambient module、块、函数签名参数继续 fail-closed。新增 semantic
  与 `js/no-undef` 回归，semantic/lint core、Clippy、完整 workspace、架构和文档门禁通过。
- 2026-09-14：收窄不完整 ambient 名称的保守边界。字符串 ambient module 的同名擦除声明不再
  遮蔽另一个 scope 中已解析的本地值；块和未表示 namespace 仍保持文件级 fail-closed。新增
  semantic 回归先复现失败后转绿，semantic/lint core、Clippy 和完整 workspace 通过。
- 2026-09-14：将同一边界延伸到规则消费端。`js/no-unused-vars`、`js/prefer-const`、
  `js/no-shadow`、`js/no-use-before-define`、React Hooks 捕获和 CommonJS `require` 图事实不再
  按文件级名称集合跳过已表示本地符号；字符串 ambient module 的同名擦除声明继续保持不可证明。
  类型空间声明也不再被同名不完整值集合隐藏；新增 lint core 回归先复现失败后转绿。
- 2026-09-14：修正 `declare global` 与同名模块本地绑定的身份边界。全局投影不再把已有本地
  SymbolId 标成 ambient；新增 semantic 身份与 `js/no-use-before-define` 回归，先复现失败后
  转绿，避免声明级规则跳过真实本地绑定。
- 2026-09-14：P1 ambient 语义再推进一块。字符串 ambient module body 的直接值成员现在进入
  独立外部模块源码 scope；同一源文件的重复模块字符串共享该 scope。body 内本地 `typeof`
  查询可解析，未知外部值保持 `Unavailable`，不会回退捕获文件模块中的同名绑定。新增
  semantic 失败回归并转绿，semantic/lint core 全量通过。
- 2026-09-14：P1 签名值语义再推进一块。被擦除的无函数体声明重载现在恢复独立签名 scope，
  参数只在该签名的返回类型/类型谓词查询中按源码身份解析，不泄漏到模块或其他签名；当 parser
  没有提供值参数绑定时，纯函数类型、块和未表示的其他签名环境继续保持 `Unavailable`。新增先失败后转绿的 semantic 回归，
  semantic/lint core 全量通过。
- 2026-09-14：P1 纯类型签名值语义再推进一块。parser 已记录值参数绑定的 `TsSignature` 现在恢复独立
  signature scope；签名返回类型/类型谓词中的 `typeof parameter` 按源码身份解析，不泄漏到模块或其他签名，
  并保留最近 ambient namespace/module 或函数环境的父 scope。
  无值参数绑定的纯类型签名和其他未表示类型环境仍保持 `Unavailable`。新增先失败后转绿的 semantic 回归，
  semantic/lint core 全量通过。
- 2026-09-14：P1 函数体值语义再推进一块。函数体中直接 `declare` 值成员现在复用已有 function-body
  scope，体内查询与原始运行时引用按函数身份解析，函数外不创建投影；参数环境和未表示函数头继续
  fail-closed。新增先失败后转绿的 semantic 回归，semantic/lint core 全量通过。
- 2026-09-14：P1 switch 值语义再推进一块。switch 环境中直接 `declare` 值成员现在复用 resolver 的
  switch scope，case 内查询与原始运行时引用按该 scope 解析，switch 外不创建投影；未知/未表示环境继续
  fail-closed。新增先失败后转绿的 semantic 回归，semantic/lint core 全量通过。
- 2026-09-14：P1 ambient 类型成员再推进一块。同一源文件中重复字符串 ambient module 声明现在合并
  类型成员与声明 identity；后续 body 的类型引用可以解析前一段声明，未表示的外部模块仍保持未知。
  新增先失败后转绿的 source-types semantic 回归，semantic 全量通过。
- 2026-09-14：P5/P1 类型字面量再推进一块。parser-owned switch facts 新增 BigInt 原始值，按任意进制、
  分隔符和正负号规范为精确十进制文本；类型服务适配保留 BigInt literal identity，`switch-exhaustiveness-check`
  现在可以证明 bigint 联合及枚举成员覆盖，不把大整数降级为浮点数。新增 parser/core 先失败后转绿的
  正反例，相关 semantic/lint 回归继续执行。
- 2026-09-14：P5 断言语义再推进一块。`ts/no-unnecessary-type-assertion` 现在比较有限类型图的
  字面量、联合/交叉成员、约束、继承和标准库身份；字面量到宽类型、不同联合结构和不同对象身份
  不再因共享基础类别被误报。新增先失败后转绿的源类型正反例，核心类型回归通过。
- 2026-09-14：收紧 BigInt 类型事实边界。适配器现在拒绝空数字、非法进制字符和损坏的后端值，
  将其作为 `WAKE_LINT_ANALYSIS` 失败而不是静默归一为零值；新增适配器单元回归，原生类型服务
  23 项（含 BigInt 穷举）继续通过。
- 2026-09-14：使用 Windows TypeScript 7.0.2 原生宿主补充 BigInt 联合回归；`9007199254740993n`
  与十六进制、分隔符等价的 `case` 均通过精确值覆盖证明，类型服务/核心/应用聚焦套件通过。
- 2026-09-14：P1 块值语义再推进一块。普通语句块中的直接 `declare` 值成员复用已有 block scope，
  块内查询与原始运行时引用按块身份解析，块外仍使用外层绑定，不创建模块级投影；函数体、ambient
  module/namespace 和未表示签名继续 fail-closed。新增先失败后转绿的 semantic 回归，
  semantic/lint core 全量通过。
- 2026-09-14：P1 擦除声明身份再推进一块。源码 semantic 按 parser 保留的函数、class 与 enum
  声明锚点恢复 `declare` 值绑定类别；`declare namespace` 与同名擦除 class/function/enum 的
  合并不再被 `js/no-redeclare` 误报。新增先失败后转绿的绑定回归，semantic source-query、lint
  bindings 与 Clippy 聚焦门禁通过。
- 2026-09-14：P1 擦除声明 occurrence 再推进一块。同一值作用域中的重复 `declare` 值绑定现在
  保留共享符号下的每个源码 occurrence；`js/no-redeclare` 能报告重复擦除声明，同时仍不把
  `declare global` 的同名本地绑定重新标成 ambient。新增重复声明先失败后转绿，规则聚焦回归通过。
- 2026-09-14：P1 class static block 值范围再推进一块。parser-owned `JsBlock` 现在保留完整
  `static {…}` 容器范围，与 semantic 的独立 static-block scope 对齐；其中被擦除的值声明和
  `typeof`/运行时引用按同一源码身份解析，不再落入 `Unavailable`。新增 parser/semantic 失败回归
  后转绿，相关聚焦套件与 Clippy 通过。
- 2026-09-14：P1 catch 值范围再推进一块。catch 参数环境与 parser-owned body `JsBlock` 现在
  同时拥有 resolver region；body 内被擦除的值声明按 catch scope 解析，body 外仍保持未解析。
  新增先失败后转绿的 semantic 回归，source-query、semantic/lint 聚焦门禁继续通过。
- 2026-09-14：P1 擦除值声明类别再推进一块。parser 为 `var`、`let`、`const`、`using` 保留
  binding kind，semantic 投影不再把所有 ambient 值统一降成 `Const`；原始 `DeclKind` 可供声明、
  作用域和规则消费端保持一致。新增先失败后转绿的 semantic 回归，parser/semantic 全量与 Clippy 通过。
- 2026-09-14：为上述类别事实补充 parser-owned 直接回归，覆盖 ambient `var`/`let`/`const`、函数、
  class 与运行时 `using`；源码标识符输出现在有独立测试锁定原始类别，避免后续只在 semantic 投影层
  观察到类别而漏掉 parser 事实。
- 2026-09-14：P1 运行时 `namespace` 内的擦除值声明纳入源码 scope 投影。namespace 的 IIFE 参数
  身份现在作为 parser-owned body 与已保留函数 scope 的连接点，body 内 `typeof`/运行时引用按
  namespace scope 解析，外层和相邻 namespace 不捕获该值；新增 semantic 失败回归并已转绿。
  `js/no-undef` 也补充了 namespace 内部可解析、外部仍报告的规则回归。
- 2026-09-14：上述 P1 修复后的 workspace 全套测试（含 wake_app 243 项 lint/应用回归、CLI、LSP、
  parser、semantic 和所有 doctest）通过；目标机不可用的 Chromium 与非 Windows 原生包测试继续保持
  明确 ignored/待 runner 证据状态。
- 2026-09-14：补跑 workspace 全 target Clippy、npm 类型检查、TypeScript 7.0.2 兼容 fixture、迁移报告、
  独立扩展 SDK、VS Code 清单、release coverage、真实项目与 LSP smoke，均通过；当前 Windows 工作树
  缺少 Linux 原生二进制，`npm:pack:check` 因此保留跨平台 tarball 待目标 runner 验证，不以占位文件通过门禁。
- 2026-09-14：重新运行 key 性能基线并生成 `.tmp/lint-performance/report.json`；lint 16 KiB/128 KiB
  分别约 0.753 ms/5.744 ms，lexer、TSX 编译与打包基准均未触发失败阈值。parser 约 +14.6%，但
  当前基线因 benchmark 文件随工作树变更而按契约标记为 report-only（`enforce=false`），不作为
  发布阻断；报告保留 Windows runner 的完整环境与置信区间。
- 2026-09-14：在同一变更后的 Windows 环境重新运行被默认跳过的原生 TypeScript 证据：
  `wake_app` 的 `lint_types` 6 项、类型服务库 23 项及 CLI 类型入口 1 项全部通过，确认 P5
  项目、PnP/zip 文件系统、stdio/backend 生命周期、未保存源码、watch、缓存和分析退出码仍保持一致。
- 2026-09-14：已安装 `x86_64-unknown-linux-gnu` Rust target 并尝试本机交叉构建 `wake_cli`；构建
  在 `ring` 的 C 编译阶段因缺少 `x86_64-linux-gnu-gcc` 和 Linux libc 头文件停止，未生成或登记
  Linux 原生包。该结果保留为可复现的目标 runner 阻塞，不能替代 Linux/macOS 构建机证据。
- 2026-09-14：Windows `wake_node` release 与真实 N-API addon 构建通过，`npm/wake-win32-x64-msvc`
  tarball 已由包合同校验；完整 `npm:pack:check` 已推进到 Linux 包并明确停止于缺失 Linux addon，
  未使用占位二进制绕过门禁。
- 2026-09-14：使用刚构建的 Windows addon 运行 Node API 与 lint 集成回归，Wake API/CLI 共 54 项、
  CSS realm 4 项及 Federation 61 项全部通过；LSP、真实项目和文档检查继续通过。
- 2026-09-15：P1 参数环境再推进一块。带默认值或计算解构键的函数/箭头参数会复用 semantic
  已建立的独立参数 scope；默认值、参数类型和返回类型中的 `typeof parameter` 不再被整段标为
  `Unavailable`，仍保留参数不能捕获函数体绑定的作用域边界。新增先失败后转绿的
  `source_type_queries` 回归，semantic 全量测试通过。
- 2026-09-15：P1 纯类型签名再推进一块。无值参数的 `TsSignature` 现在也拥有独立 source scope，
  因此 `type Fn = () => typeof outer` 可解析最近完整的外层值，而缺失名称继续保持
  `Unavailable`；新增先失败后转绿的 semantic 回归，27 项 source-query 测试通过。
- 2026-09-15：P1 枚举值语义再推进一块。parser 为枚举成员保留独立的 source binding 身份，
  semantic 为每个枚举建立只覆盖初始化表达式的源码 scope；`typeof Member` 现在解析所属枚举
  成员，且成员不会泄漏为模块级普通值；初始化中的裸成员引用也重绑定到该 source scope，
  `js/no-use-before-define` 可以报告前向成员引用。新增先失败后转绿的 semantic/lint 回归。
- 2026-09-15：补充具名函数表达式的 P1 查询回归。默认参数与函数体中的 `typeof named`、
  `typeof parameter` 解析到各自的源码身份，函数外同名查询保持未解析；该回归锁定
  FunctionName scope 与参数 scope 的边界。
- 2026-09-15：补充运行时 namespace 内 switch 的 P1 查询回归。namespace 投影现在排除
  `JsSwitchBody`，擦除的 switch 值绑定只保留在 switch scope；namespace 外层同名查询不再被
  错误提升，新增先失败后转绿的 semantic 回归。
- 2026-09-15：补充运行时 namespace 内 catch 参数的 P1 查询回归。namespace 投影现在复用
  resolver 的 `Catch` scope，不再把 catch 参数复制到 namespace scope；新增先失败后转绿的
  semantic 回归。
- 2026-09-15：补充运行时 namespace 内 `for`/`for-in`/`for-of` 绑定的 P1 查询回归。
  namespace 投影保留 resolver 的循环块与嵌套块父子关系，不再把循环头绑定提升到 namespace
  scope；新增先失败后转绿的 semantic 回归。
- 2026-09-15：补充 runtime namespace 内 class static block 嵌套块的 P1 查询回归。namespace
  投影不再改挂 static block 的内层块，静态块绑定继续通过真实父作用域解析；新增先失败后转绿
  的 semantic 回归。
- 2026-09-15：补充 runtime namespace 内 static block 函数捕获的 P1 回归。static block 中函数
  的返回类型查询与运行时引用继续通过真实 static-block 父 scope 解析，namespace 外同名查询仍
  保持未解析；新增先失败后转绿的 semantic 回归。
- 2026-09-15：P5 `await-thenable` 扩展标准库 PromiseLike 身份。类型图现在保留 PromiseLike
  的 default-lib 符号身份，并沿引用、继承、联合、交叉和约束传播 thenable 证明；普通同名接口
  与未知结构继续 fail-closed。新增先失败后转绿的核心类型回归。
- 2026-09-15：P5 的 Promise 消费规则复用同一标准 thenable 证明。`ts/no-floating-promises` 与
  `ts/no-misused-promises` 现在也识别标准库 PromiseLike 及其可证明派生类型，避免同一类型在
  `await` 与同步条件/顶层调用中出现不一致诊断；新增先失败后转绿的核心正反例。
- 2026-09-15：修正 P5 类型服务的标准 Promise 身份请求边界。只有顶层浮动调用而没有 `await` 或
  条件的文件也会查询 Promise/PromiseLike 身份，避免 `ts/no-floating-promises` 因类型身份缺失而
  静默漏报；新增先失败后转绿的类型服务决策回归。
- 2026-09-15：P5 `no-misused-promises` 扩展到条件表达式和 `&&`/`||` 的实际操作数；parser 保留
  原始条件范围，类型服务按真实表达式节点查询，避免只检查 `if`/循环而漏掉表达式条件。
- 2026-09-15：P5 `no-floating-promises` 接入动态 `import()` 的原生返回类型。动态导入现在沿用
  原始调用范围查询 Promise 身份，顶层浮动导入可报告，`await`、赋值和 `void` 消费仍按表达式语义跳过。
- 2026-09-15：表达式条件与动态导入事实改变了持久化输入身份，parser/core pipeline 分别推进至
  `v33`/`v40`，旧缓存不会复用缺少这些源码事实的类型结果。
- 2026-09-15：P5 赋值/返回类型事实递归保留泛型引用实参，`Promise<any>` 等嵌套 any/error 不再
  被外层对象类别吞掉；新增原生 TypeScript 7.0.2 赋值与 return 失败回归先失败后转绿，核心
  pipeline 版本推进到 `v41`。
- 2026-09-15：P5 `no-unnecessary-type-assertion` 现在可证明同一引用目标及逐项等价的安全泛型实参，
  同一 `Box<string>` 的重复断言不再被对象身份保护逻辑漏报；含 `any`/`unknown`/`error` 的不完整
  实参、匿名结构对象与不同引用实参继续 fail-closed。新增先失败后转绿的核心类型回归，核心
  pipeline 版本推进到 `v43`。
- 2026-09-15：P5 `no-floating-promises` 扩展到原始表达式语句的最终类型，裸 Promise/PromiseLike
  标识符和成员表达式与调用返回值共享标准库身份及 fail-closed 规则；核心 pipeline 版本推进到 `v44`。
- 2026-09-15：P5 `no-misused-promises` 扩展到回调参数位置，保留实际回调签名与原生 contextual
  `void` 返回签名；any/unknown/error/缺失事实继续 fail-closed，核心 pipeline 版本推进到 `v45`。
- 2026-09-15：在 Windows TypeScript 7.0.2 原生服务上新增异步回调传入 `void` 参数的正反例，
  完整 `lint::type_service -- --ignored` 回归由 27 项扩展为 29/29 通过；参数地址、实际签名、
  contextual 签名和既有调用/模板/浮动 Promise 查询顺序保持稳定。
- 2026-09-15：P5 `no-misused-promises` 继续覆盖变量初始化与赋值位置，复用 parser-owned
  assignment value 范围查询原生 contextual callback 签名；缺失或不确定 contextual 类型继续
  fail-closed，核心 pipeline 版本推进到 `v46`。
- 2026-09-15：P5 `no-misused-promises` 继续覆盖 return 表达式位置，复用 parser-owned
  return argument 范围查询原生 contextual callback 签名；Promise-returning 回调落入 `void`
  返回函数类型时报告，缺失或不确定 contextual 类型继续 fail-closed，核心 pipeline 版本推进到
  `v47`。
- 2026-09-15：P5 `no-misused-promises` 继续覆盖对象属性与 JSX 属性值，新增 parser-owned
  callback value 范围并查询原生 contextual callback 签名；对象/JSX spread 保持未知并
  fail-closed，核心 pipeline 版本推进到 `v48`，parser source pipeline 同步推进到 `v34`。
- 2026-09-15：P1 项目图补齐跨文件 global ambient 值投影。声明文件中的 `declare global` 与
  script 顶层 `declare` 值按源码名称合并到项目图，逐文件 lint 使用外部 ambient SymbolId；
  本地模块绑定优先，namespace/module 成员不泄漏到全局，核心 pipeline 版本推进到 `v49`。
- 2026-09-15：P1 项目图继续投影跨文件 ambient namespace 根。`declare global`/script 声明文件
  的 namespace 根可在消费文件解析，成员仍只保留在 namespace 内，核心 pipeline 版本推进到 `v50`。
- 2026-09-15：P5 `no-misused-promises` 补齐迁移配置边界。规则注册表新增
  `checks_conditionals` 与 `checks_void_return` 两个默认开启的独立开关，分别控制同步条件和
  void 回调检查；catalog 同时列出 `promiseCallback` 消息 ID，避免配置解释与实际诊断集合不一致。
- 2026-09-15：P1 收紧跨文件 global ambient 投影。`declare global`/script ambient 函数签名的参数
  不再被误投影为项目级值；新增 no-undef 失败回归锁定函数/嵌套签名绑定只在其原始签名作用域内有效，
  核心 pipeline 推进到 `v51`。
- 2026-09-15：P5 断言等价证明补齐标准 PromiseLike 身份。相同标准库 thenable 的断言现在与
  Promise、Function、RegExp 一样可被源绑定类型图证明为无需断言，核心 pipeline 推进到 `v52`。
- 2026-09-15：P5 断言等价证明收紧泛型参数身份。不同源码类型参数即使拥有相同约束也不再
  被当成同一类型，新增正反例回归，核心 pipeline 推进到 `v53`。
- 2026-09-15：P5 断言等价证明对未建模类型保持 fail-closed。两个独立 `Other` 类型节点不再
  因默认关系相同而被误报为无需断言，核心 pipeline 推进到 `v54`。
- 2026-09-15：P5 断言等价证明补齐标准 PromiseLike 身份的递归节点比较。共享泛型引用目标但
  一个节点来自标准库 `PromiseLike`、另一个不是时不再被误报为同一类型，核心 pipeline 推进到
  `v55`。
- 2026-09-15：P1 源码值作用域补齐运行时 namespace 中的 ambient namespace 父子关系。通过
  `declare` 包装的嵌套 namespace 现在挂在最近运行时 namespace 的 IIFE 参数作用域下，根模块
  不再错误解析其成员；核心 pipeline 推进到 `v56`。
- 2026-09-15：P5 `no-misused-promises` 完成 `checks_void_return` 的迁移对象配置。除布尔短写外，
  `arguments`、`attributes`、`properties`、`returns`、`variables` 可分别启停，省略成员默认开启；
  事实完整性校验与诊断按启用位置执行，核心 pipeline 推进到 `v57`。
- 2026-09-15：P1 源码值作用域补齐 runtime namespace 与同名 ambient namespace 的声明合并。
  两种声明现在复用同一个 namespace 根及成员作用域，声明顺序不会把 ambient 成员泄漏到模块根，
  核心 pipeline 推进到 `v58`。
- 2026-09-15：P1 收紧 `declare global` namespace 的值空间边界。global namespace 使用独立根作用域，
  与同名模块 runtime namespace 保持遮蔽而不合并；点分路径和重复声明仍在各自空间内合并，
  核心 pipeline 推进到 `v59`。
- 2026-09-15：P5 类型事实补齐匿名结构对象、接口和 class 的属性值关系。原生
  `getPropertiesOfType`/`getTypeOfSymbol` 结果现在进入受限 source-bound 类型图，`{ value: any }` 及嵌套属性的
  `no-unsafe-assignment`/`no-unsafe-return` 递归证明不再被对象外层隐藏；缺失属性事实继续
  fail-closed，核心 pipeline 推进到 `v60`。结构对象赋值回归、原生类型服务 37/37、核心和
  应用门禁通过。
- 2026-09-15：P5 结构属性关系改为保留递归环并由核心单调固定点传播 any/error；`A -> B -> A`
  这类递归接口不会再通过删除回边而漏掉深层不安全属性，核心 pipeline 推进到 `v61`。
  核心递归关系回归、原生类型服务 38/38、应用和 Clippy 门禁通过。
- 2026-09-15：P5 赋值/返回类型事实补齐原生索引签名值类型。`{ [key: string]: any }`、错误值索引及
  命名属性共享同一结构关系和递归固定点；索引键与符号身份不进入核心，核心 pipeline 推进到 `v62`。
  新增原生 TypeScript 7.0.2 any/error 索引回归先失败后转绿。
- 2026-09-15：P1 资源绑定作用域回归完成：`using`/`await using` 与 `let`/`const` 一样只在其
  所属块或 `for` 迭代环境内解析，资源名不得泄漏到外层；循环头绑定在循环体和迭代表达式中可见，
  async 资源声明继续保持函数的异步作用域。现有 resolver/source 投影已满足该契约，新增
  `source_type_queries` 回归锁定普通块、循环头、async 资源声明及运行时 namespace 的顶层
  资源 IIFE 投影边界。
- 2026-09-15：P1 parser-owned 声明事实修复实现参数默认值泄漏。普通函数、重载、arrow、方法、
  构造器和嵌套解构默认值现在只保留参数模式与类型注解，生成 `.d.ts` 时删除可执行初始化器，
  新增 parser 回归并覆盖 TS2371 边界。
- 2026-09-15：P1 增加被擦除 `declare class` 成员签名的值作用域回归。成员类型查询继续解析
  最近外层值绑定，签名参数只在对应成员签名内可见；semantic source-query 回归推进到 42/42。
- 2026-09-15：P1 资源绑定作用域回归补齐运行时 namespace 边界。顶层 `using` 的类型查询与读取
  继续绑定 namespace IIFE 的资源环境，块/循环/`await using` 资源名不会泄漏到外层；semantic
  source-query 回归推进到 44/44。
- 2026-09-15：P1/R6 模板字符串先补齐 raw/cooked 双值事实。模板元素的 `raw` 继续保留源码
  转义，`cooked` 现在通过 lexer 统一解码普通 ECMAScript 转义；parser pipeline 推进至 `v35`，
  新增 raw/cooked 正反例。孤立 UTF-16 代理项仍等待无损字符串表示设计，当前不会伪造替换字符。
- 2026-09-15：P5 赋值/返回类型事实补齐函数属性的调用/构造签名返回类型。`{ callback: () => any }`
  及 unresolved 返回值现在沿结构属性关系递归传播，核心 pipeline 推进到 `v63`；参数标签和
  符号身份仍不进入核心。
- 2026-09-15：P5 断言等价证明补齐 enum 声明身份。不同 enum 即使拥有相同数字/字符串成员值，
  也不会再被当成同一类型；适配器把原生 enum symbol 映射为会话内身份，核心 pipeline 推进到 `v64`。
- 2026-09-15：P5 断言等价证明补齐 `unique symbol` 声明身份。不同 unique symbol 不再因共享
  `Symbol` 类别而误报不必要断言，适配器使用会话内身份，核心 pipeline 推进到 `v65`。
- 2026-09-15：P5 继续审计类实例的 polymorphic `this` 返回类型。类型服务保留 `this` 返回值
  与其 class 声明目标的关系；同一具体实例的断言可证明时报告，不完整或跨派生实例关系继续
  fail-closed。原生 TypeScript 回归已验证 `box.method() as Box` 与 `box as Box` 均可正确报告。
- 2026-09-15：P5 结构对象断言补齐 source-bound 属性形状。两个独立 interface 在属性名、
  可选性和递归值类型一致时按结构等价报告重复断言；不同属性名、可选性、class 名义身份和缺失
  属性关系继续 fail-closed。适配器把属性名/可选性转换为核心字段，不暴露编译器句柄；核心
  pipeline 推进到 `v66`，新增核心可选性回归，原生 TypeScript 服务回归增至 47/47。
- 2026-09-15：P5 增加非泛型 private class 实例断言回归，确认原生声明目标与私有成员身份在
  等价证明中保持区分；不同 class 的同形实例不会被误报为同一类型。
- 2026-09-15：P5 增加跨文件类型事实回归。`import` 引入的 any 值及其结构属性在同一
  TypeScript project 快照中继续参与 `no-unsafe-assignment`/`no-unsafe-return` 递归证明；
  类型规则 project 回归增至 35/35，无需把模块名或导出名称复制进核心类型图。
- 2026-09-15：上述 P1/P5 增量的 parser 92/92、semantic source-query 44/44、`wake_lint_core`
  全套、`wake_app` library 222/222、原生 TypeScript 服务 47/47（设置 `WAKE_LINT_TYPESCRIPT_EXE`）、
  parser/semantic Clippy、格式、文档和差异门禁通过。
- 2026-09-15：上述增量的核心类型回归、原生类型服务 33/33、workspace Clippy、文档和差异
  门禁通过；workspace 测试主体通过，但本机 Windows `Global\\wake-output-publication-v1`
  mutex 返回 `ERROR_ACCESS_DENIED`，因此一个既有 `wake_app` 输出提交集成测试受宿主权限阻断。
- 2026-09-15：使用 workspace PnP loader 运行架构门禁；`check-architecture.mjs` 通过，
  `scripts/check-architecture.test.mjs` 的 59 项契约测试全部通过。直接裸调用 Node 会因未加载
  `.pnp.cjs` 报缺包，不能作为架构失败证据。
- 2026-09-15：P5 修复泛型实参无序比较、递归失败分支污染、索引/调用签名缺失造成的冗余断言误报。
  类型事实显式区分完整与未采集的对象形状，索引键/值/只读性参与比较；完整空接口和递归同形接口
  可以证明等价，调用/构造参数关系未建模的对象继续跳过。核心 pipeline 推进到 `v67`。
- 2026-09-15：P1/P5 修复表达式语句中断言和括号被擦除后的查询范围；parser pipeline 推进到 `v36`。
  原生 wire 通过直接表达式子节点索引定位所属语句，嵌套箭头函数不再被外层调用的覆盖范围误判为歧义。
- 2026-09-15：P1 模板 cooked 值补齐原始 CR/CRLF 归一化；字符串/模板的 LS/PS 行接续不再保留
  多余字符。新增 lexer/parser 失败用例及 Node 运行时对照，普通、压缩与模板降级输出全部一致。
- 2026-09-15：P5 结构断言补齐具名属性只读性，修复只读接口/`Readonly<T>` 转可写接口的误报；
  核心 pipeline 推进到 `v68`。声明权限从同一项目快照的私有 modifier 节点归一化，保留原始
  文件名访问未保存 overlay；getter/setter、普通 readonly 属性名和跨文件接口均有原生回归。
  合成/映射属性的有效写入权限尚未完整建模，保持形状不完整，不能从原声明猜测权限。
- 2026-09-15：本轮 `v68`/`v36` 验证通过：lexer 24 项、parser 94 项及各自集成测试；semantic、
  transform、codegen、lint core/LSP 全套；核心断言与形状验证 20 项；app library 222 项及应用集成
  测试；原生 `lint::type_service -- --ignored` 47 项（不包含独立的输出锁 helper）；compiler core
  和 minifier 全套。相关七个 crate 的 all-targets Clippy、架构检查/59 项契约测试、格式、文档、
  diff 检查，以及重新构建后的 CLI 多语言项目 smoke 和 LSP 协议 smoke 均通过。
  本轮没有将 P1、P5 或完整 P0–P7 标记为完成：无损 UTF-16、完整类型关系、扩展宿主接入及目标
  平台发布验收继续按阶段表推进。
- 2026-09-16：P5 补齐已实例化映射及普通合成属性的最终只读性，覆盖 `Readonly`、`Pick`、
  `Record`、`Partial`、`Required`、键重映射和 `+readonly`/`-readonly`；spread 不再错误继承
  原声明的只读权限。未验证的实例化/延迟绑定关系继续保持不完整。
- 2026-09-16：P1/P5 由 parser 记录 `as const`/`<const>` 的语法身份，修复上下文类型相等导致的
  冗余常量断言误报；核心 pipeline 推进到 `v69`，parser 推进到 `v37`。原生类型服务 48 项、
  核心断言回归 21 项、parser 断言回归 4 项通过；含注释、Unicode、同名普通类型与结构属性的
  正反例，编译 AST 结构哈希保持一致。
- 2026-09-16：上述 `v69`/`v37` 的 parser、lint core、app 和 LSP 全套回归、all-targets Clippy、
  架构检查及 59 项契约测试通过；迁移/SDK/后端定位 7 项 Node 测试、扩展 manifest、lint benchmark
  编译、重新构建的 CLI 项目/LSP 协议 smoke、文档、格式与 diff 检查均通过。
- 2026-09-16：P1/R6 建立无损字符串基础设施及候选 ADR 0068。先复现 `\ud800` 被旧解码器
  解成空值，再加入 `JsString`、独立驻留的 `JsAtom` 和 `decode_escaped_value`。11 项新增测试
  覆盖全部 65,536 个单码元、代理对、UTF-16 排序、跨边界拼接、内容身份和并发去重。
  common/lexer/parser/codegen 共 463 项测试、common/lexer all-targets Clippy、架构检查及
  59 项架构契约测试通过。旧 parser 仍使用 UTF-8 路径并拒绝孤立代理项；AST/优化 IR/源码
  事实/emitter 的迁移及端到端运行时、缓存验收待完成，未改变 parser 或 lint pipeline 身份。
- 2026-09-16：继续将无损值接通 lexer、AST、原始 JSX/switch 事实、优化 IR、常量折叠、
  TS enum、装饰器及普通/优化 emitter。修复普通输出跳过孤立代理项属性装饰器和 `\u{z}`
  未产生词法错误；属性/case 重复判断、truthiness 和 ARIA 分类保持码元语义。parser/core/
  minifier/codegen pipeline 分别推进到 `v38`/`v70`/`v21`/`v3`，定义值指纹按码元生成。
  Node 对照覆盖普通、压缩、模板降级、TSX，以及冷缓存/全新会话命中/内容失效。
- 2026-09-16：本轮 common/lexer/parser/transform/minify/codegen/lint core/compiler core/CSS-in-JS
  共 1033 项回归通过，另有新增 TSX 运行时对照通过；semantic/bundler/app/LSP 共 758 项通过，
  55 项默认忽略，其中原生 TypeScript 服务 48 项另行启用并全部通过。workspace all-targets
  check/Clippy、59 项架构测试、重新构建的 CLI 项目/LSP 协议 smoke、文档、格式和 diff 检查通过。
  静态模块元数据、原生类型服务字符串协议和 tagged 模板非法转义仍按 R6 继续，未将 P1 或
  整个 P0–P7 标记为完成。
- 2026-09-16：P1/R6 补齐 tagged 模板非法转义：lexer 接受 NotEscapeSequence，统一 TV 解码
  返回缺失 cooked，parser 只在无标签模板中报告早期错误。21 类非法转义覆盖四种模板片段，
  raw、其他片段、嵌套标签、TS 泛型推测和普通字符串错误互不混淆。普通类型擦除与源码/声明
  收集共用类型语法，嵌套对象、元组、签名、索引和 ambient 类型不再绕过模板检查；合法类型
  继续擦除且不增加运行时依赖。parser/core pipeline 推进到 `v39`/`v71`。
- 2026-09-16：本轮 lexer/parser/semantic/transform/minify/codegen/lint core/compiler core/app/LSP
  共 1248 项测试通过，55 项默认忽略，其中原生类型服务 48 项另行启用并通过。新增 Node
  对照验证普通/压缩/映射及启用模板降级的 raw/undefined 行为；workspace all-targets Clippy、
  59 项架构测试、重建后的 CLI 项目/LSP smoke、文档、格式及 diff 检查通过。
  静态模块与类型服务的字符串边界、P5 完整类型关系、P6 宿主和 P7 发布验收仍未完成。
- 2026-09-16：P1/P5/R6 将普通字符串类型字面量接入无损 `JsString`。先复现原生 JSON 通道将
  不同孤立代理项与三个实际替换字符合并，导致 switch 漏报和冗余断言误报，再以同一类型的
  无截断表示和 Wake parser 恢复值；别名、模板实例化和跨文件长字符串均有回归。枚举成员
  表示与含替换字符的结构属性名称仍无法通用恢复，明确返回分析失败，不作有损等价证明。
  核心 pipeline 推进到 `v72`；静态模块字符串、完整类型关系与扩展宿主仍待完成。
- 2026-09-16：上述 `v72` 的 lint core/app/LSP 共 528 项回归通过，58 项默认忽略，其中原生
  类型服务 51 项另行启用并全部通过。workspace all-targets Clippy、架构检查及 59 项契约测试、
  重建后的 CLI 项目/LSP 协议 smoke 通过；完整 P0–P7 继续保持实施中。
- 2026-09-16：P1/R6 将 import attributes 的字符串键和值从 UTF-8 名称路径迁移到 JsAtom/JsString，
  覆盖静态 import/re-export、TS import-type、动态 import options 和两类重复导入规则。先复现
  parser 拒绝合法码元、动态属性被降为未知及重复键漏报，再贯通源码事实、owned 请求、优化 IR
  和 emitter。Node 模块链接回调验证普通/优化/映射 ESM 产物的实际码元；项目回归覆盖解析别名、
  缓存命中及内容失效。静态同值键报告语法错误，动态对象保持后写覆盖，导出名约束保持独立。
  parser/core/minifier/codegen pipeline 分别推进到 `v40`/`v73`/`v22`/`v4`。
- 2026-09-16：上述属性变更的 AST/parser/semantic/transform/minify/codegen/lint core/bundler/app/LSP
  共 1598 项回归通过，58 项默认忽略，其中原生类型服务 51 项另行启用并通过；workspace
  all-targets Clippy、架构检查及 59 项契约测试、重建后的 CLI 项目/LSP smoke、格式、文档和
  diff 检查通过。静态模块说明符、
  ambient 模块与类型服务剩余字符串边界、P5/P6/P7 继续实施。
