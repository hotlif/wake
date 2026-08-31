//! # wake_ecma_lexer — 字节级词法分析器
//!
//! DESIGN §4.3。全量 ES2022 token；parser 驱动 `/` 二义（正则 vs 除号）；ASI 换行标志；错误恢复。
//!
//! 两种用法：
//! - **parser 驱动**（P2）：逐 token 调 [`Lexer::next`]，由语法上下文传入 `regex_allowed`。
//! - **独立**（`wake tokenize` / 测试）：[`tokenize`] 用启发式自行判定 `regex_allowed`，一次出全部 token。

mod lexer;
mod token;
mod unicode;

pub use lexer::{Lexer, LexerCheckpoint};
pub use token::{Keyword, Token, TokenKind};

use wake_common::Diagnostic;

/// 独立词法分析：一次扫出全部 token（含结尾 `Eof`）与诊断。**不驻留标识符**（惰性，DESIGN §4.3）。
///
/// `/` 的正则/除号二义用启发式（看前一个有意义 token）判定——足够 `tokenize` 输出与测试；
/// 精确判定由 parser 在 P2 驱动。
pub fn tokenize(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lexer = Lexer::new(src);
    let mut tokens = Vec::new();
    let mut prev: Option<TokenKind> = None;
    loop {
        let regex_allowed = regex_allowed_after(prev);
        let tok = lexer.next(regex_allowed);
        let is_eof = tok.is_eof();
        prev = Some(tok.kind);
        tokens.push(tok);
        if is_eof {
            break;
        }
    }
    (tokens, lexer.take_diagnostics())
}

/// 启发式：给定前一个有意义 token，下一处 `/` 是否应作正则字面量起始。
///
/// 规则：正则出现在「表达式起始」处。前一个 token 若能 **结束一个表达式**（标识符/字面量/`)`/`]`/
/// 后缀 `++`/`--`/值关键字），则 `/` 是除号；否则（运算符/`(`/`,`/关键字如 `return` 等）是正则。
///
/// parser（P2）复用此启发式驱动 lexer；grammar 已知上下文的少数边角（如 `if (x) /re/`）后续再精修。
pub fn regex_allowed_after(prev: Option<TokenKind>) -> bool {
    use TokenKind::*;
    let Some(kind) = prev else { return true };
    match kind {
        // 能结束表达式 → 后面是除号。
        Ident | PrivateIdent | Number | BigInt | Str | Regex | TemplateNoSub | TemplateTail
        | RParen | RBracket | PlusPlus | MinusMinus => false,
        // 值关键字结束表达式。
        Keyword(k) => !matches!(
            k,
            self::Keyword::This
                | self::Keyword::Super
                | self::Keyword::True
                | self::Keyword::False
                | self::Keyword::Null
        ),
        // 其余（运算符、`(`、`{`、`,`、`;`、`=>`、`}` 等）→ 表达式起始 → 正则。
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let (toks, diags) = tokenize(src);
        assert!(
            diags.is_empty(),
            "unexpected diagnostics for {src:?}: {diags:?}"
        );
        toks.into_iter()
            .map(|t| t.kind)
            .filter(|k| !matches!(k, TokenKind::Eof))
            .collect()
    }

    #[test]
    fn punctuators() {
        use TokenKind::*;
        assert_eq!(kinds("=>"), vec![Arrow]);
        assert_eq!(kinds(">>>="), vec![UshrEq]);
        assert_eq!(kinds("??="), vec![QuestionQuestionEq]);
        assert_eq!(kinds("...x").len(), 2);
        assert_eq!(kinds("a ** b"), vec![Ident, StarStar, Ident]);
    }

    #[test]
    fn optional_chaining_vs_number() {
        use TokenKind::*;
        let ks = kinds("a?.b");
        assert_eq!(ks[1], QuestionDot);
        // `?.5` 不是可选链，而是 `?` 后跟数字 `.5`。
        let ks2 = kinds("x?.5:y");
        assert_eq!(ks2[1], Question);
        assert_eq!(ks2[2], Number);
    }

    #[test]
    fn keywords_and_idents() {
        use TokenKind::*;
        let ks = kinds("function foo() {}");
        assert_eq!(ks[0], Keyword(crate::Keyword::Function));
        assert_eq!(ks[1], Ident);
        assert_eq!(kinds("async")[0], Keyword(crate::Keyword::Async));
    }

    #[test]
    fn numbers() {
        use TokenKind::*;
        assert_eq!(kinds("0xFF"), vec![Number]);
        assert_eq!(kinds("0b1010"), vec![Number]);
        assert_eq!(kinds("0o777"), vec![Number]);
        assert_eq!(kinds("1_000_000"), vec![Number]);
        assert_eq!(kinds("3.14e-2"), vec![Number]);
        assert_eq!(kinds("42n"), vec![BigInt]);
        assert_eq!(kinds(".5"), vec![Number]);
    }

    #[test]
    fn radix_prefix_requires_at_least_one_digit() {
        for source in ["0x", "0X", "0o", "0O", "0b", "0B"] {
            let (tokens, diagnostics) = tokenize(source);
            assert!(
                diagnostics.iter().any(|diagnostic| diagnostic.is_error()),
                "a bare radix prefix is not a JavaScript numeric literal: {source:?}; {tokens:?}"
            );
        }
    }

    #[test]
    fn strings_and_templates() {
        use TokenKind::*;
        assert_eq!(kinds(r#""hello""#), vec![Str]);
        assert_eq!(kinds("'a\\n b'"), vec![Str]);
        assert_eq!(kinds("`nosub`"), vec![TemplateNoSub]);
        assert_eq!(kinds("`a${b}c`"), vec![TemplateHead, Ident, TemplateTail]);
        // 嵌套模板：外层 head，内层 head。
        let ks = kinds("`${`x${y}`}`");
        assert_eq!(ks[0], TemplateHead);
        assert_eq!(ks[1], TemplateHead);
    }

    #[test]
    fn carriage_return_handling() {
        use TokenKind::*;
        // CRLF 行尾：\r 是 token 间 trivia，不进字符串；第二个字符串 newline_before=真。
        let (toks, diags) = tokenize("\"a\"\r\n\"b\"");
        assert!(diags.is_empty(), "{diags:?}");
        let strs: Vec<_> = toks.iter().filter(|t| t.kind == Str).collect();
        assert_eq!(strs.len(), 2);
        assert!(strs[1].newline_before);
        // 字符串内未转义的 CR 仍须报错（find_string_stop 的 \r 检测经边界化后不能漏）。
        let (_t, diags2) = tokenize("\"ab\rcd\"");
        assert!(!diags2.is_empty(), "字符串内未转义 CR 应报错");
        // 纯 LF（无 \r）源码里的字符串仍正确闭合（此前扫到 EOF 的退化路径的正确性守卫）。
        let (toks3, diags3) = tokenize("const s = \"hello\";\nconst t = \"world\";\n");
        assert!(diags3.is_empty(), "{diags3:?}");
        assert_eq!(toks3.iter().filter(|t| t.kind == Str).count(), 2);
    }

    #[test]
    fn regex_vs_division() {
        use TokenKind::*;
        assert_eq!(kinds("x = /ab+c/g").last(), Some(&Regex));
        let ks = kinds("a / b");
        assert_eq!(ks[1], Slash);
        // 正则字符类中的 `/` 不结束正则。
        assert_eq!(kinds("/[/]/").first(), Some(&Regex));
    }

    #[test]
    fn private_ident() {
        let (toks, diags) = tokenize("this.#x");
        assert!(diags.is_empty());
        assert_eq!(toks[2].kind, TokenKind::PrivateIdent);
    }

    #[test]
    fn newline_before_flag() {
        let (toks, _) = tokenize("a\nb");
        assert!(!toks[0].newline_before);
        assert!(toks[1].newline_before);
    }

    #[test]
    fn unicode_identifier() {
        let (toks, diags) = tokenize("let 变量 = 1");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(toks[1].kind, TokenKind::Ident);
    }

    #[test]
    fn comments_and_whitespace() {
        let (toks, diags) = tokenize("a // line\n/* block */ b");
        assert!(diags.is_empty());
        assert_eq!(toks.len(), 3); // a, b, eof
        assert!(toks[1].newline_before);
    }

    #[test]
    fn lazy_value_and_identifier_text() {
        // 标识符文本惰性取（零拷贝借切片）。
        let mut lex = Lexer::new("héllo + x");
        let t = lex.next(true);
        assert_eq!(lex.identifier_text(t.span), "héllo");

        let mut lex2 = Lexer::new(r#""a\nA""#);
        let s = lex2.next(true);
        assert_eq!(lex2.string_value(s.span), "a\nA");

        let mut lex3 = Lexer::new("0xFF");
        let n = lex3.next(true);
        assert_eq!(lex3.number_value(n.span), 255.0);
    }

    #[test]
    fn escaped_identifier_text_decodes() {
        // `\u{68}i` 解码为 "hi"，且因含转义 → 普通 Ident（非关键字 if）。
        let mut lex = Lexer::new(r"\u{69}f");
        let t = lex.next(true);
        assert_eq!(t.kind, TokenKind::Ident);
        assert_eq!(lex.identifier_text(t.span), "if");
    }

    #[test]
    fn error_recovery_continues() {
        // 非法字符后仍继续扫描。
        let (toks, diags) = tokenize("\0 a");
        assert!(!diags.is_empty());
        assert!(toks.iter().any(|t| matches!(t.kind, TokenKind::Ident)));
    }
}
