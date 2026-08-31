//! # wake_ecma_minify — owned Closure-style JavaScript optimizer
//!
//! [`optimize`] lowers a parser-owned frozen AST once into [`codegen_bridge::TypedProgram`], applies
//! trusted structural edits, runs an explicitly ordered fixed-point pipeline, finalizes safe
//! names, and returns a lifetime-independent [`OptimizedProgram`]. No parser-AST or span-side-table
//! optimization path remains.

mod const_eval;
mod optimizer;
mod owned_optimizer;

mod typed_analysis;
mod typed_decorators;
mod typed_edits;
mod typed_inline;
mod typed_ir;
mod typed_lowering;
mod typed_mangle;
mod typed_modules;
mod typed_passes;
mod typed_pipeline;

/// Narrow implementation bridge used only by `wake_ecma_codegen` to emit an optimizer-owned
/// program. This is not a second optimization entry point.
#[doc(hidden)]
pub mod codegen_bridge {
    pub use crate::const_eval::write_number_minified;
    pub use crate::typed_ir::{
        ArrowBodyKind, ClassContext, ExportDefaultValueKind, ForInitializerKind, ForLeftKind,
        FunctionContext, ImportSpecifierKind, IrList, IrModuleName, IrName, IrNode, IrNodeData,
        IrOrigin, IrPropertyKey, ListId, ModuleNameKind, NameId, NameRole, NameSyntax, NodeId,
        PropertyKeyKind, SyntheticOriginKind, TypedIrError, TypedProgram,
    };
    pub use crate::typed_modules::{
        FinalizedTypedProgram, TypedChunkId, TypedDiscardedStaticRequest, TypedFinalModuleFacts,
        TypedFinalModuleReport, TypedFinalModuleTarget, TypedModuleError, TypedModuleId,
        TypedModuleMode, TypedModulePlan, TypedModuleRequestEdge, TypedModuleRequestKind,
        TypedModuleSpecifierRewrite, TypedResolvedModule, finalize_owned_typed_modules,
        finalize_typed_modules,
    };
}

pub use const_eval::ConstVal;
pub(crate) use const_eval::write_number_minified;
pub use optimizer::*;

#[cfg(test)]
mod tests;
