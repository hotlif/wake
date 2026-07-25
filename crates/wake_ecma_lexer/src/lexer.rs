//! 字节级词法分析器（DESIGN §4.3）。
//!
//! - 输入按 `&[u8]` 处理，ASCII 首字节查 256 项 [`CLASS`] 跳转表，非 ASCII 才进 Unicode 慢路径。
//! - 零拷贝：token 只带 [`Span`]，值（标识符除外）惰性解码。
//! - parser 驱动：`/` 的除号/正则二义由 [`Lexer::next`] 的 `regex_allowed` 参数决定。
//! - 模板串用花括号栈处理 `` `a${b}c` `` 嵌套。
//! - 错误恢复：非法输入产出 [`TokenKind::Error`] 并继续，不中断后续诊断（DESIGN §4.3）。

use std::borrow::Cow;

use memchr::{memchr, memchr2, memchr3};
use wake_common::{Diagnostic, Span};

use crate::token::{Keyword, Token, TokenKind};
use crate::unicode::{
    is_id_continue, is_id_start, is_non_ascii_id_continue, is_non_ascii_id_start,
};

/// 首字节分类（256 项跳转表的取值）。空白/换行在 trivia 跳过阶段处理，不进入主分派。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    /// 空白（含 tab / 垂直制表 / form feed 等）。
    Whitespace,
    /// 换行符（\n \r）。
    LineTerminator,
    /// ASCII 标识符起始（字母 / `$` / `_`）。
    IdentStart,
    /// ASCII 数字。
    Digit,
    /// `'` 或 `"`。
    Quote,
    /// 反引号。
    Backtick,
    /// 标点/运算符首字节（走 `scan_punct`）。
    Punct,
    /// 非 ASCII（>=0x80）：Unicode 慢路径。
    NonAscii,
    /// 其他非法首字节。
    Invalid,
}

const fn build_class_table() -> [Class; 256] {
    let mut t = [Class::Invalid; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        t[i] = if b >= 0x80 {
            Class::NonAscii
        } else if b == b'\n' || b == b'\r' {
            Class::LineTerminator
        } else if b == b' ' || b == b'\t' || b == 0x0B || b == 0x0C {
            Class::Whitespace
        } else if b.is_ascii_digit() {
            Class::Digit
        } else if b.is_ascii_alphabetic() || b == b'$' || b == b'_' {
            Class::IdentStart
        } else if b == b'\'' || b == b'"' {
            Class::Quote
        } else if b == b'`' {
            Class::Backtick
        } else {
            // 所有 ASCII 标点交给 scan_punct 精细分辨；非标点的控制字符在那里报错。
            Class::Punct
        };
        i += 1;
    }
    t
}

/// 首字节 → [`Class`] 的 256 项跳转表（DESIGN §4.3「256 项首字节跳转表」）。
static CLASS: [Class; 256] = build_class_table();

/// ASCII 标识符延续字符表：`a-z A-Z 0-9 $ _` 为 `true`，其余（含 ≥0x80）为 `false`。
/// 标识符快路径用单次查表取代 `is_ascii_alphanumeric() || $ || _` 的多次范围比较——
/// 标识符是真实代码里最大宗的 token（react-dom 占 42% 字节），这条循环是热中之热。
const fn build_id_cont_table() -> [bool; 256] {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 128 {
        let b = i as u8;
        t[i] = matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'$' | b'_');
        i += 1;
    }
    t
}
static ID_CONT_ASCII: [bool; 256] = build_id_cont_table();

/// 花括号栈项：区分普通 `{` 与模板替换的边界。
#[derive(Clone, Copy, PartialEq)]
enum Brace {
    /// 普通 `{`（对象/块）。
    Normal,
    /// 模板替换 `${` 打开的花括号，遇到匹配 `}` 时恢复模板扫描。
    Template,
}

/// 词法状态快照（不透明）。由 [`Lexer::checkpoint`] 产生，交给 [`Lexer::rewind`] 回溯。
pub struct LexerCheckpoint {
    pos: usize,
    brace_stack: Vec<Brace>,
    diag_len: usize,
}

/// 词法分析器。持有源码、游标与诊断。**不** 持有 interner——标识符驻留惰性交给 parser（DESIGN §4.3）。
pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    diagnostics: Vec<Diagnostic>,
    brace_stack: Vec<Brace>,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Lexer<'a> {
        // 跳过 UTF-8 BOM（U+FEFF）。
        let start = if src.as_bytes().starts_with(&[0xEF, 0xBB, 0xBF]) {
            3
        } else {
            0
        };
        let mut lexer = Lexer {
            src,
            bytes: src.as_bytes(),
            pos: start,
            diagnostics: Vec::new(),
            brace_stack: Vec::new(),
        };
        lexer.skip_hashbang();
        lexer
    }

    /// 至今累积的诊断（错误恢复模式下可能多条）。
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// 记录当前词法状态，供 parser 试探解析（如 TS 调用类型实参 `f<T>()`）后回溯。
    /// brace_stack 完整克隆（通常极小），diagnostics 记长度以丢弃试探期间的误报。
    pub fn checkpoint(&self) -> LexerCheckpoint {
        LexerCheckpoint {
            pos: self.pos,
            brace_stack: self.brace_stack.clone(),
            diag_len: self.diagnostics.len(),
        }
    }

    /// 回退到 `checkpoint` 记录的词法状态。
    pub fn rewind(&mut self, cp: LexerCheckpoint) {
        self.pos = cp.pos;
        self.brace_stack = cp.brace_stack;
        self.diagnostics.truncate(cp.diag_len);
    }

    // ==================================================================
    // 游标原语
    // ==================================================================

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    #[inline]
    fn peek_at(&self, off: usize) -> Option<u8> {
        self.bytes.get(self.pos + off).copied()
    }

    #[inline]
    fn bump(&mut self) {
        self.pos += 1;
    }

    #[inline]
    fn span_from(&self, lo: usize) -> Span {
        Span::new(lo as u32, self.pos as u32)
    }

    /// 从当前字节解码一个 `char`（用于非 ASCII 慢路径），返回 (char, 字节长度)。
    fn current_char(&self) -> Option<(char, usize)> {
        self.src[self.pos..]
            .chars()
            .next()
            .map(|c| (c, c.len_utf8()))
    }

    fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.diagnostics.push(
            Diagnostic::error(msg)
                .with_code("WAKE0100")
                .with_primary(span, "此处"),
        );
    }

    fn skip_hashbang(&mut self) {
        if self.pos == 0 && self.peek() == Some(b'#') && self.peek_at(1) == Some(b'!') {
            while let Some(b) = self.peek() {
                if b == b'\n' || b == b'\r' {
                    break;
                }
                self.bump();
            }
        }
    }

    // ==================================================================
    // 主入口：parser 驱动
    // ==================================================================

    /// 取下一个 token。`regex_allowed` 为真时把 `/` 当正则字面量起始，否则当除号（DESIGN §4.3）。
    pub fn next(&mut self, regex_allowed: bool) -> Token {
        let newline_before = self.skip_trivia();
        let lo = self.pos;

        let Some(b) = self.peek() else {
            return Token::new(TokenKind::Eof, self.span_from(lo), newline_before);
        };

        let kind = match CLASS[b as usize] {
            Class::IdentStart => self.scan_ident_ascii(lo),
            Class::Digit => self.scan_number(),
            Class::Quote => self.scan_string(b),
            Class::Backtick => {
                self.bump(); // 吃掉 `
                self.scan_template_from_backtick(lo)
            }
            Class::Punct => return self.scan_punct(lo, regex_allowed, newline_before),
            Class::NonAscii => self.scan_non_ascii(lo),
            // 空白/换行已被 skip_trivia 处理；若仍到达此处则是逻辑错误。
            Class::Whitespace | Class::LineTerminator => unreachable!("trivia 应已跳过"),
            Class::Invalid => {
                self.bump();
                self.error(self.span_from(lo), format!("非法字符 U+{:04X}", b));
                TokenKind::Error
            }
        };

        Token::new(kind, self.span_from(lo), newline_before)
    }

    /// 重定位到 `from` 再取一个普通 token（供 JSX 解析结束后恢复正常词法）。
    pub fn next_at(&mut self, from: u32, regex_allowed: bool) -> Token {
        self.pos = from as usize;
        self.next(regex_allowed)
    }

    /// 从 `from` 处取一个 JSX 子节点 token（DESIGN §4.3）：
    /// `<`/`{` 返回对应标点，否则扫描到下一个 `<`/`{`/EOF 之间的原始文本，返回 [`TokenKind::JsxText`]。
    pub fn next_jsx_child_token(&mut self, from: u32) -> Token {
        self.pos = from as usize;
        let lo = self.pos;
        match self.peek() {
            None => Token::new(TokenKind::Eof, self.span_from(lo), false),
            Some(b'<') => {
                self.bump();
                Token::new(TokenKind::Lt, self.span_from(lo), false)
            }
            Some(b'{') => {
                self.bump();
                Token::new(TokenKind::LBrace, self.span_from(lo), false)
            }
            _ => {
                match memchr2(b'<', b'{', &self.bytes[self.pos..]) {
                    Some(off) => self.pos += off,
                    None => self.pos = self.bytes.len(),
                }
                Token::new(TokenKind::JsxText, self.span_from(lo), false)
            }
        }
    }

    /// 读取 `pos` 处的原始字节（供 JSX 解析判断 `</` 闭合标签）。
    pub fn byte_at(&self, pos: u32) -> Option<u8> {
        self.bytes.get(pos as usize).copied()
    }

    /// 便捷：正则允许上下文（表达式起始位置）。
    #[inline]
    pub fn next_token_regex_allowed(&mut self) -> Token {
        self.next(true)
    }

    /// 便捷：除号允许上下文（表达式之后）。
    #[inline]
    pub fn next_token_div_allowed(&mut self) -> Token {
        self.next(false)
    }

    // ==================================================================
    // Trivia：空白 / 换行 / 注释
    // ==================================================================

    /// 跳过空白、换行与注释，返回其间是否出现过换行（ASI 用）。
    fn skip_trivia(&mut self) -> bool {
        let mut newline = false;
        while let Some(b) = self.peek() {
            match b {
                b' ' | b'\t' | 0x0B | 0x0C => self.bump(),
                b'\n' => {
                    newline = true;
                    self.bump();
                }
                b'\r' => {
                    newline = true;
                    self.bump();
                    if self.peek() == Some(b'\n') {
                        self.bump();
                    }
                }
                b'/' => match self.peek_at(1) {
                    Some(b'/') => self.skip_line_comment(),
                    Some(b'*') => newline |= self.skip_block_comment(),
                    _ => break, // 真正的 `/` token，交给分派
                },
                0xEF | 0xE2 | 0xC2 => {
                    // 可能是非 ASCII 空白（U+FEFF BOM / U+2028/2029 行分隔 / U+00A0 NBSP 等）。
                    if let Some(consumed) = self.try_skip_unicode_space() {
                        newline |= consumed;
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        newline
    }

    fn skip_line_comment(&mut self) {
        self.pos += 2; // //
        // memchr 批量扫到行终止符（U+2028/2029 属罕见边角，交由下一轮 trivia 处理）。
        match memchr2(b'\n', b'\r', &self.bytes[self.pos..]) {
            Some(off) => self.pos += off,
            None => self.pos = self.bytes.len(),
        }
    }

    /// 跳过块注释，返回其中是否含换行。未闭合则报错并跳到文件尾。
    fn skip_block_comment(&mut self) -> bool {
        let lo = self.pos;
        self.pos += 2; // /*
        let mut newline = false;
        loop {
            // memchr 批量跳到下一个 `*` 或换行（换行需计入 newline 标志）。
            match memchr3(b'*', b'\n', b'\r', &self.bytes[self.pos..]) {
                None => {
                    self.error(self.span_from(lo), "未闭合的块注释 `/* ... */`");
                    self.pos = self.bytes.len();
                    return newline;
                }
                Some(off) => {
                    self.pos += off;
                    match self.bytes[self.pos] {
                        b'*' => {
                            if self.peek_at(1) == Some(b'/') {
                                self.pos += 2;
                                return newline;
                            }
                            self.bump(); // 孤立的 `*`，继续
                        }
                        _ => {
                            // \n 或 \r
                            newline = true;
                            self.bump();
                        }
                    }
                }
            }
        }
    }

    /// 尝试跳过一个非 ASCII 空白/行分隔字符。返回 `Some(是否换行)`；不是空白则 `None`。
    fn try_skip_unicode_space(&mut self) -> Option<bool> {
        let (c, len) = self.current_char()?;
        match c {
            // 行分隔 / 段分隔视作换行。
            '\u{2028}' | '\u{2029}' => {
                self.pos += len;
                Some(true)
            }
            // 各类 Unicode 空白 + BOM。
            '\u{00A0}'
            | '\u{FEFF}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}' => {
                self.pos += len;
                Some(false)
            }
            _ => None,
        }
    }

    // ==================================================================
    // 标识符 / 关键字
    // ==================================================================

    /// ASCII 起始的标识符快路径。含转义（`\u`）时转慢路径。
    fn scan_ident_ascii(&mut self, lo: usize) -> TokenKind {
        // 快路径：连续 ASCII 标识符字符，无反斜杠。直接在切片上推进游标 `i`——
        // `i < len` 守卫下 `bytes[i]` 的边界检查被 LLVM 消除，且查 ID_CONT_ASCII 表一次比较，
        // 比每字节 `peek()`（Option + 边界检查）+ 多次范围比较快得多。标识符是最大宗 token。
        let bytes = self.bytes;
        let len = bytes.len();
        let mut i = self.pos;
        while i < len {
            let b = bytes[i];
            if ID_CONT_ASCII[b as usize] {
                i += 1;
            } else if b == b'\\' || b >= 0x80 {
                // 遇到转义或非 ASCII → 转慢路径重扫（它会把 pos 重置到 lo）。
                return self.scan_ident_slow(lo);
            } else {
                break;
            }
        }
        self.pos = i;
        // 快路径全 ASCII 无转义 → 可能是关键字（对原始切片直接比较，无需驻留）。
        classify_ident(&self.src[lo..i])
    }

    /// 非 ASCII 起始的标识符（或非法字符）。
    fn scan_non_ascii(&mut self, lo: usize) -> TokenKind {
        let Some((c, _)) = self.current_char() else {
            self.bump();
            return TokenKind::Error;
        };
        if is_non_ascii_id_start(c) {
            self.scan_ident_slow(lo)
        } else {
            let len = c.len_utf8();
            self.pos += len;
            self.error(self.span_from(lo), format!("非法字符 `{c}`"));
            TokenKind::Error
        }
    }

    /// 慢路径：含 `\u` 转义或非 ASCII 的标识符。**只校验并前进**，不解码不驻留（惰性，DESIGN §4.3）。
    ///
    /// 走到慢路径的标识符必含转义或非 ASCII 字符，而所有关键字都是纯 ASCII 无转义，
    /// 故慢路径结果恒为 [`TokenKind::Ident`]（不可能是关键字）。真正的文本解码在
    /// [`Lexer::identifier_text`] 惰性完成。
    fn scan_ident_slow(&mut self, lo: usize) -> TokenKind {
        self.pos = lo;
        let mut first = true;
        let mut any = false;

        while let Some(b) = self.peek() {
            if b == b'\\' {
                self.bump();
                if self.peek() != Some(b'u') {
                    self.error(self.span_from(lo), "标识符中的转义必须是 `\\u`");
                    return TokenKind::Error;
                }
                self.bump();
                match self.scan_unicode_escape_value(lo) {
                    Some(cp) => {
                        let ok = if first {
                            is_id_start(cp)
                        } else {
                            is_id_continue(cp)
                        };
                        if !ok {
                            self.error(self.span_from(lo), "转义得到的字符不是合法标识符字符");
                            return TokenKind::Error;
                        }
                    }
                    None => return TokenKind::Error,
                }
            } else if b < 0x80 {
                let ok = if first {
                    b.is_ascii_alphabetic() || b == b'$' || b == b'_'
                } else {
                    b.is_ascii_alphanumeric() || b == b'$' || b == b'_'
                };
                if !ok {
                    break;
                }
                self.bump();
            } else {
                let (c, len) = self.current_char().unwrap();
                let ok = if first {
                    is_non_ascii_id_start(c)
                } else {
                    is_non_ascii_id_continue(c)
                };
                if !ok {
                    break;
                }
                self.pos += len;
            }
            first = false;
            any = true;
        }

        if !any {
            self.bump();
            self.error(self.span_from(lo), "无效标识符");
            return TokenKind::Error;
        }
        TokenKind::Ident
    }

    // ==================================================================
    // 数字
    // ==================================================================

    fn scan_number(&mut self) -> TokenKind {
        let lo = self.pos;
        let mut is_bigint_allowed = true;

        if self.peek() == Some(b'0') {
            match self.peek_at(1) {
                Some(b'x') | Some(b'X') => {
                    self.pos += 2;
                    self.scan_radix_digits(lo, |b| b.is_ascii_hexdigit());
                    return self.finish_number(lo, true);
                }
                Some(b'o') | Some(b'O') => {
                    self.pos += 2;
                    self.scan_radix_digits(lo, |b| (b'0'..=b'7').contains(&b));
                    return self.finish_number(lo, true);
                }
                Some(b'b') | Some(b'B') => {
                    self.pos += 2;
                    self.scan_radix_digits(lo, |b| b == b'0' || b == b'1');
                    return self.finish_number(lo, true);
                }
                _ => {}
            }
        }

        // 十进制整数部分。
        self.scan_decimal_digits();
        // 小数点。
        if self.peek() == Some(b'.') {
            is_bigint_allowed = false;
            self.bump();
            self.scan_decimal_digits();
        }
        // 指数。
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            is_bigint_allowed = false;
            self.bump();
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.bump();
            }
            if !matches!(self.peek(), Some(b) if b.is_ascii_digit()) {
                self.error(self.span_from(lo), "指数缺少数字");
                return TokenKind::Error;
            }
            self.scan_decimal_digits();
        }

        self.finish_number(lo, is_bigint_allowed)
    }

    /// 收尾：可选 BigInt 后缀 `n`，并检查紧邻标识符字符（非法）。
    fn finish_number(&mut self, lo: usize, bigint_allowed: bool) -> TokenKind {
        let is_bigint = self.peek() == Some(b'n');
        if is_bigint {
            if !bigint_allowed {
                self.error(self.span_from(lo), "BigInt 不能带小数点或指数");
                return TokenKind::Error;
            }
            self.bump();
        }
        // 数字后紧跟标识符起始字符是错误（如 `3in`），报错但恢复。
        if let Some(b) = self.peek()
            && (b.is_ascii_alphabetic() || b == b'_' || b == b'$' || b >= 0x80)
        {
            self.error(self.span_from(lo), "数字后紧邻标识符字符");
        }
        if is_bigint {
            TokenKind::BigInt
        } else {
            TokenKind::Number
        }
    }

    fn scan_decimal_digits(&mut self) {
        self.scan_radix_digits(self.pos, |b| b.is_ascii_digit());
    }

    /// 扫描某进制的数字序列，允许合法位置的数字分隔符 `_`。
    fn scan_radix_digits(&mut self, lo: usize, is_digit: impl Fn(u8) -> bool) {
        let mut last_was_sep = false;
        let mut any = false;
        while let Some(b) = self.peek() {
            if is_digit(b) {
                self.bump();
                last_was_sep = false;
                any = true;
            } else if b == b'_' {
                if !any || last_was_sep {
                    self.error(self.span_from(lo), "数字分隔符 `_` 位置非法");
                }
                self.bump();
                last_was_sep = true;
            } else {
                break;
            }
        }
        if last_was_sep {
            self.error(self.span_from(lo), "数字不能以分隔符 `_` 结尾");
        }
    }

    // ==================================================================
    // 字符串
    // ==================================================================

    fn scan_string(&mut self, quote: u8) -> TokenKind {
        let lo = self.pos;
        self.bump(); // 开引号
        loop {
            // memchr 批量跳到下一个「引号 / 反斜杠 / 换行」——普通字符段一次扫过。
            let Some(off) = self.find_string_stop(quote) else {
                self.error(self.span_from(lo), "未闭合的字符串字面量");
                return TokenKind::Error;
            };
            self.pos += off;
            match self.bytes[self.pos] {
                b if b == quote => {
                    self.bump();
                    return TokenKind::Str;
                }
                b'\\' => {
                    self.bump();
                    self.consume_escape_for_validation(lo);
                }
                _ => {
                    // \n 或 \r
                    self.error(self.span_from(lo), "字符串字面量中出现未转义的换行");
                    return TokenKind::Error;
                }
            }
        }
    }

    /// 从当前位置起，找到「引号 / 反斜杠 / 换行(\n \r)」中最早出现者的相对偏移。
    fn find_string_stop(&self, quote: u8) -> Option<usize> {
        let hay = &self.bytes[self.pos..];
        // memchr 只能同时找 3 个字节；第 4 个 `\r` 单独找。**关键**：只在首个
        // quote/`\`/`\n` 之前找 `\r`——更靠后的 `\r` 不可能是更早的停止点。否则当源码用
        // 纯 LF 行尾（`\r` 全无，如 react-dom）时，`memchr(\r)` 会每次扫到 EOF，
        // 令字符串多的大文件退化到 O(字符串数 × 剩余长度)（实测 react-dom 因此慢 ~6×）。
        let a = memchr3(quote, b'\\', b'\n', hay);
        let bound = a.unwrap_or(hay.len());
        // `\r`（若有）必落在 `[0, bound)` 内，严格早于 `a`，故它就是更早的停止点；否则用 `a`。
        memchr(b'\r', &hay[..bound]).or(a)
    }

    /// 消费一个转义序列（仅做基本合法性校验，值解码在 [`Lexer::string_value`]）。
    fn consume_escape_for_validation(&mut self, lo: usize) {
        match self.peek() {
            None => self.error(self.span_from(lo), "字符串以未完成的转义结束"),
            // 行接续：\ 后跟换行。
            Some(b'\n') => self.bump(),
            Some(b'\r') => {
                self.bump();
                if self.peek() == Some(b'\n') {
                    self.bump();
                }
            }
            Some(b'x') => {
                self.bump();
                for _ in 0..2 {
                    if !matches!(self.peek(), Some(b) if b.is_ascii_hexdigit()) {
                        self.error(self.span_from(lo), "`\\x` 需要两位十六进制");
                        return;
                    }
                    self.bump();
                }
            }
            Some(b'u') => {
                self.bump();
                self.scan_unicode_escape_value(lo);
            }
            // 其它单字符转义（含 \0 \n \t \\ \' \" 等）与非法但可恢复的转义。
            Some(_) => {
                if self.peek().is_some_and(|b| b >= 0x80) {
                    let len = self.current_char().map(|(_, l)| l).unwrap_or(1);
                    self.pos += len;
                } else {
                    self.bump();
                }
            }
        }
    }

    /// 扫描 `\u` 之后的部分（`{...}` 或四位十六进制），返回解码码点。失败报错并返回 None。
    fn scan_unicode_escape_value(&mut self, lo: usize) -> Option<char> {
        if self.peek() == Some(b'{') {
            self.bump();
            let mut value: u32 = 0;
            let mut any = false;
            while let Some(b) = self.peek() {
                if b == b'}' {
                    self.bump();
                    if !any {
                        self.error(self.span_from(lo), "`\\u{}` 为空");
                        return None;
                    }
                    return char::from_u32(value).or_else(|| {
                        self.error(self.span_from(lo), "码点超出 Unicode 范围");
                        None
                    });
                }
                let d = hex_val(b)?;
                value = value.saturating_mul(16).saturating_add(d as u32);
                any = true;
                self.bump();
                if value > 0x10_FFFF {
                    self.error(self.span_from(lo), "码点超出 0x10FFFF");
                    return None;
                }
            }
            self.error(self.span_from(lo), "未闭合的 `\\u{`");
            None
        } else {
            let mut value: u32 = 0;
            for _ in 0..4 {
                match self.peek().and_then(hex_val) {
                    Some(d) => {
                        value = value * 16 + d as u32;
                        self.bump();
                    }
                    None => {
                        self.error(self.span_from(lo), "`\\u` 需要四位十六进制");
                        return None;
                    }
                }
            }
            char::from_u32(value).or_else(|| {
                self.error(self.span_from(lo), "非法的 UTF-16 码元（孤立代理项）");
                None
            })
        }
    }

    // ==================================================================
    // 模板串
    // ==================================================================

    /// 从已消费的 `` ` `` 开始扫描模板：产出 `TemplateNoSub` 或 `TemplateHead`。
    fn scan_template_from_backtick(&mut self, lo: usize) -> TokenKind {
        self.scan_template_body(lo, true)
    }

    /// 从 `}` 恢复模板扫描：产出 `TemplateMiddle` 或 `TemplateTail`。调用前 `}` 已消费。
    fn scan_template_continuation(&mut self, lo: usize) -> TokenKind {
        self.scan_template_body(lo, false)
    }

    /// 模板体扫描。`head` 区分是 `` ` `` 起还是 `}` 起，用于产出正确的四种 template 种类。
    fn scan_template_body(&mut self, lo: usize, head: bool) -> TokenKind {
        loop {
            // memchr 批量跳到「反引号 / `$` / 反斜杠」。模板允许换行，无需扫换行。
            let Some(off) = memchr3(b'`', b'$', b'\\', &self.bytes[self.pos..]) else {
                self.error(self.span_from(lo), "未闭合的模板字符串");
                self.pos = self.bytes.len();
                return TokenKind::Error;
            };
            self.pos += off;
            match self.bytes[self.pos] {
                b'`' => {
                    self.bump();
                    return if head {
                        TokenKind::TemplateNoSub
                    } else {
                        TokenKind::TemplateTail
                    };
                }
                b'$' => {
                    if self.peek_at(1) == Some(b'{') {
                        self.pos += 2;
                        self.brace_stack.push(Brace::Template);
                        return if head {
                            TokenKind::TemplateHead
                        } else {
                            TokenKind::TemplateMiddle
                        };
                    }
                    self.bump(); // 孤立的 `$`
                }
                _ => {
                    // 反斜杠
                    self.bump();
                    self.consume_escape_for_validation(lo);
                }
            }
        }
    }

    // ==================================================================
    // 正则
    // ==================================================================

    fn scan_regex(&mut self, lo: usize) -> TokenKind {
        // 进入时 `/` 已消费。
        let mut in_class = false;
        loop {
            match self.peek() {
                None | Some(b'\n') | Some(b'\r') => {
                    self.error(self.span_from(lo), "未闭合的正则字面量");
                    return TokenKind::Error;
                }
                Some(b'\\') => {
                    self.bump();
                    if self.peek().is_some() {
                        self.bump(); // 跳过被转义字符
                    }
                }
                Some(b'[') => {
                    in_class = true;
                    self.bump();
                }
                Some(b']') => {
                    in_class = false;
                    self.bump();
                }
                Some(b'/') if !in_class => {
                    self.bump();
                    break;
                }
                Some(b) if b >= 0x80 => {
                    let len = self.current_char().map(|(_, l)| l).unwrap_or(1);
                    self.pos += len;
                }
                _ => self.bump(),
            }
        }
        // flags：连续标识符字符。
        while let Some(b) = self.peek() {
            if b.is_ascii_alphabetic() {
                self.bump();
            } else {
                break;
            }
        }
        TokenKind::Regex
    }

    // ==================================================================
    // 标点 / 运算符
    // ==================================================================

    fn scan_punct(&mut self, lo: usize, regex_allowed: bool, newline_before: bool) -> Token {
        use TokenKind::*;
        let b = self.peek().unwrap();
        self.bump();
        let kind = match b {
            b'(' => LParen,
            b')' => RParen,
            b'{' => {
                self.brace_stack.push(Brace::Normal);
                LBrace
            }
            b'}' => {
                // 若匹配的是模板替换的 `{`，恢复模板扫描而非产出 RBrace。
                if self.brace_stack.pop() == Some(Brace::Template) {
                    return Token::new(
                        self.scan_template_continuation(lo),
                        self.span_from(lo),
                        newline_before,
                    );
                }
                RBrace
            }
            b'[' => LBracket,
            b']' => RBracket,
            b';' => Semicolon,
            b',' => Comma,
            b'~' => Tilde,
            b'@' => At,
            b':' => Colon,
            b'?' => match self.peek() {
                Some(b'.') if !matches!(self.peek_at(1), Some(d) if d.is_ascii_digit()) => {
                    self.bump();
                    QuestionDot
                }
                Some(b'?') => {
                    self.bump();
                    if self.peek() == Some(b'=') {
                        self.bump();
                        QuestionQuestionEq
                    } else {
                        QuestionQuestion
                    }
                }
                _ => Question,
            },
            b'.' => {
                if matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                    // `.5` 形式的数字。
                    self.pos = lo;
                    return Token::new(self.scan_number(), self.span_from(lo), newline_before);
                }
                if self.peek() == Some(b'.') && self.peek_at(1) == Some(b'.') {
                    self.pos += 2;
                    DotDotDot
                } else {
                    Dot
                }
            }
            b'<' => match self.peek() {
                Some(b'<') => {
                    self.bump();
                    self.eq_variant(Shl, ShlEq)
                }
                Some(b'=') => {
                    self.bump();
                    LtEq
                }
                _ => Lt,
            },
            b'>' => match self.peek() {
                Some(b'>') => {
                    self.bump();
                    match self.peek() {
                        Some(b'>') => {
                            self.bump();
                            self.eq_variant(Ushr, UshrEq)
                        }
                        _ => self.eq_variant(Shr, ShrEq),
                    }
                }
                Some(b'=') => {
                    self.bump();
                    GtEq
                }
                _ => Gt,
            },
            b'=' => match self.peek() {
                Some(b'=') => {
                    self.bump();
                    if self.peek() == Some(b'=') {
                        self.bump();
                        EqEqEq
                    } else {
                        EqEq
                    }
                }
                Some(b'>') => {
                    self.bump();
                    Arrow
                }
                _ => Eq,
            },
            b'!' => match self.peek() {
                Some(b'=') => {
                    self.bump();
                    if self.peek() == Some(b'=') {
                        self.bump();
                        NotEqEq
                    } else {
                        NotEq
                    }
                }
                _ => Bang,
            },
            b'+' => match self.peek() {
                Some(b'+') => {
                    self.bump();
                    PlusPlus
                }
                Some(b'=') => {
                    self.bump();
                    PlusEq
                }
                _ => Plus,
            },
            b'-' => match self.peek() {
                Some(b'-') => {
                    self.bump();
                    MinusMinus
                }
                Some(b'=') => {
                    self.bump();
                    MinusEq
                }
                _ => Minus,
            },
            b'*' => match self.peek() {
                Some(b'*') => {
                    self.bump();
                    self.eq_variant(StarStar, StarStarEq)
                }
                Some(b'=') => {
                    self.bump();
                    StarEq
                }
                _ => Star,
            },
            b'%' => self.eq_variant(Percent, PercentEq),
            b'^' => self.eq_variant(Caret, CaretEq),
            b'&' => match self.peek() {
                Some(b'&') => {
                    self.bump();
                    self.eq_variant(AmpAmp, AmpAmpEq)
                }
                Some(b'=') => {
                    self.bump();
                    AmpEq
                }
                _ => Amp,
            },
            b'|' => match self.peek() {
                Some(b'|') => {
                    self.bump();
                    self.eq_variant(PipePipe, PipePipeEq)
                }
                Some(b'=') => {
                    self.bump();
                    PipeEq
                }
                _ => Pipe,
            },
            b'/' => {
                if regex_allowed {
                    return Token::new(self.scan_regex(lo), self.span_from(lo), newline_before);
                }
                self.eq_variant(Slash, SlashEq)
            }
            b'#' => {
                // 私有字段 `#x`。
                if self
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_' || c == b'$' || c >= 0x80)
                {
                    return Token::new(
                        self.scan_private_ident(),
                        self.span_from(lo),
                        newline_before,
                    );
                }
                self.error(self.span_from(lo), "`#` 后需跟标识符（私有字段）");
                Error
            }
            b'\\' => {
                // 以 `\u` 转义起始的标识符（如 `\u{69}f` 即 `if`）。`\` 已被消费，检查 `u`。
                if self.peek() == Some(b'u') {
                    return Token::new(
                        self.scan_ident_slow(lo),
                        self.span_from(lo),
                        newline_before,
                    );
                }
                self.error(
                    self.span_from(lo),
                    "非法的 `\\`（仅 `\\u` 转义可用于标识符）",
                );
                Error
            }
            _ => {
                self.error(self.span_from(lo), format!("非法字符 U+{:04X}", b));
                Error
            }
        };
        Token::new(kind, self.span_from(lo), newline_before)
    }

    /// `#name` 私有字段。`#` 已消费；把后续名字扫完即可（文本惰性由 identifier_text 取）。
    fn scan_private_ident(&mut self) -> TokenKind {
        while let Some(b) = self.peek() {
            if b.is_ascii_alphanumeric() || b == b'$' || b == b'_' {
                self.bump();
            } else if b >= 0x80 {
                let (c, len) = self.current_char().unwrap();
                if is_non_ascii_id_continue(c) {
                    self.pos += len;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        TokenKind::PrivateIdent
    }

    /// 若下一个字节是 `=` 则取 `with_eq`（并消费），否则 `base`。
    #[inline]
    fn eq_variant(&mut self, base: TokenKind, with_eq: TokenKind) -> TokenKind {
        if self.peek() == Some(b'=') {
            self.bump();
            with_eq
        } else {
            base
        }
    }

    // ==================================================================
    // 值惰性解码（parser 需要时调用）
    // ==================================================================

    /// 数字 token 的数值（惰性）。调用方保证 `span` 来自一个 [`TokenKind::Number`]。
    pub fn number_value(&self, span: Span) -> f64 {
        parse_number(span.slice(self.src))
    }

    /// 字符串 token 的解码值（去引号、处理转义）。无转义时零拷贝借源码切片（常见情形，
    /// 含每个 import/export 说明符），含转义时才解码为拥有串——与 [`Self::identifier_text`] 同构。
    pub fn string_value(&self, span: Span) -> Cow<'a, str> {
        let raw = span.slice(self.src);
        // 去掉首尾引号。
        let inner = &raw[1..raw.len().saturating_sub(1)];
        if !inner.contains('\\') {
            Cow::Borrowed(inner)
        } else {
            Cow::Owned(decode_escapes(inner))
        }
    }

    /// 标识符/私有字段的文本（惰性；parser 需要驻留时调用）。无转义时零拷贝借源码切片，
    /// 含 `\u` 转义时解码为拥有串。私有字段保留前导 `#`。
    pub fn identifier_text(&self, span: Span) -> Cow<'a, str> {
        let raw = span.slice(self.src);
        if !raw.contains('\\') {
            Cow::Borrowed(raw)
        } else {
            // 私有字段的 `#` 不参与转义解码。
            let (prefix, body) = raw.split_at(usize::from(raw.starts_with('#')));
            let mut out = String::with_capacity(raw.len());
            out.push_str(prefix);
            out.push_str(&decode_ident_escapes(body));
            Cow::Owned(out)
        }
    }
}

/// 分类一个 **全 ASCII 无转义** 的标识符切片为关键字或普通标识符（不驻留）。
#[inline]
fn classify_ident(text: &str) -> TokenKind {
    // 所有关键字都是 2..=10 个小写 ASCII 字母（`of`..`instanceof`）。首字节非小写字母、
    // 或长度越界者一定不是关键字，直接判 Ident，跳过 `from_ident` 的 50 分支字符串 match。
    // 真实代码里大量标识符是大写开头的组件名 / 单字符 / 超长名，这条守卫省掉绝大多数 match。
    let bytes = text.as_bytes();
    if bytes.len() < 2 || bytes.len() > 10 || !bytes[0].is_ascii_lowercase() {
        return TokenKind::Ident;
    }
    match Keyword::from_ident(text) {
        Some(kw) => TokenKind::Keyword(kw),
        None => TokenKind::Ident,
    }
}

// ======================================================================
// 独立辅助
// ======================================================================

#[inline]
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 解码标识符中的 `\u` 转义（标识符只允许 `\u` 转义；扫描期已校验合法性）。
fn decode_ident_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        chars.next(); // 'u'
        let cp = if chars.peek() == Some(&'{') {
            chars.next();
            let mut v = 0u32;
            for c in chars.by_ref() {
                if c == '}' {
                    break;
                }
                if let Some(d) = c.to_digit(16) {
                    v = v.saturating_mul(16).saturating_add(d);
                }
            }
            v
        } else {
            let mut v = 0u32;
            for _ in 0..4 {
                if let Some(d) = chars.next().and_then(|c| c.to_digit(16)) {
                    v = v * 16 + d;
                }
            }
            v
        };
        if let Some(ch) = char::from_u32(cp) {
            out.push(ch);
        }
    }
    out
}

/// 解析数字字面量文本为 `f64`（十进制/十六进制/八进制/二进制，忽略分隔符与 BigInt 后缀）。
fn parse_number(text: &str) -> f64 {
    // 常见情形：无 `_` 分隔符，直接在借用切片上解析，省一次 String 分配 + char 重编码。
    if !text.as_bytes().contains(&b'_') {
        return parse_number_inner(text);
    }
    let t: String = text.chars().filter(|&c| c != '_').collect();
    parse_number_inner(&t)
}

fn parse_number_inner(t: &str) -> f64 {
    let t = t.strip_suffix('n').unwrap_or(t);
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u128::from_str_radix(hex, 16)
            .map(|v| v as f64)
            .unwrap_or(f64::NAN);
    }
    if let Some(oct) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        return u128::from_str_radix(oct, 8)
            .map(|v| v as f64)
            .unwrap_or(f64::NAN);
    }
    if let Some(bin) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        return u128::from_str_radix(bin, 2)
            .map(|v| v as f64)
            .unwrap_or(f64::NAN);
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// 解码字符串内部（不含引号）的转义序列。非法转义尽力保留原字符（错误已在扫描期报出）。
fn decode_escapes(inner: &str) -> String {
    if !inner.contains('\\') {
        return inner.to_owned();
    }
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            None => {}
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{0008}'),
            Some('f') => out.push('\u{000C}'),
            Some('v') => out.push('\u{000B}'),
            Some('0') if !chars.peek().is_some_and(|c| c.is_ascii_digit()) => out.push('\0'),
            Some('\n') => {}
            Some('\r') if chars.peek() == Some(&'\n') => {
                chars.next();
            }
            Some('\r') => {}
            Some('x') => {
                let hi = chars.next().and_then(|c| c.to_digit(16));
                let lo = chars.next().and_then(|c| c.to_digit(16));
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push(char::from((h * 16 + l) as u8));
                }
            }
            Some('u') => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    let mut v = 0u32;
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                        if let Some(d) = c.to_digit(16) {
                            v = v.saturating_mul(16).saturating_add(d);
                        }
                    }
                    if let Some(ch) = char::from_u32(v) {
                        out.push(ch);
                    }
                } else {
                    let mut v = 0u32;
                    for _ in 0..4 {
                        if let Some(d) = chars.next().and_then(|c| c.to_digit(16)) {
                            v = v * 16 + d;
                        }
                    }
                    if let Some(ch) = char::from_u32(v) {
                        out.push(ch);
                    }
                }
            }
            Some(other) => out.push(other),
        }
    }
    out
}
