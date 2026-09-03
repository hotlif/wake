//! Declaration facts collected by the ordinary ECMAScript/TypeScript parser.
//!
//! This module deliberately does not tokenize or parse statements itself.  The collector is an
//! optional sink owned by [`super::Parser`]; grammar branches in `stmt`/`ts` publish facts only
//! after the same branch that builds the runtime AST has accepted the syntax.

use std::collections::BTreeSet;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use bumpalo::Bump;
use wake_common::{Atom, Interner, Span};
use wake_ecma_ast::{
    Class, ExportDefaultKind, Expression, Function, MemberProperty, MethodKind, ModuleExportName,
    ObjectMember, Program, PropertyKey, PropertyKind, SourceType, Statement, VariableDeclarator,
};

use super::{ParseOptions, Parser};

/// A declaration-producing construct accepted by the main parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeclarationItemKind {
    Import,
    ReExport,
    Interface,
    TypeAlias,
    Enum,
    Namespace,
    Function,
    Class,
    Variable,
    Export,
    Ambient,
}

/// Why a declaration module refers to another module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeclarationRequestRole {
    ImportType,
    ImportValue,
    ExportType,
    ExportValue,
    ImportTypeExpression,
}

/// Parser-owned reason an import participates in declaration output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeclarationImportUsage {
    TypeOnly,
    ReferencedValue,
    RuntimeSideEffect,
}

/// A typed module request embedded in a rendered declaration item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationRequestFact {
    specifier: Arc<str>,
    source_span: Span,
    template_range: Range<usize>,
    role: DeclarationRequestRole,
}

impl DeclarationRequestFact {
    pub fn specifier(&self) -> &str {
        &self.specifier
    }

    pub fn source_span(&self) -> Span {
        self.source_span
    }

    /// Byte range of the quoted module literal inside [`DeclarationItemFact::template`].
    pub fn template_range(&self) -> Range<usize> {
        self.template_range.clone()
    }

    pub fn role(&self) -> DeclarationRequestRole {
        self.role
    }

    pub fn is_type_only(&self) -> bool {
        matches!(
            self.role,
            DeclarationRequestRole::ImportType
                | DeclarationRequestRole::ExportType
                | DeclarationRequestRole::ImportTypeExpression
        )
    }
}

/// Immutable output fragment and the parser facts that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationItemFact {
    kind: DeclarationItemKind,
    source_span: Span,
    name: Option<Arc<str>>,
    name_span: Option<Span>,
    exported: bool,
    default_export: bool,
    has_declare_modifier: bool,
    template: Arc<str>,
    requests: Arc<[DeclarationRequestFact]>,
    ambient_template: Arc<str>,
    ambient_requests: Arc<[DeclarationRequestFact]>,
    contains_forbidden_any: bool,
    import_usage: Option<DeclarationImportUsage>,
    /// Parser-interned binding carried only by overload signatures. This is deliberately private:
    /// it is render bookkeeping, not a declaration API name recovered from text.
    overload_binding: Option<Atom>,
}

impl DeclarationItemFact {
    pub fn kind(&self) -> DeclarationItemKind {
        self.kind
    }

    pub fn source_span(&self) -> Span {
        self.source_span
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn name_span(&self) -> Option<Span> {
        self.name_span
    }

    pub fn is_exported(&self) -> bool {
        self.exported
    }

    pub fn is_default_export(&self) -> bool {
        self.default_export
    }

    pub fn has_declare_modifier(&self) -> bool {
        self.has_declare_modifier
    }

    pub fn template(&self) -> &str {
        &self.template
    }

    pub fn requests(&self) -> &[DeclarationRequestFact] {
        &self.requests
    }

    /// Template for use inside an already-ambient `declare module { ... }` body.
    pub fn ambient_template(&self) -> &str {
        &self.ambient_template
    }

    /// Request ranges corresponding to [`Self::ambient_template`].
    pub fn ambient_requests(&self) -> &[DeclarationRequestFact] {
        &self.ambient_requests
    }

    pub fn contains_forbidden_any(&self) -> bool {
        self.contains_forbidden_any
    }

    pub fn import_usage(&self) -> Option<DeclarationImportUsage> {
        self.import_usage
    }
}

/// Frozen declaration facts for one parsed source module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationFacts {
    source: Arc<str>,
    items: Arc<[DeclarationItemFact]>,
}

impl DeclarationFacts {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn items(&self) -> &[DeclarationItemFact] {
        &self.items
    }

    pub fn requests(&self) -> impl Iterator<Item = &DeclarationRequestFact> {
        self.items.iter().flat_map(|item| item.requests.iter())
    }

    pub fn contains_forbidden_any(&self) -> bool {
        self.items.iter().any(|item| item.contains_forbidden_any)
    }
}

/// A strict declaration validation failure, with a byte span in the original source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationFactError {
    span: Span,
    message: Arc<str>,
}

impl DeclarationFactError {
    pub(crate) fn new(span: Span, message: impl Into<Arc<str>>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }

    pub fn span(&self) -> Span {
        self.span
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for DeclarationFactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at byte {}..{}",
            self.message, self.span.lo, self.span.hi
        )
    }
}

impl std::error::Error for DeclarationFactError {}

#[derive(Clone, Copy)]
pub(crate) struct DeclarationCollectorMark {
    item_len: usize,
    request_len: usize,
    error_len: usize,
    any_len: usize,
    annotation_len: usize,
    function_len: usize,
    variable_len: usize,
    arrow_len: usize,
    const_assertion_len: usize,
    import_len: usize,
    type_reference_len: usize,
    value_reference_len: usize,
    active_type_binding_len: usize,
    active_value_binding_len: usize,
    pending_infer_binding_len: usize,
    infer_scope_len: usize,
    export_assignment_binding_len: usize,
    class_len: usize,
    class_member_len: usize,
    type_depth: u32,
}

#[derive(Clone)]
pub(crate) struct PendingRequest {
    pub specifier: Arc<str>,
    pub span: Span,
    pub role: DeclarationRequestRole,
}

#[derive(Clone, Copy)]
struct PendingFunction {
    span: Span,
    keyword_span: Span,
    name_span: Option<Span>,
    name: Option<Atom>,
    return_type: Option<Span>,
    body_span: Option<Span>,
    is_async: bool,
}

#[derive(Clone, Copy)]
struct PendingVariable {
    span: Span,
    name_span: Option<Span>,
    name: Option<Atom>,
    annotation: Option<Span>,
}

#[derive(Clone)]
struct PendingImport {
    span: Span,
    bindings: Vec<Atom>,
    always_keep: bool,
    type_only: bool,
}

#[derive(Clone, Copy)]
struct PendingArrow {
    span: Span,
    signature_span: Span,
    return_type: Option<Span>,
    is_async: bool,
}

#[derive(Clone, Copy)]
struct PendingClass {
    span: Span,
    keyword_span: Span,
    name_span: Option<Span>,
    body_open: Span,
    body_close: Span,
}

#[derive(Clone, Copy)]
enum PendingClassMemberKind {
    Method {
        key: Option<Atom>,
        method_kind: MethodKind,
        return_type: Option<Span>,
        body_span: Option<Span>,
        async_span: Option<Span>,
    },
    Property {
        annotation: Option<Span>,
        initializer_eq: Option<Span>,
    },
    IndexSignature,
    StaticBlock,
}

#[derive(Clone, Copy)]
struct PendingClassMember {
    span: Span,
    signature_lo: u32,
    kind: PendingClassMemberKind,
}

pub(crate) struct DeclarationCollector<'src> {
    source: &'src str,
    strict: bool,
    reject_any: bool,
    items: Vec<DeclarationItemFact>,
    requests: Vec<PendingRequest>,
    errors: Vec<DeclarationFactError>,
    any_spans: Vec<Span>,
    type_annotations: Vec<Span>,
    functions: Vec<PendingFunction>,
    variables: Vec<PendingVariable>,
    arrows: Vec<PendingArrow>,
    const_assertions: Vec<Span>,
    imports: Vec<PendingImport>,
    type_references: Vec<Atom>,
    value_references: Vec<Atom>,
    active_type_bindings: Vec<Atom>,
    active_value_bindings: Vec<Atom>,
    pending_infer_bindings: Vec<Atom>,
    infer_scopes: Vec<usize>,
    export_assignment_bindings: Vec<Atom>,
    classes: Vec<PendingClass>,
    class_members: Vec<PendingClassMember>,
    type_depth: u32,
}

impl<'src> DeclarationCollector<'src> {
    pub(crate) fn new(source: &'src str, strict: bool, reject_any: bool) -> Self {
        Self {
            source,
            strict,
            reject_any,
            items: Vec::new(),
            requests: Vec::new(),
            errors: Vec::new(),
            any_spans: Vec::new(),
            type_annotations: Vec::new(),
            functions: Vec::new(),
            variables: Vec::new(),
            arrows: Vec::new(),
            const_assertions: Vec::new(),
            imports: Vec::new(),
            type_references: Vec::new(),
            value_references: Vec::new(),
            active_type_bindings: Vec::new(),
            active_value_bindings: Vec::new(),
            pending_infer_bindings: Vec::new(),
            infer_scopes: Vec::new(),
            export_assignment_bindings: Vec::new(),
            classes: Vec::new(),
            class_members: Vec::new(),
            type_depth: 0,
        }
    }

    pub(crate) fn mark(&self) -> DeclarationCollectorMark {
        DeclarationCollectorMark {
            item_len: self.items.len(),
            request_len: self.requests.len(),
            error_len: self.errors.len(),
            any_len: self.any_spans.len(),
            annotation_len: self.type_annotations.len(),
            function_len: self.functions.len(),
            variable_len: self.variables.len(),
            arrow_len: self.arrows.len(),
            const_assertion_len: self.const_assertions.len(),
            import_len: self.imports.len(),
            type_reference_len: self.type_references.len(),
            value_reference_len: self.value_references.len(),
            active_type_binding_len: self.active_type_bindings.len(),
            active_value_binding_len: self.active_value_bindings.len(),
            pending_infer_binding_len: self.pending_infer_bindings.len(),
            infer_scope_len: self.infer_scopes.len(),
            export_assignment_binding_len: self.export_assignment_bindings.len(),
            class_len: self.classes.len(),
            class_member_len: self.class_members.len(),
            type_depth: self.type_depth,
        }
    }

    pub(crate) fn rewind(&mut self, mark: DeclarationCollectorMark) {
        self.items.truncate(mark.item_len);
        self.requests.truncate(mark.request_len);
        self.errors.truncate(mark.error_len);
        self.any_spans.truncate(mark.any_len);
        self.type_annotations.truncate(mark.annotation_len);
        self.functions.truncate(mark.function_len);
        self.variables.truncate(mark.variable_len);
        self.arrows.truncate(mark.arrow_len);
        self.const_assertions.truncate(mark.const_assertion_len);
        self.imports.truncate(mark.import_len);
        self.type_references.truncate(mark.type_reference_len);
        self.value_references.truncate(mark.value_reference_len);
        self.active_type_bindings
            .truncate(mark.active_type_binding_len);
        self.active_value_bindings
            .truncate(mark.active_value_binding_len);
        self.pending_infer_bindings
            .truncate(mark.pending_infer_binding_len);
        self.infer_scopes.truncate(mark.infer_scope_len);
        self.export_assignment_bindings
            .truncate(mark.export_assignment_binding_len);
        self.classes.truncate(mark.class_len);
        self.class_members.truncate(mark.class_member_len);
        self.type_depth = mark.type_depth;
    }

    pub(crate) fn error(&mut self, span: Span, message: impl Into<Arc<str>>) {
        self.errors.push(DeclarationFactError::new(span, message));
    }

    pub(crate) fn begin_type(&mut self) {
        self.type_depth += 1;
    }

    pub(crate) fn end_type(&mut self) {
        self.type_depth = self.type_depth.saturating_sub(1);
    }

    pub(crate) fn in_type(&self) -> bool {
        self.type_depth != 0
    }

    pub(crate) fn record_any(&mut self, span: Span) {
        if self.type_depth != 0 {
            self.any_spans.push(span);
        }
    }

    pub(crate) fn record_implicit_any(&mut self, span: Span) {
        self.any_spans.push(span);
    }

    pub(crate) fn record_annotation(&mut self, span: Span) {
        self.type_annotations.push(span);
    }

    pub(crate) fn record_function(
        &mut self,
        span: Span,
        keyword_span: Span,
        name_span: Option<Span>,
        name: Option<Atom>,
        return_type: Option<Span>,
        body_span: Option<Span>,
        is_async: bool,
    ) {
        self.functions.push(PendingFunction {
            span,
            keyword_span,
            name_span,
            name,
            return_type,
            body_span,
            is_async,
        });
    }

    pub(crate) fn record_function_overload(&mut self, span: Span) {
        let Some(function) = self
            .functions
            .iter()
            .rev()
            .find(|function| function.span == span)
        else {
            return;
        };
        if function.name_span.is_none() {
            self.error(span, "function declaration requires a name");
            return;
        }
        let mut builder = TemplateBuilder::new(self.source, &self.requests, &self.any_spans);
        builder.push_str("declare ");
        builder.push_span(Span::new(function.keyword_span.lo, span.hi));
        let (template, requests, contains_forbidden_any) = builder.finish();
        let (ambient_template, ambient_requests) =
            remove_known_template_range(&template, &requests, 0.."declare ".len());
        self.items.push(DeclarationItemFact {
            kind: DeclarationItemKind::Function,
            source_span: self.trim_span(span),
            name: function
                .name_span
                .map(|name| Arc::from(&self.source[name.lo as usize..name.hi as usize])),
            name_span: function.name_span,
            exported: false,
            default_export: false,
            has_declare_modifier: false,
            ambient_template,
            ambient_requests,
            template,
            requests,
            contains_forbidden_any,
            import_usage: None,
            overload_binding: function.name,
        });
    }

    pub(crate) fn record_variable(
        &mut self,
        span: Span,
        name_span: Option<Span>,
        name: Option<Atom>,
        annotation: Option<Span>,
    ) {
        self.variables.push(PendingVariable {
            span,
            name_span,
            name,
            annotation,
        });
    }

    pub(crate) fn record_arrow(
        &mut self,
        span: Span,
        signature_span: Span,
        return_type: Option<Span>,
        is_async: bool,
    ) {
        self.arrows.push(PendingArrow {
            span,
            signature_span,
            return_type,
            is_async,
        });
    }

    pub(crate) fn record_const_assertion(&mut self, span: Span) {
        self.const_assertions.push(span);
    }

    pub(crate) fn record_class(
        &mut self,
        span: Span,
        keyword_span: Span,
        name_span: Option<Span>,
        body_open: Span,
        body_close: Span,
    ) {
        self.classes.push(PendingClass {
            span,
            keyword_span,
            name_span,
            body_open,
            body_close,
        });
    }

    pub(crate) fn record_class_method(
        &mut self,
        span: Span,
        signature_lo: u32,
        key: Option<Atom>,
        method_kind: MethodKind,
        return_type: Option<Span>,
        body_span: Option<Span>,
        async_span: Option<Span>,
    ) {
        self.class_members.push(PendingClassMember {
            span,
            signature_lo,
            kind: PendingClassMemberKind::Method {
                key,
                method_kind,
                return_type,
                body_span,
                async_span,
            },
        });
    }

    pub(crate) fn record_class_property(
        &mut self,
        span: Span,
        signature_lo: u32,
        annotation: Option<Span>,
        initializer_eq: Option<Span>,
    ) {
        self.class_members.push(PendingClassMember {
            span,
            signature_lo,
            kind: PendingClassMemberKind::Property {
                annotation,
                initializer_eq,
            },
        });
    }

    pub(crate) fn record_class_index(&mut self, span: Span, signature_lo: u32) {
        self.class_members.push(PendingClassMember {
            span,
            signature_lo,
            kind: PendingClassMemberKind::IndexSignature,
        });
    }

    pub(crate) fn record_class_static_block(&mut self, span: Span, signature_lo: u32) {
        self.class_members.push(PendingClassMember {
            span,
            signature_lo,
            kind: PendingClassMemberKind::StaticBlock,
        });
    }

    pub(crate) fn record_request(
        &mut self,
        specifier: impl Into<Arc<str>>,
        span: Span,
        role: DeclarationRequestRole,
    ) {
        self.requests.push(PendingRequest {
            specifier: specifier.into(),
            span,
            role,
        });
    }

    pub(crate) fn record_import(
        &mut self,
        span: Span,
        bindings: impl IntoIterator<Item = Atom>,
        always_keep: bool,
        type_only: bool,
    ) {
        self.imports.push(PendingImport {
            span: self.trim_span(span),
            bindings: bindings.into_iter().collect(),
            always_keep,
            type_only,
        });
    }

    pub(crate) fn record_type_reference(&mut self, binding: Atom) {
        if !self.active_type_bindings.contains(&binding) {
            self.type_references.push(binding);
        }
    }

    pub(crate) fn record_value_reference(&mut self, binding: Atom) {
        if !self.active_value_bindings.contains(&binding) {
            self.value_references.push(binding);
        }
    }

    pub(crate) fn type_scope_mark(&self) -> usize {
        self.active_type_bindings.len()
    }

    pub(crate) fn type_reference_mark(&self) -> usize {
        self.type_references.len()
    }

    pub(crate) fn activate_type_bindings_since(
        &mut self,
        reference_mark: usize,
        bindings: &[Atom],
    ) {
        let retained = self
            .type_references
            .drain(reference_mark..)
            .filter(|reference| !bindings.contains(reference))
            .collect::<Vec<_>>();
        self.type_references.extend(retained);
        self.active_type_bindings.extend_from_slice(bindings);
    }

    pub(crate) fn record_type_binding(&mut self, binding: Atom) {
        self.active_type_bindings.push(binding);
    }

    pub(crate) fn restore_type_scope(&mut self, mark: usize) {
        self.active_type_bindings.truncate(mark);
    }

    pub(crate) fn value_scope_mark(&self) -> usize {
        self.active_value_bindings.len()
    }

    pub(crate) fn value_reference_mark(&self) -> usize {
        self.value_references.len()
    }

    pub(crate) fn activate_value_bindings_since(
        &mut self,
        reference_mark: usize,
        bindings: &[Atom],
    ) {
        let retained = self
            .value_references
            .drain(reference_mark..)
            .filter(|reference| !bindings.contains(reference))
            .collect::<Vec<_>>();
        self.value_references.extend(retained);
        self.active_value_bindings.extend_from_slice(bindings);
    }

    pub(crate) fn restore_value_scope(&mut self, mark: usize) {
        self.active_value_bindings.truncate(mark);
    }

    pub(crate) fn begin_infer_scope(&mut self) {
        self.infer_scopes.push(self.pending_infer_bindings.len());
    }

    pub(crate) fn record_infer_binding(&mut self, binding: Atom) -> bool {
        if self.infer_scopes.is_empty() {
            return false;
        }
        self.pending_infer_bindings.push(binding);
        true
    }

    pub(crate) fn activate_infer_scope(&mut self) -> usize {
        let active_mark = self.active_type_bindings.len();
        let Some(pending_mark) = self.infer_scopes.pop() else {
            return active_mark;
        };
        self.active_type_bindings
            .extend_from_slice(&self.pending_infer_bindings[pending_mark..]);
        self.pending_infer_bindings.truncate(pending_mark);
        active_mark
    }

    pub(crate) fn record_export_assignment(&mut self, span: Span, root_binding: Atom) {
        self.export_assignment_bindings.push(root_binding);
        self.record_source_item(DeclarationItemKind::Ambient, span, None, true, false);
    }

    pub(crate) fn item_mark(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn mark_declared_since(&mut self, mark: usize, declare_lo: u32) {
        for item in &mut self.items[mark..] {
            item.has_declare_modifier = true;
            if item.kind == DeclarationItemKind::Ambient && item.source_span.lo == declare_lo {
                (item.ambient_template, item.ambient_requests) = remove_known_template_range(
                    &item.ambient_template,
                    &item.ambient_requests,
                    0.."declare ".len(),
                );
            }
        }
    }

    pub(crate) fn discard_items_since(&mut self, mark: usize) {
        self.items.truncate(mark);
    }

    pub(crate) fn record_source_item(
        &mut self,
        kind: DeclarationItemKind,
        span: Span,
        name_span: Option<Span>,
        exported: bool,
        default_export: bool,
    ) {
        let span = self.trim_span(span);
        if span.is_empty() {
            return;
        }
        self.items
            .push(self.source_item(kind, span, name_span, exported, default_export));
    }

    pub(crate) fn wrap_last_item(&mut self, wrapper_span: Span) {
        let Some(index) = self.items.iter().rposition(|item| {
            wrapper_span.lo <= item.source_span.lo && item.source_span.hi <= wrapper_span.hi
        }) else {
            return;
        };
        let mut item = self.items.remove(index);
        item.source_span = self.trim_span(wrapper_span);
        item.exported = true;
        let (template, requests) = prefix_template("export ", &item.template, &item.requests);
        let (ambient_template, ambient_requests) =
            prefix_template("export ", &item.ambient_template, &item.ambient_requests);
        item.template = template;
        item.requests = requests;
        item.ambient_template = ambient_template;
        item.ambient_requests = ambient_requests;
        self.items.push(item);
    }

    fn source_item(
        &self,
        kind: DeclarationItemKind,
        span: Span,
        name_span: Option<Span>,
        exported: bool,
        default_export: bool,
    ) -> DeclarationItemFact {
        let raw_template = &self.source[span.lo as usize..span.hi as usize];
        let raw_requests = self
            .requests
            .iter()
            .filter(|request| span.contains(request.span))
            .map(|request| DeclarationRequestFact {
                specifier: Arc::clone(&request.specifier),
                source_span: request.span,
                template_range: (request.span.lo - span.lo) as usize
                    ..(request.span.hi - span.lo) as usize,
                role: request.role,
            })
            .collect::<Vec<_>>();
        let raw_requests: Arc<[DeclarationRequestFact]> = raw_requests.into();
        let (template, requests, ambient_template, ambient_requests) = if matches!(
            kind,
            DeclarationItemKind::Enum | DeclarationItemKind::Namespace
        ) && !exported
        {
            let (template, requests) = prefix_template("declare ", raw_template, &raw_requests);
            (
                template,
                requests,
                Arc::from(raw_template),
                Arc::clone(&raw_requests),
            )
        } else {
            let template: Arc<str> = Arc::from(raw_template);
            (
                Arc::clone(&template),
                Arc::clone(&raw_requests),
                template,
                Arc::clone(&raw_requests),
            )
        };
        DeclarationItemFact {
            kind,
            source_span: span,
            name: name_span.map(|name| Arc::from(&self.source[name.lo as usize..name.hi as usize])),
            name_span,
            exported,
            default_export,
            has_declare_modifier: false,
            template,
            requests,
            ambient_template,
            ambient_requests,
            contains_forbidden_any: self.any_spans.iter().any(|any| span.contains(*any)),
            import_usage: None,
            overload_binding: None,
        }
    }

    fn trim_span(&self, span: Span) -> Span {
        let source = &self.source[span.lo as usize..span.hi as usize];
        let leading = source.len() - source.trim_start().len();
        let trailing = source.len() - source.trim_end().len();
        Span::new(
            span.lo + leading as u32,
            span.hi.saturating_sub(trailing as u32),
        )
    }

    fn record_program(&mut self, program: &Program<'_>, source_type: SourceType) {
        let mut retained_bindings = program
            .body
            .iter()
            .filter_map(|statement| match statement {
                Statement::ExportDefault(export) => match export.declaration {
                    ExportDefaultKind::Expression(Expression::Identifier(identifier)) => {
                        Some(identifier.name)
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>();
        retained_bindings.extend(self.export_assignment_bindings.iter().copied());
        self.value_references
            .extend(retained_bindings.iter().copied());

        for statement in program.body.iter().copied() {
            self.record_statement(
                statement,
                false,
                false,
                false,
                &retained_bindings,
                source_type,
            );
        }
    }

    fn record_declared_statement(&mut self, statement: Statement<'_>, source_type: SourceType) {
        self.record_statement(statement, false, false, true, &[], source_type);
    }

    fn record_statement(
        &mut self,
        statement: Statement<'_>,
        exported: bool,
        default_export: bool,
        declared: bool,
        default_bindings: &[Atom],
        source_type: SourceType,
    ) {
        let statement_span = self.trim_span(statement.span());
        if self.items.iter().any(|item| {
            item.source_span == statement_span
                && matches!(
                    item.kind,
                    DeclarationItemKind::Enum
                        | DeclarationItemKind::Namespace
                        | DeclarationItemKind::Ambient
                )
        }) {
            return;
        }
        match statement {
            Statement::Empty(_) => {}
            Statement::Import(import) => {
                self.record_if_missing(
                    DeclarationItemKind::Import,
                    import.span,
                    None,
                    false,
                    false,
                );
            }
            Statement::ExportAll(export) => {
                self.record_if_missing(
                    DeclarationItemKind::ReExport,
                    export.span,
                    None,
                    true,
                    false,
                );
            }
            Statement::ExportNamed(export) => {
                if export.source.is_none() {
                    self.value_references
                        .extend(export.specifiers.iter().filter_map(|specifier| {
                            match specifier.local {
                                ModuleExportName::Ident(identifier) => Some(identifier.name),
                                ModuleExportName::String(_) => None,
                            }
                        }));
                }
                if let Some(declaration) = export.declaration {
                    let declaration_span = self.trim_span(declaration.span());
                    if self
                        .items
                        .iter()
                        .any(|item| item.source_span == declaration_span)
                    {
                        self.wrap_last_item(export.span);
                        return;
                    }
                    self.record_statement(
                        declaration,
                        true,
                        false,
                        declared,
                        default_bindings,
                        source_type,
                    );
                } else {
                    self.record_if_missing(
                        DeclarationItemKind::Export,
                        export.span,
                        None,
                        true,
                        false,
                    );
                }
            }
            Statement::ExportDefault(export) => match export.declaration {
                ExportDefaultKind::Function(function) => {
                    self.record_function_item(function, true, true, declared, source_type)
                }
                ExportDefaultKind::Class(class) => {
                    self.record_class_item(class, true, true, declared)
                }
                ExportDefaultKind::Expression(Expression::Identifier(_)) => {
                    self.record_if_missing(
                        DeclarationItemKind::Export,
                        export.span,
                        None,
                        true,
                        true,
                    );
                }
                ExportDefaultKind::Expression(expression) => self.error(
                    expression.span(),
                    "default export expressions need a named declaration",
                ),
            },
            Statement::FunctionDeclaration(function) => {
                self.record_function_item(
                    function,
                    exported,
                    default_export,
                    declared,
                    source_type,
                );
            }
            Statement::VariableDeclaration(variable) => {
                if (self.strict || declared)
                    && variable.declarations.iter().any(|item| item.init.is_some())
                {
                    self.error(
                        variable.span,
                        "declaration variables must not have initializers",
                    );
                }
                for declaration in variable.declarations.iter() {
                    let event = self
                        .variables
                        .iter()
                        .copied()
                        .find(|event| event.span == declaration.span);
                    let is_default = event
                        .and_then(|event| event.name)
                        .is_some_and(|name| default_bindings.contains(&name));
                    if exported
                        || is_default
                        || declared
                        || event.is_some_and(|event| event.annotation.is_some())
                    {
                        if let Some(event) = event {
                            self.record_variable_item(
                                declaration,
                                event,
                                exported,
                                is_default,
                                source_type,
                            );
                        } else {
                            self.error(
                                declaration.span,
                                "parser did not retain variable declaration facts",
                            );
                        }
                    }
                }
            }
            Statement::ClassDeclaration(class) => {
                self.record_class_item(class, exported, default_export, declared);
            }
            other => {
                if self.strict {
                    self.error(
                        other.span(),
                        "declaration module contains an executable top-level statement",
                    );
                }
            }
        }
    }

    fn record_function_item(
        &mut self,
        function: &Function<'_>,
        exported: bool,
        default_export: bool,
        declared: bool,
        source_type: SourceType,
    ) {
        let Some(event) = self
            .functions
            .iter()
            .copied()
            .find(|event| event.span == function.span)
        else {
            self.error(
                function.span,
                "parser did not retain function declaration facts",
            );
            return;
        };
        if event.name_span.is_none() && !default_export {
            self.error(function.span, "function declaration requires a name");
            return;
        }
        if (self.strict || declared) && event.body_span.is_some() {
            self.error(
                function.span,
                "declaration functions must not have implementations",
            );
            return;
        }
        let follows_overload = event.name.is_some_and(|name| {
            self.items
                .iter()
                .rev()
                .find(|item| item.source_span.hi <= event.span.lo)
                .is_some_and(|item| {
                    item.kind == DeclarationItemKind::Function
                        && item.overload_binding == Some(name)
                })
        });
        if event.body_span.is_some() && follows_overload {
            return;
        }
        let header_end = event.body_span.map_or(event.span.hi, |body| body.lo);
        if event.return_type.is_none()
            && event.body_span.is_some()
            && !matches!(source_type, SourceType::Tsx)
        {
            self.error(
                function.span,
                "public function needs an explicit return type",
            );
            return;
        }

        let mut builder = TemplateBuilder::new(self.source, &self.requests, &self.any_spans);
        if default_export {
            builder.push_str("export default ");
        } else if exported {
            builder.push_str("export ");
        } else {
            builder.push_str("declare ");
        }
        // `async` is a runtime modifier and is intentionally excluded. The main parser recorded
        // the accepted `function` keyword span, so no textual search is needed.
        let _ = event.is_async;
        builder.push_span(self.trim_end_span(Span::new(event.keyword_span.lo, header_end)));
        if event.return_type.is_none() && event.body_span.is_some() {
            builder.push_str(": import(\"react\").JSX.Element");
        }
        if !builder.output.ends_with(';') {
            builder.push_str(";");
        }
        let (template, requests, contains_forbidden_any) = builder.finish();
        let (ambient_template, ambient_requests) = if default_export || exported {
            (Arc::clone(&template), Arc::clone(&requests))
        } else {
            remove_known_template_range(&template, &requests, 0.."declare ".len())
        };
        self.items.push(DeclarationItemFact {
            kind: DeclarationItemKind::Function,
            source_span: self.trim_span(function.span),
            name: event
                .name_span
                .map(|name| Arc::from(&self.source[name.lo as usize..name.hi as usize])),
            name_span: event.name_span,
            exported,
            default_export,
            has_declare_modifier: false,
            ambient_template,
            ambient_requests,
            template,
            requests,
            contains_forbidden_any,
            import_usage: None,
            overload_binding: None,
        });
    }

    fn record_variable_item(
        &mut self,
        declaration: &VariableDeclarator<'_>,
        event: PendingVariable,
        exported: bool,
        default_export: bool,
        source_type: SourceType,
    ) {
        let Some(name_span) = event.name_span else {
            self.error(
                declaration.span,
                "public destructuring declarations need an explicit named declaration",
            );
            return;
        };
        if event.annotation.is_none()
            && let Some(initializer) = declaration.init
        {
            match initializer {
                Expression::Identifier(_) | Expression::Member(_) => {
                    if let Some(binding) = export_assignment_root_binding(initializer) {
                        self.record_value_reference(binding);
                    }
                }
                Expression::Conditional(conditional) => {
                    for branch in [conditional.consequent, conditional.alternate] {
                        if let Some(binding) = export_assignment_root_binding(branch) {
                            self.record_value_reference(binding);
                        }
                    }
                }
                _ => {}
            }
        }
        let mut builder = TemplateBuilder::new(self.source, &self.requests, &self.any_spans);
        if exported {
            builder.push_str("export ");
        }
        builder.push_str("declare const ");
        builder.push_span(name_span);
        builder.push_str(": ");
        if let Some(annotation) = event.annotation {
            builder.push_span(self.trim_span(annotation));
        } else if let Some(initializer) = declaration.init {
            let arrow = self
                .arrows
                .iter()
                .copied()
                .find(|arrow| arrow.span == initializer.span());
            let constant = self
                .const_assertions
                .iter()
                .any(|span| event.span.contains(*span));
            if !append_inferred_type(
                &mut builder,
                initializer,
                arrow,
                &self.type_annotations,
                constant,
                source_type,
            ) {
                self.error(
                    declaration.span,
                    format!(
                        "public value `{}` needs an explicit type annotation",
                        &self.source[name_span.lo as usize..name_span.hi as usize]
                    ),
                );
                return;
            }
        } else {
            self.error(
                declaration.span,
                "public value needs an explicit type annotation",
            );
            return;
        }
        builder.push_str(";");
        let (template, requests, contains_forbidden_any) = builder.finish();
        let declaration_start = if exported { "export ".len() } else { 0 };
        let (ambient_template, ambient_requests) = remove_known_template_range(
            &template,
            &requests,
            declaration_start..declaration_start + "declare ".len(),
        );
        self.items.push(DeclarationItemFact {
            kind: DeclarationItemKind::Variable,
            source_span: self.trim_span(event.span),
            name: Some(Arc::from(
                &self.source[name_span.lo as usize..name_span.hi as usize],
            )),
            name_span: Some(name_span),
            exported,
            default_export,
            has_declare_modifier: false,
            ambient_template,
            ambient_requests,
            template,
            requests,
            contains_forbidden_any,
            import_usage: None,
            overload_binding: None,
        });
    }

    fn record_class_item(
        &mut self,
        class: &Class<'_>,
        exported: bool,
        default_export: bool,
        declared: bool,
    ) {
        let Some(event) = self
            .classes
            .iter()
            .copied()
            .find(|event| event.span == class.span)
        else {
            self.error(class.span, "parser did not retain class declaration facts");
            return;
        };
        if event.name_span.is_none() && !default_export {
            self.error(class.span, "class declaration requires a name");
            return;
        }
        if let Some(super_class) = class.super_class
            && let Some(binding) = export_assignment_root_binding(super_class)
        {
            self.record_value_reference(binding);
        }
        let mut members = self
            .class_members
            .iter()
            .copied()
            .filter(|member| {
                event.body_open.hi <= member.span.lo && member.span.hi <= event.body_close.lo
            })
            .collect::<Vec<_>>();
        members.sort_by_key(|member| member.span.lo);
        let overloads = members
            .iter()
            .filter_map(|member| match member.kind {
                PendingClassMemberKind::Method {
                    key,
                    body_span: None,
                    ..
                } => key,
                _ => None,
            })
            .collect::<Vec<_>>();

        for member in &members {
            match member.kind {
                PendingClassMemberKind::Method {
                    method_kind,
                    return_type,
                    body_span,
                    ..
                } => {
                    if (self.strict || declared) && body_span.is_some() {
                        self.error(
                            member.span,
                            "declaration methods must not have implementations",
                        );
                    }
                    if !matches!(method_kind, MethodKind::Constructor | MethodKind::Set)
                        && return_type.is_none()
                    {
                        self.error(
                            member.span,
                            "public class methods need an explicit return type",
                        );
                    }
                }
                PendingClassMemberKind::Property {
                    annotation,
                    initializer_eq,
                } => {
                    if (self.strict || declared) && initializer_eq.is_some() {
                        self.error(
                            member.span,
                            "declaration properties must not have initializers",
                        );
                    }
                    if annotation.is_none() {
                        self.error(
                            member.span,
                            "public class properties need an explicit type annotation",
                        );
                    }
                }
                PendingClassMemberKind::StaticBlock => {
                    if self.strict || declared {
                        self.error(
                            member.span,
                            "declaration classes must not contain static blocks",
                        );
                    }
                }
                PendingClassMemberKind::IndexSignature => {}
            }
        }
        if self
            .errors
            .iter()
            .any(|error| event.span.contains(error.span))
        {
            return;
        }

        let standalone_prefix = if default_export {
            "export default "
        } else if exported {
            "export "
        } else {
            "declare "
        };
        let ambient_prefix = if default_export {
            "export default "
        } else if exported {
            "export "
        } else {
            ""
        };
        let (template, requests, contains_forbidden_any) =
            self.build_class_template(event, &members, &overloads, standalone_prefix);
        let (ambient_template, ambient_requests, ambient_any) =
            self.build_class_template(event, &members, &overloads, ambient_prefix);
        self.items.push(DeclarationItemFact {
            kind: DeclarationItemKind::Class,
            source_span: self.trim_span(class.span),
            name: event
                .name_span
                .map(|name| Arc::from(&self.source[name.lo as usize..name.hi as usize])),
            name_span: event.name_span,
            exported,
            default_export,
            has_declare_modifier: false,
            template,
            requests,
            ambient_template,
            ambient_requests,
            contains_forbidden_any: contains_forbidden_any || ambient_any,
            import_usage: None,
            overload_binding: None,
        });
    }

    fn build_class_template(
        &self,
        class: PendingClass,
        members: &[PendingClassMember],
        overloads: &[Atom],
        prefix: &str,
    ) -> (Arc<str>, Arc<[DeclarationRequestFact]>, bool) {
        let mut builder = TemplateBuilder::new(self.source, &self.requests, &self.any_spans);
        builder.push_str(prefix);
        builder.push_span(Span::new(class.keyword_span.lo, class.body_open.hi));
        for member in members {
            match member.kind {
                PendingClassMemberKind::StaticBlock => continue,
                PendingClassMemberKind::Method { key, body_span, .. }
                    if body_span.is_some() && key.is_some_and(|key| overloads.contains(&key)) =>
                {
                    continue;
                }
                PendingClassMemberKind::Method {
                    body_span,
                    async_span,
                    ..
                } => {
                    builder.push_str("\n  ");
                    let end = body_span.map_or(member.span.hi, |body| body.lo);
                    if let Some(async_span) = async_span {
                        builder.push_span(trim_span_in(
                            self.source,
                            Span::new(member.signature_lo, async_span.lo),
                        ));
                        builder.push_span(trim_span_in(self.source, Span::new(async_span.hi, end)));
                    } else {
                        builder.push_span(trim_span_in(
                            self.source,
                            Span::new(member.signature_lo, end),
                        ));
                    }
                    if !builder.output.ends_with(';') {
                        builder.push_str(";");
                    }
                }
                PendingClassMemberKind::Property { initializer_eq, .. } => {
                    builder.push_str("\n  ");
                    let end = initializer_eq.map_or(member.span.hi, |equal| equal.lo);
                    builder.push_span(trim_span_in(
                        self.source,
                        Span::new(member.signature_lo, end),
                    ));
                    if !builder.output.ends_with(';') {
                        builder.push_str(";");
                    }
                }
                PendingClassMemberKind::IndexSignature => {
                    builder.push_str("\n  ");
                    builder.push_span(trim_span_in(
                        self.source,
                        Span::new(member.signature_lo, member.span.hi),
                    ));
                    if !builder.output.ends_with(';') {
                        builder.push_str(";");
                    }
                }
            }
        }
        builder.push_str("\n}");
        builder.finish()
    }

    fn record_if_missing(
        &mut self,
        kind: DeclarationItemKind,
        span: Span,
        name_span: Option<Span>,
        exported: bool,
        default_export: bool,
    ) {
        if self
            .items
            .iter()
            .any(|item| item.source_span == self.trim_span(span))
        {
            return;
        }
        self.record_source_item(kind, span, name_span, exported, default_export);
    }

    fn trim_end_span(&self, span: Span) -> Span {
        let source = &self.source[span.lo as usize..span.hi as usize];
        let trailing = source.len() - source.trim_end().len();
        Span::new(span.lo, span.hi.saturating_sub(trailing as u32))
    }

    pub(crate) fn finish(mut self) -> Result<DeclarationFacts, DeclarationFactError> {
        if self.strict {
            if let Some(span) = self
                .type_annotations
                .iter()
                .copied()
                .find(|span| span.is_empty())
            {
                self.errors.push(DeclarationFactError::new(
                    span,
                    "type annotation must not be empty",
                ));
            }
            if self.reject_any
                && let Some(span) = self.any_spans.first().copied()
            {
                self.errors.push(DeclarationFactError::new(
                    span,
                    "declaration type must not contain `any`",
                ));
            }
        }
        if let Some(error) = self.errors.into_iter().next() {
            return Err(error);
        }
        let type_references = &self.type_references;
        let value_references = &self.value_references;
        let imports = &self.imports;
        self.items.retain_mut(|item| {
            if item.kind != DeclarationItemKind::Import {
                return true;
            }
            let Some(import) = imports
                .iter()
                .find(|import| import.span == item.source_span)
            else {
                item.import_usage = Some(DeclarationImportUsage::ReferencedValue);
                return true;
            };
            if import.always_keep {
                item.import_usage = Some(DeclarationImportUsage::TypeOnly);
                true
            } else if import.bindings.is_empty() {
                item.import_usage = Some(DeclarationImportUsage::RuntimeSideEffect);
                true
            } else if import.bindings.iter().any(|binding| {
                type_references.contains(binding) || value_references.contains(binding)
            }) {
                item.import_usage = Some(if import.type_only {
                    DeclarationImportUsage::TypeOnly
                } else {
                    DeclarationImportUsage::ReferencedValue
                });
                true
            } else {
                false
            }
        });
        for item in &mut self.items {
            item.overload_binding = None;
        }
        self.items.sort_by_key(|item| item.source_span.lo);
        Ok(DeclarationFacts {
            source: Arc::from(self.source),
            items: self.items.into(),
        })
    }
}

struct TemplateBuilder<'a> {
    source: &'a str,
    pending_requests: &'a [PendingRequest],
    any_spans: &'a [Span],
    output: String,
    requests: Vec<DeclarationRequestFact>,
    contains_forbidden_any: bool,
}

impl<'a> TemplateBuilder<'a> {
    fn new(source: &'a str, pending_requests: &'a [PendingRequest], any_spans: &'a [Span]) -> Self {
        Self {
            source,
            pending_requests,
            any_spans,
            output: String::new(),
            requests: Vec::new(),
            contains_forbidden_any: false,
        }
    }

    fn push_str(&mut self, value: &str) {
        self.output.push_str(value);
    }

    fn push_span(&mut self, span: Span) {
        let output_start = self.output.len();
        self.output
            .push_str(&self.source[span.lo as usize..span.hi as usize]);
        self.contains_forbidden_any |= self.any_spans.iter().any(|any| span.contains(*any));
        self.requests.extend(
            self.pending_requests
                .iter()
                .filter(|request| span.contains(request.span))
                .map(|request| DeclarationRequestFact {
                    specifier: Arc::clone(&request.specifier),
                    source_span: request.span,
                    template_range: output_start + (request.span.lo - span.lo) as usize
                        ..output_start + (request.span.hi - span.lo) as usize,
                    role: request.role,
                }),
        );
    }

    fn finish(mut self) -> (Arc<str>, Arc<[DeclarationRequestFact]>, bool) {
        self.requests
            .sort_by_key(|request| request.template_range.start);
        (
            Arc::from(self.output),
            self.requests.into(),
            self.contains_forbidden_any,
        )
    }
}

fn append_inferred_type(
    builder: &mut TemplateBuilder<'_>,
    expression: Expression<'_>,
    arrow: Option<PendingArrow>,
    annotations: &[Span],
    constant: bool,
    source_type: SourceType,
) -> bool {
    match expression {
        Expression::Arrow(value) => {
            let Some(arrow) = arrow else {
                return false;
            };
            let annotated_parameters = annotations
                .iter()
                .filter(|annotation| arrow.signature_span.contains(**annotation))
                .count();
            if annotated_parameters < value.params.len() {
                return false;
            }
            let mut signature = trim_span_in(builder.source, arrow.signature_span);
            if arrow.is_async {
                signature.lo = (signature.lo + "async".len() as u32).min(signature.hi);
                signature = trim_span_in(builder.source, signature);
            }
            builder.push_span(signature);
            builder.push_str(" => ");
            if let Some(return_type) = arrow.return_type {
                builder.push_span(trim_span_in(builder.source, return_type));
            } else if matches!(source_type, SourceType::Tsx) {
                if arrow.is_async {
                    builder.push_str("Promise<import(\"react\").JSX.Element>");
                } else {
                    builder.push_str("import(\"react\").JSX.Element");
                }
            } else {
                return false;
            }
            true
        }
        Expression::Identifier(_) | Expression::Member(_) => {
            builder.push_str("typeof ");
            builder.push_span(expression.span());
            true
        }
        Expression::Conditional(conditional)
            if matches!(
                conditional.consequent,
                Expression::Identifier(_) | Expression::Member(_)
            ) && matches!(
                conditional.alternate,
                Expression::Identifier(_) | Expression::Member(_)
            ) =>
        {
            builder.push_str("typeof ");
            builder.push_span(conditional.consequent.span());
            builder.push_str(" | typeof ");
            builder.push_span(conditional.alternate.span());
            true
        }
        Expression::Call(call)
            if matches!(call.callee, Expression::Identifier(_))
                && &builder.source
                    [call.callee.span().lo as usize..call.callee.span().hi as usize]
                    == "defineTokens"
                && call.arguments.len() == 1 =>
        {
            if let Some(value) = infer_static_type(call.arguments[0], builder.source, false, true) {
                builder.push_str(&value);
                true
            } else {
                false
            }
        }
        _ => {
            if let Some(value) = infer_static_type(expression, builder.source, true, constant) {
                builder.push_str(&value);
                true
            } else {
                false
            }
        }
    }
}

fn infer_static_type(
    expression: Expression<'_>,
    source: &str,
    literal: bool,
    constant: bool,
) -> Option<String> {
    match expression {
        Expression::StringLiteral(value) => Some(if literal {
            source[value.span.lo as usize..value.span.hi as usize].to_string()
        } else {
            "string".to_string()
        }),
        Expression::NumberLiteral(value) => Some(if literal {
            source[value.span.lo as usize..value.span.hi as usize].to_string()
        } else {
            "number".to_string()
        }),
        Expression::BigIntLiteral(value) => Some(if literal {
            source[value.span.lo as usize..value.span.hi as usize].to_string()
        } else {
            "bigint".to_string()
        }),
        Expression::BooleanLiteral(value) => Some(if literal {
            value.value.to_string()
        } else {
            "boolean".to_string()
        }),
        Expression::NullLiteral(_) => Some("null".to_string()),
        Expression::TemplateLiteral(_) => Some("string".to_string()),
        Expression::Object(object) => {
            let mut members = Vec::new();
            for member in object.properties.iter() {
                let ObjectMember::Property(property) = member else {
                    return None;
                };
                if property.kind != PropertyKind::Init || property.method || property.computed {
                    return None;
                }
                let key = property_key_span(property.key)?;
                let key = &source[key.lo as usize..key.hi as usize];
                let value = infer_static_type(property.value, source, false, true)?;
                members.push(format!("readonly {key}: {value}"));
            }
            Some(format!("{{ {}; }}", members.join("; ")))
        }
        Expression::Array(array) => {
            let values = array
                .elements
                .iter()
                .map(|element| {
                    element
                        .as_ref()
                        .copied()
                        .and_then(|value| infer_static_type(value, source, constant, constant))
                })
                .collect::<Option<Vec<_>>>()?;
            if constant {
                Some(format!("readonly [{}]", values.join(", ")))
            } else {
                let values = values.into_iter().collect::<BTreeSet<_>>();
                Some(format!(
                    "Array<{}>",
                    values.into_iter().collect::<Vec<_>>().join(" | ")
                ))
            }
        }
        _ => None,
    }
}

fn property_key_span(key: PropertyKey<'_>) -> Option<Span> {
    match key {
        PropertyKey::Ident(identifier) | PropertyKey::Private(identifier) => Some(identifier.span),
        PropertyKey::String(value) => Some(value.span),
        PropertyKey::Number(value) => Some(value.span),
        PropertyKey::Computed(_) => None,
    }
}

fn export_assignment_root_binding(expression: Expression<'_>) -> Option<Atom> {
    match expression {
        Expression::Identifier(identifier) => Some(identifier.name),
        Expression::Member(member)
            if !member.optional && matches!(member.property, MemberProperty::Ident(_)) =>
        {
            export_assignment_root_binding(member.object)
        }
        _ => None,
    }
}

fn trim_span_in(source: &str, span: Span) -> Span {
    let value = &source[span.lo as usize..span.hi as usize];
    let leading = value.len() - value.trim_start().len();
    let trailing = value.len() - value.trim_end().len();
    Span::new(
        span.lo + leading as u32,
        span.hi.saturating_sub(trailing as u32),
    )
}

fn remove_known_template_range(
    template: &str,
    requests: &[DeclarationRequestFact],
    removed: Range<usize>,
) -> (Arc<str>, Arc<[DeclarationRequestFact]>) {
    debug_assert!(removed.start <= removed.end && removed.end <= template.len());
    debug_assert!(requests.iter().all(|request| {
        request.template_range.end <= removed.start || removed.end <= request.template_range.start
    }));
    let mut output = String::with_capacity(template.len() - (removed.end - removed.start));
    output.push_str(&template[..removed.start]);
    output.push_str(&template[removed.end..]);
    let delta = removed.end - removed.start;
    let requests = requests
        .iter()
        .cloned()
        .map(|mut request| {
            if request.template_range.start >= removed.end {
                request.template_range.start -= delta;
                request.template_range.end -= delta;
            }
            request
        })
        .collect::<Vec<_>>();
    (Arc::from(output), requests.into())
}

fn prefix_template(
    prefix: &str,
    template: &str,
    requests: &[DeclarationRequestFact],
) -> (Arc<str>, Arc<[DeclarationRequestFact]>) {
    let mut output = String::with_capacity(prefix.len() + template.len());
    output.push_str(prefix);
    output.push_str(template);
    let requests = requests
        .iter()
        .cloned()
        .map(|mut request| {
            request.template_range.start += prefix.len();
            request.template_range.end += prefix.len();
            request
        })
        .collect::<Vec<_>>();
    (Arc::from(output), requests.into())
}

impl<'a, 'src, const LOWER: bool> Parser<'a, 'src, LOWER> {
    pub(crate) fn declaration_record_import(
        &mut self,
        span: Span,
        bindings: impl IntoIterator<Item = Atom>,
        always_keep: bool,
        type_only: bool,
    ) {
        if let Some(collector) = &mut self.declaration {
            collector.record_import(span, bindings, always_keep, type_only);
        }
    }

    pub(crate) fn declaration_record_type_reference(&mut self, binding: Atom) {
        if let Some(collector) = &mut self.declaration {
            collector.record_type_reference(binding);
        }
    }

    pub(crate) fn declaration_record_value_reference(&mut self, binding: Atom) {
        if let Some(collector) = &mut self.declaration {
            collector.record_value_reference(binding);
        }
    }

    pub(crate) fn declaration_type_scope_mark(&self) -> Option<usize> {
        self.declaration
            .as_ref()
            .map(DeclarationCollector::type_scope_mark)
    }

    pub(crate) fn declaration_type_reference_mark(&self) -> Option<usize> {
        self.declaration
            .as_ref()
            .map(DeclarationCollector::type_reference_mark)
    }

    pub(crate) fn declaration_activate_type_bindings_since(
        &mut self,
        reference_mark: Option<usize>,
        bindings: &[Atom],
    ) {
        if let (Some(collector), Some(reference_mark)) = (&mut self.declaration, reference_mark) {
            collector.activate_type_bindings_since(reference_mark, bindings);
        }
    }

    pub(crate) fn declaration_record_type_binding(&mut self, binding: Atom) {
        if let Some(collector) = &mut self.declaration {
            collector.record_type_binding(binding);
        }
    }

    pub(crate) fn declaration_restore_type_scope(&mut self, mark: Option<usize>) {
        if let (Some(collector), Some(mark)) = (&mut self.declaration, mark) {
            collector.restore_type_scope(mark);
        }
    }

    pub(crate) fn declaration_value_scope_mark(&self) -> Option<usize> {
        self.declaration
            .as_ref()
            .map(DeclarationCollector::value_scope_mark)
    }

    pub(crate) fn declaration_value_reference_mark(&self) -> Option<usize> {
        self.declaration
            .as_ref()
            .map(DeclarationCollector::value_reference_mark)
    }

    pub(crate) fn declaration_activate_value_bindings_since(
        &mut self,
        reference_mark: Option<usize>,
        bindings: &[Atom],
    ) {
        if let (Some(collector), Some(reference_mark)) = (&mut self.declaration, reference_mark) {
            collector.activate_value_bindings_since(reference_mark, bindings);
        }
    }

    pub(crate) fn declaration_restore_value_scope(&mut self, mark: Option<usize>) {
        if let (Some(collector), Some(mark)) = (&mut self.declaration, mark) {
            collector.restore_value_scope(mark);
        }
    }

    pub(crate) fn declaration_begin_infer_scope(&mut self) {
        if let Some(collector) = &mut self.declaration {
            collector.begin_infer_scope();
        }
    }

    pub(crate) fn declaration_record_infer_binding(&mut self, binding: Atom) -> bool {
        self.declaration
            .as_mut()
            .is_some_and(|collector| collector.record_infer_binding(binding))
    }

    pub(crate) fn declaration_activate_infer_scope(&mut self) -> Option<usize> {
        self.declaration
            .as_mut()
            .map(DeclarationCollector::activate_infer_scope)
    }

    pub(crate) fn declaration_record_request(&mut self, span: Span, role: DeclarationRequestRole) {
        let specifier: Arc<str> = Arc::from(self.lexer.string_value(span).as_ref());
        if let Some(collector) = &mut self.declaration {
            collector.record_request(specifier, span, role);
        }
    }

    pub(crate) fn declaration_record_export_assignment(
        &mut self,
        span: Span,
        expression: Expression<'a>,
    ) {
        let root_binding = export_assignment_root_binding(expression);
        if let Some(collector) = &mut self.declaration {
            if let Some(root_binding) = root_binding {
                collector.record_export_assignment(span, root_binding);
            } else {
                collector.error(
                    expression.span(),
                    "declaration export assignment requires an identifier or dotted name",
                );
            }
        }
    }

    pub(crate) fn declaration_record_source_item(
        &mut self,
        kind: DeclarationItemKind,
        span: Span,
        name_span: Option<Span>,
    ) {
        if let Some(collector) = &mut self.declaration {
            collector.record_source_item(kind, span, name_span, false, false);
        }
    }

    pub(crate) fn declaration_record_export_item(&mut self, kind: DeclarationItemKind, span: Span) {
        if let Some(collector) = &mut self.declaration {
            collector.record_source_item(kind, span, None, true, false);
        }
    }

    pub(crate) fn declaration_wrap_last_item(&mut self, span: Span) {
        if let Some(collector) = &mut self.declaration {
            collector.wrap_last_item(span);
        }
    }

    pub(crate) fn declaration_begin_type(&mut self) {
        if let Some(collector) = &mut self.declaration {
            collector.begin_type();
        }
    }

    pub(crate) fn declaration_end_type(&mut self) {
        if let Some(collector) = &mut self.declaration {
            collector.end_type();
        }
    }

    pub(crate) fn declaration_in_type(&self) -> bool {
        self.declaration
            .as_ref()
            .is_some_and(DeclarationCollector::in_type)
    }

    pub(crate) fn declaration_requires_strict_type_syntax(&self) -> bool {
        self.declaration
            .as_ref()
            .is_some_and(|collector| collector.strict)
    }

    pub(crate) fn declaration_record_any(&mut self, span: Span) {
        if let Some(collector) = &mut self.declaration {
            collector.record_any(span);
        }
    }

    pub(crate) fn declaration_record_implicit_any(&mut self, span: Span) {
        if let Some(collector) = &mut self.declaration {
            collector.record_implicit_any(span);
        }
    }

    pub(crate) fn declaration_record_type_annotation(&mut self, span: Span) {
        if let Some(collector) = &mut self.declaration {
            collector.record_annotation(span);
        }
    }

    pub(crate) fn declaration_record_function(
        &mut self,
        span: Span,
        keyword_span: Span,
        name_span: Option<Span>,
        name: Option<Atom>,
        return_type: Option<Span>,
        body_span: Option<Span>,
        is_async: bool,
    ) {
        if let Some(collector) = &mut self.declaration {
            collector.record_function(
                span,
                keyword_span,
                name_span,
                name,
                return_type,
                body_span,
                is_async,
            );
        }
    }

    pub(crate) fn declaration_record_function_overload(&mut self, span: Span) {
        if let Some(collector) = &mut self.declaration {
            collector.record_function_overload(span);
        }
    }

    pub(crate) fn declaration_record_variable(
        &mut self,
        span: Span,
        name_span: Option<Span>,
        name: Option<Atom>,
        annotation: Option<Span>,
    ) {
        if let Some(collector) = &mut self.declaration {
            collector.record_variable(span, name_span, name, annotation);
        }
    }

    pub(crate) fn declaration_record_arrow(
        &mut self,
        span: Span,
        signature_span: Span,
        return_type: Option<Span>,
        is_async: bool,
    ) {
        if let Some(collector) = &mut self.declaration {
            collector.record_arrow(span, signature_span, return_type, is_async);
        }
    }

    pub(crate) fn declaration_record_const_assertion(&mut self, span: Span) {
        if let Some(collector) = &mut self.declaration {
            collector.record_const_assertion(span);
        }
    }

    pub(crate) fn declaration_record_class(
        &mut self,
        span: Span,
        keyword_span: Span,
        name_span: Option<Span>,
        body_open: Span,
        body_close: Span,
    ) {
        if let Some(collector) = &mut self.declaration {
            collector.record_class(span, keyword_span, name_span, body_open, body_close);
        }
    }

    pub(crate) fn declaration_record_class_method(
        &mut self,
        span: Span,
        signature_lo: u32,
        key: Option<Atom>,
        method_kind: MethodKind,
        return_type: Option<Span>,
        body_span: Option<Span>,
        async_span: Option<Span>,
    ) {
        if let Some(collector) = &mut self.declaration {
            collector.record_class_method(
                span,
                signature_lo,
                key,
                method_kind,
                return_type,
                body_span,
                async_span,
            );
        }
    }

    pub(crate) fn declaration_record_class_property(
        &mut self,
        span: Span,
        signature_lo: u32,
        annotation: Option<Span>,
        initializer_eq: Option<Span>,
    ) {
        if let Some(collector) = &mut self.declaration {
            collector.record_class_property(span, signature_lo, annotation, initializer_eq);
        }
    }

    pub(crate) fn declaration_record_class_index(&mut self, span: Span, signature_lo: u32) {
        if let Some(collector) = &mut self.declaration {
            collector.record_class_index(span, signature_lo);
        }
    }

    pub(crate) fn declaration_record_class_static_block(&mut self, span: Span, signature_lo: u32) {
        if let Some(collector) = &mut self.declaration {
            collector.record_class_static_block(span, signature_lo);
        }
    }

    pub(crate) fn declaration_is_collecting(&self) -> bool {
        self.declaration.is_some()
    }

    pub(crate) fn declaration_item_mark(&self) -> Option<usize> {
        self.declaration
            .as_ref()
            .map(DeclarationCollector::item_mark)
    }

    pub(crate) fn declaration_mark_declared_since(&mut self, mark: Option<usize>, declare_lo: u32) {
        if let (Some(collector), Some(mark)) = (&mut self.declaration, mark) {
            collector.mark_declared_since(mark, declare_lo);
        }
    }

    pub(crate) fn declaration_discard_items_since(&mut self, mark: Option<usize>) {
        if let (Some(collector), Some(mark)) = (&mut self.declaration, mark) {
            collector.discard_items_since(mark);
        }
    }

    pub(crate) fn declaration_record_declared_statement(&mut self, statement: Statement<'a>) {
        let source_type = self.source_type;
        if let Some(collector) = &mut self.declaration {
            collector.record_declared_statement(statement, source_type);
        }
    }
}

/// Parse one TypeScript declaration-producing module with the normal parser and freeze its facts.
///
/// This accepts ordinary implementation modules. Use [`validate_declaration_module`] when the
/// source is required to be a declaration body containing no executable implementation.
pub fn parse_declaration_facts(
    source: &str,
    source_type: SourceType,
) -> Result<DeclarationFacts, DeclarationFactError> {
    parse_with_collector(source, source_type, false, false)
}

/// Strictly validate a declaration body and return the same immutable facts used for rendering.
pub fn validate_declaration_module(
    source: &str,
    source_type: SourceType,
) -> Result<DeclarationFacts, DeclarationFactError> {
    parse_with_collector(source, source_type, true, true)
}

/// Strictly validate declaration-only syntax while retaining the parser's typed `any` fact.
///
/// This is intended for product layers that must classify an otherwise-valid declaration body
/// separately from a public-API policy failure without parsing the source twice. Callers inspect
/// [`DeclarationFacts::contains_forbidden_any`] on the returned facts.
pub fn validate_declaration_module_allow_any(
    source: &str,
    source_type: SourceType,
) -> Result<DeclarationFacts, DeclarationFactError> {
    parse_with_collector(source, source_type, true, false)
}

fn parse_with_collector(
    source: &str,
    source_type: SourceType,
    strict: bool,
    reject_any: bool,
) -> Result<DeclarationFacts, DeclarationFactError> {
    if !source_type.is_typescript() {
        return Err(DeclarationFactError::new(
            Span::DUMMY,
            "declaration facts require TypeScript or TSX source",
        ));
    }

    let interner = Interner::new();
    let arena = Bump::new();
    let mut parser = Parser::<false>::new(
        source,
        &interner,
        &arena,
        source_type,
        ParseOptions::default(),
    );
    parser.declaration = Some(DeclarationCollector::new(source, strict, reject_any));
    let program = parser.parse_program();
    let mut collector = parser
        .declaration
        .take()
        .expect("declaration collector remains installed");
    collector.record_program(&program, source_type);
    parser.declaration = Some(collector);
    if parser
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.is_error())
    {
        return Err(DeclarationFactError::new(
            parser.cur.span,
            "source contains invalid TypeScript syntax",
        ));
    }
    parser
        .declaration
        .take()
        .expect("declaration collector remains installed")
        .finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_line_declarations_are_owned_by_the_main_parser() {
        let facts = parse_declaration_facts(
            "export interface A { value: string } export type B = import('./b').B;",
            SourceType::TypeScript,
        )
        .unwrap();

        assert_eq!(facts.items().len(), 2);
        assert_eq!(facts.items()[0].kind(), DeclarationItemKind::Interface);
        assert_eq!(facts.items()[1].kind(), DeclarationItemKind::TypeAlias);
        let requests = facts.requests().collect::<Vec<_>>();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].specifier(), "./b");
        assert_eq!(
            requests[0].role(),
            DeclarationRequestRole::ImportTypeExpression
        );
    }

    #[test]
    fn interface_member_name_any_is_not_a_forbidden_type() {
        validate_declaration_module(
            "export interface Allowed { any?: string; any(): void; any?(): void; nested: [string?] }",
            SourceType::TypeScript,
        )
        .unwrap();

        let error = validate_declaration_module(
            "export interface Rejected { any?: string; value: any }",
            SourceType::TypeScript,
        )
        .unwrap_err();
        assert!(error.message().contains("must not contain `any`"));

        let error = validate_declaration_module(
            "export type OptionalAny = [any?];",
            SourceType::TypeScript,
        )
        .unwrap_err();
        assert!(error.message().contains("must not contain `any`"));

        let error = validate_declaration_module(
            "export type InterpolatedAny = `${any}`;",
            SourceType::TypeScript,
        )
        .unwrap_err();
        assert!(error.message().contains("must not contain `any`"));
    }

    #[test]
    fn speculative_type_requests_do_not_escape_checkpoint() {
        let facts = parse_declaration_facts(
            "type Imported = import('./types').T; const pending = async<T>(import('./runtime'));",
            SourceType::TypeScript,
        )
        .unwrap();
        let requests = facts.requests().collect::<Vec<_>>();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].specifier(), "./types");
    }

    #[test]
    fn import_equals_requests_and_roles_are_parser_owned() {
        let facts = parse_declaration_facts(
            "import Value = require('./value'); import type Model = require(\"./model\"); import Alias = Namespace.Member; export interface Public { value: Value; model: Model; alias: Alias; }",
            SourceType::TypeScript,
        )
        .unwrap();
        let imports = facts
            .items()
            .iter()
            .filter(|item| item.kind() == DeclarationItemKind::Import)
            .collect::<Vec<_>>();
        assert_eq!(imports.len(), 3);
        let requests = facts.requests().collect::<Vec<_>>();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].specifier(), "./value");
        assert_eq!(requests[0].role(), DeclarationRequestRole::ImportValue);
        assert_eq!(requests[1].specifier(), "./model");
        assert_eq!(requests[1].role(), DeclarationRequestRole::ImportType);
    }

    #[test]
    fn export_assignment_is_a_parser_owned_declaration_fact() {
        let facts = parse_declaration_facts(
            "const api = { version: '1' }; export = api;",
            SourceType::TypeScript,
        )
        .unwrap();
        assert!(facts.items().iter().any(|item| {
            item.kind() == DeclarationItemKind::Variable
                && item.template().starts_with("declare const api:")
        }));
        let assignment = facts
            .items()
            .iter()
            .find(|item| item.kind() == DeclarationItemKind::Ambient)
            .unwrap();
        assert_eq!(assignment.template(), "export = api;");
        assert_eq!(assignment.ambient_template(), "export = api;");

        validate_declaration_module_allow_any(
            "declare namespace Api { interface Value { ok: true; } } export = Api.Value;",
            SourceType::TypeScript,
        )
        .unwrap();
        for invalid in ["export = run();", "export = { value: true };"] {
            assert!(
                parse_declaration_facts(invalid, SourceType::TypeScript).is_err(),
                "invalid export assignment unexpectedly produced facts: {invalid}"
            );
            assert!(
                validate_declaration_module_allow_any(invalid, SourceType::TypeScript).is_err(),
                "invalid export assignment unexpectedly validated: {invalid}"
            );
        }
    }

    #[test]
    fn request_facts_ignore_attribute_and_export_name_literals() {
        let facts = parse_declaration_facts(
            "import type from './value.js' with { type: 'json' }; export default type; export { 'source-name' as 'public-name' } from \"./dep.js\" with { type: 'json' };",
            SourceType::TypeScript,
        )
        .unwrap();
        let requests = facts.requests().collect::<Vec<_>>();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].specifier(), "./value.js");
        assert_eq!(requests[0].role(), DeclarationRequestRole::ImportValue);
        assert_eq!(requests[1].specifier(), "./dep.js");
        assert_eq!(requests[1].role(), DeclarationRequestRole::ExportValue);
    }

    #[test]
    fn request_facts_expose_cooked_value_and_exact_quoted_span() {
        let source = r#"import type { Value } from './de\u0070.js'; export interface Public { value: Value; }"#;
        let facts = parse_declaration_facts(source, SourceType::TypeScript).unwrap();
        let request = facts.requests().next().unwrap();
        assert_eq!(request.specifier(), "./dep.js");
        let item = &facts.items()[0];
        assert_eq!(
            &item.template()[request.template_range()],
            r#"'./de\u0070.js'"#
        );
    }

    #[test]
    fn declaration_imports_are_selected_by_parser_owned_type_references() {
        let facts = parse_declaration_facts(
            r#"
                import './runtime.scss';
                import logo from './logo.png';
                import styles from './styles.scss';
                import { Model } from './model.js';
                import type { Token } from './token.js';
                const runtimeImage = logo;
                export interface Public { model: Model; styles: string; token?: Token; }
                export type Extra = import('./extra.js').Value;
                export { Shared } from './shared.js';
            "#,
            SourceType::TypeScript,
        )
        .unwrap();

        let imports = facts
            .items()
            .iter()
            .filter(|item| item.kind() == DeclarationItemKind::Import)
            .collect::<Vec<_>>();
        assert_eq!(imports.len(), 3);
        assert!(imports.iter().any(|item| {
            item.template().contains("runtime.scss")
                && item.import_usage() == Some(DeclarationImportUsage::RuntimeSideEffect)
        }));
        assert!(imports.iter().any(|item| {
            item.template().contains("./model.js")
                && item.import_usage() == Some(DeclarationImportUsage::ReferencedValue)
        }));
        assert!(imports.iter().any(|item| {
            item.template().contains("./token.js")
                && item.import_usage() == Some(DeclarationImportUsage::TypeOnly)
        }));
        assert!(!facts.items().iter().any(|item| {
            item.template().contains("logo.png") || item.template().contains("styles.scss")
        }));
        assert!(
            facts
                .requests()
                .any(|request| request.specifier() == "./extra.js")
        );
        assert!(
            facts
                .requests()
                .any(|request| request.specifier() == "./shared.js")
        );
    }

    #[test]
    fn type_export_alias_does_not_retain_an_import_matching_only_the_public_name() {
        let facts = parse_declaration_facts(
            "import type { Foo } from './foo.js'; import type { Public } from './public.js'; export type { Foo as Public };",
            SourceType::TypeScript,
        )
        .unwrap();

        let imports = facts
            .items()
            .iter()
            .filter(|item| item.kind() == DeclarationItemKind::Import)
            .map(DeclarationItemFact::template)
            .collect::<Vec<_>>();
        assert!(imports.iter().any(|item| item.contains("./foo.js")));
        assert!(!imports.iter().any(|item| item.contains("./public.js")));
    }

    #[test]
    fn generic_type_bindings_shadow_same_named_imports() {
        let facts = parse_declaration_facts(
            r#"
                import { T } from './shadowed.js';
                import { FunctionValue } from './shadowed-function.js';
                import { ClassValue } from './shadowed-class.js';
                import { ArrowValue } from './shadowed-arrow.js';
                import { Model } from './model.js';
                export type Box<T> = T;
                export function identity<FunctionValue>(value: FunctionValue): FunctionValue {
                    return value;
                }
                export class Store<ClassValue> { value: ClassValue; }
                export const arrow = <ArrowValue,>(value: ArrowValue): ArrowValue => value;
                export interface Public<Value> { model: Model; value: Value; }
            "#,
            SourceType::TypeScript,
        )
        .unwrap();

        assert!(!facts.items().iter().any(|item| {
            item.kind() == DeclarationItemKind::Import && item.template().contains("./shadowed")
        }));
        assert!(facts.items().iter().any(|item| {
            item.kind() == DeclarationItemKind::Import
                && item.template().contains("./model.js")
                && item.import_usage() == Some(DeclarationImportUsage::ReferencedValue)
        }));
    }

    #[test]
    fn type_and_value_namespaces_and_local_type_bindings_are_distinct() {
        let facts = parse_declaration_facts(
            r#"
                import { ValueQuery } from './value-query.js';
                import { LocalInfer } from './local-infer.js';
                import { PatternInfer } from './pattern-infer.js';
                import { FalseInfer } from './false-infer.js';
                import { LocalKey } from './local-key.js';
                import { ConstraintKey } from './constraint-key.js';
                import { AfterKey } from './after-key.js';
                import { ParameterValue } from './parameter-value.js';
                import { SignatureValue } from './signature-value.js';
                import { ArrowSignatureValue } from './arrow-signature-value.js';
                import { AsyncArrowValue } from './async-arrow-value.js';
                import { SingleArrowValue } from './single-arrow-value.js';
                import { AsyncSingleArrowValue } from './async-single-arrow-value.js';
                import { AfterArrowValue } from './after-arrow-value.js';
                import type { ForwardType } from './forward-type.js';
                import { type InlineForward } from './inline-forward.js';
                export type Query<ValueQuery> = typeof ValueQuery;
                export type Local<T> = T extends infer LocalInfer ? LocalInfer : never;
                export type Pattern<T> = T extends [infer PatternInfer, PatternInfer] ? never : never;
                export type FalseBranch<T> = T extends infer FalseInfer ? never : FalseInfer;
                export type Mapped<Keys> = { [LocalKey in Keys as LocalKey]: LocalKey };
                export type Constraint = { [ConstraintKey in ConstraintKey]: never };
                export type After<Keys> = { [AfterKey in Keys]: never } | AfterKey;
                export declare function inspect(first: typeof ParameterValue, ParameterValue: string): typeof ParameterValue;
                export declare function forward<T extends ForwardType, ForwardType>(): void;
                export type InlineShadow<InlineForward> = InlineForward;
                export type Callback = (first: typeof SignatureValue, SignatureValue: string) => typeof SignatureValue;
                export const callback = (first: typeof ArrowSignatureValue, ArrowSignatureValue: string): typeof ArrowSignatureValue => ArrowSignatureValue;
                export const asyncCallback = async <T,>(first: typeof AsyncArrowValue, { value: AsyncArrowValue }: { value: string }): Promise<typeof AsyncArrowValue> => AsyncArrowValue;
                export const single: (value: string) => string = SingleArrowValue => (null as typeof SingleArrowValue);
                export const asyncSingle: (value: string) => Promise<string> = async AsyncSingleArrowValue => (null as typeof AsyncSingleArrowValue);
                export type AfterArrow = typeof AfterArrowValue;
            "#,
            SourceType::TypeScript,
        )
        .unwrap();

        let retained_imports = facts
            .items()
            .iter()
            .filter(|item| item.kind() == DeclarationItemKind::Import)
            .map(DeclarationItemFact::template)
            .collect::<Vec<_>>();
        for retained in [
            "value-query.js",
            "pattern-infer.js",
            "false-infer.js",
            "constraint-key.js",
            "after-key.js",
            "after-arrow-value.js",
        ] {
            assert!(
                retained_imports.iter().any(|item| item.contains(retained)),
                "missing external namespace reference {retained}: {retained_imports:?}"
            );
        }
        for shadowed in [
            "local-infer.js",
            "local-key.js",
            "parameter-value.js",
            "signature-value.js",
            "arrow-signature-value.js",
            "async-arrow-value.js",
            "single-arrow-value.js",
            "async-single-arrow-value.js",
            "forward-type.js",
            "inline-forward.js",
        ] {
            assert!(
                !retained_imports.iter().any(|item| item.contains(shadowed)),
                "retained locally shadowed reference {shadowed}: {retained_imports:?}"
            );
        }
    }

    #[test]
    fn strict_declaration_validation_rejects_runtime_statements() {
        let error = validate_declaration_module("run();", SourceType::TypeScript).unwrap_err();
        assert!(error.message().contains("executable top-level"));
    }

    #[test]
    fn implementation_functions_and_public_values_have_typed_templates() {
        let facts = parse_declaration_facts(
            r#"
                function overloaded(value: string): string;
                function overloaded(value: number): number;
                function overloaded(value: string | number): string | number { return value; }
                export const scalar = 1e-6;
                export const tuple = ['a', 'b'] as const;
                export const tokens = { color: 'red', nested: { gap: 4 } };
                export const render = ({ title }: { title: string }) => <span>{title}</span>;
            "#,
            SourceType::Tsx,
        )
        .unwrap();
        let templates = facts
            .items()
            .iter()
            .map(DeclarationItemFact::template)
            .collect::<Vec<_>>();
        assert!(templates.contains(&"declare function overloaded(value: string): string;"));
        assert!(templates.contains(&"declare function overloaded(value: number): number;"));
        assert!(
            !templates
                .iter()
                .any(|item| item.contains("string | number)"))
        );
        assert!(templates.contains(&"export declare const scalar: 1e-6;"));
        assert!(templates.contains(&"export declare const tuple: readonly ['a', 'b'];"));
        assert!(templates.contains(&
            "export declare const tokens: { readonly color: string; readonly nested: { readonly gap: number; }; };"
        ));
        assert!(templates.contains(&
            "export declare const render: ({ title }: { title: string }) => import(\"react\").JSX.Element;"
        ));
    }

    #[test]
    fn overload_suppression_is_scoped_to_the_current_parser_item_stream() {
        let facts = parse_declaration_facts(
            r#"
                namespace Nested {
                    export function load(value: string): string;
                    export function load(value: string): string { return value; }
                }
                export function load(value: number): number { return value; }
            "#,
            SourceType::TypeScript,
        )
        .unwrap_err();
        assert!(
            facts
                .message()
                .contains("declaration functions must not have implementations")
        );

        let facts = parse_declaration_facts(
            r#"
                declare namespace Nested { export function load(value: string): string; }
                export function load(value: number): number { return value; }
            "#,
            SourceType::TypeScript,
        )
        .unwrap();
        assert!(facts.items().iter().any(|item| {
            item.kind() == DeclarationItemKind::Function
                && item.template() == "export function load(value: number): number;"
        }));
    }

    #[test]
    fn class_members_are_projected_from_parser_owned_boundaries() {
        let facts = parse_declaration_facts(
            r#"
                export default class Service<T> implements Disposable {
                    readonly value: T = createValue();
                    method(input: T): import('./types').Result { return use(input); }
                    overloaded(input: string): string;
                    overloaded(input: number): number;
                    overloaded(input: string | number): string | number { return input; }
                    [key: string]: unknown;
                }
            "#,
            SourceType::TypeScript,
        )
        .unwrap();
        let class = facts
            .items()
            .iter()
            .find(|item| item.kind() == DeclarationItemKind::Class)
            .unwrap();
        assert!(
            class
                .template()
                .starts_with("export default class Service<T>")
        );
        assert!(class.template().contains("readonly value: T;"));
        assert!(
            class
                .template()
                .contains("method(input: T): import('./types').Result;")
        );
        assert_eq!(class.template().matches("overloaded(").count(), 2);
        assert!(class.template().contains("[key: string]: unknown;"));
        assert!(!class.template().contains("createValue"));
        assert!(!class.template().contains("return use"));
        assert_eq!(class.requests().len(), 1);
    }

    #[test]
    fn strict_class_signatures_pass_but_implementations_fail() {
        validate_declaration_module(
            "export class Valid { value: string; method(input: string): void; }",
            SourceType::TypeScript,
        )
        .unwrap();
        let error = validate_declaration_module(
            "export class Invalid { method(input: string): void {} }",
            SourceType::TypeScript,
        )
        .unwrap_err();
        assert!(error.message().contains("must not have implementations"));
    }

    #[test]
    fn declared_items_have_standalone_and_ambient_templates() {
        let facts = validate_declaration_module(
            r#"
                export declare function load(value: string): import('./types').Result;
                export declare class Client { request(): Promise<void>; }
                export declare const version: string;
                declare function helper(): void;
            "#,
            SourceType::TypeScript,
        )
        .unwrap();
        let templates = facts
            .items()
            .iter()
            .map(|item| (item.template(), item.ambient_template()))
            .collect::<Vec<_>>();
        assert!(templates.contains(&(
            "export declare function load(value: string): import('./types').Result;",
            "export function load(value: string): import('./types').Result;"
        )));
        assert!(templates.contains(&(
            "export declare class Client {\n  request(): Promise<void>;\n}",
            "export class Client {\n  request(): Promise<void>;\n}"
        )));
        assert!(templates.contains(&(
            "export declare const version: string;",
            "export const version: string;"
        )));
        assert!(templates.contains(&(
            "declare function helper(): void;",
            "function helper(): void;"
        )));
        let load = facts
            .items()
            .iter()
            .find(|item| item.name() == Some("load"))
            .unwrap();
        assert!(load.has_declare_modifier());
        assert_eq!(load.requests().len(), 1);
        assert_eq!(load.ambient_requests().len(), 1);
        assert_ne!(
            load.requests()[0].template_range(),
            load.ambient_requests()[0].template_range()
        );
    }

    #[test]
    fn declaration_containers_have_distinct_standalone_and_ambient_templates() {
        let facts = validate_declaration_module(
            "declare namespace Types { interface Value { ok: true; } } declare enum Mode { One } declare global { interface Window { ready: true; } }",
            SourceType::TypeScript,
        )
        .unwrap();
        let namespace = facts
            .items()
            .iter()
            .find(|item| item.kind() == DeclarationItemKind::Namespace)
            .unwrap();
        assert!(namespace.template().starts_with("declare namespace Types"));
        assert!(namespace.ambient_template().starts_with("namespace Types"));
        let enumeration = facts
            .items()
            .iter()
            .find(|item| item.kind() == DeclarationItemKind::Enum)
            .unwrap();
        assert!(enumeration.template().starts_with("declare enum Mode"));
        assert!(enumeration.ambient_template().starts_with("enum Mode"));
        let global = facts
            .items()
            .iter()
            .find(|item| item.kind() == DeclarationItemKind::Ambient)
            .unwrap();
        assert!(global.template().starts_with("declare global"));
        assert!(global.ambient_template().starts_with("global"));
    }

    #[test]
    fn strict_allow_any_returns_policy_fact_without_reparsing() {
        let facts = validate_declaration_module_allow_any(
            "export interface Public { value: any; }",
            SourceType::TypeScript,
        )
        .unwrap();
        assert!(facts.contains_forbidden_any());
        assert!(
            validate_declaration_module(
                "export interface Public { value: any; }",
                SourceType::TypeScript,
            )
            .is_err()
        );
        assert!(
            validate_declaration_module_allow_any(
                "export const invalid: string = run();",
                SourceType::TypeScript,
            )
            .is_err()
        );

        let implicit_any = validate_declaration_module_allow_any(
            "export interface Unsafe { value; method(input); (input); new (input); } export function load(input);",
            SourceType::TypeScript,
        )
        .unwrap();
        assert!(implicit_any.contains_forbidden_any());
        assert!(
            validate_declaration_module(
                "export interface Unsafe { value; method(input); (input); new (input); } export function load(input);",
                SourceType::TypeScript,
            )
            .is_err()
        );
        let explicit = validate_declaration_module_allow_any(
            "export interface Safe { value: string; method(input: string): void; (input: string): number; new (input: string): Safe; set current(input: string); }",
            SourceType::TypeScript,
        )
        .unwrap();
        assert!(!explicit.contains_forbidden_any());
    }

    #[test]
    fn strict_validation_rejects_unclosed_type_and_class_delimiters() {
        for source in [
            "export interface Broken { value: string;",
            "export type Broken = [string, number;",
            "export type Broken = import('./types';",
            "export class Broken { value: string;",
        ] {
            assert!(
                validate_declaration_module_allow_any(source, SourceType::TypeScript).is_err(),
                "unclosed declaration unexpectedly validated: {source}"
            );
        }

        validate_declaration_module_allow_any(
            "export interface Closed { value: string; } export type Pair = [string, number]; export type Imported = import('./types').Value; export class Complete { value: string; }",
            SourceType::TypeScript,
        )
        .unwrap();
    }

    #[test]
    fn strict_validation_rejects_missing_required_declaration_tokens() {
        for source in [
            "export interface {}",
            "export interface MissingBody",
            "export type = string;",
            "export type MissingEquals string;",
            "export type Empty = ;",
            "export function (): void;",
            "export const : string;",
            "export type EmptyParameters<> = string;",
            "export type MissingParameter<,> = string;",
            "export type ParameterHole<T,, U> = string;",
            "export type EmptyArguments = Array<>;",
            "export type ArgumentHole = Map<string,, number>;",
            "export type MissingInfer<T> = T extends infer ? string : number;",
            "export type ExecutableImport = import(run()).Value;",
            "export type MissingNegativeLiteral = -;",
            "export type InvalidConst<const T> = T;",
            "export type OptionalIndex = { [key: string]?: boolean };",
            "export type ParameterProperty = (public value: string) => void;",
            "export type InvalidReadonlyRemoval = { -readonly value: string };",
            "export interface InvalidReadonlyAddition { +readonly value: string; }",
            "type Keys = 'key'; export type MixedMapped<T> = { [Key in Keys]: T; extra: string };",
            "type Keys = 'key'; export interface MappedInterface { [Key in Keys]: string; }",
        ] {
            assert!(
                validate_declaration_module_allow_any(source, SourceType::TypeScript).is_err(),
                "incomplete declaration unexpectedly validated: {source}"
            );
        }

        validate_declaration_module_allow_any(
            "export interface Consumer<in Input> { consume(value: Input): void; destructure({ value }: { value: Input }): void; } export interface Producer<out Output> { produce(): Output; } export declare function tuple<const Values extends readonly unknown[]>(values: Values): Values; export declare class Box<const Value> { constructor(public value: Value); map<const Output>(value: Output): Output; } export type Imported = import('pkg', { with: { 'resolution-mode': 'import' } }).Value; export type Negative = -1n; export type Destructured = ({ value }: { value: string }) => void;",
            SourceType::TypeScript,
        )
        .unwrap();
    }

    #[test]
    fn strict_validation_rejects_balanced_invalid_type_members() {
        for source in [
            "export interface Broken { value: string = run(); }",
            "export interface Broken { load(): void { run(); } }",
            "export type Broken = { value: string = run(); };",
            "export type Broken = { load(): void { run(); } };",
            "export type Broken = { [key = run()]: string };",
            "export type Broken = [value: string = run()];",
            "export type Broken = Value[lookup()];",
        ] {
            assert!(
                validate_declaration_module_allow_any(source, SourceType::TypeScript).is_err(),
                "balanced invalid declaration unexpectedly validated: {source}"
            );
        }
    }

    #[test]
    fn strict_validation_accepts_structured_type_members() {
        validate_declaration_module_allow_any(
            r#"
                export interface Valid<Value> {
                    readonly value?: Value;
                    readonly?: string;
                    readonly?(): void;
                    1n: string;
                    map<Output = Value>(input: Value, ...rest: readonly Value[]): Output;
                    (input: Value): Value;
                    new (input: Value): Valid<Value>;
                    [key: string]: Value;
                }
                export type Accessors<Value> = {
                    +readonly [Key in keyof Value as `get${Capitalize<string & Key>}`]-?:
                        (value: Value[Key]) => Value[Key];
                };
                export type Values<Value> = [head: Value, optional?: string, ...rest: number[]];
            "#,
            SourceType::TypeScript,
        )
        .unwrap();
    }

    #[test]
    fn local_annotated_values_are_kept_for_exported_type_references() {
        let facts = parse_declaration_facts(
            "const internal: unique symbol = Symbol(); export type Public = typeof internal;",
            SourceType::TypeScript,
        )
        .unwrap();
        assert!(facts.items().iter().any(|item| {
            item.kind() == DeclarationItemKind::Variable
                && item.template() == "declare const internal: unique symbol;"
        }));
    }

    #[test]
    fn strict_validation_reaches_ambient_container_members() {
        validate_declaration_module(
            "declare namespace Safe { const value: string; function load(): void; }",
            SourceType::TypeScript,
        )
        .unwrap();

        let error = validate_declaration_module(
            "declare namespace Unsafe { const value: string = run(); }",
            SourceType::TypeScript,
        )
        .unwrap_err();
        assert!(error.message().contains("must not have initializers"));

        let error = validate_declaration_module(
            "declare module 'remote' { function load(): void { run(); } }",
            SourceType::TypeScript,
        )
        .unwrap_err();
        assert!(error.message().contains("must not have implementations"));

        let error = validate_declaration_module(
            "declare global { const value: string = run(); }",
            SourceType::TypeScript,
        )
        .unwrap_err();
        assert!(error.message().contains("must not have initializers"));
    }
}
