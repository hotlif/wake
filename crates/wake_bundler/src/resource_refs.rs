//! Parser-owned static resource references. Paths and emitted URLs are linker decisions.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use wake_common::{FxHashMap, Interner, Span};
use wake_ecma_ast::{Expression, MemberProperty, Program, Visit, walk_expression};

#[derive(Clone, Debug)]
pub(crate) struct ResourceReference {
    pub span: Span,
    pub request: String,
    pub worker: bool,
}

pub(crate) fn decode_path(path: &str) -> Result<String, String> {
    let hex = |value: u8| match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    };
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let value = bytes
                .get(index + 1)
                .and_then(|value| hex(*value))
                .zip(bytes.get(index + 2).and_then(|value| hex(*value)))
                .map(|(high, low)| high * 16 + low)
                .ok_or_else(|| "invalid percent encoding in resource URL".to_owned())?;
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| "resource URL path is not valid UTF-8".to_owned())
}

pub(crate) fn encode_file_name(name: &str) -> String {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 15) as usize]));
        }
    }
    encoded
}

pub(crate) fn collect(program: &Program<'_>, interner: &Interner) -> Vec<ResourceReference> {
    let semantic = wake_ecma_semantic::analyze(program, interner);
    let bound: FxHashMap<_, _> = semantic
        .references
        .iter()
        .filter(|reference| reference.resolved.is_some())
        .map(|reference| (reference.span, ()))
        .collect();
    struct Collector<'a> {
        interner: &'a Interner,
        bound: FxHashMap<Span, ()>,
        references: Vec<ResourceReference>,
    }
    impl Collector<'_> {
        fn global(&self, expression: &Expression<'_>, name: &str) -> bool {
            matches!(expression, Expression::Identifier(id)
                if self.interner.resolve(id.name) == name && !self.bound.contains_key(&id.span))
        }

        fn url(&self, expression: &Expression<'_>) -> Option<ResourceReference> {
            let Expression::New(url) = expression else {
                return None;
            };
            if !self.global(&url.callee, "URL") || url.arguments.len() != 2 {
                return None;
            }
            let Expression::StringLiteral(literal) = url.arguments[0] else {
                return None;
            };
            let Expression::Member(member) = url.arguments[1] else {
                return None;
            };
            let Expression::MetaProperty(meta) = member.object else {
                return None;
            };
            let MemberProperty::Ident(property) = member.property else {
                return None;
            };
            if member.optional
                || self.interner.resolve(property.name) != "url"
                || self.interner.resolve(meta.meta) != "import"
                || self.interner.resolve(meta.property) != "meta"
            {
                return None;
            }
            let value = self.interner.resolve_js(literal.value);
            let request = value.as_str()?;
            if !request.starts_with("./") && !request.starts_with("../") {
                return None;
            }
            Some(ResourceReference {
                span: literal.span,
                request: request.to_owned(),
                worker: false,
            })
        }

        fn push(&mut self, reference: ResourceReference) {
            if !self
                .references
                .iter()
                .any(|existing| existing.span == reference.span)
            {
                self.references.push(reference);
            }
        }
    }
    impl<'ast> Visit<'ast> for Collector<'_> {
        fn visit_expression(&mut self, expression: &Expression<'ast>) {
            if let Expression::New(worker) = expression
                && self.global(&worker.callee, "Worker")
                && let Some(argument) = worker.arguments.first()
                && let Some(mut reference) = self.url(argument)
            {
                reference.worker = true;
                self.push(reference);
            }
            if let Some(reference) = self.url(expression) {
                self.push(reference);
            }
            walk_expression(self, expression);
        }
    }
    let mut collector = Collector {
        interner,
        bound,
        references: Vec::new(),
    };
    collector.visit_program(program);
    collector
        .references
        .sort_by_key(|reference| reference.span.lo);
    collector.references
}

thread_local! {
    static WORKER_STACK: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

pub(crate) struct WorkerGuard;

impl WorkerGuard {
    pub(crate) fn enter(path: &Path) -> Result<Self, String> {
        WORKER_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            if stack.contains(&path.to_path_buf()) || stack.len() >= 64 {
                return Err(format!("recursive Worker build: {}", path.display()));
            }
            stack.push(path.to_path_buf());
            Ok(Self)
        })
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        WORKER_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}
