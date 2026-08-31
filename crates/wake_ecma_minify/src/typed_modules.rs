//! Structural ESM/CommonJS planning and final module-request lowering for [`TypedProgram`].
//!
//! Abstract module requests are ordinary call nodes whose callee carries a plan-owned sentinel
//! [`SymbolId`]. This keeps every decision inside the owned IR/symbol model without adding source
//! strings or span overlays. Finalization recognizes sentinels by symbol identity, replaces every
//! request transactionally, and finalization rejects any live sentinel before typed codegen.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::fmt;

use wake_ecma_ast::{BinaryOperator, LogicalOperator, VarKind};
use wake_ecma_semantic::{DeclKind, SymbolId};

use crate::typed_analysis::{NameAccess, TypedAnalysis};
use crate::typed_ir::{
    ChildRole, ClassContext, DerivedOriginKind, ExportDefaultValueKind, FunctionContext,
    ImportSpecifierKind, IrModuleName, IrNode, IrNodeData, IrOrigin, IrPropertyKey, ListId,
    ModuleNameKind, NameId, NameRole, NameSyntax, NodeId, SyntheticOriginKind, TypedIrError,
    TypedProgram,
};
use crate::typed_lowering::{Binding, SyntheticFactory};

/// Stable linker module identity; unlike `NodeId`, it may cross module-task boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypedModuleId(pub u32);

/// Stable dynamic chunk identity supplied only after chunking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypedChunkId(pub u32);

/// One linker-owned live symbol root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypedModuleSymbol {
    pub module: TypedModuleId,
    pub symbol: SymbolId,
}

/// Monotonic linker liveness input. Planning may retain a subset but never manufactures roots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypedLinkerLiveness {
    pub roots: BTreeSet<TypedModuleSymbol>,
}

impl TypedLinkerLiveness {
    pub fn insert(&mut self, module: TypedModuleId, symbol: SymbolId) {
        self.roots.insert(TypedModuleSymbol { module, symbol });
    }

    pub fn contains(&self, module: TypedModuleId, symbol: SymbolId) -> bool {
        self.roots.contains(&TypedModuleSymbol { module, symbol })
    }
}

/// Module syntax retained for output or lowered into a CommonJS/runtime contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypedModuleMode {
    PreserveEsm,
    PreserveCommonJs,
    BundledCommonJs,
}

/// Optimizer-stage policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedModuleOptions {
    pub mode: TypedModuleMode,
    pub module_id: TypedModuleId,
    /// `true` represents the absence of linker liveness information. `false` makes `roots`
    /// authoritative, including the meaningful empty set which removes every unused export.
    pub preserve_all_exports: bool,
    /// Preserve plain `export *` forwarding unless the linker authoritatively reports that this
    /// module has no consumed public names. Star-provided names have no module-local `SymbolId`,
    /// so this proof cannot be reconstructed from `linker_liveness.roots` alone.
    pub preserve_export_star: bool,
    /// Exact public export names observed by the linker when `preserve_all_exports` is false.
    /// Source re-exports have no module-local `SymbolId`, so their liveness must remain keyed by
    /// the public name rather than being reconstructed from `linker_liveness.roots`.
    pub observed_export_names: BTreeSet<String>,
    pub linker_liveness: TypedLinkerLiveness,
}

impl Default for TypedModuleOptions {
    fn default() -> Self {
        Self {
            mode: TypedModuleMode::PreserveEsm,
            module_id: TypedModuleId(0),
            preserve_all_exports: true,
            preserve_export_star: true,
            observed_export_names: BTreeSet::new(),
            linker_liveness: TypedLinkerLiveness::default(),
        }
    }
}

/// Static or dynamic abstract dependency edge in source order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypedModuleRequestKind {
    StaticImport,
    DynamicImport,
    Require,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedModuleRequestEdge {
    pub specifier: String,
    pub kind: TypedModuleRequestKind,
    pub origin: IrOrigin,
}

/// One collision-free free binding expected from the generated module wrapper/runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedRuntimeBinding {
    pub symbol: SymbolId,
    pub original_name: String,
}

/// All wrapper/runtime bindings owned by one module plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedModuleRuntimeBindings {
    pub exports: TypedRuntimeBinding,
    pub export_live: TypedRuntimeBinding,
    pub export_all: TypedRuntimeBinding,
    pub mark_esmodule: TypedRuntimeBinding,
    pub internal_require: TypedRuntimeBinding,
    pub internal_require_async: TypedRuntimeBinding,
    pub internal_import: TypedRuntimeBinding,
    pub external_require: TypedRuntimeBinding,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NamespaceRequest {
    symbol: SymbolId,
    specifier: String,
    requires_namespace_interop: bool,
}

/// Optimizer-owned module plan. It contains symbol identities, never source-span decisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedModulePlan {
    mode: TypedModuleMode,
    module_id: TypedModuleId,
    preserve_all_exports: bool,
    preserve_export_star: bool,
    observed_export_names: BTreeSet<String>,
    prepared_revision: u64,
    sealed_revision: Option<u64>,
    static_import_request: TypedRuntimeBinding,
    require_request: TypedRuntimeBinding,
    dynamic_request: TypedRuntimeBinding,
    default_read: TypedRuntimeBinding,
    namespace_read: TypedRuntimeBinding,
    runtime: TypedModuleRuntimeBindings,
    namespace_requests: Vec<NamespaceRequest>,
    local_bindings: Vec<TypedRuntimeBinding>,
    retained_liveness: BTreeSet<TypedModuleSymbol>,
    requests: Vec<TypedModuleRequestEdge>,
    has_top_level_await: bool,
    had_esm: bool,
    finalized: bool,
}

impl TypedModulePlan {
    pub const fn mode(&self) -> TypedModuleMode {
        self.mode
    }

    pub const fn module_id(&self) -> TypedModuleId {
        self.module_id
    }

    pub const fn prepared_revision(&self) -> u64 {
        self.prepared_revision
    }

    pub const fn sealed_revision(&self) -> Option<u64> {
        self.sealed_revision
    }

    pub const fn runtime(&self) -> &TypedModuleRuntimeBindings {
        &self.runtime
    }

    pub fn retained_liveness(&self) -> &BTreeSet<TypedModuleSymbol> {
        &self.retained_liveness
    }

    pub fn requests(&self) -> &[TypedModuleRequestEdge] {
        &self.requests
    }

    pub const fn has_top_level_await(&self) -> bool {
        self.has_top_level_await
    }

    pub const fn is_finalized(&self) -> bool {
        self.finalized
    }

    /// Stable plan component for the enclosing optimizer/cache fingerprint.
    pub fn fingerprint_component(&self) -> u64 {
        let mut hash = Fnv64::new();
        hash.write(
            format!(
                "mode={:?};module={:?};preserve_all_exports={};preserve_export_star={};",
                self.mode, self.module_id, self.preserve_all_exports, self.preserve_export_star
            )
            .as_bytes(),
        );
        for name in &self.observed_export_names {
            hash.write(format!("observed_export={name};").as_bytes());
        }
        for binding in self.sentinel_bindings().into_iter().chain([
            &self.runtime.exports,
            &self.runtime.export_live,
            &self.runtime.export_all,
            &self.runtime.mark_esmodule,
            &self.runtime.internal_require,
            &self.runtime.internal_require_async,
            &self.runtime.internal_import,
            &self.runtime.external_require,
        ]) {
            hash.write(format!("symbol={}:{};", binding.symbol, binding.original_name).as_bytes());
        }
        for namespace in &self.namespace_requests {
            hash.write(
                format!(
                    "namespace={}:{}:{};",
                    namespace.symbol, namespace.specifier, namespace.requires_namespace_interop
                )
                .as_bytes(),
            );
        }
        for binding in &self.local_bindings {
            hash.write(format!("local={}:{};", binding.symbol, binding.original_name).as_bytes());
        }
        for root in &self.retained_liveness {
            hash.write(format!("live={}:{};", root.module.0, root.symbol).as_bytes());
        }
        for request in &self.requests {
            hash.write(
                format!(
                    "request={:?}:{}:{:?};",
                    request.kind, request.specifier, request.origin
                )
                .as_bytes(),
            );
        }
        hash.write(
            format!(
                "tla={};esm={};sealed={:?};final={};",
                self.has_top_level_await, self.had_esm, self.sealed_revision, self.finalized
            )
            .as_bytes(),
        );
        hash.finish()
    }

    fn sentinel_bindings(&self) -> [&TypedRuntimeBinding; 5] {
        [
            &self.static_import_request,
            &self.require_request,
            &self.dynamic_request,
            &self.default_read,
            &self.namespace_read,
        ]
    }

    fn pending_bindings(&self) -> [&TypedRuntimeBinding; 11] {
        [
            &self.static_import_request,
            &self.require_request,
            &self.dynamic_request,
            &self.default_read,
            &self.namespace_read,
            &self.runtime.export_live,
            &self.runtime.export_all,
            &self.runtime.mark_esmodule,
            &self.runtime.internal_require_async,
            &self.runtime.internal_import,
            &self.runtime.external_require,
        ]
    }

    fn namespace_specifier(&self, symbol: SymbolId) -> Option<&str> {
        self.namespace_requests
            .iter()
            .find(|namespace| namespace.symbol == symbol)
            .map(|namespace| namespace.specifier.as_str())
    }

    fn namespace_requires_interop(&self, symbol: SymbolId) -> bool {
        self.namespace_requests
            .iter()
            .find(|namespace| namespace.symbol == symbol)
            .is_some_and(|namespace| namespace.requires_namespace_interop)
    }
}

/// Final target for one original module specifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypedFinalModuleTarget {
    External {
        rewritten_specifier: String,
    },
    Internal {
        module_id: TypedModuleId,
        is_esm: bool,
        async_dependency: bool,
        dynamic_chunk: Option<TypedChunkId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedResolvedModule {
    pub specifier: String,
    pub request_kind: TypedModuleRequestKind,
    pub target: TypedFinalModuleTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedModuleSpecifierRewrite {
    pub specifier: String,
    pub request_kind: TypedModuleRequestKind,
    pub rewritten_specifier: String,
}

/// Link/chunk facts which become stable only immediately before final emission.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypedFinalModuleFacts {
    pub modules: Vec<TypedResolvedModule>,
    pub specifier_rewrites: BTreeMap<String, String>,
    pub request_rewrites: Vec<TypedModuleSpecifierRewrite>,
    /// Preserve-CommonJS output may deliberately express external `import()` through the host
    /// `require` contract. Bundled output leaves this false and uses native import/runtime chunks.
    pub lower_external_dynamic_to_require: bool,
    pub no_esmodule: bool,
}

/// Finalization report consumed by wrapper/chunk generation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypedFinalModuleReport {
    pub lowered_static_requests: usize,
    pub lowered_require_requests: usize,
    pub lowered_dynamic_requests: usize,
    pub lowered_default_reads: usize,
    pub rewritten_native_specifiers: usize,
    pub requires_async_module: bool,
}

/// One compiler-generated synchronous static request whose value is structurally discarded.
///
/// The typed finalizer owns the semantic proof; code generation later turns `node` into an exact
/// byte range, and the final bundler layout decides whether the target is already eagerly
/// executed. Keeping the arena id private prevents downstream text scanners from manufacturing
/// this fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypedDiscardedStaticRequest {
    node: NodeId,
    target: TypedModuleId,
}

impl TypedDiscardedStaticRequest {
    pub const fn node(self) -> NodeId {
        self.node
    }

    pub const fn target(self) -> TypedModuleId {
        self.target
    }
}

/// Owned module IR which crossed the complete finalization and validation boundary.
///
/// The fields are private so typed code generation can accept this type without revalidating the
/// complete arena. Instances are created only by [`finalize_owned_typed_modules`].
#[derive(Debug)]
pub struct FinalizedTypedProgram {
    program: TypedProgram,
    plan: TypedModulePlan,
    discarded_static_requests: Vec<TypedDiscardedStaticRequest>,
}

impl FinalizedTypedProgram {
    pub const fn program(&self) -> &TypedProgram {
        &self.program
    }

    pub const fn plan(&self) -> &TypedModulePlan {
        &self.plan
    }

    pub fn discarded_static_requests(&self) -> &[TypedDiscardedStaticRequest] {
        &self.discarded_static_requests
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedModulePhase {
    Plan,
    Finalize,
    EmitValidation,
}

/// Module lowering never falls back to emitting unresolved sentinels.
#[derive(Debug)]
pub enum TypedModuleError {
    InvalidInput {
        phase: TypedModulePhase,
        message: String,
    },
    StaleAnalysis {
        program_revision: u64,
        analysis_revision: u64,
    },
    StalePlan {
        program_revision: u64,
        plan_revision: u64,
    },
    Unsupported {
        phase: TypedModulePhase,
        node: Option<NodeId>,
        message: String,
    },
    PendingRequests {
        count: usize,
        symbols: Vec<SymbolId>,
    },
    Ir(TypedIrError),
}

impl fmt::Display for TypedModuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { phase, message } => {
                write!(
                    formatter,
                    "typed module {phase:?} input is invalid: {message}"
                )
            }
            Self::StaleAnalysis {
                program_revision,
                analysis_revision,
            } => write!(
                formatter,
                "typed module analysis revision {analysis_revision} does not match program revision {program_revision}"
            ),
            Self::StalePlan {
                program_revision,
                plan_revision,
            } => write!(
                formatter,
                "typed module plan revision {plan_revision} does not match program revision {program_revision}"
            ),
            Self::Unsupported {
                phase,
                node,
                message,
            } => write!(
                formatter,
                "typed module {phase:?} cannot lower node {node:?}: {message}"
            ),
            Self::PendingRequests { count, symbols } => write!(
                formatter,
                "typed program still contains {count} pending module request(s) for sentinel symbols {symbols:?}"
            ),
            Self::Ir(error) => write!(formatter, "typed module IR failure: {error}"),
        }
    }
}

impl Error for TypedModuleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Ir(error) => Some(error),
            _ => None,
        }
    }
}

impl From<TypedIrError> for TypedModuleError {
    fn from(value: TypedIrError) -> Self {
        Self::Ir(value)
    }
}

impl From<&TypedRuntimeBinding> for Binding {
    fn from(value: &TypedRuntimeBinding) -> Self {
        Self {
            name: value.original_name.clone(),
            symbol: value.symbol,
        }
    }
}

const MODULE_ORIGIN: IrOrigin = IrOrigin::Synthetic {
    anchor: None,
    kind: SyntheticOriginKind::Optimization,
};

fn derived_origin(origin: IrOrigin) -> IrOrigin {
    let anchor = match origin {
        IrOrigin::Source(span) => Some(span),
        IrOrigin::Derived { anchor, .. } | IrOrigin::Synthetic { anchor, .. } => anchor,
    };
    IrOrigin::Derived {
        anchor,
        kind: DerivedOriginKind::Optimization,
    }
}

fn module_error(
    phase: TypedModulePhase,
    node: Option<NodeId>,
    message: impl Into<String>,
) -> TypedModuleError {
    TypedModuleError::Unsupported {
        phase,
        node,
        message: message.into(),
    }
}

#[derive(Clone, Debug)]
struct ImportedBinding {
    symbol: SymbolId,
    local_name: String,
    imported_name: Option<String>,
    kind: ImportSpecifierKind,
    namespace: Binding,
}

#[derive(Clone, Debug)]
struct CurrentBinding {
    original_name: String,
    emitted_name: String,
    symbol: SymbolId,
}

fn allocate_module_plan(
    program: &mut TypedProgram,
    options: &TypedModuleOptions,
    reserved: &mut HashSet<String>,
) -> Result<TypedModulePlan, TypedModuleError> {
    let static_import_request = allocate_runtime_binding(
        program,
        reserved,
        "__wake_static_import_request",
        DeclKind::Function,
    )?;
    let require_request = allocate_runtime_binding(
        program,
        reserved,
        "__wake_require_request",
        DeclKind::Function,
    )?;
    let dynamic_request = allocate_runtime_binding(
        program,
        reserved,
        "__wake_dynamic_request",
        DeclKind::Function,
    )?;
    let default_read =
        allocate_runtime_binding(program, reserved, "__wake_default_read", DeclKind::Function)?;
    let namespace_read = allocate_runtime_binding(
        program,
        reserved,
        "__wake_namespace_read",
        DeclKind::Function,
    )?;
    let runtime = TypedModuleRuntimeBindings {
        exports: allocate_exact_runtime_binding(program, "exports", DeclKind::Var)?,
        export_live: allocate_runtime_binding(
            program,
            reserved,
            "__wake_export_live",
            DeclKind::Function,
        )?,
        export_all: allocate_runtime_binding(
            program,
            reserved,
            "__wake_export_all",
            DeclKind::Function,
        )?,
        mark_esmodule: allocate_runtime_binding(
            program,
            reserved,
            "__wake_mark_esmodule",
            DeclKind::Function,
        )?,
        internal_require: allocate_exact_runtime_binding(
            program,
            "__wake_require__",
            DeclKind::Function,
        )?,
        internal_require_async: allocate_runtime_binding(
            program,
            reserved,
            "__wake_require_async",
            DeclKind::Function,
        )?,
        internal_import: allocate_runtime_binding(
            program,
            reserved,
            "__wake_import",
            DeclKind::Function,
        )?,
        external_require: allocate_runtime_binding(
            program,
            reserved,
            "__wake_external_require",
            DeclKind::Function,
        )?,
    };
    let retained_liveness = options
        .linker_liveness
        .roots
        .iter()
        .copied()
        .filter(|root| root.module == options.module_id)
        .collect();
    let has_top_level_await = contains_top_level_await(program)?;
    Ok(TypedModulePlan {
        mode: options.mode,
        module_id: options.module_id,
        preserve_all_exports: options.preserve_all_exports,
        preserve_export_star: options.preserve_export_star,
        observed_export_names: options.observed_export_names.clone(),
        prepared_revision: 0,
        sealed_revision: None,
        static_import_request,
        require_request,
        dynamic_request,
        default_read,
        namespace_read,
        runtime,
        namespace_requests: Vec::new(),
        local_bindings: Vec::new(),
        retained_liveness,
        requests: Vec::new(),
        has_top_level_await,
        had_esm: false,
        finalized: false,
    })
}

/// Build the optimizer-owned module plan and apply all optimizer-stage structural edits.
///
/// The operation is atomic: neither the program nor its revision changes when validation or a
/// local safety proof fails. The supplied analysis must describe the exact input revision.
#[cfg(test)]
pub(crate) fn plan_typed_modules(
    program: &mut TypedProgram,
    analysis: &TypedAnalysis,
    options: &TypedModuleOptions,
) -> Result<TypedModulePlan, TypedModuleError> {
    program.validate()?;
    let (planned, plan) = plan_owned_typed_modules(program.clone(), analysis, options)?;
    *program = planned;
    Ok(plan)
}

/// Consume an optimizer-owned program while planning modules, so the production path does not
/// clone the complete arena merely to provide rollback for a value no caller can observe again.
pub(crate) fn plan_owned_typed_modules(
    mut working: TypedProgram,
    analysis: &TypedAnalysis,
    options: &TypedModuleOptions,
) -> Result<(TypedProgram, TypedModulePlan), TypedModuleError> {
    // The consuming production scheduler owns a validated epoch. The borrowed test adapter above
    // validates before cloning, while the planner validates the complete result before returning.
    debug_assert!(working.validate().is_ok());
    if analysis.revision() != working.revision() {
        return Err(TypedModuleError::StaleAnalysis {
            program_revision: working.revision(),
            analysis_revision: analysis.revision(),
        });
    }
    validate_liveness(&working, options)?;

    let repaired_parser_bindings = repair_parser_lowered_import_bindings(&mut working)?;
    let repaired_analysis = repaired_parser_bindings
        .then(|| TypedAnalysis::rebuild(&working))
        .transpose()?;
    let analysis = repaired_analysis.as_ref().unwrap_or(analysis);
    let mut reserved = collect_reserved_names(&working);
    let mut plan = allocate_module_plan(&mut working, options, &mut reserved)?;

    match options.mode {
        TypedModuleMode::PreserveEsm => collect_native_module_requests(&working, &mut plan)?,
        TypedModuleMode::PreserveCommonJs | TypedModuleMode::BundledCommonJs => {
            lower_esm_to_common_js(&mut working, analysis, options, &mut plan, &mut reserved)?;
        }
    }
    abstractify_runtime_requests(&mut working, &mut plan)?;
    working.validate()?;
    plan.prepared_revision = working.revision();
    Ok((working, plan))
}

/// Plan an exact binding-free bundled module without constructing a whole-program semantic
/// snapshot. This is deliberately narrower than the general planner: the owned program may only
/// contain empty ESM markers, empty statements and expression statements whose subtrees cannot
/// introduce bindings, calls, dynamic scope or suspension. Rejection returns the unchanged owner
/// so the caller can run the ordinary analysis-backed path.
pub(crate) fn try_plan_owned_trivial_bundled_module(
    mut working: TypedProgram,
    options: &TypedModuleOptions,
) -> Result<(TypedProgram, Option<TypedModulePlan>), TypedModuleError> {
    debug_assert!(working.validate().is_ok());
    if options.mode != TypedModuleMode::BundledCommonJs
        || options.preserve_all_exports
        || !options.linker_liveness.roots.is_empty()
        || !working.symbols().is_empty()
    {
        return Ok((working, None));
    }
    validate_liveness(&working, options)?;
    let body = program_body(&working)?;
    let statements = working
        .list(body)
        .expect("validated program body")
        .items()
        .to_vec();
    let mut empty_exports = Vec::new();
    for &statement in &statements {
        match working
            .node(statement)
            .expect("validated top-level statement")
            .data()
        {
            IrNodeData::EmptyStatement => {}
            IrNodeData::ExportNamedDeclaration {
                declaration: None,
                specifiers,
                source: None,
                attributes: None,
            } if working
                .list(*specifiers)
                .is_some_and(|specifiers| specifiers.items().is_empty()) =>
            {
                empty_exports.push(statement);
            }
            IrNodeData::ExpressionStatement { .. } => {
                for node in working.subtree_preorder(statement)? {
                    let data = working.node(node).expect("validated effect subtree").data();
                    if let IrNodeData::Name { name } = data
                        && working
                            .name(*name)
                            .expect("validated name occurrence")
                            .symbol()
                            .is_some()
                    {
                        return Ok((working, None));
                    }
                    if matches!(
                        data,
                        IrNodeData::Function { .. }
                            | IrNodeData::ArrowFunction { .. }
                            | IrNodeData::Class { .. }
                            | IrNodeData::CallExpression { .. }
                            | IrNodeData::NewExpression { .. }
                            | IrNodeData::TaggedTemplateExpression { .. }
                            | IrNodeData::AwaitExpression { .. }
                            | IrNodeData::YieldExpression { .. }
                            | IrNodeData::ImportExpression { .. }
                            | IrNodeData::MetaProperty { .. }
                            | IrNodeData::ThisExpression
                            | IrNodeData::SuperExpression
                            | IrNodeData::WithStatement { .. }
                    ) {
                        return Ok((working, None));
                    }
                }
            }
            _ => return Ok((working, None)),
        }
    }

    let had_esm = !empty_exports.is_empty();
    for statement in empty_exports {
        replace_program_statement(&mut working, statement, &[])?;
    }
    let mut reserved = collect_reserved_names(&working);
    let mut plan = allocate_module_plan(&mut working, options, &mut reserved)?;
    plan.had_esm = had_esm;
    working.validate()?;
    plan.prepared_revision = working.revision();
    Ok((working, Some(plan)))
}

/// Parser-injected JSX runtime imports intentionally carry DUMMY spans, so the source semantic
/// model cannot assign them identities without risking span collisions. Repair only that trusted
/// structural pattern in the owned IR: the DUMMY import binding plus parser-shaped call/fragment
/// occurrences. User-authored identifiers with the same spelling keep their original resolution.
fn repair_parser_lowered_import_bindings(
    program: &mut TypedProgram,
) -> Result<bool, TypedModuleError> {
    #[derive(Clone)]
    struct Repair {
        binding_name: NameId,
        local: String,
        imported: String,
    }

    let body = program_body(program)?;
    let mut repairs = Vec::new();
    for &statement in program.list(body).expect("validated body").items() {
        let Some(record) = program.node(statement) else {
            continue;
        };
        if !matches!(
            record.origin(),
            IrOrigin::Derived {
                anchor: None,
                kind: DerivedOriginKind::ParserLowering
            }
        ) {
            continue;
        }
        let IrNodeData::ImportDeclaration {
            specifiers, source, ..
        } = record.data()
        else {
            continue;
        };
        let Some(source) = string_value(program, *source) else {
            continue;
        };
        if !source.ends_with("/jsx-runtime") && !source.ends_with("/jsx-dev-runtime") {
            continue;
        }
        for &specifier in program
            .list(*specifiers)
            .expect("validated JSX import specifiers")
            .items()
        {
            let IrNodeData::ImportSpecifier {
                kind: ImportSpecifierKind::Named,
                imported: Some(imported),
                local,
            } = program
                .node(specifier)
                .expect("JSX import specifier")
                .data()
            else {
                continue;
            };
            let Some((binding_name, binding)) = name_record(program, *local) else {
                continue;
            };
            if binding.symbol().is_some() {
                continue;
            }
            let Some(imported) = module_name_text(program, *imported) else {
                continue;
            };
            if !matches!(imported.as_str(), "jsx" | "jsxs" | "jsxDEV" | "Fragment") {
                continue;
            }
            repairs.push(Repair {
                binding_name,
                local: binding.original().to_owned(),
                imported,
            });
        }
    }
    if repairs.is_empty() {
        return Ok(false);
    }

    for repair in repairs {
        let references = program
            .preorder_validated()?
            .into_iter()
            .filter_map(|node| {
                let record = program.node(node)?;
                let IrNodeData::Identifier { .. } = record.data() else {
                    return None;
                };
                let (name, occurrence) = name_record(program, node)?;
                if occurrence.role() != NameRole::Reference || occurrence.original() != repair.local
                {
                    return None;
                }
                let generated = if repair.imported == "Fragment" {
                    matches!(
                        record.origin(),
                        IrOrigin::Source(span) if span.lo == span.hi
                    ) || matches!(
                        record.origin(),
                        IrOrigin::Derived {
                            kind: DerivedOriginKind::ParserLowering,
                            ..
                        }
                    )
                } else {
                    record.parent().is_some_and(|parent| {
                        parent.role() == ChildRole::Callee
                            && matches!(
                                program.node(parent.parent()).map(|node| node.data()),
                                Some(IrNodeData::CallExpression { .. })
                            )
                            && program
                                .node(parent.parent())
                                .is_some_and(|call| call.origin() == record.origin())
                    })
                };
                generated.then_some((name, occurrence.symbol()))
            })
            .collect::<Vec<_>>();
        let existing_imports = references
            .iter()
            .filter_map(|(_, symbol)| *symbol)
            .filter(|&symbol| {
                program.symbol(symbol).is_some_and(|record| {
                    record.decl_kind() == DeclKind::Import && record.original_name() == repair.local
                })
            })
            .collect::<BTreeSet<_>>();
        let symbol = if existing_imports.is_empty() {
            program.allocate_symbol(repair.local.clone(), DeclKind::Import)?
        } else if existing_imports.len() == 1 {
            *existing_imports
                .first()
                .expect("one existing JSX import symbol")
        } else {
            return Err(module_error(
                TypedModulePhase::Plan,
                None,
                format!(
                    "parser-lowered JSX binding `{}` resolved to ambiguous import symbols {existing_imports:?}",
                    repair.local
                ),
            ));
        };
        program.set_name_symbol(repair.binding_name, Some(symbol))?;
        for (reference, current) in references {
            if current.is_none() {
                program.set_name_symbol(reference, Some(symbol))?;
            }
        }
    }
    Ok(true)
}

/// Replace every plan-owned abstract request using final linker/chunk facts.
///
/// Program and plan commit together. A missing fact means a conservative external dependency;
/// duplicate or malformed facts, stale revisions and unresolved sentinels are diagnosed.
pub fn finalize_typed_modules(
    program: &mut TypedProgram,
    plan: &mut TypedModulePlan,
    facts: &TypedFinalModuleFacts,
) -> Result<TypedFinalModuleReport, TypedModuleError> {
    let (finalized, report) = finalize_owned_typed_modules(program.clone(), plan.clone(), facts)?;
    *program = finalized.program;
    *plan = finalized.plan;
    Ok(report)
}

/// Consume one optimizer-owned program and sealed module plan, discarding both on failure.
///
/// Unlike [`finalize_typed_modules`], this boundary does not need a transactional arena clone:
/// callers have surrendered ownership, so a failed partial rewrite cannot be observed. Success
/// returns an unforgeable finalized type which typed codegen can emit without another whole-arena
/// validation.
pub fn finalize_owned_typed_modules(
    mut program: TypedProgram,
    mut plan: TypedModulePlan,
    facts: &TypedFinalModuleFacts,
) -> Result<(FinalizedTypedProgram, TypedFinalModuleReport), TypedModuleError> {
    let mut discarded_static_requests = Vec::new();
    let report = finalize_typed_modules_in_place(
        &mut program,
        &mut plan,
        facts,
        &mut discarded_static_requests,
    )?;
    Ok((
        FinalizedTypedProgram {
            program,
            plan,
            discarded_static_requests,
        },
        report,
    ))
}

fn finalize_typed_modules_in_place(
    program: &mut TypedProgram,
    plan: &mut TypedModulePlan,
    facts: &TypedFinalModuleFacts,
    discarded_static_requests: &mut Vec<TypedDiscardedStaticRequest>,
) -> Result<TypedFinalModuleReport, TypedModuleError> {
    let Some(sealed_revision) = plan.sealed_revision else {
        return Err(TypedModuleError::InvalidInput {
            phase: TypedModulePhase::Finalize,
            message: "module plan must be sealed after optimization and mangle".into(),
        });
    };
    if program.revision() != sealed_revision {
        return Err(TypedModuleError::StalePlan {
            program_revision: program.revision(),
            plan_revision: sealed_revision,
        });
    }
    if plan.finalized {
        validate_no_pending_module_requests_after_validation(program, plan)?;
        return Err(TypedModuleError::InvalidInput {
            phase: TypedModulePhase::Finalize,
            message: "module plan was already finalized".into(),
        });
    }
    let resolved = validate_final_facts(facts)?;
    let mut report = TypedFinalModuleReport {
        requires_async_module: plan.has_top_level_await,
        ..TypedFinalModuleReport::default()
    };

    finalize_requests(
        program,
        plan,
        &resolved,
        facts,
        &mut report,
        discarded_static_requests,
    )?;
    rewrite_native_specifiers(program, facts, &mut report)?;
    insert_esmodule_marker(program, plan, facts.no_esmodule)?;
    finalize_runtime_sentinels(program, plan)?;
    program.validate()?;
    validate_no_pending_module_requests_after_validation(program, plan)?;
    plan.finalized = true;
    plan.sealed_revision = Some(program.revision());
    Ok(report)
}

/// Emission boundary invariant: no live request/default sentinel may reach typed codegen.
#[cfg(test)]
fn validate_no_pending_module_requests(
    program: &TypedProgram,
    plan: &TypedModulePlan,
) -> Result<(), TypedModuleError> {
    program.validate()?;
    validate_no_pending_module_requests_after_validation(program, plan)
}

fn validate_no_pending_module_requests_after_validation(
    program: &TypedProgram,
    plan: &TypedModulePlan,
) -> Result<(), TypedModuleError> {
    let sentinels = plan
        .pending_bindings()
        .into_iter()
        .map(|binding| binding.symbol)
        .collect::<BTreeSet<_>>();
    let mut count = 0_usize;
    let mut found = BTreeSet::new();
    for node in program.subtree_preorder(program.root())? {
        let IrNodeData::Name { name } = program.node(node).expect("validated node").data() else {
            continue;
        };
        let Some(symbol) = program.name(*name).expect("validated name").symbol() else {
            continue;
        };
        if sentinels.contains(&symbol) {
            count += 1;
            found.insert(symbol);
        }
    }
    if count == 0 {
        Ok(())
    } else {
        Err(TypedModuleError::PendingRequests {
            count,
            symbols: found.into_iter().collect(),
        })
    }
}

/// Seal a plan against the post-fixed-point, post-mangle tree.
///
/// Request edges are rebuilt from the live root, so eliminated requests and tombstones never keep
/// dependencies alive. The seal deliberately runs after all ordinary optimizer mutations.
pub fn seal_typed_module_plan(
    program: &TypedProgram,
    plan: &mut TypedModulePlan,
) -> Result<(), TypedModuleError> {
    if plan.finalized {
        return Err(TypedModuleError::InvalidInput {
            phase: TypedModulePhase::Plan,
            message: "a finalized module plan cannot be resealed".into(),
        });
    }
    let requests = live_module_requests(program, plan)?;
    validate_pending_request_shapes(program, plan)?;
    let live_symbols = program
        .preorder_validated()?
        .into_iter()
        .filter_map(|node| {
            let IrNodeData::Name { name } = program.node(node)?.data() else {
                return None;
            };
            program.name(*name)?.symbol()
        })
        .collect::<HashSet<_>>();
    let mut next = plan.clone();
    next.requests = requests;
    next.retained_liveness
        .retain(|root| live_symbols.contains(&root.symbol));
    next.sealed_revision = Some(program.revision());
    *plan = next;
    Ok(())
}

fn live_module_requests(
    program: &TypedProgram,
    plan: &TypedModulePlan,
) -> Result<Vec<TypedModuleRequestEdge>, TypedModuleError> {
    let mut output = Vec::new();
    for node in program.preorder_validated()? {
        let record = program.node(node).expect("validated live node");
        match record.data() {
            IrNodeData::ImportDeclaration { source, .. }
            | IrNodeData::ExportAllDeclaration { source, .. }
            | IrNodeData::ExportNamedDeclaration {
                source: Some(source),
                ..
            } => {
                let specifier = string_value(program, *source).ok_or_else(|| {
                    module_error(
                        TypedModulePhase::Plan,
                        Some(*source),
                        "live native module source is not a string literal",
                    )
                })?;
                output.push(TypedModuleRequestEdge {
                    specifier: specifier.to_owned(),
                    kind: TypedModuleRequestKind::StaticImport,
                    origin: record.origin(),
                });
            }
            IrNodeData::CallExpression {
                callee, arguments, ..
            } => {
                let Some(symbol) = identifier_symbol(program, *callee) else {
                    continue;
                };
                let kind = if symbol == plan.static_import_request.symbol {
                    Some(TypedModuleRequestKind::StaticImport)
                } else if symbol == plan.require_request.symbol {
                    Some(TypedModuleRequestKind::Require)
                } else if symbol == plan.dynamic_request.symbol {
                    Some(TypedModuleRequestKind::DynamicImport)
                } else {
                    None
                };
                let Some(kind) = kind else { continue };
                let arguments = program
                    .list(*arguments)
                    .expect("validated request arguments")
                    .items();
                let Some(&source) = arguments.first() else {
                    return Err(module_error(
                        TypedModulePhase::Plan,
                        Some(node),
                        "live module sentinel has no source argument",
                    ));
                };
                let specifier = string_value(program, source).ok_or_else(|| {
                    module_error(
                        TypedModulePhase::Plan,
                        Some(source),
                        "live module sentinel source is not a string literal",
                    )
                })?;
                output.push(TypedModuleRequestEdge {
                    specifier: specifier.to_owned(),
                    kind,
                    origin: record.origin(),
                });
            }
            _ => {}
        }
    }
    Ok(output)
}

fn validate_pending_request_shapes(
    program: &TypedProgram,
    plan: &TypedModulePlan,
) -> Result<(), TypedModuleError> {
    let sentinels = plan
        .pending_bindings()
        .into_iter()
        .map(|binding| binding.symbol)
        .collect::<BTreeSet<_>>();
    for node in program.preorder_validated()? {
        let IrNodeData::Name { name } = program.node(node).expect("validated node").data() else {
            continue;
        };
        let record = program.name(*name).expect("validated name");
        let Some(symbol) = record.symbol() else {
            continue;
        };
        if !sentinels.contains(&symbol) {
            continue;
        }
        let Some(identifier) = program.node(node).and_then(|node| node.parent()) else {
            return Err(module_error(
                TypedModulePhase::Plan,
                Some(node),
                "module sentinel name is detached from an identifier",
            ));
        };
        let Some(call) = program
            .node(identifier.parent())
            .and_then(|node| node.parent())
        else {
            return Err(module_error(
                TypedModulePhase::Plan,
                Some(node),
                "module sentinel identifier is not a call callee",
            ));
        };
        if identifier.role() != ChildRole::IdentifierName
            || call.role() != ChildRole::Callee
            || !matches!(
                program.node(call.parent()).map(|node| node.data()),
                Some(IrNodeData::CallExpression { .. })
            )
        {
            return Err(module_error(
                TypedModulePhase::Plan,
                Some(node),
                "module sentinel occurs outside a direct call callee",
            ));
        }
    }
    Ok(())
}

fn validate_liveness(
    program: &TypedProgram,
    options: &TypedModuleOptions,
) -> Result<(), TypedModuleError> {
    for root in &options.linker_liveness.roots {
        if root.module == options.module_id && program.symbol(root.symbol).is_none() {
            return Err(TypedModuleError::InvalidInput {
                phase: TypedModulePhase::Plan,
                message: format!(
                    "linker liveness references unknown symbol {} in module {}",
                    root.symbol, root.module.0
                ),
            });
        }
    }
    Ok(())
}

fn collect_reserved_names(program: &TypedProgram) -> HashSet<String> {
    let mut names = HashSet::new();
    for name in program.names() {
        names.insert(name.original().to_owned());
        names.insert(name.emitted().to_owned());
    }
    for symbol in program.symbols() {
        names.insert(symbol.original_name().to_owned());
    }
    names
}

fn allocate_runtime_binding(
    program: &mut TypedProgram,
    reserved: &mut HashSet<String>,
    requested: &str,
    kind: DeclKind,
) -> Result<TypedRuntimeBinding, TypedModuleError> {
    let mut name = requested.to_owned();
    let mut suffix = 1_u32;
    while reserved.contains(&name) {
        name = format!("{requested}${suffix}");
        suffix = suffix
            .checked_add(1)
            .ok_or_else(|| TypedModuleError::InvalidInput {
                phase: TypedModulePhase::Plan,
                message: "module runtime binding suffix overflow".into(),
            })?;
    }
    reserved.insert(name.clone());
    let symbol = program.allocate_symbol(name.clone(), kind)?;
    Ok(TypedRuntimeBinding {
        symbol,
        original_name: name,
    })
}

fn allocate_exact_runtime_binding(
    program: &mut TypedProgram,
    name: &str,
    kind: DeclKind,
) -> Result<TypedRuntimeBinding, TypedModuleError> {
    let symbol = program.allocate_symbol(name.to_owned(), kind)?;
    Ok(TypedRuntimeBinding {
        symbol,
        original_name: name.to_owned(),
    })
}

fn allocate_namespace_binding(
    program: &mut TypedProgram,
    reserved: &mut HashSet<String>,
    ordinal: usize,
) -> Result<Binding, TypedModuleError> {
    let runtime = allocate_runtime_binding(
        program,
        reserved,
        &format!("__wake_namespace_{ordinal}"),
        DeclKind::Const,
    )?;
    Ok(Binding::from(&runtime))
}

fn contains_top_level_await(program: &TypedProgram) -> Result<bool, TypedModuleError> {
    for node in program.preorder_validated()? {
        if !matches!(
            program.node(node).expect("validated node").data(),
            IrNodeData::AwaitExpression { .. }
        ) {
            continue;
        }
        let mut cursor = node;
        let mut nested = false;
        while let Some(parent) = program.node(cursor).and_then(|node| node.parent()) {
            cursor = parent.parent();
            if matches!(
                program.node(cursor).expect("validated parent").data(),
                IrNodeData::Function { .. } | IrNodeData::ArrowFunction { .. }
            ) {
                nested = true;
                break;
            }
        }
        if !nested {
            return Ok(true);
        }
    }
    Ok(false)
}

fn program_body(program: &TypedProgram) -> Result<ListId, TypedModuleError> {
    let IrNodeData::Program { body, .. } = program
        .node(program.root())
        .ok_or_else(|| TypedModuleError::InvalidInput {
            phase: TypedModulePhase::Plan,
            message: "program root is missing".into(),
        })?
        .data()
    else {
        return Err(TypedModuleError::InvalidInput {
            phase: TypedModulePhase::Plan,
            message: "typed module planning requires a Program root".into(),
        });
    };
    Ok(*body)
}

fn string_value(program: &TypedProgram, node: NodeId) -> Option<&str> {
    match program.node(node)?.data() {
        IrNodeData::StringLiteral { value } => Some(value),
        IrNodeData::Name { name } => {
            let name = program.name(*name)?;
            (name.syntax() == NameSyntax::String).then(|| name.original())
        }
        _ => None,
    }
}

fn name_record(program: &TypedProgram, node: NodeId) -> Option<(NameId, &crate::typed_ir::IrName)> {
    let node = match program.node(node)?.data() {
        IrNodeData::Identifier { name } => *name,
        IrNodeData::Name { .. } => node,
        _ => return None,
    };
    let IrNodeData::Name { name } = program.node(node)?.data() else {
        return None;
    };
    Some((*name, program.name(*name)?))
}

fn module_name_text(program: &TypedProgram, name: IrModuleName) -> Option<String> {
    match name.kind {
        ModuleNameKind::Identifier => {
            name_record(program, name.value).map(|(_, record)| record.original().to_owned())
        }
        ModuleNameKind::String => string_value(program, name.value).map(str::to_owned),
    }
}

fn set_derived_origin(
    factory: &SyntheticFactory<'_>,
    node: NodeId,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    factory
        .program
        .borrow_mut()
        .set_origin(node, derived_origin(origin))?;
    Ok(node)
}

fn binding_reference(
    factory: &SyntheticFactory<'_>,
    binding: &TypedRuntimeBinding,
) -> Result<NodeId, TypedModuleError> {
    symbol_reference(factory, binding.symbol, &binding.original_name)
}

fn symbol_reference(
    factory: &SyntheticFactory<'_>,
    symbol: SymbolId,
    original_name: &str,
) -> Result<NodeId, TypedModuleError> {
    let emitted = {
        let program = factory.program.borrow();
        program
            .symbol(symbol)
            .ok_or_else(|| {
                module_error(
                    TypedModulePhase::Finalize,
                    None,
                    format!("unknown symbol {symbol} while creating a module reference"),
                )
            })?
            .emitted_name()
            .to_owned()
    };
    let identifier = factory.identifier(original_name, NameRole::Reference, Some(symbol))?;
    if emitted != original_name {
        let occurrence = {
            let program = factory.program.borrow();
            let IrNodeData::Identifier { name } =
                program.node(identifier).expect("new identifier").data()
            else {
                unreachable!()
            };
            let IrNodeData::Name { name } = program.node(*name).expect("new name").data() else {
                unreachable!()
            };
            *name
        };
        factory
            .program
            .borrow_mut()
            .set_emitted_name(occurrence, emitted)?;
    }
    Ok(identifier)
}

fn binding_from_identifier(
    program: &TypedProgram,
    node: NodeId,
) -> Result<Binding, TypedModuleError> {
    let (_, name) = name_record(program, node).ok_or_else(|| {
        module_error(
            TypedModulePhase::Plan,
            Some(node),
            "module binding is not an identifier",
        )
    })?;
    let symbol = name.symbol().ok_or_else(|| {
        module_error(
            TypedModulePhase::Plan,
            Some(node),
            "module binding has no owned SymbolId",
        )
    })?;
    Ok(Binding {
        name: name.original().to_owned(),
        symbol,
    })
}

fn variable_declaration(
    factory: &SyntheticFactory<'_>,
    kind: VarKind,
    binding: &Binding,
    initializer: NodeId,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    let binding_node = factory.binding_pattern(binding)?;
    let declarator =
        factory
            .program
            .borrow_mut()
            .append_detached_node_with(derived_origin(origin), |_| {
                Ok(IrNodeData::VariableDeclarator {
                    binding: binding_node,
                    initializer: Some(initializer),
                })
            })?;
    let declaration = factory.program.borrow_mut().append_detached_node_with(
        derived_origin(origin),
        |builder| {
            let declarations = builder.list(ChildRole::DeclarationItems, [declarator])?;
            Ok(IrNodeData::VariableDeclaration { kind, declarations })
        },
    )?;
    Ok(declaration)
}

fn request_call(
    factory: &SyntheticFactory<'_>,
    binding: &TypedRuntimeBinding,
    arguments: Vec<NodeId>,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    let callee = binding_reference(factory, binding)?;
    let call = factory.call(callee, arguments)?;
    set_derived_origin(factory, call, origin)
}

fn request_statement(
    factory: &SyntheticFactory<'_>,
    binding: &TypedRuntimeBinding,
    specifier: &str,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    let source = factory.string(specifier)?;
    let call = request_call(factory, binding, vec![source], origin)?;
    let statement = factory.expression_statement(call)?;
    set_derived_origin(factory, statement, origin)
}

fn request_declaration(
    factory: &SyntheticFactory<'_>,
    request: &TypedRuntimeBinding,
    namespace: &Binding,
    specifier: &str,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    let source = factory.string(specifier)?;
    let initializer = request_call(factory, request, vec![source], origin)?;
    variable_declaration(factory, VarKind::Const, namespace, initializer, origin)
}

fn collect_native_module_requests(
    program: &TypedProgram,
    plan: &mut TypedModulePlan,
) -> Result<(), TypedModuleError> {
    let body = program_body(program)?;
    for &statement in program.list(body).expect("validated body list").items() {
        let record = program.node(statement).expect("validated statement");
        if matches!(
            record.data(),
            IrNodeData::ImportDeclaration { .. }
                | IrNodeData::ExportAllDeclaration { .. }
                | IrNodeData::ExportNamedDeclaration { .. }
                | IrNodeData::ExportDefaultDeclaration { .. }
        ) {
            plan.had_esm = true;
        }
        let source = match record.data() {
            IrNodeData::ImportDeclaration { source, .. }
            | IrNodeData::ExportAllDeclaration { source, .. } => Some(*source),
            IrNodeData::ExportNamedDeclaration {
                source: Some(source),
                ..
            } => Some(*source),
            _ => None,
        };
        let Some(source) = source else { continue };
        let specifier = string_value(program, source).ok_or_else(|| {
            module_error(
                TypedModulePhase::Plan,
                Some(source),
                "module source is not a string literal",
            )
        })?;
        plan.requests.push(TypedModuleRequestEdge {
            specifier: specifier.to_owned(),
            kind: TypedModuleRequestKind::StaticImport,
            origin: record.origin(),
        });
    }
    Ok(())
}

fn lower_esm_to_common_js(
    program: &mut TypedProgram,
    analysis: &TypedAnalysis,
    options: &TypedModuleOptions,
    plan: &mut TypedModulePlan,
    reserved: &mut HashSet<String>,
) -> Result<(), TypedModuleError> {
    let body = program_body(program)?;
    let original = program
        .list(body)
        .expect("validated body list")
        .items()
        .to_vec();
    let had_esm = original.iter().any(|&node| {
        matches!(
            program.node(node).expect("validated statement").data(),
            IrNodeData::ImportDeclaration { .. }
                | IrNodeData::ExportNamedDeclaration { .. }
                | IrNodeData::ExportDefaultDeclaration { .. }
                | IrNodeData::ExportAllDeclaration { .. }
        )
    });
    plan.had_esm = had_esm;
    let local_export_names = collect_local_export_names(program, &original)?;
    let module_bindings = analysis
        .scopes()
        .first()
        .into_iter()
        .flat_map(|scope| scope.symbols().iter().copied())
        .filter_map(|symbol| {
            program
                .symbol(symbol)
                .map(|record| (record.original_name().to_owned(), symbol))
        })
        .collect::<HashMap<_, _>>();
    let mut imports = HashMap::<SymbolId, ImportedBinding>::new();
    let mut eval_snapshots = Vec::<ImportedBinding>::new();
    let mut namespace_ordinal = 0_usize;

    for statement in original.iter().copied() {
        if !matches!(
            program.node(statement).map(|node| node.data()),
            Some(IrNodeData::ImportDeclaration { .. })
        ) {
            continue;
        }
        let replacements = lower_import_declaration(
            program,
            analysis,
            options,
            plan,
            reserved,
            statement,
            &mut namespace_ordinal,
            &local_export_names,
            &mut imports,
            &mut eval_snapshots,
        )?;
        replace_program_statement(program, statement, &replacements)?;
    }

    refresh_imports_before_direct_eval(program, analysis, &plan.default_read, &eval_snapshots)?;

    for statement in original.iter().copied() {
        if !matches!(
            program.node(statement).map(|node| node.data()),
            Some(
                IrNodeData::ExportNamedDeclaration { .. }
                    | IrNodeData::ExportDefaultDeclaration { .. }
                    | IrNodeData::ExportAllDeclaration { .. }
            )
        ) {
            continue;
        }
        let replacements = lower_export_declaration(
            program,
            options,
            plan,
            reserved,
            statement,
            &mut namespace_ordinal,
            &imports,
            &module_bindings,
        )?;
        replace_program_statement(program, statement, &replacements)?;
    }

    Ok(())
}

fn collect_local_export_names(
    program: &TypedProgram,
    statements: &[NodeId],
) -> Result<HashSet<String>, TypedModuleError> {
    let mut names = HashSet::new();
    for &statement in statements {
        let Some(IrNodeData::ExportNamedDeclaration {
            declaration: None,
            specifiers,
            source: None,
            ..
        }) = program.node(statement).map(|node| node.data())
        else {
            continue;
        };
        for &specifier in program
            .list(*specifiers)
            .expect("validated export specifiers")
            .items()
        {
            let IrNodeData::ExportSpecifier { local, .. } =
                program.node(specifier).expect("export specifier").data()
            else {
                return Err(module_error(
                    TypedModulePhase::Plan,
                    Some(specifier),
                    "export specifier list contains non-specifier syntax",
                ));
            };
            let local_name = module_name_text(program, *local).ok_or_else(|| {
                module_error(
                    TypedModulePhase::Plan,
                    Some(local.value),
                    "local export name is malformed",
                )
            })?;
            names.insert(local_name);
        }
    }
    Ok(names)
}

fn lower_import_declaration(
    program: &mut TypedProgram,
    analysis: &TypedAnalysis,
    options: &TypedModuleOptions,
    plan: &mut TypedModulePlan,
    reserved: &mut HashSet<String>,
    statement: NodeId,
    namespace_ordinal: &mut usize,
    local_export_names: &HashSet<String>,
    imported_bindings: &mut HashMap<SymbolId, ImportedBinding>,
    eval_snapshots: &mut Vec<ImportedBinding>,
) -> Result<Vec<NodeId>, TypedModuleError> {
    let origin = program.node(statement).expect("validated import").origin();
    let (specifiers, source, attributes) =
        match program.node(statement).expect("validated import").data() {
            IrNodeData::ImportDeclaration {
                specifiers,
                source,
                attributes,
            } => (*specifiers, *source, *attributes),
            _ => unreachable!("caller filtered imports"),
        };
    if attributes.is_some() && options.mode == TypedModuleMode::PreserveCommonJs {
        return Err(module_error(
            TypedModulePhase::Plan,
            Some(statement),
            "CommonJS lowering of import attributes is not semantics preserving",
        ));
    }
    let specifier = string_value(program, source)
        .ok_or_else(|| {
            module_error(
                TypedModulePhase::Plan,
                Some(source),
                "import source is not a string literal",
            )
        })?
        .to_owned();
    plan.requests.push(TypedModuleRequestEdge {
        specifier: specifier.clone(),
        kind: TypedModuleRequestKind::StaticImport,
        origin,
    });

    #[derive(Clone)]
    struct Specifier {
        kind: ImportSpecifierKind,
        binding: Binding,
        imported: Option<String>,
        live: bool,
        frozen: bool,
    }
    let mut entries = Vec::new();
    for &node in program
        .list(specifiers)
        .expect("validated import specifiers")
        .items()
    {
        let (kind, imported, local) = match program.node(node).expect("specifier").data() {
            IrNodeData::ImportSpecifier {
                kind,
                imported,
                local,
            } => (*kind, *imported, *local),
            _ => {
                return Err(module_error(
                    TypedModulePhase::Plan,
                    Some(node),
                    "import specifier list contains non-specifier syntax",
                ));
            }
        };
        let binding = binding_from_identifier(program, local)?;
        let imported = match imported {
            Some(name) => Some(module_name_text(program, name).ok_or_else(|| {
                module_error(
                    TypedModulePhase::Plan,
                    Some(name.value),
                    "imported module name is malformed",
                )
            })?),
            None => None,
        };
        let frozen = analysis
            .symbol(binding.symbol)
            .is_some_and(|facts| facts.is_frozen());
        let live = frozen
            || analysis
                .symbol(binding.symbol)
                .is_some_and(|facts| !facts.reads().is_empty())
            || options
                .linker_liveness
                .contains(options.module_id, binding.symbol)
            || (local_export_names.contains(&binding.name)
                && exported_symbol_live(options, binding.symbol));
        entries.push(Specifier {
            kind,
            binding,
            imported,
            live,
            frozen,
        });
    }

    let has_live = entries.iter().any(|entry| entry.live);
    if !has_live {
        let factory = SyntheticFactory::new(program);
        return Ok(vec![request_statement(
            &factory,
            &plan.static_import_request,
            &specifier,
            origin,
        )?]);
    }

    let namespace = entries
        .iter()
        .find(|entry| entry.live && entry.kind == ImportSpecifierKind::Namespace)
        .map(|entry| entry.binding.clone())
        .unwrap_or_else(|| {
            // Placeholder is overwritten immediately below; allocation is fallible and cannot be
            // expressed by `unwrap_or_else`.
            Binding {
                name: String::new(),
                symbol: SymbolId::MAX,
            }
        });
    let namespace = if namespace.symbol == SymbolId::MAX {
        let result = allocate_namespace_binding(program, reserved, *namespace_ordinal)?;
        *namespace_ordinal += 1;
        result
    } else {
        namespace
    };
    // Namespace interop must never overwrite the raw request binding used by default/named
    // imports. For plain CommonJS, `* as namespace` is a stable wrapper whose `.default` points
    // at raw `module.exports`; a simultaneous default import must still receive the raw value.
    // Allocate both identities before mangle so finalization only fills the structured sentinel
    // and every post-mangle reference can reuse the already committed spelling.
    let requires_namespace_interop = entries
        .iter()
        .any(|entry| entry.live && entry.kind == ImportSpecifierKind::Namespace);
    let request_namespace = if requires_namespace_interop {
        let result = allocate_namespace_binding(program, reserved, *namespace_ordinal)?;
        *namespace_ordinal += 1;
        result
    } else {
        namespace.clone()
    };
    plan.namespace_requests.push(NamespaceRequest {
        symbol: request_namespace.symbol,
        specifier: specifier.clone(),
        requires_namespace_interop: false,
    });
    let has_direct_eval = analysis
        .scopes()
        .iter()
        .any(|scope| scope.contains_direct_eval());

    for entry in &entries {
        if !entry.live {
            continue;
        }
        let entry_namespace = if entry.kind == ImportSpecifierKind::Namespace {
            namespace.clone()
        } else {
            request_namespace.clone()
        };
        let imported = ImportedBinding {
            symbol: entry.binding.symbol,
            local_name: entry.binding.name.clone(),
            imported_name: entry.imported.clone(),
            kind: entry.kind,
            namespace: entry_namespace,
        };
        // A dynamic-scope-visible binding remains a real lexical name for eval/with lookup.
        // Ordinary source reads still use the live namespace expression; direct eval refreshes
        // the local slot immediately before evaluation so an intervening dependency mutation is
        // observable without freezing unrelated scopes.
        replace_import_reads(
            program,
            analysis,
            &plan.default_read,
            &imported,
            entry.frozen,
        )?;
        imported_bindings.insert(entry.binding.symbol, imported.clone());
        if entry.frozen && has_direct_eval && entry.kind != ImportSpecifierKind::Namespace {
            eval_snapshots.push(imported);
        }
    }

    let factory = SyntheticFactory::new(program);
    let mut declarations = vec![request_declaration(
        &factory,
        &plan.static_import_request,
        &request_namespace,
        &specifier,
        origin,
    )?];
    if requires_namespace_interop {
        let raw = factory.reference(&request_namespace)?;
        let initializer = request_call(&factory, &plan.namespace_read, vec![raw], origin)?;
        declarations.push(variable_declaration(
            &factory,
            VarKind::Const,
            &namespace,
            initializer,
            origin,
        )?);
    }
    for entry in entries
        .iter()
        .filter(|entry| entry.live && entry.frozen && entry.kind != ImportSpecifierKind::Namespace)
    {
        let imported = ImportedBinding {
            symbol: entry.binding.symbol,
            local_name: entry.binding.name.clone(),
            imported_name: entry.imported.clone(),
            kind: entry.kind,
            namespace: request_namespace.clone(),
        };
        let initializer = import_value_expression(&factory, &plan.default_read, &imported, origin)?;
        declarations.push(variable_declaration(
            &factory,
            if has_direct_eval {
                VarKind::Let
            } else {
                VarKind::Const
            },
            &entry.binding,
            initializer,
            origin,
        )?);
    }
    Ok(declarations)
}

fn import_value_expression(
    factory: &SyntheticFactory<'_>,
    default_read: &TypedRuntimeBinding,
    imported: &ImportedBinding,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    let namespace = import_namespace_reference(factory, imported, origin)?;
    match imported.kind {
        ImportSpecifierKind::Namespace => Ok(namespace),
        ImportSpecifierKind::Named => {
            let property = factory.string(
                imported
                    .imported_name
                    .as_deref()
                    .unwrap_or(imported.local_name.as_str()),
            )?;
            Ok(factory.computed_member(namespace, property)?)
        }
        ImportSpecifierKind::Default => {
            request_call(factory, default_read, vec![namespace], origin)
        }
    }
}

fn import_namespace_reference(
    factory: &SyntheticFactory<'_>,
    imported: &ImportedBinding,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    // The generated identifier carries the namespace SymbolId and final namespace spelling, but
    // its source-map identity is the source import occurrence being replaced. Keeping those two
    // identities separate lets `(0, namespace.member)()` map `namespace` back to an aliased local
    // such as `local` without reintroducing a span/name side table.
    let identifier = factory.identifier(
        &imported.local_name,
        NameRole::Reference,
        Some(imported.namespace.symbol),
    )?;
    let (name_node, name) = {
        let program = factory.program.borrow();
        let IrNodeData::Identifier { name: name_node } = program
            .node(identifier)
            .expect("new import reference")
            .data()
        else {
            unreachable!("SyntheticFactory::identifier returned non-identifier syntax")
        };
        let IrNodeData::Name { name } = program
            .node(*name_node)
            .expect("new import reference name")
            .data()
        else {
            unreachable!("identifier child is not a name")
        };
        (*name_node, *name)
    };
    let mut program = factory.program.borrow_mut();
    program.set_emitted_name(name, imported.namespace.name.clone())?;
    program.set_origin(name_node, derived_origin(origin))?;
    program.set_origin(identifier, derived_origin(origin))?;
    Ok(identifier)
}

fn replace_import_reads(
    program: &mut TypedProgram,
    analysis: &TypedAnalysis,
    default_read: &TypedRuntimeBinding,
    imported: &ImportedBinding,
    preserve_with_body_reads: bool,
) -> Result<(), TypedModuleError> {
    if imported.kind == ImportSpecifierKind::Namespace
        && imported.symbol == imported.namespace.symbol
    {
        return Ok(());
    }
    let reads = analysis
        .symbol(imported.symbol)
        .map(|facts| facts.reads().to_vec())
        .unwrap_or_default();
    for read in reads {
        let Some(name_use) = analysis.name_use(read) else {
            continue;
        };
        if name_use.access() != NameAccess::Read {
            // Import bindings are immutable assignment targets. A read-write occurrence such as
            // `value++` must retain the original local target so the runtime preserves its normal
            // assignment error; replacing it with `namespace.value++` would mutate the export.
            continue;
        }
        let name_node = name_use.node();
        let Some(identifier) = program
            .node(name_node)
            .and_then(|node| node.parent())
            .map(|parent| parent.parent())
            .filter(|&identifier| {
                matches!(
                    program.node(identifier).map(|node| node.data()),
                    Some(IrNodeData::Identifier { name }) if *name == name_node
                )
            })
        else {
            // Export-local module names are handled while lowering their enclosing declaration.
            continue;
        };
        if program
            .node(identifier)
            .is_none_or(|node| node.is_tombstone())
            || ancestor_matches(program, identifier, |data| {
                matches!(
                    data,
                    IrNodeData::ImportSpecifier { .. } | IrNodeData::ExportSpecifier { .. }
                )
            })
            || (preserve_with_body_reads && is_inside_with_body(program, identifier))
        {
            continue;
        }
        let origin = program.node(identifier).expect("live identifier").origin();
        let strip_receiver = is_receiver_position(program, identifier);
        let factory = SyntheticFactory::new(program);
        let namespace = import_namespace_reference(&factory, imported, origin)?;
        let replacement = match imported.kind {
            ImportSpecifierKind::Namespace => namespace,
            ImportSpecifierKind::Named => {
                let property = factory.string(
                    imported
                        .imported_name
                        .as_deref()
                        .unwrap_or(imported.local_name.as_str()),
                )?;
                factory.computed_member(namespace, property)?
            }
            ImportSpecifierKind::Default => {
                request_call(&factory, default_read, vec![namespace], origin)?
            }
        };
        let replacement = if strip_receiver {
            sequence_zero(&factory, replacement, origin)?
        } else {
            set_derived_origin(&factory, replacement, origin)?
        };
        replace_expression_occurrence(&factory, identifier, replacement)?;
    }
    Ok(())
}

/// Refresh each imported lexical slot immediately before a direct eval which can resolve that
/// name. The generated `let` binding keeps the source spelling (mangling sees the same frozen
/// symbol), while ordinary statically visible reads continue to use namespace-backed live reads.
/// A nearer lexical declaration with the same spelling shadows the import, so that eval site must
/// not update the outer module slot.
fn refresh_imports_before_direct_eval(
    program: &mut TypedProgram,
    analysis: &TypedAnalysis,
    default_read: &TypedRuntimeBinding,
    imports: &[ImportedBinding],
) -> Result<(), TypedModuleError> {
    if imports.is_empty() {
        return Ok(());
    }
    let evals = program
        .preorder_validated()?
        .into_iter()
        .filter(|&node| is_direct_eval_call(program, node))
        .collect::<Vec<_>>();
    for eval in evals.into_iter().rev() {
        if program.node(eval).is_none_or(|node| node.is_tombstone()) {
            continue;
        }
        let visible = imports
            .iter()
            .filter(|imported| import_is_visible_at(analysis, program, imported, eval))
            .cloned()
            .collect::<Vec<_>>();
        if visible.is_empty() {
            continue;
        }
        let origin = program.node(eval).expect("live direct eval").origin();
        let original_eval = program.clone_detached_subtree(eval)?;
        let factory = SyntheticFactory::new(program);
        let mut expressions = Vec::with_capacity(visible.len() + 1);
        for imported in visible {
            let current = import_value_expression(&factory, default_read, &imported, origin)?;
            let target = factory.reference(&Binding {
                name: imported.local_name.clone(),
                symbol: imported.symbol,
            })?;
            let assignment = factory.assignment(target, current)?;
            expressions.push(set_derived_origin(&factory, assignment, origin)?);
        }
        expressions.push(original_eval);
        let sequence = factory.program.borrow_mut().append_detached_node_with(
            derived_origin(origin),
            |builder| {
                let expressions = builder.list(ChildRole::SequenceItems, expressions)?;
                Ok(IrNodeData::SequenceExpression { expressions })
            },
        )?;
        replace_expression_occurrence(&factory, eval, sequence)?;
    }
    Ok(())
}

fn is_direct_eval_call(program: &TypedProgram, node: NodeId) -> bool {
    let Some(IrNodeData::CallExpression {
        callee,
        optional: false,
        ..
    }) = program.node(node).map(|node| node.data())
    else {
        return false;
    };
    name_record(program, *callee).is_some_and(|(_, name)| {
        name.role() == NameRole::Reference
            && name.syntax() == NameSyntax::Identifier
            && name.symbol().is_none()
            && name.original() == "eval"
    })
}

fn import_is_visible_at(
    analysis: &TypedAnalysis,
    program: &TypedProgram,
    imported: &ImportedBinding,
    node: NodeId,
) -> bool {
    let Some(declaration_scope) = analysis
        .symbol(imported.symbol)
        .and_then(crate::typed_analysis::TypedSymbolFacts::declaration_scope)
    else {
        return false;
    };
    let Some(mut scope) = analysis.node_scope(node) else {
        return false;
    };
    while scope != declaration_scope {
        let Some(facts) = analysis.scope(scope) else {
            return false;
        };
        if facts.symbols().iter().copied().any(|symbol| {
            symbol != imported.symbol
                && program
                    .symbol(symbol)
                    .is_some_and(|record| record.original_name() == imported.local_name)
        }) {
            return false;
        }
        let Some(parent) = facts.parent() else {
            return false;
        };
        scope = parent;
    }
    true
}

fn is_inside_with_body(program: &TypedProgram, mut node: NodeId) -> bool {
    while let Some(parent) = program.node(node).and_then(|node| node.parent()) {
        if parent.role() == ChildRole::WithBody {
            return true;
        }
        node = parent.parent();
    }
    false
}

fn ancestor_matches(
    program: &TypedProgram,
    mut node: NodeId,
    predicate: impl Fn(&IrNodeData) -> bool,
) -> bool {
    while let Some(parent) = program.node(node).and_then(|node| node.parent()) {
        node = parent.parent();
        if predicate(program.node(node).expect("validated ancestor").data()) {
            return true;
        }
    }
    false
}

fn is_receiver_position(program: &TypedProgram, node: NodeId) -> bool {
    let Some(link) = program.node(node).and_then(|node| node.parent()) else {
        return false;
    };
    matches!(
        (
            program.node(link.parent()).map(|node| node.data()),
            link.role()
        ),
        (Some(IrNodeData::CallExpression { .. }), ChildRole::Callee)
            | (
                Some(IrNodeData::TaggedTemplateExpression { .. }),
                ChildRole::Tag
            )
    )
}

fn sequence_zero(
    factory: &SyntheticFactory<'_>,
    expression: NodeId,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    let zero = factory.number(0.0)?;
    let sequence = factory.program.borrow_mut().append_detached_node_with(
        derived_origin(origin),
        |builder| {
            let expressions = builder.list(ChildRole::SequenceItems, [zero, expression])?;
            Ok(IrNodeData::SequenceExpression { expressions })
        },
    )?;
    Ok(sequence)
}

fn replace_expression_occurrence(
    factory: &SyntheticFactory<'_>,
    target: NodeId,
    replacement: NodeId,
) -> Result<(), TypedModuleError> {
    let shorthand_property = {
        let program = factory.program.borrow();
        if let Some(link) = program.node(target).and_then(|node| node.parent())
            && link.role() == ChildRole::PropertyValue
        {
            match program.node(link.parent()).map(|node| node.data()) {
                Some(IrNodeData::ObjectProperty {
                    key,
                    kind,
                    method,
                    shorthand: true,
                    computed,
                    prototype_setter,
                    ..
                }) => Some((
                    link.parent(),
                    *key,
                    *kind,
                    *method,
                    *computed,
                    *prototype_setter,
                    program.node(link.parent()).expect("property").origin(),
                )),
                _ => None,
            }
        } else {
            None
        }
    };
    if let Some((property, key, kind, method, computed, prototype_setter, origin)) =
        shorthand_property
    {
        let key_value = factory
            .program
            .borrow_mut()
            .clone_detached_subtree(key.value)?;
        let rebuilt = factory.program.borrow_mut().append_detached_node_with(
            derived_origin(origin),
            |_| {
                Ok(IrNodeData::ObjectProperty {
                    key: IrPropertyKey {
                        kind: key.kind,
                        value: key_value,
                    },
                    value: replacement,
                    kind,
                    method,
                    shorthand: false,
                    computed,
                    prototype_setter,
                })
            },
        )?;
        factory
            .program
            .borrow_mut()
            .replace_node(property, rebuilt)?;
    } else {
        factory
            .program
            .borrow_mut()
            .replace_node(target, replacement)?;
    }
    Ok(())
}

fn replace_program_statement(
    program: &mut TypedProgram,
    target: NodeId,
    replacements: &[NodeId],
) -> Result<(), TypedModuleError> {
    let link = program
        .node(target)
        .and_then(|node| node.parent())
        .ok_or_else(|| {
            module_error(
                TypedModulePhase::Plan,
                Some(target),
                "module declaration is detached",
            )
        })?;
    if link.role() != ChildRole::ProgramBody {
        return Err(module_error(
            TypedModulePhase::Plan,
            Some(target),
            "nested module declaration is not legal",
        ));
    }
    let list = link.list().expect("program body relation is a list");
    let index = program
        .list(list)
        .expect("validated program body")
        .items()
        .iter()
        .position(|&item| item == target)
        .expect("attached target occurs in body");
    program.splice_list(list, index..index + 1, replacements)?;
    Ok(())
}

fn directive_prefix_len(program: &TypedProgram, body: ListId) -> usize {
    program
        .list(body)
        .expect("validated body")
        .items()
        .iter()
        .take_while(|&&statement| {
            matches!(
                program.node(statement).expect("statement").data(),
                IrNodeData::ExpressionStatement {
                    directive: true,
                    ..
                }
            )
        })
        .count()
}

#[allow(clippy::too_many_arguments)]
fn lower_export_declaration(
    program: &mut TypedProgram,
    options: &TypedModuleOptions,
    plan: &mut TypedModulePlan,
    reserved: &mut HashSet<String>,
    statement: NodeId,
    namespace_ordinal: &mut usize,
    imports: &HashMap<SymbolId, ImportedBinding>,
    module_bindings: &HashMap<String, SymbolId>,
) -> Result<Vec<NodeId>, TypedModuleError> {
    let origin = program.node(statement).expect("validated export").origin();
    let data = program
        .node(statement)
        .expect("validated export")
        .data()
        .clone();
    match data {
        IrNodeData::ExportNamedDeclaration {
            declaration,
            specifiers,
            source,
            attributes,
        } => {
            if attributes.is_some() && options.mode == TypedModuleMode::PreserveCommonJs {
                return Err(module_error(
                    TypedModulePhase::Plan,
                    Some(statement),
                    "CommonJS lowering of export attributes is not semantics preserving",
                ));
            }
            if let Some(declaration) = declaration {
                let bindings = declared_bindings(program, declaration)?;
                let cloned = program.clone_detached_subtree(declaration)?;
                let factory = SyntheticFactory::new(program);
                let mut output = vec![cloned];
                for binding in bindings {
                    if exported_symbol_live(options, binding.symbol)
                        && exported_name_live(options, &binding.name)
                    {
                        let value = factory.reference(&binding)?;
                        output.push(export_live_statement(
                            &factory,
                            plan,
                            &binding.name,
                            value,
                            origin,
                        )?);
                    }
                }
                return Ok(output);
            }

            let source_specifier = source
                .map(|source| {
                    string_value(program, source)
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            module_error(
                                TypedModulePhase::Plan,
                                Some(source),
                                "re-export source is not a string literal",
                            )
                        })
                })
                .transpose()?;
            let items = program
                .list(specifiers)
                .expect("validated export specifiers")
                .items()
                .to_vec();
            let mut retained = Vec::new();
            for specifier in items {
                let (local, exported) =
                    match program.node(specifier).expect("export specifier").data() {
                        IrNodeData::ExportSpecifier { local, exported } => (*local, *exported),
                        _ => {
                            return Err(module_error(
                                TypedModulePhase::Plan,
                                Some(specifier),
                                "export specifier list contains non-specifier syntax",
                            ));
                        }
                    };
                let exported_name = module_name_text(program, exported).ok_or_else(|| {
                    module_error(
                        TypedModulePhase::Plan,
                        Some(exported.value),
                        "exported name is malformed",
                    )
                })?;
                let local_name = source_specifier
                    .as_ref()
                    .map(|_| {
                        module_name_text(program, local).ok_or_else(|| {
                            module_error(
                                TypedModulePhase::Plan,
                                Some(local.value),
                                "re-export local name is malformed",
                            )
                        })
                    })
                    .transpose()?;
                let binding_live = if source_specifier.is_none() {
                    let binding =
                        resolve_local_export_binding(program, local, imports, module_bindings)?;
                    exported_symbol_live(options, binding.symbol)
                } else {
                    true
                };
                if exported_name_live(options, &exported_name) && binding_live {
                    retained.push((local, exported_name, local_name));
                }
            }

            if let Some(specifier) = source_specifier.as_deref() {
                plan.requests.push(TypedModuleRequestEdge {
                    specifier: specifier.to_owned(),
                    kind: TypedModuleRequestKind::StaticImport,
                    origin,
                });
                if retained.is_empty() {
                    let factory = SyntheticFactory::new(program);
                    return Ok(vec![request_statement(
                        &factory,
                        &plan.static_import_request,
                        specifier,
                        origin,
                    )?]);
                }
            }

            let namespace = if let Some(specifier) = source_specifier.as_deref() {
                let namespace = allocate_namespace_binding(program, reserved, *namespace_ordinal)?;
                *namespace_ordinal += 1;
                plan.namespace_requests.push(NamespaceRequest {
                    symbol: namespace.symbol,
                    specifier: specifier.to_owned(),
                    requires_namespace_interop: false,
                });
                Some(namespace)
            } else {
                None
            };
            let factory = SyntheticFactory::new(program);
            let mut output = Vec::new();
            if let (Some(namespace), Some(specifier)) =
                (namespace.as_ref(), source_specifier.as_deref())
            {
                output.push(request_declaration(
                    &factory,
                    &plan.static_import_request,
                    namespace,
                    specifier,
                    origin,
                )?);
            }
            for (local, exported_name, local_name) in retained {
                let value = if let Some(namespace) = namespace.as_ref() {
                    let local_name = local_name.expect("source re-export local name");
                    let object = factory.reference(namespace)?;
                    if local_name == "default" {
                        request_call(&factory, &plan.default_read, vec![object], origin)?
                    } else {
                        let property = factory.string(&local_name)?;
                        factory.computed_member(object, property)?
                    }
                } else {
                    export_local_value(&factory, plan, local, imports, module_bindings, origin)?
                };
                output.push(export_live_statement(
                    &factory,
                    plan,
                    &exported_name,
                    value,
                    origin,
                )?);
            }
            Ok(output)
        }
        IrNodeData::ExportAllDeclaration {
            exported,
            source,
            attributes,
        } => {
            if attributes.is_some() && options.mode == TypedModuleMode::PreserveCommonJs {
                return Err(module_error(
                    TypedModulePhase::Plan,
                    Some(statement),
                    "CommonJS lowering of export-all attributes is not semantics preserving",
                ));
            }
            let specifier = string_value(program, source)
                .ok_or_else(|| {
                    module_error(
                        TypedModulePhase::Plan,
                        Some(source),
                        "export-all source is not a string literal",
                    )
                })?
                .to_owned();
            plan.requests.push(TypedModuleRequestEdge {
                specifier: specifier.clone(),
                kind: TypedModuleRequestKind::StaticImport,
                origin,
            });
            let exported_name = exported
                .map(|exported| {
                    module_name_text(program, exported).ok_or_else(|| {
                        module_error(
                            TypedModulePhase::Plan,
                            Some(exported.value),
                            "export-all namespace name is malformed",
                        )
                    })
                })
                .transpose()?;
            let keep_forwarding = exported_name.as_deref().map_or_else(
                || options.preserve_all_exports || options.preserve_export_star,
                |name| exported_name_live(options, name),
            );
            if !keep_forwarding {
                let factory = SyntheticFactory::new(program);
                return Ok(vec![request_statement(
                    &factory,
                    &plan.static_import_request,
                    &specifier,
                    origin,
                )?]);
            }
            let namespace = allocate_namespace_binding(program, reserved, *namespace_ordinal)?;
            *namespace_ordinal += 1;
            plan.namespace_requests.push(NamespaceRequest {
                symbol: namespace.symbol,
                specifier: specifier.clone(),
                requires_namespace_interop: exported_name.is_some(),
            });
            let factory = SyntheticFactory::new(program);
            let mut output = vec![request_declaration(
                &factory,
                &plan.static_import_request,
                &namespace,
                &specifier,
                origin,
            )?];
            if let Some(name) = exported_name {
                let value = factory.reference(&namespace)?;
                output.push(export_live_statement(&factory, plan, &name, value, origin)?);
            } else {
                let exports = binding_reference(&factory, &plan.runtime.exports)?;
                let namespace = factory.reference(&namespace)?;
                let call = request_call(
                    &factory,
                    &plan.runtime.export_all,
                    vec![exports, namespace],
                    origin,
                )?;
                let statement = factory.expression_statement(call)?;
                output.push(set_derived_origin(&factory, statement, origin)?);
            }
            Ok(output)
        }
        IrNodeData::ExportDefaultDeclaration { value, kind } => {
            lower_export_default(program, options, plan, reserved, value, kind, origin)
        }
        _ => Err(module_error(
            TypedModulePhase::Plan,
            Some(statement),
            "expected an export declaration",
        )),
    }
}

fn declared_bindings(
    program: &TypedProgram,
    declaration: NodeId,
) -> Result<Vec<Binding>, TypedModuleError> {
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    match program
        .node(declaration)
        .expect("validated declaration")
        .data()
    {
        IrNodeData::VariableDeclaration { declarations, .. } => {
            for &declarator in program
                .list(*declarations)
                .expect("validated variable declarators")
                .items()
            {
                let IrNodeData::VariableDeclarator { binding, .. } = program
                    .node(declarator)
                    .expect("variable declarator")
                    .data()
                else {
                    return Err(module_error(
                        TypedModulePhase::Plan,
                        Some(declarator),
                        "variable declaration contains a non-declarator",
                    ));
                };
                collect_pattern_bindings(program, *binding, &mut seen, &mut output)?;
            }
        }
        IrNodeData::Function {
            name: Some(name), ..
        }
        | IrNodeData::Class {
            name: Some(name), ..
        } => push_owned_binding(program, *name, &mut seen, &mut output)?,
        IrNodeData::Function { name: None, .. } | IrNodeData::Class { name: None, .. } => {}
        _ => {
            return Err(module_error(
                TypedModulePhase::Plan,
                Some(declaration),
                "export declaration is not a variable, function or class declaration",
            ));
        }
    }
    Ok(output)
}

fn collect_pattern_bindings(
    program: &TypedProgram,
    pattern: NodeId,
    seen: &mut HashSet<SymbolId>,
    output: &mut Vec<Binding>,
) -> Result<(), TypedModuleError> {
    match program
        .node(pattern)
        .expect("validated binding pattern")
        .data()
    {
        IrNodeData::Identifier { .. } => push_owned_binding(program, pattern, seen, output),
        IrNodeData::ArrayPattern { elements } => {
            for &element in program
                .list(*elements)
                .expect("validated array pattern")
                .items()
            {
                collect_pattern_bindings(program, element, seen, output)?;
            }
            Ok(())
        }
        IrNodeData::ObjectPattern { properties, rest } => {
            for &property in program
                .list(*properties)
                .expect("validated object pattern")
                .items()
            {
                collect_pattern_bindings(program, property, seen, output)?;
            }
            if let Some(rest) = rest {
                collect_pattern_bindings(program, *rest, seen, output)?;
            }
            Ok(())
        }
        IrNodeData::ObjectPatternProperty { value, .. } => {
            collect_pattern_bindings(program, *value, seen, output)
        }
        IrNodeData::AssignmentPattern { left, .. } => {
            collect_pattern_bindings(program, *left, seen, output)
        }
        IrNodeData::RestPattern { argument } => {
            collect_pattern_bindings(program, *argument, seen, output)
        }
        other => Err(module_error(
            TypedModulePhase::Plan,
            Some(pattern),
            format!("unsupported exported binding pattern {other:?}"),
        )),
    }
}

fn push_owned_binding(
    program: &TypedProgram,
    identifier: NodeId,
    seen: &mut HashSet<SymbolId>,
    output: &mut Vec<Binding>,
) -> Result<(), TypedModuleError> {
    let (_, name) = name_record(program, identifier).ok_or_else(|| {
        module_error(
            TypedModulePhase::Plan,
            Some(identifier),
            "exported declaration binding is not an identifier",
        )
    })?;
    let symbol = name.symbol().ok_or_else(|| {
        module_error(
            TypedModulePhase::Plan,
            Some(identifier),
            format!(
                "exported binding `{}` has no owned SymbolId",
                name.original()
            ),
        )
    })?;
    if seen.insert(symbol) {
        output.push(Binding {
            name: name.original().to_owned(),
            symbol,
        });
    }
    Ok(())
}

fn exported_symbol_live(options: &TypedModuleOptions, symbol: SymbolId) -> bool {
    options.preserve_all_exports || options.linker_liveness.contains(options.module_id, symbol)
}

fn exported_name_live(options: &TypedModuleOptions, name: &str) -> bool {
    options.preserve_all_exports || options.observed_export_names.contains(name)
}

fn export_local_value(
    factory: &SyntheticFactory<'_>,
    plan: &TypedModulePlan,
    local: IrModuleName,
    imports: &HashMap<SymbolId, ImportedBinding>,
    module_bindings: &HashMap<String, SymbolId>,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    let binding =
        resolve_local_export_binding(&factory.program.borrow(), local, imports, module_bindings)?;
    let Some(imported) = imports.get(&binding.symbol) else {
        return Ok(factory.reference(&binding)?);
    };
    let namespace = import_namespace_reference(factory, imported, origin)?;
    match imported.kind {
        ImportSpecifierKind::Namespace => Ok(namespace),
        ImportSpecifierKind::Named => {
            let property = factory.string(
                imported
                    .imported_name
                    .as_deref()
                    .unwrap_or(imported.local_name.as_str()),
            )?;
            Ok(factory.computed_member(namespace, property)?)
        }
        ImportSpecifierKind::Default => {
            request_call(factory, &plan.default_read, vec![namespace], origin)
        }
    }
}

fn resolve_local_export_binding(
    program: &TypedProgram,
    local: IrModuleName,
    imports: &HashMap<SymbolId, ImportedBinding>,
    module_bindings: &HashMap<String, SymbolId>,
) -> Result<Binding, TypedModuleError> {
    let (_, name) = name_record(program, local.value).ok_or_else(|| {
        module_error(
            TypedModulePhase::Plan,
            Some(local.value),
            "local export name has no semantic occurrence",
        )
    })?;
    let original = name.original();
    let symbol = name
        .symbol()
        .or_else(|| module_bindings.get(original).copied())
        .or_else(|| {
            let mut matches = imports
                .values()
                .filter(|imported| imported.local_name == original)
                .map(|imported| imported.symbol);
            let symbol = matches.next()?;
            matches
                .all(|candidate| candidate == symbol)
                .then_some(symbol)
        })
        .ok_or_else(|| {
            module_error(
                TypedModulePhase::Plan,
                Some(local.value),
                format!("local export `{original}` has no unambiguous owned SymbolId"),
            )
        })?;
    Ok(Binding {
        name: original.to_owned(),
        symbol,
    })
}

fn export_live_statement(
    factory: &SyntheticFactory<'_>,
    plan: &TypedModulePlan,
    exported: &str,
    value: NodeId,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    let getter = ordinary_function_expression(factory, &[], value)?;
    let exports = binding_reference(factory, &plan.runtime.exports)?;
    let name = factory.string(exported)?;
    let callee = binding_reference(factory, &plan.runtime.export_live)?;
    let call = factory.call(callee, vec![exports, name, getter])?;
    let call = set_derived_origin(factory, call, origin)?;
    let statement = factory.expression_statement(call)?;
    set_derived_origin(factory, statement, origin)
}

fn lower_export_default(
    program: &mut TypedProgram,
    options: &TypedModuleOptions,
    plan: &mut TypedModulePlan,
    reserved: &mut HashSet<String>,
    value: NodeId,
    kind: ExportDefaultValueKind,
    origin: IrOrigin,
) -> Result<Vec<NodeId>, TypedModuleError> {
    let named = match program.node(value).expect("default value").data() {
        IrNodeData::Function { name, .. } | IrNodeData::Class { name, .. } => {
            name.and_then(|name| binding_from_identifier(program, name).ok())
        }
        _ => None,
    };
    if let Some(binding) = named {
        let declaration = match kind {
            ExportDefaultValueKind::Function => {
                clone_function_context(program, value, FunctionContext::Declaration)?
            }
            ExportDefaultValueKind::Class => {
                clone_class_context(program, value, ClassContext::Declaration)?
            }
            ExportDefaultValueKind::Expression => {
                return Err(module_error(
                    TypedModulePhase::Plan,
                    Some(value),
                    "default expression unexpectedly carries a declaration name",
                ));
            }
        };
        if exported_symbol_live(options, binding.symbol) && exported_name_live(options, "default") {
            let factory = SyntheticFactory::new(program);
            let reference = factory.reference(&binding)?;
            let getter = export_live_statement(&factory, plan, "default", reference, origin)?;
            return Ok(vec![declaration, getter]);
        }
        return Ok(vec![declaration]);
    }

    let runtime =
        allocate_runtime_binding(program, reserved, "__wake_default_export", DeclKind::Const)?;
    plan.local_bindings.push(runtime.clone());
    let binding = Binding::from(&runtime);
    let initializer = match kind {
        ExportDefaultValueKind::Function => {
            clone_function_context(program, value, FunctionContext::Expression)?
        }
        ExportDefaultValueKind::Class => {
            clone_class_context(program, value, ClassContext::Expression)?
        }
        ExportDefaultValueKind::Expression => program.clone_detached_subtree(value)?,
    };
    let factory = SyntheticFactory::new(program);
    let declaration =
        variable_declaration(&factory, VarKind::Const, &binding, initializer, origin)?;
    if exported_name_live(options, "default") {
        let reference = factory.reference(&binding)?;
        let getter = export_live_statement(&factory, plan, "default", reference, origin)?;
        Ok(vec![declaration, getter])
    } else {
        Ok(vec![declaration])
    }
}

fn clone_function_context(
    program: &mut TypedProgram,
    source: NodeId,
    context: FunctionContext,
) -> Result<NodeId, TypedModuleError> {
    let (name, parameters, body, is_async, is_generator, origin) =
        match program.node(source).expect("function source").data() {
            IrNodeData::Function {
                name,
                parameters,
                body,
                is_async,
                is_generator,
                ..
            } => (
                *name,
                *parameters,
                *body,
                *is_async,
                *is_generator,
                program.node(source).expect("function source").origin(),
            ),
            _ => {
                return Err(module_error(
                    TypedModulePhase::Plan,
                    Some(source),
                    "default function payload is not a Function node",
                ));
            }
        };
    let name = name
        .map(|node| program.clone_detached_subtree(node))
        .transpose()?;
    let parameters = program
        .list(parameters)
        .expect("function parameters")
        .items()
        .to_vec()
        .into_iter()
        .map(|node| program.clone_detached_subtree(node))
        .collect::<Result<Vec<_>, _>>()?;
    let body = body
        .map(|node| program.clone_detached_subtree(node))
        .transpose()?;
    Ok(
        program.append_detached_node_with(derived_origin(origin), |builder| {
            let parameters = builder.list(ChildRole::FunctionParameters, parameters)?;
            Ok(IrNodeData::Function {
                context,
                name,
                parameters,
                body,
                is_async,
                is_generator,
            })
        })?,
    )
}

fn clone_class_context(
    program: &mut TypedProgram,
    source: NodeId,
    context: ClassContext,
) -> Result<NodeId, TypedModuleError> {
    let (name, super_class, members, decorators, origin) =
        match program.node(source).expect("class source").data() {
            IrNodeData::Class {
                name,
                super_class,
                members,
                decorators,
                ..
            } => (
                *name,
                *super_class,
                *members,
                *decorators,
                program.node(source).expect("class source").origin(),
            ),
            _ => {
                return Err(module_error(
                    TypedModulePhase::Plan,
                    Some(source),
                    "default class payload is not a Class node",
                ));
            }
        };
    let name = name
        .map(|node| program.clone_detached_subtree(node))
        .transpose()?;
    let super_class = super_class
        .map(|node| program.clone_detached_subtree(node))
        .transpose()?;
    let members = program
        .list(members)
        .expect("class members")
        .items()
        .to_vec()
        .into_iter()
        .map(|node| program.clone_detached_subtree(node))
        .collect::<Result<Vec<_>, _>>()?;
    let decorators = program
        .list(decorators)
        .expect("class decorators")
        .items()
        .to_vec()
        .into_iter()
        .map(|node| program.clone_detached_subtree(node))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(
        program.append_detached_node_with(derived_origin(origin), |builder| {
            let members = builder.list(ChildRole::ClassMembers, members)?;
            let decorators = builder.list(ChildRole::Decorators, decorators)?;
            Ok(IrNodeData::Class {
                context,
                name,
                super_class,
                members,
                decorators,
            })
        })?,
    )
}

fn abstractify_runtime_requests(
    program: &mut TypedProgram,
    plan: &mut TypedModulePlan,
) -> Result<(), TypedModuleError> {
    let nodes = program.preorder_validated()?;
    for node in nodes.into_iter().rev() {
        let Some(record) = program.node(node) else {
            continue;
        };
        if record.is_tombstone() {
            continue;
        }
        let origin = record.origin();
        match record.data().clone() {
            IrNodeData::CallExpression {
                callee,
                arguments,
                optional: false,
            } if is_unresolved_require(program, callee) => {
                let items = program
                    .list(arguments)
                    .expect("validated call arguments")
                    .items();
                if items.len() != 1 {
                    continue;
                }
                let Some(specifier) = string_value(program, items[0]).map(str::to_owned) else {
                    continue;
                };
                let source = program.clone_detached_subtree(items[0])?;
                let factory = SyntheticFactory::new(program);
                let replacement =
                    request_call(&factory, &plan.require_request, vec![source], origin)?;
                factory
                    .program
                    .borrow_mut()
                    .replace_node(node, replacement)?;
                plan.requests.push(TypedModuleRequestEdge {
                    specifier,
                    kind: TypedModuleRequestKind::Require,
                    origin,
                });
            }
            IrNodeData::ImportExpression { source, options } => {
                let Some(specifier) = string_value(program, source).map(str::to_owned) else {
                    continue;
                };
                let source = program.clone_detached_subtree(source)?;
                let options = options
                    .map(|options| program.clone_detached_subtree(options))
                    .transpose()?;
                let mut arguments = vec![source];
                if let Some(options) = options {
                    arguments.push(options);
                }
                let factory = SyntheticFactory::new(program);
                let replacement = request_call(&factory, &plan.dynamic_request, arguments, origin)?;
                factory
                    .program
                    .borrow_mut()
                    .replace_node(node, replacement)?;
                plan.requests.push(TypedModuleRequestEdge {
                    specifier,
                    kind: TypedModuleRequestKind::DynamicImport,
                    origin,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_unresolved_require(program: &TypedProgram, callee: NodeId) -> bool {
    let Some((_, name)) = name_record(program, callee) else {
        return false;
    };
    name.role() == NameRole::Reference
        && name.syntax() == NameSyntax::Identifier
        && name.symbol().is_none()
        && name.original() == "require"
}

fn validate_final_facts(
    facts: &TypedFinalModuleFacts,
) -> Result<HashMap<(String, TypedModuleRequestKind), TypedFinalModuleTarget>, TypedModuleError> {
    let mut resolved = HashMap::new();
    for module in &facts.modules {
        if module.specifier.is_empty() {
            return Err(TypedModuleError::InvalidInput {
                phase: TypedModulePhase::Finalize,
                message: "resolved module specifier must not be empty".into(),
            });
        }
        if let TypedFinalModuleTarget::External {
            rewritten_specifier,
        } = &module.target
            && rewritten_specifier.is_empty()
        {
            return Err(TypedModuleError::InvalidInput {
                phase: TypedModulePhase::Finalize,
                message: format!(
                    "external rewrite for `{}` must not be empty",
                    module.specifier
                ),
            });
        }
        if resolved
            .insert(
                (module.specifier.clone(), module.request_kind),
                module.target.clone(),
            )
            .is_some()
        {
            return Err(TypedModuleError::InvalidInput {
                phase: TypedModulePhase::Finalize,
                message: format!(
                    "duplicate final facts for `{}'/{:?}",
                    module.specifier, module.request_kind
                ),
            });
        }
    }
    for (source, rewrite) in &facts.specifier_rewrites {
        if source.is_empty() || rewrite.is_empty() {
            return Err(TypedModuleError::InvalidInput {
                phase: TypedModulePhase::Finalize,
                message: "specifier rewrites must have non-empty source and target".into(),
            });
        }
    }
    let mut rewrite_keys = HashSet::new();
    for rewrite in &facts.request_rewrites {
        if rewrite.specifier.is_empty() || rewrite.rewritten_specifier.is_empty() {
            return Err(TypedModuleError::InvalidInput {
                phase: TypedModulePhase::Finalize,
                message: "kind-specific rewrites must have non-empty source and target".into(),
            });
        }
        if !rewrite_keys.insert((rewrite.specifier.clone(), rewrite.request_kind)) {
            return Err(TypedModuleError::InvalidInput {
                phase: TypedModulePhase::Finalize,
                message: format!(
                    "duplicate kind-specific rewrite for `{}'/{:?}",
                    rewrite.specifier, rewrite.request_kind
                ),
            });
        }
    }
    Ok(resolved)
}

fn resolved_target<'a>(
    resolved: &'a HashMap<(String, TypedModuleRequestKind), TypedFinalModuleTarget>,
    specifier: &str,
    kind: TypedModuleRequestKind,
) -> Option<&'a TypedFinalModuleTarget> {
    resolved.get(&(specifier.to_owned(), kind)).or_else(|| {
        (kind == TypedModuleRequestKind::DynamicImport)
            .then(|| resolved.get(&(specifier.to_owned(), TypedModuleRequestKind::StaticImport)))
            .flatten()
    })
}

fn rewrite_with_kind<'a>(
    facts: &'a TypedFinalModuleFacts,
    specifier: &str,
    kind: TypedModuleRequestKind,
) -> Option<&'a str> {
    facts
        .request_rewrites
        .iter()
        .find(|rewrite| rewrite.specifier == specifier && rewrite.request_kind == kind)
        .map(|rewrite| rewrite.rewritten_specifier.as_str())
        .or_else(|| facts.specifier_rewrites.get(specifier).map(String::as_str))
}

fn apply_external_rewrite(
    target: TypedFinalModuleTarget,
    facts: &TypedFinalModuleFacts,
    specifier: &str,
    kind: TypedModuleRequestKind,
) -> TypedFinalModuleTarget {
    match target {
        TypedFinalModuleTarget::External {
            rewritten_specifier,
        } => TypedFinalModuleTarget::External {
            rewritten_specifier: rewrite_with_kind(facts, specifier, kind)
                .unwrap_or(&rewritten_specifier)
                .to_owned(),
        },
        internal => internal,
    }
}

fn finalize_requests(
    program: &mut TypedProgram,
    plan: &mut TypedModulePlan,
    resolved: &HashMap<(String, TypedModuleRequestKind), TypedFinalModuleTarget>,
    facts: &TypedFinalModuleFacts,
    report: &mut TypedFinalModuleReport,
    discarded_static_requests: &mut Vec<TypedDiscardedStaticRequest>,
) -> Result<(), TypedModuleError> {
    let nodes = program.preorder_validated()?;
    for node in nodes.into_iter().rev() {
        let Some(record) = program.node(node) else {
            continue;
        };
        if record.is_tombstone() {
            continue;
        }
        let IrNodeData::CallExpression {
            callee, arguments, ..
        } = record.data()
        else {
            continue;
        };
        let Some(symbol) = identifier_symbol(program, *callee) else {
            continue;
        };
        let kind = if symbol == plan.static_import_request.symbol {
            Some(TypedModuleRequestKind::StaticImport)
        } else if symbol == plan.require_request.symbol {
            Some(TypedModuleRequestKind::Require)
        } else if symbol == plan.dynamic_request.symbol {
            Some(TypedModuleRequestKind::DynamicImport)
        } else {
            None
        };
        if symbol == plan.default_read.symbol {
            finalize_default_read(program, plan, node, *arguments, resolved, facts, report)?;
            continue;
        }
        if symbol == plan.namespace_read.symbol {
            finalize_namespace_read(program, plan, node, *arguments, resolved, facts)?;
            continue;
        }
        let Some(kind) = kind else { continue };
        let arguments = program
            .list(*arguments)
            .expect("validated request arguments")
            .items()
            .to_vec();
        if arguments.is_empty() {
            return Err(module_error(
                TypedModulePhase::Finalize,
                Some(node),
                "module request sentinel has no source argument",
            ));
        }
        let specifier = string_value(program, arguments[0])
            .ok_or_else(|| {
                module_error(
                    TypedModulePhase::Finalize,
                    Some(arguments[0]),
                    "module request source is not a string literal",
                )
            })?
            .to_owned();
        let target = resolved_target(resolved, &specifier, kind)
            .cloned()
            .unwrap_or_else(|| TypedFinalModuleTarget::External {
                rewritten_specifier: rewrite_with_kind(facts, &specifier, kind)
                    .unwrap_or(&specifier)
                    .to_owned(),
            });
        let target = apply_external_rewrite(target, facts, &specifier, kind);
        let discarded_static_target = if kind == TypedModuleRequestKind::StaticImport
            && static_request_value_is_discarded(program, node)
        {
            match &target {
                TypedFinalModuleTarget::Internal {
                    module_id,
                    async_dependency: false,
                    ..
                } => Some(*module_id),
                TypedFinalModuleTarget::Internal {
                    async_dependency: true,
                    ..
                }
                | TypedFinalModuleTarget::External { .. } => None,
            }
        } else {
            None
        };
        let origin = program.node(node).expect("request node").origin();
        let replacement = match kind {
            TypedModuleRequestKind::StaticImport => {
                if arguments.len() != 1 {
                    return Err(module_error(
                        TypedModulePhase::Finalize,
                        Some(node),
                        "static request sentinel must have exactly one argument",
                    ));
                }
                report.lowered_static_requests += 1;
                let namespace = request_namespace_symbol(program, node);
                let namespace_interop = namespace.is_some_and(|symbol| {
                    plan.namespace_requires_interop(symbol)
                        && !matches!(
                            target,
                            TypedFinalModuleTarget::Internal { is_esm: true, .. }
                        )
                });
                final_static_request(
                    program,
                    plan,
                    &target,
                    origin,
                    true,
                    namespace_interop,
                    report,
                )?
            }
            TypedModuleRequestKind::Require => {
                if arguments.len() != 1 {
                    return Err(module_error(
                        TypedModulePhase::Finalize,
                        Some(node),
                        "require sentinel must have exactly one argument",
                    ));
                }
                report.lowered_require_requests += 1;
                final_static_request(program, plan, &target, origin, false, false, report)?
            }
            TypedModuleRequestKind::DynamicImport => {
                if arguments.len() > 2 {
                    return Err(module_error(
                        TypedModulePhase::Finalize,
                        Some(node),
                        "dynamic request sentinel has too many arguments",
                    ));
                }
                let options = arguments
                    .get(1)
                    .copied()
                    .map(|options| program.clone_detached_subtree(options))
                    .transpose()?;
                report.lowered_dynamic_requests += 1;
                final_dynamic_request(
                    program,
                    plan,
                    &target,
                    options,
                    origin,
                    facts.lower_external_dynamic_to_require,
                )?
            }
        };
        program.replace_node(node, replacement)?;
        if let Some(target) = discarded_static_target {
            discarded_static_requests.push(TypedDiscardedStaticRequest {
                node: replacement,
                target,
            });
        }
    }
    Ok(())
}

/// Static imports are module-top-level syntax. Statement merging may place their generated request
/// into a root sequence, so walk only through direct sequence parents and require the enclosing
/// expression statement to belong to the Program body. A non-final sequence operand is discarded
/// regardless of the outer value; the final operand is discarded only when that outer sequence is.
fn static_request_value_is_discarded(program: &TypedProgram, request: NodeId) -> bool {
    let mut current = request;
    loop {
        let Some(parent_link) = program.node(current).and_then(IrNode::parent) else {
            return false;
        };
        let parent = parent_link.parent();
        match program.node(parent).map(IrNode::data) {
            Some(IrNodeData::SequenceExpression { expressions }) => {
                let Some(items) = program.list(*expressions).map(|list| list.items()) else {
                    return false;
                };
                let Some(&last) = items.last() else {
                    return false;
                };
                // Parent/list membership was validated before finalization. Only the final
                // sequence element contributes the sequence value, so this proof remains O(1)
                // even for generated barrels with thousands of adjacent requests.
                if last != current {
                    return sequence_is_in_program_expression_statement(program, parent);
                }
                current = parent;
            }
            Some(IrNodeData::ExpressionStatement { expression, .. }) if *expression == current => {
                return expression_statement_is_in_program(program, parent);
            }
            _ => return false,
        }
    }
}

fn sequence_is_in_program_expression_statement(
    program: &TypedProgram,
    mut sequence: NodeId,
) -> bool {
    loop {
        let Some(parent) = program
            .node(sequence)
            .and_then(IrNode::parent)
            .map(|link| link.parent())
        else {
            return false;
        };
        match program.node(parent).map(IrNode::data) {
            Some(IrNodeData::SequenceExpression { .. }) => sequence = parent,
            Some(IrNodeData::ExpressionStatement { expression, .. }) if *expression == sequence => {
                return expression_statement_is_in_program(program, parent);
            }
            _ => return false,
        }
    }
}

fn expression_statement_is_in_program(program: &TypedProgram, statement: NodeId) -> bool {
    program
        .node(statement)
        .and_then(IrNode::parent)
        .is_some_and(|parent| {
            matches!(
                program.node(parent.parent()).map(IrNode::data),
                Some(IrNodeData::Program { .. })
            )
        })
}

fn identifier_symbol(program: &TypedProgram, node: NodeId) -> Option<SymbolId> {
    name_record(program, node).and_then(|(_, name)| name.symbol())
}

fn request_namespace_symbol(program: &TypedProgram, request: NodeId) -> Option<SymbolId> {
    let declarator = program.node(request)?.parent()?.parent();
    let IrNodeData::VariableDeclarator { binding, .. } = program.node(declarator)?.data() else {
        return None;
    };
    identifier_symbol(program, *binding)
}

fn final_static_request(
    program: &mut TypedProgram,
    plan: &mut TypedModulePlan,
    target: &TypedFinalModuleTarget,
    origin: IrOrigin,
    allow_await: bool,
    namespace_interop: bool,
    report: &mut TypedFinalModuleReport,
) -> Result<NodeId, TypedModuleError> {
    let factory = SyntheticFactory::new(program);
    let loaded = match target {
        TypedFinalModuleTarget::External {
            rewritten_specifier,
        } => {
            let source = factory.string(rewritten_specifier)?;
            let require = factory.global("require")?;
            let call = factory.call(require, vec![source])?;
            set_derived_origin(&factory, call, origin)
        }
        TypedFinalModuleTarget::Internal {
            module_id,
            async_dependency,
            ..
        } => {
            let id = factory.number(f64::from(module_id.0))?;
            let require = binding_reference(&factory, &plan.runtime.internal_require)?;
            let call = factory.call(require, vec![id])?;
            let call = set_derived_origin(&factory, call, origin)?;
            if *async_dependency && allow_await {
                report.requires_async_module = true;
                let awaited = factory
                    .program
                    .borrow_mut()
                    .append_detached_node_with(derived_origin(origin), |_| {
                        Ok(IrNodeData::AwaitExpression { argument: call })
                    })?;
                Ok(awaited)
            } else {
                Ok(call)
            }
        }
    }?;
    if namespace_interop {
        namespace_interop_expression(&factory, plan, loaded, origin)
    } else {
        Ok(loaded)
    }
}

fn final_dynamic_request(
    program: &mut TypedProgram,
    plan: &TypedModulePlan,
    target: &TypedFinalModuleTarget,
    options: Option<NodeId>,
    origin: IrOrigin,
    lower_external_to_require: bool,
) -> Result<NodeId, TypedModuleError> {
    let factory = SyntheticFactory::new(program);
    match target {
        TypedFinalModuleTarget::External {
            rewritten_specifier,
        } => {
            let source = factory.string(rewritten_specifier)?;
            if lower_external_to_require && options.is_none() {
                let require = factory.global("require")?;
                let loaded = factory.call(require, vec![source])?;
                let promise = factory.global("Promise")?;
                let resolve = factory.member(promise, "resolve")?;
                let call = factory.call(resolve, vec![loaded])?;
                return set_derived_origin(&factory, call, origin);
            }
            Ok(factory
                .program
                .borrow_mut()
                .append_detached_node_with(derived_origin(origin), |_| {
                    Ok(IrNodeData::ImportExpression { source, options })
                })?)
        }
        TypedFinalModuleTarget::Internal {
            module_id,
            dynamic_chunk,
            ..
        } => {
            let id = factory.number(f64::from(module_id.0))?;
            if let Some(chunk) = dynamic_chunk {
                let require = binding_reference(&factory, &plan.runtime.internal_require)?;
                let import = factory.member(require, "import")?;
                let chunk = factory.number(f64::from(chunk.0))?;
                let call = factory.call(import, vec![chunk, id])?;
                set_derived_origin(&factory, call, origin)
            } else {
                let require = binding_reference(&factory, &plan.runtime.internal_require)?;
                let loaded = factory.call(require, vec![id])?;
                let promise = factory.global("Promise")?;
                let resolve = factory.member(promise, "resolve")?;
                let call = factory.call(resolve, vec![loaded])?;
                set_derived_origin(&factory, call, origin)
            }
        }
    }
}

fn namespace_interop_expression(
    factory: &SyntheticFactory<'_>,
    plan: &mut TypedModulePlan,
    loaded: NodeId,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    let mut reserved = collect_reserved_names(&factory.program.borrow());
    let value = allocate_runtime_binding(
        &mut factory.program.borrow_mut(),
        &mut reserved,
        "__wake_namespace_value",
        DeclKind::Param,
    )?;
    plan.local_bindings.push(value.clone());
    let value = Binding::from(&value);

    let truthy = factory.reference(&value)?;
    let marker_object = factory.reference(&value)?;
    let marker = factory.member(marker_object, "__esModule")?;
    let test = factory.logical(LogicalOperator::And, truthy, marker)?;
    let consequent = factory.reference(&value)?;

    let object = factory.global("Object")?;
    let assign = factory.member(object, "assign")?;
    let empty = factory.object(Vec::new())?;
    let named = factory.reference(&value)?;
    let default_value = factory.reference(&value)?;
    let default_property = factory.data_property("default", default_value)?;
    let default_object = factory.object(vec![default_property])?;
    let alternate = factory.call(assign, vec![empty, named, default_object])?;
    let body = factory.conditional(test, consequent, alternate)?;
    let wrapper = ordinary_function_expression(factory, std::slice::from_ref(&value), body)?;
    let call = factory.call(wrapper, vec![loaded])?;
    set_derived_origin(factory, call, origin)
}

fn finalize_namespace_read(
    program: &mut TypedProgram,
    plan: &mut TypedModulePlan,
    request: NodeId,
    arguments: ListId,
    resolved: &HashMap<(String, TypedModuleRequestKind), TypedFinalModuleTarget>,
    facts: &TypedFinalModuleFacts,
) -> Result<(), TypedModuleError> {
    let arguments = program
        .list(arguments)
        .expect("validated namespace-read arguments")
        .items()
        .to_vec();
    if arguments.len() != 1 {
        return Err(module_error(
            TypedModulePhase::Finalize,
            Some(request),
            "namespace-read sentinel must have exactly one raw namespace argument",
        ));
    }
    let raw_symbol = identifier_symbol(program, arguments[0]).ok_or_else(|| {
        module_error(
            TypedModulePhase::Finalize,
            Some(arguments[0]),
            "namespace-read argument has no owned SymbolId",
        )
    })?;
    let specifier = plan
        .namespace_specifier(raw_symbol)
        .ok_or_else(|| {
            module_error(
                TypedModulePhase::Finalize,
                Some(request),
                format!("raw namespace symbol {raw_symbol} has no module request"),
            )
        })?
        .to_owned();
    let target = resolved_target(resolved, &specifier, TypedModuleRequestKind::StaticImport)
        .cloned()
        .unwrap_or_else(|| TypedFinalModuleTarget::External {
            rewritten_specifier: rewrite_with_kind(
                facts,
                &specifier,
                TypedModuleRequestKind::StaticImport,
            )
            .unwrap_or(&specifier)
            .to_owned(),
        });
    let origin = program.node(request).expect("namespace read").origin();
    let raw = binding_for_symbol_current(program, raw_symbol)?;
    let factory = SyntheticFactory::new(program);
    let raw = current_binding_reference(&factory, &raw)?;
    let replacement = match target {
        TypedFinalModuleTarget::Internal { is_esm: true, .. } => {
            set_derived_origin(&factory, raw, origin)?
        }
        TypedFinalModuleTarget::Internal { is_esm: false, .. }
        | TypedFinalModuleTarget::External { .. } => {
            namespace_interop_expression(&factory, plan, raw, origin)?
        }
    };
    factory
        .program
        .borrow_mut()
        .replace_node(request, replacement)?;
    Ok(())
}

fn finalize_default_read(
    program: &mut TypedProgram,
    plan: &TypedModulePlan,
    request: NodeId,
    arguments: ListId,
    resolved: &HashMap<(String, TypedModuleRequestKind), TypedFinalModuleTarget>,
    facts: &TypedFinalModuleFacts,
    report: &mut TypedFinalModuleReport,
) -> Result<(), TypedModuleError> {
    let arguments = program
        .list(arguments)
        .expect("validated default-read arguments")
        .items()
        .to_vec();
    if arguments.len() != 1 {
        return Err(module_error(
            TypedModulePhase::Finalize,
            Some(request),
            "default-read sentinel must have exactly one namespace argument",
        ));
    }
    let namespace_symbol = identifier_symbol(program, arguments[0]).ok_or_else(|| {
        module_error(
            TypedModulePhase::Finalize,
            Some(arguments[0]),
            "default-read namespace has no owned SymbolId",
        )
    })?;
    let specifier = plan.namespace_specifier(namespace_symbol).ok_or_else(|| {
        module_error(
            TypedModulePhase::Finalize,
            Some(request),
            format!("namespace symbol {namespace_symbol} has no module request"),
        )
    })?;
    let target = resolved_target(resolved, specifier, TypedModuleRequestKind::StaticImport)
        .cloned()
        .unwrap_or_else(|| TypedFinalModuleTarget::External {
            rewritten_specifier: rewrite_with_kind(
                facts,
                specifier,
                TypedModuleRequestKind::StaticImport,
            )
            .unwrap_or(specifier)
            .to_owned(),
        });
    let origin = program.node(request).expect("default read").origin();
    let strip_receiver = is_receiver_position(program, request);
    let factory = SyntheticFactory::new(program);
    let replacement = match target {
        TypedFinalModuleTarget::Internal { is_esm: true, .. } => {
            let object = factory
                .program
                .borrow_mut()
                .clone_detached_subtree(arguments[0])?;
            let member = factory.member(object, "default")?;
            if strip_receiver {
                sequence_zero(&factory, member, origin)?
            } else {
                set_derived_origin(&factory, member, origin)?
            }
        }
        TypedFinalModuleTarget::Internal { is_esm: false, .. }
        | TypedFinalModuleTarget::External { .. } => {
            let test_object = factory
                .program
                .borrow_mut()
                .clone_detached_subtree(arguments[0])?;
            let marker_object = factory
                .program
                .borrow_mut()
                .clone_detached_subtree(arguments[0])?;
            let marker = factory.member(marker_object, "__esModule")?;
            let test = factory.logical(LogicalOperator::And, test_object, marker)?;
            let consequent_object = factory
                .program
                .borrow_mut()
                .clone_detached_subtree(arguments[0])?;
            let consequent = factory.member(consequent_object, "default")?;
            let alternate = factory
                .program
                .borrow_mut()
                .clone_detached_subtree(arguments[0])?;
            let conditional = factory.conditional(test, consequent, alternate)?;
            set_derived_origin(&factory, conditional, origin)?
        }
    };
    factory
        .program
        .borrow_mut()
        .replace_node(request, replacement)?;
    report.lowered_default_reads += 1;
    Ok(())
}

fn binding_for_symbol_current(
    program: &TypedProgram,
    symbol: SymbolId,
) -> Result<CurrentBinding, TypedModuleError> {
    let original_name = program
        .symbol(symbol)
        .ok_or_else(|| {
            module_error(
                TypedModulePhase::Finalize,
                None,
                format!("unknown namespace symbol {symbol}"),
            )
        })?
        .original_name()
        .to_owned();
    let emitted = program
        .preorder_validated()?
        .into_iter()
        .filter_map(|node| {
            let IrNodeData::Name { name } = program.node(node)?.data() else {
                return None;
            };
            let name = program.name(*name)?;
            (name.symbol() == Some(symbol)).then(|| name.emitted().to_owned())
        })
        .collect::<BTreeSet<_>>();
    if emitted.len() != 1 {
        return Err(module_error(
            TypedModulePhase::Finalize,
            None,
            format!(
                "namespace symbol {symbol} must have one current emitted spelling, found {emitted:?}"
            ),
        ));
    }
    Ok(CurrentBinding {
        original_name,
        emitted_name: emitted.into_iter().next().expect("one spelling"),
        symbol,
    })
}

fn current_binding_reference(
    factory: &SyntheticFactory<'_>,
    binding: &CurrentBinding,
) -> Result<NodeId, TypedModuleError> {
    let identifier = factory.identifier(
        &binding.original_name,
        NameRole::Reference,
        Some(binding.symbol),
    )?;
    if binding.original_name != binding.emitted_name {
        let occurrence = {
            let program = factory.program.borrow();
            let IrNodeData::Identifier { name } =
                program.node(identifier).expect("new identifier").data()
            else {
                unreachable!()
            };
            let IrNodeData::Name { name } = program.node(*name).expect("new name").data() else {
                unreachable!()
            };
            *name
        };
        factory
            .program
            .borrow_mut()
            .set_emitted_name(occurrence, &binding.emitted_name)?;
    }
    Ok(identifier)
}

fn rewrite_native_specifiers(
    program: &mut TypedProgram,
    facts: &TypedFinalModuleFacts,
    report: &mut TypedFinalModuleReport,
) -> Result<(), TypedModuleError> {
    let nodes = program.preorder_validated()?;
    for node in nodes {
        let Some(link) = program.node(node).and_then(|node| node.parent()) else {
            continue;
        };
        if !matches!(
            link.role(),
            ChildRole::ImportSource | ChildRole::ModuleSource
        ) {
            continue;
        }
        // Dynamic imports were already finalized with their DynamicImport rewrite profile.
        // Their source edge also uses `ImportSource`, so do not overwrite it with the static
        // import/re-export profile while rewriting native module declarations.
        if matches!(
            program.node(link.parent()).map(|node| node.data()),
            Some(IrNodeData::ImportExpression { .. })
        ) {
            continue;
        }
        let Some(source) = string_value(program, node).map(str::to_owned) else {
            continue;
        };
        let rewrite = rewrite_with_kind(facts, &source, TypedModuleRequestKind::StaticImport)
            .or_else(|| {
                match facts.modules.iter().find(|entry| {
                    entry.specifier == source
                        && entry.request_kind == TypedModuleRequestKind::StaticImport
                }) {
                    Some(TypedResolvedModule {
                        target:
                            TypedFinalModuleTarget::External {
                                rewritten_specifier,
                            },
                        ..
                    }) => Some(rewritten_specifier.as_str()),
                    _ => None,
                }
            });
        let Some(rewrite) = rewrite else { continue };
        if rewrite == source {
            continue;
        }
        match program.node(node).expect("native source").data() {
            IrNodeData::StringLiteral { .. } => program.replace_node_data(
                node,
                IrNodeData::StringLiteral {
                    value: rewrite.to_owned(),
                },
            )?,
            IrNodeData::Name { name } => program.set_emitted_name(*name, rewrite.to_owned())?,
            _ => unreachable!("string_value accepted only literal/name syntax"),
        }
        report.rewritten_native_specifiers += 1;
    }
    Ok(())
}

fn insert_esmodule_marker(
    program: &mut TypedProgram,
    plan: &TypedModulePlan,
    no_esmodule: bool,
) -> Result<(), TypedModuleError> {
    if no_esmodule || !plan.had_esm || plan.mode == TypedModuleMode::PreserveEsm {
        return Ok(());
    }
    let body = program_body(program)?;
    let factory = SyntheticFactory::new(program);
    let exports = binding_reference(&factory, &plan.runtime.exports)?;
    let call = request_call(
        &factory,
        &plan.runtime.mark_esmodule,
        vec![exports],
        MODULE_ORIGIN,
    )?;
    let marker = factory.expression_statement(call)?;
    let marker = set_derived_origin(&factory, marker, MODULE_ORIGIN)?;
    let insert = directive_prefix_len(&factory.program.borrow(), body);
    factory
        .program
        .borrow_mut()
        .splice_list(body, insert..insert, &[marker])?;
    Ok(())
}

fn finalize_runtime_sentinels(
    program: &mut TypedProgram,
    plan: &mut TypedModulePlan,
) -> Result<(), TypedModuleError> {
    let nodes = program.preorder_validated()?;
    for node in nodes.into_iter().rev() {
        let Some(record) = program.node(node) else {
            continue;
        };
        if record.is_tombstone() {
            continue;
        }
        let IrNodeData::CallExpression {
            callee, arguments, ..
        } = record.data()
        else {
            continue;
        };
        let Some(symbol) = identifier_symbol(program, *callee) else {
            continue;
        };
        let origin = record.origin();
        let arguments = program
            .list(*arguments)
            .expect("validated runtime arguments")
            .items()
            .to_vec();
        let replacement = if symbol == plan.runtime.export_live.symbol {
            if arguments.len() != 3 {
                return Err(module_error(
                    TypedModulePhase::Finalize,
                    Some(node),
                    "export-live sentinel must have exports, name and getter arguments",
                ));
            }
            let exports = program.clone_detached_subtree(arguments[0])?;
            let name = program.clone_detached_subtree(arguments[1])?;
            let getter = program.clone_detached_subtree(arguments[2])?;
            let factory = SyntheticFactory::new(program);
            object_define_property(&factory, exports, name, getter, true, origin)?
        } else if symbol == plan.runtime.mark_esmodule.symbol {
            if arguments.len() != 1 {
                return Err(module_error(
                    TypedModulePhase::Finalize,
                    Some(node),
                    "ES-module marker sentinel must have one exports argument",
                ));
            }
            let exports = program.clone_detached_subtree(arguments[0])?;
            let factory = SyntheticFactory::new(program);
            let name = factory.string("__esModule")?;
            let value = factory.boolean(true)?;
            let descriptor = factory.object(vec![factory.data_property("value", value)?])?;
            let object = factory.global("Object")?;
            let define = factory.member(object, "defineProperty")?;
            let call = factory.call(define, vec![exports, name, descriptor])?;
            set_derived_origin(&factory, call, origin)?
        } else if symbol == plan.runtime.export_all.symbol {
            if arguments.len() != 2 {
                return Err(module_error(
                    TypedModulePhase::Finalize,
                    Some(node),
                    "export-all sentinel must have exports and namespace arguments",
                ));
            }
            let exports = program.clone_detached_subtree(arguments[0])?;
            let namespace_for_keys = program.clone_detached_subtree(arguments[1])?;
            let namespace_for_getter = program.clone_detached_subtree(arguments[1])?;
            let mut reserved = collect_reserved_names(program);
            let key_runtime = allocate_runtime_binding(
                program,
                &mut reserved,
                "__wake_export_key",
                DeclKind::Param,
            )?;
            plan.local_bindings.push(key_runtime.clone());
            let key = Binding::from(&key_runtime);
            let factory = SyntheticFactory::new(program);

            let object = factory.global("Object")?;
            let keys = factory.member(object, "keys")?;
            let keys = factory.call(keys, vec![namespace_for_keys])?;
            let for_each = factory.member(keys, "forEach")?;

            let key_for_member = factory.reference(&key)?;
            let value = factory.computed_member(namespace_for_getter, key_for_member)?;
            let getter = ordinary_function_expression(&factory, &[], value)?;
            let key_for_define = factory.reference(&key)?;
            let define =
                object_define_property(&factory, exports, key_for_define, getter, true, origin)?;
            let key_default = factory.reference(&key)?;
            let default = factory.string("default")?;
            let not_default = binary_expression(
                &factory,
                BinaryOperator::StrictNotEq,
                key_default,
                default,
                origin,
            )?;
            let key_marker = factory.reference(&key)?;
            let marker = factory.string("__esModule")?;
            let not_marker = binary_expression(
                &factory,
                BinaryOperator::StrictNotEq,
                key_marker,
                marker,
                origin,
            )?;
            let condition = factory.logical(LogicalOperator::And, not_default, not_marker)?;
            let body = factory.logical(LogicalOperator::And, condition, define)?;
            let callback =
                ordinary_function_expression(&factory, std::slice::from_ref(&key), body)?;
            let call = factory.call(for_each, vec![callback])?;
            set_derived_origin(&factory, call, origin)?
        } else if symbol == plan.runtime.internal_require_async.symbol
            || symbol == plan.runtime.internal_import.symbol
            || symbol == plan.runtime.external_require.symbol
        {
            return Err(module_error(
                TypedModulePhase::Finalize,
                Some(node),
                "obsolete runtime sentinel survived request finalization",
            ));
        } else {
            continue;
        };
        program.replace_node(node, replacement)?;
    }
    Ok(())
}

fn object_define_property(
    factory: &SyntheticFactory<'_>,
    object_value: NodeId,
    name: NodeId,
    getter: NodeId,
    enumerable: bool,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    let enumerable = factory.boolean(enumerable)?;
    let enumerable = factory.data_property("enumerable", enumerable)?;
    let getter = factory.data_property("get", getter)?;
    let descriptor = factory.object(vec![enumerable, getter])?;
    let object = factory.global("Object")?;
    let define = factory.member(object, "defineProperty")?;
    let call = factory.call(define, vec![object_value, name, descriptor])?;
    set_derived_origin(factory, call, origin)
}

/// Module-runtime closures are created after target lowering has already run. Emit syntax that is
/// valid for every supported target instead of manufacturing a fresh arrow that an ES5-targeted
/// build can no longer transform. These closures never observe lexical `this`/`arguments`.
fn ordinary_function_expression(
    factory: &SyntheticFactory<'_>,
    parameters: &[Binding],
    value: NodeId,
) -> Result<NodeId, TypedModuleError> {
    let return_value = factory.return_statement(Some(value))?;
    Ok(factory.function_expression(parameters, vec![return_value])?)
}

fn binary_expression(
    factory: &SyntheticFactory<'_>,
    operator: BinaryOperator,
    left: NodeId,
    right: NodeId,
    origin: IrOrigin,
) -> Result<NodeId, TypedModuleError> {
    Ok(factory
        .program
        .borrow_mut()
        .append_detached_node_with(derived_origin(origin), |_| {
            Ok(IrNodeData::BinaryExpression {
                operator,
                left,
                right,
            })
        })?)
}

struct Fnv64(u64);

impl Fnv64 {
    const fn new() -> Self {
        Self(0xcbf29ce484222325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }

    const fn finish(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::Interner;
    use wake_ecma_ast::SourceType;

    fn lower(source: &str, source_type: SourceType) -> TypedProgram {
        let interner = Interner::new();
        let parsed = wake_ecma_parser::parse(source, &interner, source_type);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let semantic = wake_ecma_semantic::analyze(program);
            TypedProgram::lower(program, &interner, Some(&semantic)).unwrap()
        })
    }

    fn cjs_options() -> TypedModuleOptions {
        TypedModuleOptions {
            mode: TypedModuleMode::BundledCommonJs,
            ..TypedModuleOptions::default()
        }
    }

    fn plan(program: &mut TypedProgram, options: &TypedModuleOptions) -> TypedModulePlan {
        let analysis = TypedAnalysis::rebuild(program).unwrap();
        plan_typed_modules(program, &analysis, options).unwrap()
    }

    fn external_facts(specifiers: &[&str]) -> TypedFinalModuleFacts {
        TypedFinalModuleFacts {
            modules: specifiers
                .iter()
                .map(|specifier| TypedResolvedModule {
                    specifier: (*specifier).to_owned(),
                    request_kind: TypedModuleRequestKind::StaticImport,
                    target: TypedFinalModuleTarget::External {
                        rewritten_specifier: (*specifier).to_owned(),
                    },
                })
                .collect(),
            ..TypedFinalModuleFacts::default()
        }
    }

    fn count_nodes(program: &TypedProgram, predicate: impl Fn(&IrNodeData) -> bool) -> usize {
        program
            .preorder()
            .unwrap()
            .into_iter()
            .filter(|&node| predicate(program.node(node).unwrap().data()))
            .count()
    }

    fn name_text(program: &TypedProgram, node: NodeId) -> Option<&str> {
        name_record(program, node).map(|(_, name)| name.original())
    }

    fn emitted_string(program: &TypedProgram, node: NodeId) -> Option<&str> {
        match program.node(node)?.data() {
            IrNodeData::StringLiteral { value } => Some(value),
            IrNodeData::Name { name } => {
                let name = program.name(*name)?;
                (name.syntax() == NameSyntax::String).then(|| name.emitted())
            }
            _ => None,
        }
    }

    #[test]
    fn imports_are_structural_and_receiver_safe() {
        let mut program = lower(
            "import value,{named as local} from 'dep';import * as ns from 'other';value();local();use(ns.item);",
            SourceType::Module,
        );
        let mut plan = plan(&mut program, &cjs_options());
        assert_eq!(
            count_nodes(&program, |data| matches!(
                data,
                IrNodeData::ImportDeclaration { .. }
            )),
            0
        );
        assert!(matches!(
            validate_no_pending_module_requests(&program, &plan),
            Err(TypedModuleError::PendingRequests { .. })
        ));
        seal_typed_module_plan(&program, &mut plan).unwrap();
        assert_eq!(plan.requests().len(), 2);
        finalize_typed_modules(&mut program, &mut plan, &external_facts(&["dep", "other"]))
            .unwrap();
        validate_no_pending_module_requests(&program, &plan).unwrap();
        assert!(
            count_nodes(&program, |data| matches!(
                data,
                IrNodeData::SequenceExpression { .. }
            )) >= 1
        );
        assert!(
            count_nodes(&program, |data| matches!(
                data,
                IrNodeData::ConditionalExpression { .. }
            )) >= 1
        );
        program.validate().unwrap();
    }

    #[test]
    fn many_import_reads_are_rewritten_from_analysis_identity() {
        let source = format!(
            "import{{value}}from'dep';{}",
            "consume(value);".repeat(1_024)
        );
        let mut program = lower(&source, SourceType::Module);
        let plan = plan(&mut program, &cjs_options());

        assert_eq!(plan.requests().len(), 1);
        assert!(
            count_nodes(&program, |data| matches!(
                data,
                IrNodeData::MemberExpression { .. }
            )) >= 1_024
        );
        program.validate().unwrap();
    }

    #[test]
    fn bundled_module_attributes_are_consumed_but_preserved_commonjs_rejects_them() {
        let source = "import data from 'a' with {type:'json'};use(data);export {value} from 'b' with {type:'json'};export * from 'c' with {type:'json'};";
        let mut bundled = lower(source, SourceType::Module);
        let plan = plan(&mut bundled, &cjs_options());
        assert_eq!(plan.requests().len(), 3);
        assert_eq!(
            count_nodes(&bundled, |data| matches!(
                data,
                IrNodeData::ImportAttributes { .. }
            )),
            0
        );

        let mut preserved = lower(source, SourceType::Module);
        let analysis = TypedAnalysis::rebuild(&preserved).unwrap();
        let error = plan_typed_modules(
            &mut preserved,
            &analysis,
            &TypedModuleOptions {
                mode: TypedModuleMode::PreserveCommonJs,
                ..TypedModuleOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("import attributes"), "{error}");
    }

    #[test]
    fn imported_binding_can_be_reexported_when_export_occurrence_has_no_symbol() {
        let mut program = lower(
            "import {changing} from 'dep';export {changing as observed};",
            SourceType::Module,
        );
        let mut plan = plan(&mut program, &cjs_options());
        seal_typed_module_plan(&program, &mut plan).unwrap();
        finalize_typed_modules(
            &mut program,
            &mut plan,
            &TypedFinalModuleFacts {
                modules: vec![TypedResolvedModule {
                    specifier: "dep".into(),
                    request_kind: TypedModuleRequestKind::StaticImport,
                    target: TypedFinalModuleTarget::Internal {
                        module_id: TypedModuleId(1),
                        is_esm: true,
                        async_dependency: false,
                        dynamic_chunk: None,
                    },
                }],
                ..TypedFinalModuleFacts::default()
            },
        )
        .unwrap();
        validate_no_pending_module_requests(&program, &plan).unwrap();
        let strings = program
            .preorder()
            .unwrap()
            .into_iter()
            .filter_map(|node| emitted_string(&program, node).map(str::to_owned))
            .collect::<BTreeSet<_>>();
        assert!(strings.contains("changing"));
        assert!(strings.contains("observed"));
    }

    #[test]
    fn parser_lowered_jsx_runtime_bindings_are_repaired_structurally() {
        let mut program = lower(
            "const view=<div><span>hi</span><b>bye</b></div>;use(view);",
            SourceType::Jsx,
        );
        assert!(repair_parser_lowered_import_bindings(&mut program).unwrap());
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        for expected in ["_jsx", "_jsxs"] {
            let symbol = program
                .names()
                .iter()
                .find(|name| {
                    name.original() == expected
                        && name.role() == NameRole::ImportBinding
                        && name.symbol().is_some()
                })
                .and_then(crate::typed_ir::IrName::symbol)
                .unwrap_or_else(|| panic!("missing repaired {expected} import binding"));
            let occurrences = program
                .preorder()
                .unwrap()
                .into_iter()
                .filter_map(|node| {
                    let (_, name) = name_record(&program, node)?;
                    (name.original() == expected).then(|| {
                        (
                            node,
                            program.node(node).map(|record| record.origin()),
                            program.node(node).and_then(|record| record.parent()),
                            name.role(),
                            name.symbol(),
                        )
                    })
                })
                .collect::<Vec<_>>();
            assert!(
                analysis.reference_count(symbol) > 0,
                "repaired {expected} binding has no generated call references: {occurrences:?}"
            );
        }
        let mut plan = plan(&mut program, &cjs_options());
        seal_typed_module_plan(&program, &mut plan).unwrap();
        assert!(plan.requests().iter().any(|request| {
            request.kind == TypedModuleRequestKind::StaticImport
                && request.specifier == "react/jsx-runtime"
        }));
    }

    #[test]
    fn parser_lowered_jsx_repair_preserves_same_spelling_local_shadow() {
        let mut program = lower(
            "const view=<div/>;function invoke(_jsx){return _jsx('user')}use(view,invoke);",
            SourceType::Jsx,
        );
        assert!(repair_parser_lowered_import_bindings(&mut program).unwrap());
        let imported = program
            .names()
            .iter()
            .find(|name| name.original() == "_jsx" && name.role() == NameRole::ImportBinding)
            .and_then(crate::typed_ir::IrName::symbol)
            .expect("repaired JSX import symbol");
        let shadow = program
            .names()
            .iter()
            .find(|name| {
                name.original() == "_jsx"
                    && name.role() == NameRole::Binding
                    && name.symbol().is_some_and(|symbol| symbol != imported)
            })
            .and_then(crate::typed_ir::IrName::symbol)
            .expect("function-local _jsx shadow symbol");
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        assert!(analysis.reference_count(imported) > 0);
        assert!(analysis.reference_count(shadow) > 0);

        let mut plan = plan_typed_modules(&mut program, &analysis, &cjs_options()).unwrap();
        seal_typed_module_plan(&program, &mut plan).unwrap();
        assert!(plan.requests().iter().any(|request| {
            request.kind == TypedModuleRequestKind::StaticImport
                && request.specifier == "react/jsx-runtime"
        }));
    }

    #[test]
    fn exports_reexports_all_and_final_marker_are_ordinary_ir() {
        let mut program = lower(
            "export const own=1;export {remote as renamed} from 'dep';export * from 'all';export default function named(){}",
            SourceType::Module,
        );
        let mut plan = plan(&mut program, &cjs_options());
        seal_typed_module_plan(&program, &mut plan).unwrap();
        finalize_typed_modules(&mut program, &mut plan, &external_facts(&["dep", "all"])).unwrap();
        assert_eq!(
            count_nodes(&program, |data| matches!(
                data,
                IrNodeData::ExportNamedDeclaration { .. }
                    | IrNodeData::ExportAllDeclaration { .. }
                    | IrNodeData::ExportDefaultDeclaration { .. }
            )),
            0
        );
        assert!(program.preorder().unwrap().into_iter().any(|node| {
            matches!(
                program.node(node).unwrap().data(),
                IrNodeData::StringLiteral { value } if value == "__esModule"
            )
        }));
        assert!(program.preorder().unwrap().into_iter().any(|node| {
            let IrNodeData::MemberExpression { property, .. } = program.node(node).unwrap().data()
            else {
                return false;
            };
            name_text(&program, *property) == Some("forEach")
        }));
        validate_no_pending_module_requests(&program, &plan).unwrap();
    }

    #[test]
    fn exact_empty_liveness_executes_export_star_dependency_without_forwarding() {
        let mut program = lower("export * from 'dep';", SourceType::Module);
        let options = TypedModuleOptions {
            mode: TypedModuleMode::BundledCommonJs,
            preserve_all_exports: false,
            preserve_export_star: false,
            ..TypedModuleOptions::default()
        };
        let mut plan = plan(&mut program, &options);

        assert!(plan.namespace_requests.is_empty());
        assert_eq!(plan.requests().len(), 1);
        assert_eq!(
            count_nodes(&program, |data| {
                let IrNodeData::Name { name } = data else {
                    return false;
                };
                program
                    .name(*name)
                    .and_then(crate::typed_ir::IrName::symbol)
                    == Some(plan.runtime.export_all.symbol)
            }),
            0
        );

        seal_typed_module_plan(&program, &mut plan).unwrap();
        assert_eq!(
            plan.requests(),
            &[TypedModuleRequestEdge {
                specifier: "dep".into(),
                kind: TypedModuleRequestKind::StaticImport,
                origin: plan.requests()[0].origin,
            }]
        );
        finalize_typed_modules(&mut program, &mut plan, &external_facts(&["dep"])).unwrap();
        validate_no_pending_module_requests(&program, &plan).unwrap();
        assert_eq!(
            count_nodes(&program, |data| {
                let IrNodeData::CallExpression { callee, .. } = data else {
                    return false;
                };
                name_text(&program, *callee) == Some("require")
            }),
            1,
            "the re-export target must still execute for side effects"
        );
        assert_eq!(
            count_nodes(&program, |data| {
                let IrNodeData::MemberExpression { property, .. } = data else {
                    return false;
                };
                name_text(&program, *property) == Some("forEach")
            }),
            0,
            "no runtime export-star forwarding should survive an exact empty proof"
        );
    }

    #[test]
    fn requested_or_namespace_export_star_keeps_forwarding() {
        let exact_named = TypedModuleOptions {
            mode: TypedModuleMode::BundledCommonJs,
            preserve_all_exports: false,
            preserve_export_star: true,
            ..TypedModuleOptions::default()
        };
        let mut named = lower("export * from 'dep';", SourceType::Module);
        let named_plan = plan(&mut named, &exact_named);
        assert_eq!(named_plan.namespace_requests.len(), 1);
        assert!(named.preorder().unwrap().into_iter().any(|node| {
            let IrNodeData::Name { name } = named.node(node).unwrap().data() else {
                return false;
            };
            named.name(*name).and_then(crate::typed_ir::IrName::symbol)
                == Some(named_plan.runtime.export_all.symbol)
        }));

        let mut namespace = lower("export * as ns from 'dep';", SourceType::Module);
        let namespace_plan = plan(
            &mut namespace,
            &TypedModuleOptions {
                preserve_export_star: false,
                observed_export_names: BTreeSet::from(["ns".to_owned()]),
                ..exact_named
            },
        );
        assert_eq!(namespace_plan.namespace_requests.len(), 1);
    }

    #[test]
    fn exact_public_names_filter_reexport_getters_but_keep_source_effects() {
        let mut program = lower(
            "export {default as kept,b as dead} from 'dep';export {c as dropped} from 'other';",
            SourceType::Module,
        );
        let options = TypedModuleOptions {
            mode: TypedModuleMode::BundledCommonJs,
            preserve_all_exports: false,
            preserve_export_star: false,
            observed_export_names: BTreeSet::from(["kept".to_owned()]),
            ..TypedModuleOptions::default()
        };
        let mut plan = plan(&mut program, &options);

        assert_eq!(plan.requests.len(), 2);
        assert_eq!(plan.namespace_requests.len(), 1);
        assert_eq!(
            count_nodes(&program, |data| {
                let IrNodeData::Name { name } = data else {
                    return false;
                };
                program
                    .name(*name)
                    .and_then(crate::typed_ir::IrName::symbol)
                    == Some(plan.runtime.export_live.symbol)
            }),
            1
        );

        seal_typed_module_plan(&program, &mut plan).unwrap();
        finalize_typed_modules(&mut program, &mut plan, &external_facts(&["dep", "other"]))
            .unwrap();
        let strings = program
            .preorder()
            .unwrap()
            .into_iter()
            .filter_map(|node| emitted_string(&program, node).map(str::to_owned))
            .collect::<BTreeSet<_>>();
        assert!(strings.contains("kept"));
        assert!(!strings.contains("dead"));
        assert!(!strings.contains("dropped"));
        assert_eq!(
            count_nodes(&program, |data| {
                let IrNodeData::CallExpression { callee, .. } = data else {
                    return false;
                };
                name_text(&program, *callee) == Some("require")
            }),
            2,
            "both re-export sources must still execute"
        );
    }

    #[test]
    fn observed_aliases_control_getters_independently_from_binding_retention() {
        let mut program = lower(
            "const value=1;consume(value);export {value as kept,value as deadAlias};",
            SourceType::Module,
        );
        let value_symbol = program
            .names()
            .iter()
            .find(|name| name.original() == "value" && name.role() == NameRole::Binding)
            .and_then(crate::typed_ir::IrName::symbol)
            .expect("value binding");
        let mut options = TypedModuleOptions {
            mode: TypedModuleMode::BundledCommonJs,
            preserve_all_exports: false,
            observed_export_names: BTreeSet::from(["kept".to_owned()]),
            ..TypedModuleOptions::default()
        };
        options
            .linker_liveness
            .insert(options.module_id, value_symbol);
        let first_plan = plan(&mut program, &options);
        let strings = program
            .preorder()
            .unwrap()
            .into_iter()
            .filter_map(|node| emitted_string(&program, node).map(str::to_owned))
            .collect::<BTreeSet<_>>();
        assert!(strings.contains("kept"));
        assert!(!strings.contains("deadAlias"));

        let mut different = options;
        different.observed_export_names = BTreeSet::from(["deadAlias".to_owned()]);
        let mut second = lower(
            "const value=1;consume(value);export {value as kept,value as deadAlias};",
            SourceType::Module,
        );
        let second_plan = plan(&mut second, &different);
        assert_ne!(
            first_plan.fingerprint_component(),
            second_plan.fingerprint_component()
        );
    }

    #[test]
    fn anonymous_default_getter_follows_public_name_observation() {
        let source = "export default sideEffect();";
        let mut live = lower(source, SourceType::Module);
        let live_options = TypedModuleOptions {
            mode: TypedModuleMode::BundledCommonJs,
            preserve_all_exports: false,
            observed_export_names: BTreeSet::from(["default".to_owned()]),
            ..TypedModuleOptions::default()
        };
        let live_plan = plan(&mut live, &live_options);
        assert_eq!(
            count_nodes(&live, |data| {
                let IrNodeData::Name { name } = data else {
                    return false;
                };
                live.name(*name).and_then(crate::typed_ir::IrName::symbol)
                    == Some(live_plan.runtime.export_live.symbol)
            }),
            1
        );

        let mut dead = lower(source, SourceType::Module);
        let dead_plan = plan(
            &mut dead,
            &TypedModuleOptions {
                observed_export_names: BTreeSet::new(),
                ..live_options
            },
        );
        assert_eq!(
            count_nodes(&dead, |data| {
                let IrNodeData::Name { name } = data else {
                    return false;
                };
                dead.name(*name).and_then(crate::typed_ir::IrName::symbol)
                    == Some(dead_plan.runtime.export_live.symbol)
            }),
            0
        );
        assert!(dead.preorder().unwrap().into_iter().any(|node| {
            let IrNodeData::CallExpression { callee, .. } = dead.node(node).unwrap().data() else {
                return false;
            };
            name_text(&dead, *callee) == Some("sideEffect")
        }));
    }

    #[test]
    fn no_esmodule_is_a_final_link_fact_not_a_plan_identity() {
        let source = "export const value=1;";
        let mut marked = lower(source, SourceType::Module);
        let mut marked_plan = plan(&mut marked, &cjs_options());
        let plan_fingerprint = marked_plan.fingerprint_component();
        seal_typed_module_plan(&marked, &mut marked_plan).unwrap();
        let mut unmarked = marked.clone();
        let mut unmarked_plan = marked_plan.clone();

        finalize_typed_modules(
            &mut marked,
            &mut marked_plan,
            &TypedFinalModuleFacts::default(),
        )
        .unwrap();
        finalize_typed_modules(
            &mut unmarked,
            &mut unmarked_plan,
            &TypedFinalModuleFacts {
                no_esmodule: true,
                ..TypedFinalModuleFacts::default()
            },
        )
        .unwrap();
        let marker_count = |program: &TypedProgram| {
            count_nodes(
                program,
                |data| matches!(data, IrNodeData::StringLiteral { value } if value == "__esModule"),
            )
        };
        assert_eq!(marker_count(&marked), 1);
        assert_eq!(marker_count(&unmarked), 0);
        assert_ne!(marked.fingerprint(), unmarked.fingerprint());
        assert_ne!(plan_fingerprint, 0);
    }

    #[test]
    fn absent_empty_and_exact_linker_liveness_have_distinct_export_semantics() {
        let source = "export const first=1,second=2;";
        let exported_names = |program: &TypedProgram| {
            program
                .preorder()
                .unwrap()
                .into_iter()
                .filter_map(|node| emitted_string(program, node).map(str::to_owned))
                .filter(|name| name == "first" || name == "second")
                .collect::<BTreeSet<_>>()
        };

        let mut preserve_all = lower(source, SourceType::Module);
        let preserve_plan = plan(&mut preserve_all, &cjs_options());
        assert!(preserve_plan.preserve_all_exports);
        assert_eq!(
            exported_names(&preserve_all),
            BTreeSet::from(["first".to_owned(), "second".to_owned()])
        );

        let mut preserve_none = lower(source, SourceType::Module);
        let none_options = TypedModuleOptions {
            mode: TypedModuleMode::BundledCommonJs,
            preserve_all_exports: false,
            ..TypedModuleOptions::default()
        };
        let none_plan = plan(&mut preserve_none, &none_options);
        assert!(!none_plan.preserve_all_exports);
        assert!(exported_names(&preserve_none).is_empty());
        assert_ne!(
            preserve_plan.fingerprint_component(),
            none_plan.fingerprint_component(),
            "None (preserve all) and Some(empty) liveness must not share cache identity"
        );

        let mut preserve_first = lower(source, SourceType::Module);
        let first_symbol = preserve_first
            .names()
            .iter()
            .find(|name| name.original() == "first" && name.role() == NameRole::Binding)
            .and_then(crate::typed_ir::IrName::symbol)
            .expect("first export symbol");
        let mut exact_options = none_options;
        exact_options
            .observed_export_names
            .insert("first".to_owned());
        exact_options
            .linker_liveness
            .insert(exact_options.module_id, first_symbol);
        let exact_plan = plan(&mut preserve_first, &exact_options);
        assert_eq!(
            exported_names(&preserve_first),
            BTreeSet::from(["first".to_owned()])
        );
        assert!(exact_plan.retained_liveness().contains(&TypedModuleSymbol {
            module: exact_options.module_id,
            symbol: first_symbol,
        }));
    }

    #[test]
    fn exported_function_exposes_only_its_own_declaration_name() {
        let mut program = lower(
            "export function factorial(n){return n<2?1:n*factorial(n-1)}",
            SourceType::Module,
        );
        let _plan = plan(&mut program, &cjs_options());
        let strings = program
            .preorder()
            .unwrap()
            .into_iter()
            .filter_map(|node| emitted_string(&program, node).map(str::to_owned))
            .collect::<BTreeSet<_>>();
        assert!(strings.contains("factorial"));
        assert!(
            !strings.contains("n"),
            "function parameters are not exports"
        );
    }

    #[test]
    fn exact_liveness_filters_aliases_and_named_default_export() {
        let source = "const keep=1,drop=2;export {keep as kept,drop as dropped};export default function App(){}";
        let mut program = lower(source, SourceType::Module);
        let keep = program
            .names()
            .iter()
            .find(|name| name.original() == "keep" && name.role() == NameRole::Binding)
            .and_then(crate::typed_ir::IrName::symbol)
            .expect("keep symbol");
        let mut options = cjs_options();
        options.preserve_all_exports = false;
        options.observed_export_names.insert("kept".to_owned());
        options.linker_liveness.insert(options.module_id, keep);
        let _plan = plan(&mut program, &options);
        let strings = program
            .preorder()
            .unwrap()
            .into_iter()
            .filter_map(|node| emitted_string(&program, node).map(str::to_owned))
            .collect::<BTreeSet<_>>();
        assert!(strings.contains("kept"));
        assert!(!strings.contains("dropped"));
        assert!(!strings.contains("default"));
    }

    #[test]
    fn require_dynamic_import_cycles_and_external_fallback_lower_structurally() {
        let mut program = lower(
            "const a=require('a');const b=import('b');const c=require('external');",
            SourceType::Module,
        );
        let mut plan = plan(&mut program, &TypedModuleOptions::default());
        seal_typed_module_plan(&program, &mut plan).unwrap();
        let facts = TypedFinalModuleFacts {
            modules: vec![
                TypedResolvedModule {
                    specifier: "a".into(),
                    request_kind: TypedModuleRequestKind::Require,
                    target: TypedFinalModuleTarget::Internal {
                        module_id: TypedModuleId(2),
                        is_esm: false,
                        async_dependency: false,
                        dynamic_chunk: None,
                    },
                },
                TypedResolvedModule {
                    specifier: "b".into(),
                    request_kind: TypedModuleRequestKind::DynamicImport,
                    target: TypedFinalModuleTarget::Internal {
                        module_id: TypedModuleId(1),
                        is_esm: false,
                        async_dependency: false,
                        dynamic_chunk: Some(TypedChunkId(9)),
                    },
                },
            ],
            specifier_rewrites: BTreeMap::from([("external".into(), "pkg".into())]),
            request_rewrites: Vec::new(),
            lower_external_dynamic_to_require: false,
            no_esmodule: false,
        };
        let report = finalize_typed_modules(&mut program, &mut plan, &facts).unwrap();
        assert_eq!(report.lowered_static_requests, 0);
        assert_eq!(report.lowered_require_requests, 2);
        assert_eq!(report.lowered_dynamic_requests, 1);
        assert!(program.preorder().unwrap().into_iter().any(|node| {
            let IrNodeData::MemberExpression { property, .. } = program.node(node).unwrap().data()
            else {
                return false;
            };
            name_text(&program, *property) == Some("import")
        }));
        assert!(program.preorder().unwrap().into_iter().any(|node| {
            matches!(
                program.node(node).unwrap().data(),
                IrNodeData::StringLiteral { value } if value == "pkg"
            )
        }));
        validate_no_pending_module_requests(&program, &plan).unwrap();
    }

    #[test]
    fn top_level_await_and_async_static_dependency_report_async_wrapper() {
        let mut program = lower(
            "import value from 'dep';await consume(value);",
            SourceType::Module,
        );
        let mut plan = plan(&mut program, &cjs_options());
        assert!(plan.has_top_level_await());
        seal_typed_module_plan(&program, &mut plan).unwrap();
        let facts = TypedFinalModuleFacts {
            modules: vec![TypedResolvedModule {
                specifier: "dep".into(),
                request_kind: TypedModuleRequestKind::StaticImport,
                target: TypedFinalModuleTarget::Internal {
                    module_id: TypedModuleId(3),
                    is_esm: true,
                    async_dependency: true,
                    dynamic_chunk: None,
                },
            }],
            ..TypedFinalModuleFacts::default()
        };
        let report = finalize_typed_modules(&mut program, &mut plan, &facts).unwrap();
        assert!(report.requires_async_module);
        assert!(
            count_nodes(&program, |data| matches!(
                data,
                IrNodeData::AwaitExpression { .. }
            )) >= 2
        );
    }

    #[test]
    fn seal_rebuilds_only_live_requests_and_liveness_is_monotonic() {
        let mut program = lower("import 'drop';const live=1;use(live);", SourceType::Module);
        let live_symbol = program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| {
                let (_, name) = name_record(&program, node)?;
                (name.original() == "live" && name.role() == NameRole::Binding)
                    .then_some(name.symbol()?)
            })
            .unwrap();
        let mut options = cjs_options();
        options
            .linker_liveness
            .insert(options.module_id, live_symbol);
        options.linker_liveness.insert(TypedModuleId(99), u32::MAX);
        let mut plan = plan(&mut program, &options);
        let body = program_body(&program).unwrap();
        let request_statement = program.list(body).unwrap().items()[0];
        program.splice_list(body, 0..1, &[]).unwrap();
        assert!(program.node(request_statement).unwrap().is_tombstone());
        seal_typed_module_plan(&program, &mut plan).unwrap();
        assert!(plan.requests().is_empty());
        assert_eq!(plan.retained_liveness().len(), 1);
        assert!(plan.retained_liveness().contains(&TypedModuleSymbol {
            module: options.module_id,
            symbol: live_symbol,
        }));
    }

    #[test]
    fn finalize_is_revision_checked_and_failure_is_atomic() {
        let mut program = lower("const value=require('dep');", SourceType::Script);
        let mut plan = plan(&mut program, &TypedModuleOptions::default());
        seal_typed_module_plan(&program, &mut plan).unwrap();
        let origin = program.node(program.root()).unwrap().origin();
        program.set_origin(program.root(), origin).unwrap();
        let program_before = program.fingerprint();
        let plan_before = plan.clone();
        assert!(matches!(
            finalize_typed_modules(&mut program, &mut plan, &TypedFinalModuleFacts::default()),
            Err(TypedModuleError::StalePlan { .. })
        ));
        assert_eq!(program.fingerprint(), program_before);
        assert_eq!(plan, plan_before);
    }

    #[test]
    fn consuming_finalize_reuses_owned_arena_and_matches_atomic_output() {
        let mut program = lower("const value=1;use(value);", SourceType::Script);
        let mut module_plan = plan(&mut program, &TypedModuleOptions::default());
        seal_typed_module_plan(&program, &mut module_plan).unwrap();

        let original_nodes = program.nodes().as_ptr();
        let mut atomic_program = program.clone();
        let mut atomic_plan = module_plan.clone();
        let atomic_report = finalize_typed_modules(
            &mut atomic_program,
            &mut atomic_plan,
            &TypedFinalModuleFacts::default(),
        )
        .unwrap();

        let (finalized, consuming_report) =
            finalize_owned_typed_modules(program, module_plan, &TypedFinalModuleFacts::default())
                .unwrap();

        assert_eq!(finalized.program().nodes().as_ptr(), original_nodes);
        assert_eq!(finalized.program(), &atomic_program);
        assert_eq!(finalized.plan(), &atomic_plan);
        assert_eq!(consuming_report, atomic_report);
    }

    #[test]
    fn finalize_copies_post_mangle_spelling_for_new_namespace_reads() {
        let mut program = lower(
            "import value from 'dep';function consume(){return value}consume();",
            SourceType::Module,
        );
        let mut plan = plan(&mut program, &cjs_options());
        let namespace = plan.namespace_requests[0].symbol;
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        crate::typed_mangle::mangle_typed_program(
            &mut program,
            &analysis,
            &["exports", "__wake_require__"],
        )
        .unwrap();
        seal_typed_module_plan(&program, &mut plan).unwrap();
        finalize_typed_modules(
            &mut program,
            &mut plan,
            &TypedFinalModuleFacts {
                modules: vec![TypedResolvedModule {
                    specifier: "dep".into(),
                    request_kind: TypedModuleRequestKind::StaticImport,
                    target: TypedFinalModuleTarget::Internal {
                        module_id: TypedModuleId(4),
                        is_esm: true,
                        async_dependency: false,
                        dynamic_chunk: None,
                    },
                }],
                ..TypedFinalModuleFacts::default()
            },
        )
        .unwrap();
        let spellings = program
            .preorder()
            .unwrap()
            .into_iter()
            .filter_map(|node| {
                let IrNodeData::Name { name } = program.node(node)?.data() else {
                    return None;
                };
                let record = program.name(*name)?;
                (record.symbol() == Some(namespace)).then(|| record.emitted().to_owned())
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(spellings.len(), 1);
        validate_no_pending_module_requests(&program, &plan).unwrap();
    }

    #[test]
    fn native_esm_specifiers_rewrite_without_changing_module_syntax() {
        let mut program = lower(
            "import value from 'old';export {value};",
            SourceType::Module,
        );
        let mut plan = plan(&mut program, &TypedModuleOptions::default());
        seal_typed_module_plan(&program, &mut plan).unwrap();
        let report = finalize_typed_modules(
            &mut program,
            &mut plan,
            &TypedFinalModuleFacts {
                specifier_rewrites: BTreeMap::from([("old".into(), "new".into())]),
                ..TypedFinalModuleFacts::default()
            },
        )
        .unwrap();
        assert_eq!(report.rewritten_native_specifiers, 1);
        assert_eq!(
            count_nodes(&program, |data| matches!(
                data,
                IrNodeData::ImportDeclaration { .. }
            )),
            1
        );
        assert!(
            program
                .preorder()
                .unwrap()
                .into_iter()
                .any(|node| emitted_string(&program, node) == Some("new"))
        );
    }

    #[test]
    fn import_and_require_of_same_specifier_use_distinct_resolution_profiles() {
        let mut program = lower(
            "import 'same';const loaded=require('same');",
            SourceType::Module,
        );
        let mut plan = plan(&mut program, &TypedModuleOptions::default());
        seal_typed_module_plan(&program, &mut plan).unwrap();
        assert!(plan.requests().iter().any(|edge| {
            edge.specifier == "same" && edge.kind == TypedModuleRequestKind::StaticImport
        }));
        assert!(plan.requests().iter().any(|edge| {
            edge.specifier == "same" && edge.kind == TypedModuleRequestKind::Require
        }));
        let facts = TypedFinalModuleFacts {
            modules: vec![
                TypedResolvedModule {
                    specifier: "same".into(),
                    request_kind: TypedModuleRequestKind::StaticImport,
                    target: TypedFinalModuleTarget::External {
                        rewritten_specifier: "ignored-import".into(),
                    },
                },
                TypedResolvedModule {
                    specifier: "same".into(),
                    request_kind: TypedModuleRequestKind::Require,
                    target: TypedFinalModuleTarget::External {
                        rewritten_specifier: "ignored-require".into(),
                    },
                },
            ],
            request_rewrites: vec![
                TypedModuleSpecifierRewrite {
                    specifier: "same".into(),
                    request_kind: TypedModuleRequestKind::StaticImport,
                    rewritten_specifier: "import-profile".into(),
                },
                TypedModuleSpecifierRewrite {
                    specifier: "same".into(),
                    request_kind: TypedModuleRequestKind::Require,
                    rewritten_specifier: "require-profile".into(),
                },
            ],
            ..TypedFinalModuleFacts::default()
        };
        finalize_typed_modules(&mut program, &mut plan, &facts).unwrap();
        let values = program
            .preorder()
            .unwrap()
            .into_iter()
            .filter_map(|node| emitted_string(&program, node).map(str::to_owned))
            .collect::<Vec<_>>();
        assert!(values.contains(&"import-profile".to_owned()));
        assert!(values.contains(&"require-profile".to_owned()));
    }

    #[test]
    fn raw_require_never_awaits_an_async_internal_target() {
        let mut program = lower("const loaded=require('dep');", SourceType::Script);
        let mut plan = plan(&mut program, &TypedModuleOptions::default());
        seal_typed_module_plan(&program, &mut plan).unwrap();
        let report = finalize_typed_modules(
            &mut program,
            &mut plan,
            &TypedFinalModuleFacts {
                modules: vec![TypedResolvedModule {
                    specifier: "dep".into(),
                    request_kind: TypedModuleRequestKind::Require,
                    target: TypedFinalModuleTarget::Internal {
                        module_id: TypedModuleId(7),
                        is_esm: true,
                        async_dependency: true,
                        dynamic_chunk: None,
                    },
                }],
                ..TypedFinalModuleFacts::default()
            },
        )
        .unwrap();
        assert!(!report.requires_async_module);
        assert_eq!(
            count_nodes(&program, |data| matches!(
                data,
                IrNodeData::AwaitExpression { .. }
            )),
            0
        );
    }

    #[test]
    fn default_and_namespace_interop_distinguish_internal_esm_and_cjs() {
        let source = "import value,* as namespace from 'dep';use(value,namespace);";
        let build = |is_esm| {
            let mut program = lower(source, SourceType::Module);
            let mut plan = plan(&mut program, &cjs_options());
            seal_typed_module_plan(&program, &mut plan).unwrap();
            finalize_typed_modules(
                &mut program,
                &mut plan,
                &TypedFinalModuleFacts {
                    modules: vec![TypedResolvedModule {
                        specifier: "dep".into(),
                        request_kind: TypedModuleRequestKind::StaticImport,
                        target: TypedFinalModuleTarget::Internal {
                            module_id: TypedModuleId(8),
                            is_esm,
                            async_dependency: false,
                            dynamic_chunk: None,
                        },
                    }],
                    no_esmodule: true,
                    ..TypedFinalModuleFacts::default()
                },
            )
            .unwrap();
            program
        };
        let esm = build(true);
        let cjs = build(false);
        let object_assigns = |program: &TypedProgram| {
            program
                .preorder()
                .unwrap()
                .into_iter()
                .filter(|&node| {
                    let IrNodeData::MemberExpression { property, .. } =
                        program.node(node).unwrap().data()
                    else {
                        return false;
                    };
                    name_text(program, *property) == Some("assign")
                })
                .count()
        };
        assert_eq!(object_assigns(&esm), 0);
        assert_eq!(object_assigns(&cjs), 1);
        assert!(
            count_nodes(&cjs, |data| matches!(
                data,
                IrNodeData::ConditionalExpression { .. }
            )) > count_nodes(&esm, |data| matches!(
                data,
                IrNodeData::ConditionalExpression { .. }
            ))
        );
    }

    #[test]
    fn frozen_import_is_materialized_locally_without_blocking_module_planning() {
        let mut program = lower(
            "import {visible} from 'dep';function read(){eval('');return visible}const unrelated=1;use(unrelated);",
            SourceType::Module,
        );
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        let mut plan = plan_typed_modules(&mut program, &analysis, &cjs_options()).unwrap();
        let visible = program
            .names()
            .iter()
            .find(|name| name.original() == "visible" && name.role() == NameRole::Binding)
            .and_then(crate::typed_ir::IrName::symbol)
            .expect("materialized visible binding");
        assert!(
            TypedAnalysis::rebuild(&program)
                .unwrap()
                .symbol(visible)
                .is_some_and(|facts| facts.is_frozen())
        );
        assert!(
            program
                .preorder()
                .unwrap()
                .into_iter()
                .any(|node| { emitted_string(&program, node) == Some("visible") })
        );
        seal_typed_module_plan(&program, &mut plan).unwrap();
        finalize_typed_modules(
            &mut program,
            &mut plan,
            &TypedFinalModuleFacts {
                modules: vec![TypedResolvedModule {
                    specifier: "dep".into(),
                    request_kind: TypedModuleRequestKind::StaticImport,
                    target: TypedFinalModuleTarget::Internal {
                        module_id: TypedModuleId(1),
                        is_esm: true,
                        async_dependency: false,
                        dynamic_chunk: None,
                    },
                }],
                ..TypedFinalModuleFacts::default()
            },
        )
        .unwrap();
        validate_no_pending_module_requests(&program, &plan).unwrap();
    }

    #[test]
    fn external_dynamic_import_stays_native_and_uses_dynamic_rewrite_profile() {
        let mut program = lower("const lazy=import('old');", SourceType::Module);
        let mut plan = plan(&mut program, &TypedModuleOptions::default());
        seal_typed_module_plan(&program, &mut plan).unwrap();
        finalize_typed_modules(
            &mut program,
            &mut plan,
            &TypedFinalModuleFacts {
                request_rewrites: vec![TypedModuleSpecifierRewrite {
                    specifier: "old".into(),
                    request_kind: TypedModuleRequestKind::DynamicImport,
                    rewritten_specifier: "new".into(),
                }],
                ..TypedFinalModuleFacts::default()
            },
        )
        .unwrap();
        assert_eq!(
            count_nodes(&program, |data| matches!(
                data,
                IrNodeData::ImportExpression { .. }
            )),
            1
        );
        assert!(
            program
                .preorder()
                .unwrap()
                .into_iter()
                .any(|node| emitted_string(&program, node) == Some("new"))
        );
        assert!(!program.preorder().unwrap().into_iter().any(|node| {
            name_text(&program, node) == Some("require")
                || name_text(&program, node) == Some("__wake_require__")
        }));
    }

    #[test]
    fn source_request_origin_anchors_the_final_external_require() {
        let mut program = lower("import 'dep';", SourceType::Module);
        let import_origin = program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| {
                let record = program.node(node)?;
                matches!(record.data(), IrNodeData::ImportDeclaration { .. })
                    .then_some(record.origin())
            })
            .unwrap();
        let options = TypedModuleOptions {
            mode: TypedModuleMode::PreserveCommonJs,
            ..TypedModuleOptions::default()
        };
        let mut plan = plan(&mut program, &options);
        seal_typed_module_plan(&program, &mut plan).unwrap();
        finalize_typed_modules(&mut program, &mut plan, &external_facts(&["dep"])).unwrap();
        let final_origin = program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| {
                let record = program.node(node)?;
                let IrNodeData::CallExpression { callee, .. } = record.data() else {
                    return None;
                };
                (name_text(&program, *callee) == Some("require")).then_some(record.origin())
            })
            .unwrap();
        assert_eq!(
            source_anchor_for_test(final_origin),
            source_anchor_for_test(import_origin)
        );
        assert!(matches!(final_origin, IrOrigin::Derived { .. }));
    }

    #[test]
    fn default_reexport_uses_marker_safe_cjs_interop() {
        let mut program = lower("export {default as value} from 'dep';", SourceType::Module);
        let mut plan = plan(&mut program, &cjs_options());
        seal_typed_module_plan(&program, &mut plan).unwrap();
        finalize_typed_modules(
            &mut program,
            &mut plan,
            &TypedFinalModuleFacts {
                modules: vec![TypedResolvedModule {
                    specifier: "dep".into(),
                    request_kind: TypedModuleRequestKind::StaticImport,
                    target: TypedFinalModuleTarget::Internal {
                        module_id: TypedModuleId(10),
                        is_esm: false,
                        async_dependency: false,
                        dynamic_chunk: None,
                    },
                }],
                no_esmodule: true,
                ..TypedFinalModuleFacts::default()
            },
        )
        .unwrap();
        assert!(
            count_nodes(&program, |data| matches!(
                data,
                IrNodeData::ConditionalExpression { .. }
            )) >= 1
        );
        validate_no_pending_module_requests(&program, &plan).unwrap();
    }

    fn source_anchor_for_test(origin: IrOrigin) -> Option<wake_common::Span> {
        match origin {
            IrOrigin::Source(span) => Some(span),
            IrOrigin::Derived { anchor, .. } | IrOrigin::Synthetic { anchor, .. } => anchor,
        }
    }
}
