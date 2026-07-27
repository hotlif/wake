//! 构建期静态求值：把 AST 表达式求成 [`StaticValue`]。
//!
//! Linaria 用真实 JS 虚拟机求值模块，wake 无 JS 运行时，改为对一个**刻意受限的纯数据子集**
//! 做递归求值：字面量 / 模板字符串 / 对象 / 数组 / 标识符引用 / 成员访问 / 数字算术与字符串拼接。
//! 函数调用、条件表达式、比较与位运算一律求值失败（返回 `None`）——宁可报警跳过，也不猜测语义。
//!
//! 该子集足以覆盖 design token 模式：
//! ```js
//! const vars = { 'a.b': '--x' };
//! const token = { a: { b: `var(${vars['a.b']}, 8px)` } };
//! // css`padding: ${token.a.b};`
//! ```

use wake_common::{FxHashMap, Interner};
use wake_ecma_ast::*;

/// 一个可在构建期确定的值。
#[derive(Clone, Debug, PartialEq)]
pub enum StaticValue {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
    Undefined,
    /// 对象：**保序**（用 Vec 而非 HashMap），以便产物确定性。
    Obj(Vec<(String, StaticValue)>),
    Arr(Vec<StaticValue>),
}

impl StaticValue {
    /// **JS 模板字符串拼接**时的文本形式（如求值 `` `var(${vars.x}, 8px)` ``）。
    ///
    /// 与 [`StaticValue::to_css`] 的分工：本方法模拟 JS 的字符串拼接，只接受原始值；
    /// 对象/数组在 JS 里会拼成 `[object Object]` 这类垃圾文本，故一律返回 `None` 让求值失败。
    /// 而 `to_css` 是 **CSS 插值**语义，会把对象展开成声明串。
    ///
    /// 与 `ConstVal::to_source` 的关键区别：字符串**不加引号**——`${color}` 要的是
    /// `red` 而不是 `"red"`。数字按 JS 语义去掉多余小数（`8` 而非 `8.0`）。
    pub fn to_css_text(&self) -> Option<String> {
        match self {
            StaticValue::Str(s) => Some(s.clone()),
            StaticValue::Num(n) => Some(format_number(*n)),
            StaticValue::Bool(b) => Some(b.to_string()),
            // null/undefined/对象/数组插进 CSS 只会产生垃圾文本（JS 会得到 "[object Object]"），
            // 视为求值失败，由调用方报警跳过该声明。
            _ => None,
        }
    }

    /// 按 Linaria `toCSS` 语义把值渲染为 CSS 文本（`@wyw-in-js/processor-utils/utils/toCSS`）。
    ///
    /// - 数组 → 逐项渲染后以换行连接
    /// - 原始值 → 直接字符串化
    /// - 对象 → 展开为声明串：丢弃 falsy 非数字值；值为对象/数组时该键当**选择器**（不转连字符），
    ///   否则 `连字符键: 值[px];`，各项以空格连接
    ///
    /// 返回 `None` 表示不可 CSS 化（`null`/`undefined` 等）——调用方据此报警跳过。
    pub fn to_css(&self) -> Option<String> {
        match self {
            StaticValue::Str(s) => Some(s.clone()),
            StaticValue::Num(n) => Some(format_number(*n)),
            StaticValue::Bool(b) => Some(b.to_string()),
            StaticValue::Arr(items) => {
                let mut parts = Vec::with_capacity(items.len());
                for it in items {
                    parts.push(it.to_css()?);
                }
                Some(parts.join("\n"))
            }
            StaticValue::Obj(entries) => {
                let mut parts: Vec<String> = Vec::new();
                for (k, v) in entries {
                    // Linaria: `.filter(([, value]) => typeof value === 'number' || value)`
                    // —— 数字全留（含 0），其余 falsy（null/undefined/false/""）丢弃。
                    match v {
                        StaticValue::Num(_) => {}
                        StaticValue::Null | StaticValue::Undefined => continue,
                        StaticValue::Bool(false) => continue,
                        StaticValue::Str(s) if s.is_empty() => continue,
                        _ => {}
                    }
                    match v {
                        // 嵌套对象/数组 → 该键是选择器（**不**转连字符），值递归展开
                        StaticValue::Obj(_) | StaticValue::Arr(_) => {
                            parts.push(format!("{k} {{ {} }}", v.to_css()?));
                        }
                        _ => {
                            let val = match v {
                                StaticValue::Num(n) => format_css_number(k, *n),
                                other => other.to_css()?,
                            };
                            parts.push(format!("{}: {};", hyphenate(k), val));
                        }
                    }
                }
                Some(parts.join(" "))
            }
            StaticValue::Null | StaticValue::Undefined => None,
        }
    }

    /// 按属性名取值（对象）或按下标取值（数组，键须为纯数字）。
    pub fn get(&self, key: &str) -> Option<&StaticValue> {
        match self {
            StaticValue::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            StaticValue::Arr(items) => key.parse::<usize>().ok().and_then(|i| items.get(i)),
            _ => None,
        }
    }
}

/// 按 JS `String(n)` 语义格式化数字（整数不带小数点）。
fn format_number(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// 对象属性里的数字值：非 0 且属性不在 [`UNITLESS`] 表中时补 `px`（对齐 Linaria/React 行为）。
fn format_css_number(key: &str, n: f64) -> String {
    let s = format_number(n);
    if n != 0.0 && !is_unitless(key) {
        format!("{s}px")
    } else {
        s
    }
}

/// 驼峰属性名 → 连字符。`--custom` 原样；`ms` 前缀特判为 `-ms-`（对齐 Linaria `hyphenate`）。
fn hyphenate(s: &str) -> String {
    if s.starts_with("--") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    // `msTransform` → `ms-transform` → `-ms-transform`
    if let Some(rest) = out.strip_prefix("ms-") {
        return format!("-ms-{rest}");
    }
    out
}

/// 取值为纯数字时**不**补 `px` 的属性（源自 Linaria `units.ts` 的 `unitless` 表）。
const UNITLESS: &[&str] = &[
    "animationIterationCount",
    "borderImageOutset",
    "borderImageSlice",
    "borderImageWidth",
    "boxFlex",
    "boxFlexGroup",
    "boxOrdinalGroup",
    "columnCount",
    "columns",
    "flex",
    "flexGrow",
    "flexPositive",
    "flexShrink",
    "flexNegative",
    "flexOrder",
    "gridRow",
    "gridRowEnd",
    "gridRowSpan",
    "gridRowStart",
    "gridColumn",
    "gridColumnEnd",
    "gridColumnSpan",
    "gridColumnStart",
    "fontWeight",
    "lineClamp",
    "lineHeight",
    "opacity",
    "order",
    "orphans",
    "tabSize",
    "widows",
    "zIndex",
    "zoom",
    // SVG 相关
    "fillOpacity",
    "floodOpacity",
    "stopOpacity",
    "strokeDasharray",
    "strokeDashoffset",
    "strokeMiterlimit",
    "strokeOpacity",
    "strokeWidth",
];

/// 该属性是否无单位。先剥厂商前缀（`WebkitBoxFlex` → `boxFlex`）再查表；
/// 同时接受连字符写法（`z-index` 等价于 `zIndex`）。
fn is_unitless(key: &str) -> bool {
    let stripped = strip_vendor_prefix(key);
    UNITLESS
        .iter()
        .any(|u| *u == stripped || hyphenate(u) == stripped)
}

/// 剥掉 `Webkit`/`Moz`/`O`/`ms` 厂商前缀并把其后首字母小写：
/// `WebkitBoxFlex` → `boxFlex`（对齐 Linaria 的 `/^(Webkit|Moz|O|ms)([A-Z])(.+)$/` 替换）。
fn strip_vendor_prefix(key: &str) -> String {
    for p in ["Webkit", "Moz", "ms", "O"] {
        if let Some(rest) = key.strip_prefix(p)
            && let Some(first) = rest.chars().next()
            && first.is_ascii_uppercase()
        {
            return format!(
                "{}{}",
                first.to_ascii_lowercase(),
                &rest[first.len_utf8()..]
            );
        }
    }
    key.to_string()
}

/// 求值作用域：名字 → 值。模块顶层 `const` 逐条累积填入。
pub type Scope = FxHashMap<String, StaticValue>;

/// 求值上下文。
pub struct EvalCtx<'a> {
    pub interner: &'a Interner,
    /// 本模块已求值的顶层绑定。
    pub scope: &'a Scope,
    /// 本模块 import 进来的绑定：本地名 → 值（由跨模块静态导出解析而来）。
    pub imported: &'a Scope,
}

impl EvalCtx<'_> {
    fn lookup(&self, name: &str) -> Option<&StaticValue> {
        // 本地优先（同名 import 被本地遮蔽的情形极少，但本地更近）。
        self.scope.get(name).or_else(|| self.imported.get(name))
    }
}

/// 递归求值一个表达式；无法在构建期确定时返回 `None`。
pub fn eval(expr: &Expression, ctx: &EvalCtx) -> Option<StaticValue> {
    match expr {
        Expression::StringLiteral(s) => Some(StaticValue::Str(ctx.interner.resolve(s.value))),
        Expression::NumberLiteral(n) => Some(StaticValue::Num(n.value)),
        Expression::BooleanLiteral(b) => Some(StaticValue::Bool(b.value)),
        Expression::NullLiteral(_) => Some(StaticValue::Null),

        Expression::Identifier(id) => {
            let name = ctx.interner.resolve(id.name);
            if name == "undefined" {
                return Some(StaticValue::Undefined);
            }
            ctx.lookup(&name).cloned()
        }

        // `-8` 在 AST 里是一元负号包数字字面量。
        Expression::Unary(u) => {
            let v = eval(&u.argument, ctx)?;
            match (u.operator, v) {
                (UnaryOperator::Minus, StaticValue::Num(n)) => Some(StaticValue::Num(-n)),
                (UnaryOperator::Plus, StaticValue::Num(n)) => Some(StaticValue::Num(n)),
                _ => None,
            }
        }

        // 数字算术与字符串拼接：design token 常写 `${UNIT * 2}px`、`${PREFIX + name}`。
        // 只支持**纯算术/拼接**，不涉及比较、位运算与短路——保持「不猜语义」的边界。
        Expression::Binary(b) => {
            let l = eval(&b.left, ctx)?;
            let r = eval(&b.right, ctx)?;
            match (b.operator, &l, &r) {
                (BinaryOperator::Add, StaticValue::Num(x), StaticValue::Num(y)) => {
                    Some(StaticValue::Num(x + y))
                }
                (BinaryOperator::Sub, StaticValue::Num(x), StaticValue::Num(y)) => {
                    Some(StaticValue::Num(x - y))
                }
                (BinaryOperator::Mul, StaticValue::Num(x), StaticValue::Num(y)) => {
                    Some(StaticValue::Num(x * y))
                }
                (BinaryOperator::Div, StaticValue::Num(x), StaticValue::Num(y)) => {
                    Some(StaticValue::Num(x / y))
                }
                (BinaryOperator::Rem, StaticValue::Num(x), StaticValue::Num(y)) => {
                    Some(StaticValue::Num(x % y))
                }
                (BinaryOperator::Exp, StaticValue::Num(x), StaticValue::Num(y)) => {
                    Some(StaticValue::Num(x.powf(*y)))
                }
                // JS `+`：任一侧为字符串即字符串拼接。
                (BinaryOperator::Add, StaticValue::Str(_), _)
                | (BinaryOperator::Add, _, StaticValue::Str(_)) => Some(StaticValue::Str(format!(
                    "{}{}",
                    l.to_css_text()?,
                    r.to_css_text()?
                ))),
                _ => None,
            }
        }

        Expression::TemplateLiteral(t) => eval_template(t, ctx).map(StaticValue::Str),

        Expression::Object(o) => {
            let mut entries: Vec<(String, StaticValue)> = Vec::new();
            for member in o.properties.iter() {
                match member {
                    ObjectMember::Property(p) => {
                        // 方法、getter/setter、计算键一律不支持（非纯数据）。
                        if p.method || p.kind != PropertyKind::Init || p.computed {
                            return None;
                        }
                        let key = match &p.key {
                            PropertyKey::Ident(id) => ctx.interner.resolve(id.name),
                            PropertyKey::String(s) => ctx.interner.resolve(s.value),
                            PropertyKey::Number(n) => format_number(n.value),
                            _ => return None,
                        };
                        let val = eval(&p.value, ctx)?;
                        // 后写覆盖前写（JS 对象字面量语义）。
                        match entries.iter_mut().find(|(k, _)| *k == key) {
                            Some(slot) => slot.1 = val,
                            None => entries.push((key, val)),
                        }
                    }
                    // `{ ...base, x: 1 }`：base 可求值为对象时展开，否则失败。
                    ObjectMember::Spread(s) => match eval(&s.argument, ctx)? {
                        StaticValue::Obj(inner) => {
                            for (k, v) in inner {
                                match entries.iter_mut().find(|(ek, _)| *ek == k) {
                                    Some(slot) => slot.1 = v,
                                    None => entries.push((k, v)),
                                }
                            }
                        }
                        _ => return None,
                    },
                }
            }
            Some(StaticValue::Obj(entries))
        }

        Expression::Array(a) => {
            let mut items = Vec::with_capacity(a.elements.len());
            for el in a.elements.iter() {
                match el {
                    // spread 以 `Expression::Spread` 出现在元素位，非纯数据 → 失败。
                    Some(Expression::Spread(_)) => return None,
                    Some(e) => items.push(eval(e, ctx)?),
                    // 稀疏洞 `[1,,3]` → undefined
                    None => items.push(StaticValue::Undefined),
                }
            }
            Some(StaticValue::Arr(items))
        }

        // `token.a.b` / `vars['a.b']` —— design token 的主力形态。
        Expression::Member(m) => {
            let obj = eval(&m.object, ctx)?;
            let key = match &m.property {
                MemberProperty::Ident(id) => ctx.interner.resolve(id.name),
                MemberProperty::Computed(e) => match eval(e, ctx)? {
                    StaticValue::Str(s) => s,
                    StaticValue::Num(n) => format_number(n),
                    _ => return None,
                },
                MemberProperty::Private(_) => return None,
            };
            obj.get(&key).cloned()
        }

        _ => None,
    }
}

/// 求值模板字符串（无标签），拼接为一个字符串。
///
/// 与 `wake_ecma_minify::const_eval` 的模板求值不同：这里插值用 [`StaticValue::to_css_text`]，
/// 字符串**不带引号**，符合 CSS 拼接语义。
pub fn eval_template(t: &TemplateLiteral, ctx: &EvalCtx) -> Option<String> {
    let mut out = String::new();
    for (i, q) in t.quasis.iter().enumerate() {
        out.push_str(&cooked_text(q, ctx.interner)?);
        if let Some(e) = t.expressions.get(i) {
            out.push_str(&eval(e, ctx)?.to_css_text()?);
        }
    }
    Some(out)
}

/// 取模板片段的 cooked 文本（非法转义时为 `None`）。
pub fn cooked_text(q: &TemplateElement, interner: &Interner) -> Option<String> {
    q.cooked.map(|a| interner.resolve(a))
}

/// 收集模块顶层 `const`/`let`/`var` 的可静态求值绑定（按声明序逐条累积，后者可引用前者）。
///
/// 只走**顶层**声明：函数体/块内的同名变量不参与，避免作用域误命中。
pub fn collect_module_scope(program: &Program, interner: &Interner, imported: &Scope) -> Scope {
    let mut scope = Scope::default();
    for stmt in program.body.iter() {
        // `export const x = ...` 的声明藏在 ExportNamed 里。
        let decl = match stmt {
            Statement::VariableDeclaration(d) => Some(*d),
            Statement::ExportNamed(e) => e.declaration.and_then(|s| match s {
                Statement::VariableDeclaration(d) => Some(d),
                _ => None,
            }),
            _ => None,
        };
        let Some(decl) = decl else { continue };
        for d in decl.declarations.iter() {
            let (Pattern::Ident(id), Some(init)) = (&d.id, &d.init) else {
                continue;
            };
            let ctx = EvalCtx {
                interner,
                scope: &scope,
                imported,
            };
            if let Some(v) = eval(init, &ctx) {
                scope.insert(interner.resolve(id.name), v);
            }
        }
    }
    scope
}

/// 模块的静态导出：导出名 → 值（`"default"` 表示默认导出）。
pub type StaticExports = FxHashMap<String, StaticValue>;

/// 收集一个模块可被其它模块静态引用的导出常量。
///
/// 覆盖 `export const x = ...`、`export default <expr>`、`export { a, b }`、`export { a as b }`。
pub fn collect_static_exports(program: &Program, interner: &Interner) -> StaticExports {
    collect_static_exports_with(program, interner, &Scope::default())
}

/// 同上，但可传入本模块 **import 绑定已求值的静态值**，使导出常量能引用其它模块的常量。
///
/// 这是「跨模块多层常量传播」的基础：调用方按依赖拓扑序推进，先算被依赖模块的导出，
/// 再把它们装配成本模块的 `imported` 传进来，如此可传播任意层数。
pub fn collect_static_exports_with(
    program: &Program,
    interner: &Interner,
    imported: &Scope,
) -> StaticExports {
    let empty = imported;
    let scope = collect_module_scope(program, interner, imported);
    let mut out = StaticExports::default();

    for stmt in program.body.iter() {
        match stmt {
            Statement::ExportNamed(e) => {
                // `export const x = 1;`
                if let Some(Statement::VariableDeclaration(d)) = e.declaration {
                    for decl in d.declarations.iter() {
                        if let Pattern::Ident(id) = &decl.id {
                            let name = interner.resolve(id.name);
                            if let Some(v) = scope.get(&name) {
                                out.insert(name, v.clone());
                            }
                        }
                    }
                }
                // `export { a, b as c };`（无 from：本地绑定再导出）
                if e.source.is_none() {
                    for spec in e.specifiers.iter() {
                        let local = export_name(&spec.local, interner);
                        let exported = export_name(&spec.exported, interner);
                        if let Some(v) = scope.get(&local) {
                            out.insert(exported, v.clone());
                        }
                    }
                }
            }
            Statement::ExportDefault(d) => {
                if let ExportDefaultKind::Expression(e) = &d.declaration {
                    let ctx = EvalCtx {
                        interner,
                        scope: &scope,
                        imported: &empty,
                    };
                    if let Some(v) = eval(e, &ctx) {
                        out.insert("default".to_string(), v);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn export_name(n: &ModuleExportName, interner: &Interner) -> String {
    match n {
        ModuleExportName::Ident(id) => interner.resolve(id.name),
        ModuleExportName::String(a) => interner.resolve(*a),
    }
}

/// 收集本模块的 import 绑定：本地名 → (说明符, 导入的导出名)。
///
/// `import token from './t.js'` → `("token", ("./t.js", "default"))`
/// `import { a as b } from './t.js'` → `("b", ("./t.js", "a"))`
/// `import * as ns from './t.js'` → `("ns", ("./t.js", "*"))`
pub fn collect_imports(program: &Program, interner: &Interner) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for stmt in program.body.iter() {
        let Statement::Import(imp) = stmt else {
            continue;
        };
        // 类型-only import 在 parse 期已擦除，这里见到的都是运行时 import。
        let spec_text = interner.resolve(imp.source);
        for s in imp.specifiers.iter() {
            let (local, imported) = match s {
                ImportSpecifier::Default { local, .. } => (*local, "default".to_string()),
                ImportSpecifier::Namespace { local, .. } => (*local, "*".to_string()),
                ImportSpecifier::Named {
                    local, imported, ..
                } => (*local, export_name(imported, interner)),
            };
            out.push((interner.resolve(local.name), spec_text.clone(), imported));
        }
    }
    out
}
