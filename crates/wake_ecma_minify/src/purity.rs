use wake_common::Interner;
use wake_ecma_ast::*;

const PURE_BUILTINS: &[&str] = &[
    "Array.from",
    "Array.isArray",
    "Array.of",
    "Array.prototype.at",
    "Array.prototype.concat",
    "Array.prototype.entries",
    "Array.prototype.every",
    "Array.prototype.filter",
    "Array.prototype.find",
    "Array.prototype.findIndex",
    "Array.prototype.findLast",
    "Array.prototype.findLastIndex",
    "Array.prototype.flat",
    "Array.prototype.flatMap",
    "Array.prototype.forEach",
    "Array.prototype.includes",
    "Array.prototype.indexOf",
    "Array.prototype.join",
    "Array.prototype.keys",
    "Array.prototype.lastIndexOf",
    "Array.prototype.map",
    "Array.prototype.reduce",
    "Array.prototype.reduceRight",
    "Array.prototype.slice",
    "Array.prototype.some",
    "Array.prototype.toLocaleString",
    "Array.prototype.toReversed",
    "Array.prototype.toSorted",
    "Array.prototype.toSpliced",
    "Array.prototype.toString",
    "Array.prototype.values",
    "Array.prototype.with",
    "BigInt.asIntN",
    "BigInt.asUintN",
    "BigInt.prototype.toString",
    "BigInt.prototype.valueOf",
    "Boolean.prototype.toString",
    "Boolean.prototype.valueOf",
    "JSON.parse",
    "JSON.stringify",
    "Math.abs",
    "Math.acos",
    "Math.acosh",
    "Math.asin",
    "Math.asinh",
    "Math.atan",
    "Math.atan2",
    "Math.atanh",
    "Math.cbrt",
    "Math.ceil",
    "Math.clz32",
    "Math.cos",
    "Math.cosh",
    "Math.exp",
    "Math.expm1",
    "Math.floor",
    "Math.fround",
    "Math.hypot",
    "Math.imul",
    "Math.log",
    "Math.log10",
    "Math.log1p",
    "Math.log2",
    "Math.max",
    "Math.min",
    "Math.pow",
    "Math.random",
    "Math.round",
    "Math.sign",
    "Math.sin",
    "Math.sinh",
    "Math.sqrt",
    "Math.tan",
    "Math.tanh",
    "Math.trunc",
    "Number.isFinite",
    "Number.isInteger",
    "Number.isNaN",
    "Number.isSafeInteger",
    "Number.parseFloat",
    "Number.parseInt",
    "Number.prototype.toString",
    "Number.prototype.valueOf",
    "Object.assign",
    "Object.create",
    "Object.entries",
    "Object.freeze",
    "Object.fromEntries",
    "Object.getOwnPropertyDescriptor",
    "Object.getOwnPropertyDescriptors",
    "Object.getOwnPropertyNames",
    "Object.getOwnPropertySymbols",
    "Object.getPrototypeOf",
    "Object.is",
    "Object.isExtensible",
    "Object.isFrozen",
    "Object.isSealed",
    "Object.keys",
    "Object.preventExtensions",
    "Object.seal",
    "Object.setPrototypeOf",
    "Object.values",
    "RegExp.prototype.test",
    "RegExp.prototype.toString",
    "String.fromCharCode",
    "String.fromCodePoint",
    "String.prototype.charAt",
    "String.prototype.charCodeAt",
    "String.prototype.codePointAt",
    "String.prototype.concat",
    "String.prototype.endsWith",
    "String.prototype.includes",
    "String.prototype.indexOf",
    "String.prototype.isWellFormed",
    "String.prototype.lastIndexOf",
    "String.prototype.localeCompare",
    "String.prototype.match",
    "String.prototype.matchAll",
    "String.prototype.normalize",
    "String.prototype.padEnd",
    "String.prototype.padStart",
    "String.prototype.repeat",
    "String.prototype.replace",
    "String.prototype.replaceAll",
    "String.prototype.search",
    "String.prototype.slice",
    "String.prototype.split",
    "String.prototype.startsWith",
    "String.prototype.substring",
    "String.prototype.toLocaleLowerCase",
    "String.prototype.toLocaleUpperCase",
    "String.prototype.toLowerCase",
    "String.prototype.toString",
    "String.prototype.toUpperCase",
    "String.prototype.toWellFormed",
    "String.prototype.trim",
    "String.prototype.trimEnd",
    "String.prototype.trimStart",
    "String.prototype.valueOf",
    "Symbol.prototype.toString",
    "Symbol.prototype.valueOf",
    "decodeURI",
    "decodeURIComponent",
    "encodeURI",
    "encodeURIComponent",
    "escape",
    "isFinite",
    "isNaN",
    "parseFloat",
    "parseInt",
    "unescape",
];

fn is_pure_builtin(name: &str) -> bool {
    PURE_BUILTINS.binary_search(&name).is_ok()
}

fn prototype_for_literal(e: &Expression) -> Option<&'static str> {
    match e {
        Expression::StringLiteral(_) => Some("String.prototype"),
        Expression::NumberLiteral(_) => Some("Number.prototype"),
        Expression::BooleanLiteral(_) => Some("Boolean.prototype"),
        Expression::RegExpLiteral(_) => Some("RegExp.prototype"),
        Expression::Array(_) => Some("Array.prototype"),
        Expression::Object(_) | Expression::Class(_) => Some("Object.prototype"),
        Expression::NullLiteral(_) | Expression::BigIntLiteral(_) => None,
        _ => None,
    }
}

fn reconstruct_member_chain(e: &Expression, interner: Option<&Interner>) -> Option<String> {
    match e {
        Expression::Identifier(id) => Some(interner?.resolve(id.name)),
        Expression::Member(m) if !m.optional => {
            let prop = match &m.property {
                MemberProperty::Ident(id) => interner?.resolve(id.name),
                _ => return None,
            };
            // If the base is a literal, map to prototype method
            if let Some(proto) = prototype_for_literal(&m.object) {
                return Some(format!("{}.{}", proto, prop));
            }
            let base = reconstruct_member_chain(&m.object, interner)?;
            Some(format!("{}.{}", base, prop))
        }
        _ => None,
    }
}

pub fn call_is_pure(callee: &Expression, args: &[Expression], interner: Option<&Interner>) -> bool {
    for arg in args {
        if !expr_is_pure(arg) {
            return false;
        }
    }
    let Some(name) = reconstruct_member_chain(callee, interner) else {
        return false;
    };
    is_pure_builtin(&name)
}

pub use crate::const_eval::expr_is_pure;

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::Interner;
    use wake_ecma_ast::{Expression, SourceType, Statement};
    use wake_ecma_parser::parse;

    fn call_is_pure_test(callee_src: &str, args_src: &[&str]) -> bool {
        let it = Interner::new();
        let args = args_src.join(", ");
        let full_src = format!("{}({});", callee_src, args);
        let out = parse(&full_src, &it, SourceType::Script);
        assert!(!out.has_errors(), "parse error: {:?}", out.diagnostics);
        out.module.with_ast(|p| {
            let stmt = p.body.first().expect("no statements");
            match stmt {
                Statement::Expression(es) => {
                    if let Expression::Call(c) = &es.expression {
                        let args_vec: Vec<_> = c.arguments.iter().cloned().collect();
                        call_is_pure(&c.callee, &args_vec, Some(&it))
                    } else {
                        panic!("expected call expression");
                    }
                }
                _ => panic!("expected expression statement"),
            }
        })
    }

    fn expr_is_pure_test(src: &str) -> bool {
        let it = Interner::new();
        let out = parse(src, &it, SourceType::Script);
        assert!(!out.has_errors(), "parse error: {:?}", out.diagnostics);
        out.module.with_ast(|p| {
            let stmt = p.body.first().expect("no statements");
            match stmt {
                Statement::Expression(es) => expr_is_pure(&es.expression),
                _ => panic!("expected expression statement"),
            }
        })
    }

    #[test]
    fn sorted() {
        for w in PURE_BUILTINS.windows(2) {
            assert!(w[0] <= w[1], "{} > {}", w[0], w[1]);
        }
    }

    // ── Global pure functions ──
    #[test]
    fn pure_parse_int() {
        assert!(call_is_pure_test("parseInt", &["\"42\""]));
    }
    #[test]
    fn pure_parse_float() {
        assert!(call_is_pure_test("parseFloat", &["\"3.14\""]));
    }
    #[test]
    fn pure_encode_uri() {
        assert!(call_is_pure_test("encodeURI", &["\"hello\""]));
    }
    #[test]
    fn pure_decode_uri() {
        assert!(call_is_pure_test("decodeURI", &["\"hello%20\""]));
    }
    #[test]
    fn pure_is_finite() {
        assert!(call_is_pure_test("isFinite", &["42"]));
    }
    #[test]
    fn pure_is_nan() {
        assert!(call_is_pure_test("isNaN", &["NaN"]));
    }
    #[test]
    fn pure_escape() {
        assert!(call_is_pure_test("escape", &["\"x\""]));
    }
    #[test]
    fn pure_unescape() {
        assert!(call_is_pure_test("unescape", &["\"x\""]));
    }

    // ── Math ──
    #[test]
    fn pure_math_abs() {
        assert!(call_is_pure_test("Math.abs", &["-5"]));
    }
    #[test]
    fn pure_math_max() {
        assert!(call_is_pure_test("Math.max", &["1", "2"]));
    }
    #[test]
    fn pure_math_floor() {
        assert!(call_is_pure_test("Math.floor", &["3.14"]));
    }
    #[test]
    fn pure_math_random() {
        assert!(call_is_pure_test("Math.random", &[]));
    }

    // ── Static methods ──
    #[test]
    fn pure_string_from_char_code() {
        assert!(call_is_pure_test("String.fromCharCode", &["65"]));
    }
    #[test]
    fn pure_array_is_array() {
        assert!(call_is_pure_test("Array.isArray", &["[]"]));
    }
    #[test]
    fn pure_array_from() {
        assert!(call_is_pure_test("Array.from", &["[1,2,3]"]));
    }

    // ── Prototype methods ──
    #[test]
    fn pure_str_slice() {
        assert!(call_is_pure_test("\"hello\".slice", &["1", "3"]));
    }
    #[test]
    fn pure_str_replace() {
        assert!(call_is_pure_test("\"hello\".replace", &["\"l\"", "\"x\""]));
    }
    #[test]
    fn pure_str_trim() {
        assert!(call_is_pure_test("\"  hi  \".trim", &[]));
    }
    #[test]
    fn pure_arr_map() {
        assert!(call_is_pure_test("[1,2,3].map", &["x => x"]));
    }
    #[test]
    fn pure_arr_filter() {
        assert!(call_is_pure_test("[1,2,3].filter", &["x => x>1"]));
    }
    #[test]
    fn pure_arr_join() {
        assert!(call_is_pure_test("[1,2,3].join", &["\",\""]));
    }
    #[test]
    fn pure_arr_concat() {
        assert!(call_is_pure_test("[1,2].concat", &["[3,4]"]));
    }
    #[test]
    fn pure_arr_flat() {
        assert!(call_is_pure_test("[[1],[2]].flat", &[]));
    }

    // ── JSON ──
    #[test]
    fn pure_json_parse() {
        assert!(call_is_pure_test("JSON.parse", &["\"{}\""]));
    }
    #[test]
    fn pure_json_stringify() {
        assert!(call_is_pure_test("JSON.stringify", &["{a:1}"]));
    }

    // ── Object ──
    #[test]
    fn pure_obj_keys() {
        assert!(call_is_pure_test("Object.keys", &["{a:1}"]));
    }
    #[test]
    fn pure_obj_values() {
        assert!(call_is_pure_test("Object.values", &["{a:1}"]));
    }
    #[test]
    fn pure_obj_assign() {
        assert!(call_is_pure_test("Object.assign", &["{}", "{a:1}"]));
    }

    // ── Number ──
    #[test]
    fn pure_num_is_nan() {
        assert!(call_is_pure_test("Number.isNaN", &["NaN"]));
    }
    #[test]
    fn pure_num_is_int() {
        assert!(call_is_pure_test("Number.isInteger", &["42"]));
    }

    // ── RegExp ──
    #[test]
    fn pure_regexp_test() {
        assert!(call_is_pure_test("/abc/.test", &["\"abc\""]));
    }

    // ── Impure ──
    #[test]
    fn impure_console() {
        assert!(!call_is_pure_test("console.log", &["\"h\""]));
    }
    #[test]
    fn impure_fetch() {
        assert!(!call_is_pure_test("fetch", &["\"/api\""]));
    }
    #[test]
    fn impure_custom() {
        assert!(!call_is_pure_test("myFunc", &["42"]));
    }
    #[test]
    fn impure_push() {
        assert!(!call_is_pure_test("[].push", &["1"]));
    }
    #[test]
    fn impure_pop() {
        assert!(!call_is_pure_test("[].pop", &[]));
    }
    #[test]
    fn impure_sort() {
        assert!(!call_is_pure_test("[3,1,2].sort", &[]));
    }
    #[test]
    fn impure_date() {
        assert!(!call_is_pure_test("Date.now", &[]));
    }
    #[test]
    fn impure_opt_chain() {
        let it = Interner::new();
        let out = parse("foo?.bar();", &it, SourceType::Script);
        out.module.with_ast(|p| {
            let stmt = p.body.first().unwrap();
            if let Statement::Expression(es) = stmt {
                if let Expression::Call(c) = &es.expression {
                    let a: Vec<_> = c.arguments.iter().cloned().collect();
                    assert!(!call_is_pure(&c.callee, &a, Some(&it)));
                }
            }
        });
    }

    // ── Expression purity ──
    #[test]
    fn expr_number() {
        assert!(expr_is_pure_test("42;"));
    }
    #[test]
    fn expr_string() {
        assert!(expr_is_pure_test("\"h\";"));
    }
    #[test]
    fn expr_bool() {
        assert!(expr_is_pure_test("true;"));
    }
    #[test]
    fn expr_null() {
        assert!(expr_is_pure_test("null;"));
    }
    #[test]
    fn expr_binary() {
        assert!(expr_is_pure_test("1+2;"));
    }
    #[test]
    fn expr_impure_assign() {
        assert!(!expr_is_pure_test("x=42;"));
    }
    #[test]
    fn expr_impure_update() {
        assert!(!expr_is_pure_test("x++;"));
    }
    #[test]
    fn impure_wrong_arg() {
        assert!(!call_is_pure_test("Math.max", &["x++"]));
    }
}
