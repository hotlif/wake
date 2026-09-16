//! Unevaluated original type queries. Scope regions are attached by the native resolver, never
//! inferred from a nearby reference or identifier spelling. Missing source environments stay unknown.
use crate::{
    DeclKind, ScopeId, SemanticModel, SourceSemanticInput, SourceTypeQuery,
    SourceTypeQueryResolution as Resolution, SymbolId,
};
use std::{cmp::Reverse, collections::BTreeSet};
use wake_common::{Atom, FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::{SourceFunction, SourceIdentifierRole, SourceNodeKind};

#[derive(Clone, Copy, Debug)]
struct Region {
    span: Span,
    scope: ScopeId,
    depth: usize,
}

pub(crate) struct QueryCollector {
    queries: Vec<(Span, Atom)>,
    functions: FxHashMap<Span, SourceFunction>,
    seen_functions: FxHashSet<Span>,
    regions: Vec<Region>,
    unavailable: Vec<Span>,
    available_namespaces: FxHashSet<Span>,
    ambient_modules: Vec<Region>,
    signature_regions: Vec<Region>,
}

impl QueryCollector {
    pub fn new(input: SourceSemanticInput<'_>, interner: &Interner) -> Self {
        let mut queries = Vec::new();
        for id in input.identifiers {
            if id.role == SourceIdentifierRole::TypeQuery {
                queries.push((id.span, interner.intern(&id.name)));
            }
        }
        queries.sort_by_key(|(span, _)| (span.lo, span.hi));
        queries.dedup();
        Self {
            queries,
            functions: input
                .functions
                .iter()
                .map(|f| (f.span, f.clone()))
                .collect(),
            seen_functions: FxHashSet::default(),
            regions: vec![Region {
                span: Span::new(0, u32::MAX),
                scope: 0,
                depth: 1,
            }],
            unavailable: input
                .syntax
                .iter()
                .filter(|node| {
                    matches!(
                        node.kind,
                        SourceNodeKind::TsEnum
                            | SourceNodeKind::TsNamespace
                            | SourceNodeKind::TsAmbientModule
                            | SourceNodeKind::TsSignature
                    )
                })
                .map(|node| node.span)
                .collect(),
            available_namespaces: FxHashSet::default(),
            ambient_modules: Vec::new(),
            signature_regions: Vec::new(),
        }
    }

    pub fn region(&mut self, span: Span, scope: ScopeId, depth: usize) {
        if span.lo < span.hi {
            self.regions.push(Region { span, scope, depth });
        }
    }

    pub fn scope_for(&self, span: Span) -> Option<ScopeId> {
        self.regions
            .iter()
            .filter(|region| region.span == span)
            .max_by_key(|region| region.depth)
            .map(|region| region.scope)
    }

    pub fn enclosing_scope(&self, span: Span) -> Option<(ScopeId, usize)> {
        self.regions
            .iter()
            .filter(|region| region.span.lo <= span.lo && span.hi <= region.span.hi)
            .max_by_key(|region| (region.depth, Reverse(region.span.hi - region.span.lo)))
            .map(|region| (region.scope, region.depth))
    }

    pub fn namespace(&mut self, node: Span, body: Span, scope: ScopeId, depth: usize) {
        if node.lo < node.hi {
            self.available_namespaces.insert(node);
        }
        self.region(body, scope, depth);
    }

    pub fn source_scope(&mut self, span: Span, scope: ScopeId, depth: usize) {
        if span.lo < span.hi {
            self.available_namespaces.insert(span);
        }
        self.region(span, scope, depth);
    }

    pub fn ambient_module(&mut self, body: Span, scope: ScopeId, depth: usize) {
        if body.lo < body.hi {
            let region = Region {
                span: body,
                scope,
                depth,
            };
            self.ambient_modules.push(region);
            self.regions.push(region);
        }
    }

    pub fn signature(&mut self, span: Span, scope: ScopeId, depth: usize) {
        if span.lo < span.hi {
            let region = Region { span, scope, depth };
            self.signature_regions.push(region);
            self.regions.push(region);
        }
    }

    pub fn function(
        &mut self,
        span: Span,
        scope: ScopeId,
        depth: usize,
        _separate: bool,
        is_arrow: bool,
    ) {
        if let Some(function) = self.functions.get(&span).cloned()
            && function.is_arrow == is_arrow
        {
            self.seen_functions.insert(span);
            self.region(function.scope, scope, depth);
        }
    }

    pub fn finish(
        mut self,
        model: &SemanticModel,
        source_symbols: &FxHashSet<SymbolId>,
        incomplete_names: &FxHashSet<Atom>,
        incomplete_local_regions: &[(Span, Atom)],
        incomplete_external_names: &FxHashSet<Atom>,
    ) -> Vec<SourceTypeQuery> {
        // An erased declaration may shadow a retained declaration. Until original TS declaration
        // scopes are complete, every query of such a name stays unavailable, including outer uses.
        for function in self.functions.values() {
            let erased_signature = function.body.is_none()
                && self
                    .signature_regions
                    .iter()
                    .any(|region| region.span == function.scope);
            if (!erased_signature && function.body.is_none())
                || (function.body.is_some() && !self.seen_functions.contains(&function.span))
            {
                self.unavailable.push(function.scope);
            }
        }
        self.unavailable
            .retain(|span| !self.available_namespaces.contains(span));
        self.unavailable.sort_by_key(|span| (span.lo, span.hi));
        let mut unknown: Vec<Span> = Vec::new();
        for span in self.unavailable {
            if let Some(last) = unknown.last_mut()
                && last.hi >= span.lo
            {
                last.hi = last.hi.max(span.hi);
            } else if span.lo < span.hi {
                unknown.push(span);
            }
        }
        // One sweep over queries and region boundaries avoids scanning every scope per query.
        let mut events = self
            .regions
            .iter()
            .enumerate()
            .flat_map(|(i, r)| [(r.span.lo, true, i), (r.span.hi, false, i)])
            .collect::<Vec<_>>();
        events.sort_unstable();
        let mut active = BTreeSet::new();
        let mut event_index = 0;
        let mut unknown_index = 0;
        self.queries
            .into_iter()
            .map(|(span, name)| {
                while event_index < events.len() && events[event_index].0 <= span.lo {
                    let (_, start, index) = events[event_index];
                    let region = &self.regions[index];
                    let key = (
                        region.depth,
                        Reverse(region.span.hi - region.span.lo),
                        index,
                    );
                    if start {
                        active.insert(key);
                    } else {
                        active.remove(&key);
                    }
                    event_index += 1;
                }
                while unknown_index < unknown.len() && unknown[unknown_index].hi <= span.lo {
                    unknown_index += 1;
                }
                let scope = self.regions[active.last().expect("module covers source").2].scope;
                let resolved = model.resolve_in(scope, name);
                let blocked_by_local_region =
                    incomplete_local_regions
                        .iter()
                        .any(|(region, region_name)| {
                            *region_name == name && region.lo <= span.lo && span.hi <= region.hi
                        });
                let ambient_local = self.ambient_modules.iter().any(|region| {
                    region.scope == scope
                        && region.span.lo <= span.lo
                        && span.hi <= region.span.hi
                        && resolved.is_some_and(|id| {
                            source_symbols.contains(&id)
                                && model.symbols[id as usize].scope == scope
                        })
                });
                let signature_source = self.signature_regions.iter().any(|region| {
                    region.scope == scope
                        && region.span.lo <= span.lo
                        && span.hi <= region.span.hi
                        && resolved.is_some_and(|id| source_symbols.contains(&id))
                });
                let complete_source = ambient_local
                    || signature_source
                    || (incomplete_external_names.contains(&name)
                        && !blocked_by_local_region
                        && resolved.is_some_and(|id| source_symbols.contains(&id)));
                let unavailable = unknown.get(unknown_index).is_some_and(|range| {
                    range.lo < span.hi && span.lo < range.hi && !ambient_local && !signature_source
                }) || (incomplete_names.contains(&name) && !complete_source);
                if unavailable {
                    return SourceTypeQuery {
                        name,
                        span,
                        scope: None,
                        resolution: Resolution::Unavailable,
                    };
                }
                let resolution = match resolved {
                    Some(id)
                        if source_symbols.contains(&id)
                            || model.symbols[id as usize].decl_kind == DeclKind::Arguments =>
                    {
                        Resolution::Resolved(id)
                    }
                    Some(_) => Resolution::Unavailable,
                    None => Resolution::Unresolved,
                };
                SourceTypeQuery {
                    name,
                    span,
                    scope: Some(scope),
                    resolution,
                }
            })
            .collect()
    }
}
