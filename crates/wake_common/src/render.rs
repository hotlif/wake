//! 诊断的终端渲染：对标 rustc 报错体验（带源码上下文 + 彩色下划线），DESIGN §4.1。

use std::fmt::Write as _;

use crate::diagnostic::{Diagnostic, Severity};
use crate::source::SourceFile;

const RESET: &str = "\x1b[0m";
const BLUE: &str = "\x1b[1;34m"; // 行号 / 竖线 / `-->`

/// 渲染样式。
#[derive(Clone, Copy, Debug)]
pub struct RenderStyle {
    pub color: bool,
}

impl RenderStyle {
    pub fn plain() -> RenderStyle {
        RenderStyle { color: false }
    }

    pub fn colored() -> RenderStyle {
        RenderStyle { color: true }
    }
}

struct Painter {
    color: bool,
}

impl Painter {
    fn paint(&self, code: &str, s: &str, out: &mut String) {
        if self.color {
            out.push_str(code);
            out.push_str(s);
            out.push_str(RESET);
        } else {
            out.push_str(s);
        }
    }
}

/// 把一条诊断渲染为字符串。`source` 需与诊断 span 同源。
///
/// 输出形如：
/// ```text
/// error[WAKE0001]: unexpected token
///  --> a.js:3:5
///   |
/// 3 |     foo(bar
///   |         ^^^ expected `)`
///   |
///   = note: ...
/// ```
pub fn render(diag: &Diagnostic, source: &SourceFile, style: RenderStyle) -> String {
    let p = Painter { color: style.color };
    let mut out = String::new();

    // —— 头部：severity[code]: message ——
    let mut header = String::new();
    header.push_str(diag.severity.as_str());
    if let Some(code) = &diag.code {
        let _ = write!(header, "[{code}]");
    }
    p.paint(diag.severity.ansi(), &header, &mut out);
    p.paint("\x1b[1m", &format!(": {}", diag.message), &mut out); // 粗体主消息
    out.push('\n');

    // 计算 gutter 宽度（最大行号的十进制位数）。
    let max_line = diag
        .labels
        .iter()
        .map(|l| source.line_range(l.span).1 as u32 + 1)
        .max()
        .unwrap_or(1);
    let gutter_w = decimal_width(max_line);
    let pad = " ".repeat(gutter_w);

    // —— 位置行：--> name:line:col ——
    if let Some(span) = diag.primary_span() {
        let loc = source.location(span.lo);
        p.paint(BLUE, &format!("{pad}--> "), &mut out);
        let _ = writeln!(out, "{}:{}:{}", source.name(), loc.line, loc.column);
    }

    // 空竖线行
    let bar = |out: &mut String| {
        p.paint(BLUE, &format!("{pad} |"), out);
        out.push('\n');
    };
    bar(&mut out);

    // —— 各标注块（按 span 起点排序）——
    let mut labels: Vec<&crate::diagnostic::Label> = diag.labels.iter().collect();
    labels.sort_by_key(|l| (l.span.lo, l.span.hi));

    for label in labels {
        render_label(&p, &mut out, source, label, gutter_w, diag.severity);
    }

    // 收尾空竖线 + notes
    bar(&mut out);
    for note in &diag.notes {
        p.paint(BLUE, &format!("{pad} = "), &mut out);
        p.paint("\x1b[1m", "note: ", &mut out);
        let _ = writeln!(out, "{note}");
    }

    out
}

fn render_label(
    p: &Painter,
    out: &mut String,
    source: &SourceFile,
    label: &crate::diagnostic::Label,
    gutter_w: usize,
    severity: Severity,
) {
    let (line_lo, line_hi) = source.line_range(label.span);
    // 第一版：只对单行 span 画精确下划线；跨行 span 退化为在起始行整行末尾标注。
    let line_idx = line_lo;
    let line_no = (line_idx + 1) as u32;
    let text = source.line_text(line_idx);
    let line_start = source.line_start(line_idx);

    // 行号 + 源码
    let ln = format!("{line_no:>gutter_w$}");
    p.paint(BLUE, &format!("{ln} | "), out);
    out.push_str(text);
    out.push('\n');

    // 下划线行
    let pad = " ".repeat(gutter_w);
    p.paint(BLUE, &format!("{pad} | "), out);

    // 列偏移（字符计）：从行首到 span.lo
    let col = char_col(source.src(), line_start, label.span.lo);
    // 下划线长度：单行内为 span 覆盖字符数，至少 1；跨行则到本行末尾。
    let underline_end = if line_hi > line_lo {
        line_start + text.len() as u32
    } else {
        label.span.hi
    };
    let width = char_col(
        source.src(),
        label.span.lo.max(line_start),
        underline_end.max(label.span.lo + 1),
    )
    .max(1);

    out.push_str(&" ".repeat(col));
    let marker = if label.primary { "^" } else { "-" };
    let color = if label.primary { severity.ansi() } else { BLUE };
    let underline = marker.repeat(width);
    if let Some(msg) = &label.message {
        p.paint(color, &format!("{underline} {msg}"), out);
    } else {
        p.paint(color, &underline, out);
    }
    out.push('\n');
}

/// 从 `from` 到 `to`（字节偏移）之间的字符数。
fn char_col(src: &str, from: u32, to: u32) -> usize {
    let (from, to) = (from as usize, to.min(src.len() as u32) as usize);
    if from >= to {
        return 0;
    }
    src[from..to].chars().count()
}

fn decimal_width(mut n: u32) -> usize {
    let mut w = 1;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Span;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // 跳过到 'm'
                for c2 in chars.by_ref() {
                    if c2 == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn renders_basic_error() {
        let src = "let a = 1;\nlet b = 2\nfoo(bar\n";
        let sf = SourceFile::new("a.js", src);
        // 定位 "bar"（第 3 行）—— 在 "foo(" 之后
        let bar_start = src.find("bar").unwrap() as u32;
        let diag = Diagnostic::error("unexpected token")
            .with_code("WAKE0001")
            .with_primary(Span::new(bar_start, bar_start + 3), "expected `)`");

        let text = render(&diag, &sf, RenderStyle::plain());
        assert!(
            text.contains("error[WAKE0001]: unexpected token"),
            "\n{text}"
        );
        assert!(text.contains("--> a.js:3:5"), "\n{text}");
        assert!(text.contains("foo(bar"), "\n{text}");
        assert!(text.contains("^^^ expected `)`"), "\n{text}");
    }

    #[test]
    fn colored_contains_ansi_but_strips_to_plain_shape() {
        let sf = SourceFile::new("a.js", "let x = ;");
        let diag = Diagnostic::error("expected expression").with_primary(Span::new(8, 9), "here");
        let colored = render(&diag, &sf, RenderStyle::colored());
        assert!(colored.contains('\x1b'));
        let plain = render(&diag, &sf, RenderStyle::plain());
        assert_eq!(strip_ansi(&colored), plain);
    }

    #[test]
    fn secondary_label_uses_dashes() {
        let sf = SourceFile::new("a.js", "abc def");
        let diag = Diagnostic::warning("something").with_secondary(Span::new(0, 3), "defined here");
        let text = render(&diag, &sf, RenderStyle::plain());
        assert!(text.contains("--- defined here"), "\n{text}");
    }
}
