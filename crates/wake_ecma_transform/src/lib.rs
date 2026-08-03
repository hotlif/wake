//! wake_ecma_transform — ECMAScript 目标环境与转换计划。
//!
//! Browserslist 的查询和配置发现属于边缘层；本 crate 只接收稳定、已排序的浏览器目标，
//! 再把它们编译成与具体 pass 对应的 [`FeatureSet`]。这样编译核心不依赖 browserslist
//! 数据库，目标集合和兼容矩阵版本也可直接进入缓存键。

use std::{cmp::Ordering, fmt};

use bumpalo::Bump;
use wake_common::{Atom, Interner, Span};
use wake_ecma_ast::{
    AVec, ArrayExpression, ArrowBody, ArrowFunction, AssignmentExpression, AssignmentOperator,
    BinaryExpression, BinaryOperator, BlockStatement, BreakStatement, CallExpression, CatchClause,
    ConditionalExpression, Expression, ExpressionStatement, ForLeft, ForOfStatement, ForStatement,
    Function, FunctionBody, Ident, IfStatement, LabeledStatement, LogicalExpression,
    LogicalOperator, MemberExpression, MemberProperty, NewExpression, ObjectExpression,
    ObjectMember, ObjectPattern, ObjectProperty, Pattern, PropertyKey, RestElement,
    ReturnStatement, SequenceExpression, Statement, StringLiteral, TemplateLiteral, TryStatement,
    UnaryExpression, UnaryOperator, VarKind, VariableDeclaration, VariableDeclarator, Visit,
};

/// 兼容矩阵版本。任何最低版本或判定规则变化都必须递增。
pub const COMPAT_DATA_VERSION: u32 = 3;

/// Wake 零配置和直接 API 使用的现代浏览器基线。
///
/// 边缘层仍可传入更旧或更广的 Browserslist 结果；这组最低版本只定义默认行为。
pub const MODERN_BROWSER_BASELINE: [(&str, &str); 5] = [
    ("chrome", "120"),
    ("edge", "120"),
    ("firefox", "121"),
    ("safari", "17.2"),
    ("ios", "17.2"),
];

/// 一个规范化 Browserslist 结果。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BrowserTarget {
    pub name: String,
    pub version: String,
}

impl BrowserTarget {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        let name = name.into().trim().to_ascii_lowercase();
        Self {
            name: match name.as_str() {
                "ios" | "ios_saf" | "ios-saf" | "ios safari" | "ios_safari" => "ios".to_string(),
                _ => name,
            },
            version: version.into().trim().to_ascii_lowercase(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct BrowserVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl BrowserVersion {
    const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parse the conservative lower endpoint returned by Browserslist.
    ///
    /// Browserslist can return ranges such as `15.2-15.3`; feature support must be valid for the
    /// oldest version in that range. Moving labels such as Safari Technology Preview remain
    /// unknown and therefore take the conservative compatibility path.
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let lower = raw.split('-').next()?;
        let mut components = lower.split('.');
        let major = components.next()?.parse().ok()?;
        let minor = components
            .next()
            .map(str::parse)
            .transpose()
            .ok()?
            .unwrap_or(0);
        let patch = components
            .next()
            .map(str::parse)
            .transpose()
            .ok()?
            .unwrap_or(0);
        if components.next().is_some() {
            return None;
        }
        Some(Self::new(major, minor, patch))
    }
}

/// 由目标浏览器决定的独立语法转换。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EcmaFeature {
    ArrowFunction = 0,
    TemplateLiteral = 1,
    ShorthandProperties = 2,
    FunctionParameters = 3,
    ExponentiationOperator = 4,
    AsyncAwait = 5,
    ObjectRestSpread = 6,
    OptionalCatchBinding = 7,
    OptionalChaining = 8,
    NullishCoalescing = 9,
    LogicalAssignment = 10,
    ClassFields = 11,
    PrivateFields = 12,
    ClassStaticBlock = 13,
    Spread = 14,
    Destructuring = 15,
    ForOf = 16,
}

impl EcmaFeature {
    pub const ALL: [Self; 17] = [
        Self::ArrowFunction,
        Self::TemplateLiteral,
        Self::ShorthandProperties,
        Self::FunctionParameters,
        Self::ExponentiationOperator,
        Self::AsyncAwait,
        Self::ObjectRestSpread,
        Self::OptionalCatchBinding,
        Self::OptionalChaining,
        Self::NullishCoalescing,
        Self::LogicalAssignment,
        Self::ClassFields,
        Self::PrivateFields,
        Self::ClassStaticBlock,
        Self::Spread,
        Self::Destructuring,
        Self::ForOf,
    ];

    pub const fn babel_plugin(self) -> &'static str {
        match self {
            Self::ArrowFunction => "transform-arrow-functions",
            Self::TemplateLiteral => "transform-template-literals",
            Self::ShorthandProperties => "transform-shorthand-properties",
            Self::FunctionParameters => "transform-parameters",
            Self::ExponentiationOperator => "transform-exponentiation-operator",
            Self::AsyncAwait => "transform-async-to-generator",
            Self::ObjectRestSpread => "transform-object-rest-spread",
            Self::OptionalCatchBinding => "transform-optional-catch-binding",
            Self::OptionalChaining => "transform-optional-chaining",
            Self::NullishCoalescing => "transform-nullish-coalescing-operator",
            Self::LogicalAssignment => "transform-logical-assignment-operators",
            Self::ClassFields => "transform-class-properties",
            Self::PrivateFields => "transform-private-methods",
            Self::ClassStaticBlock => "transform-class-static-block",
            Self::Spread => "transform-spread",
            Self::Destructuring => "transform-destructuring",
            Self::ForOf => "transform-for-of",
        }
    }

    pub fn from_babel_plugin(name: &str) -> Option<Self> {
        let name = name
            .strip_prefix("@babel/plugin-")
            .or_else(|| name.strip_prefix("plugin-"))
            .unwrap_or(name);
        Self::ALL
            .into_iter()
            .find(|feature| feature.babel_plugin() == name)
    }
}

/// Whether a parsed synchronous `for...of` can be lowered by [`lower_for_of`].
///
/// `for await...of` needs an async-iterator state machine and `using` heads need resource
/// disposal. Both remain native until their dedicated passes exist. Invalid multi-declarator or
/// initialized heads are also retained so this pass never turns a parser diagnostic into
/// executable-but-different code.
pub fn for_of_needs_lowering(features: FeatureSet, statement: &ForOfStatement<'_>) -> bool {
    if !features.contains(EcmaFeature::ForOf) || statement.is_await {
        return false;
    }
    match statement.left {
        ForLeft::Variable(declaration) => {
            !declaration.kind.is_using()
                && declaration.declarations.len() == 1
                && declaration.declarations[0].init.is_none()
        }
        ForLeft::Target(_) => true,
    }
}

/// Lower a synchronous `for...of` to a state-helper driven `try/catch/finally` loop.
///
/// The emitted shape deliberately mirrors the iterator protocol rather than materializing the
/// iterable as an array. Abrupt completion from `break`, `return`, a labeled `continue`, binding
/// initialization, or the body therefore reaches `IteratorClose`; failures from `next()` itself
/// do not. Direct source labels are placed on the generated inner `for`, preserving
/// `continue label`.
///
/// A lexical loop head has a separate, permanently-uninitialized environment while its RHS and
/// `GetIterator` run. `tdz_pattern` describes those original BoundNames (before destructuring
/// lowering). The generated labeled block jumps over a DUMMY-span `let` declaration, so closures
/// created by the RHS retain that uninitialized environment even after the loop finishes.
#[allow(clippy::too_many_arguments)]
pub fn lower_for_of<'a>(
    arena: &'a Bump,
    interner: &Interner,
    helper: Atom,
    state_atom: Atom,
    error_atom: Atom,
    tdz_label_atom: Atom,
    features: FeatureSet,
    labels: &[Ident],
    tdz_pattern: Option<Pattern<'a>>,
    statement: &'a ForOfStatement<'a>,
) -> Statement<'a> {
    if !for_of_needs_lowering(features, statement) {
        return wrap_for_of_labels(arena, Statement::ForOf(statement), labels, statement.span);
    }

    let span = Span::DUMMY;
    let state = || Expression::Identifier(arena.alloc(Ident::new(span, state_atom)));

    let helper_call = {
        let mut arguments = AVec::with_capacity_in(1, arena);
        arguments.push(statement.right);
        Expression::Call(arena.alloc(CallExpression {
            span,
            callee: Expression::Identifier(arena.alloc(Ident::new(span, helper))),
            arguments,
            optional: false,
        }))
    };
    let mut state_declarators = AVec::with_capacity_in(1, arena);
    state_declarators.push(VariableDeclarator {
        span,
        id: Pattern::Ident(arena.alloc(Ident::new(span, state_atom))),
        init: Some(helper_call),
    });
    let state_declaration = Statement::VariableDeclaration(arena.alloc(VariableDeclaration {
        span,
        kind: VarKind::Var,
        declarations: state_declarators,
    }));

    let start = for_of_method_call(arena, interner, state(), "s", AVec::new_in(arena));
    let start = Statement::Expression(arena.alloc(ExpressionStatement {
        span,
        expression: start,
    }));

    let next = for_of_method_call(arena, interner, state(), "n", AVec::new_in(arena));
    let test = Expression::Unary(arena.alloc(UnaryExpression {
        span,
        operator: UnaryOperator::LogicalNot,
        argument: next,
    }));

    let value = member(arena, interner, span, state(), "v");
    let (var_head_declaration, initialization) = match statement.left {
        ForLeft::Variable(declaration) => {
            let declarator = &declaration.declarations[0];
            if declaration.kind == VarKind::Var
                && let Pattern::Ident(ident) = declarator.id
            {
                // A var-head performs an assignment on every iteration; spelling it as another
                // initialized declaration can fool constant propagation when the same var had an
                // earlier literal initializer. Keep the hoisted declaration and the write as
                // distinct AST operations.
                let mut declarations = AVec::with_capacity_in(1, arena);
                declarations.push(VariableDeclarator {
                    span: declarator.span,
                    id: declarator.id,
                    init: None,
                });
                let declaration =
                    Statement::VariableDeclaration(arena.alloc(VariableDeclaration {
                        span: declaration.span,
                        kind: VarKind::Var,
                        declarations,
                    }));
                let assignment = Statement::Expression(arena.alloc(ExpressionStatement {
                    span,
                    expression: Expression::Assignment(arena.alloc(AssignmentExpression {
                        span,
                        operator: AssignmentOperator::Assign,
                        left: Expression::Identifier(
                            arena.alloc(Ident::new(Span::DUMMY, ident.name)),
                        ),
                        right: value,
                    })),
                }));
                (Some(declaration), assignment)
            } else {
                let mut declarations = AVec::with_capacity_in(1, arena);
                declarations.push(VariableDeclarator {
                    span: declarator.span,
                    id: declarator.id,
                    init: Some(value),
                });
                (
                    None,
                    Statement::VariableDeclaration(arena.alloc(VariableDeclaration {
                        span: declaration.span,
                        kind: declaration.kind,
                        declarations,
                    })),
                )
            }
        }
        ForLeft::Target(target) => (
            None,
            Statement::Expression(arena.alloc(ExpressionStatement {
                span: target.span(),
                expression: Expression::Assignment(arena.alloc(AssignmentExpression {
                    span: target.span(),
                    operator: AssignmentOperator::Assign,
                    left: target,
                    right: value,
                })),
            })),
        ),
    };
    let mut iteration_body = AVec::with_capacity_in(2, arena);
    iteration_body.push(initialization);
    // Keep the source body nested. Its lexical environment begins only after loop-head binding
    // initialization, which matters for defaults that share a name with a body-local binding.
    iteration_body.push(statement.body);
    let iteration_body = Statement::Block(arena.alloc(BlockStatement {
        span,
        body: iteration_body,
    }));
    let loop_statement = Statement::For(arena.alloc(ForStatement {
        span,
        init: None,
        test: Some(test),
        update: None,
        body: iteration_body,
    }));
    let loop_statement = wrap_for_of_labels(arena, loop_statement, labels, statement.span);

    let mut try_body = AVec::with_capacity_in(2, arena);
    try_body.push(start);
    try_body.push(loop_statement);
    let try_block = arena.alloc(BlockStatement {
        span,
        body: try_body,
    });

    let error_ident = arena.alloc(Ident::new(span, error_atom));
    let mut error_arguments = AVec::with_capacity_in(1, arena);
    error_arguments.push(Expression::Identifier(error_ident));
    let save_error = for_of_method_call(arena, interner, state(), "e", error_arguments);
    let mut catch_body = AVec::with_capacity_in(1, arena);
    catch_body.push(Statement::Expression(arena.alloc(ExpressionStatement {
        span,
        expression: save_error,
    })));
    let catch_body = arena.alloc(BlockStatement {
        span,
        body: catch_body,
    });
    let catch = arena.alloc(CatchClause {
        span,
        param: Some(Pattern::Ident(error_ident)),
        body: catch_body,
    });

    let close = for_of_method_call(arena, interner, state(), "f", AVec::new_in(arena));
    let mut finally_body = AVec::with_capacity_in(1, arena);
    finally_body.push(Statement::Expression(arena.alloc(ExpressionStatement {
        span,
        expression: close,
    })));
    let finally_block = arena.alloc(BlockStatement {
        span,
        body: finally_body,
    });
    let guarded_loop = Statement::Try(arena.alloc(TryStatement {
        span,
        block: try_block,
        handler: Some(catch),
        finalizer: Some(finally_block),
    }));

    let mut outer_body = AVec::with_capacity_in(5, arena);
    if let Some(declaration) = var_head_declaration {
        outer_body.push(declaration);
    }
    outer_body.push(state_declaration);
    outer_body.push(guarded_loop);

    let mut tdz_declarators = AVec::new_in(arena);
    if let Some(pattern) = tdz_pattern {
        collect_for_of_tdz_declarators(arena, pattern, &mut tdz_declarators);
    }
    if tdz_declarators.is_empty() {
        return Statement::Block(arena.alloc(BlockStatement {
            span: statement.span,
            body: outer_body,
        }));
    }

    let tdz_label = Ident::new(span, tdz_label_atom);
    outer_body.push(Statement::Break(arena.alloc(BreakStatement {
        span,
        label: Some(tdz_label),
    })));
    // This statement is deliberately unreachable. Its lexical declaration is nevertheless
    // instantiated when the labeled block is entered, so BoundNames stay in TDZ forever.
    outer_body.push(Statement::VariableDeclaration(arena.alloc(
        VariableDeclaration {
            span,
            kind: VarKind::Let,
            declarations: tdz_declarators,
        },
    )));
    let body = Statement::Block(arena.alloc(BlockStatement {
        span: statement.span,
        body: outer_body,
    }));
    Statement::Labeled(arena.alloc(LabeledStatement {
        span: statement.span,
        label: tdz_label,
        body,
    }))
}

fn for_of_method_call<'a>(
    arena: &'a Bump,
    interner: &Interner,
    object: Expression<'a>,
    method_name: &str,
    arguments: AVec<'a, Expression<'a>>,
) -> Expression<'a> {
    let span = Span::DUMMY;
    Expression::Call(arena.alloc(CallExpression {
        span,
        callee: member(arena, interner, span, object, method_name),
        arguments,
        optional: false,
    }))
}

fn wrap_for_of_labels<'a>(
    arena: &'a Bump,
    mut body: Statement<'a>,
    labels: &[Ident],
    fallback_span: Span,
) -> Statement<'a> {
    for label in labels.iter().rev() {
        let body_span = body.span();
        let hi = if body_span.is_dummy() {
            fallback_span.hi
        } else {
            body_span.hi
        };
        body = Statement::Labeled(arena.alloc(LabeledStatement {
            span: Span::new(label.span.lo, hi),
            label: *label,
            body,
        }));
    }
    body
}

fn collect_for_of_tdz_declarators<'a>(
    arena: &'a Bump,
    pattern: Pattern<'a>,
    declarations: &mut AVec<'a, VariableDeclarator<'a>>,
) {
    match pattern {
        Pattern::Ident(ident) => declarations.push(VariableDeclarator {
            span: Span::DUMMY,
            id: Pattern::Ident(arena.alloc(Ident::new(Span::DUMMY, ident.name))),
            init: None,
        }),
        Pattern::Array(array) => {
            for element in array.elements.iter().flatten() {
                collect_for_of_tdz_declarators(arena, *element, declarations);
            }
        }
        Pattern::Object(object) => {
            for property in object.properties.iter() {
                collect_for_of_tdz_declarators(arena, property.value, declarations);
            }
            if let Some(rest) = object.rest {
                collect_for_of_tdz_declarators(arena, rest.argument, declarations);
            }
        }
        Pattern::Assignment(assignment) => {
            collect_for_of_tdz_declarators(arena, assignment.left, declarations);
        }
        Pattern::Rest(rest) => {
            collect_for_of_tdz_declarators(arena, rest.argument, declarations);
        }
    }
}

pub fn lower_object_spread<'a>(
    arena: &'a Bump,
    interner: &Interner,
    helper: Atom,
    features: FeatureSet,
    object: &'a ObjectExpression<'a>,
) -> Expression<'a> {
    if !features.contains(EcmaFeature::ObjectRestSpread)
        || !object
            .properties
            .iter()
            .any(|member| matches!(member, ObjectMember::Spread(_)))
    {
        return Expression::Object(object);
    }
    let define = interner.intern("define");
    let proto = interner.intern("proto");
    let mut target = Expression::Object(arena.alloc(ObjectExpression {
        span: object.span,
        properties: AVec::new_in(arena),
    }));
    for member in object.properties.iter() {
        let (method, argument) = match member {
            ObjectMember::Spread(spread) => (None, spread.argument),
            ObjectMember::Property(property) if property.prototype_setter => {
                (Some(proto), property.value)
            }
            ObjectMember::Property(property) => {
                // Build each explicit definition as its own source object.  Besides preserving
                // computed-key/value evaluation order, this lets `.define` apply getter and setter
                // definitions one at a time, just like PropertyDefinitionEvaluation does.
                //
                // Shorthand/method lowering may already have rendered an ordinary `__proto__`
                // definition as `__proto__: value`.  Re-emit that key as computed so the temporary
                // source object cannot accidentally treat it as the prototype-setter production.
                let property: &'a ObjectProperty<'a> = if !property.prototype_setter
                    && !property.computed
                    && match property.key {
                        PropertyKey::Ident(ident) => {
                            interner.with_resolved(ident.name, |name| name == "__proto__")
                        }
                        PropertyKey::String(string) => {
                            interner.with_resolved(string.value, |name| name == "__proto__")
                        }
                        _ => false,
                    } {
                    let (span, value) = match property.key {
                        PropertyKey::Ident(ident) => (ident.span, ident.name),
                        PropertyKey::String(string) => (string.span, string.value),
                        _ => unreachable!("ordinary __proto__ key was matched above"),
                    };
                    let key = Expression::StringLiteral(arena.alloc(StringLiteral { span, value }));
                    arena.alloc(ObjectProperty {
                        span: property.span,
                        key: PropertyKey::Computed(key),
                        value: property.value,
                        kind: property.kind,
                        method: property.method,
                        shorthand: false,
                        computed: true,
                        prototype_setter: false,
                    })
                } else {
                    property
                };
                let mut properties = AVec::with_capacity_in(1, arena);
                properties.push(ObjectMember::Property(property));
                (
                    Some(define),
                    Expression::Object(arena.alloc(ObjectExpression {
                        span: property.span,
                        properties,
                    })),
                )
            }
        };
        let helper_ident = Expression::Identifier(arena.alloc(Ident::new(object.span, helper)));
        let callee = if let Some(method) = method {
            Expression::Member(arena.alloc(MemberExpression {
                span: object.span,
                object: helper_ident,
                property: MemberProperty::Ident(Ident::new(Span::DUMMY, method)),
                optional: false,
            }))
        } else {
            helper_ident
        };
        let mut arguments = AVec::with_capacity_in(2, arena);
        arguments.push(target);
        arguments.push(argument);
        target = Expression::Call(arena.alloc(CallExpression {
            span: object.span,
            callee,
            arguments,
            optional: false,
        }));
    }
    target
}

/// Number of collision-free temporary names needed to lower one variable declarator.
pub fn destructuring_temporary_count(pattern: Pattern<'_>) -> usize {
    match pattern {
        Pattern::Ident(_) => 0,
        Pattern::Rest(rest) => destructuring_temporary_count(rest.argument),
        Pattern::Assignment(assignment) => 1 + destructuring_temporary_count(assignment.left),
        Pattern::Array(array) => {
            1 + array
                .elements
                .iter()
                .flatten()
                .map(|pattern| destructuring_temporary_count(*pattern))
                .sum::<usize>()
        }
        Pattern::Object(object) => {
            2 + object
                .properties
                .iter()
                .map(|property| destructuring_temporary_count(property.value))
                .sum::<usize>()
                + if object.rest.is_some() {
                    2 * object
                        .properties
                        .iter()
                        .filter(|property| matches!(property.key, PropertyKey::Computed(_)))
                        .count()
                } else {
                    0
                }
        }
    }
}

pub fn pattern_has_object_rest(pattern: Pattern<'_>) -> bool {
    match pattern {
        Pattern::Ident(_) => false,
        Pattern::Rest(rest) => pattern_has_object_rest(rest.argument),
        Pattern::Assignment(assignment) => pattern_has_object_rest(assignment.left),
        Pattern::Array(array) => array
            .elements
            .iter()
            .flatten()
            .any(|pattern| pattern_has_object_rest(*pattern)),
        Pattern::Object(object) => {
            object.rest.is_some()
                || object
                    .properties
                    .iter()
                    .any(|property| pattern_has_object_rest(property.value))
        }
    }
}

/// Whether a binding pattern must be lowered for the selected feature set.
///
/// `ObjectRestSpread` is intentionally considered independently from `Destructuring`: targets
/// such as Chrome 55 support ordinary destructuring while still requiring object-rest lowering.
pub fn binding_pattern_needs_lowering(features: FeatureSet, pattern: Pattern<'_>) -> bool {
    match pattern {
        Pattern::Ident(_) => false,
        Pattern::Rest(rest) => binding_pattern_needs_lowering(features, rest.argument),
        Pattern::Assignment(assignment) => {
            binding_pattern_needs_lowering(features, assignment.left)
        }
        Pattern::Array(_) | Pattern::Object(_) => {
            features.contains(EcmaFeature::Destructuring)
                || (features.contains(EcmaFeature::ObjectRestSpread)
                    && pattern_has_object_rest(pattern))
        }
    }
}

pub fn complex_parameter_temporary_count(params: &[Pattern<'_>]) -> usize {
    params
        .iter()
        .copied()
        .map(|param| match param {
            Pattern::Array(_) | Pattern::Object(_) => 1 + destructuring_temporary_count(param),
            Pattern::Assignment(assignment)
                if matches!(assignment.left, Pattern::Array(_) | Pattern::Object(_)) =>
            {
                1 + destructuring_temporary_count(assignment.left)
            }
            Pattern::Rest(rest) if !matches!(rest.argument, Pattern::Ident(_)) => {
                1 + destructuring_temporary_count(rest.argument)
            }
            _ => 0,
        })
        .sum()
}

/// Exact number of collision-free names consumed when lowering parameters for `features`.
pub fn complex_parameter_temporary_count_for_features(
    features: FeatureSet,
    params: &[Pattern<'_>],
) -> usize {
    params
        .iter()
        .copied()
        .map(|param| match param {
            Pattern::Array(_) | Pattern::Object(_)
                if binding_pattern_needs_lowering(features, param) =>
            {
                1 + destructuring_temporary_count(param)
            }
            Pattern::Assignment(assignment)
                if matches!(assignment.left, Pattern::Array(_) | Pattern::Object(_))
                    && (features.contains(EcmaFeature::FunctionParameters)
                        || binding_pattern_needs_lowering(features, assignment.left)) =>
            {
                1 + destructuring_temporary_count(assignment.left)
            }
            Pattern::Rest(rest)
                if !matches!(rest.argument, Pattern::Ident(_))
                    && features.contains(EcmaFeature::FunctionParameters) =>
            {
                destructuring_temporary_count(rest.argument)
            }
            Pattern::Rest(rest)
                if !matches!(rest.argument, Pattern::Ident(_))
                    && binding_pattern_needs_lowering(features, rest.argument) =>
            {
                1 + destructuring_temporary_count(rest.argument)
            }
            _ => 0,
        })
        .sum()
}

fn pattern_contains_array(pattern: Pattern<'_>) -> bool {
    match pattern {
        Pattern::Ident(_) => false,
        Pattern::Rest(rest) => pattern_contains_array(rest.argument),
        Pattern::Assignment(assignment) => pattern_contains_array(assignment.left),
        Pattern::Array(_) => true,
        Pattern::Object(object) => {
            object
                .properties
                .iter()
                .any(|property| pattern_contains_array(property.value))
                || object
                    .rest
                    .is_some_and(|rest| pattern_contains_array(rest.argument))
        }
    }
}

/// Whether parameter lowering will materialize an array binding through the iterator helper.
pub fn complex_parameters_need_iterator_helper(
    features: FeatureSet,
    params: &[Pattern<'_>],
) -> bool {
    params.iter().copied().any(|param| match param {
        Pattern::Array(_) | Pattern::Object(_)
            if binding_pattern_needs_lowering(features, param) =>
        {
            pattern_contains_array(param)
        }
        Pattern::Assignment(assignment)
            if matches!(assignment.left, Pattern::Array(_) | Pattern::Object(_))
                && (features.contains(EcmaFeature::FunctionParameters)
                    || binding_pattern_needs_lowering(features, assignment.left)) =>
        {
            pattern_contains_array(assignment.left)
        }
        Pattern::Rest(rest)
            if !matches!(rest.argument, Pattern::Ident(_))
                && (features.contains(EcmaFeature::FunctionParameters)
                    || binding_pattern_needs_lowering(features, rest.argument)) =>
        {
            pattern_contains_array(rest.argument)
        }
        _ => false,
    })
}

/// Whether moving unsupported parameter syntax into `body` would change identifier resolution.
///
/// A function with a non-simple parameter list evaluates defaults and computed binding keys in a
/// ParameterEnvironment outside the function body's lexical/variable environments. Injecting the
/// lowered expressions at the start of the body can therefore make a body-local `let`, `const`,
/// `class`, `var` or function declaration shadow the name that the source parameter expression
/// resolved to. Keep the source parameter list in that case until parameter environments can be
/// represented explicitly by the lowering pipeline.
///
/// Only expressions belonging to parameters that this feature set would actually move are
/// inspected. This keeps an unrelated native default from blocking object-rest-only lowering.
pub fn parameter_lowering_has_body_binding_conflict(
    features: FeatureSet,
    params: &[Pattern<'_>],
    body: &FunctionBody<'_>,
) -> bool {
    let mut body_bindings = Vec::new();

    // These declarations are scoped to the function body block and are visible (or in TDZ) at
    // every injected initializer position. Nested lexical declarations do not shadow a body-top
    // initializer, so collect only direct body statements here.
    for statement in body.statements.iter() {
        match statement {
            Statement::VariableDeclaration(declaration)
                if !matches!(declaration.kind, VarKind::Var) =>
            {
                for declarator in declaration.declarations.iter() {
                    collect_binding_atoms(declarator.id, &mut body_bindings);
                }
            }
            Statement::ClassDeclaration(class) => {
                if let Some(id) = class.id {
                    body_bindings.push(id.name);
                }
            }
            _ => {}
        }
    }

    // `var` and function declarations are function-scoped even when nested in a statement. They
    // are not visible while source parameters run, but become visible to body-injected code.
    let mut collector = BodyVarBindingCollector {
        bindings: &mut body_bindings,
    };
    for statement in body.statements.iter() {
        collector.visit_statement(statement);
    }

    if body_bindings.is_empty() {
        return false;
    }

    let mut references = LoweredParameterReferenceVisitor {
        body_bindings: &body_bindings,
        conflict: false,
    };
    for parameter in params.iter().copied() {
        visit_moved_parameter_expressions(&mut references, features, parameter);
        if references.conflict {
            return true;
        }
    }
    false
}

fn collect_binding_atoms(pattern: Pattern<'_>, output: &mut Vec<Atom>) {
    match pattern {
        Pattern::Ident(ident) => output.push(ident.name),
        Pattern::Rest(rest) => collect_binding_atoms(rest.argument, output),
        Pattern::Assignment(assignment) => collect_binding_atoms(assignment.left, output),
        Pattern::Array(array) => {
            for element in array.elements.iter().flatten().copied() {
                collect_binding_atoms(element, output);
            }
        }
        Pattern::Object(object) => {
            for property in object.properties.iter() {
                collect_binding_atoms(property.value, output);
            }
            if let Some(rest) = object.rest {
                collect_binding_atoms(rest.argument, output);
            }
        }
    }
}

struct BodyVarBindingCollector<'b> {
    bindings: &'b mut Vec<Atom>,
}

impl<'a> Visit<'a> for BodyVarBindingCollector<'_> {
    fn visit_statement(&mut self, node: &Statement<'a>) {
        match node {
            Statement::VariableDeclaration(declaration) => {
                if declaration.kind == VarKind::Var {
                    for declarator in declaration.declarations.iter() {
                        collect_binding_atoms(declarator.id, self.bindings);
                    }
                }
            }
            Statement::FunctionDeclaration(function) => {
                if let Some(id) = function.id {
                    self.bindings.push(id.name);
                }
            }
            Statement::ClassDeclaration(_) => {}
            _ => wake_ecma_ast::walk_statement(self, node),
        }
    }

    // Do not attribute declarations owned by nested execution scopes to the outer body.
    fn visit_expression(&mut self, node: &Expression<'a>) {
        if !matches!(node, Expression::Arrow(_)) {
            wake_ecma_ast::walk_expression(self, node);
        }
    }

    fn visit_function(&mut self, _node: &Function<'a>) {}

    fn visit_class(&mut self, _node: &wake_ecma_ast::Class<'a>) {}
}

struct LoweredParameterReferenceVisitor<'b> {
    body_bindings: &'b [Atom],
    conflict: bool,
}

impl<'a> Visit<'a> for LoweredParameterReferenceVisitor<'_> {
    fn visit_ident(&mut self, node: &Ident) {
        if self.body_bindings.contains(&node.name) {
            self.conflict = true;
        }
    }

    /// Binding identifiers declare names; only defaults and computed keys are reads performed
    /// while materializing a pattern.
    fn visit_pattern(&mut self, node: &Pattern<'a>) {
        if self.conflict {
            return;
        }
        match node {
            Pattern::Ident(_) => {}
            Pattern::Rest(rest) => self.visit_pattern(&rest.argument),
            Pattern::Assignment(assignment) => {
                self.visit_pattern(&assignment.left);
                self.visit_expression(&assignment.right);
            }
            Pattern::Array(array) => {
                for element in array.elements.iter().flatten() {
                    self.visit_pattern(element);
                }
            }
            Pattern::Object(object) => {
                for property in object.properties.iter() {
                    if let PropertyKey::Computed(key) = property.key {
                        self.visit_expression(&key);
                    }
                    self.visit_pattern(&property.value);
                }
                if let Some(rest) = object.rest {
                    self.visit_pattern(&rest.argument);
                }
            }
        }
    }
}

fn visit_moved_parameter_expressions<'a>(
    visitor: &mut LoweredParameterReferenceVisitor<'_>,
    features: FeatureSet,
    parameter: Pattern<'a>,
) {
    if features.contains(EcmaFeature::FunctionParameters) {
        match parameter {
            Pattern::Assignment(assignment) if matches!(assignment.left, Pattern::Ident(_)) => {
                visitor.visit_expression(&assignment.right);
                return;
            }
            Pattern::Rest(rest) if matches!(rest.argument, Pattern::Ident(_)) => return,
            _ => {}
        }
    }

    if let Pattern::Rest(rest) = parameter
        && !matches!(rest.argument, Pattern::Ident(_))
        && (features.contains(EcmaFeature::FunctionParameters)
            || binding_pattern_needs_lowering(features, rest.argument))
    {
        visitor.visit_pattern(&rest.argument);
        return;
    }

    match parameter {
        Pattern::Array(_) | Pattern::Object(_)
            if binding_pattern_needs_lowering(features, parameter) =>
        {
            visitor.visit_pattern(&parameter);
        }
        Pattern::Assignment(assignment)
            if matches!(assignment.left, Pattern::Array(_) | Pattern::Object(_))
                && (features.contains(EcmaFeature::FunctionParameters)
                    || binding_pattern_needs_lowering(features, assignment.left)) =>
        {
            visitor.visit_pattern(&assignment.left);
            visitor.visit_expression(&assignment.right);
        }
        _ => {}
    }
}

/// Lower structured function parameters to temporary identifiers plus body-local `var`
/// destructuring declarations.
pub fn lower_complex_parameters<'a>(
    arena: &'a Bump,
    interner: &Interner,
    iterator_helper: Atom,
    object_helper: Atom,
    features: FeatureSet,
    function: &'a Function<'a>,
    temporaries: &[Atom],
) -> &'a Function<'a> {
    let has_binding_parameter_to_lower = function
        .params
        .iter()
        .copied()
        .any(|param| binding_pattern_needs_lowering(features, param));
    if function.body.is_none()
        || (!features.contains(EcmaFeature::Destructuring)
            && !features.contains(EcmaFeature::FunctionParameters)
            && !has_binding_parameter_to_lower)
    {
        return function;
    }
    let mut temps = temporaries.iter().copied();
    let mut params = AVec::with_capacity_in(function.params.len(), arena);
    let mut initializers = AVec::new_in(arena);
    for (index, param) in function.params.iter().copied().enumerate() {
        if features.contains(EcmaFeature::FunctionParameters) {
            match param {
                Pattern::Assignment(assignment) if matches!(assignment.left, Pattern::Ident(_)) => {
                    let Pattern::Ident(ident) = assignment.left else {
                        unreachable!()
                    };
                    params.push(Pattern::Ident(ident));
                    initializers.push(default_parameter_statement(
                        arena,
                        assignment.span,
                        ident,
                        assignment.right,
                    ));
                    continue;
                }
                Pattern::Rest(rest) if matches!(rest.argument, Pattern::Ident(_)) => {
                    let Pattern::Ident(ident) = rest.argument else {
                        unreachable!()
                    };
                    initializers.push(rest_parameter_statement(
                        arena, interner, rest.span, ident, index,
                    ));
                    continue;
                }
                _ => {}
            }
        }
        if let Pattern::Rest(rest) = param
            && !matches!(rest.argument, Pattern::Ident(_))
            && (features.contains(EcmaFeature::FunctionParameters)
                || binding_pattern_needs_lowering(features, rest.argument))
        {
            let value = if features.contains(EcmaFeature::FunctionParameters) {
                rest_parameter_value(arena, interner, rest.span, index)
            } else {
                // The target supports rest parameters, but not some syntax within the binding
                // pattern. Keep native argument collection and move only the destructuring into
                // the body so arrow `this`/ordinary-function `arguments` semantics stay intact.
                let argument_atom = temps.next().expect("temporary count matches parameters");
                let argument_ident = arena.alloc(Ident::new(rest.span, argument_atom));
                params.push(Pattern::Rest(arena.alloc(RestElement {
                    span: rest.span,
                    argument: Pattern::Ident(argument_ident),
                })));
                Expression::Identifier(argument_ident)
            };
            let mut declarations = AVec::new_in(arena);
            lower_pattern_binding(
                arena,
                interner,
                iterator_helper,
                object_helper,
                VarKind::Var,
                rest.argument,
                value,
                &mut temps,
                &mut declarations,
            );
            initializers.push(Statement::VariableDeclaration(arena.alloc(
                VariableDeclaration {
                    span: rest.span,
                    kind: VarKind::Var,
                    declarations,
                },
            )));
            continue;
        }
        let (pattern, default) = match param {
            Pattern::Array(_) | Pattern::Object(_)
                if binding_pattern_needs_lowering(features, param) =>
            {
                (param, None)
            }
            Pattern::Assignment(assignment)
                if matches!(assignment.left, Pattern::Array(_) | Pattern::Object(_))
                    && (features.contains(EcmaFeature::FunctionParameters)
                        || binding_pattern_needs_lowering(features, assignment.left)) =>
            {
                (assignment.left, Some(assignment.right))
            }
            _ => {
                params.push(param);
                continue;
            }
        };
        let argument_atom = temps.next().expect("temporary count matches parameters");
        let argument_ident = arena.alloc(Ident::new(param.span(), argument_atom));
        params.push(Pattern::Ident(argument_ident));
        let argument = Expression::Identifier(argument_ident);
        let value = default.map_or(argument, |fallback| {
            undefined_fallback(arena, param.span(), argument, fallback)
        });
        let mut declarations = AVec::new_in(arena);
        lower_pattern_binding(
            arena,
            interner,
            iterator_helper,
            object_helper,
            VarKind::Var,
            pattern,
            value,
            &mut temps,
            &mut declarations,
        );
        initializers.push(Statement::VariableDeclaration(arena.alloc(
            VariableDeclaration {
                span: param.span(),
                kind: VarKind::Var,
                declarations,
            },
        )));
    }
    if initializers.is_empty() {
        return function;
    }
    let body = function.body.expect("checked above");
    let directive_count = body
        .statements
        .iter()
        .take_while(|statement| {
            matches!(
                statement,
                Statement::Expression(expression)
                    if matches!(expression.expression, Expression::StringLiteral(_))
            )
        })
        .count();
    let mut statements = AVec::with_capacity_in(body.statements.len() + initializers.len(), arena);
    statements.extend(body.statements[..directive_count].iter().copied());
    statements.extend(initializers);
    statements.extend(body.statements[directive_count..].iter().copied());
    let body = arena.alloc(FunctionBody {
        span: body.span,
        statements,
        strict: body.strict,
    });
    arena.alloc(Function {
        span: function.span,
        id: function.id,
        params,
        body: Some(body),
        is_async: function.is_async,
        is_generator: function.is_generator,
    })
}

/// Lower a variable declarator destructuring pattern into flat declarators. Structured values and
/// defaults are captured before reuse, preserving getter calls and RHS single-evaluation.
pub fn lower_variable_destructuring<'a>(
    arena: &'a Bump,
    interner: &Interner,
    iterator_helper: Atom,
    object_helper: Atom,
    features: FeatureSet,
    kind: VarKind,
    declarator: VariableDeclarator<'a>,
    temporaries: &[Atom],
) -> AVec<'a, VariableDeclarator<'a>> {
    let mut output = AVec::new_in(arena);
    if !binding_pattern_needs_lowering(features, declarator.id) || declarator.init.is_none() {
        output.push(declarator);
        return output;
    }
    let mut temps = temporaries.iter().copied();
    lower_pattern_binding(
        arena,
        interner,
        iterator_helper,
        object_helper,
        kind,
        declarator.id,
        declarator.init.expect("checked above"),
        &mut temps,
        &mut output,
    );
    output
}

fn lower_pattern_binding<'a>(
    arena: &'a Bump,
    interner: &Interner,
    iterator_helper: Atom,
    object_helper: Atom,
    kind: VarKind,
    pattern: Pattern<'a>,
    value: Expression<'a>,
    temps: &mut impl Iterator<Item = Atom>,
    output: &mut AVec<'a, VariableDeclarator<'a>>,
) {
    match pattern {
        Pattern::Ident(ident) => output.push(VariableDeclarator {
            span: ident.span,
            id: pattern,
            init: Some(value),
        }),
        Pattern::Rest(rest) => {
            lower_pattern_binding(
                arena,
                interner,
                iterator_helper,
                object_helper,
                kind,
                rest.argument,
                value,
                temps,
                output,
            );
        }
        Pattern::Assignment(assignment) => {
            let captured = capture_value(arena, assignment.span, kind, value, temps, output);
            let fallback = undefined_fallback(arena, assignment.span, captured, assignment.right);
            lower_pattern_binding(
                arena,
                interner,
                iterator_helper,
                object_helper,
                kind,
                assignment.left,
                fallback,
                temps,
                output,
            );
        }
        Pattern::Array(array) => {
            let limit = if array
                .elements
                .iter()
                .flatten()
                .any(|element| matches!(element, Pattern::Rest(_)))
            {
                None
            } else {
                Some(array.elements.len())
            };
            let captured = capture_value(
                arena,
                array.span,
                kind,
                to_array(arena, iterator_helper, array.span, value, limit),
                temps,
                output,
            );
            for (index, element) in array.elements.iter().copied().enumerate() {
                let Some(element) = element else { continue };
                let extracted = match element {
                    Pattern::Rest(rest) => {
                        let slice = member(arena, interner, rest.span, captured, "slice");
                        let mut arguments = AVec::with_capacity_in(1, arena);
                        arguments.push(Expression::NumberLiteral(arena.alloc(
                            wake_ecma_ast::NumberLiteral {
                                span: rest.span,
                                value: index as f64,
                            },
                        )));
                        let value = Expression::Call(arena.alloc(CallExpression {
                            span: rest.span,
                            callee: slice,
                            arguments,
                            optional: false,
                        }));
                        lower_pattern_binding(
                            arena,
                            interner,
                            iterator_helper,
                            object_helper,
                            kind,
                            rest.argument,
                            value,
                            temps,
                            output,
                        );
                        continue;
                    }
                    _ => Expression::Member(arena.alloc(MemberExpression {
                        span: element.span(),
                        object: captured,
                        property: MemberProperty::Computed(Expression::NumberLiteral(arena.alloc(
                            wake_ecma_ast::NumberLiteral {
                                span: element.span(),
                                value: index as f64,
                            },
                        ))),
                        optional: false,
                    })),
                };
                lower_pattern_binding(
                    arena,
                    interner,
                    iterator_helper,
                    object_helper,
                    kind,
                    element,
                    extracted,
                    temps,
                    output,
                );
            }
        }
        Pattern::Object(object) => {
            lower_object_pattern(
                arena,
                interner,
                iterator_helper,
                object_helper,
                kind,
                object,
                value,
                temps,
                output,
            );
        }
    }
}

fn lower_object_pattern<'a>(
    arena: &'a Bump,
    interner: &Interner,
    iterator_helper: Atom,
    object_helper: Atom,
    kind: VarKind,
    object: &'a ObjectPattern<'a>,
    value: Expression<'a>,
    temps: &mut impl Iterator<Item = Atom>,
    output: &mut AVec<'a, VariableDeclarator<'a>>,
) {
    let captured = capture_value(arena, object.span, kind, value, temps, output);
    // Object binding patterns perform RequireObjectCoercible before evaluating any computed
    // property keys. Keep this explicit even for `{}`: unlike a member read, an empty pattern has
    // no later operation that would otherwise throw for null/undefined.
    capture_value(
        arena,
        object.span,
        kind,
        require_object_coercible_check(arena, object.span, captured),
        temps,
        output,
    );
    let mut excluded = AVec::with_capacity_in(object.properties.len(), arena);
    for property in object.properties.iter() {
        let (member_property, excluded_key) = match property.key {
            PropertyKey::Ident(ident) => (
                MemberProperty::Ident(ident),
                Expression::StringLiteral(arena.alloc(StringLiteral {
                    span: ident.span,
                    value: ident.name,
                })),
            ),
            PropertyKey::Computed(expression) => {
                let key = if object.rest.is_some() {
                    let raw = capture_value(arena, property.span, kind, expression, temps, output);
                    capture_value(
                        arena,
                        property.span,
                        kind,
                        to_property_key(arena, interner, property.span, raw),
                        temps,
                        output,
                    )
                } else {
                    expression
                };
                (MemberProperty::Computed(key), key)
            }
            PropertyKey::String(string) => {
                let key = Expression::StringLiteral(string);
                (MemberProperty::Computed(key), key)
            }
            PropertyKey::Number(number) => {
                let property = Expression::NumberLiteral(number);
                let excluded = to_string_key(arena, interner, number.span, property);
                (MemberProperty::Computed(property), excluded)
            }
            PropertyKey::Private(ident) => (
                MemberProperty::Private(ident),
                Expression::StringLiteral(arena.alloc(StringLiteral {
                    span: ident.span,
                    value: ident.name,
                })),
            ),
        };
        excluded.push(Some(excluded_key));
        let extracted = Expression::Member(arena.alloc(MemberExpression {
            span: property.span,
            object: captured,
            property: member_property,
            optional: false,
        }));
        lower_pattern_binding(
            arena,
            interner,
            iterator_helper,
            object_helper,
            kind,
            property.value,
            extracted,
            temps,
            output,
        );
    }
    if let Some(rest) = object.rest {
        let helper = Expression::Identifier(arena.alloc(Ident::new(rest.span, object_helper)));
        let callee = member(arena, interner, rest.span, helper, "rest");
        let excluded = Expression::Array(arena.alloc(ArrayExpression {
            span: rest.span,
            elements: excluded,
        }));
        let mut arguments = AVec::with_capacity_in(2, arena);
        arguments.push(captured);
        arguments.push(excluded);
        let rest_value = Expression::Call(arena.alloc(CallExpression {
            span: rest.span,
            callee,
            arguments,
            optional: false,
        }));
        lower_pattern_binding(
            arena,
            interner,
            iterator_helper,
            object_helper,
            kind,
            rest.argument,
            rest_value,
            temps,
            output,
        );
    }
}

fn capture_value<'a>(
    arena: &'a Bump,
    span: Span,
    kind: VarKind,
    value: Expression<'a>,
    temps: &mut impl Iterator<Item = Atom>,
    output: &mut AVec<'a, VariableDeclarator<'a>>,
) -> Expression<'a> {
    let atom = temps.next().expect("temporary count matches pattern");
    let ident = arena.alloc(Ident::new(span, atom));
    output.push(VariableDeclarator {
        span,
        id: Pattern::Ident(ident),
        init: Some(value),
    });
    let _ = kind;
    Expression::Identifier(ident)
}

fn undefined_fallback<'a>(
    arena: &'a Bump,
    span: Span,
    value: Expression<'a>,
    fallback: Expression<'a>,
) -> Expression<'a> {
    let zero =
        Expression::NumberLiteral(arena.alloc(wake_ecma_ast::NumberLiteral { span, value: 0.0 }));
    let undefined = Expression::Unary(arena.alloc(UnaryExpression {
        span,
        operator: UnaryOperator::Void,
        argument: zero,
    }));
    let test = Expression::Binary(arena.alloc(BinaryExpression {
        span,
        operator: BinaryOperator::StrictEq,
        left: value,
        right: undefined,
    }));
    Expression::Conditional(arena.alloc(ConditionalExpression {
        span,
        test,
        consequent: fallback,
        alternate: value,
    }))
}

/// Lower array spread through `concat`, first materializing every spread operand with the
/// per-module iterator helper. Passing the original value directly to concat would not consume
/// generic iterables and would incorrectly observe Symbol.isConcatSpreadable.
pub fn lower_array_spread<'a>(
    arena: &'a Bump,
    interner: &Interner,
    helper: Atom,
    features: FeatureSet,
    array: &'a ArrayExpression<'a>,
) -> Expression<'a> {
    if !features.contains(EcmaFeature::Spread)
        || !array
            .elements
            .iter()
            .any(|element| matches!(element, Some(Expression::Spread(_))))
    {
        return Expression::Array(array);
    }

    let empty = Expression::Array(arena.alloc(ArrayExpression {
        span: array.span,
        elements: AVec::new_in(arena),
    }));
    let concat = member(arena, interner, array.span, empty, "concat");
    let mut arguments = AVec::new_in(arena);
    let mut pending = AVec::new_in(arena);
    for element in array.elements.iter().copied() {
        match element {
            Some(Expression::Spread(spread)) => {
                if !pending.is_empty() {
                    arguments.push(Expression::Array(arena.alloc(ArrayExpression {
                        span: array.span,
                        elements: std::mem::replace(&mut pending, AVec::new_in(arena)),
                    })));
                }
                arguments.push(to_array(arena, helper, spread.span, spread.argument, None));
            }
            other => pending.push(other),
        }
    }
    if !pending.is_empty() {
        arguments.push(Expression::Array(arena.alloc(ArrayExpression {
            span: array.span,
            elements: pending,
        })));
    }
    Expression::Call(arena.alloc(CallExpression {
        span: array.span,
        callee: concat,
        arguments,
        optional: false,
    }))
}

/// Lower call spread to Function#apply. Ordinary member calls require two scope-owned
/// temporaries: one captures the receiver before a getter can rebind its source identifier, and
/// one captures the function value before spread arguments are evaluated. `super.property(...)`
/// uses the lexical `this` receiver and never treats `super` as a value.
pub fn lower_call_spread<'a>(
    arena: &'a Bump,
    interner: &Interner,
    helper: Atom,
    temps: Option<[Atom; 2]>,
    features: FeatureSet,
    call: &'a CallExpression<'a>,
) -> Expression<'a> {
    if !features.contains(EcmaFeature::Spread)
        // A native derived constructor can only initialize `this` through the syntactic
        // `super(...)` form. `super.apply(...)` is not an equivalent lowering; removing this
        // spread therefore requires lowering the surrounding class as well.
        || matches!(call.callee, Expression::Super(_))
        || !call
            .arguments
            .iter()
            .any(|argument| matches!(argument, Expression::Spread(_)))
    {
        return Expression::Call(call);
    }
    if let Expression::Member(member) = call.callee
        && matches!(member.object, Expression::Super(_))
    {
        return lower_super_member_spread_call(arena, interner, helper, call);
    }
    if let Expression::Member(member_expression) = call.callee {
        let Some([function_temp, receiver_temp]) = temps else {
            // The parser only supplies these bindings in an execution scope that can own their
            // `var` declaration. Parameter initializers, class fields/static blocks and async
            // arrows conservatively retain native spread syntax instead of leaking a binding.
            return Expression::Call(call);
        };
        return lower_member_spread_call(
            arena,
            interner,
            helper,
            call,
            member_expression,
            function_temp,
            receiver_temp,
        );
    }
    let args = spread_arguments_array(arena, interner, helper, call.span, &call.arguments);
    call_apply(
        arena,
        interner,
        call.span,
        call.callee,
        undefined_expression(arena, call.span),
        args,
    )
}

fn lower_super_member_spread_call<'a>(
    arena: &'a Bump,
    interner: &Interner,
    helper: Atom,
    call: &'a CallExpression<'a>,
) -> Expression<'a> {
    let apply = member(arena, interner, call.span, call.callee, "apply");
    let spread_arguments =
        spread_arguments_array(arena, interner, helper, call.span, &call.arguments);
    let mut arguments = AVec::with_capacity_in(2, arena);
    arguments.push(Expression::This(call.span));
    arguments.push(spread_arguments);
    Expression::Call(arena.alloc(CallExpression {
        span: call.span,
        callee: apply,
        arguments,
        optional: call.optional,
    }))
}

fn lower_member_spread_call<'a>(
    arena: &'a Bump,
    interner: &Interner,
    helper: Atom,
    call: &'a CallExpression<'a>,
    member_expression: &'a MemberExpression<'a>,
    function_temp: Atom,
    receiver_temp: Atom,
) -> Expression<'a> {
    let receiver = Expression::Identifier(arena.alloc(Ident::new(call.span, receiver_temp)));
    let function = Expression::Identifier(arena.alloc(Ident::new(call.span, function_temp)));
    let captured_member = Expression::Member(arena.alloc(MemberExpression {
        span: member_expression.span,
        object: receiver,
        property: member_expression.property,
        optional: false,
    }));
    let spread_arguments =
        spread_arguments_array(arena, interner, helper, call.span, &call.arguments);
    let applied = call_apply(
        arena,
        interner,
        call.span,
        function,
        receiver,
        spread_arguments,
    );

    // Native evaluation order is receiver -> property key/getter -> arguments -> call. Keeping
    // all three operations in one sequence preserves lexical `this`/`arguments`/`new.target`, as
    // well as `await` and `yield` in the spread arguments.
    let mut expressions = AVec::with_capacity_in(3, arena);
    expressions.push(assign(arena, call.span, receiver, member_expression.object));
    expressions.push(assign(arena, call.span, function, captured_member));
    expressions.push(applied);
    Expression::Sequence(arena.alloc(SequenceExpression {
        span: call.span,
        expressions,
    }))
}

/// Lower constructor spread using the standard bind/apply construction pattern.
pub fn lower_new_spread<'a>(
    arena: &'a Bump,
    interner: &Interner,
    helper: Atom,
    features: FeatureSet,
    new: &'a NewExpression<'a>,
) -> Expression<'a> {
    if !features.contains(EcmaFeature::Spread)
        || !new
            .arguments
            .iter()
            .any(|argument| matches!(argument, Expression::Spread(_)))
    {
        return Expression::New(new);
    }
    let function =
        Expression::Identifier(arena.alloc(Ident::new(new.span, interner.intern("Function"))));
    let prototype = member(arena, interner, new.span, function, "prototype");
    let bind = member(arena, interner, new.span, prototype, "bind");
    let apply = member(arena, interner, new.span, bind, "apply");
    let mut seed = AVec::with_capacity_in(1, arena);
    seed.push(Some(Expression::NullLiteral(new.span)));
    let seed = Expression::Array(arena.alloc(ArrayExpression {
        span: new.span,
        elements: seed,
    }));
    let tail = spread_arguments_array(arena, interner, helper, new.span, &new.arguments);
    let concat = member(arena, interner, new.span, seed, "concat");
    let mut concat_arguments = AVec::with_capacity_in(1, arena);
    concat_arguments.push(tail);
    let bound_arguments = Expression::Call(arena.alloc(CallExpression {
        span: new.span,
        callee: concat,
        arguments: concat_arguments,
        optional: false,
    }));
    let mut apply_arguments = AVec::with_capacity_in(2, arena);
    apply_arguments.push(new.callee);
    apply_arguments.push(bound_arguments);
    let bound = Expression::Call(arena.alloc(CallExpression {
        span: new.span,
        callee: apply,
        arguments: apply_arguments,
        optional: false,
    }));
    // A sequence expression gives codegen an explicit precedence boundary around the call used as
    // a constructor: `new (0, bind.apply(...))()`.
    let mut constructor = AVec::with_capacity_in(2, arena);
    constructor.push(Expression::NumberLiteral(arena.alloc(
        wake_ecma_ast::NumberLiteral {
            span: new.span,
            value: 0.0,
        },
    )));
    constructor.push(bound);
    let constructor = Expression::Sequence(arena.alloc(SequenceExpression {
        span: new.span,
        expressions: constructor,
    }));
    Expression::New(arena.alloc(NewExpression {
        span: new.span,
        callee: constructor,
        arguments: AVec::new_in(arena),
    }))
}

fn spread_arguments_array<'a>(
    arena: &'a Bump,
    interner: &Interner,
    helper: Atom,
    span: Span,
    values: &[Expression<'a>],
) -> Expression<'a> {
    let mut elements = AVec::with_capacity_in(values.len(), arena);
    for value in values.iter().copied() {
        elements.push(Some(value));
    }
    let array = arena.alloc(ArrayExpression { span, elements });
    lower_array_spread(
        arena,
        interner,
        helper,
        FeatureSet(1 << EcmaFeature::Spread as u8),
        array,
    )
}

fn to_array<'a>(
    arena: &'a Bump,
    helper: Atom,
    span: Span,
    value: Expression<'a>,
    limit: Option<usize>,
) -> Expression<'a> {
    let callee = Expression::Identifier(arena.alloc(Ident::new(span, helper)));
    let mut arguments = AVec::with_capacity_in(usize::from(limit.is_some()) + 1, arena);
    arguments.push(value);
    if let Some(limit) = limit {
        arguments.push(Expression::NumberLiteral(arena.alloc(
            wake_ecma_ast::NumberLiteral {
                span,
                value: limit as f64,
            },
        )));
    }
    Expression::Call(arena.alloc(CallExpression {
        span,
        callee,
        arguments,
        optional: false,
    }))
}

fn member<'a>(
    arena: &'a Bump,
    interner: &Interner,
    span: Span,
    object: Expression<'a>,
    property: &str,
) -> Expression<'a> {
    Expression::Member(arena.alloc(MemberExpression {
        span,
        object,
        property: MemberProperty::Ident(Ident::new(span, interner.intern(property))),
        optional: false,
    }))
}

/// 紧凑、可复制的 pass 开关集合。
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FeatureSet(u64);

impl FeatureSet {
    pub const fn insert(&mut self, feature: EcmaFeature) {
        self.0 |= 1 << feature as u8;
    }

    pub const fn contains(self, feature: EcmaFeature) -> bool {
        self.0 & (1 << feature as u8) != 0
    }

    pub const fn remove(&mut self, feature: EcmaFeature) {
        self.0 &= !(1 << feature as u8);
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub fn iter(self) -> impl Iterator<Item = EcmaFeature> {
        EcmaFeature::ALL
            .into_iter()
            .filter(move |feature| self.contains(*feature))
    }
}

impl fmt::Debug for FeatureSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

/// 编译核心使用的稳定目标环境。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TargetEnv {
    targets: Vec<BrowserTarget>,
    required: FeatureSet,
}

impl Default for TargetEnv {
    fn default() -> Self {
        Self::baseline()
    }
}

/// TypeScript 转换口径。类型语法由 parser 消费，值空间 lowering 由统一管线负责。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TypeScriptOptions {
    pub enabled: bool,
    /// 与 tsc/Babel 一致，仅移除类型导入；值导入仍参与模块图。
    pub only_remove_type_imports: bool,
}

/// React JSX automatic runtime 口径。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReactOptions {
    pub enabled: bool,
    pub development: bool,
    pub import_source: String,
}

impl Default for ReactOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            development: false,
            import_source: "react".to_string(),
        }
    }
}

/// 一次模块转换的统一配置。
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct TransformOptions {
    pub target: TargetEnv,
    pub typescript: TypeScriptOptions,
    pub react: ReactOptions,
}

impl TransformOptions {
    /// 稳定配置指纹；组合目标、TS 和 React 口径，供 parse/transform 任务键使用。
    pub fn fingerprint(&self) -> u64 {
        let mut hash = self.target.fingerprint();
        for byte in [
            u8::from(self.typescript.enabled),
            u8::from(self.typescript.only_remove_type_imports),
            u8::from(self.react.enabled),
            u8::from(self.react.development),
        ]
        .into_iter()
        .chain(self.react.import_source.bytes())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }
}

/// parser 构建节点时调用的 lowering 原语。
///
/// 该入口位于 transform crate，parser 只负责确定语法结构。返回值仍分配在同一个 arena，
/// 因而在 `ModuleAst` 冻结前完成，不需要破坏 holder 的只读不变量。
pub fn lower_binary<'a>(
    arena: &'a Bump,
    interner: &Interner,
    features: FeatureSet,
    span: Span,
    operator: BinaryOperator,
    left: Expression<'a>,
    right: Expression<'a>,
) -> Expression<'a> {
    if operator != BinaryOperator::Exp || !features.contains(EcmaFeature::ExponentiationOperator) {
        return Expression::Binary(arena.alloc(BinaryExpression {
            span,
            operator,
            left,
            right,
        }));
    }

    // `a ** b` → `Math.pow(a, b)`；左右操作数的求值次数和顺序保持不变。
    let math = Expression::Identifier(arena.alloc(Ident::new(span, interner.intern("Math"))));
    let callee = Expression::Member(arena.alloc(MemberExpression {
        span,
        object: math,
        property: MemberProperty::Ident(Ident::new(span, interner.intern("pow"))),
        optional: false,
    }));
    let mut arguments = AVec::with_capacity_in(2, arena);
    arguments.push(left);
    arguments.push(right);
    Expression::Call(arena.alloc(CallExpression {
        span,
        callee,
        arguments,
        optional: false,
    }))
}

/// Whether an assignment left-hand side is the expression-shaped cover form of an array/object
/// destructuring target.
pub fn is_destructuring_assignment_target(left: Expression<'_>) -> bool {
    matches!(left, Expression::Array(_) | Expression::Object(_))
}

/// Whether a raw assignment target contains object rest at any nesting depth.
pub fn assignment_target_has_object_rest(target: Expression<'_>) -> bool {
    match target {
        Expression::Array(array) => array
            .elements
            .iter()
            .flatten()
            .any(|element| match element {
                Expression::Spread(spread) => assignment_target_has_object_rest(spread.argument),
                element => assignment_target_has_object_rest(*element),
            }),
        Expression::Object(object) => object.properties.iter().any(|member| match member {
            ObjectMember::Property(property) => assignment_target_has_object_rest(property.value),
            ObjectMember::Spread(_) => true,
        }),
        Expression::Assignment(assignment) if assignment.operator == AssignmentOperator::Assign => {
            assignment_target_has_object_rest(assignment.left)
        }
        Expression::Spread(spread) => assignment_target_has_object_rest(spread.argument),
        _ => false,
    }
}

/// Assignment destructuring must run either when all destructuring syntax is unsupported, or when
/// only object rest is unsupported by an otherwise destructuring-capable target.
pub fn destructuring_assignment_needs_lowering(features: FeatureSet, left: Expression<'_>) -> bool {
    is_destructuring_assignment_target(left)
        && (features.contains(EcmaFeature::Destructuring)
            || (features.contains(EcmaFeature::ObjectRestSpread)
                && assignment_target_has_object_rest(left)))
}

/// Number of collision-free scope-owned `var` names needed by assignment destructuring.
pub fn destructuring_assignment_temporary_count(target: Expression<'_>) -> usize {
    if !is_destructuring_assignment_target(target) {
        return 0;
    }
    1 + assignment_pattern_temporary_count(target)
}

fn assignment_pattern_temporary_count(target: Expression<'_>) -> usize {
    match target {
        Expression::Assignment(assignment) if assignment.operator == AssignmentOperator::Assign => {
            1 + assignment_pattern_temporary_count(assignment.left)
        }
        Expression::Array(array) => {
            1 + array
                .elements
                .iter()
                .flatten()
                .map(|element| match element {
                    Expression::Spread(spread) => {
                        assignment_pattern_temporary_count(spread.argument)
                    }
                    element => assignment_pattern_temporary_count(*element),
                })
                .sum::<usize>()
        }
        Expression::Object(object) => {
            1 + object
                .properties
                .iter()
                .map(|member| match member {
                    ObjectMember::Property(property) => {
                        assignment_pattern_temporary_count(property.value)
                            + usize::from(matches!(property.key, PropertyKey::Computed(_))) * 2
                    }
                    ObjectMember::Spread(spread) => {
                        assignment_pattern_temporary_count(spread.argument)
                    }
                })
                .sum::<usize>()
        }
        Expression::Spread(spread) => assignment_pattern_temporary_count(spread.argument),
        _ => 0,
    }
}

/// Lower array/object destructuring assignment to a scope-owned sequence while returning the
/// original RHS. The parser declares every temporary in the nearest safe Program/function scope,
/// so `this`, `arguments`, `super`, `new.target`, `await`, and `yield` remain in their source
/// context. Object key/property/default evaluation and assignment-target ordering stay single-pass.
/// Array patterns materialize only the prefix needed by patterns without rest, while fine-grained
/// iterator/target interleaving remains a separate pass.
pub fn lower_destructuring_assignment<'a>(
    arena: &'a Bump,
    interner: &Interner,
    iterator_helper: Atom,
    object_helper: Atom,
    features: FeatureSet,
    span: Span,
    left: Expression<'a>,
    right: Expression<'a>,
    temporaries: &[Atom],
) -> Expression<'a> {
    if !destructuring_assignment_needs_lowering(features, left) {
        return Expression::Assignment(arena.alloc(AssignmentExpression {
            span,
            operator: AssignmentOperator::Assign,
            left,
            right,
        }));
    }

    let mut temps = temporaries.iter().copied();
    let result_atom = temps
        .next()
        .expect("temporary count matches destructuring assignment");
    let result = Expression::Identifier(arena.alloc(Ident::new(span, result_atom)));
    let mut operations = Vec::new();
    operations.push(assign(arena, span, result, right));
    lower_assignment_pattern(
        arena,
        interner,
        iterator_helper,
        object_helper,
        left,
        result,
        &mut operations,
        &mut temps,
    );
    operations.push(result);
    debug_assert!(temps.next().is_none());

    let mut expressions = AVec::with_capacity_in(operations.len(), arena);
    expressions.extend(operations);
    Expression::Sequence(arena.alloc(SequenceExpression { span, expressions }))
}

#[allow(clippy::too_many_arguments)]
fn lower_assignment_pattern<'a>(
    arena: &'a Bump,
    interner: &Interner,
    iterator_helper: Atom,
    object_helper: Atom,
    target: Expression<'a>,
    value: Expression<'a>,
    operations: &mut Vec<Expression<'a>>,
    temps: &mut impl Iterator<Item = Atom>,
) {
    match target {
        Expression::Assignment(assignment) if assignment.operator == AssignmentOperator::Assign => {
            let atom = temps
                .next()
                .expect("temporary count matches assignment default");
            let captured = Expression::Identifier(arena.alloc(Ident::new(assignment.span, atom)));
            let capture = assign(arena, assignment.span, captured, value);
            let selected = undefined_fallback(arena, assignment.span, captured, assignment.right);
            let mut expressions = AVec::with_capacity_in(2, arena);
            expressions.push(capture);
            expressions.push(selected);
            let selected = Expression::Sequence(arena.alloc(SequenceExpression {
                span: assignment.span,
                expressions,
            }));
            lower_assignment_pattern(
                arena,
                interner,
                iterator_helper,
                object_helper,
                assignment.left,
                selected,
                operations,
                temps,
            );
        }
        Expression::Array(array) => {
            let limit = if array
                .elements
                .iter()
                .flatten()
                .any(|element| matches!(element, Expression::Spread(_)))
            {
                None
            } else {
                Some(array.elements.len())
            };
            let atom = temps
                .next()
                .expect("temporary count matches array assignment pattern");
            let captured = capture_assignment_value(
                arena,
                array.span,
                atom,
                to_array(arena, iterator_helper, array.span, value, limit),
                operations,
            );
            for (index, element) in array.elements.iter().copied().enumerate() {
                let Some(element) = element else { continue };
                let (target, extracted) = match element {
                    Expression::Spread(spread) => {
                        let slice = member(arena, interner, spread.span, captured, "slice");
                        let mut arguments = AVec::with_capacity_in(1, arena);
                        arguments.push(Expression::NumberLiteral(arena.alloc(
                            wake_ecma_ast::NumberLiteral {
                                span: spread.span,
                                value: index as f64,
                            },
                        )));
                        (
                            spread.argument,
                            Expression::Call(arena.alloc(CallExpression {
                                span: spread.span,
                                callee: slice,
                                arguments,
                                optional: false,
                            })),
                        )
                    }
                    target => (
                        target,
                        Expression::Member(arena.alloc(MemberExpression {
                            span: target.span(),
                            object: captured,
                            property: MemberProperty::Computed(Expression::NumberLiteral(
                                arena.alloc(wake_ecma_ast::NumberLiteral {
                                    span: target.span(),
                                    value: index as f64,
                                }),
                            )),
                            optional: false,
                        })),
                    ),
                };
                lower_assignment_pattern(
                    arena,
                    interner,
                    iterator_helper,
                    object_helper,
                    target,
                    extracted,
                    operations,
                    temps,
                );
            }
        }
        Expression::Object(object) => lower_object_assignment_pattern(
            arena,
            interner,
            iterator_helper,
            object_helper,
            object,
            value,
            operations,
            temps,
        ),
        Expression::Spread(spread) => lower_assignment_pattern(
            arena,
            interner,
            iterator_helper,
            object_helper,
            spread.argument,
            value,
            operations,
            temps,
        ),
        _ => {
            // Native assignment evaluation establishes a member Reference before evaluating the
            // RHS. Keeping extraction/default work on the RHS preserves that ordering.
            operations.push(assign(arena, target.span(), target, value));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_object_assignment_pattern<'a>(
    arena: &'a Bump,
    interner: &Interner,
    iterator_helper: Atom,
    object_helper: Atom,
    object: &'a ObjectExpression<'a>,
    value: Expression<'a>,
    operations: &mut Vec<Expression<'a>>,
    temps: &mut impl Iterator<Item = Atom>,
) {
    let object_atom = temps
        .next()
        .expect("temporary count matches object assignment pattern");
    let captured = capture_assignment_value(arena, object.span, object_atom, value, operations);
    let has_rest = object
        .properties
        .iter()
        .any(|member| matches!(member, ObjectMember::Spread(_)));
    operations.push(require_object_coercible_check(arena, object.span, captured));
    let mut excluded = Vec::with_capacity(object.properties.len());
    for member_node in object.properties.iter() {
        match member_node {
            ObjectMember::Property(property) => {
                let member_property = match property.key {
                    PropertyKey::Ident(ident) => {
                        if has_rest {
                            excluded.push(Expression::StringLiteral(arena.alloc(StringLiteral {
                                span: ident.span,
                                value: ident.name,
                            })));
                        }
                        MemberProperty::Ident(ident)
                    }
                    PropertyKey::String(string) => {
                        if has_rest {
                            excluded.push(Expression::StringLiteral(string));
                        }
                        MemberProperty::Computed(Expression::StringLiteral(string))
                    }
                    PropertyKey::Number(number) => {
                        if has_rest {
                            excluded.push(to_string_key(
                                arena,
                                interner,
                                property.span,
                                Expression::NumberLiteral(number),
                            ));
                        }
                        MemberProperty::Computed(Expression::NumberLiteral(number))
                    }
                    PropertyKey::Computed(expression) => {
                        let raw_atom = temps
                            .next()
                            .expect("temporary count matches raw computed key");
                        let raw = capture_assignment_value(
                            arena,
                            property.span,
                            raw_atom,
                            expression,
                            operations,
                        );
                        let key_atom = temps
                            .next()
                            .expect("temporary count matches normalized computed key");
                        let key = capture_assignment_value(
                            arena,
                            property.span,
                            key_atom,
                            to_property_key(arena, interner, property.span, raw),
                            operations,
                        );
                        if has_rest {
                            excluded.push(key);
                        }
                        MemberProperty::Computed(key)
                    }
                    PropertyKey::Private(ident) => {
                        if has_rest {
                            excluded.push(Expression::StringLiteral(arena.alloc(StringLiteral {
                                span: ident.span,
                                value: ident.name,
                            })));
                        }
                        MemberProperty::Private(ident)
                    }
                };
                let extracted = Expression::Member(arena.alloc(MemberExpression {
                    span: property.span,
                    object: captured,
                    property: member_property,
                    optional: false,
                }));
                lower_assignment_pattern(
                    arena,
                    interner,
                    iterator_helper,
                    object_helper,
                    property.value,
                    extracted,
                    operations,
                    temps,
                );
            }
            ObjectMember::Spread(rest) => {
                let helper =
                    Expression::Identifier(arena.alloc(Ident::new(rest.span, object_helper)));
                let callee = member(arena, interner, rest.span, helper, "rest");
                let mut elements = AVec::with_capacity_in(excluded.len(), arena);
                elements.extend(excluded.iter().copied().map(Some));
                let excluded = Expression::Array(arena.alloc(ArrayExpression {
                    span: rest.span,
                    elements,
                }));
                let mut arguments = AVec::with_capacity_in(2, arena);
                arguments.push(captured);
                arguments.push(excluded);
                let rest_value = Expression::Call(arena.alloc(CallExpression {
                    span: rest.span,
                    callee,
                    arguments,
                    optional: false,
                }));
                lower_assignment_pattern(
                    arena,
                    interner,
                    iterator_helper,
                    object_helper,
                    rest.argument,
                    rest_value,
                    operations,
                    temps,
                );
            }
        }
    }
}

fn capture_assignment_value<'a>(
    arena: &'a Bump,
    span: Span,
    atom: Atom,
    value: Expression<'a>,
    operations: &mut Vec<Expression<'a>>,
) -> Expression<'a> {
    let captured = Expression::Identifier(arena.alloc(Ident::new(span, atom)));
    operations.push(assign(arena, span, captured, value));
    captured
}

fn require_object_coercible_check<'a>(
    arena: &'a Bump,
    span: Span,
    value: Expression<'a>,
) -> Expression<'a> {
    let zero =
        Expression::NumberLiteral(arena.alloc(wake_ecma_ast::NumberLiteral { span, value: 0.0 }));
    let undefined = Expression::Unary(arena.alloc(UnaryExpression {
        span,
        operator: UnaryOperator::Void,
        argument: zero,
    }));
    let is_null = Expression::Binary(arena.alloc(BinaryExpression {
        span,
        operator: BinaryOperator::StrictEq,
        left: value,
        right: Expression::NullLiteral(span),
    }));
    let is_undefined = Expression::Binary(arena.alloc(BinaryExpression {
        span,
        operator: BinaryOperator::StrictEq,
        left: value,
        right: undefined,
    }));
    let test = Expression::Logical(arena.alloc(LogicalExpression {
        span,
        operator: LogicalOperator::Or,
        left: is_null,
        right: is_undefined,
    }));
    // This branch is selected only for null/undefined, producing the required TypeError without
    // an observable property access for valid objects.
    let failure = Expression::Member(arena.alloc(MemberExpression {
        span,
        object: value,
        property: MemberProperty::Computed(zero),
        optional: false,
    }));
    Expression::Conditional(arena.alloc(ConditionalExpression {
        span,
        test,
        consequent: failure,
        alternate: zero,
    }))
}

fn to_string_key<'a>(
    arena: &'a Bump,
    interner: &Interner,
    span: Span,
    value: Expression<'a>,
) -> Expression<'a> {
    let mut arguments = AVec::with_capacity_in(1, arena);
    arguments.push(value);
    Expression::Call(arena.alloc(CallExpression {
        span,
        callee: Expression::Identifier(arena.alloc(Ident::new(span, interner.intern("String")))),
        arguments,
        optional: false,
    }))
}

fn to_property_key<'a>(
    arena: &'a Bump,
    interner: &Interner,
    span: Span,
    value: Expression<'a>,
) -> Expression<'a> {
    let kind = Expression::Unary(arena.alloc(UnaryExpression {
        span,
        operator: UnaryOperator::Typeof,
        argument: value,
    }));
    let symbol = Expression::StringLiteral(arena.alloc(StringLiteral {
        span,
        value: interner.intern("symbol"),
    }));
    let test = Expression::Binary(arena.alloc(BinaryExpression {
        span,
        operator: BinaryOperator::StrictEq,
        left: kind,
        right: symbol,
    }));
    Expression::Conditional(arena.alloc(ConditionalExpression {
        span,
        test,
        consequent: value,
        alternate: to_string_key(arena, interner, span, value),
    }))
}

/// Lower assignment operators while preserving the single reference evaluation required by
/// compound and logical assignments.
///
/// Complex member targets require parser-owned temporaries in the current execution scope. When
/// no such scope is available (parameter defaults, class initializers, cover grammar and async
/// arrows), the original syntax is retained instead of leaking a binding into an outer scope.
pub fn lower_assignment<'a>(
    arena: &'a Bump,
    interner: &Interner,
    temps: Option<[Atom; 3]>,
    features: FeatureSet,
    span: Span,
    operator: AssignmentOperator,
    left: Expression<'a>,
    right: Expression<'a>,
) -> Expression<'a> {
    if operator == AssignmentOperator::Exp
        && features.contains(EcmaFeature::ExponentiationOperator)
        && matches!(left, Expression::Identifier(_))
    {
        let pow = lower_binary(
            arena,
            interner,
            features,
            span,
            BinaryOperator::Exp,
            left,
            right,
        );
        return assign(arena, span, left, pow);
    }

    if features.contains(EcmaFeature::LogicalAssignment)
        && matches!(left, Expression::Identifier(_))
    {
        match operator {
            AssignmentOperator::And | AssignmentOperator::Or => {
                let logical = if operator == AssignmentOperator::And {
                    LogicalOperator::And
                } else {
                    LogicalOperator::Or
                };
                return Expression::Logical(arena.alloc(LogicalExpression {
                    span,
                    operator: logical,
                    left,
                    right: assign(arena, span, left, right),
                }));
            }
            AssignmentOperator::Coalesce => {
                // `x ??= y` → `x !== null && x !== void 0 ? x : x = y`.
                let not_null = strict_not(arena, span, left, Expression::NullLiteral(span));
                let zero = Expression::NumberLiteral(
                    arena.alloc(wake_ecma_ast::NumberLiteral { span, value: 0.0 }),
                );
                let undefined = Expression::Unary(arena.alloc(UnaryExpression {
                    span,
                    operator: UnaryOperator::Void,
                    argument: zero,
                }));
                let not_undefined = strict_not(arena, span, left, undefined);
                let test = Expression::Logical(arena.alloc(LogicalExpression {
                    span,
                    operator: LogicalOperator::And,
                    left: not_null,
                    right: not_undefined,
                }));
                return Expression::Conditional(arena.alloc(ConditionalExpression {
                    span,
                    test,
                    consequent: left,
                    alternate: assign(arena, span, left, right),
                }));
            }
            _ => {}
        }
    }

    if let Expression::Member(member) = left {
        let needs_exponentiation = operator == AssignmentOperator::Exp
            && features.contains(EcmaFeature::ExponentiationOperator);
        let needs_logical = matches!(
            operator,
            AssignmentOperator::And | AssignmentOperator::Or | AssignmentOperator::Coalesce
        ) && features.contains(EcmaFeature::LogicalAssignment);
        if needs_exponentiation || needs_logical {
            let Some([receiver_temp, key_temp, value_temp]) = temps else {
                return Expression::Assignment(arena.alloc(AssignmentExpression {
                    span,
                    operator,
                    left,
                    right,
                }));
            };
            return lower_member_assignment(
                arena,
                interner,
                receiver_temp,
                key_temp,
                value_temp,
                features,
                span,
                operator,
                member,
                right,
            );
        }
    }

    Expression::Assignment(arena.alloc(AssignmentExpression {
        span,
        operator,
        left,
        right,
    }))
}

pub fn assignment_needs_temporaries(
    features: FeatureSet,
    operator: AssignmentOperator,
    left: Expression<'_>,
) -> bool {
    if !matches!(left, Expression::Member(_)) {
        return false;
    }
    (operator == AssignmentOperator::Exp && features.contains(EcmaFeature::ExponentiationOperator))
        || (matches!(
            operator,
            AssignmentOperator::And | AssignmentOperator::Or | AssignmentOperator::Coalesce
        ) && features.contains(EcmaFeature::LogicalAssignment))
}

/// Lower an arrow expression for the selected target.
///
/// Synchronous arrows can become function expressions; lexical `this` is preserved with
/// `.bind(this)`, while arrows that reference lexical `arguments`, `super` or `new.target` are
/// retained until the scope-capture pass is available. Async arrows remain arrows. When async,
/// arrow and native parameter syntax are all supported, unsupported destructuring inside async
/// arrow parameters can still move into the body without emulating rest through lexical
/// `arguments`.
pub fn lower_arrow<'a>(
    arena: &'a Bump,
    interner: &Interner,
    iterator_helper: Atom,
    object_helper: Atom,
    parameter_temporaries: &[Atom],
    features: FeatureSet,
    arrow: &'a ArrowFunction<'a>,
) -> Expression<'a> {
    let lower_arrow_syntax = features.contains(EcmaFeature::ArrowFunction);
    let lower_binding_parameters = arrow
        .params
        .iter()
        .copied()
        .any(|param| binding_pattern_needs_lowering(features, param));
    let lower_parameters =
        features.contains(EcmaFeature::FunctionParameters) || lower_binding_parameters;
    if arrow.is_async
        && (lower_arrow_syntax
            || features.contains(EcmaFeature::AsyncAwait)
            || features.contains(EcmaFeature::FunctionParameters)
            || !lower_binding_parameters)
    {
        return Expression::Arrow(arrow);
    }
    if !lower_arrow_syntax && !lower_parameters {
        return Expression::Arrow(arrow);
    }

    let arguments = interner.intern("arguments");
    let mut hazards = ArrowHazards {
        arguments,
        uses_this: false,
        unsupported: false,
    };
    // Arrow parameter initializers and computed binding keys execute in the same lexical
    // `this`/`arguments`/`super`/`new.target` environment as the body. Scanning only the body can
    // therefore turn a correct arrow into an observably different (or syntactically invalid)
    // ordinary function.
    for parameter in arrow.params.iter() {
        hazards.visit_pattern(parameter);
    }
    match arrow.body {
        ArrowBody::Block(body) => {
            for statement in body.statements.iter() {
                hazards.visit_statement(statement);
            }
        }
        ArrowBody::Expression(expression) => hazards.visit_expression(&expression),
    }
    if lower_arrow_syntax && hazards.unsupported {
        return Expression::Arrow(arrow);
    }

    let mut params = AVec::with_capacity_in(arrow.params.len(), arena);
    params.extend(arrow.params.iter().copied());
    let body = match arrow.body {
        ArrowBody::Block(body) => body,
        ArrowBody::Expression(expression) => {
            let mut statements = AVec::with_capacity_in(1, arena);
            statements.push(Statement::Return(arena.alloc(ReturnStatement {
                span: arrow.span,
                argument: Some(expression),
            })));
            arena.alloc(wake_ecma_ast::FunctionBody {
                span: arrow.span,
                statements,
                strict: false,
            })
        }
    };
    let function = arena.alloc(wake_ecma_ast::Function {
        span: arrow.span,
        id: None,
        params,
        body: Some(body),
        is_async: arrow.is_async,
        is_generator: false,
    });
    let function = lower_complex_parameters(
        arena,
        interner,
        iterator_helper,
        object_helper,
        features,
        function,
        parameter_temporaries,
    );
    if arrow.is_async || !lower_arrow_syntax {
        return Expression::Arrow(arena.alloc(ArrowFunction {
            span: arrow.span,
            params: {
                let mut params = AVec::with_capacity_in(function.params.len(), arena);
                params.extend(function.params.iter().copied());
                params
            },
            body: ArrowBody::Block(function.body.expect("arrow lowering creates a body")),
            is_async: arrow.is_async,
        }));
    }
    let function = Expression::Function(function);
    if !hazards.uses_this {
        return function;
    }

    let bind = Expression::Member(arena.alloc(MemberExpression {
        span: arrow.span,
        object: function,
        property: MemberProperty::Ident(Ident::new(arrow.span, interner.intern("bind"))),
        optional: false,
    }));
    let mut arguments = AVec::with_capacity_in(1, arena);
    arguments.push(Expression::This(arrow.span));
    Expression::Call(arena.alloc(CallExpression {
        span: arrow.span,
        callee: bind,
        arguments,
        optional: false,
    }))
}

/// Lower an untagged template literal through `String#concat`, avoiding the numeric-addition
/// ambiguity of a naive `"" + expr + expr` rewrite.
pub fn lower_template<'a>(
    arena: &'a Bump,
    interner: &Interner,
    features: FeatureSet,
    template: &'a TemplateLiteral<'a>,
) -> Expression<'a> {
    if !features.contains(EcmaFeature::TemplateLiteral) {
        return Expression::TemplateLiteral(template);
    }
    let first = template.quasis.first().expect("template has one quasi");
    let first_value = first.cooked.unwrap_or(first.raw);
    let base = Expression::StringLiteral(arena.alloc(StringLiteral {
        span: first.span,
        value: first_value,
    }));
    if template.expressions.is_empty() {
        return base;
    }
    let concat = Expression::Member(arena.alloc(MemberExpression {
        span: template.span,
        object: base,
        property: MemberProperty::Ident(Ident::new(template.span, interner.intern("concat"))),
        optional: false,
    }));
    let mut arguments = AVec::with_capacity_in(template.expressions.len() * 2, arena);
    for (index, expression) in template.expressions.iter().copied().enumerate() {
        arguments.push(expression);
        let quasi = template.quasis[index + 1];
        let value = quasi.cooked.unwrap_or(quasi.raw);
        if interner.with_resolved(value, |text| !text.is_empty()) {
            arguments.push(Expression::StringLiteral(arena.alloc(StringLiteral {
                span: quasi.span,
                value,
            })));
        }
    }
    Expression::Call(arena.alloc(CallExpression {
        span: template.span,
        callee: concat,
        arguments,
        optional: false,
    }))
}

pub const fn lower_object_shorthand(features: FeatureSet) -> bool {
    features.contains(EcmaFeature::ShorthandProperties)
}

/// Returns whether an object method must keep method syntax because it reads lexical `super`.
///
/// Object methods carry a `[[HomeObject]]`; rewriting one to `key: function` would either change
/// the receiver used by `super.property` or emit invalid function syntax. Parameter defaults and
/// computed binding keys share the method's lexical `super`, as do nested arrows. A nested ordinary
/// function starts a new function boundary and is deliberately not attributed to the method.
pub fn object_method_uses_lexical_super(function: &Function<'_>) -> bool {
    let mut visitor = LexicalSuperVisitor { found: false };
    for parameter in function.params.iter() {
        visitor.visit_pattern(parameter);
    }
    if let Some(body) = function.body {
        for statement in body.statements.iter() {
            visitor.visit_statement(statement);
        }
    }
    visitor.found
}

pub fn lower_optional_catch_binding<'a>(
    arena: &'a Bump,
    temp: Atom,
    features: FeatureSet,
    param: Option<Pattern<'a>>,
    span: Span,
) -> Option<Pattern<'a>> {
    if param.is_none() && features.contains(EcmaFeature::OptionalCatchBinding) {
        Some(Pattern::Ident(arena.alloc(Ident::new(span, temp))))
    } else {
        param
    }
}

/// Lower simple default parameters and identifier rest parameters by rebuilding the function and
/// injecting initialization statements after its directive prologue.
pub fn lower_function_parameters<'a>(
    arena: &'a Bump,
    interner: &Interner,
    features: FeatureSet,
    function: &'a Function<'a>,
) -> &'a Function<'a> {
    if !features.contains(EcmaFeature::FunctionParameters) || function.body.is_none() {
        return function;
    }

    let mut params = AVec::with_capacity_in(function.params.len(), arena);
    let mut initializers = AVec::new_in(arena);
    for (index, param) in function.params.iter().copied().enumerate() {
        match param {
            Pattern::Assignment(assignment) => {
                let Pattern::Ident(ident) = assignment.left else {
                    params.push(param);
                    continue;
                };
                params.push(Pattern::Ident(ident));
                initializers.push(default_parameter_statement(
                    arena,
                    assignment.span,
                    ident,
                    assignment.right,
                ));
            }
            Pattern::Rest(rest) => {
                let Pattern::Ident(ident) = rest.argument else {
                    params.push(param);
                    continue;
                };
                initializers.push(rest_parameter_statement(
                    arena, interner, rest.span, ident, index,
                ));
            }
            _ => params.push(param),
        }
    }
    if initializers.is_empty() {
        return function;
    }

    let body = function.body.expect("checked above");
    let directive_count = body
        .statements
        .iter()
        .take_while(|statement| {
            matches!(
                statement,
                Statement::Expression(expression)
                    if matches!(expression.expression, Expression::StringLiteral(_))
            )
        })
        .count();
    let mut statements = AVec::with_capacity_in(body.statements.len() + initializers.len(), arena);
    statements.extend(body.statements[..directive_count].iter().copied());
    statements.extend(initializers);
    statements.extend(body.statements[directive_count..].iter().copied());
    let body = arena.alloc(FunctionBody {
        span: body.span,
        statements,
        strict: body.strict,
    });
    arena.alloc(Function {
        span: function.span,
        id: function.id,
        params,
        body: Some(body),
        is_async: function.is_async,
        is_generator: function.is_generator,
    })
}

fn default_parameter_statement<'a>(
    arena: &'a Bump,
    span: Span,
    ident: &'a Ident,
    default: Expression<'a>,
) -> Statement<'a> {
    let value = Expression::Identifier(ident);
    let zero =
        Expression::NumberLiteral(arena.alloc(wake_ecma_ast::NumberLiteral { span, value: 0.0 }));
    let undefined = Expression::Unary(arena.alloc(UnaryExpression {
        span,
        operator: UnaryOperator::Void,
        argument: zero,
    }));
    let test = Expression::Binary(arena.alloc(BinaryExpression {
        span,
        operator: BinaryOperator::StrictEq,
        left: value,
        right: undefined,
    }));
    let assignment = assign(arena, span, value, default);
    let consequent = Statement::Expression(arena.alloc(ExpressionStatement {
        span,
        expression: assignment,
    }));
    Statement::If(arena.alloc(IfStatement {
        span,
        test,
        consequent,
        alternate: None,
    }))
}

fn rest_parameter_statement<'a>(
    arena: &'a Bump,
    interner: &Interner,
    span: Span,
    ident: &'a Ident,
    start: usize,
) -> Statement<'a> {
    let init = rest_parameter_value(arena, interner, span, start);
    let mut declarations = AVec::with_capacity_in(1, arena);
    declarations.push(VariableDeclarator {
        span,
        id: Pattern::Ident(ident),
        init: Some(init),
    });
    Statement::VariableDeclaration(arena.alloc(VariableDeclaration {
        span,
        kind: VarKind::Var,
        declarations,
    }))
}

fn rest_parameter_value<'a>(
    arena: &'a Bump,
    interner: &Interner,
    span: Span,
    start: usize,
) -> Expression<'a> {
    let mut expression =
        Expression::Identifier(arena.alloc(Ident::new(span, interner.intern("Array"))));
    for name in ["prototype", "slice", "call"] {
        expression = Expression::Member(arena.alloc(MemberExpression {
            span,
            object: expression,
            property: MemberProperty::Ident(Ident::new(span, interner.intern(name))),
            optional: false,
        }));
    }
    let mut arguments = AVec::with_capacity_in(2, arena);
    arguments.push(Expression::Identifier(
        arena.alloc(Ident::new(span, interner.intern("arguments"))),
    ));
    arguments.push(Expression::NumberLiteral(arena.alloc(
        wake_ecma_ast::NumberLiteral {
            span,
            value: start as f64,
        },
    )));
    Expression::Call(arena.alloc(CallExpression {
        span,
        callee: expression,
        arguments,
        optional: false,
    }))
}

struct LexicalSuperVisitor {
    found: bool,
}

impl<'a> Visit<'a> for LexicalSuperVisitor {
    fn visit_expression(&mut self, node: &Expression<'a>) {
        if self.found {
            return;
        }
        if matches!(node, Expression::Super(_)) {
            self.found = true;
        } else {
            wake_ecma_ast::walk_expression(self, node);
        }
    }

    // Ordinary functions do not inherit the surrounding method's lexical `super`. Arrows are not
    // represented as `Function`, so the default expression walker still traverses them.
    fn visit_function(&mut self, _node: &Function<'a>) {}

    // A nested class supplies its own `super` environment to member bodies, field initializers and
    // static blocks. Its heritage, decorators and computed member names are evaluated in the
    // surrounding expression environment and therefore still belong to the object method.
    fn visit_class(&mut self, node: &wake_ecma_ast::Class<'a>) {
        for decorator in node.decorators.iter() {
            self.visit_expression(decorator);
        }
        if let Some(super_class) = node.super_class {
            self.visit_expression(&super_class);
        }
        for member in node.body.iter() {
            match member {
                wake_ecma_ast::ClassMember::Method(method) => {
                    for decorator in method.decorators.iter() {
                        self.visit_expression(decorator);
                    }
                    if let PropertyKey::Computed(key) = method.key {
                        self.visit_expression(&key);
                    }
                }
                wake_ecma_ast::ClassMember::Property(property) => {
                    for decorator in property.decorators.iter() {
                        self.visit_expression(decorator);
                    }
                    if let PropertyKey::Computed(key) = property.key {
                        self.visit_expression(&key);
                    }
                }
                wake_ecma_ast::ClassMember::StaticBlock(_) => {}
            }
        }
    }
}

struct ArrowHazards {
    arguments: Atom,
    uses_this: bool,
    unsupported: bool,
}

impl<'a> Visit<'a> for ArrowHazards {
    fn visit_expression(&mut self, node: &Expression<'a>) {
        match node {
            Expression::This(_) => self.uses_this = true,
            Expression::Super(_) | Expression::MetaProperty(_) => self.unsupported = true,
            _ => wake_ecma_ast::walk_expression(self, node),
        }
    }

    fn visit_ident(&mut self, node: &Ident) {
        if node.name == self.arguments {
            self.unsupported = true;
        }
    }

    /// Binding identifiers are declarations, not reads. Traverse only the expression-bearing
    /// portions of a parameter pattern so a parameter literally named `arguments` does not create
    /// a false lexical hazard while defaults and computed keys remain visible.
    fn visit_pattern(&mut self, node: &Pattern<'a>) {
        match node {
            Pattern::Ident(_) => {}
            Pattern::Rest(rest) => self.visit_pattern(&rest.argument),
            Pattern::Assignment(assignment) => {
                self.visit_pattern(&assignment.left);
                self.visit_expression(&assignment.right);
            }
            Pattern::Array(array) => {
                for element in array.elements.iter().flatten() {
                    self.visit_pattern(element);
                }
            }
            Pattern::Object(object) => {
                for property in object.properties.iter() {
                    if let PropertyKey::Computed(key) = property.key {
                        self.visit_expression(&key);
                    }
                    self.visit_pattern(&property.value);
                }
                if let Some(rest) = object.rest {
                    self.visit_pattern(&rest.argument);
                }
            }
        }
    }

    // A nested ordinary function owns its own `this`/`arguments`.
    fn visit_function(&mut self, _node: &wake_ecma_ast::Function<'a>) {}
}

fn lower_member_assignment<'a>(
    arena: &'a Bump,
    interner: &Interner,
    receiver_temp: Atom,
    key_temp: Atom,
    value_temp: Atom,
    features: FeatureSet,
    span: Span,
    operator: AssignmentOperator,
    member: &'a MemberExpression<'a>,
    right: Expression<'a>,
) -> Expression<'a> {
    let receiver = Expression::Identifier(arena.alloc(Ident::new(span, receiver_temp)));
    let key = Expression::Identifier(arena.alloc(Ident::new(span, key_temp)));
    let value = Expression::Identifier(arena.alloc(Ident::new(span, value_temp)));
    let captures_receiver = !matches!(member.object, Expression::Super(_));
    let target_object = if captures_receiver {
        receiver
    } else {
        member.object
    };
    let property = match member.property {
        MemberProperty::Computed(_) => MemberProperty::Computed(key),
        property => property,
    };
    let target = Expression::Member(arena.alloc(MemberExpression {
        span: member.span,
        object: target_object,
        property,
        optional: false,
    }));

    let mut expressions = AVec::with_capacity_in(5, arena);
    if captures_receiver {
        expressions.push(assign(arena, span, receiver, member.object));
    }
    if let MemberProperty::Computed(raw_key) = member.property {
        // Computed property access performs ToPropertyKey while creating the reference, before
        // the getter runs. Capture the raw expression once, then overwrite the same collision-free
        // slot with its normalized string/Symbol key. Reconstructing the reference for the later
        // write can no longer invoke user conversion code a second time.
        expressions.push(assign(arena, span, key, raw_key));
        expressions.push(assign(
            arena,
            span,
            key,
            to_property_key(arena, interner, span, key),
        ));
    }
    // All four operators read the property exactly once. This capture is also important when a
    // getter rebinds the source identifier used as the receiver: the eventual write still uses the
    // receiver value captured while the original reference was evaluated.
    expressions.push(assign(arena, span, value, target));

    let core = match operator {
        AssignmentOperator::Exp => {
            let powered = lower_binary(
                arena,
                interner,
                features,
                span,
                BinaryOperator::Exp,
                value,
                right,
            );
            assign(arena, span, target, powered)
        }
        AssignmentOperator::And | AssignmentOperator::Or => {
            let logical = if operator == AssignmentOperator::And {
                LogicalOperator::And
            } else {
                LogicalOperator::Or
            };
            Expression::Logical(arena.alloc(LogicalExpression {
                span,
                operator: logical,
                left: value,
                right: assign(arena, span, target, right),
            }))
        }
        AssignmentOperator::Coalesce => {
            let fallback = assign(arena, span, target, right);
            nullish_value_or(arena, span, value, fallback)
        }
        _ => unreachable!("caller filters member assignment operators"),
    };
    expressions.push(core);
    Expression::Sequence(arena.alloc(SequenceExpression { span, expressions }))
}

fn nullish_value_or<'a>(
    arena: &'a Bump,
    span: Span,
    value: Expression<'a>,
    fallback: Expression<'a>,
) -> Expression<'a> {
    let not_null = strict_not(arena, span, value, Expression::NullLiteral(span));
    let zero =
        Expression::NumberLiteral(arena.alloc(wake_ecma_ast::NumberLiteral { span, value: 0.0 }));
    let undefined = Expression::Unary(arena.alloc(UnaryExpression {
        span,
        operator: UnaryOperator::Void,
        argument: zero,
    }));
    let not_undefined = strict_not(arena, span, value, undefined);
    let test = Expression::Logical(arena.alloc(LogicalExpression {
        span,
        operator: LogicalOperator::And,
        left: not_null,
        right: not_undefined,
    }));
    Expression::Conditional(arena.alloc(ConditionalExpression {
        span,
        test,
        consequent: value,
        alternate: fallback,
    }))
}

/// Lower nullish coalescing. Non-repeatable operands use the supplied collision-free temporary;
/// when the parser cannot safely assign one to the current execution scope, syntax is preserved.
pub fn lower_logical<'a>(
    arena: &'a Bump,
    temp: Option<Atom>,
    features: FeatureSet,
    span: Span,
    operator: LogicalOperator,
    left: Expression<'a>,
    right: Expression<'a>,
) -> Expression<'a> {
    if operator != LogicalOperator::Coalesce || !features.contains(EcmaFeature::NullishCoalescing) {
        return Expression::Logical(arena.alloc(LogicalExpression {
            span,
            operator,
            left,
            right,
        }));
    }

    if !repeatable(left) {
        let Some(temp) = temp else {
            return Expression::Logical(arena.alloc(LogicalExpression {
                span,
                operator,
                left,
                right,
            }));
        };
        let temp_ident = Ident::new(span, temp);
        let temp_expr = Expression::Identifier(arena.alloc(temp_ident));
        if features.contains(EcmaFeature::ArrowFunction) {
            let capture = Expression::Assignment(arena.alloc(AssignmentExpression {
                span,
                operator: AssignmentOperator::Assign,
                left: temp_expr,
                right: left,
            }));
            let conditional = nullish_value_or(arena, span, temp_expr, right);
            let mut expressions = AVec::with_capacity_in(2, arena);
            expressions.push(capture);
            expressions.push(conditional);
            return Expression::Sequence(arena.alloc(SequenceExpression { span, expressions }));
        }
        // A lexical IIFE provides a scope-local temporary without changing `this`/`arguments`
        // when arrow syntax itself remains available in the output target.
        let body = nullish_value_or(arena, span, temp_expr, right);
        let mut params = AVec::with_capacity_in(1, arena);
        params.push(Pattern::Ident(arena.alloc(temp_ident)));
        let arrow = Expression::Arrow(arena.alloc(ArrowFunction {
            span,
            params,
            body: ArrowBody::Expression(body),
            is_async: false,
        }));
        let mut arguments = AVec::with_capacity_in(1, arena);
        arguments.push(left);
        return Expression::Call(arena.alloc(CallExpression {
            span,
            callee: arrow,
            arguments,
            optional: false,
        }));
    }

    let not_null = strict_not(arena, span, left, Expression::NullLiteral(span));
    let zero =
        Expression::NumberLiteral(arena.alloc(wake_ecma_ast::NumberLiteral { span, value: 0.0 }));
    let undefined = Expression::Unary(arena.alloc(UnaryExpression {
        span,
        operator: UnaryOperator::Void,
        argument: zero,
    }));
    let not_undefined = strict_not(arena, span, left, undefined);
    let test = Expression::Logical(arena.alloc(LogicalExpression {
        span,
        operator: LogicalOperator::And,
        left: not_null,
        right: not_undefined,
    }));
    Expression::Conditional(arena.alloc(ConditionalExpression {
        span,
        test,
        consequent: left,
        alternate: right,
    }))
}

/// Lower a complete optional member/call chain after the parser has consumed its full tail.
///
/// Handling the whole chain is essential: `a?.b.c` must short-circuit the `.c` access as well.
/// `force_sequence_capture` means `temporaries` are declared by the owning execution scope, so
/// captures must stay as sequences there instead of moving expressions into a lexical IIFE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionalChainMode {
    Value,
    Delete,
}

pub fn lower_optional_chain<'a>(
    arena: &'a Bump,
    interner: &Interner,
    spread_helper: Option<Atom>,
    temporaries: Option<[Atom; 2]>,
    force_sequence_capture: bool,
    call_atom: Atom,
    features: FeatureSet,
    mode: OptionalChainMode,
    expression: Expression<'a>,
) -> Expression<'a> {
    lower_optional_chain_impl(
        arena,
        interner,
        spread_helper,
        temporaries,
        force_sequence_capture,
        call_atom,
        features,
        mode,
        expression,
        None,
    )
}

/// Lower the optional chain inside a parenthesized call callee while retaining its Reference
/// receiver. The result is an always-callable forwarding function: this deliberately postpones
/// a null/non-callable TypeError until after the outer call arguments (including spread
/// iterators) have been evaluated, matching `(obj?.method)(arguments)` evaluation order.
pub fn lower_parenthesized_optional_callee<'a>(
    arena: &'a Bump,
    interner: &Interner,
    spread_helper: Option<Atom>,
    [function_temp, receiver_temp]: [Atom; 2],
    call_atom: Atom,
    features: FeatureSet,
    expression: Expression<'a>,
) -> Expression<'a> {
    if !features.contains(EcmaFeature::OptionalChaining) {
        return expression;
    }

    let lowered = lower_optional_chain_impl(
        arena,
        interner,
        spread_helper,
        Some([function_temp, receiver_temp]),
        true,
        call_atom,
        features,
        OptionalChainMode::Value,
        expression,
        Some(receiver_temp),
    );
    let span = expression.span();
    let function = Expression::Identifier(arena.alloc(Ident::new(span, function_temp)));
    let receiver = Expression::Identifier(arena.alloc(Ident::new(span, receiver_temp)));
    let forwarding = forwarding_callee(arena, interner, span, function, receiver);
    let mut expressions = AVec::with_capacity_in(2, arena);
    expressions.push(assign(arena, span, function, lowered));
    expressions.push(forwarding);
    Expression::Sequence(arena.alloc(SequenceExpression { span, expressions }))
}

#[allow(clippy::too_many_arguments)]
fn lower_optional_chain_impl<'a>(
    arena: &'a Bump,
    interner: &Interner,
    spread_helper: Option<Atom>,
    temporaries: Option<[Atom; 2]>,
    force_sequence_capture: bool,
    call_atom: Atom,
    features: FeatureSet,
    mode: OptionalChainMode,
    expression: Expression<'a>,
    final_receiver: Option<Atom>,
) -> Expression<'a> {
    if !features.contains(EcmaFeature::OptionalChaining) {
        return expression;
    }

    // The parser supplies scope-owned `var` bindings only in Program/function/synchronous-arrow
    // execution scopes. A missing pair means this is a cover/parameter/class/async-arrow region
    // without either a safe scope capture or a valid lexical-IIFE fallback; retaining the original
    // chain is safer than leaking a binding into an outer scope.
    let Some([temp, receiver_temp]) = temporaries else {
        return expression;
    };

    let mut operations = Vec::new();
    let base = flatten_chain(expression, &mut operations);
    let Some(index) = operations.iter().position(|operation| operation.optional()) else {
        return finish_optional_chain(
            arena,
            mode,
            expression,
            final_receiver.map(|receiver| (temp, receiver)),
        );
    };

    let tested = apply_chain(
        arena,
        interner,
        spread_helper,
        [temp, receiver_temp],
        force_sequence_capture,
        features,
        base,
        &operations[..index],
    );
    // `obj.method?.()` needs both the function value and the exact receiver value that was used
    // to read it. Capturing even an identifier receiver matters: its getter may rebind that name
    // before the call happens. `super` itself is never materialized as a value; its receiver is
    // the current lexical `this`.
    if let (ChainOperation::Call(call), Expression::Member(member)) = (operations[index], tested) {
        let span = call.span;
        let captures_receiver = !matches!(member.object, Expression::Super(_));
        let receiver = if captures_receiver {
            Expression::Identifier(arena.alloc(Ident::new(span, receiver_temp)))
        } else {
            Expression::This(span)
        };
        let function = if captures_receiver {
            Expression::Member(arena.alloc(MemberExpression {
                span: member.span,
                object: receiver,
                property: member.property,
                optional: false,
            }))
        } else {
            tested
        };
        let value = Expression::Identifier(arena.alloc(Ident::new(span, temp)));
        let called = call_with_receiver(
            arena,
            interner,
            spread_helper,
            features,
            span,
            call_atom,
            value,
            receiver,
            call,
        );
        let alternate = apply_chain(
            arena,
            interner,
            spread_helper,
            [temp, receiver_temp],
            force_sequence_capture,
            features,
            called,
            &operations[index + 1..],
        );
        let alternate = lower_optional_chain_impl(
            arena,
            interner,
            spread_helper,
            Some([temp, receiver_temp]),
            force_sequence_capture,
            call_atom,
            features,
            mode,
            alternate,
            final_receiver,
        );
        let conditional = nullish_conditional(arena, span, value, alternate, mode);
        let inner = capture_optional_value(
            arena,
            span,
            temp,
            conditional,
            function,
            features,
            force_sequence_capture,
        );
        if captures_receiver {
            return capture_optional_value(
                arena,
                span,
                receiver_temp,
                inner,
                member.object,
                features,
                force_sequence_capture,
            );
        }
        return inner;
    }

    let span = operations[index].span();
    let value = if repeatable(tested) {
        tested
    } else {
        Expression::Identifier(arena.alloc(Ident::new(span, temp)))
    };
    let first = operations[index].without_optional();
    let alternate = apply_chain(
        arena,
        interner,
        spread_helper,
        [temp, receiver_temp],
        force_sequence_capture,
        features,
        value,
        std::slice::from_ref(&first),
    );
    let alternate = apply_chain(
        arena,
        interner,
        spread_helper,
        [temp, receiver_temp],
        force_sequence_capture,
        features,
        alternate,
        &operations[index + 1..],
    );
    let alternate = lower_optional_chain_impl(
        arena,
        interner,
        spread_helper,
        Some([temp, receiver_temp]),
        force_sequence_capture,
        call_atom,
        features,
        mode,
        alternate,
        final_receiver,
    );
    let conditional = nullish_conditional(arena, span, value, alternate, mode);

    if repeatable(tested) {
        return conditional;
    }

    capture_optional_value(
        arena,
        span,
        temp,
        conditional,
        tested,
        features,
        force_sequence_capture,
    )
}

fn finish_optional_chain<'a>(
    arena: &'a Bump,
    mode: OptionalChainMode,
    mut expression: Expression<'a>,
    callee_temporaries: Option<(Atom, Atom)>,
) -> Expression<'a> {
    if let Some((value_temp, receiver_temp)) = callee_temporaries {
        let span = expression.span();
        let receiver = Expression::Identifier(arena.alloc(Ident::new(span, receiver_temp)));
        expression = if let Expression::Member(member) = expression {
            let captured_object = if matches!(member.object, Expression::Super(_)) {
                Expression::This(span)
            } else {
                member.object
            };
            let value = if matches!(member.object, Expression::Super(_)) {
                expression
            } else {
                Expression::Member(arena.alloc(MemberExpression {
                    span: member.span,
                    object: receiver,
                    property: member.property,
                    optional: false,
                }))
            };
            let mut expressions = AVec::with_capacity_in(2, arena);
            expressions.push(assign(arena, span, receiver, captured_object));
            expressions.push(value);
            Expression::Sequence(arena.alloc(SequenceExpression { span, expressions }))
        } else {
            // A chain ending in a call/value has no Reference receiver. Evaluate it before
            // clearing the receiver because its own lowering may still use the same scratch temp.
            let value = Expression::Identifier(arena.alloc(Ident::new(span, value_temp)));
            let mut expressions = AVec::with_capacity_in(3, arena);
            expressions.push(assign(arena, span, value, expression));
            expressions.push(assign(
                arena,
                span,
                receiver,
                undefined_expression(arena, span),
            ));
            expressions.push(value);
            Expression::Sequence(arena.alloc(SequenceExpression { span, expressions }))
        };
    }
    match mode {
        OptionalChainMode::Value => expression,
        OptionalChainMode::Delete => Expression::Unary(arena.alloc(UnaryExpression {
            span: expression.span(),
            operator: UnaryOperator::Delete,
            argument: expression,
        })),
    }
}

fn capture_optional_value<'a>(
    arena: &'a Bump,
    span: Span,
    temp: Atom,
    body: Expression<'a>,
    argument: Expression<'a>,
    features: FeatureSet,
    force_sequence_capture: bool,
) -> Expression<'a> {
    if force_sequence_capture || features.contains(EcmaFeature::ArrowFunction) {
        let target = Expression::Identifier(arena.alloc(Ident::new(span, temp)));
        let mut expressions = AVec::with_capacity_in(2, arena);
        expressions.push(assign(arena, span, target, argument));
        expressions.push(body);
        Expression::Sequence(arena.alloc(SequenceExpression { span, expressions }))
    } else {
        lexical_iife(arena, span, temp, body, argument)
    }
}

fn lexical_iife<'a>(
    arena: &'a Bump,
    span: Span,
    temp: Atom,
    body: Expression<'a>,
    argument: Expression<'a>,
) -> Expression<'a> {
    let ident = Ident::new(span, temp);
    let mut params = AVec::with_capacity_in(1, arena);
    params.push(Pattern::Ident(arena.alloc(ident)));
    let arrow = Expression::Arrow(arena.alloc(ArrowFunction {
        span,
        params,
        body: ArrowBody::Expression(body),
        is_async: false,
    }));
    let mut arguments = AVec::with_capacity_in(1, arena);
    arguments.push(argument);
    Expression::Call(arena.alloc(CallExpression {
        span,
        callee: arrow,
        arguments,
        optional: false,
    }))
}

fn forwarding_callee<'a>(
    arena: &'a Bump,
    interner: &Interner,
    span: Span,
    function: Expression<'a>,
    receiver: Expression<'a>,
) -> Expression<'a> {
    let arguments =
        Expression::Identifier(arena.alloc(Ident::new(span, interner.intern("arguments"))));
    let call = call_apply(arena, interner, span, function, receiver, arguments);
    let mut statements = AVec::with_capacity_in(1, arena);
    statements.push(Statement::Return(arena.alloc(ReturnStatement {
        span,
        argument: Some(call),
    })));
    Expression::Function(arena.alloc(Function {
        span,
        id: None,
        params: AVec::new_in(arena),
        body: Some(arena.alloc(FunctionBody {
            span,
            statements,
            strict: false,
        })),
        is_async: false,
        is_generator: false,
    }))
}

fn call_with_receiver<'a>(
    arena: &'a Bump,
    interner: &Interner,
    spread_helper: Option<Atom>,
    features: FeatureSet,
    span: Span,
    call_atom: Atom,
    function: Expression<'a>,
    receiver: Expression<'a>,
    original: &'a CallExpression<'a>,
) -> Expression<'a> {
    if features.contains(EcmaFeature::Spread)
        && original
            .arguments
            .iter()
            .any(|argument| matches!(argument, Expression::Spread(_)))
    {
        let helper = spread_helper.expect("spread helper allocated before optional lowering");
        let apply_member = member(arena, interner, span, function, "apply");
        let spread_arguments =
            spread_arguments_array(arena, interner, helper, span, &original.arguments);
        let mut arguments = AVec::with_capacity_in(2, arena);
        arguments.push(receiver);
        arguments.push(spread_arguments);
        return Expression::Call(arena.alloc(CallExpression {
            span,
            callee: apply_member,
            arguments,
            optional: false,
        }));
    }
    let call_member = Expression::Member(arena.alloc(MemberExpression {
        span,
        object: function,
        property: MemberProperty::Ident(Ident::new(span, call_atom)),
        optional: false,
    }));
    let mut arguments = AVec::with_capacity_in(original.arguments.len() + 1, arena);
    arguments.push(receiver);
    arguments.extend(original.arguments.iter().copied());
    Expression::Call(arena.alloc(CallExpression {
        span,
        callee: call_member,
        arguments,
        optional: false,
    }))
}

pub fn has_optional_chain(expression: Expression<'_>) -> bool {
    match expression {
        Expression::Member(member) => member.optional || has_optional_chain(member.object),
        Expression::Call(call) => call.optional || has_optional_chain(call.callee),
        _ => false,
    }
}

pub fn has_call_spread(expression: Expression<'_>) -> bool {
    match expression {
        Expression::Member(member) => has_call_spread(member.object),
        Expression::Call(call) => {
            call.arguments
                .iter()
                .any(|argument| matches!(argument, Expression::Spread(_)))
                || has_call_spread(call.callee)
        }
        _ => false,
    }
}

#[derive(Clone, Copy)]
enum ChainOperation<'a> {
    Member(&'a MemberExpression<'a>),
    Call(&'a CallExpression<'a>),
}

impl ChainOperation<'_> {
    fn optional(self) -> bool {
        match self {
            Self::Member(member) => member.optional,
            Self::Call(call) => call.optional,
        }
    }

    fn span(self) -> Span {
        match self {
            Self::Member(member) => member.span,
            Self::Call(call) => call.span,
        }
    }

    fn without_optional(self) -> Self {
        self
    }
}

fn flatten_chain<'a>(
    expression: Expression<'a>,
    operations: &mut Vec<ChainOperation<'a>>,
) -> Expression<'a> {
    match expression {
        Expression::Member(member) => {
            let base = flatten_chain(member.object, operations);
            operations.push(ChainOperation::Member(member));
            base
        }
        Expression::Call(call) => {
            let base = flatten_chain(call.callee, operations);
            operations.push(ChainOperation::Call(call));
            base
        }
        _ => expression,
    }
}

fn apply_chain<'a>(
    arena: &'a Bump,
    interner: &Interner,
    spread_helper: Option<Atom>,
    temporaries: [Atom; 2],
    force_sequence_capture: bool,
    features: FeatureSet,
    mut expression: Expression<'a>,
    operations: &[ChainOperation<'a>],
) -> Expression<'a> {
    for operation in operations {
        expression = match *operation {
            ChainOperation::Member(member) => Expression::Member(arena.alloc(MemberExpression {
                span: member.span,
                object: expression,
                property: member.property,
                optional: false,
            })),
            ChainOperation::Call(call) => {
                if features.contains(EcmaFeature::Spread)
                    && call
                        .arguments
                        .iter()
                        .any(|argument| matches!(argument, Expression::Spread(_)))
                {
                    let helper =
                        spread_helper.expect("spread helper allocated before optional lowering");
                    lower_optional_chain_spread_call(
                        arena,
                        interner,
                        helper,
                        temporaries,
                        force_sequence_capture,
                        features,
                        expression,
                        call,
                    )
                } else {
                    let mut arguments = AVec::with_capacity_in(call.arguments.len(), arena);
                    arguments.extend(call.arguments.iter().copied());
                    Expression::Call(arena.alloc(CallExpression {
                        span: call.span,
                        callee: expression,
                        arguments,
                        optional: false,
                    }))
                }
            }
        };
    }
    expression
}

fn lower_optional_chain_spread_call<'a>(
    arena: &'a Bump,
    interner: &Interner,
    helper: Atom,
    [function_temp, receiver_temp]: [Atom; 2],
    force_sequence_capture: bool,
    features: FeatureSet,
    callee: Expression<'a>,
    call: &'a CallExpression<'a>,
) -> Expression<'a> {
    let spread_arguments =
        spread_arguments_array(arena, interner, helper, call.span, &call.arguments);

    if let Expression::Member(member_expression) = callee {
        if matches!(member_expression.object, Expression::Super(_)) {
            let function =
                Expression::Identifier(arena.alloc(Ident::new(call.span, function_temp)));
            let applied = call_apply(
                arena,
                interner,
                call.span,
                function,
                Expression::This(call.span),
                spread_arguments,
            );
            return capture_optional_value(
                arena,
                call.span,
                function_temp,
                applied,
                callee,
                features,
                force_sequence_capture,
            );
        }

        let receiver = Expression::Identifier(arena.alloc(Ident::new(call.span, receiver_temp)));
        let function = Expression::Identifier(arena.alloc(Ident::new(call.span, function_temp)));
        let member = Expression::Member(arena.alloc(MemberExpression {
            span: member_expression.span,
            object: receiver,
            property: member_expression.property,
            optional: false,
        }));
        let applied = call_apply(
            arena,
            interner,
            call.span,
            function,
            receiver,
            spread_arguments,
        );
        let with_function = capture_optional_value(
            arena,
            call.span,
            function_temp,
            applied,
            member,
            features,
            force_sequence_capture,
        );
        return capture_optional_value(
            arena,
            call.span,
            receiver_temp,
            with_function,
            member_expression.object,
            features,
            force_sequence_capture,
        );
    }

    // A direct call has an undefined receiver. `null` would be observably different for strict
    // functions, even though sloppy functions coerce both values to the global object.
    call_apply(
        arena,
        interner,
        call.span,
        callee,
        undefined_expression(arena, call.span),
        spread_arguments,
    )
}

fn call_apply<'a>(
    arena: &'a Bump,
    interner: &Interner,
    span: Span,
    function: Expression<'a>,
    receiver: Expression<'a>,
    arguments_array: Expression<'a>,
) -> Expression<'a> {
    let apply = member(arena, interner, span, function, "apply");
    let mut arguments = AVec::with_capacity_in(2, arena);
    arguments.push(receiver);
    arguments.push(arguments_array);
    Expression::Call(arena.alloc(CallExpression {
        span,
        callee: apply,
        arguments,
        optional: false,
    }))
}

fn undefined_expression<'a>(arena: &'a Bump, span: Span) -> Expression<'a> {
    let zero =
        Expression::NumberLiteral(arena.alloc(wake_ecma_ast::NumberLiteral { span, value: 0.0 }));
    Expression::Unary(arena.alloc(UnaryExpression {
        span,
        operator: UnaryOperator::Void,
        argument: zero,
    }))
}

fn nullish_conditional<'a>(
    arena: &'a Bump,
    span: Span,
    value: Expression<'a>,
    alternate: Expression<'a>,
    mode: OptionalChainMode,
) -> Expression<'a> {
    let is_null = Expression::Binary(arena.alloc(BinaryExpression {
        span,
        operator: BinaryOperator::StrictEq,
        left: value,
        right: Expression::NullLiteral(span),
    }));
    let zero =
        Expression::NumberLiteral(arena.alloc(wake_ecma_ast::NumberLiteral { span, value: 0.0 }));
    let undefined = Expression::Unary(arena.alloc(UnaryExpression {
        span,
        operator: UnaryOperator::Void,
        argument: zero,
    }));
    let is_undefined = Expression::Binary(arena.alloc(BinaryExpression {
        span,
        operator: BinaryOperator::StrictEq,
        left: value,
        right: undefined,
    }));
    let test = Expression::Logical(arena.alloc(LogicalExpression {
        span,
        operator: LogicalOperator::Or,
        left: is_null,
        right: is_undefined,
    }));
    let consequent = match mode {
        OptionalChainMode::Value => undefined,
        OptionalChainMode::Delete => Expression::BooleanLiteral(
            arena.alloc(wake_ecma_ast::BooleanLiteral { span, value: true }),
        ),
    };
    Expression::Conditional(arena.alloc(ConditionalExpression {
        span,
        test,
        consequent,
        alternate,
    }))
}

fn repeatable(expression: Expression<'_>) -> bool {
    matches!(
        expression,
        Expression::Identifier(_)
            | Expression::NumberLiteral(_)
            | Expression::StringLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
            | Expression::BigIntLiteral(_)
            | Expression::This(_)
    )
}

/// Whether reusing an expression duplicates no observable work.
pub fn is_repeatable(expression: Expression<'_>) -> bool {
    repeatable(expression)
}

fn assign<'a>(
    arena: &'a Bump,
    span: Span,
    left: Expression<'a>,
    right: Expression<'a>,
) -> Expression<'a> {
    Expression::Assignment(arena.alloc(AssignmentExpression {
        span,
        operator: AssignmentOperator::Assign,
        left,
        right,
    }))
}

fn strict_not<'a>(
    arena: &'a Bump,
    span: Span,
    left: Expression<'a>,
    right: Expression<'a>,
) -> Expression<'a> {
    Expression::Binary(arena.alloc(BinaryExpression {
        span,
        operator: BinaryOperator::StrictNotEq,
        left,
        right,
    }))
}

impl TargetEnv {
    /// 构建目标，规范化浏览器别名，并将同一浏览器的单调 `>=` 结果折叠到最低版本。
    pub fn new(targets: Vec<BrowserTarget>) -> Self {
        let mut targets = targets
            .into_iter()
            .map(|target| BrowserTarget::new(target.name, target.version))
            .collect::<Vec<_>>();
        targets.sort_by(compare_browser_targets);
        targets.dedup();

        let mut normalized: Vec<BrowserTarget> = Vec::with_capacity(targets.len());
        for target in targets {
            let parsed = BrowserVersion::parse(&target.version);
            let already_has_numeric_floor = parsed.is_some()
                && normalized.last().is_some_and(|previous| {
                    previous.name == target.name
                        && BrowserVersion::parse(&previous.version).is_some()
                });
            if !already_has_numeric_floor {
                normalized.push(target);
            }
        }

        let mut required = FeatureSet::default();
        for feature in EcmaFeature::ALL {
            if normalized.iter().any(|target| !supports(target, feature)) {
                required.insert(feature);
            }
        }
        Self {
            targets: normalized,
            required,
        }
    }

    /// Wake 的零配置 Web 基线。
    pub fn baseline() -> Self {
        Self::new(
            MODERN_BROWSER_BASELINE
                .into_iter()
                .map(|(name, version)| BrowserTarget::new(name, version))
                .collect(),
        )
    }

    /// 显式 ESNext 模式：无运行时目标，不因兼容性自动启用 lowering。
    pub fn modern() -> Self {
        Self::new(Vec::new())
    }

    /// [`Self::modern`] 的语义更明确别名。
    pub fn esnext() -> Self {
        Self::modern()
    }

    pub fn targets(&self) -> &[BrowserTarget] {
        &self.targets
    }

    pub const fn required_features(&self) -> FeatureSet {
        self.required
    }

    /// 应用类似 preset-env 的 include/exclude。include 强制启用，exclude 最后生效。
    pub fn apply_overrides(
        &mut self,
        include: impl IntoIterator<Item = impl AsRef<str>>,
        exclude: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<(), String> {
        for name in include {
            let raw = name.as_ref();
            let feature = EcmaFeature::from_babel_plugin(raw)
                .ok_or_else(|| format!("未知 transform include `{raw}`"))?;
            self.required.insert(feature);
        }
        for name in exclude {
            let raw = name.as_ref();
            let feature = EcmaFeature::from_babel_plugin(raw)
                .ok_or_else(|| format!("未知 transform exclude `{raw}`"))?;
            self.required.remove(feature);
        }
        Ok(())
    }

    /// 确定性的 FNV-1a 指纹，供任务键组合；不使用进程随机种子的 `Hash`。
    pub fn fingerprint(&self) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in COMPAT_DATA_VERSION
            .to_le_bytes()
            .into_iter()
            .chain(self.required.bits().to_le_bytes())
            .chain(self.targets.iter().flat_map(|t| {
                t.name
                    .bytes()
                    .chain([0])
                    .chain(t.version.bytes())
                    .chain([0xff])
            }))
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }
}

fn compare_browser_targets(left: &BrowserTarget, right: &BrowserTarget) -> Ordering {
    left.name.cmp(&right.name).then_with(|| {
        match (
            BrowserVersion::parse(&left.version),
            BrowserVersion::parse(&right.version),
        ) {
            (Some(left_version), Some(right_version)) => left_version
                .cmp(&right_version)
                .then_with(|| left.version.cmp(&right.version)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => left.version.cmp(&right.version),
        }
    })
}

fn supports(target: &BrowserTarget, feature: EcmaFeature) -> bool {
    let Some(version) = BrowserVersion::parse(&target.version) else {
        return false;
    };
    if target.name == "node" && feature == EcmaFeature::ForOf {
        return version >= BrowserVersion::new(6, 5, 0);
    }
    let minimum = match (target.name.as_str(), feature) {
        ("chrome", EcmaFeature::ArrowFunction) => 45,
        ("chrome", EcmaFeature::TemplateLiteral) => 41,
        ("chrome", EcmaFeature::ShorthandProperties) => 43,
        ("chrome", EcmaFeature::FunctionParameters) => 49,
        ("chrome", EcmaFeature::ExponentiationOperator) => 52,
        ("chrome", EcmaFeature::AsyncAwait) => 55,
        ("chrome", EcmaFeature::ObjectRestSpread) => 60,
        ("chrome", EcmaFeature::OptionalCatchBinding) => 66,
        ("chrome", EcmaFeature::OptionalChaining) => 80,
        ("chrome", EcmaFeature::NullishCoalescing) => 80,
        ("chrome", EcmaFeature::LogicalAssignment) => 85,
        ("chrome", EcmaFeature::ClassFields) => 72,
        ("chrome", EcmaFeature::PrivateFields) => 74,
        ("chrome", EcmaFeature::ClassStaticBlock) => 94,
        ("chrome", EcmaFeature::Spread) => 46,
        ("chrome", EcmaFeature::Destructuring) => 49,
        ("chrome", EcmaFeature::ForOf) => 51,

        ("firefox", EcmaFeature::ArrowFunction) => 22,
        ("firefox", EcmaFeature::TemplateLiteral) => 34,
        ("firefox", EcmaFeature::ShorthandProperties) => 33,
        ("firefox", EcmaFeature::FunctionParameters) => 15,
        ("firefox", EcmaFeature::ExponentiationOperator) => 52,
        ("firefox", EcmaFeature::AsyncAwait) => 52,
        ("firefox", EcmaFeature::ObjectRestSpread) => 55,
        ("firefox", EcmaFeature::OptionalCatchBinding) => 58,
        ("firefox", EcmaFeature::OptionalChaining) => 74,
        ("firefox", EcmaFeature::NullishCoalescing) => 72,
        ("firefox", EcmaFeature::LogicalAssignment) => 79,
        ("firefox", EcmaFeature::ClassFields) => 69,
        ("firefox", EcmaFeature::PrivateFields) => 90,
        ("firefox", EcmaFeature::ClassStaticBlock) => 93,
        ("firefox", EcmaFeature::Spread) => 27,
        ("firefox", EcmaFeature::Destructuring) => 41,
        ("firefox", EcmaFeature::ForOf) => 53,

        ("safari", EcmaFeature::ArrowFunction) => 10,
        ("safari", EcmaFeature::TemplateLiteral) => 9,
        ("safari", EcmaFeature::ShorthandProperties) => 9,
        ("safari", EcmaFeature::FunctionParameters) => 10,
        ("safari", EcmaFeature::ExponentiationOperator) => 10,
        ("safari", EcmaFeature::AsyncAwait) => 11,
        ("safari", EcmaFeature::ObjectRestSpread) => 11,
        ("safari", EcmaFeature::OptionalCatchBinding) => 11,
        ("safari", EcmaFeature::OptionalChaining) => 14,
        ("safari", EcmaFeature::NullishCoalescing) => 14,
        ("safari", EcmaFeature::LogicalAssignment) => 14,
        ("safari", EcmaFeature::ClassFields) => 14,
        ("safari", EcmaFeature::PrivateFields) => 15,
        ("safari", EcmaFeature::ClassStaticBlock) => 16,
        ("safari", EcmaFeature::Spread) => 10,
        ("safari", EcmaFeature::Destructuring) => 8,
        ("safari", EcmaFeature::ForOf) => 10,

        // iOS Safari is a distinct Browserslist family (`ios_saf`). The current syntax-only
        // thresholds mirror the corresponding WebKit/Safari releases, but stay as separate rows
        // so platform-specific bugfix data can diverge without changing target normalization.
        ("ios", EcmaFeature::ArrowFunction) => 10,
        ("ios", EcmaFeature::TemplateLiteral) => 9,
        ("ios", EcmaFeature::ShorthandProperties) => 9,
        ("ios", EcmaFeature::FunctionParameters) => 10,
        ("ios", EcmaFeature::ExponentiationOperator) => 10,
        ("ios", EcmaFeature::AsyncAwait) => 11,
        ("ios", EcmaFeature::ObjectRestSpread) => 11,
        ("ios", EcmaFeature::OptionalCatchBinding) => 11,
        ("ios", EcmaFeature::OptionalChaining) => 14,
        ("ios", EcmaFeature::NullishCoalescing) => 14,
        ("ios", EcmaFeature::LogicalAssignment) => 14,
        ("ios", EcmaFeature::ClassFields) => 14,
        ("ios", EcmaFeature::PrivateFields) => 15,
        ("ios", EcmaFeature::ClassStaticBlock) => 16,
        ("ios", EcmaFeature::Spread) => 10,
        ("ios", EcmaFeature::Destructuring) => 8,
        ("ios", EcmaFeature::ForOf) => 10,

        ("edge", EcmaFeature::ArrowFunction) => 12,
        ("edge", EcmaFeature::TemplateLiteral) => 12,
        ("edge", EcmaFeature::ShorthandProperties) => 12,
        ("edge", EcmaFeature::FunctionParameters) => 14,
        ("edge", EcmaFeature::ExponentiationOperator) => 14,
        ("edge", EcmaFeature::AsyncAwait) => 15,
        ("edge", EcmaFeature::ObjectRestSpread) => 79,
        ("edge", EcmaFeature::OptionalCatchBinding) => 79,
        ("edge", EcmaFeature::OptionalChaining) => 80,
        ("edge", EcmaFeature::NullishCoalescing) => 80,
        ("edge", EcmaFeature::LogicalAssignment) => 85,
        ("edge", EcmaFeature::ClassFields) => 79,
        ("edge", EcmaFeature::PrivateFields) => 84,
        ("edge", EcmaFeature::ClassStaticBlock) => 94,
        ("edge", EcmaFeature::Spread) => 12,
        ("edge", EcmaFeature::Destructuring) => 14,
        ("edge", EcmaFeature::ForOf) => 15,

        // 除明确记录的小版本边界外，Node/V8 能力按主版本建模。
        ("node", EcmaFeature::ArrowFunction) => 6,
        ("node", EcmaFeature::TemplateLiteral) => 4,
        ("node", EcmaFeature::ShorthandProperties) => 4,
        ("node", EcmaFeature::FunctionParameters) => 6,
        ("node", EcmaFeature::ExponentiationOperator) => 7,
        ("node", EcmaFeature::AsyncAwait) => 8,
        ("node", EcmaFeature::ObjectRestSpread) => 8,
        ("node", EcmaFeature::OptionalCatchBinding) => 10,
        ("node", EcmaFeature::OptionalChaining) => 14,
        ("node", EcmaFeature::NullishCoalescing) => 14,
        ("node", EcmaFeature::LogicalAssignment) => 16,
        ("node", EcmaFeature::ClassFields) => 12,
        ("node", EcmaFeature::PrivateFields) => 14,
        ("node", EcmaFeature::ClassStaticBlock) => 16,
        ("node", EcmaFeature::Spread) => 5,
        ("node", EcmaFeature::Destructuring) => 6,
        // Node for-of 的 6.5 小版本边界在 match 之前单独处理。
        ("node", EcmaFeature::ForOf) => 6,
        // 未建模的目标必须保守地要求 pass。
        _ => return false,
    };
    version.major >= minimum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parenthesized_optional_callee_captures_receiver_and_returns_forwarder() {
        let arena = Bump::new();
        let interner = Interner::new();
        let span = Span::new(0, 12);
        let object =
            Expression::Identifier(arena.alloc(Ident::new(span, interner.intern("object"))));
        let expression = Expression::Member(arena.alloc(MemberExpression {
            span,
            object,
            property: MemberProperty::Ident(Ident::new(span, interner.intern("method"))),
            optional: true,
        }));
        let mut features = FeatureSet::default();
        features.insert(EcmaFeature::OptionalChaining);
        let lowered = lower_parenthesized_optional_callee(
            &arena,
            &interner,
            None,
            [interner.intern("__function"), interner.intern("__receiver")],
            interner.intern("call"),
            features,
            expression,
        );

        let Expression::Sequence(sequence) = lowered else {
            panic!("callee lowering must capture a value before returning a forwarder")
        };
        assert!(matches!(
            sequence.expressions.first(),
            Some(Expression::Assignment(_))
        ));
        let Some(Expression::Function(forwarder)) = sequence.expressions.last() else {
            panic!("callee lowering must always return a callable forwarder")
        };
        assert!(matches!(
            forwarder.body.and_then(|body| body.statements.first()),
            Some(Statement::Return(_))
        ));
        assert!(!has_optional_chain(lowered));
    }

    #[test]
    fn direct_super_spread_is_preserved_without_class_lowering() {
        let arena = Bump::new();
        let interner = Interner::new();
        let span = Span::new(0, 1);
        let value =
            Expression::Identifier(arena.alloc(Ident::new(span, interner.intern("arguments"))));
        let mut arguments = AVec::new_in(&arena);
        arguments.push(Expression::Spread(arena.alloc(
            wake_ecma_ast::SpreadElement {
                span,
                argument: value,
            },
        )));
        let call = arena.alloc(CallExpression {
            span,
            callee: Expression::Super(span),
            arguments,
            optional: false,
        });
        let mut features = FeatureSet::default();
        features.insert(EcmaFeature::Spread);

        let lowered = lower_call_spread(
            &arena,
            &interner,
            interner.intern("__spread"),
            None,
            features,
            call,
        );

        let Expression::Call(lowered_call) = lowered else {
            panic!("super spread must remain a call");
        };
        assert!(std::ptr::eq(call, lowered_call));
        assert!(matches!(lowered_call.callee, Expression::Super(_)));
    }

    #[test]
    fn lexical_super_scan_crosses_arrows_but_stops_at_ordinary_functions() {
        let arena = Bump::new();
        let span = Span::new(0, 1);

        let mut nested_statements = AVec::new_in(&arena);
        nested_statements.push(Statement::Return(arena.alloc(ReturnStatement {
            span,
            argument: Some(Expression::Super(span)),
        })));
        let nested = arena.alloc(Function {
            span,
            id: None,
            params: AVec::new_in(&arena),
            body: Some(arena.alloc(FunctionBody {
                span,
                statements: nested_statements,
                strict: false,
            })),
            is_async: false,
            is_generator: false,
        });
        let mut outer_statements = AVec::new_in(&arena);
        outer_statements.push(Statement::FunctionDeclaration(nested));
        let outer = Function {
            span,
            id: None,
            params: AVec::new_in(&arena),
            body: Some(arena.alloc(FunctionBody {
                span,
                statements: outer_statements,
                strict: false,
            })),
            is_async: false,
            is_generator: false,
        };
        assert!(!object_method_uses_lexical_super(&outer));

        let arrow = Expression::Arrow(arena.alloc(ArrowFunction {
            span,
            params: AVec::new_in(&arena),
            body: ArrowBody::Expression(Expression::Super(span)),
            is_async: false,
        }));
        let mut arrow_statements = AVec::new_in(&arena);
        arrow_statements.push(Statement::Expression(arena.alloc(ExpressionStatement {
            span,
            expression: arrow,
        })));
        let outer_with_arrow = Function {
            span,
            id: None,
            params: AVec::new_in(&arena),
            body: Some(arena.alloc(FunctionBody {
                span,
                statements: arrow_statements,
                strict: false,
            })),
            is_async: false,
            is_generator: false,
        };
        assert!(object_method_uses_lexical_super(&outer_with_arrow));
    }

    #[test]
    fn old_and_modern_targets_select_different_passes() {
        let old = TargetEnv::new(vec![BrowserTarget::new("chrome", "49")]);
        assert!(
            old.required_features()
                .contains(EcmaFeature::OptionalChaining)
        );
        assert!(
            old.required_features()
                .contains(EcmaFeature::ExponentiationOperator)
        );

        let modern = TargetEnv::new(vec![BrowserTarget::new("chrome", "120")]);
        assert_eq!(modern.required_features().bits(), 0);
    }

    #[test]
    fn default_baseline_is_explicit_normalized_and_feature_free() {
        let baseline = TargetEnv::baseline();
        assert_eq!(TargetEnv::default(), baseline);
        assert_eq!(
            baseline.targets(),
            [
                BrowserTarget::new("chrome", "120"),
                BrowserTarget::new("edge", "120"),
                BrowserTarget::new("firefox", "121"),
                BrowserTarget::new("ios", "17.2"),
                BrowserTarget::new("safari", "17.2"),
            ]
        );
        assert_eq!(baseline.required_features().bits(), 0);
        assert!(TargetEnv::esnext().targets().is_empty());
        assert_eq!(TargetEnv::esnext(), TargetEnv::modern());

        let expanded = TargetEnv::new(vec![
            BrowserTarget::new("chrome", "121"),
            BrowserTarget::new("chrome", "120"),
            BrowserTarget::new("edge", "121"),
            BrowserTarget::new("edge", "120"),
            BrowserTarget::new("firefox", "122"),
            BrowserTarget::new("firefox", "121"),
            BrowserTarget::new("safari", "17.3"),
            BrowserTarget::new("safari", "17.2"),
            BrowserTarget::new("ios_saf", "17.3"),
            BrowserTarget::new("ios", "17.2"),
        ]);
        assert_eq!(expanded, baseline);
        assert_eq!(expanded.fingerprint(), baseline.fingerprint());
    }

    #[test]
    fn version_ranges_minor_boundaries_and_ios_aliases_are_normalized() {
        assert!(
            BrowserVersion::parse("17.1") < BrowserVersion::parse("17.2"),
            "minor versions must participate in ordering"
        );
        assert_eq!(
            BrowserVersion::parse("17.2-17.3"),
            BrowserVersion::parse("17.2")
        );
        assert_eq!(
            BrowserVersion::parse("17.2.0"),
            BrowserVersion::parse("17.2")
        );
        assert!(BrowserVersion::parse("TP").is_none());
        assert!(BrowserVersion::parse("all").is_none());
        assert!(BrowserVersion::parse("17.x").is_none());

        let ios = TargetEnv::new(vec![BrowserTarget {
            name: " IOS_SAF ".to_string(),
            version: " 17.2 ".to_string(),
        }]);
        assert_eq!(ios.targets(), [BrowserTarget::new("ios", "17.2")]);
        assert_eq!(ios.required_features().bits(), 0);
        let old_ios = TargetEnv::new(vec![BrowserTarget::new("iOS", "13.7")]);
        assert!(
            old_ios
                .required_features()
                .contains(EcmaFeature::OptionalChaining)
        );
    }

    #[test]
    fn for_of_compatibility_uses_precise_version_boundaries() {
        let chrome_50 = TargetEnv::new(vec![BrowserTarget::new("chrome", "50")]);
        let chrome_51 = TargetEnv::new(vec![BrowserTarget::new("chrome", "51")]);
        assert!(chrome_50.required_features().contains(EcmaFeature::ForOf));
        assert!(!chrome_51.required_features().contains(EcmaFeature::ForOf));

        let node_6_4 = TargetEnv::new(vec![BrowserTarget::new("node", "6.4")]);
        let node_6_5 = TargetEnv::new(vec![BrowserTarget::new("node", "6.5")]);
        assert!(node_6_4.required_features().contains(EcmaFeature::ForOf));
        assert!(!node_6_5.required_features().contains(EcmaFeature::ForOf));
    }

    #[test]
    fn for_of_metadata_overrides_and_fingerprint_are_stable() {
        assert_eq!(COMPAT_DATA_VERSION, 3);
        assert_eq!(EcmaFeature::ForOf as u8, 16);
        assert_eq!(EcmaFeature::ALL.len(), 17);
        assert_eq!(EcmaFeature::ForOf.babel_plugin(), "transform-for-of");
        assert_eq!(
            EcmaFeature::from_babel_plugin("@babel/plugin-transform-for-of"),
            Some(EcmaFeature::ForOf)
        );
        assert_eq!(
            EcmaFeature::from_babel_plugin("plugin-transform-for-of"),
            Some(EcmaFeature::ForOf)
        );

        let mut env = TargetEnv::modern();
        let baseline_fingerprint = env.fingerprint();
        env.apply_overrides(
            ["@babel/plugin-transform-for-of"],
            std::iter::empty::<&str>(),
        )
        .unwrap();
        assert_eq!(env.required_features().bits(), 1_u64 << 16);
        assert_ne!(env.fingerprint(), baseline_fingerprint);

        env.apply_overrides(std::iter::empty::<&str>(), ["plugin-transform-for-of"])
            .unwrap();
        assert_eq!(env.required_features().bits(), 0);
        assert_eq!(env.fingerprint(), baseline_fingerprint);
    }

    #[test]
    fn normalization_makes_fingerprint_order_independent() {
        let a = TargetEnv::new(vec![
            BrowserTarget::new("firefox", "120"),
            BrowserTarget::new("chrome", "120"),
        ]);
        let b = TargetEnv::new(vec![
            BrowserTarget::new("chrome", "120"),
            BrowserTarget::new("firefox", "120"),
            BrowserTarget::new("chrome", "120"),
        ]);
        assert_eq!(a, b);
        assert_eq!(a.fingerprint(), b.fingerprint());

        let range_first = TargetEnv::new(vec![
            BrowserTarget::new("safari", "17.2-17.3"),
            BrowserTarget::new("safari", "17.2"),
        ]);
        let exact_first = TargetEnv::new(vec![
            BrowserTarget::new("safari", "17.2"),
            BrowserTarget::new("safari", "17.2-17.3"),
        ]);
        assert_eq!(range_first, exact_first);
        assert_eq!(
            range_first.targets(),
            [BrowserTarget::new("safari", "17.2")]
        );
    }

    #[test]
    fn unknown_targets_are_conservative() {
        let env = TargetEnv::new(vec![BrowserTarget::new("unknown", "999")]);
        assert_eq!(
            env.required_features().iter().count(),
            EcmaFeature::ALL.len()
        );

        let unknown_version = TargetEnv::new(vec![
            BrowserTarget::new("safari", "17.2"),
            BrowserTarget::new("safari", "TP"),
        ]);
        assert_eq!(
            unknown_version.required_features().iter().count(),
            EcmaFeature::ALL.len(),
            "an unparseable same-family target must not be hidden by a known numeric floor"
        );
    }

    #[test]
    fn typescript_and_react_options_participate_in_fingerprint() {
        let base = TransformOptions::default();
        let mut ts = base.clone();
        ts.typescript.enabled = true;
        let mut react = base.clone();
        react.react.enabled = true;
        react.react.import_source = "preact".to_string();
        assert_ne!(base.fingerprint(), ts.fingerprint());
        assert_ne!(base.fingerprint(), react.fingerprint());
    }

    #[test]
    fn include_and_exclude_override_target_detection() {
        let mut env = TargetEnv::new(vec![BrowserTarget::new("chrome", "120")]);
        env.apply_overrides(
            ["transform-arrow-functions"],
            ["transform-template-literals"],
        )
        .unwrap();
        assert!(env.required_features().contains(EcmaFeature::ArrowFunction));
        assert!(
            !env.required_features()
                .contains(EcmaFeature::TemplateLiteral)
        );
        assert!(
            env.apply_overrides(["does-not-exist"], std::iter::empty::<&str>())
                .is_err()
        );
    }
}
