//! 源文件与换行偏移表：热路径只用字节 [`Span`]，行列号在此按需二分还原（DESIGN §4.1）。

use crate::span::Span;

/// 1 基的行列位置（用于诊断显示）。`column` 以 **字符**（非字节）计。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineCol {
    pub line: u32,
    pub column: u32,
}

/// 一个源文件：名字 + 内容 + 预算的换行偏移表。
///
/// `line_starts[i]` 是第 `i`（0 基）行首字节偏移；`line_starts[0] == 0`。
pub struct SourceFile {
    name: String,
    src: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    pub fn new(name: impl Into<String>, src: impl Into<String>) -> SourceFile {
        let name = name.into();
        let src = src.into();
        let line_starts = compute_line_starts(&src);
        SourceFile {
            name,
            src,
            line_starts,
        }
    }

    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[inline]
    pub fn src(&self) -> &str {
        &self.src
    }

    #[inline]
    pub fn len(&self) -> u32 {
        self.src.len() as u32
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.src.is_empty()
    }

    /// 行数（至少 1）。
    #[inline]
    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    /// 字节偏移 → 0 基行号。二分查找，O(log n)。
    fn line_index(&self, offset: u32) -> usize {
        // partition_point 返回第一个 start > offset 的位置；其前一行即所在行。
        self.line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1)
    }

    /// 字节偏移 → 1 基行列（column 按字符计）。越界 offset 被夹取到文件末尾。
    pub fn location(&self, offset: u32) -> LineCol {
        let offset = offset.min(self.len());
        let line_idx = self.line_index(offset);
        let line_start = self.line_starts[line_idx];
        // column：行首到 offset 的字符数 + 1
        let prefix = &self.src[line_start as usize..offset as usize];
        let column = prefix.chars().count() as u32 + 1;
        LineCol {
            line: line_idx as u32 + 1,
            column,
        }
    }

    /// 字节偏移 → **0 基行 + 0 基 UTF-16 列**（Source Map V3 的坐标口径）。
    ///
    /// 与 [`SourceFile::location`] 的区别：后者是给人看的诊断坐标（1 基、按 Unicode 字符计），
    /// 而 sourcemap 规范要求 0 基且列按 **UTF-16 码元**计——BMP 外字符（emoji、部分 CJK 扩展）
    /// 占 2 个码元，若按字符计会与浏览器的解码结果差位。
    pub fn location0_utf16(&self, offset: u32) -> (u32, u32) {
        let offset = offset.min(self.len());
        let line_idx = self.line_index(offset);
        let line_start = self.line_starts[line_idx];
        // 行首到 offset 的 UTF-16 码元数（`len_utf16` 对 BMP 外字符返回 2）。
        let column: usize = self.src[line_start as usize..offset as usize]
            .chars()
            .map(char::len_utf16)
            .sum();
        (line_idx as u32, column as u32)
    }

    /// 取某一行（0 基）的文本，不含行尾换行符。
    pub fn line_text(&self, line_index: usize) -> &str {
        let start = self.line_starts[line_index] as usize;
        let end = if line_index + 1 < self.line_starts.len() {
            self.line_starts[line_index + 1] as usize
        } else {
            self.src.len()
        };
        self.src[start..end].trim_end_matches(['\n', '\r'])
    }

    /// 某一行（0 基）行首的字节偏移。
    #[inline]
    pub fn line_start(&self, line_index: usize) -> u32 {
        self.line_starts[line_index]
    }

    /// 取一个 span 覆盖的行区间（0 基，闭区间）。
    pub fn line_range(&self, span: Span) -> (usize, usize) {
        let lo = self.line_index(span.lo);
        let hi = self.line_index(span.hi.max(span.lo).min(self.len()));
        (lo, hi)
    }
}

fn compute_line_starts(src: &str) -> Vec<u32> {
    let mut starts = Vec::with_capacity(src.len() / 32 + 1);
    starts.push(0);
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i as u32 + 1);
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line() {
        let sf = SourceFile::new("a.js", "let x = 1;");
        assert_eq!(sf.line_count(), 1);
        assert_eq!(sf.location(0), LineCol { line: 1, column: 1 });
        assert_eq!(sf.location(4), LineCol { line: 1, column: 5 });
    }

    #[test]
    fn multi_line() {
        let sf = SourceFile::new("a.js", "aa\nbbb\ncccc");
        assert_eq!(sf.line_count(), 3);
        // 'b' 起始于偏移 3
        assert_eq!(sf.location(3), LineCol { line: 2, column: 1 });
        assert_eq!(sf.location(5), LineCol { line: 2, column: 3 });
        // 'c' 起始于偏移 7
        assert_eq!(sf.location(7), LineCol { line: 3, column: 1 });
        assert_eq!(sf.line_text(1), "bbb");
        assert_eq!(sf.line_text(2), "cccc");
    }

    #[test]
    fn unicode_column_counts_chars() {
        // "héllo"：é 是 2 字节；'l' 在字节 3，但列应为 3（字符计）
        let sf = SourceFile::new("a.js", "héllo");
        let l = sf.location(3); // 'l' 的字节偏移
        assert_eq!(l, LineCol { line: 1, column: 3 });
    }

    #[test]
    fn crlf_line_text_trimmed() {
        let sf = SourceFile::new("a.js", "x\r\ny");
        assert_eq!(sf.line_text(0), "x");
        assert_eq!(sf.line_text(1), "y");
    }

    #[test]
    fn location0_utf16_is_zero_based() {
        let sf = SourceFile::new("a.js", "aa\nbbb");
        // location 是 1 基，location0_utf16 是 0 基
        assert_eq!(sf.location(0), LineCol { line: 1, column: 1 });
        assert_eq!(sf.location0_utf16(0), (0, 0));
        // 'b' 起始于偏移 3 → 第 2 行（0 基 1）第 1 列（0 基 0）
        assert_eq!(sf.location0_utf16(3), (1, 0));
        assert_eq!(sf.location0_utf16(5), (1, 2));
    }

    #[test]
    fn location0_utf16_counts_code_units_not_chars() {
        // "é" 是 1 个字符 / 1 个 UTF-16 码元 / 2 字节
        let sf = SourceFile::new("a.js", "héllo");
        assert_eq!(sf.location0_utf16(3), (0, 2)); // 'l' 前有 h,é = 2 码元

        // "𝒳"（U+1D4B3）是 1 个字符 / **2 个 UTF-16 码元** / 4 字节。
        // location 按字符计得 1，location0_utf16 须得 2——这正是浏览器的口径。
        let sf2 = SourceFile::new("a.js", "𝒳x");
        assert_eq!(sf2.location(4).column, 2); // 1 基字符列
        assert_eq!(sf2.location0_utf16(4), (0, 2)); // 0 基 UTF-16 列
    }

    #[test]
    fn span_line_range() {
        let sf = SourceFile::new("a.js", "l1\nl2\nl3\nl4");
        // 覆盖 l2..l3
        let span = Span::new(3, 8);
        assert_eq!(sf.line_range(span), (1, 2));
    }
}
