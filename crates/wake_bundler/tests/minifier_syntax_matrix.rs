//! Wake-owned end-to-end syntax matrix for the optimizer pipeline.
//!
//! The fixtures in this file are intentionally small and original.  Together they exercise every
//! JavaScript AST family that can reach the bundler after parsing, plus the TypeScript/JSX/TSX
//! lowering front-ends.  Host support is not an acceptance precondition: syntax which an installed
//! Node version cannot execute still has to build in readable and optimized modes and the emitted
//! chunks still have to reparse.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use wake_bundler::{BuildOutput, IncrementalBundler};
use wake_common::{Interner, MemoryFileSystem};
use wake_ecma_ast::expr::{
    ArrowBody, AssignmentOperator, BinaryOperator, ClassMember, Expression, LogicalOperator,
    MemberProperty, MethodKind, ObjectMember, PropertyKey, PropertyKind, UnaryOperator,
    UpdateOperator,
};
use wake_ecma_ast::module::{
    AttributesKeyword, ExportDefaultKind, ImportSpecifier, ModuleExportName,
};
use wake_ecma_ast::pattern::Pattern;
use wake_ecma_ast::stmt::{ForInit, ForLeft, Statement, VarKind};
use wake_ecma_ast::visit::{Visit, walk_class, walk_expression, walk_pattern, walk_statement};
use wake_ecma_ast::{Class, SourceType};

const JSX_RUNTIME: &str = r#"
function node(type, props) { return { type: typeof type === "function" ? type.name : type, props: props || {} }; }
exports.jsx = node;
exports.jsxs = node;
exports.Fragment = "matrix-fragment";
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeCoverage {
    Differential,
    BuildAndReparseOnly(&'static str),
}

#[derive(Clone, Copy)]
struct Fixture {
    name: &'static str,
    entry: &'static str,
    files: &'static [(&'static str, &'static str)],
    runtime: RuntimeCoverage,
}

fn build(fixture: Fixture, minify: bool, source_map: bool) -> BuildOutput {
    let fs = MemoryFileSystem::new();
    for (path, source) in fixture.files {
        fs.insert(path, source.as_bytes().to_vec());
    }
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    if minify {
        bundler.enable_minify();
    }
    if source_map {
        bundler.enable_sourcemap();
    }
    let output = bundler.build(Path::new(fixture.entry));
    assert!(
        !output.has_errors(),
        "[{} / minify={minify} / map={source_map}] build failed: {:?}",
        fixture.name,
        output.diagnostics
    );
    output
}

fn javascript_without_map_trailer(code: &str) -> &str {
    let code = code
        .rfind("//# sourceMappingURL=")
        .map_or(code, |trailer| &code[..trailer]);
    code.trim_end_matches(['\r', '\n'])
}

fn assert_reparses(fixture: Fixture, mode: &str, output: &BuildOutput) {
    assert!(
        !output.chunks.is_empty(),
        "[{}] emitted no chunks",
        fixture.name
    );
    for chunk in &output.chunks {
        let interner = Interner::new();
        let parsed = wake_ecma_parser::parse(&chunk.code, &interner, SourceType::Script);
        assert!(
            !parsed.has_errors(),
            "[{} / {mode} / chunk={}] emitted JavaScript must reparse: {:?}\n--- code ---\n{}",
            fixture.name,
            chunk.file_name,
            parsed.diagnostics,
            chunk.code
        );
    }
}

fn chunks_by_id(output: &BuildOutput) -> Vec<(u32, &str)> {
    let mut chunks = output
        .chunks
        .iter()
        .map(|chunk| (chunk.chunk_id, javascript_without_map_trailer(&chunk.code)))
        .collect::<Vec<_>>();
    chunks.sort_unstable_by_key(|(id, _)| *id);
    chunks
}

fn assert_mapped_and_unmapped_match(
    fixture: Fixture,
    unmapped: &BuildOutput,
    mapped: &BuildOutput,
) {
    assert_eq!(
        chunks_by_id(mapped),
        chunks_by_id(unmapped),
        "[{}] source-map collection changed JavaScript bytes",
        fixture.name
    );
    for chunk in &mapped.chunks {
        let map = chunk.source_map.as_deref().unwrap_or_else(|| {
            panic!(
                "[{} / chunk={}] minify + sourceMap must emit a map",
                fixture.name, chunk.file_name
            )
        });
        let value: serde_json::Value = serde_json::from_str(map).unwrap_or_else(|error| {
            panic!(
                "[{} / chunk={}] invalid source map: {error}",
                fixture.name, chunk.file_name
            )
        });
        assert_eq!(value["version"], 3, "[{}] source map version", fixture.name);
        assert!(
            value["sources"]
                .as_array()
                .is_some_and(|sources| !sources.is_empty()),
            "[{} / chunk={}] source map has no sources: {value}",
            fixture.name,
            chunk.file_name
        );
        assert!(
            value["mappings"]
                .as_str()
                .is_some_and(|mappings| !mappings.is_empty()),
            "[{} / chunk={}] source map has no mappings field: {value}",
            fixture.name,
            chunk.file_name
        );
    }
}

fn assert_repeat_is_deterministic(fixture: Fixture, first: &BuildOutput, second: &BuildOutput) {
    let describe = |output: &BuildOutput| {
        output
            .chunks
            .iter()
            .map(|chunk| {
                (
                    chunk.chunk_id,
                    chunk.kind,
                    chunk.name.clone(),
                    chunk.file_name.clone(),
                    chunk.module_ids.clone(),
                    chunk.imports.clone(),
                    chunk.code.clone(),
                    chunk.source_map.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        describe(first),
        describe(second),
        "[{}] two cold optimized builds were not deterministic",
        fixture.name
    );
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn execute(bundle: &str) -> Output {
    let observer = r#"
Promise.resolve(module.exports).then(
  value => {
    const seen = new WeakSet();
    const normalized = JSON.stringify(value, (_key, item) => {
      if (typeof item === "bigint") return item.toString() + "n";
      if (typeof item === "number" && Object.is(item, -0)) return "-0";
      if (typeof item === "number" && Number.isNaN(item)) return "NaN";
      if (typeof item === "function") return "[function " + item.name + "]";
      if (item && typeof item === "object") {
        if (seen.has(item)) return "[circular]";
        seen.add(item);
      }
      return item;
    });
    process.stdout.write("__WAKE_MATRIX__" + normalized);
  },
  error => {
    process.stderr.write("__WAKE_MATRIX_REJECTION__" + error.name + ":" + error.message);
    process.exitCode = 29;
  }
);
"#;
    Command::new("node")
        .arg("-e")
        .arg(format!("{bundle}\n{observer}"))
        .output()
        .expect("Node was available immediately before matrix execution")
}

fn assert_runtime_differential(fixture: Fixture, readable: &BuildOutput, optimized: &BuildOutput) {
    if !node_available() {
        eprintln!(
            "node unavailable; [{}] retained mandatory build/reparse coverage",
            fixture.name
        );
        return;
    }
    let readable = execute(&readable.bundle);
    let optimized = execute(&optimized.bundle);
    assert_eq!(
        optimized.status.code(),
        readable.status.code(),
        "[{}] optimized exit behavior changed\nreadable stderr: {}\noptimized stderr: {}",
        fixture.name,
        String::from_utf8_lossy(&readable.stderr),
        String::from_utf8_lossy(&optimized.stderr)
    );
    assert_eq!(
        optimized.stdout, readable.stdout,
        "[{}] optimized logs/exports/side-effect order changed",
        fixture.name
    );
    assert_eq!(
        optimized.stderr, readable.stderr,
        "[{}] optimized stderr changed",
        fixture.name
    );
    assert!(
        readable.status.success(),
        "[{}] readable fixture failed in Node: {}",
        fixture.name,
        String::from_utf8_lossy(&readable.stderr)
    );
    assert!(
        readable
            .stdout
            .windows(b"__WAKE_MATRIX__".len())
            .any(|bytes| bytes == b"__WAKE_MATRIX__"),
        "[{}] runtime observer did not finish: {}",
        fixture.name,
        String::from_utf8_lossy(&readable.stdout)
    );
}

#[derive(Default)]
struct AstCoverage {
    statements: BTreeSet<&'static str>,
    expressions: BTreeSet<&'static str>,
    patterns: BTreeSet<&'static str>,
    variable_kinds: BTreeSet<&'static str>,
    for_initializers: BTreeSet<&'static str>,
    for_lefts: BTreeSet<&'static str>,
    unary_operators: BTreeSet<&'static str>,
    update_operators: BTreeSet<&'static str>,
    binary_operators: BTreeSet<&'static str>,
    logical_operators: BTreeSet<&'static str>,
    assignment_operators: BTreeSet<&'static str>,
    member_properties: BTreeSet<&'static str>,
    property_keys: BTreeSet<&'static str>,
    property_kinds: BTreeSet<&'static str>,
    object_members: BTreeSet<&'static str>,
    arrow_bodies: BTreeSet<&'static str>,
    class_members: BTreeSet<&'static str>,
    method_kinds: BTreeSet<&'static str>,
    import_specifiers: BTreeSet<&'static str>,
    export_default_kinds: BTreeSet<&'static str>,
    module_export_names: BTreeSet<&'static str>,
    attribute_keywords: BTreeSet<&'static str>,
}

impl AstCoverage {
    fn record_var_kind(&mut self, kind: VarKind) {
        self.variable_kinds.insert(match kind {
            VarKind::Var => "var",
            VarKind::Let => "let",
            VarKind::Const => "const",
            VarKind::Using => "using",
            VarKind::AwaitUsing => "await-using",
        });
    }

    fn record_module_name(&mut self, name: ModuleExportName) {
        self.module_export_names.insert(match name {
            ModuleExportName::Ident(_) => "identifier",
            ModuleExportName::String(_) => "string",
        });
    }

    fn record_attribute_keyword(&mut self, keyword: AttributesKeyword) {
        self.attribute_keywords.insert(match keyword {
            AttributesKeyword::With => "with",
            AttributesKeyword::Assert => "assert",
        });
    }

    fn record_property_key(&mut self, key: PropertyKey<'_>) {
        self.property_keys.insert(match key {
            PropertyKey::Ident(_) => "identifier",
            PropertyKey::String(_) => "string",
            PropertyKey::Number(_) => "number",
            PropertyKey::Computed(_) => "computed",
            PropertyKey::Private(_) => "private",
        });
    }
}

impl<'a> Visit<'a> for AstCoverage {
    fn visit_statement(&mut self, node: &Statement<'a>) {
        let statement_kind = match node {
            Statement::VariableDeclaration(declaration) => {
                self.record_var_kind(declaration.kind);
                "variable-declaration"
            }
            Statement::FunctionDeclaration(_) => "function-declaration",
            Statement::ClassDeclaration(_) => "class-declaration",
            Statement::Block(_) => "block",
            Statement::Empty(_) => "empty",
            Statement::Expression(_) => "expression",
            Statement::If(_) => "if",
            Statement::For(statement) => {
                if let Some(initializer) = statement.init {
                    let initializer_kind = match initializer {
                        ForInit::Variable(declaration) => {
                            self.record_var_kind(declaration.kind);
                            "variable"
                        }
                        ForInit::Expression(_) => "expression",
                    };
                    self.for_initializers.insert(initializer_kind);
                }
                "for"
            }
            Statement::ForIn(statement) => {
                let left_kind = match statement.left {
                    ForLeft::Variable(declaration) => {
                        self.record_var_kind(declaration.kind);
                        "variable"
                    }
                    ForLeft::Target(_) => "target",
                };
                self.for_lefts.insert(left_kind);
                "for-in"
            }
            Statement::ForOf(statement) => {
                let left_kind = match statement.left {
                    ForLeft::Variable(declaration) => {
                        self.record_var_kind(declaration.kind);
                        "variable"
                    }
                    ForLeft::Target(_) => "target",
                };
                self.for_lefts.insert(left_kind);
                if statement.is_await {
                    "for-await-of"
                } else {
                    "for-of"
                }
            }
            Statement::While(_) => "while",
            Statement::DoWhile(_) => "do-while",
            Statement::Switch(_) => "switch",
            Statement::Return(_) => "return",
            Statement::Break(_) => "break",
            Statement::Continue(_) => "continue",
            Statement::Throw(_) => "throw",
            Statement::Try(_) => "try",
            Statement::Labeled(_) => "labeled",
            Statement::With(_) => "with",
            Statement::Debugger(_) => "debugger",
            Statement::Import(declaration) => {
                for specifier in &declaration.specifiers {
                    let specifier_kind = match specifier {
                        ImportSpecifier::Named { imported, .. } => {
                            self.record_module_name(*imported);
                            "named"
                        }
                        ImportSpecifier::Default { .. } => "default",
                        ImportSpecifier::Namespace { .. } => "namespace",
                    };
                    self.import_specifiers.insert(specifier_kind);
                }
                if let Some(attributes) = declaration.attributes {
                    self.record_attribute_keyword(attributes.keyword);
                    for attribute in attributes.items {
                        self.record_module_name(attribute.key);
                    }
                }
                "import"
            }
            Statement::ExportNamed(declaration) => {
                for specifier in &declaration.specifiers {
                    self.record_module_name(specifier.local);
                    self.record_module_name(specifier.exported);
                }
                if let Some(attributes) = declaration.attributes {
                    self.record_attribute_keyword(attributes.keyword);
                    for attribute in attributes.items {
                        self.record_module_name(attribute.key);
                    }
                }
                "export-named"
            }
            Statement::ExportDefault(declaration) => {
                self.export_default_kinds
                    .insert(match declaration.declaration {
                        ExportDefaultKind::Function(_) => "function",
                        ExportDefaultKind::Class(_) => "class",
                        ExportDefaultKind::Expression(_) => "expression",
                    });
                "export-default"
            }
            Statement::ExportAll(declaration) => {
                if let Some(exported) = declaration.exported {
                    self.record_module_name(exported);
                }
                if let Some(attributes) = declaration.attributes {
                    self.record_attribute_keyword(attributes.keyword);
                    for attribute in attributes.items {
                        self.record_module_name(attribute.key);
                    }
                }
                "export-all"
            }
        };
        self.statements.insert(statement_kind);
        walk_statement(self, node);
    }

    fn visit_expression(&mut self, node: &Expression<'a>) {
        let expression_kind = match node {
            Expression::NumberLiteral(_) => "number-literal",
            Expression::StringLiteral(_) => "string-literal",
            Expression::BooleanLiteral(_) => "boolean-literal",
            Expression::NullLiteral(_) => "null-literal",
            Expression::BigIntLiteral(_) => "bigint-literal",
            Expression::RegExpLiteral(_) => "regexp-literal",
            Expression::TemplateLiteral(_) => "template-literal",
            Expression::Identifier(_) => "identifier",
            Expression::This(_) => "this",
            Expression::Super(_) => "super",
            Expression::MetaProperty(_) => "meta-property",
            Expression::Array(_) => "array",
            Expression::Object(object) => {
                for member in &object.properties {
                    let member_kind = match member {
                        ObjectMember::Property(property) => {
                            self.record_property_key(property.key);
                            self.property_kinds.insert(match property.kind {
                                PropertyKind::Init => "init",
                                PropertyKind::Get => "get",
                                PropertyKind::Set => "set",
                            });
                            "property"
                        }
                        ObjectMember::Spread(_) => "spread",
                    };
                    self.object_members.insert(member_kind);
                }
                "object"
            }
            Expression::Function(_) => "function",
            Expression::Arrow(arrow) => {
                self.arrow_bodies.insert(match arrow.body {
                    ArrowBody::Block(_) => "block",
                    ArrowBody::Expression(_) => "expression",
                });
                "arrow"
            }
            Expression::Class(_) => "class",
            Expression::Unary(unary) => {
                self.unary_operators.insert(match unary.operator {
                    UnaryOperator::Minus => "minus",
                    UnaryOperator::Plus => "plus",
                    UnaryOperator::LogicalNot => "logical-not",
                    UnaryOperator::BitwiseNot => "bitwise-not",
                    UnaryOperator::Typeof => "typeof",
                    UnaryOperator::Void => "void",
                    UnaryOperator::Delete => "delete",
                });
                "unary"
            }
            Expression::Update(update) => {
                self.update_operators.insert(match update.operator {
                    UpdateOperator::Increment => "increment",
                    UpdateOperator::Decrement => "decrement",
                });
                "update"
            }
            Expression::Binary(binary) => {
                self.binary_operators.insert(match binary.operator {
                    BinaryOperator::Add => "add",
                    BinaryOperator::Sub => "sub",
                    BinaryOperator::Mul => "mul",
                    BinaryOperator::Div => "div",
                    BinaryOperator::Rem => "rem",
                    BinaryOperator::Exp => "exp",
                    BinaryOperator::BitAnd => "bit-and",
                    BinaryOperator::BitOr => "bit-or",
                    BinaryOperator::BitXor => "bit-xor",
                    BinaryOperator::Shl => "shl",
                    BinaryOperator::Shr => "shr",
                    BinaryOperator::Ushr => "ushr",
                    BinaryOperator::Eq => "eq",
                    BinaryOperator::NotEq => "not-eq",
                    BinaryOperator::StrictEq => "strict-eq",
                    BinaryOperator::StrictNotEq => "strict-not-eq",
                    BinaryOperator::Lt => "lt",
                    BinaryOperator::Gt => "gt",
                    BinaryOperator::LtEq => "lt-eq",
                    BinaryOperator::GtEq => "gt-eq",
                    BinaryOperator::In => "in",
                    BinaryOperator::Instanceof => "instanceof",
                });
                "binary"
            }
            Expression::Logical(logical) => {
                self.logical_operators.insert(match logical.operator {
                    LogicalOperator::And => "and",
                    LogicalOperator::Or => "or",
                    LogicalOperator::Coalesce => "coalesce",
                });
                "logical"
            }
            Expression::Assignment(assignment) => {
                self.assignment_operators.insert(match assignment.operator {
                    AssignmentOperator::Assign => "assign",
                    AssignmentOperator::Add => "add",
                    AssignmentOperator::Sub => "sub",
                    AssignmentOperator::Mul => "mul",
                    AssignmentOperator::Div => "div",
                    AssignmentOperator::Rem => "rem",
                    AssignmentOperator::Exp => "exp",
                    AssignmentOperator::Shl => "shl",
                    AssignmentOperator::Shr => "shr",
                    AssignmentOperator::Ushr => "ushr",
                    AssignmentOperator::BitAnd => "bit-and",
                    AssignmentOperator::BitOr => "bit-or",
                    AssignmentOperator::BitXor => "bit-xor",
                    AssignmentOperator::And => "and",
                    AssignmentOperator::Or => "or",
                    AssignmentOperator::Coalesce => "coalesce",
                });
                "assignment"
            }
            Expression::Conditional(_) => "conditional",
            Expression::Call(_) => "call",
            Expression::New(_) => "new",
            Expression::Member(member) => {
                self.member_properties.insert(match member.property {
                    MemberProperty::Ident(_) => "identifier",
                    MemberProperty::Computed(_) => "computed",
                    MemberProperty::Private(_) => "private",
                });
                "member"
            }
            Expression::Sequence(_) => "sequence",
            Expression::TaggedTemplate(_) => "tagged-template",
            Expression::Spread(_) => "spread",
            Expression::Await(_) => "await",
            Expression::Yield(_) => "yield",
            Expression::Import(_) => "dynamic-import",
        };
        self.expressions.insert(expression_kind);
        walk_expression(self, node);
    }

    fn visit_pattern(&mut self, node: &Pattern<'a>) {
        let pattern_kind = match node {
            Pattern::Ident(_) => "identifier",
            Pattern::Array(_) => "array",
            Pattern::Object(object) => {
                for property in &object.properties {
                    self.record_property_key(property.key);
                }
                "object"
            }
            Pattern::Assignment(_) => "assignment",
            Pattern::Rest(_) => "rest",
        };
        self.patterns.insert(pattern_kind);
        walk_pattern(self, node);
    }

    fn visit_class(&mut self, node: &Class<'a>) {
        for member in &node.body {
            let member_kind = match member {
                ClassMember::Method(method) => {
                    self.record_property_key(method.key);
                    self.method_kinds.insert(match method.kind {
                        MethodKind::Constructor => "constructor",
                        MethodKind::Method => "method",
                        MethodKind::Get => "get",
                        MethodKind::Set => "set",
                    });
                    "method"
                }
                ClassMember::Property(property) => {
                    self.record_property_key(property.key);
                    "property"
                }
                ClassMember::StaticBlock(_) => "static-block",
            };
            self.class_members.insert(member_kind);
        }
        walk_class(self, node);
    }
}

fn source_type(path: &str) -> Option<SourceType> {
    if path.ends_with(".tsx") {
        Some(SourceType::Tsx)
    } else if path.ends_with(".ts") {
        Some(SourceType::TypeScript)
    } else if path.ends_with(".jsx") {
        Some(SourceType::Jsx)
    } else if path.ends_with(".cjs") {
        Some(SourceType::Script)
    } else if path.ends_with(".js") {
        Some(SourceType::Module)
    } else {
        None
    }
}

fn collect_ast_coverage() -> AstCoverage {
    let mut coverage = AstCoverage::default();
    let mut visited = BTreeSet::new();
    for fixture in FIXTURES {
        for &(path, source) in fixture.files {
            let Some(source_type) = source_type(path) else {
                continue;
            };
            if !visited.insert((path, source)) {
                continue;
            }
            let interner = Interner::new();
            let parsed = wake_ecma_parser::parse(source, &interner, source_type);
            assert!(
                !parsed.has_errors(),
                "[ast-coverage / {path}] parse failed: {:?}",
                parsed.diagnostics
            );
            parsed
                .module
                .with_ast(|program| coverage.visit_program(program));
        }
    }
    coverage
}

fn expected<const N: usize>(values: [&'static str; N]) -> BTreeSet<&'static str> {
    values.into_iter().collect()
}

const STATEMENTS_AND_PATTERNS: &[(&str, &str)] = &[(
    "src/index.js",
    r#"
const trace = [];
function bareReturn(stop) { if (stop) return; return 1; }
async function consumeAsync() {
  let total = 0;
  for await (const item of [Promise.resolve(2), Promise.resolve(3)]) total += item;
  return total;
}
function exercise(seed) {
  var total = seed;
  let cursor = 0;
  const [first, , second = 2, ...tail] = [1, , undefined, 4, 5];
  const { alpha: renamed = 3, nested: { value }, plain, ...remaining } = {
    alpha: undefined, nested: { value: 4 }, plain: 5, extra: 6
  };
  const dynamicPatternKey = "dynamic";
  const {
    [dynamicPatternKey]: computedBinding,
    "string-key": stringBinding,
    7: numericBinding
  } = { dynamic: 7, "string-key": 8, 7: 9 };
  { total += first + second + tail.length + renamed + value + plain + remaining.extra + computedBinding + stringBinding + numericBinding; }
  ;
  debugger;
  if (total > 0) total += 1; else total -= 1;
  outer: for (let index = 0; index < 4; index++) {
    if (index === 0) continue;
    if (index === 1) continue outer;
    if (index === 3) break outer;
    total += index;
  }
  for (cursor = 0; cursor < 2; cursor++) total += cursor;
  for (;;) { total++; break; }
  for (const key in { x: 1, y: 2 }) total += key.length;
  let keyTarget;
  for (keyTarget in { zz: 1 }) total += keyTarget.length;
  for (const item of [1, 2]) total += item;
  let itemTarget;
  for (itemTarget of [3]) total += itemTarget;
  while (cursor > 0) { cursor--; if (cursor === 0) break; }
  do { cursor++; } while (cursor < 1);
  switch (seed) {
    case 1: total += 10; break;
    case 2: total += 20;
    default: total += 30;
  }
  try {
    if (seed === 1) throw new Error("owned");
  } catch ({ message }) {
    trace.push(message);
  } finally {
    trace.push("finally");
  }
  try { throw 7; } catch { trace.push("catch-without-binding"); }
  label: { total += 1; break label; }
  return total;
}
const assigned = {};
let left, right, objectRest;
[left, , right = 9] = [7, , undefined];
({ p: assigned.value, q: right, ...objectRest } = { p: 8, q: 10, r: 11 });
const asyncTotal = await consumeAsync();
export const observation = {
  result: exercise(1), bare: bareReturn(false), stopped: bareReturn(true),
  assigned, left, right, objectRest, asyncTotal, trace
};
"#,
)];

const EXPRESSIONS_AND_OPERATORS: &[(&str, &str)] = &[(
    "src/index.js",
    r#"
const events = [];
const identifier = 3;
const number = 1.5;
const string = "owned";
const boolean = true;
const nil = null;
const bigint = 42n;
const regexp = /wa+ke/gi;
const template = `value:${identifier}`;
function tag(strings, value) { return strings.raw[0] + value; }
const tagged = tag`tag:${identifier}`;
const array = [number, , ...[identifier, 4]];
const shorthand = 5;
const computedKey = "computed";
const object = {
  plain: 1,
  "string-key": 2,
  3: 3,
  [computedKey]: 4,
  shorthand,
  get read() { return this.plain; },
  set write(value) { events.push("set:" + value); },
  method(value) { return value + this.plain; },
  __proto__: null,
  ...{ spread: 6 }
};
object.write = 7;
const anonymous = function (value) { return value + 1; };
const named = function recursive(value) { return value ? recursive(value - 1) + 1 : 0; };
const generator = function* () { yield; yield 1; yield* [2]; return 3; };
const asyncFunction = async function () { return await Promise.resolve(4); };
const expressionArrow = value => value * 2;
const blockArrow = (value = 2) => { return value + 1; };
const asyncArrow = async value => await value;
class Base { base() { return 1; } }
const ClassExpression = class NamedExpression extends Base {
  #secret = 2;
  read() { return this.#secret + super.base(); }
};
function Target(value) { this.value = value; this.target = new.target.name; }
let update = 2;
const updates = [++update, update++, --update, update--];
const unary = [-identifier, +string.length, !false, ~1, typeof missingGlobal, void 0];
const deletionTarget = { value: 1 };
unary.push(delete deletionTarget.value);
const binary = [
  5 + 2, 5 - 2, 5 * 2, 5 / 2, 5 % 2, 2 ** 3,
  6 & 3, 4 | 1, 6 ^ 3, 2 << 2, 8 >> 1, 8 >>> 1,
  1 == "1", 1 != "2", 1 === 1, 1 !== "1",
  1 < 2, 2 > 1, 1 <= 1, 2 >= 2, "plain" in object, array instanceof Array
];
const logical = [true && 1, false || 2, null ?? 3];
let assignment = 1;
const assignments = [
  assignment = 8,
  assignment += 2,
  assignment -= 1,
  assignment *= 2,
  assignment /= 3,
  assignment %= 4,
  assignment **= 2,
  assignment <<= 1,
  assignment >>= 1,
  assignment >>>= 1,
  assignment &= 7,
  assignment |= 8,
  assignment ^= 3,
  assignment &&= 9,
  assignment ||= 10,
  assignment ??= 11
];
const conditional = assignment ? "yes" : "no";
const optionalFunction = anonymous;
const calls = [anonymous(1), optionalFunction?.(2), object.method(3)];
const constructed = new Target(4);
const members = [object.plain, object[computedKey], object?.plain, object?.[computedKey]];
const sequence = (events.push("sequence"), 12);
const iterator = generator();
const generated = [iterator.next(), iterator.next(), iterator.next(), iterator.next()];
const promised = await Promise.all([asyncFunction(), asyncArrow(Promise.resolve(5))]);
const spreadCall = Math.max(...[1, 8, 3]);
export const observation = {
  literals: [number, string, boolean, nil, bigint, regexp.source, template, tagged],
  array, objectValues: [object.read, object.spread], anonymous: anonymous(2), named: named(3),
  arrows: [expressionArrow(3), blockArrow()], classValue: new ClassExpression().read(),
  constructed, updates, unary, binary, logical, assignments, conditional, calls, members,
  sequence, generated, promised, spreadCall, events
};
"#,
)];

const CLASS_MEMBERS_AND_DECORATORS: &[(&str, &str)] = &[(
    "src/index.ts",
    r#"
function keep(value: any, _context: any): any { return value; }
@keep
class Parent { inherited(): number { return 1; } }
@keep
class Everything extends Parent {
  static staticField = 1;
  static { this.staticField += 1; }
  #privateField = 2;
  undecoratedField: number;
  ["computedField"] = 3;
  @keep accessor automatic = 4;
  constructor() { super(); this.undecoratedField = 5; }
  @keep method(): number { return super.inherited(); }
  get value(): number { return this.#privateField; }
  set value(next: number) { this.#privateField = next; }
  static staticMethod(): number { return this.staticField; }
  #privateMethod(): number { return this.#privateField; }
  readPrivate(): number { return this.#privateMethod(); }
}
export const observation = new Everything().readPrivate() + Everything.staticMethod();
"#,
)];

const MODULE_DECLARATIONS: &[(&str, &str)] = &[
    (
        "src/index.js",
        r#"
import "./side-effect.js";
import defaultFunction from "./default-function.js";
import DefaultClass from "./default-class.js";
import defaultExpression from "./default-expression.js";
import * as namespace from "./values.js";
import { named, local as renamed, "hyphen-name" as hyphen } from "./values.js";
export const declared = named + renamed;
export function exportedFunction() { return defaultFunction(); }
export class ExportedClass extends DefaultClass {}
const localExport = namespace.extra;
export { localExport, hyphen as renamedAgain };
export { named as forwarded } from "./values.js";
export * from "./star.js";
export * as starNamespace from "./star.js";
export default {
  sum: declared + defaultExpression + localExport + hyphen,
  sideEffects: globalThis.__wakeOwnedSideEffects,
  classValue: new ExportedClass().value,
  functionValue: exportedFunction()
};
"#,
    ),
    (
        "src/side-effect.js",
        "globalThis.__wakeOwnedSideEffects = (globalThis.__wakeOwnedSideEffects || 0) + 1;",
    ),
    (
        "src/default-function.js",
        "export default function ownedDefault() { return 7; }",
    ),
    (
        "src/default-class.js",
        "export default class OwnedDefault { constructor() { this.value = 8; } }",
    ),
    ("src/default-expression.js", "export default 9;"),
    (
        "src/values.js",
        "export const named = 1; const local = 2; export { local, local as 'hyphen-name' }; export const extra = 3;",
    ),
    ("src/star.js", "export const starValue = 4;"),
];

const DYNAMIC_IMPORT: &[(&str, &str)] = &[
    (
        "src/index.js",
        r#"
const loaded = await import("./lazy.js", { with: { type: "javascript" } });
export const observation = loaded.value + loaded.default;
"#,
    ),
    ("src/lazy.js", "export const value = 20; export default 22;"),
];

const META_PROPERTIES: &[(&str, &str)] = &[(
    "src/index.js",
    r#"
function Constructed() { this.constructorName = new.target.name; }
export const observation = {
  constructorName: new Constructed().constructorName,
  importMetaUrlType: typeof import.meta.url
};
"#,
)];

const IMPORT_ATTRIBUTES: &[(&str, &str)] = &[
    (
        "src/index.ts",
        r#"
import first from "./first.json" with { type: "json", "wake-mode": "owned" };
import second from "./second.json" assert { type: "json" };
export { value as forwarded } from "./values.js" with { type: "javascript" };
export * from "./star.js" with { type: "javascript" };
export * as attributedNamespace from "./namespace.js" with { type: "javascript" };
export const observation: number = first.value + second.value;
"#,
    ),
    ("src/first.json", r#"{"value": 20}"#),
    ("src/second.json", r#"{"value": 22}"#),
    ("src/values.js", "export const value = 1;"),
    ("src/star.js", "export const fromStar = 2;"),
    ("src/namespace.js", "export const fromNamespace = 3;"),
];

const TYPESCRIPT_VALUE_LOWERING: &[(&str, &str)] = &[(
    "src/index.ts",
    r#"
interface Named { readonly name: string }
type FrozenPair<Value> = readonly [Value, Value];
enum Color { Red, Green = 4, Blue }
const enum Direction { Up = 1, Down }
namespace OwnedMetrics { export const base = 6; }
namespace OwnedMetrics { export function multiply(value: number): number { return OwnedMetrics.base * value; } }
abstract class Entity implements Named {
  abstract readonly name: string;
  constructor(public readonly id: string, private rank = 2) {}
  protected score(): number { return this.rank; }
}
class User extends Entity {
  nickname: string = "wake";
  constructor(id: string, public override readonly name: string) { super(id); }
  value(): number { return this.score(); }
}
function format(value: string): string;
function format(value: number): string;
function format(value: string | number): string { return String(value); }
function isNamed(value: unknown): value is Named { return !!value && typeof value === "object"; }
function assertNamed(value: unknown): asserts value is Named { if (!isNamed(value)) throw Error("not named"); }
function receiver(this: { prefix: string }, value: string): string { return this.prefix + value; }
const frozen = [{ name: "Wake" }] as const satisfies readonly Named[];
const identity = <const Value extends Named>(value: Value): Value => value;
const user = new User("id", identity(frozen[0]).name);
assertNamed(user);
export const observation: FrozenPair<string | number> = [
  receiver.call({ prefix: "!" }, format(user.name)),
  OwnedMetrics.multiply(user.value()) + Color.Blue + Direction.Down
];
"#,
)];

const TYPESCRIPT_ERASURE: &[(&str, &str)] = &[(
    "src/index.ts",
    r#"
import type { RemoteShape } from "./missing-types.js";
import type LegacyTypes = require("./missing-legacy-types.js");
import { type RemoteOptions } from "./also-missing.js";
export type { RemoteResult } from "./missing-result.js";
export { type RemoteConfig } from "./missing-config.js";
export as namespace WakeOwnedMatrix;
interface Row { readonly id: string; label?: string }
type Producer<out Value> = () => Value;
type Consumer<in Value> = (value: Value) => void;
type Unwrapped<Value> = Value extends Promise<infer Inner> ? Inner : Value;
type Accessors<Value extends Row> = { [Key in keyof Value as `read${Capitalize<string & Key>}`]-?: () => Value[Key] };
type Tuple = [head: string, count?: number, ...flags: boolean[]];
type Split<Value extends string> = Value extends `${infer Head}-${infer Tail}` ? [Head, Tail] : never;
type Imported = import("./type-query.js").Remote;
declare const ambient: unique symbol;
declare function ambientCall<Value>(value: Value): Value;
declare class AmbientClass { readonly ready: boolean; }
const rows = [{ id: "owned", label: "Wake" }] as const satisfies readonly Row[];
type RowId = typeof rows[number]["id"];
const asserted = (<Row>{ id: rows[0].id })!.id;
export const observation = asserted;
"#,
)];

const TYPESCRIPT_IMPORT_EQUALS: &[(&str, &str)] = &[
    (
        "src/index.ts",
        r#"
import api = require("./api.ts");
import Alias = api.Nested;
export import PublicApi = require("./public.ts");
export = { sum: api.add(20, 22), alias: Alias.value, public: PublicApi.value };
"#,
    ),
    (
        "src/api.ts",
        "const api = { add(a: number, b: number): number { return a + b; }, Nested: { value: 7 } }; export = api;",
    ),
    ("src/public.ts", "export = { value: 9 };"),
];

const JSX_LOWERING: &[(&str, &str)] = &[
    (
        "src/index.jsx",
        r#"
const UI = { Item: function Item() {} };
const extra = { title: "owned" };
const id = "matrix";
export const observation = <>
  <section id={id} hidden data-count={2} {...extra}>
    text &amp; more
    {/* deliberately empty JSX expression */}
    <UI.Item key="member">{[1, 2].map(value => <span key={value}>{value}</span>)}</UI.Item>
  </section>
</>;
"#,
    ),
    ("node_modules/react/jsx-runtime.js", JSX_RUNTIME),
];

const TSX_LOWERING: &[(&str, &str)] = &[
    (
        "src/index.tsx",
        r#"
interface Item { id: string; label?: string }
interface ViewProps<Value> { value: Value }
function View<Value extends Item>({ value }: ViewProps<Value>) {
  return <article data-id={value.id}>{value.label ?? "missing"}</article>;
}
const identity = <Value,>(value: Value): Value => value;
const constrained = <Value extends Item>(value: Value): Value => value;
const defaulted = <Value = Item>(value: Value): Value => value;
const constant = <const Value extends Item>(value: Value): Value => value;
const asyncIdentity = async <Value,>(value: Value): Promise<Value> => value;
const item = constant(defaulted(constrained(identity({ id: "tsx", label: "Wake" }))));
void asyncIdentity(item);
export const observation = <View<Item> value={item} />;
"#,
    ),
    ("node_modules/react/jsx-runtime.js", JSX_RUNTIME),
];

const WITH_AND_DIRECT_EVAL: &[(&str, &str)] = &[(
    "src/index.cjs",
    r#"
var visible = 2;
var result = 0;
var scope = { visible: 40 };
with (scope) { result = visible + eval("visible"); }
module.exports = { observation: result, outer: visible };
"#,
)];

const RESOURCE_MANAGEMENT: &[(&str, &str)] = &[(
    "src/index.ts",
    r#"
class Resource { [Symbol.dispose](): void {} }
class AsyncResource { async [Symbol.asyncDispose](): Promise<void> {} }
export async function consume(): Promise<number> {
  using first = new Resource();
  await using second = new AsyncResource();
  void first;
  void second;
  return 42;
}
"#,
)];

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "statements-and-binding-patterns",
        entry: "src/index.js",
        files: STATEMENTS_AND_PATTERNS,
        runtime: RuntimeCoverage::Differential,
    },
    Fixture {
        name: "expressions-and-operators",
        entry: "src/index.js",
        files: EXPRESSIONS_AND_OPERATORS,
        runtime: RuntimeCoverage::Differential,
    },
    Fixture {
        name: "class-members-and-stage3-decorators",
        entry: "src/index.ts",
        files: CLASS_MEMBERS_AND_DECORATORS,
        runtime: RuntimeCoverage::BuildAndReparseOnly(
            "decorator and auto-accessor host support differs by Node release",
        ),
    },
    Fixture {
        name: "module-declarations",
        entry: "src/index.js",
        files: MODULE_DECLARATIONS,
        runtime: RuntimeCoverage::Differential,
    },
    Fixture {
        name: "dynamic-import-with-options",
        entry: "src/index.js",
        files: DYNAMIC_IMPORT,
        runtime: RuntimeCoverage::Differential,
    },
    Fixture {
        name: "new-target-and-import-meta",
        entry: "src/index.js",
        files: META_PROPERTIES,
        runtime: RuntimeCoverage::Differential,
    },
    Fixture {
        name: "import-and-export-attributes",
        entry: "src/index.ts",
        files: IMPORT_ATTRIBUTES,
        runtime: RuntimeCoverage::Differential,
    },
    Fixture {
        name: "typescript-value-lowering",
        entry: "src/index.ts",
        files: TYPESCRIPT_VALUE_LOWERING,
        runtime: RuntimeCoverage::Differential,
    },
    Fixture {
        name: "typescript-type-erasure",
        entry: "src/index.ts",
        files: TYPESCRIPT_ERASURE,
        runtime: RuntimeCoverage::Differential,
    },
    Fixture {
        name: "typescript-import-equals-export-assignment",
        entry: "src/index.ts",
        files: TYPESCRIPT_IMPORT_EQUALS,
        runtime: RuntimeCoverage::Differential,
    },
    Fixture {
        name: "jsx-lowering",
        entry: "src/index.jsx",
        files: JSX_LOWERING,
        runtime: RuntimeCoverage::Differential,
    },
    Fixture {
        name: "tsx-lowering-and-generic-disambiguation",
        entry: "src/index.tsx",
        files: TSX_LOWERING,
        runtime: RuntimeCoverage::Differential,
    },
    Fixture {
        name: "with-and-direct-eval",
        entry: "src/index.cjs",
        files: WITH_AND_DIRECT_EVAL,
        runtime: RuntimeCoverage::Differential,
    },
    Fixture {
        name: "explicit-resource-management",
        entry: "src/index.ts",
        files: RESOURCE_MANAGEMENT,
        runtime: RuntimeCoverage::BuildAndReparseOnly(
            "using/await using is not implemented by every supported Node host",
        ),
    },
];

#[test]
fn complete_owned_syntax_matrix_builds_reparses_maps_and_is_deterministic() {
    for &fixture in FIXTURES {
        let readable = build(fixture, false, false);
        let optimized = build(fixture, true, false);
        let mapped = build(fixture, true, true);
        let repeated = build(fixture, true, false);
        let mapped_repeated = build(fixture, true, true);

        assert_reparses(fixture, "readable", &readable);
        assert_reparses(fixture, "optimized", &optimized);
        assert_reparses(fixture, "optimized-mapped", &mapped);
        assert_mapped_and_unmapped_match(fixture, &optimized, &mapped);
        assert_repeat_is_deterministic(fixture, &optimized, &repeated);
        assert_repeat_is_deterministic(fixture, &mapped, &mapped_repeated);

        match fixture.runtime {
            RuntimeCoverage::Differential => {
                assert_runtime_differential(fixture, &readable, &optimized)
            }
            RuntimeCoverage::BuildAndReparseOnly(reason) => {
                eprintln!("[{}] runtime differential skipped: {reason}", fixture.name);
            }
        }
    }
}

#[test]
fn matrix_declares_each_ast_and_lowering_family() {
    // This list is intentionally explicit: additions to the public AST should extend the fixture
    // matrix and this review checklist in the same change.
    let covered_families = [
        "statement: declarations/block/empty/expression/control-flow/module",
        "statement: for/for-in/for-of/for-await/while/do/switch/try/labels/with/debugger",
        "expression: literals/identifier/this/super/meta/array/object/function/arrow/class",
        "expression: unary/update/binary/logical/assignment/conditional/call/new/member",
        "expression: sequence/tagged-template/spread/await/yield/dynamic-import",
        "pattern: identifier/array/object/assignment/rest/elision/computed/nested",
        "class: constructor/method/get/set/field/private/computed/static-block/decorator/accessor",
        "module: side-effect/default/named/namespace imports and every export kind/attributes",
        "lowering: TypeScript values/types/import-equals/export-assignment/using",
        "lowering: JSX elements/fragments/members/spreads/children and TSX generics/type arguments",
    ];
    assert_eq!(covered_families.len(), 10);
    assert_eq!(FIXTURES.len(), 14);
}

#[test]
fn matrix_covers_every_ast_enum_variant_and_operator() {
    let coverage = collect_ast_coverage();
    assert_eq!(
        coverage.statements,
        expected([
            "variable-declaration",
            "function-declaration",
            "class-declaration",
            "block",
            "empty",
            "expression",
            "if",
            "for",
            "for-in",
            "for-of",
            "for-await-of",
            "while",
            "do-while",
            "switch",
            "return",
            "break",
            "continue",
            "throw",
            "try",
            "labeled",
            "with",
            "debugger",
            "import",
            "export-named",
            "export-default",
            "export-all",
        ])
    );
    assert_eq!(
        coverage.expressions,
        expected([
            "number-literal",
            "string-literal",
            "boolean-literal",
            "null-literal",
            "bigint-literal",
            "regexp-literal",
            "template-literal",
            "identifier",
            "this",
            "super",
            "meta-property",
            "array",
            "object",
            "function",
            "arrow",
            "class",
            "unary",
            "update",
            "binary",
            "logical",
            "assignment",
            "conditional",
            "call",
            "new",
            "member",
            "sequence",
            "tagged-template",
            "spread",
            "await",
            "yield",
            "dynamic-import",
        ])
    );
    assert_eq!(
        coverage.patterns,
        expected(["identifier", "array", "object", "assignment", "rest"])
    );
    assert_eq!(
        coverage.variable_kinds,
        expected(["var", "let", "const", "using", "await-using"])
    );
    assert_eq!(
        coverage.for_initializers,
        expected(["variable", "expression"])
    );
    assert_eq!(coverage.for_lefts, expected(["variable", "target"]));
    assert_eq!(
        coverage.unary_operators,
        expected([
            "minus",
            "plus",
            "logical-not",
            "bitwise-not",
            "typeof",
            "void",
            "delete",
        ])
    );
    assert_eq!(
        coverage.update_operators,
        expected(["increment", "decrement"])
    );
    assert_eq!(
        coverage.binary_operators,
        expected([
            "add",
            "sub",
            "mul",
            "div",
            "rem",
            "exp",
            "bit-and",
            "bit-or",
            "bit-xor",
            "shl",
            "shr",
            "ushr",
            "eq",
            "not-eq",
            "strict-eq",
            "strict-not-eq",
            "lt",
            "gt",
            "lt-eq",
            "gt-eq",
            "in",
            "instanceof",
        ])
    );
    assert_eq!(
        coverage.logical_operators,
        expected(["and", "or", "coalesce"])
    );
    assert_eq!(
        coverage.assignment_operators,
        expected([
            "assign", "add", "sub", "mul", "div", "rem", "exp", "shl", "shr", "ushr", "bit-and",
            "bit-or", "bit-xor", "and", "or", "coalesce",
        ])
    );
    assert_eq!(
        coverage.member_properties,
        expected(["identifier", "computed", "private"])
    );
    assert_eq!(
        coverage.property_keys,
        expected(["identifier", "string", "number", "computed", "private"])
    );
    assert_eq!(coverage.property_kinds, expected(["init", "get", "set"]));
    assert_eq!(coverage.object_members, expected(["property", "spread"]));
    assert_eq!(coverage.arrow_bodies, expected(["block", "expression"]));
    assert_eq!(
        coverage.class_members,
        expected(["method", "property", "static-block"])
    );
    assert_eq!(
        coverage.method_kinds,
        expected(["constructor", "method", "get", "set"])
    );
    assert_eq!(
        coverage.import_specifiers,
        expected(["named", "default", "namespace"])
    );
    assert_eq!(
        coverage.export_default_kinds,
        expected(["function", "class", "expression"])
    );
    assert_eq!(
        coverage.module_export_names,
        expected(["identifier", "string"])
    );
    assert_eq!(coverage.attribute_keywords, expected(["with", "assert"]));
}
