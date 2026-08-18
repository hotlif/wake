# `@crab-dev/css` 工程设计

本文定义 `@crab-dev/css` 的 v0.1 契约与后续工程门禁。事实基线是当前仓库的
`npm/css`、`wake_css_in_js` 与 Wake bundler；尚未通过对应验收项的能力只属于路线图，
不能写进发布说明或用户文档的“已支持”列表。

## 1. 定位

`@crab-dev/css` 是 Wake 原生的、类型优先的构建期 CSS authoring API。它借鉴
[Linaria](https://github.com/callstack/linaria) 的标签模板和静态 CSS 抽取体验，但把安全、
确定性与失效边界提升为编译器契约：

- 静态样式在构建期抽取；生产业务运行时不解析、排序或注入这些规则；
- 编译器只解释可证明无副作用的纯数据表达式，不执行项目模块，也不回退到 JavaScript VM；
- 真正的动态值通过 CSS 自定义属性显式进入，边界由 `createVar` / `assignVars` 表达；
- 类名、动画名和变量名由版本化输入生成，可以用黑盒测试验证稳定性；
- 无法安全求值时失败并定位源码，不以猜测、静默运行时降级或执行用户代码换取兼容性。

这里的“零运行时”特指生产环境中静态 CSS 的生成与挂载。`cx` 和 `assignVars` 是刻意保留的微型数据
转换函数；使用它们不会在浏览器中解析 CSS 或创建样式表。

## 2. v0.1 范围与非目标

v0.1 聚焦七个框架无关 API：`css`、`cx`、`keyframes`、`globalStyle`、`createVar`、
`assignVars` 和 `defineTokens`。它们足以覆盖局部规则、类名组合、动画、全局规则、由组件状态
驱动的值，以及显式不可变的跨模块 design token。

下列能力不是 v0.1 已实现功能：

- `styled.*`、React 组件工厂或 props 插值；
- `ThemeProvider`、主题对象合并或主题类型生成；
- atomic CSS 生成、atomic 冲突消解或原子类去重；
- 执行任意 JavaScript/TypeScript、函数调用、getter、宏或构建期副作用；
- Babel、webpack、Vite、Rollup、esbuild 等独立适配器；
- Sass/Less、PostCSS 插件链、自动前缀和完整 CSS 优化器；
- 把编译器当成不可信源码的完整安全沙箱。它消除了用户 JavaScript 执行面，但解析器、
  resolver 和文件系统仍需各自的输入与资源上限。

使用标准 CSS 自定义属性手工搭建主题是可行的，但这不等于已提供 theme API。Crab 的全局副作用
必须使用顶层 `globalStyle`，以便后续按 owner 做可靠存活分析；内联 `@keyframes` 和
`:global()` 均不是公开契约。

## 3. 架构

### 3.1 分层

| 层 | 当前职责 | 禁止承担的职责 |
| --- | --- | --- |
| `npm/css` | ESM/CJS 入口、TypeScript 品牌类型、未经编译保护、`cx`、`createVar` 的保守运行时后备、`assignVars`、`defineTokens` 的深冻结 | 解析 CSS、生成 `<style>`、访问 React |
| `wake_css` | 共享 CSS CST、节点/声明/错误与源码 span；普通 CSS 的 import、URL、压缩和 Modules 变换 | CSS-in-JS 静态求值、编辑器协议、产品编排 |
| `wake_css_in_js` | 按 import 来源和语义符号识别 API，安全静态求值；消费共享 CST 展开嵌套并产出 JS 替换、CSS 和诊断 | 启动 Node/VM、读取网络、执行被编译模块、维护另一套 CSS 扫描器 |
| `wake_bundler` | 扫描依赖、传播静态导出、按模块调用转换、删除已完全消解的 marker import、聚合 CSS | 重复实现 API 语义 |
| Wake dev/build | 开发时服务重建产物，生产时输出带内容 hash 的 CSS 资源 | 让服务端执行依赖 DOM 的样式逻辑 |
| `wake_css_language` / `wake_css_lsp` | 编辑器模板发现、虚拟 CSS、消费共享 CST 提供提示和诊断协议 | 复制静态求值规则、维护私有 CSS 解析器、执行用户模块、接管 TypeScript 符号导航 |

编译器以 import 绑定的语义身份识别 marker，而不是搜索字符串 `css`。因此 import alias 可用，
局部参数或块变量对同名 API 的遮蔽不得被误编译。只有来自 `@crab-dev/css` 的七个名字具有
完整 v0.1 语义；仓库中的 JavaScript、TypeScript、示例、fixture 与发布依赖不得使用其他
CSS-in-JS 入口。

### 3.2 构建数据流

1. Scan 在模块图中发现受支持包的具名 import；完全未使用 CSS-in-JS 的项目跳过后续工作。
2. 编译器建立 import symbol、引用和顶层绑定的映射，避免同名误命中。
3. bundler 以依赖后序收集纯数据导出，并把可证明的 import 值传播到使用模块。环只使用已知
   部分；未知值不会被猜测。
4. 名称分配先于模板渲染，使同模块及跨模块样式引用可以求值而不形成“名称依赖 CSS 内容、
   CSS 又依赖名称”的环。
5. 模板静态片段原样保留，插值经安全求值器转为 CSS 文本；失败在 `@crab-dev/css` 下产生
   构建错误。渲染后的 CSS 由 `wake_css::syntax::CssSyntaxTree` 按 `StyleBlock`、`Keyframes` 或
   `Stylesheet` 入口解析，作用域检查、URL、嵌套、选择器、关键帧和声明边界都复用同一棵树，
   不存在 regex/字节扫描回退。
6. `css` 产出作用域类规则，`keyframes` 产出命名动画，`globalStyle` 产出完整全局规则；
   相应表达式替换成字符串或 `void 0`。
7. 只有某条 import 的全部绑定都被安全消解时，codegen 才能删除该 import。`cx`、动态位置的
   `createVar` 或 `assignVars` 仍需要 npm 运行时。
8. 生产构建把各模块 CSS 按稳定模块/声明顺序聚合为 CSS asset；开发构建当前随重建产物提供
   样式，并由现有 live-reload 路径刷新页面。

### 3.3 产物模型

转换结果包含四类彼此独立的数据：

- `span -> JavaScript replacement`；
- 可以安全删除的 import span；
- 按源码声明顺序排列的 CSS 文本；
- 带源码 span 的 warning/error。

替换与 CSS 必须来自同一次转换，不能只缓存 JS 而遗失样式。当前 bundler 在 CSS-in-JS 活跃时
因此不复用只保存模块体的旧持久缓存条目；这是正确性保护，不是最终缓存设计。

## 4. 公开 API 契约

品牌类型防止 API 之间的意外混用，但产物在 JavaScript 中仍是普通字符串或 style 对象。

### 4.1 `css`

```ts
function css(
  strings: TemplateStringsArray,
  ...interpolations: readonly CSSInterpolation[]
): ClassName
```

`css` 是编译期标签。Wake 把模板替换成稳定类名字符串，并以该类为父选择器抽取规则。v0.1
支持普通声明、受支持的嵌套选择器、条件 at-rule 和静态插值。跨样式选择器引用要显式加点：
``.${base}``，因为 `base` 的值是裸类名。

### 4.2 `cx`

```ts
function cx(...values: readonly ClassValue[]): ClassName
```

`cx` 按输入顺序展开字符串、falsy 值、任意深度数组和对象条件键，再以单个空格连接。v0.1
没有 atomic 生成器，因此也不承诺 atomic class 的属性冲突消解。编译器只在能证明语义
等价的简单调用上内联；其余调用保留小型运行时，这不影响结果契约。

### 4.3 `keyframes`

```ts
function keyframes(
  strings: TemplateStringsArray,
  ...interpolations: readonly CSSInterpolation[]
): KeyframesName
```

`keyframes` 是编译期标签，返回可直接用于 `animation` / `animation-name` 的名称。名称局部化，
模板体必须是关键帧步骤，不再包一层 `@keyframes`。

### 4.4 `globalStyle`

```ts
function globalStyle(
  strings: TemplateStringsArray,
  ...interpolations: readonly CSSInterpolation[]
): void
```

`globalStyle` 是编译期标签，模板中书写完整规则，例如 `:root { ... }` 或 `@font-face { ... }`。
它不返回类名，应放在入口可达模块的顶层。抽取顺序是可观察行为，因此重复选择器的同优先级
覆盖必须服从稳定模块/声明顺序。

### 4.5 `createVar`

```ts
function createVar(debugName?: string): CSSVar
```

`createVar` 返回形如 `var(--crab-...)` 的品牌字符串，可以直接插入 CSS；调用者不能再外包一层
`var()`。模块顶层 `const` 是规范用法，Wake 会把它静态替换成由模块身份与 binding 派生的
确定字符串。运行时后备只保证当前 JavaScript realm 内唯一，供未满足静态形态时保持语义；不得在组件 render、
循环或请求处理函数中反复创建变量。

### 4.6 `assignVars`

```ts
function assignVars(
  variables: Readonly<Record<CSSVar, string | number>>,
): Record<string, string | number>
```

`assignVars({ [accent]: value })` 校验 key 是 `var(--...)` 引用，剥掉外层 `var()`，返回
`{ "--crab-...": value }`，可直接传给 React 的 `style` 或写入其他框架的 style 绑定。它只做
数据转换，不触碰 DOM，不注入规则，也不持有全局主题状态。

### 4.7 `defineTokens`

```ts
function defineTokens<const T extends TokenValue>(value: T): DeepReadonlyToken<T>
```

`defineTokens` 为跨模块结构化 token 建立显式不可变身份。只有从 `@crab-dev/css` 语义导入、
直接初始化模块顶层 `const` 的调用会被编译器识别；参数必须能由 allowlist 静态求值。TypeScript
返回深只读类型，npm 运行时在不访问 getter、不执行用户函数的前提下深冻结同一个对象图。
普通对象/数组不获得该身份，跨 ESM 插值仍按不安全共享值拒绝。

## 5. 安全静态求值

### 5.1 允许集合

求值器采用 allowlist，而不是“尽量执行”。v0.1 的可证明纯数据集合包括：

- 字符串、有限数字、布尔、`null`、`undefined`；
- 无标签模板字符串；
- 数组、对象字面量与可静态求值的对象 spread；
- 对象成员与数组下标访问；
- 数字的一元正负、算术表达式，以及至少一侧为字符串时的 `+` 拼接；
- 前面已求值的顶层简单绑定；
- 通过静态 import/export 传播的上述值；
- 编译器明确登记的内建 marker，例如规范形态的顶层 `createVar()` 和 `defineTokens()`。

对象属性到 CSS 声明的兼容转换属于迁移能力，不扩大 JavaScript 求值范围。所有输出保持输入
属性顺序；哈希表迭代顺序不得泄漏进 CSS。

### 5.2 拒绝集合

函数/方法调用、构造器、getter、Proxy、条件与短路控制流、动态 import、环境变量、时间、随机
数、网络、文件读取和模块副作用均不执行。遇到不在 allowlist 的 AST 形态时：

1. 返回“未知”，而不是构造近似值；
2. 在插值源码 span 上报告错误，列出允许的替代写法；
3. `@crab-dev/css` 构建失败，不得留下半条声明或转为运行时 CSS-in-JS；
4. 任何非 `@crab-dev/css` 入口都不属于产品契约；不得通过 warning、静默丢弃声明或运行时
   降级把不受支持的入口伪装成成功构建。

普通对象/数组只有在模块内直接成员读取且未逃逸时才可静态使用；别名、函数参数、return、
spread、写入以及消费模块中的修改都会使对应值变为未知。跨 ESM 边界只传播 primitive、
class、keyframes、CSS variable 名，以及由规范 `defineTokens()` 建立的深冻结结构；这避免兄弟
importer mutation 造成静默旧值。模板内相对 `url()` 也 fail closed，因为当前独立 CSS
聚合尚未把 URL 依赖交给 asset loader；应暂时移入普通 `.css` 文件。

此模型的安全结论是“构建 CSS 时不执行项目 JavaScript”，不是“任意恶意仓库都可以无限资源
安全地编译”。解析深度、文件大小、依赖图规模和路径权限仍需要独立防护。

## 6. 确定性与名称

v0.1 名称身份采用版本化 schema，并至少混入规范化模块 ID、API 种类、binding 提示和同名
ordinal；CSS 内容不进入类名/动画名身份。这样只改声明值时名称保持稳定，利于 DevTools、缓存
与 HMR。路径分隔符统一为 `/`，Windows drive 形式规范化；任何进一步改变都必须提升名称 schema，
不能无版本地改变已发布名称。

确定性要求是：相同源码图、相同规范化模块 ID、相同编译器 schema 与相同选项，JS、CSS、
诊断顺序和 asset hash 逐字节一致。它不自动等于“任意绝对目录中的两个 checkout 名称相同”；
跨 checkout 稳定性只有在模块 ID 项目相对化并通过跨平台 fixture 后才能承诺。

规则合并顺序必须来自稳定模块拓扑和源码声明顺序，不得来自并行任务完成顺序或 hash map 顺序。
同名全局选择器和 cascade layer 的顺序测试是发布门禁。

## 7. 缓存、HMR 与 SSR 路线

### 7.1 持久缓存

v0.1 为防止“JS 命中缓存但 CSS 消失”，CSS-in-JS 活跃时避开只保存 codegen body 的旧缓存。
后续应引入原子的 `StyleArtifact`，至少保存：

- source digest、规范化模块 ID、名称 schema 和编译器版本；
- 本模块 AST/静态导出摘要与被读取 import 值的摘要；
- replacements、removable imports、CSS、诊断和 CSS source map；
- 会影响嵌套、压缩、目标和输出顺序的全部选项。

缓存命中恢复完整 artifact；任一字段缺失都视为 miss。冷构建与缓存构建必须 byte-for-byte 对拍，
不能把 Span、进程内 Atom/Symbol ID 或绝对机器路径直接持久化。

### 7.2 样式 HMR

v0.1 codegen 已为每个模块分配确定的 style ID；浏览器以 bundle runtime 的 require 函数对象作为
WeakMap owner，再在 owner 内按 style ID upsert/remove。这样相同输入的生成代码逐字节一致，独立
bundle 不会覆盖彼此，同一 runtime 的动态 chunk 则共享样式槽。重复执行模块不会累积匿名
`<style>`，复用节点时保持 cascade slot。dev server 对外仍以重建/live reload 为兼容基线；
后续细粒度 HMR 还需覆盖模块删除 prune、错误恢复保留最后成功样式以及跨 chunk 更新顺序。

### 7.3 SSR

生产 CSS 已是独立 asset，服务端执行 JS 不应访问 `document`。完整 SSR 仍需 entry/chunk 到 CSS
asset 的 manifest、去重与稳定 link 顺序；在这套协议交付前，框架/应用壳负责把 Wake 输出的 CSS
资源链接进 HTML。`assignVars` 的结果是可序列化 style 数据，服务端与客户端必须复用同一个静态
变量名，禁止 render 内 `createVar()`。

### 7.4 Source map 与诊断

JS source map 不等于 CSS source map。后续 artifact 要记录每条生成规则到模板片段的映射，并让
HMR overlay、生产 `.css.map` 和缓存恢复共享同一来源模型。在此之前不得宣称 CSS source map 已
完整支持。

## 8. 相对 Linaria 的可验证优势

Linaria 的[官方说明](https://github.com/callstack/linaria#features)同时记录了它的优点与边界：静态
CSS 抽取、熟悉的嵌套语法和 CSS 变量动态样式；当前版本依赖 WyW 评估模块，其
[Stability 说明](https://github.com/callstack/linaria#stability)指出 hybrid 策略在静态证明失败时可回退 evaluator，并提醒使用者
关注慢构建、失效风暴和构建期意外代码执行。`@crab-dev/css` 不以语法总量声称“全面胜过”
Linaria，而以下差异可以通过仓库测试复现：

| 维度 | Linaria/WyW 官方模型 | Crab CSS 契约 | 验证方式 |
| --- | --- | --- | --- |
| 构建期执行面 | hybrid 可回退模块 evaluator；样式依赖模块需避免副作用 | 永不执行项目模块，只解释 allowlist AST | fixture 导入含 `throw`、文件写入或无限循环的值模块；构建应安全报错且 sentinel 不变化 |
| 不确定插值 | 可交给 evaluator，行为依赖模块结构与配置 | 新入口 fail closed，错误指向准确插值 span | diagnostic snapshot + 构建退出码 |
| Wake 集成 | 需要选择并配置对应 bundler adapter，重复处理可能出错 | Scan/Link/Emit 同一管线，无 Babel/loader 双重配置 | 干净 fixture 只安装 Wake 与本包即可 dev/build |
| 动态值 | 主要由 React `styled` 通过 CSS 变量桥接 | 核心包用 `createVar` / `assignVars` 显式桥接，框架无关 | Node runtime、React SSR 与 DOM style 集成测试 |
| 名称稳定性 | 不作为这里的否定比较 | 发布版本明确 schema 和输入集合，内容修改不改身份 | 重复构建、只改 CSS 值、并行调度和 Windows/Linux fixture 对拍 |
| API 误配置 | 取决于所用集成 | 三个编译期 tag 到达运行时立即抛 `ERR_WAKE_CSS_NOT_COMPILED` | 直接 ESM/CJS 调用测试 |

这些是设计优势，不是未经测量的性能结论。构建耗时、内存、CSS 体积、浏览器 JS 体积只有在同一
应用、同一 Node/Rust 工具链、同一冷/热缓存条件下测量后才能写“更快/更小”。Crab CSS 当前的
明显代价是仅原生支持 Wake，且没有 Linaria 的 `styled`、preprocessor、atomic 与多 bundler
生态。

## 9. 里程碑与验收指标

### M0 — npm 契约（v0.1）

- ESM、CJS 与声明文件导出完全相同的七个 API；
- `cx` 对字符串、falsy、对象和至少 32 层嵌套数组保持顺序，ESM/CJS 结果一致；
- `createVar` 返回合法 `var(--...)`，`assignVars` 正确剥壳并拒绝非法 key/非有限数字；
- 直接调用 `css`、`keyframes`、`globalStyle` 都抛同一错误码和 Wake 配置指引；
- typecheck、Node runtime test、`npm pack --dry-run` 与 tarball 文件白名单通过。

### M1 — 安全编译核心（v0.1）

- 七个 API 的 import alias、局部遮蔽、ESM 导入删除/保留各有单元测试；
- `css`、`keyframes`、`globalStyle` 在 dev 与 prod fixture 中得到预期 CSS 和 JS 替换；
- 顶层 `createVar` 在 CSS 插值、跨模块导出和 `assignVars` 中得到同一个引用；
- 至少覆盖字面量、对象/数组、成员访问、算术、模板与三层静态 import 传播；
- 函数调用、环境访问、构造器和动态控制流全部以 error 失败，测试确认用户模块未执行；
- 生产 CSS、开发样式与运行 JS 的端到端测试通过，任一错误不得只生成半套产物。

### M2 — 确定性与回归门禁（v0.1 发布门禁）

- 同一 fixture 连续构建 100 次，JS、CSS、诊断与 asset 名逐字节一致；
- 在不同线程调度和至少 Windows/Linux 上对拍；若跨 checkout 名称尚未一致，发布文档明确限制；
- 只修改 CSS 声明值时对应类名/动画名不变，修改 binding/API/module ID 时身份变化；
- 在文件前加入不同 binding 的样式，不使已有 binding 名称无谓 churn；同名重复 binding 仍唯一；
- 全局规则和 layer 顺序不受并行完成顺序影响。

### M3 — 增量 artifact 与细粒度 HMR（路线图）

- 冷构建、同进程热构建、跨进程缓存构建产物逐字节等价；
- 修改叶子 token 后，只重算读取该值的反向依赖闭包，执行计数证明无关模块为零重算；
- 连续 50 次样式编辑后每模块至多一个 style 节点，无旧规则，错误恢复保留最后成功版本；
- 模块删除、重命名、global rule 顺序变化和缓存 schema 升级均有浏览器端到端测试；
- 提交可复现的冷/热构建耗时与峰值内存报告，再决定是否设置性能预算。

### M4 — SSR 与 CSS source map（路线图）

- manifest 能从 entry/chunk 得到完整、有序、去重的 CSS asset 集；
- Node SSR 在无 DOM 环境执行成功，首屏 HTML 每个 CSS asset 只链接一次；
- 服务端/客户端 `createVar` 名称一致，hydration 无 style 属性差异；
- dev overlay 与生产 `.css.map` 都能把生成规则定位回模板文件和行列；
- code splitting、动态 import、base/public path 与 CSP nonce 策略分别有 fixture。

### M5 — 可选扩展（未承诺）

`styled`、theme、atomic、preprocessor 或其他 bundler adapter 需要各自的 RFC、运行时预算、缓存身份
和迁移方案。它们不会因为其他库已经提供就自动进入 Crab CSS 路线图。
