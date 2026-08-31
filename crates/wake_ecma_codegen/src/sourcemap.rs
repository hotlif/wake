//! # Source Map V3 —— Base64 VLQ 编码与序列化
//!
//! DESIGN §4.6 / WAKE-COMPATIBILITY §M4d。codegen 在发射时记录「产物位置 ↔ 源码字节偏移」
//! （[`Mapping`]），bundler 拼接 bundle 时按行偏移平移合并，最终由 [`SourceMap`] 序列化为
//! 规范的 V3 JSON。
//!
//! ## 坐标口径
//! - 产物侧行列：**0 基**，列按 **UTF-16 码元**（与浏览器一致）。
//! - 源码侧：codegen 只记录**字节偏移**（`Span::lo`），行列换算推迟到序列化阶段由
//!   [`wake_common::SourceFile::location0_utf16`] 完成——热路径不算行列（DESIGN §4.1）。

use std::fmt::Write as _;

/// 一条映射记录：产物某位置 ← 源码某字节偏移。
///
/// 源侧保持字节偏移而非行列，是为了让 codegen 热路径零换算；行列在序列化时批量还原。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    /// 产物行（0 基）。
    pub gen_line: u32,
    /// 产物列（0 基，UTF-16 码元）。
    pub gen_col: u32,
    /// 源文件下标（对应 [`SourceMap::sources`]）。
    pub src_index: u32,
    /// 源码字节偏移（`Span::lo`）。
    pub src_offset: u32,
    /// 可选原标识符名下标（对应模块或最终 [`SourceMap::names`]）。
    /// Source Map V3 仅在该值存在时为当前段编码第五个 VLQ 字段。
    pub name_index: Option<u32>,
    /// `true` 表示 Source Map V3 的单字段段：该位置起生成代码不对应任何源位置。
    ///
    /// 这类段用于合成括号、链接器包装器和其它生成标点。源侧字段在这种段上被忽略；保留
    /// 固定宽度结构是为了让热路径与持久缓存继续使用紧凑的 `Copy` 记录。
    pub is_unmapped: bool,
}

impl Mapping {
    /// 创建一个只携带生成列的 Source Map V3 unmapped segment。
    pub const fn unmapped(gen_line: u32, gen_col: u32) -> Self {
        Self {
            gen_line,
            gen_col,
            src_index: 0,
            src_offset: 0,
            name_index: None,
            is_unmapped: true,
        }
    }
}

/// 一个模块 codegen 产出的映射集合（模块内局部坐标，尚未平移到 bundle）。
#[derive(Clone, Debug, Default)]
pub struct ModuleMappings {
    /// 按产物位置递增顺序记录的映射（`src_index` 此时恒为 0，合并时由 bundler 重写）。
    pub mappings: Vec<Mapping>,
    /// 当前模块映射使用的原标识符名表；[`Mapping::name_index`] 指向这里。
    pub names: Vec<String>,
}

impl ModuleMappings {
    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }

    pub fn len(&self) -> usize {
        self.mappings.len()
    }
}

/// 一个待序列化的 Source Map V3。
#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    /// 源文件名（与 `sources_content` 同序同长）。
    pub sources: Vec<String>,
    /// 各源文件的完整文本（`null` 用空串表示缺失，见 [`SourceMap::to_json`]）。
    pub sources_content: Vec<Option<String>>,
    /// 全部映射（产物坐标须**按行、列递增**排序后才能正确编码）。
    pub mappings: Vec<Mapping>,
    /// 原标识符名表；命名映射的第五字段指向这里。
    pub names: Vec<String>,
    /// 可选的 `file` 字段（产物文件名）。
    pub file: Option<String>,
}

impl SourceMap {
    pub fn new() -> SourceMap {
        SourceMap::default()
    }

    /// 登记一个源文件，返回其下标（供 [`Mapping::src_index`]）。
    pub fn add_source(&mut self, name: impl Into<String>, content: Option<String>) -> u32 {
        self.sources.push(name.into());
        self.sources_content.push(content);
        (self.sources.len() - 1) as u32
    }

    /// Deterministically intern an original identifier name in first-use order.
    pub fn add_name(&mut self, name: impl Into<String>) -> u32 {
        let name = name.into();
        if let Some(index) = self.names.iter().position(|current| current == &name) {
            return index as u32;
        }
        self.names.push(name);
        (self.names.len() - 1) as u32
    }

    /// 序列化为 Source Map V3 JSON。
    ///
    /// `resolve`：把 `(src_index, src_offset)` 还原为 `(行, 列)`（0 基、UTF-16）。由调用方
    /// 持有各源文件的换行表（[`wake_common::SourceFile`]）后提供，本 crate 属编译核心、
    /// 不直接持有源文本。
    pub fn to_json(&self, mut resolve: impl FnMut(u32, u32) -> (u32, u32)) -> String {
        // mappings 必须按产物位置有序；调用方通常已有序，这里做一次稳定排序兜底。
        let mut sorted = self.mappings.clone();
        sorted.sort_by_key(|m| (m.gen_line, m.gen_col));
        for mapping in &mut sorted {
            if !mapping.is_unmapped
                && mapping
                    .name_index
                    .is_some_and(|index| index as usize >= self.names.len())
            {
                debug_assert!(false, "source-map name index is out of bounds");
                mapping.name_index = None;
            }
        }

        let mut out = String::with_capacity(sorted.len() * 8 + 256);
        out.push_str("{\"version\":3,");
        if let Some(file) = &self.file {
            out.push_str("\"file\":");
            json_string(&mut out, file);
            out.push(',');
        }
        out.push_str("\"sources\":[");
        for (i, s) in self.sources.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            json_string(&mut out, s);
        }
        out.push_str("],\"sourcesContent\":[");
        for (i, c) in self.sources_content.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            match c {
                Some(text) => json_string(&mut out, text),
                None => out.push_str("null"),
            }
        }
        out.push_str("],\"names\":[");
        for (i, name) in self.names.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            json_string(&mut out, name);
        }
        out.push_str("],\"mappings\":\"");
        out.push_str(&encode_mappings(&sorted, &mut resolve));
        out.push_str("\"}");
        out
    }
}

/// 把有序映射编码为 V3 的 `mappings` 字符串。
///
/// 分号分隔产物行、逗号分隔段；unmapped 段只有 1 个 VLQ 字段（产物列），普通段有
/// 4 个字段（产物列、源下标、源行、源列），命名段追加第 5 个原名下标字段。除产物列在换行时重置外，
/// 其余字段**跨行连续**做差分；unmapped 段不改变任何源侧差分基准。
fn encode_mappings(sorted: &[Mapping], resolve: &mut impl FnMut(u32, u32) -> (u32, u32)) -> String {
    let mut out = String::new();
    // 差分基准（规范：产物列每行归零，其余三者全局连续）。
    let mut prev_gen_col: i64 = 0;
    let mut prev_src_index: i64 = 0;
    let mut prev_src_line: i64 = 0;
    let mut prev_src_col: i64 = 0;
    let mut prev_name_index: i64 = 0;
    let mut cur_line: u32 = 0;
    let mut first_in_line = true;

    for m in sorted {
        // 补足行分隔符；跨行时产物列基准归零。
        while cur_line < m.gen_line {
            out.push(';');
            cur_line += 1;
            prev_gen_col = 0;
            first_in_line = true;
        }
        if !first_in_line {
            out.push(',');
        }
        first_in_line = false;

        vlq(&mut out, m.gen_col as i64 - prev_gen_col);
        prev_gen_col = m.gen_col as i64;
        if m.is_unmapped {
            continue;
        }

        let (src_line, src_col) = resolve(m.src_index, m.src_offset);
        vlq(&mut out, m.src_index as i64 - prev_src_index);
        vlq(&mut out, src_line as i64 - prev_src_line);
        vlq(&mut out, src_col as i64 - prev_src_col);
        if let Some(name_index) = m.name_index {
            vlq(&mut out, name_index as i64 - prev_name_index);
            prev_name_index = name_index as i64;
        }
        prev_src_index = m.src_index as i64;
        prev_src_line = src_line as i64;
        prev_src_col = src_col as i64;
    }
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 追加一个 Base64 VLQ 编码的有符号整数。
///
/// 编码：符号位放最低位（负数置 1），其余为绝对值；每 5 bit 一组，高位续接标志 `0x20`。
pub fn vlq(out: &mut String, value: i64) {
    // 符号位入最低位。用 i64 承接，避免 i32::MIN 取反溢出。
    let mut v = if value < 0 {
        ((-value) as u64) << 1 | 1
    } else {
        (value as u64) << 1
    };
    loop {
        let mut digit = (v & 0x1f) as usize;
        v >>= 5;
        if v > 0 {
            digit |= 0x20; // 还有后续组
        }
        out.push(B64[digit] as char);
        if v == 0 {
            break;
        }
    }
}

/// 追加一个 JSON 字符串字面量（含转义）。
fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // JSON 要求转义 U+2028/2029 之外的控制字符；行分隔符在 JS 上下文亦须转义。
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(n: i64) -> String {
        let mut s = String::new();
        vlq(&mut s, n);
        s
    }

    #[test]
    fn vlq_matches_spec_vectors() {
        // 对照 Source Map V3 规范/mozilla source-map 的已知向量。
        assert_eq!(v(0), "A");
        assert_eq!(v(1), "C");
        assert_eq!(v(-1), "D");
        assert_eq!(v(2), "E");
        assert_eq!(v(-2), "F");
        assert_eq!(v(16), "gB");
        assert_eq!(v(-16), "hB");
        assert_eq!(v(15), "e");
        assert_eq!(v(123), "2H");
        assert_eq!(v(-123), "3H");
    }

    #[test]
    fn vlq_handles_large_values() {
        // 大值需多组续接；确保不 panic 且可解回（长度合理）。
        let s = v(1 << 30);
        assert!(s.len() >= 6, "大整数应编码为多组: {s}");
        // i32::MIN 级别的负数不得溢出（内部用 i64 承接）。
        let s2 = v(-(1i64 << 31));
        assert!(!s2.is_empty());
    }

    #[test]
    fn mappings_encode_line_and_column_deltas() {
        // 两条映射：产物 (0,0)→源偏移 0；产物 (1,2)→源偏移 10。
        let mut sm = SourceMap::new();
        let idx = sm.add_source("a.js", Some("let x = 1;\nlet y = 2;".into()));
        sm.mappings.push(Mapping {
            gen_line: 0,
            gen_col: 0,
            src_index: idx,
            src_offset: 0,
            name_index: None,
            is_unmapped: false,
        });
        sm.mappings.push(Mapping {
            gen_line: 1,
            gen_col: 2,
            src_index: idx,
            src_offset: 11,
            name_index: None,
            is_unmapped: false,
        });
        // 源偏移 0 → (0,0)；偏移 11 → (1,0)
        let json = sm.to_json(|_, off| if off == 0 { (0, 0) } else { (1, 0) });

        // 第一段 AAAA（全 0 差分）；换行后第二段 EACA：列+2、源下标+0、源行+1、源列+0
        assert!(json.contains("\"mappings\":\"AAAA;EACA\""), "实际: {json}");
        assert!(json.contains("\"version\":3"));
        assert!(json.contains("\"sources\":[\"a.js\"]"));
        assert!(
            json.contains("\"names\":[]"),
            "ordinary mappings stay unnamed: {json}"
        );
    }

    #[test]
    fn empty_lines_emit_semicolons() {
        // 产物第 3 行才有映射 → 前面补 3 个分号。
        let mut sm = SourceMap::new();
        let idx = sm.add_source("a.js", None);
        sm.mappings.push(Mapping {
            gen_line: 3,
            gen_col: 0,
            src_index: idx,
            src_offset: 0,
            name_index: None,
            is_unmapped: false,
        });
        let json = sm.to_json(|_, _| (0, 0));
        assert!(json.contains("\"mappings\":\";;;AAAA\""), "实际: {json}");
        // 缺失的 sourcesContent 编码为 null
        assert!(json.contains("\"sourcesContent\":[null]"));
    }

    #[test]
    fn json_escapes_control_and_quotes() {
        let mut sm = SourceMap::new();
        sm.add_source("a\"b.js", Some("let s = \"x\";\n\tconst t = 1;".into()));
        let json = sm.to_json(|_, _| (0, 0));
        assert!(json.contains(r#""a\"b.js""#), "文件名引号须转义: {json}");
        assert!(json.contains("\\t"), "制表符须转义: {json}");
        assert!(json.contains("\\n"), "换行须转义: {json}");
    }

    #[test]
    fn mappings_sorted_by_generated_position() {
        // 乱序输入应被排序后编码（否则差分会算错）。
        let mut sm = SourceMap::new();
        let idx = sm.add_source("a.js", None);
        sm.mappings.push(Mapping {
            gen_line: 1,
            gen_col: 0,
            src_index: idx,
            src_offset: 0,
            name_index: None,
            is_unmapped: false,
        });
        sm.mappings.push(Mapping {
            gen_line: 0,
            gen_col: 0,
            src_index: idx,
            src_offset: 0,
            name_index: None,
            is_unmapped: false,
        });
        let json = sm.to_json(|_, _| (0, 0));
        // 排序后：第 0 行一段、第 1 行一段
        assert!(json.contains("\"mappings\":\"AAAA;AAAA\""), "实际: {json}");
    }

    #[test]
    fn named_segments_emit_names_and_a_fifth_vlq_field() {
        let mut sm = SourceMap::new();
        let idx = sm.add_source("a.js", Some("const descriptiveName = 1;".into()));
        let name = sm.add_name("descriptiveName");
        sm.mappings.push(Mapping {
            gen_line: 0,
            gen_col: 0,
            src_index: idx,
            src_offset: 0,
            name_index: None,
            is_unmapped: false,
        });
        sm.mappings.push(Mapping {
            gen_line: 0,
            gen_col: 6,
            src_index: idx,
            src_offset: 6,
            name_index: Some(name),
            is_unmapped: false,
        });

        let json = sm.to_json(|_, offset| (0, offset));
        assert!(json.contains("\"names\":[\"descriptiveName\"]"), "{json}");
        // First segment has four fields. The named segment has a fifth, zero-delta name field.
        assert!(json.contains("\"mappings\":\"AAAA,MAAMA\""), "{json}");
    }

    #[test]
    fn name_deltas_continue_across_unnamed_segments() {
        let mut sm = SourceMap::new();
        let source = sm.add_source("a.js", None);
        let alpha = sm.add_name("alpha");
        let beta = sm.add_name("beta");
        for (gen_col, name_index) in [(0, Some(alpha)), (1, None), (2, Some(beta))] {
            sm.mappings.push(Mapping {
                gen_line: 0,
                gen_col,
                src_index: source,
                src_offset: 0,
                name_index,
                is_unmapped: false,
            });
        }

        let json = sm.to_json(|_, _| (0, 0));
        assert!(json.contains("\"names\":[\"alpha\",\"beta\"]"), "{json}");
        // `beta` is +1 from the prior named segment even though an unnamed segment sits between.
        assert!(json.contains("\"mappings\":\"AAAAA,CAAA,CAAAC\""), "{json}");
    }

    #[test]
    fn unmapped_segments_encode_only_the_generated_column() {
        let mut sm = SourceMap::new();
        let source = sm.add_source("a.js", Some("value".into()));
        sm.mappings.push(Mapping {
            gen_line: 0,
            gen_col: 0,
            src_index: source,
            src_offset: 0,
            name_index: None,
            is_unmapped: false,
        });
        sm.mappings.push(Mapping::unmapped(0, 3));
        sm.mappings.push(Mapping {
            gen_line: 0,
            gen_col: 4,
            src_index: source,
            src_offset: 1,
            name_index: None,
            is_unmapped: false,
        });

        let json = sm.to_json(|_, offset| (0, offset));
        // `G` is the one-field generated-column delta (+3). The following mapped segment keeps
        // source deltas relative to the preceding *mapped* segment, as required by V3.
        assert!(json.contains("\"mappings\":\"AAAA,G,CAAC\""), "{json}");
    }
}
