//! Token 定义（DESIGN §4.3）。
//!
//! Token **不携带字符串、也不驻留标识符**，只携带 [`Span`] 与 [`TokenKind`]（DESIGN §4.3：
//! 「需要值时——标识符驻留、字符串转义解码——惰性进行」）。标识符文本/数字值/字符串值都经
//! [`crate::Lexer`] 的惰性方法按需取，interning 交给 parser（P2）。

use wake_common::Span;

/// 一个词法记号：种类 + 源码区间 + 「其前是否有换行」（ASI 用，DESIGN §4.3）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// 本 token 与上一个 token 之间是否跨了换行（含被换行分隔的注释）。ASI 判断留给 parser。
    pub newline_before: bool,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span, newline_before: bool) -> Token {
        Token {
            kind,
            span,
            newline_before,
        }
    }

    #[inline]
    pub fn is_eof(&self) -> bool {
        matches!(self.kind, TokenKind::Eof)
    }
}

/// 记号种类。保持 `Copy` 且紧凑；标识符/私有字段内嵌一个 `Atom`（u32）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// 文件结束。
    Eof,
    /// 词法错误（错误恢复用，DESIGN §4.3）。不中断后续扫描。
    Error,

    // —— 标识符与关键字 ——
    /// 普通标识符。文本经 [`crate::Lexer::identifier_text`] 惰性取；driver 需要时才驻留。
    Ident,
    /// 私有字段名 `#x`。
    PrivateIdent,
    /// 保留字/关键字（见 [`Keyword`]）。
    Keyword(Keyword),

    // —— 字面量（值惰性解码）——
    /// 数字字面量（十进制/十六进制/八进制/二进制/浮点/指数/分隔符）。
    Number,
    /// BigInt 字面量（以 `n` 结尾）。
    BigInt,
    /// 字符串字面量（单/双引号）。
    Str,
    /// JSX 子节点文本（`>` 与 `<`/`{` 之间的原始文本，parser 驱动，DESIGN §4.3）。
    JsxText,
    /// 无插值模板 `` `...` ``。
    TemplateNoSub,
    /// 模板头 `` `...${ ``。
    TemplateHead,
    /// 模板中段 `` }...${ ``。
    TemplateMiddle,
    /// 模板尾 `` }...` ``。
    TemplateTail,
    /// 正则字面量 `/.../flags`（parser 驱动，仅在 regex 允许的上下文产出）。
    Regex,

    // —— 标点/运算符 ——
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `;`
    Semicolon,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `...`
    DotDotDot,
    /// `?`
    Question,
    /// `?.`
    QuestionDot,
    /// `??`
    QuestionQuestion,
    /// `??=`
    QuestionQuestionEq,
    /// `:`
    Colon,
    /// `=>`
    Arrow,
    /// `@`（装饰器）
    At,

    /// `=`
    Eq,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `**`
    StarStar,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `+=`
    PlusEq,
    /// `-=`
    MinusEq,
    /// `*=`
    StarEq,
    /// `**=`
    StarStarEq,
    /// `/=`
    SlashEq,
    /// `%=`
    PercentEq,

    /// `&`
    Amp,
    /// `|`
    Pipe,
    /// `^`
    Caret,
    /// `~`
    Tilde,
    /// `!`
    Bang,
    /// `&&`
    AmpAmp,
    /// `||`
    PipePipe,
    /// `&=`
    AmpEq,
    /// `|=`
    PipeEq,
    /// `^=`
    CaretEq,
    /// `&&=`
    AmpAmpEq,
    /// `||=`
    PipePipeEq,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `>>>`
    Ushr,
    /// `<<=`
    ShlEq,
    /// `>>=`
    ShrEq,
    /// `>>>=`
    UshrEq,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    LtEq,
    /// `>=`
    GtEq,
    /// `==`
    EqEq,
    /// `!=`
    NotEq,
    /// `===`
    EqEqEq,
    /// `!==`
    NotEqEq,
    /// `++`
    PlusPlus,
    /// `--`
    MinusMinus,
}

impl TokenKind {
    /// 标点/运算符的固定文本表示（`None` 表示非固定文本的种类，如标识符/字面量）。
    pub fn punct_str(self) -> Option<&'static str> {
        use TokenKind::*;
        Some(match self {
            LParen => "(",
            RParen => ")",
            LBrace => "{",
            RBrace => "}",
            LBracket => "[",
            RBracket => "]",
            Semicolon => ";",
            Comma => ",",
            Dot => ".",
            DotDotDot => "...",
            Question => "?",
            QuestionDot => "?.",
            QuestionQuestion => "??",
            QuestionQuestionEq => "??=",
            Colon => ":",
            Arrow => "=>",
            At => "@",
            Eq => "=",
            Plus => "+",
            Minus => "-",
            Star => "*",
            StarStar => "**",
            Slash => "/",
            Percent => "%",
            PlusEq => "+=",
            MinusEq => "-=",
            StarEq => "*=",
            StarStarEq => "**=",
            SlashEq => "/=",
            PercentEq => "%=",
            Amp => "&",
            Pipe => "|",
            Caret => "^",
            Tilde => "~",
            Bang => "!",
            AmpAmp => "&&",
            PipePipe => "||",
            AmpEq => "&=",
            PipeEq => "|=",
            CaretEq => "^=",
            AmpAmpEq => "&&=",
            PipePipeEq => "||=",
            Shl => "<<",
            Shr => ">>",
            Ushr => ">>>",
            ShlEq => "<<=",
            ShrEq => ">>=",
            UshrEq => ">>>=",
            Lt => "<",
            Gt => ">",
            LtEq => "<=",
            GtEq => ">=",
            EqEq => "==",
            NotEq => "!=",
            EqEqEq => "===",
            NotEqEq => "!==",
            PlusPlus => "++",
            MinusMinus => "--",
            _ => return None,
        })
    }

    /// 简短描述（诊断 / tokenize 输出用）。
    pub fn describe(self) -> &'static str {
        use TokenKind::*;
        match self {
            Eof => "<eof>",
            Error => "<error>",
            Ident => "identifier",
            PrivateIdent => "private-identifier",
            Keyword(k) => k.as_str(),
            Number => "number",
            BigInt => "bigint",
            Str => "string",
            JsxText => "jsx-text",
            TemplateNoSub => "template",
            TemplateHead => "template-head",
            TemplateMiddle => "template-middle",
            TemplateTail => "template-tail",
            Regex => "regex",
            other => other.punct_str().unwrap_or("<punct>"),
        }
    }
}

/// 关键字（保留字 + 常见上下文关键字）。上下文关键字（`async`/`of`/`as`/…）是否「关键字」由
/// parser 按语法上下文裁决——lexer 只做识别，[`Keyword::is_reserved`] 标注严格保留字。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keyword {
    // 严格保留字
    Break,
    Case,
    Catch,
    Class,
    Const,
    Continue,
    Debugger,
    Default,
    Delete,
    Do,
    Else,
    Enum,
    Export,
    Extends,
    False,
    Finally,
    For,
    Function,
    If,
    Import,
    In,
    Instanceof,
    New,
    Null,
    Return,
    Super,
    Switch,
    This,
    Throw,
    True,
    Try,
    Typeof,
    Var,
    Void,
    While,
    With,
    // 上下文/严格模式保留字
    Await,
    Yield,
    Let,
    Static,
    Async,
    Of,
    As,
    From,
    Get,
    Set,
    Implements,
    Interface,
    Package,
    Private,
    Protected,
    Public,
}

impl Keyword {
    /// 从标识符文本识别关键字。非关键字返回 `None`（→ 普通 `Ident`）。
    pub fn from_ident(s: &str) -> Option<Keyword> {
        use Keyword::*;
        Some(match s {
            "break" => Break,
            "case" => Case,
            "catch" => Catch,
            "class" => Class,
            "const" => Const,
            "continue" => Continue,
            "debugger" => Debugger,
            "default" => Default,
            "delete" => Delete,
            "do" => Do,
            "else" => Else,
            "enum" => Enum,
            "export" => Export,
            "extends" => Extends,
            "false" => False,
            "finally" => Finally,
            "for" => For,
            "function" => Function,
            "if" => If,
            "import" => Import,
            "in" => In,
            "instanceof" => Instanceof,
            "new" => New,
            "null" => Null,
            "return" => Return,
            "super" => Super,
            "switch" => Switch,
            "this" => This,
            "throw" => Throw,
            "true" => True,
            "try" => Try,
            "typeof" => Typeof,
            "var" => Var,
            "void" => Void,
            "while" => While,
            "with" => With,
            "await" => Await,
            "yield" => Yield,
            "let" => Let,
            "static" => Static,
            "async" => Async,
            "of" => Of,
            "as" => As,
            "from" => From,
            "get" => Get,
            "set" => Set,
            "implements" => Implements,
            "interface" => Interface,
            "package" => Package,
            "private" => Private,
            "protected" => Protected,
            "public" => Public,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        use Keyword::*;
        match self {
            Break => "break",
            Case => "case",
            Catch => "catch",
            Class => "class",
            Const => "const",
            Continue => "continue",
            Debugger => "debugger",
            Default => "default",
            Delete => "delete",
            Do => "do",
            Else => "else",
            Enum => "enum",
            Export => "export",
            Extends => "extends",
            False => "false",
            Finally => "finally",
            For => "for",
            Function => "function",
            If => "if",
            Import => "import",
            In => "in",
            Instanceof => "instanceof",
            New => "new",
            Null => "null",
            Return => "return",
            Super => "super",
            Switch => "switch",
            This => "this",
            Throw => "throw",
            True => "true",
            Try => "try",
            Typeof => "typeof",
            Var => "var",
            Void => "void",
            While => "while",
            With => "with",
            Await => "await",
            Yield => "yield",
            Let => "let",
            Static => "static",
            Async => "async",
            Of => "of",
            As => "as",
            From => "from",
            Get => "get",
            Set => "set",
            Implements => "implements",
            Interface => "interface",
            Package => "package",
            Private => "private",
            Protected => "protected",
            Public => "public",
        }
    }

    /// 是否为 ECMAScript **严格保留字**（在任何上下文都不能作标识符）。
    /// 上下文关键字（`async`/`of`/`as`/`from`/`get`/`set`/`let`/`static`/`yield`/`await` 等）返回 false。
    pub fn is_reserved(self) -> bool {
        use Keyword::*;
        matches!(
            self,
            Break
                | Case
                | Catch
                | Class
                | Const
                | Continue
                | Debugger
                | Default
                | Delete
                | Do
                | Else
                | Enum
                | Export
                | Extends
                | False
                | Finally
                | For
                | Function
                | If
                | Import
                | In
                | Instanceof
                | New
                | Null
                | Return
                | Super
                | Switch
                | This
                | Throw
                | True
                | Try
                | Typeof
                | Var
                | Void
                | While
                | With
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punct_roundtrip() {
        assert_eq!(TokenKind::Arrow.punct_str(), Some("=>"));
        assert_eq!(TokenKind::UshrEq.punct_str(), Some(">>>="));
        assert_eq!(TokenKind::Number.punct_str(), None);
    }

    #[test]
    fn keyword_classification() {
        assert_eq!(Keyword::from_ident("function"), Some(Keyword::Function));
        assert_eq!(Keyword::from_ident("async"), Some(Keyword::Async));
        assert_eq!(Keyword::from_ident("notakeyword"), None);
        assert!(Keyword::Function.is_reserved());
        assert!(!Keyword::Async.is_reserved());
        assert!(!Keyword::Of.is_reserved());
    }

    #[test]
    fn token_kind_is_small() {
        // 不再内嵌 Atom：最大 payload 是 Keyword(u8)，TokenKind 极紧凑。
        assert!(std::mem::size_of::<TokenKind>() <= 4);
    }
}
