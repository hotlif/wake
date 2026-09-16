//! Source-only export capture shares the existing resolver's scope and binding identity.

use wake_common::{Atom, FxHashSet, Interner, Span};
use wake_ecma_ast::{Ident, SourceExport, SourceExportKind};

use crate::{ScopeId, SemanticModel, SourceExportUse, SourceExportUseKind, SymbolId};

#[derive(Default)]
pub(crate) struct ExportCollector {
    declarations: FxHashSet<Span>,
    defaults: FxHashSet<Span>,
    named: FxHashSet<(Span, Atom)>,
    uses: Vec<(Ident, ScopeId, SourceExportUseKind)>,
}

impl ExportCollector {
    pub fn new(exports: &[SourceExport], interner: &Interner) -> Self {
        let mut collector = Self::default();
        for export in exports {
            match export.kind {
                SourceExportKind::Declaration => {
                    if let Some(target) = export.target {
                        collector.declarations.insert(target);
                    }
                }
                SourceExportKind::Default => {
                    collector.defaults.insert(export.span);
                }
                SourceExportKind::Named if export.source.is_none() && !export.type_only => {
                    for specifier in &export.specifiers {
                        if !specifier.type_only && specifier.local.identifier {
                            collector.named.insert((
                                specifier.local.span,
                                interner.intern(&specifier.local.value),
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        collector
    }

    pub fn is_declaration(&self, span: Span) -> bool {
        self.declarations.contains(&span)
    }
    pub fn is_default(&self, span: Span) -> bool {
        self.defaults.contains(&span)
    }
    pub fn is_named(&self, span: Span, name: Atom) -> bool {
        self.named.contains(&(span, name))
    }

    pub fn record(&mut self, identifier: Ident, scope: ScopeId, kind: SourceExportUseKind) {
        self.uses.push((identifier, scope, kind));
    }

    pub fn finish(
        self,
        model: &SemanticModel,
        source_symbols: &FxHashSet<SymbolId>,
    ) -> Vec<SourceExportUse> {
        // A declaration occurrence identifies its symbol even when another declaration changes
        // the scope's final binding. Export specifiers refer to the final instantiated binding.
        let occurrences = model
            .binding_occurrences
            .iter()
            .map(|binding| ((binding.span, binding.name, binding.scope), binding.symbol))
            .collect::<wake_common::FxHashMap<_, _>>();
        let mut uses = self
            .uses
            .into_iter()
            .filter_map(|(id, scope, kind)| {
                let resolved = match kind {
                    SourceExportUseKind::Named => model.resolve_in(scope, id.name),
                    SourceExportUseKind::Declaration => {
                        occurrences.get(&(id.span, id.name, scope)).copied()
                    }
                };
                if kind == SourceExportUseKind::Declaration
                    && !resolved.is_some_and(|id| source_symbols.contains(&id))
                {
                    return None;
                }
                Some(SourceExportUse {
                    name: id.name,
                    span: id.span,
                    scope,
                    resolved,
                    kind,
                })
            })
            .collect::<Vec<_>>();
        uses.sort_by_key(|usage| (usage.span.lo, usage.span.hi));
        uses.dedup();
        uses
    }
}
