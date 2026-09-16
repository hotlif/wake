//! Conservative facts about completed expression values. Evaluation effects are preserved: this
//! rule reports mistakes but never replaces an expression or executes user coercion hooks.
use wake_common::Interner;
use wake_ecma_ast::*;

const NULL: u16 = 1;
const UNDEFINED: u16 = 2;
const BOOLEAN: u16 = 4;
const NUMBER: u16 = 8;
const BIGINT: u16 = 16;
const STRING: u16 = 32;
const OBJECT: u16 = 64;
const SYMBOL: u16 = 128;
const NULLISH: u16 = NULL | UNDEFINED;
const NUMERIC: u16 = NUMBER | BIGINT;
const UNKNOWN: u16 = NULLISH | BOOLEAN | NUMERIC | STRING | OBJECT | SYMBOL;

pub(crate) fn check(expression: Expression<'_>, interner: &Interner) -> Option<&'static str> {
    match expression {
        Expression::Logical(value) => match value.operator {
            LogicalOperator::And | LogicalOperator::Or => {
                truthiness(value.left, interner).map(|_| "constant-short-circuit")
            }
            LogicalOperator::Coalesce => nullish(value.left).map(|_| "constant-nullish"),
        },
        Expression::Binary(value) if crate::rules::is_comparison(value.operator) => {
            let constants = primitive_constant(value.left) && primitive_constant(value.right);
            let strict = matches!(
                value.operator,
                BinaryOperator::StrictEq | BinaryOperator::StrictNotEq
            );
            let loose = matches!(value.operator, BinaryOperator::Eq | BinaryOperator::NotEq);
            let left_fresh = fresh_reference(value.left);
            let right_fresh = fresh_reference(value.right);
            let fixed = constants
                || (strict
                    && (left_fresh || right_fresh || types(value.left) & types(value.right) == 0))
                || (loose
                    && ((left_fresh && (right_fresh || nullish(value.right) == Some(true)))
                        || (right_fresh && nullish(value.left) == Some(true))));
            fixed.then_some("constant-comparison")
        }
        _ => None,
    }
}

fn fresh_reference(expression: Expression<'_>) -> bool {
    // Assignment can publish the reference; new can return an existing object; class static
    // initialization can publish `this`. None of those prove unobservable fresh identity.
    matches!(
        expression,
        Expression::Object(_)
            | Expression::Array(_)
            | Expression::Function(_)
            | Expression::Arrow(_)
            | Expression::RegExpLiteral(_)
    )
}

fn nullish(expression: Expression<'_>) -> Option<bool> {
    let kinds = types(expression);
    if kinds & NULLISH == 0 {
        Some(false)
    } else if kinds & !NULLISH == 0 {
        Some(true)
    } else {
        None
    }
}

fn types(expression: Expression<'_>) -> u16 {
    match expression {
        Expression::NullLiteral(_) => NULL,
        Expression::NumberLiteral(_) => NUMBER,
        Expression::BigIntLiteral(_) => BIGINT,
        Expression::StringLiteral(_) | Expression::TemplateLiteral(_) => STRING,
        Expression::BooleanLiteral(_) | Expression::PrivateIn(_) => BOOLEAN,
        Expression::Object(_)
        | Expression::Array(_)
        | Expression::Function(_)
        | Expression::Arrow(_)
        | Expression::Class(_)
        | Expression::RegExpLiteral(_)
        | Expression::New(_)
        | Expression::Import(_) => OBJECT,
        Expression::Unary(value) => match value.operator {
            UnaryOperator::Void => UNDEFINED,
            UnaryOperator::Typeof => STRING,
            UnaryOperator::Delete | UnaryOperator::LogicalNot => BOOLEAN,
            UnaryOperator::Plus => NUMBER,
            UnaryOperator::Minus | UnaryOperator::BitwiseNot => numeric_type(value.argument),
        },
        Expression::Binary(value) => match value.operator {
            BinaryOperator::Add => {
                let left = types(value.left);
                let right = types(value.right);
                if left == STRING || right == STRING {
                    STRING
                } else if left == BIGINT && right == BIGINT {
                    BIGINT
                } else if (left | right) & (UNKNOWN & !(NULLISH | BOOLEAN | NUMBER)) == 0 {
                    NUMBER
                } else {
                    NUMERIC | STRING
                }
            }
            BinaryOperator::Ushr => NUMBER,
            BinaryOperator::Sub
            | BinaryOperator::Mul
            | BinaryOperator::Div
            | BinaryOperator::Rem
            | BinaryOperator::Exp
            | BinaryOperator::Shl
            | BinaryOperator::Shr
            | BinaryOperator::BitAnd
            | BinaryOperator::BitOr
            | BinaryOperator::BitXor => {
                let left = numeric_type(value.left);
                let right = numeric_type(value.right);
                if left == BIGINT && right == BIGINT {
                    BIGINT
                } else if left == NUMBER && right == NUMBER {
                    NUMBER
                } else {
                    NUMERIC
                }
            }
            _ => BOOLEAN,
        },
        Expression::Update(value) => numeric_type(value.argument),
        Expression::Sequence(value) => value
            .expressions
            .last()
            .map(|value| types(*value))
            .unwrap_or(UNKNOWN),
        Expression::Assignment(value) if value.operator == AssignmentOperator::Assign => {
            types(value.right)
        }
        Expression::Conditional(value) => types(value.consequent) | types(value.alternate),
        Expression::Logical(value) => match value.operator {
            LogicalOperator::And => types(value.left) | types(value.right),
            LogicalOperator::Or | LogicalOperator::Coalesce => {
                (types(value.left) & !NULLISH) | types(value.right)
            }
        },
        _ => UNKNOWN,
    }
}

fn numeric_type(expression: Expression<'_>) -> u16 {
    let kinds = types(expression);
    if kinds == BIGINT {
        BIGINT
    } else if kinds & (BIGINT | OBJECT) == 0 {
        NUMBER
    } else {
        NUMERIC
    }
}

fn primitive_constant(expression: Expression<'_>) -> bool {
    match expression {
        Expression::NumberLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::NullLiteral(_) => true,
        Expression::TemplateLiteral(value) => value.expressions.is_empty(),
        Expression::Unary(value) => match value.operator {
            UnaryOperator::Void => true,
            UnaryOperator::Typeof => {
                let kinds = types(value.argument);
                (kinds.count_ones() == 1 && kinds != OBJECT)
                    || fresh_reference(value.argument)
                    || matches!(value.argument, Expression::Class(_))
            }
            UnaryOperator::Delete => false,
            _ => primitive_constant(value.argument),
        },
        Expression::Sequence(value) => value
            .expressions
            .last()
            .is_some_and(|value| primitive_constant(*value)),
        Expression::Assignment(value) if value.operator == AssignmentOperator::Assign => {
            primitive_constant(value.right)
        }
        _ => false,
    }
}

fn truthiness(expression: Expression<'_>, interner: &Interner) -> Option<bool> {
    match expression {
        Expression::NullLiteral(_) => Some(false),
        Expression::BooleanLiteral(value) => Some(value.value),
        Expression::NumberLiteral(value) => Some(value.value != 0.0 && !value.value.is_nan()),
        Expression::StringLiteral(value) => Some(!interner.resolve_js(value.value).is_empty()),
        Expression::BigIntLiteral(value) => Some(interner.with_resolved(value.raw, |text| {
            let text = text.strip_suffix('n').unwrap_or(text);
            let text = ["0x", "0X", "0o", "0O", "0b", "0B"]
                .iter()
                .find_map(|prefix| text.strip_prefix(prefix))
                .unwrap_or(text);
            text.bytes().any(|byte| byte != b'0' && byte != b'_')
        })),
        Expression::Object(_)
        | Expression::Array(_)
        | Expression::Function(_)
        | Expression::Arrow(_)
        | Expression::Class(_)
        | Expression::RegExpLiteral(_)
        | Expression::Import(_) => Some(true),
        Expression::TemplateLiteral(value) => {
            if value.quasis.iter().any(|quasi| {
                quasi
                    .cooked
                    .is_some_and(|text| !interner.resolve_js(text).is_empty())
            }) {
                Some(true)
            } else if value.expressions.is_empty() {
                Some(false)
            } else {
                None
            }
        }
        Expression::Unary(value) => match value.operator {
            UnaryOperator::Void => Some(false),
            UnaryOperator::Typeof => Some(true),
            UnaryOperator::LogicalNot => truthiness(value.argument, interner).map(|value| !value),
            _ => None,
        },
        Expression::Assignment(value) if value.operator == AssignmentOperator::Assign => {
            truthiness(value.right, interner)
        }
        Expression::Sequence(value) => value
            .expressions
            .last()
            .and_then(|value| truthiness(*value, interner)),
        Expression::Conditional(value) => {
            let left = truthiness(value.consequent, interner);
            if left == truthiness(value.alternate, interner) {
                left
            } else {
                None
            }
        }
        Expression::Logical(value) => {
            let left = truthiness(value.left, interner);
            let right = truthiness(value.right, interner);
            match value.operator {
                LogicalOperator::And => match left {
                    Some(false) => Some(false),
                    Some(true) => right,
                    _ if right == Some(false) => Some(false),
                    _ => None,
                },
                LogicalOperator::Or => match left {
                    Some(true) => Some(true),
                    Some(false) => right,
                    _ if right == Some(true) => Some(true),
                    _ => None,
                },
                LogicalOperator::Coalesce => match nullish(value.left) {
                    Some(true) => right,
                    Some(false) => left,
                    _ => None,
                },
            }
        }
        _ => None,
    }
}
