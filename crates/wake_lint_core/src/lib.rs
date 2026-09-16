//! Pure single-file lint analysis. Contracts and staged scope: engineering/LINT.md.
//!
//! This crate owns no filesystem, configuration discovery, runtime, or source publication.
//! All returned diagnostics own their data and refer to the caller's UTF-8 source snapshot.

mod a11y;
mod aria;
mod baseline;
mod bindings;
mod catalog;
mod collections;
mod configuration;
mod constant_binary;
mod directives;
mod fix;
mod flow;
mod globals;
mod hooks;
mod imports;
mod indent;
mod jsx;
mod module_graph;
mod module_requests;
mod prefer_const;
mod rules;
mod scope;
mod source_helpers;
mod style;
mod type_imports;
mod type_source;
mod typed;
mod typescript;
mod undefined;
mod unused;

use std::collections::BTreeMap;
use std::fmt;

pub use baseline::{
    BaselineCandidates, BaselineEntry, BaselineMatch, apply_baseline, baseline_candidates,
    validate_baseline,
};
pub use catalog::{RuleInfo, rule_catalog};
pub use configuration::{
    EffectiveRule, PRESETS, Parameter, ParameterKind, Preset, RuleConfiguration, RuleSetting,
    effective_configuration, resolve_setting,
};
pub use fix::{
    FixApplication, FixResult, LintFix, TextEdit, apply_fixes, fix_text, fix_text_with_baseline,
};
pub use globals::{
    ENVIRONMENT_GLOBALS_VERSION, EffectiveGlobal, GlobalMode, STANDARD_GLOBALS_VERSION,
    effective_globals, validate_environments, validate_globals,
};
pub use module_graph::{ModuleFile, ModuleGraph, ModuleId, ModuleResolution, PackageDependency};
pub use module_requests::{ModuleRequest, ModuleRequestKind, ModuleRequests, inspect_module};
use serde::{Deserialize, Serialize};
pub use type_source::TypeSource;
pub use typed::{
    CallArgumentType, CallType, MemberType, TypeId, TypeKind, TypeLiteral, TypeNode, TypedSource,
};
use wake_common::{Diagnostic, Interner};
use wake_ecma_ast::Visit;
pub use wake_ecma_ast::{
    SourceAssertionKind, SourceAssignmentKind, SourceCallKind, SourceCallbackKind,
    SourceConditionKind, SourceMemberKind,
};
pub use wake_ecma_parser::SourceType;
use wake_ecma_parser::{ParseOptions, parse_source};

pub const PIPELINE_VERSION: &str = "wake-lint-core-v74";
/// Included alongside the lint pipeline in all persistent single-file identities.
pub const PARSER_PIPELINE_VERSION: &str = wake_ecma_parser::PIPELINE_VERSION;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleLevel {
    Off,
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LintOptions {
    /// Enable the versioned built-in recommended preset before explicit overrides.
    pub recommended: bool,
    pub presets: Vec<String>,
    pub report_unused_disable: RuleLevel,
    pub rules: BTreeMap<String, RuleSetting>,
    pub globals: BTreeMap<String, GlobalMode>,
    pub environments: Vec<String>,
}

impl Default for LintOptions {
    fn default() -> Self {
        Self {
            recommended: true,
            presets: Vec::new(),
            report_unused_disable: RuleLevel::Warn,
            rules: BTreeMap::new(),
            globals: BTreeMap::new(),
            environments: Vec::new(),
        }
    }
}

pub struct RuleMetadata {
    pub id: &'static str,
    pub description: &'static str,
    pub parameters: &'static [Parameter],
    pub analysis: Analysis,
    pub category: &'static str,
    pub languages: &'static [&'static str],
    pub fixable: bool,
    pub message_ids: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Analysis {
    Syntax,
    Scope,
    ControlFlow,
    ScopeControlFlow,
    ModuleGraph,
    TypeInformation,
}

pub static RULES: &[RuleMetadata] = &[
    RuleMetadata {
        id: "ts/no-floating-promises",
        description: "Require Promise-returning expression statements to be handled",
        parameters: &[],
        analysis: Analysis::TypeInformation,
        category: "problem",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["floating"],
    },
    RuleMetadata {
        id: "ts/no-unnecessary-type-assertion",
        description: "Disallow type assertions that do not change the proven type category",
        parameters: &[],
        analysis: Analysis::TypeInformation,
        category: "problem",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["unnecessary"],
    },
    RuleMetadata {
        id: "ts/await-thenable",
        description: "Require await operands to be Promise or thenable types",
        parameters: &[],
        analysis: Analysis::TypeInformation,
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["notThenable"],
    },
    RuleMetadata {
        id: "ts/switch-exhaustiveness-check",
        description: "Require a default or exhaustive handling for union discriminants",
        parameters: &[],
        analysis: Analysis::TypeInformation,
        category: "problem",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["missingDefault"],
    },
    RuleMetadata {
        id: "ts/no-misused-promises",
        description: "Disallow Promise values in synchronous conditions and void callbacks",
        parameters: &[
            Parameter::boolean("checks_conditionals", true),
            Parameter::boolean_or_object(
                "checks_void_return",
                true,
                &[
                    "arguments",
                    "attributes",
                    "properties",
                    "returns",
                    "variables",
                ],
            ),
        ],
        analysis: Analysis::TypeInformation,
        category: "problem",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["promiseCondition", "promiseCallback"],
    },
    RuleMetadata {
        id: "ts/no-unsafe-return",
        description: "Disallow returning any or unresolved values",
        parameters: &[],
        analysis: Analysis::TypeInformation,
        category: "problem",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["unsafeReturn", "errorReturn"],
    },
    RuleMetadata {
        id: "ts/no-unsafe-assignment",
        description: "Disallow assigning any or unresolved values",
        parameters: &[],
        analysis: Analysis::TypeInformation,
        category: "problem",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["unsafeAssignment", "errorAssignment"],
    },
    RuleMetadata {
        id: "ts/no-unsafe-member-access",
        description: "Disallow any receivers and computed keys in original member accesses",
        parameters: &[Parameter::boolean("allow_optional", false)],
        analysis: Analysis::TypeInformation,
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["unsafeMember", "errorMember", "unsafeKey", "errorKey"],
    },
    RuleMetadata {
        id: "ts/restrict-template-expressions",
        description: "Require explicitly allowed types in untagged template substitutions",
        parameters: &[
            Parameter::boolean("allow_any", true),
            Parameter::boolean("allow_number", true),
            Parameter::boolean("allow_boolean", true),
            Parameter::boolean("allow_nullish", true),
            Parameter::boolean("allow_regexp", false),
            Parameter::boolean("allow_never", false),
        ],
        analysis: Analysis::TypeInformation,
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["invalid"],
    },
    RuleMetadata {
        id: "ts/no-unsafe-call",
        description: "Disallow calls through any or standard Function types without usable signatures",
        parameters: &[],
        analysis: Analysis::TypeInformation,
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &[
            "unsafeCall",
            "unsafeNew",
            "unsafeTag",
            "errorCall",
            "errorNew",
            "errorTag",
        ],
    },
    RuleMetadata {
        id: "import/no-unresolved",
        description: "Resolve original module requests in the project environment",
        parameters: &[
            Parameter::boolean("commonjs", true),
            Parameter::boolean("dynamic_imports", true),
            Parameter::boolean("include_types", true),
            Parameter::boolean("report_unknown", true),
            Parameter {
                name: "ignore",
                kind: ParameterKind::RegexList,
            },
        ],
        analysis: Analysis::ModuleGraph,
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["unresolved", "unknown"],
    },
    RuleMetadata {
        id: "import/no-cycle",
        description: "Detect cycles and incomplete dependency proofs by original module identity",
        parameters: &[
            Parameter::boolean("commonjs", false),
            Parameter::boolean("dynamic_imports", false),
            Parameter::boolean("include_types", false),
            Parameter::boolean("ignore_external", false),
        ],
        analysis: Analysis::ModuleGraph,
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["cycle", "incomplete"],
    },
    RuleMetadata {
        id: "import/no-duplicates",
        description: "Disallow repeated imports of the same resolved module and attributes",
        parameters: &[Parameter::boolean("separate_type_imports", true)],
        analysis: Analysis::ModuleGraph,
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["duplicate"],
    },
    RuleMetadata {
        id: "import/no-restricted-paths",
        description: "Enforce project-relative importer and target path zones",
        parameters: &[Parameter {
            name: "zones",
            kind: ParameterKind::PathZones,
        }],
        analysis: Analysis::ModuleGraph,
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["restricted"],
    },
    RuleMetadata {
        id: "import/no-extraneous-dependencies",
        description: "Require dependencies to be declared by the importing package",
        parameters: &[
            Parameter::boolean("dev_dependencies", false),
            Parameter::boolean("optional_dependencies", true),
            Parameter::boolean("peer_dependencies", true),
            Parameter::boolean("include_types", true),
        ],
        analysis: Analysis::ModuleGraph,
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["extraneous"],
    },
    RuleMetadata {
        id: "import/order",
        description: "Check import groups, alphabetization and separation without reordering execution",
        parameters: &[
            Parameter {
                name: "groups",
                kind: ParameterKind::Permutation(module_graph::GROUPS),
            },
            Parameter {
                name: "alphabetize",
                kind: ParameterKind::Choice {
                    default: "ignore",
                    values: &["ignore", "asc", "desc"],
                },
            },
            Parameter::boolean("case_insensitive", true),
            Parameter {
                name: "newlines",
                kind: ParameterKind::Choice {
                    default: "ignore",
                    values: &["ignore", "always", "never"],
                },
            },
        ],
        analysis: Analysis::ModuleGraph,
        category: "layout",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["group", "alphabetical", "newline"],
    },
    RuleMetadata {
        id: "react-hooks/exhaustive-deps",
        description: "Check original reactive captures against Hook dependency arrays",
        parameters: &[Parameter {
            name: "additional_effect_hooks",
            kind: ParameterKind::Regex(""),
        }],
        analysis: Analysis::Scope,
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &[
            "missing",
            "missing-array",
            "unknown-callback",
            "dynamic",
            "async",
            "unavailable",
            "stale-write",
            "duplicate",
            "external",
            "mutable",
            "unnecessary",
        ],
    },
    RuleMetadata {
        id: "react-hooks/rules-of-hooks",
        description: "Require Hooks in React functions with consistent native execution paths",
        parameters: &[],
        analysis: Analysis::ScopeControlFlow,
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &[
            "context",
            "async",
            "generator",
            "parameters",
            "exception",
            "loop",
            "conditional",
        ],
    },
    RuleMetadata {
        id: "js/no-unused-vars",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["unused", "usedIgnored"],
        analysis: Analysis::Scope,
        parameters: &[
            Parameter {
                name: "vars",
                kind: ParameterKind::Choice {
                    default: "all",
                    values: &["all", "local"],
                },
            },
            Parameter {
                name: "args",
                kind: ParameterKind::Choice {
                    default: "after-used",
                    values: &["after-used", "all", "none"],
                },
            },
            Parameter {
                name: "caught_errors",
                kind: ParameterKind::Choice {
                    default: "all",
                    values: &["all", "none"],
                },
            },
            Parameter::boolean("ignore_rest_siblings", false),
            Parameter::boolean("report_used_ignore_pattern", false),
            Parameter {
                name: "vars_ignore_pattern",
                kind: ParameterKind::Regex(""),
            },
            Parameter {
                name: "args_ignore_pattern",
                kind: ParameterKind::Regex(""),
            },
            Parameter {
                name: "caught_errors_ignore_pattern",
                kind: ParameterKind::Regex(""),
            },
        ],
        description: "Disallow unused original value and type bindings",
    },
    RuleMetadata {
        id: "react/jsx-uses-vars",
        category: "suggestion",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &[],
        analysis: Analysis::Scope,
        parameters: &[],
        description: "Count resolved JSX component roots as binding uses",
    },
    RuleMetadata {
        id: "js/prefer-const",
        category: "suggestion",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: true,
        message_ids: &["prefer"],
        analysis: Analysis::Scope,
        parameters: &[
            Parameter {
                name: "destructuring",
                kind: ParameterKind::Choice {
                    default: "any",
                    values: &["any", "all"],
                },
            },
            Parameter::boolean("ignore_read_before_assign", false),
        ],
        description: "Prefer const for bindings without reassignment",
    },
    RuleMetadata {
        id: "js/no-undef",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["undefined"],
        analysis: Analysis::Scope,
        parameters: &[Parameter::boolean("typeof", false)],
        description: "Disallow unresolved original runtime value references",
    },
    RuleMetadata {
        id: "react/jsx-no-undef",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["undefined"],
        analysis: Analysis::Scope,
        parameters: &[],
        description: "Disallow unresolved JSX component roots",
    },
    RuleMetadata {
        id: "js/no-shadow",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["shadow"],
        analysis: Analysis::Scope,
        parameters: &[
            Parameter::boolean("hoist", true),
            Parameter::boolean("ignore_named_expressions", true),
        ],
        description: "Disallow explicit value declarations shadowing outer source bindings",
    },
    RuleMetadata {
        id: "js/no-use-before-define",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["before"],
        analysis: Analysis::Scope,
        parameters: &[
            Parameter::boolean("functions", true),
            Parameter::boolean("classes", true),
            Parameter::boolean("variables", true),
        ],
        description: "Disallow value references textually preceding their declaration",
    },
    RuleMetadata {
        id: "js/no-redeclare",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["duplicate"],
        analysis: Analysis::Scope,
        parameters: &[],
        description: "Disallow repeated declarations in the same value scope",
    },
    RuleMetadata {
        id: "ts/consistent-type-exports",
        category: "suggestion",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["type"],
        analysis: Analysis::Scope,
        parameters: &[],
        description: "Mark local type-only exports explicitly",
    },
    RuleMetadata {
        id: "ts/consistent-type-imports",
        category: "suggestion",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["type"],
        analysis: Analysis::Scope,
        parameters: &[],
        description: "Use explicit type imports for bindings used only in types",
    },
    RuleMetadata {
        id: "react/jsx-key",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["key"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Require keys on JSX list items",
    },
    RuleMetadata {
        id: "react/no-array-index-key",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["index"],
        analysis: Analysis::Scope,
        parameters: &[],
        description: "Avoid array callback indexes in JSX keys",
    },
    RuleMetadata {
        id: "a11y/click-events-have-key-events",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["keyboard"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Provide keyboard handlers for click interactions",
    },
    RuleMetadata {
        id: "a11y/interactive-supports-focus",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["focus"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Make interactive ARIA elements focusable",
    },
    RuleMetadata {
        id: "a11y/aria-props",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["property"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Require recognized ARIA property names",
    },
    RuleMetadata {
        id: "a11y/aria-proptypes",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["value"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Require valid static ARIA property values",
    },
    RuleMetadata {
        id: "a11y/aria-role",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["role"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Require usable concrete ARIA roles",
    },
    RuleMetadata {
        id: "a11y/alt-text",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["alternative"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Require text alternatives for image elements and objects",
    },
    RuleMetadata {
        id: "a11y/anchor-has-content",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["content"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Require accessible content for anchors",
    },
    RuleMetadata {
        id: "a11y/anchor-is-valid",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["href"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Require usable anchor destinations",
    },
    RuleMetadata {
        id: "a11y/label-has-associated-control",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["content", "control"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Require text and a control association for labels",
    },
    RuleMetadata {
        id: "ts/array-type",
        category: "suggestion",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["array", "generic"],
        analysis: Analysis::Syntax,
        parameters: &[Parameter {
            name: "syntax",
            kind: ParameterKind::Choice {
                default: "array",
                values: &["array", "generic"],
            },
        }],
        description: "Require consistent array type syntax",
    },
    RuleMetadata {
        id: "js/no-duplicate-imports",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["duplicate"],
        analysis: Analysis::Syntax,
        parameters: &[Parameter::boolean("allow_separate_type_imports", false)],
        description: "Disallow duplicate static imports of the same module and attributes",
    },
    RuleMetadata {
        id: "js/no-constant-binary-expression",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &[
            "constant-short-circuit",
            "constant-nullish",
            "constant-comparison",
        ],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Disallow constant binary comparisons and short-circuit decisions",
    },
    RuleMetadata {
        id: "js/no-unreachable",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["flow"],
        analysis: Analysis::ControlFlow,
        parameters: &[],
        description: "Disallow unreachable statements after abrupt completion",
    },
    RuleMetadata {
        id: "js/consistent-return",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["flow"],
        analysis: Analysis::ControlFlow,
        parameters: &[],
        description: "Require consistent value-returning function paths",
    },
    RuleMetadata {
        id: "js/no-fallthrough",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["flow"],
        analysis: Analysis::ControlFlow,
        parameters: &[],
        description: "Require an explanation for nonempty switch case fallthrough",
    },
    RuleMetadata {
        id: "js/no-unsafe-finally",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["flow"],
        analysis: Analysis::ControlFlow,
        parameters: &[],
        description: "Disallow abrupt completions escaping finally blocks",
    },
    RuleMetadata {
        id: "ts/no-namespace",
        category: "problem",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["namespace"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Disallow named TypeScript namespaces and modules",
    },
    RuleMetadata {
        id: "ts/no-empty-interface",
        category: "problem",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["empty"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Disallow interfaces without members",
    },
    RuleMetadata {
        id: "ts/ban-ts-comment",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["directive"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Require explained ts-expect-error and disallow unchecked suppressions",
    },
    RuleMetadata {
        id: "js/no-self-assign",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["self"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Disallow assigning identifiers to themselves",
    },
    RuleMetadata {
        id: "js/no-self-compare",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["self"],
        analysis: Analysis::Syntax,
        parameters: &[],
        description: "Disallow comparing identical primitive expressions",
    },
    RuleMetadata {
        id: "js/use-isnan",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["unexpected"],
        analysis: Analysis::Scope,
        parameters: &[],
        description: "Use a NaN predicate instead of comparing with global NaN",
    },
    RuleMetadata {
        id: "js/no-console",
        category: "suggestion",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["unexpected"],
        analysis: Analysis::Scope,
        parameters: &[],
        description: "Disallow global console member access",
    },
    RuleMetadata {
        id: "js/no-async-promise-executor",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["unexpected"],
        analysis: Analysis::Scope,
        parameters: &[],
        description: "Disallow async executors of the global Promise constructor",
    },
    RuleMetadata {
        id: "js/no-promise-executor-return",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["unexpected"],
        analysis: Analysis::Scope,
        parameters: &[],
        description: "Disallow returning values from global Promise executors",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "ts/no-explicit-any",
        category: "problem",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["any"],
        parameters: &[],
        description: "Disallow explicit any types",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "ts/no-non-null-assertion",
        category: "problem",
        languages: &["ts", "tsx"],
        fixable: false,
        message_ids: &["assertion"],
        parameters: &[],
        description: "Disallow postfix non-null assertions",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "js/no-sparse-arrays",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["sparse"],
        parameters: &[],
        description: "Disallow holes in array expressions",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "js/valid-typeof",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["invalid"],
        parameters: &[],
        description: "Require valid literal typeof result comparisons",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "js/no-cond-assign",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["assignment"],
        parameters: &[],
        description: "Disallow assignments in condition operands",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "js/no-var",
        category: "suggestion",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["var"],
        parameters: &[],
        description: "Use lexical declarations instead of var",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "style/indent",
        category: "layout",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: true,
        message_ids: &["indent"],
        parameters: &[
            Parameter {
                name: "style",
                kind: ParameterKind::Choice {
                    default: "spaces",
                    values: &["spaces", "tabs"],
                },
            },
            Parameter {
                name: "width",
                kind: ParameterKind::Integer {
                    default: 2,
                    minimum: 1,
                    maximum: 8,
                },
            },
        ],
        description: "Use structural indentation for code lines",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "style/comma-dangle",
        category: "layout",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: true,
        message_ids: &["missing", "extra"],
        parameters: &[Parameter {
            name: "mode",
            kind: ParameterKind::Choice {
                default: "never",
                values: &["never", "always", "always-multiline", "only-multiline"],
            },
        }],
        description: "Require or omit optional trailing list commas",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "style/semi",
        category: "layout",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: true,
        message_ids: &["missing", "extra"],
        parameters: &[Parameter {
            name: "mode",
            kind: ParameterKind::Choice {
                default: "always",
                values: &["always", "never"],
            },
        }],
        description: "Require or safely omit statement semicolons",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "style/no-trailing-spaces",
        category: "layout",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: true,
        message_ids: &["trailing"],
        parameters: &[],
        description: "Disallow spaces and tabs at line ends",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "style/quotes",
        category: "layout",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: true,
        message_ids: &["quote"],
        parameters: &[Parameter {
            name: "quote",
            kind: ParameterKind::Choice {
                default: "single",
                values: &["single", "double"],
            },
        }],
        description: "Use the configured quotes in JavaScript and TypeScript strings",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "js/no-debugger",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["unexpected"],
        parameters: &[],
        description: "Disallow debugger statements",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "js/eqeqeq",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["strict"],
        parameters: &[Parameter::boolean("allow_null", false)],
        description: "Require strict equality comparisons",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "js/no-empty",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["empty"],
        parameters: &[Parameter::boolean("allow_catch", false)],
        description: "Disallow empty blocks without comments",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "js/no-duplicate-case",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["duplicate"],
        parameters: &[],
        description: "Disallow provably duplicate literal case labels",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "js/no-dupe-keys",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["duplicate"],
        parameters: &[],
        description: "Disallow duplicate static object keys",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "js/no-constant-condition",
        category: "problem",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: false,
        message_ids: &["constant"],
        parameters: &[Parameter::boolean("check_loops", true)],
        description: "Disallow provably constant conditions",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "react/jsx-no-duplicate-props",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["duplicate"],
        parameters: &[],
        description: "Disallow duplicate explicit JSX attributes",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "react/no-danger",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["dangerous"],
        parameters: &[],
        description: "Disallow explicit dangerouslySetInnerHTML attributes",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "react/no-children-prop",
        category: "problem",
        languages: &["jsx", "tsx"],
        fixable: false,
        message_ids: &["children"],
        parameters: &[],
        description: "Use JSX children instead of an explicit children attribute",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "react/self-closing-comp",
        category: "suggestion",
        languages: &["jsx", "tsx"],
        fixable: true,
        message_ids: &["empty"],
        parameters: &[],
        description: "Use self-closing elements when there is no child source",
    },
    RuleMetadata {
        analysis: Analysis::Syntax,
        id: "style/eol-last",
        category: "layout",
        languages: &["js", "jsx", "ts", "tsx"],
        fixable: true,
        message_ids: &["missing"],
        parameters: &[Parameter {
            name: "linebreak",
            kind: ParameterKind::Choice {
                default: "lf",
                values: &["lf", "crlf"],
            },
        }],
        description: "Require a final line terminator in nonempty source",
    },
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LintDiagnostic {
    pub rule_id: String,
    pub level: RuleLevel,
    pub message_id: String,
    pub message: String,
    pub start: u32,
    pub end: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<LintFix>,
}

#[derive(Debug)]
pub struct LintResult {
    pub diagnostics: Vec<LintDiagnostic>,
    /// Full original parser diagnostics, including labels and notes. Never suppressed by rules.
    pub parse_diagnostics: Vec<Diagnostic>,
}

impl LintResult {
    pub fn has_errors(&self) -> bool {
        self.parse_diagnostics.iter().any(Diagnostic::is_error)
            || self.diagnostics.iter().any(|d| d.level == RuleLevel::Error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LintError {
    Configuration(String),
    InvalidData(String),
    Fix(String),
    Analysis(String),
}

impl fmt::Display for LintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Configuration(message)
            | Self::InvalidData(message)
            | Self::Fix(message)
            | Self::Analysis(message) => message,
        };
        f.write_str(message)
    }
}

impl std::error::Error for LintError {}

/// Validate decoded single-file findings after the caller has verified the cache content key.
/// This checks protocol invariants; it does not prove that a caller fabricated no diagnostics.
pub fn validate_cached_diagnostics(
    source: &str,
    options: &LintOptions,
    diagnostics: &[LintDiagnostic],
) -> Result<(), LintError> {
    let levels = effective_rules(options)?;
    for (index, diagnostic) in diagnostics.iter().enumerate() {
        let (expected_level, valid_message, fixable) = match diagnostic.rule_id.as_str() {
            "wake/invalid-directive" => {
                (RuleLevel::Error, diagnostic.message_id == "invalid", false)
            }
            "wake/unused-disable" => (
                options.report_unused_disable,
                diagnostic.message_id == "unused",
                false,
            ),
            id => {
                let rule = RULES
                    .iter()
                    .find(|rule| rule.id == id)
                    .ok_or_else(|| LintError::InvalidData("Unknown cached rule".into()))?;
                (
                    levels[id],
                    rule.message_ids.contains(&diagnostic.message_id.as_str()),
                    rule.fixable,
                )
            }
        };
        if expected_level == RuleLevel::Off
            || expected_level != diagnostic.level
            || !valid_message
            || diagnostic.start > diagnostic.end
            || diagnostic.end as usize > source.len()
            || !source.is_char_boundary(diagnostic.start as usize)
            || !source.is_char_boundary(diagnostic.end as usize)
        {
            return Err(LintError::InvalidData("Invalid cached diagnostic".into()));
        }
        if let Some(fix) = &diagnostic.fix {
            if !fixable {
                return Err(LintError::InvalidData("Unexpected cached fix".into()));
            }
            fix::validate_fixes(source, std::slice::from_ref(fix))?;
        }
        if let Some(previous) = index.checked_sub(1).map(|index| &diagnostics[index])
            && ((
                previous.start,
                previous.end,
                &previous.rule_id,
                &previous.message_id,
            ) > (
                diagnostic.start,
                diagnostic.end,
                &diagnostic.rule_id,
                &diagnostic.message_id,
            ) || previous == diagnostic)
        {
            return Err(LintError::InvalidData(
                "Unsorted or duplicate cached diagnostic".into(),
            ));
        }
    }
    Ok(())
}

/// Validate rule identities and materialize effective levels without parsing a source file.
pub fn effective_rules(options: &LintOptions) -> Result<BTreeMap<String, RuleLevel>, LintError> {
    Ok(effective_configuration(options)?
        .into_iter()
        .map(|(id, rule)| (id, rule.configuration.level))
        .collect())
}

pub fn lint_text(
    source: &str,
    source_type: SourceType,
    options: &LintOptions,
) -> Result<LintResult, LintError> {
    lint_text_inner(source, source_type, options, None, None)
}

fn lint_text_inner(
    source: &str,
    source_type: SourceType,
    options: &LintOptions,
    module: Option<(&ModuleGraph, ModuleId)>,
    typed: Option<&TypedSource>,
) -> Result<LintResult, LintError> {
    let configuration = effective_configuration(options)?;
    if let Some(typed) = typed {
        typed.validate(&configuration)?;
    }
    if typed.is_none()
        && RULES.iter().any(|rule| {
            rule.analysis == Analysis::TypeInformation
                && configuration[rule.id].configuration.level != RuleLevel::Off
        })
    {
        return Err(LintError::Analysis(
            "Enabled type rules require source-bound type facts".into(),
        ));
    }
    if module.is_none()
        && module_graph::RULE_IDS
            .iter()
            .any(|id| configuration[*id].configuration.level != RuleLevel::Off)
    {
        return Err(LintError::Analysis(
            "Enabled module rules require a source-bound project graph".into(),
        ));
    }
    let levels = configuration
        .iter()
        .map(|(id, rule)| (id.clone(), rule.configuration.level))
        .collect();
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, source_type, ParseOptions::default());
    if parsed.parsed.has_errors() {
        return Ok(LintResult {
            diagnostics: Vec::new(),
            parse_diagnostics: parsed.parsed.diagnostics,
        });
    }
    let mut visitor = rules::RuleVisitor::new(
        source,
        &interner,
        &parsed.comments,
        &parsed.tokens,
        &levels,
        &configuration,
    );
    parsed
        .parsed
        .module
        .with_ast(|program| visitor.visit_program(program));
    let mut diagnostics = visitor.diagnostics;
    parsed.parsed.module.with_ast(|program| {
        collections::check_keys(
            source,
            program,
            &interner,
            &parsed,
            &levels,
            &mut diagnostics,
        );
    });
    imports::check(&parsed.imports, &configuration, &mut diagnostics);
    if RULES.iter().any(|rule| {
        matches!(rule.analysis, Analysis::Scope | Analysis::ScopeControlFlow)
            && levels[rule.id] != RuleLevel::Off
    }) {
        let globals = effective_globals(options)?;
        parsed
            .parsed
            .module
            .with_ast(|program| -> Result<(), LintError> {
                let mut semantic = wake_ecma_semantic::analyze_source(
                    program,
                    &interner,
                    wake_ecma_semantic::SourceSemanticInput {
                        identifiers: &parsed.identifiers,
                        exports: &parsed.exports,
                        syntax: &parsed.syntax,
                        functions: &parsed.functions,
                        namespaces: &parsed.namespaces,
                    },
                );
                if let Some((graph, _)) = module {
                    semantic
                        .project_external_ambient_values(graph.global_ambient_names(), &interner);
                }
                if levels[hooks::RULE] != RuleLevel::Off {
                    let facts = wake_ecma_semantic::analyze_call_execution(program)
                        .map_err(|error| LintError::Analysis(error.to_string()))?;
                    hooks::check(
                        program,
                        &parsed,
                        &interner,
                        &semantic,
                        &facts,
                        levels[hooks::RULE],
                        &mut diagnostics,
                    );
                }
                scope::check(program, &interner, &semantic, &levels, &mut diagnostics);
                if levels[hooks::dependencies::RULE] != RuleLevel::Off {
                    hooks::dependencies::check(
                        program,
                        &parsed,
                        &interner,
                        &semantic,
                        &configuration[hooks::dependencies::RULE],
                        &mut diagnostics,
                    )?;
                }
                prefer_const::check(
                    source,
                    program,
                    &interner,
                    &parsed,
                    &semantic,
                    &configuration,
                    &mut diagnostics,
                );
                undefined::check(
                    program,
                    &interner,
                    &semantic,
                    &parsed.syntax,
                    &configuration,
                    &globals,
                    &mut diagnostics,
                );
                bindings::check(
                    &interner,
                    &parsed,
                    &semantic,
                    &configuration,
                    &mut diagnostics,
                );
                let types = (source_type.is_typescript()
                    && [
                        "ts/consistent-type-imports",
                        "ts/consistent-type-exports",
                        "js/no-unused-vars",
                    ]
                    .iter()
                    .any(|id| levels[*id] != RuleLevel::Off))
                .then(|| {
                    wake_ecma_semantic::analyze_source_types(
                        &interner,
                        wake_ecma_semantic::SourceTypeInput {
                            identifiers: &parsed.identifiers,
                            syntax: &parsed.syntax,
                            scopes: &parsed.type_scopes,
                            declarations: &parsed.type_declarations,
                            imports: &parsed.imports,
                            exports: &parsed.exports,
                            namespaces: &parsed.namespaces,
                        },
                    )
                });
                if let Some(types) = &types {
                    type_imports::check(
                        &interner,
                        &parsed,
                        &semantic,
                        types,
                        &levels,
                        &mut diagnostics,
                    );
                }
                unused::check(
                    program,
                    &interner,
                    &parsed,
                    &semantic,
                    types.as_ref(),
                    &configuration,
                    &mut diagnostics,
                );
                collections::check_indexes(
                    source,
                    program,
                    &interner,
                    &parsed,
                    &semantic,
                    &levels,
                    &mut diagnostics,
                );
                Ok(())
            })?;
    }
    typescript::check(&parsed.syntax, &levels, &mut diagnostics);
    typescript::check_array_type(&parsed, &configuration, &mut diagnostics);
    if RULES
        .iter()
        .any(|rule| rule.analysis == Analysis::ControlFlow && levels[rule.id] != RuleLevel::Off)
    {
        parsed.parsed.module.with_ast(|program| {
            let facts = wake_ecma_semantic::analyze_control_flow(program);
            flow::check(
                source,
                &parsed.syntax,
                &parsed.comments,
                &facts,
                &levels,
                &mut diagnostics,
            );
        });
    }
    typescript::check_comments(source, &parsed.comments, &levels, &mut diagnostics);
    style::check(source, &parsed.tokens, &configuration, &mut diagnostics);
    style::check_terminators(&parsed.terminators, &configuration, &mut diagnostics);
    style::check_commas(source, &parsed.lists, &configuration, &mut diagnostics);
    indent::check(source, &parsed, &configuration, &mut diagnostics);
    jsx::check(
        source,
        &parsed.syntax,
        &parsed.comments,
        &levels,
        &mut diagnostics,
    );
    a11y::check(source, &parsed, &levels, &mut diagnostics);
    if levels["style/eol-last"] != RuleLevel::Off
        && !source.is_empty()
        && !source.ends_with(['\n', '\r', '\u{2028}', '\u{2029}'])
    {
        diagnostics.push(LintDiagnostic {
            rule_id: "style/eol-last".into(),
            level: levels["style/eol-last"],
            message_id: "missing".into(),
            message: "Missing final line terminator.".into(),
            start: source.len() as u32,
            end: source.len() as u32,
            fix: Some(LintFix {
                edits: vec![TextEdit {
                    start: source.len() as u32,
                    end: source.len() as u32,
                    text: if configuration["style/eol-last"].configuration.options["linebreak"]
                        == "crlf"
                    {
                        "\r\n"
                    } else {
                        "\n"
                    }
                    .into(),
                }],
            }),
        });
    }
    if let Some((graph, id)) = module {
        graph.check(id, &configuration, &mut diagnostics)?;
    }
    if let Some(typed) = typed {
        typed.check(&configuration, &mut diagnostics);
    }
    directives::apply(
        source,
        &parsed.comments,
        &mut diagnostics,
        options.report_unused_disable,
    );
    diagnostics.sort_by(|a, b| {
        (a.start, a.end, &a.rule_id, &a.message_id).cmp(&(
            b.start,
            b.end,
            &b.rule_id,
            &b.message_id,
        ))
    });
    // Nested condition roots can visit the same source assignment; each finding is returned
    // once, independent of the number of grammar paths which discover it.
    diagnostics.dedup();
    Ok(LintResult {
        diagnostics,
        parse_diagnostics: parsed.parsed.diagnostics,
    })
}
