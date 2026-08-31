//! Primitive values shared by trusted defines, typed folding, cost estimation, and emission.
//!
//! Constant evaluation itself belongs to the owned typed pipeline. This module deliberately has
//! no parser-AST evaluator and no span-indexed constant table.

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
            Self::Bool(value) => *value,
            Self::Str(value) => !value.is_empty(),
            Self::Num(value) => *value != 0.0 && !value.is_nan(),
            Self::Null | Self::Undefined => false,
        }
    }

    pub fn to_source(&self) -> String {
        match self {
            Self::Bool(true) => "true".into(),
            Self::Bool(false) => "false".into(),
            Self::Str(value) => write_string_literal(value),
            Self::Num(value) => write_number_minified(*value),
            Self::Null => "null".into(),
            // This spelling cannot be shadowed by a local binding. Parentheses make it valid in
            // every expression position where a trusted define can be substituted.
            Self::Undefined => "(void 0)".into(),
        }
    }
}

/// Emit the shortest supported JavaScript token for an IEEE-754 number.
pub fn write_number_minified(value: f64) -> String {
    if value == 0.0 && value.is_sign_negative() {
        return "-0".into();
    }
    if value.is_infinite() && value.is_sign_positive() {
        return "(1/0)".into();
    }
    if value.is_infinite() {
        return "(-1/0)".into();
    }
    if value.is_nan() {
        return "(0/0)".into();
    }

    if value.fract() == 0.0 && value.abs() <= 2_f64.powi(53) {
        let integer = value as i64;
        if integer.unsigned_abs() >= 100 {
            let decimal = integer.unsigned_abs().to_string();
            if let Some(shorter) = try_exponential(&decimal) {
                return format!("{}{shorter}", if value < 0.0 { "-" } else { "" });
            }
        }
        format!("{value:.0}")
    } else {
        let decimal = value.to_string();
        if let Some(fraction) = decimal.strip_prefix("0.") {
            format!(".{fraction}")
        } else if let Some(fraction) = decimal.strip_prefix("-0.") {
            format!("-.{fraction}")
        } else {
            decimal
        }
    }
}

/// Try exponential form: `1000` → `1e3`, `50000000` → `5e7`.
fn try_exponential(decimal: &str) -> Option<String> {
    let trailing_zeroes = decimal.len() - decimal.trim_end_matches('0').len();
    if trailing_zeroes < 2 {
        return None;
    }
    let significant_end = decimal.len() - trailing_zeroes;
    let significant = &decimal[..significant_end];
    let exponent = trailing_zeroes + significant.len().saturating_sub(1);
    let exponential = if significant.len() == 1 {
        format!("{significant}e{exponent}")
    } else {
        format!("{}.{}e{exponent}", &significant[..1], &significant[1..])
    };
    (exponential.len() < decimal.len()).then_some(exponential)
}

fn write_string_literal(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{0008}' => output.push_str("\\b"),
            '\u{000C}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            // `\\0` followed by a decimal digit is parsed as a legacy octal escape and is an
            // early error in strict/module code, so NUL always uses a fixed-width escape.
            '\0' => output.push_str("\\x00"),
            '\u{000B}' => output.push_str("\\v"),
            '\u{2028}' => output.push_str("\\u2028"),
            '\u{2029}' => output.push_str("\\u2029"),
            control if control <= '\u{001F}' => {
                use std::fmt::Write as _;
                write!(&mut output, "\\x{:02X}", control as u32)
                    .expect("writing into String cannot fail");
            }
            other => output.push(other),
        }
    }
    output.push('"');
    output
}

#[cfg(test)]
mod tests {
    use super::{ConstVal, write_number_minified};

    #[test]
    fn primitive_truthiness_matches_javascript() {
        assert!(!ConstVal::Bool(false).truthy());
        assert!(ConstVal::Str("value".into()).truthy());
        assert!(!ConstVal::Str(String::new()).truthy());
        assert!(!ConstVal::Num(-0.0).truthy());
        assert!(!ConstVal::Num(f64::NAN).truthy());
        assert!(!ConstVal::Null.truthy());
        assert!(!ConstVal::Undefined.truthy());
    }

    #[test]
    fn primitive_source_tokens_are_shadow_safe_and_escaped() {
        assert_eq!(ConstVal::Bool(true).to_source(), "true");
        assert_eq!(ConstVal::Null.to_source(), "null");
        assert_eq!(ConstVal::Undefined.to_source(), "(void 0)");
        assert_eq!(ConstVal::Num(f64::NAN).to_source(), "(0/0)");
        assert_eq!(ConstVal::Num(f64::INFINITY).to_source(), "(1/0)");
        assert_eq!(ConstVal::Str("\0\u{31}".into()).to_source(), "\"\\x001\"");
        assert_eq!(
            ConstVal::Str("line\u{2028}separator".into()).to_source(),
            "\"line\\u2028separator\""
        );
    }

    #[test]
    fn number_tokens_preserve_special_values_and_choose_short_forms() {
        let cases = [
            (-0.0, "-0"),
            (0.5, ".5"),
            (-0.5, "-.5"),
            (1_000.0, "1e3"),
            (50_000_000.0, "5e7"),
            (12_300_000.0, "1.23e7"),
            (42.0, "42"),
            (f64::NEG_INFINITY, "(-1/0)"),
        ];
        for (value, expected) in cases {
            assert_eq!(write_number_minified(value), expected, "{value:?}");
        }
    }
}
