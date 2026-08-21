use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wake_bundler::library::{LibraryGraph, LibraryGraphOptions, PreserveModuleFormat};
use wake_common::{FileSystem, OsFileSystem};
use wake_resolver::{PnpFileSystem, PnpManifest};

use super::{
    CancellationToken, OutputFile, ProjectOptions, WakeError, absolute_from, atomic_write,
    canonical_project_root, commit_staged_output, is_bare_package_name,
};

#[derive(Debug, Clone, Default)]
pub struct GenerateCssTokenOptions {
    pub project: ProjectOptions,
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateCssTokenResult {
    pub success: bool,
    pub duration_ms: f64,
    pub output_file: String,
    pub files: Vec<OutputFile>,
}

#[derive(Debug, Clone, Default)]
pub struct GenerateDocgenOptions {
    pub project: ProjectOptions,
    pub entry: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateDocgenResult {
    pub success: bool,
    pub duration_ms: f64,
    pub entry: String,
    pub output_file: String,
    pub files: Vec<OutputFile>,
}

#[derive(Debug, Clone, Default)]
pub struct LibraryBuildOptions {
    pub project: ProjectOptions,
    pub entry: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryBuildResult {
    pub success: bool,
    pub module_count: usize,
    pub updated_module_count: usize,
    pub cached_module_count: usize,
    pub duration_ms: f64,
    pub output_dir: String,
    pub esm_entry: String,
    pub cjs_entry: String,
    pub declaration_entry: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub css_entry: Option<String>,
    pub files: Vec<OutputFile>,
    pub diagnostics: Vec<super::DiagnosticInfo>,
}

#[derive(Debug, Deserialize)]
struct DocgenPackage {
    docgen: Option<DocgenPackageConfig>,
}

#[derive(Debug, Deserialize)]
struct DocgenPackageConfig {
    entry: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReactDocgenComponent {
    description: String,
    methods: Vec<Value>,
    display_name: String,
    props: BTreeMap<String, ReactDocgenProp>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    composes: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReactDocgenProp {
    required: bool,
    ts_type: Value,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_value: Option<ReactDocgenDefault>,
}

#[derive(Debug, Serialize)]
struct ReactDocgenDefault {
    value: String,
    computed: bool,
}

#[derive(Debug, Deserialize)]
struct TokenConfig {
    build: TokenBuild,
    token: toml::Table,
}

#[derive(Debug, Deserialize)]
struct TokenBuild {
    output: String,
    prefix: String,
    #[serde(default)]
    imports: Vec<String>,
}

#[derive(Clone)]
struct TokenSource {
    prefix: String,
    tokens: Vec<(String, String)>,
}

#[derive(Default)]
struct GlobalTokens {
    merged: HashMap<String, (String, String)>,
}

struct TokenLoader {
    fs: Arc<dyn FileSystem>,
    pnp: Option<PnpManifest>,
    memo: HashMap<PathBuf, TokenSource>,
    stack: Vec<PathBuf>,
}

impl TokenLoader {
    fn new(root: &Path) -> Self {
        let os: Arc<dyn FileSystem> = Arc::new(OsFileSystem);
        let pnp = PnpManifest::discover(os.as_ref(), root);
        let fs: Arc<dyn FileSystem> = if pnp.is_some() {
            Arc::new(PnpFileSystem::new(os))
        } else {
            os
        };
        Self {
            fs,
            pnp,
            memo: HashMap::new(),
            stack: Vec::new(),
        }
    }

    fn read_config(&self, path: &Path) -> Result<TokenConfig, WakeError> {
        let source = self
            .fs
            .read_to_string(path)
            .map_err(|error| WakeError::new("WAKE_TOKEN_IO", error.to_string()).at(path))?;
        toml::from_str(&source)
            .map_err(|error| WakeError::new("WAKE_TOKEN_CONFIG", error.to_string()).at(path))
    }

    fn load_globals(
        &mut self,
        imports: &[String],
        issuer: &Path,
    ) -> Result<GlobalTokens, WakeError> {
        let mut globals = GlobalTokens::default();
        for package in imports {
            if !is_bare_package_name(package) {
                return Err(WakeError::new(
                    "WAKE_TOKEN_IMPORT",
                    format!("token import must be a bare package name: {package}"),
                ));
            }
            let source = self.load_package(package, issuer)?;
            for (key, value) in source.tokens {
                globals.merged.insert(key, (source.prefix.clone(), value));
            }
        }
        Ok(globals)
    }

    fn load_package(&mut self, package: &str, issuer: &Path) -> Result<TokenSource, WakeError> {
        let package_root = self.resolve_package_root(package, issuer)?;
        let config_path = package_root.join("token.toml");
        if let Some(source) = self.memo.get(&config_path) {
            return Ok(source.clone());
        }
        if let Some(index) = self.stack.iter().position(|path| path == &config_path) {
            let mut cycle = self.stack[index..]
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            cycle.push(config_path.display().to_string());
            return Err(WakeError::new(
                "WAKE_TOKEN_CYCLE",
                format!("token import cycle: {}", cycle.join(" -> ")),
            )
            .at(&config_path));
        }

        self.stack.push(config_path.clone());
        let result = self.load_source(&config_path, &package_root);
        self.stack.pop();
        let source = result?;
        self.memo.insert(config_path, source.clone());
        Ok(source)
    }

    fn load_source(&mut self, config_path: &Path, root: &Path) -> Result<TokenSource, WakeError> {
        let config = self.read_config(config_path)?;
        validate_build(&config.build, config_path)?;
        let globals = self.load_globals(&config.build.imports, root)?;
        let mut flat = Vec::new();
        flatten_tokens(&config.token, &mut Vec::new(), &mut flat, config_path)?;
        for (_, value) in &mut flat {
            *value = resolve_refs(value, &globals, config_path)?;
        }
        Ok(TokenSource {
            prefix: config.build.prefix,
            tokens: flat,
        })
    }

    fn resolve_package_root(&self, package: &str, issuer: &Path) -> Result<PathBuf, WakeError> {
        if let Some(manifest) = &self.pnp {
            return manifest.resolve_bare(package, issuer).map_err(|error| {
                WakeError::new(
                    "WAKE_TOKEN_IMPORT",
                    format!("cannot resolve token package `{package}`: {error:?}"),
                )
                .at(issuer)
            });
        }

        let package_path = package.split('/').fold(PathBuf::new(), |mut path, part| {
            path.push(part);
            path
        });
        for ancestor in issuer.ancestors() {
            let candidate = ancestor.join("node_modules").join(&package_path);
            if self.fs.is_file(&candidate.join("package.json")) {
                return Ok(candidate);
            }
        }
        Err(WakeError::new(
            "WAKE_TOKEN_IMPORT",
            format!(
                "cannot resolve token package `{package}` from {}",
                issuer.display()
            ),
        )
        .at(issuer))
    }
}

pub fn generate_css_token(
    options: GenerateCssTokenOptions,
    cancellation: &CancellationToken,
) -> Result<GenerateCssTokenResult, WakeError> {
    cancellation.check()?;
    let started = Instant::now();
    let cwd = options
        .project
        .cwd
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if !cwd.is_dir() {
        return Err(
            WakeError::new("WAKE_TOKEN_IO", "cwd does not exist or is not a directory").at(&cwd),
        );
    }
    let root = canonical_project_root(&cwd)?;
    let config_path = absolute_from(
        &root,
        options
            .config_path
            .as_deref()
            .unwrap_or_else(|| Path::new("token.toml")),
    );

    let mut loader = TokenLoader::new(&root);
    loader.stack.push(config_path.clone());
    let config = loader.read_config(&config_path)?;
    validate_build(&config.build, &config_path)?;
    let globals = loader.load_globals(&config.build.imports, &root)?;
    loader.stack.pop();
    cancellation.check()?;

    let generated = generate_source(&config, &globals, &config_path)?;
    let output = absolute_from(&root, Path::new(&config.build.output));
    cancellation.check()?;
    atomic_write(&output, generated.as_bytes())?;
    Ok(GenerateCssTokenResult {
        success: true,
        duration_ms: started.elapsed().as_secs_f64() * 1000.0,
        output_file: output.to_string_lossy().into_owned(),
        files: vec![OutputFile {
            path: output.to_string_lossy().into_owned(),
            kind: "asset".to_string(),
            bytes: generated.len(),
        }],
    })
}

fn validate_build(build: &TokenBuild, path: &Path) -> Result<(), WakeError> {
    if build.output.trim().is_empty() {
        return Err(WakeError::new("WAKE_TOKEN_CONFIG", "build.output must not be empty").at(path));
    }
    if !valid_css_name(&build.prefix) {
        return Err(WakeError::new(
            "WAKE_TOKEN_CONFIG",
            format!(
                "build.prefix is not a safe CSS identifier: {}",
                build.prefix
            ),
        )
        .at(path));
    }
    Ok(())
}

fn valid_css_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn flatten_tokens(
    table: &toml::Table,
    path: &mut Vec<String>,
    output: &mut Vec<(String, String)>,
    config_path: &Path,
) -> Result<(), WakeError> {
    for (key, value) in table {
        if key.is_empty() {
            return Err(
                WakeError::new("WAKE_TOKEN_CONFIG", "token keys must not be empty").at(config_path),
            );
        }
        path.push(key.clone());
        match value {
            toml::Value::Table(child) => flatten_tokens(child, path, output, config_path)?,
            toml::Value::String(value) => output.push((path.join("."), value.trim().to_string())),
            toml::Value::Integer(value) => output.push((path.join("."), value.to_string())),
            toml::Value::Float(value) => output.push((path.join("."), value.to_string())),
            toml::Value::Boolean(value) => output.push((path.join("."), value.to_string())),
            toml::Value::Datetime(value) => output.push((path.join("."), value.to_string())),
            toml::Value::Array(_) => {
                return Err(WakeError::new(
                    "WAKE_TOKEN_CONFIG",
                    format!("token `{}` must be a scalar value", path.join(".")),
                )
                .at(config_path));
            }
        }
        path.pop();
    }
    Ok(())
}

fn resolve_refs(
    value: &str,
    globals: &GlobalTokens,
    config_path: &Path,
) -> Result<String, WakeError> {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("$ref(") {
        output.push_str(&rest[..start]);
        let reference = &rest[start + 5..];
        let Some(end) = reference.find(')') else {
            return Err(WakeError::new(
                "WAKE_TOKEN_REF",
                format!("unterminated token reference in `{value}`"),
            )
            .at(config_path));
        };
        let key = reference[..end].trim();
        let Some((prefix, fallback)) = globals.merged.get(key) else {
            return Err(WakeError::new(
                "WAKE_TOKEN_REF",
                format!("token reference `$ref({key})` was not found"),
            )
            .at(config_path));
        };
        output.push_str("var(--");
        output.push_str(prefix);
        output.push('-');
        output.push_str(&key.replace('.', "-"));
        output.push_str(", ");
        output.push_str(fallback);
        output.push(')');
        rest = &reference[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

fn generate_source(
    config: &TokenConfig,
    globals: &GlobalTokens,
    config_path: &Path,
) -> Result<String, WakeError> {
    let mut vars = Vec::new();
    let body = format_token_table(
        &config.token,
        &mut vec![config.build.prefix.clone()],
        1,
        &mut vars,
        globals,
        config_path,
    )?;
    let vars_body = vars
        .iter()
        .map(|(key, value)| {
            format!(
                "    '{}': '{}'",
                escape_single_quoted(key),
                escape_single_quoted(value)
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    Ok(format!(
        "/**\n * THIS FILE IS AUTO-GENERATED. DO NOT MODIFY MANUALLY.\n */\n\nimport {{ defineTokens }} from '@crab-dev/css';\n\nexport const vars = defineTokens({{\n{vars_body}\n}});\n\nconst token = defineTokens({{\n{body}\n}});\n\nexport default token;\n"
    ))
}

fn format_token_table(
    table: &toml::Table,
    path: &mut Vec<String>,
    level: usize,
    vars: &mut Vec<(String, String)>,
    globals: &GlobalTokens,
    config_path: &Path,
) -> Result<String, WakeError> {
    let indent = "    ".repeat(level);
    let mut entries = Vec::new();
    for (key, value) in table {
        path.push(key.clone());
        if let toml::Value::Table(child) = value {
            let child = format_token_table(child, path, level + 1, vars, globals, config_path)?;
            entries.push(format!(
                "{indent}'{}': {{\n{child}\n{indent}}}",
                escape_single_quoted(key)
            ));
        } else {
            let raw = scalar_string(value, path, config_path)?;
            let fallback = resolve_refs(raw.trim(), globals, config_path)?;
            let dot_key = path[1..].join(".");
            let css_name = format!("--{}", path.join("-"));
            vars.push((dot_key.clone(), css_name));
            entries.push(format!(
                "{indent}'{}': `var(${{vars['{}']}}, {})`",
                escape_single_quoted(key),
                escape_single_quoted(&dot_key),
                escape_template_text(&fallback)
            ));
        }
        path.pop();
    }
    Ok(entries.join(",\n"))
}

fn scalar_string<'a>(
    value: &'a toml::Value,
    path: &[String],
    config_path: &Path,
) -> Result<std::borrow::Cow<'a, str>, WakeError> {
    match value {
        toml::Value::String(value) => Ok(std::borrow::Cow::Borrowed(value)),
        toml::Value::Integer(value) => Ok(std::borrow::Cow::Owned(value.to_string())),
        toml::Value::Float(value) => Ok(std::borrow::Cow::Owned(value.to_string())),
        toml::Value::Boolean(value) => Ok(std::borrow::Cow::Owned(value.to_string())),
        toml::Value::Datetime(value) => Ok(std::borrow::Cow::Owned(value.to_string())),
        toml::Value::Array(_) | toml::Value::Table(_) => Err(WakeError::new(
            "WAKE_TOKEN_CONFIG",
            format!("token `{}` must be a scalar value", path.join(".")),
        )
        .at(config_path)),
    }
}

fn escape_single_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

fn escape_template_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace("${", "\\${")
}

pub fn generate_docgen(
    options: GenerateDocgenOptions,
    cancellation: &CancellationToken,
) -> Result<GenerateDocgenResult, WakeError> {
    cancellation.check()?;
    let started = Instant::now();
    let cwd = options
        .project
        .cwd
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if !cwd.is_dir() {
        return Err(
            WakeError::new("WAKE_DOCGEN_IO", "cwd does not exist or is not a directory").at(&cwd),
        );
    }
    let root = canonical_project_root(&cwd)?;
    let entry = resolve_docgen_entry(&root, options.entry.as_deref())?;
    cancellation.check()?;
    let component = wake_tsdoc::extract_component_api(&entry)
        .map_err(|error| WakeError::new("WAKE_DOCGEN_TYPE", error.to_string()).at(&entry))?;

    let mut props = BTreeMap::new();
    for prop in component.api.props {
        let mut description = prop.description;
        if let Some(deprecated) = prop.deprecated {
            if !description.is_empty() {
                description.push('\n');
            }
            description.push_str("@deprecated");
            if !deprecated.is_empty() {
                description.push(' ');
                description.push_str(&deprecated);
            }
        }
        props.insert(
            prop.name,
            ReactDocgenProp {
                required: prop.required,
                ts_type: react_docgen_type(&prop.type_text),
                description,
                default_value: prop.default_value.map(|value| ReactDocgenDefault {
                    value,
                    computed: false,
                }),
            },
        );
    }
    let mut composes = component
        .api
        .inherited
        .into_iter()
        .map(|group| group.name)
        .collect::<Vec<_>>();
    composes.sort();
    composes.dedup();
    let documentation = ReactDocgenComponent {
        description: component.description,
        methods: Vec::new(),
        display_name: component.display_name,
        props,
        composes,
    };
    let relative = entry.strip_prefix(&root).map_err(|_| {
        WakeError::new(
            "WAKE_DOCGEN_ENTRY",
            "docgen entry must stay inside the package root",
        )
        .at(&entry)
    })?;
    let entry_key = format!("./{}", path_to_slash(relative));
    let mut document = BTreeMap::new();
    document.insert(entry_key.clone(), vec![documentation]);
    let generated = serde_json::to_vec(&document)
        .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))?;
    cancellation.check()?;
    let output = root.join("public/docgen.json");
    atomic_write(&output, &generated)?;
    Ok(GenerateDocgenResult {
        success: true,
        duration_ms: started.elapsed().as_secs_f64() * 1000.0,
        entry: entry_key,
        output_file: output.to_string_lossy().into_owned(),
        files: vec![OutputFile {
            path: output.to_string_lossy().into_owned(),
            kind: "asset".to_string(),
            bytes: generated.len(),
        }],
    })
}

pub fn build_library(
    options: LibraryBuildOptions,
    cancellation: &CancellationToken,
) -> Result<LibraryBuildResult, WakeError> {
    cancellation.check()?;
    let started = Instant::now();
    let cwd = options
        .project
        .cwd
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let root = canonical_project_root(&cwd)?;
    let entry = options
        .entry
        .unwrap_or_else(|| PathBuf::from("src/index.ts"));
    let entry_path = absolute_from(&root, &entry);

    let graph = LibraryGraph::scan(LibraryGraphOptions::new(&root, &entry_path))
        .map_err(|error| WakeError::new("WAKE_LIBRARY_BUILD", error).at(&entry_path))?;
    cancellation.check()?;
    let esm = graph
        .emit(PreserveModuleFormat::EsModule)
        .map_err(|error| WakeError::new("WAKE_LIBRARY_BUILD", error).at(&entry_path))?;
    let cjs = graph
        .emit(PreserveModuleFormat::CommonJs)
        .map_err(|error| WakeError::new("WAKE_LIBRARY_BUILD", error).at(&entry_path))?;
    let declarations = wake_tsdoc::emit_library_declarations(&root, &entry_path)
        .map_err(|error| WakeError::new("WAKE_LIBRARY_TYPE", error.to_string()).at(&entry_path))?;
    cancellation.check()?;

    let staging = tempfile::Builder::new()
        .prefix(".wake-library-stage-")
        .tempdir_in(&root)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&root))?;
    let mut files = Vec::new();
    for module in &esm.modules {
        write_library_file(
            staging.path(),
            Path::new("esm").join(&module.file_name),
            module.code.as_bytes(),
            if module.file_name == esm.entry {
                "entry"
            } else {
                "chunk"
            },
            &root,
            &mut files,
        )?;
    }
    for module in &cjs.modules {
        write_library_file(
            staging.path(),
            Path::new("cjs").join(&module.file_name),
            module.code.as_bytes(),
            if module.file_name == cjs.entry {
                "entry"
            } else {
                "chunk"
            },
            &root,
            &mut files,
        )?;
    }
    for declaration in &declarations {
        write_library_file(
            staging.path(),
            Path::new("declarations").join(&declaration.file_name),
            declaration.code.as_bytes(),
            "declaration",
            &root,
            &mut files,
        )?;
    }
    let css_entry = esm
        .css
        .as_ref()
        .filter(|css| !css.trim().is_empty())
        .map(|css| {
            let relative = PathBuf::from("css/index.css");
            write_library_file(
                staging.path(),
                relative,
                css.as_bytes(),
                "css",
                &root,
                &mut files,
            )?;
            Ok::<String, WakeError>(
                root.join("css")
                    .join("index.css")
                    .to_string_lossy()
                    .into_owned(),
            )
        })
        .transpose()?;
    cancellation.check()?;
    commit_library_outputs(&root, staging.path())?;

    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(LibraryBuildResult {
        success: true,
        module_count: esm.runtime_module_count,
        updated_module_count: esm.runtime_module_count,
        cached_module_count: 0,
        duration_ms: started.elapsed().as_secs_f64() * 1000.0,
        output_dir: root.to_string_lossy().into_owned(),
        esm_entry: root
            .join("esm")
            .join("index.mjs")
            .to_string_lossy()
            .into_owned(),
        cjs_entry: root
            .join("cjs")
            .join("index.cjs")
            .to_string_lossy()
            .into_owned(),
        declaration_entry: root
            .join("declarations")
            .join("index.d.ts")
            .to_string_lossy()
            .into_owned(),
        css_entry,
        files,
        diagnostics: Vec::new(),
    })
}

fn write_library_file(
    staging: &Path,
    relative: PathBuf,
    bytes: &[u8],
    kind: &str,
    root: &Path,
    files: &mut Vec<OutputFile>,
) -> Result<(), WakeError> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(WakeError::new(
            "WAKE_LIBRARY_OUTPUT",
            format!(
                "library output path is not project-relative: {}",
                relative.display()
            ),
        ));
    }
    let path = staging.join(&relative);
    let parent = path.parent().unwrap_or(staging);
    std::fs::create_dir_all(parent)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(parent))?;
    std::fs::write(&path, bytes)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
    files.push(OutputFile {
        path: root.join(relative).to_string_lossy().into_owned(),
        kind: kind.to_string(),
        bytes: bytes.len(),
    });
    Ok(())
}

const LIBRARY_OUTPUT_DIRS: [&str; 4] = ["esm", "cjs", "declarations", "css"];

fn commit_library_outputs(root: &Path, staging: &Path) -> Result<(), WakeError> {
    commit_staged_output(
        staging,
        root,
        Some(&LIBRARY_OUTPUT_DIRS),
        "library",
        ".wake-library-backup-",
    )
}

fn react_docgen_type(type_text: &str) -> Value {
    let value = type_text.trim();
    if value.contains('|') {
        return json!({ "name": "union", "raw": value });
    }
    if value.contains("=>") || value.starts_with("function") {
        return json!({ "name": "signature", "type": "function", "raw": value });
    }
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '$'))
    {
        json!({ "name": value })
    } else {
        json!({ "name": value, "raw": value })
    }
}

fn resolve_docgen_entry(root: &Path, explicit: Option<&Path>) -> Result<PathBuf, WakeError> {
    if let Some(entry) = explicit {
        return validate_docgen_entry(root, absolute_from(root, entry));
    }
    let package_path = root.join("package.json");
    let package_source = std::fs::read_to_string(&package_path)
        .map_err(|error| WakeError::new("WAKE_DOCGEN_IO", error.to_string()).at(&package_path))?;
    let package: DocgenPackage = serde_json::from_str(&package_source).map_err(|error| {
        WakeError::new("WAKE_DOCGEN_CONFIG", error.to_string()).at(&package_path)
    })?;
    if let Some(entry) = package.docgen.and_then(|config| config.entry) {
        return validate_docgen_entry(root, absolute_from(root, Path::new(&entry)));
    }
    resolve_docgen_entry_from_index(root)
}

fn validate_docgen_entry(root: &Path, entry: PathBuf) -> Result<PathBuf, WakeError> {
    if !entry.starts_with(root) {
        return Err(WakeError::new(
            "WAKE_DOCGEN_ENTRY",
            "docgen entry must stay inside the package root",
        )
        .at(&entry));
    }
    if !entry.is_file() {
        return Err(WakeError::new("WAKE_DOCGEN_ENTRY", "docgen entry does not exist").at(&entry));
    }
    Ok(entry)
}

fn resolve_docgen_entry_from_index(root: &Path) -> Result<PathBuf, WakeError> {
    let index = root.join("src/index.ts");
    let source = std::fs::read_to_string(&index)
        .map_err(|error| WakeError::new("WAKE_DOCGEN_IO", error.to_string()).at(&index))?;

    let direct = Regex::new(
        r#"(?m)^\s*export\s*\{\s*(?:default|[A-Za-z_$][\w$]*\s+as\s+default)\s*\}\s*from\s*["'](\.?\.?/[^"']+)["']"#,
    )
    .expect("valid docgen re-export regex");
    if let Some(captures) = direct.captures(&source) {
        return resolve_source_entry(root, &index, captures.get(1).unwrap().as_str());
    }

    let default = Regex::new(r"(?m)^\s*export\s+default\s+([A-Za-z_$][\w$]*)\s*;?")
        .expect("valid docgen default regex");
    let captures = default.captures(&source).ok_or_else(|| {
        WakeError::new(
            "WAKE_DOCGEN_ENTRY",
            "cannot find a default component export in src/index.ts; set docgen.entry",
        )
        .at(&index)
    })?;
    let mut name = captures.get(1).unwrap().as_str().to_string();
    let alias = Regex::new(
        r"(?m)^\s*(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=;]+)?=\s*([A-Za-z_$][\w$]*)\s*;?",
    )
    .expect("valid docgen alias regex");
    for _ in 0..16 {
        let next = alias.captures_iter(&source).find_map(|captures| {
            (captures.get(1).unwrap().as_str() == name)
                .then(|| captures.get(2).unwrap().as_str().to_string())
        });
        let Some(next) = next else { break };
        name = next;
    }
    let import = Regex::new(&format!(
        r#"(?m)^\s*import\s+{}\s*(?:,\s*\{{[^}}]*\}})?\s*from\s*["'](\.?\.?/[^"']+)["']"#,
        regex::escape(&name)
    ))
    .expect("valid docgen import regex");
    if let Some(captures) = import.captures(&source) {
        return resolve_source_entry(root, &index, captures.get(1).unwrap().as_str());
    }

    let local = Regex::new(&format!(
        r"(?m)^\s*(?:export\s+)?(?:const|let|var|function|class)\s+{}\b",
        regex::escape(&name)
    ))
    .expect("valid local component regex");
    if local.is_match(&source) {
        return Ok(index);
    }
    Err(WakeError::new(
        "WAKE_DOCGEN_ENTRY",
        format!("cannot trace default component `{name}` from src/index.ts; set docgen.entry"),
    )
    .at(&index))
}

fn resolve_source_entry(root: &Path, index: &Path, specifier: &str) -> Result<PathBuf, WakeError> {
    let mut base = absolute_from(index.parent().unwrap_or(root), Path::new(specifier));
    if matches!(
        base.extension().and_then(|extension| extension.to_str()),
        Some("js" | "mjs" | "cjs")
    ) {
        base.set_extension("");
    }
    if base.is_file() {
        return validate_docgen_entry(root, base);
    }
    for extension in ["tsx", "ts", "jsx", "js"] {
        let candidate = base.with_extension(extension);
        if candidate.is_file() {
            return validate_docgen_entry(root, candidate);
        }
    }
    Err(WakeError::new(
        "WAKE_DOCGEN_ENTRY",
        format!("cannot resolve component source `{specifier}`"),
    )
    .at(index))
}

fn path_to_slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    struct Fixture {
        root: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                root: tempfile::tempdir().unwrap(),
            }
        }

        fn path(&self, path: &str) -> PathBuf {
            self.root.path().join(path)
        }

        fn write(&self, path: &str, source: &str) {
            let path = self.path(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }

        fn generate(&self) -> Result<GenerateCssTokenResult, WakeError> {
            generate_css_token(
                GenerateCssTokenOptions {
                    project: ProjectOptions {
                        cwd: Some(self.root.path().to_path_buf()),
                        config_path: None,
                    },
                    config_path: None,
                },
                &CancellationToken::default(),
            )
        }

        fn docgen(&self, entry: Option<&str>) -> Result<GenerateDocgenResult, WakeError> {
            generate_docgen(
                GenerateDocgenOptions {
                    project: ProjectOptions {
                        cwd: Some(self.root.path().to_path_buf()),
                        config_path: None,
                    },
                    entry: entry.map(PathBuf::from),
                },
                &CancellationToken::default(),
            )
        }

        fn build_library(&self) -> Result<LibraryBuildResult, WakeError> {
            super::build_library(
                LibraryBuildOptions {
                    project: ProjectOptions {
                        cwd: Some(self.root.path().to_path_buf()),
                        config_path: None,
                    },
                    entry: None,
                },
                &CancellationToken::default(),
            )
        }
    }

    #[test]
    fn builds_library_contract_and_replaces_all_outputs_transactionally() {
        let fixture = Fixture::new();
        fixture.write("package.json", r#"{"name":"@demo/button","type":"module"}"#);
        fixture.write(
            "src/index.ts",
            "import Button from './button.js';\nexport type { ButtonProps } from './button.js';\nexport default Button;\n",
        );
        fixture.write(
            "src/button.tsx",
            "import type { FC } from 'react';\nexport interface ButtonProps { label: string; }\nconst Button: FC<ButtonProps> = (props) => <button>{props.label}</button>;\nexport default Button;\n",
        );
        fixture.write("css/stale.css", "stale");

        let result = fixture.build_library().unwrap();
        assert!(Path::new(&result.esm_entry).is_file());
        assert!(Path::new(&result.cjs_entry).is_file());
        assert!(Path::new(&result.declaration_entry).is_file());
        assert!(result.css_entry.is_none());
        assert!(!fixture.path("css").exists());
        assert!(
            fs::read_to_string(fixture.path("declarations/index.d.ts"))
                .unwrap()
                .contains("./_wake/src/button.js")
        );

        let previous = fs::read_to_string(fixture.path("esm/index.mjs")).unwrap();
        fixture.write("src/index.ts", "export const answer = compute();\n");
        let error = fixture.build_library().unwrap_err();
        assert_eq!(error.code, "WAKE_LIBRARY_TYPE");
        assert_eq!(
            fs::read_to_string(fixture.path("esm/index.mjs")).unwrap(),
            previous
        );
    }

    #[test]
    fn repeated_library_build_keeps_output_directories_stable() {
        let fixture = Fixture::new();
        fixture.write("package.json", r#"{"name":"@demo/button","type":"module"}"#);
        fixture.write(
            "src/index.ts",
            "export type { ButtonProps } from './button.js'; export { default } from './button.js';",
        );
        fixture.write(
            "src/button.tsx",
            "export interface ButtonProps { label: string; } export default function Button() { return null; }",
        );

        fixture.build_library().unwrap();

        #[cfg(windows)]
        let _declarations_guard = {
            use std::os::windows::fs::OpenOptionsExt;

            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0x0000_0001 | 0x0000_0002)
                .custom_flags(0x0200_0000)
                .open(fixture.path("declarations"))
                .unwrap()
        };

        fixture.build_library().unwrap();
        fixture.write(
            "src/button.tsx",
            "export interface ButtonProps { label: string; disabled: boolean; } export default function Button() { return null; }",
        );
        fixture.build_library().unwrap();

        let declaration =
            fs::read_to_string(fixture.path("declarations/_wake/src/button.d.ts")).unwrap();
        assert!(declaration.contains("disabled: boolean"), "{declaration}");
    }

    #[cfg(windows)]
    #[test]
    fn library_commit_rolls_back_files_replaced_before_a_windows_lock_failure() {
        use std::os::windows::fs::OpenOptionsExt;

        let fixture = Fixture::new();
        fixture.write("cjs/a.cjs", "old-a");
        fixture.write("cjs/b.cjs", "old-b");
        let staging = tempfile::Builder::new()
            .prefix(".wake-library-test-stage-")
            .tempdir_in(fixture.root.path())
            .unwrap();
        fs::create_dir_all(staging.path().join("cjs")).unwrap();
        fs::write(staging.path().join("cjs/a.cjs"), "new-a").unwrap();
        fs::write(staging.path().join("cjs/b.cjs"), "new-b").unwrap();
        let _locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0001)
            .open(fixture.path("cjs/b.cjs"))
            .unwrap();

        let error = commit_library_outputs(fixture.root.path(), staging.path()).unwrap_err();
        assert_eq!(error.code, "WAKE_IO");
        assert!(
            error.message.contains("install failed"),
            "{}",
            error.message
        );
        assert!(error.message.contains("cjs\\b.cjs"), "{}", error.message);
        assert_eq!(
            fs::read_to_string(fixture.path("cjs/a.cjs")).unwrap(),
            "old-a"
        );
        assert_eq!(
            fs::read_to_string(fixture.path("cjs/b.cjs")).unwrap(),
            "old-b"
        );
    }

    #[test]
    fn generates_nested_tokens_and_inline_refs_in_source_order() {
        let fixture = Fixture::new();
        fixture.write(
            "node_modules/@scope/base/package.json",
            r#"{"name":"@scope/base"}"#,
        );
        fixture.write(
            "node_modules/@scope/base/token.toml",
            r#"[build]
output = "./src/token.ts"
prefix = "base"

[token]
color.primary = "red"
space.small = "4px"
"#,
        );
        fixture.write(
            "token.toml",
            r#"[build]
output = "./generated/token.ts"
prefix = "button"
imports = ["@scope/base"]

[token]
color.text = "$ref(color.primary)"
border = "1px solid $ref(color.primary)"
"#,
        );

        let result = fixture.generate().unwrap();
        assert!(result.success);
        let generated = fs::read_to_string(fixture.path("generated/token.ts")).unwrap();
        assert!(generated.contains("'color.text': '--button-color-text'"));
        assert!(
            generated
                .contains("'text': `var(${vars['color.text']}, var(--base-color-primary, red))`")
        );
        assert!(generated.contains(
            "'border': `var(${vars['border']}, 1px solid var(--base-color-primary, red))`"
        ));
        let token_body = generated.split("const token =").nth(1).unwrap();
        assert!(token_body.find("'color': {").unwrap() < token_body.find("'border':").unwrap());
    }

    #[test]
    fn later_import_overrides_an_earlier_reference() {
        let fixture = Fixture::new();
        for (name, prefix, value) in [("a", "first", "red"), ("b", "second", "blue")] {
            fixture.write(
                &format!("node_modules/{name}/package.json"),
                &format!(r#"{{"name":"{name}"}}"#),
            );
            fixture.write(
                &format!("node_modules/{name}/token.toml"),
                &format!(
                    "[build]\noutput='./token.ts'\nprefix='{prefix}'\n[token]\ncolor='{value}'\n"
                ),
            );
        }
        fixture.write(
            "token.toml",
            "[build]\noutput='./token.ts'\nprefix='local'\nimports=['a','b']\n[token]\ncolor='$ref(color)'\n",
        );
        fixture.generate().unwrap();
        let generated = fs::read_to_string(fixture.path("token.ts")).unwrap();
        assert!(generated.contains("var(--second-color, blue)"));
    }

    #[test]
    fn rejects_missing_references_without_replacing_old_output() {
        let fixture = Fixture::new();
        fixture.write("old.ts", "last known good");
        fixture.write(
            "token.toml",
            "[build]\noutput='./old.ts'\nprefix='local'\n[token]\ncolor='$ref(missing)'\n",
        );
        let error = fixture.generate().unwrap_err();
        assert_eq!(error.code, "WAKE_TOKEN_REF");
        assert_eq!(
            fs::read_to_string(fixture.path("old.ts")).unwrap(),
            "last known good"
        );
    }

    #[test]
    fn rejects_recursive_package_imports() {
        let fixture = Fixture::new();
        for (name, dependency) in [("a", "b"), ("b", "a")] {
            fixture.write(
                &format!("node_modules/{name}/package.json"),
                &format!(r#"{{"name":"{name}"}}"#),
            );
            fixture.write(
                &format!("node_modules/{name}/token.toml"),
                &format!(
                    "[build]\noutput='./token.ts'\nprefix='{name}'\nimports=['{dependency}']\n[token]\nvalue='1'\n"
                ),
            );
        }
        fixture.write(
            "token.toml",
            "[build]\noutput='./token.ts'\nprefix='local'\nimports=['a']\n[token]\nvalue='1'\n",
        );
        let error = fixture.generate().unwrap_err();
        assert_eq!(error.code, "WAKE_TOKEN_CYCLE");
    }

    #[test]
    fn escapes_generated_typescript_literals() {
        let fixture = Fixture::new();
        fixture.write(
            "token.toml",
            "[build]\noutput='./nested/token.ts'\nprefix='local'\n[token]\n\"quote'key\"='tick` ${danger} \\\\ path'\n",
        );
        fixture.generate().unwrap();
        let generated = fs::read_to_string(fixture.path("nested/token.ts")).unwrap();
        assert!(generated.contains("'quote\\'key': '--local-quote\\'key'"));
        assert!(
            generated.contains("tick\\` \\${danger} \\\\\\\\ path"),
            "{generated}"
        );
    }

    #[test]
    fn resolves_imported_tokens_through_yarn_pnp_data() {
        let fixture = Fixture::new();
        fixture.write(
            ".pnp.cjs",
            "// discovery marker; data lives beside this file",
        );
        fixture.write(
            ".pnp.data.json",
            r#"{
  "enableTopLevelFallback": false,
  "fallbackExclusionList": [],
  "fallbackPool": [],
  "packageRegistryData": [
    [null, [[null, {
      "packageLocation": "./",
      "packageDependencies": [["base", "workspace:base"]],
      "linkType": "SOFT"
    }]]],
    ["base", [["workspace:base", {
      "packageLocation": "./packages/base/",
      "packageDependencies": [["base", "workspace:base"]],
      "linkType": "SOFT"
    }]]]
  ]
}"#,
        );
        fixture.write("packages/base/package.json", r#"{"name":"base"}"#);
        fixture.write(
            "packages/base/token.toml",
            "[build]\noutput='./token.ts'\nprefix='base'\n[token]\ncolor='red'\n",
        );
        fixture.write(
            "token.toml",
            "[build]\noutput='./token.ts'\nprefix='local'\nimports=['base']\n[token]\ncolor='$ref(color)'\n",
        );

        fixture.generate().unwrap();
        let generated = fs::read_to_string(fixture.path("token.ts")).unwrap();
        assert!(generated.contains("var(--base-color, red)"), "{generated}");
    }

    #[test]
    fn generates_compact_react_docgen_shape_from_package_entry() {
        let fixture = Fixture::new();
        fixture.write(
            "package.json",
            r#"{"name":"example","docgen":{"entry":"./src/button.tsx"}}"#,
        );
        fixture.write(
            "src/button.tsx",
            r#"
                import type { FC } from "react";
                export interface ButtonProps {
                    /** Visible text. */
                    label: string;
                    /** Disabled state. */
                    disabled?: boolean;
                }
                /** Primary button. */
                const Button: FC<ButtonProps> = ({ label, disabled = false }) => null;
                export default Button;
            "#,
        );
        let result = fixture.docgen(None).unwrap();
        assert_eq!(result.entry, "./src/button.tsx");
        let bytes = fs::read(fixture.path("public/docgen.json")).unwrap();
        assert!(!bytes.contains(&b'\n'), "docgen JSON must stay compact");
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let component = &value["./src/button.tsx"][0];
        assert_eq!(component["displayName"], "Button");
        assert_eq!(component["description"], "Primary button.");
        assert_eq!(component["props"]["label"]["required"], true);
        assert_eq!(component["props"]["label"]["tsType"]["name"], "string");
        assert_eq!(
            component["props"]["disabled"]["defaultValue"]["value"],
            "false"
        );
    }

    #[test]
    fn traces_index_aliases_and_reexports_for_docgen() {
        let fixture = Fixture::new();
        fixture.write("package.json", r#"{"name":"example"}"#);
        fixture.write(
            "src/index.ts",
            "import DividerImpl from './divider.js';\nconst Divider = DividerImpl;\nexport default Divider;\n",
        );
        fixture.write(
            "src/divider.tsx",
            "interface DividerProps { vertical?: boolean; }\nconst DividerImpl = ({ vertical = true }: DividerProps) => null;\nexport default DividerImpl;\n",
        );
        let result = fixture.docgen(None).unwrap();
        assert_eq!(result.entry, "./src/divider.tsx");

        fixture.write("src/index.ts", "export { default } from './divider.js';");
        let result = fixture.docgen(None).unwrap();
        assert_eq!(result.entry, "./src/divider.tsx");
    }

    #[test]
    fn docgen_failure_preserves_the_last_valid_output() {
        let fixture = Fixture::new();
        fixture.write("package.json", r#"{"name":"example"}"#);
        fixture.write("src/index.ts", "export default Missing;");
        fixture.write("public/docgen.json", "last known good");
        let error = fixture.docgen(None).unwrap_err();
        assert_eq!(error.code, "WAKE_DOCGEN_ENTRY");
        assert_eq!(
            fs::read_to_string(fixture.path("public/docgen.json")).unwrap(),
            "last known good"
        );
    }
}
