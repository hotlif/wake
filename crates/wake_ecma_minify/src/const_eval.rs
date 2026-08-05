use wake_common::{Atom, Interner};
use wake_ecma_ast::*;

#[derive(Clone, Debug, PartialEq)]
pub enum ConstVal {
    Bool(bool),
    Str(String),
    Num(f64),
    Null,
    Undefined,
}

impl ConstVal {
    pub fn truthy(&self) -> bool {
        match self {
            ConstVal::Bool(b) => *b,
            ConstVal::Str(s) => !s.is_empty(),
            ConstVal::Num(n) => *n != 0.0 && !n.is_nan(),
            ConstVal::Null | ConstVal::Undefined => false,
        }
    }

    pub fn to_source(&self) -> String {
        match self {
            ConstVal::Bool(b) => {
                if *b {
                    "true".into()
                } else {
                    "false".into()
                }
            }
            ConstVal::Str(s) => format!("{:?}", s),
            ConstVal::Num(n) => write_number_minified(*n),
            ConstVal::Null => "null".into(),
            ConstVal::Undefined => "undefined".into(),
        }
    }
}

pub fn write_number_minified(n: f64) -> String {
    write_number_impl(n, true)
}

fn write_number_impl(n: f64, minified: bool) -> String {
    if n == 0.0 && n.is_sign_negative() {
        return "-0".into();
    }
    if n.is_infinite() && n > 0.0 {
        return "Infinity".into();
    }
    if n.is_infinite() {
        return "-Infinity".into();
    }
    if n.is_nan() {
        return "NaN".into();
    }

    if n.fract() == 0.0 && n.is_finite() && n.abs() <= 2_f64.powi(53) {
        let int_val = n as i64;
        if minified && int_val.unsigned_abs() >= 100 {
            let dec = format!("{}", int_val.unsigned_abs());
            if let Some(shorter) = try_exponential(&dec) {
                let sign = if n < 0.0 { "-" } else { "" };
                return format!("{}{}", sign, shorter);
            }
        }
        format!("{:.0}", n)
    } else {
        let s = format!("{}", n);
        if minified {
            if let Some(stripped) = s.strip_prefix("0.") {
                format!(".{stripped}")
            } else if let Some(stripped) = s.strip_prefix("-0.") {
                format!("-.{stripped}")
            } else {
                s
            }
        } else {
            s
        }
    }
}

/// Try exponential form: `1000` → `1e3`, `50000000` → `5e7`, etc.
fn try_exponential(dec: &str) -> Option<String> {
    let trailing_zeros = dec.len() - dec.trim_end_matches('0').len();
    if trailing_zeros < 2 {
        return None;
    }
    let sig_end = dec.len() - trailing_zeros;
    let exp = trailing_zeros;
    let sig = &dec[..sig_end];
    let exp_str = if sig.len() == 1 {
        format!("{}e{}", sig, exp)
    } else {
        format!("{}.{}e{}", &sig[..1], &sig[1..], exp)
    };
    if exp_str.len() < dec.len() {
        Some(exp_str)
    } else {
        None
    }
}

#[derive(Default)]
pub struct ConstCtx<'a> {
    pub defines: &'a [(&'a str, &'a str)],
    pub known_vars: &'a [(&'a Atom, ConstVal)],
    pub interner: Option<&'a Interner>,
}

// ── Expression purity check ──

pub fn expr_is_pure(e: &Expression) -> bool {
    use Expression::*;
    match e {
        NumberLiteral(_) | StringLiteral(_) | BooleanLiteral(_) | NullLiteral(_)
        | BigIntLiteral(_) | RegExpLiteral(_) | Identifier(_) | This(_) | Super(_)
        | MetaProperty(_) | Function(_) | Arrow(_) => true,
        Class(c) => class_is_pure(c),
        TemplateLiteral(t) => t.expressions.iter().all(expr_is_pure),
        Array(a) => a.elements.iter().flatten().all(expr_is_pure),
        Object(o) => o.properties.iter().all(|m| match m {
            ObjectMember::Property(p) => key_is_pure(&p.key) && expr_is_pure(&p.value),
            ObjectMember::Spread(s) => expr_is_pure(&s.argument),
        }),
        Unary(u) => expr_is_pure(&u.argument),
        Update(_) => false,
        Binary(b) => expr_is_pure(&b.left) && expr_is_pure(&b.right),
        Logical(l) => expr_is_pure(&l.left) && expr_is_pure(&l.right),
        Assignment(_) => false,
        Conditional(c) => {
            expr_is_pure(&c.test) && expr_is_pure(&c.consequent) && expr_is_pure(&c.alternate)
        }
        Sequence(s) => s.expressions.iter().all(expr_is_pure),
        Call(c) => expr_is_pure(&c.callee) && c.arguments.iter().all(expr_is_pure),
        New(n) => expr_is_pure(&n.callee) && n.arguments.iter().all(expr_is_pure),
        Member(m) => {
            expr_is_pure(&m.object)
                && match &m.property {
                    MemberProperty::Ident(_) | MemberProperty::Private(_) => true,
                    MemberProperty::Computed(e) => expr_is_pure(e),
                }
        }
        Spread(s) => expr_is_pure(&s.argument),
        Await(_) | Yield(_) | Import(_) | TaggedTemplate(_) => false,
    }
}

fn class_is_pure(c: &Class) -> bool {
    c.body.iter().all(|m| match m {
        ClassMember::Method(d) => key_is_pure(&d.key),
        ClassMember::Property(p) => key_is_pure(&p.key),
        ClassMember::StaticBlock(_) => true,
    })
}

fn key_is_pure(key: &PropertyKey) -> bool {
    match key {
        PropertyKey::Computed(e) => expr_is_pure(e),
        _ => true,
    }
}

// ── Constant evaluation ──

pub fn const_eval<'a>(e: &Expression<'a>, ctx: &ConstCtx) -> Option<ConstVal> {
    match e {
        Expression::NumberLiteral(n) => Some(ConstVal::Num(n.value)),
        Expression::StringLiteral(s) => {
            let resolved = ctx.interner?.resolve(s.value);
            Some(ConstVal::Str(resolved))
        }
        Expression::BooleanLiteral(b) => Some(ConstVal::Bool(b.value)),
        Expression::NullLiteral(_) => Some(ConstVal::Null),
        Expression::Identifier(id) => {
            let it = ctx.interner?;
            let name = it.resolve(id.name);
            for (k, v) in ctx.known_vars {
                if it.resolve(**k) == name {
                    return Some(v.clone());
                }
            }
            match name.as_str() {
                "undefined" => Some(ConstVal::Undefined),
                "NaN" => Some(ConstVal::Num(f64::NAN)),
                "Infinity" => Some(ConstVal::Num(f64::INFINITY)),
                _ => None,
            }
        }
        Expression::Unary(u) => const_eval_unary(u, ctx),
        Expression::Binary(b) => const_eval_binary(b, ctx),
        Expression::Logical(l) => const_eval_logical(l, ctx),
        Expression::Conditional(c) => const_eval_conditional(c, ctx),
        Expression::TemplateLiteral(t) => const_eval_template(t, ctx),
        Expression::Member(m) => match_define_from_member(m, ctx.defines, ctx.interner),
        _ => None,
    }
}

fn const_eval_unary<'a>(u: &UnaryExpression<'a>, ctx: &ConstCtx) -> Option<ConstVal> {
    use UnaryOperator::*;
    match u.operator {
        LogicalNot => const_eval(&u.argument, ctx).map(|v| ConstVal::Bool(!v.truthy())),
        Plus => const_eval(&u.argument, ctx).map(|v| to_number_val(&v)),
        Minus => const_eval(&u.argument, ctx).map(|v| ConstVal::Num(-to_number(&v))),
        Void => Some(ConstVal::Undefined),
        Typeof => const_eval_typeof(&u.argument, ctx),
        BitwiseNot => const_eval(&u.argument, ctx).map(|v| {
            let n = to_int32(&v);
            ConstVal::Num((!n) as f64)
        }),
        Delete => None,
    }
}

fn to_number_val(v: &ConstVal) -> ConstVal {
    ConstVal::Num(to_number(v))
}

fn const_eval_typeof<'a>(e: &Expression<'a>, ctx: &ConstCtx) -> Option<ConstVal> {
    match e {
        Expression::Identifier(id) => {
            let it = ctx.interner?;
            let name = it.resolve(id.name);
            let typ = match name.as_str() {
                "undefined" => "undefined",
                "NaN" | "Infinity" => "number",
                "Math" | "JSON" | "console" | "window" | "global" | "globalThis" => "object",
                "Symbol" => "function",
                _ => return None,
            };
            Some(ConstVal::Str(typ.into()))
        }
        _ => Some(ConstVal::Str(
            match e {
                Expression::StringLiteral(_) => "string",
                Expression::NumberLiteral(_) => "number",
                Expression::BooleanLiteral(_) => "boolean",
                Expression::NullLiteral(_) => "object",
                Expression::BigIntLiteral(_) => "bigint",
                Expression::Function(_) | Expression::Arrow(_) => "function",
                Expression::Array(_) | Expression::Object(_) | Expression::Class(_) => "object",
                Expression::RegExpLiteral(_) => "object",
                _ => return None,
            }
            .into(),
        )),
    }
}

fn const_eval_binary<'a>(bin: &BinaryExpression<'a>, ctx: &ConstCtx) -> Option<ConstVal> {
    use BinaryOperator::*;
    match bin.operator {
        StrictEq | StrictNotEq => {
            let l = const_eval(&bin.left, ctx)?;
            let r = const_eval(&bin.right, ctx)?;
            let eq = strict_equals(&l, &r);
            Some(ConstVal::Bool(eq == (bin.operator == StrictEq)))
        }
        Eq | NotEq => {
            let l = const_eval(&bin.left, ctx)?;
            let r = const_eval(&bin.right, ctx)?;
            if same_type(&l, &r) {
                let eq = strict_equals(&l, &r);
                Some(ConstVal::Bool(eq == (bin.operator == Eq)))
            } else if matches!(&l, ConstVal::Null) && matches!(&r, ConstVal::Undefined)
                || matches!(&l, ConstVal::Undefined) && matches!(&r, ConstVal::Null)
            {
                Some(ConstVal::Bool(bin.operator == Eq))
            } else {
                None
            }
        }
        Lt | Gt | LtEq | GtEq => {
            let lv = const_eval(&bin.left, ctx)?;
            let rv = const_eval(&bin.right, ctx)?;
            match (&lv, &rv) {
                (ConstVal::Num(a), ConstVal::Num(bb)) => {
                    let result = match bin.operator {
                        Lt => a < bb,
                        Gt => a > bb,
                        LtEq => a <= bb,
                        GtEq => a >= bb,
                        _ => unreachable!(),
                    };
                    Some(ConstVal::Bool(result))
                }
                (ConstVal::Str(a), ConstVal::Str(bb)) => {
                    let result = match bin.operator {
                        Lt => a < bb,
                        Gt => a > bb,
                        LtEq => a <= bb,
                        GtEq => a >= bb,
                        _ => unreachable!(),
                    };
                    Some(ConstVal::Bool(result))
                }
                _ => None,
            }
        }
        Add => {
            let l = const_eval(&bin.left, ctx)?;
            let r = const_eval(&bin.right, ctx)?;
            let lstr = match &l {
                ConstVal::Str(s) => Some(s.clone()),
                _ => None,
            };
            let rstr = match &r {
                ConstVal::Str(s) => Some(s.clone()),
                _ => None,
            };
            match (lstr, rstr) {
                (Some(a), _) => Some(ConstVal::Str(format!("{}{}", a, string_val(&r)))),
                (_, Some(b)) => Some(ConstVal::Str(format!("{}{}", string_val(&l), b))),
                _ if matches!((&l, &r), (ConstVal::Num(_), ConstVal::Num(_))) => {
                    if let ConstVal::Num(a) = l
                        && let ConstVal::Num(b) = r
                    {
                        return Some(ConstVal::Num(a + b));
                    }
                    unreachable!()
                }
                _ => None,
            }
        }
        Sub | Mul | Div | Rem | Exp => {
            let lv = const_eval(&bin.left, ctx).map(|v| to_number(&v))?;
            let rv = const_eval(&bin.right, ctx).map(|v| to_number(&v))?;
            let result = match bin.operator {
                Sub => lv - rv,
                Mul => lv * rv,
                Div => lv / rv,
                Rem => lv % rv,
                Exp => lv.powf(rv),
                _ => unreachable!(),
            };
            Some(ConstVal::Num(result))
        }
        BitAnd | BitOr | BitXor => {
            let l = const_eval(&bin.left, ctx).map(|v| to_int32(&v))?;
            let r = const_eval(&bin.right, ctx).map(|v| to_int32(&v))?;
            let result = match bin.operator {
                BitAnd => l & r,
                BitOr => l | r,
                BitXor => l ^ r,
                _ => unreachable!(),
            };
            Some(ConstVal::Num(result as f64))
        }
        Shl | Shr | Ushr => {
            let l = const_eval(&bin.left, ctx).map(|v| to_int32(&v))?;
            let r = const_eval(&bin.right, ctx).map(|v| to_uint32(&v) & 0x1F)?;
            let result = match bin.operator {
                Shl => (l as u32).wrapping_shl(r) as i32,
                Shr => l.wrapping_shr(r),
                Ushr => (l as u32).wrapping_shr(r) as i32,
                _ => unreachable!(),
            };
            Some(ConstVal::Num(result as f64))
        }
        In | Instanceof => None,
    }
}

fn const_eval_logical<'a>(l: &LogicalExpression<'a>, ctx: &ConstCtx) -> Option<ConstVal> {
    use LogicalOperator::*;
    let left = const_eval(&l.left, ctx)?;
    match l.operator {
        And => {
            if left.truthy() {
                const_eval(&l.right, ctx)
            } else {
                Some(left)
            }
        }
        Or => {
            if left.truthy() {
                Some(left)
            } else {
                const_eval(&l.right, ctx)
            }
        }
        Coalesce => {
            if matches!(left, ConstVal::Null | ConstVal::Undefined) {
                const_eval(&l.right, ctx)
            } else {
                Some(left)
            }
        }
    }
}

fn const_eval_conditional<'a>(c: &ConditionalExpression<'a>, ctx: &ConstCtx) -> Option<ConstVal> {
    let test = const_eval(&c.test, ctx)?;
    if test.truthy() {
        const_eval(&c.consequent, ctx)
    } else {
        const_eval(&c.alternate, ctx)
    }
}

fn const_eval_template<'a>(t: &TemplateLiteral<'a>, ctx: &ConstCtx) -> Option<ConstVal> {
    let it = ctx.interner?;
    let mut result = String::new();
    for (i, q) in t.quasis.iter().enumerate() {
        result.push_str(&it.resolve(q.raw));
        if i < t.expressions.len() {
            let val = const_eval(&t.expressions[i], ctx)?;
            result.push_str(&val.to_source());
        }
    }
    Some(ConstVal::Str(result))
}

pub fn const_eval_bool<'a>(e: &Expression<'a>, ctx: &ConstCtx) -> Option<bool> {
    match e {
        Expression::Logical(l) => match l.operator {
            LogicalOperator::And => {
                if const_eval_bool(&l.left, ctx)? {
                    const_eval_bool(&l.right, ctx)
                } else {
                    Some(false)
                }
            }
            LogicalOperator::Or => {
                if const_eval_bool(&l.left, ctx)? {
                    Some(true)
                } else {
                    const_eval_bool(&l.right, ctx)
                }
            }
            LogicalOperator::Coalesce => None,
        },
        _ => const_eval(e, ctx).map(|v| v.truthy()),
    }
}

// ── Define matching ──

pub fn match_define_from_member<'a>(
    m: &MemberExpression<'a>,
    defines: &[(&str, &str)],
    interner: Option<&Interner>,
) -> Option<ConstVal> {
    let chain = build_static_chain_from_expr(&Expression::Member(m), interner)?;
    let (_, val) = defines.iter().find(|(k, _)| *k == chain)?;
    parse_define_literal(val)
}

fn build_static_chain_from_expr(e: &Expression, interner: Option<&Interner>) -> Option<String> {
    match e {
        Expression::Identifier(id) => {
            let name = interner?.resolve(id.name);
            Some(name)
        }
        Expression::MetaProperty(m) => {
            let meta = interner?.resolve(m.meta);
            let property = interner?.resolve(m.property);
            Some(format!("{}.{}", meta, property))
        }
        Expression::Member(m) if !m.optional => match &m.property {
            MemberProperty::Ident(p) => {
                let base = build_static_chain_from_expr(&m.object, interner)?;
                let name = interner?.resolve(p.name);
                Some(format!("{}.{}", base, name))
            }
            _ => None,
        },
        _ => None,
    }
}

fn parse_define_literal(lit: &str) -> Option<ConstVal> {
    let t = lit.trim();
    if t.len() >= 2 {
        let b = t.as_bytes();
        if (b[0] == b'"' && b[t.len() - 1] == b'"') || (b[0] == b'\'' && b[t.len() - 1] == b'\'') {
            return Some(ConstVal::Str(t[1..t.len() - 1].to_string()));
        }
    }
    match t {
        "true" => Some(ConstVal::Bool(true)),
        "false" => Some(ConstVal::Bool(false)),
        "null" => Some(ConstVal::Null),
        "undefined" => Some(ConstVal::Undefined),
        _ => t.parse::<f64>().ok().map(ConstVal::Num),
    }
}

// ── Helpers ──

fn strict_equals(l: &ConstVal, r: &ConstVal) -> bool {
    match (l, r) {
        (ConstVal::Bool(a), ConstVal::Bool(b)) => a == b,
        (ConstVal::Str(a), ConstVal::Str(b)) => a == b,
        (ConstVal::Num(a), ConstVal::Num(b)) => a == b,
        (ConstVal::Null, ConstVal::Null) => true,
        (ConstVal::Undefined, ConstVal::Undefined) => true,
        _ => false,
    }
}

fn same_type(l: &ConstVal, r: &ConstVal) -> bool {
    std::mem::discriminant(l) == std::mem::discriminant(r)
}

fn to_number(v: &ConstVal) -> f64 {
    match v {
        ConstVal::Num(n) => *n,
        ConstVal::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        ConstVal::Null => 0.0,
        ConstVal::Undefined => f64::NAN,
        ConstVal::Str(s) => s.parse::<f64>().unwrap_or(f64::NAN),
    }
}

fn to_int32(v: &ConstVal) -> i32 {
    let n = to_number(v);
    if n.is_nan() || n.is_infinite() {
        0
    } else {
        (n as i64).wrapping_rem(0x1_0000_0000) as i32
    }
}

fn to_uint32(v: &ConstVal) -> u32 {
    to_int32(v) as u32
}

/// Get the string value of a ConstVal (without quotes for strings).
fn string_val(v: &ConstVal) -> String {
    match v {
        ConstVal::Str(s) => s.clone(),
        _ => v.to_source(),
    }
}

pub fn has_hoisted_decl(stmt: &Statement) -> bool {
    match stmt {
        Statement::VariableDeclaration(d) => d.kind == VarKind::Var,
        Statement::FunctionDeclaration(_) => true,
        Statement::Block(b) => b.body.iter().any(has_hoisted_decl),
        Statement::If(s) => {
            has_hoisted_decl(&s.consequent) || s.alternate.as_ref().is_some_and(has_hoisted_decl)
        }
        Statement::Expression(_)
        | Statement::Empty(_)
        | Statement::Return(_)
        | Statement::Break(_)
        | Statement::Continue(_)
        | Statement::Throw(_)
        | Statement::Debugger(_)
        | Statement::ClassDeclaration(_) => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::Interner;
    use wake_ecma_ast::SourceType;
    use wake_ecma_parser::parse;

    fn eval_helper(src: &str, defines: &[(&str, &str)]) -> Option<ConstVal> {
        let it = Interner::new();
        let out = parse(src, &it, SourceType::Script);
        assert!(!out.has_errors(), "parse error: {:?}", out.diagnostics);
        out.module.with_ast(|p| {
            let stmt = p.body.first().expect("no statements");
            let expr = match stmt {
                Statement::Expression(es) => &es.expression,
                _ => panic!("expected ExpressionStatement"),
            };
            let ctx = ConstCtx {
                defines,
                known_vars: &[],
                interner: Some(&it),
            };
            const_eval(expr, &ctx)
        })
    }

    fn eval(src: &str) -> Option<ConstVal> {
        eval_helper(src, &[])
    }

    fn assert_eval(src: &str, expected: ConstVal) {
        let val = eval(src);
        if let Some(ConstVal::Num(a)) = &val
            && let ConstVal::Num(b) = &expected
        {
            assert!(
                a.is_nan() && b.is_nan() || (a == b),
                "eval `{}`: got {:?}, expected {:?}",
                src,
                val,
                expected
            );
            return;
        }
        assert_eq!(val, Some(expected), "eval `{}`", src);
    }

    fn assert_not_eval(src: &str) {
        let val = eval(src);
        assert!(
            val.is_none(),
            "expected not evaluable: `{}` = {:?}",
            src,
            val
        );
    }

    #[test]
    fn eval_number() {
        assert_eval("42;", ConstVal::Num(42.0));
    }
    #[test]
    fn eval_neg_number() {
        assert_eval("-3.125;", ConstVal::Num(-3.125));
    }
    #[test]
    fn eval_string_double() {
        assert_eval("\"hello\";", ConstVal::Str("hello".into()));
    }
    #[test]
    fn eval_string_single() {
        assert_eval("'world';", ConstVal::Str("world".into()));
    }
    #[test]
    fn eval_true() {
        assert_eval("true;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_false() {
        assert_eval("false;", ConstVal::Bool(false));
    }
    #[test]
    fn eval_null() {
        assert_eval("null;", ConstVal::Null);
    }
    #[test]
    fn eval_undefined() {
        assert_eval("undefined;", ConstVal::Undefined);
    }
    #[test]
    fn eval_nan() {
        assert_eval("NaN;", ConstVal::Num(f64::NAN));
    }
    #[test]
    fn eval_infinity() {
        assert_eval("Infinity;", ConstVal::Num(f64::INFINITY));
    }

    #[test]
    fn eval_not_true() {
        assert_eval("!true;", ConstVal::Bool(false));
    }
    #[test]
    fn eval_not_false() {
        assert_eval("!false;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_not_zero() {
        assert_eval("!0;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_not_one() {
        assert_eval("!1;", ConstVal::Bool(false));
    }
    #[test]
    fn eval_not_empty_str() {
        assert_eval("!\"\";", ConstVal::Bool(true));
    }
    #[test]
    fn eval_not_null() {
        assert_eval("!null;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_not_undefined() {
        assert_eval("!undefined;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_double_not() {
        assert_eval("!!42;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_double_not_false() {
        assert_eval("!!0;", ConstVal::Bool(false));
    }
    #[test]
    fn eval_unary_plus() {
        assert_eval("+42;", ConstVal::Num(42.0));
    }
    #[test]
    fn eval_unary_plus_true() {
        assert_eval("+true;", ConstVal::Num(1.0));
    }
    #[test]
    fn eval_unary_minus() {
        assert_eval("-42;", ConstVal::Num(-42.0));
    }
    #[test]
    fn eval_unary_minus_neg() {
        assert_eval("-(-5);", ConstVal::Num(5.0));
    }
    #[test]
    fn eval_void() {
        assert_eval("void 0;", ConstVal::Undefined);
    }
    #[test]
    fn eval_typeof_num() {
        assert_eval("typeof 42;", ConstVal::Str("number".into()));
    }
    #[test]
    fn eval_typeof_str() {
        assert_eval("typeof \"x\";", ConstVal::Str("string".into()));
    }
    #[test]
    fn eval_typeof_bool() {
        assert_eval("typeof true;", ConstVal::Str("boolean".into()));
    }
    #[test]
    fn eval_typeof_null() {
        assert_eval("typeof null;", ConstVal::Str("object".into()));
    }
    #[test]
    fn eval_typeof_func() {
        assert_eval("typeof function(){};", ConstVal::Str("function".into()));
    }

    #[test]
    fn eval_add() {
        assert_eval("2 + 3;", ConstVal::Num(5.0));
    }
    #[test]
    fn eval_str_concat() {
        assert_eval("\"a\" + \"b\";", ConstVal::Str("ab".into()));
    }
    #[test]
    fn eval_sub() {
        assert_eval("10 - 3;", ConstVal::Num(7.0));
    }
    #[test]
    fn eval_mul() {
        assert_eval("7 * 8;", ConstVal::Num(56.0));
    }
    #[test]
    fn eval_div() {
        assert_eval("10 / 2;", ConstVal::Num(5.0));
    }

    #[test]
    fn eval_strict_eq() {
        assert_eval("1 === 1;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_strict_neq() {
        assert_eval("1 !== 2;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_strict_cross_type() {
        assert_eval("\"1\" === 1;", ConstVal::Bool(false));
    }
    #[test]
    fn eval_lt() {
        assert_eval("1 < 2;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_gt() {
        assert_eval("2 > 1;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_lte() {
        assert_eval("1 <= 1;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_str_lt() {
        assert_eval("\"a\" < \"b\";", ConstVal::Bool(true));
    }

    #[test]
    fn eval_and() {
        assert_eval("true && true;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_or() {
        assert_eval("false || true;", ConstVal::Bool(true));
    }
    #[test]
    fn eval_coalesce() {
        assert_eval("null ?? 42;", ConstVal::Num(42.0));
    }

    #[test]
    fn eval_ternary() {
        assert_eval("true ? 1 : 2;", ConstVal::Num(1.0));
    }
    #[test]
    fn eval_ternary_false() {
        assert_eval("false ? 1 : 2;", ConstVal::Num(2.0));
    }

    #[test]
    fn eval_template() {
        assert_eval("`hello`;", ConstVal::Str("hello".into()));
    }
    #[test]
    fn eval_template_expr() {
        assert_eval("`${42}`;", ConstVal::Str("42".into()));
    }

    #[test]
    fn eval_bitwise_and() {
        assert_eval("5 & 3;", ConstVal::Num(1.0));
    }
    #[test]
    fn eval_bitwise_or() {
        assert_eval("5 | 3;", ConstVal::Num(7.0));
    }
    #[test]
    fn eval_bitwise_not() {
        assert_eval("~5;", ConstVal::Num(-6.0));
    }

    #[test]
    fn not_eval_var() {
        assert_not_eval("x;");
    }
    #[test]
    fn not_eval_call() {
        assert_not_eval("foo();");
    }
    #[test]
    fn not_eval_assign() {
        assert_not_eval("x = 42;");
    }

    #[test]
    fn eval_define() {
        let val = eval_helper(
            "process.env.NODE_ENV;",
            &[("process.env.NODE_ENV", "\"production\"")],
        );
        assert_eq!(val, Some(ConstVal::Str("production".into())));
    }

    #[test]
    fn source_format() {
        assert_eq!(ConstVal::Bool(true).to_source(), "true");
        assert_eq!(ConstVal::Null.to_source(), "null");
        assert_eq!(ConstVal::Undefined.to_source(), "undefined");
        assert_eq!(ConstVal::Num(42.0).to_source(), "42");
        assert_eq!(ConstVal::Str("hello".to_string()).to_source(), "\"hello\"");
    }

    #[test]
    fn hoisted_var_detected() {
        let it = Interner::new();
        let out = parse("var x = 1;", &it, SourceType::Script);
        out.module
            .with_ast(|p| assert!(has_hoisted_decl(&p.body[0])));
    }
    #[test]
    fn hoisted_func_detected() {
        let it = Interner::new();
        let out = parse("function f(){}", &it, SourceType::Script);
        out.module
            .with_ast(|p| assert!(has_hoisted_decl(&p.body[0])));
    }
    #[test]
    fn non_hoisted() {
        let it = Interner::new();
        let out = parse("let x = 1;", &it, SourceType::Script);
        out.module
            .with_ast(|p| assert!(!has_hoisted_decl(&p.body[0])));
    }
}
