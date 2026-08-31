//! Epoch-coherent analysis for the owned typed optimization IR.
//!
//! This module deliberately accepts only [`TypedProgram`]. It never projects facts through parser
//! spans or consults the frozen parser AST: a structural edit followed by [`TypedAnalysis::rebuild`]
//! therefore has exactly one source of truth, the current live IR tree.

use std::collections::{BTreeMap, BTreeSet};

use wake_ecma_ast::{AssignmentOperator, BinaryOperator, PropertyKind, UnaryOperator};
use wake_ecma_semantic::{DeclKind, SymbolId};

use crate::typed_ir::{
    ArrowBodyKind, ClassContext, FunctionContext, IrNodeData, ListId, NameId, NameRole, NodeId,
    PropertyKeyKind, TypedIrError, TypedProgram,
};

/// Stable index into one rebuilt scope snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypedScopeId(u32);

impl TypedScopeId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Lexical environment reconstructed from the current typed tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedScopeKind {
    Module,
    Function,
    Block,
    Catch,
    Class,
}

/// How one live name occurrence uses its semantic binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameAccess {
    Declaration,
    Read,
    Write,
    ReadWrite,
    NonBinding,
}

impl NameAccess {
    const fn reads(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }
}

/// One live name occurrence, addressed without a source-span lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameUse {
    node: NodeId,
    name: NameId,
    scope: TypedScopeId,
    symbol: Option<SymbolId>,
    access: NameAccess,
}

impl NameUse {
    pub const fn node(&self) -> NodeId {
        self.node
    }

    pub const fn name(&self) -> NameId {
        self.name
    }

    #[cfg(test)]
    pub const fn scope(&self) -> TypedScopeId {
        self.scope
    }

    pub const fn symbol(&self) -> Option<SymbolId> {
        self.symbol
    }

    pub const fn access(&self) -> NameAccess {
        self.access
    }
}

/// Scope-local facts rebuilt together with references and data flow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedScopeFacts {
    parent: Option<TypedScopeId>,
    kind: TypedScopeKind,
    function_boundary: TypedScopeId,
    symbols: Vec<SymbolId>,
    frozen: bool,
    contains_direct_eval: bool,
    contains_with: bool,
}

impl TypedScopeFacts {
    pub const fn parent(&self) -> Option<TypedScopeId> {
        self.parent
    }

    #[cfg(test)]
    pub const fn function_boundary(&self) -> TypedScopeId {
        self.function_boundary
    }

    pub fn symbols(&self) -> &[SymbolId] {
        &self.symbols
    }

    pub const fn is_frozen(&self) -> bool {
        self.frozen
    }

    pub const fn contains_direct_eval(&self) -> bool {
        self.contains_direct_eval
    }

    pub const fn contains_with(&self) -> bool {
        self.contains_with
    }
}

/// Escape and dynamic-observation facts for one stable semantic binding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TypedEscapeSummary {
    captured: bool,
    passed_to_unknown: bool,
    returned_or_thrown: bool,
    stored_externally: bool,
    dynamically_observed: bool,
    aliased: bool,
}

impl TypedEscapeSummary {
    pub const fn captured(self) -> bool {
        self.captured
    }

    #[cfg(test)]
    pub const fn returned_or_thrown(self) -> bool {
        self.returned_or_thrown
    }

    #[cfg(test)]
    pub const fn dynamically_observed(self) -> bool {
        self.dynamically_observed
    }

    pub const fn aliased(self) -> bool {
        self.aliased
    }

    pub const fn escaped(self) -> bool {
        self.passed_to_unknown
            || self.returned_or_thrown
            || self.stored_externally
            || self.dynamically_observed
    }
}

/// Current-tree facts for one semantic binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedSymbolFacts {
    symbol: SymbolId,
    declaration_scope: Option<TypedScopeId>,
    declarations: Vec<NameId>,
    reads: Vec<NameId>,
    writes: Vec<NameId>,
    frozen: bool,
    escape: TypedEscapeSummary,
}

impl TypedSymbolFacts {
    pub const fn declaration_scope(&self) -> Option<TypedScopeId> {
        self.declaration_scope
    }

    pub fn declarations(&self) -> &[NameId] {
        &self.declarations
    }

    pub fn reads(&self) -> &[NameId] {
        &self.reads
    }

    pub fn writes(&self) -> &[NameId] {
        &self.writes
    }

    pub const fn is_frozen(&self) -> bool {
        self.frozen
    }

    pub const fn escape(&self) -> TypedEscapeSummary {
        self.escape
    }
}

/// Conservative observable behavior of evaluating one current IR node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TypedEffectSummary {
    may_have_side_effects: bool,
    may_throw: bool,
    reads_member: bool,
    writes_state: bool,
    calls_unknown: bool,
    accesses_unresolved: bool,
    suspends: bool,
}

impl TypedEffectSummary {
    pub const fn may_have_side_effects(self) -> bool {
        self.may_have_side_effects
    }

    pub const fn may_throw(self) -> bool {
        self.may_throw
    }

    pub const fn reads_member(self) -> bool {
        self.reads_member
    }

    pub const fn writes_state(self) -> bool {
        self.writes_state
    }

    pub const fn calls_unknown(self) -> bool {
        self.calls_unknown
    }

    pub const fn accesses_unresolved(self) -> bool {
        self.accesses_unresolved
    }

    pub const fn suspends(self) -> bool {
        self.suspends
    }

    fn combine(&mut self, other: Self) {
        self.may_have_side_effects |= other.may_have_side_effects;
        self.may_throw |= other.may_throw;
        self.reads_member |= other.reads_member;
        self.writes_state |= other.writes_state;
        self.calls_unknown |= other.calls_unknown;
        self.accesses_unresolved |= other.accesses_unresolved;
        self.suspends |= other.suspends;
    }
}

/// Stable index into the per-rebuild control-flow graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypedCfgBlockId(u32);

impl TypedCfgBlockId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedCfgBlockKind {
    Entry,
    Exit,
    FunctionEntry,
    FunctionExit,
    Statement,
    Condition,
    LoopHeader,
    LoopExit,
    SwitchExit,
    TryDispatch,
    CatchEntry,
    FinallyEntry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TypedCfgEdgeKind {
    Normal,
    True,
    False,
    Loop,
    Exception,
    Finally,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypedCfgEdge {
    from: TypedCfgBlockId,
    to: TypedCfgBlockId,
    kind: TypedCfgEdgeKind,
}

impl TypedCfgEdge {
    #[cfg(test)]
    pub const fn to(self) -> TypedCfgBlockId {
        self.to
    }

    #[cfg(test)]
    pub const fn kind(self) -> TypedCfgEdgeKind {
        self.kind
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlowEvent {
    Read { name: NameId, symbol: SymbolId },
    Initialize(SymbolId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedCfgBlock {
    id: TypedCfgBlockId,
    kind: TypedCfgBlockKind,
    events: Vec<FlowEvent>,
}

impl TypedCfgBlock {
    #[cfg(test)]
    pub const fn id(&self) -> TypedCfgBlockId {
        self.id
    }

    #[cfg(test)]
    pub const fn kind(&self) -> TypedCfgBlockKind {
        self.kind
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypedControlFlowGraph {
    blocks: Vec<TypedCfgBlock>,
    edges: Vec<TypedCfgEdge>,
    roots: BTreeMap<TypedCfgBlockId, BTreeSet<SymbolId>>,
}

impl TypedControlFlowGraph {
    #[cfg(test)]
    pub fn blocks(&self) -> &[TypedCfgBlock] {
        &self.blocks
    }

    #[cfg(test)]
    pub fn edges(&self) -> &[TypedCfgEdge] {
        &self.edges
    }
}

#[derive(Clone, Copy, Debug)]
struct FunctionRegion {
    owner: NodeId,
    body: NodeId,
    body_kind: ArrowBodyKind,
    scope: TypedScopeId,
}

/// One coherent current-tree analysis generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedAnalysis {
    revision: u64,
    scopes: Vec<TypedScopeFacts>,
    node_scopes: Vec<Option<TypedScopeId>>,
    name_uses: Vec<Option<NameUse>>,
    symbols: Vec<TypedSymbolFacts>,
    effects: Vec<Option<TypedEffectSummary>>,
    cfg: TypedControlFlowGraph,
    definitely_initialized_reads: Vec<Option<bool>>,
}

impl TypedAnalysis {
    /// Rebuild every fact from the current live typed tree.
    pub fn rebuild(program: &TypedProgram) -> Result<Self, TypedIrError> {
        program.validate()?;
        Self::rebuild_validated(program)
    }

    /// Rebuild facts for a program revision already validated by the owning scheduler.
    pub(crate) fn rebuild_validated(program: &TypedProgram) -> Result<Self, TypedIrError> {
        Analyzer::new(program).build()
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn scopes(&self) -> &[TypedScopeFacts] {
        &self.scopes
    }

    pub fn scope(&self, scope: TypedScopeId) -> Option<&TypedScopeFacts> {
        self.scopes.get(scope.index())
    }

    pub fn node_scope(&self, node: NodeId) -> Option<TypedScopeId> {
        self.node_scopes.get(node.index()).copied().flatten()
    }

    pub fn name_use(&self, name: NameId) -> Option<&NameUse> {
        self.name_uses.get(name.index()).and_then(Option::as_ref)
    }

    pub fn symbol(&self, symbol: SymbolId) -> Option<&TypedSymbolFacts> {
        self.symbols.get(symbol as usize)
    }

    #[cfg(test)]
    pub fn reference_count(&self, symbol: SymbolId) -> usize {
        self.symbol(symbol).map_or(0, |facts| facts.reads.len())
    }

    pub fn effect(&self, node: NodeId) -> Option<TypedEffectSummary> {
        self.effects.get(node.index()).copied().flatten()
    }

    #[cfg(test)]
    pub const fn cfg(&self) -> &TypedControlFlowGraph {
        &self.cfg
    }

    pub fn read_is_definitely_initialized(&self, name: NameId) -> Option<bool> {
        self.definitely_initialized_reads
            .get(name.index())
            .copied()
            .flatten()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestedAccess {
    Default,
    Read,
    Write,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DefinitePrimitiveKind {
    Number,
    String,
    Boolean,
    Null,
    BigInt,
    Undefined,
}

fn known_binary_result(
    operator: BinaryOperator,
    left: DefinitePrimitiveKind,
    right: DefinitePrimitiveKind,
) -> Option<DefinitePrimitiveKind> {
    use BinaryOperator::{
        Add, BitAnd, BitOr, BitXor, Div, Eq, Exp, Gt, GtEq, In, Instanceof, Lt, LtEq, Mul, NotEq,
        Rem, Shl, Shr, StrictEq, StrictNotEq, Sub, Ushr,
    };

    match operator {
        StrictEq | StrictNotEq | Eq | NotEq | Lt | Gt | LtEq | GtEq => {
            Some(DefinitePrimitiveKind::Boolean)
        }
        Add if left == DefinitePrimitiveKind::String || right == DefinitePrimitiveKind::String => {
            Some(DefinitePrimitiveKind::String)
        }
        Add if left == DefinitePrimitiveKind::BigInt && right == DefinitePrimitiveKind::BigInt => {
            Some(DefinitePrimitiveKind::BigInt)
        }
        Add if left != DefinitePrimitiveKind::BigInt && right != DefinitePrimitiveKind::BigInt => {
            Some(DefinitePrimitiveKind::Number)
        }
        Sub | Mul | Div | Rem | Exp | BitAnd | BitOr | BitXor | Shl | Shr | Ushr
            if left != DefinitePrimitiveKind::BigInt && right != DefinitePrimitiveKind::BigInt =>
        {
            Some(DefinitePrimitiveKind::Number)
        }
        Add | Sub | Mul | Div | Rem | Exp | BitAnd | BitOr | BitXor | Shl | Shr | Ushr | In
        | Instanceof => None,
    }
}

#[derive(Clone, Copy)]
enum EscapeReason {
    PassedToUnknown,
    ReturnOrThrow,
    StoredExternally,
    Alias,
}

struct Analyzer<'program> {
    program: &'program TypedProgram,
    scopes: Vec<TypedScopeFacts>,
    scope_stack: Vec<TypedScopeId>,
    node_scopes: Vec<Option<TypedScopeId>>,
    name_uses: Vec<Option<NameUse>>,
    symbols: Vec<TypedSymbolFacts>,
    effects: Vec<Option<TypedEffectSummary>>,
    function_regions: Vec<FunctionRegion>,
    dynamic_region_depth: usize,
}

impl<'program> Analyzer<'program> {
    fn new(program: &'program TypedProgram) -> Self {
        let symbols = program
            .symbols()
            .iter()
            .enumerate()
            .map(|(symbol, _)| TypedSymbolFacts {
                symbol: symbol as SymbolId,
                declaration_scope: None,
                declarations: Vec::new(),
                reads: Vec::new(),
                writes: Vec::new(),
                frozen: false,
                escape: TypedEscapeSummary::default(),
            })
            .collect();
        Self {
            program,
            scopes: Vec::new(),
            scope_stack: Vec::new(),
            node_scopes: vec![None; program.nodes().len()],
            name_uses: vec![None; program.names().len()],
            symbols,
            effects: vec![None; program.nodes().len()],
            function_regions: Vec::new(),
            dynamic_region_depth: 0,
        }
    }

    fn build(mut self) -> Result<TypedAnalysis, TypedIrError> {
        let module = self.push_scope(TypedScopeKind::Module, self.program.root());
        debug_assert_eq!(module.index(), 0);
        self.visit(self.program.root(), RequestedAccess::Default);
        self.scope_stack.pop();

        self.finish_symbol_facts();
        let mut cfg_builder = CfgBuilder::new(
            self.program,
            &self.scopes,
            &self.node_scopes,
            &self.name_uses,
            &self.symbols,
        );
        cfg_builder.build_program(self.program.root());
        for region in &self.function_regions {
            cfg_builder.build_function(*region);
        }
        let cfg = cfg_builder.finish();
        let definitely_initialized_reads = solve_definite_initialization(
            &cfg,
            self.program.names().len(),
            self.program.symbols().len(),
        );

        for (name_index, initialized) in definitely_initialized_reads.iter().enumerate() {
            let Some(initialized) = initialized else {
                continue;
            };
            let Some(name_use) = self.name_uses[name_index] else {
                continue;
            };
            let Some(parent) = self
                .program
                .node(name_use.node)
                .and_then(|node| node.parent())
                .map(|link| link.parent())
            else {
                continue;
            };
            let Some(effect) = self.effects[parent.index()].as_mut() else {
                continue;
            };
            if !effect.may_have_side_effects
                && !effect.reads_member
                && !effect.writes_state
                && !effect.calls_unknown
                && !effect.accesses_unresolved
                && !effect.suspends
            {
                effect.may_throw = !initialized;
            }
        }

        Ok(TypedAnalysis {
            revision: self.program.revision(),
            scopes: self.scopes,
            node_scopes: self.node_scopes,
            name_uses: self.name_uses,
            symbols: self.symbols,
            effects: self.effects,
            cfg,
            definitely_initialized_reads,
        })
    }

    fn current_scope(&self) -> TypedScopeId {
        *self.scope_stack.last().expect("typed analysis scope stack")
    }

    fn push_scope(&mut self, kind: TypedScopeKind, _owner: NodeId) -> TypedScopeId {
        let parent = self.scope_stack.last().copied();
        let id = TypedScopeId(self.scopes.len() as u32);
        let function_boundary = if matches!(kind, TypedScopeKind::Module | TypedScopeKind::Function)
        {
            id
        } else {
            parent
                .map(|parent| self.scopes[parent.index()].function_boundary)
                .unwrap_or(id)
        };
        let frozen = self.dynamic_region_depth > 0;
        self.scopes.push(TypedScopeFacts {
            parent,
            kind,
            function_boundary,
            symbols: Vec::new(),
            frozen,
            contains_direct_eval: false,
            contains_with: false,
        });
        self.scope_stack.push(id);
        id
    }

    fn pop_scope(&mut self) {
        self.scope_stack.pop();
    }

    fn set_node_scope(&mut self, node: NodeId, scope: TypedScopeId) {
        self.node_scopes[node.index()] = Some(scope);
    }

    fn list_items(&self, list: ListId) -> Vec<NodeId> {
        self.program
            .list(list)
            .expect("validated typed IR list")
            .items()
            .to_vec()
    }

    fn visit_list(&mut self, list: ListId, access: RequestedAccess) -> TypedEffectSummary {
        let mut effect = TypedEffectSummary::default();
        for node in self.list_items(list) {
            effect.combine(self.visit(node, access));
        }
        effect
    }

    fn visit_optional(
        &mut self,
        node: Option<NodeId>,
        access: RequestedAccess,
    ) -> TypedEffectSummary {
        node.map_or_else(TypedEffectSummary::default, |node| self.visit(node, access))
    }

    fn visit(&mut self, node: NodeId, requested: RequestedAccess) -> TypedEffectSummary {
        let scope = self.current_scope();
        self.set_node_scope(node, scope);
        let data = self
            .program
            .node(node)
            .expect("validated typed IR node")
            .data()
            .clone();
        let mut effect = match data {
            IrNodeData::Program { body, .. } => self.visit_list(body, RequestedAccess::Default),
            IrNodeData::VariableDeclaration { declarations, .. } => {
                self.visit_list(declarations, RequestedAccess::Default)
            }
            IrNodeData::VariableDeclarator {
                binding,
                initializer,
            } => {
                let mut effect = self.visit(binding, RequestedAccess::Default);
                effect.combine(self.visit_optional(initializer, RequestedAccess::Read));
                if let Some(initializer) = initializer {
                    self.mark_escape(initializer, EscapeReason::Alias);
                }
                effect
            }
            IrNodeData::Function {
                context,
                name,
                parameters,
                body,
                is_async: _,
                is_generator: _,
            } => {
                if matches!(
                    context,
                    FunctionContext::Declaration | FunctionContext::ExportDefault
                ) {
                    self.visit_optional(name, RequestedAccess::Default);
                }
                let function_scope = self.push_scope(TypedScopeKind::Function, node);
                self.set_node_scope(node, scope);
                if matches!(
                    context,
                    FunctionContext::Expression | FunctionContext::Method
                ) {
                    self.visit_optional(name, RequestedAccess::Default);
                }
                self.visit_list(parameters, RequestedAccess::Default);
                if let Some(body) = body {
                    self.visit(body, RequestedAccess::Default);
                    self.function_regions.push(FunctionRegion {
                        owner: node,
                        body,
                        body_kind: ArrowBodyKind::Block,
                        scope: function_scope,
                    });
                }
                self.pop_scope();
                // Creating an ordinary function does not execute its body.
                TypedEffectSummary::default()
            }
            IrNodeData::FunctionBody { statements, .. } => {
                self.visit_list(statements, RequestedAccess::Default)
            }
            IrNodeData::Class {
                context,
                name,
                super_class,
                members,
                decorators,
            } => {
                if matches!(
                    context,
                    ClassContext::Declaration | ClassContext::ExportDefault
                ) {
                    self.visit_optional(name, RequestedAccess::Default);
                }
                let outer_scope = self.current_scope();
                self.push_scope(TypedScopeKind::Class, node);
                self.set_node_scope(node, outer_scope);
                if context == ClassContext::Expression {
                    self.visit_optional(name, RequestedAccess::Default);
                }
                let mut effect = self.visit_optional(super_class, RequestedAccess::Read);
                effect.combine(self.visit_list(decorators, RequestedAccess::Read));
                effect.combine(self.visit_list(members, RequestedAccess::Default));
                self.pop_scope();
                effect
            }
            IrNodeData::Block { body } => {
                let block_scope = self.push_scope(TypedScopeKind::Block, node);
                self.set_node_scope(node, block_scope);
                let effect = self.visit_list(body, RequestedAccess::Default);
                self.pop_scope();
                effect
            }
            IrNodeData::EmptyStatement => TypedEffectSummary::default(),
            IrNodeData::DebuggerStatement => TypedEffectSummary {
                may_have_side_effects: true,
                ..TypedEffectSummary::default()
            },
            IrNodeData::ExpressionStatement { expression, .. } => {
                self.visit(expression, RequestedAccess::Read)
            }
            IrNodeData::IfStatement {
                test,
                consequent,
                alternate,
            } => {
                let mut effect = self.visit(test, RequestedAccess::Read);
                effect.combine(self.visit(consequent, RequestedAccess::Default));
                effect.combine(self.visit_optional(alternate, RequestedAccess::Default));
                effect
            }
            IrNodeData::ForStatement {
                initializer,
                initializer_kind: _,
                test,
                update,
                body,
            } => {
                let loop_scope = self.push_scope(TypedScopeKind::Block, node);
                self.set_node_scope(node, loop_scope);
                let mut effect = self.visit_optional(initializer, RequestedAccess::Default);
                effect.combine(self.visit_optional(test, RequestedAccess::Read));
                effect.combine(self.visit(body, RequestedAccess::Default));
                effect.combine(self.visit_optional(update, RequestedAccess::Read));
                self.pop_scope();
                effect
            }
            IrNodeData::ForInStatement {
                left,
                left_kind: _,
                right,
                body,
            }
            | IrNodeData::ForOfStatement {
                left,
                left_kind: _,
                right,
                body,
                is_await: _,
            } => {
                let loop_scope = self.push_scope(TypedScopeKind::Block, node);
                self.set_node_scope(node, loop_scope);
                let mut effect = self.visit(right, RequestedAccess::Read);
                effect.combine(self.visit(left, RequestedAccess::Write));
                effect.combine(self.visit(body, RequestedAccess::Default));
                self.pop_scope();
                effect
            }
            IrNodeData::WhileStatement { test, body } => {
                let mut effect = self.visit(test, RequestedAccess::Read);
                effect.combine(self.visit(body, RequestedAccess::Default));
                effect
            }
            IrNodeData::DoWhileStatement { body, test } => {
                let mut effect = self.visit(body, RequestedAccess::Default);
                effect.combine(self.visit(test, RequestedAccess::Read));
                effect
            }
            IrNodeData::SwitchStatement {
                discriminant,
                cases,
            } => {
                let switch_scope = self.push_scope(TypedScopeKind::Block, node);
                self.set_node_scope(node, switch_scope);
                let mut effect = self.visit(discriminant, RequestedAccess::Read);
                effect.combine(self.visit_list(cases, RequestedAccess::Default));
                self.pop_scope();
                effect
            }
            IrNodeData::SwitchCase { test, consequent } => {
                let mut effect = self.visit_optional(test, RequestedAccess::Read);
                effect.combine(self.visit_list(consequent, RequestedAccess::Default));
                effect
            }
            IrNodeData::ReturnStatement { argument } => {
                let effect = self.visit_optional(argument, RequestedAccess::Read);
                if let Some(argument) = argument {
                    self.mark_escape(argument, EscapeReason::ReturnOrThrow);
                }
                effect
            }
            IrNodeData::BreakStatement { label } | IrNodeData::ContinueStatement { label } => {
                self.visit_optional(label, RequestedAccess::Default)
            }
            IrNodeData::ThrowStatement { argument } => {
                let mut effect = self.visit(argument, RequestedAccess::Read);
                effect.may_have_side_effects = true;
                effect.may_throw = true;
                self.mark_escape(argument, EscapeReason::ReturnOrThrow);
                effect
            }
            IrNodeData::TryStatement {
                block,
                handler,
                finalizer,
            } => {
                let mut effect = self.visit(block, RequestedAccess::Default);
                effect.combine(self.visit_optional(handler, RequestedAccess::Default));
                effect.combine(self.visit_optional(finalizer, RequestedAccess::Default));
                effect
            }
            IrNodeData::CatchClause { parameter, body } => {
                let catch_scope = self.push_scope(TypedScopeKind::Catch, node);
                self.set_node_scope(node, catch_scope);
                let mut effect = self.visit_optional(parameter, RequestedAccess::Default);
                effect.combine(self.visit(body, RequestedAccess::Default));
                self.pop_scope();
                effect
            }
            IrNodeData::LabeledStatement { label, body } => {
                let mut effect = self.visit(label, RequestedAccess::Default);
                effect.combine(self.visit(body, RequestedAccess::Default));
                effect
            }
            IrNodeData::WithStatement { object, body } => {
                let mut effect = self.visit(object, RequestedAccess::Read);
                self.freeze_visible(false, true);
                self.dynamic_region_depth += 1;
                effect.combine(self.visit(body, RequestedAccess::Default));
                self.dynamic_region_depth -= 1;
                effect.may_have_side_effects = true;
                effect.may_throw = true;
                effect
            }
            IrNodeData::NumberLiteral { value: _ }
            | IrNodeData::StringLiteral { value: _ }
            | IrNodeData::BooleanLiteral { value: _ }
            | IrNodeData::NullLiteral
            | IrNodeData::BigIntLiteral { raw: _ }
            | IrNodeData::RegExpLiteral {
                pattern: _,
                flags: _,
            }
            | IrNodeData::TemplateElement {
                cooked: _,
                raw: _,
                tail: _,
            }
            | IrNodeData::ThisExpression
            | IrNodeData::SuperExpression
            | IrNodeData::Elision => TypedEffectSummary::default(),
            IrNodeData::TemplateLiteral {
                quasis,
                expressions,
            } => {
                let mut effect = self.visit_list(quasis, RequestedAccess::Default);
                let expression_nodes = self.list_items(expressions);
                for expression in expression_nodes {
                    effect.combine(self.visit(expression, RequestedAccess::Read));
                    if !self.is_definite_primitive(expression) {
                        effect.may_have_side_effects = true;
                        effect.may_throw = true;
                    }
                }
                effect
            }
            IrNodeData::Name { name } => {
                self.visit_name(node, name, requested);
                TypedEffectSummary::default()
            }
            IrNodeData::Identifier { name } => {
                self.visit(name, requested);
                let Some(name_id) = self.name_id(name) else {
                    unreachable!("validated Identifier name")
                };
                let name = self.program.name(name_id).expect("validated typed name");
                let access = self.name_uses[name_id.index()]
                    .expect("identifier name was visited")
                    .access;
                if access.reads() && name.symbol().is_none() {
                    TypedEffectSummary {
                        may_throw: true,
                        accesses_unresolved: true,
                        ..TypedEffectSummary::default()
                    }
                } else {
                    TypedEffectSummary::default()
                }
            }
            IrNodeData::MetaProperty { meta, property } => {
                let mut effect = self.visit(meta, RequestedAccess::Default);
                effect.combine(self.visit(property, RequestedAccess::Default));
                effect
            }
            IrNodeData::ArrayExpression { elements } => self.visit_list(elements, requested),
            IrNodeData::ObjectExpression { members } => self.visit_list(members, requested),
            IrNodeData::ObjectProperty {
                key,
                value,
                kind,
                method,
                shorthand: _,
                computed,
                prototype_setter: _,
            } => {
                let mut effect = if computed {
                    self.visit(key.value, RequestedAccess::Read)
                } else {
                    self.visit(key.value, RequestedAccess::Default)
                };
                effect.combine(self.visit(value, requested));
                if computed && !self.is_definite_primitive(key.value) {
                    effect.may_have_side_effects = true;
                    effect.may_throw = true;
                }
                if matches!(kind, PropertyKind::Get | PropertyKind::Set) || method {
                    // Defining an accessor/method does not invoke it. Its function body was still
                    // analyzed independently by `visit(Function)` above.
                }
                effect
            }
            IrNodeData::UnaryExpression { operator, argument } => {
                let mut effect = self.visit(argument, RequestedAccess::Read);
                match operator {
                    UnaryOperator::LogicalNot | UnaryOperator::Typeof | UnaryOperator::Void => {
                        if operator == UnaryOperator::Typeof
                            && self.is_unresolved_identifier(argument)
                        {
                            effect.may_throw = false;
                        }
                    }
                    UnaryOperator::Minus | UnaryOperator::Plus | UnaryOperator::BitwiseNot => {
                        let argument_kind = self.definite_primitive_kind(argument);
                        let coercion_is_proven_safe = argument_kind.is_some()
                            && !(operator == UnaryOperator::Plus
                                && argument_kind == Some(DefinitePrimitiveKind::BigInt));
                        if !coercion_is_proven_safe {
                            effect.may_have_side_effects = true;
                            effect.may_throw = true;
                        }
                    }
                    UnaryOperator::Delete => {
                        effect.may_have_side_effects = true;
                        effect.may_throw = true;
                        effect.writes_state = true;
                    }
                }
                effect
            }
            IrNodeData::UpdateExpression {
                operator: _,
                prefix: _,
                argument,
            } => {
                let mut effect = self.visit(argument, RequestedAccess::ReadWrite);
                effect.may_have_side_effects = true;
                effect.may_throw = true;
                effect.writes_state = true;
                effect
            }
            IrNodeData::BinaryExpression {
                operator,
                left,
                right,
            } => {
                let mut effect = self.visit(left, RequestedAccess::Read);
                effect.combine(self.visit(right, RequestedAccess::Read));
                let coercion_is_proven_safe = matches!(
                    operator,
                    BinaryOperator::StrictEq | BinaryOperator::StrictNotEq
                ) || self
                    .definite_primitive_kind(left)
                    .zip(self.definite_primitive_kind(right))
                    .and_then(|(left, right)| known_binary_result(operator, left, right))
                    .is_some();
                if !coercion_is_proven_safe {
                    effect.may_have_side_effects = true;
                    effect.may_throw = true;
                }
                effect
            }
            IrNodeData::LogicalExpression {
                operator: _,
                left,
                right,
            } => {
                let mut effect = self.visit(left, RequestedAccess::Read);
                effect.combine(self.visit(right, RequestedAccess::Read));
                effect
            }
            IrNodeData::AssignmentExpression {
                operator,
                left,
                right,
            } => {
                let left_access = if operator == AssignmentOperator::Assign {
                    RequestedAccess::Write
                } else {
                    RequestedAccess::ReadWrite
                };
                let mut effect = self.visit(left, left_access);
                effect.combine(self.visit(right, RequestedAccess::Read));
                effect.may_have_side_effects = true;
                effect.may_throw = true;
                effect.writes_state = true;
                if self.assignment_target_is_external(left) {
                    self.mark_escape(right, EscapeReason::StoredExternally);
                } else {
                    self.mark_escape(right, EscapeReason::Alias);
                }
                effect
            }
            IrNodeData::ConditionalExpression {
                test,
                consequent,
                alternate,
            } => {
                let mut effect = self.visit(test, RequestedAccess::Read);
                effect.combine(self.visit(consequent, RequestedAccess::Read));
                effect.combine(self.visit(alternate, RequestedAccess::Read));
                effect
            }
            IrNodeData::CallExpression {
                callee,
                arguments,
                optional: _,
            } => {
                if self.is_direct_eval(callee) {
                    self.freeze_visible(true, false);
                }
                let mut effect = self.visit(callee, RequestedAccess::Read);
                for argument in self.list_items(arguments) {
                    effect.combine(self.visit(argument, RequestedAccess::Read));
                    self.mark_escape(argument, EscapeReason::PassedToUnknown);
                }
                effect.may_have_side_effects = true;
                effect.may_throw = true;
                effect.calls_unknown = true;
                effect
            }
            IrNodeData::NewExpression { callee, arguments } => {
                let mut effect = self.visit(callee, RequestedAccess::Read);
                for argument in self.list_items(arguments) {
                    effect.combine(self.visit(argument, RequestedAccess::Read));
                    self.mark_escape(argument, EscapeReason::PassedToUnknown);
                }
                effect.may_have_side_effects = true;
                effect.may_throw = true;
                effect.calls_unknown = true;
                effect
            }
            IrNodeData::MemberExpression {
                object,
                property,
                property_kind,
                optional: _,
            } => {
                let mut effect = self.visit(object, RequestedAccess::Read);
                let property_access = if property_kind == PropertyKeyKind::Computed {
                    RequestedAccess::Read
                } else {
                    RequestedAccess::Default
                };
                effect.combine(self.visit(property, property_access));
                effect.may_have_side_effects = true;
                effect.may_throw = true;
                effect.reads_member = true;
                if matches!(
                    requested,
                    RequestedAccess::Write | RequestedAccess::ReadWrite
                ) {
                    effect.writes_state = true;
                }
                effect
            }
            IrNodeData::SequenceExpression { expressions } => {
                self.visit_list(expressions, RequestedAccess::Read)
            }
            IrNodeData::TaggedTemplateExpression { tag, quasi } => {
                let mut effect = self.visit(tag, RequestedAccess::Read);
                effect.combine(self.visit(quasi, RequestedAccess::Read));
                self.mark_escape(quasi, EscapeReason::PassedToUnknown);
                effect.may_have_side_effects = true;
                effect.may_throw = true;
                effect.calls_unknown = true;
                effect
            }
            IrNodeData::SpreadElement { argument } => {
                let mut effect = self.visit(argument, RequestedAccess::Read);
                effect.may_have_side_effects = true;
                effect.may_throw = true;
                effect.reads_member = true;
                effect
            }
            IrNodeData::AwaitExpression { argument } => {
                let mut effect = self.visit(argument, RequestedAccess::Read);
                effect.may_have_side_effects = true;
                effect.may_throw = true;
                effect.suspends = true;
                effect
            }
            IrNodeData::YieldExpression {
                argument,
                delegate: _,
            } => {
                let mut effect = self.visit_optional(argument, RequestedAccess::Read);
                effect.may_have_side_effects = true;
                effect.may_throw = true;
                effect.suspends = true;
                if let Some(argument) = argument {
                    self.mark_escape(argument, EscapeReason::ReturnOrThrow);
                }
                effect
            }
            IrNodeData::ImportExpression { source, options } => {
                let mut effect = self.visit(source, RequestedAccess::Read);
                effect.combine(self.visit_optional(options, RequestedAccess::Read));
                effect.may_have_side_effects = true;
                effect.may_throw = true;
                effect.calls_unknown = true;
                effect
            }
            IrNodeData::ArrowFunction {
                parameters,
                body,
                body_kind,
                is_async: _,
            } => {
                let outer_scope = self.current_scope();
                let function_scope = self.push_scope(TypedScopeKind::Function, node);
                self.set_node_scope(node, outer_scope);
                self.visit_list(parameters, RequestedAccess::Default);
                self.visit(body, RequestedAccess::Read);
                self.function_regions.push(FunctionRegion {
                    owner: node,
                    body,
                    body_kind,
                    scope: function_scope,
                });
                self.pop_scope();
                TypedEffectSummary::default()
            }
            IrNodeData::MethodDefinition {
                key,
                value,
                kind: _,
                is_static: _,
                computed,
                decorators,
            } => {
                let mut effect = if computed {
                    self.visit(key.value, RequestedAccess::Read)
                } else {
                    self.visit(key.value, RequestedAccess::Default)
                };
                effect.combine(self.visit_list(decorators, RequestedAccess::Read));
                self.visit(value, RequestedAccess::Default);
                if computed && !self.is_definite_primitive(key.value) {
                    effect.may_have_side_effects = true;
                    effect.may_throw = true;
                }
                effect
            }
            IrNodeData::PropertyDefinition {
                key,
                value,
                is_static,
                computed,
                decorators,
                accessor: _,
            } => {
                let mut effect = if computed {
                    self.visit(key.value, RequestedAccess::Read)
                } else {
                    self.visit(key.value, RequestedAccess::Default)
                };
                effect.combine(self.visit_list(decorators, RequestedAccess::Read));
                let value_effect = self.visit_optional(value, RequestedAccess::Read);
                if is_static {
                    effect.combine(value_effect);
                }
                if computed && !self.is_definite_primitive(key.value) {
                    effect.may_have_side_effects = true;
                    effect.may_throw = true;
                }
                effect
            }
            IrNodeData::StaticBlock { body } => {
                let block_scope = self.push_scope(TypedScopeKind::Block, node);
                self.set_node_scope(node, block_scope);
                let effect = self.visit_list(body, RequestedAccess::Default);
                self.pop_scope();
                effect
            }
            IrNodeData::ArrayPattern { elements } => {
                self.visit_list(elements, RequestedAccess::Default)
            }
            IrNodeData::ObjectPattern { properties, rest } => {
                let mut effect = self.visit_list(properties, RequestedAccess::Default);
                effect.combine(self.visit_optional(rest, RequestedAccess::Default));
                effect
            }
            IrNodeData::ObjectPatternProperty {
                key,
                value,
                shorthand: _,
                computed,
            } => {
                let mut effect = if computed {
                    self.visit(key.value, RequestedAccess::Read)
                } else {
                    self.visit(key.value, RequestedAccess::Default)
                };
                effect.combine(self.visit(value, RequestedAccess::Default));
                effect
            }
            IrNodeData::AssignmentPattern { left, right } => {
                let mut effect = self.visit(left, RequestedAccess::Default);
                effect.combine(self.visit(right, RequestedAccess::Read));
                effect
            }
            IrNodeData::RestPattern { argument } => self.visit(argument, RequestedAccess::Default),
            IrNodeData::ImportDeclaration {
                specifiers,
                source,
                attributes,
            } => {
                let mut effect = self.visit_list(specifiers, RequestedAccess::Default);
                effect.combine(self.visit(source, RequestedAccess::Default));
                effect.combine(self.visit_optional(attributes, RequestedAccess::Default));
                effect
            }
            IrNodeData::ImportSpecifier {
                kind: _,
                imported,
                local,
            } => {
                let mut effect = imported.map_or_else(TypedEffectSummary::default, |name| {
                    self.visit(name.value, RequestedAccess::Default)
                });
                effect.combine(self.visit(local, RequestedAccess::Default));
                effect
            }
            IrNodeData::ImportAttributes { keyword: _, items } => {
                self.visit_list(items, RequestedAccess::Default)
            }
            IrNodeData::ImportAttribute { key, value } => {
                let mut effect = self.visit(key.value, RequestedAccess::Default);
                effect.combine(self.visit(value, RequestedAccess::Default));
                effect
            }
            IrNodeData::ExportNamedDeclaration {
                declaration,
                specifiers,
                source,
                attributes,
            } => {
                let mut effect = self.visit_optional(declaration, RequestedAccess::Default);
                effect.combine(self.visit_list(specifiers, RequestedAccess::Default));
                effect.combine(self.visit_optional(source, RequestedAccess::Default));
                effect.combine(self.visit_optional(attributes, RequestedAccess::Default));
                effect
            }
            IrNodeData::ExportSpecifier { local, exported } => {
                let mut effect = self.visit(local.value, RequestedAccess::Read);
                effect.combine(self.visit(exported.value, RequestedAccess::Default));
                effect
            }
            IrNodeData::ExportDefaultDeclaration { value, kind: _ } => {
                self.visit(value, RequestedAccess::Read)
            }
            IrNodeData::ExportAllDeclaration {
                exported,
                source,
                attributes,
            } => {
                let mut effect = exported.map_or_else(TypedEffectSummary::default, |name| {
                    self.visit(name.value, RequestedAccess::Default)
                });
                effect.combine(self.visit(source, RequestedAccess::Default));
                effect.combine(self.visit_optional(attributes, RequestedAccess::Default));
                effect
            }
        };
        if matches!(
            self.program.node(node).expect("validated node").data(),
            IrNodeData::ForOfStatement { is_await: true, .. }
        ) {
            effect.may_have_side_effects = true;
            effect.may_throw = true;
            effect.suspends = true;
        }
        self.effects[node.index()] = Some(effect);
        effect
    }

    fn name_id(&self, node: NodeId) -> Option<NameId> {
        match self.program.node(node)?.data() {
            IrNodeData::Name { name } => Some(*name),
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

    fn visit_name(&mut self, node: NodeId, name_id: NameId, requested: RequestedAccess) {
        let name = self.program.name(name_id).expect("validated typed name");
        let access = match name.role() {
            NameRole::Binding
            | NameRole::FunctionName
            | NameRole::ClassName
            | NameRole::ImportBinding => NameAccess::Declaration,
            NameRole::Reference | NameRole::ExportLocal => match requested {
                RequestedAccess::Write => NameAccess::Write,
                RequestedAccess::ReadWrite => NameAccess::ReadWrite,
                RequestedAccess::Default | RequestedAccess::Read => NameAccess::Read,
            },
            NameRole::AssignmentTarget => match requested {
                RequestedAccess::ReadWrite => NameAccess::ReadWrite,
                RequestedAccess::Read => NameAccess::Read,
                RequestedAccess::Default | RequestedAccess::Write => NameAccess::Write,
            },
            NameRole::Property
            | NameRole::PrivateProperty
            | NameRole::LabelDeclaration
            | NameRole::LabelReference
            | NameRole::ImportName
            | NameRole::ModuleSpecifier
            | NameRole::ExportedName
            | NameRole::AttributeKey
            | NameRole::MetaKeyword
            | NameRole::MetaProperty => NameAccess::NonBinding,
        };
        let scope = if access == NameAccess::Declaration {
            name.symbol()
                .and_then(|symbol| self.program.symbol(symbol))
                .map_or_else(
                    || self.current_scope(),
                    |symbol| match symbol.decl_kind() {
                        DeclKind::Var | DeclKind::Function => self.nearest_function_scope(),
                        DeclKind::Let
                        | DeclKind::Const
                        | DeclKind::Class
                        | DeclKind::Param
                        | DeclKind::Import
                        | DeclKind::CatchParam
                        | DeclKind::Using => self.current_scope(),
                    },
                )
        } else {
            self.current_scope()
        };
        self.set_node_scope(node, scope);
        let name_use = NameUse {
            node,
            name: name_id,
            scope,
            symbol: name.symbol(),
            access,
        };
        self.name_uses[name_id.index()] = Some(name_use);
        let Some(symbol) = name.symbol() else {
            return;
        };
        let facts = &mut self.symbols[symbol as usize];
        match access {
            NameAccess::Declaration => {
                facts.declaration_scope.get_or_insert(scope);
                facts.declarations.push(name_id);
                self.scopes[scope.index()].symbols.push(symbol);
            }
            NameAccess::Read => facts.reads.push(name_id),
            NameAccess::Write => facts.writes.push(name_id),
            NameAccess::ReadWrite => {
                facts.reads.push(name_id);
                facts.writes.push(name_id);
            }
            NameAccess::NonBinding => {}
        }
        if name.role() == NameRole::ExportLocal {
            facts.escape.stored_externally = true;
        }
    }

    fn nearest_function_scope(&self) -> TypedScopeId {
        self.scope_stack
            .iter()
            .rev()
            .copied()
            .find(|scope| {
                matches!(
                    self.scopes[scope.index()].kind,
                    TypedScopeKind::Module | TypedScopeKind::Function
                )
            })
            .expect("module scope exists")
    }

    fn freeze_visible(&mut self, direct_eval: bool, with_environment: bool) {
        for &scope in &self.scope_stack {
            self.scopes[scope.index()].frozen = true;
        }
        let current = self.current_scope();
        self.scopes[current.index()].contains_direct_eval |= direct_eval;
        self.scopes[current.index()].contains_with |= with_environment;
    }

    fn is_direct_eval(&self, callee: NodeId) -> bool {
        let Some(IrNodeData::Identifier { name }) =
            self.program.node(callee).map(|node| node.data())
        else {
            return false;
        };
        let Some(name_id) = self.name_id(*name) else {
            return false;
        };
        let name = self.program.name(name_id).expect("validated name");
        name.original() == "eval" && name.symbol().is_none() && name.role() == NameRole::Reference
    }

    fn is_unresolved_identifier(&self, node: NodeId) -> bool {
        let Some(IrNodeData::Identifier { name }) = self.program.node(node).map(|node| node.data())
        else {
            return false;
        };
        self.name_id(*name)
            .and_then(|name| self.program.name(name))
            .is_some_and(|name| name.symbol().is_none())
    }

    fn is_definite_primitive(&self, node: NodeId) -> bool {
        self.definite_primitive_kind(node).is_some()
    }

    fn definite_primitive_kind(&self, node: NodeId) -> Option<DefinitePrimitiveKind> {
        match self
            .program
            .node(node)
            .expect("validated typed IR node")
            .data()
        {
            IrNodeData::NumberLiteral { .. } => Some(DefinitePrimitiveKind::Number),
            IrNodeData::StringLiteral { .. } => Some(DefinitePrimitiveKind::String),
            IrNodeData::BooleanLiteral { .. } => Some(DefinitePrimitiveKind::Boolean),
            IrNodeData::NullLiteral => Some(DefinitePrimitiveKind::Null),
            IrNodeData::BigIntLiteral { .. } => Some(DefinitePrimitiveKind::BigInt),
            IrNodeData::TemplateLiteral { expressions, .. } => self
                .list_items(*expressions)
                .into_iter()
                .all(|expression| self.definite_primitive_kind(expression).is_some())
                .then_some(DefinitePrimitiveKind::String),
            IrNodeData::UnaryExpression { operator, argument } => match operator {
                UnaryOperator::LogicalNot | UnaryOperator::Delete => {
                    Some(DefinitePrimitiveKind::Boolean)
                }
                UnaryOperator::Typeof => Some(DefinitePrimitiveKind::String),
                UnaryOperator::Void => Some(DefinitePrimitiveKind::Undefined),
                UnaryOperator::Plus => (self.definite_primitive_kind(*argument)?
                    != DefinitePrimitiveKind::BigInt)
                    .then_some(DefinitePrimitiveKind::Number),
                UnaryOperator::Minus | UnaryOperator::BitwiseNot => Some(
                    if self.definite_primitive_kind(*argument)? == DefinitePrimitiveKind::BigInt {
                        DefinitePrimitiveKind::BigInt
                    } else {
                        DefinitePrimitiveKind::Number
                    },
                ),
            },
            IrNodeData::BinaryExpression {
                operator,
                left,
                right,
            } => known_binary_result(
                *operator,
                self.definite_primitive_kind(*left)?,
                self.definite_primitive_kind(*right)?,
            ),
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
            | IrNodeData::TemplateElement { .. }
            | IrNodeData::RegExpLiteral { .. }
            | IrNodeData::Name { .. }
            | IrNodeData::Identifier { .. }
            | IrNodeData::ThisExpression
            | IrNodeData::SuperExpression
            | IrNodeData::MetaProperty { .. }
            | IrNodeData::ArrayExpression { .. }
            | IrNodeData::Elision
            | IrNodeData::ObjectExpression { .. }
            | IrNodeData::ObjectProperty { .. }
            | IrNodeData::UpdateExpression { .. }
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

    fn assignment_target_is_external(&self, node: NodeId) -> bool {
        match self
            .program
            .node(node)
            .expect("validated typed IR node")
            .data()
        {
            IrNodeData::MemberExpression { .. } => true,
            IrNodeData::Identifier { name } => self
                .name_id(*name)
                .and_then(|name| self.program.name(name))
                .is_some_and(|name| name.symbol().is_none()),
            IrNodeData::ArrayExpression { .. } | IrNodeData::ObjectExpression { .. } => true,
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
            | IrNodeData::ThisExpression
            | IrNodeData::SuperExpression
            | IrNodeData::MetaProperty { .. }
            | IrNodeData::Elision
            | IrNodeData::ObjectProperty { .. }
            | IrNodeData::UnaryExpression { .. }
            | IrNodeData::UpdateExpression { .. }
            | IrNodeData::BinaryExpression { .. }
            | IrNodeData::LogicalExpression { .. }
            | IrNodeData::AssignmentExpression { .. }
            | IrNodeData::ConditionalExpression { .. }
            | IrNodeData::CallExpression { .. }
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
            | IrNodeData::AssignmentPattern { .. }
            | IrNodeData::RestPattern { .. }
            | IrNodeData::ImportDeclaration { .. }
            | IrNodeData::ImportSpecifier { .. }
            | IrNodeData::ImportAttributes { .. }
            | IrNodeData::ImportAttribute { .. }
            | IrNodeData::ExportNamedDeclaration { .. }
            | IrNodeData::ExportSpecifier { .. }
            | IrNodeData::ExportDefaultDeclaration { .. }
            | IrNodeData::ExportAllDeclaration { .. } => false,
        }
    }

    fn mark_escape(&mut self, root: NodeId, reason: EscapeReason) {
        let Some(root_scope) = self.node_scopes[root.index()] else {
            return;
        };
        let boundary = self.scopes[root_scope.index()].function_boundary;
        let mut symbols = BTreeSet::new();
        let Ok(nodes) = self.program.subtree_preorder(root) else {
            return;
        };
        for node in nodes {
            let Some(IrNodeData::Name { name }) = self.program.node(node).map(|node| node.data())
            else {
                continue;
            };
            let Some(name_use) = self.name_uses[name.index()] else {
                continue;
            };
            let Some(symbol) = name_use.symbol else {
                continue;
            };
            if !name_use.access.reads()
                || self.scopes[name_use.scope.index()].function_boundary != boundary
            {
                continue;
            }
            symbols.insert(symbol);
        }
        for symbol in symbols {
            let escape = &mut self.symbols[symbol as usize].escape;
            match reason {
                EscapeReason::PassedToUnknown => escape.passed_to_unknown = true,
                EscapeReason::ReturnOrThrow => escape.returned_or_thrown = true,
                EscapeReason::StoredExternally => escape.stored_externally = true,
                EscapeReason::Alias => escape.aliased = true,
            }
        }
    }

    fn finish_symbol_facts(&mut self) {
        for facts in &mut self.symbols {
            let Some(declaration_scope) = facts.declaration_scope else {
                continue;
            };
            facts.frozen = self.scopes[declaration_scope.index()].frozen;
            facts.escape.dynamically_observed = facts.frozen;
            let declaring_boundary = self.scopes[declaration_scope.index()].function_boundary;
            facts.escape.captured = facts.reads.iter().chain(&facts.writes).any(|name| {
                let Some(name_use) = self.name_uses[name.index()] else {
                    return false;
                };
                self.scopes[name_use.scope.index()].function_boundary != declaring_boundary
            });
            facts.declarations.sort_unstable();
            facts.declarations.dedup();
            facts.reads.sort_unstable();
            facts.reads.dedup();
            facts.writes.sort_unstable();
            facts.writes.dedup();
        }
        for scope in &mut self.scopes {
            scope.symbols.sort_unstable();
            scope.symbols.dedup();
        }
    }
}

struct CfgBuilder<'program, 'analysis> {
    program: &'program TypedProgram,
    scopes: &'analysis [TypedScopeFacts],
    node_scopes: &'analysis [Option<TypedScopeId>],
    name_uses: &'analysis [Option<NameUse>],
    symbols: &'analysis [TypedSymbolFacts],
    cfg: TypedControlFlowGraph,
    exit_stack: Vec<TypedCfgBlockId>,
    break_stack: Vec<TypedCfgBlockId>,
    continue_stack: Vec<TypedCfgBlockId>,
    finally_stack: Vec<TypedCfgBlockId>,
}

impl<'program, 'analysis> CfgBuilder<'program, 'analysis> {
    fn new(
        program: &'program TypedProgram,
        scopes: &'analysis [TypedScopeFacts],
        node_scopes: &'analysis [Option<TypedScopeId>],
        name_uses: &'analysis [Option<NameUse>],
        symbols: &'analysis [TypedSymbolFacts],
    ) -> Self {
        Self {
            program,
            scopes,
            node_scopes,
            name_uses,
            symbols,
            cfg: TypedControlFlowGraph::default(),
            exit_stack: Vec::new(),
            break_stack: Vec::new(),
            continue_stack: Vec::new(),
            finally_stack: Vec::new(),
        }
    }

    fn finish(mut self) -> TypedControlFlowGraph {
        self.cfg.edges.sort_unstable();
        self.cfg.edges.dedup();
        self.cfg
    }

    fn build_program(&mut self, root: NodeId) {
        let scope = self.node_scopes[root.index()].expect("program scope");
        let entry = self.new_block(None, TypedCfgBlockKind::Entry);
        let exit = self.new_block(None, TypedCfgBlockKind::Exit);
        self.cfg.roots.insert(entry, self.entry_initialized(scope));
        self.exit_stack.push(exit);
        let IrNodeData::Program { body, .. } = self
            .program
            .node(root)
            .expect("validated program root")
            .data()
        else {
            unreachable!("validated typed program root")
        };
        let open = self.build_sequence(*body, vec![entry]);
        self.connect_all(&open, exit, TypedCfgEdgeKind::Normal);
        self.exit_stack.pop();
    }

    fn build_function(&mut self, region: FunctionRegion) {
        let entry = self.new_block(Some(region.owner), TypedCfgBlockKind::FunctionEntry);
        let exit = self.new_block(Some(region.owner), TypedCfgBlockKind::FunctionExit);
        self.cfg
            .roots
            .insert(entry, self.entry_initialized(region.scope));
        self.exit_stack.push(exit);
        let open = match region.body_kind {
            ArrowBodyKind::Block => {
                let IrNodeData::FunctionBody { statements, .. } = self
                    .program
                    .node(region.body)
                    .expect("validated function body")
                    .data()
                else {
                    unreachable!("block function region must own FunctionBody")
                };
                self.build_sequence(*statements, vec![entry])
            }
            ArrowBodyKind::Expression => {
                let block = self.eval_block(region.body, TypedCfgBlockKind::Statement, vec![entry]);
                self.add_edge(block, exit, TypedCfgEdgeKind::Normal);
                Vec::new()
            }
        };
        self.connect_all(&open, exit, TypedCfgEdgeKind::Normal);
        self.exit_stack.pop();
    }

    fn entry_initialized(&self, scope: TypedScopeId) -> BTreeSet<SymbolId> {
        let boundary = self.scopes[scope.index()].function_boundary;
        self.symbols
            .iter()
            .filter(|facts| {
                facts.declaration_scope.is_some_and(|declaration_scope| {
                    self.scopes[declaration_scope.index()].function_boundary == boundary
                }) && self.program.symbol(facts.symbol).is_some_and(|symbol| {
                    matches!(
                        symbol.decl_kind(),
                        DeclKind::Var | DeclKind::Function | DeclKind::Import | DeclKind::Param
                    )
                })
            })
            .map(|facts| facts.symbol)
            .collect()
    }

    fn new_block(&mut self, _owner: Option<NodeId>, kind: TypedCfgBlockKind) -> TypedCfgBlockId {
        let id = TypedCfgBlockId(self.cfg.blocks.len() as u32);
        self.cfg.blocks.push(TypedCfgBlock {
            id,
            kind,
            events: Vec::new(),
        });
        id
    }

    fn add_edge(&mut self, from: TypedCfgBlockId, to: TypedCfgBlockId, kind: TypedCfgEdgeKind) {
        self.cfg.edges.push(TypedCfgEdge { from, to, kind });
    }

    fn connect_all(
        &mut self,
        from: &[TypedCfgBlockId],
        to: TypedCfgBlockId,
        kind: TypedCfgEdgeKind,
    ) {
        for &from in from {
            self.add_edge(from, to, kind);
        }
    }

    fn eval_block(
        &mut self,
        owner: NodeId,
        kind: TypedCfgBlockKind,
        incoming: Vec<TypedCfgBlockId>,
    ) -> TypedCfgBlockId {
        let block = self.new_block(Some(owner), kind);
        self.connect_all(&incoming, block, TypedCfgEdgeKind::Normal);
        let reads = self.read_events(owner);
        self.cfg.blocks[block.index()].events.extend(reads);
        block
    }

    fn empty_gate(
        &mut self,
        owner: NodeId,
        incoming: TypedCfgBlockId,
        edge: TypedCfgEdgeKind,
    ) -> TypedCfgBlockId {
        let gate = self.new_block(Some(owner), TypedCfgBlockKind::Statement);
        self.add_edge(incoming, gate, edge);
        gate
    }

    fn read_events(&self, root: NodeId) -> Vec<FlowEvent> {
        let Some(root_scope) = self.node_scopes[root.index()] else {
            return Vec::new();
        };
        let boundary = self.scopes[root_scope.index()].function_boundary;
        self.program
            .subtree_preorder(root)
            .expect("CFG reads a validated live subtree")
            .into_iter()
            .filter_map(|node| {
                let IrNodeData::Name { name } = self.program.node(node)?.data() else {
                    return None;
                };
                let name_use = self.name_uses[name.index()]?;
                if !name_use.access.reads()
                    || self.scopes[name_use.scope.index()].function_boundary != boundary
                {
                    return None;
                }
                Some(FlowEvent::Read {
                    name: name_use.name,
                    symbol: name_use.symbol?,
                })
            })
            .collect()
    }

    fn declaration_symbols(&self, root: NodeId) -> Vec<SymbolId> {
        let mut symbols = self
            .program
            .subtree_preorder(root)
            .expect("CFG declarations read a validated live subtree")
            .into_iter()
            .filter_map(|node| {
                let IrNodeData::Name { name } = self.program.node(node)?.data() else {
                    return None;
                };
                let name_use = self.name_uses[name.index()]?;
                (name_use.access == NameAccess::Declaration)
                    .then_some(name_use.symbol)
                    .flatten()
            })
            .collect::<Vec<_>>();
        symbols.sort_unstable();
        symbols.dedup();
        symbols
    }

    fn build_sequence(
        &mut self,
        list: ListId,
        mut incoming: Vec<TypedCfgBlockId>,
    ) -> Vec<TypedCfgBlockId> {
        let statements = self
            .program
            .list(list)
            .expect("validated statement list")
            .items()
            .to_vec();
        for statement in statements {
            incoming = self.build_statement(statement, incoming);
        }
        incoming
    }

    fn build_statement(
        &mut self,
        node: NodeId,
        incoming: Vec<TypedCfgBlockId>,
    ) -> Vec<TypedCfgBlockId> {
        let data = self
            .program
            .node(node)
            .expect("validated current statement")
            .data()
            .clone();
        match data {
            IrNodeData::Block { body }
            | IrNodeData::FunctionBody {
                statements: body, ..
            }
            | IrNodeData::StaticBlock { body } => self.build_sequence(body, incoming),
            IrNodeData::VariableDeclaration { declarations, .. } => {
                let mut open = incoming;
                for declarator in self
                    .program
                    .list(declarations)
                    .expect("validated declarations")
                    .items()
                    .to_vec()
                {
                    let block = self.eval_block(declarator, TypedCfgBlockKind::Statement, open);
                    for symbol in self.declaration_symbols(declarator) {
                        let kind = self
                            .program
                            .symbol(symbol)
                            .expect("typed symbol")
                            .decl_kind();
                        if !matches!(kind, DeclKind::Var | DeclKind::Function | DeclKind::Import) {
                            self.cfg.blocks[block.index()]
                                .events
                                .push(FlowEvent::Initialize(symbol));
                        }
                    }
                    open = vec![block];
                }
                open
            }
            IrNodeData::Function { .. } => {
                let block = self.eval_block(node, TypedCfgBlockKind::Statement, incoming);
                vec![block]
            }
            IrNodeData::Class { name, .. } => {
                let block = self.eval_block(node, TypedCfgBlockKind::Statement, incoming);
                if let Some(name) = name {
                    for symbol in self.declaration_symbols(name) {
                        self.cfg.blocks[block.index()]
                            .events
                            .push(FlowEvent::Initialize(symbol));
                    }
                }
                vec![block]
            }
            IrNodeData::EmptyStatement => incoming,
            IrNodeData::DebuggerStatement | IrNodeData::ExpressionStatement { .. } => {
                let block = self.eval_block(node, TypedCfgBlockKind::Statement, incoming);
                vec![block]
            }
            IrNodeData::IfStatement {
                test,
                consequent,
                alternate,
            } => {
                let condition = self.eval_block(test, TypedCfgBlockKind::Condition, incoming);
                let true_gate = self.empty_gate(consequent, condition, TypedCfgEdgeKind::True);
                let mut open = self.build_statement(consequent, vec![true_gate]);
                if let Some(alternate) = alternate {
                    let false_gate = self.empty_gate(alternate, condition, TypedCfgEdgeKind::False);
                    open.extend(self.build_statement(alternate, vec![false_gate]));
                } else {
                    let false_gate = self.empty_gate(node, condition, TypedCfgEdgeKind::False);
                    open.push(false_gate);
                }
                open
            }
            IrNodeData::ForStatement {
                initializer,
                initializer_kind: _,
                test,
                update,
                body,
            } => {
                let mut open = incoming;
                if let Some(initializer) = initializer {
                    open = self.build_statement(initializer, open);
                }
                let header_owner = test.or(initializer).unwrap_or(node);
                let header = self.eval_block(header_owner, TypedCfgBlockKind::LoopHeader, open);
                let exit = self.new_block(Some(node), TypedCfgBlockKind::LoopExit);
                self.add_edge(header, exit, TypedCfgEdgeKind::False);
                self.break_stack.push(exit);
                self.continue_stack.push(header);
                let body_gate = self.empty_gate(body, header, TypedCfgEdgeKind::True);
                let mut body_open = self.build_statement(body, vec![body_gate]);
                if let Some(update) = update {
                    let update_block =
                        self.eval_block(update, TypedCfgBlockKind::Statement, body_open);
                    body_open = vec![update_block];
                }
                self.connect_all(&body_open, header, TypedCfgEdgeKind::Loop);
                self.continue_stack.pop();
                self.break_stack.pop();
                vec![exit]
            }
            IrNodeData::ForInStatement {
                left,
                left_kind: _,
                right,
                body,
            }
            | IrNodeData::ForOfStatement {
                left,
                left_kind: _,
                right,
                body,
                is_await: _,
            } => {
                let header = self.eval_block(right, TypedCfgBlockKind::LoopHeader, incoming);
                let exit = self.new_block(Some(node), TypedCfgBlockKind::LoopExit);
                self.add_edge(header, exit, TypedCfgEdgeKind::False);
                self.break_stack.push(exit);
                self.continue_stack.push(header);
                let left_block = self.eval_block(left, TypedCfgBlockKind::Statement, vec![header]);
                for symbol in self.declaration_symbols(left) {
                    self.cfg.blocks[left_block.index()]
                        .events
                        .push(FlowEvent::Initialize(symbol));
                }
                let body_open = self.build_statement(body, vec![left_block]);
                self.connect_all(&body_open, header, TypedCfgEdgeKind::Loop);
                self.continue_stack.pop();
                self.break_stack.pop();
                vec![exit]
            }
            IrNodeData::WhileStatement { test, body } => {
                let header = self.eval_block(test, TypedCfgBlockKind::LoopHeader, incoming);
                let exit = self.new_block(Some(node), TypedCfgBlockKind::LoopExit);
                self.add_edge(header, exit, TypedCfgEdgeKind::False);
                self.break_stack.push(exit);
                self.continue_stack.push(header);
                let gate = self.empty_gate(body, header, TypedCfgEdgeKind::True);
                let body_open = self.build_statement(body, vec![gate]);
                self.connect_all(&body_open, header, TypedCfgEdgeKind::Loop);
                self.continue_stack.pop();
                self.break_stack.pop();
                vec![exit]
            }
            IrNodeData::DoWhileStatement { body, test } => {
                let header = self.new_block(Some(node), TypedCfgBlockKind::LoopHeader);
                self.connect_all(&incoming, header, TypedCfgEdgeKind::Normal);
                let exit = self.new_block(Some(node), TypedCfgBlockKind::LoopExit);
                self.break_stack.push(exit);
                self.continue_stack.push(header);
                let body_open = self.build_statement(body, vec![header]);
                let condition = self.eval_block(test, TypedCfgBlockKind::Condition, body_open);
                self.add_edge(condition, header, TypedCfgEdgeKind::Loop);
                self.add_edge(condition, exit, TypedCfgEdgeKind::False);
                self.continue_stack.pop();
                self.break_stack.pop();
                vec![exit]
            }
            IrNodeData::SwitchStatement {
                discriminant,
                cases,
            } => {
                let condition =
                    self.eval_block(discriminant, TypedCfgBlockKind::Condition, incoming);
                let exit = self.new_block(Some(node), TypedCfgBlockKind::SwitchExit);
                self.break_stack.push(exit);
                let mut had_default = false;
                for case in self
                    .program
                    .list(cases)
                    .expect("validated switch cases")
                    .items()
                    .to_vec()
                {
                    let IrNodeData::SwitchCase { test, consequent } = self
                        .program
                        .node(case)
                        .expect("validated switch case")
                        .data()
                    else {
                        unreachable!("switch case list grammar")
                    };
                    had_default |= test.is_none();
                    let gate = self.empty_gate(case, condition, TypedCfgEdgeKind::True);
                    let open = self.build_sequence(*consequent, vec![gate]);
                    self.connect_all(&open, exit, TypedCfgEdgeKind::Normal);
                }
                if !had_default {
                    self.add_edge(condition, exit, TypedCfgEdgeKind::False);
                }
                self.break_stack.pop();
                vec![exit]
            }
            IrNodeData::SwitchCase {
                test: _,
                consequent,
            } => self.build_sequence(consequent, incoming),
            IrNodeData::ReturnStatement { argument: _ } => {
                let block = self.eval_block(node, TypedCfgBlockKind::Statement, incoming);
                let target = self
                    .finally_stack
                    .last()
                    .or_else(|| self.exit_stack.last())
                    .copied();
                if let Some(target) = target {
                    let kind = if self.finally_stack.is_empty() {
                        TypedCfgEdgeKind::Normal
                    } else {
                        TypedCfgEdgeKind::Finally
                    };
                    self.add_edge(block, target, kind);
                }
                Vec::new()
            }
            IrNodeData::ThrowStatement { argument: _ } => {
                let block = self.eval_block(node, TypedCfgBlockKind::Statement, incoming);
                let target = self
                    .finally_stack
                    .last()
                    .or_else(|| self.exit_stack.last())
                    .copied();
                if let Some(target) = target {
                    let kind = if self.finally_stack.is_empty() {
                        TypedCfgEdgeKind::Exception
                    } else {
                        TypedCfgEdgeKind::Finally
                    };
                    self.add_edge(block, target, kind);
                }
                Vec::new()
            }
            IrNodeData::BreakStatement { label: _ } => {
                let block = self.eval_block(node, TypedCfgBlockKind::Statement, incoming);
                if let Some(target) = self.break_stack.last().copied() {
                    self.add_edge(block, target, TypedCfgEdgeKind::Normal);
                }
                Vec::new()
            }
            IrNodeData::ContinueStatement { label: _ } => {
                let block = self.eval_block(node, TypedCfgBlockKind::Statement, incoming);
                if let Some(target) = self.continue_stack.last().copied() {
                    self.add_edge(block, target, TypedCfgEdgeKind::Loop);
                }
                Vec::new()
            }
            IrNodeData::TryStatement {
                block,
                handler,
                finalizer,
            } => self.build_try(node, block, handler, finalizer, incoming),
            IrNodeData::CatchClause { parameter, body } => {
                let entry = self.new_block(Some(node), TypedCfgBlockKind::CatchEntry);
                self.connect_all(&incoming, entry, TypedCfgEdgeKind::Normal);
                if let Some(parameter) = parameter {
                    for symbol in self.declaration_symbols(parameter) {
                        self.cfg.blocks[entry.index()]
                            .events
                            .push(FlowEvent::Initialize(symbol));
                    }
                }
                self.build_statement(body, vec![entry])
            }
            IrNodeData::LabeledStatement { label: _, body }
            | IrNodeData::WithStatement { object: _, body } => {
                let block = self.eval_block(node, TypedCfgBlockKind::Statement, incoming);
                self.build_statement(body, vec![block])
            }
            IrNodeData::ExportNamedDeclaration {
                declaration,
                specifiers: _,
                source: _,
                attributes: _,
            } => {
                if let Some(declaration) = declaration {
                    self.build_statement(declaration, incoming)
                } else {
                    let block = self.eval_block(node, TypedCfgBlockKind::Statement, incoming);
                    vec![block]
                }
            }
            IrNodeData::ExportDefaultDeclaration { value, kind: _ } => {
                self.build_statement(value, incoming)
            }
            IrNodeData::ImportDeclaration { .. }
            | IrNodeData::ExportAllDeclaration { .. }
            | IrNodeData::VariableDeclarator { .. }
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
            | IrNodeData::ArrayPattern { .. }
            | IrNodeData::ObjectPattern { .. }
            | IrNodeData::ObjectPatternProperty { .. }
            | IrNodeData::AssignmentPattern { .. }
            | IrNodeData::RestPattern { .. }
            | IrNodeData::ImportSpecifier { .. }
            | IrNodeData::ImportAttributes { .. }
            | IrNodeData::ImportAttribute { .. }
            | IrNodeData::ExportSpecifier { .. } => {
                let block = self.eval_block(node, TypedCfgBlockKind::Statement, incoming);
                vec![block]
            }
            IrNodeData::Program { .. } => unreachable!("nested Program node"),
        }
    }

    fn build_try(
        &mut self,
        owner: NodeId,
        block: NodeId,
        handler: Option<NodeId>,
        finalizer: Option<NodeId>,
        incoming: Vec<TypedCfgBlockId>,
    ) -> Vec<TypedCfgBlockId> {
        let dispatch = self.new_block(Some(owner), TypedCfgBlockKind::TryDispatch);
        self.connect_all(&incoming, dispatch, TypedCfgEdgeKind::Normal);
        let finally_entry =
            finalizer.map(|_| self.new_block(Some(owner), TypedCfgBlockKind::FinallyEntry));
        if let Some(finally_entry) = finally_entry {
            self.finally_stack.push(finally_entry);
        }
        let try_gate = self.empty_gate(block, dispatch, TypedCfgEdgeKind::Normal);
        let mut open = self.build_statement(block, vec![try_gate]);
        if let Some(handler) = handler {
            let catch_gate = self.new_block(Some(handler), TypedCfgBlockKind::CatchEntry);
            self.add_edge(dispatch, catch_gate, TypedCfgEdgeKind::Exception);
            open.extend(self.build_statement(handler, vec![catch_gate]));
        } else if let Some(finally_entry) = finally_entry {
            self.add_edge(dispatch, finally_entry, TypedCfgEdgeKind::Exception);
        } else if let Some(exit) = self.exit_stack.last().copied() {
            self.add_edge(dispatch, exit, TypedCfgEdgeKind::Exception);
        }
        if finally_entry.is_some() {
            self.finally_stack.pop();
        }
        if let (Some(finalizer), Some(finally_entry)) = (finalizer, finally_entry) {
            self.connect_all(&open, finally_entry, TypedCfgEdgeKind::Finally);
            self.build_statement(finalizer, vec![finally_entry])
        } else {
            open
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DenseSymbolSet {
    words: Vec<u64>,
}

impl DenseSymbolSet {
    fn empty(symbol_count: usize) -> Self {
        Self {
            words: vec![0; symbol_count.div_ceil(u64::BITS as usize)],
        }
    }

    fn from_symbols<'a>(
        symbol_count: usize,
        symbols: impl IntoIterator<Item = &'a SymbolId>,
    ) -> Self {
        let mut set = Self::empty(symbol_count);
        for &symbol in symbols {
            set.insert(symbol);
        }
        set
    }

    fn insert(&mut self, symbol: SymbolId) {
        let index = symbol as usize;
        if let Some(word) = self.words.get_mut(index / u64::BITS as usize) {
            *word |= 1_u64 << (index % u64::BITS as usize);
        }
    }

    fn contains(&self, symbol: SymbolId) -> bool {
        let index = symbol as usize;
        self.words
            .get(index / u64::BITS as usize)
            .is_some_and(|word| word & (1_u64 << (index % u64::BITS as usize)) != 0)
    }

    fn intersect_with(&mut self, other: &Self) {
        for (word, other) in self.words.iter_mut().zip(&other.words) {
            *word &= *other;
        }
    }
}

fn solve_definite_initialization(
    cfg: &TypedControlFlowGraph,
    name_count: usize,
    symbol_count: usize,
) -> Vec<Option<bool>> {
    let mut all_symbols = DenseSymbolSet::empty(symbol_count);
    for initialized in cfg.roots.values() {
        for &symbol in initialized {
            all_symbols.insert(symbol);
        }
    }
    for block in &cfg.blocks {
        for event in &block.events {
            match *event {
                FlowEvent::Read { symbol, .. } | FlowEvent::Initialize(symbol) => {
                    all_symbols.insert(symbol);
                }
            }
        }
    }
    let mut predecessors = vec![Vec::new(); cfg.blocks.len()];
    for edge in &cfg.edges {
        predecessors[edge.to.index()].push(edge.from);
    }
    let mut root_sets = vec![None; cfg.blocks.len()];
    for (&root, initialized) in &cfg.roots {
        root_sets[root.index()] = Some(DenseSymbolSet::from_symbols(
            symbol_count,
            initialized.iter(),
        ));
    }
    let mut incoming = vec![all_symbols.clone(); cfg.blocks.len()];
    let mut outgoing = vec![all_symbols; cfg.blocks.len()];
    for (root, initialized) in root_sets.iter().enumerate() {
        let Some(initialized) = initialized else {
            continue;
        };
        incoming[root] = initialized.clone();
        outgoing[root] = transfer(initialized, &cfg.blocks[root]);
    }

    loop {
        let mut changed = false;
        for block in &cfg.blocks {
            let next_in = if let Some(initialized) = &root_sets[block.id.index()] {
                initialized.clone()
            } else if let Some((&first, rest)) = predecessors[block.id.index()].split_first() {
                let mut intersection = outgoing[first.index()].clone();
                for predecessor in rest {
                    intersection.intersect_with(&outgoing[predecessor.index()]);
                }
                intersection
            } else {
                DenseSymbolSet::empty(symbol_count)
            };
            let next_out = transfer(&next_in, block);
            if incoming[block.id.index()] != next_in || outgoing[block.id.index()] != next_out {
                incoming[block.id.index()] = next_in;
                outgoing[block.id.index()] = next_out;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut reads = vec![None; name_count];
    for block in &cfg.blocks {
        let mut initialized = incoming[block.id.index()].clone();
        for event in &block.events {
            match *event {
                FlowEvent::Read { name, symbol } => {
                    let current = initialized.contains(symbol);
                    reads[name.index()] = Some(reads[name.index()].unwrap_or(true) && current);
                }
                FlowEvent::Initialize(symbol) => {
                    initialized.insert(symbol);
                }
            }
        }
    }
    reads
}

fn transfer(input: &DenseSymbolSet, block: &TypedCfgBlock) -> DenseSymbolSet {
    let mut output = input.clone();
    for event in &block.events {
        if let FlowEvent::Initialize(symbol) = *event {
            output.insert(symbol);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use wake_common::Interner;
    use wake_ecma_ast::SourceType;

    use super::*;

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

    fn root_body(program: &TypedProgram) -> ListId {
        match program.node(program.root()).unwrap().data() {
            IrNodeData::Program { body, .. } => *body,
            other => panic!("expected Program root, found {other:?}"),
        }
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

    fn read_names(
        program: &TypedProgram,
        analysis: &TypedAnalysis,
        symbol: SymbolId,
    ) -> Vec<NameId> {
        let mut reads = analysis.symbol(symbol).unwrap().reads().to_vec();
        reads.sort_unstable_by_key(|name| {
            analysis
                .name_use(*name)
                .map(|name_use| name_use.node().index())
                .unwrap_or(usize::MAX)
        });
        assert!(reads.iter().all(|name| {
            program
                .name(*name)
                .is_some_and(|name| name.symbol() == Some(symbol))
        }));
        reads
    }

    #[test]
    fn rebuild_after_structural_deletion_drops_references_and_cfg_nodes() {
        let mut program = lower_source(
            "let retained=1;consume(retained);consume(2);",
            SourceType::Script,
        );
        let symbol = symbol_named(&program, "retained");
        let before = TypedAnalysis::rebuild(&program).unwrap();
        assert_eq!(before.reference_count(symbol), 1);
        let before_blocks = before.cfg().blocks().len();

        let body = root_body(&program);
        let removed = program.splice_list(body, 1..2, &[]).unwrap();
        assert_eq!(removed.len(), 1);
        let after = TypedAnalysis::rebuild(&program).unwrap();
        assert_eq!(after.revision(), program.revision());
        assert_eq!(after.reference_count(symbol), 0);
        assert!(after.cfg().blocks().len() < before_blocks);
        assert!(program.node(removed[0]).unwrap().is_tombstone());
    }

    #[test]
    fn closure_capture_is_derived_from_function_boundaries() {
        let program = lower_source(
            "let captured=1;function read(){return captured}",
            SourceType::Script,
        );
        let symbol = symbol_named(&program, "captured");
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        let facts = analysis.symbol(symbol).unwrap();
        assert!(facts.escape().captured());
        assert!(facts.escape().returned_or_thrown());
        let read = facts.reads()[0];
        assert_ne!(
            analysis
                .scope(facts.declaration_scope().unwrap())
                .unwrap()
                .function_boundary(),
            analysis
                .scope(analysis.name_use(read).unwrap().scope())
                .unwrap()
                .function_boundary()
        );
    }

    #[test]
    fn direct_eval_freezes_visible_environment_but_not_sibling_scope() {
        let program = lower_source(
            "function guarded(){let frozen=1;eval('frozen')}function sibling(){let free=2;return free}",
            SourceType::Script,
        );
        let frozen = symbol_named(&program, "frozen");
        let free = symbol_named(&program, "free");
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        assert!(analysis.symbol(frozen).unwrap().is_frozen());
        assert!(
            analysis
                .symbol(frozen)
                .unwrap()
                .escape()
                .dynamically_observed()
        );
        assert!(!analysis.symbol(free).unwrap().is_frozen());
        assert!(
            analysis
                .scopes()
                .iter()
                .any(|scope| scope.contains_direct_eval())
        );
    }

    #[test]
    fn with_freezes_only_its_visible_and_nested_local_environment() {
        let program = lower_source(
            "function guarded(box){let visible=1;with(box){let nested=2;visible+nested}}function sibling(){let free=3;return free}",
            SourceType::Script,
        );
        let visible = symbol_named(&program, "visible");
        let nested = symbol_named(&program, "nested");
        let free = symbol_named(&program, "free");
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        assert!(analysis.symbol(visible).unwrap().is_frozen());
        assert!(analysis.symbol(nested).unwrap().is_frozen());
        assert!(!analysis.symbol(free).unwrap().is_frozen());
        assert!(analysis.scopes().iter().any(|scope| scope.contains_with()));
    }

    #[test]
    fn tdz_solver_distinguishes_reads_before_and_after_initialization() {
        let program = lower_source(
            "consume(value);let value=1;consume(value);",
            SourceType::Script,
        );
        let value = symbol_named(&program, "value");
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        let reads = read_names(&program, &analysis, value);
        assert_eq!(reads.len(), 2);
        assert_eq!(
            analysis.read_is_definitely_initialized(reads[0]),
            Some(false)
        );
        assert_eq!(
            analysis.read_is_definitely_initialized(reads[1]),
            Some(true)
        );
    }

    #[test]
    fn dense_tdz_state_tracks_symbols_across_multiple_machine_words() {
        let mut source = String::new();
        for index in 0..130 {
            source.push_str(&format!("let filler{index}={index};"));
        }
        source.push_str("consume(wide);let wide=1;consume(wide);");
        let program = lower_source(&source, SourceType::Script);
        let wide = symbol_named(&program, "wide");
        assert!(
            wide >= 128,
            "fixture must cross two dense-set word boundaries"
        );
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        let reads = read_names(&program, &analysis, wide);
        assert_eq!(reads.len(), 2);
        assert_eq!(
            analysis.read_is_definitely_initialized(reads[0]),
            Some(false)
        );
        assert_eq!(
            analysis.read_is_definitely_initialized(reads[1]),
            Some(true)
        );
    }

    #[test]
    fn member_getter_unknown_call_and_proxy_construction_stay_observable() {
        let program = lower_source(
            "const object={get value(){return 1}};object.value;unknown(object);new Proxy(object,{});",
            SourceType::Script,
        );
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        let member = program
            .nodes()
            .iter()
            .find(|node| matches!(node.data(), IrNodeData::MemberExpression { .. }))
            .unwrap()
            .id();
        let member_effect = analysis.effect(member).unwrap();
        assert!(member_effect.reads_member());
        assert!(member_effect.may_have_side_effects());
        assert!(member_effect.may_throw());

        for node in program.nodes().iter().filter(|node| {
            matches!(
                node.data(),
                IrNodeData::CallExpression { .. } | IrNodeData::NewExpression { .. }
            )
        }) {
            let effect = analysis.effect(node.id()).unwrap();
            assert!(effect.calls_unknown());
            assert!(effect.may_have_side_effects());
            assert!(effect.may_throw());
        }
    }

    #[test]
    fn primitive_coercion_and_throw_boundaries_are_conservative() {
        let program = lower_source(
            "1n+1;+1n;null in null;null instanceof null;1+2;1n+2n;",
            SourceType::Script,
        );
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        let mut mixed_bigint = None;
        let mut bigint_add = None;
        let mut numeric_add = None;
        let mut unary_bigint = None;
        let mut throwing_relations = Vec::new();
        for node in program.preorder().unwrap() {
            match program.node(node).unwrap().data() {
                IrNodeData::BinaryExpression {
                    operator: BinaryOperator::Add,
                    left,
                    right,
                } => match (
                    program.node(*left).unwrap().data(),
                    program.node(*right).unwrap().data(),
                ) {
                    (IrNodeData::BigIntLiteral { .. }, IrNodeData::NumberLiteral { .. }) => {
                        mixed_bigint = Some(node)
                    }
                    (IrNodeData::BigIntLiteral { .. }, IrNodeData::BigIntLiteral { .. }) => {
                        bigint_add = Some(node)
                    }
                    (IrNodeData::NumberLiteral { .. }, IrNodeData::NumberLiteral { .. }) => {
                        numeric_add = Some(node)
                    }
                    _ => {}
                },
                IrNodeData::UnaryExpression {
                    operator: UnaryOperator::Plus,
                    argument,
                } if matches!(
                    program.node(*argument).map(|node| node.data()),
                    Some(IrNodeData::BigIntLiteral { .. })
                ) =>
                {
                    unary_bigint = Some(node)
                }
                IrNodeData::BinaryExpression {
                    operator: BinaryOperator::In | BinaryOperator::Instanceof,
                    ..
                } => throwing_relations.push(node),
                _ => {}
            }
        }
        for node in [mixed_bigint.unwrap(), unary_bigint.unwrap()]
            .into_iter()
            .chain(throwing_relations)
        {
            let effect = analysis.effect(node).unwrap();
            assert!(effect.may_throw(), "node {node:?} lost its throw boundary");
            assert!(effect.may_have_side_effects());
        }
        for node in [numeric_add.unwrap(), bigint_add.unwrap()] {
            let effect = analysis.effect(node).unwrap();
            assert!(!effect.may_throw());
            assert!(!effect.may_have_side_effects());
        }
    }

    #[test]
    fn await_yield_and_await_for_of_are_suspension_points() {
        let program = lower_source(
            "async function load(value){await value;for await(const item of value){yieldValue(item)}}function* generate(value){yield value}",
            SourceType::Script,
        );
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        let suspensions = program
            .nodes()
            .iter()
            .filter(|node| {
                matches!(
                    node.data(),
                    IrNodeData::AwaitExpression { .. }
                        | IrNodeData::YieldExpression { .. }
                        | IrNodeData::ForOfStatement { is_await: true, .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(suspensions.len(), 3);
        for node in suspensions {
            let effect = analysis.effect(node.id()).unwrap();
            assert!(effect.suspends());
            assert!(effect.may_have_side_effects());
            assert!(effect.may_throw());
        }
    }

    #[test]
    fn try_finally_cfg_has_normal_exception_and_finally_paths() {
        let program = lower_source(
            "function work(flag){try{if(flag)return step();throw fail()}catch(error){recover(error)}finally{cleanup()}}",
            SourceType::Script,
        );
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        assert!(
            analysis
                .cfg()
                .blocks()
                .iter()
                .any(|block| block.kind() == TypedCfgBlockKind::TryDispatch)
        );
        let finally = analysis
            .cfg()
            .blocks()
            .iter()
            .find(|block| block.kind() == TypedCfgBlockKind::FinallyEntry)
            .expect("finally entry")
            .id();
        assert!(
            analysis
                .cfg()
                .edges()
                .iter()
                .any(|edge| { edge.to() == finally && edge.kind() == TypedCfgEdgeKind::Finally })
        );
        assert!(
            analysis
                .cfg()
                .edges()
                .iter()
                .any(|edge| { edge.kind() == TypedCfgEdgeKind::Exception })
        );
    }
}
