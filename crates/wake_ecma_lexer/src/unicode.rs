//! 标识符字符分类（DESIGN §4.3：`XID_Start/XID_Continue` 分层区间表）。
//!
//! ASCII 快路径在 [`crate::Lexer`] 内联（首字节跳转表）；本模块只处理 **非 ASCII 慢路径**。
//!
//! ## Phase 1 实现说明（近似）
//!
//! 精确的 `ID_Start`/`ID_Continue` 区间表体量很大，需从 UCD 生成。Phase 1 底线用 std 的
//! Unicode 属性（`char::is_alphabetic` / `is_alphanumeric`）作 **近似**——它对真实代码里的
//! 非 ASCII 标识符（如中文、日文、带重音拉丁字母）判定正确，与精确 `ID_Start` 仅在极少数
//! 边角码点上有差异。**待办（1.3 refinement / P7）**：换成从 UCD 生成的精确分层区间表，
//! 消除这点差异并提速（区间二分 vs std 的多级查表）。ASCII 是快路径，不经过这里。

/// 非 ASCII 码点是否可作标识符 **起始**（`ID_Start` 近似）。
#[inline]
pub fn is_non_ascii_id_start(c: char) -> bool {
    debug_assert!(!c.is_ascii());
    c.is_alphabetic() || is_other_id_start(c)
}

/// 非 ASCII 码点是否可作标识符 **后续**（`ID_Continue` 近似 + ZWNJ/ZWJ）。
#[inline]
pub fn is_non_ascii_id_continue(c: char) -> bool {
    debug_assert!(!c.is_ascii());
    c.is_alphanumeric()
        || is_other_id_start(c)
        || is_id_continue_extra(c)
        // ZWNJ / ZWJ 允许出现在标识符中段（ECMAScript 明确列出）。
        || c == '\u{200C}'
        || c == '\u{200D}'
}

/// `Other_ID_Start` 里少数不被 `is_alphabetic` 覆盖的历史码点。
#[inline]
fn is_other_id_start(c: char) -> bool {
    matches!(
        c,
        '\u{1885}' | '\u{1886}' | '\u{2118}' | '\u{212E}' | '\u{309B}' | '\u{309C}'
    )
}

/// `Other_ID_Continue`：`is_alphanumeric` 未覆盖但规范允许延续的码点。
#[inline]
fn is_id_continue_extra(c: char) -> bool {
    matches!(
        c,
        '\u{00B7}' | '\u{0387}' | '\u{1369}'..='\u{1371}' | '\u{19DA}'
    )
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
