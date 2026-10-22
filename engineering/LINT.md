# Wake 原生 lint 实施契约

状态：实施中。0.1.43 以实验性能力交付 `wake lint` 和 Node 入口；完整 P0–P7 尚未完成。
长期候选边界见 [ADR 0049](decisions/0049-native-lint-product.md)。

按问题定位：[阶段状态](#范围与完成定义) · [配置与项目](#配置与项目契约-目标) · [规则与分析](#规则与分析契约-目标) ·
[规则清单](#规则基线清单) · [诊断与修复](#诊断-抑制与修复契约-目标) · [缓存与扩展](#缓存-类型-扩展及发布-目标) · [实施门禁](#实施日志与门禁)。
标为“目标”的章节描述验收契约，已实现范围与剩余事项以状态说明及下方阶段表为准；用户用法见 [Lint 参考](../docs/reference/cli/lint.mdx)。

## 范围与完成定义

目标是 Wake 原生 JS、TS、JSX、TSX 和 React lint 产品。允许配置和规则迁移；不承诺运行 ESLint
JavaScript 插件、兼容其内部 AST 或逐字复制诊断。完整交付必须同时完成下述 P0–P7 和规则清单，
不能用少量规则、只有 CLI 或只有源码 token 的实现替代整个目标。

| 阶段 | 交付与验收 | 状态 |
| --- | --- | --- |
| P0 | 契约、规则基线、ADR；源码保留与类型服务实验支持后续边界决策 | 已完成：契约/ADR、源码 wire 边界、Windows TypeScript 7.0.2 宿主协议、多项目、未保存文本、PnP、取消与失败关闭证据均已通过；更完整类型语义归入 P5 |
| P1 | parser-owned 原始 JS/TS/JSX 结构、token、注释、语义事实；编译行为等价 | 部分完成：注释、token、JSX、TS 类型结构、局部类型符号/namespace 合并、enum member source scope、跨文件 global ambient 值投影、无损运行时字符串/导入属性和 tagged 模板转义；静态模块字符串及类型服务字符串边界仍待完成 |
| P2 | 配置、文件发现、规则引擎、CLI/Node、抑制、首批规则，真实项目闭环 | 已实现基础闭环；真实 CLI 项目矩阵覆盖 JS/MJS/CJS/JSX/TS/MTS/CTS/TSX/Markdown、修复预览和配置解释，并通过 React/TypeScript、Docs、workspace、PnP fixture 及 Preact/p-map 外部快照 |
| P3 | 安全修复、缓存、并发、watch、配置解释、批量抑制 | 已实现修复、缓存、基线、上下文、watch 与有界文件并发；Windows/Linux/macOS lint-product CI 矩阵已接入，目标运行证据待验收 |
| P4 | 本文规则基线逐项通过测试、文档和真实项目验证 | 规则基线与核心正反例已实现，真实 CLI 矩阵覆盖代表性 JS/TS/MTS/CTS/JSX/TSX/React 路径、React/Docs/PnP 仓库 fixture、三类项目根 fixture 及 Preact/p-map 外部快照；完整规则语义仍受 P1/P5 边界约束 |
| P5 | 类型服务、跨文件类型规则、未保存源码、项目与 PnP 失效 | 十条类型规则均已接入原生类型服务及项目流程；字面量联合（含 BigInt）、枚举成员、结构类型断言、同一泛型引用等价、具名/可选/索引属性、可调用属性返回值和泛型实参 any/error 递归证明已接入，剩余完整类型语义仍需后续扩展 |
| P6 | LSP、VS Code、扩展 SDK、处理器、扩展安装/测试/升级 | 已接入 LSP、VS Code 客户端、quick-fix、Node SDK、独立扩展 SDK 和内建 Markdown 处理器；宿主自动启用第三方规则仍保持显式边界 |
| P7 | ESLint 迁移报告、性能基线、支持平台及 tarball 消费验证 | 迁移报告、lint Criterion 基线、JS/SDK tarball 消费检查、Windows 原生包及 Linux x64/ARM64 原生包 manifest 已通过；发布前 5×3 tarball consumer 已接入 lint CLI/CommonJS/ESM 诊断及安装后的 Unix test-host 可执行位检查，目标 Linux/macOS runner 的实际通过证据仍待验收 |

## 配置与项目契约（目标）

### 原始名称与源码值

未定义名称规则使用原生源码值引用身份及显式全局配置。`js/no-undef` 检查普通值读取/写入，
`typeof` 参数默认 false，仅豁免直接 typeof 标识符；`typeof missing.member` 的根仍检查。
`react/jsx-no-undef` 独立检查开标签组件根；JS 规则排除该位置以避免重复诊断。类型名称、
类型查询、导出 live binding 不作为运行时求值引用；原始值语义不完整的名称不能据此报未定义。
项目图会把已解析声明文件中的 `declare global`、其中的 ambient namespace 根和 script 顶层
ambient 值合并为外部源码值身份，供其他项目文件解析；namespace 成员不提升，ambient 函数签名
参数和嵌套函数绑定也不投影，本地模块绑定优先，无法确定来源时继续 fail-closed。
运行时 namespace 内的 ambient namespace 继续挂在最近的运行时 namespace 源作用域下，不能退回
模块根或泄漏为文件级绑定。
同名 runtime namespace（包括多段 runtime 声明）与 `declare namespace` 按源码声明合并同一个
namespace 根及成员作用域；点分名称的每一级也沿同一路径合并，声明顺序不能改变同一根的可见性。
两条规则默认关闭、无自动修复，不改变既有版本化预设。

未使用绑定规则 `js/no-unused-vars` 与 JSX 使用标记 `react/jsx-uses-vars` 默认关闭。使用由
原生符号身份决定，包含读取、显式导出及类型使用；写入、丢弃结果的自增与自身递归不使绑定
变为已使用。类型参数、别名与接口使用独立类型身份并按原始声明位置关联双空间绑定，不能
以同名类型消除值绑定的未使用诊断。ambient、不完整值绑定、隐式 arguments 与合成声明跳过。
存在 with 或未遮蔽的直接 eval 时，整个文件保守跳过未使用检查，避免将动态读取当成未使用。
JSX 标记只影响已解析组件根，不独立产生诊断。删除声明可能丢失副作用，两条规则均不修复。

参数为 vars=all/local（默认 all）、args=after-used/all/none（默认 after-used）、
caught_errors=all/none（默认 all）、ignore_rest_siblings=false；vars_ignore_pattern、
args_ignore_pattern、caught_errors_ignore_pattern 默认为空（不忽略）。非空模式采用 Rust regex
语法，长度至多 4096 个字符，必须在 off/未匹配配置层同样验证；不是 JavaScript 正则字面量。
report_used_ignore_pattern=false；启用后报告实际使用但匹配忽略模式的绑定。类型使用和运行时
使用共享忽略规则，纯类型名称不参与函数实参位置的 after-used 判断。

`js/prefer-const` 默认关闭，按原生符号统计写入，不按名字匹配。检查带初始化器的 let、
for-in/of 逐次绑定，以及同一作用域中可放置声明的独立赋值语句；跨块/闭包赋值不建议迁移。
参数为 destructuring=any/all（默认 any）、ignore_read_before_assign=false。解构 all 需要整组
均可转换；普通多声明可分别报告，但 for 初始化器不能拆分。安全修复仅替换整个声明的 let
token，必须所有绑定均可转换且无缺失初始化器，保留注释、表达式和求值次序。延后赋值不自动
移动代码；with/未遮蔽的直接 eval 与原始值语义不完整的名称保守跳过。

### 字符、字符串与源码身份

源码标识符按 Unicode 17.0.0 的 ID_Start/ID_Continue 及 ECMAScript 的 $、_、ZWNJ、ZWJ
规则分类；原文与 Unicode 转义共用词法器。显式 globals 使用同一 parser，但只接收未转义名称。
字符属性采用已锁定 registry 依赖 unicode-id-start 1.4.0，保持原生 lexer 的语法所有权；不
生成或复制外部源码表到仓库。parser 身份随词法接受集合变化而更新，使 lint/构建缓存失效。
数字后的非 ASCII 字符也按实际标识符属性判断，Unicode 空白与行分隔符不能被误报为数字尾部名称。
JSX 引号属性由 parser 显式选择原生 lexer 属性模式，允许原始换行；反斜杠是普通字符，
不能转义闭合引号，也不验证 JS 转义。实体解码继续由 JSX parser 负责；普通 JS 字符串规则不变。

模板元素的 raw 保存源码拼写，cooked 按 ECMAScript TV 解码：原始 CR/CRLF 归一化为 LF，
转义 `\r` 仍产生 CR；字符串与模板中的反斜杠 LF/CR/CRLF/LS/PS 行接续均不产生字符，
未转义的 LS/PS 保留。源码 raw 与运行时标签函数的 strings.raw（TRV）不混用；emitter
输出源码模板，由运行时解释 TRV。普通、压缩及模板降级产物须与原始源码执行结果一致。
非法十六进制/Unicode 转义、超范围码点与数字转义使所在模板元素的 cooked 为 `None`
（运行时 `undefined`），保留完整 raw，并且不影响其他元素的 cooked。parser 仅在 tagged
模板中接受该值；无标签模板与 TypeScript 模板字面量类型必须诊断。标签不豁免未闭合模板、
插值表达式语法错误或插值内普通字符串的非法转义。
普通编译的类型擦除也必须验证嵌套模板，不得仅在源码事实/声明收集模式中验证。
语义依据见 [ECMAScript TV/TRV](https://tc39.es/ecma262/multipage/ecmascript-language-lexical-grammar.html#sec-static-semantics-tv)。
运行时字符串及属性键采用无损 `JsString`/`JsAtom`：普通、压缩、模板降级和持久缓存产物
保留孤立 UTF-16 代理项，重复属性/case 和常量判断按实际码元比较，不能与替换字符混同。
import attributes 的字符串键和值采用无损码元身份，静态 import/re-export、TS import-type 和
可静态求值的动态 import options 共用此语义；标识符键与同值字符串键相等。重复导入判断
按码元比较和排序，不能把不同孤立代理项或实际替换字符合并，也不能将已知属性降为未知。
普通与优化输出必须保留这些键和值；模块导出名继续使用其良构 Unicode 约束。
静态属性子句的同值键（包括标识符/字符串及不同转义拼写）按码元报告重复键语法错误；
动态 options 是对象表达式，静态投影保留后写属性覆盖前写属性的语义。
源码模块事实、ambient 模块名称、擦除的 TS import/export-type 与声明请求保留 JsString；
动态 import/require 的字面量请求同样保持已知码元身份。模块规则仅在进入 UTF-8 解析环境时
报告无法表示的说明符为 unresolved，不能把合法字面量降为动态未知。声明图保留不需本地
解析的外部请求原文；含孤立代理项的相对路径在文件系统边界明确失败，不能探测替换后的路径。
运行时静态 import/export AST 与依赖记录尚使用 UTF-8，不能表示的值明确诊断，不静默丢弃；
该迁移及原生类型服务字符串协议仍待后续验收，见 ROADMAP R6。
类型字面量与结构属性名称同样必须保留任意 UTF-16 码元，孤立代理项不能变成替换字符，
也不能与实际 U+FFFD 或反斜杠转义文本混为一谈；穷尽性和断言比较须使用无损值身份。
原生 7.0.2 的 JSON 字符串值会把孤立代理项替换为 U+FFFD。适配器遇到含 U+FFFD 的
字面量时须查询同一类型身份的无截断表示，由 Wake parser 验证为单个完整字符串字面量后
解码为 JsString；不得把类型别名或成员名当字符串值。枚举成员和结构属性名称尚无通用无损
恢复路径时返回分析失败，不能继续用有损值证明穷尽或结构等价；该边界继续纳入 R6。
合法的 ModuleExportName 本身要求良构 Unicode，依据
[ECMAScript 模块早期错误](https://tc39.es/ecma262/2025/multipage/ecmascript-language-scripts-and-modules.html#sec-module-semantics-static-semantics-early-errors)。

### 配置、文件发现与宿主环境

- 唯一声明式入口为 `wake.config.toml` 的 `[lint]`；lint 不执行 JS/TS 配置。
- 处理器通过 `[lint.processors]` 声明 `glob = "markdown"`；内建 Markdown 处理器只提取 fenced
  JavaScript 代码块，使用与宿主文档相同的 UTF-8 字节范围发布诊断。代码块外内容被遮蔽但不改写
  原文；当前处理器只提供诊断，`--fix`/写回在映射不可证明时明确拒绝，不把虚拟文档修复写回宿主。
- 默认项目根为命令工作目录；显式 `--root` 覆盖。只读取该根的配置，子包配置不隐式级联。
  Node 和编辑器必须明确传入同一项目根，多根工作区分别创建项目。
- `files` 默认匹配 JS/MJS/CJS/JSX/TS/MTS/CTS/TSX；声明文件默认忽略，可显式启用。
  `node_modules`、`.git`、`.yarn`、`.wake`、`dist`、`docs-dist` 以及 PnP 生成的
  `.pnp.cjs`、`.pnp.mjs`、`.pnp.loader.mjs` 默认忽略；显式路径仍会报告“被忽略”的执行错误。
- 模式相对项目根，路径统一为 `/`；支持 `*`、`?`、`**`。`**/` 可匹配零级目录。
  不隐式读 `.gitignore`；忽略为集合排除，不支持含糊的 `!` 反选，非法模式报错。
- 配置合并顺序：引擎默认 → 按声明顺序的预设 → 顶层规则 → 按声明顺序的匹配 override →
  显式 CLI 规则设置。规则设置整体替换，不递归拼接参数数组。
- 规则等级为 `off`、`warn`、`error`；带参数使用 `{ level = "warn", options = { ... } }`。
  未知字段、规则、预设或无效参数是配置错误，即使等级为 off 也校验规则身份和参数。
- 默认启用版本化 `recommended` 预设；默认不启用风格、类型检查及需全项目图的规则。
  `all` 必须显式选择，其变化随版本说明发布。预设变更进入缓存身份。
- `--print-config <file>` 输出最终配置和各设置来源；读取 stdin 必须提供虚拟文件名。
- globals 采用 readonly/writable/off，按标准默认、根配置、匹配 override、显式请求顺序逐名
  覆盖。off 删除默认或先前全局，名称必须是单一未转义标识符；所有层在匹配前校验。
  标准默认集合冻结为 ES2024，不从 Node/浏览器宿主探测；`environments = ["browser", "node"]`
  可显式加入版本化的浏览器或 Node 全局集合，环境名称先规范化排序后合并，由显式 globals 最后覆盖。
  可用环境名称为 `browser`、`node`，未知名称即使没有启用规则也报配置错误；环境集合及版本进入缓存身份。
  配置解释保留模式和来源，缓存使用最终有效绑定，等价层叠具有相同身份。
- 显式路径不存在、显式模式无匹配、显式文件被忽略分别报可区分的执行错误；通过未来显式开关
  才可容忍无匹配。递归发现不跟随目录符号链接，重复物理文件只分析一次。

## 规则与分析契约（目标）

### React Hooks

两条 `react-hooks/` 规则默认关闭、无自动修复。调用路径契约见
[ADR 0062](decisions/0062-native-call-execution-paths.md)；两条规则均已接入原生规则引擎。
`rules-of-hooks` 结合原始绑定与函数身份检查组件/自定义 Hook 的调用位置、提前返回、条件与循环。
React 导入别名和命名空间由符号识别；普通对象同名成员不冒充 React。自定义 Hook 使用原始
绑定的 use + 大写字母/数字命名约定；不完整或无法解析的绑定不猜测。普通 Hooks 不允许进入
try/catch/finally；React `use` 可用于条件与循环，仍受 React 函数及异常捕获限制。
`exhaustive-deps` 以回调捕获的真实符号和属性路径比较依赖列表，区分组件局部响应值、模块值、
回调自身绑定与 React 明确定义的稳定输出；动态依赖和无法分析的回调必须报告能力缺口。
缺失/多余/重复依赖不自动改写，避免改变 effect 执行频率或引入更新循环。详细参数与稳定集合
由实现前的聚焦契约和测试冻结；不直接继承 ESLint JS 配置或执行其规则。

`rules-of-hooks` 的组合依赖在目录中记为 `scope-control-flow`。有效所有者为名称首字母大写的
函数/变量、自定义 use + 大写字母/数字函数、匿名默认导出，以及真实 React `memo`/`forwardRef`
的第一个函数参数（含具名 render 回调）；其他具名函数表达式优先使用自身名字。变量声明
包含 for 初始化器，赋给已有原始标识符绑定的匿名函数继承绑定名称。类方法、初始化器、普通回调、模块、
async/generator 和参数默认值不能调用 Hook。不可达调用不诊断；可达的普通 Hook 不能进入
try/catch/finally 或循环，且在有正常返回路径时必须为每条正常路径必经。未捕获 throw 不构成
提前正常返回。React `use` 仅豁免条件与循环限制；异常区域与函数所有者限制仍有效。
React 来源须是静态 `react` 导入的原始符号；具名别名以及 default/namespace 成员受支持，
其他库的具名自定义 Hook 按其原始导出名称识别。普通对象成员、动态 require、经赋值传递的
匿名别名不进行 React 来源推测。每次调用仅报告一个最直接的位置原因，不提供修复。

核心错误区分配置、无效数据、修复与分析失败；图预算超限通过 CLI/Node/上下文统一返回
`WAKE_LINT_ANALYSIS`（CLI 退出码 2），包括修复重分析。失败不会缓存部分结果或发布源码。
资源限制不作为用户可调开关；图默认单文件最多 100,000 节点、400,000 边及 10,000,000 工作步。

`exhaustive-deps` 依赖 scope 事实，默认关闭且无自动修复。识别真实 React `useEffect`、
`useLayoutEffect`、`useInsertionEffect`、`useMemo`、`useCallback` 的第一个回调/第二个数组，
以及 `useImperativeHandle` 的第二个回调/第三个数组。`additional_effect_hooks` 为可选 Rust
正则（默认空，最多 4096 字符），匹配原始已解析的自定义 Hook 名称，采用第一个回调/第二个数组。
effect 可省略数组；memo/callback 缺少数组会诊断。数组须为字面量；spread、空位和复杂条目
均报告动态依赖。回调可为内联函数或无后续写入的局部函数绑定；未知、重赋值或异步 effect
回调必须报告，不能推测其捕获。

响应值是所属函数内、回调之外的原始值绑定（含参数）；回调局部和模块绑定不需依赖。捕获
按真实符号与静态属性路径比较；`props.user` 覆盖 `props.user.name`，动态属性捕获对象和键，
成员方法调用依赖接收对象。嵌套回调中的捕获也计入。稳定集合冻结为无写入的原始 const
字面量、真实 React useRef 返回对象及 useState/useReducer/useTransition 数组的第二个绑定。
不捕获响应值的局部函数可稳定，递归函数集合只在整个集合没有不稳定外部捕获时成立。
稳定集合不靠同名函数推断。缺失依赖逐条报告；重复条目、外部/可变 `.current` 依赖报告；
memo/callback 另报告未使用依赖，effect 允许额外响应依赖触发重新同步。回调内对响应绑定
直接赋值报告陈旧赋值；这不包括 ref.current 等对象属性写入。所有结论保留原始范围与符号，
未表示的 TS 值环境报告分析不可用，不把无法解析的捕获当作依赖已齐全。
可变 `.current` 仅由真实 useRef 输出身份确认，普通对象同名属性仍是响应路径。父路径捕获
覆盖子路径，避免重复建议；局部回调绑定自身已在数组中时，它的身份覆盖内部捕获。无数组的
effect 仍检查异步回调、陈旧赋值和不可用作用域。捕获与稳定性传播共享单文件 10,000,000
工作步上限，超限返回同一分析错误，不返回中间诊断。

### 风格规则

`style/semi` 默认关闭，mode=always/never（默认 always）。parser 在真实语法路径记录普通
语句、导入/导出、类型别名、类字段及 do-while 的显式或 ASI 结束位置；for 头部分隔符、
空语句、接口/类型成员分隔符不属于本规则。记录随推测解析回滚，普通编译不分配此集合。
always 在缺少分号的结束位置插入，never 只删除 parser 能保守证明可省略的显式分号。
同一行必须的分隔、后续调用/索引/模板/正则及运算符等表达式延续屏障保留；do-while 使用
其独立的可省略语法。修复保留注释、Unicode 与换行，不重新解释词法文本或改写空语句。
类字段名本身也是 static/get/set/async 等成员前缀时，保留其与后续成员之间的显式分隔。
中立事实沿用 ADR 0053 的 AST 数据、parser 语法所有权，parser/cache 身份随事实更新。

`style/comma-dangle` 默认关闭，mode=never/always/always-multiline/only-multiline（默认 never）。
parser 记录原始数组/对象（含解构）、实参/形参、具名导入/导出、导入属性、TS 类型参数、元组
和枚举列表的边界与尾逗号。普通括号/序列表达式不当作形参，推测路径不得重复产生事实。
多行指最后元素与闭合符之间有 ECMAScript 换行；always-multiline 要求此时存在尾逗号，
only-multiline 仅允许此时存在。空列表、数组尾部空位、rest 参数不新增逗号；原始 spread
处保守不新增，以免 cover grammar 成为 rest。never 不删除数组空位或 TSX 泛型消歧所需逗号。
修复只插入/删除语法记录的一个逗号，不移动元素、注释或改变数组长度。

`style/indent` 默认关闭，style=spaces/tabs（默认 spaces），width 为 1–8 的整数（默认 2）。
每个语法分组、无花括号控制流体与 switch case 正文增加一级；闭合符对齐对应分组外层。
跨行声明/表达式至少增加一级续行缩进；不按已有缩进或可视列猜测对齐，不保留任意手工列对齐。
JSX 子元素与多行属性、TS 类型分组使用原始 parser 事实。只修复非空行开头的空格/tab；
字符串、模板、JSX 文本正文和多行注释内部保持原文，空行不检查。所有换行与 Unicode 内容保留。
JSX 标签前的行首空白仅在 parser 的 LF 文本折叠明确忽略它时修复；CR/LS/PS 文本边界保留。
width 即使 tabs 模式或规则关闭也必须验证；tabs 每级一个 tab，width 不改变 tab 模式的文本。

### 规则注册与作用域分析

注册表是规则 ID、类别、语言、默认等级、参数 schema、消息 ID、文档和分析依赖的唯一事实源。
所有规则 ID 均命名空间化。规则只读不可变源码与分析事实，返回诊断及候选文本编辑，不访问磁盘。
分析依赖分为词法、语法、作用域、控制流、模块图和类型；只有启用规则需要时才计算。
不将 parser 注入的 JSX helper 当成用户声明或引用，不以文本匹配替代 Hooks/类型/作用域分析。

`js/no-redeclare` 默认关闭、无参数、无修复；按原始声明 occurrence 的真实值作用域检查重复
声明，后续声明逐条报告。参数与 var 共享绑定时计入，独立参数/函数体环境、独立块和隐式
arguments/Annex B 绑定不伪造重复。TS 的 namespace 与 class/function/enum 合并、重复 namespace
和 enum 声明允许；纯类型接口/别名与擦除的函数重载不当成运行时重复声明。没有环境全局
配置时不猜测宿主内建名称。

`js/no-use-before-define` 默认关闭、无修复；报告原始值引用在其目标最早显式声明之前出现的
文本顺序问题，包含闭包中的前向引用和枚举成员初始化中的前向成员引用。`functions`、`classes`、
`variables` 默认 true，可分别关闭对应目标类别（参数和枚举成员归 variables）。这是声明顺序规则，不声称诊断完整 TDZ 或运行时
控制流；类型引用/查询、live-binding 具名导出、未解析名称和不完整值名称不充当求值引用。

`js/no-shadow` 默认关闭、无修复；显式值声明遮蔽父作用域的原始值绑定时报告，每个本地名字
只报告首条声明。`hoist = true` 默认包含外侧稍后出现的声明，false 只检查已经出现在前面的
外侧声明；`ignore_named_expressions = true` 默认跳过具名函数/类表达式的私有自引用名称。
独立作用域之间没有父子关系时不报告；隐式 arguments、参数复制、Annex B 复制和合成 helper
不伪造成外侧源码声明。未完整表示的值名称保守跳过，不猜测宿主内建或类型空间的遮蔽。
同一缺失值声明事实也约束内建名称规则：擦除的本地 console、Promise、NaN 等声明不能
被误认成宿主全局。原始值环境完全表示前，相关名称在该文件中保守跳过。

原始静态 import 记录覆盖声明范围、源节点父级、解码后的模块字符串与范围、默认/命名空间/具名/
equals 绑定、外部名与本地名、声明和说明符的 type 标志，以及带关键字和范围的引入属性。
TS 擦除不能删除这些源码事实；equals 的实体别名保留右侧范围且不伪造外部模块。普通编译解析
仍不分配这些集合；源码收集不改变编译 AST、运行时依赖和既有声明事实。

原始 export 记录保留具名、星号、默认、声明、TS export-assignment 与 UMD namespace 形式，
包含本地/导出名称各自的解码值和范围、类型修饰符、来源、引入属性、声明或表达式目标范围，
以及源节点父级。无 `from` 的具名标识符才是本地引用；别名、外部重导出名和 UMD 导出名
不能伪造本地使用。显式类型导出生成 TypeReference，不混入值引用。类型导出的引入属性
按同一语法解析，但仍不产生运行时依赖；这些语法记录本身不宣称已完成 TS 类型空间绑定。

源码值导出使用由 semantic 以相同 SymbolId 单独返回，不充当表达式求值读取；提前导出
后续声明的 live binding 不触发 TDZ。具名本地导出、导出声明（含解构）、具名默认函数/类和
运行时 TS namespace 成员保留原始绑定关系，重导出与纯类型导出不能让同名值变量变成已使用。
源码导出收集不改变普通 `analyze` 的符号、声明和引用表，合成 namespace 赋值不充当用户读取。

源码类型查询另存为不求值的使用记录，不添加运行时 Read，也不改变编译 SymbolId。parser 保留
原始函数、参数/返回类型起点和函数体范围；方法的计算键与装饰器位于函数参数环境之外。semantic
按真实块、循环、switch case（包括运行时 namespace 内的 switch）、catch（包括 catch 参数）、类静态块、枚举成员与函数环境解析 `typeof value` 的根名称，成员名
不伪造成本地引用；switch 判别表达式位于 case 的词法环境之外。
查询结果区分已解析、在已表示环境中未找到，以及事实不完整而不可解析。顶层 `declare` 与
`declare global` 的值绑定会恢复到模块值环境，并重新解析对应的原始运行时引用；非字符串 ambient
namespace 的根和直接值成员会进入各自源码值作用域，字符串 ambient module 的本地值成员也在
独立外部模块作用域中解析；无函数体的声明签名参数进入独立签名值作用域，因此其返回类型或类型谓词中的
`typeof parameter` 可按源码身份解析；具有 parser 值参数绑定的纯函数/方法类型签名也进入独立签名值作用域；无值参数绑定的纯类型签名复用最近外层值 scope，完整的外层名称可以解析，缺失或外部未知名称仍保持 `Unavailable`；已有语义 scope 的普通块、函数体、switch 环境和枚举成员初始化环境中直接 `declare` 值成员也按所属 scope 身份解析；有独立参数环境的函数头复用真实参数 scope，默认值、计算解构键和返回类型中的 `typeof parameter` 可按源码身份解析；具名函数表达式的自引用名称在默认参数和函数体类型查询中保持参数环境外侧的独立身份，不能泄漏到外部。字符串 ambient module 的未知外部值不能回退到文件模块作用域。枚举成员标识保留为独立 source binding，只在所属枚举初始化环境中可见，不伪造成模块级普通值声明。
无函数体声明签名中，首个值绑定是函数/类样声明本身，后续参数绑定只进入该签名的独立值作用域，不会泄漏到模块作用域或其他签名，
不得回退到同名外部变量或把它当成 `no-undef` 的证明。这是原始 TS 语义的增量事实，不能据此
宣称类型空间与所有 ambient 绑定已经完整；函数体内已有值绑定仍使用普通 semantic 的身份。
被擦除的 `declare class` 成员类型签名同样保留最近外层值作用域；成员签名参数仍只在自身签名
范围内可见，不把类成员名或参数泄漏到模块值空间。
函数事实还记录原始箭头身份；当消费者传入已经把箭头降级为普通函数的编译树时，不能将
降级函数新产生的隐式 arguments 绑定当成原始查询目标。缺少对应环境时保留 Unavailable。

原始类型局部作用范围由 parser 的产生式和作用范围恢复点记录，不能从名称或相邻值引用猜测。
泛型参数覆盖同一参数列表的约束、默认类型和所属声明/函数/类，嵌套签名分别拥有自己的范围。
映射键仅覆盖 `as` 重映射与值类型，不覆盖自身 `in` 约束；条件类型的 infer 绑定覆盖自身约束
和对应真分支，不覆盖 extends 模式中的普通引用或假分支。同一 infer 声明在这两个范围中
保持同一个源码 occurrence，不能复制成两条声明。记录保留 cooked 绑定名称、原始范围、类别和父范围，
失败试探完整回滚。它们是语法事实，不意味着已完成模块类型声明、类型合并或类型检查。

原始类型断言保留 `as`、尖括号、非空尾缀和 `satisfies` 的不同种类，以及整个表达式、操作数
和可选类型范围。范围遵守关系运算优先级并包含原始括号与链式断言；构造器操作数不包含
先行 `new`。源码模式在擦除前记录并随推测回滚，普通编译不分配该集合。是否可移除断言
仍须由类型事实和相应规则判断，不能仅依据语法或两段文本相同。

原始调用另存普通/可选调用、构造器、标签模板和动态 import 的完整范围及实参范围。调用头
包含 callee 与可选/类型参数语法，构造器头不含 `new`；标签模板的参数是源码插值，不包含
隐式字符串数组。模板插值保留擦除前的完整表达式范围，纯类型模板不混入运行时集合。
这些事实在产生式提交时记录，失败的泛型/箭头试探回滚；JSX、namespace、enum 的合成调用
不进入集合。动态 import 的可选参数允许尾逗号，for 初始化中的参数仍按允许 `in` 的表达式解析。

原始成员访问保留整个访问、接收者和属性的范围，区分命名、私有和计算属性，并记录可选链
标记。接收者和计算键保留括号及擦除前的类型断言，new 的构造器成员不把 `new` 记入接收者。
嵌套访问按产生式提交顺序记录；失败的泛型/箭头试探回滚。类型索引、JSX 标签名与生成的
namespace/enum/JSX helper 不伪造表达式成员记录。普通编译不分配这些源码元数据。

`ts/no-unsafe-member-access` 对每个原始成员访问分别检查接收者及计算键的 any/error 类型，
使用泛型约束；接收者诊断落在属性范围，计算键诊断落在键表达式。链中每个不安全操作均可
报告，不从其他操作的诊断推断安全。`allow_optional` 默认 false；设为 true 只豁免显式 `?.`
操作的接收者检查，不豁免计算键。unknown 或普通对象不被当作 any；缺少事实是分析失败。
规则默认关闭，无自动修复。

类型别名和接口另存原始声明节点与名称，普通块、函数体、switch case 体和类静态块保留词法
容器，不能因类型擦除而把块内声明提升到模块。namespace 保留每个点分名称、原始主体范围，
字符串 ambient module 保留解码后的模块名；嵌套类型声明继续指向原始容器，不能归属降级 IIFE。
类和 enum 的名称也保留原始声明身份，类表达式的名称仅属于自身范围；类声明的名称属于外侧
声明环境。类范围从 class 关键字开始，不把先行装饰器表达式纳入自身名称/泛型范围。

类型绑定表使用独立于编译值 SymbolId 的身份；接口合并和 namespace 的多段声明保留每个
声明 occurrence。普通块内声明不泄漏；类型参数、映射键与 infer 只在 parser 指定范围内
可见。显式导出的 namespace 类型成员可以被该 namespace 的另一段声明引用，私有成员不能。
导入绑定的类型使用按本地身份记录，不推断外部模块成员；值引用与不求值的类型查询不混入
普通类型引用。未解析类型名称仅表示本地表没有目标，不作为缺少类型库或模块的错误诊断。
ambient namespace 的成员继承声明上下文并隐式导出，跨声明的使用不能误归属同名模块导入。
`declare global` 的声明进入独立全局层；模块本地导入与声明仍可遮蔽它，不与全局声明合并。
当同名本地 SymbolId 已存在时，全局投影不得把该本地绑定标成 ambient，也不得让声明级规则
跳过它；本地绑定的使用、未使用和遮蔽判断继续按本地身份进行。
字符串 ambient module 可能扩展外部模块。没有模块图时，其中在本地未找到的类型引用保留
Unavailable，不能越过这层未知成员去捕获外部同名导入；ambient body 内已表示的本地声明可在
其独立作用域解析；同一源文件中相同模块字符串的 ambient body 共享值与类型成员 scope；项目图
合并 `declare global` 与 script 顶层 ambient 值，字符串 ambient module 的跨文件成员类型合并仍由
原生 TypeScript 项目服务负责。
字符串 ambient module 的同名保护按查询 scope 生效，不能遮蔽已经解析的本地值；函数体或未表示
  namespace 的不完整名称仍保持文件级保守保护，避免用外层绑定证明源码环境完整。规则消费端
  同样按源码符号身份应用该边界；字符串 ambient module 的同名擦除声明不能抑制另一个作用域中
  已表示本地绑定的 `no-unused-vars` 等声明级诊断。

函数参数绑定遵循 [FunctionDeclarationInstantiation](https://tc39.es/ecma262/2024/multipage/ordinary-and-exotic-objects-behaviours.html#sec-functiondeclarationinstantiation)：
参数没有表达式时与同名 var/function 共享绑定；有默认值或计算解构键时，函数体 var 环境与
参数环境分离，参数初始化创建的闭包不能解析到体内声明。每条声明 occurrence 保留原始类别。
参数到同名体内 var 的隐式初值复制单独记录；压缩分析从当前 IR 的声明作用域重建这种名字约束，
保留相关声明与拼写，避免两个独立符号各自改名后丢失参数初值。

具名函数表达式的自引用名称拥有函数参数环境外侧的独立词法环境。同名参数、体内 var/function
或 let/const 分别按自身绑定遮蔽它；默认参数创建的闭包仍只能看到参数或外侧自引用名。该名称
不泄漏到外部；对它的赋值保留 JavaScript 的不可变自引用语义，不能被压缩器当成普通可写变量。

普通函数（含方法）按函数实例化规则拥有隐式 arguments 绑定；箭头函数继承外层绑定。名为
arguments 的参数，以及无参数表达式时的同名函数/词法声明，会按规范阻止创建隐式绑定；同名
var 不应丢失参数对象初值。semantic 必须取得当前 Interner 来识别内建名字，不能假设 Atom
有固定数值或让源码分析与普通编译各自解析一遍。隐式声明不伪造源码 declaration occurrence。
压缩器需要保留 arguments 的拼写、初值以及非严格简单参数列表与索引的双向映射；允许保守地
限制受影响参数的优化，但不能以参数重命名、常量传播或删除声明改变这种关系。

非严格 Script 的块级普通函数遵循
[ECMAScript Annex B.3.2](https://tc39.es/ecma262/2024/multipage/additional-ecmascript-features-for-web-browsers.html#sec-block-level-function-declarations-web-legacy-compatibility-semantics)：
块内词法函数与可选外层 var 是不同绑定，执行到声明时才复制函数值。参数或沿途词法声明冲突
会阻止外层绑定；async/generator、严格代码和类静态块不使用该兼容性复制。隐式外层 var 没有
伪造的源码声明 occurrence，复制记录不冒充用户表达式。未执行的分支也必须保留外层 var 的
存在性。压缩分析从当前 IR 识别含非严格块级函数的输入，在可证明安全地表达隐式复制之前，
保留该输入的声明、拼写和控制结构，避免独立重命名或删除分支改变运行结果；正常严格输入继续优化。
只有非严格 `if` 的直接分支与合法标签位置接受普通函数声明；严格代码、async/generator
分支以及循环/with 的直接语句位置拒绝该形式。需要声明时可显式写块，不将非法输入静默降级。
含参数表达式的函数可能在执行块级 `function arguments` 时，才在函数体 var 环境中创建同名
绑定。复制事实保存目标环境，并以空目标符号表示这种运行时创建；不把它伪装成参数环境中
既有 arguments 的静态写入。这类动态解析也受上述优化保护约束。

`ts/consistent-type-imports` 默认关闭、无参数、无自动修复。对 TS/TSX 中有实际类型使用、没有
运行时读取/写入/值导出的普通 import 绑定要求显式 type；具名、默认和 namespace 导入分别按
本地声明身份判断。不把内层泛型/块声明的同名引用计给导入；已有 type 导入、完全未使用绑定、
副作用导入、带 attributes 或 equals 的导入不报告。`typeof` 的已解析导入使用属于类型使用；
事实不完整的查询保守地阻止建议。类型导入不会自动加入预设，也不自动删除模块副作用。

`ts/consistent-type-exports` 默认关闭、无参数、无修复；本地具名 export 的目标仅包含接口、
类型别名或显式 type import 且没有值绑定时，要求在导出项或整条导出上标记 type。存在值导出、
擦除后尚未表示的同名值声明，或目标是普通 import/外部重导出时不猜测。值投影集中保留这些
不完整名称，类型规则不以空的编译解析结果证明源码没有值声明；不完整值名称也不能屏蔽已经
解析到源码类型符号的类型导出，两个命名空间按符号身份分别判断。

`ts/array-type` 默认关闭，只检查 TS/TSX 类型语法。`syntax = "array"`（默认）要求 `T[]` /
`readonly T[]`，报告单参数、非限定名的 `Array<T>` / `ReadonlyArray<T>`；`syntax = "generic"`
要求泛型形式，报告数组后缀。索引访问 `T[K]`、元组、值表达式和带命名空间限定的自定义类型
不属于该规则。规则按语法书写形式工作，不解析同名类型的定义；当前仅报告，不提供自动修复。
`extends`/`implements` 的基类型保留合法泛型形式，内部实参仍按数组语法检查。
原始类型引用、泛型参数、前缀算子、数组与索引访问各自由 parser 保留，名称按 lexer 解码。

### JSX 无障碍规则

无障碍规则读取 parser 保存的 JSX 值事实：属性、spread、子表达式和文本关联原始节点；
字符串实体解码、子文本空白归一化与编译解析使用同一实现。只把字面量、无插值模板和简单
数值/void 一元式保存为确定原始值；动态表达式及名为 undefined/NaN 的标识符保持 Unknown。
布尔简写没有伪造的表达式范围，空注释容器保存 Empty；这些事实不执行表达式或解析运行时 DOM。

无障碍规则默认关闭，只检查 JSX/TSX 的内建标签，组件与运行时 spread 保持不确定状态。已知
属性按源码顺序覆盖，后续 spread 可能覆盖已有值，后续显式属性可重新确定该值。动态表达式
不凭名称判定空值；不能证明缺陷时不报告。基线行为参考
[WAI 图片替代文本](https://www.w3.org/WAI/WCAG22/Techniques/html/H37.html) 与
[label 关联](https://www.w3.org/WAI/WCAG22/Techniques/html/H44.html)，不宣称代替运行时可访问性审计。

- `a11y/alt-text`：img、area、input[type=image] 需要文本替代（img 可用空 alt 表示装饰），
  object 可用非空 title、ARIA 名称或回退内容；ARIA 名称包括 aria-label/aria-labelledby。
- `a11y/anchor-has-content`：a 需要可访问的标签/内容；忽略静态 hidden/aria-hidden 子树，
  允许数字 0、图片替代文本、动态内容及组件，空注释、false/null 与纯空白不算文本。
- `a11y/anchor-is-valid`：a 的 href 不能确定为缺失、空、#、javascript: 或非字符串原始值；
  未知 href 不报告，空锚点应由 button 等合适标签承担交互。
- `a11y/label-has-associated-control`：可见 label 需要文本及关联。非空 htmlFor 建立显式关联；
  未设置 htmlFor 时允许内部可标记控件。空 htmlFor 不回退为隐式关联；不跨另一个 label，
  不将 input[type=hidden] 当成控件，也不猜测远处 id 是否存在。

上述规则只报告，不自动生成替代文本、事件处理器、URL 或关联 id。

ARIA 名称和值校验使用固定的 [WAI-ARIA 1.3 2026-06-04 草案](https://www.w3.org/TR/2026/WD-wai-aria-1.3-20260604/)
词汇（包含 1.2 属性及仍保留的 deprecated 项）；这是版本化词汇选择，不宣称草案已经成为正式标准。
角色另包含 [DPUB-ARIA 1.1](https://www.w3.org/TR/2025/REC-dpub-aria-1.1-20250612/)
与 [Graphics ARIA 1.0](https://www.w3.org/TR/graphics-aria-1.0/) 的具名角色。

- `a11y/aria-props` 报告显式 aria-* 属性拼写错误，名称须为规范小写形式；不检查组件 prop。
- `a11y/aria-proptypes` 检查覆盖顺序确定的最终原始值。null/void 表示不设置属性；动态值不报告。
  字符串类允许 DOM 原始值字符串化；ID 引用需要非空单项或列表，枚举/枚举列表需要规定 token，
  数字须有限、整数须无小数（字符串整数使用十进制整数字面形式）。空字符串只属于字符串类型，
  不检查 ID 目标是否存在、角色允许哪些属性或数字之间的大小关系。
- `a11y/aria-role` 检查显式、可确定的 role：空串、非字符串、没有已知具体角色的 token 列表
  均报告；未知和抽象角色不能独立充当角色，有具体角色的后备列表允许保留未来 token。
  缺失/null/void、动态 role 和后续 spread 不报告。

键盘规则参考 [WAI 键盘交互实践](https://www.w3.org/WAI/ARIA/apg/practices/keyboard-interface/)，
只分析 JSX 可确定的标记结构；静态隐藏的祖先、禁用状态及动态 spread 会影响判断。

- `a11y/click-events-have-key-events`：可见、未禁用、没有原生键盘交互的标签显式设置 onClick
  时，需要 onKeyDown/onKeyUp/onKeyPress 至少之一。null/void/false 不算事件处理器；显式动态
  表达式按处理器存在检查，但 spread 只能表示不确定。原生控件、带 href 的链接、带 controls
  的音视频、可编辑内容和 details 中第一个 summary 豁免；none/presentation 角色豁免。
- `a11y/interactive-supports-focus`：可见、未禁用的交互 ARIA 角色设置点击或键盘处理器时，
  需要原生焦点能力或整数 tabIndex。允许 -1 支持程序化/移动焦点；本规则不证明完整 Tab 顺序。
  角色覆盖按钮、链接、输入、选项、菜单、树、网格与其交互成员；进度条和 meter 不算交互控件。

两条规则均不插入处理器或 tabIndex；祖先隐藏判断同样用于链接内容和 label 检查。

### React 列表与 key

列表规则参考 [React 列表与 key](https://react.dev/learn/rendering-lists)。parser 在 spread 降级前
保存原始数组范围、元素/空槽、spread 标志和元素表达式范围；普通编译不分配这个事实集合。
数组 cover grammar 的记录须与最终表达式/绑定结构共同解释，不能把解构默认值当成列表元素。
JSX lowering 注入的 children 数组不产生原始数组事实。

- `react/jsx-key` 检查显式数组元素，以及 `.map` / `.flatMap` 的内联函数返回值中的直接 JSX。
  条件/逻辑分支和最后一个序列表达式保留列表上下文；不穿透调用包装器、对象属性或嵌套函数。
  列表元素最外层需要 key，内部普通 JSX 子节点不要求额外 key；短 Fragment 无法传 key，须报告。
  显式 key 或不确定的展开属性可满足静态存在检查；确定为 void 的 key 视为缺失。
- `react/no-array-index-key` 通过符号身份识别 map/flatMap 回调的第二个标识符参数；报告 key
  表达式对该绑定的读取，包含模板和运算，排除同名遮蔽。只检查直接内联回调，不猜测任意函数
  的参数语义或追踪跨函数别名；key 的后续显式覆盖和 spread 按源码顺序解释。

两条规则默认关闭、无自动修复，不生成可能改变组件身份的 key，也不宣称静态 key 一定唯一。

### JavaScript 导入与表达式规则

`js/no-duplicate-imports` 默认关闭，按相同源节点作用域、解码后模块字符串和属性内容检查重复
静态 ES import，属性顺序不敏感。默认也检查纯类型与值导入的重复；布尔参数
`allow_separate_type_imports = true` 允许两者各保留一次，内联全 type 的导入属于纯类型组。
动态 import、TS import-equals 和不同属性内容不合并；每个后续重复的模块字符串范围各报告一次，
不执行路径解析或自动删除可能承载副作用、注释及使用关系的声明。

`js/no-constant-binary-expression` 默认关闭、无参数和修复，适用于 JS/TS/JSX/TSX。基于真实表达式
结构检查 `&&`/`||` 左侧恒定真值、`??` 左侧恒定空值性、常量原始值比较、严格相等的类型不相交，
以及无法在右侧求值前被取得的新建数组/对象/函数/正则引用比较。不传播赋值产生的引用新鲜性，
不假设构造器返回新对象、类静态初始化不泄漏类，也不把未知标识符 undefined/NaN 当成全局常量。
宽松比较保留对象到原始值转换语义，不把 `[] == false` 与 `[] === false` 混为一类。

### 类型服务规则

`ts/await-thenable` 默认关闭、无修复。它检查原始 `await` 操作数的类型事实：标准库 Promise、
标准库 PromiseLike 及其可证明的派生/联合/交叉类型允许；已解析为明确非 thenable 的类型（如 number、string、
boolean、null、undefined、symbol、普通对象和函数）报告。`any`、`unknown`、error 类型及
缺失或不完整的类型关系不作为安全证明，也不猜测名称；操作数的类型事实必须绑定同一份未保存
源码快照和项目。每个 await 只产生一条诊断，范围为操作数，不自动改写异步语义。

`ts/no-unnecessary-type-assertion` 默认关闭、无修复。对 `as`、角括号断言和 `satisfies` 保留
原始操作数及断言表达式的类型事实；只有两者在当前项目中可由源绑定类型图证明为结构等价时报告，
同一引用目标及其逐项等价且不含 `any`/`unknown`/`error` 的泛型实参也参与证明，因此同一
`Box<string>` 的重复断言可被识别；标准库 Function、Promise、PromiseLike 与 RegExp 身份也参与
等价证明，且标准身份字段在递归比较的最终节点关系中保持一致；类型参数先按源码身份比较再考虑约束；未建模的 `Other` 类型身份保持 fail-closed，
不同 enum 声明的身份也必须保留，即使成员运行时值同为数字或字符串，不能仅凭字面量相等消除跨 enum 断言；
不同 `unique symbol` 声明也必须保留名义身份，不能仅凭 `Symbol` 类别消除断言；
接口和匿名对象的结构证明要求完整属性形状：具名属性名称、可选性、只读性、递归值类型，以及索引签名的键类型、值类型和只读性都参与比较。
属性写入权限不可证明时保持形状不完整；不得把只读属性与可写属性的断言消除。
已完成实例化的映射属性使用后端最终权限，覆盖 `Readonly`、`Pick`、`Record`、`Partial`、
`Required`、`+readonly`/`-readonly` 和键重映射；复制原始声明不能覆盖映射产生的权限变化。
普通对象和 spread 的合成属性、const 上下文的只读属性同样使用最终权限；未验证的合成关系继续
保持不完整。`as const`/`<const>` 建立字面量和只读上下文，不能仅因上下文已反映在操作数类型中
而被报告为冗余断言。
没有完整函数参数事实的调用/构造对象不能只凭具名属性判等；未采集的对象形状与已验证的空形状必须区分。
泛型实参按声明位置逐项比较，引用目标必须是同一个源绑定节点；联合/交叉成员仍允许顺序不同。
递归比较只对当前路径上的回边使用循环假设，失败分支不能成为另一分支的等价证据；超出比较预算时跳过诊断。
带继承关系的 class 泛型实例也同时保留声明目标和基类关系，不能因继承信息存在而丢失泛型引用身份；
非泛型 class 实例同样保留 class 声明身份，尤其是含 private/protected 成员的两个不同声明，不能
仅凭相同成员形状消除断言；
而不完整实参、字面量到宽类型、不同联合成员或不同对象身份
的断言不会因共享基础类别而误报。非空断言不由
类型规则重复检查（`ts/no-non-null-assertion` 负责语法规则），any、unknown、error、缺失类型
关系和跨项目未归属值均跳过。诊断范围为完整断言表达式。

`ts/no-floating-promises` 默认关闭、无修复。检查原始表达式语句的最终表达式类型，包括调用、动态
`import()` 以及裸 Promise/PromiseLike 标识符或成员表达式；
标准库 Promise、PromiseLike 或其可证明派生类型必须被 `await`、返回、赋值或显式 `void` 消费；
即使文件只有顶层浮动表达式，也必须保留标准库身份并执行该判断。
表达式语句的查询范围保留完整原始表达式，包括 `as`、`satisfies`、非空断言和括号包装；
不能使用擦除后的操作数范围查询表达式语句的直接子节点。
嵌套函数体中的表达式绑定自身的语句父节点，不能因外层调用表达式也覆盖该范围而拒绝有效查询。
嵌套调用按最外层表达式判断，未解析、any、unknown、error 和非 thenable 返回不报告；类型事实
必须来自同一项目快照，不能从函数名或字符串推断 Promise。

`ts/no-unsafe-assignment` 与 `ts/no-unsafe-return` 默认关闭、无修复。它们分别检查原始变量初始化/
赋值表达式的右值和带值 `return` 的表达式；右值被原生类型服务证明为 `any` 或 error 类型时报告，
泛型引用的类型实参也递归参与证明，因此 `Promise<any>`、`Map<string, any>` 等嵌套 any/error
不能通过外层对象类别隐藏；
匿名结构对象、接口、class 和映射类型的属性类型也递归参与证明，因此 `{ value: any }`、索引签名、嵌套对象属性和数组/元组成员
中的 any/error 不能通过结构对象外层隐藏；函数属性的调用签名返回类型也递归参与证明；索引签名值类型参与不安全传播，键类型和只读性另供结构断言比较，编译器符号句柄不进入核心；
这些关系跨越同一 TypeScript project 中的 import/export 文件边界，仍绑定当前文件系统快照；
原生类型服务缺少属性关系时保持 fail-closed。
递归属性图使用单调固定点传播 any/error，不因循环引用删除有效属性关系。
明确安全的类型、`unknown`、缺失事实和语法错误不猜测。诊断范围是右值，事实必须绑定同一源码快照。

`ts/no-misused-promises` 默认关闭、无修复。配置 `checks_conditionals`（默认 true）和
`checks_void_return`（默认 true）分别控制同步条件与回调位置检查；`checks_void_return` 也接受
包含 `arguments`、`attributes`、`properties`、`returns`、`variables` 布尔成员的对象，未指定成员
默认开启，便于直接迁移 ESLint 配置。关闭的检查族不要求调用方提供对应的类型事实。检查 `if`、`while`、`do while`、传统 `for`、条件表达式
以及 `&&`/`||` 的实际条件操作数；同时检查调用参数中返回标准库 Promise/PromiseLike 的回调是否
被传给 contextual type 明确要求 `void` 返回的函数位置，并检查变量初始化/赋值、`return` 表达式、
对象属性和 JSX 属性位置的同类 contextual callback。标准库 Promise、PromiseLike 及可证明派生/交叉/联合 thenable 用作同步
真值条件时报告，`async`/Promise 回调落入同步 `void` 参数时报告；
`await`、明确非 thenable、any/unknown/error 或缺失事实不作正向证明。条件、参数、赋值和 return
事实按 parser-owned 范围及原生 contextual type 查询绑定，不能从名称推断；对象属性和 JSX 属性的
spread 值没有静态属性身份时继续 fail-closed。

`ts/switch-exhaustiveness-check` 默认关闭、无修复。对没有 `default` 的原始 `switch`，若判别式
被项目类型服务证明为联合类型则报告；含 `default`、非联合、any/unknown/error 或缺失事实跳过。
字符串、数字、布尔、`bigint`、`null` 和 `undefined` 字面量联合会按 JavaScript `switch` 相等
语义检查每个成员是否被源码 `case` 覆盖；BigInt 保留精确的规范十进制值，不以浮点数近似，因而
不同进制和数字分隔符仍能匹配同一成员。未知 case 表达式和未保留字面量的成员继续 fail-closed
报告。枚举成员表达式会查询其源绑定的原生成员类型并按运行时值匹配，覆盖字符串、数字、bigint
和 `const enum` 成员；类型服务若返回格式非法的 BigInt 值（包括空数字或不属于声明进制的字符），
必须返回 `WAKE_LINT_ANALYSIS`，不能把它降级为零值继续发布诊断。更完整的类型语义仍留在后续验收。

类型模板规则 `ts/restrict-template-expressions` 只检查无标签模板的原始插值，不检查标签函数
自行解释的参数。字符串、字符串字面量、模板字符串类型、string mapping 与字符串品牌交叉
始终允许；联合须全部允许，交叉有一个允许成员即可，泛型使用约束。默认允许 number/bigint、
boolean、null/undefined 和 any；`allow_number`、`allow_boolean`、`allow_nullish`、`allow_any`
可分别关闭。`allow_regexp` 和 `allow_never` 默认 false；RegExp 及派生类型须证明标准库身份。
对象、数组、symbol、unknown、void、无约束类型参数没有隐式安全证明。规则默认关闭、不修复，
诊断范围为插值表达式；缺失插值类型事实是分析失败，不能当作字符串。每份源绑定的类型关系
与签名共同受 200,000 条预算限制。

### 规则基线清单

以下是首个完整发布的有限基线，当前阶段状态见[范围与完成定义](#范围与完成定义)；名称相似不表示参数与 ESLint 完全相同。
每项实施前补充参数、正反例、支持语言及修复边界；未经登记不得宣称兼容。

| 批次 | 规则 ID（共同前缀只写一次） |
| --- | --- |
| P2 / `js/` | `no-debugger`, `eqeqeq`, `no-empty`, `no-duplicate-case`, `no-dupe-keys`, `no-constant-condition` |
| P4 / `js/` | `no-unused-vars`, `no-undef`, `no-redeclare`, `no-shadow`, `no-use-before-define`, `no-unreachable`, `consistent-return`, `no-fallthrough`, `no-unsafe-finally`, `no-cond-assign`, `no-constant-binary-expression`, `no-self-assign`, `no-self-compare`, `no-duplicate-imports`, `no-async-promise-executor`, `no-promise-executor-return`, `no-sparse-arrays`, `valid-typeof`, `use-isnan`, `prefer-const`, `no-var`, `no-console` |
| P4 / `ts/` | `no-explicit-any`, `no-non-null-assertion`, `consistent-type-imports`, `consistent-type-exports`, `no-namespace`, `no-empty-interface`, `array-type`, `ban-ts-comment` |
| P4 / `react/` | `jsx-key`, `jsx-no-duplicate-props`, `jsx-no-undef`, `jsx-uses-vars`, `no-danger`, `no-children-prop`, `no-array-index-key`, `self-closing-comp` |
| P4 / `react-hooks/` | `rules-of-hooks`, `exhaustive-deps` |
| P4 / `a11y/` | `alt-text`, `anchor-has-content`, `anchor-is-valid`, `aria-props`, `aria-proptypes`, `aria-role`, `label-has-associated-control`, `click-events-have-key-events`, `interactive-supports-focus` |
| P4 / `import/` | `no-unresolved`, `no-cycle`, `no-duplicates`, `no-restricted-paths`, `no-extraneous-dependencies`, `order` |
| P4 / `style/` | `semi`, `quotes`, `indent`, `comma-dangle`, `no-trailing-spaces`, `eol-last` |
| P5 / `ts/` | `no-floating-promises`, `no-misused-promises`, `await-thenable`, `no-unnecessary-type-assertion`, `no-unsafe-assignment`, `no-unsafe-call`, `no-unsafe-member-access`, `no-unsafe-return`, `restrict-template-expressions`, `switch-exhaustiveness-check` |

## 诊断、抑制与修复契约（目标）

- 诊断携带规则 ID、等级、消息 ID、参数、原始源码主范围、附加标签、文档链接、fix/suggestions。
  排序按规范路径、起点、终点、规则 ID、消息 ID；并行和缓存不能改变顺序。
- 内部范围是 UTF-8 字节半开区间，文本接口行列从 1 开始；LSP 明确转换为 UTF-16。
  坐标以检查的源码快照计算，不能在渲染时改读较新的磁盘文件。
- 支持 `wake-lint-disable`、`wake-lint-enable`、`wake-lint-disable-line`、
  `wake-lint-disable-next-line`，后接逗号分隔规则和可选 `-- 原因`。
  指令只来自 lexer 识别的真实注释，不解析字符串、正则或 JSX 文本中的伪指令。
  block disable 持续到 enable 或 EOF；重复 disable 幂等，不作为栈；空规则列表作用于全部规则。
  支持报告未知规则、无效指令和未使用抑制。语法/配置/内部错误不能被规则抑制。
- 单条 fix 是不可拆分编辑集合；范围合法、UTF-8 边界正确且集合内部不重叠。同起点插入视为冲突。
  按诊断稳定顺序选择互不冲突的完整 fix，未选择项等待下一轮重新分析。
- `--fix` 只应用规则声明为安全的修复；suggestion 必须由用户显式选择。
  每轮重新解析，最多 10 轮，检测重复源码和不收敛。新语法错误拒绝该轮输出。
- 写入前核对原始内容；每个文件采用原子替换并保留适用的文件属性。跨文件批次不承诺整体原子性。
  冲突、只读文件和写入失败返回可区分错误；stdin 和 dry-run 永不写磁盘。
- CLI 退出码：0 通过，1 存在规则 error 或超过 `--max-warnings`，2 配置/执行失败。
  修复命令依据最终源码重新检查；JSON 和纯文本输出不得包含 ANSI。
- 批量抑制是版本化显式基线文件，匹配规范相对路径、规则和源码上下文指纹，不能仅靠行号。
  模糊匹配不得自动抑制，过期条目可报告/清理；基线变化必须失效缓存。

## 缓存、类型、扩展及发布（目标）

### 模块图与导入规则

模块规则边界见 [ADR 0063](decisions/0063-lint-module-graph-snapshots.md)。源码
请求投影保留静态 import/re-export（含被擦除的纯类型项）、TS import-equals、动态 import
和未遮蔽 require 的原始字符串范围与属性。静态声明来源直接使用 parser-owned 事实，不将
JSX 合成导入或 namespace 辅助代码当作用户请求；import-equals 不重复记为普通 require。
动态请求不是字符串字面量、引入属性不能静态确定时保留未知状态。with、直接 eval 或原始值
绑定不完整不能证明 require 是模块加载函数。parser 错误时不返回部分请求作为有效图输入。
原始 `import("pkg").Type` 表达式按独立纯类型请求保留，使用同一类型语法路径记录字符串、
范围和 `with` 属性；未知属性容器保留不可知状态。source 模式复用既有结构化 attribute
语法，普通擦除编译不分配该集合，推测路径必须回滚记录。项目入口通过统一解析环境将
这些请求与传递依赖绑定，纯核心检查拓扑和导入策略。

模块图规则实施契约：核心图拥有不可变源码和 parser 请求，应用只填入
解析结果，不接受脱离源码的诊断范围。每个解析目标使用完整模块身份，包请求另外携带当前
issuer 最近 manifest 的 production/dev/optional/peer/self 声明事实。未设置的解析结果、重复
身份、越界目标和动态请求的伪静态目标拒绝冻结；语法失败的依赖节点保留不完整状态。
图最多 20,000 个文件、200,000 个请求、128 MiB 源码；拓扑每种边选择最多 10,000,000 工作步。
循环算法必须为迭代式线性 SCC，按边选择缓存，不能逐文件重复遍历整个图。

六条规则默认关闭、无自动修复，目录分析依赖为 `module-graph`。纯单文件入口启用这些规则
但没有项目事实时返回分析失败。项目诊断在真实注释指令之前加入，之后统一排序并应用基线。
图与文件由同一 owned 输入绑定，不能对旧图传入新源码继续报告旧范围。
项目执行只把有效启用模块规则的所选文件作为图根；传递依赖仍可来自未选择/被发现规则忽略
的文件。混合项目中的普通文件继续使用单文件缓存。模块检查在局部安全修复完成后，以所有
所选文件的最终虚拟源码和未保存覆盖建立图，完整规则/指令/基线成功后才发布所选源码。
模块项目预备阶段最多冻结 20,000 个所选文件及 128 MiB 输入/最终源码，图自身仍使用上述独立
预算；超限作为分析失败，不能发布部分修复。只启用单文件规则的项目保留现有窗口执行路径。

- `import/no-unresolved` 检查静态导入/重导出及 TS import-type，`commonjs`、`dynamic_imports`、
  `include_types`、`report_unknown` 均默认 true；`ignore` 是至多 64 个 Rust regex，逐个至多
  4096 字符。真正缺失目标报告 unresolved，非字面量或不可知解析报告 unknown；忽略仅按原始
  已知 specifier 匹配，不把动态字符串伪造为已解析。
- `import/no-cycle` 默认仅检查顶层静态运行时 import/re-export；`commonjs`、`dynamic_imports`、
  `include_types`、`ignore_external` 默认 false。启用相应选项才纳入 require/import-equals、
  动态或纯类型边。同 SCC 的边报告 cycle；可达缺失/未知请求、语法失败或动态作用域节点报告
  incomplete，不能据此证明无环。opaque 代码也是不完整节点，明确无 JS 依赖的资源是叶子。
- `import/no-duplicates` 以解析身份、原始声明容器和排序后的 attributes 比较静态 import；
  `separate_type_imports=true` 允许独立纯类型导入。不同别名解析到相同目标可重复，未解析目标
  不猜测同一身份；未知属性不进行合并证明。
- `import/no-restricted-paths` 的 `zones` 默认空，最多 64 项；每项必填 `from`、`to` Rust regex，
  匹配相对项目根且使用 `/` 的目标路径/issuer 路径，允许根外 `../`。可选 `except` 为目标路径
  regex 列表，可选 `message` 至多 4096 字符。字段封闭，所有 regex 同样受 4096 字符上限约束。
  每个命中的 zone 分别检查，完全相同的最终诊断统一去重；内建模块无文件路径，不能命中文件区域。
- `import/no-extraneous-dependencies` 按 issuer 的包声明而非安装可见性判断；self-reference 和
  production 允许，解析失败仍保留已知的包声明事实；`dev_dependencies=false`、`optional_dependencies=true`、`peer_dependencies=true`、
  `include_types=true`。不对内建模块或相对/绝对路径伪造包声明要求；包子路径仍按包根名称检查。
- `import/order` 检查静态 import/import-equals（包括副作用项），不检查 re-export。`groups`
  为 builtin/external/internal/parent/sibling/index/type/unknown 的完整无重复排列，默认即此顺序；
  `alphabetize=ignore/asc/desc`（默认 ignore）、`case_insensitive=true`、`newlines=ignore/always/never`
  （默认 ignore）。按原始声明容器分别检查；空白行定义为只含空白的完整源行，注释行不算空行。
  unknown 只影响排序分组，不能成为解析成功证明。规则不移动副作用导入或改写求值顺序。

### 缓存、监听与服务边界

- 缓存键包含源码、有效配置、语言、分析/规则/插件版本及抑制配置；图和类型规则进一步包含
  解析环境、tsconfig、锁文件/PnP 与实际依赖变化。损坏缓存视为 miss，不能复用旧诊断。
- watch/LSP 任务绑定项目代次与文档版本，旧结果不能覆盖新版本；取消、关闭、删除、重命名均有测试。
- watcher 对项目根内的文件、根父目录和真正的外部依赖分别建立最小覆盖；快照为路径身份保留的
  每级祖先目录不单独注册，避免把整个用户目录当作监听根。无法注册外部依赖时仍按 watcher
  诊断处理，不发布过期检查结果。
- 类型能力候选为独立类型服务和 Wake 原生规则。P0 实验必须覆盖 TS API 可用性、原始节点映射、
  未保存文本、monorepo/PnP 和取消；实验未完成前不得冻结服务线路或宣称类型规则可用。
  启用类型规则但缺少类型事实时明确失败，不退化成名称猜测。
- 原生类型服务的 stdio 帧只接受单个 `Content-Length` 头；未知、重复、截断或超限的帧头直接
  终止本轮分析，不能把后端输出当作可恢复的类型事实。
- 类型源码 wire 的路径比较沿用初始化响应 `useCaseSensitiveFileNames`，与项目成员身份保持
  一致；忽略 ASCII 大小写仅在后端明确声明不区分大小写时启用，不因宿主操作系统猜测策略。
- 内建规则不依赖动态插件宿主。自定义扩展通过版本化只读语法/分析视图与编辑协议，不能持有内部
  arena 或冻结编译 AST。P6 先验证独立规则包的安装、测试、错误隔离与升级，再接受宿主 ADR。
- 处理器拥有虚拟文档到宿主的精确范围映射；映射不可逆时只报告诊断，不回写修复。
- 迁移工具不隐式执行旧 JS 配置，映射全部已登记原生规则及安全的未命名空间/TypeScript ESLint
  别名，并输出已转换、语义差异、未知和需人工处理的逐项报告。
- 性能基线比较冷/热/单文件编辑/规则耗时/峰值内存；性能结果必须同时验证诊断和修复等价。
  ESLint 对照使用锁定 registry 版本和外部依赖，不复制第三方源码进入仓库。

## 实施日志与门禁

实施时选择 [TESTING.md 的 Lint 验证入口](TESTING.md#1-4-lint)，并遵循以下专项要求：

- 每个行为切片必须先记录失败测试，再实现、聚焦验证。重构先运行基线。
- lexer/parser 变化运行对应 crate 测试；涉及 lowering 的变更追加 transform/codegen/compiler 回归。
- ADR 或边界变更运行 `corepack yarn architecture:test` 和 `corepack yarn architecture:check`。
  CLI/Node/LSP 接通后运行各自契约测试、npm 类型与包消费检查；发布使用 `engineering/TESTING.md`
  的支持平台矩阵。没有执行的门禁不能记为通过。

历史切片、pipeline 版本和当时运行的测试数量移至 [Lint 实施记录](history/LINT_IMPLEMENTATION.md)。
这些记录只证明对应代码状态；当前完成度以上方阶段表为准，不把历史通过记录视为本次验证结果。
