//! Owned, typed and structurally mutable JavaScript optimization IR.
//!
//! This arena does not retain a [`Program`] or encode rewrites as source-span overlays. Every
//! syntax occurrence owns the data required to
//! emit it again, while stable node/list/name IDs make structural passes deterministic.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Range;

use wake_common::{Interner, Span};
use wake_ecma_ast::{
    ArrowBody, AssignmentOperator, AttributesKeyword, BinaryOperator, Class, ClassMember,
    ExportDefaultKind, Expression, ForInit, ForLeft, Function, Ident, ImportAttributes,
    ImportSpecifier, LogicalOperator, MemberProperty, MethodKind, ModuleExportName, ObjectMember,
    Pattern, Program, PropertyKey, PropertyKind, SourceType, Statement, UnaryOperator,
    UpdateOperator, VarKind,
};
use wake_ecma_semantic::{DeclKind, SemanticModel, SymbolId};

/// Stable append-only node-arena index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    /// Zero-based arena index.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Stable append-only child-list arena index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ListId(u32);

impl ListId {
    /// Zero-based arena index.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Stable append-only name-occurrence index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NameId(u32);

impl NameId {
    /// Zero-based name-arena index.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Why a parser/lowering node is not directly source-backed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DerivedOriginKind {
    /// Parser lowering produced the node (for example JSX runtime calls).
    ParserLowering,
    /// A typed optimizer rewrite derived the node from existing syntax.
    Optimization,
}

/// Why a node with no source syntax was synthesized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntheticOriginKind {
    /// Trusted structured edit.
    TrustedEdit,
    /// Optimizer-created syntax.
    Optimization,
    /// Test or embedding API construction.
    External,
}

/// Source-map provenance for an IR node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrOrigin {
    /// Direct parser source occurrence.
    Source(Span),
    /// A lowering/rewrite derived from source syntax.
    Derived {
        /// Optional nearest source anchor.
        anchor: Option<Span>,
        /// Derivation category.
        kind: DerivedOriginKind,
    },
    /// Syntax with no direct source occurrence.
    Synthetic {
        /// Optional source-map anchor.
        anchor: Option<Span>,
        /// Synthesis category.
        kind: SyntheticOriginKind,
    },
}

impl IrOrigin {
    /// Nearest source anchor, if this node can be associated with source syntax.
    pub const fn anchor(self) -> Option<Span> {
        match self {
            Self::Source(span) => Some(span),
            Self::Derived { anchor, .. } | Self::Synthetic { anchor, .. } => anchor,
        }
    }

    fn from_parser_span(span: Span) -> Self {
        if span.is_dummy() {
            Self::Derived {
                anchor: None,
                kind: DerivedOriginKind::ParserLowering,
            }
        } else {
            Self::Source(span)
        }
    }

    fn parser_derived(anchor: Span) -> Self {
        Self::Derived {
            anchor: (!anchor.is_dummy()).then_some(anchor),
            kind: DerivedOriginKind::ParserLowering,
        }
    }
}

/// Concrete semantic position of a name occurrence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameRole {
    Binding,
    Reference,
    AssignmentTarget,
    FunctionName,
    ClassName,
    Property,
    PrivateProperty,
    LabelDeclaration,
    LabelReference,
    ImportName,
    ImportBinding,
    ModuleSpecifier,
    ExportLocal,
    ExportedName,
    AttributeKey,
    MetaKeyword,
    MetaProperty,
}

/// Grammar used by a name occurrence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameSyntax {
    Identifier,
    PrivateIdentifier,
    String,
    Keyword,
}

/// Owned spelling and semantic identity for one name occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrName {
    original: String,
    emitted: String,
    role: NameRole,
    syntax: NameSyntax,
    symbol: Option<SymbolId>,
}

/// Optimizer-owned metadata for one stable semantic binding identity.
///
/// IDs are indices in [`TypedProgram::symbols`]. Imported standalone owners receive a fresh ID
/// range, so an arrow parameter parsed in a define can never alias a module binding which happened
/// to use the same parser-local integer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrSymbol {
    original_name: String,
    emitted_name: String,
    decl_kind: DeclKind,
}

impl IrSymbol {
    pub fn original_name(&self) -> &str {
        &self.original_name
    }

    /// Final deterministic spelling shared by every occurrence of this semantic binding.
    pub fn emitted_name(&self) -> &str {
        &self.emitted_name
    }

    pub const fn decl_kind(&self) -> DeclKind {
        self.decl_kind
    }
}

impl IrName {
    pub fn original(&self) -> &str {
        &self.original
    }

    pub fn emitted(&self) -> &str {
        &self.emitted
    }

    pub const fn role(&self) -> NameRole {
        self.role
    }

    pub const fn syntax(&self) -> NameSyntax {
        self.syntax
    }

    pub const fn symbol(&self) -> Option<SymbolId> {
        self.symbol
    }
}

/// Parent field occupied by a child or child list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChildRole {
    ProgramBody,
    DeclarationItems,
    Binding,
    Initializer,
    Expression,
    IdentifierName,
    BlockBody,
    Test,
    Consequent,
    Alternate,
    ForInitializer,
    ForTest,
    ForUpdate,
    ForLeft,
    ForRight,
    LoopBody,
    SwitchDiscriminant,
    SwitchCases,
    SwitchCaseTest,
    SwitchCaseBody,
    ReturnArgument,
    ThrowArgument,
    TryBlock,
    CatchClause,
    FinallyBlock,
    CatchParameter,
    CatchBody,
    Label,
    LabeledBody,
    WithObject,
    WithBody,
    FunctionName,
    FunctionParameters,
    FunctionBody,
    FunctionStatements,
    ArrowBody,
    ClassName,
    ClassSuper,
    ClassMembers,
    Decorators,
    MethodKey,
    MethodValue,
    PropertyKey,
    PropertyValue,
    StaticBlockBody,
    UnaryArgument,
    UpdateArgument,
    Left,
    Right,
    Callee,
    Arguments,
    Object,
    MemberProperty,
    SequenceItems,
    Tag,
    Template,
    TemplateQuasis,
    TemplateExpressions,
    SpreadArgument,
    AwaitArgument,
    YieldArgument,
    ImportSource,
    ModuleSource,
    ImportOptions,
    ArrayElements,
    ObjectMembers,
    PatternElements,
    PatternProperties,
    PatternRest,
    PatternDefault,
    ImportSpecifiers,
    ImportImported,
    ImportLocal,
    ImportAttributes,
    AttributeItems,
    AttributeKey,
    AttributeValue,
    ExportDeclaration,
    ExportSpecifiers,
    ExportLocal,
    Exported,
    ExportDefaultValue,
    ExportAllName,
    MetaKeyword,
    MetaProperty,
}

/// Parent relation carried by every attached node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParentLink {
    parent: NodeId,
    role: ChildRole,
    list: Option<ListId>,
}

impl ParentLink {
    pub const fn parent(self) -> NodeId {
        self.parent
    }

    pub const fn role(self) -> ChildRole {
        self.role
    }

    pub const fn list(self) -> Option<ListId> {
        self.list
    }
}

/// Stable structural list owned by one parent field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrList {
    id: ListId,
    parent: NodeId,
    role: ChildRole,
    items: Vec<NodeId>,
}

impl IrList {
    pub const fn id(&self) -> ListId {
        self.id
    }

    pub const fn parent(&self) -> NodeId {
        self.parent
    }

    pub const fn role(&self) -> ChildRole {
        self.role
    }

    pub fn items(&self) -> &[NodeId] {
        &self.items
    }
}

/// Function syntax context needed for re-emission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionContext {
    Declaration,
    Expression,
    ExportDefault,
    Method,
}

/// Class syntax context needed for re-emission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassContext {
    Declaration,
    Expression,
    ExportDefault,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForInitializerKind {
    Variable,
    Expression,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForLeftKind {
    Variable,
    Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrowBodyKind {
    Block,
    Expression,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportSpecifierKind {
    Named,
    Default,
    Namespace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportDefaultValueKind {
    Function,
    Class,
    Expression,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyKeyKind {
    Identifier,
    String,
    Number,
    Computed,
    Private,
}

/// Owned property-key syntax plus its child occurrence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrPropertyKey {
    pub kind: PropertyKeyKind,
    pub value: NodeId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleNameKind {
    Identifier,
    String,
}

/// Owned module import/export name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrModuleName {
    pub kind: ModuleNameKind,
    pub value: NodeId,
}

/// Fully owned syntax payload. Every child is addressed structurally by [`NodeId`] or [`ListId`].
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum IrNodeData {
    Program {
        source_type: SourceType,
        strict: bool,
        spread_helper: Option<String>,
        object_spread_helper: Option<String>,
        for_of_helper: Option<String>,
        body: ListId,
    },
    VariableDeclaration {
        kind: VarKind,
        declarations: ListId,
    },
    VariableDeclarator {
        binding: NodeId,
        initializer: Option<NodeId>,
    },
    Function {
        context: FunctionContext,
        name: Option<NodeId>,
        parameters: ListId,
        body: Option<NodeId>,
        is_async: bool,
        is_generator: bool,
    },
    FunctionBody {
        statements: ListId,
        strict: bool,
    },
    Class {
        context: ClassContext,
        name: Option<NodeId>,
        super_class: Option<NodeId>,
        members: ListId,
        decorators: ListId,
    },
    Block {
        body: ListId,
    },
    EmptyStatement,
    DebuggerStatement,
    ExpressionStatement {
        expression: NodeId,
        /// True only for an unparenthesized string-literal directive-prologue occurrence.
        directive: bool,
    },
    IfStatement {
        test: NodeId,
        consequent: NodeId,
        alternate: Option<NodeId>,
    },
    ForStatement {
        initializer: Option<NodeId>,
        initializer_kind: Option<ForInitializerKind>,
        test: Option<NodeId>,
        update: Option<NodeId>,
        body: NodeId,
    },
    ForInStatement {
        left: NodeId,
        left_kind: ForLeftKind,
        right: NodeId,
        body: NodeId,
    },
    ForOfStatement {
        left: NodeId,
        left_kind: ForLeftKind,
        right: NodeId,
        body: NodeId,
        is_await: bool,
    },
    WhileStatement {
        test: NodeId,
        body: NodeId,
    },
    DoWhileStatement {
        body: NodeId,
        test: NodeId,
    },
    SwitchStatement {
        discriminant: NodeId,
        cases: ListId,
    },
    SwitchCase {
        test: Option<NodeId>,
        consequent: ListId,
    },
    ReturnStatement {
        argument: Option<NodeId>,
    },
    BreakStatement {
        label: Option<NodeId>,
    },
    ContinueStatement {
        label: Option<NodeId>,
    },
    ThrowStatement {
        argument: NodeId,
    },
    TryStatement {
        block: NodeId,
        handler: Option<NodeId>,
        finalizer: Option<NodeId>,
    },
    CatchClause {
        parameter: Option<NodeId>,
        body: NodeId,
    },
    LabeledStatement {
        label: NodeId,
        body: NodeId,
    },
    WithStatement {
        object: NodeId,
        body: NodeId,
    },
    NumberLiteral {
        value: f64,
    },
    StringLiteral {
        value: String,
    },
    BooleanLiteral {
        value: bool,
    },
    NullLiteral,
    BigIntLiteral {
        raw: String,
    },
    RegExpLiteral {
        pattern: String,
        flags: String,
    },
    TemplateLiteral {
        quasis: ListId,
        expressions: ListId,
    },
    TemplateElement {
        cooked: Option<String>,
        raw: String,
        tail: bool,
    },
    Name {
        name: NameId,
    },
    Identifier {
        name: NodeId,
    },
    ThisExpression,
    SuperExpression,
    MetaProperty {
        meta: NodeId,
        property: NodeId,
    },
    ArrayExpression {
        elements: ListId,
    },
    Elision,
    ObjectExpression {
        members: ListId,
    },
    ObjectProperty {
        key: IrPropertyKey,
        value: NodeId,
        kind: PropertyKind,
        method: bool,
        shorthand: bool,
        computed: bool,
        prototype_setter: bool,
    },
    UnaryExpression {
        operator: UnaryOperator,
        argument: NodeId,
    },
    UpdateExpression {
        operator: UpdateOperator,
        prefix: bool,
        argument: NodeId,
    },
    BinaryExpression {
        operator: BinaryOperator,
        left: NodeId,
        right: NodeId,
    },
    LogicalExpression {
        operator: LogicalOperator,
        left: NodeId,
        right: NodeId,
    },
    AssignmentExpression {
        operator: AssignmentOperator,
        left: NodeId,
        right: NodeId,
    },
    ConditionalExpression {
        test: NodeId,
        consequent: NodeId,
        alternate: NodeId,
    },
    CallExpression {
        callee: NodeId,
        arguments: ListId,
        optional: bool,
    },
    NewExpression {
        callee: NodeId,
        arguments: ListId,
    },
    MemberExpression {
        object: NodeId,
        property: NodeId,
        property_kind: PropertyKeyKind,
        optional: bool,
    },
    SequenceExpression {
        expressions: ListId,
    },
    TaggedTemplateExpression {
        tag: NodeId,
        quasi: NodeId,
    },
    SpreadElement {
        argument: NodeId,
    },
    AwaitExpression {
        argument: NodeId,
    },
    YieldExpression {
        argument: Option<NodeId>,
        delegate: bool,
    },
    ImportExpression {
        source: NodeId,
        options: Option<NodeId>,
    },
    ArrowFunction {
        parameters: ListId,
        body: NodeId,
        body_kind: ArrowBodyKind,
        is_async: bool,
    },
    MethodDefinition {
        key: IrPropertyKey,
        value: NodeId,
        kind: MethodKind,
        is_static: bool,
        computed: bool,
        decorators: ListId,
    },
    PropertyDefinition {
        key: IrPropertyKey,
        value: Option<NodeId>,
        is_static: bool,
        computed: bool,
        decorators: ListId,
        accessor: bool,
    },
    StaticBlock {
        body: ListId,
    },
    ArrayPattern {
        elements: ListId,
    },
    ObjectPattern {
        properties: ListId,
        rest: Option<NodeId>,
    },
    ObjectPatternProperty {
        key: IrPropertyKey,
        value: NodeId,
        shorthand: bool,
        computed: bool,
    },
    AssignmentPattern {
        left: NodeId,
        right: NodeId,
    },
    RestPattern {
        argument: NodeId,
    },
    ImportDeclaration {
        specifiers: ListId,
        source: NodeId,
        attributes: Option<NodeId>,
    },
    ImportSpecifier {
        kind: ImportSpecifierKind,
        imported: Option<IrModuleName>,
        local: NodeId,
    },
    ImportAttributes {
        keyword: AttributesKeyword,
        items: ListId,
    },
    ImportAttribute {
        key: IrModuleName,
        value: NodeId,
    },
    ExportNamedDeclaration {
        declaration: Option<NodeId>,
        specifiers: ListId,
        source: Option<NodeId>,
        attributes: Option<NodeId>,
    },
    ExportSpecifier {
        local: IrModuleName,
        exported: IrModuleName,
    },
    ExportDefaultDeclaration {
        value: NodeId,
        kind: ExportDefaultValueKind,
    },
    ExportAllDeclaration {
        exported: Option<IrModuleName>,
        source: NodeId,
        attributes: Option<NodeId>,
    },
}

/// One append-only node record.
#[derive(Clone, Debug, PartialEq)]
pub struct IrNode {
    id: NodeId,
    parent: Option<ParentLink>,
    origin: IrOrigin,
    data: IrNodeData,
    tombstone: bool,
}

impl IrNode {
    pub const fn id(&self) -> NodeId {
        self.id
    }

    pub const fn parent(&self) -> Option<ParentLink> {
        self.parent
    }

    pub const fn origin(&self) -> IrOrigin {
        self.origin
    }

    pub const fn data(&self) -> &IrNodeData {
        &self.data
    }

    pub const fn is_tombstone(&self) -> bool {
        self.tombstone
    }
}

/// Structural IR error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedIrError {
    pub node: Option<NodeId>,
    pub message: String,
}

impl fmt::Display for TypedIrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.node {
            Some(node) => write!(
                formatter,
                "typed IR node {}: {}",
                node.index(),
                self.message
            ),
            None => formatter.write_str(&self.message),
        }
    }
}

impl std::error::Error for TypedIrError {}

/// Lifetime-independent typed program arena.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedProgram {
    root: NodeId,
    nodes: Vec<IrNode>,
    lists: Vec<IrList>,
    names: Vec<IrName>,
    symbols: Vec<IrSymbol>,
    revision: u64,
}

/// Parser-to-IR work which an earlier, parser-semantic proof has shown to be unnecessary.
///
/// The plan deliberately addresses top-level statements by ordinal rather than by source span:
/// spans are provenance, not stable mutation identities, and transformed syntax can share them.
/// Lowering still verifies the requested statement shape before applying an entry, so a stale or
/// otherwise mismatched plan degrades to ordinary lowering instead of deleting unrelated syntax.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TypedLoweringPlan {
    elided_top_level_export_functions: Vec<usize>,
    elided_top_level_export_consts: Vec<usize>,
}

impl TypedLoweringPlan {
    fn insert_ordinal(ordinals: &mut Vec<usize>, ordinal: usize) {
        match ordinals.binary_search(&ordinal) {
            Ok(_) => {}
            Err(index) => ordinals.insert(index, ordinal),
        }
    }

    pub(crate) fn elide_top_level_export_function(&mut self, ordinal: usize) {
        Self::insert_ordinal(&mut self.elided_top_level_export_functions, ordinal);
    }

    pub(crate) fn elide_top_level_export_const(&mut self, ordinal: usize) {
        Self::insert_ordinal(&mut self.elided_top_level_export_consts, ordinal);
    }

    fn elides_top_level_export_function(&self, ordinal: usize) -> bool {
        self.elided_top_level_export_functions
            .binary_search(&ordinal)
            .is_ok()
    }

    fn elides_top_level_export_const(&self, ordinal: usize) -> bool {
        self.elided_top_level_export_consts
            .binary_search(&ordinal)
            .is_ok()
    }

    #[cfg(test)]
    pub(crate) fn elided_top_level_export_functions(&self) -> &[usize] {
        &self.elided_top_level_export_functions
    }

    #[cfg(test)]
    pub(crate) fn elided_top_level_export_consts(&self) -> &[usize] {
        &self.elided_top_level_export_consts
    }
}

/// Prove that evaluating and then discarding a direct export declaration is unobservable without
/// resolving a single binding. The initializer grammar is intentionally JSON-like: no property
/// lookup, coercion, iterator, accessor, computed key, spread, call, class evaluation or dynamic
/// scope can hide below it. Simple identifiers also avoid destructuring's iterator/getter work.
pub(crate) fn is_presemantic_inert_export_const(
    declaration: &wake_ecma_ast::VariableDeclaration<'_>,
) -> bool {
    declaration.kind == VarKind::Const
        && !declaration.span.is_dummy()
        && !declaration.declarations.is_empty()
        && declaration.declarations.iter().all(|declarator| {
            !declarator.span.is_dummy()
                && matches!(declarator.id, Pattern::Ident(identifier) if !identifier.span.is_dummy())
                && declarator
                    .init
                    .is_some_and(is_presemantic_inert_initializer)
        })
}

fn is_presemantic_inert_initializer(expression: Expression<'_>) -> bool {
    if expression.span().is_dummy() {
        return false;
    }
    match expression {
        Expression::NumberLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::RegExpLiteral(_) => true,
        Expression::TemplateLiteral(template) => {
            template.expressions.is_empty()
                && template.quasis.iter().all(|quasi| !quasi.span.is_dummy())
        }
        Expression::Array(array) => array.elements.iter().all(|element| {
            element.is_none_or(|element| {
                !matches!(element, Expression::Spread(_))
                    && is_presemantic_inert_initializer(element)
            })
        }),
        Expression::Object(object) => object.properties.iter().all(|member| {
            let ObjectMember::Property(property) = member else {
                return false;
            };
            if property.span.is_dummy()
                || property.kind != PropertyKind::Init
                || property.method
                || property.shorthand
                || property.computed
            {
                return false;
            }
            let inert_key = match property.key {
                PropertyKey::Ident(identifier) => !identifier.span.is_dummy(),
                PropertyKey::String(literal) => !literal.span.is_dummy(),
                PropertyKey::Number(literal) => !literal.span.is_dummy(),
                PropertyKey::Computed(_) | PropertyKey::Private(_) => false,
            };
            inert_key && is_presemantic_inert_initializer(property.value)
        }),
        // JSON permits negative numbers. Constrain unary evaluation to an actual numeric literal
        // so BigInt unary-plus throws and user-defined coercion can never enter this proof.
        Expression::Unary(unary) => {
            matches!(unary.operator, UnaryOperator::Minus | UnaryOperator::Plus)
                && matches!(unary.argument, Expression::NumberLiteral(_))
        }
        Expression::Identifier(_)
        | Expression::This(_)
        | Expression::Super(_)
        | Expression::MetaProperty(_)
        | Expression::Function(_)
        | Expression::Arrow(_)
        | Expression::Class(_)
        | Expression::Update(_)
        | Expression::Binary(_)
        | Expression::Logical(_)
        | Expression::Assignment(_)
        | Expression::Conditional(_)
        | Expression::Call(_)
        | Expression::New(_)
        | Expression::Member(_)
        | Expression::Sequence(_)
        | Expression::TaggedTemplate(_)
        | Expression::Spread(_)
        | Expression::Await(_)
        | Expression::Yield(_)
        | Expression::Import(_) => false,
    }
}

/// Transactional constructor for one detached compound node.
///
/// Child nodes must already be detached. Lists allocated through [`Self::list`] remain private to
/// the transaction and are committed only when the enclosing node payload references them with
/// the same role. This keeps transient half-owned lists out of the public IR API.
pub struct DetachedNodeBuilder<'program> {
    program: &'program mut TypedProgram,
    parent: NodeId,
    first_list: usize,
}

impl DetachedNodeBuilder<'_> {
    /// Allocate one list field for the node currently being built.
    pub fn list(
        &mut self,
        role: ChildRole,
        items: impl IntoIterator<Item = NodeId>,
    ) -> Result<ListId, TypedIrError> {
        debug_assert!(self.program.lists.len() >= self.first_list);
        let items = items.into_iter().collect::<Vec<_>>();
        let mut unique = HashSet::with_capacity(items.len());
        for &item in &items {
            self.program.ensure_detached_builder_child(item)?;
            if !unique.insert(item) {
                return Err(error(
                    Some(item),
                    "detached node list contains the same occurrence more than once",
                ));
            }
            let category = self.program.nodes[item.index()].data.category();
            if !role_accepts(role, category) {
                return Err(error(
                    Some(item),
                    format!("{role:?} list does not accept {category:?} syntax"),
                ));
            }
        }
        let id = push_index(self.program.lists.len(), "list")?;
        let id = ListId(id);
        self.program.lists.push(IrList {
            id,
            parent: self.parent,
            role,
            items,
        });
        Ok(id)
    }
}

/// Owned standalone expression tree, suitable for validated defines and structured edits.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedExpressionOwner {
    ir: TypedProgram,
}

#[derive(Clone, Copy)]
enum ImportedOriginPolicy {
    External,
    Reanchor {
        anchor: Span,
        kind: SyntheticOriginKind,
    },
}

impl ImportedOriginPolicy {
    const fn map(self, _foreign: IrOrigin) -> IrOrigin {
        match self {
            Self::External => IrOrigin::Synthetic {
                anchor: None,
                kind: SyntheticOriginKind::External,
            },
            Self::Reanchor { anchor, kind } => IrOrigin::Synthetic {
                anchor: Some(anchor),
                kind,
            },
        }
    }
}

impl TypedExpressionOwner {
    pub const fn root(&self) -> NodeId {
        self.ir.root
    }

    pub fn nodes(&self) -> &[IrNode] {
        &self.ir.nodes
    }

    pub fn lists(&self) -> &[IrList] {
        &self.ir.lists
    }

    pub fn names(&self) -> &[IrName] {
        &self.ir.names
    }

    pub fn symbols(&self) -> &[IrSymbol] {
        &self.ir.symbols
    }

    pub fn node(&self, id: NodeId) -> Option<&IrNode> {
        self.ir.node(id)
    }

    pub fn fingerprint(&self) -> u64 {
        self.ir.fingerprint()
    }

    pub fn validate(&self) -> Result<(), TypedIrError> {
        self.ir.validate_root(false)
    }
}

/// Lower one parser-validated expression into a lifetime-independent owner.
///
/// The caller retains responsibility for rejecting parser diagnostics before extracting the
/// expression. The returned owner contains no AST, arena or interner reference.
pub fn lower_expression_owner(
    expression: &Expression<'_>,
    interner: &Interner,
    semantic: Option<&SemanticModel>,
) -> Result<TypedExpressionOwner, TypedIrError> {
    let resolver = NameResolver::new(interner, semantic);
    let mut lowerer = Lowerer {
        ir: TypedProgram {
            root: NodeId(0),
            nodes: Vec::new(),
            lists: Vec::new(),
            names: Vec::new(),
            symbols: owned_symbols(interner, semantic),
            revision: 0,
        },
        interner,
        resolver,
    };
    let root = lowerer.expression(expression, NameRole::Reference);
    lowerer.ir.root = root;
    lowerer.ir.normalize_directive_prologues();
    lowerer.ir.validate_root(false)?;
    Ok(TypedExpressionOwner { ir: lowerer.ir })
}

impl TypedProgram {
    /// Analyze bindings and immediately lower one parser-owned program into the owned typed IR.
    ///
    /// This is the normal construction path for optimization. Neither the semantic model nor the
    /// parser AST is retained after lowering; callers also cannot accidentally request an
    /// optimizer input without stable [`SymbolId`] identities.
    pub fn lower_analyzed(
        program: &Program<'_>,
        interner: &Interner,
    ) -> Result<Self, TypedIrError> {
        let semantic = wake_ecma_semantic::analyze(program);
        Self::lower(program, interner, Some(&semantic))
    }

    /// Exhaustively lower one parser/lowering AST. No reference to `program` survives this call.
    pub fn lower(
        program: &Program<'_>,
        interner: &Interner,
        semantic: Option<&SemanticModel>,
    ) -> Result<Self, TypedIrError> {
        Self::lower_with_plan(program, interner, semantic, &TypedLoweringPlan::default())
    }

    /// Lower one program while honoring parser-semantic work-elision decisions owned by the
    /// optimizer adapter. This is crate-private because callers without the matching semantic
    /// proof must use [`Self::lower`].
    pub(crate) fn lower_with_plan(
        program: &Program<'_>,
        interner: &Interner,
        semantic: Option<&SemanticModel>,
        plan: &TypedLoweringPlan,
    ) -> Result<Self, TypedIrError> {
        let resolver = NameResolver::new(interner, semantic);
        let mut lowerer = Lowerer {
            ir: Self {
                root: NodeId(0),
                nodes: Vec::new(),
                lists: Vec::new(),
                names: Vec::new(),
                symbols: owned_symbols(interner, semantic),
                revision: 0,
            },
            interner,
            resolver,
        };
        let root = lowerer.lower_program(program, plan);
        lowerer.ir.root = root;
        lowerer.ir.normalize_directive_prologues();
        lowerer.ir.validate()?;
        Ok(lowerer.ir)
    }

    pub const fn root(&self) -> NodeId {
        self.root
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn nodes(&self) -> &[IrNode] {
        &self.nodes
    }

    pub fn lists(&self) -> &[IrList] {
        &self.lists
    }

    pub fn names(&self) -> &[IrName] {
        &self.names
    }

    pub fn symbols(&self) -> &[IrSymbol] {
        &self.symbols
    }

    pub fn node(&self, id: NodeId) -> Option<&IrNode> {
        self.nodes.get(id.index())
    }

    pub fn list(&self, id: ListId) -> Option<&IrList> {
        self.lists.get(id.index())
    }

    pub fn name(&self, id: NameId) -> Option<&IrName> {
        self.names.get(id.index())
    }

    pub fn symbol(&self, id: SymbolId) -> Option<&IrSymbol> {
        self.symbols.get(id as usize)
    }

    /// Allocate a stable optimizer-created binding identity.
    pub fn allocate_symbol(
        &mut self,
        original_name: impl Into<String>,
        decl_kind: DeclKind,
    ) -> Result<SymbolId, TypedIrError> {
        let id = u32::try_from(self.symbols.len())
            .map_err(|_| error(None, "typed IR symbol arena exceeded u32::MAX entries"))?;
        let original_name = original_name.into();
        self.symbols.push(IrSymbol {
            emitted_name: original_name.clone(),
            original_name,
            decl_kind,
        });
        self.revision = self.revision.wrapping_add(1);
        Ok(id)
    }

    /// Update the canonical emitted spelling shared by future occurrences of a binding.
    pub(crate) fn set_symbol_emitted_name(
        &mut self,
        id: SymbolId,
        emitted: impl Into<String>,
    ) -> Result<(), TypedIrError> {
        let Some(symbol) = self.symbols.get_mut(id as usize) else {
            return Err(error(None, format!("unknown symbol id {id}")));
        };
        symbol.emitted_name = emitted.into();
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Change only the emitted spelling of one occurrence; the original spelling remains stable.
    pub fn set_emitted_name(
        &mut self,
        id: NameId,
        emitted: impl Into<String>,
    ) -> Result<(), TypedIrError> {
        let Some(name) = self.names.get_mut(id.index()) else {
            return Err(error(None, format!("unknown name id {}", id.index())));
        };
        name.emitted = emitted.into();
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Change the semantic identity attached to one name occurrence.
    pub fn set_name_symbol(
        &mut self,
        id: NameId,
        symbol: Option<SymbolId>,
    ) -> Result<(), TypedIrError> {
        if let Some(symbol) = symbol
            && self.symbol(symbol).is_none()
        {
            return Err(error(None, format!("unknown symbol id {symbol}")));
        }
        let Some(name) = self.names.get_mut(id.index()) else {
            return Err(error(None, format!("unknown name id {}", id.index())));
        };
        name.symbol = symbol;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Reassign source-map provenance without changing syntax ownership.
    pub fn set_origin(&mut self, id: NodeId, origin: IrOrigin) -> Result<(), TypedIrError> {
        self.ensure_live(id)?;
        self.nodes[id.index()].origin = origin;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Replace scalar syntax flags/operators while preserving the exact structural edge set.
    /// Rewrites which add, remove, or move children must use the detached builder plus
    /// [`Self::replace_node`] instead.
    pub fn replace_node_data(
        &mut self,
        id: NodeId,
        replacement: IrNodeData,
    ) -> Result<(), TypedIrError> {
        self.ensure_live(id)?;
        let current = &self.nodes[id.index()].data;
        if current.category() != replacement.category()
            || !same_edges(&current.edges(), &replacement.edges())
        {
            return Err(error(
                Some(id),
                "scalar payload replacement must preserve node category and structural edges",
            ));
        }
        let old = std::mem::replace(&mut self.nodes[id.index()].data, replacement);
        if let Err(error) = self.validate_grammar_ancestors(id) {
            self.nodes[id.index()].data = old;
            return Err(error);
        }
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Append a detached leaf which a later replace/splice operation can attach.
    pub fn append_detached_leaf(
        &mut self,
        data: IrNodeData,
        origin: IrOrigin,
    ) -> Result<NodeId, TypedIrError> {
        if !data.edges().is_empty() {
            return Err(error(
                None,
                "detached append currently accepts only leaf payloads",
            ));
        }
        self.append_detached_node_with(origin, |_| Ok(data))
    }

    /// Append one detached name occurrence. Optimizer-created names are independent occurrences;
    /// changing their emitted spelling never mutates another identifier with the same text.
    pub fn append_detached_name(
        &mut self,
        original: impl Into<String>,
        role: NameRole,
        syntax: NameSyntax,
        symbol: Option<SymbolId>,
        origin: IrOrigin,
    ) -> Result<NodeId, TypedIrError> {
        if let Some(symbol) = symbol
            && self.symbol(symbol).is_none()
        {
            return Err(error(
                None,
                format!("unknown symbol id {symbol} for detached name"),
            ));
        }
        let original = original.into();
        let name = NameId(push_index(self.names.len(), "name")?);
        self.names.push(IrName {
            emitted: original.clone(),
            original,
            role,
            syntax,
            symbol,
        });
        match self.append_detached_leaf(IrNodeData::Name { name }, origin) {
            Ok(node) => Ok(node),
            Err(error) => {
                self.names.pop();
                Err(error)
            }
        }
    }

    /// Atomically append one detached compound node.
    ///
    /// The closure may allocate child lists through [`DetachedNodeBuilder::list`]. Every singular
    /// child and list item must already be a live detached subtree created by this program. On an
    /// error, all lists allocated by the closure are rolled back and no parent link is changed.
    pub fn append_detached_node_with(
        &mut self,
        origin: IrOrigin,
        build: impl FnOnce(&mut DetachedNodeBuilder<'_>) -> Result<IrNodeData, TypedIrError>,
    ) -> Result<NodeId, TypedIrError> {
        let parent = NodeId(push_index(self.nodes.len(), "node")?);
        let first_list = self.lists.len();
        let data = {
            let mut builder = DetachedNodeBuilder {
                program: self,
                parent,
                first_list,
            };
            match build(&mut builder) {
                Ok(data) => data,
                Err(error) => {
                    builder.program.lists.truncate(first_list);
                    return Err(error);
                }
            }
        };

        let validation = self.validate_detached_node_payload(parent, first_list, &data);
        if let Err(error) = validation {
            self.lists.truncate(first_list);
            return Err(error);
        }

        self.nodes.push(IrNode {
            id: parent,
            parent: None,
            origin,
            data,
            tombstone: false,
        });
        for edge in self.nodes[parent.index()].data.edges() {
            match edge {
                Edge::Child(role, child) => {
                    self.nodes[child.index()].parent = Some(ParentLink {
                        parent,
                        role,
                        list: None,
                    });
                }
                Edge::List(role, list) => {
                    for child in self.lists[list.index()].items.iter().copied() {
                        self.nodes[child.index()].parent = Some(ParentLink {
                            parent,
                            role,
                            list: Some(list),
                        });
                    }
                }
            }
        }
        if let Err(error) = self.validate_node_grammar(parent) {
            for child in self.child_ids_unchecked(parent) {
                self.nodes[child.index()].parent = None;
            }
            self.nodes.pop();
            self.lists.truncate(first_list);
            return Err(error);
        }
        self.revision = self.revision.wrapping_add(1);
        Ok(parent)
    }

    /// Deep-clone one live occurrence as a detached subtree. Node, list, and name identities are
    /// all fresh while syntax, emitted spellings, semantic symbols, and source origins are kept.
    /// This is the primitive used by branch promotion and proof-backed inlining.
    pub fn clone_detached_subtree(&mut self, source: NodeId) -> Result<NodeId, TypedIrError> {
        self.ensure_live(source)?;
        let node_len = self.nodes.len();
        let list_len = self.lists.len();
        let name_len = self.names.len();
        let revision = self.revision;
        match self.clone_detached_subtree_inner(source) {
            Ok(root) => Ok(root),
            Err(error) => {
                self.nodes.truncate(node_len);
                self.lists.truncate(list_len);
                self.names.truncate(name_len);
                self.revision = revision;
                Err(error)
            }
        }
    }

    /// Clone an owned expression tree into this arena as a detached subtree. All node/list/name
    /// identities are remapped; the owner may therefore be imported repeatedly without aliasing.
    pub fn import_expression_owner(
        &mut self,
        owner: &TypedExpressionOwner,
    ) -> Result<NodeId, TypedIrError> {
        self.import_expression_owner_with_policy(owner, ImportedOriginPolicy::External)
    }

    /// Import a parser-validated external expression and map all of its syntax to one trusted
    /// source occurrence in this program. Foreign 0-based parser spans are never copied into the
    /// host source-map domain.
    pub fn import_expression_owner_at(
        &mut self,
        owner: &TypedExpressionOwner,
        anchor: Span,
        kind: SyntheticOriginKind,
    ) -> Result<NodeId, TypedIrError> {
        if anchor.is_dummy() {
            return Err(error(
                None,
                "an imported expression source-map anchor must not be Span::DUMMY",
            ));
        }
        self.import_expression_owner_with_policy(
            owner,
            ImportedOriginPolicy::Reanchor { anchor, kind },
        )
    }

    fn import_expression_owner_with_policy(
        &mut self,
        owner: &TypedExpressionOwner,
        origin_policy: ImportedOriginPolicy,
    ) -> Result<NodeId, TypedIrError> {
        owner.validate()?;
        let node_offset = u32::try_from(self.nodes.len())
            .map_err(|_| error(None, "typed IR node arena exceeded u32::MAX entries"))?;
        let list_offset = u32::try_from(self.lists.len())
            .map_err(|_| error(None, "typed IR list arena exceeded u32::MAX entries"))?;
        let name_offset = u32::try_from(self.names.len())
            .map_err(|_| error(None, "typed IR name arena exceeded u32::MAX entries"))?;
        let symbol_offset = u32::try_from(self.symbols.len())
            .map_err(|_| error(None, "typed IR symbol arena exceeded u32::MAX entries"))?;
        checked_arena_growth(self.nodes.len(), owner.ir.nodes.len(), "node")?;
        checked_arena_growth(self.lists.len(), owner.ir.lists.len(), "list")?;
        checked_arena_growth(self.names.len(), owner.ir.names.len(), "name")?;
        checked_arena_growth(self.symbols.len(), owner.ir.symbols.len(), "symbol")?;

        self.symbols.extend(owner.ir.symbols.iter().cloned());
        self.names
            .extend(owner.ir.names.iter().cloned().map(|mut name| {
                name.symbol = name.symbol.map(|symbol| {
                    symbol
                        .checked_add(symbol_offset)
                        .expect("symbol arena growth was prevalidated")
                });
                name
            }));
        self.lists.extend(owner.ir.lists.iter().map(|list| {
            IrList {
                id: offset_list(list.id, list_offset),
                parent: offset_node(list.parent, node_offset),
                role: list.role,
                items: list
                    .items
                    .iter()
                    .map(|&node| offset_node(node, node_offset))
                    .collect(),
            }
        }));
        for node in &owner.ir.nodes {
            let mut data = node.data.clone();
            data.offset_ids(node_offset, list_offset, name_offset);
            self.nodes.push(IrNode {
                id: offset_node(node.id, node_offset),
                parent: node.parent.map(|parent| ParentLink {
                    parent: offset_node(parent.parent, node_offset),
                    role: parent.role,
                    list: parent.list.map(|list| offset_list(list, list_offset)),
                }),
                origin: origin_policy.map(node.origin),
                data,
                tombstone: node.tombstone,
            });
        }
        let root = offset_node(owner.root(), node_offset);
        debug_assert!(self.nodes[root.index()].parent.is_none());
        self.revision = self.revision.wrapping_add(1);
        self.validate()?;
        Ok(root)
    }

    /// Replace an attached node by a detached node ID and tombstone the removed subtree.
    pub fn replace_node(
        &mut self,
        target: NodeId,
        replacement: NodeId,
    ) -> Result<(), TypedIrError> {
        self.ensure_live(target)?;
        self.ensure_detached_live(replacement)?;
        if target == replacement {
            return Err(error(Some(target), "replacement must differ from target"));
        }
        let link = self.nodes[target.index()].parent;
        match link {
            None if target == self.root => {
                if self.nodes[replacement.index()].data.category() != NodeCategory::Program {
                    return Err(error(
                        Some(replacement),
                        "program root replacement must be Program syntax",
                    ));
                }
                self.root = replacement;
                self.nodes[replacement.index()].parent = None;
            }
            None => return Err(error(Some(target), "detached target is not the root")),
            Some(link) => {
                if !role_accepts(link.role, self.nodes[replacement.index()].data.category()) {
                    return Err(error(
                        Some(replacement),
                        format!(
                            "{:?} does not accept {:?} syntax",
                            link.role,
                            self.nodes[replacement.index()].data.category()
                        ),
                    ));
                }
                if let Some(list_id) = link.list {
                    let list = self
                        .lists
                        .get_mut(list_id.index())
                        .ok_or_else(|| error(Some(target), "parent list is missing"))?;
                    let Some(slot) = list.items.iter().position(|&item| item == target) else {
                        return Err(error(Some(target), "parent list does not contain target"));
                    };
                    list.items[slot] = replacement;
                } else {
                    let parent = &mut self.nodes[link.parent.index()];
                    if !parent.data.replace_singular(target, replacement) {
                        return Err(error(
                            Some(target),
                            "parent payload does not reference target",
                        ));
                    }
                }
                self.nodes[replacement.index()].parent = Some(link);
            }
        }
        self.nodes[target.index()].parent = None;
        self.mark_tombstone_subtree(target);
        self.revision = self.revision.wrapping_add(1);
        // The detached replacement was constructed through the typed builders (or cloned from a
        // validated subtree), and the role check above proves the changed edge. Re-validating the
        // complete arena for every local rewrite makes a pass quadratic on large modules. Validate
        // only the grammar path affected here; the scheduler still performs full-arena validation
        // at its transaction boundaries.
        self.validate_grammar_ancestors(replacement)
    }

    /// Replace a range of a stable child list with detached node IDs.
    pub fn splice_list(
        &mut self,
        list_id: ListId,
        range: Range<usize>,
        replacements: &[NodeId],
    ) -> Result<Vec<NodeId>, TypedIrError> {
        let Some(list) = self.lists.get(list_id.index()) else {
            return Err(error(None, format!("unknown list id {}", list_id.index())));
        };
        if range.start > range.end || range.end > list.items.len() {
            return Err(error(
                None,
                format!(
                    "invalid splice range {:?} for {} items",
                    range,
                    list.items.len()
                ),
            ));
        }
        let mut unique = HashSet::new();
        for &replacement in replacements {
            self.ensure_detached_live(replacement)?;
            if !unique.insert(replacement) {
                return Err(error(
                    Some(replacement),
                    "replacement list contains a duplicate node",
                ));
            }
            if !role_accepts(list.role, self.nodes[replacement.index()].data.category()) {
                return Err(error(
                    Some(replacement),
                    format!(
                        "{:?} list does not accept {:?} syntax",
                        list.role,
                        self.nodes[replacement.index()].data.category()
                    ),
                ));
            }
        }
        let parent = list.parent;
        let role = list.role;
        let removed = self.lists[list_id.index()].items[range.clone()].to_vec();
        for &node in &removed {
            self.nodes[node.index()].parent = None;
        }
        self.lists[list_id.index()]
            .items
            .splice(range, replacements.iter().copied());
        for &node in replacements {
            self.nodes[node.index()].parent = Some(ParentLink {
                parent,
                role,
                list: Some(list_id),
            });
        }
        for &node in &removed {
            self.mark_tombstone_subtree(node);
        }
        self.revision = self.revision.wrapping_add(1);
        // Parent/list ownership is established explicitly above. Only list-sensitive grammar at
        // the parent (and its ancestors) can change; whole-arena validation remains a pipeline
        // boundary invariant instead of an O(n) charge for every splice.
        self.validate_grammar_ancestors(parent)?;
        Ok(removed)
    }

    /// Tombstone a detached subtree. Attached syntax must first be removed with `splice_list` or
    /// replaced with `replace_node`, preventing mandatory child fields from becoming invalid.
    pub fn tombstone_subtree(&mut self, node: NodeId) -> Result<(), TypedIrError> {
        self.ensure_live(node)?;
        if self.nodes[node.index()].parent.is_some() || node == self.root {
            return Err(error(
                Some(node),
                "attached syntax cannot be tombstoned without a structural edit",
            ));
        }
        self.mark_tombstone_subtree(node);
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Deterministic root-first traversal of the live program.
    pub fn preorder(&self) -> Result<Vec<NodeId>, TypedIrError> {
        self.validate()?;
        self.preorder_validated()
    }

    /// Deterministic root-first traversal for an already-validated program revision.
    ///
    /// The typed scheduler validates at ownership boundaries and every mutator preserves local
    /// parent/list/grammar invariants. Internal passes use this entry so one logical traversal does
    /// not silently prepend another whole-arena validation sweep.
    pub(crate) fn preorder_validated(&self) -> Result<Vec<NodeId>, TypedIrError> {
        self.subtree_preorder(self.root)
    }

    /// Root-first traversal of one already-validated live subtree.
    ///
    /// Analysis rebuild validates the complete arena once at its boundary, then uses this helper
    /// for local escape and CFG queries. Re-scanning every name in the module for each expression
    /// made those queries quadratic on generated modules.
    pub(crate) fn subtree_preorder(&self, root: NodeId) -> Result<Vec<NodeId>, TypedIrError> {
        self.ensure_live(root)?;
        let mut output = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            output.push(node);
            let mut children = self.child_ids(node)?;
            children.reverse();
            stack.extend(children);
        }
        Ok(output)
    }

    /// Pointer-free deterministic structural fingerprint.
    pub fn fingerprint(&self) -> u64 {
        let mut hash = Fnv64::new();
        hash.write(format!("root={:?};revision={};", self.root, self.revision).as_bytes());
        for name in &self.names {
            hash.write(format!("name={name:?};").as_bytes());
        }
        for symbol in &self.symbols {
            hash.write(format!("symbol={symbol:?};").as_bytes());
        }
        for list in &self.lists {
            hash.write(format!("list={list:?};").as_bytes());
        }
        for node in &self.nodes {
            hash.write(format!("node={node:?};").as_bytes());
        }
        hash.finish()
    }

    /// Check ownership, parent-role, list and reachability invariants.
    pub fn validate(&self) -> Result<(), TypedIrError> {
        self.validate_root(true)
    }

    fn validate_root(&self, require_program: bool) -> Result<(), TypedIrError> {
        let Some(root) = self.nodes.get(self.root.index()) else {
            return Err(error(None, "root node is missing"));
        };
        if root.tombstone || root.parent.is_some() {
            return Err(error(Some(self.root), "root must be live and detached"));
        }
        if require_program && !matches!(root.data, IrNodeData::Program { .. }) {
            return Err(error(
                Some(self.root),
                "program root must contain Program syntax",
            ));
        }
        for (index, node) in self.nodes.iter().enumerate() {
            if node.id.index() != index {
                return Err(error(
                    Some(node.id),
                    "node ID does not match arena position",
                ));
            }
            if let IrNodeData::Name { name } = node.data
                && self.names.get(name.index()).is_none()
            {
                return Err(error(
                    Some(node.id),
                    "name node references an unknown NameId",
                ));
            }
            if let IrNodeData::Name { name } = node.data
                && let Some(symbol) = self.names[name.index()].symbol
                && self.symbols.get(symbol as usize).is_none()
            {
                return Err(error(
                    Some(node.id),
                    format!("name occurrence references unknown symbol id {symbol}"),
                ));
            }
            for edge in node.data.edges() {
                match edge {
                    Edge::Child(role, child) => {
                        let Some(record) = self.nodes.get(child.index()) else {
                            return Err(error(
                                Some(node.id),
                                format!("child {} is missing", child.index()),
                            ));
                        };
                        if !role_accepts(role, record.data.category()) {
                            return Err(error(
                                Some(child),
                                format!(
                                    "{role:?} does not accept {:?} syntax",
                                    record.data.category()
                                ),
                            ));
                        }
                        let expected = ParentLink {
                            parent: node.id,
                            role,
                            list: None,
                        };
                        if record.parent != Some(expected) {
                            return Err(error(
                                Some(child),
                                format!("singular parent link mismatch for {role:?}"),
                            ));
                        }
                        if !node.tombstone && record.tombstone {
                            return Err(error(
                                Some(child),
                                "live node references a tombstone child",
                            ));
                        }
                    }
                    Edge::List(role, list_id) => {
                        let Some(list) = self.lists.get(list_id.index()) else {
                            return Err(error(
                                Some(node.id),
                                format!("child list {} is missing", list_id.index()),
                            ));
                        };
                        if list.id != list_id || list.parent != node.id || list.role != role {
                            return Err(error(
                                Some(node.id),
                                format!("list ownership mismatch for {role:?}"),
                            ));
                        }
                        for &child in &list.items {
                            let Some(record) = self.nodes.get(child.index()) else {
                                return Err(error(
                                    Some(node.id),
                                    format!("list child {} is missing", child.index()),
                                ));
                            };
                            if !role_accepts(role, record.data.category()) {
                                return Err(error(
                                    Some(child),
                                    format!(
                                        "{role:?} list does not accept {:?} syntax",
                                        record.data.category()
                                    ),
                                ));
                            }
                            let expected = ParentLink {
                                parent: node.id,
                                role,
                                list: Some(list_id),
                            };
                            if record.parent != Some(expected) {
                                return Err(error(
                                    Some(child),
                                    format!("list parent link mismatch for {role:?}"),
                                ));
                            }
                            if !node.tombstone && record.tombstone {
                                return Err(error(
                                    Some(child),
                                    "live list contains a tombstone child",
                                ));
                            }
                        }
                    }
                }
            }
            if !node.tombstone {
                self.validate_node_grammar(node.id)?;
            }
        }
        for (index, list) in self.lists.iter().enumerate() {
            if list.id.index() != index {
                return Err(error(
                    None,
                    format!("list ID {} does not match arena position", list.id.index()),
                ));
            }
        }
        if let IrNodeData::TemplateLiteral {
            quasis,
            expressions,
        } = root.data
        {
            // A root template cannot occur, but keep the generic invariant below for diagnostics.
            self.validate_template_lengths(root.id, quasis, expressions)?;
        }
        for node in &self.nodes {
            if let IrNodeData::TemplateLiteral {
                quasis,
                expressions,
            } = node.data
            {
                self.validate_template_lengths(node.id, quasis, expressions)?;
            }
        }
        let mut reachable = HashSet::new();
        let mut stack = self
            .nodes
            .iter()
            .filter(|node| !node.tombstone && node.parent.is_none())
            .map(IrNode::id)
            .collect::<Vec<_>>();
        while let Some(node) = stack.pop() {
            if !reachable.insert(node) {
                return Err(error(
                    Some(node),
                    "live structure contains a cycle or duplicate child",
                ));
            }
            stack.extend(self.child_ids_unchecked(node));
        }
        for node in &self.nodes {
            if !node.tombstone && !reachable.contains(&node.id) {
                return Err(error(
                    Some(node.id),
                    "live node is unreachable from every detached root",
                ));
            }
        }
        Ok(())
    }

    fn validate_template_lengths(
        &self,
        node: NodeId,
        quasis: ListId,
        expressions: ListId,
    ) -> Result<(), TypedIrError> {
        let q = self
            .lists
            .get(quasis.index())
            .map_or(0, |list| list.items.len());
        let e = self
            .lists
            .get(expressions.index())
            .map_or(0, |list| list.items.len());
        if q != e + 1 {
            return Err(error(
                Some(node),
                format!("template must have expressions + 1 quasis, found {q} and {e}"),
            ));
        }
        Ok(())
    }

    fn child_ids(&self, node: NodeId) -> Result<Vec<NodeId>, TypedIrError> {
        self.nodes
            .get(node.index())
            .ok_or_else(|| error(Some(node), "unknown node"))?;
        Ok(self.child_ids_unchecked(node))
    }

    fn child_ids_unchecked(&self, node: NodeId) -> Vec<NodeId> {
        let mut children = Vec::new();
        for edge in self.nodes[node.index()].data.edges() {
            match edge {
                Edge::Child(_, child) => children.push(child),
                Edge::List(_, list) => {
                    children.extend(self.lists[list.index()].items.iter().copied())
                }
            }
        }
        children
    }

    fn validate_node_grammar(&self, node: NodeId) -> Result<(), TypedIrError> {
        let invalid = |message: &str| error(Some(node), message);
        match self.nodes[node.index()].data() {
            IrNodeData::Program { body, .. } => self.validate_directive_list(*body, node)?,
            IrNodeData::VariableDeclaration { declarations, .. } => {
                if self.lists[declarations.index()].items.is_empty() {
                    return Err(invalid("variable declaration must contain a declarator"));
                }
            }
            IrNodeData::VariableDeclarator { .. } => {}
            IrNodeData::Function {
                context,
                name,
                parameters,
                ..
            } => {
                if *context == FunctionContext::Declaration && name.is_none() {
                    return Err(invalid("function declaration must have a name"));
                }
                if *context == FunctionContext::Method && name.is_some() {
                    return Err(invalid(
                        "method function payload cannot carry a function name",
                    ));
                }
                if let Some(parent) = self.nodes[node.index()].parent {
                    let expected = match context {
                        FunctionContext::Declaration | FunctionContext::Expression => None,
                        FunctionContext::ExportDefault => Some(ChildRole::ExportDefaultValue),
                        FunctionContext::Method => Some(ChildRole::MethodValue),
                    };
                    if expected.is_some_and(|role| parent.role != role) {
                        return Err(invalid(
                            "function context does not match its structural role",
                        ));
                    }
                }
                self.validate_rest_is_last(*parameters, node)?;
            }
            IrNodeData::FunctionBody { statements, .. } => {
                self.validate_directive_list(*statements, node)?;
            }
            IrNodeData::Class { context, name, .. } => {
                if *context == ClassContext::Declaration && name.is_none() {
                    return Err(invalid("class declaration must have a name"));
                }
                if let Some(parent) = self.nodes[node.index()].parent
                    && *context == ClassContext::ExportDefault
                    && parent.role != ChildRole::ExportDefaultValue
                {
                    return Err(invalid("class context does not match its structural role"));
                }
            }
            IrNodeData::Block { .. }
            | IrNodeData::EmptyStatement
            | IrNodeData::DebuggerStatement
            | IrNodeData::WhileStatement { .. }
            | IrNodeData::DoWhileStatement { .. }
            | IrNodeData::ReturnStatement { .. }
            | IrNodeData::BreakStatement { .. }
            | IrNodeData::ContinueStatement { .. }
            | IrNodeData::ThrowStatement { .. }
            | IrNodeData::LabeledStatement { .. }
            | IrNodeData::WithStatement { .. }
            | IrNodeData::NumberLiteral { .. }
            | IrNodeData::StringLiteral { .. }
            | IrNodeData::BooleanLiteral { .. }
            | IrNodeData::NullLiteral
            | IrNodeData::BigIntLiteral { .. }
            | IrNodeData::RegExpLiteral { .. }
            | IrNodeData::ThisExpression
            | IrNodeData::SuperExpression
            | IrNodeData::UnaryExpression { .. }
            | IrNodeData::UpdateExpression { .. }
            | IrNodeData::BinaryExpression { .. }
            | IrNodeData::LogicalExpression { .. }
            | IrNodeData::AssignmentExpression { .. }
            | IrNodeData::CallExpression { .. }
            | IrNodeData::NewExpression { .. }
            | IrNodeData::TaggedTemplateExpression { .. }
            | IrNodeData::SpreadElement { .. }
            | IrNodeData::AwaitExpression { .. }
            | IrNodeData::YieldExpression { .. }
            | IrNodeData::ImportExpression { .. }
            | IrNodeData::StaticBlock { .. }
            | IrNodeData::AssignmentPattern { .. }
            | IrNodeData::RestPattern { .. }
            | IrNodeData::ImportAttributes { .. }
            | IrNodeData::ImportAttribute { .. }
            | IrNodeData::ExportSpecifier { .. } => {}
            IrNodeData::ExpressionStatement {
                expression,
                directive,
            } => {
                if *directive
                    && (!matches!(
                        self.nodes[expression.index()].data,
                        IrNodeData::StringLiteral { .. }
                    ) || self.nodes[node.index()].parent.is_none_or(|parent| {
                        !matches!(
                            parent.role,
                            ChildRole::ProgramBody | ChildRole::FunctionStatements
                        )
                    }))
                {
                    return Err(invalid(
                        "directive flag requires a string literal in a directive-prologue list",
                    ));
                }
            }
            IrNodeData::IfStatement {
                consequent,
                alternate,
                ..
            } => {
                if self.nodes[consequent.index()].data.category() != NodeCategory::Statement
                    || alternate.is_some_and(|alternate| {
                        self.nodes[alternate.index()].data.category() != NodeCategory::Statement
                    })
                {
                    return Err(invalid("if branches must be statement syntax"));
                }
            }
            IrNodeData::ForStatement {
                initializer,
                initializer_kind,
                ..
            } => match (initializer, initializer_kind) {
                (None, None) => {}
                (Some(initializer), Some(ForInitializerKind::Variable))
                    if matches!(
                        self.nodes[initializer.index()].data,
                        IrNodeData::VariableDeclaration { .. }
                    ) => {}
                (Some(initializer), Some(ForInitializerKind::Expression))
                    if matches!(
                        self.nodes[initializer.index()].data.category(),
                        NodeCategory::Expression | NodeCategory::Identifier
                    ) => {}
                _ => {
                    return Err(invalid(
                        "for initializer kind must match the optional initializer syntax",
                    ));
                }
            },
            IrNodeData::ForInStatement {
                left, left_kind, ..
            }
            | IrNodeData::ForOfStatement {
                left, left_kind, ..
            } => match left_kind {
                ForLeftKind::Variable
                    if matches!(
                        self.nodes[left.index()].data,
                        IrNodeData::VariableDeclaration { .. }
                    ) => {}
                ForLeftKind::Target
                    if matches!(
                        self.nodes[left.index()].data.category(),
                        NodeCategory::Expression | NodeCategory::Identifier
                    ) => {}
                _ => return Err(invalid("for-left kind does not match its child syntax")),
            },
            IrNodeData::SwitchStatement { cases, .. } => {
                let defaults = self.lists[cases.index()]
                    .items
                    .iter()
                    .filter(|case| {
                        matches!(
                            self.nodes[case.index()].data,
                            IrNodeData::SwitchCase { test: None, .. }
                        )
                    })
                    .count();
                if defaults > 1 {
                    return Err(invalid(
                        "switch statement cannot contain multiple default cases",
                    ));
                }
            }
            IrNodeData::SwitchCase { .. } => {}
            IrNodeData::TryStatement {
                block,
                handler,
                finalizer,
            } => {
                if handler.is_none() && finalizer.is_none() {
                    return Err(invalid("try statement requires a catch or finally clause"));
                }
                if !matches!(self.nodes[block.index()].data, IrNodeData::Block { .. })
                    || finalizer.is_some_and(|finalizer| {
                        !matches!(self.nodes[finalizer.index()].data, IrNodeData::Block { .. })
                    })
                {
                    return Err(invalid("try and finally children must be block statements"));
                }
            }
            IrNodeData::CatchClause { body, .. } => {
                if !matches!(self.nodes[body.index()].data, IrNodeData::Block { .. }) {
                    return Err(invalid("catch body must be a block statement"));
                }
            }
            IrNodeData::TemplateLiteral {
                quasis,
                expressions,
            } => {
                self.validate_template_lengths(node, *quasis, *expressions)?;
                let items = &self.lists[quasis.index()].items;
                for (index, quasi) in items.iter().enumerate() {
                    let IrNodeData::TemplateElement { tail, .. } = self.nodes[quasi.index()].data
                    else {
                        return Err(invalid("template quasi list contains non-quasi syntax"));
                    };
                    if tail != (index + 1 == items.len()) {
                        return Err(invalid("template tail flag does not match quasi position"));
                    }
                }
            }
            IrNodeData::TemplateElement { .. } => {}
            IrNodeData::Name { name } => {
                let name = &self.names[name.index()];
                let valid = match name.role {
                    NameRole::PrivateProperty => name.syntax == NameSyntax::PrivateIdentifier,
                    NameRole::ModuleSpecifier => name.syntax == NameSyntax::String,
                    NameRole::MetaKeyword => name.syntax == NameSyntax::Keyword,
                    NameRole::MetaProperty
                    | NameRole::Binding
                    | NameRole::Reference
                    | NameRole::AssignmentTarget
                    | NameRole::FunctionName
                    | NameRole::ClassName
                    | NameRole::LabelDeclaration
                    | NameRole::LabelReference
                    | NameRole::ImportBinding => name.syntax == NameSyntax::Identifier,
                    NameRole::Property
                    | NameRole::ImportName
                    | NameRole::ExportLocal
                    | NameRole::ExportedName
                    | NameRole::AttributeKey => {
                        matches!(name.syntax, NameSyntax::Identifier | NameSyntax::String)
                    }
                };
                if !valid {
                    return Err(invalid("name role and syntax are inconsistent"));
                }
            }
            IrNodeData::Identifier { name } => {
                if !self.name_node_has_syntax(*name, NameSyntax::Identifier) {
                    return Err(invalid("identifier must own an identifier-syntax name"));
                }
            }
            IrNodeData::MetaProperty { meta, property } => {
                if !self.name_node_has_syntax(*meta, NameSyntax::Keyword)
                    || !self.name_node_has_syntax(*property, NameSyntax::Identifier)
                {
                    return Err(invalid("meta-property names have invalid syntax"));
                }
            }
            IrNodeData::ArrayExpression { .. } | IrNodeData::Elision => {}
            IrNodeData::ObjectExpression { .. } => {}
            IrNodeData::ObjectProperty {
                key,
                value,
                kind,
                method,
                shorthand,
                computed,
                prototype_setter,
            } => {
                self.validate_property_key(node, *key, *computed)?;
                if (*method || *kind != PropertyKind::Init)
                    && !matches!(self.nodes[value.index()].data, IrNodeData::Function { .. })
                {
                    return Err(invalid("method/accessor property value must be a function"));
                }
                if *shorthand
                    && (*method
                        || *computed
                        || *kind != PropertyKind::Init
                        || key.kind != PropertyKeyKind::Identifier
                        || !matches!(
                            self.nodes[value.index()].data,
                            IrNodeData::Identifier { .. }
                        ))
                {
                    return Err(invalid("object shorthand has incompatible syntax"));
                }
                if *prototype_setter
                    && (*computed
                        || *method
                        || *shorthand
                        || *kind != PropertyKind::Init
                        || !matches!(
                            key.kind,
                            PropertyKeyKind::Identifier | PropertyKeyKind::String
                        ))
                {
                    return Err(invalid("prototype-setter flags are inconsistent"));
                }
            }
            IrNodeData::MemberExpression {
                property,
                property_kind,
                ..
            } => self.validate_member_property(node, *property, *property_kind)?,
            IrNodeData::ConditionalExpression {
                consequent,
                alternate,
                ..
            } => {
                if !matches!(
                    self.nodes[consequent.index()].data.category(),
                    NodeCategory::Expression | NodeCategory::Identifier
                ) || !matches!(
                    self.nodes[alternate.index()].data.category(),
                    NodeCategory::Expression | NodeCategory::Identifier
                ) {
                    return Err(invalid("conditional branches must be expression syntax"));
                }
            }
            IrNodeData::SequenceExpression { expressions } => {
                if self.lists[expressions.index()].items.is_empty() {
                    return Err(invalid("sequence expression must not be empty"));
                }
            }
            IrNodeData::ArrowFunction { parameters, .. } => {
                self.validate_rest_is_last(*parameters, node)?;
            }
            IrNodeData::MethodDefinition {
                key,
                value,
                kind,
                computed,
                ..
            } => {
                self.validate_property_key(node, *key, *computed)?;
                if !matches!(
                    self.nodes[value.index()].data,
                    IrNodeData::Function {
                        context: FunctionContext::Method,
                        ..
                    }
                ) {
                    return Err(invalid(
                        "class method value must use method function context",
                    ));
                }
                if matches!(kind, MethodKind::Get | MethodKind::Set) {
                    let IrNodeData::Function { parameters, .. } = self.nodes[value.index()].data
                    else {
                        unreachable!()
                    };
                    let expected = usize::from(*kind == MethodKind::Set);
                    if self.lists[parameters.index()].items.len() != expected {
                        return Err(invalid("getter/setter parameter count is invalid"));
                    }
                }
            }
            IrNodeData::PropertyDefinition { key, computed, .. } => {
                self.validate_property_key(node, *key, *computed)?;
            }
            IrNodeData::ArrayPattern { elements } => {
                self.validate_rest_is_last(*elements, node)?;
            }
            IrNodeData::ObjectPattern { rest, .. } => {
                if rest.is_some_and(|rest| {
                    !matches!(
                        self.nodes[rest.index()].data,
                        IrNodeData::RestPattern { .. }
                    )
                }) {
                    return Err(invalid("object pattern rest child must be a rest pattern"));
                }
            }
            IrNodeData::ObjectPatternProperty {
                key,
                value,
                shorthand,
                computed,
            } => {
                self.validate_property_key(node, *key, *computed)?;
                let shorthand_value_is_compatible = match self.nodes[value.index()].data {
                    IrNodeData::Identifier { .. } => true,
                    IrNodeData::AssignmentPattern { left, .. } => {
                        matches!(self.nodes[left.index()].data, IrNodeData::Identifier { .. })
                    }
                    _ => false,
                };
                if *shorthand
                    && (*computed
                        || key.kind != PropertyKeyKind::Identifier
                        || !shorthand_value_is_compatible)
                {
                    return Err(invalid("object-pattern shorthand has incompatible syntax"));
                }
            }
            IrNodeData::ImportDeclaration { source, .. } => {
                if !self.name_node_has_syntax(*source, NameSyntax::String) {
                    return Err(invalid("import source must be a string-syntax name"));
                }
            }
            IrNodeData::ImportSpecifier {
                kind,
                imported,
                local,
            } => {
                if (*kind == ImportSpecifierKind::Named) != imported.is_some() {
                    return Err(invalid(
                        "named import requires imported name; default/namespace forbid it",
                    ));
                }
                if !self.name_node_has_syntax(*local, NameSyntax::Identifier) {
                    return Err(invalid("import local must be an identifier-syntax name"));
                }
                if let Some(imported) = imported {
                    self.validate_module_name(node, *imported)?;
                }
            }
            IrNodeData::ExportNamedDeclaration {
                declaration,
                specifiers,
                source,
                attributes,
            } => {
                if declaration.is_some()
                    && (!self.lists[specifiers.index()].items.is_empty()
                        || source.is_some()
                        || attributes.is_some())
                {
                    return Err(invalid(
                        "export declaration cannot also contain specifiers/source/attributes",
                    ));
                }
                if source.is_none() && attributes.is_some() {
                    return Err(invalid("export attributes require a module source"));
                }
                if source
                    .is_some_and(|source| !self.name_node_has_syntax(source, NameSyntax::String))
                {
                    return Err(invalid("export source must be a string-syntax name"));
                }
            }
            IrNodeData::ExportDefaultDeclaration { value, kind } => {
                let valid = match kind {
                    ExportDefaultValueKind::Function => matches!(
                        self.nodes[value.index()].data,
                        IrNodeData::Function {
                            context: FunctionContext::ExportDefault,
                            ..
                        }
                    ),
                    ExportDefaultValueKind::Class => matches!(
                        self.nodes[value.index()].data,
                        IrNodeData::Class {
                            context: ClassContext::ExportDefault,
                            ..
                        }
                    ),
                    ExportDefaultValueKind::Expression => matches!(
                        self.nodes[value.index()].data.category(),
                        NodeCategory::Expression | NodeCategory::Identifier
                    ),
                };
                if !valid {
                    return Err(invalid("export-default kind does not match its value"));
                }
            }
            IrNodeData::ExportAllDeclaration {
                exported, source, ..
            } => {
                if !self.name_node_has_syntax(*source, NameSyntax::String) {
                    return Err(invalid("export-all source must be a string-syntax name"));
                }
                if let Some(exported) = exported {
                    self.validate_module_name(node, *exported)?;
                }
            }
        }
        Ok(())
    }

    fn normalize_directive_prologues(&mut self) {
        for node in &mut self.nodes {
            if let IrNodeData::ExpressionStatement { directive, .. } = &mut node.data {
                *directive = false;
            }
        }
        let directive_lists = self
            .nodes
            .iter()
            .filter_map(|node| match node.data {
                IrNodeData::Program { body, .. } => Some(body),
                IrNodeData::FunctionBody { statements, .. } => Some(statements),
                IrNodeData::VariableDeclaration { .. }
                | IrNodeData::VariableDeclarator { .. }
                | IrNodeData::Function { .. }
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
                | IrNodeData::ExportAllDeclaration { .. } => None,
            })
            .collect::<Vec<_>>();
        for list in directive_lists {
            let statements = self.lists[list.index()].items.clone();
            for statement in statements {
                let IrNodeData::ExpressionStatement { expression, .. } =
                    self.nodes[statement.index()].data
                else {
                    break;
                };
                let candidate = matches!(
                    self.nodes[expression.index()].data,
                    IrNodeData::StringLiteral { .. }
                ) && matches!(
                    (
                        self.nodes[statement.index()].origin,
                        self.nodes[expression.index()].origin
                    ),
                    (IrOrigin::Source(statement), IrOrigin::Source(expression))
                        if statement.lo == expression.lo
                );
                if !candidate {
                    break;
                }
                let IrNodeData::ExpressionStatement { directive, .. } =
                    &mut self.nodes[statement.index()].data
                else {
                    unreachable!()
                };
                *directive = true;
            }
        }
    }

    fn validate_grammar_ancestors(&self, node: NodeId) -> Result<(), TypedIrError> {
        let mut current = Some(node);
        while let Some(node) = current {
            self.validate_node_grammar(node)?;
            current = self.nodes[node.index()].parent.map(ParentLink::parent);
        }
        Ok(())
    }

    fn name_node_has_syntax(&self, node: NodeId, syntax: NameSyntax) -> bool {
        matches!(
            self.nodes.get(node.index()).map(IrNode::data),
            Some(IrNodeData::Name { name }) if self.names[name.index()].syntax == syntax
        )
    }

    fn validate_property_key(
        &self,
        owner: NodeId,
        key: IrPropertyKey,
        computed: bool,
    ) -> Result<(), TypedIrError> {
        let valid = match key.kind {
            PropertyKeyKind::Identifier => {
                !computed && self.name_node_has_syntax(key.value, NameSyntax::Identifier)
            }
            PropertyKeyKind::String => {
                !computed && self.name_node_has_syntax(key.value, NameSyntax::String)
            }
            PropertyKeyKind::Number => {
                !computed
                    && matches!(
                        self.nodes[key.value.index()].data,
                        IrNodeData::NumberLiteral { .. }
                    )
            }
            PropertyKeyKind::Computed => {
                computed
                    && matches!(
                        self.nodes[key.value.index()].data.category(),
                        NodeCategory::Expression | NodeCategory::Identifier
                    )
            }
            PropertyKeyKind::Private => {
                !computed && self.name_node_has_syntax(key.value, NameSyntax::PrivateIdentifier)
            }
        };
        if valid {
            Ok(())
        } else {
            Err(error(
                Some(owner),
                "property-key kind/computed flag does not match its child syntax",
            ))
        }
    }

    fn validate_member_property(
        &self,
        owner: NodeId,
        property: NodeId,
        kind: PropertyKeyKind,
    ) -> Result<(), TypedIrError> {
        let valid = match kind {
            PropertyKeyKind::Identifier => {
                self.name_node_has_syntax(property, NameSyntax::Identifier)
            }
            PropertyKeyKind::Computed => matches!(
                self.nodes[property.index()].data.category(),
                NodeCategory::Expression | NodeCategory::Identifier
            ),
            PropertyKeyKind::Private => {
                self.name_node_has_syntax(property, NameSyntax::PrivateIdentifier)
            }
            PropertyKeyKind::String | PropertyKeyKind::Number => false,
        };
        if valid {
            Ok(())
        } else {
            Err(error(
                Some(owner),
                "member-property kind does not match its child syntax",
            ))
        }
    }

    fn validate_module_name(&self, owner: NodeId, name: IrModuleName) -> Result<(), TypedIrError> {
        let syntax = match name.kind {
            ModuleNameKind::Identifier => NameSyntax::Identifier,
            ModuleNameKind::String => NameSyntax::String,
        };
        if self.name_node_has_syntax(name.value, syntax) {
            Ok(())
        } else {
            Err(error(
                Some(owner),
                "module-name kind does not match its child syntax",
            ))
        }
    }

    fn validate_rest_is_last(&self, list: ListId, owner: NodeId) -> Result<(), TypedIrError> {
        let items = &self.lists[list.index()].items;
        if items.iter().enumerate().any(|(index, item)| {
            matches!(
                self.nodes[item.index()].data,
                IrNodeData::RestPattern { .. }
            ) && index + 1 != items.len()
        }) {
            Err(error(
                Some(owner),
                "rest pattern must be the final list item",
            ))
        } else {
            Ok(())
        }
    }

    fn validate_directive_list(&self, list: ListId, owner: NodeId) -> Result<(), TypedIrError> {
        let mut ended = false;
        for statement in &self.lists[list.index()].items {
            let directive = matches!(
                self.nodes[statement.index()].data,
                IrNodeData::ExpressionStatement {
                    directive: true,
                    ..
                }
            );
            if directive && ended {
                return Err(error(
                    Some(owner),
                    "directive statements must form one contiguous list prefix",
                ));
            }
            ended |= !directive;
        }
        Ok(())
    }

    fn ensure_live(&self, node: NodeId) -> Result<(), TypedIrError> {
        let Some(record) = self.nodes.get(node.index()) else {
            return Err(error(Some(node), "unknown node"));
        };
        if record.tombstone {
            return Err(error(Some(node), "node is tombstoned"));
        }
        Ok(())
    }

    fn ensure_detached_builder_child(&self, node: NodeId) -> Result<(), TypedIrError> {
        self.ensure_live(node)?;
        if node == self.root {
            return Err(error(
                Some(node),
                "the active program root cannot become a detached builder child",
            ));
        }
        if self.nodes[node.index()].parent.is_some() {
            return Err(error(
                Some(node),
                "compound-node children must be detached live subtrees",
            ));
        }
        Ok(())
    }

    fn validate_detached_node_payload(
        &self,
        parent: NodeId,
        first_list: usize,
        data: &IrNodeData,
    ) -> Result<(), TypedIrError> {
        let mut children = HashSet::new();
        let mut referenced_lists = HashSet::new();
        for edge in data.edges() {
            match edge {
                Edge::Child(role, child) => {
                    self.ensure_detached_builder_child(child)?;
                    let category = self.nodes[child.index()].data.category();
                    if !role_accepts(role, category) {
                        return Err(error(
                            Some(child),
                            format!("{role:?} does not accept {category:?} syntax"),
                        ));
                    }
                    if !children.insert(child) {
                        return Err(error(
                            Some(child),
                            "compound node cannot own one occurrence through multiple fields",
                        ));
                    }
                }
                Edge::List(role, list_id) => {
                    if list_id.index() < first_list || list_id.index() >= self.lists.len() {
                        return Err(error(
                            Some(parent),
                            "compound node references a list outside its builder transaction",
                        ));
                    }
                    if !referenced_lists.insert(list_id) {
                        return Err(error(
                            Some(parent),
                            "compound node references the same list through multiple fields",
                        ));
                    }
                    let list = &self.lists[list_id.index()];
                    if list.parent != parent || list.role != role {
                        return Err(error(
                            Some(parent),
                            format!("builder list ownership mismatch for {role:?}"),
                        ));
                    }
                    for &child in &list.items {
                        self.ensure_detached_builder_child(child)?;
                        if !children.insert(child) {
                            return Err(error(
                                Some(child),
                                "compound node cannot own one occurrence through multiple fields",
                            ));
                        }
                    }
                }
            }
        }
        for index in first_list..self.lists.len() {
            let list = ListId(u32::try_from(index).map_err(|_| {
                error(
                    Some(parent),
                    "typed IR list arena exceeded u32::MAX entries",
                )
            })?);
            if !referenced_lists.contains(&list) {
                return Err(error(
                    Some(parent),
                    format!(
                        "builder list {} is not referenced by its node payload",
                        index
                    ),
                ));
            }
        }
        Ok(())
    }

    fn clone_detached_subtree_inner(&mut self, source: NodeId) -> Result<NodeId, TypedIrError> {
        let record = self
            .nodes
            .get(source.index())
            .cloned()
            .ok_or_else(|| error(Some(source), "unknown source subtree"))?;
        if record.tombstone {
            return Err(error(Some(source), "cannot clone a tombstoned subtree"));
        }

        if let IrNodeData::Name { name } = record.data {
            let cloned_name = self
                .names
                .get(name.index())
                .cloned()
                .ok_or_else(|| error(Some(source), "name node references an unknown NameId"))?;
            let new_name = NameId(push_index(self.names.len(), "name")?);
            self.names.push(cloned_name);
            return match self
                .append_detached_leaf(IrNodeData::Name { name: new_name }, record.origin)
            {
                Ok(node) => Ok(node),
                Err(error) => {
                    self.names.pop();
                    Err(error)
                }
            };
        }

        let mut data = record.data;
        let edges = data.edges();
        let mut cloned_lists = Vec::new();
        for edge in edges {
            match edge {
                Edge::Child(_, old) => {
                    let new = self.clone_detached_subtree_inner(old)?;
                    if !data.replace_singular(old, new) {
                        return Err(error(
                            Some(source),
                            "failed to remap a singular child while cloning",
                        ));
                    }
                }
                Edge::List(role, old) => {
                    let items = self
                        .lists
                        .get(old.index())
                        .ok_or_else(|| error(Some(source), "source subtree list is missing"))?
                        .items
                        .clone();
                    let mut cloned = Vec::with_capacity(items.len());
                    for item in items {
                        cloned.push(self.clone_detached_subtree_inner(item)?);
                    }
                    cloned_lists.push((role, old, cloned));
                }
            }
        }

        self.append_detached_node_with(record.origin, move |builder| {
            for (role, old, items) in cloned_lists {
                let new = builder.list(role, items)?;
                if !data.replace_list(old, new) {
                    return Err(error(
                        Some(source),
                        "failed to remap a child list while cloning",
                    ));
                }
            }
            Ok(data)
        })
    }

    fn ensure_detached_live(&self, node: NodeId) -> Result<(), TypedIrError> {
        self.ensure_live(node)?;
        if self.nodes[node.index()].parent.is_some() || node == self.root {
            return Err(error(Some(node), "replacement node must be detached"));
        }
        Ok(())
    }

    fn mark_tombstone_subtree(&mut self, root: NodeId) {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            stack.extend(self.child_ids_unchecked(node));
            self.nodes[node.index()].tombstone = true;
        }
    }
}

fn error(node: Option<NodeId>, message: impl Into<String>) -> TypedIrError {
    TypedIrError {
        node,
        message: message.into(),
    }
}

fn push_index(len: usize, kind: &str) -> Result<u32, TypedIrError> {
    u32::try_from(len).map_err(|_| {
        error(
            None,
            format!("typed IR {kind} arena exceeded u32::MAX entries"),
        )
    })
}

fn checked_arena_growth(current: usize, added: usize, kind: &str) -> Result<(), TypedIrError> {
    let last = current
        .checked_add(added)
        .ok_or_else(|| error(None, format!("typed IR {kind} arena size overflow")))?;
    u32::try_from(last).map_err(|_| {
        error(
            None,
            format!("typed IR {kind} arena exceeded u32::MAX entries"),
        )
    })?;
    Ok(())
}

fn offset_node(id: NodeId, offset: u32) -> NodeId {
    NodeId(
        id.0.checked_add(offset)
            .expect("node offset was prevalidated"),
    )
}

fn offset_list(id: ListId, offset: u32) -> ListId {
    ListId(
        id.0.checked_add(offset)
            .expect("list offset was prevalidated"),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Child(ChildRole, NodeId),
    List(ChildRole, ListId),
}

fn same_edges(left: &[Edge], right: &[Edge]) -> bool {
    left == right
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeCategory {
    Program,
    Statement,
    Expression,
    Identifier,
    Pattern,
    Name,
    VariableDeclarator,
    FunctionBody,
    FunctionValue,
    ExportDefaultValue,
    ClassMember,
    SwitchCase,
    CatchClause,
    TemplateElement,
    Elision,
    ObjectMember,
    Spread,
    PatternProperty,
    ImportSpecifier,
    ImportAttributes,
    ImportAttribute,
    ExportSpecifier,
}

impl IrNodeData {
    fn category(&self) -> NodeCategory {
        match self {
            Self::Program { .. } => NodeCategory::Program,
            Self::Function { context, .. } => match context {
                FunctionContext::Declaration => NodeCategory::Statement,
                FunctionContext::Expression => NodeCategory::Expression,
                FunctionContext::ExportDefault => NodeCategory::ExportDefaultValue,
                FunctionContext::Method => NodeCategory::FunctionValue,
            },
            Self::Class { context, .. } => match context {
                ClassContext::Declaration => NodeCategory::Statement,
                ClassContext::Expression => NodeCategory::Expression,
                ClassContext::ExportDefault => NodeCategory::ExportDefaultValue,
            },
            Self::VariableDeclaration { .. }
            | Self::Block { .. }
            | Self::EmptyStatement
            | Self::DebuggerStatement
            | Self::ExpressionStatement { .. }
            | Self::IfStatement { .. }
            | Self::ForStatement { .. }
            | Self::ForInStatement { .. }
            | Self::ForOfStatement { .. }
            | Self::WhileStatement { .. }
            | Self::DoWhileStatement { .. }
            | Self::SwitchStatement { .. }
            | Self::ReturnStatement { .. }
            | Self::BreakStatement { .. }
            | Self::ContinueStatement { .. }
            | Self::ThrowStatement { .. }
            | Self::TryStatement { .. }
            | Self::LabeledStatement { .. }
            | Self::WithStatement { .. }
            | Self::ImportDeclaration { .. }
            | Self::ExportNamedDeclaration { .. }
            | Self::ExportDefaultDeclaration { .. }
            | Self::ExportAllDeclaration { .. } => NodeCategory::Statement,
            Self::VariableDeclarator { .. } => NodeCategory::VariableDeclarator,
            Self::FunctionBody { .. } => NodeCategory::FunctionBody,
            Self::NumberLiteral { .. }
            | Self::StringLiteral { .. }
            | Self::BooleanLiteral { .. }
            | Self::NullLiteral
            | Self::BigIntLiteral { .. }
            | Self::RegExpLiteral { .. }
            | Self::TemplateLiteral { .. }
            | Self::ThisExpression
            | Self::SuperExpression
            | Self::MetaProperty { .. }
            | Self::ArrayExpression { .. }
            | Self::ObjectExpression { .. }
            | Self::UnaryExpression { .. }
            | Self::UpdateExpression { .. }
            | Self::BinaryExpression { .. }
            | Self::LogicalExpression { .. }
            | Self::AssignmentExpression { .. }
            | Self::ConditionalExpression { .. }
            | Self::CallExpression { .. }
            | Self::NewExpression { .. }
            | Self::MemberExpression { .. }
            | Self::SequenceExpression { .. }
            | Self::TaggedTemplateExpression { .. }
            | Self::AwaitExpression { .. }
            | Self::YieldExpression { .. }
            | Self::ImportExpression { .. }
            | Self::ArrowFunction { .. } => NodeCategory::Expression,
            Self::Identifier { .. } => NodeCategory::Identifier,
            Self::SpreadElement { .. } => NodeCategory::Spread,
            Self::Name { .. } => NodeCategory::Name,
            Self::TemplateElement { .. } => NodeCategory::TemplateElement,
            Self::Elision => NodeCategory::Elision,
            Self::ObjectProperty { .. } => NodeCategory::ObjectMember,
            Self::MethodDefinition { .. }
            | Self::PropertyDefinition { .. }
            | Self::StaticBlock { .. } => NodeCategory::ClassMember,
            Self::ArrayPattern { .. }
            | Self::ObjectPattern { .. }
            | Self::AssignmentPattern { .. }
            | Self::RestPattern { .. } => NodeCategory::Pattern,
            Self::ObjectPatternProperty { .. } => NodeCategory::PatternProperty,
            Self::SwitchCase { .. } => NodeCategory::SwitchCase,
            Self::CatchClause { .. } => NodeCategory::CatchClause,
            Self::ImportSpecifier { .. } => NodeCategory::ImportSpecifier,
            Self::ImportAttributes { .. } => NodeCategory::ImportAttributes,
            Self::ImportAttribute { .. } => NodeCategory::ImportAttribute,
            Self::ExportSpecifier { .. } => NodeCategory::ExportSpecifier,
        }
    }
}

fn role_accepts(role: ChildRole, category: NodeCategory) -> bool {
    use ChildRole as R;
    use NodeCategory as C;
    match role {
        R::ProgramBody
        | R::BlockBody
        | R::FunctionStatements
        | R::SwitchCaseBody
        | R::StaticBlockBody
        | R::LoopBody
        | R::LabeledBody
        | R::WithBody
        | R::TryBlock
        | R::FinallyBlock
        | R::CatchBody
        | R::ExportDeclaration => category == C::Statement,
        R::Consequent | R::Alternate => {
            matches!(category, C::Statement | C::Expression | C::Identifier)
        }
        R::DeclarationItems => category == C::VariableDeclarator,
        R::Binding | R::CatchParameter | R::FunctionParameters | R::PatternRest => {
            matches!(category, C::Pattern | C::Identifier)
        }
        R::PatternElements => matches!(category, C::Pattern | C::Identifier | C::Elision),
        R::Expression
        | R::Initializer
        | R::Test
        | R::ForTest
        | R::ForUpdate
        | R::ForRight
        | R::SwitchDiscriminant
        | R::SwitchCaseTest
        | R::ReturnArgument
        | R::ThrowArgument
        | R::WithObject
        | R::ClassSuper
        | R::Decorators
        | R::PropertyValue
        | R::UnaryArgument
        | R::UpdateArgument
        | R::Left
        | R::Right
        | R::Callee
        | R::Arguments
        | R::Object
        | R::SequenceItems
        | R::Tag
        | R::TemplateExpressions
        | R::SpreadArgument
        | R::AwaitArgument
        | R::YieldArgument
        | R::ImportSource
        | R::ImportOptions
        | R::PatternDefault => matches!(category, C::Expression | C::Identifier | C::Spread),
        R::ForInitializer | R::ForLeft => matches!(
            category,
            C::Statement | C::Expression | C::Identifier | C::Pattern
        ),
        R::FunctionName
        | R::ClassName
        | R::IdentifierName
        | R::Label
        | R::ImportImported
        | R::ImportLocal
        | R::AttributeKey
        | R::AttributeValue
        | R::ExportLocal
        | R::Exported
        | R::ExportAllName
        | R::MetaKeyword
        | R::MetaProperty
        | R::ModuleSource => matches!(category, C::Name | C::Expression),
        R::FunctionBody => category == C::FunctionBody,
        R::ArrowBody => matches!(category, C::FunctionBody | C::Expression | C::Identifier),
        R::ClassMembers => category == C::ClassMember,
        R::MethodKey | R::PropertyKey | R::MemberProperty => {
            matches!(category, C::Name | C::Expression | C::Identifier)
        }
        R::MethodValue => category == C::FunctionValue,
        R::SwitchCases => category == C::SwitchCase,
        R::CatchClause => category == C::CatchClause,
        R::Template => category == C::Expression,
        R::TemplateQuasis => category == C::TemplateElement,
        R::ArrayElements => matches!(
            category,
            C::Expression | C::Identifier | C::Spread | C::Elision
        ),
        R::ObjectMembers => matches!(category, C::ObjectMember | C::Spread),
        R::PatternProperties => category == C::PatternProperty,
        R::ImportSpecifiers => category == C::ImportSpecifier,
        R::ImportAttributes => category == C::ImportAttributes,
        R::AttributeItems => category == C::ImportAttribute,
        R::ExportSpecifiers => category == C::ExportSpecifier,
        R::ExportDefaultValue => matches!(
            category,
            C::ExportDefaultValue | C::Expression | C::Identifier
        ),
    }
}

impl IrNodeData {
    fn edges(&self) -> Vec<Edge> {
        use ChildRole as R;
        let mut edges = Vec::new();
        macro_rules! child {
            ($role:expr, $value:expr) => {
                edges.push(Edge::Child($role, $value))
            };
        }
        macro_rules! optional {
            ($role:expr, $value:expr) => {
                if let Some(value) = $value {
                    edges.push(Edge::Child($role, value));
                }
            };
        }
        macro_rules! list {
            ($role:expr, $value:expr) => {
                edges.push(Edge::List($role, $value))
            };
        }
        macro_rules! key {
            ($role:expr, $value:expr) => {
                child!($role, $value.value);
            };
        }
        match *self {
            Self::Program { body, .. } => list!(R::ProgramBody, body),
            Self::VariableDeclaration { declarations, .. } => {
                list!(R::DeclarationItems, declarations);
            }
            Self::VariableDeclarator {
                binding,
                initializer,
            } => {
                child!(R::Binding, binding);
                optional!(R::Initializer, initializer);
            }
            Self::Function {
                name,
                parameters,
                body,
                ..
            } => {
                optional!(R::FunctionName, name);
                list!(R::FunctionParameters, parameters);
                optional!(R::FunctionBody, body);
            }
            Self::FunctionBody { statements, .. } => list!(R::FunctionStatements, statements),
            Self::Class {
                name,
                super_class,
                members,
                decorators,
                ..
            } => {
                optional!(R::ClassName, name);
                optional!(R::ClassSuper, super_class);
                list!(R::ClassMembers, members);
                list!(R::Decorators, decorators);
            }
            Self::Block { body } => list!(R::BlockBody, body),
            Self::ExpressionStatement { expression, .. } => child!(R::Expression, expression),
            Self::IfStatement {
                test,
                consequent,
                alternate,
            } => {
                child!(R::Test, test);
                child!(R::Consequent, consequent);
                optional!(R::Alternate, alternate);
            }
            Self::ForStatement {
                initializer,
                test,
                update,
                body,
                ..
            } => {
                optional!(R::ForInitializer, initializer);
                optional!(R::ForTest, test);
                optional!(R::ForUpdate, update);
                child!(R::LoopBody, body);
            }
            Self::ForInStatement {
                left, right, body, ..
            }
            | Self::ForOfStatement {
                left, right, body, ..
            } => {
                child!(R::ForLeft, left);
                child!(R::ForRight, right);
                child!(R::LoopBody, body);
            }
            Self::WhileStatement { test, body } => {
                child!(R::Test, test);
                child!(R::LoopBody, body);
            }
            Self::DoWhileStatement { body, test } => {
                child!(R::LoopBody, body);
                child!(R::Test, test);
            }
            Self::SwitchStatement {
                discriminant,
                cases,
            } => {
                child!(R::SwitchDiscriminant, discriminant);
                list!(R::SwitchCases, cases);
            }
            Self::SwitchCase { test, consequent } => {
                optional!(R::SwitchCaseTest, test);
                list!(R::SwitchCaseBody, consequent);
            }
            Self::ReturnStatement { argument } => optional!(R::ReturnArgument, argument),
            Self::BreakStatement { label } | Self::ContinueStatement { label } => {
                optional!(R::Label, label);
            }
            Self::ThrowStatement { argument } => child!(R::ThrowArgument, argument),
            Self::TryStatement {
                block,
                handler,
                finalizer,
            } => {
                child!(R::TryBlock, block);
                optional!(R::CatchClause, handler);
                optional!(R::FinallyBlock, finalizer);
            }
            Self::CatchClause { parameter, body } => {
                optional!(R::CatchParameter, parameter);
                child!(R::CatchBody, body);
            }
            Self::LabeledStatement { label, body } => {
                child!(R::Label, label);
                child!(R::LabeledBody, body);
            }
            Self::WithStatement { object, body } => {
                child!(R::WithObject, object);
                child!(R::WithBody, body);
            }
            Self::TemplateLiteral {
                quasis,
                expressions,
            } => {
                list!(R::TemplateQuasis, quasis);
                list!(R::TemplateExpressions, expressions);
            }
            Self::Identifier { name } => child!(R::IdentifierName, name),
            Self::MetaProperty { meta, property } => {
                child!(R::MetaKeyword, meta);
                child!(R::MetaProperty, property);
            }
            Self::ArrayExpression { elements } => list!(R::ArrayElements, elements),
            Self::ObjectExpression { members } => list!(R::ObjectMembers, members),
            Self::ObjectProperty {
                key: property_key,
                value,
                ..
            } => {
                key!(R::PropertyKey, property_key);
                child!(R::PropertyValue, value);
            }
            Self::UnaryExpression { argument, .. } => child!(R::UnaryArgument, argument),
            Self::UpdateExpression { argument, .. } => child!(R::UpdateArgument, argument),
            Self::BinaryExpression { left, right, .. }
            | Self::LogicalExpression { left, right, .. }
            | Self::AssignmentExpression { left, right, .. } => {
                child!(R::Left, left);
                child!(R::Right, right);
            }
            Self::ConditionalExpression {
                test,
                consequent,
                alternate,
            } => {
                child!(R::Test, test);
                child!(R::Consequent, consequent);
                child!(R::Alternate, alternate);
            }
            Self::CallExpression {
                callee, arguments, ..
            }
            | Self::NewExpression { callee, arguments } => {
                child!(R::Callee, callee);
                list!(R::Arguments, arguments);
            }
            Self::MemberExpression {
                object, property, ..
            } => {
                child!(R::Object, object);
                child!(R::MemberProperty, property);
            }
            Self::SequenceExpression { expressions } => list!(R::SequenceItems, expressions),
            Self::TaggedTemplateExpression { tag, quasi } => {
                child!(R::Tag, tag);
                child!(R::Template, quasi);
            }
            Self::SpreadElement { argument } => child!(R::SpreadArgument, argument),
            Self::AwaitExpression { argument } => child!(R::AwaitArgument, argument),
            Self::YieldExpression { argument, .. } => optional!(R::YieldArgument, argument),
            Self::ImportExpression { source, options } => {
                child!(R::ImportSource, source);
                optional!(R::ImportOptions, options);
            }
            Self::ArrowFunction {
                parameters, body, ..
            } => {
                list!(R::FunctionParameters, parameters);
                child!(R::ArrowBody, body);
            }
            Self::MethodDefinition {
                key: property_key,
                value,
                decorators,
                ..
            } => {
                key!(R::MethodKey, property_key);
                child!(R::MethodValue, value);
                list!(R::Decorators, decorators);
            }
            Self::PropertyDefinition {
                key: property_key,
                value,
                decorators,
                ..
            } => {
                key!(R::PropertyKey, property_key);
                optional!(R::PropertyValue, value);
                list!(R::Decorators, decorators);
            }
            Self::StaticBlock { body } => list!(R::StaticBlockBody, body),
            Self::ArrayPattern { elements } => list!(R::PatternElements, elements),
            Self::ObjectPattern { properties, rest } => {
                list!(R::PatternProperties, properties);
                optional!(R::PatternRest, rest);
            }
            Self::ObjectPatternProperty {
                key: property_key,
                value,
                ..
            } => {
                key!(R::PropertyKey, property_key);
                child!(R::Binding, value);
            }
            Self::AssignmentPattern { left, right } => {
                child!(R::Binding, left);
                child!(R::PatternDefault, right);
            }
            Self::RestPattern { argument } => child!(R::Binding, argument),
            Self::ImportDeclaration {
                specifiers,
                source,
                attributes,
            } => {
                list!(R::ImportSpecifiers, specifiers);
                child!(R::ModuleSource, source);
                optional!(R::ImportAttributes, attributes);
            }
            Self::ImportSpecifier {
                imported, local, ..
            } => {
                if let Some(imported) = imported {
                    child!(R::ImportImported, imported.value);
                }
                child!(R::ImportLocal, local);
            }
            Self::ImportAttributes { items, .. } => list!(R::AttributeItems, items),
            Self::ImportAttribute {
                key: attribute_key,
                value,
            } => {
                child!(R::AttributeKey, attribute_key.value);
                child!(R::AttributeValue, value);
            }
            Self::ExportNamedDeclaration {
                declaration,
                specifiers,
                source,
                attributes,
            } => {
                optional!(R::ExportDeclaration, declaration);
                list!(R::ExportSpecifiers, specifiers);
                optional!(R::ModuleSource, source);
                optional!(R::ImportAttributes, attributes);
            }
            Self::ExportSpecifier { local, exported } => {
                child!(R::ExportLocal, local.value);
                child!(R::Exported, exported.value);
            }
            Self::ExportDefaultDeclaration { value, .. } => child!(R::ExportDefaultValue, value),
            Self::ExportAllDeclaration {
                exported,
                source,
                attributes,
            } => {
                if let Some(exported) = exported {
                    child!(R::ExportAllName, exported.value);
                }
                child!(R::ModuleSource, source);
                optional!(R::ImportAttributes, attributes);
            }
            Self::NumberLiteral { .. }
            | Self::StringLiteral { .. }
            | Self::BooleanLiteral { .. }
            | Self::NullLiteral
            | Self::BigIntLiteral { .. }
            | Self::RegExpLiteral { .. }
            | Self::TemplateElement { .. }
            | Self::Name { .. }
            | Self::ThisExpression
            | Self::SuperExpression
            | Self::Elision
            | Self::EmptyStatement
            | Self::DebuggerStatement => {}
        }
        edges
    }

    fn replace_singular(&mut self, old: NodeId, new: NodeId) -> bool {
        let mut replaced = false;
        macro_rules! slot {
            ($value:expr) => {
                if *$value == old {
                    *$value = new;
                    replaced = true;
                }
            };
        }
        macro_rules! optional_slot {
            ($value:expr) => {
                if let Some(value) = $value.as_mut() {
                    slot!(value);
                }
            };
        }
        macro_rules! key_slot {
            ($value:expr) => {
                slot!(&mut $value.value);
            };
        }
        match self {
            Self::VariableDeclarator {
                binding,
                initializer,
            } => {
                slot!(binding);
                optional_slot!(initializer);
            }
            Self::Function { name, body, .. } => {
                optional_slot!(name);
                optional_slot!(body);
            }
            Self::Class {
                name, super_class, ..
            } => {
                optional_slot!(name);
                optional_slot!(super_class);
            }
            Self::ExpressionStatement { expression, .. } => slot!(expression),
            Self::IfStatement {
                test,
                consequent,
                alternate,
            } => {
                slot!(test);
                slot!(consequent);
                optional_slot!(alternate);
            }
            Self::ForStatement {
                initializer,
                test,
                update,
                body,
                ..
            } => {
                optional_slot!(initializer);
                optional_slot!(test);
                optional_slot!(update);
                slot!(body);
            }
            Self::ForInStatement {
                left, right, body, ..
            }
            | Self::ForOfStatement {
                left, right, body, ..
            } => {
                slot!(left);
                slot!(right);
                slot!(body);
            }
            Self::WhileStatement { test, body } => {
                slot!(test);
                slot!(body);
            }
            Self::DoWhileStatement { body, test } => {
                slot!(body);
                slot!(test);
            }
            Self::SwitchStatement { discriminant, .. } => slot!(discriminant),
            Self::SwitchCase { test, .. } => optional_slot!(test),
            Self::ReturnStatement { argument } | Self::YieldExpression { argument, .. } => {
                optional_slot!(argument)
            }
            Self::BreakStatement { label } | Self::ContinueStatement { label } => {
                optional_slot!(label)
            }
            Self::ThrowStatement { argument }
            | Self::UnaryExpression { argument, .. }
            | Self::UpdateExpression { argument, .. }
            | Self::SpreadElement { argument }
            | Self::AwaitExpression { argument }
            | Self::RestPattern { argument } => slot!(argument),
            Self::TryStatement {
                block,
                handler,
                finalizer,
            } => {
                slot!(block);
                optional_slot!(handler);
                optional_slot!(finalizer);
            }
            Self::CatchClause { parameter, body } => {
                optional_slot!(parameter);
                slot!(body);
            }
            Self::LabeledStatement { label, body } => {
                slot!(label);
                slot!(body);
            }
            Self::WithStatement { object, body } => {
                slot!(object);
                slot!(body);
            }
            Self::Identifier { name } => slot!(name),
            Self::MetaProperty { meta, property } => {
                slot!(meta);
                slot!(property);
            }
            Self::ObjectProperty { key, value, .. } => {
                key_slot!(key);
                slot!(value);
            }
            Self::BinaryExpression { left, right, .. }
            | Self::LogicalExpression { left, right, .. }
            | Self::AssignmentExpression { left, right, .. }
            | Self::AssignmentPattern { left, right } => {
                slot!(left);
                slot!(right);
            }
            Self::ConditionalExpression {
                test,
                consequent,
                alternate,
            } => {
                slot!(test);
                slot!(consequent);
                slot!(alternate);
            }
            Self::CallExpression { callee, .. } | Self::NewExpression { callee, .. } => {
                slot!(callee)
            }
            Self::MemberExpression {
                object, property, ..
            } => {
                slot!(object);
                slot!(property);
            }
            Self::TaggedTemplateExpression { tag, quasi } => {
                slot!(tag);
                slot!(quasi);
            }
            Self::ImportExpression { source, options } => {
                slot!(source);
                optional_slot!(options);
            }
            Self::ArrowFunction { body, .. } => slot!(body),
            Self::MethodDefinition { key, value, .. } => {
                key_slot!(key);
                slot!(value);
            }
            Self::PropertyDefinition { key, value, .. } => {
                key_slot!(key);
                optional_slot!(value);
            }
            Self::ObjectPattern { rest, .. } => optional_slot!(rest),
            Self::ObjectPatternProperty { key, value, .. } => {
                key_slot!(key);
                slot!(value);
            }
            Self::ImportDeclaration {
                source, attributes, ..
            } => {
                slot!(source);
                optional_slot!(attributes);
            }
            Self::ImportSpecifier {
                imported, local, ..
            } => {
                if let Some(imported) = imported.as_mut() {
                    slot!(&mut imported.value);
                }
                slot!(local);
            }
            Self::ImportAttribute { key, value } => {
                slot!(&mut key.value);
                slot!(value);
            }
            Self::ExportNamedDeclaration {
                declaration,
                source,
                attributes,
                ..
            } => {
                optional_slot!(declaration);
                optional_slot!(source);
                optional_slot!(attributes);
            }
            Self::ExportSpecifier { local, exported } => {
                slot!(&mut local.value);
                slot!(&mut exported.value);
            }
            Self::ExportDefaultDeclaration { value, .. } => slot!(value),
            Self::ExportAllDeclaration {
                exported,
                source,
                attributes,
                ..
            } => {
                if let Some(exported) = exported.as_mut() {
                    slot!(&mut exported.value);
                }
                slot!(source);
                optional_slot!(attributes);
            }
            Self::Program { .. }
            | Self::VariableDeclaration { .. }
            | Self::FunctionBody { .. }
            | Self::Block { .. }
            | Self::EmptyStatement
            | Self::DebuggerStatement
            | Self::NumberLiteral { .. }
            | Self::StringLiteral { .. }
            | Self::BooleanLiteral { .. }
            | Self::NullLiteral
            | Self::BigIntLiteral { .. }
            | Self::RegExpLiteral { .. }
            | Self::TemplateLiteral { .. }
            | Self::TemplateElement { .. }
            | Self::Name { .. }
            | Self::ThisExpression
            | Self::SuperExpression
            | Self::ArrayExpression { .. }
            | Self::Elision
            | Self::ObjectExpression { .. }
            | Self::SequenceExpression { .. }
            | Self::StaticBlock { .. }
            | Self::ArrayPattern { .. }
            | Self::ImportAttributes { .. } => {}
        }
        replaced
    }

    fn replace_list(&mut self, old: ListId, new: ListId) -> bool {
        let mut replaced = false;
        macro_rules! list_slot {
            ($value:expr) => {
                if *$value == old {
                    *$value = new;
                    replaced = true;
                }
            };
        }
        match self {
            Self::Program { body, .. } => list_slot!(body),
            Self::VariableDeclaration { declarations, .. } => list_slot!(declarations),
            Self::Function { parameters, .. } | Self::ArrowFunction { parameters, .. } => {
                list_slot!(parameters);
            }
            Self::FunctionBody { statements, .. } => list_slot!(statements),
            Self::Class {
                members,
                decorators,
                ..
            } => {
                list_slot!(members);
                list_slot!(decorators);
            }
            Self::Block { body } => list_slot!(body),
            Self::SwitchStatement { cases, .. } => list_slot!(cases),
            Self::SwitchCase { consequent, .. } => list_slot!(consequent),
            Self::TemplateLiteral {
                quasis,
                expressions,
            } => {
                list_slot!(quasis);
                list_slot!(expressions);
            }
            Self::ArrayExpression { elements } | Self::ArrayPattern { elements } => {
                list_slot!(elements);
            }
            Self::ObjectExpression { members } => list_slot!(members),
            Self::CallExpression { arguments, .. } | Self::NewExpression { arguments, .. } => {
                list_slot!(arguments);
            }
            Self::SequenceExpression { expressions } => list_slot!(expressions),
            Self::MethodDefinition { decorators, .. }
            | Self::PropertyDefinition { decorators, .. } => list_slot!(decorators),
            Self::StaticBlock { body } => list_slot!(body),
            Self::ObjectPattern { properties, .. } => list_slot!(properties),
            Self::ImportDeclaration { specifiers, .. } => list_slot!(specifiers),
            Self::ImportAttributes { items, .. } => list_slot!(items),
            Self::ExportNamedDeclaration { specifiers, .. } => list_slot!(specifiers),
            Self::VariableDeclarator { .. }
            | Self::EmptyStatement
            | Self::DebuggerStatement
            | Self::ExpressionStatement { .. }
            | Self::IfStatement { .. }
            | Self::ForStatement { .. }
            | Self::ForInStatement { .. }
            | Self::ForOfStatement { .. }
            | Self::WhileStatement { .. }
            | Self::DoWhileStatement { .. }
            | Self::ReturnStatement { .. }
            | Self::BreakStatement { .. }
            | Self::ContinueStatement { .. }
            | Self::ThrowStatement { .. }
            | Self::TryStatement { .. }
            | Self::CatchClause { .. }
            | Self::LabeledStatement { .. }
            | Self::WithStatement { .. }
            | Self::NumberLiteral { .. }
            | Self::StringLiteral { .. }
            | Self::BooleanLiteral { .. }
            | Self::NullLiteral
            | Self::BigIntLiteral { .. }
            | Self::RegExpLiteral { .. }
            | Self::TemplateElement { .. }
            | Self::Name { .. }
            | Self::Identifier { .. }
            | Self::ThisExpression
            | Self::SuperExpression
            | Self::MetaProperty { .. }
            | Self::Elision
            | Self::ObjectProperty { .. }
            | Self::UnaryExpression { .. }
            | Self::UpdateExpression { .. }
            | Self::BinaryExpression { .. }
            | Self::LogicalExpression { .. }
            | Self::AssignmentExpression { .. }
            | Self::ConditionalExpression { .. }
            | Self::MemberExpression { .. }
            | Self::TaggedTemplateExpression { .. }
            | Self::SpreadElement { .. }
            | Self::AwaitExpression { .. }
            | Self::YieldExpression { .. }
            | Self::ImportExpression { .. }
            | Self::ObjectPatternProperty { .. }
            | Self::AssignmentPattern { .. }
            | Self::RestPattern { .. }
            | Self::ImportSpecifier { .. }
            | Self::ImportAttribute { .. }
            | Self::ExportSpecifier { .. }
            | Self::ExportDefaultDeclaration { .. }
            | Self::ExportAllDeclaration { .. } => {}
        }
        replaced
    }

    fn offset_ids(&mut self, node_offset: u32, list_offset: u32, name_offset: u32) {
        let edges = self.edges();
        // Remap descending so a positive offset can never turn an already-rewritten low ID into
        // an unprocessed old ID (for example old list 0 -> 1 while old list 1 still exists).
        let mut child_ids = edges
            .iter()
            .filter_map(|edge| match edge {
                Edge::Child(_, child) => Some(*child),
                Edge::List(_, _) => None,
            })
            .collect::<Vec<_>>();
        child_ids.sort_unstable_by(|left, right| right.cmp(left));
        child_ids.dedup();
        for old in child_ids {
            let new = offset_node(old, node_offset);
            let replaced = self.replace_singular(old, new);
            debug_assert!(replaced);
        }
        let mut list_ids = edges
            .iter()
            .filter_map(|edge| match edge {
                Edge::Child(_, _) => None,
                Edge::List(_, list) => Some(*list),
            })
            .collect::<Vec<_>>();
        list_ids.sort_unstable_by(|left, right| right.cmp(left));
        list_ids.dedup();
        for old in list_ids {
            let new = offset_list(old, list_offset);
            let replaced = self.replace_list(old, new);
            debug_assert!(replaced);
        }
        if let Self::Name { name } = self {
            name.0 = name
                .0
                .checked_add(name_offset)
                .expect("name offset was prevalidated");
        }
    }
}

struct Fnv64(u64);

impl Fnv64 {
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    const fn finish(self) -> u64 {
        self.0
    }
}

fn owned_symbols(interner: &Interner, semantic: Option<&SemanticModel>) -> Vec<IrSymbol> {
    semantic.map_or_else(Vec::new, |semantic| {
        semantic
            .symbols
            .iter()
            .map(|symbol| {
                let original_name = interner.resolve(symbol.name);
                IrSymbol {
                    emitted_name: original_name.clone(),
                    original_name,
                    decl_kind: symbol.decl_kind,
                }
            })
            .collect()
    })
}

#[derive(Default)]
struct NameResolver {
    bindings: HashMap<(Span, String), Option<SymbolId>>,
    references: HashMap<(Span, String), Option<SymbolId>>,
}

impl NameResolver {
    fn new(interner: &Interner, semantic: Option<&SemanticModel>) -> Self {
        let mut resolver = Self::default();
        let Some(semantic) = semantic else {
            return resolver;
        };
        for occurrence in &semantic.binding_occurrences {
            insert_symbol(
                &mut resolver.bindings,
                occurrence.span,
                interner.resolve(occurrence.name),
                occurrence.symbol,
            );
        }
        for reference in &semantic.references {
            let Some(symbol) = reference.resolved else {
                continue;
            };
            insert_symbol(
                &mut resolver.references,
                reference.span,
                interner.resolve(reference.name),
                symbol,
            );
        }
        resolver
    }

    fn resolve(&self, span: Span, spelling: &str, role: NameRole) -> Option<SymbolId> {
        if span.is_dummy() {
            return None;
        }
        let table = match role {
            NameRole::Binding
            | NameRole::FunctionName
            | NameRole::ClassName
            | NameRole::ImportBinding => &self.bindings,
            NameRole::Reference | NameRole::AssignmentTarget | NameRole::ExportLocal => {
                &self.references
            }
            NameRole::Property
            | NameRole::PrivateProperty
            | NameRole::LabelDeclaration
            | NameRole::LabelReference
            | NameRole::ImportName
            | NameRole::ModuleSpecifier
            | NameRole::ExportedName
            | NameRole::AttributeKey
            | NameRole::MetaKeyword
            | NameRole::MetaProperty => return None,
        };
        table.get(&(span, spelling.to_owned())).copied().flatten()
    }
}

fn insert_symbol(
    table: &mut HashMap<(Span, String), Option<SymbolId>>,
    span: Span,
    spelling: String,
    symbol: SymbolId,
) {
    if span.is_dummy() {
        return;
    }
    table
        .entry((span, spelling))
        .and_modify(|candidate| {
            if *candidate != Some(symbol) {
                *candidate = None;
            }
        })
        .or_insert(Some(symbol));
}

struct Lowerer<'i> {
    ir: TypedProgram,
    interner: &'i Interner,
    resolver: NameResolver,
}

impl Lowerer<'_> {
    fn next_node(&self) -> NodeId {
        NodeId(u32::try_from(self.ir.nodes.len()).expect("typed IR node arena exceeds u32::MAX"))
    }

    fn list_for(&mut self, parent: NodeId, role: ChildRole, items: Vec<NodeId>) -> ListId {
        let id = ListId(
            u32::try_from(self.ir.lists.len()).expect("typed IR list arena exceeds u32::MAX"),
        );
        self.ir.lists.push(IrList {
            id,
            parent,
            role,
            items,
        });
        id
    }

    fn finish(&mut self, span: Span, data: IrNodeData) -> NodeId {
        self.finish_with_origin(IrOrigin::from_parser_span(span), data)
    }

    fn finish_with_origin(&mut self, origin: IrOrigin, data: IrNodeData) -> NodeId {
        let id = self.next_node();
        self.ir.nodes.push(IrNode {
            id,
            parent: None,
            origin,
            data,
            tombstone: false,
        });
        for edge in self.ir.nodes[id.index()].data.edges() {
            match edge {
                Edge::Child(role, child) => {
                    let record = &mut self.ir.nodes[child.index()];
                    debug_assert!(record.parent.is_none());
                    record.parent = Some(ParentLink {
                        parent: id,
                        role,
                        list: None,
                    });
                }
                Edge::List(role, list) => {
                    let list_record = &self.ir.lists[list.index()];
                    debug_assert_eq!(list_record.parent, id);
                    debug_assert_eq!(list_record.role, role);
                    for &child in &list_record.items {
                        let record = &mut self.ir.nodes[child.index()];
                        debug_assert!(record.parent.is_none());
                        record.parent = Some(ParentLink {
                            parent: id,
                            role,
                            list: Some(list),
                        });
                    }
                }
            }
        }
        id
    }

    fn name_text(
        &mut self,
        spelling: String,
        span: Span,
        role: NameRole,
        syntax: NameSyntax,
    ) -> NodeId {
        self.name_text_with_origin(
            spelling,
            span,
            role,
            syntax,
            IrOrigin::from_parser_span(span),
        )
    }

    fn name_text_with_origin(
        &mut self,
        spelling: String,
        semantic_span: Span,
        role: NameRole,
        syntax: NameSyntax,
        origin: IrOrigin,
    ) -> NodeId {
        let symbol = self.resolver.resolve(semantic_span, &spelling, role);
        let name = NameId(
            u32::try_from(self.ir.names.len()).expect("typed IR name arena exceeds u32::MAX"),
        );
        self.ir.names.push(IrName {
            original: spelling.clone(),
            emitted: spelling,
            role,
            syntax,
            symbol,
        });
        self.finish_with_origin(origin, IrNodeData::Name { name })
    }

    fn ident(&mut self, ident: Ident, role: NameRole) -> NodeId {
        let syntax = if role == NameRole::PrivateProperty {
            NameSyntax::PrivateIdentifier
        } else {
            NameSyntax::Identifier
        };
        self.name_text(self.interner.resolve(ident.name), ident.span, role, syntax)
    }

    fn lower_program(&mut self, program: &Program<'_>, plan: &TypedLoweringPlan) -> NodeId {
        let body = program
            .body
            .iter()
            .enumerate()
            .map(|(ordinal, statement)| {
                let is_export_function = matches!(
                    statement,
                    Statement::ExportNamed(declaration)
                        if declaration.specifiers.is_empty()
                            && declaration.source.is_none()
                            && declaration.attributes.is_none()
                            && matches!(
                            declaration.declaration,
                            Some(Statement::FunctionDeclaration(_))
                        )
                ) || matches!(
                    statement,
                    Statement::ExportDefault(declaration)
                        if matches!(declaration.declaration, ExportDefaultKind::Function(_))
                );
                let is_inert_export_const = matches!(
                    statement,
                    Statement::ExportNamed(export)
                        if export.specifiers.is_empty()
                            && export.source.is_none()
                            && export.attributes.is_none()
                            && !export.span.is_dummy()
                            && matches!(
                                export.declaration,
                                Some(Statement::VariableDeclaration(declaration))
                                    if is_presemantic_inert_export_const(declaration)
                            )
                );
                if (plan.elides_top_level_export_function(ordinal) && is_export_function)
                    || (plan.elides_top_level_export_const(ordinal) && is_inert_export_const)
                {
                    self.empty_export(statement.span())
                } else {
                    self.statement(statement)
                }
            })
            .collect();
        let parent = self.next_node();
        let body = self.list_for(parent, ChildRole::ProgramBody, body);
        self.finish(
            program.span,
            IrNodeData::Program {
                source_type: program.source_type,
                strict: program.strict,
                spread_helper: program
                    .spread_helper
                    .map(|atom| self.interner.resolve(atom)),
                object_spread_helper: program
                    .object_spread_helper
                    .map(|atom| self.interner.resolve(atom)),
                for_of_helper: program
                    .for_of_helper
                    .map(|atom| self.interner.resolve(atom)),
                body,
            },
        )
    }

    /// Keep an ESM marker at the original source origin when a proven dead, pure export
    /// declaration does not need to enter the owned arena. Bundled CommonJS planning uses this
    /// marker to retain the module's ESM identity (`__esModule`) even though the declaration
    /// itself is absent.
    fn empty_export(&mut self, span: Span) -> NodeId {
        let parent = self.next_node();
        let specifiers = self.list_for(parent, ChildRole::ExportSpecifiers, Vec::new());
        self.finish(
            span,
            IrNodeData::ExportNamedDeclaration {
                declaration: None,
                specifiers,
                source: None,
                attributes: None,
            },
        )
    }

    fn statement(&mut self, statement: &Statement<'_>) -> NodeId {
        match statement {
            Statement::VariableDeclaration(declaration) => self.variable_declaration(declaration),
            Statement::FunctionDeclaration(function) => {
                self.function(function, FunctionContext::Declaration)
            }
            Statement::ClassDeclaration(class) => self.class(class, ClassContext::Declaration),
            Statement::Block(block) => {
                let body = block.body.iter().map(|item| self.statement(item)).collect();
                let parent = self.next_node();
                let body = self.list_for(parent, ChildRole::BlockBody, body);
                self.finish(block.span, IrNodeData::Block { body })
            }
            Statement::Empty(span) => self.finish(*span, IrNodeData::EmptyStatement),
            Statement::Debugger(span) => self.finish(*span, IrNodeData::DebuggerStatement),
            Statement::Expression(expression) => {
                let value = self.expression(&expression.expression, NameRole::Reference);
                self.finish(
                    expression.span,
                    IrNodeData::ExpressionStatement {
                        expression: value,
                        directive: false,
                    },
                )
            }
            Statement::If(statement) => {
                let test = self.expression(&statement.test, NameRole::Reference);
                let consequent = self.statement(&statement.consequent);
                let alternate = statement
                    .alternate
                    .as_ref()
                    .map(|alternate| self.statement(alternate));
                self.finish(
                    statement.span,
                    IrNodeData::IfStatement {
                        test,
                        consequent,
                        alternate,
                    },
                )
            }
            Statement::For(statement) => {
                let (initializer, initializer_kind) = match statement.init {
                    Some(ForInit::Variable(declaration)) => (
                        Some(self.variable_declaration(declaration)),
                        Some(ForInitializerKind::Variable),
                    ),
                    Some(ForInit::Expression(expression)) => (
                        Some(self.expression(&expression, NameRole::Reference)),
                        Some(ForInitializerKind::Expression),
                    ),
                    None => (None, None),
                };
                let test = statement
                    .test
                    .as_ref()
                    .map(|expression| self.expression(expression, NameRole::Reference));
                let update = statement
                    .update
                    .as_ref()
                    .map(|expression| self.expression(expression, NameRole::Reference));
                let body = self.statement(&statement.body);
                self.finish(
                    statement.span,
                    IrNodeData::ForStatement {
                        initializer,
                        initializer_kind,
                        test,
                        update,
                        body,
                    },
                )
            }
            Statement::ForIn(statement) => {
                let (left, left_kind) = self.for_left(statement.left);
                let right = self.expression(&statement.right, NameRole::Reference);
                let body = self.statement(&statement.body);
                self.finish(
                    statement.span,
                    IrNodeData::ForInStatement {
                        left,
                        left_kind,
                        right,
                        body,
                    },
                )
            }
            Statement::ForOf(statement) => {
                let (left, left_kind) = self.for_left(statement.left);
                let right = self.expression(&statement.right, NameRole::Reference);
                let body = self.statement(&statement.body);
                self.finish(
                    statement.span,
                    IrNodeData::ForOfStatement {
                        left,
                        left_kind,
                        right,
                        body,
                        is_await: statement.is_await,
                    },
                )
            }
            Statement::While(statement) => {
                let test = self.expression(&statement.test, NameRole::Reference);
                let body = self.statement(&statement.body);
                self.finish(statement.span, IrNodeData::WhileStatement { test, body })
            }
            Statement::DoWhile(statement) => {
                let body = self.statement(&statement.body);
                let test = self.expression(&statement.test, NameRole::Reference);
                self.finish(statement.span, IrNodeData::DoWhileStatement { body, test })
            }
            Statement::Switch(statement) => {
                let discriminant = self.expression(&statement.discriminant, NameRole::Reference);
                let cases = statement
                    .cases
                    .iter()
                    .map(|case| {
                        let test = case
                            .test
                            .as_ref()
                            .map(|test| self.expression(test, NameRole::Reference));
                        let consequent = case
                            .consequent
                            .iter()
                            .map(|item| self.statement(item))
                            .collect();
                        let parent = self.next_node();
                        let consequent =
                            self.list_for(parent, ChildRole::SwitchCaseBody, consequent);
                        self.finish(case.span, IrNodeData::SwitchCase { test, consequent })
                    })
                    .collect();
                let parent = self.next_node();
                let cases = self.list_for(parent, ChildRole::SwitchCases, cases);
                self.finish(
                    statement.span,
                    IrNodeData::SwitchStatement {
                        discriminant,
                        cases,
                    },
                )
            }
            Statement::Return(statement) => {
                let argument = statement
                    .argument
                    .as_ref()
                    .map(|argument| self.expression(argument, NameRole::Reference));
                self.finish(statement.span, IrNodeData::ReturnStatement { argument })
            }
            Statement::Break(statement) => {
                let label = statement
                    .label
                    .map(|label| self.ident(label, NameRole::LabelReference));
                self.finish(statement.span, IrNodeData::BreakStatement { label })
            }
            Statement::Continue(statement) => {
                let label = statement
                    .label
                    .map(|label| self.ident(label, NameRole::LabelReference));
                self.finish(statement.span, IrNodeData::ContinueStatement { label })
            }
            Statement::Throw(statement) => {
                let argument = self.expression(&statement.argument, NameRole::Reference);
                self.finish(statement.span, IrNodeData::ThrowStatement { argument })
            }
            Statement::Try(statement) => {
                let block = self.block_statement(statement.block);
                let handler = statement.handler.map(|handler| {
                    let parameter = handler.param.as_ref().map(|param| self.pattern(param));
                    let body = self.block_statement(handler.body);
                    self.finish(handler.span, IrNodeData::CatchClause { parameter, body })
                });
                let finalizer = statement
                    .finalizer
                    .map(|finalizer| self.block_statement(finalizer));
                self.finish(
                    statement.span,
                    IrNodeData::TryStatement {
                        block,
                        handler,
                        finalizer,
                    },
                )
            }
            Statement::Labeled(statement) => {
                let label = self.ident(statement.label, NameRole::LabelDeclaration);
                let body = self.statement(&statement.body);
                self.finish(statement.span, IrNodeData::LabeledStatement { label, body })
            }
            Statement::With(statement) => {
                let object = self.expression(&statement.object, NameRole::Reference);
                let body = self.statement(&statement.body);
                self.finish(statement.span, IrNodeData::WithStatement { object, body })
            }
            Statement::Import(declaration) => {
                let specifiers = declaration
                    .specifiers
                    .iter()
                    .map(|specifier| self.import_specifier(*specifier))
                    .collect();
                let attributes = declaration
                    .attributes
                    .map(|attributes| self.import_attributes(attributes));
                let source = self.name_text_with_origin(
                    self.interner.resolve(declaration.source),
                    declaration.span,
                    NameRole::ModuleSpecifier,
                    NameSyntax::String,
                    IrOrigin::parser_derived(declaration.span),
                );
                let parent = self.next_node();
                let specifiers = self.list_for(parent, ChildRole::ImportSpecifiers, specifiers);
                self.finish(
                    declaration.span,
                    IrNodeData::ImportDeclaration {
                        specifiers,
                        source,
                        attributes,
                    },
                )
            }
            Statement::ExportNamed(declaration) => {
                let declaration_node = declaration
                    .declaration
                    .as_ref()
                    .map(|declaration| self.statement(declaration));
                let has_source = declaration.source.is_some();
                let specifiers = declaration
                    .specifiers
                    .iter()
                    .map(|specifier| {
                        let local_role = if has_source {
                            NameRole::ImportName
                        } else {
                            NameRole::ExportLocal
                        };
                        let local = self.module_name(specifier.local, specifier.span, local_role);
                        let exported = self.module_name(
                            specifier.exported,
                            specifier.span,
                            NameRole::ExportedName,
                        );
                        self.finish(
                            specifier.span,
                            IrNodeData::ExportSpecifier { local, exported },
                        )
                    })
                    .collect();
                let attributes = declaration
                    .attributes
                    .map(|attributes| self.import_attributes(attributes));
                let source = declaration.source.map(|source| {
                    self.name_text_with_origin(
                        self.interner.resolve(source),
                        declaration.span,
                        NameRole::ModuleSpecifier,
                        NameSyntax::String,
                        IrOrigin::parser_derived(declaration.span),
                    )
                });
                let parent = self.next_node();
                let specifiers = self.list_for(parent, ChildRole::ExportSpecifiers, specifiers);
                self.finish(
                    declaration.span,
                    IrNodeData::ExportNamedDeclaration {
                        declaration: declaration_node,
                        specifiers,
                        source,
                        attributes,
                    },
                )
            }
            Statement::ExportDefault(declaration) => {
                let (value, kind) = match declaration.declaration {
                    ExportDefaultKind::Function(function) => (
                        self.function(function, FunctionContext::ExportDefault),
                        ExportDefaultValueKind::Function,
                    ),
                    ExportDefaultKind::Class(class) => (
                        self.class(class, ClassContext::ExportDefault),
                        ExportDefaultValueKind::Class,
                    ),
                    ExportDefaultKind::Expression(expression) => (
                        self.expression(&expression, NameRole::Reference),
                        ExportDefaultValueKind::Expression,
                    ),
                };
                self.finish(
                    declaration.span,
                    IrNodeData::ExportDefaultDeclaration { value, kind },
                )
            }
            Statement::ExportAll(declaration) => {
                let exported = declaration.exported.map(|exported| {
                    self.module_name(exported, declaration.span, NameRole::ExportedName)
                });
                let attributes = declaration
                    .attributes
                    .map(|attributes| self.import_attributes(attributes));
                let source = self.name_text_with_origin(
                    self.interner.resolve(declaration.source),
                    declaration.span,
                    NameRole::ModuleSpecifier,
                    NameSyntax::String,
                    IrOrigin::parser_derived(declaration.span),
                );
                self.finish(
                    declaration.span,
                    IrNodeData::ExportAllDeclaration {
                        exported,
                        source,
                        attributes,
                    },
                )
            }
        }
    }

    fn block_statement(&mut self, block: &wake_ecma_ast::BlockStatement<'_>) -> NodeId {
        let body = block.body.iter().map(|item| self.statement(item)).collect();
        let parent = self.next_node();
        let body = self.list_for(parent, ChildRole::BlockBody, body);
        self.finish(block.span, IrNodeData::Block { body })
    }

    fn variable_declaration(
        &mut self,
        declaration: &wake_ecma_ast::VariableDeclaration<'_>,
    ) -> NodeId {
        let declarations = declaration
            .declarations
            .iter()
            .map(|declarator| {
                let binding = self.pattern(&declarator.id);
                let initializer = declarator
                    .init
                    .as_ref()
                    .map(|init| self.expression(init, NameRole::Reference));
                self.finish(
                    declarator.span,
                    IrNodeData::VariableDeclarator {
                        binding,
                        initializer,
                    },
                )
            })
            .collect();
        let parent = self.next_node();
        let declarations = self.list_for(parent, ChildRole::DeclarationItems, declarations);
        self.finish(
            declaration.span,
            IrNodeData::VariableDeclaration {
                kind: declaration.kind,
                declarations,
            },
        )
    }

    fn for_left(&mut self, left: ForLeft<'_>) -> (NodeId, ForLeftKind) {
        match left {
            ForLeft::Variable(declaration) => (
                self.variable_declaration(declaration),
                ForLeftKind::Variable,
            ),
            ForLeft::Target(target) => (
                self.expression(&target, NameRole::AssignmentTarget),
                ForLeftKind::Target,
            ),
        }
    }

    fn function(&mut self, function: &Function<'_>, context: FunctionContext) -> NodeId {
        let name = function
            .id
            .map(|name| self.ident(name, NameRole::FunctionName));
        let parameters = function
            .params
            .iter()
            .map(|param| self.pattern(param))
            .collect();
        let body = function.body.map(|body| {
            let statements = body
                .statements
                .iter()
                .map(|statement| self.statement(statement))
                .collect();
            let parent = self.next_node();
            let statements = self.list_for(parent, ChildRole::FunctionStatements, statements);
            self.finish(
                body.span,
                IrNodeData::FunctionBody {
                    statements,
                    strict: body.strict,
                },
            )
        });
        let parent = self.next_node();
        let parameters = self.list_for(parent, ChildRole::FunctionParameters, parameters);
        self.finish(
            function.span,
            IrNodeData::Function {
                context,
                name,
                parameters,
                body,
                is_async: function.is_async,
                is_generator: function.is_generator,
            },
        )
    }

    fn class(&mut self, class: &Class<'_>, context: ClassContext) -> NodeId {
        let name = class.id.map(|name| self.ident(name, NameRole::ClassName));
        let super_class = class
            .super_class
            .as_ref()
            .map(|super_class| self.expression(super_class, NameRole::Reference));
        let members = class
            .body
            .iter()
            .map(|member| self.class_member(member))
            .collect();
        let decorators = class
            .decorators
            .iter()
            .map(|decorator| self.expression(decorator, NameRole::Reference))
            .collect();
        let parent = self.next_node();
        let members = self.list_for(parent, ChildRole::ClassMembers, members);
        let decorators = self.list_for(parent, ChildRole::Decorators, decorators);
        self.finish(
            class.span,
            IrNodeData::Class {
                context,
                name,
                super_class,
                members,
                decorators,
            },
        )
    }

    fn class_member(&mut self, member: &ClassMember<'_>) -> NodeId {
        match member {
            ClassMember::Method(method) => {
                let key = self.property_key(method.key, NameRole::Property);
                let value = self.function(method.value, FunctionContext::Method);
                let decorators = method
                    .decorators
                    .iter()
                    .map(|decorator| self.expression(decorator, NameRole::Reference))
                    .collect();
                let parent = self.next_node();
                let decorators = self.list_for(parent, ChildRole::Decorators, decorators);
                self.finish(
                    method.span,
                    IrNodeData::MethodDefinition {
                        key,
                        value,
                        kind: method.kind,
                        is_static: method.is_static,
                        computed: method.computed,
                        decorators,
                    },
                )
            }
            ClassMember::Property(property) => {
                let key = self.property_key(property.key, NameRole::Property);
                let value = property
                    .value
                    .as_ref()
                    .map(|value| self.expression(value, NameRole::Reference));
                let decorators = property
                    .decorators
                    .iter()
                    .map(|decorator| self.expression(decorator, NameRole::Reference))
                    .collect();
                let parent = self.next_node();
                let decorators = self.list_for(parent, ChildRole::Decorators, decorators);
                self.finish(
                    property.span,
                    IrNodeData::PropertyDefinition {
                        key,
                        value,
                        is_static: property.is_static,
                        computed: property.computed,
                        decorators,
                        accessor: property.accessor,
                    },
                )
            }
            ClassMember::StaticBlock(block) => {
                let body = block.body.iter().map(|item| self.statement(item)).collect();
                let parent = self.next_node();
                let body = self.list_for(parent, ChildRole::StaticBlockBody, body);
                self.finish(block.span, IrNodeData::StaticBlock { body })
            }
        }
    }

    fn expression(&mut self, expression: &Expression<'_>, name_role: NameRole) -> NodeId {
        match expression {
            Expression::NumberLiteral(literal) => self.finish(
                literal.span,
                IrNodeData::NumberLiteral {
                    value: literal.value,
                },
            ),
            Expression::StringLiteral(literal) => self.finish(
                literal.span,
                IrNodeData::StringLiteral {
                    value: self.interner.resolve(literal.value),
                },
            ),
            Expression::BooleanLiteral(literal) => self.finish(
                literal.span,
                IrNodeData::BooleanLiteral {
                    value: literal.value,
                },
            ),
            Expression::NullLiteral(span) => self.finish(*span, IrNodeData::NullLiteral),
            Expression::BigIntLiteral(literal) => self.finish(
                literal.span,
                IrNodeData::BigIntLiteral {
                    raw: self.interner.resolve(literal.raw),
                },
            ),
            Expression::RegExpLiteral(literal) => self.finish(
                literal.span,
                IrNodeData::RegExpLiteral {
                    pattern: self.interner.resolve(literal.pattern),
                    flags: self.interner.resolve(literal.flags),
                },
            ),
            Expression::TemplateLiteral(template) => self.template_literal(template),
            Expression::Identifier(identifier) => {
                let name = self.ident(**identifier, name_role);
                self.finish(identifier.span, IrNodeData::Identifier { name })
            }
            Expression::This(span) => self.finish(*span, IrNodeData::ThisExpression),
            Expression::Super(span) => self.finish(*span, IrNodeData::SuperExpression),
            Expression::MetaProperty(meta) => {
                let meta_name = self.name_text_with_origin(
                    self.interner.resolve(meta.meta),
                    meta.span,
                    NameRole::MetaKeyword,
                    NameSyntax::Keyword,
                    IrOrigin::parser_derived(meta.span),
                );
                let property = self.name_text_with_origin(
                    self.interner.resolve(meta.property),
                    meta.span,
                    NameRole::MetaProperty,
                    NameSyntax::Identifier,
                    IrOrigin::parser_derived(meta.span),
                );
                self.finish(
                    meta.span,
                    IrNodeData::MetaProperty {
                        meta: meta_name,
                        property,
                    },
                )
            }
            Expression::Array(array) => {
                let elements = array
                    .elements
                    .iter()
                    .map(|element| match element {
                        Some(element) => self.expression(element, name_role),
                        None => self.finish_with_origin(
                            IrOrigin::parser_derived(array.span),
                            IrNodeData::Elision,
                        ),
                    })
                    .collect();
                let parent = self.next_node();
                let elements = self.list_for(parent, ChildRole::ArrayElements, elements);
                self.finish(array.span, IrNodeData::ArrayExpression { elements })
            }
            Expression::Object(object) => {
                let members = object
                    .properties
                    .iter()
                    .map(|member| match member {
                        ObjectMember::Property(property) => {
                            let key = self.property_key(property.key, NameRole::Property);
                            let value = self.expression(&property.value, name_role);
                            self.finish(
                                property.span,
                                IrNodeData::ObjectProperty {
                                    key,
                                    value,
                                    kind: property.kind,
                                    method: property.method,
                                    shorthand: property.shorthand,
                                    computed: property.computed,
                                    prototype_setter: property.prototype_setter,
                                },
                            )
                        }
                        ObjectMember::Spread(spread) => {
                            let argument = self.expression(&spread.argument, name_role);
                            self.finish(spread.span, IrNodeData::SpreadElement { argument })
                        }
                    })
                    .collect();
                let parent = self.next_node();
                let members = self.list_for(parent, ChildRole::ObjectMembers, members);
                self.finish(object.span, IrNodeData::ObjectExpression { members })
            }
            Expression::Function(function) => self.function(function, FunctionContext::Expression),
            Expression::Arrow(arrow) => {
                let parameters = arrow
                    .params
                    .iter()
                    .map(|param| self.pattern(param))
                    .collect();
                let (body, body_kind) = match arrow.body {
                    ArrowBody::Block(body) => {
                        let statements = body
                            .statements
                            .iter()
                            .map(|statement| self.statement(statement))
                            .collect();
                        let parent = self.next_node();
                        let statements =
                            self.list_for(parent, ChildRole::FunctionStatements, statements);
                        (
                            self.finish(
                                body.span,
                                IrNodeData::FunctionBody {
                                    statements,
                                    strict: body.strict,
                                },
                            ),
                            ArrowBodyKind::Block,
                        )
                    }
                    ArrowBody::Expression(body) => (
                        self.expression(&body, NameRole::Reference),
                        ArrowBodyKind::Expression,
                    ),
                };
                let parent = self.next_node();
                let parameters = self.list_for(parent, ChildRole::FunctionParameters, parameters);
                self.finish(
                    arrow.span,
                    IrNodeData::ArrowFunction {
                        parameters,
                        body,
                        body_kind,
                        is_async: arrow.is_async,
                    },
                )
            }
            Expression::Class(class) => self.class(class, ClassContext::Expression),
            Expression::Unary(unary) => {
                let argument = self.expression(&unary.argument, NameRole::Reference);
                self.finish(
                    unary.span,
                    IrNodeData::UnaryExpression {
                        operator: unary.operator,
                        argument,
                    },
                )
            }
            Expression::Update(update) => {
                let argument = self.expression(&update.argument, NameRole::AssignmentTarget);
                self.finish(
                    update.span,
                    IrNodeData::UpdateExpression {
                        operator: update.operator,
                        prefix: update.prefix,
                        argument,
                    },
                )
            }
            Expression::Binary(binary) => {
                let left = self.expression(&binary.left, NameRole::Reference);
                let right = self.expression(&binary.right, NameRole::Reference);
                self.finish(
                    binary.span,
                    IrNodeData::BinaryExpression {
                        operator: binary.operator,
                        left,
                        right,
                    },
                )
            }
            Expression::Logical(logical) => {
                let left = self.expression(&logical.left, NameRole::Reference);
                let right = self.expression(&logical.right, NameRole::Reference);
                self.finish(
                    logical.span,
                    IrNodeData::LogicalExpression {
                        operator: logical.operator,
                        left,
                        right,
                    },
                )
            }
            Expression::Assignment(assignment) => {
                let left = self.expression(&assignment.left, NameRole::AssignmentTarget);
                let right = self.expression(&assignment.right, NameRole::Reference);
                self.finish(
                    assignment.span,
                    IrNodeData::AssignmentExpression {
                        operator: assignment.operator,
                        left,
                        right,
                    },
                )
            }
            Expression::Conditional(conditional) => {
                let test = self.expression(&conditional.test, NameRole::Reference);
                let consequent = self.expression(&conditional.consequent, NameRole::Reference);
                let alternate = self.expression(&conditional.alternate, NameRole::Reference);
                self.finish(
                    conditional.span,
                    IrNodeData::ConditionalExpression {
                        test,
                        consequent,
                        alternate,
                    },
                )
            }
            Expression::Call(call) => {
                let callee = self.expression(&call.callee, NameRole::Reference);
                let arguments = call
                    .arguments
                    .iter()
                    .map(|argument| self.expression(argument, NameRole::Reference))
                    .collect();
                let parent = self.next_node();
                let arguments = self.list_for(parent, ChildRole::Arguments, arguments);
                self.finish(
                    call.span,
                    IrNodeData::CallExpression {
                        callee,
                        arguments,
                        optional: call.optional,
                    },
                )
            }
            Expression::New(new_expression) => {
                let callee = self.expression(&new_expression.callee, NameRole::Reference);
                let arguments = new_expression
                    .arguments
                    .iter()
                    .map(|argument| self.expression(argument, NameRole::Reference))
                    .collect();
                let parent = self.next_node();
                let arguments = self.list_for(parent, ChildRole::Arguments, arguments);
                self.finish(
                    new_expression.span,
                    IrNodeData::NewExpression { callee, arguments },
                )
            }
            Expression::Member(member) => {
                let object = self.expression(&member.object, NameRole::Reference);
                let (property, property_kind) = match member.property {
                    MemberProperty::Ident(identifier) => (
                        self.ident(identifier, NameRole::Property),
                        PropertyKeyKind::Identifier,
                    ),
                    MemberProperty::Computed(expression) => (
                        self.expression(&expression, NameRole::Reference),
                        PropertyKeyKind::Computed,
                    ),
                    MemberProperty::Private(identifier) => (
                        self.ident(identifier, NameRole::PrivateProperty),
                        PropertyKeyKind::Private,
                    ),
                };
                self.finish(
                    member.span,
                    IrNodeData::MemberExpression {
                        object,
                        property,
                        property_kind,
                        optional: member.optional,
                    },
                )
            }
            Expression::Sequence(sequence) => {
                let expressions = sequence
                    .expressions
                    .iter()
                    .map(|expression| self.expression(expression, NameRole::Reference))
                    .collect();
                let parent = self.next_node();
                let expressions = self.list_for(parent, ChildRole::SequenceItems, expressions);
                self.finish(
                    sequence.span,
                    IrNodeData::SequenceExpression { expressions },
                )
            }
            Expression::TaggedTemplate(tagged) => {
                let tag = self.expression(&tagged.tag, NameRole::Reference);
                let quasi = self.template_literal(tagged.quasi);
                self.finish(
                    tagged.span,
                    IrNodeData::TaggedTemplateExpression { tag, quasi },
                )
            }
            Expression::Spread(spread) => {
                let argument = self.expression(&spread.argument, NameRole::Reference);
                self.finish(spread.span, IrNodeData::SpreadElement { argument })
            }
            Expression::Await(await_expression) => {
                let argument = self.expression(&await_expression.argument, NameRole::Reference);
                self.finish(
                    await_expression.span,
                    IrNodeData::AwaitExpression { argument },
                )
            }
            Expression::Yield(yield_expression) => {
                let argument = yield_expression
                    .argument
                    .as_ref()
                    .map(|argument| self.expression(argument, NameRole::Reference));
                self.finish(
                    yield_expression.span,
                    IrNodeData::YieldExpression {
                        argument,
                        delegate: yield_expression.delegate,
                    },
                )
            }
            Expression::Import(import_expression) => {
                let source = self.expression(&import_expression.source, NameRole::Reference);
                let options = import_expression
                    .options
                    .as_ref()
                    .map(|options| self.expression(options, NameRole::Reference));
                self.finish(
                    import_expression.span,
                    IrNodeData::ImportExpression { source, options },
                )
            }
        }
    }

    fn template_literal(&mut self, template: &wake_ecma_ast::TemplateLiteral<'_>) -> NodeId {
        let quasis = template
            .quasis
            .iter()
            .map(|quasi| {
                self.finish(
                    quasi.span,
                    IrNodeData::TemplateElement {
                        cooked: quasi.cooked.map(|cooked| self.interner.resolve(cooked)),
                        raw: self.interner.resolve(quasi.raw),
                        tail: quasi.tail,
                    },
                )
            })
            .collect();
        let expressions = template
            .expressions
            .iter()
            .map(|expression| self.expression(expression, NameRole::Reference))
            .collect();
        let parent = self.next_node();
        let quasis = self.list_for(parent, ChildRole::TemplateQuasis, quasis);
        let expressions = self.list_for(parent, ChildRole::TemplateExpressions, expressions);
        self.finish(
            template.span,
            IrNodeData::TemplateLiteral {
                quasis,
                expressions,
            },
        )
    }

    fn pattern(&mut self, pattern: &Pattern<'_>) -> NodeId {
        match pattern {
            Pattern::Ident(identifier) => {
                let name = self.ident(**identifier, NameRole::Binding);
                self.finish(identifier.span, IrNodeData::Identifier { name })
            }
            Pattern::Array(array) => {
                let elements = array
                    .elements
                    .iter()
                    .map(|element| match element {
                        Some(element) => self.pattern(element),
                        None => self.finish_with_origin(
                            IrOrigin::parser_derived(array.span),
                            IrNodeData::Elision,
                        ),
                    })
                    .collect();
                let parent = self.next_node();
                let elements = self.list_for(parent, ChildRole::PatternElements, elements);
                self.finish(array.span, IrNodeData::ArrayPattern { elements })
            }
            Pattern::Object(object) => {
                let properties = object
                    .properties
                    .iter()
                    .map(|property| {
                        let key = self.property_key(property.key, NameRole::Property);
                        let value = self.pattern(&property.value);
                        self.finish(
                            property.span,
                            IrNodeData::ObjectPatternProperty {
                                key,
                                value,
                                shorthand: property.shorthand,
                                computed: property.computed,
                            },
                        )
                    })
                    .collect();
                let rest = object.rest.map(|rest| {
                    let argument = self.pattern(&rest.argument);
                    self.finish(rest.span, IrNodeData::RestPattern { argument })
                });
                let parent = self.next_node();
                let properties = self.list_for(parent, ChildRole::PatternProperties, properties);
                self.finish(object.span, IrNodeData::ObjectPattern { properties, rest })
            }
            Pattern::Assignment(assignment) => {
                let left = self.pattern(&assignment.left);
                let right = self.expression(&assignment.right, NameRole::Reference);
                self.finish(
                    assignment.span,
                    IrNodeData::AssignmentPattern { left, right },
                )
            }
            Pattern::Rest(rest) => {
                let argument = self.pattern(&rest.argument);
                self.finish(rest.span, IrNodeData::RestPattern { argument })
            }
        }
    }

    fn property_key(&mut self, key: PropertyKey<'_>, role: NameRole) -> IrPropertyKey {
        match key {
            PropertyKey::Ident(identifier) => IrPropertyKey {
                kind: PropertyKeyKind::Identifier,
                value: self.ident(identifier, role),
            },
            PropertyKey::String(literal) => IrPropertyKey {
                kind: PropertyKeyKind::String,
                value: self.name_text(
                    self.interner.resolve(literal.value),
                    literal.span,
                    role,
                    NameSyntax::String,
                ),
            },
            PropertyKey::Number(literal) => IrPropertyKey {
                kind: PropertyKeyKind::Number,
                value: self.finish(
                    literal.span,
                    IrNodeData::NumberLiteral {
                        value: literal.value,
                    },
                ),
            },
            PropertyKey::Computed(expression) => IrPropertyKey {
                kind: PropertyKeyKind::Computed,
                value: self.expression(&expression, NameRole::Reference),
            },
            PropertyKey::Private(identifier) => IrPropertyKey {
                kind: PropertyKeyKind::Private,
                value: self.ident(identifier, NameRole::PrivateProperty),
            },
        }
    }

    fn module_name(
        &mut self,
        name: ModuleExportName,
        fallback_span: Span,
        role: NameRole,
    ) -> IrModuleName {
        match name {
            ModuleExportName::Ident(identifier) => IrModuleName {
                kind: ModuleNameKind::Identifier,
                value: self.ident(identifier, role),
            },
            ModuleExportName::String(name) => IrModuleName {
                kind: ModuleNameKind::String,
                value: self.name_text_with_origin(
                    self.interner.resolve(name),
                    fallback_span,
                    role,
                    NameSyntax::String,
                    IrOrigin::parser_derived(fallback_span),
                ),
            },
        }
    }

    fn import_specifier(&mut self, specifier: ImportSpecifier) -> NodeId {
        match specifier {
            ImportSpecifier::Named {
                span,
                imported,
                local,
            } => {
                let imported = Some(self.module_name(imported, span, NameRole::ImportName));
                let local = self.ident(local, NameRole::ImportBinding);
                self.finish(
                    span,
                    IrNodeData::ImportSpecifier {
                        kind: ImportSpecifierKind::Named,
                        imported,
                        local,
                    },
                )
            }
            ImportSpecifier::Default { span, local } => {
                let local = self.ident(local, NameRole::ImportBinding);
                self.finish(
                    span,
                    IrNodeData::ImportSpecifier {
                        kind: ImportSpecifierKind::Default,
                        imported: None,
                        local,
                    },
                )
            }
            ImportSpecifier::Namespace { span, local } => {
                let local = self.ident(local, NameRole::ImportBinding);
                self.finish(
                    span,
                    IrNodeData::ImportSpecifier {
                        kind: ImportSpecifierKind::Namespace,
                        imported: None,
                        local,
                    },
                )
            }
        }
    }

    fn import_attributes(&mut self, attributes: &ImportAttributes<'_>) -> NodeId {
        let items = attributes
            .items
            .iter()
            .map(|attribute| {
                let key = self.module_name(attribute.key, attribute.span, NameRole::AttributeKey);
                let value = self.finish_with_origin(
                    IrOrigin::parser_derived(attribute.span),
                    IrNodeData::StringLiteral {
                        value: self.interner.resolve(attribute.value),
                    },
                );
                self.finish(attribute.span, IrNodeData::ImportAttribute { key, value })
            })
            .collect();
        let parent = self.next_node();
        let items = self.list_for(parent, ChildRole::AttributeItems, items);
        self.finish(
            attributes.span,
            IrNodeData::ImportAttributes {
                keyword: attributes.keyword,
                items,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use wake_ecma_ast::SourceType;

    fn lower_source(source: &str, source_type: SourceType) -> TypedProgram {
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

    fn root_body(ir: &TypedProgram) -> ListId {
        match ir.node(ir.root()).unwrap().data() {
            IrNodeData::Program { body, .. } => *body,
            other => panic!("expected Program root, found {other:?}"),
        }
    }

    #[test]
    fn analyzed_constructor_matches_explicit_semantic_lowering() {
        let source = "function outer(value){const local=value+1;return()=>local}outer(2);";
        let interner = Interner::new();
        let parsed = wake_ecma_parser::parse(source, &interner, SourceType::Script);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let semantic = wake_ecma_semantic::analyze(program);
            let explicit = TypedProgram::lower(program, &interner, Some(&semantic)).unwrap();
            let analyzed = TypedProgram::lower_analyzed(program, &interner).unwrap();
            assert_eq!(analyzed, explicit);
            assert_eq!(analyzed.fingerprint(), explicit.fingerprint());
            assert!(analyzed.names().iter().any(|name| name.symbol().is_some()));
            assert_eq!(analyzed.symbols(), explicit.symbols());
        });
    }

    #[test]
    fn lowers_javascript_typescript_jsx_and_tsx_to_owned_typed_nodes() {
        let fixtures = [
            (
                SourceType::Module,
                r#"
import primary, { value as renamed } from "./dep.js";
export { renamed as visible };
export * as everything from "./more.js";
const key = "field";
const object = { __proto__: null, [key]: 1, get value(){ return 2 }, ...primary };
function* generate(input = 1, ...rest) { yield* rest; return input; }
export default class Example extends primary.Base {
  #secret = 1;
  static field = 2;
  static { this.ready = true; }
  method(optional) { return optional?.[key] ?? object.value; }
}
void generate;
"#,
            ),
            (
                SourceType::TypeScript,
                r#"
interface Shape { value: number }
enum Kind { First = 1, Second }
namespace Bag { export const value: number = Kind.Second; }
class Point { constructor(public x: number, private y = 2) {} }
export const result = new Point(Bag.value).x satisfies number;
"#,
            ),
            (
                SourceType::Jsx,
                r#"
const value = 3;
export default <section data-value={value}><span>{value}</span>{...[]}</section>;
"#,
            ),
            (
                SourceType::Tsx,
                r#"
type Props = { title: string };
const Card = ({ title }: Props) => <article><h1>{title}</h1></article>;
export default <Card title={"owned" as string} />;
"#,
            ),
        ];

        for (source_type, source) in fixtures {
            let ir = lower_source(source, source_type);
            ir.validate().unwrap();
            assert!(ir.nodes().len() > 4, "{source_type:?} did not lower a tree");
            match ir.node(ir.root()).unwrap().data() {
                IrNodeData::Program {
                    source_type: actual,
                    ..
                } => assert_eq!(*actual, source_type),
                other => panic!("expected Program root, found {other:?}"),
            }
            assert!(
                ir.nodes().iter().all(|node| !node.is_tombstone()),
                "fresh lowering must contain only live syntax"
            );
        }
    }

    #[test]
    fn lowers_every_object_binding_property_form_without_losing_shorthand_syntax() {
        let ir = lower_source(
            "const {short,withDefault=1,key:alias,other:renamed=2,...rest}=source;",
            SourceType::Script,
        );
        ir.validate().unwrap();

        let object = ir
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| match ir.node(node)?.data() {
                IrNodeData::ObjectPattern { properties, rest } => Some((*properties, *rest)),
                _ => None,
            })
            .expect("fixture must contain one object binding pattern");
        let properties = ir.list(object.0).unwrap().items();
        assert_eq!(properties.len(), 4);
        assert!(
            object.1.is_some(),
            "object rest must remain structurally owned"
        );

        let shapes = properties
            .iter()
            .map(|property| match ir.node(*property).unwrap().data() {
                IrNodeData::ObjectPatternProperty {
                    value, shorthand, ..
                } => (*shorthand, ir.node(*value).unwrap().data().category()),
                other => panic!("expected object-pattern property, found {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            shapes,
            vec![
                (true, NodeCategory::Identifier),
                (true, NodeCategory::Pattern),
                (false, NodeCategory::Identifier),
                (false, NodeCategory::Pattern),
            ]
        );
        assert!(matches!(
            ir.node(object.1.unwrap()).unwrap().data(),
            IrNodeData::RestPattern { .. }
        ));
    }

    #[test]
    fn shared_and_dummy_spans_still_produce_distinct_occurrence_ids() {
        let ir = lower_source(
            r#"
interface Gone { value: number }
class Box { constructor(public value: number) {} }
const view = <Box value={1} />;
export default view;
"#,
            SourceType::Tsx,
        );

        let unique: HashSet<_> = ir.nodes().iter().map(IrNode::id).collect();
        assert_eq!(unique.len(), ir.nodes().len());

        let dummy_nodes: Vec<_> = ir
            .nodes()
            .iter()
            .filter(|node| {
                matches!(
                    node.origin(),
                    IrOrigin::Derived {
                        kind: DerivedOriginKind::ParserLowering,
                        ..
                    }
                )
            })
            .map(IrNode::id)
            .collect();
        assert!(
            dummy_nodes.len() >= 2,
            "TSX lowering should create several DUMMY-span occurrences: {dummy_nodes:?}"
        );
        assert_eq!(
            dummy_nodes.iter().copied().collect::<HashSet<_>>().len(),
            dummy_nodes.len()
        );

        let mut by_span: HashMap<(u32, u32), Vec<NodeId>> = HashMap::new();
        for node in ir.nodes() {
            if let IrOrigin::Source(span) = node.origin() {
                by_span
                    .entry((span.lo, span.hi))
                    .or_default()
                    .push(node.id());
            }
        }
        let shared = by_span
            .values()
            .find(|nodes| nodes.len() >= 2)
            .expect("parent/child or lowered occurrences should share a real source span");
        assert_eq!(
            shared.iter().copied().collect::<HashSet<_>>().len(),
            shared.len()
        );

        // DUMMY occurrence names are intentionally unbound: a span collision must never attach
        // an arbitrary semantic symbol to every generated occurrence.
        for node in ir.nodes().iter().filter(|node| {
            matches!(
                node.origin(),
                IrOrigin::Derived {
                    kind: DerivedOriginKind::ParserLowering,
                    ..
                }
            )
        }) {
            if let IrNodeData::Name { name } = node.data() {
                assert_eq!(ir.name(*name).unwrap().symbol(), None);
            }
        }
    }

    #[test]
    fn owns_every_string_and_survives_module_ast_and_interner_drop() {
        let ir = {
            let source = r#"
import thing from "./owned-dependency.js";
const label = `owned-${thing}`;
export { label as "public-owned-name" };
"#;
            let interner = Interner::new();
            let parsed = wake_ecma_parser::parse(source, &interner, SourceType::Module);
            assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
            parsed.module.with_ast(|program| {
                let semantic = wake_ecma_semantic::analyze(program);
                TypedProgram::lower(program, &interner, Some(&semantic)).unwrap()
            })
            // `parsed.module` and `interner` are released at block exit.
        };

        let first_walk = ir.preorder().unwrap();
        let first_fingerprint = ir.fingerprint();
        let second_walk = ir.preorder().unwrap();
        assert_eq!(first_walk, second_walk);
        assert_eq!(first_fingerprint, ir.fingerprint());
        assert!(
            ir.names()
                .iter()
                .any(|name| name.original() == "./owned-dependency.js")
        );
        assert!(
            ir.names()
                .iter()
                .any(|name| name.original() == "public-owned-name")
        );
        assert!(ir.nodes().iter().any(|node| {
            matches!(node.data(), IrNodeData::TemplateElement { raw, .. } if raw.contains("owned-"))
        }));
    }

    #[test]
    fn lowering_is_pointer_independent_and_fingerprint_stable() {
        let source = "const value={answer:42};export default value.answer;";
        let first = lower_source(source, SourceType::Module);
        let second = lower_source(source, SourceType::Module);
        assert_eq!(first.preorder().unwrap(), second.preorder().unwrap());
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_eq!(first, second);
    }

    #[test]
    fn structural_splice_and_replace_rewrite_the_real_parent_list() {
        let mut ir = lower_source("first();second();third();", SourceType::Script);
        let body = root_body(&ir);
        let before = ir.list(body).unwrap().items().to_vec();
        assert_eq!(before.len(), 3);

        let inserted = ir
            .append_detached_leaf(
                IrNodeData::DebuggerStatement,
                IrOrigin::Synthetic {
                    anchor: None,
                    kind: SyntheticOriginKind::Optimization,
                },
            )
            .unwrap();
        let removed = ir.splice_list(body, 1..2, &[inserted]).unwrap();
        assert_eq!(removed, vec![before[1]]);
        assert_eq!(
            ir.list(body).unwrap().items(),
            &[before[0], inserted, before[2]]
        );
        assert!(ir.node(before[1]).unwrap().is_tombstone());
        assert_eq!(
            ir.node(inserted).unwrap().parent(),
            Some(ParentLink {
                parent: ir.root(),
                role: ChildRole::ProgramBody,
                list: Some(body),
            })
        );

        let replacement = ir
            .append_detached_leaf(
                IrNodeData::EmptyStatement,
                IrOrigin::Synthetic {
                    anchor: None,
                    kind: SyntheticOriginKind::Optimization,
                },
            )
            .unwrap();
        ir.replace_node(inserted, replacement).unwrap();
        assert_eq!(
            ir.list(body).unwrap().items(),
            &[before[0], replacement, before[2]]
        );
        assert!(ir.node(inserted).unwrap().is_tombstone());
        ir.validate().unwrap();

        let detached = ir
            .append_detached_leaf(
                IrNodeData::NullLiteral,
                IrOrigin::Synthetic {
                    anchor: None,
                    kind: SyntheticOriginKind::External,
                },
            )
            .unwrap();
        ir.tombstone_subtree(detached).unwrap();
        assert!(ir.node(detached).unwrap().is_tombstone());
    }

    #[test]
    fn every_name_occurrence_has_an_independent_name_id() {
        let ir = lower_source(
            "const value=1;const object={value};export {value};object.value;",
            SourceType::Module,
        );
        let name_nodes: Vec<_> = ir
            .nodes()
            .iter()
            .filter_map(|node| match node.data() {
                IrNodeData::Name { name } => Some((node.id(), *name)),
                _ => None,
            })
            .collect();
        assert_eq!(
            name_nodes
                .iter()
                .map(|(_, name)| *name)
                .collect::<HashSet<_>>()
                .len(),
            name_nodes.len()
        );
        assert!(name_nodes.len() >= 6);

        let value_symbols: Vec<_> = name_nodes
            .iter()
            .filter_map(|(_, name)| {
                let name = ir.name(*name).unwrap();
                (name.original() == "value" && name.symbol().is_some()).then_some(name.symbol())
            })
            .collect();
        assert!(value_symbols.len() >= 2);
        assert!(
            value_symbols
                .iter()
                .all(|symbol| *symbol == value_symbols[0])
        );
    }

    #[test]
    fn ambiguous_shared_span_bindings_are_unbound_instead_of_cross_wired() {
        let interner = Interner::new();
        let parsed = wake_ecma_parser::parse(
            "let collision=1;{let collision=2;}",
            &interner,
            SourceType::Script,
        );
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let ir = parsed.module.with_ast(|program| {
            let mut semantic = wake_ecma_semantic::analyze(program);
            assert_eq!(semantic.symbols.len(), 2);
            // Model a lowering which inherited the exact same occurrence span for two distinct
            // symbols. The resolver must treat the coordinate as ambiguous, never choose one.
            let collision_span = semantic.binding_occurrences[0].span;
            for occurrence in &mut semantic.binding_occurrences {
                occurrence.span = collision_span;
            }
            TypedProgram::lower(program, &interner, Some(&semantic)).unwrap()
        });
        let bindings = ir
            .nodes()
            .iter()
            .filter_map(|node| match node.data() {
                IrNodeData::Name { name } => {
                    let name = ir.name(*name).unwrap();
                    (name.original() == "collision" && name.role() == NameRole::Binding)
                        .then_some((node.id(), name.symbol()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(bindings.len(), 2);
        assert_ne!(bindings[0].0, bindings[1].0);
        assert_eq!(bindings[0].1, None);
        assert_eq!(bindings[1].1, None);
    }

    #[test]
    fn standalone_expression_owner_is_owned_and_imports_as_distinct_subtrees() {
        let owner = {
            let interner = Interner::new();
            let parsed = wake_ecma_parser::parse(
                "({message:`owned-${41+1}`,answer:42})",
                &interner,
                SourceType::Script,
            );
            assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
            parsed.module.with_ast(|program| {
                let semantic = wake_ecma_semantic::analyze(program);
                let Statement::Expression(statement) = &program.body[0] else {
                    panic!("standalone expression parser envelope changed")
                };
                lower_expression_owner(&statement.expression, &interner, Some(&semantic)).unwrap()
            })
        };
        owner.validate().unwrap();
        let owner_fingerprint = owner.fingerprint();
        assert_eq!(owner_fingerprint, owner.fingerprint());
        assert!(
            owner
                .names()
                .iter()
                .any(|name| name.original() == "message")
        );

        let mut target = lower_source("globalThis.result=0;", SourceType::Script);
        let first = target.import_expression_owner(&owner).unwrap();
        let second = target.import_expression_owner(&owner).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            std::mem::discriminant(target.node(first).unwrap().data()),
            std::mem::discriminant(target.node(second).unwrap().data())
        );

        let body = root_body(&target);
        let statement = target.list(body).unwrap().items()[0];
        let IrNodeData::ExpressionStatement { expression, .. } =
            target.node(statement).unwrap().data()
        else {
            panic!("expected expression statement")
        };
        let assignment = *expression;
        let IrNodeData::AssignmentExpression { right, .. } =
            target.node(assignment).unwrap().data()
        else {
            panic!("expected assignment expression")
        };
        let old_right = *right;
        target.replace_node(old_right, first).unwrap();
        target.tombstone_subtree(second).unwrap();
        target.validate().unwrap();
        assert!(target.node(old_right).unwrap().is_tombstone());
        assert!(target.node(second).unwrap().is_tombstone());
    }

    #[test]
    fn compound_builder_and_subtree_clone_commit_real_structure_transactionally() {
        let mut ir = lower_source("left();right();", SourceType::Script);
        let body = root_body(&ir);
        let statements = ir.list(body).unwrap().items().to_vec();
        let expression = |ir: &TypedProgram, statement| {
            let IrNodeData::ExpressionStatement { expression, .. } =
                ir.node(statement).unwrap().data()
            else {
                panic!("expected expression statement")
            };
            *expression
        };
        let left = ir
            .clone_detached_subtree(expression(&ir, statements[0]))
            .unwrap();
        let right = ir
            .clone_detached_subtree(expression(&ir, statements[1]))
            .unwrap();
        let sequence = ir
            .append_detached_node_with(
                IrOrigin::Derived {
                    anchor: Some(Span::new(0, 7)),
                    kind: DerivedOriginKind::Optimization,
                },
                |builder| {
                    let expressions = builder.list(ChildRole::SequenceItems, [left, right])?;
                    Ok(IrNodeData::SequenceExpression { expressions })
                },
            )
            .unwrap();
        let old = expression(&ir, statements[0]);
        ir.replace_node(old, sequence).unwrap();
        ir.validate().unwrap();

        let IrNodeData::SequenceExpression { expressions } = ir.node(sequence).unwrap().data()
        else {
            panic!("expected committed sequence")
        };
        assert_eq!(ir.list(*expressions).unwrap().items(), &[left, right]);
        assert_ne!(left, expression(&ir, statements[1]));

        let list_count = ir.lists().len();
        let detached = ir
            .append_detached_leaf(
                IrNodeData::NullLiteral,
                IrOrigin::Synthetic {
                    anchor: None,
                    kind: SyntheticOriginKind::Optimization,
                },
            )
            .unwrap();
        let error = ir
            .append_detached_node_with(
                IrOrigin::Synthetic {
                    anchor: None,
                    kind: SyntheticOriginKind::Optimization,
                },
                |builder| {
                    let _unused = builder.list(ChildRole::ArrayElements, [detached])?;
                    Ok(IrNodeData::NullLiteral)
                },
            )
            .unwrap_err();
        assert!(error.message.contains("not referenced"));
        assert_eq!(ir.lists().len(), list_count);
        assert_eq!(ir.node(detached).unwrap().parent(), None);
        ir.tombstone_subtree(detached).unwrap();
        ir.validate().unwrap();
    }

    #[test]
    fn imported_expression_symbols_are_fresh_and_foreign_spans_are_reanchored() {
        let owner = {
            let interner = Interner::new();
            let parsed = wake_ecma_parser::parse("(value=>value)", &interner, SourceType::Script);
            assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
            parsed.module.with_ast(|program| {
                let semantic = wake_ecma_semantic::analyze(program);
                let Statement::Expression(statement) = &program.body[0] else {
                    panic!("expected standalone expression")
                };
                lower_expression_owner(&statement.expression, &interner, Some(&semantic)).unwrap()
            })
        };
        assert_eq!(owner.symbols().len(), 1);

        let mut target = lower_source("const value=0;globalThis.out=1;", SourceType::Script);
        let host_symbols = target.symbols().len();
        assert_eq!(host_symbols, 1);
        let anchor = Span::new(14, 31);
        let first = target
            .import_expression_owner_at(&owner, anchor, SyntheticOriginKind::TrustedEdit)
            .unwrap();
        let second = target.import_expression_owner(&owner).unwrap();
        assert_eq!(target.symbols().len(), host_symbols + 2);

        let subtree = |ir: &TypedProgram, root: NodeId| {
            let mut nodes = Vec::new();
            let mut stack = vec![root];
            while let Some(node) = stack.pop() {
                nodes.push(node);
                stack.extend(ir.child_ids_unchecked(node));
            }
            nodes
        };
        let first_nodes = subtree(&target, first);
        let first_symbols = first_nodes
            .iter()
            .filter_map(|&node| match target.node(node).unwrap().data() {
                IrNodeData::Name { name } => target.name(*name).unwrap().symbol(),
                _ => None,
            })
            .collect::<HashSet<_>>();
        assert_eq!(first_symbols, HashSet::from([host_symbols as SymbolId]));
        assert!(first_nodes.iter().all(|&node| {
            target.node(node).unwrap().origin()
                == IrOrigin::Synthetic {
                    anchor: Some(anchor),
                    kind: SyntheticOriginKind::TrustedEdit,
                }
        }));

        let second_nodes = subtree(&target, second);
        let second_symbols = second_nodes
            .iter()
            .filter_map(|&node| match target.node(node).unwrap().data() {
                IrNodeData::Name { name } => target.name(*name).unwrap().symbol(),
                _ => None,
            })
            .collect::<HashSet<_>>();
        assert_eq!(
            second_symbols,
            HashSet::from([(host_symbols + 1) as SymbolId])
        );
        assert!(second_nodes.iter().all(|&node| {
            target.node(node).unwrap().origin()
                == IrOrigin::Synthetic {
                    anchor: None,
                    kind: SyntheticOriginKind::External,
                }
        }));

        target.tombstone_subtree(first).unwrap();
        target.tombstone_subtree(second).unwrap();
        target.validate().unwrap();
    }

    #[test]
    fn grammar_invariants_reject_inconsistent_scalar_and_compound_edits() {
        let mut ir = lower_source(
            "const object={field:1};const text=`value`;",
            SourceType::Script,
        );
        let property = ir
            .nodes()
            .iter()
            .find(|node| matches!(node.data(), IrNodeData::ObjectProperty { .. }))
            .map(IrNode::id)
            .unwrap();
        let mut invalid_property = ir.node(property).unwrap().data().clone();
        let IrNodeData::ObjectProperty { computed, .. } = &mut invalid_property else {
            unreachable!()
        };
        *computed = true;
        let error = ir
            .replace_node_data(property, invalid_property)
            .unwrap_err();
        assert!(error.message.contains("property-key"));
        assert!(matches!(
            ir.node(property).unwrap().data(),
            IrNodeData::ObjectProperty {
                computed: false,
                ..
            }
        ));

        let quasi = ir
            .nodes()
            .iter()
            .find(|node| matches!(node.data(), IrNodeData::TemplateElement { .. }))
            .map(IrNode::id)
            .unwrap();
        let mut invalid_quasi = ir.node(quasi).unwrap().data().clone();
        let IrNodeData::TemplateElement { tail, .. } = &mut invalid_quasi else {
            unreachable!()
        };
        *tail = false;
        assert!(
            ir.replace_node_data(quasi, invalid_quasi)
                .unwrap_err()
                .message
                .contains("template tail")
        );

        let local = ir
            .append_detached_name(
                "local",
                NameRole::ImportBinding,
                NameSyntax::Identifier,
                None,
                IrOrigin::Synthetic {
                    anchor: None,
                    kind: SyntheticOriginKind::Optimization,
                },
            )
            .unwrap();
        let node_count = ir.nodes().len();
        let error = ir
            .append_detached_node_with(
                IrOrigin::Synthetic {
                    anchor: None,
                    kind: SyntheticOriginKind::Optimization,
                },
                |_| {
                    Ok(IrNodeData::ImportSpecifier {
                        kind: ImportSpecifierKind::Named,
                        imported: None,
                        local,
                    })
                },
            )
            .unwrap_err();
        assert!(error.message.contains("named import"));
        assert_eq!(ir.nodes().len(), node_count);
        assert_eq!(ir.node(local).unwrap().parent(), None);
        ir.tombstone_subtree(local).unwrap();
        ir.validate().unwrap();
    }

    #[test]
    fn optimizer_created_symbols_and_origin_mutators_are_owned_and_checked() {
        let mut ir = lower_source("let value=1;value;", SourceType::Script);
        let symbol = ir.allocate_symbol("temporary", DeclKind::Let).unwrap();
        let name = ir
            .append_detached_name(
                "temporary",
                NameRole::Binding,
                NameSyntax::Identifier,
                Some(symbol),
                IrOrigin::Synthetic {
                    anchor: None,
                    kind: SyntheticOriginKind::Optimization,
                },
            )
            .unwrap();
        let IrNodeData::Name { name: name_id } = ir.node(name).unwrap().data() else {
            unreachable!()
        };
        assert_eq!(ir.name(*name_id).unwrap().symbol(), Some(symbol));
        assert_eq!(ir.symbol(symbol).unwrap().original_name(), "temporary");
        assert!(ir.set_name_symbol(*name_id, Some(symbol + 1)).is_err());
        let anchor = Span::new(0, 3);
        ir.set_origin(
            name,
            IrOrigin::Derived {
                anchor: Some(anchor),
                kind: DerivedOriginKind::Optimization,
            },
        )
        .unwrap();
        assert_eq!(
            ir.node(name).unwrap().origin(),
            IrOrigin::Derived {
                anchor: Some(anchor),
                kind: DerivedOriginKind::Optimization,
            }
        );
        ir.tombstone_subtree(name).unwrap();
        ir.validate().unwrap();
    }

    #[test]
    fn directive_provenance_distinguishes_parentheses_and_prologue_boundaries() {
        let ir = lower_source(
            r#""use strict";("marker");"later";function run(){"inside";("wrapped");"late";}"#,
            SourceType::Script,
        );
        let statement_directive = |node: NodeId| match ir.node(node).unwrap().data() {
            IrNodeData::ExpressionStatement { directive, .. } => *directive,
            other => panic!("expected expression statement, found {other:?}"),
        };
        let body = root_body(&ir);
        let root = ir.list(body).unwrap().items();
        assert_eq!(
            root[..3]
                .iter()
                .copied()
                .map(statement_directive)
                .collect::<Vec<_>>(),
            vec![true, false, false]
        );

        let IrNodeData::Function {
            body: Some(function_body),
            ..
        } = ir.node(root[3]).unwrap().data()
        else {
            panic!("expected function declaration")
        };
        let IrNodeData::FunctionBody { statements, .. } = ir.node(*function_body).unwrap().data()
        else {
            panic!("expected function body")
        };
        assert_eq!(
            ir.list(*statements)
                .unwrap()
                .items()
                .iter()
                .copied()
                .map(statement_directive)
                .collect::<Vec<_>>(),
            vec![true, false, false]
        );
        ir.validate().unwrap();
    }
}
