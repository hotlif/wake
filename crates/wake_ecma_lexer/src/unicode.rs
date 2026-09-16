//! Unicode 17.0.0 ID_Start / ID_Continue, plus ECMAScript's explicit additions.
//!
//! ASCII 快路径在 [`crate::Lexer`] 内联（首字节跳转表）；本模块只处理 **非 ASCII 慢路径**。
//!
//! Property tables come from the pinned registry dependency, not Rust's evolving char properties.
//! Do not substitute XID: ECMAScript accepts ID characters excluded by normalization closure.

/// 非 ASCII 码点是否可作标识符 **起始**。
#[inline]
pub fn is_non_ascii_id_start(c: char) -> bool {
    debug_assert!(!c.is_ascii());
    unicode_id_start::is_id_start_unicode(c)
}

/// 非 ASCII 码点是否可作标识符 **后续**（ID_Continue + ZWNJ/ZWJ）。
#[inline]
pub fn is_non_ascii_id_continue(c: char) -> bool {
    debug_assert!(!c.is_ascii());
    unicode_id_start::is_id_continue_unicode(c)
        // ZWNJ / ZWJ 允许出现在标识符中段（ECMAScript 明确列出）。
        || c == '\u{200C}'
        || c == '\u{200D}'
}

/// 判定一个 **已解码**（可能来自 `\u` 转义）的字符能否作标识符起始（含 ASCII）。
#[inline]
pub fn is_id_start(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphabetic() || c == '$' || c == '_'
    } else {
        is_non_ascii_id_start(c)
    }
}

/// 判定一个 **已解码** 的字符能否作标识符后续（含 ASCII）。
#[inline]
pub fn is_id_continue(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphanumeric() || c == '$' || c == '_'
    } else {
        is_non_ascii_id_continue(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_rules() {
        assert!(is_id_start('a'));
        assert!(is_id_start('$'));
        assert!(is_id_start('_'));
        assert!(!is_id_start('1'));
        assert!(is_id_continue('1'));
        assert!(!is_id_continue('-'));
    }

    #[test]
    fn unicode_letters() {
        assert!(is_id_start('中'));
        assert!(is_id_continue('文'));
        assert!(is_id_start('é'));
        assert!(is_id_start('π'));
        assert!(!is_id_start('☃')); // 雪人不是字母
    }
}
