//! Native, dependency-light TypeScript props documentation for Wake docs.
//!
//! This is deliberately separate from the normal bundler AST. It resolves the shapes common in
//! React component props and keeps unsupported package types as inherited sources instead of
//! recursively loading all of `node_modules`.

use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiDoc {
    pub symbol: String,
    pub source: String,
    pub description: String,
    pub props: Vec<ApiProp>,
    pub inherited: Vec<InheritedGroup>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiProp {
    pub name: String,
    pub type_text: String,
    pub required: bool,
    pub description: String,
    pub default_value: Option<String>,
    pub deprecated: Option<String>,
    pub since: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InheritedGroup {
    pub name: String,
    pub source: String,
    pub type_text: String,
}

#[derive(Debug)]
pub enum ApiError {
    Io(PathBuf, String),
    SymbolNotFound { path: PathBuf, symbol: String },
    CircularType(Vec<String>),
    InvalidSource(PathBuf, String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "cannot read `{}`: {error}", path.display()),
            Self::SymbolNotFound { path, symbol } => {
                write!(f, "cannot find type `{symbol}` in `{}`", path.display())
            }
            Self::CircularType(chain) => write!(f, "local type cycle: {}", chain.join(" -> ")),
            Self::InvalidSource(path, error) => {
                write!(
                    f,
                    "invalid type declaration in `{}`: {error}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ApiError {}

/// Extract an interface or object type alias. Local imports/re-exports are followed. External
/// types are represented by [`InheritedGroup`] values and produce a non-fatal warning.
pub fn extract_api(
    source: impl AsRef<Path>,
    symbol: &str,
    component: Option<&str>,
) -> Result<ApiDoc, ApiError> {
    let source = canonical(source.as_ref())?;
    let mut resolver = Resolver::default();
    let mut resolved = resolver.resolve(&source, symbol, &mut Vec::new())?;
    if let Some(component) = component {
        let defaults = infer_defaults(&read(&source)?, component);
        for prop in &mut resolved.props {
            if prop.default_value.is_none() {
                prop.default_value = defaults.get(&prop.name).cloned();
            }
        }
    }
    resolved.props.sort_by(|a, b| {
        b.required
            .cmp(&a.required)
            .then_with(|| a.name.cmp(&b.name))
    });
    deduplicate(&mut resolved);
    Ok(ApiDoc {
        symbol: symbol.to_string(),
        source: source.to_string_lossy().into_owned(),
        description: resolved.description,
        props: resolved.props,
        inherited: resolved.inherited,
        warnings: resolved.warnings,
    })
}

#[derive(Debug, Clone, Default)]
struct Resolved {
    description: String,
    props: Vec<ApiProp>,
    inherited: Vec<InheritedGroup>,
    warnings: Vec<String>,
}

#[derive(Debug, Default)]
struct Resolver {
    cache: BTreeMap<(PathBuf, String), Resolved>,
}

#[derive(Debug)]
struct Declaration {
    description: String,
    props: Vec<ApiProp>,
    bases: Vec<String>,
    reexport: Option<(String, String)>,
}

#[derive(Debug)]
struct Import {
    imported: String,
    source: String,
}

impl Resolver {
    fn resolve(
        &mut self,
        path: &Path,
        symbol: &str,
        stack: &mut Vec<(PathBuf, String)>,
    ) -> Result<Resolved, ApiError> {
        let key = (path.to_path_buf(), symbol.to_string());
        if let Some(cached) = self.cache.get(&key) {
            return Ok(cached.clone());
        }
        if let Some(start) = stack.iter().position(|item| item == &key) {
            let mut chain: Vec<String> = stack[start..]
                .iter()
                .map(|(file, name)| format!("{}#{name}", file.display()))
                .collect();
            chain.push(format!("{}#{symbol}", path.display()));
            return Err(ApiError::CircularType(chain));
        }

        stack.push(key.clone());
        let source = read(path)?;
        let imports = parse_imports(&source);
        let declaration =
            find_declaration(&source, path, symbol)?.ok_or_else(|| ApiError::SymbolNotFound {
                path: path.to_path_buf(),
                symbol: symbol.to_string(),
            })?;
        if let Some((name, specifier)) = declaration.reexport {
            let imported_path = resolve_local_import(path, &specifier)?;
            let result = self.resolve(&imported_path, &name, stack)?;
            stack.pop();
            self.cache.insert(key, result.clone());
            return Ok(result);
        }

        let mut result = Resolved {
            description: declaration.description,
            props: declaration.props,
            ..Resolved::default()
        };
        for expression in declaration.bases {
            self.merge_expression(path, &imports, &expression, stack, &mut result)?;
        }
        stack.pop();
        deduplicate(&mut result);
        self.cache.insert(key, result.clone());
        Ok(result)
    }

    fn merge_expression(
        &mut self,
        path: &Path,
        imports: &BTreeMap<String, Import>,
        expression: &str,
        stack: &mut Vec<(PathBuf, String)>,
        result: &mut Resolved,
    ) -> Result<(), ApiError> {
        let expression = strip_parens(expression.trim());
        for part in split_top_level(expression, '&') {
            let part = strip_parens(part.trim());
            if part.is_empty() {
                continue;
            }
            if let Some((utility, inner, keys)) = parse_utility(part) {
                let mut temporary = Resolved::default();
                self.merge_expression(path, imports, &inner, stack, &mut temporary)?;
                apply_utility(
                    utility,
                    &mut temporary.props,
                    keys.as_deref().unwrap_or_default(),
                );
                merge(result, temporary);
            } else if part.starts_with('{') && part.ends_with('}') {
                result
                    .props
                    .extend(parse_members(&part[1..part.len() - 1], path));
            } else if split_top_level(part, '|').len() > 1 {
                result.inherited.push(InheritedGroup {
                    name: part.to_string(),
                    source: path.to_string_lossy().into_owned(),
                    type_text: part.to_string(),
                });
                result.warnings.push(format!(
                    "union `{part}` cannot be flattened safely; preserved as a type source"
                ));
            } else {
                let name = leading_identifier(part);
                if name.is_empty() {
                    result.warnings.push(format!("unsupported type `{part}`"));
                    continue;
                }
                if let Some(import) = imports.get(name) {
                    if import.source.starts_with('.') {
                        let imported_path = resolve_local_import(path, &import.source)?;
                        merge(
                            result,
                            self.resolve(&imported_path, &import.imported, stack)?,
                        );
                    } else {
                        result.inherited.push(InheritedGroup {
                            name: name.to_string(),
                            source: import.source.clone(),
                            type_text: part.to_string(),
                        });
                    }
                    continue;
                }
                match self.resolve(path, name, stack) {
                    Ok(resolved) => merge(result, resolved),
                    Err(ApiError::SymbolNotFound { .. }) => {
                        result.inherited.push(InheritedGroup {
                            name: name.to_string(),
                            source: path.to_string_lossy().into_owned(),
                            type_text: part.to_string(),
                        });
                        result
                            .warnings
                            .push(format!("cannot expand `{part}`; preserved as source text"));
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }
}

fn find_declaration(
    source: &str,
    path: &Path,
    symbol: &str,
) -> Result<Option<Declaration>, ApiError> {
    let escaped = regex::escape(symbol);
    let interface = Regex::new(&format!(
        r"(?m)(?:export\s+)?(?:declare\s+)?interface\s+{escaped}(?:\s*<[^{{]*>)?\s*([^{{]*)\{{"
    ))
    .expect("valid interface regex");
    if let Some(captures) = interface.captures(source) {
        let whole = captures.get(0).expect("interface capture");
        let open = whole.end() - 1;
        let close = find_matching(source, open, '{', '}').ok_or_else(|| {
            ApiError::InvalidSource(
                path.to_path_buf(),
                format!("interface {symbol} has no `}}`"),
            )
        })?;
        let header = captures.get(1).map_or("", |value| value.as_str());
        let bases = header
            .trim()
            .strip_prefix("extends")
            .map(|value| split_top_level(value, ','))
            .unwrap_or_default();
        return Ok(Some(Declaration {
            description: preceding_jsdoc(source, whole.start()).description,
            props: parse_members(&source[open + 1..close], path),
            bases,
            reexport: None,
        }));
    }

    let alias = Regex::new(&format!(
        r"(?m)(?:export\s+)?(?:declare\s+)?type\s+{escaped}(?:\s*<[^=]*>)?\s*="
    ))
    .expect("valid alias regex");
    if let Some(found) = alias.find(source) {
        let end = find_statement_end(source, found.end());
        return Ok(Some(Declaration {
            description: preceding_jsdoc(source, found.start()).description,
            props: Vec::new(),
            bases: vec![source[found.end()..end].trim().to_string()],
            reexport: None,
        }));
    }

    let reexport = Regex::new(r#"(?m)export\s*\{([^}]*)\}\s*from\s*["']([^"']+)["']"#)
        .expect("valid re-export regex");
    for captures in reexport.captures_iter(source) {
        for item in captures[1].split(',') {
            let words: Vec<_> = item.split_whitespace().collect();
            let pair = match words.as_slice() {
                [name] => Some((*name, *name)),
                [name, "as", alias] => Some((*name, *alias)),
                _ => None,
            };
            if let Some((imported, exported)) = pair
                && exported == symbol
            {
                return Ok(Some(Declaration {
                    description: String::new(),
                    props: Vec::new(),
                    bases: Vec::new(),
                    reexport: Some((imported.to_string(), captures[2].to_string())),
                }));
            }
        }
    }
    Ok(None)
}

fn parse_imports(source: &str) -> BTreeMap<String, Import> {
    let regex = Regex::new(r#"(?m)import\s+(?:type\s+)?\{([^}]*)\}\s+from\s+["']([^"']+)["']"#)
        .expect("valid import regex");
    let mut imports = BTreeMap::new();
    for captures in regex.captures_iter(source) {
        for item in captures[1].split(',') {
            let words: Vec<_> = item.split_whitespace().collect();
            let pair = match words.as_slice() {
                [name] => Some((*name, *name)),
                [name, "as", alias] => Some((*name, *alias)),
                _ => None,
            };
            if let Some((imported, local)) = pair {
                imports.insert(
                    local.to_string(),
                    Import {
                        imported: imported.to_string(),
                        source: captures[2].to_string(),
                    },
                );
            }
        }
    }
    imports
}

fn parse_members(body: &str, path: &Path) -> Vec<ApiProp> {
    let mut result = Vec::new();
    let mut cursor = 0;
    while cursor < body.len() {
        while cursor < body.len() && body.as_bytes()[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= body.len() {
            break;
        }
        let start = cursor;
        if body[cursor..].starts_with("/**") {
            let Some(relative_end) = body[cursor + 3..].find("*/") else {
                break;
            };
            cursor += relative_end + 5;
            while cursor < body.len() && body.as_bytes()[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
        }
        let mut depth = Depth::default();
        let mut quote = None;
        let mut escaped = false;
        while cursor < body.len() {
            let ch = body.as_bytes()[cursor] as char;
            if let Some(active) = quote {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == active {
                    quote = None;
                }
            } else if matches!(ch, '\'' | '"' | '`') {
                quote = Some(ch);
            } else if depth.is_zero() && ch == ';' {
                cursor += 1;
                break;
            } else {
                depth.update(ch);
            }
            cursor += 1;
        }
        if let Some(prop) = parse_member(body[start..cursor].trim(), path) {
            result.push(prop);
        }
    }
    result
}

fn parse_member(raw: &str, path: &Path) -> Option<ApiProp> {
    let (comment, declaration) = leading_jsdoc(raw);
    let colon = find_top_level(declaration, ':')?;
    let mut name = declaration[..colon].trim();
    if name.starts_with('[') || name.starts_with('(') {
        return None;
    }
    let required = !name.ends_with('?');
    name = name
        .trim_end_matches('?')
        .trim_matches(|ch| ch == '\'' || ch == '"');
    if !is_property_name(name) {
        return None;
    }
    let docs = parse_jsdoc(comment);
    Some(ApiProp {
        name: name.to_string(),
        type_text: declaration[colon + 1..]
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_string(),
        required,
        description: docs.description,
        default_value: docs.tags.get("default").cloned(),
        deprecated: docs.tags.get("deprecated").cloned(),
        since: docs.tags.get("since").cloned(),
        source: path.to_string_lossy().into_owned(),
    })
}

fn parse_utility(value: &str) -> Option<(&str, String, Option<Vec<String>>)> {
    for name in ["Partial", "Required", "Pick", "Omit"] {
        let prefix = format!("{name}<");
        if value.starts_with(&prefix) && value.ends_with('>') {
            let args = split_top_level(&value[prefix.len()..value.len() - 1], ',');
            if matches!(name, "Partial" | "Required") && args.len() == 1 {
                return Some((name, args[0].trim().to_string(), None));
            }
            if matches!(name, "Pick" | "Omit") && args.len() == 2 {
                let keys = split_top_level(&args[1], '|')
                    .into_iter()
                    .map(|key| {
                        key.trim()
                            .trim_matches(|ch| ch == '\'' || ch == '"')
                            .to_string()
                    })
                    .collect();
                return Some((name, args[0].trim().to_string(), Some(keys)));
            }
        }
    }
    None
}

fn apply_utility(name: &str, props: &mut Vec<ApiProp>, keys: &[String]) {
    match name {
        "Partial" => props.iter_mut().for_each(|prop| prop.required = false),
        "Required" => props.iter_mut().for_each(|prop| prop.required = true),
        "Pick" => props.retain(|prop| keys.contains(&prop.name)),
        "Omit" => props.retain(|prop| !keys.contains(&prop.name)),
        _ => {}
    }
}

fn infer_defaults(source: &str, component: &str) -> BTreeMap<String, String> {
    let regex = Regex::new(&format!(
        r"(?s)(?:function\s+{}|const\s+{}\s*=\s*)[^{{=]*\(\s*\{{",
        regex::escape(component),
        regex::escape(component)
    ))
    .expect("valid component regex");
    let Some(found) = regex.find(source) else {
        return BTreeMap::new();
    };
    let open = found.end() - 1;
    let Some(close) = find_matching(source, open, '{', '}') else {
        return BTreeMap::new();
    };
    split_top_level(&source[open + 1..close], ',')
        .into_iter()
        .filter_map(|item| {
            let equal = find_top_level(&item, '=')?;
            let name = item[..equal].trim();
            let value = item[equal + 1..].trim();
            (is_property_name(name) && is_static_literal(value))
                .then(|| (name.to_string(), value.to_string()))
        })
        .collect()
}

#[derive(Debug, Default)]
struct JsDoc {
    description: String,
    tags: BTreeMap<String, String>,
}

fn preceding_jsdoc(source: &str, offset: usize) -> JsDoc {
    let prefix = source[..offset].trim_end();
    if !prefix.ends_with("*/") {
        return JsDoc::default();
    }
    prefix
        .rfind("/**")
        .map(|start| parse_jsdoc(Some(&prefix[start..])))
        .unwrap_or_default()
}

fn leading_jsdoc(value: &str) -> (Option<&str>, &str) {
    let value = value.trim_start();
    if value.starts_with("/**")
        && let Some(end) = value.find("*/")
    {
        return (Some(&value[..end + 2]), value[end + 2..].trim());
    }
    (None, value)
}

fn parse_jsdoc(comment: Option<&str>) -> JsDoc {
    let Some(comment) = comment else {
        return JsDoc::default();
    };
    let mut docs = JsDoc::default();
    let mut description = Vec::new();
    let mut active_tag: Option<String> = None;
    for raw in comment
        .trim_start_matches("/**")
        .trim_end_matches("*/")
        .lines()
    {
        let line = raw.trim().trim_start_matches('*').trim();
        if let Some(tag) = line.strip_prefix('@') {
            let mut parts = tag.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or_default().to_string();
            docs.tags.insert(
                name.clone(),
                parts.next().unwrap_or_default().trim().to_string(),
            );
            active_tag = Some(name);
        } else if !line.is_empty() {
            if let Some(name) = &active_tag {
                let value = docs.tags.entry(name.clone()).or_default();
                if !value.is_empty() {
                    value.push(' ');
                }
                value.push_str(line);
            } else {
                description.push(line);
            }
        }
    }
    docs.description = description.join(" ");
    docs
}

fn resolve_local_import(path: &Path, specifier: &str) -> Result<PathBuf, ApiError> {
    let base = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(specifier);
    let mut candidates = vec![base.clone()];
    if base.extension().is_some() {
        let stem = base.with_extension("");
        candidates.extend(["ts", "tsx", "d.ts"].map(|ext| stem.with_extension(ext)));
    } else {
        candidates.extend(["ts", "tsx", "d.ts"].map(|ext| base.with_extension(ext)));
        candidates.extend(["index.ts", "index.tsx", "index.d.ts"].map(|name| base.join(name)));
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .map(|candidate| fs::canonicalize(&candidate).unwrap_or(candidate))
        .ok_or_else(|| ApiError::Io(base, format!("cannot resolve local import `{specifier}`")))
}

fn read(path: &Path) -> Result<String, ApiError> {
    fs::read_to_string(path).map_err(|error| ApiError::Io(path.to_path_buf(), error.to_string()))
}

fn canonical(path: &Path) -> Result<PathBuf, ApiError> {
    fs::canonicalize(path).map_err(|error| ApiError::Io(path.to_path_buf(), error.to_string()))
}

fn merge(target: &mut Resolved, source: Resolved) {
    target.props.extend(source.props);
    target.inherited.extend(source.inherited);
    target.warnings.extend(source.warnings);
}

fn deduplicate(result: &mut Resolved) {
    let mut props = BTreeSet::new();
    result.props.retain(|prop| props.insert(prop.name.clone()));
    let mut groups = BTreeSet::new();
    result
        .inherited
        .retain(|group| groups.insert((group.name.clone(), group.source.clone())));
    result.warnings.sort();
    result.warnings.dedup();
}

fn split_top_level(value: &str, delimiter: char) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut depth = Depth::default();
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == active {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
        } else if depth.is_zero() && ch == delimiter {
            result.push(value[start..index].to_string());
            start = index + ch.len_utf8();
        } else {
            depth.update(ch);
        }
    }
    result.push(value[start..].to_string());
    result
}

#[derive(Debug, Default)]
struct Depth {
    round: usize,
    square: usize,
    curly: usize,
    angle: usize,
}

impl Depth {
    fn update(&mut self, ch: char) {
        match ch {
            '(' => self.round += 1,
            ')' => self.round = self.round.saturating_sub(1),
            '[' => self.square += 1,
            ']' => self.square = self.square.saturating_sub(1),
            '{' => self.curly += 1,
            '}' => self.curly = self.curly.saturating_sub(1),
            '<' => self.angle += 1,
            '>' => self.angle = self.angle.saturating_sub(1),
            _ => {}
        }
    }

    fn is_zero(&self) -> bool {
        self.round == 0 && self.square == 0 && self.curly == 0 && self.angle == 0
    }
}

fn find_matching(source: &str, start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0;
    let mut quote = None;
    let mut escaped = false;
    for (offset, ch) in source[start..].char_indices() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == active {
                quote = None;
            }
        } else if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
        } else if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some(start + offset);
            }
        }
    }
    None
}

fn find_statement_end(source: &str, start: usize) -> usize {
    let mut depth = Depth::default();
    let mut quote = None;
    for (offset, ch) in source[start..].char_indices() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
        } else if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
        } else if depth.is_zero() && ch == ';' {
            return start + offset;
        } else {
            depth.update(ch);
        }
    }
    source.len()
}

fn find_top_level(value: &str, needle: char) -> Option<usize> {
    let mut depth = Depth::default();
    let mut quote = None;
    for (index, ch) in value.char_indices() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
        } else if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
        } else if depth.is_zero() && ch == needle {
            return Some(index);
        } else {
            depth.update(ch);
        }
    }
    None
}

fn strip_parens(mut value: &str) -> &str {
    loop {
        let trimmed = value.trim();
        if trimmed.starts_with('(')
            && trimmed.ends_with(')')
            && find_matching(trimmed, 0, '(', ')') == Some(trimmed.len() - 1)
        {
            value = &trimmed[1..trimmed.len() - 1];
        } else {
            return trimmed;
        }
    }
}

fn leading_identifier(value: &str) -> &str {
    let end = value
        .char_indices()
        .find_map(|(index, ch)| {
            (!ch.is_ascii_alphanumeric() && ch != '_' && ch != '$').then_some(index)
        })
        .unwrap_or(value.len());
    &value[..end]
}

fn is_property_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_' || ch == '$')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$' | '-'))
}

fn is_static_literal(value: &str) -> bool {
    matches!(value, "true" | "false" | "null" | "undefined")
        || value.parse::<f64>().is_ok()
        || (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
        || (value.starts_with('[') && value.ends_with(']') && !value.contains("..."))
        || (value.starts_with('{') && value.ends_with('}') && !value.contains("..."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture(files: &[(&str, &str)]) -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("wake-tsdoc-{id}"));
        fs::create_dir_all(&root).expect("fixture directory");
        for (name, source) in files {
            fs::write(root.join(name), source).expect("fixture file");
        }
        root
    }

    #[test]
    fn extracts_jsdoc_and_static_component_defaults() {
        let root = fixture(&[(
            "button.tsx",
            r#"
                /** Button properties. */
                export interface ButtonProps {
                    /** Visible label. */
                    label: string;
                    /**
                     * Visual intent.
                     * @default "primary"
                     * @since 1.2.0
                     */
                    intent?: "primary" | "danger";
                    /**
                     * Old flag.
                     * @deprecated Use intent.
                     */
                    danger?: boolean;
                    count?: number;
                }
                export function Button({ count = 2 }: ButtonProps) { return count }
            "#,
        )]);
        let doc = extract_api(root.join("button.tsx"), "ButtonProps", Some("Button")).unwrap();
        assert_eq!(doc.description, "Button properties.");
        assert_eq!(doc.props.len(), 4);
        assert_eq!(doc.props[0].name, "label");
        let intent = doc.props.iter().find(|prop| prop.name == "intent").unwrap();
        assert_eq!(intent.default_value.as_deref(), Some("\"primary\""));
        assert_eq!(intent.since.as_deref(), Some("1.2.0"));
        let danger = doc.props.iter().find(|prop| prop.name == "danger").unwrap();
        assert_eq!(danger.deprecated.as_deref(), Some("Use intent."));
        let count = doc.props.iter().find(|prop| prop.name == "count").unwrap();
        assert_eq!(count.default_value.as_deref(), Some("2"));
    }

    #[test]
    fn resolves_local_utilities_and_collapses_package_types() {
        let root = fixture(&[
            (
                "base.ts",
                "export interface BaseProps { id: string; hidden?: boolean; }",
            ),
            (
                "input.tsx",
                r#"
                    import type { BaseProps } from "./base";
                    import type { HTMLAttributes } from "react";
                    export type InputProps = Required<Pick<BaseProps, "id">>
                        & Omit<BaseProps, "id">
                        & HTMLAttributes<HTMLInputElement>
                        & { value?: string };
                "#,
            ),
        ]);
        let doc = extract_api(root.join("input.tsx"), "InputProps", None).unwrap();
        assert!(
            doc.props
                .iter()
                .any(|prop| prop.name == "id" && prop.required)
        );
        assert!(doc.props.iter().any(|prop| prop.name == "hidden"));
        assert!(doc.props.iter().any(|prop| prop.name == "value"));
        assert!(doc.inherited.iter().any(|group| group.source == "react"));
    }

    #[test]
    fn reports_non_converging_local_cycles() {
        let root = fixture(&[(
            "cycle.ts",
            "export type A = B & { a: string }; export type B = A & { b: string };",
        )]);
        assert!(matches!(
            extract_api(root.join("cycle.ts"), "A", None),
            Err(ApiError::CircularType(_))
        ));
    }
}
