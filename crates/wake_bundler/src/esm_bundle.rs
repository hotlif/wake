//! Conservative native-ESM facade. Never infer export semantics from generated JavaScript.

use wake_common::{Atom, Interner};
use wake_ecma_ast::visit::{Visit, walk_expression};
use wake_ecma_ast::{ExportDefaultKind, Expression, Pattern, Program, Statement, VarKind};

/// Validate source semantics for every graph module; collect only entry export names.
pub(crate) fn exports(
    program: &Program<'_>,
    interner: &Interner,
    entry: bool,
) -> Result<Vec<String>, &'static str> {
    let semantic = wake_ecma_semantic::analyze(program, interner);
    if semantic.references.iter().any(|reference| {
        interner.resolve(reference.name) == "eval"
            || (reference.resolved.is_none()
                && matches!(
                    interner.resolve(reference.name).as_ref(),
                    "require" | "module" | "exports" | "__dirname" | "__filename" | "arguments"
                ))
    }) {
        return Err("ESM bundle does not support CommonJS globals or eval");
    }
    struct Meta<'i> {
        found: bool,
        interner: &'i Interner,
    }
    impl<'a> Visit<'a> for Meta<'_> {
        fn visit_expression(&mut self, node: &Expression<'a>) {
            if let Expression::MetaProperty(meta) = node
                && self.interner.resolve(meta.meta) == "import"
            {
                self.found = true;
            }
            walk_expression(self, node);
        }
    }
    let mut meta = Meta {
        found: false,
        interner,
    };
    meta.visit_program(program);
    if meta.found {
        return Err("ESM bundle does not yet support import.meta");
    }
    if !entry {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    let mut bindings: Vec<Atom> = Vec::new();
    for statement in &program.body {
        match statement {
            Statement::ExportNamed(export) => {
                if export.source.is_some() || !export.specifiers.is_empty() {
                    return Err(
                        "ESM bundle requires direct immutable entry export declarations; export lists and re-exports are not supported",
                    );
                }
                match export.declaration {
                    Some(Statement::VariableDeclaration(declaration))
                        if declaration.kind == VarKind::Const =>
                    {
                        for declaration in &declaration.declarations {
                            let Pattern::Ident(id) = declaration.id else {
                                return Err(
                                    "ESM bundle does not yet support destructured entry exports",
                                );
                            };
                            names.push(interner.resolve(id.name).to_string());
                            bindings.push(id.name);
                        }
                    }
                    Some(Statement::FunctionDeclaration(function)) => {
                        let id = function
                            .id
                            .as_ref()
                            .ok_or("ESM named function export has no binding")?;
                        names.push(interner.resolve(id.name).to_string());
                        bindings.push(id.name);
                    }
                    Some(Statement::ClassDeclaration(class)) => {
                        let id = class
                            .id
                            .as_ref()
                            .ok_or("ESM named class export has no binding")?;
                        names.push(interner.resolve(id.name).to_string());
                        bindings.push(id.name);
                    }
                    _ => {
                        return Err(
                            "ESM bundle requires immutable const/function/class entry exports",
                        );
                    }
                }
            }
            Statement::ExportDefault(export) => {
                names.push("default".to_string());
                match export.declaration {
                    ExportDefaultKind::Function(function) => {
                        if let Some(id) = &function.id {
                            bindings.push(id.name);
                        }
                    }
                    ExportDefaultKind::Class(class) => {
                        if let Some(id) = &class.id {
                            bindings.push(id.name);
                        }
                    }
                    ExportDefaultKind::Expression(_) => {}
                }
            }
            Statement::ExportAll(_) => {
                return Err("ESM bundle does not yet support entry re-exports");
            }
            _ => {}
        }
    }
    for binding in bindings {
        let Some(symbol) = semantic
            .scopes
            .first()
            .and_then(|scope| scope.bindings.get(&binding))
        else {
            return Err("ESM bundle could not prove an entry export binding");
        };
        if semantic.references.iter().any(|reference| {
            reference.resolved.as_ref() == Some(symbol) && reference.access.is_write()
        }) {
            return Err("ESM bundle cannot publish a mutable entry export as an immutable binding");
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// All accepted modules have ESM strict semantics, including undefined top-level `this`.
pub(crate) const PRELUDE: &str = r#"(function(root) {
var __wake_cache__ = {};
function __wake_require__(id) {
  var cached = __wake_cache__[id];
  if (cached) return cached.exports;
  var module = { exports: {} };
  __wake_cache__[id] = module;
  __wake_modules__[id].call(void 0, module, module.exports, __wake_require__);
  return module.exports;
}
"#;

pub(crate) fn append_exports(output: &mut String, names: &[String]) {
    output.push('\n');
    for (index, name) in names.iter().enumerate() {
        output.push_str(&format!(
            "const __wake_esm_export_{index} = __wake_esm_entry__[{}];\nexport {{ __wake_esm_export_{index} as {name} }};\n",
            serde_json::to_string(name).expect("export name serializes")
        ));
    }
    // A side-effect-only entry must still be parsed as ESM by hosts.
    if names.is_empty() {
        output.push_str("export {};\n");
    }
}
