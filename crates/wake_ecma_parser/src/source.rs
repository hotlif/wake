//! Source facts emitted by the existing grammar before erasure and lowering. Node IDs are
//! local to one parse and are not a public plugin ABI. Error-recovery trees are not fix inputs.
//!
//! Import locals, including unaliased named imports, are binding occurrences. A declaration-wide
//! or inline TypeScript `type` modifier makes that local a TypeBinding; value imports remain
//! ValueBinding. Imported external names are not local bindings or runtime references.
//! Original exports retain both local/exported names, type modifiers, sources and attributes.
//! Only named exports without `from` contribute local identifier references; aliases and
//! external re-exports do not. Erased declarations keep a source target range, not a fake AST.
//! Type references, operators, array suffixes and indexed accesses retain original grammar
//! ranges; references own their type-argument node and arrays never include indexed-access keys.
//! JSX values retain decoded attribute/text literals, normalized child text and original
//! expression spans. Dynamic expressions stay Unknown; ordinary parsing allocates no value list.
//! Original function regions exclude method keys/decorators from parameter scope and retain
//! erased overload headers and concise-arrow body ranges. Speculation rolls them back together.
//! Statement terminators retain explicit tokens or ASI insertion points and a conservative
//! omission proof from the grammar. For separators, empty statements and type members are excluded.
//! Import-type expressions retain literal requests and structured attributes before erasure;
//! source collection validates this grammar and rolls facts back with speculative type parsing.
//! Assertions retain their full original expression, operand and optional type ranges before
//! erasure: `as`, angle assertions, non-null tails and `satisfies` are distinct. Parentheses,
//! chained assertions and relational precedence belong to these grammar-owned UTF-8 ranges.
//! They participate in speculative rollback and allocate no collection in ordinary compilation.
//! Calls, constructor applications, tagged templates and template substitutions retain original
//! argument ranges. A call's head covers its callee plus optional/type-argument syntax, excluding
//! a constructor's `new`. Generated JSX/namespace calls are never recorded as source calls.
//! Dynamic import's arguments allow `in` inside a for initializer and an optional trailing comma.
//! Switch facts retain proven primitive case values and original CaseClause ranges so a type
//! service can bind enum-member case expressions without exposing the backend AST.

use wake_common::{Atom, Interner, Span};
use wake_ecma_ast::{
    Expression, ImportAttributeKey, ImportAttributes, ModuleExportName, SourceExport,
    SourceExportKind, SourceExportName, SourceExportSpecifier, SourceIdentifier,
    SourceIdentifierRole, SourceImport, SourceImportAttribute, SourceImportAttributes,
    SourceImportBinding, SourceImportBindingKind, SourceJsxValue, SourceModuleSpecifier,
    SourcePrimitiveValue, SourceValueBindingKind,
};
pub use wake_ecma_ast::{SourceNode, SourceNodeKind};
use wake_ecma_lexer::{Token, TokenKind};

use crate::{Comment, ParseOptions, ParseOutput, SourceType, parse_with_mode};

pub struct SourceParseOutput {
    pub parsed: ParseOutput,
    pub comments: Vec<Comment>,
    pub syntax: Vec<SourceNode>,
    pub tokens: Vec<SourceToken>,
    pub identifiers: Vec<SourceIdentifier>,
    pub imports: Vec<SourceImport>,
    pub type_imports: Vec<wake_ecma_ast::SourceTypeImport>,
    pub exports: Vec<SourceExport>,
    pub jsx_values: Vec<wake_ecma_ast::SourceJsxValue>,
    pub arrays: Vec<wake_ecma_ast::SourceArray>,
    pub functions: Vec<wake_ecma_ast::SourceFunction>,
    pub type_scopes: Vec<wake_ecma_ast::SourceTypeScope>,
    pub type_declarations: Vec<wake_ecma_ast::SourceTypeDeclaration>,
    pub namespaces: Vec<wake_ecma_ast::SourceNamespace>,
    pub terminators: Vec<wake_ecma_ast::SourceTerminator>,
    pub lists: Vec<wake_ecma_ast::SourceList>,
    pub assertions: Vec<wake_ecma_ast::SourceTypeAssertion>,
    pub calls: Vec<wake_ecma_ast::SourceCall>,
    pub awaits: Vec<wake_ecma_ast::SourceAwait>,
    pub expression_statements: Vec<wake_ecma_ast::SourceExpressionStatement>,
    pub assignments: Vec<wake_ecma_ast::SourceAssignment>,
    pub callbacks: Vec<wake_ecma_ast::SourceCallback>,
    pub returns: Vec<wake_ecma_ast::SourceReturn>,
    pub conditions: Vec<wake_ecma_ast::SourceCondition>,
    pub switches: Vec<wake_ecma_ast::SourceSwitch>,
    pub templates: Vec<wake_ecma_ast::SourceTemplate>,
    pub members: Vec<wake_ecma_ast::SourceMember>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceTokenContext {
    /// Ordinary JavaScript tokens, including erased TypeScript syntax.
    JavaScript,
    JsxTag,
    JsxText,
}

/// A token consumed by the parser's committed grammar path, without trivia or EOF.
/// JSX names can include hyphens and retain their entire raw spelling in a single token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceToken {
    pub kind: TokenKind,
    pub span: Span,
    pub newline_before: bool,
    pub context: SourceTokenContext,
}

/// Parse once, retaining comments, JSX grammar facts and erased TypeScript type ranges in
/// addition to compilation output and committed tokens. This is not a full JS/TS source AST;
/// balanced-skipped type interiors remain opaque ranges. Tokens never include lookahead or
/// synthetic lowering output. Ordinary parsing does not allocate the source collections.
pub fn parse_source(
    source: &str,
    interner: &Interner,
    source_type: SourceType,
    options: ParseOptions<'_>,
) -> SourceParseOutput {
    let mut capture = SourceCapture::new(true);
    let parsed = if options.transform_features.is_empty() {
        parse_with_mode::<false>(source, interner, source_type, options, Some(&mut capture))
    } else {
        parse_with_mode::<true>(source, interner, source_type, options, Some(&mut capture))
    };
    SourceParseOutput {
        parsed,
        comments: capture.comments,
        syntax: capture.syntax,
        tokens: capture.tokens,
        identifiers: capture.identifiers,
        imports: capture.imports,
        type_imports: capture.type_imports,
        exports: capture.exports,
        jsx_values: capture.jsx_values,
        arrays: capture.arrays,
        functions: capture.functions,
        type_scopes: capture.type_scopes,
        type_declarations: capture.type_declarations,
        namespaces: capture.namespaces,
        terminators: capture.terminators,
        lists: capture.lists,
        assertions: capture.assertions,
        calls: capture.calls,
        awaits: capture.awaits,
        expression_statements: capture.expression_statements,
        assignments: capture.assignments,
        callbacks: capture.callbacks,
        returns: capture.returns,
        conditions: capture.conditions,
        switches: capture.switches,
        templates: capture.templates,
        members: capture.members,
    }
}

pub(crate) struct SourceCapture {
    pub comments: Vec<Comment>,
    pub syntax: Vec<SourceNode>,
    pub tokens: Vec<SourceToken>,
    pub identifiers: Vec<SourceIdentifier>,
    pub imports: Vec<SourceImport>,
    pub type_imports: Vec<wake_ecma_ast::SourceTypeImport>,
    pub exports: Vec<SourceExport>,
    pub jsx_values: Vec<wake_ecma_ast::SourceJsxValue>,
    pub arrays: Vec<wake_ecma_ast::SourceArray>,
    pub functions: Vec<wake_ecma_ast::SourceFunction>,
    pub type_scopes: Vec<wake_ecma_ast::SourceTypeScope>,
    pub type_declarations: Vec<wake_ecma_ast::SourceTypeDeclaration>,
    pub namespaces: Vec<wake_ecma_ast::SourceNamespace>,
    pub terminators: Vec<wake_ecma_ast::SourceTerminator>,
    pub lists: Vec<wake_ecma_ast::SourceList>,
    pub assertions: Vec<wake_ecma_ast::SourceTypeAssertion>,
    pub calls: Vec<wake_ecma_ast::SourceCall>,
    pub awaits: Vec<wake_ecma_ast::SourceAwait>,
    pub expression_statements: Vec<wake_ecma_ast::SourceExpressionStatement>,
    pub assignments: Vec<wake_ecma_ast::SourceAssignment>,
    pub callbacks: Vec<wake_ecma_ast::SourceCallback>,
    pub returns: Vec<wake_ecma_ast::SourceReturn>,
    pub conditions: Vec<wake_ecma_ast::SourceCondition>,
    pub switches: Vec<wake_ecma_ast::SourceSwitch>,
    pub templates: Vec<wake_ecma_ast::SourceTemplate>,
    pub members: Vec<wake_ecma_ast::SourceMember>,
    pub collect_syntax: bool,
}

impl SourceCapture {
    pub fn new(collect_syntax: bool) -> Self {
        Self {
            comments: Vec::new(),
            syntax: Vec::new(),
            tokens: Vec::new(),
            identifiers: Vec::new(),
            imports: Vec::new(),
            type_imports: Vec::new(),
            exports: Vec::new(),
            jsx_values: Vec::new(),
            arrays: Vec::new(),
            functions: Vec::new(),
            type_scopes: Vec::new(),
            type_declarations: Vec::new(),
            namespaces: Vec::new(),
            terminators: Vec::new(),
            lists: Vec::new(),
            assertions: Vec::new(),
            calls: Vec::new(),
            awaits: Vec::new(),
            expression_statements: Vec::new(),
            assignments: Vec::new(),
            callbacks: Vec::new(),
            returns: Vec::new(),
            conditions: Vec::new(),
            switches: Vec::new(),
            templates: Vec::new(),
            members: Vec::new(),
            collect_syntax,
        }
    }
}

#[derive(Default)]
pub(crate) struct SourceCollector {
    pub nodes: Vec<SourceNode>,
    pub tokens: Vec<SourceToken>,
    pub identifiers: Vec<SourceIdentifier>,
    pub imports: Vec<SourceImport>,
    pub type_imports: Vec<wake_ecma_ast::SourceTypeImport>,
    pub exports: Vec<SourceExport>,
    pub jsx_values: Vec<SourceJsxValue>,
    pub arrays: Vec<wake_ecma_ast::SourceArray>,
    pub functions: Vec<wake_ecma_ast::SourceFunction>,
    pub type_scopes: Vec<wake_ecma_ast::SourceTypeScope>,
    pub type_declarations: Vec<wake_ecma_ast::SourceTypeDeclaration>,
    pub namespaces: Vec<wake_ecma_ast::SourceNamespace>,
    pub terminators: Vec<wake_ecma_ast::SourceTerminator>,
    pub lists: Vec<wake_ecma_ast::SourceList>,
    pub assertions: Vec<wake_ecma_ast::SourceTypeAssertion>,
    pub calls: Vec<wake_ecma_ast::SourceCall>,
    pub awaits: Vec<wake_ecma_ast::SourceAwait>,
    pub expression_statements: Vec<wake_ecma_ast::SourceExpressionStatement>,
    pub assignments: Vec<wake_ecma_ast::SourceAssignment>,
    pub callbacks: Vec<wake_ecma_ast::SourceCallback>,
    pub returns: Vec<wake_ecma_ast::SourceReturn>,
    pub conditions: Vec<wake_ecma_ast::SourceCondition>,
    pub switches: Vec<wake_ecma_ast::SourceSwitch>,
    pub templates: Vec<wake_ecma_ast::SourceTemplate>,
    pub members: Vec<wake_ecma_ast::SourceMember>,
    pub active_type_scopes: Vec<usize>,
    pending_infer: Vec<SourceIdentifier>,
    infer_marks: Vec<usize>,
    import_stack: Vec<usize>,
    export_stack: Vec<usize>,
    stack: Vec<usize>,
}

#[derive(Clone, Copy)]
pub(crate) struct SourceMark {
    nodes: usize,
    tokens: usize,
    identifiers: usize,
    imports: usize,
    type_imports: usize,
    import_stack: usize,
    exports: usize,
    export_stack: usize,
    jsx_values: usize,
    arrays: usize,
    functions: usize,
    type_scopes: usize,
    type_declarations: usize,
    namespaces: usize,
    terminators: usize,
    lists: usize,
    assertions: usize,
    calls: usize,
    awaits: usize,
    expression_statements: usize,
    assignments: usize,
    callbacks: usize,
    returns: usize,
    conditions: usize,
    switches: usize,
    templates: usize,
    members: usize,
    active_type_scopes: usize,
    pending_infer: usize,
    infer_marks: usize,
    stack: usize,
}

impl SourceCollector {
    fn type_scope_begin(&mut self, kind: wake_ecma_ast::SourceTypeScopeKind, lo: u32) -> usize {
        let index = self.type_scopes.len();
        self.type_scopes.push(wake_ecma_ast::SourceTypeScope {
            kind,
            span: Span::new(lo, lo),
            parent: self.active_type_scopes.last().copied(),
            bindings: Vec::new(),
        });
        self.active_type_scopes.push(index);
        index
    }

    pub fn restore_type_scopes(&mut self, mark: usize, hi: u32) {
        for index in self.active_type_scopes.drain(mark..) {
            let scope = &mut self.type_scopes[index];
            scope.span.hi = hi.max(scope.span.lo);
        }
    }

    pub fn mark(&self) -> SourceMark {
        SourceMark {
            nodes: self.nodes.len(),
            tokens: self.tokens.len(),
            identifiers: self.identifiers.len(),
            imports: self.imports.len(),
            type_imports: self.type_imports.len(),
            import_stack: self.import_stack.len(),
            exports: self.exports.len(),
            export_stack: self.export_stack.len(),
            jsx_values: self.jsx_values.len(),
            arrays: self.arrays.len(),
            functions: self.functions.len(),
            type_scopes: self.type_scopes.len(),
            type_declarations: self.type_declarations.len(),
            namespaces: self.namespaces.len(),
            terminators: self.terminators.len(),
            lists: self.lists.len(),
            assertions: self.assertions.len(),
            calls: self.calls.len(),
            awaits: self.awaits.len(),
            expression_statements: self.expression_statements.len(),
            assignments: self.assignments.len(),
            callbacks: self.callbacks.len(),
            returns: self.returns.len(),
            conditions: self.conditions.len(),
            switches: self.switches.len(),
            templates: self.templates.len(),
            members: self.members.len(),
            active_type_scopes: self.active_type_scopes.len(),
            pending_infer: self.pending_infer.len(),
            infer_marks: self.infer_marks.len(),
            stack: self.stack.len(),
        }
    }

    pub fn rewind(&mut self, mark: SourceMark) {
        self.nodes.truncate(mark.nodes);
        self.tokens.truncate(mark.tokens);
        self.identifiers.truncate(mark.identifiers);
        self.imports.truncate(mark.imports);
        self.type_imports.truncate(mark.type_imports);
        self.import_stack.truncate(mark.import_stack);
        self.exports.truncate(mark.exports);
        self.export_stack.truncate(mark.export_stack);
        self.jsx_values.truncate(mark.jsx_values);
        self.arrays.truncate(mark.arrays);
        self.functions.truncate(mark.functions);
        self.type_scopes.truncate(mark.type_scopes);
        self.type_declarations.truncate(mark.type_declarations);
        self.namespaces.truncate(mark.namespaces);
        self.terminators.truncate(mark.terminators);
        self.lists.truncate(mark.lists);
        self.assertions.truncate(mark.assertions);
        self.calls.truncate(mark.calls);
        self.awaits.truncate(mark.awaits);
        self.expression_statements
            .truncate(mark.expression_statements);
        self.assignments.truncate(mark.assignments);
        self.callbacks.truncate(mark.callbacks);
        self.returns.truncate(mark.returns);
        self.conditions.truncate(mark.conditions);
        self.switches.truncate(mark.switches);
        self.templates.truncate(mark.templates);
        self.members.truncate(mark.members);
        self.active_type_scopes.truncate(mark.active_type_scopes);
        self.pending_infer.truncate(mark.pending_infer);
        self.infer_marks.truncate(mark.infer_marks);
        self.stack.truncate(mark.stack);
    }

    fn begin(&mut self, kind: SourceNodeKind, lo: u32) {
        let index = self.nodes.len();
        self.nodes.push(SourceNode {
            kind,
            span: Span::new(lo, lo),
            parent: self.stack.last().copied(),
        });
        self.stack.push(index);
    }

    fn end(&mut self, hi: u32) {
        let index = self
            .stack
            .pop()
            .expect("source grammar scopes are balanced");
        self.nodes[index].span.hi = hi.max(self.nodes[index].span.lo);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SourceListMark {
    kind: wake_ecma_ast::SourceListKind,
    open: Span,
    tokens: usize,
}

impl<const LOWER: bool> crate::Parser<'_, '_, LOWER> {
    pub(crate) fn source_member(&mut self, member: wake_ecma_ast::SourceMember) {
        if let Some(source) = &mut self.source_syntax {
            source.members.push(member);
        }
    }

    pub(crate) fn source_call(
        &mut self,
        kind: wake_ecma_ast::SourceCallKind,
        span: Span,
        head: Span,
        optional: bool,
        arguments: Vec<Span>,
    ) {
        if let Some(source) = &mut self.source_syntax {
            source.calls.push(wake_ecma_ast::SourceCall {
                kind,
                span,
                head,
                optional,
                arguments,
            });
        }
    }

    pub(crate) fn source_await(&mut self, span: Span, argument: Span) {
        if let Some(source) = &mut self.source_syntax {
            source
                .awaits
                .push(wake_ecma_ast::SourceAwait { span, argument });
        }
    }

    pub(crate) fn source_expression_statement(&mut self, span: Span, expression: Span) {
        if let Some(source) = &mut self.source_syntax {
            source
                .expression_statements
                .push(wake_ecma_ast::SourceExpressionStatement { span, expression });
        }
    }

    pub(crate) fn source_assignment(
        &mut self,
        kind: wake_ecma_ast::SourceAssignmentKind,
        span: Span,
        target: Span,
        value: Span,
    ) {
        if let Some(source) = &mut self.source_syntax {
            source.assignments.push(wake_ecma_ast::SourceAssignment {
                kind,
                span,
                target,
                value,
            });
        }
    }

    pub(crate) fn source_callback(
        &mut self,
        kind: wake_ecma_ast::SourceCallbackKind,
        span: Span,
        value: Span,
    ) {
        if let Some(source) = &mut self.source_syntax {
            source
                .callbacks
                .push(wake_ecma_ast::SourceCallback { kind, span, value });
        }
    }

    pub(crate) fn source_return(&mut self, span: Span, argument: Span) {
        if let Some(source) = &mut self.source_syntax {
            source
                .returns
                .push(wake_ecma_ast::SourceReturn { span, argument });
        }
    }

    pub(crate) fn source_condition(
        &mut self,
        kind: wake_ecma_ast::SourceConditionKind,
        span: Span,
        test: Span,
    ) {
        if let Some(source) = &mut self.source_syntax {
            let condition = wake_ecma_ast::SourceCondition { kind, span, test };
            if !source.conditions.contains(&condition) {
                source.conditions.push(condition);
            }
        }
    }

    /// Preserve expression-level truthiness sites for typed Promise misuse checks. Logical
    /// coalescing is excluded because it selects on nullishness rather than boolean truthiness;
    /// nested conditionals and logical operands retain their own original expression ranges.
    pub(crate) fn source_condition_expression(&mut self, expression: Expression<'_>) {
        match expression {
            Expression::Conditional(conditional) => {
                self.source_condition(
                    wake_ecma_ast::SourceConditionKind::Conditional,
                    conditional.span,
                    conditional.test.span(),
                );
                self.source_condition_expression(conditional.test);
            }
            Expression::Logical(logical)
                if logical.operator != wake_ecma_ast::LogicalOperator::Coalesce =>
            {
                self.source_logical_condition_operand(logical.span, logical.left);
                self.source_logical_condition_operand(logical.span, logical.right);
            }
            Expression::Sequence(sequence) => {
                for expression in sequence.expressions.iter().copied() {
                    self.source_condition_expression(expression);
                }
            }
            _ => {}
        }
    }

    fn source_logical_condition_operand(&mut self, owner: Span, expression: Expression<'_>) {
        match expression {
            Expression::Logical(logical)
                if logical.operator != wake_ecma_ast::LogicalOperator::Coalesce =>
            {
                self.source_logical_condition_operand(logical.span, logical.left);
                self.source_logical_condition_operand(logical.span, logical.right);
            }
            Expression::Conditional(conditional) => {
                self.source_condition(
                    wake_ecma_ast::SourceConditionKind::Conditional,
                    conditional.span,
                    conditional.test.span(),
                );
                self.source_condition_expression(conditional.test);
            }
            _ => self.source_condition(
                wake_ecma_ast::SourceConditionKind::Logical,
                owner,
                expression.span(),
            ),
        }
    }

    pub(crate) fn source_switch(
        &mut self,
        span: Span,
        discriminant: Span,
        has_default: bool,
        cases: Vec<SourcePrimitiveValue>,
        case_spans: Vec<Span>,
        case_clause_spans: Vec<Span>,
    ) {
        if let Some(source) = &mut self.source_syntax {
            source.switches.push(wake_ecma_ast::SourceSwitch {
                span,
                discriminant,
                has_default,
                cases,
                case_spans,
                case_clause_spans,
            });
        }
    }

    pub(crate) fn source_template(&mut self, span: Span, tagged: bool, expressions: Vec<Span>) {
        if let Some(source) = &mut self.source_syntax {
            source.templates.push(wake_ecma_ast::SourceTemplate {
                span,
                tagged,
                expressions,
            });
        }
    }

    pub(crate) fn source_tagged_call(&mut self, span: Span, head: Span, quasi: Span) {
        if let Some(source) = &mut self.source_syntax {
            let template = source
                .templates
                .last()
                .expect("template precedes its tagged call");
            assert_eq!(template.span, quasi);
            source.calls.push(wake_ecma_ast::SourceCall {
                kind: wake_ecma_ast::SourceCallKind::TaggedTemplate,
                span,
                head,
                optional: false,
                arguments: template.expressions.clone(),
            });
        }
    }

    pub(crate) fn source_assertion(
        &mut self,
        kind: wake_ecma_ast::SourceAssertionKind,
        span: Span,
        operand: Span,
        type_span: Option<Span>,
        is_const: bool,
    ) {
        if let Some(source) = &mut self.source_syntax {
            source.assertions.push(wake_ecma_ast::SourceTypeAssertion {
                kind,
                span,
                operand,
                type_span,
                is_const,
            });
        }
    }

    pub(crate) fn source_list_start(
        &self,
        kind: wake_ecma_ast::SourceListKind,
    ) -> Option<SourceListMark> {
        self.source_syntax.as_ref().map(|source| SourceListMark {
            kind,
            open: self.cur.span,
            tokens: source.tokens.len(),
        })
    }

    pub(crate) fn source_list_finish(
        &mut self,
        mark: Option<SourceListMark>,
        can_trail: bool,
    ) -> Option<usize> {
        let mark = mark?;
        let source = self.source_syntax.as_mut()?;
        let mut close = self.cur.span;
        if mark.kind == wake_ecma_ast::SourceListKind::TypeParameters {
            // A nested type list may split a shift token; only the first `>` closes this list.
            close.hi = close.lo + 1;
        }
        let tokens = &source.tokens[mark.tokens..];
        let comma = tokens
            .last()
            .filter(|token| token.kind == TokenKind::Comma)
            .map(|token| token.span);
        let last = tokens
            .get(
                tokens
                    .len()
                    .checked_sub(if comma.is_some() { 2 } else { 1 })?,
            )
            .filter(|token| token.span != mark.open && token.kind != TokenKind::Comma)
            .map(|token| token.span);
        let index = source.lists.len();
        source.lists.push(wake_ecma_ast::SourceList {
            kind: mark.kind,
            open: mark.open,
            close,
            last,
            comma,
            can_trail,
            must_trail: false,
        });
        Some(index)
    }

    pub(crate) fn source_list_parameters(&mut self, index: Option<usize>) {
        if let Some(index) = index
            && let Some(source) = &mut self.source_syntax
        {
            source.lists[index].kind = wake_ecma_ast::SourceListKind::Parameters;
        }
    }

    pub(crate) fn source_list_require_comma(&mut self, index: Option<usize>) {
        if let Some(index) = index
            && let Some(source) = &mut self.source_syntax
        {
            source.lists[index].must_trail = true;
        }
    }

    pub(crate) fn source_type_declaration_name(
        &mut self,
        span: Span,
        name: Atom,
        in_own_scope: bool,
    ) {
        if let Some(collector) = &mut self.source_syntax {
            let node = *collector
                .stack
                .last()
                .expect("type declaration owns its node");
            let type_only = matches!(
                collector.nodes[node].kind,
                SourceNodeKind::TsTypeAlias | SourceNodeKind::TsInterface
            );
            let value_kind = match collector.nodes[node].kind {
                SourceNodeKind::JsClass => Some(SourceValueBindingKind::Class),
                SourceNodeKind::TsEnum => Some(SourceValueBindingKind::Const),
                _ => None,
            };
            let name = SourceIdentifier {
                span,
                name: self.interner.with_resolved(name, str::to_owned),
                role: if type_only {
                    SourceIdentifierRole::TypeBinding
                } else {
                    SourceIdentifierRole::ValueBinding
                },
                value_kind,
            };
            if type_only {
                collector.identifiers.push(name.clone());
            } else if let Some(identifier) =
                collector.identifiers.iter_mut().rev().find(|identifier| {
                    identifier.span == span && identifier.role == SourceIdentifierRole::ValueBinding
                })
            {
                identifier.value_kind = value_kind;
            }
            collector
                .type_declarations
                .push(wake_ecma_ast::SourceTypeDeclaration {
                    node,
                    name,
                    in_own_scope,
                });
        }
    }

    pub(crate) fn source_namespace_begin(&mut self) -> Option<usize> {
        self.source_syntax.as_mut().map(|collector| {
            let index = collector.namespaces.len();
            let is_ambient = collector.stack.iter().any(|&node| {
                matches!(
                    collector.nodes[node].kind,
                    SourceNodeKind::TsDeclare
                        | SourceNodeKind::TsAmbientModule
                        | SourceNodeKind::TsGlobalAugmentation
                )
            });
            collector.namespaces.push(wake_ecma_ast::SourceNamespace {
                node: *collector.stack.last().expect("namespace owns its node"),
                names: Vec::new(),
                ambient: None,
                is_ambient,
                body: None,
            });
            index
        })
    }

    pub(crate) fn source_namespace_header(
        &mut self,
        index: Option<usize>,
        names: &[wake_ecma_ast::Ident],
        ambient: Option<Span>,
    ) {
        let ambient = ambient.map(|span| SourceModuleSpecifier {
            value: self.lexer.string_value(span),
            span,
        });
        if let (Some(collector), Some(index)) = (&mut self.source_syntax, index) {
            let namespace = &mut collector.namespaces[index];
            namespace.names = names
                .iter()
                .map(|name| SourceIdentifier {
                    span: name.span,
                    name: self.interner.with_resolved(name.name, str::to_owned),
                    role: SourceIdentifierRole::ValueBinding,
                    value_kind: None,
                })
                .collect();
            namespace.ambient = ambient;
            namespace.body = (self.cur.kind == TokenKind::LBrace)
                .then(|| Span::new(self.cur.span.lo, self.cur.span.lo));
        }
    }

    pub(crate) fn source_namespace_end(&mut self, index: Option<usize>) {
        if let (Some(collector), Some(index)) = (&mut self.source_syntax, index)
            && let Some(body) = &mut collector.namespaces[index].body
        {
            body.hi = self.prev_end.max(body.lo);
        }
    }

    pub(crate) fn source_type_scope_begin(
        &mut self,
        kind: wake_ecma_ast::SourceTypeScopeKind,
        lo: u32,
    ) -> Option<usize> {
        self.source_syntax
            .as_mut()
            .map(|collector| collector.type_scope_begin(kind, lo))
    }

    pub(crate) fn source_type_parameter(&mut self, index: Option<usize>, span: Span, name: Atom) {
        if let (Some(collector), Some(index)) = (&mut self.source_syntax, index) {
            let binding = SourceIdentifier {
                name: self.interner.with_resolved(name, str::to_owned),
                span,
                role: SourceIdentifierRole::TypeBinding,
                value_kind: None,
            };
            collector.identifiers.push(binding.clone());
            collector.type_scopes[index].bindings.push(binding);
        }
    }

    pub(crate) fn source_begin_infer(&mut self) {
        if let Some(collector) = &mut self.source_syntax {
            collector.infer_marks.push(collector.pending_infer.len());
        }
    }

    pub(crate) fn source_infer_binding(&mut self, span: Span, name: Atom) {
        if let Some(collector) = &mut self.source_syntax {
            let binding = SourceIdentifier {
                name: self.interner.with_resolved(name, str::to_owned),
                span,
                role: SourceIdentifierRole::TypeBinding,
                value_kind: None,
            };
            collector.identifiers.push(binding.clone());
            if !collector.infer_marks.is_empty() {
                collector.pending_infer.push(binding);
            }
        }
    }

    pub(crate) fn source_infer_constraint(&mut self, span: Span, name: Atom) {
        if let Some(collector) = &mut self.source_syntax {
            let index = collector.type_scope_begin(
                wake_ecma_ast::SourceTypeScopeKind::InferConstraint,
                self.cur.span.lo,
            );
            collector.type_scopes[index]
                .bindings
                .push(SourceIdentifier {
                    name: self.interner.with_resolved(name, str::to_owned),
                    span,
                    role: SourceIdentifierRole::TypeBinding,
                    value_kind: None,
                });
        }
    }

    pub(crate) fn source_activate_infer(&mut self) {
        if let Some(collector) = &mut self.source_syntax {
            let mark = collector
                .infer_marks
                .pop()
                .expect("conditional pattern owns infer bindings");
            let bindings: Vec<_> = collector.pending_infer.drain(mark..).collect();
            if !bindings.is_empty() {
                let index = collector.type_scope_begin(
                    wake_ecma_ast::SourceTypeScopeKind::ConditionalTrue,
                    self.cur.span.lo,
                );
                collector.type_scopes[index].bindings = bindings;
            }
        }
    }

    pub(crate) fn source_function(
        &mut self,
        span: Span,
        scope_lo: u32,
        body: Option<Span>,
        is_arrow: bool,
    ) {
        if let Some(collector) = &mut self.source_syntax {
            collector.functions.push(wake_ecma_ast::SourceFunction {
                span,
                scope: Span::new(scope_lo, span.hi),
                body,
                is_arrow,
            });
        }
    }

    pub(crate) fn source_array(&mut self, array: &wake_ecma_ast::ArrayExpression<'_>) {
        if let Some(collector) = &mut self.source_syntax {
            collector.arrays.push(wake_ecma_ast::SourceArray {
                span: array.span,
                parent: collector.stack.last().copied(),
                elements: array
                    .elements
                    .iter()
                    .map(|element| {
                        element.map(|expression| {
                            let (value, spread) = match expression {
                                Expression::Spread(spread) => (spread.argument, true),
                                expression => (expression, false),
                            };
                            wake_ecma_ast::SourceArrayElement {
                                span: expression.span(),
                                expression: value.span(),
                                spread,
                            }
                        })
                    })
                    .collect(),
            });
        }
    }

    pub(crate) fn source_jsx_value(&mut self, expression: Option<Expression<'_>>, shorthand: bool) {
        if let Some(collector) = &mut self.source_syntax {
            let node = *collector
                .stack
                .last()
                .expect("JSX value owns a source node");
            collector.jsx_values.push(SourceJsxValue {
                node,
                expression: if shorthand {
                    None
                } else {
                    expression.map(|value| value.span())
                },
                value: expression.map_or(SourcePrimitiveValue::Empty, |expression| {
                    source_primitive(self.interner, expression)
                }),
            });
        }
    }

    pub(crate) fn source_jsx_text(&mut self, text: Option<&str>) {
        if let Some(collector) = &mut self.source_syntax {
            let node = *collector.stack.last().expect("JSX text owns a source node");
            collector.jsx_values.push(SourceJsxValue {
                node,
                expression: None,
                value: text.map_or(SourcePrimitiveValue::Empty, |text| {
                    SourcePrimitiveValue::String(text.into())
                }),
            });
        }
    }

    pub(crate) fn source_export_begin(&mut self, lo: u32) {
        if let Some(collector) = &mut self.source_syntax {
            let index = collector.exports.len();
            collector.exports.push(SourceExport {
                span: Span::new(lo, lo),
                parent: collector.stack.last().copied(),
                kind: SourceExportKind::Named,
                type_only: false,
                source: None,
                specifiers: Vec::new(),
                exported: None,
                target: None,
                attributes: None,
            });
            collector.export_stack.push(index);
        }
    }

    pub(crate) fn source_export_end(&mut self, hi: u32) {
        if let Some(collector) = &mut self.source_syntax {
            let index = collector
                .export_stack
                .pop()
                .expect("export capture is balanced");
            let export = &mut collector.exports[index];
            export.span.hi = hi;
            if export.kind == SourceExportKind::Named && export.source.is_none() {
                for specifier in &export.specifiers {
                    if specifier.local.identifier {
                        collector.identifiers.push(SourceIdentifier {
                            name: specifier.local.value.clone(),
                            span: specifier.local.span,
                            role: if specifier.type_only {
                                SourceIdentifierRole::TypeReference
                            } else {
                                SourceIdentifierRole::ValueReference
                            },
                            value_kind: None,
                        });
                    }
                }
            }
        }
    }

    pub(crate) fn source_export_form(
        &mut self,
        kind: SourceExportKind,
        type_only: bool,
        target: Option<Span>,
    ) {
        if let Some(collector) = &mut self.source_syntax
            && let Some(&index) = collector.export_stack.last()
        {
            let export = &mut collector.exports[index];
            export.kind = kind;
            export.type_only = type_only;
            export.target = target;
        }
    }

    pub(crate) fn source_export_name(&mut self, name: ModuleExportName, span: Span) {
        if let Some(collector) = &mut self.source_syntax
            && let Some(&index) = collector.export_stack.last()
        {
            collector.exports[index].exported = Some(export_name(self.interner, name, span));
        }
    }

    pub(crate) fn source_export_specifier(
        &mut self,
        lo: u32,
        local: ModuleExportName,
        local_span: Span,
        exported: ModuleExportName,
        exported_span: Span,
        type_only: bool,
    ) {
        let span = self.span_to(lo);
        if let Some(collector) = &mut self.source_syntax
            && let Some(&index) = collector.export_stack.last()
        {
            collector.exports[index]
                .specifiers
                .push(SourceExportSpecifier {
                    span,
                    local: export_name(self.interner, local, local_span),
                    exported: export_name(self.interner, exported, exported_span),
                    type_only,
                });
        }
    }

    pub(crate) fn source_export_module(
        &mut self,
        span: Span,
        attributes: Option<&ImportAttributes<'_>>,
    ) {
        if let Some(collector) = &mut self.source_syntax
            && let Some(&index) = collector.export_stack.last()
        {
            collector.exports[index].source = Some(SourceModuleSpecifier {
                value: self.lexer.string_value(span),
                span,
            });
            collector.exports[index].attributes = source_attributes(self.interner, attributes);
        }
    }

    pub(crate) fn source_import_begin(&mut self, lo: u32) {
        if let Some(collector) = &mut self.source_syntax {
            let index = collector.imports.len();
            collector.imports.push(SourceImport {
                span: Span::new(lo, lo),
                parent: collector.stack.last().copied(),
                type_only: false,
                equals_target: None,
                source: None,
                bindings: Vec::new(),
                attributes: None,
            });
            collector.import_stack.push(index);
        }
    }

    pub(crate) fn source_import_end(&mut self, hi: u32) {
        if let Some(collector) = &mut self.source_syntax {
            let index = collector
                .import_stack
                .pop()
                .expect("import capture is balanced");
            collector.imports[index].span.hi = hi;
        }
    }

    pub(crate) fn source_import_type_only(&mut self) {
        if let Some(collector) = &mut self.source_syntax
            && let Some(&index) = collector.import_stack.last()
        {
            collector.imports[index].type_only = true;
        }
    }

    pub(crate) fn source_import_specifier(
        &mut self,
        kind: SourceImportBindingKind,
        lo: u32,
        local: wake_ecma_ast::Ident,
        imported: Option<ModuleExportName>,
        type_only: bool,
    ) {
        self.source_import_binding(local, type_only);
        let span = self.span_to(lo);
        if let Some(collector) = &mut self.source_syntax
            && let Some(&index) = collector.import_stack.last()
        {
            collector.imports[index].bindings.push(SourceImportBinding {
                kind,
                span,
                type_only,
                local: SourceIdentifier {
                    name: self.interner.with_resolved(local.name, str::to_owned),
                    span: local.span,
                    role: if type_only {
                        SourceIdentifierRole::TypeBinding
                    } else {
                        SourceIdentifierRole::ValueBinding
                    },
                    value_kind: None,
                },
                imported: imported.map(|name| {
                    self.interner
                        .with_resolved(module_name_atom(name), str::to_owned)
                }),
            });
        }
    }

    pub(crate) fn source_import_module(
        &mut self,
        span: Span,
        attributes: Option<&ImportAttributes<'_>>,
    ) {
        if let Some(collector) = &mut self.source_syntax
            && let Some(&index) = collector.import_stack.last()
        {
            let import = &mut collector.imports[index];
            import.source = Some(SourceModuleSpecifier {
                value: self.lexer.string_value(span),
                span,
            });
            import.attributes = source_attributes(self.interner, attributes);
        }
    }

    pub(crate) fn source_import_equals_target(&mut self, expression: Expression<'_>) {
        if let Some(collector) = &mut self.source_syntax
            && let Some(&index) = collector.import_stack.last()
        {
            collector.imports[index].equals_target = Some(expression.span());
        }
        if let Expression::Call(call) = expression
            && matches!(call.callee, Expression::Identifier(id) if id.name == self.require_atom)
            && let [Expression::StringLiteral(literal)] = call.arguments.as_slice()
        {
            self.source_import_module(literal.span, None);
        }
    }

    pub(crate) fn source_type_import(
        &mut self,
        span: Span,
        source: Option<SourceModuleSpecifier>,
        attributes: Option<Vec<SourceImportAttribute>>,
    ) {
        if let (Some(collector), Some(source)) = (&mut self.source_syntax, source) {
            collector
                .type_imports
                .push(wake_ecma_ast::SourceTypeImport {
                    span,
                    source,
                    attributes_known: attributes.is_some(),
                    attributes: attributes.unwrap_or_default(),
                });
        }
    }

    pub(crate) fn source_import_binding(&mut self, local: wake_ecma_ast::Ident, type_only: bool) {
        let role = if type_only {
            SourceIdentifierRole::TypeBinding
        } else {
            SourceIdentifierRole::ValueBinding
        };
        if let Some(collector) = &mut self.source_syntax
            && let Some(last) = collector.identifiers.last_mut()
            && last.span == local.span
            && last.role == SourceIdentifierRole::ValueBinding
        {
            last.role = role;
            return;
        }
        self.source_identifier(local.span, local.name, role);
    }

    pub(crate) fn source_identifier(&mut self, span: Span, name: Atom, role: SourceIdentifierRole) {
        if let Some(collector) = &mut self.source_syntax {
            let identifier = SourceIdentifier {
                span,
                name: self.interner.with_resolved(name, str::to_owned),
                role,
                value_kind: None,
            };
            if collector.identifiers.last() != Some(&identifier) {
                collector.identifiers.push(identifier);
            }
        }
    }

    pub(crate) fn source_binding_kind(
        &mut self,
        span: Span,
        kind: wake_ecma_ast::SourceValueBindingKind,
    ) {
        if let Some(collector) = &mut self.source_syntax
            && let Some(identifier) = collector.identifiers.iter_mut().rev().find(|identifier| {
                identifier.span == span && identifier.role == SourceIdentifierRole::ValueBinding
            })
        {
            identifier.value_kind = Some(kind);
        }
    }

    pub(crate) fn source_token(&mut self, token: Token, context: SourceTokenContext) {
        if let Some(collector) = &mut self.source_syntax
            && !token.is_eof()
        {
            collector.tokens.push(SourceToken {
                kind: token.kind,
                span: token.span,
                newline_before: token.newline_before,
                context,
            });
        }
    }

    pub(crate) fn source_begin(&mut self, kind: SourceNodeKind, lo: u32) {
        if let Some(collector) = &mut self.source_syntax {
            collector.begin(kind, lo);
        }
    }

    pub(crate) fn source_end(&mut self, hi: u32) {
        if let Some(collector) = &mut self.source_syntax {
            collector.end(hi);
        }
    }

    pub(crate) fn source_leaf(&mut self, kind: SourceNodeKind, span: Span) {
        if let Some(collector) = &mut self.source_syntax {
            collector.begin(kind, span.lo);
            collector.end(span.hi);
        }
    }
}

fn module_name_atom(name: ModuleExportName) -> Atom {
    match name {
        ModuleExportName::Ident(id) => id.name,
        ModuleExportName::String(atom) => atom,
    }
}

pub(super) fn source_primitive(
    interner: &Interner,
    expression: Expression<'_>,
) -> SourcePrimitiveValue {
    use wake_ecma_ast::UnaryOperator;
    match expression {
        Expression::StringLiteral(literal) => {
            SourcePrimitiveValue::String(interner.resolve_js(literal.value))
        }
        Expression::NumberLiteral(literal) => SourcePrimitiveValue::Number(literal.value),
        Expression::BigIntLiteral(literal) => {
            SourcePrimitiveValue::BigInt(interner.with_resolved(literal.raw, normalize_bigint))
        }
        Expression::BooleanLiteral(literal) => SourcePrimitiveValue::Boolean(literal.value),
        Expression::NullLiteral(_) => SourcePrimitiveValue::Null,
        Expression::TemplateLiteral(template) if template.expressions.is_empty() => template
            .quasis
            .first()
            .and_then(|element| element.cooked)
            .map_or(SourcePrimitiveValue::Unknown, |value| {
                SourcePrimitiveValue::String(interner.resolve_js(value))
            }),
        Expression::Unary(unary) => match (unary.operator, unary.argument) {
            (UnaryOperator::Void, _) => SourcePrimitiveValue::Undefined,
            (UnaryOperator::Minus, Expression::NumberLiteral(literal)) => {
                SourcePrimitiveValue::Number(-literal.value)
            }
            (UnaryOperator::Plus, Expression::NumberLiteral(literal)) => {
                SourcePrimitiveValue::Number(literal.value)
            }
            (UnaryOperator::Minus, Expression::BigIntLiteral(literal)) => {
                SourcePrimitiveValue::BigInt(
                    interner
                        .with_resolved(literal.raw, |raw| negate_bigint(&normalize_bigint(raw))),
                )
            }
            (UnaryOperator::Plus, Expression::BigIntLiteral(literal)) => {
                SourcePrimitiveValue::BigInt(interner.with_resolved(literal.raw, normalize_bigint))
            }
            _ => SourcePrimitiveValue::Unknown,
        },
        _ => SourcePrimitiveValue::Unknown,
    }
}

/// Normalize a valid BigInt token to signed decimal text without using a floating-point value.
fn normalize_bigint(raw: &str) -> String {
    let raw = raw.replace('_', "");
    let (negative, unsigned) = raw
        .strip_prefix('-')
        .map_or((false, raw.as_str()), |value| (true, value));
    let (base, digits) = if let Some(value) = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
    {
        (16u32, value)
    } else if let Some(value) = unsigned
        .strip_prefix("0b")
        .or_else(|| unsigned.strip_prefix("0B"))
    {
        (2u32, value)
    } else if let Some(value) = unsigned
        .strip_prefix("0o")
        .or_else(|| unsigned.strip_prefix("0O"))
    {
        (8u32, value)
    } else {
        (10u32, unsigned)
    };
    let mut limbs = vec![0u32];
    for digit in digits.chars() {
        let value = digit.to_digit(base).unwrap_or(0);
        let mut carry = value;
        for limb in &mut limbs {
            let product = u64::from(*limb) * u64::from(base) + u64::from(carry);
            *limb = (product % 1_000_000_000) as u32;
            carry = (product / 1_000_000_000) as u32;
        }
        if carry != 0 {
            limbs.push(carry);
        }
    }
    while limbs.len() > 1 && limbs.last() == Some(&0) {
        limbs.pop();
    }
    let mut result = limbs.pop().unwrap_or(0).to_string();
    for limb in limbs.into_iter().rev() {
        result.push_str(&format!("{limb:09}"));
    }
    if negative && result != "0" {
        result.insert(0, '-');
    }
    result
}

fn negate_bigint(value: &str) -> String {
    if value == "0" {
        "0".into()
    } else if let Some(value) = value.strip_prefix('-') {
        value.into()
    } else {
        format!("-{value}")
    }
}

fn export_name(interner: &Interner, name: ModuleExportName, span: Span) -> SourceExportName {
    SourceExportName {
        value: interner.with_resolved(module_name_atom(name), str::to_owned),
        span,
        identifier: matches!(name, ModuleExportName::Ident(_)),
    }
}

fn source_attributes(
    interner: &Interner,
    attributes: Option<&ImportAttributes<'_>>,
) -> Option<SourceImportAttributes> {
    attributes.map(|attributes| SourceImportAttributes {
        keyword: attributes.keyword.as_str().into(),
        span: attributes.span,
        entries: attributes
            .items
            .iter()
            .map(|item| SourceImportAttribute {
                key: match item.key {
                    ImportAttributeKey::Ident(id) => interner.resolve(id.name).into(),
                    ImportAttributeKey::String(value) => interner.resolve_js(value),
                },
                value: interner.resolve_js(item.value),
                span: item.span,
            })
            .collect(),
    })
}
