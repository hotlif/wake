//! Explicit globals, with no host/environment introspection. The frozen set is ECMA-262 2024
//! section 19 plus ECMA-402 2024 section 8 (Intl). Readonly is lint policy, not a JS descriptor.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use wake_common::Interner;
use wake_ecma_ast::{Expression, Statement};

use crate::{LintError, LintOptions};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GlobalMode {
    Readonly,
    Writable,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EffectiveGlobal {
    pub mode: GlobalMode,
    pub source: String,
}

pub const STANDARD_GLOBALS_VERSION: &str = "es2024@1";
pub const ENVIRONMENT_GLOBALS_VERSION: &str = "1";

const BROWSER_GLOBALS: &[&str] = &[
    "AbortController",
    "AbortSignal",
    "Blob",
    "CSS",
    "CustomEvent",
    "Document",
    "Element",
    "Event",
    "File",
    "FileReader",
    "FormData",
    "Headers",
    "HTMLElement",
    "Image",
    "IntersectionObserver",
    "Location",
    "MutationObserver",
    "Node",
    "Request",
    "Response",
    "ResizeObserver",
    "TextDecoder",
    "TextEncoder",
    "URL",
    "URLSearchParams",
    "WebAssembly",
    "WebSocket",
    "Window",
    "alert",
    "atob",
    "btoa",
    "cancelAnimationFrame",
    "clearInterval",
    "clearTimeout",
    "confirm",
    "console",
    "crypto",
    "document",
    "fetch",
    "getComputedStyle",
    "history",
    "localStorage",
    "location",
    "navigator",
    "performance",
    "prompt",
    "queueMicrotask",
    "requestAnimationFrame",
    "sessionStorage",
    "setInterval",
    "setTimeout",
    "structuredClone",
    "window",
];

const NODE_GLOBALS: &[&str] = &[
    "AbortController",
    "AbortSignal",
    "Buffer",
    "FormData",
    "Headers",
    "MessageChannel",
    "MessagePort",
    "MessageEvent",
    "Request",
    "Response",
    "TextDecoder",
    "TextEncoder",
    "URL",
    "URLSearchParams",
    "WebAssembly",
    "clearImmediate",
    "clearInterval",
    "clearTimeout",
    "console",
    "crypto",
    "fetch",
    "global",
    "module",
    "performance",
    "process",
    "queueMicrotask",
    "require",
    "setImmediate",
    "setInterval",
    "setTimeout",
    "structuredClone",
    "__dirname",
    "__filename",
    "exports",
];

const STANDARD: &[&str] = &[
    "AggregateError",
    "Array",
    "ArrayBuffer",
    "Atomics",
    "BigInt",
    "BigInt64Array",
    "BigUint64Array",
    "Boolean",
    "DataView",
    "Date",
    "Error",
    "EvalError",
    "FinalizationRegistry",
    "Float32Array",
    "Float64Array",
    "Function",
    "Infinity",
    "Int8Array",
    "Int16Array",
    "Int32Array",
    "Intl",
    "JSON",
    "Map",
    "Math",
    "NaN",
    "Number",
    "Object",
    "Promise",
    "Proxy",
    "RangeError",
    "ReferenceError",
    "Reflect",
    "RegExp",
    "Set",
    "SharedArrayBuffer",
    "String",
    "Symbol",
    "SyntaxError",
    "TypeError",
    "URIError",
    "Uint8Array",
    "Uint8ClampedArray",
    "Uint16Array",
    "Uint32Array",
    "WeakMap",
    "WeakRef",
    "WeakSet",
    "decodeURI",
    "decodeURIComponent",
    "encodeURI",
    "encodeURIComponent",
    "eval",
    "globalThis",
    "isFinite",
    "isNaN",
    "parseFloat",
    "parseInt",
    "undefined",
];

/// Validate only user-supplied names with the existing grammar; do not maintain a second Unicode
/// identifier implementation. No source, AST or Interner escapes this check. Escaped spellings,
/// parentheses, comments and surrounding trivia are rejected by the exact occurrence comparison.
pub fn validate_globals(globals: &BTreeMap<String, GlobalMode>) -> Result<(), LintError> {
    if globals.is_empty() {
        return Ok(());
    }
    let interner = Interner::new();
    for name in globals.keys() {
        let parsed = wake_ecma_parser::parse(name, &interner, wake_ecma_parser::SourceType::Script);
        let valid = !parsed.has_errors()
            && parsed.module.with_ast(|program| {
                matches!(program.body.as_slice(), [Statement::Expression(statement)]
                if matches!(statement.expression, Expression::Identifier(id)
                    if id.span.lo == 0 && id.span.hi as usize == name.len()
                    && interner.with_resolved(id.name, |decoded| decoded == name)))
            });
        if !valid {
            return Err(LintError::Configuration(format!(
                "Invalid lint global identifier: {name:?}"
            )));
        }
    }
    Ok(())
}

/// Validate the closed, versioned environment names before any source matching occurs.
pub fn validate_environments(environments: &[String]) -> Result<(), LintError> {
    for environment in environments {
        if !matches!(environment.as_str(), "browser" | "node") {
            return Err(LintError::Configuration(format!(
                "Unknown lint environment: {environment:?}"
            )));
        }
    }
    Ok(())
}

pub fn effective_globals(
    options: &LintOptions,
) -> Result<BTreeMap<String, EffectiveGlobal>, LintError> {
    validate_environments(&options.environments)?;
    validate_globals(&options.globals)?;
    let mut globals: BTreeMap<_, _> = STANDARD
        .iter()
        .map(|&name| {
            (
                name.into(),
                EffectiveGlobal {
                    mode: GlobalMode::Readonly,
                    source: format!("standard:{STANDARD_GLOBALS_VERSION}"),
                },
            )
        })
        .collect();
    let environments: BTreeSet<_> = options.environments.iter().map(String::as_str).collect();
    for environment in environments {
        let names = match environment {
            "browser" => BROWSER_GLOBALS,
            "node" => NODE_GLOBALS,
            _ => unreachable!("validate_environments checked environment names"),
        };
        let source = format!("environment:{environment}@{ENVIRONMENT_GLOBALS_VERSION}");
        for &name in names {
            globals.insert(
                name.into(),
                EffectiveGlobal {
                    mode: GlobalMode::Readonly,
                    source: source.clone(),
                },
            );
        }
    }
    globals.extend(options.globals.iter().map(|(name, &mode)| {
        (
            name.clone(),
            EffectiveGlobal {
                mode,
                source: "globals".into(),
            },
        )
    }));
    Ok(globals)
}
