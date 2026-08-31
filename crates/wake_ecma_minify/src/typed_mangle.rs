//! Final deterministic renaming over the owned typed IR.
//!
//! Planning reads only [`TypedProgram`] and its same-revision [`TypedAnalysis`]. The commit phase
//! writes each selected occurrence's owned `IrName.emitted`; no parser AST, source coordinate, or
//! parser-span rename table participates.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use wake_ecma_ast::{AssignmentOperator, PropertyKind, UnaryOperator, VarKind};
use wake_ecma_semantic::{DeclKind, SymbolId};

use crate::typed_analysis::{TypedAnalysis, TypedScopeId};
use crate::typed_ir::{
    ClassContext, FunctionContext, IrNodeData, ListId, NameId, NameRole, NodeId, PropertyKeyKind,
    TypedIrError, TypedProgram,
};

const RUNTIME_NAMES: &[&str] = &[
    "exports",
    "module",
    "require",
    "__wake_require__",
    "__wake_interop_default",
    "__wake_interop_star",
    "globalThis",
];

const FIRST_NAME_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const REST_NAME_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

fn nth_name(mut index: usize) -> String {
    let mut name = String::new();
    name.push(FIRST_NAME_CHARS[index % FIRST_NAME_CHARS.len()] as char);
    index /= FIRST_NAME_CHARS.len();
    while index > 0 {
        index -= 1;
        name.push(REST_NAME_CHARS[index % REST_NAME_CHARS.len()] as char);
        index /= REST_NAME_CHARS.len();
    }
    name
}

fn is_reserved(name: &str) -> bool {
    matches!(
        name,
        "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "null"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
            | "let"
            | "static"
            | "using"
            | "implements"
            | "interface"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "arguments"
            | "eval"
            | "undefined"
            | "NaN"
            | "Infinity"
    )
}

/// Public, host, reflection, and protocol properties which never enter local-shape renaming.
const RESERVED_PROPERTIES: &[&str] = &[
    "__proto__",
    "prototype",
    "constructor",
    "name",
    "length",
    "caller",
    "callee",
    "arguments",
    "call",
    "apply",
    "bind",
    "toString",
    "valueOf",
    "toJSON",
    "then",
    "catch",
    "finally",
    "iterator",
    "asyncIterator",
    "dispose",
    "asyncDispose",
    "hasOwnProperty",
    "propertyIsEnumerable",
    "getOwnPropertyDescriptor",
    "getOwnPropertyDescriptors",
    "getOwnPropertyNames",
    "getOwnPropertySymbols",
    "defineProperty",
    "defineProperties",
    "getPrototypeOf",
    "setPrototypeOf",
    "keys",
    "values",
    "entries",
    "assign",
    "create",
    "freeze",
    "seal",
    "stringify",
    "parse",
    "children",
    "key",
    "ref",
    "props",
    "state",
    "context",
    "type",
    "value",
    "checked",
    "selected",
    "disabled",
    "className",
    "style",
    "id",
    "href",
    "src",
    "target",
    "event",
    "currentTarget",
    "preventDefault",
    "stopPropagation",
    "addEventListener",
    "removeEventListener",
    "querySelector",
    "querySelectorAll",
    "appendChild",
    "removeChild",
    "parentNode",
    "ownerDocument",
    "nodeType",
    "nodeName",
    "textContent",
    "innerHTML",
    "outerHTML",
    "dataset",
    "exports",
    "module",
    "require",
    "resolve",
    "filename",
    "dirname",
    "url",
    "path",
    "env",
    "platform",
    "versions",
    "stdout",
    "stderr",
    "stdin",
    "on",
    "once",
    "emit",
    "removeListener",
    "error",
    "message",
    "stack",
    "code",
    "status",
    "statusCode",
    "headers",
    "body",
    "default",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypedMangleStats {
    changed_occurrences: usize,
    renamed_symbols: usize,
    reused_slots: usize,
    renamed_private_names: usize,
    renamed_properties: usize,
}

impl TypedMangleStats {
    #[cfg(test)]
    pub const fn changed_occurrences(&self) -> usize {
        self.changed_occurrences
    }

    pub const fn renamed_symbols(&self) -> usize {
        self.renamed_symbols
    }

    pub const fn reused_slots(&self) -> usize {
        self.reused_slots
    }

    pub const fn renamed_private_names(&self) -> usize {
        self.renamed_private_names
    }

    pub const fn renamed_properties(&self) -> usize {
        self.renamed_properties
    }

    #[cfg(test)]
    pub const fn changed(&self) -> bool {
        self.changed_occurrences != 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypedMangleError {
    StaleAnalysis {
        program_revision: u64,
        analysis_revision: u64,
    },
    InvalidIr(TypedIrError),
}

impl fmt::Display for TypedMangleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleAnalysis {
                program_revision,
                analysis_revision,
            } => write!(
                formatter,
                "typed mangle requires a same-revision analysis (program {program_revision}, analysis {analysis_revision})"
            ),
            Self::InvalidIr(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TypedMangleError {}

impl From<TypedIrError> for TypedMangleError {
    fn from(error: TypedIrError) -> Self {
        Self::InvalidIr(error)
    }
}

/// Plan and atomically commit final typed-IR names.
pub fn mangle_typed_program(
    program: &mut TypedProgram,
    analysis: &TypedAnalysis,
    reserved: &[&str],
) -> Result<TypedMangleStats, TypedMangleError> {
    if program.revision() != analysis.revision() {
        return Err(TypedMangleError::StaleAnalysis {
            program_revision: program.revision(),
            analysis_revision: analysis.revision(),
        });
    }

    let metadata = Metadata::collect(program)?;
    let mut changes = BTreeMap::<NameId, String>::new();
    let (symbol_names, reused_slots) = plan_symbol_names(program, analysis, &metadata, reserved);
    for (&symbol, emitted) in &symbol_names {
        program.set_symbol_emitted_name(symbol, emitted.clone())?;
        for &name in metadata
            .symbol_occurrences
            .get(symbol as usize)
            .into_iter()
            .flatten()
        {
            changes.insert(name, emitted.clone());
        }
    }
    let renamed_private_names = plan_private_names(program, &metadata, &mut changes);
    let renamed_properties = plan_closed_properties(program, analysis, &metadata, &mut changes);

    let mut changed_occurrences = 0usize;
    for (name, emitted) in changes {
        if program
            .name(name)
            .is_some_and(|current| current.emitted() != emitted)
        {
            program.set_emitted_name(name, emitted)?;
            changed_occurrences += 1;
        }
    }
    Ok(TypedMangleStats {
        changed_occurrences,
        renamed_symbols: symbol_names.len(),
        reused_slots,
        renamed_private_names,
        renamed_properties,
    })
}

#[derive(Default)]
struct Metadata {
    preorder: Vec<NodeId>,
    rank: Vec<usize>,
    unresolved_names: BTreeSet<String>,
    direct_export_symbols: BTreeSet<SymbolId>,
    runtime_observed_symbols: BTreeSet<SymbolId>,
    runtime_carriers: BTreeSet<SymbolId>,
    named_expression_symbols: BTreeSet<SymbolId>,
    classes: Vec<NodeId>,
    direct_eval_calls: Vec<NodeId>,
    const_declarators: Vec<NodeId>,
    symbol_occurrences: Vec<Vec<NameId>>,
}

impl Metadata {
    fn collect(program: &TypedProgram) -> Result<Self, TypedIrError> {
        let preorder = program.preorder_validated()?;
        let mut metadata = Self {
            rank: vec![usize::MAX; program.nodes().len()],
            preorder: preorder.clone(),
            symbol_occurrences: vec![Vec::new(); program.symbols().len()],
            ..Self::default()
        };
        for (rank, &node) in preorder.iter().enumerate() {
            metadata.rank[node.index()] = rank;
            let data = program.node(node).expect("validated typed node").data();
            match data {
                IrNodeData::Name { name } => {
                    let name_id = *name;
                    let name = program.name(name_id).expect("validated name");
                    if let Some(symbol) = name.symbol()
                        && let Some(occurrences) =
                            metadata.symbol_occurrences.get_mut(symbol as usize)
                    {
                        occurrences.push(name_id);
                    }
                    if name.symbol().is_none()
                        && matches!(
                            name.role(),
                            NameRole::Reference | NameRole::AssignmentTarget
                        )
                    {
                        metadata.unresolved_names.insert(name.original().to_owned());
                    }
                }
                IrNodeData::Function { context, name, .. } => {
                    if let Some(symbol) = name.and_then(|name| symbol_of_name_node(program, name)) {
                        metadata.runtime_carriers.insert(symbol);
                        if *context == FunctionContext::Expression {
                            metadata.named_expression_symbols.insert(symbol);
                        }
                    }
                }
                IrNodeData::Class { context, name, .. } => {
                    metadata.classes.push(node);
                    if let Some(symbol) = name.and_then(|name| symbol_of_name_node(program, name)) {
                        metadata.runtime_carriers.insert(symbol);
                        if *context == ClassContext::Expression {
                            metadata.named_expression_symbols.insert(symbol);
                        }
                    }
                }
                IrNodeData::VariableDeclarator {
                    binding,
                    initializer,
                } => {
                    if parent_variable_kind(program, node) == Some(VarKind::Const) {
                        metadata.const_declarators.push(node);
                    }
                    if let Some(initializer) = initializer
                        && is_anonymous_runtime_value(program, *initializer)
                        && let Some(symbol) = symbol_of_identifier(program, *binding)
                    {
                        metadata.runtime_carriers.insert(symbol);
                    }
                }
                IrNodeData::AssignmentExpression {
                    operator,
                    left,
                    right,
                } => {
                    if *operator == AssignmentOperator::Assign
                        && is_anonymous_runtime_value(program, *right)
                        && let Some(symbol) = symbol_of_identifier(program, *left)
                    {
                        metadata.runtime_carriers.insert(symbol);
                    }
                }
                IrNodeData::AssignmentPattern { left, right } => {
                    if is_anonymous_runtime_value(program, *right)
                        && let Some(symbol) = symbol_of_identifier(program, *left)
                    {
                        metadata.runtime_carriers.insert(symbol);
                    }
                }
                IrNodeData::MemberExpression {
                    object, property, ..
                } => {
                    if static_property_spelling(program, *property).as_deref() == Some("name")
                        && let Some(symbol) = symbol_of_identifier(program, *object)
                    {
                        metadata.runtime_observed_symbols.insert(symbol);
                    }
                }
                IrNodeData::CallExpression {
                    callee, optional, ..
                } => {
                    if !optional && is_direct_eval(program, *callee) {
                        metadata.direct_eval_calls.push(node);
                    }
                }
                IrNodeData::ExportNamedDeclaration {
                    declaration: Some(declaration),
                    ..
                } => {
                    metadata
                        .direct_export_symbols
                        .extend(declaration_symbols_under(program, *declaration));
                }
                IrNodeData::ExportDefaultDeclaration { value, .. } => {
                    if let IrNodeData::Function { name, .. } | IrNodeData::Class { name, .. } =
                        program.node(*value).expect("validated export value").data()
                        && let Some(symbol) =
                            name.and_then(|name| symbol_of_name_node(program, name))
                    {
                        metadata.direct_export_symbols.insert(symbol);
                    }
                }
                IrNodeData::Program { .. }
                | IrNodeData::VariableDeclaration { .. }
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
                | IrNodeData::NumberLiteral { .. }
                | IrNodeData::StringLiteral { .. }
                | IrNodeData::BooleanLiteral { .. }
                | IrNodeData::NullLiteral
                | IrNodeData::BigIntLiteral { .. }
                | IrNodeData::RegExpLiteral { .. }
                | IrNodeData::TemplateLiteral { .. }
                | IrNodeData::TemplateElement { .. }
                | IrNodeData::Identifier { .. }
                | IrNodeData::ThisExpression
                | IrNodeData::SuperExpression
                | IrNodeData::MetaProperty { .. }
                | IrNodeData::ArrayExpression { .. }
                | IrNodeData::Elision
                | IrNodeData::ObjectExpression { .. }
                | IrNodeData::ObjectProperty { .. }
                | IrNodeData::UnaryExpression { .. }
                | IrNodeData::UpdateExpression { .. }
                | IrNodeData::BinaryExpression { .. }
                | IrNodeData::LogicalExpression { .. }
                | IrNodeData::ConditionalExpression { .. }
                | IrNodeData::NewExpression { .. }
                | IrNodeData::SequenceExpression { .. }
                | IrNodeData::TaggedTemplateExpression { .. }
                | IrNodeData::SpreadElement { .. }
                | IrNodeData::AwaitExpression { .. }
                | IrNodeData::YieldExpression { .. }
                | IrNodeData::ImportExpression { .. }
                | IrNodeData::ArrowFunction { .. }
                | IrNodeData::MethodDefinition { .. }
                | IrNodeData::PropertyDefinition { .. }
                | IrNodeData::StaticBlock { .. }
                | IrNodeData::ArrayPattern { .. }
                | IrNodeData::ObjectPattern { .. }
                | IrNodeData::ObjectPatternProperty { .. }
                | IrNodeData::RestPattern { .. }
                | IrNodeData::ImportDeclaration { .. }
                | IrNodeData::ImportSpecifier { .. }
                | IrNodeData::ImportAttributes { .. }
                | IrNodeData::ImportAttribute { .. }
                | IrNodeData::ExportNamedDeclaration {
                    declaration: None, ..
                }
                | IrNodeData::ExportSpecifier { .. }
                | IrNodeData::ExportAllDeclaration { .. } => {}
            }
        }
        metadata
            .classes
            .sort_unstable_by_key(|node| metadata.rank[node.index()]);
        metadata
            .const_declarators
            .sort_unstable_by_key(|node| metadata.rank[node.index()]);
        Ok(metadata)
    }
}

fn symbol_of_name_node(program: &TypedProgram, node: NodeId) -> Option<SymbolId> {
    let IrNodeData::Name { name } = program.node(node)?.data() else {
        return None;
    };
    program.name(*name)?.symbol()
}

fn name_id_of_node(program: &TypedProgram, node: NodeId) -> Option<NameId> {
    let IrNodeData::Name { name } = program.node(node)?.data() else {
        return None;
    };
    Some(*name)
}

fn symbol_of_identifier(program: &TypedProgram, node: NodeId) -> Option<SymbolId> {
    let IrNodeData::Identifier { name } = program.node(node)?.data() else {
        return None;
    };
    symbol_of_name_node(program, *name)
}

fn name_id_of_identifier(program: &TypedProgram, node: NodeId) -> Option<NameId> {
    let IrNodeData::Identifier { name } = program.node(node)?.data() else {
        return None;
    };
    name_id_of_node(program, *name)
}

fn parent_node(program: &TypedProgram, node: NodeId) -> Option<NodeId> {
    program.node(node)?.parent().map(|parent| parent.parent())
}

fn parent_variable_kind(program: &TypedProgram, declarator: NodeId) -> Option<VarKind> {
    let parent = parent_node(program, declarator)?;
    let IrNodeData::VariableDeclaration { kind, .. } = program.node(parent)?.data() else {
        return None;
    };
    Some(*kind)
}

fn is_anonymous_runtime_value(program: &TypedProgram, node: NodeId) -> bool {
    match program.node(node).map(|node| node.data()) {
        Some(IrNodeData::Function { name: None, .. })
        | Some(IrNodeData::ArrowFunction { .. })
        | Some(IrNodeData::Class { name: None, .. }) => true,
        Some(
            IrNodeData::Program { .. }
            | IrNodeData::VariableDeclaration { .. }
            | IrNodeData::VariableDeclarator { .. }
            | IrNodeData::Function { name: Some(_), .. }
            | IrNodeData::FunctionBody { .. }
            | IrNodeData::Class { name: Some(_), .. }
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
            | IrNodeData::NumberLiteral { .. }
            | IrNodeData::StringLiteral { .. }
            | IrNodeData::BooleanLiteral { .. }
            | IrNodeData::NullLiteral
            | IrNodeData::BigIntLiteral { .. }
            | IrNodeData::RegExpLiteral { .. }
            | IrNodeData::TemplateLiteral { .. }
            | IrNodeData::TemplateElement { .. }
            | IrNodeData::Name { .. }
            | IrNodeData::Identifier { .. }
            | IrNodeData::ThisExpression
            | IrNodeData::SuperExpression
            | IrNodeData::MetaProperty { .. }
            | IrNodeData::ArrayExpression { .. }
            | IrNodeData::Elision
            | IrNodeData::ObjectExpression { .. }
            | IrNodeData::ObjectProperty { .. }
            | IrNodeData::UnaryExpression { .. }
            | IrNodeData::UpdateExpression { .. }
            | IrNodeData::BinaryExpression { .. }
            | IrNodeData::LogicalExpression { .. }
            | IrNodeData::AssignmentExpression { .. }
            | IrNodeData::ConditionalExpression { .. }
            | IrNodeData::CallExpression { .. }
            | IrNodeData::NewExpression { .. }
            | IrNodeData::MemberExpression { .. }
            | IrNodeData::SequenceExpression { .. }
            | IrNodeData::TaggedTemplateExpression { .. }
            | IrNodeData::SpreadElement { .. }
            | IrNodeData::AwaitExpression { .. }
            | IrNodeData::YieldExpression { .. }
            | IrNodeData::ImportExpression { .. }
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
            | IrNodeData::ExportAllDeclaration { .. },
        )
        | None => false,
    }
}

fn static_property_spelling(program: &TypedProgram, node: NodeId) -> Option<String> {
    match program.node(node)?.data() {
        IrNodeData::Name { name } => Some(program.name(*name)?.original().to_owned()),
        IrNodeData::StringLiteral { value } => Some(value.clone()),
        IrNodeData::Program { .. }
        | IrNodeData::VariableDeclaration { .. }
        | IrNodeData::VariableDeclarator { .. }
        | IrNodeData::Function { .. }
        | IrNodeData::FunctionBody { .. }
        | IrNodeData::Class { .. }
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
        | IrNodeData::NumberLiteral { .. }
        | IrNodeData::BooleanLiteral { .. }
        | IrNodeData::NullLiteral
        | IrNodeData::BigIntLiteral { .. }
        | IrNodeData::RegExpLiteral { .. }
        | IrNodeData::TemplateLiteral { .. }
        | IrNodeData::TemplateElement { .. }
        | IrNodeData::Identifier { .. }
        | IrNodeData::ThisExpression
        | IrNodeData::SuperExpression
        | IrNodeData::MetaProperty { .. }
        | IrNodeData::ArrayExpression { .. }
        | IrNodeData::Elision
        | IrNodeData::ObjectExpression { .. }
        | IrNodeData::ObjectProperty { .. }
        | IrNodeData::UnaryExpression { .. }
        | IrNodeData::UpdateExpression { .. }
        | IrNodeData::BinaryExpression { .. }
        | IrNodeData::LogicalExpression { .. }
        | IrNodeData::AssignmentExpression { .. }
        | IrNodeData::ConditionalExpression { .. }
        | IrNodeData::CallExpression { .. }
        | IrNodeData::NewExpression { .. }
        | IrNodeData::MemberExpression { .. }
        | IrNodeData::SequenceExpression { .. }
        | IrNodeData::TaggedTemplateExpression { .. }
        | IrNodeData::SpreadElement { .. }
        | IrNodeData::AwaitExpression { .. }
        | IrNodeData::YieldExpression { .. }
        | IrNodeData::ImportExpression { .. }
        | IrNodeData::ArrowFunction { .. }
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
        | IrNodeData::ExportAllDeclaration { .. } => None,
    }
}

fn is_direct_eval(program: &TypedProgram, callee: NodeId) -> bool {
    let Some(name) = name_id_of_identifier(program, callee) else {
        return false;
    };
    let name = program.name(name).expect("validated eval name");
    name.original() == "eval" && name.symbol().is_none() && name.role() == NameRole::Reference
}

fn declaration_symbols_under(program: &TypedProgram, root: NodeId) -> Vec<SymbolId> {
    let mut symbols = BTreeSet::new();
    match program.node(root).map(|node| node.data()) {
        Some(IrNodeData::VariableDeclaration { declarations, .. }) => {
            for declarator in list_items(program, *declarations) {
                if let Some(IrNodeData::VariableDeclarator { binding, .. }) =
                    program.node(declarator).map(|node| node.data())
                {
                    collect_binding_symbols(program, *binding, &mut symbols);
                }
            }
        }
        Some(IrNodeData::Function {
            name: Some(name), ..
        })
        | Some(IrNodeData::Class {
            name: Some(name), ..
        }) => {
            if let Some(symbol) = symbol_of_name_node(program, *name) {
                symbols.insert(symbol);
            }
        }
        Some(IrNodeData::Function { name: None, .. } | IrNodeData::Class { name: None, .. })
        | None => {}
        Some(_) => {}
    }
    symbols.into_iter().collect()
}

fn collect_binding_symbols(
    program: &TypedProgram,
    pattern: NodeId,
    symbols: &mut BTreeSet<SymbolId>,
) {
    match program.node(pattern).map(|node| node.data()) {
        Some(IrNodeData::Identifier { .. }) => {
            if let Some(symbol) = symbol_of_identifier(program, pattern) {
                symbols.insert(symbol);
            }
        }
        Some(IrNodeData::ArrayPattern { elements }) => {
            for element in list_items(program, *elements) {
                collect_binding_symbols(program, element, symbols);
            }
        }
        Some(IrNodeData::ObjectPattern { properties, rest }) => {
            for property in list_items(program, *properties) {
                collect_binding_symbols(program, property, symbols);
            }
            if let Some(rest) = rest {
                collect_binding_symbols(program, *rest, symbols);
            }
        }
        Some(IrNodeData::ObjectPatternProperty { value, .. }) => {
            collect_binding_symbols(program, *value, symbols);
        }
        Some(IrNodeData::AssignmentPattern { left, .. }) => {
            collect_binding_symbols(program, *left, symbols);
        }
        Some(IrNodeData::RestPattern { argument }) => {
            collect_binding_symbols(program, *argument, symbols);
        }
        Some(_) | None => {}
    }
}

fn plan_symbol_names(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    metadata: &Metadata,
    reserved: &[&str],
) -> (BTreeMap<SymbolId, String>, usize) {
    let reserved = reserved
        .iter()
        .chain(RUNTIME_NAMES.iter())
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    let mut frozen = BTreeSet::<SymbolId>::new();
    for symbol in 0..program.symbols().len() {
        let symbol = symbol as SymbolId;
        let Some(facts) = analysis.symbol(symbol) else {
            frozen.insert(symbol);
            continue;
        };
        let original = program
            .symbol(symbol)
            .expect("typed symbol metadata")
            .original_name();
        let runtime_observable = metadata.runtime_carriers.contains(&symbol)
            && runtime_carrier_is_observed(program, analysis, metadata, symbol);
        if facts.declarations().is_empty()
            || facts.declaration_scope().is_none()
            || facts.is_frozen()
            || metadata.direct_export_symbols.contains(&symbol)
            || metadata.named_expression_symbols.contains(&symbol)
            || runtime_observable
            || RUNTIME_NAMES.contains(&original)
            || (original.len() <= 1 && !reserved.contains(original))
        {
            frozen.insert(symbol);
        }
    }

    loop {
        let mut forbidden = metadata.unresolved_names.clone();
        forbidden.extend(reserved.iter().cloned());
        forbidden.extend(frozen.iter().filter_map(|symbol| {
            program
                .symbol(*symbol)
                .map(|metadata| metadata.original_name().to_owned())
        }));

        let mut candidates = (0..program.symbols().len())
            .map(|symbol| symbol as SymbolId)
            .filter(|symbol| !frozen.contains(symbol))
            .collect::<Vec<_>>();
        candidates.sort_unstable_by(|left, right| {
            let left_facts = analysis.symbol(*left).expect("candidate facts");
            let right_facts = analysis.symbol(*right).expect("candidate facts");
            left_facts
                .declaration_scope()
                .expect("candidate scope")
                .index()
                .cmp(
                    &right_facts
                        .declaration_scope()
                        .expect("candidate scope")
                        .index(),
                )
                .then_with(|| {
                    metadata.symbol_occurrences[*right as usize]
                        .len()
                        .cmp(&metadata.symbol_occurrences[*left as usize].len())
                })
                .then_with(|| left.cmp(right))
        });

        let mut planned = BTreeMap::<SymbolId, String>::new();
        let mut owners = BTreeMap::<String, Vec<TypedScopeId>>::new();
        let mut exclusive_names = BTreeSet::<String>::new();
        let mut all_planned_names = BTreeSet::<String>::new();
        let mut next_name_by_scope = vec![0usize; analysis.scopes().len()];
        let mut next_exclusive_name = 0usize;
        for symbol in candidates {
            let facts = analysis.symbol(symbol).expect("candidate facts");
            let scope = facts.declaration_scope().expect("candidate scope");
            let captured = facts.escape().captured();
            // Restarting the shortest-name search at `a` for every symbol is quadratic for large
            // generated modules with thousands of bindings in one scope. A name which was already
            // rejected or consumed in that same scope can never become available later in this
            // immutable plan, so each scope owns a monotonic cursor. Sibling scopes still start at
            // `a` and therefore retain deterministic lifetime-based slot reuse.
            let mut index = if captured {
                next_exclusive_name
            } else {
                next_name_by_scope[scope.index()]
            };
            let emitted = loop {
                let candidate = nth_name(index);
                index += 1;
                if is_reserved(&candidate) || forbidden.contains(&candidate) {
                    continue;
                }
                let compatible = if captured {
                    !all_planned_names.contains(&candidate)
                } else {
                    !exclusive_names.contains(&candidate)
                        && owners.get(&candidate).is_none_or(|existing| {
                            existing
                                .iter()
                                .all(|other| scopes_are_disjoint(analysis, scope, *other))
                        })
                };
                if compatible {
                    break candidate;
                }
            };
            if captured {
                next_exclusive_name = index;
                exclusive_names.insert(emitted.clone());
            } else {
                next_name_by_scope[scope.index()] = index;
            }
            all_planned_names.insert(emitted.clone());
            owners.entry(emitted.clone()).or_default().push(scope);
            planned.insert(symbol, emitted);
        }

        let rejected = planned
            .iter()
            .filter_map(|(&symbol, emitted)| {
                let original = program
                    .symbol(symbol)
                    .expect("planned symbol metadata")
                    .original_name();
                (original == emitted
                    || !symbol_rename_does_not_grow(program, analysis, metadata, symbol, emitted))
                .then_some(symbol)
            })
            .collect::<Vec<_>>();
        if rejected.is_empty() {
            planned.retain(|symbol, emitted| {
                program
                    .symbol(*symbol)
                    .is_some_and(|metadata| metadata.original_name() != emitted)
            });
            let reused_slots = planned
                .values()
                .fold(BTreeMap::<&str, usize>::new(), |mut counts, name| {
                    *counts.entry(name.as_str()).or_default() += 1;
                    counts
                })
                .values()
                .map(|count| count.saturating_sub(1))
                .sum();
            return (planned, reused_slots);
        }
        frozen.extend(rejected);
    }
}

fn runtime_carrier_is_observed(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    metadata: &Metadata,
    symbol: SymbolId,
) -> bool {
    if metadata.runtime_observed_symbols.contains(&symbol) {
        return true;
    }
    let Some(facts) = analysis.symbol(symbol) else {
        return true;
    };
    facts.reads().iter().any(|read| {
        let Some(name_use) = analysis.name_use(*read) else {
            return true;
        };
        let Some(identifier) = parent_node(program, name_use.node()) else {
            return true;
        };
        let Some(parent) = parent_node(program, identifier) else {
            return true;
        };
        !matches!(
            program.node(parent).map(|node| node.data()),
            Some(IrNodeData::CallExpression {
                callee,
                optional: false,
                ..
            }) if *callee == identifier
        )
    })
}

fn symbol_rename_does_not_grow(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    rename_metadata: &Metadata,
    symbol: SymbolId,
    emitted: &str,
) -> bool {
    let Some(metadata) = program.symbol(symbol) else {
        return false;
    };
    let Some(occurrences) = rename_metadata.symbol_occurrences.get(symbol as usize) else {
        return false;
    };
    if occurrences.is_empty() {
        return false;
    }
    let original = metadata.original_name();
    let old_cost = original.len().saturating_mul(occurrences.len());
    let mut new_cost = emitted.len().saturating_mul(occurrences.len());
    for &name in occurrences {
        let Some(name_use) = analysis.name_use(name) else {
            continue;
        };
        let Some(identifier) = parent_node(program, name_use.node()) else {
            continue;
        };
        let Some(parent) = parent_node(program, identifier) else {
            continue;
        };
        match program.node(parent).map(|node| node.data()) {
            Some(IrNodeData::ObjectProperty {
                value,
                shorthand: true,
                ..
            }) if *value == identifier => {
                new_cost = new_cost.saturating_add(original.len() + 1);
            }
            Some(IrNodeData::ImportSpecifier {
                imported: Some(imported),
                local,
                ..
            }) if *local == identifier
                && static_property_spelling(program, imported.value).as_deref()
                    == Some(original) =>
            {
                new_cost = new_cost.saturating_add(original.len() + 4);
            }
            Some(IrNodeData::ExportSpecifier { local, exported })
                if local.value == name_use.node()
                    && static_property_spelling(program, exported.value).as_deref()
                        == Some(original) =>
            {
                new_cost = new_cost.saturating_add(original.len() + 4);
            }
            Some(
                IrNodeData::Program { .. }
                | IrNodeData::VariableDeclaration { .. }
                | IrNodeData::VariableDeclarator { .. }
                | IrNodeData::Function { .. }
                | IrNodeData::FunctionBody { .. }
                | IrNodeData::Class { .. }
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
                | IrNodeData::NumberLiteral { .. }
                | IrNodeData::StringLiteral { .. }
                | IrNodeData::BooleanLiteral { .. }
                | IrNodeData::NullLiteral
                | IrNodeData::BigIntLiteral { .. }
                | IrNodeData::RegExpLiteral { .. }
                | IrNodeData::TemplateLiteral { .. }
                | IrNodeData::TemplateElement { .. }
                | IrNodeData::Name { .. }
                | IrNodeData::Identifier { .. }
                | IrNodeData::ThisExpression
                | IrNodeData::SuperExpression
                | IrNodeData::MetaProperty { .. }
                | IrNodeData::ArrayExpression { .. }
                | IrNodeData::Elision
                | IrNodeData::ObjectExpression { .. }
                | IrNodeData::ObjectProperty { .. }
                | IrNodeData::UnaryExpression { .. }
                | IrNodeData::UpdateExpression { .. }
                | IrNodeData::BinaryExpression { .. }
                | IrNodeData::LogicalExpression { .. }
                | IrNodeData::AssignmentExpression { .. }
                | IrNodeData::ConditionalExpression { .. }
                | IrNodeData::CallExpression { .. }
                | IrNodeData::NewExpression { .. }
                | IrNodeData::MemberExpression { .. }
                | IrNodeData::SequenceExpression { .. }
                | IrNodeData::TaggedTemplateExpression { .. }
                | IrNodeData::SpreadElement { .. }
                | IrNodeData::AwaitExpression { .. }
                | IrNodeData::YieldExpression { .. }
                | IrNodeData::ImportExpression { .. }
                | IrNodeData::ArrowFunction { .. }
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
                | IrNodeData::ExportAllDeclaration { .. },
            )
            | None => {}
        }
    }
    new_cost <= old_cost
}

fn scopes_are_disjoint(analysis: &TypedAnalysis, left: TypedScopeId, right: TypedScopeId) -> bool {
    left != right
        && !scope_is_ancestor(analysis, left, right)
        && !scope_is_ancestor(analysis, right, left)
}

fn scope_is_ancestor(
    analysis: &TypedAnalysis,
    ancestor: TypedScopeId,
    mut scope: TypedScopeId,
) -> bool {
    loop {
        if scope == ancestor {
            return true;
        }
        let Some(parent) = analysis.scope(scope).and_then(|facts| facts.parent()) else {
            return false;
        };
        scope = parent;
    }
}

#[cfg(test)]
fn all_symbol_occurrences(program: &TypedProgram, symbol: SymbolId) -> Vec<NameId> {
    program
        .nodes()
        .iter()
        .filter_map(|node| {
            let IrNodeData::Name { name } = node.data() else {
                return None;
            };
            program
                .name(*name)
                .is_some_and(|name| name.symbol() == Some(symbol))
                .then_some(*name)
        })
        .collect()
}

#[derive(Default)]
struct PrivateClassPlan {
    node: Option<NodeId>,
    declarations: BTreeMap<String, Vec<NameId>>,
    occurrences: BTreeMap<String, BTreeSet<NameId>>,
    invalid: bool,
}

fn plan_private_names(
    program: &TypedProgram,
    metadata: &Metadata,
    changes: &mut BTreeMap<NameId, String>,
) -> usize {
    let mut classes = metadata
        .classes
        .iter()
        .copied()
        .map(|node| PrivateClassPlan {
            node: Some(node),
            ..PrivateClassPlan::default()
        })
        .collect::<Vec<_>>();
    let class_indices = classes
        .iter()
        .enumerate()
        .filter_map(|(index, class)| class.node.map(|node| (node, index)))
        .collect::<BTreeMap<_, _>>();

    for class in &mut classes {
        let node = class.node.expect("private class node");
        let IrNodeData::Class { members, .. } = program.node(node).expect("validated class").data()
        else {
            unreachable!("metadata class list")
        };
        for member in list_items(program, *members) {
            match program.node(member).expect("validated class member").data() {
                IrNodeData::MethodDefinition {
                    key, decorators, ..
                }
                | IrNodeData::PropertyDefinition {
                    key, decorators, ..
                } => {
                    if key.kind == PropertyKeyKind::Private
                        && let Some(name_id) = name_id_of_node(program, key.value)
                    {
                        let spelling = program
                            .name(name_id)
                            .expect("private declaration name")
                            .original()
                            .to_owned();
                        class
                            .declarations
                            .entry(spelling)
                            .or_default()
                            .push(name_id);
                        if !list_items(program, *decorators).is_empty() {
                            class.invalid = true;
                        }
                    }
                }
                IrNodeData::StaticBlock { .. } => {}
                IrNodeData::Program { .. }
                | IrNodeData::VariableDeclaration { .. }
                | IrNodeData::VariableDeclarator { .. }
                | IrNodeData::Function { .. }
                | IrNodeData::FunctionBody { .. }
                | IrNodeData::Class { .. }
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
                | IrNodeData::NumberLiteral { .. }
                | IrNodeData::StringLiteral { .. }
                | IrNodeData::BooleanLiteral { .. }
                | IrNodeData::NullLiteral
                | IrNodeData::BigIntLiteral { .. }
                | IrNodeData::RegExpLiteral { .. }
                | IrNodeData::TemplateLiteral { .. }
                | IrNodeData::TemplateElement { .. }
                | IrNodeData::Name { .. }
                | IrNodeData::Identifier { .. }
                | IrNodeData::ThisExpression
                | IrNodeData::SuperExpression
                | IrNodeData::MetaProperty { .. }
                | IrNodeData::ArrayExpression { .. }
                | IrNodeData::Elision
                | IrNodeData::ObjectExpression { .. }
                | IrNodeData::ObjectProperty { .. }
                | IrNodeData::UnaryExpression { .. }
                | IrNodeData::UpdateExpression { .. }
                | IrNodeData::BinaryExpression { .. }
                | IrNodeData::LogicalExpression { .. }
                | IrNodeData::AssignmentExpression { .. }
                | IrNodeData::ConditionalExpression { .. }
                | IrNodeData::CallExpression { .. }
                | IrNodeData::NewExpression { .. }
                | IrNodeData::MemberExpression { .. }
                | IrNodeData::SequenceExpression { .. }
                | IrNodeData::TaggedTemplateExpression { .. }
                | IrNodeData::SpreadElement { .. }
                | IrNodeData::AwaitExpression { .. }
                | IrNodeData::YieldExpression { .. }
                | IrNodeData::ImportExpression { .. }
                | IrNodeData::ArrowFunction { .. }
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
                    unreachable!("validated class member list grammar")
                }
            }
        }
    }

    for &node in &metadata.preorder {
        let IrNodeData::Name { name } = program.node(node).expect("typed node").data() else {
            continue;
        };
        let name_record = program.name(*name).expect("typed name");
        if name_record.role() != NameRole::PrivateProperty {
            continue;
        }
        let spelling = name_record.original();
        if let Some(class_index) =
            nearest_declaring_class(program, node, spelling, &class_indices, &classes)
        {
            classes[class_index]
                .occurrences
                .entry(spelling.to_owned())
                .or_default()
                .insert(*name);
        }
    }
    for &call in &metadata.direct_eval_calls {
        let mut cursor = Some(call);
        while let Some(node) = cursor {
            if let Some(&class) = class_indices.get(&node) {
                classes[class].invalid = true;
            }
            cursor = parent_node(program, node);
        }
    }

    let mut renamed = 0usize;
    for class in classes.iter().filter(|class| !class.invalid) {
        let occupied = class.declarations.keys().cloned().collect::<BTreeSet<_>>();
        let mut generated = BTreeSet::new();
        let mut ordered = class.occurrences.iter().collect::<Vec<_>>();
        ordered.sort_unstable_by(|(left, left_occurrences), (right, right_occurrences)| {
            right_occurrences
                .len()
                .cmp(&left_occurrences.len())
                .then_with(|| left.cmp(right))
        });
        let mut next = 0usize;
        for (spelling, occurrences) in ordered {
            let short = loop {
                let short = nth_name(next);
                next += 1;
                if !is_reserved(&short)
                    && !occupied.contains(&short)
                    && generated.insert(short.clone())
                {
                    break short;
                }
            };
            if short.len() >= spelling.len() {
                continue;
            }
            for &name in occurrences {
                changes.insert(name, short.clone());
            }
            renamed += 1;
        }
    }
    renamed
}

fn nearest_declaring_class(
    program: &TypedProgram,
    mut node: NodeId,
    spelling: &str,
    indices: &BTreeMap<NodeId, usize>,
    classes: &[PrivateClassPlan],
) -> Option<usize> {
    loop {
        if let Some(&class) = indices.get(&node)
            && classes[class].declarations.contains_key(spelling)
        {
            return Some(class);
        }
        node = parent_node(program, node)?;
    }
}

fn list_items(program: &TypedProgram, list: ListId) -> Vec<NodeId> {
    program
        .list(list)
        .expect("validated typed list")
        .items()
        .to_vec()
}

fn property_is_reserved(spelling: &str) -> bool {
    spelling.starts_with("__") || RESERVED_PROPERTIES.contains(&spelling)
}

#[derive(Default)]
struct ClosedShape {
    keys: BTreeMap<String, BTreeSet<NameId>>,
    accesses: BTreeMap<String, BTreeSet<NameId>>,
    valid: bool,
}

#[derive(Clone, Copy)]
struct MemberAccess {
    member: NodeId,
    property: NameId,
}

fn plan_closed_properties(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    metadata: &Metadata,
    changes: &mut BTreeMap<NameId, String>,
) -> usize {
    let mut shapes = BTreeMap::<SymbolId, ClosedShape>::new();
    for &declarator in &metadata.const_declarators {
        let IrNodeData::VariableDeclarator {
            binding,
            initializer: Some(initializer),
        } = program.node(declarator).expect("const declarator").data()
        else {
            continue;
        };
        let Some(symbol) = symbol_of_identifier(program, *binding) else {
            continue;
        };
        let Some(keys) = closed_object_keys(program, *initializer) else {
            continue;
        };
        shapes.insert(
            symbol,
            ClosedShape {
                keys,
                accesses: BTreeMap::new(),
                valid: true,
            },
        );
    }
    if shapes.is_empty() {
        return 0;
    }

    let mut symbol_roots = shapes
        .keys()
        .map(|&symbol| (symbol, symbol))
        .collect::<BTreeMap<_, _>>();
    let mut alias_initializers = BTreeSet::<NameId>::new();
    loop {
        let mut changed = false;
        for &declarator in &metadata.const_declarators {
            let IrNodeData::VariableDeclarator {
                binding,
                initializer: Some(initializer),
            } = program.node(declarator).expect("const declarator").data()
            else {
                continue;
            };
            let (Some(alias), Some(source), Some(source_name)) = (
                symbol_of_identifier(program, *binding),
                symbol_of_identifier(program, *initializer),
                name_id_of_identifier(program, *initializer),
            ) else {
                continue;
            };
            let Some(&root) = symbol_roots.get(&source) else {
                continue;
            };
            if symbol_roots.insert(alias, root) != Some(root) {
                changed = true;
            }
            alias_initializers.insert(source_name);
        }
        if !changed {
            break;
        }
    }

    for (&symbol, &root) in &symbol_roots {
        let Some(shape) = shapes.get_mut(&root) else {
            continue;
        };
        let Some(symbol_metadata) = program.symbol(symbol) else {
            shape.valid = false;
            continue;
        };
        let Some(facts) = analysis.symbol(symbol) else {
            shape.valid = false;
            continue;
        };
        if symbol_metadata.decl_kind() != DeclKind::Const
            || facts.is_frozen()
            || !facts.writes().is_empty()
            || metadata.direct_export_symbols.contains(&symbol)
        {
            shape.valid = false;
            continue;
        }
        for &read in facts.reads() {
            if alias_initializers.contains(&read) {
                continue;
            }
            let Some(access) = static_member_access(program, analysis, read) else {
                shape.valid = false;
                break;
            };
            let spelling = program
                .name(access.property)
                .expect("static member property")
                .original()
                .to_owned();
            if !shape.keys.contains_key(&spelling) || member_is_deleted(program, access.member) {
                shape.valid = false;
                break;
            }
            shape
                .accesses
                .entry(spelling)
                .or_default()
                .insert(access.property);
        }
    }

    let mut renamed = 0usize;
    for shape in shapes.values().filter(|shape| shape.valid) {
        let occupied = shape.keys.keys().cloned().collect::<BTreeSet<_>>();
        let mut generated = BTreeSet::new();
        let mut ordered = shape
            .keys
            .iter()
            .map(|(spelling, declarations)| {
                let count =
                    declarations.len() + shape.accesses.get(spelling).map_or(0, BTreeSet::len);
                (spelling, count)
            })
            .collect::<Vec<_>>();
        ordered.sort_unstable_by(|(left, left_count), (right, right_count)| {
            right_count.cmp(left_count).then_with(|| left.cmp(right))
        });
        let mut next = 0usize;
        for (spelling, occurrences) in ordered {
            if occurrences == 0 || property_is_reserved(spelling) {
                continue;
            }
            let short = loop {
                let short = nth_name(next);
                next += 1;
                if !is_reserved(&short)
                    && !property_is_reserved(&short)
                    && !occupied.contains(&short)
                    && generated.insert(short.clone())
                {
                    break short;
                }
            };
            if short.len().saturating_mul(occurrences) >= spelling.len().saturating_mul(occurrences)
            {
                continue;
            }
            for &name in shape
                .keys
                .get(spelling)
                .into_iter()
                .flatten()
                .chain(shape.accesses.get(spelling).into_iter().flatten())
            {
                changes.insert(name, short.clone());
            }
            renamed += 1;
        }
    }
    renamed
}

fn closed_object_keys(
    program: &TypedProgram,
    object: NodeId,
) -> Option<BTreeMap<String, BTreeSet<NameId>>> {
    let IrNodeData::ObjectExpression { members } = program.node(object)?.data() else {
        return None;
    };
    let mut keys = BTreeMap::<String, BTreeSet<NameId>>::new();
    for member in list_items(program, *members) {
        match program.node(member)?.data() {
            IrNodeData::ObjectProperty {
                key,
                kind,
                method,
                shorthand,
                computed,
                prototype_setter,
                ..
            } => {
                if *kind != PropertyKind::Init
                    || *method
                    || *shorthand
                    || *computed
                    || *prototype_setter
                    || key.kind != PropertyKeyKind::Identifier
                {
                    return None;
                }
                let name = name_id_of_node(program, key.value)?;
                let spelling = program.name(name)?.original();
                if spelling == "__proto__" {
                    return None;
                }
                keys.entry(spelling.to_owned()).or_default().insert(name);
            }
            IrNodeData::SpreadElement { .. } => return None,
            IrNodeData::Program { .. }
            | IrNodeData::VariableDeclaration { .. }
            | IrNodeData::VariableDeclarator { .. }
            | IrNodeData::Function { .. }
            | IrNodeData::FunctionBody { .. }
            | IrNodeData::Class { .. }
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
            | IrNodeData::NumberLiteral { .. }
            | IrNodeData::StringLiteral { .. }
            | IrNodeData::BooleanLiteral { .. }
            | IrNodeData::NullLiteral
            | IrNodeData::BigIntLiteral { .. }
            | IrNodeData::RegExpLiteral { .. }
            | IrNodeData::TemplateLiteral { .. }
            | IrNodeData::TemplateElement { .. }
            | IrNodeData::Name { .. }
            | IrNodeData::Identifier { .. }
            | IrNodeData::ThisExpression
            | IrNodeData::SuperExpression
            | IrNodeData::MetaProperty { .. }
            | IrNodeData::ArrayExpression { .. }
            | IrNodeData::Elision
            | IrNodeData::ObjectExpression { .. }
            | IrNodeData::UnaryExpression { .. }
            | IrNodeData::UpdateExpression { .. }
            | IrNodeData::BinaryExpression { .. }
            | IrNodeData::LogicalExpression { .. }
            | IrNodeData::AssignmentExpression { .. }
            | IrNodeData::ConditionalExpression { .. }
            | IrNodeData::CallExpression { .. }
            | IrNodeData::NewExpression { .. }
            | IrNodeData::MemberExpression { .. }
            | IrNodeData::SequenceExpression { .. }
            | IrNodeData::TaggedTemplateExpression { .. }
            | IrNodeData::AwaitExpression { .. }
            | IrNodeData::YieldExpression { .. }
            | IrNodeData::ImportExpression { .. }
            | IrNodeData::ArrowFunction { .. }
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
                unreachable!("validated object-member list grammar")
            }
        }
    }
    (!keys.is_empty()).then_some(keys)
}

fn static_member_access(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    read: NameId,
) -> Option<MemberAccess> {
    let name_node = analysis.name_use(read)?.node();
    let identifier = parent_node(program, name_node)?;
    let member = parent_node(program, identifier)?;
    let IrNodeData::MemberExpression {
        object,
        property,
        property_kind,
        optional,
    } = program.node(member)?.data()
    else {
        return None;
    };
    if *object != identifier || *optional || *property_kind != PropertyKeyKind::Identifier {
        return None;
    }
    Some(MemberAccess {
        member,
        property: name_id_of_node(program, *property)?,
    })
}

fn member_is_deleted(program: &TypedProgram, member: NodeId) -> bool {
    let Some(parent) = parent_node(program, member) else {
        return false;
    };
    matches!(
        program.node(parent).map(|node| node.data()),
        Some(IrNodeData::UnaryExpression {
            operator: UnaryOperator::Delete,
            argument,
        }) if *argument == member
    )
}

#[cfg(test)]
mod tests {
    use wake_common::Interner;
    use wake_ecma_ast::SourceType;

    use super::*;

    fn lower(source: &str, source_type: SourceType) -> TypedProgram {
        let interner = Interner::new();
        let parsed = wake_ecma_parser::parse(source, &interner, source_type);
        assert!(
            !parsed.has_errors(),
            "fixture failed to parse: {:?}",
            parsed.diagnostics
        );
        parsed.module.with_ast(|program| {
            let semantic = wake_ecma_semantic::analyze(program);
            TypedProgram::lower(program, &interner, Some(&semantic)).unwrap()
        })
    }

    fn run(
        source: &str,
        source_type: SourceType,
        reserved: &[&str],
    ) -> (TypedProgram, TypedMangleStats) {
        let mut program = lower(source, source_type);
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        let stats = mangle_typed_program(&mut program, &analysis, reserved).unwrap();
        (program, stats)
    }

    fn symbol_named(program: &TypedProgram, spelling: &str) -> SymbolId {
        program
            .symbols()
            .iter()
            .enumerate()
            .find_map(|(symbol, metadata)| {
                (metadata.original_name() == spelling).then_some(symbol as SymbolId)
            })
            .unwrap_or_else(|| panic!("missing symbol {spelling}"))
    }

    fn emitted_symbol(program: &TypedProgram, spelling: &str) -> String {
        let symbol = symbol_named(program, spelling);
        let emitted = all_symbol_occurrences(program, symbol)
            .into_iter()
            .map(|name| program.name(name).unwrap().emitted().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(emitted.len(), 1, "all SymbolId occurrences must agree");
        emitted.into_iter().next().unwrap()
    }

    fn emitted_private(program: &TypedProgram, spelling: &str) -> BTreeSet<String> {
        program
            .names()
            .iter()
            .filter(|name| name.role() == NameRole::PrivateProperty && name.original() == spelling)
            .map(|name| name.emitted().to_owned())
            .collect()
    }

    fn emitted_property(program: &TypedProgram, spelling: &str) -> BTreeSet<String> {
        program
            .names()
            .iter()
            .filter(|name| name.role() == NameRole::Property && name.original() == spelling)
            .map(|name| name.emitted().to_owned())
            .collect()
    }

    #[test]
    fn renames_every_symbol_occurrence_and_respects_reserved_candidates() {
        let (program, stats) = run(
            "function calculate(longParameter){let longLocal=longParameter+1;return longLocal}calculate(1);",
            SourceType::Script,
            &["a"],
        );
        assert!(stats.renamed_symbols() >= 3);
        assert!(stats.changed_occurrences() >= 6);
        for spelling in ["calculate", "longParameter", "longLocal"] {
            let emitted = emitted_symbol(&program, spelling);
            assert_ne!(emitted, spelling);
            assert_ne!(emitted, "a");
            assert!(!is_reserved(&emitted));
        }

        let (program, _) = run("function wrapper(a){return a}", SourceType::Script, &["a"]);
        assert_ne!(
            emitted_symbol(&program, "a"),
            "a",
            "an existing one-byte binding must move away from a reserved wrapper slot"
        );
    }

    #[test]
    fn eval_and_with_freeze_only_visible_symbols() {
        let (program, _) = run(
            "function guarded(box){let evalVisible=1;eval('evalVisible');with(box){let withVisible=2;evalVisible+withVisible}}function sibling(longParameter){let longLocal=longParameter+1;return longLocal}",
            SourceType::Script,
            &[],
        );
        assert_eq!(emitted_symbol(&program, "evalVisible"), "evalVisible");
        assert_eq!(emitted_symbol(&program, "withVisible"), "withVisible");
        assert_ne!(emitted_symbol(&program, "longParameter"), "longParameter");
        assert_ne!(emitted_symbol(&program, "longLocal"), "longLocal");
    }

    #[test]
    fn import_export_public_names_and_direct_export_binding_are_preserved() {
        let (program, _) = run(
            "import {publicName as longLocal} from 'dep';export const publicBinding=longLocal;export{longLocal as publicAlias};",
            SourceType::Module,
            &[],
        );
        assert_eq!(emitted_symbol(&program, "publicBinding"), "publicBinding");
        assert_ne!(emitted_symbol(&program, "longLocal"), "longLocal");
        for public in ["publicName", "publicAlias"] {
            assert!(program.names().iter().any(|name| {
                name.original() == public && name.emitted() == public && name.symbol().is_none()
            }));
        }

        let (program, _) = run(
            "export function publicFunction(descriptiveParameter){return descriptiveParameter+1}",
            SourceType::Module,
            &[],
        );
        assert_eq!(emitted_symbol(&program, "publicFunction"), "publicFunction");
        assert_ne!(
            emitted_symbol(&program, "descriptiveParameter"),
            "descriptiveParameter",
            "only the exported declaration name is public; its local parameter may shrink"
        );
    }

    #[test]
    fn observable_and_escaped_runtime_names_stay_fixed() {
        let (program, _) = run(
            "function descriptive(){};consume(descriptive.name);const inferred=()=>1;consume(inferred.name);const escaped=()=>1;consume(escaped);const holder=function Inner(){return Inner};holder();",
            SourceType::Script,
            &[],
        );
        for spelling in ["descriptive", "inferred", "escaped", "Inner"] {
            assert_eq!(emitted_symbol(&program, spelling), spelling);
        }
        assert_ne!(emitted_symbol(&program, "holder"), "holder");
    }

    #[test]
    fn direct_only_function_name_can_shrink_when_its_result_escapes() {
        let (program, stats) = run(
            "function calculate(value){return value+1}globalThis.result=calculate(1);",
            SourceType::Script,
            &[],
        );
        assert_ne!(emitted_symbol(&program, "calculate"), "calculate");
        assert!(stats.renamed_symbols() >= 2);
    }

    #[test]
    fn fixed_descendant_name_cannot_capture_renamed_ancestor() {
        let (program, _) = run(
            "let longAncestor=1;{let a=2;consume(longAncestor,a)}",
            SourceType::Script,
            &[],
        );
        assert_eq!(emitted_symbol(&program, "a"), "a");
        assert_ne!(emitted_symbol(&program, "longAncestor"), "a");
        assert_ne!(emitted_symbol(&program, "longAncestor"), "longAncestor");
    }

    #[test]
    fn uncaptured_disjoint_scopes_reuse_slots_but_captures_do_not() {
        let (program, stats) = run(
            "function first(longParameter){let firstLocal=longParameter;return firstLocal}function second(otherParameter){let secondLocal=otherParameter;return secondLocal}",
            SourceType::Script,
            &[],
        );
        assert!(stats.reused_slots() >= 2);
        assert_eq!(
            emitted_symbol(&program, "longParameter"),
            emitted_symbol(&program, "otherParameter")
        );

        let (captured, _) = run(
            "function first(longCaptured){return()=>longCaptured}function second(otherParameter){return otherParameter}",
            SourceType::Script,
            &[],
        );
        assert_ne!(
            emitted_symbol(&captured, "longCaptured"),
            emitted_symbol(&captured, "otherParameter")
        );
    }

    #[test]
    fn private_names_are_consistent_and_dynamic_or_decorated_classes_stay_fixed() {
        let (program, stats) = run(
            "class Example{#longSecret=1;read(){return this.#longSecret}}new Example().read();",
            SourceType::Script,
            &[],
        );
        assert_eq!(stats.renamed_private_names(), 1);
        let emitted = emitted_private(&program, "longSecret");
        assert_eq!(emitted.len(), 1);
        assert_ne!(emitted.iter().next().unwrap(), "longSecret");

        let (private_host_spelling, private_stats) = run(
            "class Example{#value=1;read(){return this.#value}}",
            SourceType::Script,
            &[],
        );
        assert_eq!(private_stats.renamed_private_names(), 1);
        assert_ne!(
            emitted_private(&private_host_spelling, "value"),
            BTreeSet::from(["value".to_owned()]),
            "host-property reservations apply only to public names, never lexical #private names"
        );

        for source in [
            "class Example{#longSecret=1;read(){return eval('this.#longSecret')}}",
            "function dec(value,context){consume(context.name)}class Example{@dec #longSecret=1;read(){return this.#longSecret}}",
        ] {
            let (program, stats) = run(source, SourceType::Script, &[]);
            assert_eq!(stats.renamed_private_names(), 0);
            assert_eq!(
                emitted_private(&program, "longSecret"),
                BTreeSet::from(["longSecret".to_owned()])
            );
        }
    }

    #[test]
    fn closed_const_shape_and_alias_are_renamed_consistently() {
        let (program, stats) = run(
            "function read(){const object={longProperty:1,otherProperty:2};const alias=object;return alias.longProperty+object.otherProperty}read();",
            SourceType::Script,
            &[],
        );
        assert_eq!(stats.renamed_properties(), 2);
        for spelling in ["longProperty", "otherProperty"] {
            let emitted = emitted_property(&program, spelling);
            assert_eq!(emitted.len(), 1, "key and every access must agree");
            assert_ne!(emitted.iter().next().unwrap(), spelling);
        }
    }

    #[test]
    fn unsafe_or_public_object_shapes_never_rename() {
        let fixtures = [
            "function f(key){const object={longProperty:1};return object[key]}f('longProperty');",
            "function f(){const object={longProperty:1};return object}f();",
            "const object={longProperty:1};Object.keys(object);",
            "const object={longProperty:1};JSON.stringify(object);",
            "const object={longProperty:1};new Proxy(object,{});",
            "const object={longProperty:1};delete object.longProperty;",
            "const source={longProperty:1};const object={...source};object.longProperty;",
            "const object={longProperty:1};const {longProperty,...rest}=object;",
            "const object={longProperty:1};for(const key in object)consume(key);",
            "const object={longProperty(){return 1}};object.longProperty();",
            "const object={get longProperty(){return 1}};object.longProperty;",
            "export const object={longProperty:1};object.longProperty;",
        ];
        for source in fixtures {
            let source_type = if source.starts_with("export") {
                SourceType::Module
            } else {
                SourceType::Script
            };
            let (program, stats) = run(source, source_type, &[]);
            assert_eq!(
                stats.renamed_properties(),
                0,
                "unsafe shape was renamed for {source}"
            );
            assert!(emitted_property(&program, "longProperty").contains("longProperty"));
        }
    }

    #[test]
    fn protocol_host_and_public_class_properties_stay_fixed() {
        let (program, stats) = run(
            "const object={value:1,children:2,__proto__:null};consume(object.value,object.children);class Example{longPublic=1;read(){return this.longPublic}}",
            SourceType::Script,
            &[],
        );
        assert_eq!(stats.renamed_properties(), 0);
        for spelling in ["value", "children", "__proto__", "longPublic"] {
            assert!(emitted_property(&program, spelling).contains(spelling));
        }
    }

    #[test]
    fn output_is_deterministic_and_large_sources_do_not_disable_renaming() {
        let mut source = String::new();
        for index in 0..2_000 {
            source.push_str(&format!(
                "let veryLongBinding{index}={index};consume(veryLongBinding{index});"
            ));
        }
        assert!(source.len() > 4096);
        let (first, first_stats) = run(&source, SourceType::Script, &[]);
        let (second, second_stats) = run(&source, SourceType::Script, &[]);
        assert!(first_stats.changed());
        assert_eq!(first_stats, second_stats);
        assert_ne!(
            emitted_symbol(&first, "veryLongBinding1999"),
            "veryLongBinding1999"
        );
        assert_eq!(
            first
                .names()
                .iter()
                .map(|name| name.emitted())
                .collect::<Vec<_>>(),
            second
                .names()
                .iter()
                .map(|name| name.emitted())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn stale_analysis_is_rejected_before_any_name_write() {
        let mut program = lower(
            "let longBinding=1;consume(longBinding);",
            SourceType::Script,
        );
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        let name = program
            .names()
            .iter()
            .position(|name| name.original() == "longBinding")
            .and_then(|index| {
                program.nodes().iter().find_map(|node| match node.data() {
                    IrNodeData::Name { name } if name.index() == index => Some(*name),
                    _ => None,
                })
            })
            .unwrap();
        program.set_emitted_name(name, "changed").unwrap();
        let error = mangle_typed_program(&mut program, &analysis, &[]).unwrap_err();
        assert!(matches!(error, TypedMangleError::StaleAnalysis { .. }));
    }
}
