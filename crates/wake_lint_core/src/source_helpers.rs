//! Shared projections of parser-owned source facts. No name resolution is performed here.
use wake_common::{FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::*;

pub(crate) fn pattern_names(pattern: Pattern<'_>, names: &mut Vec<Ident>) {
    match pattern {
        Pattern::Ident(id) => names.push(*id),
        Pattern::Array(array) => {
            for element in array.elements.iter().flatten() {
                pattern_names(*element, names);
            }
        }
        Pattern::Object(object) => {
            for property in &object.properties {
                pattern_names(property.value, names);
            }
            if let Some(rest) = object.rest {
                pattern_names(rest.argument, names);
            }
        }
        Pattern::Assignment(value) => pattern_names(value.left, names),
        Pattern::Rest(value) => pattern_names(value.argument, names),
    }
}

pub(crate) fn jsx_tags(syntax: &[SourceNode]) -> FxHashMap<u32, u32> {
    syntax
        .iter()
        .filter(|node| {
            node.kind == SourceNodeKind::JsxName
                && node
                    .parent
                    .is_some_and(|parent| syntax[parent].kind == SourceNodeKind::JsxOpeningElement)
        })
        .map(|node| (node.span.lo, node.span.hi))
        .collect()
}

pub(crate) fn dynamic_access(
    semantic: &wake_ecma_semantic::SourceSemanticModel,
    interner: &Interner,
    calls: &FxHashSet<Span>,
    has_with: bool,
) -> bool {
    has_with
        || semantic.references.iter().any(|&index| {
            let reference = &semantic.model.references[index];
            reference.resolved.is_none()
                && interner.resolve(reference.name) == "eval"
                && calls.contains(&reference.span)
        })
}
