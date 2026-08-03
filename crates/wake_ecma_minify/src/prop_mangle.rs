use wake_common::{Atom, FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::{
    Expression, MemberProperty, ObjectMember, Program, PropertyKey, PropertyKind, Visit,
    walk_expression,
};

use crate::mangle::{is_reserved, nth_name};

/// Property names that should NEVER be mangled.
const RESERVED_PROPS: &[&str] = &[
    "length",
    "prototype",
    "name",
    "constructor",
    "toString",
    "valueOf",
    "toLocaleString",
    "call",
    "apply",
    "bind",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "caller",
    "callee",
    "arguments",
    "__proto__",
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
    "this",
    "super",
    "new",
    "target",
    "default",
    "export",
    "now",
    "UTC",
    "parse",
    "stringify",
    "log",
    "warn",
    "error",
    "info",
    "debug",
    "trace",
    "dir",
    "abs",
    "acos",
    "acosh",
    "asin",
    "asinh",
    "atan",
    "atan2",
    "atanh",
    "cbrt",
    "ceil",
    "clz32",
    "cos",
    "cosh",
    "exp",
    "expm1",
    "floor",
    "fround",
    "hypot",
    "imul",
    "max",
    "min",
    "pow",
    "random",
    "round",
    "sign",
    "sin",
    "sinh",
    "sqrt",
    "tan",
    "tanh",
    "trunc",
    "isArray",
    "isFinite",
    "isInteger",
    "isNaN",
    "isSafeInteger",
    "isFrozen",
    "isExtensible",
    "isSealed",
    "keys",
    "values",
    "entries",
    "freeze",
    "seal",
    "assign",
    "create",
    "defineProperty",
    "defineProperties",
    "getPrototypeOf",
    "setPrototypeOf",
    "getOwnPropertyDescriptor",
    "getOwnPropertyDescriptors",
    "getOwnPropertyNames",
    "getOwnPropertySymbols",
    "fromCharCode",
    "fromCodePoint",
    "raw",
    "charAt",
    "charCodeAt",
    "codePointAt",
    "concat",
    "endsWith",
    "includes",
    "indexOf",
    "lastIndexOf",
    "localeCompare",
    "match",
    "matchAll",
    "normalize",
    "padEnd",
    "padStart",
    "repeat",
    "replace",
    "replaceAll",
    "search",
    "slice",
    "split",
    "startsWith",
    "substring",
    "toLowerCase",
    "toUpperCase",
    "toLocaleLowerCase",
    "toLocaleUpperCase",
    "trim",
    "trimEnd",
    "trimStart",
    "forEach",
    "map",
    "filter",
    "reduce",
    "reduceRight",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "every",
    "some",
    "flat",
    "flatMap",
    "from",
    "of",
    "sort",
    "push",
    "pop",
    "shift",
    "unshift",
    "splice",
    "reverse",
    "fill",
    "copyWithin",
    "toReversed",
    "toSorted",
    "toSpliced",
    "with",
    "at",
    "join",
    "toString",
    "toLocaleString",
    "entries",
    "keys",
    "values",
    "indexOf",
    "lastIndexOf",
    "includes",
    "getItem",
    "setItem",
    "removeItem",
    "clear",
    // Map / Set / WeakMap / WeakSet instance methods
    "set",
    "get",
    "has",
    "delete",
    "add",
    "size",
    "__reg",
    "__side",
    // React / JSX specific
    "createElement",
    "createRef",
    "Fragment",
    "cloneElement",
    "isValidElement",
    "useState",
    "useEffect",
    "useMemo",
    "useCallback",
    "useRef",
    "useReducer",
    "useContext",
    // Common DOM properties
    "innerHTML",
    "innerText",
    "textContent",
    "value",
    "checked",
    "disabled",
    "hidden",
    "style",
    "className",
    "classList",
    "id",
    "type",
    "name",
    "placeholder",
    "title",
    "alt",
    "src",
    "href",
    "onClick",
    "onChange",
    "onSubmit",
    "onFocus",
    "onBlur",
    "onKeyDown",
    "onKeyUp",
    "onMouseEnter",
    "onMouseLeave",
    "preventDefault",
    "stopPropagation",
    "children",
    "key",
    "ref",
    // Data / fetch
    "then",
    "catch",
    "finally",
    "status",
    "statusText",
    "ok",
    "headers",
    "json",
    "text",
    "blob",
    "arrayBuffer",
    "data",
    "error",
    "loading",
    // Module / package identity
    "version",
];

const GLOBALS: &[&str] = &[
    "Math",
    "Date",
    "JSON",
    "console",
    "Object",
    "Array",
    "String",
    "Number",
    "Boolean",
    "Symbol",
    "BigInt",
    "RegExp",
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "Promise",
    "Proxy",
    "Reflect",
    "Intl",
    "Buffer",
    "process",
    "global",
    "globalThis",
    "window",
    "document",
    "location",
    "history",
    "navigator",
    "localStorage",
    "sessionStorage",
    "setTimeout",
    "setInterval",
    "clearTimeout",
    "clearInterval",
    "fetch",
    "Worker",
    "WebSocket",
    "require",
    "module",
    "exports",
    "__dirname",
    "__filename",
    "console",
    "Buffer",
];

#[derive(Debug, Default)]
pub struct PropManglePlan {
    pub renames: FxHashMap<Atom, Atom>,
    pub span_renames: FxHashMap<Span, Atom>,
}

impl PropManglePlan {
    pub fn is_empty(&self) -> bool {
        self.span_renames.is_empty()
    }
    pub fn table(&self) -> &FxHashMap<Span, Atom> {
        &self.span_renames
    }
}

pub fn plan_prop_mangle(program: &Program, interner: &Interner) -> PropManglePlan {
    let reserved = reserved_set(interner);
    let globals = globals_set(interner);

    let mut collector = PropCollector {
        freq: FxHashMap::default(),
        member_spans: Vec::new(),
        key_spans: Vec::new(),
        interner,
        reserved,
        globals,
    };
    collector.visit_program(program);

    if collector.freq.is_empty() {
        return PropManglePlan::default();
    }

    let mut freq: Vec<(Atom, usize)> = collector.freq.into_iter().collect();
    freq.sort_by_key(|item| std::cmp::Reverse(item.1));

    let mut name_to_short: FxHashMap<Atom, Atom> = FxHashMap::default();
    let mut used_shorts: FxHashSet<Atom> = FxHashSet::default();
    let mut counter = 0usize;

    for (name, _freq) in &freq {
        let short = loop {
            let cand = nth_name(counter);
            counter += 1;
            if is_reserved(&cand) {
                continue;
            }
            let atom = interner.intern(&cand);
            if !used_shorts.contains(&atom) {
                break atom;
            }
        };
        name_to_short.insert(*name, short);
        used_shorts.insert(short);
    }

    let mut span_renames: FxHashMap<Span, Atom> = FxHashMap::default();
    for (span, name) in &collector.member_spans {
        if let Some(&short) = name_to_short.get(name) {
            span_renames.insert(*span, short);
        }
    }
    for (span, name) in &collector.key_spans {
        if let Some(&short) = name_to_short.get(name) {
            span_renames.insert(*span, short);
        }
    }

    PropManglePlan {
        renames: name_to_short,
        span_renames,
    }
}

fn reserved_set(interner: &Interner) -> FxHashSet<Atom> {
    RESERVED_PROPS.iter().map(|s| interner.intern(s)).collect()
}

fn globals_set(interner: &Interner) -> FxHashSet<Atom> {
    GLOBALS.iter().map(|s| interner.intern(s)).collect()
}

struct PropCollector<'a> {
    freq: FxHashMap<Atom, usize>,
    member_spans: Vec<(Span, Atom)>,
    key_spans: Vec<(Span, Atom)>,
    interner: &'a Interner,
    reserved: FxHashSet<Atom>,
    globals: FxHashSet<Atom>,
}

impl<'a> PropCollector<'a> {
    fn is_global_base(&self, name: Atom) -> bool {
        self.globals.contains(&name)
    }

    fn should_skip(&self, name: Atom, is_global: bool) -> bool {
        if is_global {
            return true;
        }
        if self.reserved.contains(&name) {
            return true;
        }
        let name_str = self.interner.resolve(name);
        if name_str.starts_with("__") {
            return true;
        }
        if name_str.len() <= 2 {
            return true;
        }
        false
    }
}

impl<'a> Visit<'a> for PropCollector<'a> {
    fn visit_program(&mut self, node: &Program<'a>) {
        for stmt in node.body.iter() {
            self.visit_statement(stmt);
        }
    }

    fn visit_expression(&mut self, node: &Expression<'a>) {
        match node {
            Expression::Member(m) => {
                if !m.optional
                    && let MemberProperty::Ident(id) = &m.property
                {
                    let on_global = self.is_member_of_global(&m.object);
                    if !self.should_skip(id.name, on_global) {
                        *self.freq.entry(id.name).or_insert(0) += 1;
                        self.member_spans.push((id.span, id.name));
                    }
                }
                self.visit_expression(&m.object);
            }
            Expression::Object(o) => {
                for member in o.properties.iter() {
                    match member {
                        ObjectMember::Property(p) => {
                            if !p.computed
                                && !p.method
                                && !p.shorthand
                                && !p.prototype_setter
                                && p.kind == PropertyKind::Init
                                && let PropertyKey::Ident(id) = &p.key
                                && !self.should_skip(id.name, false)
                            {
                                *self.freq.entry(id.name).or_insert(0) += 1;
                                self.key_spans.push((id.span, id.name));
                            }
                            self.visit_expression(&p.value);
                        }
                        ObjectMember::Spread(s) => {
                            self.visit_expression(&s.argument);
                        }
                    }
                }
            }
            _ => walk_expression(self, node),
        }
    }
}

impl<'a> PropCollector<'a> {
    fn is_member_of_global(&self, obj: &Expression<'a>) -> bool {
        match obj {
            Expression::Identifier(id) => self.is_global_base(id.name),
            Expression::Member(m) if !m.optional => self.is_member_of_global(&m.object),
            _ => false,
        }
    }
}
