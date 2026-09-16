//! Neutral source syntax facts constructed by the parser before erasure/lowering. This data
//! is separate from compilation AST allocation and is not a stable external plugin ABI.

use wake_common::{JsString, Span};

/// A committed statement/declaration terminator, excluding empty statements, for-header
/// separators and type-member separators. Implicit terminators have a zero-width insertion
/// span immediately after the last grammar token, before trailing comments. `can_omit` is a
/// conservative parser proof: false includes uncertain expression-continuation boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceTerminator {
    pub span: Span,
    pub explicit: bool,
    pub can_omit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceListKind {
    Array,
    ArrayPattern,
    Object,
    ObjectPattern,
    Arguments,
    Parameters,
    /// A committed cover that remains an expression, not an arrow parameter list.
    Parenthesized,
    Imports,
    Exports,
    ImportAttributes,
    TypeParameters,
    Tuple,
    Enum,
}

/// Original comma-list grammar. `last` is the final element's last token, before comments;
/// empty lists and trailing array elisions have none. Rest/uncertain cover-spread positions
/// set `can_trail` false. TSX generic arrows may require their comma for disambiguation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceList {
    pub kind: SourceListKind,
    pub open: Span,
    pub close: Span,
    pub last: Option<Span>,
    pub comma: Option<Span>,
    pub can_trail: bool,
    pub must_trail: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceIdentifierRole {
    ValueBinding,
    /// Identifier member declared by a TypeScript enum. It is scoped to the enum initializer
    /// environment and must not be treated as an ordinary module-level value binding.
    EnumMemberBinding,
    ValueReference,
    TypeBinding,
    TypeReference,
    TypeQuery,
}

/// Original value declaration category for erased bindings. This is present only on value
/// bindings; references and type-space identifiers keep it unset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceValueBindingKind {
    Var,
    Let,
    Const,
    Using,
    Function,
    Class,
}

/// Owned identifier spelling from the grammar, independent of the parser's Interner lifetime.
/// Cover-grammar candidates must still be interpreted with the final binding/reference model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceIdentifier {
    pub name: String,
    pub span: Span,
    pub role: SourceIdentifierRole,
    pub value_kind: Option<SourceValueBindingKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceImportBindingKind {
    Default,
    Namespace,
    Named,
    Equals,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceImportBinding {
    pub kind: SourceImportBindingKind,
    pub span: Span,
    pub local: SourceIdentifier,
    pub imported: Option<String>,
    pub type_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceModuleSpecifier {
    /// Decoded code units, distinct from the filesystem's accepted path text.
    pub value: JsString,
    pub span: Span,
}

/// Original import-type expression before erasure. The qualifier after `)` is separate syntax.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTypeImport {
    pub span: Span,
    pub source: SourceModuleSpecifier,
    pub attributes: Vec<SourceImportAttribute>,
    pub attributes_known: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceImportAttribute {
    pub key: JsString,
    pub value: JsString,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceImportAttributes {
    pub keyword: String,
    pub span: Span,
    pub entries: Vec<SourceImportAttribute>,
}

/// Original static import grammar, including declarations erased by TypeScript lowering.
/// Parent indexes the source-node list. All names and module strings are decoded by the parser.
/// Equals aliases without an external module keep their RHS range in equals_target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceImport {
    pub span: Span,
    pub parent: Option<usize>,
    pub type_only: bool,
    pub equals_target: Option<Span>,
    pub source: Option<SourceModuleSpecifier>,
    pub bindings: Vec<SourceImportBinding>,
    pub attributes: Option<SourceImportAttributes>,
}

/// Original export form, before TypeScript erasure and export-assignment lowering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceExportKind {
    Named,
    All,
    Default,
    Declaration,
    Assignment,
    Namespace,
}

/// Decoded module name with its original range. String names are not identifier occurrences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceExportName {
    pub value: String,
    pub span: Span,
    pub identifier: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceExportSpecifier {
    pub span: Span,
    pub local: SourceExportName,
    pub exported: SourceExportName,
    pub type_only: bool,
}

/// Grammar-owned export metadata. Named locals are references only without a `from` clause;
/// re-export names, export aliases, star namespaces and UMD names are not local references.
/// `type_only` records an explicit export-wide `type` modifier, not inferred declaration types.
/// `target` retains the declaration/default/assignment range. Parent indexes the source nodes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceExport {
    pub span: Span,
    pub parent: Option<usize>,
    pub kind: SourceExportKind,
    pub type_only: bool,
    pub source: Option<SourceModuleSpecifier>,
    pub specifiers: Vec<SourceExportSpecifier>,
    pub exported: Option<SourceExportName>,
    pub target: Option<Span>,
    pub attributes: Option<SourceImportAttributes>,
}

/// Primitive values proven by literal syntax. Unknown identifiers are never treated as globals.
#[derive(Clone, Debug, PartialEq)]
pub enum SourcePrimitiveValue {
    Unknown,
    Empty,
    Undefined,
    Null,
    Boolean(bool),
    Number(f64),
    /// Canonical signed decimal text for a BigInt literal; never a floating-point value.
    BigInt(String),
    String(JsString),
}

/// JSX attribute, spread, text or child-expression value before runtime lowering.
/// `node` indexes the source-node list. Shorthand attributes and normalized JSX text have no
/// original expression span. Text is decoded and whitespace-normalized by the JSX grammar.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceJsxValue {
    pub node: usize,
    pub expression: Option<Span>,
    pub value: SourcePrimitiveValue,
}

/// An original array element before spread lowering; absent entries in SourceArray are holes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceArrayElement {
    pub span: Span,
    pub expression: Span,
    pub spread: bool,
}

/// Original array/cover grammar. Consumers must use the final expression/binding classification.
/// Compiler-generated arrays have no record. Records are collected when the array grammar closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceArray {
    pub span: Span,
    pub parent: Option<usize>,
    pub elements: Vec<Option<SourceArrayElement>>,
}

/// An original function, method or arrow before lowering. `span` anchors its compilation node;
/// `scope` begins at the function header, excluding a method's computed key and decorators.
/// `body` includes original parentheses/type assertions of a concise arrow. Overloads have none.
/// Entries close in grammar order and include erased signatures, but never synthesized helpers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFunction {
    pub span: Span,
    pub scope: Span,
    pub body: Option<Span>,
    pub is_arrow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceTypeScopeKind {
    TypeParameters,
    MappedType,
    InferConstraint,
    ConditionalTrue,
}

/// Grammar-owned visibility of local type parameters. Bindings may precede their visibility
/// (mapped constraints and infer patterns); parent indexes another active type scope, not a
/// runtime scope. This describes syntax regions, not resolved symbols or type checking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTypeScope {
    pub kind: SourceTypeScopeKind,
    pub span: Span,
    pub parent: Option<usize>,
    pub bindings: Vec<SourceIdentifier>,
}

/// Original interface, alias, class or enum name. The node owns kind, range and containment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTypeDeclaration {
    pub node: usize,
    pub name: SourceIdentifier,
    /// A class expression binds its name only inside its own class scope.
    pub in_own_scope: bool,
}

/// Original namespace declaration, independent of generated functions and variables.
/// Dotted names are ordered outer-to-inner; string ambient modules have no identifier names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceNamespace {
    pub node: usize,
    pub names: Vec<SourceIdentifier>,
    pub ambient: Option<SourceModuleSpecifier>,
    /// Ambient declaration context, including inheritance through enclosing namespaces.
    pub is_ambient: bool,
    pub body: Option<Span>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceMemberKind {
    Named,
    Private,
    Computed,
}

/// Original receiver and property before erasure or lowering. Computed keys exclude brackets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceMember {
    pub span: Span,
    pub object: Span,
    pub property: Span,
    pub kind: SourceMemberKind,
    pub optional: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceCallKind {
    Call,
    Construct,
    TaggedTemplate,
    DynamicImport,
}

/// Committed source call. `head` includes optional/type arguments, but excludes `new`.
/// Tagged-template arguments are the original substitutions, without the implicit strings array.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceCall {
    pub kind: SourceCallKind,
    pub span: Span,
    pub head: Span,
    pub optional: bool,
    pub arguments: Vec<Span>,
}

/// Original `await` expression and its operand before any async lowering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceAwait {
    pub span: Span,
    pub argument: Span,
}

/// Expression statement boundary before semicolon insertion or lowering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceExpressionStatement {
    pub span: Span,
    pub expression: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAssignmentKind {
    Expression,
    Variable,
}

/// Original variable initializer or assignment expression before lowering. `value` is the
/// assigned right-hand side; `target` preserves the original binding/member target range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceAssignment {
    pub kind: SourceAssignmentKind,
    pub span: Span,
    pub target: Span,
    pub value: Span,
}

/// Original callback-valued object or JSX property before contextual type checking. `span`
/// identifies the property/attribute owner and `value` identifies its original value expression.
/// Spread members are intentionally not represented because their property identity is unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceCallbackKind {
    ObjectProperty,
    JsxAttribute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceCallback {
    pub kind: SourceCallbackKind,
    pub span: Span,
    pub value: Span,
}

/// Original return statement carrying a value expression. Bare `return;` has no type query site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceReturn {
    pub span: Span,
    pub argument: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceConditionKind {
    If,
    While,
    DoWhile,
    For,
    Conditional,
    Logical,
}

/// Original control-flow condition whose truthiness can accidentally consume a Promise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceCondition {
    pub kind: SourceConditionKind,
    pub span: Span,
    pub test: Span,
}

/// Original switch statement and whether it contains a default clause.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceSwitch {
    pub span: Span,
    pub discriminant: Span,
    pub has_default: bool,
    /// Primitive case values proven by the parser; unknown expressions are retained as unknown.
    pub cases: Vec<SourcePrimitiveValue>,
    /// Original expression ranges for each non-default case, aligned with `cases`.
    pub case_spans: Vec<Span>,
    /// Original `case` clause ranges for each non-default case, aligned with `case_spans`.
    pub case_clause_spans: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTemplate {
    pub span: Span,
    pub tagged: bool,
    pub expressions: Vec<Span>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAssertionKind {
    As,
    Angle,
    NonNull,
    Satisfies,
}

/// Original expression and operand before erasure; the type range excludes `as` and `< >`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceTypeAssertion {
    pub kind: SourceAssertionKind,
    pub span: Span,
    pub operand: Span,
    pub type_span: Option<Span>,
    /// The exact type production is the const keyword in an as/angle assertion. This establishes
    /// literal/readonly context; an equal contextual operand type does not make it unnecessary.
    pub is_const: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceNodeKind {
    JsBlock,
    JsSwitchBody,
    JsClass,
    JsFunctionBody,
    TsEnum,
    TsNamespace,
    TsAmbientModule,
    TsDeclare,
    TsGlobalAugmentation,
    TsInterface,
    TsTypeAlias,
    TsAny,
    TsNonNullAssertion,
    TsObjectType,
    TsTupleType,
    TsSignature,
    TsTypeMember,
    TsType,
    TsTypeAnnotation,
    TsTypeParameters,
    TsTypeArguments,
    TsTypeReference,
    TsArrayType,
    TsIndexedAccessType,
    TsTypeOperator,
    TsHeritageType,
    JsxElement,
    JsxFragment,
    JsxOpeningElement,
    JsxClosingElement,
    JsxName,
    JsxAttribute,
    JsxSpreadAttribute,
    JsxAttributeValue,
    JsxExpressionContainer,
    JsxText,
}

/// Grammar-owned UTF-8 source range. Parent is a preceding index in the same node list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceNode {
    pub kind: SourceNodeKind,
    pub span: Span,
    pub parent: Option<usize>,
}
