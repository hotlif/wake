# Wake 原生 lint 实施契约

状态：实施中。0.1.41 以实验性能力交付 `wake lint` 和 Node 入口；完整 P0–P7 尚未完成。
长期候选边界见 [ADR 0049](decisions/0049-native-lint-product.md)。

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
| P7 | ESLint 迁移报告、性能基线、支持平台及 tarball 消费验证 | 迁移报告、lint Criterion 基线、JS/SDK tarball 消费检查、Windows 原生包及 Linux x64/ARM64 原生包 manifest 已通过；Linux runner 仍需 Unix test-host 可执行位和 npm consumer，macOS 原生包仍需目标 runner |

## 配置与项目契约（目标）

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

以下是首个完整发布的有限基线，当前完成项见实施日志；名称相似不表示参数与 ESLint 完全相同。
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

- 0.1.40 候选发布汇总：77 条基线规则、CLI/Node 项目流程、LSP 和独立 SDK 已拆分提交。
  parser v41 / 核心 v74 保留类型导入、ambient 模块、声明请求及动态 import/require 的 UTF-16
  字符串身份；解析器无法接收的路径明确报告 unresolved，不把已知字面量误作动态未知请求。
  最新应用、核心、声明生成和 LSP 回归已通过。静态运行时模块说明符、完整高级类型语义、
  第三方规则宿主与完整 P7 验收仍未完成，本次发布不代表 P0–P7 全部完成。

- `ts/restrict-template-expressions` 已通过类别/参数、联合/交叉/约束、缺失事实/抑制三组核心
  测试、真实编译器及未保存依赖测试，Node 边界 33 项、核心全量、应用 lint 单元 43 项、
  Clippy 和文档检查通过。泛型实例的声明目标关系已补齐，两条类型规则的泛型继承正反例通过。
  已实现 69/77 条规则，核心 v30；不安全成员访问继续实施，P5 及完整 P0–P7 尚未完成。

类型模板规则 `ts/restrict-template-expressions` 只检查无标签模板的原始插值，不检查标签函数
自行解释的参数。字符串、字符串字面量、模板字符串类型、string mapping 与字符串品牌交叉
始终允许；联合须全部允许，交叉有一个允许成员即可，泛型使用约束。默认允许 number/bigint、
boolean、null/undefined 和 any；`allow_number`、`allow_boolean`、`allow_nullish`、`allow_any`
可分别关闭。`allow_regexp` 和 `allow_never` 默认 false；RegExp 及派生类型须证明标准库身份。
对象、数组、symbol、unknown、void、无约束类型参数没有隐式安全证明。规则默认关闭、不修复，
诊断范围为插值表达式；缺失插值类型事实是分析失败，不能当作字符串。每份源绑定的类型关系
与签名共同受 200,000 条预算限制。

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

- 显式 globals 已按 [ADR 0061](decisions/0061-explicit-lint-global-bindings.md) 接入根配置、override、
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

- 已实现 [ADR 0059](decisions/0059-bounded-lint-file-execution.md)：独立文件复用进程共享线程池，
  单批最多 min(32, workers) 个文件，全进程最多两个准备/执行批次；等待与已入队任务均可取消。
  单文件 panic 被隔离，缓存/基线按路径汇总，发布仍在协调线程执行。真实单线程/四线程矩阵的
  诊断、修复全文、写入文件、缓存统计与基线完全一致。应用二十七项、内部十一项、Rust CLI 十项、
  真实 Node 十六项、真实 npm CLI 与 watch 冒烟及 Clippy/架构/文档门禁通过。当前未公布性能数字。

- 已实现 [ADR 0058](decisions/0058-lint-watch-lifecycle.md) 的共享文件监听：CLI 逐行 JSON 事件、
  Node 事件与重复启动/停止、配置错误恢复、目录删除/重建、未保存文档更新和显式基线变化。
  根目录被符号链接或 junction 重定向时拒绝复用上下文；普通目录重建可恢复。真实 Windows
  监听四项、后端代次/缓存循环两项、Node 十六项、CLI 十项、CLI/架构七十五项及类型/文档检查通过。
  平台矩阵仍待运行；Windows 子进程强制退出测试不作为 Unix 信号优雅退出的证据。

- 已实现 [ADR 0057](decisions/0057-versioned-lint-project-context.md) 的手动 `LintContext`：
  固定项目根、多个未保存源码、严格安全整数版本和关闭墓碑、同步请求代次、单分析执行与取消。
  检查仍复用项目入口和配置发现，内存文档绕过缓存；Node ESM/CJS 提供 create/check/update/close。
  已复现并修复 Worker 退出期间 N-API Promise 完成回调导致的原生崩溃；上下文及单次 lint 均通过
  owned 结果槽主动读取，不从后台调用已退出的 JS 环境，环境清理会取消并回收线程。
  应用三项上下文与两项确定性并发/关闭测试、原有二十项 lint/cache/baseline 回归、真实 Node
  十四项、Rust CLI 九项和 CLI/架构 Wake Test 七十五项通过；npm 类型、真实 npm CLI 冒烟、
  Clippy、架构及文档检查通过。手动上下文尚无自动文件监听，也未宣称 AST/类型增量完成。

- 已实现 [ADR 0056](decisions/0056-lint-suppression-baselines.md) 的显式抑制基线：CLI 的生成、
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
  缓存已按 [ADR 0055](decisions/0055-content-addressed-lint-cache.md) 接入 CLI/Node；默认关闭，只读
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

- 配置切片见 [ADR 0052](decisions/0052-lint-effective-configuration.md)：等级简写和 `{ level, options }` 整体替换；核心统一校验参数并填充默认值。
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
- 修复切片见 [ADR 0051](decisions/0051-lint-source-fix-transactions.md)。当前安全修复仅覆盖
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
- 每个行为切片必须先记录失败测试，再实现、聚焦验证。重构先运行基线。
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
- lexer/parser 变化运行对应 crate 测试；涉及 lowering 的变更追加 transform/codegen/compiler 回归。
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
- ADR 或边界变更运行 `corepack yarn architecture:test` 和 `corepack yarn architecture:check`。
  CLI/Node/LSP 接通后运行各自契约测试、npm 类型与包消费检查；发布使用 `engineering/TESTING.md`
  的支持平台矩阵。没有执行的门禁不能记为通过。
