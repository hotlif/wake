//! AST 遍历 trait（手写）。Phase 2.1 覆盖全节点递归；将来可用宏生成（DESIGN §4.2）。
//!
//! 覆盖 [`Visit::visit_program`] / `visit_statement` / `visit_expression` / `visit_pattern` 等，
//! 默认实现继续下钻。依赖提取、结构指纹、未来 tree-shaking 都复用此遍历。

use crate::expr::*;
use crate::literal::TemplateLiteral;
use crate::module::*;
use crate::pattern::*;
use crate::stmt::*;
use crate::{Ident, Program};

/// 只读遍历。
pub trait Visit<'a>: Sized {
    fn visit_program(&mut self, node: &Program<'a>) {
        walk_program(self, node);
    }
    fn visit_statement(&mut self, node: &Statement<'a>) {
        walk_statement(self, node);
    }
    fn visit_expression(&mut self, node: &Expression<'a>) {
        walk_expression(self, node);
    }
    fn visit_pattern(&mut self, node: &Pattern<'a>) {
        walk_pattern(self, node);
    }
    fn visit_function(&mut self, node: &Function<'a>) {
        walk_function(self, node);
    }
    fn visit_class(&mut self, node: &Class<'a>) {
        walk_class(self, node);
    }
    fn visit_template(&mut self, node: &TemplateLiteral<'a>) {
        for e in node.expressions.iter() {
            self.visit_expression(e);
        }
    }
    fn visit_ident(&mut self, _node: &Ident) {}
}

pub fn walk_program<'a, V: Visit<'a>>(v: &mut V, node: &Program<'a>) {
    for stmt in node.body.iter() {
        v.visit_statement(stmt);
    }
}

pub fn walk_statement<'a, V: Visit<'a>>(v: &mut V, node: &Statement<'a>) {
    use Statement::*;
    match node {
        VariableDeclaration(d) => walk_var_decl(v, d),
        FunctionDeclaration(f) => v.visit_function(f),
        ClassDeclaration(c) => v.visit_class(c),
        Block(b) => walk_stmts(v, &b.body),
        Empty(_) | Debugger(_) | Break(_) | Continue(_) => {}
        Expression(e) => v.visit_expression(&e.expression),
        If(s) => {
            v.visit_expression(&s.test);
            v.visit_statement(&s.consequent);
            if let Some(alt) = &s.alternate {
                v.visit_statement(alt);
            }
        }
        For(s) => {
            if let Some(init) = &s.init {
                match init {
                    ForInit::Variable(d) => walk_var_decl(v, d),
                    ForInit::Expression(e) => v.visit_expression(e),
                }
            }
            if let Some(t) = &s.test {
                v.visit_expression(t);
            }
            if let Some(u) = &s.update {
                v.visit_expression(u);
            }
            v.visit_statement(&s.body);
        }
        ForIn(s) => {
            walk_for_left(v, &s.left);
            v.visit_expression(&s.right);
            v.visit_statement(&s.body);
        }
        ForOf(s) => {
            walk_for_left(v, &s.left);
            v.visit_expression(&s.right);
            v.visit_statement(&s.body);
        }
        While(s) => {
            v.visit_expression(&s.test);
            v.visit_statement(&s.body);
        }
        DoWhile(s) => {
            v.visit_statement(&s.body);
            v.visit_expression(&s.test);
        }
        Switch(s) => {
            v.visit_expression(&s.discriminant);
            for case in s.cases.iter() {
                if let Some(t) = &case.test {
                    v.visit_expression(t);
                }
                walk_stmts(v, &case.consequent);
            }
        }
        Return(s) => {
            if let Some(a) = &s.argument {
                v.visit_expression(a);
            }
        }
        Throw(s) => v.visit_expression(&s.argument),
        Try(s) => {
            walk_stmts(v, &s.block.body);
            if let Some(h) = &s.handler {
                if let Some(p) = &h.param {
                    v.visit_pattern(p);
                }
                walk_stmts(v, &h.body.body);
            }
            if let Some(f) = &s.finalizer {
                walk_stmts(v, &f.body);
            }
        }
        Labeled(s) => v.visit_statement(&s.body),
        With(s) => {
            v.visit_expression(&s.object);
            v.visit_statement(&s.body);
        }
        Import(_) => {}
        ExportNamed(s) => {
            if let Some(d) = &s.declaration {
                v.visit_statement(d);
            }
        }
        ExportDefault(s) => match s.declaration {
            ExportDefaultKind::Function(f) => v.visit_function(f),
            ExportDefaultKind::Class(c) => v.visit_class(c),
            ExportDefaultKind::Expression(e) => v.visit_expression(&e),
        },
        ExportAll(_) => {}
    }
}

fn walk_stmts<'a, V: Visit<'a>>(v: &mut V, stmts: &crate::AVec<'a, Statement<'a>>) {
    for s in stmts.iter() {
        v.visit_statement(s);
    }
}

fn walk_var_decl<'a, V: Visit<'a>>(v: &mut V, d: &VariableDeclaration<'a>) {
    for decl in d.declarations.iter() {
        v.visit_pattern(&decl.id);
        if let Some(init) = &decl.init {
            v.visit_expression(init);
        }
    }
}

fn walk_for_left<'a, V: Visit<'a>>(v: &mut V, left: &ForLeft<'a>) {
    match left {
        ForLeft::Variable(d) => walk_var_decl(v, d),
        ForLeft::Target(e) => v.visit_expression(e),
    }
}

pub fn walk_expression<'a, V: Visit<'a>>(v: &mut V, node: &Expression<'a>) {
    use Expression::*;
    match node {
        NumberLiteral(_) | StringLiteral(_) | BooleanLiteral(_) | NullLiteral(_)
        | BigIntLiteral(_) | RegExpLiteral(_) | This(_) | Super(_) | MetaProperty(_) => {}
        Identifier(i) => v.visit_ident(i),
        TemplateLiteral(t) => v.visit_template(t),
        Array(a) => {
            for el in a.elements.iter().flatten() {
                v.visit_expression(el);
            }
        }
        Object(o) => {
            for m in o.properties.iter() {
                match m {
                    ObjectMember::Property(p) => {
                        walk_property_key(v, &p.key);
                        v.visit_expression(&p.value);
                    }
                    ObjectMember::Spread(s) => v.visit_expression(&s.argument),
                }
            }
        }
        Function(f) => v.visit_function(f),
        Arrow(a) => {
            for p in a.params.iter() {
                v.visit_pattern(p);
            }
            match a.body {
                ArrowBody::Block(b) => walk_stmts(v, &b.statements),
                ArrowBody::Expression(e) => v.visit_expression(&e),
            }
        }
        Class(c) => v.visit_class(c),
        Unary(u) => v.visit_expression(&u.argument),
        Update(u) => v.visit_expression(&u.argument),
        Binary(b) => {
            v.visit_expression(&b.left);
            v.visit_expression(&b.right);
        }
        Logical(l) => {
            v.visit_expression(&l.left);
            v.visit_expression(&l.right);
        }
        Assignment(a) => {
            v.visit_expression(&a.left);
            v.visit_expression(&a.right);
        }
        Conditional(c) => {
            v.visit_expression(&c.test);
            v.visit_expression(&c.consequent);
            v.visit_expression(&c.alternate);
        }
        Call(c) => {
            v.visit_expression(&c.callee);
            for arg in c.arguments.iter() {
                v.visit_expression(arg);
            }
        }
        New(n) => {
            v.visit_expression(&n.callee);
            for arg in n.arguments.iter() {
                v.visit_expression(arg);
            }
        }
        Member(m) => {
            v.visit_expression(&m.object);
            if let MemberProperty::Computed(e) = &m.property {
                v.visit_expression(e);
            }
        }
        Sequence(s) => {
            for e in s.expressions.iter() {
                v.visit_expression(e);
            }
        }
        TaggedTemplate(t) => {
            v.visit_expression(&t.tag);
            v.visit_template(t.quasi);
        }
        Spread(s) => v.visit_expression(&s.argument),
        Await(a) => v.visit_expression(&a.argument),
        Yield(y) => {
            if let Some(a) = &y.argument {
                v.visit_expression(a);
            }
        }
        Import(i) => {
            v.visit_expression(&i.source);
            if let Some(o) = &i.options {
                v.visit_expression(o);
            }
        }
    }
}

fn walk_property_key<'a, V: Visit<'a>>(v: &mut V, key: &PropertyKey<'a>) {
    if let PropertyKey::Computed(e) = key {
        v.visit_expression(e);
    }
}

pub fn walk_pattern<'a, V: Visit<'a>>(v: &mut V, node: &Pattern<'a>) {
    match node {
        Pattern::Ident(i) => v.visit_ident(i),
        Pattern::Array(a) => {
            for el in a.elements.iter().flatten() {
                v.visit_pattern(el);
            }
        }
        Pattern::Object(o) => {
            for p in o.properties.iter() {
                walk_property_key(v, &p.key);
                v.visit_pattern(&p.value);
            }
            if let Some(rest) = &o.rest {
                v.visit_pattern(&rest.argument);
            }
        }
        Pattern::Assignment(a) => {
            v.visit_pattern(&a.left);
            v.visit_expression(&a.right);
        }
        Pattern::Rest(r) => v.visit_pattern(&r.argument),
    }
}

pub fn walk_function<'a, V: Visit<'a>>(v: &mut V, node: &Function<'a>) {
    for p in node.params.iter() {
        v.visit_pattern(p);
    }
    if let Some(body) = node.body {
        walk_stmts(v, &body.statements);
    }
}

pub fn walk_class<'a, V: Visit<'a>>(v: &mut V, node: &Class<'a>) {
    // 装饰器表达式**必须**参与遍历：它们是真实的运行时引用。漏掉会让 mangler 看不到这些
    // 引用（重命名后装饰器名对不上）、也会让 tree-shaking 误判装饰器函数未被使用而删除。
    for d in node.decorators.iter() {
        v.visit_expression(d);
    }
    if let Some(sc) = &node.super_class {
        v.visit_expression(sc);
    }
    for member in node.body.iter() {
        match member {
            ClassMember::Method(m) => {
                for d in m.decorators.iter() {
                    v.visit_expression(d);
                }
                walk_property_key(v, &m.key);
                v.visit_function(m.value);
            }
            ClassMember::Property(p) => {
                for d in p.decorators.iter() {
                    v.visit_expression(d);
                }
                walk_property_key(v, &p.key);
                if let Some(val) = &p.value {
                    v.visit_expression(val);
                }
            }
            ClassMember::StaticBlock(b) => walk_stmts(v, &b.body),
        }
    }
}
