//! Parser-owned lexical early errors for private-brand checks (`#name in value`).
//!
//! A check must resolve to a private field or method declared in its enclosing class or an
//! enclosing class environment. All names in one class body are visible throughout that body,
//! including forward references, nested functions, computed keys, and static blocks. A class
//! heritage expression is evaluated in the outer private environment, before its own names
//! become visible. These checks use AST declarations without importing semantic analysis and
//! do not change validation of pre-existing private member-access expressions.

use wake_common::{Atom, Diagnostic, FxHashSet};
use wake_ecma_ast::{Class, ClassMember, Expression, Program, PropertyKey, Visit, walk_expression};

pub(crate) fn validate(program: &Program<'_>, diagnostics: &mut Vec<Diagnostic>) {
    PrivateNameValidator {
        scopes: Vec::new(),
        diagnostics,
    }
    .visit_program(program);
}

struct PrivateNameValidator<'d> {
    scopes: Vec<FxHashSet<Atom>>,
    diagnostics: &'d mut Vec<Diagnostic>,
}

impl<'a> Visit<'a> for PrivateNameValidator<'_> {
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if let Expression::PrivateIn(check) = expression
            && !self
                .scopes
                .iter()
                .rev()
                .any(|scope| scope.contains(&check.name.name))
        {
            self.diagnostics.push(
                Diagnostic::error("私有名未在可见类作用域中声明")
                    .with_code("WAKE0200")
                    .with_primary(check.name.span, "此私有名没有可见声明"),
            );
        }
        walk_expression(self, expression);
    }

    fn visit_class(&mut self, class: &Class<'a>) {
        for decorator in &class.decorators {
            self.visit_expression(decorator);
        }
        if let Some(super_class) = &class.super_class {
            self.visit_expression(super_class);
        }

        let names = class
            .body
            .iter()
            .filter_map(|member| {
                let key = match member {
                    ClassMember::Method(method) => &method.key,
                    ClassMember::Property(property) => &property.key,
                    ClassMember::StaticBlock(_) => return None,
                };
                match key {
                    PropertyKey::Private(name) => Some(name.name),
                    _ => None,
                }
            })
            .collect();
        self.scopes.push(names);
        for member in &class.body {
            match member {
                ClassMember::Method(method) => {
                    for decorator in &method.decorators {
                        self.visit_expression(decorator);
                    }
                    if let PropertyKey::Computed(key) = &method.key {
                        self.visit_expression(key);
                    }
                    self.visit_function(method.value);
                }
                ClassMember::Property(property) => {
                    for decorator in &property.decorators {
                        self.visit_expression(decorator);
                    }
                    if let PropertyKey::Computed(key) = &property.key {
                        self.visit_expression(key);
                    }
                    if let Some(value) = &property.value {
                        self.visit_expression(value);
                    }
                }
                ClassMember::StaticBlock(block) => {
                    for statement in &block.body {
                        self.visit_statement(statement);
                    }
                }
            }
        }
        self.scopes.pop();
    }
}

#[cfg(test)]
mod tests {
    use wake_common::Interner;

    use crate::{SourceType, parse, parse_declaration_facts};

    #[test]
    fn private_brand_checks_reject_names_outside_their_lexical_class_scope() {
        for source_type in [SourceType::Module, SourceType::TypeScript, SourceType::Tsx] {
            for source in [
                "#missing in value;",
                "class C { has(value) { return #missing in value; } }",
                "class C { #missing; } #missing in value;",
                "class First { #missing; } class Second { has(value) { return #missing in value; } }",
                "class C extends (#missing in value ? Object : Object) { #missing; }",
                "class Outer { #outer; make() { return class Inner extends (#missing in value ? Object : Object) { #missing; }; } }",
                "class Outer { make() { return class Inner { #missing; }; } has(value) { return #missing in value; } }",
                "class C { #present; static { #missing in value; } }",
            ] {
                let output = parse(source, &Interner::new(), source_type);
                assert!(
                    output.has_errors(),
                    "accepted private name outside its scope: {source}"
                );
                assert!(
                    output.diagnostics.iter().any(|diagnostic| {
                        diagnostic.code.as_deref() == Some("WAKE0200")
                            && diagnostic.message.contains("私有名")
                            && diagnostic.labels.iter().any(|label| {
                                &source[label.span.lo as usize..label.span.hi as usize]
                                    == "#missing"
                            })
                    }),
                    "missing precise private-name diagnostic: {source}: {:?}",
                    output.diagnostics
                );
                if source_type.is_typescript() {
                    assert!(
                        parse_declaration_facts(source, source_type).is_err(),
                        "{source}"
                    );
                }
            }
        }
    }

    #[test]
    fn private_brand_checks_accept_forward_nested_and_static_declarations() {
        for source_type in [SourceType::Module, SourceType::TypeScript, SourceType::Tsx] {
            for source in [
                "class C { has(value) { return #x in value; } #x; }",
                "class C { static has(value) { return #x in value; } static #x; }",
                "class C { static { #x in this; } static #x; }",
                "class C { has(value) { return #method in value; } #method() {} }",
                "class C { has(value) { return #accessor in value; } get #accessor() { return 1; } }",
                "class C { has(value) { return #accessor in value; } set #accessor(value) {} }",
                "class C { [#x in value]() {} #x; }",
                "class Outer { make() { return class Inner { has(value) { return #outer in value; } }; } #outer; }",
                "class Outer { #outer; make() { return class Inner extends (#outer in value ? Object : Object) { #inner; has(value) { return #outer in value && #inner in value; } }; } }",
                "class Outer { #x; make() { return class Inner { #x; has(value) { return #x in value; } }; } has(value) { return #x in value; } }",
                "const C = class { #x; callback() { return function(value) { return #x in value; }; } };",
            ] {
                let output = parse(source, &Interner::new(), source_type);
                assert!(!output.has_errors(), "{source}: {:?}", output.diagnostics);
            }
        }
    }
}
