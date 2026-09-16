//! Private TypeScript 7.0.2 AST wire v5 address adapter. No backend AST escapes this module.
//! The wire layout is verified against the installed native API, not a public Wake AST contract.
use crate::WakeError;
use std::path::Path;
use wake_common::Span;

fn invalid(message: &str) -> WakeError {
    WakeError::new(
        "WAKE_LINT_ANALYSIS",
        format!("Type source adapter: {message}"),
    )
}

struct Change {
    utf16_end: u32,
    byte_end: u32,
    astral: bool,
}
struct Offsets {
    changes: Vec<Change>,
    utf16_len: u32,
}
impl Offsets {
    fn new(source: &str) -> Result<Self, WakeError> {
        if source.len() > 16 * 1024 * 1024 {
            return Err(invalid("source byte budget exceeded"));
        }
        let mut changes = Vec::new();
        let mut utf16 = 0;
        for (byte, ch) in source.char_indices() {
            utf16 += ch.len_utf16() as u32;
            if ch.len_utf16() != ch.len_utf8() {
                changes.push(Change {
                    utf16_end: utf16,
                    byte_end: (byte + ch.len_utf8()) as u32,
                    astral: ch.len_utf16() == 2,
                });
            }
        }
        Ok(Self {
            changes,
            utf16_len: utf16,
        })
    }
    fn to_utf8(&self, offset: u32) -> Result<u32, WakeError> {
        if offset > self.utf16_len {
            return Err(invalid("UTF-16 offset outside source"));
        }
        let index = self
            .changes
            .partition_point(|change| change.utf16_end <= offset);
        if self
            .changes
            .get(index)
            .is_some_and(|next| next.astral && offset == next.utf16_end - 1)
        {
            return Err(invalid("UTF-16 offset splits a surrogate pair"));
        }
        let delta = index.checked_sub(1).map_or(0, |index| {
            let change = &self.changes[index];
            change.byte_end - change.utf16_end
        });
        Ok(offset + delta)
    }
}

#[derive(Clone, Copy, Default)]
struct NodeAddress {
    kind: u32,
    start: u32,
    end: u32,
    parent: u32,
}

pub(super) struct WireSource<'source> {
    source: &'source str,
    path: String,
    nodes: Vec<NodeAddress>,
    addresses: Vec<u32>,
    children: Vec<u32>,
    expression_roots: Vec<u32>,
}
impl<'source> WireSource<'source> {
    pub(super) fn parse(
        bytes: &[u8],
        source: &'source str,
        path: &Path,
    ) -> Result<Self, WakeError> {
        let offsets = Offsets::new(source)?;
        let word = |offset: usize| -> Result<u32, WakeError> {
            let end = offset
                .checked_add(4)
                .ok_or_else(|| invalid("word offset overflow"))?;
            let value = bytes
                .get(offset..end)
                .ok_or_else(|| invalid("truncated wire word"))?;
            Ok(u32::from_le_bytes(value.try_into().expect("four bytes")))
        };
        if word(0)? >> 24 != 5 {
            return Err(invalid("unsupported AST wire version"));
        }
        let table = word(24)? as usize;
        let strings = word(28)? as usize;
        let extended = word(32)? as usize;
        let structured = word(36)? as usize;
        let nodes = word(40)? as usize;
        if !(44 <= table
            && table <= strings
            && strings <= extended
            && extended <= structured
            && structured <= nodes
            && nodes <= bytes.len())
            || !(strings - table).is_multiple_of(4)
            || !(bytes.len() - nodes).is_multiple_of(28)
        {
            return Err(invalid("invalid wire section offsets"));
        }
        let count = (bytes.len() - nodes) / 28;
        if !(2..=500_000).contains(&count) {
            return Err(invalid("invalid wire node count or budget"));
        }
        let entries = (strings - table) / 4;
        if entries < 2 || !entries.is_multiple_of(2) {
            return Err(invalid("invalid string offset table"));
        }
        // Each string owns an offset pair. Substrings may point back into source text;
        // the table is not a monotone list of disjoint strings.
        for index in (0..entries).step_by(2) {
            let start = word(table + index * 4)? as usize;
            let end = word(table + (index + 1) * 4)? as usize;
            if start > end || end > extended - strings {
                return Err(invalid("invalid string bounds"));
            }
        }
        let string = |index: usize| -> Result<&[u8], WakeError> {
            if index >= entries - 1 || !index.is_multiple_of(2) {
                return Err(invalid("invalid string index"));
            }
            let start = word(table + index * 4)? as usize;
            let end = word(table + (index + 1) * 4)? as usize;
            Ok(&bytes[strings + start..strings + end])
        };
        let root = nodes + 28;
        if word(root)? != 307 || word(root + 4)? != 0 || word(root + 8)? != offsets.utf16_len {
            return Err(invalid("invalid source file address"));
        }
        let data = word(root + 20)?;
        if data & 0xc000_0000 != 0x8000_0000 {
            return Err(invalid("invalid source file payload"));
        }
        let source_data = extended + (data & 0x00ff_ffff) as usize;
        if source_data + 12 > structured {
            return Err(invalid("source file payload outside section"));
        }
        if string(word(source_data)? as usize)? != source.as_bytes() {
            return Err(invalid("backend source differs from the input snapshot"));
        }
        let backend_path = std::str::from_utf8(string(word(source_data + 8)? as usize)?)
            .map_err(|_| invalid("invalid source path encoding"))?;
        #[cfg(windows)]
        let native_path = backend_path.replace('/', "\\");
        #[cfg(not(windows))]
        let native_path = backend_path;
        let actual = wake_common::fs::normalize(Path::new(&native_path));
        let expected = wake_common::fs::normalize(path);
        #[cfg(windows)]
        let matches = actual
            .as_os_str()
            .eq_ignore_ascii_case(expected.as_os_str());
        #[cfg(not(windows))]
        let matches = actual == expected;
        if !actual.is_absolute() || !matches || backend_path.contains('\0') {
            return Err(invalid(&format!(
                "backend path differs from the requested source: {} != {}",
                actual.display(),
                expected.display()
            )));
        }
        let mut records = vec![NodeAddress::default(); count];
        let mut addresses = Vec::new();
        for index in 1..count {
            let node = nodes + index * 28;
            let kind = word(node)?;
            let parent = word(node + 16)? as usize;
            let next = word(node + 12)? as usize;
            if parent >= index && !(index == 1 && parent == 1)
                || next >= count
                || next != 0 && next <= index
            {
                return Err(invalid("invalid node relationship"));
            }
            let parent = if parent == index {
                0
            } else if parent != 0 && records[parent].kind == u32::MAX {
                records[parent].parent
            } else {
                parent as u32
            };
            records[index] = NodeAddress {
                kind,
                parent,
                ..NodeAddress::default()
            };
            let start = word(node + 4)?;
            let end = word(node + 8)?;
            if start == u32::MAX && end == u32::MAX {
                continue;
            }
            if start > end {
                return Err(invalid("reversed node range"));
            }
            let start = offsets.to_utf8(start)?;
            let end = offsets.to_utf8(end)?;
            records[index].start = start;
            records[index].end = end;
            if kind != u32::MAX {
                addresses.push(index as u32);
            }
        }
        addresses.sort_unstable_by_key(|index| {
            let node = records[*index as usize];
            (node.kind, node.end, node.start, *index)
        });
        let mut children = addresses.clone();
        children.sort_unstable_by_key(|index| {
            let node = records[*index as usize];
            (node.parent, node.end, node.start, *index)
        });
        let mut template_end = None;
        for index in &children {
            let node = records[*index as usize];
            if node.kind == 240 && records[node.parent as usize].kind == 229 {
                if template_end
                    .is_some_and(|(parent, end)| parent == node.parent && end > node.start)
                {
                    return Err(invalid("overlapping template substitution containers"));
                }
                template_end = Some((node.parent, node.end));
            }
        }
        // Expression statements can contain nested function bodies and their own statements.
        // Index direct expression children by end rather than choosing every enclosing statement.
        let mut expression_roots: Vec<_> = children
            .iter()
            .copied()
            .filter(|index| {
                let node = records[*index as usize];
                records[node.parent as usize].kind == 245
            })
            .collect();
        expression_roots.sort_unstable_by_key(|index| records[*index as usize].end);
        Ok(Self {
            source,
            path: backend_path.to_owned(),
            nodes: records,
            addresses,
            children,
            expression_roots,
        })
    }

    fn range(&self, span: Span) -> Result<(), WakeError> {
        if span.lo > span.hi
            || !self.source.is_char_boundary(span.lo as usize)
            || !self.source.is_char_boundary(span.hi as usize)
        {
            return Err(invalid("query range is not a valid source boundary"));
        }
        Ok(())
    }

    /// Private declaration facts for readonly property proofs. Modifier tokens are children of
    /// their declaration; property names and nested type members cannot act as modifiers.
    pub(super) fn property_permissions(&self) -> std::collections::BTreeMap<u32, (u32, bool)> {
        let readonly: std::collections::BTreeSet<_> = self
            .nodes
            .iter()
            .filter(|node| node.kind == 148)
            .map(|node| node.parent)
            .collect();
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                matches!(node.kind, 172 | 173 | 303 | 304).then_some((
                    index as u32,
                    (node.kind, readonly.contains(&(index as u32))),
                ))
            })
            .collect()
    }

    fn nested(&self, parent: Span, child: Span) -> Result<(), WakeError> {
        self.range(parent)?;
        self.range(child)?;
        if child.lo < parent.lo || child.hi > parent.hi {
            return Err(invalid("query range is outside its original parent"));
        }
        Ok(())
    }

    fn unique(mut candidates: impl Iterator<Item = u32>) -> Result<u32, WakeError> {
        let index = candidates
            .next()
            .ok_or_else(|| invalid("no backend node matches the original source range"))?;
        if candidates.next().is_some() {
            return Err(invalid("ambiguous backend source range"));
        }
        Ok(index)
    }

    fn node(&self, kind: u32, span: Span) -> Result<u32, WakeError> {
        self.range(span)?;
        let begin = self.addresses.partition_point(|index| {
            let node = self.nodes[*index as usize];
            (node.kind, node.end) < (kind, span.hi)
        });
        Self::unique(
            self.addresses[begin..]
                .iter()
                .copied()
                .take_while(|index| {
                    let node = self.nodes[*index as usize];
                    (node.kind, node.end) == (kind, span.hi)
                })
                .filter(|index| self.nodes[*index as usize].start <= span.lo),
        )
    }

    fn handle(&self, index: u32) -> String {
        format!("{index}.{}.{}", self.nodes[index as usize].kind, self.path)
    }

    pub(super) fn address(&self, kind: u32, span: Span) -> Result<String, WakeError> {
        self.node(kind, span).map(|index| self.handle(index))
    }

    fn child(&self, parent: u32, span: Span) -> Result<u32, WakeError> {
        let begin = self.children.partition_point(|index| {
            let node = self.nodes[*index as usize];
            (node.parent, node.end) < (parent, span.hi)
        });
        Self::unique(
            self.children[begin..]
                .iter()
                .copied()
                .take_while(|index| {
                    let node = self.nodes[*index as usize];
                    (node.parent, node.end) == (parent, span.hi)
                })
                .filter(|index| self.nodes[*index as usize].start <= span.lo),
        )
    }

    pub(super) fn child_address(
        &self,
        kind: u32,
        parent: Span,
        child: Span,
    ) -> Result<String, WakeError> {
        self.nested(parent, child)?;
        self.child(self.node(kind, parent)?, child)
            .map(|index| self.handle(index))
    }

    fn descendant_of(&self, mut index: u32, ancestor: u32) -> bool {
        while index != 0 {
            if index == ancestor {
                return true;
            }
            index = self.nodes[index as usize].parent;
        }
        false
    }

    /// Bind an original value range to a descendant of a grammar-owned property/attribute
    /// without exposing backend syntax kinds to the parser. This is used where JSX and object
    /// properties may wrap the value in expression containers or method nodes.
    pub(super) fn descendant_address(
        &self,
        parent: Span,
        child: Span,
    ) -> Result<String, WakeError> {
        self.nested(parent, child)?;
        let parents = self
            .addresses
            .iter()
            .copied()
            .filter(|index| {
                let node = self.nodes[*index as usize];
                node.start <= parent.lo && node.end >= parent.hi
            })
            .collect::<Vec<_>>();
        if parents.is_empty() {
            return Err(invalid(
                "no backend property node contains the original source range",
            ));
        }
        let mut matches = self
            .addresses
            .iter()
            .copied()
            .filter(|index| {
                let node = self.nodes[*index as usize];
                node.start <= child.lo
                    && node.end >= child.hi
                    && parents
                        .iter()
                        .any(|parent| *index != *parent && self.descendant_of(*index, *parent))
            })
            .collect::<Vec<_>>();
        matches.sort_unstable_by_key(|index| {
            let node = self.nodes[*index as usize];
            (node.end - node.start, node.start, *index)
        });
        if matches.is_empty() {
            return Err(invalid("no backend node contains the original value range"));
        }
        let first = self.nodes[matches[0] as usize];
        Self::unique(matches.into_iter().take_while(|index| {
            let node = self.nodes[*index as usize];
            node.end - node.start == first.end - first.start && node.start == first.start
        }))
        .map(|index| self.handle(index))
    }

    pub(super) fn descendant_kind_address(
        &self,
        parent: Span,
        kind: u32,
    ) -> Result<String, WakeError> {
        self.range(parent)?;
        let mut matches = self
            .addresses
            .iter()
            .copied()
            .filter(|index| {
                let node = self.nodes[*index as usize];
                node.kind == kind && node.start >= parent.lo && node.end <= parent.hi
            })
            .collect::<Vec<_>>();
        matches.sort_unstable_by_key(|index| {
            let node = self.nodes[*index as usize];
            (node.end - node.start, node.start, *index)
        });
        let Some(first) = matches.first().copied() else {
            return Err(invalid(
                "no backend contextual node matches the original property range",
            ));
        };
        let first_node = self.nodes[first as usize];
        Self::unique(matches.into_iter().take_while(|index| {
            let node = self.nodes[*index as usize];
            node.start == first_node.start && node.end == first_node.end
        }))
        .map(|index| self.handle(index))
    }

    /// Expression statements are recorded before semicolon consumption by the Wake parser,
    /// while TypeScript's statement node may include the semicolon in its end offset. Bind the
    /// parent by its original start and the exact expression child instead of guessing a new
    /// source range.
    pub(super) fn expression_statement_address(
        &self,
        statement: Span,
        expression: Span,
    ) -> Result<String, WakeError> {
        self.nested(statement, expression)?;
        let begin = self
            .expression_roots
            .partition_point(|index| self.nodes[*index as usize].end < expression.hi);
        Self::unique(
            self.expression_roots[begin..]
                .iter()
                .copied()
                .take_while(|index| self.nodes[*index as usize].end == expression.hi)
                .filter(|index| {
                    let node = self.nodes[*index as usize];
                    let parent = self.nodes[node.parent as usize];
                    node.start <= expression.lo
                        && parent.start <= statement.lo
                        && parent.end >= statement.hi
                }),
        )
        .map(|index| self.handle(index))
    }

    pub(super) fn head_address(
        &self,
        kind: u32,
        parent: Span,
        head: Span,
    ) -> Result<String, WakeError> {
        self.nested(parent, head)?;
        let parent = self.node(kind, parent)?;
        let begin = self
            .children
            .partition_point(|index| self.nodes[*index as usize].parent < parent);
        let index = Self::unique(
            self.children[begin..]
                .iter()
                .copied()
                .take_while(|index| {
                    let node = self.nodes[*index as usize];
                    node.parent == parent && node.end <= head.hi
                })
                .filter(|index| {
                    let node = self.nodes[*index as usize];
                    node.start <= head.lo && node.end > head.lo
                }),
        )?;
        Ok(self.handle(index))
    }

    pub(super) fn template_address(
        &self,
        template: Span,
        expression: Span,
    ) -> Result<String, WakeError> {
        self.nested(template, expression)?;
        let parent = self.node(229, template)?;
        let begin = self.children.partition_point(|index| {
            let node = self.nodes[*index as usize];
            (node.parent, node.end) < (parent, expression.hi)
        });
        let span = *self
            .children
            .get(begin)
            .ok_or_else(|| invalid("missing template substitution"))?;
        let node = self.nodes[span as usize];
        if node.parent != parent || node.kind != 240 || node.start > expression.lo {
            return Err(invalid(
                "template substitution differs from original grammar",
            ));
        }
        self.child(span, expression).map(|index| self.handle(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Independently assembled protocol fixture containing only source/path and two addresses.
    fn fixture(source: &str, path: &str, start: u32, end: u32) -> Vec<u8> {
        fn put(bytes: &mut [u8], offset: usize, value: u32) {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        let offsets = 44;
        let strings = offsets + 16;
        let extended = strings + source.len() + path.len();
        let structured = extended + 12;
        let nodes = structured;
        let mut bytes = vec![0; nodes + 3 * 28];
        put(&mut bytes, 0, 5 << 24);
        for (field, value) in [
            (24, offsets),
            (28, strings),
            (32, extended),
            (36, structured),
            (40, nodes),
        ] {
            put(&mut bytes, field, value as u32);
        }
        put(&mut bytes, offsets, 0);
        put(&mut bytes, offsets + 4, source.len() as u32);
        put(&mut bytes, offsets + 8, source.len() as u32);
        put(&mut bytes, offsets + 12, (source.len() + path.len()) as u32);
        bytes[strings..strings + source.len()].copy_from_slice(source.as_bytes());
        bytes[strings + source.len()..extended].copy_from_slice(path.as_bytes());
        put(&mut bytes, extended + 4, 2);
        put(&mut bytes, extended + 8, 2);
        put(&mut bytes, nodes + 28, 307);
        put(
            &mut bytes,
            nodes + 28 + 8,
            source.encode_utf16().count() as u32,
        );
        put(&mut bytes, nodes + 28 + 20, 0x8000_0000);
        put(&mut bytes, nodes + 56, 214);
        put(&mut bytes, nodes + 56 + 4, start);
        put(&mut bytes, nodes + 56 + 8, end);
        put(&mut bytes, nodes + 56 + 16, 1);
        bytes
    }

    #[test]
    fn child_queries_require_the_original_parent_and_reject_ambiguous_or_outside_ranges() {
        let path = std::env::temp_dir().join("wire-children.ts");
        let mut bytes = fixture("f(1)", path.to_str().unwrap(), 0, 4);
        let nodes = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
        bytes.resize(bytes.len() + 3 * 28, 0);
        for (index, kind, start, end, parent) in [
            (3, 80u32, 0u32, 1u32, 2u32),
            (4, 9, 2, 3, 2),
            (5, 80, 0, 1, 1),
        ] {
            for (offset, value) in [(0, kind), (4, start), (8, end), (16, parent)] {
                bytes[nodes + index * 28 + offset..nodes + index * 28 + offset + 4]
                    .copy_from_slice(&value.to_le_bytes());
            }
        }
        let wire = WireSource::parse(&bytes, "f(1)", &path).unwrap();
        let call = Span::new(0, 4);
        assert!(
            wire.head_address(214, call, Span::new(0, 1))
                .unwrap()
                .starts_with("3.80.")
        );
        assert!(
            wire.child_address(214, call, Span::new(2, 3))
                .unwrap()
                .starts_with("4.9.")
        );
        assert!(wire.child_address(214, call, Span::new(2, 4)).is_err());
        assert!(wire.child_address(214, call, Span::new(0, 5)).is_err());
        bytes[nodes + 5 * 28 + 16..nodes + 5 * 28 + 20].copy_from_slice(&2u32.to_le_bytes());
        let ambiguous = WireSource::parse(&bytes, "f(1)", &path).unwrap();
        assert!(ambiguous.head_address(214, call, Span::new(0, 1)).is_err());
    }

    #[test]
    fn wire_addresses_are_bound_to_exact_source_path_and_unicode_boundaries() {
        let path = std::env::temp_dir().join("wire-a.ts");
        let source = "/* 😀 */\r\nrun();";
        let start = source.find("run").unwrap();
        let end = source.len() - 1;
        let full_start = 0;
        let bytes = fixture(
            source,
            &path.to_string_lossy(),
            full_start,
            source[..end].encode_utf16().count() as u32,
        );
        let wire = WireSource::parse(&bytes, source, &path).unwrap();
        let span = wake_common::Span::new(start as u32, end as u32);
        let address = wire.address(214, span).unwrap();
        assert!(address.starts_with("2.214."));
        assert!(wire.address(214, Span::new(4, end as u32)).is_err());
        assert!(
            wire.address(
                214,
                Span {
                    lo: source.len() as u32,
                    hi: end as u32
                }
            )
            .is_err()
        );
        assert!(
            wire.address(
                214,
                wake_common::Span::new(start as u32, source.len() as u32)
            )
            .is_err()
        );
        assert!(WireSource::parse(&bytes, "/* changed */\r\nrun();", &path).is_err());
        assert!(WireSource::parse(&bytes, source, &path.with_file_name("other.ts")).is_err());
        for mutate in [0usize, 24, 28, 32, 36, 40] {
            let mut bad = bytes.clone();
            bad[mutate..mutate + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(
                WireSource::parse(&bad, source, &path).is_err(),
                "header offset {mutate}"
            );
        }
        for length in 0..bytes.len() {
            if let Ok(partial) = WireSource::parse(&bytes[..length], source, &path) {
                assert!(
                    partial.address(214, span).is_err(),
                    "truncated address table {length}"
                );
            }
        }
        let midpoint = "/* ".encode_utf16().count() as u32 + 1;
        assert!(
            WireSource::parse(
                &fixture(source, &path.to_string_lossy(), midpoint, midpoint),
                source,
                &path
            )
            .is_err()
        );
    }

    #[test]
    #[cfg(windows)]
    fn compiler_forward_slash_verbatim_paths_preserve_source_identity() {
        let source = "f()";
        for (backend, input) in [
            ("//?/c:/type-rule/a.ts", r"C:\type-rule\a.ts"),
            ("//?/UNC/server/share/a.ts", r"\\server\share\a.ts"),
        ] {
            assert!(
                WireSource::parse(&fixture(source, backend, 0, 3), source, Path::new(input))
                    .is_ok(),
                "{backend}"
            );
        }
    }

    #[test]
    fn unicode_offset_map_is_reversible_and_rejects_partial_scalars() {
        let source = "aé中😀\r\n\u{2028}z";
        let map = Offsets::new(source).unwrap();
        for byte in 0..=source.len() {
            if source.is_char_boundary(byte) {
                let utf16 = source[..byte].encode_utf16().count() as u32;
                assert_eq!(map.to_utf8(utf16).unwrap(), byte as u32);
            }
        }
        assert!(map.to_utf8(4).is_err());
        assert!(map.to_utf8(100).is_err());
    }
}
