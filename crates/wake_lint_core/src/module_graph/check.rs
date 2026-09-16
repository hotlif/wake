use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
use wake_common::{JsString, Span};

use super::{
    ModuleFile, ModuleGraph, ModuleId, ModuleResolution as Resolution, parameters::PathZone,
    topology,
};
use crate::{
    EffectiveRule, LintDiagnostic, LintError, ModuleRequest, ModuleRequestKind as Kind,
    RuleConfiguration, RuleLevel,
};

pub(super) fn check(
    graph: &ModuleGraph,
    id: ModuleId,
    configuration: &BTreeMap<String, EffectiveRule>,
    diagnostics: &mut Vec<LintDiagnostic>,
) -> Result<(), LintError> {
    let file = graph.file(id)?;
    for &name in super::RULE_IDS {
        let config = &configuration[name].configuration;
        if config.level == RuleLevel::Off {
            continue;
        }
        let mut rule = Reporter {
            name,
            config,
            diagnostics,
        };
        match name {
            "import/no-unresolved" => unresolved(file, &mut rule),
            "import/no-cycle" => cycles(graph, id, file, &mut rule)?,
            "import/no-duplicates" => duplicates(graph, file, &mut rule),
            "import/no-restricted-paths" => restricted(graph, file, &mut rule),
            "import/no-extraneous-dependencies" => extraneous(file, &mut rule),
            "import/order" => order(file, &mut rule),
            _ => unreachable!(),
        }
    }
    Ok(())
}

struct Reporter<'a> {
    name: &'static str,
    config: &'a RuleConfiguration,
    diagnostics: &'a mut Vec<LintDiagnostic>,
}
impl Reporter<'_> {
    fn boolean(&self, name: &str) -> bool {
        self.config.options[name]
            .as_bool()
            .expect("validated boolean")
    }
    fn text(&self, name: &str) -> &str {
        self.config.options[name]
            .as_str()
            .expect("validated string")
    }
    fn report(&mut self, span: Span, message_id: &str, message: impl Into<String>) {
        self.diagnostics.push(LintDiagnostic {
            rule_id: self.name.into(),
            level: self.config.level,
            message_id: message_id.into(),
            message: message.into(),
            start: span.lo,
            end: span.hi,
            fix: None,
        });
    }
}

fn patterns(value: &serde_json::Value) -> Vec<Regex> {
    value
        .as_array()
        .expect("validated array")
        .iter()
        .map(|entry| {
            Regex::new(entry.as_str().expect("validated pattern")).expect("validated regex")
        })
        .collect()
}

fn unresolved(file: &ModuleFile, rule: &mut Reporter<'_>) {
    let ignore = patterns(&rule.config.options["ignore"]);
    for (request, resolution) in file.edges() {
        if request.type_only && !rule.boolean("include_types")
            || matches!(request.kind, Kind::Require | Kind::ImportEquals)
                && !rule.boolean("commonjs")
            || request.kind == Kind::DynamicImport && !rule.boolean("dynamic_imports")
            || request
                .specifier
                .as_ref()
                .and_then(JsString::as_str)
                .is_some_and(|specifier| ignore.iter().any(|pattern| pattern.is_match(specifier)))
        {
            continue;
        }
        match resolution {
            Resolution::Unresolved { reason, .. } => rule.report(
                request.specifier_span,
                "unresolved",
                format!("Cannot resolve module request: {reason}"),
            ),
            _ if !request.attributes_known && rule.boolean("report_unknown") => rule.report(
                request.specifier_span,
                "unknown",
                "Module request attributes cannot be determined statically.",
            ),
            Resolution::Unknown(reason) if rule.boolean("report_unknown") => rule.report(
                request.specifier_span,
                "unknown",
                format!("Cannot determine module request: {reason}"),
            ),
            _ => (),
        }
    }
    if file.incomplete && rule.boolean("report_unknown") && rule.boolean("commonjs") {
        rule.report(
            Span::new(0, 0),
            "unknown",
            "Dynamic or incomplete source scope prevents proving all module loader requests.",
        );
    }
}

fn cycles(
    graph: &ModuleGraph,
    id: ModuleId,
    file: &ModuleFile,
    rule: &mut Reporter<'_>,
) -> Result<(), LintError> {
    let policy = topology::Policy {
        commonjs: rule.boolean("commonjs"),
        dynamic: rule.boolean("dynamic_imports"),
        types: rule.boolean("include_types"),
        ignore_external: rule.boolean("ignore_external"),
    };
    let facts = graph.cycles[policy.key()]
        .get_or_init(|| topology::build(graph, policy))
        .as_ref()
        .map_err(Clone::clone)?;
    if file.incomplete {
        rule.report(
            Span::new(0, 0),
            "incomplete",
            "Source scope is incomplete; absence of dependency cycles cannot be proven.",
        );
    }
    for (request, resolution) in file.edges() {
        if !policy.includes(request, resolution) {
            continue;
        }
        let incomplete = if !request.attributes_known {
            true
        } else {
            match resolution {
                Resolution::File { target, .. } => {
                    if facts.component[id.0] == facts.component[target.0] {
                        rule.report(
                            request.specifier_span,
                            "cycle",
                            format!(
                                "Module request participates in a dependency cycle with '{}'.",
                                graph.files[target.0].path
                            ),
                        );
                        continue;
                    }
                    facts.incomplete[target.0]
                }
                Resolution::Unresolved { .. }
                | Resolution::Unknown(_)
                | Resolution::Opaque { .. } => true,
                _ => false,
            }
        };
        if incomplete {
            rule.report(request.specifier_span, "incomplete", "Dependency graph is incomplete; absence of a cycle through this request cannot be proven.");
        }
    }
    Ok(())
}

fn identity<'a>(
    graph: &'a ModuleGraph,
    resolution: &'a Resolution,
) -> Option<(&'static str, &'a str)> {
    match resolution {
        Resolution::File { target, .. } => Some(("module", &graph.files[target.0].identity)),
        Resolution::Resource { identity, .. } | Resolution::Opaque { identity, .. } => {
            Some(("module", identity))
        }
        Resolution::Builtin(name) => Some(("builtin", name)),
        _ => None,
    }
}

fn path<'a>(graph: &'a ModuleGraph, resolution: &'a Resolution) -> Option<&'a str> {
    match resolution {
        Resolution::File { target, .. } => Some(&graph.files[target.0].path),
        Resolution::Resource { path, .. } | Resolution::Opaque { path, .. } => Some(path),
        _ => None,
    }
}

fn duplicates(graph: &ModuleGraph, file: &ModuleFile, rule: &mut Reporter<'_>) {
    let mut seen = HashSet::new();
    for (request, resolution) in file.edges() {
        if request.kind != Kind::Import || !request.attributes_known {
            continue;
        }
        let Some(identity) = identity(graph, resolution) else {
            continue;
        };
        if !seen.insert((
            request.parent,
            identity,
            &request.attributes,
            rule.boolean("separate_type_imports") && request.type_only,
        )) {
            rule.report(
                request.specifier_span,
                "duplicate",
                "This resolved module is imported more than once with the same attributes.",
            );
        }
    }
}

fn restricted(graph: &ModuleGraph, file: &ModuleFile, rule: &mut Reporter<'_>) {
    let zones: Vec<PathZone> =
        serde_json::from_value(rule.config.options["zones"].clone()).expect("validated zones");
    for zone in zones {
        let to = Regex::new(&zone.to).expect("validated regex");
        if !to.is_match(&file.path) {
            continue;
        }
        let from = Regex::new(&zone.from).expect("validated regex");
        let except: Vec<_> = zone
            .except
            .iter()
            .map(|pattern| Regex::new(pattern).expect("validated regex"))
            .collect();
        for (request, resolution) in file.edges() {
            let Some(target) = path(graph, resolution) else {
                continue;
            };
            if from.is_match(target) && !except.iter().any(|pattern| pattern.is_match(target)) {
                rule.report(
                    request.specifier_span,
                    "restricted",
                    zone.message.clone().unwrap_or_else(|| {
                        format!(
                            "Importing '{target}' from '{}' violates a configured path zone.",
                            file.path
                        )
                    }),
                );
            }
        }
    }
}

fn extraneous(file: &ModuleFile, rule: &mut Reporter<'_>) {
    for (request, resolution) in file.edges() {
        if request.type_only && !rule.boolean("include_types") {
            continue;
        }
        let Some(package) = resolution.package() else {
            continue;
        };
        if package.self_reference
            || package.production
            || package.development && rule.boolean("dev_dependencies")
            || package.optional && rule.boolean("optional_dependencies")
            || package.peer && rule.boolean("peer_dependencies")
        {
            continue;
        }
        rule.report(request.specifier_span, "extraneous", format!("Package '{}' is not declared in an allowed dependency section of the importing package.", package.name));
    }
}

fn group(request: &ModuleRequest, resolution: &Resolution) -> &'static str {
    if request.type_only {
        return "type";
    }
    if matches!(resolution, Resolution::Builtin(_)) {
        return "builtin";
    }
    if matches!(
        resolution,
        Resolution::Unknown(_) | Resolution::Unresolved { .. }
    ) {
        return "unknown";
    }
    if resolution.external() {
        return "external";
    }
    let Some(specifier) = request.specifier.as_ref().and_then(JsString::as_str) else {
        return "unknown";
    };
    if specifier == ".." || specifier.starts_with("../") {
        return "parent";
    }
    if specifier == "."
        || specifier == "./"
        || specifier == "./index"
        || ["js", "jsx", "ts", "tsx", "mjs", "cjs", "mts", "cts", "json"]
            .iter()
            .any(|extension| specifier == format!("./index.{extension}"))
    {
        return "index";
    }
    if specifier.starts_with("./") {
        return "sibling";
    }
    "internal"
}

fn blank_line(gap: &str) -> bool {
    let mut previous_end = None;
    let mut chars = gap.char_indices().peekable();
    while let Some((offset, character)) = chars.next() {
        if !matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            continue;
        }
        if previous_end.is_some_and(|end| gap[end..offset].trim().is_empty()) {
            return true;
        }
        let mut end = offset + character.len_utf8();
        if character == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
            end = chars.next().expect("peeked newline").0 + 1;
        }
        previous_end = Some(end);
    }
    false
}

fn order(file: &ModuleFile, rule: &mut Reporter<'_>) {
    let groups: HashMap<_, _> = rule.config.options["groups"]
        .as_array()
        .expect("validated groups")
        .iter()
        .enumerate()
        .map(|(rank, name)| (name.as_str().expect("validated group").to_owned(), rank))
        .collect();
    let mut highest: HashMap<Option<Span>, usize> = HashMap::new();
    let mut names = HashMap::new();
    let mut previous: HashMap<Option<Span>, (&ModuleRequest, usize)> = HashMap::new();
    for (request, resolution) in file.edges() {
        if !matches!(request.kind, Kind::Import | Kind::ImportEquals) {
            continue;
        }
        let rank = groups[group(request, resolution)];
        let highest = highest.entry(request.parent).or_default();
        if rank < *highest {
            rule.report(
                request.specifier_span,
                "group",
                "Import group appears after a group which should follow it.",
            );
        }
        *highest = (*highest).max(rank);
        if let Some(specifier) = &request.specifier {
            let name = if rule.boolean("case_insensitive") {
                lowercase_specifier(specifier)
            } else {
                specifier.clone()
            };
            if let Some(prior) = names.insert((request.parent, rank), name.clone()) {
                let wrong = match rule.text("alphabetize") {
                    "asc" => prior > name,
                    "desc" => prior < name,
                    _ => false,
                };
                if wrong {
                    rule.report(
                        request.specifier_span,
                        "alphabetical",
                        "Import specifier is out of alphabetical order within its group.",
                    );
                }
            }
        }
        if let Some((prior, prior_rank)) = previous.insert(request.parent, (request, rank)) {
            let gap = &file.source[prior.span.hi as usize..request.span.lo as usize];
            let blank = blank_line(gap);
            let wrong = match rule.text("newlines") {
                "always" => blank != (prior_rank != rank),
                "never" => blank,
                _ => false,
            };
            if wrong {
                rule.report(
                    request.specifier_span,
                    "newline",
                    "Import groups do not have the configured blank-line separation.",
                );
            }
        }
    }
}

fn lowercase_specifier(value: &JsString) -> JsString {
    if let Some(text) = value.as_str() {
        return text.to_lowercase().into();
    }
    let mut units = Vec::new();
    let mut scalars = String::new();
    for scalar in char::decode_utf16(value.code_units()) {
        match scalar {
            Ok(scalar) => scalars.push(scalar),
            Err(surrogate) => {
                units.extend(scalars.to_lowercase().encode_utf16());
                scalars.clear();
                units.push(surrogate.unpaired_surrogate());
            }
        }
    }
    units.extend(scalars.to_lowercase().encode_utf16());
    JsString::from_utf16(&units)
}
