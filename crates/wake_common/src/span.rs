//! 源码位置：`Span { lo, hi }`，字节偏移，8 字节。
//!
//! 热路径上只携带 Span、绝不计算行列；行列号仅在报错时由
//! [`crate::source::SourceFile`] 的换行偏移表二分还原（DESIGN §4.1）。

/// 源文件内的字节区间 `[lo, hi)`。
///
/// 恒定 8 字节，可 `Copy`。所有 token / AST 节点 / 诊断都用它指回源文本，
/// 转换 pass 复用原始 Span 以保证 SourceMap 恒等映射（DESIGN §4.5「pass 不改 Span」纪律）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    /// 起始字节偏移（含）。
    pub lo: u32,
    /// 结束字节偏移（不含）。
    pub hi: u32,
}

const _: () = assert!(std::mem::size_of::<Span>() == 8, "Span 必须恒为 8 字节");

impl Span {
    /// 占位 span（`0..0`），用于合成节点或未知位置。
    pub const DUMMY: Span = Span { lo: 0, hi: 0 };

    #[inline]
    pub const fn new(lo: u32, hi: u32) -> Span {
        debug_assert!(lo <= hi);
        Span { lo, hi }
    }

    /// 单点位置 `[at, at)`（长度 0），用于「在此处期望某物」类诊断。
    #[inline]
    pub const fn at(at: u32) -> Span {
        Span { lo: at, hi: at }
    }

    #[inline]
    pub const fn len(self) -> u32 {
        self.hi - self.lo
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.lo >= self.hi
    }

    /// 是否是占位 span（`DUMMY`）。
    #[inline]
    pub const fn is_dummy(self) -> bool {
        self.lo == 0 && self.hi == 0
    }

    /// 该 span 是否完全包含 `other`。
    #[inline]
    pub const fn contains(self, other: Span) -> bool {
        self.lo <= other.lo && other.hi <= self.hi
    }

    /// 是否包含某个字节偏移。
    #[inline]
    pub const fn contains_offset(self, offset: u32) -> bool {
        self.lo <= offset && offset < self.hi
    }

    /// 合并两个 span 为覆盖二者的最小 span。
    #[inline]
    pub fn to(self, other: Span) -> Span {
        Span {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }

    /// 取源文本对应切片（调用方保证 `src` 与 span 同源）。
    #[inline]
    pub fn slice(self, src: &str) -> &str {
        &src[self.lo as usize..self.hi as usize]
    }
}

impl std::fmt::Debug for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}..{}", self.lo, self.hi)
    }
}

impl From<std::ops::Range<u32>> for Span {
    #[inline]
    fn from(r: std::ops::Range<u32>) -> Span {
        Span::new(r.start, r.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_is_8() {
        assert_eq!(std::mem::size_of::<Span>(), 8);
    }

    #[test]
    fn basic_ops() {
        let s = Span::new(3, 7);
        assert_eq!(s.len(), 4);
        assert!(!s.is_empty());
        assert!(s.contains(Span::new(4, 6)));
        assert!(!s.contains(Span::new(2, 6)));
        assert!(s.contains_offset(3));
        assert!(!s.contains_offset(7));
        assert_eq!(s.to(Span::new(10, 12)), Span::new(3, 12));
        assert_eq!(s.slice("0123456789"), "3456");
    }

    #[test]
    fn dummy_and_at() {
        assert!(Span::DUMMY.is_dummy());
        assert!(Span::at(5).is_empty());
        assert!(!Span::new(0, 1).is_dummy());
    }
}
