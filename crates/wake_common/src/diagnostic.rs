//! 统一诊断结构：所有阶段产出 [`Diagnostic`]，CLI 端渲染成带源码上下文的彩色报错（DESIGN §4.1）。

use std::borrow::Cow;

use crate::span::Span;

/// 诊断级别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// 阻断性错误。
    Error,
    /// 警告。
    Warning,
    /// 提示信息。
    Note,
    /// 修复建议。
    Help,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
            Severity::Help => "help",
        }
    }

    /// ANSI 前景色（粗体）。
    pub(crate) fn ansi(self) -> &'static str {
        match self {
            Severity::Error => "\x1b[1;31m",   // 粗体红
            Severity::Warning => "\x1b[1;33m", // 粗体黄
            Severity::Note => "\x1b[1;36m",    // 粗体青
            Severity::Help => "\x1b[1;32m",    // 粗体绿
        }
    }
}

/// 一个标注：指向源码某区间的下划线 + 可选说明。
#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: Option<String>,
    /// 主标注用 `^` 下划线，次标注用 `-`。
    pub primary: bool,
}

impl Label {
    pub fn primary(span: Span, message: impl Into<String>) -> Label {
        Label {
            span,
            message: Some(message.into()),
            primary: true,
        }
    }

    pub fn secondary(span: Span, message: impl Into<String>) -> Label {
        Label {
            span,
            message: Some(message.into()),
            primary: false,
        }
    }
}

/// 一条诊断：级别 + 可选错误码 + 主消息 + 若干标注 + 尾注。
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Option<Cow<'static, str>>,
    pub message: String,
    pub labels: Vec<Label>,
    /// 附加的行尾说明（`= note: ...`）。
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn new(severity: Severity, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity,
            code: None,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn error(message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(Severity::Error, message)
    }

    pub fn warning(message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(Severity::Warning, message)
    }

    pub fn with_code(mut self, code: impl Into<Cow<'static, str>>) -> Diagnostic {
        self.code = Some(code.into());
        self
    }

    /// 加主标注（`^^^`）。
    pub fn with_primary(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.labels.push(Label::primary(span, message));
        self
    }

    /// 加次标注（`---`）。
    pub fn with_secondary(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.labels.push(Label::secondary(span, message));
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Diagnostic {
        self.notes.push(note.into());
        self
    }

    /// 主标注（第一个 primary，否则第一个 label）的位置，用于 `-->` 头部。
    pub fn primary_span(&self) -> Option<Span> {
        self.labels
            .iter()
            .find(|l| l.primary)
            .or_else(|| self.labels.first())
            .map(|l| l.span)
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}
