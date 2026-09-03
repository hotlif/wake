//! # wake_common — Wake 基础设施
//!
//! Span、字符串驻留（Atom）、诊断系统 + 终端渲染、文件系统抽象。
//! 这是所有其他 crate 的地基，本身 **不** 依赖任何其他 wake crate（DESIGN §4.1）。
//!
//! - [`Span`] / [`SourceFile`]：字节偏移位置 + 行列还原。
//! - [`Atom`] / [`Interner`]：分片锁字符串驻留，比较退化为 `u32`。
//! - [`Diagnostic`] + [`render`]：统一诊断结构 + rustc 风格彩色报错。
//! - [`FileSystem`] / [`MemoryFileSystem`] / [`OsFileSystem`]：可测试的文件系统抽象。
//!
//! 全部 map 统一使用 [`FxHashMap`]（非加密 hash，DESIGN §10.8.3）。

pub mod atom;
pub mod diagnostic;
pub mod fs;
pub mod render;
pub mod source;
pub mod span;
pub mod zip;

pub use atom::{Atom, Interner};
pub use diagnostic::{Diagnostic, Label, Severity};
pub use fs::{
    FileSystem, FileSystemProjection, MemoryFileSystem, OsFileSystem, OwnedFileTree,
    OwnedFileTreeBuilder, OwnedFileTreeError, OwnedOverlayFileSystem, ProjectedFileSystem,
    ProjectedRelativePath,
};
pub use render::{RenderStyle, render};
pub use source::{LineCol, SourceFile};
pub use span::Span;

/// 项目统一的非加密 hash map（rustc-hash）。
pub type FxHashMap<K, V> = rustc_hash::FxHashMap<K, V>;
/// 项目统一的非加密 hash set。
pub type FxHashSet<T> = rustc_hash::FxHashSet<T>;

/// 内容指纹类型别名（xxh3 的输出为 u64）。真正的 hash 计算在需要的 crate 内做，
/// 这里只统一类型口径（DESIGN §10.8.5）。
pub type Hash64 = u64;
