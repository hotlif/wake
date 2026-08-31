//! Direct emission for the optimizer-owned typed IR.
//!
//! This module deliberately has no parser AST or legacy optimization-table input. Both mapped
//! and unmapped entry points execute the same token walk; mapping collection is an optional sink.

use std::collections::HashMap;
use std::fmt::Write as _;

use wake_common::Span;
use wake_ecma_ast::{
    BinaryOperator, LogicalOperator, MethodKind, PropertyKind, SourceType, UnaryOperator, VarKind,
};
use wake_ecma_minify::codegen_bridge::{
    ArrowBodyKind, ClassContext, ExportDefaultValueKind, FinalizedTypedProgram, ForInitializerKind,
    ForLeftKind, FunctionContext, ImportSpecifierKind, IrModuleName, IrNode, IrNodeData, IrOrigin,
    IrPropertyKey, ListId, ModuleNameKind, NameSyntax, NodeId, PropertyKeyKind,
    SyntheticOriginKind, TypedDiscardedStaticRequest, TypedProgram, write_number_minified,
};

use crate::{GeneratedDiscardedStaticRequest, Mapping, ModuleMappings};

/// Emit a fully finalized optimizer-owned program without collecting source mappings.
pub(crate) fn codegen_finalized_typed(program: &FinalizedTypedProgram, minify: bool) -> String {
    emit_validated(program.program(), minify, false, &[]).0
}

/// Emit a fully finalized optimizer-owned program and collect module-local source mappings.
pub(crate) fn codegen_finalized_typed_with_map(
    program: &FinalizedTypedProgram,
    minify: bool,
) -> (String, ModuleMappings) {
    let (code, mappings, _) = emit_validated(program.program(), minify, true, &[]);
    (code, mappings.expect("mapping collection was requested"))
}

/// Bundler-only mapped emission which carries semantic request proofs into exact byte ranges.
pub(crate) fn codegen_finalized_typed_with_requests(
    program: &FinalizedTypedProgram,
    minify: bool,
) -> (String, ModuleMappings, Vec<GeneratedDiscardedStaticRequest>) {
    let (code, mappings, requests) = emit_validated(
        program.program(),
        minify,
        true,
        program.discarded_static_requests(),
    );
    (
        code,
        mappings.expect("mapping collection was requested"),
        requests,
    )
}

/// Emit a sealed optimizer program whose paired module plan proves finalization cannot mutate it.
/// The proof is established inside `wake_ecma_minify`; this crate only keeps mapped and unmapped
/// output on the same token walk.
pub(crate) fn codegen_sealed_trivial_typed(program: &TypedProgram, minify: bool) -> String {
    debug_assert!(program.validate().is_ok());
    emit_validated(program, minify, false, &[]).0
}

pub(crate) fn codegen_sealed_trivial_typed_with_map(
    program: &TypedProgram,
    minify: bool,
) -> (String, ModuleMappings) {
    debug_assert!(program.validate().is_ok());
    let (code, mappings, _) = emit_validated(program, minify, true, &[]);
    (code, mappings.expect("mapping collection was requested"))
}

/// Test-only raw typed-IR emitter which checks the structural invariant at its boundary.
#[cfg(test)]
pub(crate) fn codegen_typed(program: &TypedProgram, minify: bool) -> String {
    emit_checked(program, minify, false).0
}

/// Test-only mapped counterpart of [`codegen_typed`].
///
/// The JavaScript body is byte-identical to [`codegen_typed`] because mapping is only an optional
/// side sink on the same token writer.
#[cfg(test)]
pub(crate) fn codegen_typed_with_map(
    program: &TypedProgram,
    minify: bool,
) -> (String, ModuleMappings) {
    let (code, mappings) = emit_checked(program, minify, true);
    (code, mappings.expect("mapping collection was requested"))
}

#[cfg(test)]
fn emit_checked(
    program: &TypedProgram,
    minify: bool,
    want_map: bool,
) -> (String, Option<ModuleMappings>) {
    program
        .validate()
        .expect("codegen received structurally invalid typed IR");
    let (code, mappings, _) = emit_validated(program, minify, want_map, &[]);
    (code, mappings)
}

fn emit_validated(
    program: &TypedProgram,
    minify: bool,
    want_map: bool,
    discarded_static_requests: &[TypedDiscardedStaticRequest],
) -> (
    String,
    Option<ModuleMappings>,
    Vec<GeneratedDiscardedStaticRequest>,
) {
    let mut emitter = TypedEmitter {
        program,
        out: String::new(),
        minify,
        indent: 0,
        map: want_map.then(MapState::default),
        discarded_static_request_targets: discarded_static_requests
            .iter()
            .map(|request| (request.node(), request.target().0))
            .collect(),
        discarded_static_requests: Vec::with_capacity(discarded_static_requests.len()),
    };
    emitter.emit_node(program.root());
    let mappings = emitter.map.map(|map| ModuleMappings {
        mappings: map.mappings,
        names: map.names,
    });
    (emitter.out, mappings, emitter.discarded_static_requests)
}

#[derive(Default)]
struct MapState {
    line: u32,
    col: u32,
    mappings: Vec<Mapping>,
    names: Vec<String>,
    name_indices: HashMap<String, u32>,
    last_source: Option<(u32, Option<u32>)>,
    last_unmapped: bool,
}

impl MapState {
    fn advance(&mut self, text: &str) {
        if let Some(last_newline) = text.rfind('\n') {
            self.line += text.bytes().filter(|byte| *byte == b'\n').count() as u32;
            self.col = text[last_newline + 1..]
                .chars()
                .map(char::len_utf16)
                .sum::<usize>() as u32;
            self.last_source = None;
            self.last_unmapped = false;
        } else {
            self.col += text.chars().map(char::len_utf16).sum::<usize>() as u32;
        }
    }

    fn name_index(&mut self, original: &str) -> u32 {
        if let Some(index) = self.name_indices.get(original) {
            return *index;
        }
        let index = self.names.len() as u32;
        self.names.push(original.to_owned());
        self.name_indices.insert(original.to_owned(), index);
        index
    }

    fn mapped(&mut self, span: Span, original_name: Option<&str>) {
        let name_index = original_name.map(|name| self.name_index(name));
        let source = (span.lo, name_index);
        if self.last_source == Some(source) && !self.last_unmapped {
            return;
        }
        let mapping = Mapping {
            gen_line: self.line,
            gen_col: self.col,
            src_index: 0,
            src_offset: span.lo,
            name_index,
            is_unmapped: false,
        };
        self.replace_or_push(mapping);
        self.last_source = Some(source);
        self.last_unmapped = false;
    }

    fn unmapped(&mut self) {
        if self.last_unmapped {
            return;
        }
        self.replace_or_push(Mapping::unmapped(self.line, self.col));
        self.last_source = None;
        self.last_unmapped = true;
    }

    fn replace_or_push(&mut self, mapping: Mapping) {
        if let Some(last) = self.mappings.last_mut()
            && last.gen_line == mapping.gen_line
            && last.gen_col == mapping.gen_col
        {
            *last = mapping;
        } else {
            self.mappings.push(mapping);
        }
    }
}

struct TypedEmitter<'program> {
    program: &'program TypedProgram,
    out: String,
    minify: bool,
    indent: usize,
    map: Option<MapState>,
    discarded_static_request_targets: HashMap<NodeId, u32>,
    discarded_static_requests: Vec<GeneratedDiscardedStaticRequest>,
}

// Expression precedence: larger values bind more tightly.
const P_SEQUENCE: u8 = 1;
const P_ASSIGN: u8 = 2;
const P_CONDITIONAL: u8 = 3;
const P_COALESCE: u8 = 4;
const P_LOGICAL_OR: u8 = 5;
const P_LOGICAL_AND: u8 = 6;
const P_BIT_OR: u8 = 7;
const P_BIT_XOR: u8 = 8;
const P_BIT_AND: u8 = 9;
const P_EQUALITY: u8 = 10;
const P_RELATIONAL: u8 = 11;
const P_SHIFT: u8 = 12;
const P_ADDITIVE: u8 = 13;
const P_MULTIPLICATIVE: u8 = 14;
const P_EXPONENT: u8 = 15;
const P_UNARY: u8 = 16;
const P_POSTFIX: u8 = 17;
const P_CALL_MEMBER: u8 = 18;
const P_PRIMARY: u8 = 19;

impl<'program> TypedEmitter<'program> {
    fn node(&self, id: NodeId) -> &'program IrNode {
        self.program
            .node(id)
            .unwrap_or_else(|| panic!("typed codegen encountered unknown node {}", id.index()))
    }

    fn items(&self, id: ListId) -> &'program [NodeId] {
        self.program
            .list(id)
            .unwrap_or_else(|| panic!("typed codegen encountered unknown list {}", id.index()))
            .items()
    }

    fn raw(&mut self, text: &str) {
        self.out.push_str(text);
        if let Some(map) = &mut self.map {
            map.advance(text);
        }
    }

    fn needs_separator(&self, text: &str) -> bool {
        let Some(left) = self.out.as_bytes().last().copied() else {
            return false;
        };
        let Some(right) = text.as_bytes().first().copied() else {
            return false;
        };
        fn word(byte: u8) -> bool {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
        }
        (word(left) && word(right))
            || (left == b'+' && right == b'+')
            || (left == b'-' && right == b'-')
            || (left == b'/' && matches!(right, b'/' | b'*'))
    }

    fn ensure_separator(&mut self, text: &str) {
        if self.minify && self.needs_separator(text) {
            self.mark_unmapped();
            self.raw(" ");
        }
    }

    fn mark_unmapped(&mut self) {
        if let Some(map) = &mut self.map {
            map.unmapped();
        }
    }

    fn mark_origin(&mut self, origin: IrOrigin, original_name: Option<&str>) {
        let Some(map) = &mut self.map else {
            return;
        };
        match origin {
            IrOrigin::Source(span) => map.mapped(span, original_name),
            IrOrigin::Derived {
                anchor: Some(span), ..
            } => map.mapped(span, original_name),
            IrOrigin::Synthetic {
                anchor: Some(span),
                kind: SyntheticOriginKind::TrustedEdit,
            } => map.mapped(span, original_name),
            IrOrigin::Derived { anchor: None, .. }
            | IrOrigin::Synthetic {
                kind:
                    SyntheticOriginKind::Optimization
                    | SyntheticOriginKind::External
                    | SyntheticOriginKind::TrustedEdit,
                ..
            } => map.unmapped(),
        }
    }

    fn source_token(&mut self, id: NodeId, text: &str) {
        self.ensure_separator(text);
        let origin = self.node(id).origin();
        self.mark_origin(origin, None);
        self.raw(text);
    }

    fn syntax(&mut self, pretty: &str) {
        let text = if self.minify {
            pretty.trim_matches(' ')
        } else {
            pretty
        };
        self.ensure_separator(text);
        self.mark_unmapped();
        self.raw(text);
    }

    fn required_space(&mut self) {
        if !self.minify {
            self.mark_unmapped();
            self.raw(" ");
        }
    }

    fn optional_space(&mut self) {
        if !self.minify {
            self.required_space();
        }
    }

    fn newline(&mut self) {
        if self.minify {
            return;
        }
        self.mark_unmapped();
        self.raw("\n");
        if self.indent > 0 {
            let indentation = "  ".repeat(self.indent);
            self.raw(&indentation);
        }
    }

    fn keyword(&mut self, id: NodeId, keyword: &str) {
        self.source_token(id, keyword);
    }

    fn binop(&mut self, id: NodeId, operator: &str) {
        if !self.minify || operator.as_bytes()[0].is_ascii_alphabetic() {
            self.required_space();
            self.source_token(id, operator);
            self.required_space();
        } else {
            self.source_token(id, operator);
        }
    }

    fn emit_name(&mut self, id: NodeId) {
        let (name_id, origin) = match self.node(id).data() {
            IrNodeData::Name { name } => (*name, self.node(id).origin()),
            other => panic!("typed codegen expected name node, found {other:?}"),
        };
        let name = self.program.name(name_id).unwrap_or_else(|| {
            panic!("typed codegen encountered unknown name {}", name_id.index())
        });
        let emitted = name.emitted();
        let original = name.original();
        self.ensure_separator(emitted);
        self.mark_origin(origin, Some(original));
        self.raw(emitted);
    }

    fn name_text(&self, id: NodeId) -> (&'program str, &'program str, NameSyntax) {
        let IrNodeData::Name { name } = self.node(id).data() else {
            panic!("typed codegen expected name node at {}", id.index());
        };
        let name = self
            .program
            .name(*name)
            .unwrap_or_else(|| panic!("typed codegen encountered unknown name {}", name.index()));
        (name.emitted(), name.original(), name.syntax())
    }

    fn emit_string_value(&mut self, id: NodeId, value: &str) {
        let encoded = quote_string(value, self.minify);
        self.source_token(id, &encoded);
    }

    fn emit_string_node(&mut self, id: NodeId) {
        let value = match self.node(id).data() {
            IrNodeData::StringLiteral { value } => value.as_str(),
            IrNodeData::Name { .. } => self.name_text(id).0,
            other => panic!("typed codegen expected string-bearing node, found {other:?}"),
        };
        self.emit_string_value(id, value);
    }

    fn emit_list(
        &mut self,
        list: ListId,
        separator: &str,
        mut emit: impl FnMut(&mut Self, NodeId),
    ) {
        let items = self.items(list);
        for (index, &item) in items.iter().enumerate() {
            if index > 0 {
                self.syntax(separator);
            }
            emit(self, item);
        }
    }

    /// The sole exhaustive syntax dispatch. Adding an IR variant fails compilation here until
    /// codegen assigns it concrete grammar.
    #[allow(clippy::too_many_lines)]
    fn emit_node(&mut self, id: NodeId) {
        let discarded_request = self
            .discarded_static_request_targets
            .get(&id)
            .copied()
            .map(|target_module_id| (self.out.len(), target_module_id));
        let node = self.node(id);
        assert!(
            !node.is_tombstone(),
            "typed codegen reached tombstoned node {}",
            id.index()
        );
        match *node.data() {
            IrNodeData::Program {
                source_type,
                strict,
                ref spread_helper,
                ref object_spread_helper,
                ref for_of_helper,
                body,
            } => self.emit_program(
                id,
                source_type,
                strict,
                spread_helper.as_deref(),
                object_spread_helper.as_deref(),
                for_of_helper.as_deref(),
                body,
            ),
            IrNodeData::VariableDeclaration { kind, declarations } => {
                self.emit_variable_declaration(id, kind, declarations, true);
            }
            IrNodeData::VariableDeclarator {
                binding,
                initializer,
            } => {
                self.emit_node(binding);
                if let Some(initializer) = initializer {
                    self.syntax(" = ");
                    self.emit_expr(initializer, P_ASSIGN);
                }
            }
            IrNodeData::Function {
                context,
                name,
                parameters,
                body,
                is_async,
                is_generator,
            } => self.emit_function(id, context, name, parameters, body, is_async, is_generator),
            IrNodeData::FunctionBody { statements, strict } => {
                self.emit_function_body(id, statements, strict);
            }
            IrNodeData::Class {
                context,
                name,
                super_class,
                members,
                decorators,
            } => self.emit_class(id, context, name, super_class, members, decorators),
            IrNodeData::Block { body } => self.emit_block(id, body),
            IrNodeData::EmptyStatement => self.syntax(";"),
            IrNodeData::DebuggerStatement => {
                self.keyword(id, "debugger");
                self.syntax(";");
            }
            IrNodeData::ExpressionStatement {
                expression,
                directive,
            } => {
                let wrap = self.starts_problematic(expression)
                    || (!directive
                        && matches!(
                            self.node(expression).data(),
                            IrNodeData::StringLiteral { .. }
                        ));
                if wrap {
                    self.syntax("(");
                }
                self.emit_expr(expression, P_SEQUENCE);
                if wrap {
                    self.syntax(")");
                }
                self.syntax(";");
            }
            IrNodeData::IfStatement {
                test,
                consequent,
                alternate,
            } => self.emit_if(id, test, consequent, alternate),
            IrNodeData::ForStatement {
                initializer,
                initializer_kind,
                test,
                update,
                body,
            } => self.emit_for(id, initializer, initializer_kind, test, update, body),
            IrNodeData::ForInStatement {
                left,
                left_kind,
                right,
                body,
            } => self.emit_for_in_of(id, left, left_kind, right, body, false, false),
            IrNodeData::ForOfStatement {
                left,
                left_kind,
                right,
                body,
                is_await,
            } => self.emit_for_in_of(id, left, left_kind, right, body, true, is_await),
            IrNodeData::WhileStatement { test, body } => {
                self.keyword(id, "while");
                self.optional_space();
                self.syntax("(");
                self.emit_expr(test, P_SEQUENCE);
                self.syntax(") ");
                self.emit_node(body);
            }
            IrNodeData::DoWhileStatement { body, test } => {
                self.keyword(id, "do");
                self.required_space();
                self.emit_node(body);
                self.optional_space();
                self.keyword(id, "while");
                self.optional_space();
                self.syntax("(");
                self.emit_expr(test, P_SEQUENCE);
                self.syntax(");");
            }
            IrNodeData::SwitchStatement {
                discriminant,
                cases,
            } => self.emit_switch(id, discriminant, cases),
            IrNodeData::SwitchCase { test, consequent } => {
                self.emit_switch_case(id, test, consequent);
            }
            IrNodeData::ReturnStatement { argument } => {
                self.keyword(id, "return");
                if let Some(argument) = argument {
                    self.required_space();
                    self.emit_expr(argument, P_SEQUENCE);
                }
                self.syntax(";");
            }
            IrNodeData::BreakStatement { label } => {
                self.keyword(id, "break");
                if let Some(label) = label {
                    self.required_space();
                    self.emit_node(label);
                }
                self.syntax(";");
            }
            IrNodeData::ContinueStatement { label } => {
                self.keyword(id, "continue");
                if let Some(label) = label {
                    self.required_space();
                    self.emit_node(label);
                }
                self.syntax(";");
            }
            IrNodeData::ThrowStatement { argument } => {
                self.keyword(id, "throw");
                self.required_space();
                self.emit_expr(argument, P_SEQUENCE);
                self.syntax(";");
            }
            IrNodeData::TryStatement {
                block,
                handler,
                finalizer,
            } => self.emit_try(id, block, handler, finalizer),
            IrNodeData::CatchClause { parameter, body } => {
                self.keyword(id, "catch");
                if let Some(parameter) = parameter {
                    self.optional_space();
                    self.syntax("(");
                    self.emit_node(parameter);
                    self.syntax(") ");
                } else {
                    self.required_space();
                }
                self.emit_node(body);
            }
            IrNodeData::LabeledStatement { label, body } => {
                self.emit_node(label);
                self.syntax(": ");
                self.emit_node(body);
            }
            IrNodeData::WithStatement { object, body } => {
                self.keyword(id, "with");
                self.optional_space();
                self.syntax("(");
                self.emit_expr(object, P_SEQUENCE);
                self.syntax(") ");
                self.emit_node(body);
            }
            IrNodeData::NumberLiteral { value } => {
                let number = number_source(value, self.minify);
                self.source_token(id, &number);
            }
            IrNodeData::StringLiteral { ref value } => self.emit_string_value(id, value),
            IrNodeData::BooleanLiteral { value } => self.source_token(
                id,
                if self.minify {
                    if value { "!0" } else { "!1" }
                } else if value {
                    "true"
                } else {
                    "false"
                },
            ),
            IrNodeData::NullLiteral => self.source_token(id, "null"),
            IrNodeData::BigIntLiteral { ref raw } => {
                self.source_token(id, &format!("{raw}n"));
            }
            IrNodeData::RegExpLiteral {
                ref pattern,
                ref flags,
            } => {
                self.source_token(id, &format!("/{pattern}/{flags}"));
            }
            IrNodeData::TemplateLiteral {
                quasis,
                expressions,
            } => self.emit_template(id, quasis, expressions),
            IrNodeData::TemplateElement {
                cooked: _,
                ref raw,
                tail,
            } => {
                let _ = tail;
                self.source_token(id, raw);
            }
            IrNodeData::Name { name: _ } => self.emit_name(id),
            IrNodeData::Identifier { name } => self.emit_node(name),
            IrNodeData::ThisExpression => self.source_token(id, "this"),
            IrNodeData::SuperExpression => self.source_token(id, "super"),
            IrNodeData::MetaProperty { meta, property } => {
                self.emit_node(meta);
                self.syntax(".");
                self.emit_node(property);
            }
            IrNodeData::ArrayExpression { elements } => {
                self.syntax("[");
                self.emit_array_like(elements, true);
                self.syntax("]");
            }
            IrNodeData::Elision => {}
            IrNodeData::ObjectExpression { members } => {
                self.syntax("{");
                if !self.items(members).is_empty() {
                    self.optional_space();
                    self.emit_list(members, ", ", Self::emit_node);
                    self.optional_space();
                }
                self.syntax("}");
            }
            IrNodeData::ObjectProperty {
                key,
                value,
                kind,
                method,
                shorthand,
                computed,
                prototype_setter,
            } => self.emit_object_property(
                id,
                key,
                value,
                kind,
                method,
                shorthand,
                computed,
                prototype_setter,
            ),
            IrNodeData::UnaryExpression { operator, argument } => {
                self.source_token(id, operator.as_str());
                if matches!(
                    operator,
                    UnaryOperator::Typeof | UnaryOperator::Void | UnaryOperator::Delete
                ) {
                    self.required_space();
                }
                self.emit_expr(argument, P_UNARY);
            }
            IrNodeData::UpdateExpression {
                operator,
                prefix,
                argument,
            } => {
                if prefix {
                    self.source_token(id, operator.as_str());
                    self.emit_expr(argument, P_UNARY);
                } else {
                    self.emit_expr(argument, P_POSTFIX);
                    self.source_token(id, operator.as_str());
                }
            }
            IrNodeData::BinaryExpression {
                operator,
                left,
                right,
            } => self.emit_binary(id, operator, left, right),
            IrNodeData::LogicalExpression {
                operator,
                left,
                right,
            } => self.emit_logical(id, operator, left, right),
            IrNodeData::AssignmentExpression {
                operator,
                left,
                right,
            } => {
                let object_pattern =
                    matches!(self.node(left).data(), IrNodeData::ObjectExpression { .. });
                if object_pattern {
                    self.syntax("(");
                }
                self.emit_expr(left, P_CALL_MEMBER);
                self.binop(id, operator.as_str());
                self.emit_expr(right, P_ASSIGN);
                if object_pattern {
                    self.syntax(")");
                }
            }
            IrNodeData::ConditionalExpression {
                test,
                consequent,
                alternate,
            } => {
                self.emit_expr(test, P_CONDITIONAL + 1);
                self.syntax(" ? ");
                self.emit_expr(consequent, P_ASSIGN);
                self.syntax(" : ");
                self.emit_expr(alternate, P_ASSIGN);
            }
            IrNodeData::CallExpression {
                callee,
                arguments,
                optional,
            } => {
                self.emit_expr(callee, P_CALL_MEMBER);
                if optional {
                    self.syntax("?.");
                }
                self.emit_arguments(arguments);
            }
            IrNodeData::NewExpression { callee, arguments } => {
                self.keyword(id, "new");
                self.required_space();
                let group_callee = matches!(
                    self.node(callee).data(),
                    IrNodeData::CallExpression { .. }
                        | IrNodeData::MemberExpression { optional: true, .. }
                );
                if group_callee {
                    self.syntax("(");
                }
                self.emit_expr(callee, P_CALL_MEMBER);
                if group_callee {
                    self.syntax(")");
                }
                self.emit_arguments(arguments);
            }
            IrNodeData::MemberExpression {
                object,
                property,
                property_kind,
                optional,
            } => self.emit_member(object, property, property_kind, optional),
            IrNodeData::SequenceExpression { expressions } => {
                self.emit_list(expressions, ", ", |this, expression| {
                    this.emit_expr(expression, P_ASSIGN);
                });
            }
            IrNodeData::TaggedTemplateExpression { tag, quasi } => {
                self.emit_expr(tag, P_CALL_MEMBER);
                self.emit_node(quasi);
            }
            IrNodeData::SpreadElement { argument } => {
                self.syntax("...");
                self.emit_expr(argument, P_ASSIGN);
            }
            IrNodeData::AwaitExpression { argument } => {
                self.keyword(id, "await");
                self.required_space();
                self.emit_expr(argument, P_UNARY);
            }
            IrNodeData::YieldExpression { argument, delegate } => {
                self.keyword(id, "yield");
                if delegate {
                    self.syntax("*");
                }
                if let Some(argument) = argument {
                    self.required_space();
                    self.emit_expr(argument, P_ASSIGN);
                }
            }
            IrNodeData::ImportExpression { source, options } => {
                self.keyword(id, "import");
                self.syntax("(");
                self.emit_expr(source, P_ASSIGN);
                if let Some(options) = options {
                    self.syntax(", ");
                    self.emit_expr(options, P_ASSIGN);
                }
                self.syntax(")");
            }
            IrNodeData::ArrowFunction {
                parameters,
                body,
                body_kind,
                is_async,
            } => self.emit_arrow(id, parameters, body, body_kind, is_async),
            IrNodeData::MethodDefinition {
                key,
                value,
                kind,
                is_static,
                computed,
                decorators,
            } => self.emit_method_definition(id, key, value, kind, is_static, computed, decorators),
            IrNodeData::PropertyDefinition {
                key,
                value,
                is_static,
                computed,
                decorators,
                accessor,
            } => self.emit_property_definition(
                id, key, value, is_static, computed, decorators, accessor,
            ),
            IrNodeData::StaticBlock { body } => {
                self.keyword(id, "static");
                self.required_space();
                self.emit_statement_list_block(body);
            }
            IrNodeData::ArrayPattern { elements } => {
                self.syntax("[");
                self.emit_array_like(elements, false);
                self.syntax("]");
            }
            IrNodeData::ObjectPattern { properties, rest } => {
                self.syntax("{");
                let has_properties = !self.items(properties).is_empty();
                if has_properties || rest.is_some() {
                    self.optional_space();
                }
                self.emit_list(properties, ", ", Self::emit_node);
                if let Some(rest) = rest {
                    if has_properties {
                        self.syntax(", ");
                    }
                    self.emit_node(rest);
                }
                if has_properties || rest.is_some() {
                    self.optional_space();
                }
                self.syntax("}");
            }
            IrNodeData::ObjectPatternProperty {
                key,
                value,
                shorthand,
                computed,
            } => self.emit_pattern_property(key, value, shorthand, computed),
            IrNodeData::AssignmentPattern { left, right } => {
                self.emit_node(left);
                self.syntax(" = ");
                self.emit_expr(right, P_ASSIGN);
            }
            IrNodeData::RestPattern { argument } => {
                self.syntax("...");
                self.emit_node(argument);
            }
            IrNodeData::ImportDeclaration {
                specifiers,
                source,
                attributes,
            } => self.emit_import(id, specifiers, source, attributes),
            IrNodeData::ImportSpecifier {
                kind,
                imported,
                local,
            } => self.emit_import_specifier(kind, imported, local),
            IrNodeData::ImportAttributes { keyword, items } => {
                self.source_token(id, keyword.as_str());
                self.required_space();
                self.syntax("{");
                if !self.items(items).is_empty() {
                    self.optional_space();
                    self.emit_list(items, ", ", Self::emit_node);
                    self.optional_space();
                }
                self.syntax("}");
            }
            IrNodeData::ImportAttribute { key, value } => {
                self.emit_module_name(key);
                self.syntax(": ");
                self.emit_string_node(value);
            }
            IrNodeData::ExportNamedDeclaration {
                declaration,
                specifiers,
                source,
                attributes,
            } => self.emit_export_named(id, declaration, specifiers, source, attributes),
            IrNodeData::ExportSpecifier { local, exported } => {
                self.emit_module_name(local);
                if !self.same_module_name(local, exported) {
                    self.required_space();
                    self.keyword(id, "as");
                    self.required_space();
                    self.emit_module_name(exported);
                }
            }
            IrNodeData::ExportDefaultDeclaration { value, kind } => {
                self.keyword(id, "export");
                self.required_space();
                self.keyword(id, "default");
                self.required_space();
                match kind {
                    ExportDefaultValueKind::Function | ExportDefaultValueKind::Class => {
                        self.emit_node(value);
                    }
                    ExportDefaultValueKind::Expression => self.emit_expr(value, P_ASSIGN),
                }
                if matches!(kind, ExportDefaultValueKind::Expression) {
                    self.syntax(";");
                }
            }
            IrNodeData::ExportAllDeclaration {
                exported,
                source,
                attributes,
            } => self.emit_export_all(id, exported, source, attributes),
        }
        if let Some((start, target_module_id)) = discarded_request {
            let end = self.out.len();
            self.discarded_static_requests
                .push(GeneratedDiscardedStaticRequest {
                    start: u32::try_from(start)
                        .expect("typed module body exceeds generated request range capacity"),
                    end: u32::try_from(end)
                        .expect("typed module body exceeds generated request range capacity"),
                    target_module_id,
                });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_program(
        &mut self,
        _id: NodeId,
        source_type: SourceType,
        strict: bool,
        spread_helper: Option<&str>,
        object_spread_helper: Option<&str>,
        for_of_helper: Option<&str>,
        body: ListId,
    ) {
        // Source type and strictness are analysis facts. Directives in the body carry the emitted
        // syntax; manufacturing another directive here would duplicate source semantics.
        let _ = (source_type, strict);
        assert!(
            spread_helper.is_none() && object_spread_helper.is_none() && for_of_helper.is_none(),
            "typed runtime-helper metadata must be materialized before code generation"
        );
        for (index, &statement) in self.items(body).iter().enumerate() {
            if index != 0 {
                self.newline();
            }
            self.emit_node(statement);
        }
    }

    fn emit_variable_declaration(
        &mut self,
        id: NodeId,
        kind: VarKind,
        declarations: ListId,
        terminate: bool,
    ) {
        self.source_token(id, kind.as_str());
        self.required_space();
        self.emit_list(declarations, ", ", Self::emit_node);
        if terminate {
            self.syntax(";");
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_function(
        &mut self,
        id: NodeId,
        context: FunctionContext,
        name: Option<NodeId>,
        parameters: ListId,
        body: Option<NodeId>,
        is_async: bool,
        is_generator: bool,
    ) {
        match context {
            FunctionContext::Declaration
            | FunctionContext::Expression
            | FunctionContext::ExportDefault => {
                if is_async {
                    self.keyword(id, "async");
                    self.required_space();
                }
                self.keyword(id, "function");
                if is_generator {
                    self.syntax("*");
                }
                if let Some(name) = name {
                    self.required_space();
                    self.emit_node(name);
                }
                self.emit_parameters(parameters);
                self.optional_space();
                if let Some(body) = body {
                    self.emit_node(body);
                } else {
                    self.syntax("{}");
                }
            }
            FunctionContext::Method => {
                self.emit_method_function(id, parameters, body, is_async, is_generator, None);
            }
        }
    }

    fn emit_parameters(&mut self, parameters: ListId) {
        self.syntax("(");
        self.emit_list(parameters, ", ", Self::emit_node);
        self.syntax(")");
    }

    fn emit_function_body(&mut self, id: NodeId, statements: ListId, strict: bool) {
        let _ = strict;
        self.emit_statement_list_block_with_id(id, statements);
    }

    fn emit_statement_list_block(&mut self, statements: ListId) {
        self.syntax("{");
        let len = self.items(statements).len();
        if len > 0 {
            self.indent += 1;
            for index in 0..len {
                self.newline();
                let statement = self.items(statements)[index];
                self.emit_node(statement);
            }
            self.indent -= 1;
            self.newline();
        }
        self.syntax("}");
    }

    fn emit_statement_list_block_with_id(&mut self, id: NodeId, statements: ListId) {
        self.mark_origin(self.node(id).origin(), None);
        self.emit_statement_list_block(statements);
    }

    fn emit_block(&mut self, id: NodeId, body: ListId) {
        self.emit_statement_list_block_with_id(id, body);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_class(
        &mut self,
        id: NodeId,
        context: ClassContext,
        name: Option<NodeId>,
        super_class: Option<NodeId>,
        members: ListId,
        decorators: ListId,
    ) {
        self.emit_decorators(decorators, true);
        match context {
            ClassContext::Declaration | ClassContext::Expression | ClassContext::ExportDefault => {}
        }
        self.keyword(id, "class");
        if let Some(name) = name {
            self.required_space();
            self.emit_node(name);
        }
        if let Some(super_class) = super_class {
            self.required_space();
            self.keyword(id, "extends");
            self.required_space();
            self.emit_expr(super_class, P_RELATIONAL + 1);
        }
        self.optional_space();
        self.syntax("{");
        let member_count = self.items(members).len();
        if member_count > 0 {
            self.indent += 1;
            for index in 0..member_count {
                self.newline();
                let member = self.items(members)[index];
                self.emit_node(member);
            }
            self.indent -= 1;
            self.newline();
        }
        self.syntax("}");
    }

    fn emit_decorators(&mut self, decorators: ListId, multiline: bool) {
        let len = self.items(decorators).len();
        for index in 0..len {
            let decorator = self.items(decorators)[index];
            self.syntax("@");
            self.emit_expr(decorator, P_ASSIGN);
            if multiline {
                if self.minify {
                    self.required_space();
                } else {
                    self.newline();
                }
            } else {
                self.required_space();
            }
        }
    }

    fn emit_if(&mut self, id: NodeId, test: NodeId, consequent: NodeId, alternate: Option<NodeId>) {
        self.keyword(id, "if");
        self.optional_space();
        self.syntax("(");
        self.emit_expr(test, P_SEQUENCE);
        self.syntax(") ");
        let needs_block = alternate.is_some() && self.has_dangling_else(consequent);
        if needs_block {
            self.syntax("{");
            self.emit_node(consequent);
            self.syntax("}");
        } else {
            self.emit_node(consequent);
        }
        if let Some(alternate) = alternate {
            self.optional_space();
            self.keyword(id, "else");
            self.required_space();
            self.emit_node(alternate);
        }
    }

    fn has_dangling_else(&self, id: NodeId) -> bool {
        match self.node(id).data() {
            IrNodeData::IfStatement {
                alternate: None, ..
            } => true,
            IrNodeData::IfStatement {
                alternate: Some(_), ..
            }
            | IrNodeData::Block { .. } => false,
            IrNodeData::LabeledStatement { body, .. }
            | IrNodeData::WhileStatement { body, .. }
            | IrNodeData::ForStatement { body, .. }
            | IrNodeData::ForInStatement { body, .. }
            | IrNodeData::ForOfStatement { body, .. }
            | IrNodeData::WithStatement { body, .. } => self.has_dangling_else(*body),
            _ => false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_for(
        &mut self,
        id: NodeId,
        initializer: Option<NodeId>,
        initializer_kind: Option<ForInitializerKind>,
        test: Option<NodeId>,
        update: Option<NodeId>,
        body: NodeId,
    ) {
        self.keyword(id, "for");
        self.optional_space();
        self.syntax("(");
        match (initializer, initializer_kind) {
            (Some(initializer), Some(ForInitializerKind::Variable)) => {
                let IrNodeData::VariableDeclaration { kind, declarations } =
                    *self.node(initializer).data()
                else {
                    panic!("for initializer kind disagrees with typed node")
                };
                self.emit_variable_declaration(initializer, kind, declarations, false);
            }
            (Some(initializer), Some(ForInitializerKind::Expression)) => {
                let wrap_in = self.contains_in_operator(initializer);
                if wrap_in {
                    self.syntax("(");
                }
                self.emit_expr(initializer, P_SEQUENCE);
                if wrap_in {
                    self.syntax(")");
                }
            }
            (None, None) => {}
            _ => panic!("for initializer and kind must be present together"),
        }
        self.syntax("; ");
        if let Some(test) = test {
            self.emit_expr(test, P_SEQUENCE);
        }
        self.syntax("; ");
        if let Some(update) = update {
            self.emit_expr(update, P_SEQUENCE);
        }
        self.syntax(") ");
        self.emit_node(body);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_for_in_of(
        &mut self,
        id: NodeId,
        left: NodeId,
        left_kind: ForLeftKind,
        right: NodeId,
        body: NodeId,
        is_of: bool,
        is_await: bool,
    ) {
        self.keyword(id, "for");
        if is_await {
            self.required_space();
            self.keyword(id, "await");
        }
        self.optional_space();
        self.syntax("(");
        match left_kind {
            ForLeftKind::Variable => {
                let IrNodeData::VariableDeclaration { kind, declarations } =
                    *self.node(left).data()
                else {
                    panic!("for-left kind disagrees with typed node")
                };
                self.emit_variable_declaration(left, kind, declarations, false);
            }
            ForLeftKind::Target => self.emit_expr(left, P_ASSIGN),
        }
        self.required_space();
        self.keyword(id, if is_of { "of" } else { "in" });
        self.required_space();
        self.emit_expr(right, if is_of { P_ASSIGN } else { P_SEQUENCE });
        self.syntax(") ");
        self.emit_node(body);
    }

    fn contains_in_operator(&self, id: NodeId) -> bool {
        match self.node(id).data() {
            IrNodeData::BinaryExpression {
                operator: BinaryOperator::In,
                ..
            } => true,
            IrNodeData::BinaryExpression { left, right, .. }
            | IrNodeData::LogicalExpression { left, right, .. }
            | IrNodeData::AssignmentExpression { left, right, .. } => {
                self.contains_in_operator(*left) || self.contains_in_operator(*right)
            }
            IrNodeData::SequenceExpression { expressions } => self
                .items(*expressions)
                .iter()
                .any(|expression| self.contains_in_operator(*expression)),
            IrNodeData::ConditionalExpression {
                test,
                consequent,
                alternate,
            } => {
                self.contains_in_operator(*test)
                    || self.contains_in_operator(*consequent)
                    || self.contains_in_operator(*alternate)
            }
            // Nested function/class bodies and computed literals establish contexts in which `in`
            // cannot be mistaken for the surrounding `for-in` delimiter.
            _ => false,
        }
    }

    fn emit_switch(&mut self, id: NodeId, discriminant: NodeId, cases: ListId) {
        self.keyword(id, "switch");
        self.optional_space();
        self.syntax("(");
        self.emit_expr(discriminant, P_SEQUENCE);
        self.syntax(") ");
        self.syntax("{");
        let case_count = self.items(cases).len();
        if case_count > 0 {
            self.indent += 1;
            for index in 0..case_count {
                self.newline();
                let case = self.items(cases)[index];
                self.emit_node(case);
            }
            self.indent -= 1;
            self.newline();
        }
        self.syntax("}");
    }

    fn emit_switch_case(&mut self, id: NodeId, test: Option<NodeId>, consequent: ListId) {
        if let Some(test) = test {
            self.keyword(id, "case");
            self.required_space();
            self.emit_expr(test, P_SEQUENCE);
        } else {
            self.keyword(id, "default");
        }
        self.syntax(":");
        let consequent_count = self.items(consequent).len();
        if consequent_count > 0 {
            self.indent += 1;
            for index in 0..consequent_count {
                self.newline();
                let statement = self.items(consequent)[index];
                self.emit_node(statement);
            }
            self.indent -= 1;
        }
    }

    fn emit_try(
        &mut self,
        id: NodeId,
        block: NodeId,
        handler: Option<NodeId>,
        finalizer: Option<NodeId>,
    ) {
        self.keyword(id, "try");
        self.required_space();
        self.emit_node(block);
        if let Some(handler) = handler {
            self.required_space();
            self.emit_node(handler);
        }
        if let Some(finalizer) = finalizer {
            self.required_space();
            self.keyword(id, "finally");
            self.required_space();
            self.emit_node(finalizer);
        }
    }

    fn emit_expr(&mut self, id: NodeId, minimum_precedence: u8) {
        let precedence = self.expression_precedence(id);
        let parenthesize = precedence < minimum_precedence;
        if parenthesize {
            self.syntax("(");
        }
        self.emit_node(id);
        if parenthesize {
            self.syntax(")");
        }
    }

    /// Exhaustive precedence classification intentionally names every IR variant so a newly added
    /// syntax kind cannot silently inherit an unsuitable default.
    fn expression_precedence(&self, id: NodeId) -> u8 {
        match self.node(id).data() {
            IrNodeData::SequenceExpression { .. } => P_SEQUENCE,
            IrNodeData::AssignmentExpression { .. }
            | IrNodeData::ArrowFunction { .. }
            | IrNodeData::YieldExpression { .. } => P_ASSIGN,
            IrNodeData::ConditionalExpression { .. } => P_CONDITIONAL,
            IrNodeData::LogicalExpression { operator, .. } => logical_precedence(*operator),
            IrNodeData::BinaryExpression { operator, .. } => binary_precedence(*operator),
            IrNodeData::UnaryExpression { .. }
            | IrNodeData::AwaitExpression { .. }
            | IrNodeData::SpreadElement { .. } => P_UNARY,
            IrNodeData::BooleanLiteral { .. } if self.minify => P_UNARY,
            IrNodeData::NumberLiteral { value } if number_token_starts_with_minus(*value) => {
                P_UNARY
            }
            IrNodeData::UpdateExpression { prefix: true, .. } => P_UNARY,
            IrNodeData::UpdateExpression { prefix: false, .. } => P_POSTFIX,
            IrNodeData::CallExpression { .. }
            | IrNodeData::NewExpression { .. }
            | IrNodeData::MemberExpression { .. }
            | IrNodeData::TaggedTemplateExpression { .. } => P_CALL_MEMBER,
            IrNodeData::NumberLiteral { .. }
            | IrNodeData::StringLiteral { .. }
            | IrNodeData::BooleanLiteral { .. }
            | IrNodeData::NullLiteral
            | IrNodeData::BigIntLiteral { .. }
            | IrNodeData::RegExpLiteral { .. }
            | IrNodeData::TemplateLiteral { .. }
            | IrNodeData::Identifier { .. }
            | IrNodeData::ThisExpression
            | IrNodeData::SuperExpression
            | IrNodeData::MetaProperty { .. }
            | IrNodeData::ArrayExpression { .. }
            | IrNodeData::ObjectExpression { .. }
            | IrNodeData::Function { .. }
            | IrNodeData::Class { .. }
            | IrNodeData::ImportExpression { .. } => P_PRIMARY,
            IrNodeData::Program { .. }
            | IrNodeData::VariableDeclaration { .. }
            | IrNodeData::VariableDeclarator { .. }
            | IrNodeData::FunctionBody { .. }
            | IrNodeData::Block { .. }
            | IrNodeData::EmptyStatement
            | IrNodeData::DebuggerStatement
            | IrNodeData::ExpressionStatement { .. }
            | IrNodeData::IfStatement { .. }
            | IrNodeData::ForStatement { .. }
            | IrNodeData::ForInStatement { .. }
            | IrNodeData::ForOfStatement { .. }
            | IrNodeData::WhileStatement { .. }
            | IrNodeData::DoWhileStatement { .. }
            | IrNodeData::SwitchStatement { .. }
            | IrNodeData::SwitchCase { .. }
            | IrNodeData::ReturnStatement { .. }
            | IrNodeData::BreakStatement { .. }
            | IrNodeData::ContinueStatement { .. }
            | IrNodeData::ThrowStatement { .. }
            | IrNodeData::TryStatement { .. }
            | IrNodeData::CatchClause { .. }
            | IrNodeData::LabeledStatement { .. }
            | IrNodeData::WithStatement { .. }
            | IrNodeData::TemplateElement { .. }
            | IrNodeData::Name { .. }
            | IrNodeData::Elision
            | IrNodeData::ObjectProperty { .. }
            | IrNodeData::MethodDefinition { .. }
            | IrNodeData::PropertyDefinition { .. }
            | IrNodeData::StaticBlock { .. }
            | IrNodeData::ArrayPattern { .. }
            | IrNodeData::ObjectPattern { .. }
            | IrNodeData::ObjectPatternProperty { .. }
            | IrNodeData::AssignmentPattern { .. }
            | IrNodeData::RestPattern { .. }
            | IrNodeData::ImportDeclaration { .. }
            | IrNodeData::ImportSpecifier { .. }
            | IrNodeData::ImportAttributes { .. }
            | IrNodeData::ImportAttribute { .. }
            | IrNodeData::ExportNamedDeclaration { .. }
            | IrNodeData::ExportSpecifier { .. }
            | IrNodeData::ExportDefaultDeclaration { .. }
            | IrNodeData::ExportAllDeclaration { .. } => {
                panic!(
                    "non-expression node {} reached expression emitter",
                    id.index()
                )
            }
        }
    }

    fn starts_problematic(&self, id: NodeId) -> bool {
        match self.node(id).data() {
            IrNodeData::ObjectExpression { .. }
            | IrNodeData::Function {
                context: FunctionContext::Expression,
                ..
            }
            | IrNodeData::Class {
                context: ClassContext::Expression,
                ..
            } => true,
            IrNodeData::AssignmentExpression { left, .. }
            | IrNodeData::BinaryExpression { left, .. }
            | IrNodeData::LogicalExpression { left, .. } => self.starts_problematic(*left),
            IrNodeData::MemberExpression { object, .. } => self.starts_problematic(*object),
            IrNodeData::CallExpression { callee, .. } => self.starts_problematic(*callee),
            IrNodeData::ConditionalExpression { test, .. } => self.starts_problematic(*test),
            IrNodeData::SequenceExpression { expressions } => self
                .items(*expressions)
                .first()
                .is_some_and(|first| self.starts_problematic(*first)),
            IrNodeData::TaggedTemplateExpression { tag, .. } => self.starts_problematic(*tag),
            _ => false,
        }
    }

    fn emit_binary(&mut self, id: NodeId, operator: BinaryOperator, left: NodeId, right: NodeId) {
        let precedence = binary_precedence(operator);
        if operator == BinaryOperator::Exp
            && matches!(
                self.node(left).data(),
                IrNodeData::UnaryExpression { .. } | IrNodeData::AwaitExpression { .. }
            )
        {
            self.syntax("(");
            self.emit_expr(left, P_ASSIGN);
            self.syntax(")");
        } else {
            self.emit_expr(
                left,
                if operator == BinaryOperator::Exp {
                    precedence + 1
                } else {
                    precedence
                },
            );
        }
        self.binop(id, operator.as_str());
        self.emit_expr(
            right,
            if operator == BinaryOperator::Exp {
                precedence
            } else {
                precedence + 1
            },
        );
    }

    fn emit_logical(&mut self, id: NodeId, operator: LogicalOperator, left: NodeId, right: NodeId) {
        let precedence = logical_precedence(operator);
        let group_left = operator == LogicalOperator::Coalesce && self.is_and_or(left);
        if group_left {
            self.syntax("(");
        }
        self.emit_expr(left, if group_left { P_ASSIGN } else { precedence });
        if group_left {
            self.syntax(")");
        }
        self.binop(id, operator.as_str());
        let group_right = operator == LogicalOperator::Coalesce && self.is_and_or(right);
        if group_right {
            self.syntax("(");
        }
        self.emit_expr(
            right,
            if group_right {
                P_ASSIGN
            } else {
                precedence + 1
            },
        );
        if group_right {
            self.syntax(")");
        }
    }

    fn is_and_or(&self, id: NodeId) -> bool {
        matches!(
            self.node(id).data(),
            IrNodeData::LogicalExpression {
                operator: LogicalOperator::And | LogicalOperator::Or,
                ..
            }
        )
    }

    fn emit_arguments(&mut self, arguments: ListId) {
        self.syntax("(");
        self.emit_list(arguments, ", ", |this, argument| {
            this.emit_expr(argument, P_ASSIGN);
        });
        self.syntax(")");
    }

    fn emit_member(
        &mut self,
        object: NodeId,
        property: NodeId,
        property_kind: PropertyKeyKind,
        optional: bool,
    ) {
        let number_object = matches!(self.node(object).data(), IrNodeData::NumberLiteral { .. });
        if number_object {
            self.syntax("(");
            self.emit_expr(object, P_ASSIGN);
            self.syntax(")");
        } else {
            self.emit_expr(object, P_CALL_MEMBER);
        }
        match property_kind {
            PropertyKeyKind::Identifier => {
                self.syntax(if optional { "?." } else { "." });
                self.emit_node(property);
            }
            PropertyKeyKind::Private => {
                self.syntax(if optional { "?.#" } else { ".#" });
                self.emit_node(property);
            }
            PropertyKeyKind::Computed => {
                if optional {
                    self.syntax("?.");
                }
                self.syntax("[");
                self.emit_expr(property, P_SEQUENCE);
                self.syntax("]");
            }
            PropertyKeyKind::String | PropertyKeyKind::Number => {
                panic!("member property cannot use literal property-key kind")
            }
        }
    }

    fn emit_template(&mut self, _id: NodeId, quasis: ListId, expressions: ListId) {
        let quasi_count = self.items(quasis).len();
        let expression_count = self.items(expressions).len();
        self.syntax("`");
        for index in 0..quasi_count {
            let quasi = self.items(quasis)[index];
            self.emit_node(quasi);
            if index < expression_count {
                self.syntax("${");
                let expression = self.items(expressions)[index];
                self.emit_expr(expression, P_SEQUENCE);
                self.syntax("}");
            }
        }
        self.syntax("`");
    }

    fn emit_arrow(
        &mut self,
        id: NodeId,
        parameters: ListId,
        body: NodeId,
        body_kind: ArrowBodyKind,
        is_async: bool,
    ) {
        if is_async {
            self.keyword(id, "async");
            self.required_space();
        }
        let single_identifier = (self.minify && self.items(parameters).len() == 1)
            .then(|| self.items(parameters)[0])
            .filter(|parameter| {
                matches!(self.node(*parameter).data(), IrNodeData::Identifier { .. })
            });
        if let Some(parameter) = single_identifier {
            self.emit_node(parameter);
        } else {
            // The opening parenthesis is the stable boundary token for a parenthesized arrow.
            // Mapping it through the ArrowFunction origin preserves the original `() =>`
            // location, while optimizer-created export getters remain unmapped by origin.
            self.source_token(id, "(");
            self.emit_list(parameters, ", ", Self::emit_node);
            self.syntax(")");
        }
        self.syntax(" => ");
        match body_kind {
            ArrowBodyKind::Block => self.emit_node(body),
            ArrowBodyKind::Expression => {
                let object = matches!(self.node(body).data(), IrNodeData::ObjectExpression { .. });
                if object {
                    self.syntax("(");
                }
                self.emit_expr(body, P_ASSIGN);
                if object {
                    self.syntax(")");
                }
            }
        }
    }

    fn emit_array_like(&mut self, elements: ListId, expression: bool) {
        let len = self.items(elements).len();
        for index in 0..len {
            let element = self.items(elements)[index];
            if index > 0 {
                self.syntax(", ");
            }
            if matches!(self.node(element).data(), IrNodeData::Elision) {
                // The surrounding separators are the complete syntax of an elision.
            } else if expression {
                self.emit_expr(element, P_ASSIGN);
            } else {
                self.emit_node(element);
            }
        }
        if len > 0
            && matches!(
                self.node(self.items(elements)[len - 1]).data(),
                IrNodeData::Elision
            )
        {
            self.syntax(",");
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_method_definition(
        &mut self,
        id: NodeId,
        key: IrPropertyKey,
        value: NodeId,
        kind: MethodKind,
        is_static: bool,
        computed: bool,
        decorators: ListId,
    ) {
        self.emit_decorators(decorators, true);
        if is_static {
            self.keyword(id, "static");
            self.required_space();
        }
        let IrNodeData::Function {
            context,
            name,
            parameters,
            body,
            is_async,
            is_generator,
        } = *self.node(value).data()
        else {
            panic!("method definition value must be a function")
        };
        assert_eq!(context, FunctionContext::Method);
        assert!(
            name.is_none(),
            "method function cannot carry a declaration name"
        );
        if is_async {
            self.keyword(value, "async");
            self.required_space();
        }
        match kind {
            MethodKind::Constructor | MethodKind::Method => {}
            MethodKind::Get => {
                self.keyword(id, "get");
                self.required_space();
            }
            MethodKind::Set => {
                self.keyword(id, "set");
                self.required_space();
            }
        }
        if is_generator {
            self.syntax("*");
        }
        self.emit_property_key(key, computed);
        self.emit_parameters(parameters);
        self.optional_space();
        if let Some(body) = body {
            self.emit_node(body);
        } else {
            self.syntax("{}");
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_property_definition(
        &mut self,
        id: NodeId,
        key: IrPropertyKey,
        value: Option<NodeId>,
        is_static: bool,
        computed: bool,
        decorators: ListId,
        accessor: bool,
    ) {
        self.emit_decorators(decorators, true);
        if is_static {
            self.keyword(id, "static");
            self.required_space();
        }
        if accessor {
            self.keyword(id, "accessor");
            self.required_space();
        }
        self.emit_property_key(key, computed);
        if let Some(value) = value {
            self.syntax(" = ");
            self.emit_expr(value, P_ASSIGN);
        }
        self.syntax(";");
    }

    fn emit_method_function(
        &mut self,
        id: NodeId,
        parameters: ListId,
        body: Option<NodeId>,
        is_async: bool,
        is_generator: bool,
        kind: Option<PropertyKind>,
    ) {
        if is_async {
            self.keyword(id, "async");
            self.required_space();
        }
        match kind {
            Some(PropertyKind::Get) => {
                self.keyword(id, "get");
                self.required_space();
            }
            Some(PropertyKind::Set) => {
                self.keyword(id, "set");
                self.required_space();
            }
            Some(PropertyKind::Init) | None => {}
        }
        if is_generator {
            self.syntax("*");
        }
        self.emit_parameters(parameters);
        self.optional_space();
        if let Some(body) = body {
            self.emit_node(body);
        } else {
            self.syntax("{}");
        }
    }

    fn emit_property_key(&mut self, key: IrPropertyKey, computed: bool) {
        assert_eq!(
            computed,
            key.kind == PropertyKeyKind::Computed,
            "computed property flag disagrees with typed property key"
        );
        match key.kind {
            PropertyKeyKind::Identifier => self.emit_node(key.value),
            PropertyKeyKind::String => self.emit_string_node(key.value),
            PropertyKeyKind::Number => self.emit_expr(key.value, P_PRIMARY),
            PropertyKeyKind::Computed => {
                self.syntax("[");
                self.emit_expr(key.value, P_ASSIGN);
                self.syntax("]");
            }
            PropertyKeyKind::Private => {
                self.syntax("#");
                self.emit_node(key.value);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_object_property(
        &mut self,
        id: NodeId,
        key: IrPropertyKey,
        value: NodeId,
        kind: PropertyKind,
        method: bool,
        shorthand: bool,
        computed: bool,
        prototype_setter: bool,
    ) {
        let _ = prototype_setter;
        if method || !matches!(kind, PropertyKind::Init) {
            let IrNodeData::Function {
                context,
                name,
                parameters,
                body,
                is_async,
                is_generator,
            } = *self.node(value).data()
            else {
                panic!("object method/accessor value must be a function")
            };
            assert!(
                matches!(
                    context,
                    FunctionContext::Method | FunctionContext::Expression
                ),
                "object method function has declaration-only context"
            );
            assert!(
                name.is_none(),
                "object method cannot carry a declaration name"
            );
            if is_async {
                self.keyword(value, "async");
                self.required_space();
            }
            match kind {
                PropertyKind::Init => {}
                PropertyKind::Get => {
                    self.keyword(id, "get");
                    self.required_space();
                }
                PropertyKind::Set => {
                    self.keyword(id, "set");
                    self.required_space();
                }
            }
            if is_generator {
                self.syntax("*");
            }
            self.emit_property_key(key, computed);
            self.emit_parameters(parameters);
            self.optional_space();
            if let Some(body) = body {
                self.emit_node(body);
            } else {
                self.syntax("{}");
            }
            return;
        }

        if shorthand && !computed && self.key_matches_value(key, value) {
            self.emit_node(value);
        } else {
            self.emit_property_key(key, computed);
            self.syntax(": ");
            self.emit_expr(value, P_ASSIGN);
        }
    }

    fn emit_pattern_property(
        &mut self,
        key: IrPropertyKey,
        value: NodeId,
        shorthand: bool,
        computed: bool,
    ) {
        if shorthand && !computed && self.key_matches_pattern(key, value) {
            self.emit_node(value);
        } else {
            self.emit_property_key(key, computed);
            self.syntax(": ");
            self.emit_node(value);
        }
    }

    fn key_matches_value(&self, key: IrPropertyKey, value: NodeId) -> bool {
        let IrNodeData::Identifier { name } = self.node(value).data() else {
            return false;
        };
        self.key_matches_name(key, *name)
    }

    fn key_matches_pattern(&self, key: IrPropertyKey, value: NodeId) -> bool {
        match self.node(value).data() {
            IrNodeData::Identifier { name } => self.key_matches_name(key, *name),
            IrNodeData::AssignmentPattern { left, .. } => match self.node(*left).data() {
                IrNodeData::Identifier { name } => self.key_matches_name(key, *name),
                _ => false,
            },
            _ => false,
        }
    }

    fn key_matches_name(&self, key: IrPropertyKey, name: NodeId) -> bool {
        key.kind == PropertyKeyKind::Identifier
            && self.name_text(key.value).0 == self.name_text(name).0
    }

    fn emit_module_name(&mut self, name: IrModuleName) {
        match name.kind {
            ModuleNameKind::Identifier => self.emit_node(name.value),
            ModuleNameKind::String => self.emit_string_node(name.value),
        }
    }

    fn same_module_name(&self, left: IrModuleName, right: IrModuleName) -> bool {
        left.kind == right.kind && self.name_text(left.value).0 == self.name_text(right.value).0
    }

    fn import_name_matches_local(&self, imported: IrModuleName, local: NodeId) -> bool {
        imported.kind == ModuleNameKind::Identifier
            && self.name_text(imported.value).0 == self.name_text(local).0
    }

    fn emit_import(
        &mut self,
        id: NodeId,
        specifiers: ListId,
        source: NodeId,
        attributes: Option<NodeId>,
    ) {
        self.keyword(id, "import");
        let specifiers = self.items(specifiers);
        if specifiers.is_empty() {
            self.required_space();
            self.emit_string_node(source);
        } else {
            self.required_space();
            let mut default = None;
            let mut namespace = None;
            let mut named = Vec::new();
            for &specifier in specifiers {
                let IrNodeData::ImportSpecifier { kind, .. } = self.node(specifier).data() else {
                    panic!("import specifier list contains non-specifier node")
                };
                match kind {
                    ImportSpecifierKind::Default => {
                        assert!(
                            default.replace(specifier).is_none(),
                            "duplicate default import"
                        )
                    }
                    ImportSpecifierKind::Namespace => {
                        assert!(
                            namespace.replace(specifier).is_none(),
                            "duplicate namespace import"
                        )
                    }
                    ImportSpecifierKind::Named => named.push(specifier),
                }
            }
            let mut emitted = false;
            if let Some(default) = default {
                self.emit_node(default);
                emitted = true;
            }
            if let Some(namespace) = namespace {
                if emitted {
                    self.syntax(", ");
                }
                self.emit_node(namespace);
                emitted = true;
            }
            if !named.is_empty() {
                if emitted {
                    self.syntax(", ");
                }
                self.syntax("{");
                self.optional_space();
                for (index, specifier) in named.into_iter().enumerate() {
                    if index > 0 {
                        self.syntax(", ");
                    }
                    self.emit_node(specifier);
                }
                self.optional_space();
                self.syntax("}");
            }
            self.required_space();
            self.keyword(id, "from");
            self.required_space();
            self.emit_string_node(source);
        }
        if let Some(attributes) = attributes {
            self.required_space();
            self.emit_node(attributes);
        }
        self.syntax(";");
    }

    fn emit_import_specifier(
        &mut self,
        kind: ImportSpecifierKind,
        imported: Option<IrModuleName>,
        local: NodeId,
    ) {
        match kind {
            ImportSpecifierKind::Default => {
                assert!(
                    imported.is_none(),
                    "default import cannot have imported name"
                );
                self.emit_node(local);
            }
            ImportSpecifierKind::Namespace => {
                assert!(
                    imported.is_none(),
                    "namespace import cannot have imported name"
                );
                self.syntax("*");
                self.required_space();
                self.syntax("as");
                self.required_space();
                self.emit_node(local);
            }
            ImportSpecifierKind::Named => {
                let imported = imported.expect("named import must have imported name");
                self.emit_module_name(imported);
                if !self.import_name_matches_local(imported, local) {
                    self.required_space();
                    self.syntax("as");
                    self.required_space();
                    self.emit_node(local);
                }
            }
        }
    }

    fn emit_export_named(
        &mut self,
        id: NodeId,
        declaration: Option<NodeId>,
        specifiers: ListId,
        source: Option<NodeId>,
        attributes: Option<NodeId>,
    ) {
        self.keyword(id, "export");
        self.required_space();
        if let Some(declaration) = declaration {
            assert!(
                self.items(specifiers).is_empty() && source.is_none() && attributes.is_none(),
                "export declaration cannot also carry specifiers or a source"
            );
            self.emit_node(declaration);
            return;
        }
        self.syntax("{");
        if !self.items(specifiers).is_empty() {
            self.optional_space();
            self.emit_list(specifiers, ", ", Self::emit_node);
            self.optional_space();
        }
        self.syntax("}");
        if let Some(source) = source {
            self.required_space();
            self.keyword(id, "from");
            self.required_space();
            self.emit_string_node(source);
            if let Some(attributes) = attributes {
                self.required_space();
                self.emit_node(attributes);
            }
        } else {
            assert!(attributes.is_none(), "export attributes require a source");
        }
        self.syntax(";");
    }

    fn emit_export_all(
        &mut self,
        id: NodeId,
        exported: Option<IrModuleName>,
        source: NodeId,
        attributes: Option<NodeId>,
    ) {
        self.keyword(id, "export");
        self.required_space();
        self.syntax("*");
        if let Some(exported) = exported {
            self.required_space();
            self.keyword(id, "as");
            self.required_space();
            self.emit_module_name(exported);
        }
        self.required_space();
        self.keyword(id, "from");
        self.required_space();
        self.emit_string_node(source);
        if let Some(attributes) = attributes {
            self.required_space();
            self.emit_node(attributes);
        }
        self.syntax(";");
    }
}

fn binary_precedence(operator: BinaryOperator) -> u8 {
    use BinaryOperator::*;
    match operator {
        BitOr => P_BIT_OR,
        BitXor => P_BIT_XOR,
        BitAnd => P_BIT_AND,
        Eq | NotEq | StrictEq | StrictNotEq => P_EQUALITY,
        Lt | Gt | LtEq | GtEq | In | Instanceof => P_RELATIONAL,
        Shl | Shr | Ushr => P_SHIFT,
        Add | Sub => P_ADDITIVE,
        Mul | Div | Rem => P_MULTIPLICATIVE,
        Exp => P_EXPONENT,
    }
}

fn logical_precedence(operator: LogicalOperator) -> u8 {
    match operator {
        LogicalOperator::Or => P_LOGICAL_OR,
        LogicalOperator::And => P_LOGICAL_AND,
        LogicalOperator::Coalesce => P_COALESCE,
    }
}

fn number_source(value: f64, minify: bool) -> String {
    if minify || !value.is_finite() || (value == 0.0 && value.is_sign_negative()) {
        write_number_minified(value)
    } else {
        value.to_string()
    }
}

/// Whether [`number_source`] starts with a unary minus token.
///
/// Finite negative values, including negative zero, are emitted with a leading `-`. Non-finite
/// values are always materialized as parenthesized arithmetic (`(1/0)`, `(-1/0)`, or `(0/0)`), so
/// their first token already has primary precedence even when the IEEE-754 sign bit is set.
fn number_token_starts_with_minus(value: f64) -> bool {
    value.is_finite() && value.is_sign_negative()
}

fn quote_string(value: &str, minify: bool) -> String {
    let double = escaped_string(value, '"');
    if !minify {
        return double;
    }
    let single = escaped_string(value, '\'');
    if single.len() < double.len() {
        single
    } else {
        double
    }
}

fn escaped_string(value: &str, quote: char) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push(quote);
    for character in value.chars() {
        match character {
            character if character == quote => {
                output.push('\\');
                output.push(character);
            }
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{2028}' => output.push_str("\\u2028"),
            '\u{2029}' => output.push_str("\\u2029"),
            control if (control as u32) < 0x20 => {
                let _ = write!(output, "\\x{:02x}", control as u32);
            }
            character => output.push(character),
        }
    }
    output.push(quote);
    output
}

#[cfg(test)]
mod tests {
    use super::{number_source, number_token_starts_with_minus};

    #[test]
    fn number_precedence_classification_matches_emitted_token() {
        let negative_nan = f64::from_bits(f64::NAN.to_bits() | (1_u64 << 63));
        let values = [
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.5,
            -0.5,
            1_000.0,
            -1_000.0,
            f64::from_bits(1),
            f64::from_bits((1_u64 << 63) | 1),
            f64::MAX,
            f64::MIN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            negative_nan,
        ];

        for minify in [false, true] {
            for value in values {
                let emitted = number_source(value, minify);
                assert_eq!(
                    number_token_starts_with_minus(value),
                    emitted.starts_with('-'),
                    "classification disagrees for {value:?} emitted as {emitted:?} with minify={minify}"
                );
            }
        }
    }
}
