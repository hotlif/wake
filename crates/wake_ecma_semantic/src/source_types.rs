//! Original type bindings. These identities never alias compilation SymbolId and do not imply
//! type validity, module-member resolution or evaluated reads. All input is parser-owned syntax.
use std::cmp::Reverse;
use wake_common::{Atom, FxHashMap, FxHashSet, Interner, JsString, Span};
use wake_ecma_ast::{
    SourceExport, SourceExportKind, SourceIdentifier, SourceIdentifierRole, SourceImport,
    SourceNamespace, SourceNode, SourceNodeKind, SourceTypeDeclaration, SourceTypeScope,
    SourceTypeScopeKind,
};

#[derive(Clone, Copy)]
pub struct SourceTypeInput<'a> {
    pub identifiers: &'a [SourceIdentifier],
    pub syntax: &'a [SourceNode],
    pub scopes: &'a [SourceTypeScope],
    pub declarations: &'a [SourceTypeDeclaration],
    pub imports: &'a [SourceImport],
    pub exports: &'a [SourceExport],
    pub namespaces: &'a [SourceNamespace],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TypeSymbolId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TypeScopeId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeResolution {
    Resolved(TypeSymbolId),
    Intrinsic,
    /// No local target; external type libraries and members have not been checked.
    Unresolved,
    /// An unrepresented external member environment prevents a safe outer-scope lookup.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeDeclarationKind {
    Alias,
    Interface,
    Class,
    Enum,
    Import,
    TypeImport,
    Namespace,
    Parameter,
    Mapped,
    Infer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeDeclaration {
    pub span: Span,
    pub kind: TypeDeclarationKind,
    pub scope: TypeScopeId,
}

#[derive(Debug)]
pub struct TypeSymbol {
    pub id: TypeSymbolId,
    pub name: Atom,
    pub declarations: Vec<TypeDeclaration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeScopeKind {
    Global,
    Module,
    GlobalAugmentation,
    Block,
    FunctionBody,
    Class,
    Namespace,
    AmbientModule,
    Local(SourceTypeScopeKind),
}

#[derive(Debug)]
pub struct TypeScope {
    pub span: Span,
    pub parent: Option<TypeScopeId>,
    pub kind: TypeScopeKind,
    pub namespace: Option<TypeSymbolId>,
    pub ambient: bool,
    bindings: FxHashMap<Atom, TypeSymbolId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeReference {
    pub name: Atom,
    pub span: Span,
    pub scope: TypeScopeId,
    pub resolution: TypeResolution,
}

#[derive(Debug)]
pub struct SourceTypeModel {
    pub scopes: Vec<TypeScope>,
    pub symbols: Vec<TypeSymbol>,
    pub references: Vec<TypeReference>,
    namespace_members: FxHashMap<(TypeSymbolId, Atom), TypeSymbolId>,
}

impl SourceTypeModel {
    pub fn resolve_in(&self, scope: TypeScopeId, name: Atom) -> Option<TypeSymbolId> {
        match self.resolution_in(scope, name) {
            TypeResolution::Resolved(symbol) => Some(symbol),
            _ => None,
        }
    }

    pub fn resolution_in(&self, mut scope: TypeScopeId, name: Atom) -> TypeResolution {
        loop {
            let current = &self.scopes[scope.0];
            if let Some(id) = current.bindings.get(&name) {
                return TypeResolution::Resolved(*id);
            }
            if let Some(namespace) = current.namespace
                && let Some(id) = self.namespace_members.get(&(namespace, name))
            {
                return TypeResolution::Resolved(*id);
            }
            if current.kind == TypeScopeKind::AmbientModule {
                return TypeResolution::Unavailable;
            }
            let Some(parent) = current.parent else {
                return TypeResolution::Unresolved;
            };
            scope = parent;
        }
    }

    pub fn scope_at(&self, span: Span) -> TypeScopeId {
        let mut index = self
            .scopes
            .partition_point(|scope| scope.span.lo <= span.lo)
            .saturating_sub(1);
        while self.scopes[index].span.hi < span.hi {
            index = self.scopes[index].parent.expect("module covers source").0;
        }
        TypeScopeId(index)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Origin {
    Global,
    Root,
    Syntax(usize),
    Local(usize),
    Namespace(usize, usize),
}

struct Region {
    span: Span,
    kind: TypeScopeKind,
    origin: Origin,
}

struct Builder<'a> {
    interner: &'a Interner,
    model: SourceTypeModel,
    origins: FxHashMap<Origin, TypeScopeId>,
    occurrences: FxHashMap<Span, TypeSymbolId>,
    declared: FxHashSet<Span>,
    infer_canonical: FxHashMap<Span, Span>,
    exported_targets: FxHashMap<u32, Vec<Span>>,
    /// Same-file string ambient module bodies share their type member identity; project-level
    /// cross-file merging remains owned by the module/type service.
    ambient_modules: FxHashMap<JsString, Vec<TypeScopeId>>,
}

impl<'a> Builder<'a> {
    fn new(interner: &'a Interner, input: SourceTypeInput<'_>) -> Self {
        let mut regions = vec![
            Region {
                span: Span::new(0, u32::MAX),
                kind: TypeScopeKind::Global,
                origin: Origin::Global,
            },
            Region {
                span: Span::new(0, u32::MAX),
                kind: TypeScopeKind::Module,
                origin: Origin::Root,
            },
        ];
        for (index, node) in input.syntax.iter().enumerate() {
            let kind = match node.kind {
                SourceNodeKind::JsBlock | SourceNodeKind::JsSwitchBody => TypeScopeKind::Block,
                SourceNodeKind::JsFunctionBody => TypeScopeKind::FunctionBody,
                SourceNodeKind::JsClass => TypeScopeKind::Class,
                SourceNodeKind::TsGlobalAugmentation => TypeScopeKind::GlobalAugmentation,
                _ => continue,
            };
            regions.push(Region {
                span: node.span,
                kind,
                origin: Origin::Syntax(index),
            });
        }
        for (index, scope) in input.scopes.iter().enumerate() {
            regions.push(Region {
                span: scope.span,
                kind: TypeScopeKind::Local(scope.kind),
                origin: Origin::Local(index),
            });
        }
        for (index, namespace) in input.namespaces.iter().enumerate() {
            if let Some(span) = namespace.body {
                for depth in 0..namespace.names.len().max(1) {
                    regions.push(Region {
                        span,
                        kind: if namespace.ambient.is_some() {
                            TypeScopeKind::AmbientModule
                        } else {
                            TypeScopeKind::Namespace
                        },
                        origin: Origin::Namespace(index, depth),
                    });
                }
            }
        }
        // Stable ties keep dotted namespace segments outer-to-inner and local scope grammar order.
        regions.sort_by_key(|region| (region.span.lo, Reverse(region.span.hi)));
        let mut model = SourceTypeModel {
            scopes: Vec::new(),
            symbols: Vec::new(),
            references: Vec::new(),
            namespace_members: FxHashMap::default(),
        };
        let mut stack: Vec<usize> = Vec::new();
        let mut origins = FxHashMap::default();
        for region in regions {
            while stack
                .last()
                .is_some_and(|index| model.scopes[*index].span.hi < region.span.hi)
            {
                stack.pop();
            }
            let id = TypeScopeId(model.scopes.len());
            model.scopes.push(TypeScope {
                span: region.span,
                kind: region.kind,
                parent: stack.last().copied().map(TypeScopeId),
                namespace: None,
                ambient: false,
                bindings: FxHashMap::default(),
            });
            origins.insert(region.origin, id);
            stack.push(id.0);
        }
        let mut infer_canonical = FxHashMap::default();
        for scope in input.scopes {
            if scope.kind == SourceTypeScopeKind::ConditionalTrue {
                let mut names = FxHashMap::default();
                for binding in &scope.bindings {
                    let canonical = *names.entry(&binding.name).or_insert(binding.span);
                    infer_canonical.insert(binding.span, canonical);
                }
            }
        }
        let mut exported_targets: FxHashMap<u32, Vec<Span>> = FxHashMap::default();
        for export in input.exports {
            if matches!(
                export.kind,
                SourceExportKind::Declaration | SourceExportKind::Default
            ) && let Some(target) = export.target
            {
                exported_targets.entry(target.hi).or_default().push(target);
            }
        }
        let mut ambient_modules: FxHashMap<JsString, Vec<TypeScopeId>> = FxHashMap::default();
        for (index, namespace) in input.namespaces.iter().enumerate() {
            let Some(module) = namespace.ambient.as_ref() else {
                continue;
            };
            let Some(scope) = origins.get(&Origin::Namespace(index, 0)).copied() else {
                continue;
            };
            ambient_modules
                .entry(module.value.clone())
                .or_default()
                .push(scope);
        }
        Self {
            interner,
            model,
            origins,
            occurrences: FxHashMap::default(),
            declared: FxHashSet::default(),
            infer_canonical,
            exported_targets,
            ambient_modules,
        }
    }

    fn merge_ambient_modules(&mut self) {
        for scopes in self.ambient_modules.values() {
            if scopes.len() < 2 {
                continue;
            }
            let mut canonical: FxHashMap<Atom, TypeSymbolId> = FxHashMap::default();
            for scope in scopes {
                let bindings: Vec<_> = self.model.scopes[scope.0]
                    .bindings
                    .iter()
                    .map(|(name, symbol)| (*name, *symbol))
                    .collect();
                for (name, symbol) in bindings {
                    if let Some(&target) = canonical.get(&name) {
                        if target != symbol {
                            let declarations =
                                std::mem::take(&mut self.model.symbols[symbol.0].declarations);
                            let target_declarations =
                                &mut self.model.symbols[target.0].declarations;
                            for declaration in declarations {
                                if !target_declarations.contains(&declaration) {
                                    target_declarations.push(declaration);
                                }
                            }
                            for occurrence in self.occurrences.values_mut() {
                                if *occurrence == symbol {
                                    *occurrence = target;
                                }
                            }
                        }
                    } else {
                        canonical.insert(name, symbol);
                    }
                }
            }
            for scope in scopes {
                self.model.scopes[scope.0]
                    .bindings
                    .extend(canonical.iter().map(|(name, symbol)| (*name, *symbol)));
            }
        }
    }

    fn exported(&self, span: Span) -> bool {
        self.exported_targets
            .get(&span.hi)
            .is_some_and(|targets| targets.iter().any(|target| target.lo <= span.lo))
    }

    fn declare(
        &mut self,
        scope: TypeScopeId,
        binding: &SourceIdentifier,
        kind: TypeDeclarationKind,
        exported: bool,
    ) -> TypeSymbolId {
        let name = self.interner.intern(&binding.name);
        let namespace = self.model.scopes[scope.0].namespace;
        let global = self.model.scopes[scope.0].kind == TypeScopeKind::GlobalAugmentation;
        let exported = exported || self.model.scopes[scope.0].ambient;
        let canonical = self
            .infer_canonical
            .get(&binding.span)
            .copied()
            .unwrap_or(binding.span);
        let existing = self
            .occurrences
            .get(&canonical)
            .copied()
            .or_else(|| self.model.scopes[scope.0].bindings.get(&name).copied())
            .or_else(|| {
                global
                    .then(|| self.model.scopes[0].bindings.get(&name).copied())
                    .flatten()
            })
            .or_else(|| {
                exported
                    .then(|| {
                        namespace.and_then(|namespace| {
                            self.model
                                .namespace_members
                                .get(&(namespace, name))
                                .copied()
                        })
                    })
                    .flatten()
            });
        let symbol = existing.unwrap_or_else(|| {
            let id = TypeSymbolId(self.model.symbols.len());
            self.model.symbols.push(TypeSymbol {
                id,
                name,
                declarations: Vec::new(),
            });
            id
        });
        self.occurrences.insert(binding.span, symbol);
        if self.declared.insert(binding.span) {
            self.model.symbols[symbol.0]
                .declarations
                .push(TypeDeclaration {
                    span: binding.span,
                    kind,
                    scope,
                });
        }
        self.occurrences.insert(canonical, symbol);
        self.model.scopes[scope.0].bindings.insert(name, symbol);
        if global {
            self.model.scopes[0].bindings.insert(name, symbol);
        }
        if exported && let Some(namespace) = namespace {
            self.model
                .namespace_members
                .insert((namespace, name), symbol);
        }
        symbol
    }

    fn namespaces(&mut self, input: SourceTypeInput<'_>) {
        for (index, namespace) in input.namespaces.iter().enumerate() {
            let node = &input.syntax[namespace.node];
            let mut parent = self
                .model
                .scope_at(Span::new(node.span.lo, node.span.lo + 1));
            for (depth, binding) in namespace.names.iter().enumerate() {
                let symbol = self.declare(
                    parent,
                    binding,
                    TypeDeclarationKind::Namespace,
                    depth > 0 || self.exported(node.span),
                );
                if let Some(scope) = self.origins.get(&Origin::Namespace(index, depth)).copied() {
                    self.model.scopes[scope.0].namespace = Some(symbol);
                    self.model.scopes[scope.0].ambient = namespace.is_ambient;
                    parent = scope;
                }
            }
        }
    }

    fn declarations(&mut self, input: SourceTypeInput<'_>) {
        for declaration in input.declarations {
            let node = &input.syntax[declaration.node];
            let kind = match node.kind {
                SourceNodeKind::TsTypeAlias => TypeDeclarationKind::Alias,
                SourceNodeKind::TsInterface => TypeDeclarationKind::Interface,
                SourceNodeKind::TsEnum => TypeDeclarationKind::Enum,
                SourceNodeKind::JsClass => TypeDeclarationKind::Class,
                _ => continue,
            };
            let mut scope = self.model.scope_at(declaration.name.span);
            if kind == TypeDeclarationKind::Class && !declaration.in_own_scope {
                scope = self.model.scopes[self.origins[&Origin::Syntax(declaration.node)].0]
                    .parent
                    .expect("class has enclosing scope");
            }
            self.declare(scope, &declaration.name, kind, self.exported(node.span));
        }
        for import in input.imports {
            for binding in &import.bindings {
                let scope = self.model.scope_at(binding.local.span);
                self.declare(
                    scope,
                    &binding.local,
                    if binding.type_only {
                        TypeDeclarationKind::TypeImport
                    } else {
                        TypeDeclarationKind::Import
                    },
                    self.exported(import.span),
                );
            }
        }
        for (index, source_scope) in input.scopes.iter().enumerate() {
            let kind = match source_scope.kind {
                SourceTypeScopeKind::TypeParameters => TypeDeclarationKind::Parameter,
                SourceTypeScopeKind::MappedType => TypeDeclarationKind::Mapped,
                SourceTypeScopeKind::InferConstraint | SourceTypeScopeKind::ConditionalTrue => {
                    TypeDeclarationKind::Infer
                }
            };
            let scope = self.origins[&Origin::Local(index)];
            for binding in &source_scope.bindings {
                self.declare(scope, binding, kind, false);
            }
        }
        for export in input.exports {
            if export.kind != SourceExportKind::Named || export.source.is_some() {
                continue;
            }
            for specifier in &export.specifiers {
                if !specifier.local.identifier {
                    continue;
                }
                let name = self.interner.intern(&specifier.local.value);
                let scope = self.model.scope_at(specifier.local.span);
                if let Some(namespace) = self.model.scopes[scope.0].namespace
                    && let Some(symbol) = self.model.resolve_in(scope, name)
                {
                    self.model.namespace_members.insert(
                        (namespace, self.interner.intern(&specifier.exported.value)),
                        symbol,
                    );
                }
            }
        }
    }
}

pub fn analyze_source_types(interner: &Interner, input: SourceTypeInput<'_>) -> SourceTypeModel {
    let mut builder = Builder::new(interner, input);
    builder.namespaces(input);
    builder.declarations(input);
    builder.merge_ambient_modules();
    let mut identifiers: Vec<_> = input
        .identifiers
        .iter()
        .filter(|id| id.role == SourceIdentifierRole::TypeReference)
        .collect();
    identifiers.sort_by_key(|id| (id.span.lo, id.span.hi));
    identifiers.dedup_by_key(|id| id.span);
    builder.model.references = identifiers
        .into_iter()
        .map(|id| {
            let name = interner.intern(&id.name);
            let scope = builder.model.scope_at(id.span);
            let resolution = if matches!(
                id.name.as_str(),
                "any"
                    | "unknown"
                    | "never"
                    | "string"
                    | "number"
                    | "bigint"
                    | "boolean"
                    | "symbol"
                    | "object"
                    | "undefined"
            ) {
                TypeResolution::Intrinsic
            } else {
                builder.model.resolution_in(scope, name)
            };
            TypeReference {
                name,
                span: id.span,
                scope,
                resolution,
            }
        })
        .collect();
    builder.model
}
