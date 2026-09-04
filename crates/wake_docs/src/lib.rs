//! Wake-native React component documentation compiler.
//!
//! MDX is parsed to markdown-rs mdast, rendered to TSX, and then handed to Wake's existing
//! compiler. Generated modules live under .wake/docs/generated for dev and production parity.

use markdown::mdast::{AttributeContent, AttributeValue, Node};
use markdown::{Constructs, MdxSignal, ParseOptions};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use wake_common::{Interner, OwnedFileTree, OwnedFileTreeBuilder, ProjectedRelativePath, Span};
use wake_ecma_ast::{DependencyKind, SourceType};
use wake_ecma_lexer::{Keyword, Lexer, Token, TokenKind, tokenize};
use wake_ecma_parser::parse;

const RUNTIME_APP: &str = include_str!("../runtime/app.tsx");
const RUNTIME_COMPONENTS: &str = include_str!("../runtime/components.tsx");
const RUNTIME_COMPONENT_STATE: &str = include_str!("../runtime/components-state.mjs");
const RUNTIME_ROUTES: &str = include_str!("../runtime/routes.mjs");
const RUNTIME_SEARCH: &str = include_str!("../runtime/search.mjs");
const RUNTIME_SITE_ENTRY: &str = include_str!("../runtime/site-entry.tsx");
const RUNTIME_COMPONENTS_ENTRY: &str = include_str!("../runtime/components-entry.tsx");
const RUNTIME_STYLE: &str = include_str!("../runtime/styles.css");
const RUNTIME_COMPONENT_STYLE: &str = include_str!("../runtime/components.css");
const MINIMUM_REACT_MAJOR: u64 = 19;
static ATOMIC_WRITE_LOCK: Mutex<()> = Mutex::new(());
static GENERATION_TRANSACTION_LOCK: Mutex<()> = Mutex::new(());
static NEXT_ATOMIC_WRITE_ID: AtomicU64 = AtomicU64::new(0);
const GENERATION_COMMIT_LOCK_FILE: &str = ".wake-docs-generation.lock";
const GENERATION_COMMIT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const GENERATION_COMMIT_LOCK_RETRY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    Development,
    Production,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocsMode {
    #[default]
    Site,
    Components,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocsPresentation {
    Embedded,
    #[default]
    Standalone,
}

impl DocsPresentation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Standalone => "standalone",
        }
    }
}

impl DocsMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Site => "site",
            Self::Components => "components",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DocsOptions {
    pub source_dir: PathBuf,
    pub title: String,
    pub description: String,
    pub locale: String,
    pub logo: Option<String>,
    pub repository_url: Option<String>,
    pub base_path: String,
    pub preview: Option<PathBuf>,
    pub theme_css: Option<PathBuf>,
    pub default_theme: String,
    pub accent_color: Option<String>,
    pub presentation: DocsPresentation,
}

impl Default for DocsOptions {
    fn default() -> Self {
        Self {
            source_dir: PathBuf::from("docs"),
            title: "Wake Docs".to_string(),
            description: String::new(),
            locale: "zh-CN".to_string(),
            logo: None,
            repository_url: None,
            base_path: "/".to_string(),
            preview: None,
            theme_css: None,
            default_theme: "system".to_string(),
            accent_color: None,
            presentation: DocsPresentation::Standalone,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeneratedProject {
    pub root: PathBuf,
    pub generated_dir: PathBuf,
    pub entry: PathBuf,
    pub aliases: Vec<(String, PathBuf)>,
    pub watch_roots: Vec<PathBuf>,
    pub routes: Vec<RouteInfo>,
    pub mode: DocsMode,
    pub demos: Vec<DemoDescriptor>,
    pub warnings: Vec<String>,
    pub changed_files: Vec<PathBuf>,
}

/// An immutable documentation render that has not been published to the host filesystem.
///
/// Every generated module, including `manifest.json`, is owned by [`files`](Self::files). The
/// entry is relative to that file tree so callers can mount it at a logical or physical root of
/// their choosing.
#[derive(Debug, Clone)]
pub struct RenderedProject {
    pub root: PathBuf,
    pub files: OwnedFileTree,
    pub entry_relative: ProjectedRelativePath,
    pub watch_roots: Vec<PathBuf>,
    pub routes: Vec<RouteInfo>,
    pub mode: DocsMode,
    pub demos: Vec<DemoDescriptor>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteInfo {
    pub id: String,
    pub file: String,
    pub title: String,
    pub description: String,
    pub kind: String,
    pub group: String,
    pub group_id: String,
    pub section: String,
    pub section_id: String,
    pub slug: String,
    pub status: String,
    pub draft: bool,
    pub hidden: bool,
    pub headings: Vec<HeadingInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeadingInfo {
    pub depth: u8,
    pub title: String,
    pub id: String,
}

#[derive(Debug)]
pub enum DocsError {
    Io(PathBuf, String),
    Mdx(PathBuf, String),
    Frontmatter(PathBuf, String),
    Navigation(PathBuf, String),
    InvalidMacro {
        path: PathBuf,
        line: usize,
        column: usize,
        message: String,
    },
    DuplicateRoute {
        slug: String,
        first: PathBuf,
        second: PathBuf,
    },
    InvalidConfig(String),
    PublicCollision(PathBuf),
    Api(PathBuf, String),
    InvalidPagePath(PathBuf, String),
}

impl fmt::Display for DocsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "cannot access `{}`: {error}", path.display()),
            Self::Mdx(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Frontmatter(path, error) => write!(
                f,
                "invalid TOML frontmatter in `{}`: {error}",
                path.display()
            ),
            Self::Navigation(path, error) => write!(
                f,
                "invalid documentation navigation `{}`: {error}",
                path.display()
            ),
            Self::InvalidMacro {
                path,
                line,
                column,
                message,
            } => write!(f, "{}:{line}:{column}: {message}", path.display()),
            Self::DuplicateRoute {
                slug,
                first,
                second,
            } => write!(
                f,
                "duplicate docs route `{slug}` from `{}` and `{}`",
                first.display(),
                second.display()
            ),
            Self::InvalidConfig(message) => write!(f, "invalid docs configuration: {message}"),
            Self::PublicCollision(path) => write!(
                f,
                "public asset collides with build output `{}`",
                path.display()
            ),
            Self::Api(path, error) => {
                write!(f, "API docs failed for `{}`: {error}", path.display())
            }
            Self::InvalidPagePath(path, error) => {
                write!(
                    f,
                    "invalid documentation page path `{}`: {error}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for DocsError {}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Frontmatter {
    title: Option<String>,
    description: String,
    kind: Option<String>,
    status: Option<String>,
    draft: bool,
    hidden: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NavigationConfig {
    #[serde(rename = "group")]
    groups: Vec<NavigationGroup>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NavigationGroup {
    id: String,
    title: String,
    #[serde(default)]
    pages: Vec<String>,
    #[serde(default, rename = "section")]
    sections: Vec<NavigationSection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NavigationSection {
    id: String,
    title: String,
    pages: Vec<String>,
}

#[derive(Debug)]
struct CompiledPage {
    identity: PageIdentity,
    route: RouteInfo,
    search_text: String,
    module_plan: PageModulePlan,
    api_entries: Vec<ApiEntry>,
}

#[derive(Debug)]
struct PageModulePlan {
    source: String,
    rewritten_esm: RewrittenEsm,
    body: Vec<RenderedNode>,
}

#[derive(Debug, PartialEq, Eq)]
struct RenderedPageModule {
    code: String,
    source_map: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PageIdentity {
    id: String,
    source_file: String,
    generated_module: String,
    generated_map: String,
    route_path: RoutePath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RoutePath {
    decoded: String,
    encoded: String,
}

impl PageIdentity {
    fn from_paths(root: &Path, source_dir: &Path, path: &Path) -> Result<Self, DocsError> {
        let relative = path.strip_prefix(source_dir).map_err(|_| {
            DocsError::InvalidPagePath(
                path.to_path_buf(),
                "page is outside the configured source directory".to_string(),
            )
        })?;
        let source_relative = path.strip_prefix(root).map_err(|_| {
            DocsError::InvalidPagePath(
                path.to_path_buf(),
                "page is outside the project root".to_string(),
            )
        })?;
        let id = checked_slash_path(&relative.with_extension(""), path)?;
        let source_file = checked_slash_path(source_relative, path)?;
        let generated_module = checked_slash_path(
            &Path::new("pages").join(relative).with_extension("tsx"),
            path,
        )?;
        let generated_map = format!("{generated_module}.map");
        let route_path = RoutePath::from_page_relative(relative, path)?;
        Ok(Self {
            id,
            source_file,
            generated_module,
            generated_map,
            route_path,
        })
    }
}

impl RoutePath {
    fn from_page_relative(relative: &Path, source_path: &Path) -> Result<Self, DocsError> {
        let mut route = relative.with_extension("");
        if route.file_name().and_then(|value| value.to_str()) == Some("index") {
            route.pop();
        }
        let mut decoded_segments = Vec::new();
        let mut encoded_segments = Vec::new();
        for component in route.components() {
            let Component::Normal(segment) = component else {
                return Err(DocsError::InvalidPagePath(
                    source_path.to_path_buf(),
                    "route must contain only normal relative path segments".to_string(),
                ));
            };
            let segment = checked_identity_segment(segment, source_path)?;
            if matches!(segment, "." | "..") {
                return Err(DocsError::InvalidPagePath(
                    source_path.to_path_buf(),
                    "route contains an unsafe path segment".to_string(),
                ));
            }
            decoded_segments.push(segment.to_string());
            encoded_segments.push(percent_encode_route_segment(segment));
        }
        Ok(Self {
            decoded: if decoded_segments.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", decoded_segments.join("/"))
            },
            encoded: if encoded_segments.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", encoded_segments.join("/"))
            },
        })
    }

    fn from_canonical_encoded(value: &str) -> Result<Self, DocsError> {
        if value == "/" {
            return Ok(Self {
                decoded: "/".to_string(),
                encoded: "/".to_string(),
            });
        }
        if !value.starts_with('/') || value.ends_with('/') {
            return Err(DocsError::InvalidConfig(format!(
                "documentation route `{value}` is not canonical"
            )));
        }
        let mut decoded_segments = Vec::new();
        for encoded in value[1..].split('/') {
            if encoded.is_empty() {
                return Err(DocsError::InvalidConfig(format!(
                    "documentation route `{value}` contains an empty segment"
                )));
            }
            let decoded = percent_decode_route_segment(encoded).ok_or_else(|| {
                DocsError::InvalidConfig(format!(
                    "documentation route `{value}` has invalid percent encoding"
                ))
            })?;
            if matches!(decoded.as_str(), "." | "..")
                || decoded.contains('/')
                || decoded.contains('\\')
            {
                return Err(DocsError::InvalidConfig(format!(
                    "documentation route `{value}` contains an unsafe segment"
                )));
            }
            if percent_encode_route_segment(&decoded) != encoded {
                return Err(DocsError::InvalidConfig(format!(
                    "documentation route `{value}` is not canonically encoded"
                )));
            }
            decoded_segments.push(decoded);
        }
        Ok(Self {
            decoded: format!("/{}", decoded_segments.join("/")),
            encoded: value.to_string(),
        })
    }

    fn decoded_relative_path(&self) -> PathBuf {
        self.decoded
            .trim_start_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect()
    }
}

fn checked_slash_path(path: &Path, source_path: &Path) -> Result<String, DocsError> {
    let mut segments = Vec::new();
    for component in path.components() {
        let Component::Normal(segment) = component else {
            return Err(DocsError::InvalidPagePath(
                source_path.to_path_buf(),
                "identity path must contain only normal relative segments".to_string(),
            ));
        };
        segments.push(checked_identity_segment(segment, source_path)?);
    }
    Ok(segments.join("/"))
}

fn checked_identity_segment<'a>(
    segment: &'a OsStr,
    source_path: &Path,
) -> Result<&'a str, DocsError> {
    let segment = segment.to_str().ok_or_else(|| {
        DocsError::InvalidPagePath(
            source_path.to_path_buf(),
            "path segment is not valid UTF-8".to_string(),
        )
    })?;
    if segment.contains(['/', '\\']) {
        return Err(DocsError::InvalidPagePath(
            source_path.to_path_buf(),
            "path segment contains a platform-dependent separator".to_string(),
        ));
    }
    Ok(segment)
}

fn percent_encode_route_segment(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

fn percent_decode_route_segment(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = decode_hex(*bytes.get(index + 1)?)?;
            let low = decode_hex(*bytes.get(index + 2)?)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            if !bytes[index].is_ascii_alphanumeric()
                && !matches!(bytes[index], b'-' | b'.' | b'_' | b'~')
            {
                return None;
            }
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

const fn decode_hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'A'..=b'F' => Some(value - b'A' + 10),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

#[derive(Debug, Serialize)]
struct ApiEntry {
    key: String,
    value: wake_tsdoc::ApiDoc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DemoDescriptor {
    pub id: String,
    pub title: String,
    pub group: String,
    pub component: String,
    pub order: i32,
    pub control_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DemoControl {
    name: String,
    type_text: String,
    kind: String,
    required: bool,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deprecated: Option<String>,
    options: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct DemoInfo {
    id: String,
    title: String,
    group: String,
    component: String,
    order: i32,
    controls: Vec<DemoControl>,
    warnings: Vec<String>,
    source: String,
    source_module: String,
    import_path: String,
}

impl DemoInfo {
    fn descriptor(&self) -> DemoDescriptor {
        DemoDescriptor {
            id: self.id.clone(),
            title: self.title.clone(),
            group: self.group.clone(),
            component: self.component.clone(),
            order: self.order,
            control_count: self
                .controls
                .iter()
                .filter(|control| control.kind != "readonly")
                .count(),
            warnings: self.warnings.clone(),
        }
    }
}

/// Generate the normal documentation site. This compatibility wrapper keeps the pre-workbench
/// API and behavior unchanged.
pub fn generate(
    project_root: impl AsRef<Path>,
    options: &DocsOptions,
    mode: BuildMode,
) -> Result<GeneratedProject, DocsError> {
    generate_with_mode(project_root, options, mode, DocsMode::Site)
}
/// Scan, compile, and atomically materialize the generated docs module tree.
pub fn generate_with_mode(
    project_root: impl AsRef<Path>,
    options: &DocsOptions,
    mode: BuildMode,
    docs_mode: DocsMode,
) -> Result<GeneratedProject, DocsError> {
    generate_with_mode_in(
        project_root,
        Path::new(".wake/docs/generated"),
        options,
        mode,
        docs_mode,
    )
}

/// Generate into a caller-owned derived tree. Development orchestration uses this to build a
/// candidate configuration without overwriting the accepted session's generated modules.
pub fn generate_with_mode_in(
    project_root: impl AsRef<Path>,
    generated_dir: impl AsRef<Path>,
    options: &DocsOptions,
    mode: BuildMode,
    docs_mode: DocsMode,
) -> Result<GeneratedProject, DocsError> {
    let inputs = prepare_render_inputs(project_root.as_ref(), options)?;
    let generated_dir = wake_common::fs::resolve_existing_prefix(&absolute_from(
        &inputs.root,
        generated_dir.as_ref(),
    ));
    validate_generation_namespace(&inputs.root, &generated_dir)?;
    let rendered = render_prepared(inputs, options, mode, docs_mode)?;
    let generation = rendered_file_map(&rendered.files);
    let changed_files = publish_generation(&rendered.root, &generated_dir, &generation)?;

    Ok(GeneratedProject {
        root: rendered.root.clone(),
        generated_dir: generated_dir.clone(),
        entry: generated_dir.join(rendered.entry_relative.as_path()),
        aliases: vec![
            ("@@wake/docs".to_string(), generated_dir),
            ("@@wake/docs-project".to_string(), rendered.root),
        ],
        watch_roots: rendered.watch_roots,
        routes: rendered.routes,
        mode: rendered.mode,
        demos: rendered.demos,
        warnings: rendered.warnings,
        changed_files,
    })
}

/// Render the complete generated Docs project without observing or modifying a publication tree.
///
/// The returned files are immutable and self-contained. In particular, this function does not
/// create or inspect `.wake/docs/generated`; callers choose whether and where to publish or mount
/// the render.
pub fn render_with_mode(
    project_root: impl AsRef<Path>,
    options: &DocsOptions,
    mode: BuildMode,
    docs_mode: DocsMode,
) -> Result<RenderedProject, DocsError> {
    let inputs = prepare_render_inputs(project_root.as_ref(), options)?;
    render_prepared(inputs, options, mode, docs_mode)
}

struct RenderInputs {
    root: PathBuf,
    source_dir: PathBuf,
}

fn prepare_render_inputs(
    project_root: &Path,
    options: &DocsOptions,
) -> Result<RenderInputs, DocsError> {
    validate_options(options)?;
    let root = canonical_dir(project_root)?;
    validate_react_dependencies(&root)?;
    let source_dir = absolute_from(&root, &options.source_dir);
    if !source_dir.is_dir() {
        return Err(DocsError::Io(
            source_dir,
            "docs source directory does not exist".to_string(),
        ));
    }
    let source_dir = fs::canonicalize(&source_dir).unwrap_or(source_dir);
    Ok(RenderInputs { root, source_dir })
}

fn render_prepared(
    inputs: RenderInputs,
    options: &DocsOptions,
    mode: BuildMode,
    docs_mode: DocsMode,
) -> Result<RenderedProject, DocsError> {
    let RenderInputs { root, source_dir } = inputs;

    let mut mdx_files = Vec::new();
    let mut demo_files = Vec::new();
    scan_files(&source_dir, &mut mdx_files, &mut demo_files)?;
    mdx_files.sort();
    demo_files.sort();
    if docs_mode == DocsMode::Components {
        mdx_files.clear();
    }

    let mut pages = Vec::new();
    for path in &mdx_files {
        let page = compile_page(&root, &source_dir, path)?;
        pages.push((path.clone(), page));
    }
    if docs_mode == DocsMode::Site {
        apply_navigation(&source_dir, &mut pages)?;
    }
    if mode == BuildMode::Production {
        pages.retain(|(_, page)| !page.route.draft);
    }
    ensure_unique_routes(&pages)?;

    let demos = compile_demos(&root, &source_dir, &demo_files, docs_mode)?;
    let mut generation = RenderedFileTreeBuilder::new();
    for (_, page) in &pages {
        let rendered = page.render_module();
        insert_generation_file(
            &mut generation,
            PathBuf::from(&page.identity.generated_module),
            rendered.code.as_bytes(),
        )?;
        insert_generation_file(
            &mut generation,
            PathBuf::from(&page.identity.generated_map),
            rendered.source_map.as_bytes(),
        )?;
    }
    for demo in &demos {
        let output = Path::new("demo-source").join(&demo.source_module);
        let language = normalize_code_language(
            Path::new(&demo.id)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("text"),
        );
        let highlighted = render_highlighted_code_lines(&demo.source, &language, &BTreeSet::new());
        let module = format!(
            "export default {};\nexport const language = {};\nexport const highlighted = <>{highlighted}</>;\n",
            js_string(&demo.source),
            js_string(&language)
        );
        insert_generation_file(&mut generation, output, module.as_bytes())?;
    }

    let routes: Vec<_> = pages.iter().map(|(_, page)| page.route.clone()).collect();
    let api_entries: Vec<_> = pages
        .iter()
        .flat_map(|(_, page)| page.api_entries.iter())
        .collect();
    let registry = render_registry(&pages, &demos, &api_entries);
    let search_corpus = render_search_corpus(&pages);
    let config = render_config(&root, options, docs_mode)?;
    let mut fixed = vec![
        ("registry.ts", registry.as_str()),
        ("search-corpus.ts", search_corpus.as_str()),
        ("config.tsx", config.as_str()),
        ("runtime/app.tsx", RUNTIME_APP),
        ("runtime/routes.mjs", RUNTIME_ROUTES),
        ("runtime/search.mjs", RUNTIME_SEARCH),
        ("runtime/styles.css", RUNTIME_STYLE),
    ];
    let entry_relative = match docs_mode {
        DocsMode::Site => {
            fixed.push(("runtime/site-entry.tsx", RUNTIME_SITE_ENTRY));
            "runtime/site-entry.tsx"
        }
        DocsMode::Components => {
            fixed.extend([
                ("runtime/components.tsx", RUNTIME_COMPONENTS),
                ("runtime/components-state.mjs", RUNTIME_COMPONENT_STATE),
                ("runtime/components-entry.tsx", RUNTIME_COMPONENTS_ENTRY),
                ("runtime/components.css", RUNTIME_COMPONENT_STYLE),
            ]);
            "runtime/components-entry.tsx"
        }
    };
    for (relative, content) in fixed {
        insert_generation_file(&mut generation, PathBuf::from(relative), content.as_bytes())?;
    }

    let manifest_files = generation.manifest_files();
    let manifest = serde_json::to_vec_pretty(&GenerationManifest {
        files: manifest_files,
    })
    .expect("serializable manifest");
    insert_generation_file(&mut generation, PathBuf::from("manifest.json"), &manifest)?;
    let files = generation.seal();
    let entry_relative = projected_generation_path(Path::new(entry_relative))?;

    let mut watch_roots = vec![source_dir.clone(), root.join("src")];
    if let Some(preview) = &options.preview {
        watch_roots.push(absolute_from(&root, preview));
    }
    if let Some(theme) = &options.theme_css {
        watch_roots.push(absolute_from(&root, theme));
    }
    watch_roots.sort();
    watch_roots.dedup();

    let demo_descriptors = demos.iter().map(DemoInfo::descriptor).collect();
    let warnings = demos
        .iter()
        .flat_map(|demo| demo.warnings.iter().cloned())
        .collect();

    Ok(RenderedProject {
        root,
        files,
        entry_relative,
        watch_roots,
        routes,
        mode: docs_mode,
        demos: demo_descriptors,
        warnings,
    })
}

fn compile_page(root: &Path, source_dir: &Path, path: &Path) -> Result<CompiledPage, DocsError> {
    let identity = PageIdentity::from_paths(root, source_dir, path)?;
    let source = fs::read_to_string(path)
        .map_err(|error| DocsError::Io(path.to_path_buf(), error.to_string()))?;
    let mut constructs = Constructs::gfm();
    constructs.autolink = false;
    constructs.code_indented = false;
    constructs.html_flow = false;
    constructs.html_text = false;
    constructs.frontmatter = true;
    constructs.mdx_esm = true;
    constructs.mdx_expression_flow = true;
    constructs.mdx_expression_text = true;
    constructs.mdx_jsx_flow = true;
    constructs.mdx_jsx_text = true;
    let ast = markdown::to_mdast(
        &source,
        &ParseOptions {
            constructs,
            mdx_esm_parse: Some(Box::new(parse_mdx_esm)),
            ..ParseOptions::default()
        },
    )
    .map_err(|error| DocsError::Mdx(path.to_path_buf(), error.to_string()))?;
    validate_compile_components(path, &ast)?;

    let frontmatter = find_frontmatter(path, &ast)?;
    let headings = HeadingPlan::from_ast(&ast).headings;
    let search_text = collect_search_text(&ast);
    let title = frontmatter
        .title
        .clone()
        .or_else(|| headings.first().map(|heading| heading.title.clone()))
        .unwrap_or_else(|| {
            title_case(
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("Page"),
            )
        });
    let kind = frontmatter.kind.unwrap_or_else(|| "guide".to_string());
    if !matches!(
        kind.as_str(),
        "overview" | "tutorial" | "guide" | "reference" | "component"
    ) {
        return Err(DocsError::Frontmatter(
            path.to_path_buf(),
            format!("kind `{kind}` must be overview, tutorial, guide, reference, or component"),
        ));
    }
    let status = frontmatter.status.unwrap_or_else(|| "stable".to_string());
    if !matches!(
        status.as_str(),
        "stable" | "beta" | "experimental" | "deprecated"
    ) {
        return Err(DocsError::Frontmatter(
            path.to_path_buf(),
            format!("status `{status}` must be stable, beta, experimental, or deprecated"),
        ));
    }
    let route = RouteInfo {
        id: identity.id.clone(),
        file: identity.source_file.clone(),
        title,
        description: frontmatter.description,
        kind,
        group: String::new(),
        group_id: String::new(),
        section: String::new(),
        section_id: String::new(),
        slug: identity.route_path.encoded.clone(),
        status,
        draft: frontmatter.draft,
        hidden: frontmatter.hidden,
        headings,
    };

    let mut renderer = Renderer::new(&route.file, &route.headings);
    let body = renderer.render_root(&ast);
    let rewritten_esm = rewrite_mdx_esm(root, path, &ast)?;
    let api_entries = collect_api_entries(root, path, &route.file, &ast)?;
    Ok(CompiledPage {
        identity,
        route,
        search_text,
        module_plan: PageModulePlan {
            source,
            rewritten_esm,
            body,
        },
        api_entries,
    })
}

impl CompiledPage {
    fn render_module(&self) -> RenderedPageModule {
        self.module_plan.render(&self.identity, &self.route)
    }
}

impl PageModulePlan {
    fn render(&self, identity: &PageIdentity, route: &RouteInfo) -> RenderedPageModule {
        let meta_json = serde_json::to_string(route).expect("serializable route");
        let mut writer = ModuleWriter::new(&self.source);
        writer.push_synthetic(
            "import { MdxPage, Demo, Demos, API, CodeBlock } from \"@@wake/docs/runtime/app.tsx\";\n",
        );
        for fragment in &self.rewritten_esm.fragments {
            match (fragment.source_offset, fragment.kind) {
                (Some(offset), SourceFragmentKind::Exact) => {
                    writer.push_exact(&fragment.text, offset);
                }
                (Some(offset), SourceFragmentKind::DerivedToken) => {
                    writer.push_derived(&fragment.text, offset);
                }
                (None, _) => writer.push_synthetic(&fragment.text),
            }
        }
        writer.push_synthetic(&format!(
            "\nexport const __wakeMeta = {meta_json};\nexport default function WakeMdxContent() {{\n  return <MdxPage meta={{__wakeMeta}}>\n"
        ));
        for node in &self.body {
            writer.push_synthetic("    ");
            if let Some(offset) = node.source_offset {
                writer.push_derived(&node.code, offset);
            } else {
                writer.push_synthetic(&node.code);
            }
            writer.push_synthetic("\n");
        }
        writer.push_synthetic("  </MdxPage>;\n}\n");
        let map_name = Path::new(&identity.generated_map)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("page.tsx.map");
        writer.push_synthetic(&format!("//# sourceMappingURL={map_name}\n"));
        let source_map = writer.render_source_map(identity, &self.source);
        let code = writer.finish();
        RenderedPageModule { code, source_map }
    }
}

fn find_frontmatter(path: &Path, ast: &Node) -> Result<Frontmatter, DocsError> {
    for node in ast.children().map(Vec::as_slice).unwrap_or_default() {
        if let Node::Toml(frontmatter) = node {
            return toml::from_str(&frontmatter.value)
                .map_err(|error| DocsError::Frontmatter(path.to_path_buf(), error.to_string()));
        }
    }
    Ok(Frontmatter::default())
}

fn apply_navigation(
    source_dir: &Path,
    pages: &mut [(PathBuf, CompiledPage)],
) -> Result<(), DocsError> {
    let path = source_dir.join("navigation.toml");
    let source = fs::read_to_string(&path)
        .map_err(|error| DocsError::Navigation(path.clone(), error.to_string()))?;
    let navigation: NavigationConfig = toml::from_str(&source)
        .map_err(|error| DocsError::Navigation(path.clone(), error.to_string()))?;
    if navigation.groups.is_empty() {
        return Err(DocsError::Navigation(
            path,
            "at least one [[group]] is required".to_string(),
        ));
    }

    let mut group_ids = BTreeSet::new();
    let mut placements = BTreeMap::<String, (usize, String, String, String, String)>::new();
    let mut order = 0usize;
    for group in &navigation.groups {
        validate_navigation_id(&path, "group", &group.id)?;
        if group.title.trim().is_empty() {
            return Err(DocsError::Navigation(
                path.clone(),
                format!("group `{}` must have a title", group.id),
            ));
        }
        if !group_ids.insert(group.id.clone()) {
            return Err(DocsError::Navigation(
                path.clone(),
                format!("duplicate group id `{}`", group.id),
            ));
        }
        for page in &group.pages {
            insert_navigation_page(&path, &mut placements, page, order, group, None)?;
            order += 1;
        }
        let mut section_ids = BTreeSet::new();
        for section in &group.sections {
            validate_navigation_id(&path, "section", &section.id)?;
            if section.title.trim().is_empty() || section.pages.is_empty() {
                return Err(DocsError::Navigation(
                    path.clone(),
                    format!(
                        "section `{}/{}` must have a title and at least one page",
                        group.id, section.id
                    ),
                ));
            }
            if !section_ids.insert(section.id.clone()) {
                return Err(DocsError::Navigation(
                    path.clone(),
                    format!("duplicate section id `{}/{}`", group.id, section.id),
                ));
            }
            for page in &section.pages {
                insert_navigation_page(&path, &mut placements, page, order, group, Some(section))?;
                order += 1;
            }
        }
    }

    let known = pages
        .iter()
        .map(|(_, page)| page.route.id.clone())
        .collect::<BTreeSet<_>>();
    for id in placements.keys() {
        if !known.contains(id) {
            return Err(DocsError::Navigation(
                path.clone(),
                format!("navigation references missing page `{id}`"),
            ));
        }
    }
    for (_, page) in pages.iter_mut() {
        if let Some((_, group_id, group, section_id, section)) = placements.get(&page.route.id) {
            page.route.group_id.clone_from(group_id);
            page.route.group.clone_from(group);
            page.route.section_id.clone_from(section_id);
            page.route.section.clone_from(section);
        } else if !page.route.hidden {
            return Err(DocsError::Navigation(
                path.clone(),
                format!(
                    "page `{}` is not listed in navigation.toml; list it or set hidden = true",
                    page.route.id
                ),
            ));
        }
    }
    pages.sort_by_key(|(_, page)| {
        placements
            .get(&page.route.id)
            .map_or(usize::MAX, |placement| placement.0)
    });
    Ok(())
}

fn validate_navigation_id(path: &Path, kind: &str, id: &str) -> Result<(), DocsError> {
    let valid = !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
    if valid {
        Ok(())
    } else {
        Err(DocsError::Navigation(
            path.to_path_buf(),
            format!("{kind} id `{id}` must use lowercase ASCII letters, digits, and hyphens"),
        ))
    }
}

fn normalized_page_id(value: &str) -> String {
    let value = value.trim().trim_matches('/').replace('\\', "/");
    value.strip_suffix(".mdx").unwrap_or(&value).to_string()
}

fn insert_navigation_page(
    path: &Path,
    placements: &mut BTreeMap<String, (usize, String, String, String, String)>,
    page: &str,
    order: usize,
    group: &NavigationGroup,
    section: Option<&NavigationSection>,
) -> Result<(), DocsError> {
    let id = normalized_page_id(page);
    if id.is_empty() {
        return Err(DocsError::Navigation(
            path.to_path_buf(),
            format!("group `{}` contains an empty page id", group.id),
        ));
    }
    let placement = (
        order,
        group.id.clone(),
        group.title.clone(),
        section.map_or_else(String::new, |section| section.id.clone()),
        section.map_or_else(String::new, |section| section.title.clone()),
    );
    if placements.insert(id.clone(), placement).is_some() {
        return Err(DocsError::Navigation(
            path.to_path_buf(),
            format!("page `{id}` appears more than once"),
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct HeadingPlan {
    headings: Vec<HeadingInfo>,
}

impl HeadingPlan {
    fn from_ast(ast: &Node) -> Self {
        let mut headings = Vec::new();
        let mut allocated = BTreeMap::<String, usize>::new();
        visit(ast, &mut |node| {
            if let Node::Heading(heading) = node {
                let title = heading
                    .children
                    .iter()
                    .map(Node::to_string)
                    .collect::<String>();
                let base = slugify(&title);
                let count = allocated.entry(base.clone()).or_default();
                let id = if *count == 0 {
                    base
                } else {
                    format!("{base}-{count}")
                };
                *count += 1;
                headings.push(HeadingInfo {
                    depth: heading.depth,
                    id,
                    title,
                });
            }
        });
        Self { headings }
    }
}

fn collect_search_text(ast: &Node) -> String {
    let mut search_text = String::new();
    visit(ast, &mut |node| {
        let value = match node {
            Node::Text(value) => &value.value,
            Node::InlineCode(value) => &value.value,
            Node::Code(value) => &value.value,
            _ => return,
        };
        for word in value.split_whitespace() {
            if !search_text.is_empty() {
                search_text.push(' ');
            }
            search_text.push_str(word);
        }
    });
    search_text
}

fn validate_compile_components(path: &Path, ast: &Node) -> Result<(), DocsError> {
    let mut error = None;
    visit(ast, &mut |node| {
        let (name, attributes) = match node {
            Node::MdxJsxFlowElement(element) => (element.name.as_deref(), &element.attributes),
            Node::MdxJsxTextElement(element) => (element.name.as_deref(), &element.attributes),
            _ => return,
        };
        let Some(name @ ("Demo" | "Demos" | "API")) = name else {
            return;
        };
        if error.is_some() {
            return;
        }
        if attributes
            .iter()
            .any(|attribute| matches!(attribute, AttributeContent::Expression(_)))
        {
            error = Some(macro_error(
                path,
                node,
                format!("<{name}> does not accept spread attributes at compile time"),
            ));
            return;
        }
        let required: &[&str] = match name {
            "Demo" => &["src"],
            "Demos" => &["glob"],
            "API" => &["source", "symbol"],
            _ => &[],
        };
        for key in required {
            let attribute = attributes.iter().find_map(|attribute| match attribute {
                AttributeContent::Property(property) if property.name == *key => Some(property),
                _ => None,
            });
            if attribute
                .and_then(|attribute| static_string(attribute.value.as_ref()))
                .is_none()
            {
                error = Some(macro_error(
                    path,
                    node,
                    format!("<{name}> attribute `{key}` must be a static string literal"),
                ));
                return;
            }
        }
        for attribute in attributes {
            let AttributeContent::Property(attribute) = attribute else {
                continue;
            };
            let compile_time = matches!(
                (name, attribute.name.as_str()),
                ("Demo", "src")
                    | ("Demos", "glob")
                    | ("Demos", "columns")
                    | ("API", "source")
                    | ("API", "symbol")
                    | ("API", "component")
            );
            if compile_time && !is_static_value(attribute.value.as_ref()) {
                error = Some(macro_error(
                    path,
                    node,
                    format!(
                        "<{name}> attribute `{}` must be a static literal",
                        attribute.name
                    ),
                ));
                return;
            }
        }
    });
    error.map_or(Ok(()), Err)
}

fn macro_error(path: &Path, node: &Node, message: String) -> DocsError {
    let point = node.position().map(|position| &position.start);
    DocsError::InvalidMacro {
        path: path.to_path_buf(),
        line: point.map_or(1, |point| point.line),
        column: point.map_or(1, |point| point.column),
        message,
    }
}

fn collect_api_entries(
    root: &Path,
    page_path: &Path,
    page_file: &str,
    ast: &Node,
) -> Result<Vec<ApiEntry>, DocsError> {
    let mut requests = Vec::new();
    visit(ast, &mut |node| {
        let attributes = match node {
            Node::MdxJsxFlowElement(element) if element.name.as_deref() == Some("API") => {
                Some(&element.attributes)
            }
            Node::MdxJsxTextElement(element) if element.name.as_deref() == Some("API") => {
                Some(&element.attributes)
            }
            _ => None,
        };
        let Some(attributes) = attributes else {
            return;
        };
        let source = literal_attribute(attributes, "source");
        let symbol = literal_attribute(attributes, "symbol");
        let component = literal_attribute(attributes, "component");
        if let (Some(source), Some(symbol)) = (source, symbol) {
            requests.push((source, symbol, component));
        }
    });

    let mut entries = Vec::new();
    for (source, symbol, component) in requests {
        let source_path = page_path.parent().unwrap_or(root).join(&source);
        let mut doc = wake_tsdoc::extract_api(&source_path, &symbol, component.as_deref())
            .map_err(|error| DocsError::Api(source_path.clone(), error.to_string()))?;
        relativize_api_sources(root, &mut doc);
        entries.push(ApiEntry {
            key: format!("{page_file}|{source}|{symbol}"),
            value: doc,
        });
    }
    Ok(entries)
}

fn relativize_api_sources(root: &Path, doc: &mut wake_tsdoc::ApiDoc) {
    fn relative(root: &Path, value: &str) -> String {
        let path = Path::new(value);
        if path.is_absolute() {
            slash_path(path.strip_prefix(root).unwrap_or(path))
        } else {
            value.replace('\\', "/")
        }
    }
    doc.source = relative(root, &doc.source);
    for prop in &mut doc.props {
        prop.source = relative(root, &prop.source);
    }
    for group in &mut doc.inherited {
        group.source = relative(root, &group.source);
    }
}

#[derive(Debug, Default)]
struct StaticDemoMeta {
    title: Option<String>,
    group: Option<String>,
    component: Option<String>,
    order: i32,
}

fn compile_demos(
    root: &Path,
    source_dir: &Path,
    files: &[PathBuf],
    docs_mode: DocsMode,
) -> Result<Vec<DemoInfo>, DocsError> {
    let mut demos = Vec::new();
    for path in files {
        let source = fs::read_to_string(path)
            .map_err(|error| DocsError::Io(path.clone(), error.to_string()))?;
        let relative = path.strip_prefix(root).map_err(|_| {
            DocsError::InvalidConfig(format!("demo {} is outside project root", path.display()))
        })?;
        let source_relative = path.strip_prefix(source_dir).map_err(|_| {
            DocsError::InvalidConfig(format!(
                "demo {} is outside docs source_dir",
                path.display()
            ))
        })?;
        let id = slash_path(relative);
        let source_module = slash_path(relative.with_extension("source.tsx"));
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("Demo")
            .trim_end_matches(".demo");
        let static_meta = extract_static_demo_meta(&source);
        let (derived_group, derived_component) = derive_demo_location(source_relative, stem);
        let mut warnings = Vec::new();
        if docs_mode == DocsMode::Components
            && static_demo_args(&source).is_some_and(|value| !is_json_literal(value))
        {
            warnings.push(format!(
                "{id}: meta.args contains a dynamic or non-JSON value; it will be ignored"
            ));
        }
        let controls = if docs_mode == DocsMode::Components {
            match wake_tsdoc::extract_demo_props(path) {
                Ok(Some(mut api)) => {
                    relativize_api_sources(root, &mut api);
                    warnings.extend(
                        api.warnings
                            .iter()
                            .map(|warning| format!("{id}: {warning}")),
                    );
                    api.props.iter().map(control_from_prop).collect()
                }
                Ok(None) => Vec::new(),
                Err(error) => {
                    warnings.push(format!("{id}: cannot infer demo props: {error}"));
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        demos.push(DemoInfo {
            id: id.clone(),
            title: static_meta.title.unwrap_or_else(|| title_case(stem)),
            group: static_meta.group.unwrap_or(derived_group),
            component: static_meta.component.unwrap_or(derived_component),
            order: static_meta.order,
            controls,
            warnings,
            source,
            source_module,
            import_path: format!("@@wake/docs-project/{id}"),
        });
    }
    demos.sort_by(|left, right| {
        left.group
            .to_ascii_lowercase()
            .cmp(&right.group.to_ascii_lowercase())
            .then_with(|| {
                left.component
                    .to_ascii_lowercase()
                    .cmp(&right.component.to_ascii_lowercase())
            })
            .then_with(|| left.order.cmp(&right.order))
            .then_with(|| left.title.cmp(&right.title))
    });
    Ok(demos)
}

fn derive_demo_location(relative: &Path, stem: &str) -> (String, String) {
    let mut directories = relative
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .filter(|value| !value.eq_ignore_ascii_case("demos"))
        .map(title_case)
        .collect::<Vec<_>>();
    let component = directories.pop().unwrap_or_else(|| title_case(stem));
    let group = if directories.is_empty() {
        "Other".to_string()
    } else {
        directories.join(" / ")
    };
    (group, component)
}

fn extract_static_demo_meta(source: &str) -> StaticDemoMeta {
    let Some(body) = static_demo_meta_body(source) else {
        return StaticDemoMeta::default();
    };
    StaticDemoMeta {
        title: static_demo_string(body, "title"),
        group: static_demo_string(body, "group"),
        component: static_demo_string(body, "component"),
        order: static_demo_i32(body, "order").unwrap_or_default(),
    }
}

fn static_demo_meta_body(source: &str) -> Option<&str> {
    let pattern = Regex::new(r"(?s)export\s+const\s+meta\s*=\s*\{").expect("valid demo meta regex");
    let found = pattern.find(source)?;
    let open = found.end() - 1;
    let close = find_js_matching(source, open, '{', '}')?;
    Some(&source[open + 1..close])
}

fn static_demo_property<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    let bytes = source.as_bytes();
    let mut start = 0usize;
    let mut offset = 0usize;
    let mut depths = [0usize; 3];
    let mut quote = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;

    while offset < bytes.len() {
        let byte = bytes[offset];
        let next = bytes.get(offset + 1).copied();
        if line_comment {
            line_comment = byte != b'\n';
            offset += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && next == Some(b'/') {
                block_comment = false;
                offset += 2;
            } else {
                offset += 1;
            }
            continue;
        }
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == active {
                quote = None;
            }
            offset += 1;
            continue;
        }

        match (byte, next) {
            (b'/', Some(b'/')) => {
                line_comment = true;
                offset += 2;
                continue;
            }
            (b'/', Some(b'*')) => {
                block_comment = true;
                offset += 2;
                continue;
            }
            _ => {}
        }
        match byte {
            b'"' | b'\'' | b'`' => quote = Some(byte),
            b'{' => depths[0] += 1,
            b'}' => depths[0] = depths[0].saturating_sub(1),
            b'[' => depths[1] += 1,
            b']' => depths[1] = depths[1].saturating_sub(1),
            b'(' => depths[2] += 1,
            b')' => depths[2] = depths[2].saturating_sub(1),
            b',' if depths == [0, 0, 0] => {
                if let Some(value) = static_demo_entry(&source[start..offset], key) {
                    return Some(value);
                }
                start = offset + 1;
            }
            _ => {}
        }
        offset += 1;
    }
    static_demo_entry(&source[start..], key)
}

fn static_demo_entry<'a>(entry: &'a str, key: &str) -> Option<&'a str> {
    let mut entry = entry.trim_start();
    loop {
        if let Some(comment) = entry.strip_prefix("//") {
            entry = comment.split_once('\n')?.1.trim_start();
        } else if let Some(comment) = entry.strip_prefix("/*") {
            entry = comment.split_once("*/")?.1.trim_start();
        } else {
            break;
        }
    }
    entry
        .strip_prefix(key)?
        .trim_start()
        .strip_prefix(':')
        .map(str::trim)
}

fn static_demo_string(source: &str, key: &str) -> Option<String> {
    let value = static_demo_property(source, key)?;
    if value.starts_with('"') && value.ends_with('"') {
        serde_json::from_str(value).ok()
    } else if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        Some(
            value[1..value.len() - 1]
                .replace("\\'", "'")
                .replace("\\\\", "\\"),
        )
    } else {
        None
    }
}

fn static_demo_i32(source: &str, key: &str) -> Option<i32> {
    static_demo_property(source, key)?.parse().ok()
}

fn static_demo_args(source: &str) -> Option<&str> {
    let body = static_demo_meta_body(source)?;
    static_demo_property(body, "args")
}

fn is_json_literal(source: &str) -> bool {
    let mut parser = JsonLiteralParser { source, offset: 0 };
    parser.value() && {
        parser.trivia();
        parser.offset == source.len()
    }
}

struct JsonLiteralParser<'a> {
    source: &'a str,
    offset: usize,
}

impl JsonLiteralParser<'_> {
    fn value(&mut self) -> bool {
        self.trivia();
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"' | '\'') => self.string(),
            Some('-' | '0'..='9') => self.number(),
            Some(_) => ["true", "false", "null"]
                .into_iter()
                .any(|keyword| self.keyword(keyword)),
            None => false,
        }
    }

    fn object(&mut self) -> bool {
        self.bump();
        self.trivia();
        if self.consume('}') {
            return true;
        }
        loop {
            self.trivia();
            if matches!(self.peek(), Some('"' | '\'')) {
                if !self.string() {
                    return false;
                }
            } else if !self.identifier() {
                return false;
            }
            self.trivia();
            if !self.consume(':') || !self.value() {
                return false;
            }
            self.trivia();
            if self.consume('}') {
                return true;
            }
            if !self.consume(',') {
                return false;
            }
            self.trivia();
            if self.consume('}') {
                return true;
            }
        }
    }

    fn array(&mut self) -> bool {
        self.bump();
        self.trivia();
        if self.consume(']') {
            return true;
        }
        loop {
            if !self.value() {
                return false;
            }
            self.trivia();
            if self.consume(']') {
                return true;
            }
            if !self.consume(',') {
                return false;
            }
            self.trivia();
            if self.consume(']') {
                return true;
            }
        }
    }

    fn string(&mut self) -> bool {
        let Some(quote @ ('"' | '\'')) = self.bump() else {
            return false;
        };
        let mut escaped = false;
        while let Some(character) = self.bump() {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote {
                return true;
            }
        }
        false
    }

    fn number(&mut self) -> bool {
        let start = self.offset;
        while self.peek().is_some_and(|character| {
            character.is_ascii_digit() || matches!(character, '-' | '+' | '.' | 'e' | 'E')
        }) {
            self.bump();
        }
        self.source[start..self.offset].parse::<f64>().is_ok()
    }

    fn keyword(&mut self, keyword: &str) -> bool {
        if self.source[self.offset..].starts_with(keyword) {
            self.offset += keyword.len();
            true
        } else {
            false
        }
    }

    fn identifier(&mut self) -> bool {
        let Some(first) = self.peek() else {
            return false;
        };
        if !first.is_ascii_alphabetic() && !matches!(first, '_' | '$') {
            return false;
        }
        self.bump();
        while self.peek().is_some_and(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '$')
        }) {
            self.bump();
        }
        true
    }

    fn trivia(&mut self) {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            }
            if self.source[self.offset..].starts_with("//") {
                self.offset += 2;
                while self.peek().is_some_and(|character| character != '\n') {
                    self.bump();
                }
            } else if self.source[self.offset..].starts_with("/*") {
                let Some(end) = self.source[self.offset + 2..].find("*/") else {
                    self.offset = self.source.len();
                    return;
                };
                self.offset += end + 4;
            } else {
                return;
            }
        }
    }

    fn consume(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.offset..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.offset += character.len_utf8();
        Some(character)
    }
}
fn find_js_matching(source: &str, open: usize, open_char: char, close_char: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (offset, character) in source[open..].char_indices() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == active {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') || character == '\u{60}' {
            quote = Some(character);
        } else if character == open_char {
            depth += 1;
        } else if character == close_char {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}
fn control_from_prop(prop: &wake_tsdoc::ApiProp) -> DemoControl {
    let type_text = prop.type_text.trim();
    let options = literal_union_options(type_text);
    let kind = if !options.is_empty() {
        "select"
    } else if type_text == "boolean" {
        "boolean"
    } else if type_text == "string" {
        "string"
    } else if type_text == "number" {
        "number"
    } else if type_text.starts_with('{')
        || type_text.starts_with('[')
        || type_text.ends_with("[]")
        || type_text.starts_with("Array<")
        || type_text.starts_with("ReadonlyArray<")
        || type_text.starts_with("Record<")
    {
        "json"
    } else {
        "readonly"
    };
    DemoControl {
        name: prop.name.clone(),
        type_text: prop.type_text.clone(),
        kind: kind.to_string(),
        required: prop.required,
        description: prop.description.clone(),
        default_value: prop
            .default_value
            .as_deref()
            .and_then(parse_static_json_value),
        deprecated: prop.deprecated.clone(),
        options,
    }
}

fn literal_union_options(type_text: &str) -> Vec<serde_json::Value> {
    let parts = type_text.split('|').map(str::trim).collect::<Vec<_>>();
    if parts.len() < 2 {
        return Vec::new();
    }
    let literal_parts = parts
        .iter()
        .copied()
        .filter(|part| *part != "undefined" && *part != "null")
        .collect::<Vec<_>>();
    let options = literal_parts
        .iter()
        .filter_map(|part| parse_static_json_value(part))
        .collect::<Vec<_>>();
    if options.len() == literal_parts.len() {
        options
    } else {
        Vec::new()
    }
}

fn parse_static_json_value(value: &str) -> Option<serde_json::Value> {
    let value = value.trim();
    if value == "undefined" {
        return None;
    }
    if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        return Some(serde_json::Value::String(
            value[1..value.len() - 1]
                .replace("\\'", "'")
                .replace("\\\\", "\\"),
        ));
    }
    serde_json::from_str(value).ok()
}

fn render_registry(
    pages: &[(PathBuf, CompiledPage)],
    demos: &[DemoInfo],
    api_entries: &[&ApiEntry],
) -> String {
    let mut output = String::from("// Generated by Wake. Do not edit.\nexport const pages = [\n");
    for (_, page) in pages {
        let meta = serde_json::to_string(&page.route).expect("serializable route");
        let route_path =
            serde_json::to_string(&page.identity.route_path).expect("serializable route path");
        output.push_str(&format!(
            "  {{ ...{meta}, routePath: {route_path}, load: () => import(\"@@wake/docs/{}\") }},\n",
            page.identity.generated_module
        ));
    }
    output.push_str("] as const;\nexport const demos = [\n");
    for demo in demos {
        let metadata = serde_json::to_string(&json!({
            "id": demo.id,
            "title": demo.title,
            "group": demo.group,
            "component": demo.component,
            "order": demo.order,
            "controls": demo.controls,
            "warnings": demo.warnings,
        }))
        .expect("serializable demo metadata");
        output.push_str(&format!(
            "  {{ ...{metadata}, load: () => import({}), loadSource: () => import(\"@@wake/docs/demo-source/{}\") }},\n",
            js_string(&demo.import_path),
            demo.source_module
        ));
    }
    output.push_str("] as const;\nexport const apiDocs = ");
    let map: BTreeMap<_, _> = api_entries
        .iter()
        .map(|entry| (entry.key.as_str(), &entry.value))
        .collect();
    output.push_str(&serde_json::to_string(&map).expect("serializable API docs"));
    output.push_str(" as const;\n");
    output
}

fn render_search_corpus(pages: &[(PathBuf, CompiledPage)]) -> String {
    let corpus: BTreeMap<_, _> = pages
        .iter()
        .filter(|(_, page)| !page.route.hidden)
        .map(|(_, page)| (page.route.id.as_str(), page.search_text.as_str()))
        .collect();
    format!(
        "// Generated by Wake. Do not edit.\nexport const searchTextByPage = {} as const;\n",
        serde_json::to_string(&corpus).expect("serializable search corpus")
    )
}

fn render_config(
    root: &Path,
    options: &DocsOptions,
    docs_mode: DocsMode,
) -> Result<String, DocsError> {
    let mut output = String::from("import React from \"react\";\n");
    if let Some(css) = &options.theme_css {
        output.push_str(&format!(
            "import {};\n",
            js_string(&root_relative_alias(root, &absolute_from(root, css))?)
        ));
    }
    if let Some(preview) = &options.preview {
        output.push_str(&format!(
            "export {{ default as Preview }} from {};\n",
            js_string(&root_relative_alias(root, &absolute_from(root, preview))?)
        ));
    } else {
        output.push_str("export function Preview({ children }: { children: React.ReactNode }) { return <div className=\"demo-default-preview\">{children}</div>; }\n");
    }
    let base_path = normalize_base(&options.base_path);
    let logo = options
        .logo
        .as_deref()
        .map(|value| public_asset_url(&base_path, value));
    let config = json!({
        "title": options.title, "description": options.description, "locale": options.locale, "logo": logo,
        "repositoryUrl": options.repository_url, "basePath": base_path,
        "defaultTheme": options.default_theme, "accentColor": options.accent_color,
        "presentation": options.presentation.as_str(),
        "mode": docs_mode.as_str(),
    });
    output.push_str(&format!(
        "export const siteConfig = {} as const;\n",
        serde_json::to_string(&config).expect("serializable config")
    ));
    Ok(output)
}

fn public_asset_url(base_path: &str, value: &str) -> String {
    if value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with("//")
        || value.starts_with("data:")
        || value.starts_with('#')
    {
        return value.to_string();
    }
    let asset = value.trim_start_matches('/');
    if base_path == "/" {
        format!("/{asset}")
    } else if value.starts_with(base_path) {
        value.to_string()
    } else {
        format!("{base_path}{asset}")
    }
}

struct Renderer<'a> {
    page_file: &'a str,
    headings: &'a [HeadingInfo],
    heading_index: usize,
}

#[derive(Debug)]
struct RenderedNode {
    code: String,
    source_offset: Option<usize>,
}

impl<'a> Renderer<'a> {
    fn new(page_file: &'a str, headings: &'a [HeadingInfo]) -> Self {
        Self {
            page_file,
            headings,
            heading_index: 0,
        }
    }

    fn render_root(&mut self, node: &Node) -> Vec<RenderedNode> {
        let mut output = Vec::new();
        if let Some(children) = node.children() {
            for child in children {
                if !matches!(child, Node::Toml(_) | Node::Yaml(_) | Node::MdxjsEsm(_)) {
                    output.push(RenderedNode {
                        code: self.render(child),
                        source_offset: child.position().map(|position| position.start.offset),
                    });
                }
            }
        }
        output
    }

    fn render(&mut self, node: &Node) -> String {
        match node {
            Node::Root(value) => self.render_children(&value.children),
            Node::Paragraph(value) => self.render_paragraph(&value.children),
            Node::Heading(value) => {
                let heading = self
                    .headings
                    .get(self.heading_index)
                    .expect("heading plan and renderer traversal must stay aligned");
                self.heading_index += 1;
                debug_assert_eq!(heading.depth, value.depth);
                let id = heading.id.clone();
                let title = heading.title.clone();
                let content = self.render_children(&value.children);
                format!(
                    "<h{} id={} aria-label={}>{}<a className=\"heading-anchor\" href={} aria-label={}>#</a></h{}>",
                    value.depth,
                    js_braced(&id),
                    js_braced(&title),
                    content,
                    js_braced(&format!("#{id}")),
                    js_braced(&format!("Section: {title}")),
                    value.depth
                )
            }
            Node::Text(value) => js_braced(&value.value),
            Node::Emphasis(value) => format!("<em>{}</em>", self.render_children(&value.children)),
            Node::Strong(value) => {
                format!("<strong>{}</strong>", self.render_children(&value.children))
            }
            Node::Delete(value) => format!("<del>{}</del>", self.render_children(&value.children)),
            Node::InlineCode(value) => format!("<code>{}</code>", js_braced(&value.value)),
            Node::InlineMath(value) => format!(
                "<code className=\"math-inline\">{}</code>",
                js_braced(&value.value)
            ),
            Node::Break(_) => "<br />".to_string(),
            Node::Blockquote(value) => format!(
                "<blockquote>{}</blockquote>",
                self.render_children(&value.children)
            ),
            Node::List(value) => {
                let tag = if value.ordered { "ol" } else { "ul" };
                let class_name = if value
                    .children
                    .iter()
                    .any(|node| matches!(node, Node::ListItem(item) if item.checked.is_some()))
                {
                    " className=\"task-list\""
                } else {
                    ""
                };
                let start = value
                    .start
                    .filter(|start| *start != 1)
                    .map_or(String::new(), |start| format!(" start={{{start}}}"));
                format!(
                    "<{tag}{class_name}{start}>{}</{tag}>",
                    self.render_children(&value.children)
                )
            }
            Node::ListItem(value) => {
                let checkbox = value.checked.map_or(String::new(), |checked| {
                    format!("<input type=\"checkbox\" checked={{{checked}}} disabled />")
                });
                let class_name = if value.checked.is_some() {
                    " className=\"task-list-item\""
                } else {
                    ""
                };
                format!(
                    "<li{class_name}>{checkbox}{}</li>",
                    self.render_children(&value.children)
                )
            }
            Node::Code(value) => {
                render_code_block(value.lang.as_deref(), value.meta.as_deref(), &value.value)
            }
            Node::Math(value) => format!(
                "<pre className=\"math-block\"><code>{}</code></pre>",
                js_braced(&value.value)
            ),
            Node::ThematicBreak(_) => "<hr />".to_string(),
            Node::Link(value) => {
                let title = value.title.as_deref().map_or(String::new(), |title| {
                    format!(" title={}", js_braced(title))
                });
                format!(
                    "<a href={}{title}>{}</a>",
                    js_braced(&value.url),
                    self.render_children(&value.children)
                )
            }
            Node::Image(value) => {
                let title = value.title.as_deref().map_or(String::new(), |title| {
                    format!(" title={}", js_braced(title))
                });
                format!(
                    "<img src={} alt={}{title} />",
                    js_braced(&value.url),
                    js_braced(&value.alt)
                )
            }
            Node::Table(value) => self.render_table(&value.children),
            Node::TableRow(value) => format!("<tr>{}</tr>", self.render_children(&value.children)),
            Node::TableCell(value) => format!("<td>{}</td>", self.render_children(&value.children)),
            Node::MdxTextExpression(value) => render_expression(&value.value),
            Node::MdxFlowExpression(value) => render_expression(&value.value),
            Node::MdxJsxFlowElement(value) => {
                self.render_jsx(value.name.as_deref(), &value.attributes, &value.children)
            }
            Node::MdxJsxTextElement(value) => {
                self.render_jsx(value.name.as_deref(), &value.attributes, &value.children)
            }
            Node::Html(value) => value.value.clone(),
            Node::LinkReference(value) => format!(
                "<a href={}>{}</a>",
                js_braced(&format!("#{}", value.identifier)),
                self.render_children(&value.children)
            ),
            Node::ImageReference(value) => format!(
                "<img alt={} data-reference={} />",
                js_braced(&value.alt),
                js_braced(&value.identifier)
            ),
            Node::FootnoteReference(value) => format!(
                "<sup><a href={}>{}</a></sup>",
                js_braced(&format!("#fn-{}", value.identifier)),
                js_braced(&value.identifier)
            ),
            Node::FootnoteDefinition(value) => format!(
                "<aside id={} className=\"footnote\">{}</aside>",
                js_braced(&format!("fn-{}", value.identifier)),
                self.render_children(&value.children)
            ),
            Node::Definition(_) | Node::Toml(_) | Node::Yaml(_) | Node::MdxjsEsm(_) => {
                String::new()
            }
        }
    }

    fn render_children(&mut self, children: &[Node]) -> String {
        children.iter().map(|child| self.render(child)).collect()
    }

    fn render_paragraph(&mut self, children: &[Node]) -> String {
        let jsx_only = children
            .iter()
            .filter(|child| !is_whitespace_text(child))
            .all(is_jsx_node);
        if !children
            .iter()
            .any(|child| is_paragraph_block_boundary(child, jsx_only))
        {
            return format!("<p>{}</p>", self.render_children(children));
        }

        let mut output = String::new();
        let mut phrasing = Vec::new();
        for child in children {
            if is_paragraph_block_boundary(child, jsx_only) {
                self.flush_paragraph_phrasing(&mut output, &mut phrasing);
                output.push_str(&self.render(child));
            } else {
                phrasing.push(child);
            }
        }
        self.flush_paragraph_phrasing(&mut output, &mut phrasing);
        output
    }

    fn flush_paragraph_phrasing(&mut self, output: &mut String, phrasing: &mut Vec<&Node>) {
        if phrasing.iter().any(|child| !is_whitespace_text(child)) {
            output.push_str("<p>");
            for child in phrasing.iter() {
                output.push_str(&self.render(child));
            }
            output.push_str("</p>");
        }
        phrasing.clear();
    }

    fn render_table(&mut self, rows: &[Node]) -> String {
        let Some((head, body)) = rows.split_first() else {
            return "<div className=\"table-scroll\"><table /></div>".to_string();
        };
        let head = self.render_table_row(head, "th");
        let body = body
            .iter()
            .map(|row| self.render_table_row(row, "td"))
            .collect::<String>();
        format!(
            "<div className=\"table-scroll\"><table><thead>{head}</thead><tbody>{body}</tbody></table></div>"
        )
    }

    fn render_table_row(&mut self, row: &Node, cell: &str) -> String {
        let Node::TableRow(row) = row else {
            return self.render(row);
        };
        let cells = row
            .children
            .iter()
            .map(|node| match node {
                Node::TableCell(value) => {
                    format!("<{cell}>{}</{cell}>", self.render_children(&value.children))
                }
                _ => self.render(node),
            })
            .collect::<String>();
        format!("<tr>{cells}</tr>")
    }

    fn render_jsx(
        &mut self,
        name: Option<&str>,
        attributes: &[AttributeContent],
        children: &[Node],
    ) -> String {
        let Some(name) = name else {
            return format!("<>{}</>", self.render_children(children));
        };
        let mut attrs = render_attributes(attributes);
        if matches!(name, "Demo" | "Demos" | "API") {
            attrs.push_str(&format!(" __wakePage={}", js_braced(self.page_file)));
        }
        if children.is_empty() {
            format!("<{name}{attrs} />")
        } else {
            format!("<{name}{attrs}>{}</{name}>", self.render_children(children))
        }
    }
}

fn is_whitespace_text(node: &Node) -> bool {
    matches!(node, Node::Text(value) if value.value.trim().is_empty())
}

fn is_jsx_node(node: &Node) -> bool {
    matches!(
        node,
        Node::MdxJsxFlowElement(_) | Node::MdxJsxTextElement(_)
    )
}

fn is_paragraph_block_boundary(node: &Node, jsx_only: bool) -> bool {
    match node {
        Node::MdxJsxFlowElement(_) => true,
        Node::MdxJsxTextElement(element) => match element.name.as_deref() {
            Some(name) if is_html_block_element(name) => true,
            Some(name) => jsx_only && is_custom_jsx_name(name),
            None => {
                jsx_only
                    || element
                        .children
                        .iter()
                        .any(|child| is_paragraph_block_boundary(child, false))
            }
        },
        _ => false,
    }
}

fn is_custom_jsx_name(name: &str) -> bool {
    name.split('.').next().is_some_and(|head| {
        head.chars()
            .next()
            .is_some_and(|character| character.is_ascii_uppercase() || character == '_')
    })
}

fn is_html_block_element(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "body"
            | "caption"
            | "col"
            | "colgroup"
            | "dd"
            | "details"
            | "dialog"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "header"
            | "hgroup"
            | "hr"
            | "html"
            | "legend"
            | "li"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "search"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

fn render_code_block(language: Option<&str>, meta: Option<&str>, code: &str) -> String {
    let language = normalize_code_language(language.unwrap_or("text"));
    let line_count = code.lines().count().max(1);
    let highlighted_lines = code_highlighted_lines(meta, line_count);
    let content = render_highlighted_code_lines(code, &language, &highlighted_lines);
    let title = code_title(meta)
        .map(|title| format!(" title={}", js_braced(&title)))
        .unwrap_or_default();
    format!(
        "<CodeBlock language={} code={}{title}>{content}</CodeBlock>",
        js_braced(&language),
        js_braced(code)
    )
}

fn render_highlighted_code_lines(
    code: &str,
    language: &str,
    highlighted_lines: &BTreeSet<usize>,
) -> String {
    let lines = highlight_code(code, language);
    let mut content = String::new();
    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        let class = if highlighted_lines.contains(&number) {
            "code-line is-highlighted"
        } else {
            "code-line"
        };
        content.push_str(&format!(
            "<span className=\"{class}\" data-line={{{number}}}>{line}</span>"
        ));
    }
    content
}

fn normalize_code_language(language: &str) -> String {
    match language.trim().to_ascii_lowercase().as_str() {
        "js" => "javascript",
        "ts" => "typescript",
        "sh" | "shell" | "zsh" => "bash",
        "ps1" | "pwsh" => "powershell",
        "rs" => "rust",
        "py" => "python",
        "md" => "markdown",
        "yml" => "yaml",
        "htm" | "xml" => "html",
        "" => "text",
        value => value,
    }
    .to_string()
}

fn highlight_code(code: &str, language: &str) -> Vec<String> {
    let mut lines = vec![String::new()];
    if !supports_code_highlighting(language) {
        push_code_segment(&mut lines, None, code);
        return lines;
    }
    let pattern = code_token_pattern(language);
    let mut cursor = 0;
    for matched in pattern.find_iter(code) {
        push_code_segment(&mut lines, None, &code[cursor..matched.start()]);
        let kind = classify_code_token(
            language,
            code,
            matched.start(),
            matched.end(),
            matched.as_str(),
        );
        push_code_segment(&mut lines, kind, matched.as_str());
        cursor = matched.end();
    }
    push_code_segment(&mut lines, None, &code[cursor..]);
    lines
}

fn push_code_segment(lines: &mut Vec<String>, kind: Option<&str>, value: &str) {
    for (index, part) in value.split('\n').enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        let part = part.strip_suffix('\r').unwrap_or(part);
        if part.is_empty() {
            continue;
        }
        let rendered = js_braced(part);
        if let Some(kind) = kind {
            lines.last_mut().expect("one code line").push_str(&format!(
                "<span className=\"syntax-{kind}\">{rendered}</span>"
            ));
        } else {
            lines.last_mut().expect("one code line").push_str(&rendered);
        }
    }
}

fn supports_code_highlighting(language: &str) -> bool {
    matches!(
        language,
        "javascript"
            | "typescript"
            | "jsx"
            | "tsx"
            | "rust"
            | "bash"
            | "python"
            | "powershell"
            | "sql"
            | "json"
            | "jsonc"
            | "toml"
            | "yaml"
            | "css"
            | "scss"
            | "html"
            | "mdx"
            | "markdown"
            | "c"
            | "cpp"
            | "java"
            | "go"
    )
}

fn code_token_pattern(language: &str) -> &'static Regex {
    static DEFAULT: OnceLock<Regex> = OnceLock::new();
    static HASH_COMMENT: OnceLock<Regex> = OnceLock::new();
    static MARKUP_COMMENT: OnceLock<Regex> = OnceLock::new();
    static SQL_COMMENT: OnceLock<Regex> = OnceLock::new();
    let (cache, extra_comment, template_literal) =
        if matches!(language, "bash" | "powershell" | "python" | "toml" | "yaml") {
            (&HASH_COMMENT, r"|#[^\n]*", r"|`(?:\\.|[^`\\])*`")
        } else if matches!(language, "html" | "mdx" | "markdown") {
            (&MARKUP_COMMENT, r"|(?s:<!--.*?-->)", "")
        } else if language == "sql" {
            (&SQL_COMMENT, r"|--[^\n]*", r"|`(?:\\.|[^`\\])*`")
        } else {
            (&DEFAULT, "", r"|`(?:\\.|[^`\\])*`")
        };
    cache.get_or_init(|| {
        Regex::new(&format!(
            r#"(?s:/\*.*?\*/)|//[^\n]*{extra_comment}|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'{template_literal}|#[0-9A-Fa-f]{{3,8}}\b|\b(?:0x[0-9A-Fa-f]+|\d+(?:\.\d+)?)\b|[A-Za-z_$][A-Za-z0-9_$-]*|[{{}}()\[\];,.<>:=+\-*/%!&|?~^@]+"#
        ))
        .expect("valid code token regex")
    })
}

fn classify_code_token<'a>(
    language: &str,
    code: &str,
    start: usize,
    end: usize,
    token: &'a str,
) -> Option<&'a str> {
    let hash_comments = matches!(language, "bash" | "powershell" | "python" | "toml" | "yaml");
    if token.starts_with("//")
        || token.starts_with("/*")
        || token.starts_with("<!--")
        || (language == "sql" && token.starts_with("--"))
        || (hash_comments && token.starts_with('#'))
    {
        return Some("comment");
    }
    let next = code[end..].chars().find(|value| !value.is_whitespace());
    let previous = code[..start]
        .chars()
        .rev()
        .find(|value| !value.is_whitespace());
    if token.starts_with('"') || token.starts_with(char::from(39u8)) || token.starts_with('`') {
        return Some(if next == Some(':') {
            "property"
        } else {
            "string"
        });
    }
    if token.starts_with('#')
        || token
            .chars()
            .next()
            .is_some_and(|value| value.is_ascii_digit())
    {
        return Some("number");
    }
    if token
        .chars()
        .next()
        .is_some_and(|value| value.is_ascii_alphabetic() || matches!(value, '_' | '$'))
    {
        if is_code_literal(token) {
            return Some("literal");
        }
        if is_code_keyword(language, token) {
            return Some("keyword");
        }
        if is_builtin_type(token) || token.chars().next().is_some_and(char::is_uppercase) {
            return Some("type");
        }
        if token.starts_with('$') {
            return Some("variable");
        }
        if next == Some('(') {
            return Some("function");
        }
        if previous == Some('.')
            || next == Some(':')
            || (matches!(
                language,
                "html" | "mdx" | "json" | "jsonc" | "toml" | "yaml" | "css" | "scss"
            ) && next == Some('='))
        {
            return Some("property");
        }
        return None;
    }
    if token.chars().any(|value| "=+-*/%!&|?~^<>".contains(value)) {
        Some("operator")
    } else {
        Some("punctuation")
    }
}

fn is_code_literal(value: &str) -> bool {
    matches!(
        value,
        "true" | "false" | "null" | "undefined" | "None" | "Some" | "Ok" | "Err"
    )
}

fn is_builtin_type(value: &str) -> bool {
    matches!(
        value,
        "any"
            | "boolean"
            | "never"
            | "number"
            | "object"
            | "string"
            | "symbol"
            | "unknown"
            | "void"
            | "bool"
            | "char"
            | "str"
            | "String"
            | "usize"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "f32"
            | "f64"
    )
}

fn is_code_keyword(language: &str, value: &str) -> bool {
    match language {
        "javascript" | "typescript" | "jsx" | "tsx" | "mdx" => matches!(
            value,
            "as" | "async"
                | "await"
                | "break"
                | "case"
                | "catch"
                | "class"
                | "const"
                | "continue"
                | "debugger"
                | "declare"
                | "default"
                | "delete"
                | "do"
                | "else"
                | "enum"
                | "export"
                | "extends"
                | "finally"
                | "for"
                | "from"
                | "function"
                | "get"
                | "if"
                | "implements"
                | "import"
                | "in"
                | "infer"
                | "instanceof"
                | "interface"
                | "is"
                | "keyof"
                | "let"
                | "namespace"
                | "new"
                | "of"
                | "private"
                | "protected"
                | "public"
                | "readonly"
                | "return"
                | "satisfies"
                | "set"
                | "static"
                | "super"
                | "switch"
                | "throw"
                | "try"
                | "type"
                | "typeof"
                | "var"
                | "while"
                | "with"
                | "yield"
        ),
        "rust" => matches!(
            value,
            "as" | "async"
                | "await"
                | "break"
                | "const"
                | "continue"
                | "crate"
                | "dyn"
                | "else"
                | "enum"
                | "extern"
                | "fn"
                | "for"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "mod"
                | "move"
                | "mut"
                | "pub"
                | "ref"
                | "return"
                | "self"
                | "Self"
                | "static"
                | "struct"
                | "super"
                | "trait"
                | "type"
                | "unsafe"
                | "use"
                | "where"
                | "while"
        ),
        "bash" => matches!(
            value,
            "case"
                | "do"
                | "done"
                | "elif"
                | "else"
                | "esac"
                | "fi"
                | "for"
                | "function"
                | "if"
                | "in"
                | "select"
                | "then"
                | "time"
                | "until"
                | "while"
        ),
        "python" => matches!(
            value,
            "and"
                | "as"
                | "assert"
                | "async"
                | "await"
                | "break"
                | "class"
                | "continue"
                | "def"
                | "del"
                | "elif"
                | "else"
                | "except"
                | "finally"
                | "for"
                | "from"
                | "global"
                | "if"
                | "import"
                | "in"
                | "is"
                | "lambda"
                | "nonlocal"
                | "not"
                | "or"
                | "pass"
                | "raise"
                | "return"
                | "try"
                | "while"
                | "with"
                | "yield"
        ),
        "powershell" => matches!(
            value.to_ascii_lowercase().as_str(),
            "begin"
                | "break"
                | "catch"
                | "class"
                | "continue"
                | "data"
                | "do"
                | "dynamicparam"
                | "else"
                | "elseif"
                | "end"
                | "enum"
                | "filter"
                | "finally"
                | "for"
                | "foreach"
                | "from"
                | "function"
                | "if"
                | "in"
                | "param"
                | "process"
                | "return"
                | "switch"
                | "throw"
                | "trap"
                | "try"
                | "until"
                | "using"
                | "while"
        ),
        "sql" => matches!(
            value.to_ascii_lowercase().as_str(),
            "alter"
                | "and"
                | "as"
                | "asc"
                | "begin"
                | "by"
                | "case"
                | "create"
                | "delete"
                | "desc"
                | "distinct"
                | "drop"
                | "else"
                | "end"
                | "from"
                | "group"
                | "having"
                | "in"
                | "insert"
                | "into"
                | "is"
                | "join"
                | "limit"
                | "not"
                | "null"
                | "on"
                | "or"
                | "order"
                | "select"
                | "set"
                | "table"
                | "then"
                | "union"
                | "update"
                | "values"
                | "when"
                | "where"
        ),
        "c" | "cpp" | "java" | "go" => matches!(
            value,
            "break"
                | "case"
                | "class"
                | "const"
                | "continue"
                | "default"
                | "defer"
                | "do"
                | "else"
                | "extends"
                | "final"
                | "for"
                | "func"
                | "goto"
                | "if"
                | "implements"
                | "import"
                | "interface"
                | "new"
                | "package"
                | "private"
                | "protected"
                | "public"
                | "return"
                | "static"
                | "struct"
                | "switch"
                | "throw"
                | "throws"
                | "try"
                | "typedef"
                | "var"
                | "while"
        ),
        _ => false,
    }
}

fn code_highlighted_lines(meta: Option<&str>, maximum: usize) -> BTreeSet<usize> {
    let mut lines = BTreeSet::new();
    let Some(meta) = meta else {
        return lines;
    };
    let pattern = Regex::new(r"\{([0-9,\-\s]+)\}").expect("valid line metadata regex");
    let Some(captures) = pattern.captures(meta) else {
        return lines;
    };
    for part in captures[1].split(',').map(str::trim) {
        if let Some((start, end)) = part.split_once('-') {
            if let (Ok(start), Ok(end)) =
                (start.trim().parse::<usize>(), end.trim().parse::<usize>())
            {
                let start = start.max(1).min(maximum);
                let end = end.max(1).min(maximum);
                if start <= end {
                    lines.extend(start..=end);
                }
            }
        } else if let Ok(line) = part.parse::<usize>()
            && (1..=maximum).contains(&line)
        {
            lines.insert(line);
        }
    }
    lines
}

fn code_title(meta: Option<&str>) -> Option<String> {
    let meta = meta?;
    let double = Regex::new(r#"title\s*=\s*"([^"]+)""#).expect("valid title regex");
    if let Some(captures) = double.captures(meta) {
        return Some(captures[1].to_string());
    }
    let single = Regex::new(r"title\s*=\s*'([^']+)'").expect("valid title regex");
    single
        .captures(meta)
        .map(|captures| captures[1].to_string())
}

fn render_attributes(attributes: &[AttributeContent]) -> String {
    let mut output = String::new();
    for attribute in attributes {
        match attribute {
            AttributeContent::Expression(expression) => {
                output.push_str(" {...(");
                output.push_str(&expression.value);
                output.push_str(")}");
            }
            AttributeContent::Property(property) => {
                output.push(' ');
                output.push_str(&property.name);
                if let Some(value) = &property.value {
                    match value {
                        AttributeValue::Literal(value) => {
                            output.push('=');
                            output.push_str(&js_braced(value));
                        }
                        AttributeValue::Expression(value) => {
                            output.push_str("={");
                            output.push_str(if value.value.trim().is_empty() {
                                "undefined"
                            } else {
                                &value.value
                            });
                            output.push('}');
                        }
                    }
                }
            }
        }
    }
    output
}

fn render_expression(value: &str) -> String {
    if value.trim().is_empty() {
        "{null}".to_string()
    } else {
        format!("{{{value}}}")
    }
}

fn parse_mdx_esm(value: &str) -> MdxSignal {
    let interner = Interner::new();
    let parsed = parse(value, &interner, SourceType::Tsx);
    let errors = parsed
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.is_error())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        return MdxSignal::Ok;
    }

    let error = errors
        .iter()
        .find(|diagnostic| diagnostic_offset(diagnostic) < value.len())
        .copied()
        .unwrap_or(errors[0]);
    let message = error.message.clone();
    if errors
        .iter()
        .all(|diagnostic| diagnostic_offset(diagnostic) >= value.len())
    {
        MdxSignal::Eof(message, Box::default(), Box::default())
    } else {
        MdxSignal::Error(
            message,
            diagnostic_offset(error).min(value.len()),
            Box::default(),
            Box::default(),
        )
    }
}

fn diagnostic_offset(diagnostic: &wake_common::Diagnostic) -> usize {
    diagnostic
        .labels
        .iter()
        .find(|label| label.primary)
        .or_else(|| diagnostic.labels.first())
        .map_or(0, |label| label.span.lo as usize)
}

#[derive(Debug)]
struct SpecifierReplacement {
    span: Span,
    value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceFragmentKind {
    Exact,
    DerivedToken,
}

#[derive(Debug)]
struct SourceFragment {
    text: String,
    source_offset: Option<usize>,
    kind: SourceFragmentKind,
}

#[derive(Debug)]
struct RewrittenEsm {
    fragments: Vec<SourceFragment>,
}

#[derive(Debug)]
struct MdxEsmBlock {
    value: String,
    stops: Vec<(usize, usize)>,
    source_start: usize,
}

fn rewrite_mdx_esm(root: &Path, page: &Path, ast: &Node) -> Result<RewrittenEsm, DocsError> {
    let mut blocks = Vec::new();
    visit(ast, &mut |node| {
        if let Node::MdxjsEsm(esm) = node {
            blocks.push(MdxEsmBlock {
                value: esm.value.clone(),
                stops: esm.stops.clone(),
                source_start: esm
                    .position
                    .as_ref()
                    .map_or(0, |position| position.start.offset),
            });
        }
    });

    let mut fragments = Vec::new();
    for block in blocks {
        let interner = Interner::new();
        let parsed = parse(&block.value, &interner, SourceType::Tsx);
        if let Some(error) = parsed
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.is_error())
        {
            return Err(DocsError::Mdx(page.to_path_buf(), error.message.clone()));
        }
        let (tokens, lexer_diagnostics) = tokenize(&block.value);
        if let Some(error) = lexer_diagnostics
            .iter()
            .find(|diagnostic| diagnostic.is_error())
        {
            return Err(DocsError::Mdx(page.to_path_buf(), error.message.clone()));
        }
        let lexer = Lexer::new(&block.value);
        let mut replacements = Vec::new();
        for dependency in parsed.dependencies {
            if !matches!(
                dependency.kind,
                DependencyKind::Import | DependencyKind::ExportFrom | DependencyKind::DynamicImport
            ) {
                continue;
            }
            let specifier = interner.resolve(dependency.specifier);
            if !specifier.starts_with("./") && !specifier.starts_with("../") {
                continue;
            }
            let Some(token) =
                locate_dependency_specifier(&tokens, dependency.kind, dependency.span)
            else {
                return Err(DocsError::Mdx(
                    page.to_path_buf(),
                    format!(
                        "could not locate the typed module specifier `{specifier}` in its parser span"
                    ),
                ));
            };
            if lexer.string_value(token.span).as_ref() != specifier {
                return Err(DocsError::Mdx(
                    page.to_path_buf(),
                    format!(
                        "typed module specifier `{specifier}` did not match its syntax-selected token"
                    ),
                ));
            }
            let resolved = normalize_path(&page.parent().unwrap_or(root).join(&specifier));
            let Ok(relative) = resolved.strip_prefix(root) else {
                continue;
            };
            let raw = token.span.slice(&block.value);
            let quote = raw.chars().next().unwrap_or('"');
            replacements.push(SpecifierReplacement {
                span: token.span,
                value: format!("{quote}@@wake/docs-project/{}{quote}", slash_path(relative)),
            });
        }
        replacements.sort_by_key(|replacement| replacement.span.lo);
        replacements.dedup_by_key(|replacement| replacement.span);
        let mut cursor = 0;
        for replacement in replacements {
            append_exact_esm_fragments(
                &mut fragments,
                &block,
                cursor..replacement.span.lo as usize,
            );
            fragments.push(SourceFragment {
                text: replacement.value,
                source_offset: mdx_absolute_offset(&block, replacement.span.lo as usize),
                kind: SourceFragmentKind::DerivedToken,
            });
            cursor = replacement.span.hi as usize;
        }
        append_exact_esm_fragments(&mut fragments, &block, cursor..block.value.len());
        if !block.value.ends_with('\n') {
            fragments.push(SourceFragment {
                text: "\n".to_string(),
                source_offset: None,
                kind: SourceFragmentKind::Exact,
            });
        }
    }
    Ok(RewrittenEsm { fragments })
}

fn locate_dependency_specifier(
    tokens: &[Token],
    kind: DependencyKind,
    dependency_span: Span,
) -> Option<Token> {
    let within = tokens
        .iter()
        .copied()
        .filter(|token| dependency_span.contains(token.span))
        .collect::<Vec<_>>();
    match kind {
        DependencyKind::Import => {
            if let Some(token) = within
                .iter()
                .find(|token| token.kind == TokenKind::Str && token.span == dependency_span)
            {
                return Some(*token);
            }
            top_level_token_after(&within, TokenKind::Keyword(Keyword::From), TokenKind::Str)
        }
        DependencyKind::ExportFrom => {
            top_level_token_after(&within, TokenKind::Keyword(Keyword::From), TokenKind::Str)
        }
        DependencyKind::DynamicImport => {
            let import = within
                .iter()
                .position(|token| token.kind == TokenKind::Keyword(Keyword::Import))?;
            let lparen = within[import + 1..]
                .iter()
                .position(|token| token.kind == TokenKind::LParen)?
                + import
                + 1;
            within
                .get(lparen + 1)
                .copied()
                .filter(|token| token.kind == TokenKind::Str)
        }
        DependencyKind::Require => None,
    }
}

fn top_level_token_after(
    tokens: &[Token],
    marker: TokenKind,
    expected: TokenKind,
) -> Option<Token> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::LParen | TokenKind::LBrace | TokenKind::LBracket => depth += 1,
            TokenKind::RParen | TokenKind::RBrace | TokenKind::RBracket => {
                depth = depth.saturating_sub(1);
            }
            _ if depth == 0 && token.kind == marker => {
                if let Some(candidate) = tokens
                    .get(index + 1)
                    .copied()
                    .filter(|token| token.kind == expected)
                {
                    return Some(candidate);
                }
            }
            _ => {}
        }
    }
    None
}

fn append_exact_esm_fragments(
    fragments: &mut Vec<SourceFragment>,
    block: &MdxEsmBlock,
    range: std::ops::Range<usize>,
) {
    if range.is_empty() {
        return;
    }
    let mut boundaries = vec![range.start];
    boundaries.extend(
        block
            .stops
            .iter()
            .map(|(relative, _)| *relative)
            .filter(|relative| range.start < *relative && *relative < range.end),
    );
    boundaries.push(range.end);
    for pair in boundaries.windows(2) {
        let start = pair[0];
        let end = pair[1];
        fragments.push(SourceFragment {
            text: block.value[start..end].to_string(),
            source_offset: mdx_absolute_offset(block, start),
            kind: SourceFragmentKind::Exact,
        });
    }
}

fn mdx_absolute_offset(block: &MdxEsmBlock, relative: usize) -> Option<usize> {
    let mut selected = None;
    for &(stop_relative, stop_absolute) in &block.stops {
        if stop_relative > relative {
            break;
        }
        selected = Some((stop_relative, stop_absolute));
    }
    selected
        .map(|(stop_relative, stop_absolute)| stop_absolute + relative - stop_relative)
        .or_else(|| Some(block.source_start + relative))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OriginalPoint {
    line: i64,
    column: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GeneratedSegment {
    column: i64,
    original: Option<OriginalPoint>,
}

#[derive(Debug)]
struct ModuleWriter<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
    code: String,
    line: usize,
    column: i64,
    mappings: Vec<Vec<GeneratedSegment>>,
}

impl<'a> ModuleWriter<'a> {
    fn new(source: &'a str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        Self {
            source,
            line_starts,
            code: String::new(),
            line: 0,
            column: 0,
            mappings: vec![Vec::new()],
        }
    }

    fn push_synthetic(&mut self, value: &str) {
        self.push_raw(value);
    }

    fn push_exact(&mut self, value: &str, source_offset: usize) {
        let mut cursor = 0;
        while cursor < value.len() {
            let end = value[cursor..]
                .find('\n')
                .map_or(value.len(), |relative| cursor + relative);
            if end > cursor {
                self.push_mapped_line(&value[cursor..end], source_offset + cursor);
            }
            if end == value.len() {
                break;
            }
            self.push_raw("\n");
            cursor = end + 1;
        }
    }

    fn push_derived(&mut self, value: &str, source_offset: usize) {
        if value.is_empty() {
            return;
        }
        if let Some(newline) = value.find('\n') {
            if newline > 0 {
                self.push_mapped_line(&value[..newline], source_offset);
            }
            self.push_raw(&value[newline..]);
        } else {
            self.push_mapped_line(value, source_offset);
        }
    }

    fn push_mapped_line(&mut self, value: &str, source_offset: usize) {
        debug_assert!(!value.contains('\n'));
        if value.is_empty() {
            return;
        }
        self.push_segment(Some(self.original_point(source_offset)));
        self.push_raw(value);
        self.push_segment(None);
    }

    fn push_raw(&mut self, value: &str) {
        self.code.push_str(value);
        for character in value.chars() {
            if character == '\n' {
                self.line += 1;
                self.column = 0;
                if self.mappings.len() <= self.line {
                    self.mappings.push(Vec::new());
                }
            } else {
                self.column += character.len_utf16() as i64;
            }
        }
    }

    fn push_segment(&mut self, original: Option<OriginalPoint>) {
        let segment = GeneratedSegment {
            column: self.column,
            original,
        };
        let line = &mut self.mappings[self.line];
        if line.last().is_some_and(|last| last.column == self.column) {
            *line.last_mut().expect("checked last segment") = segment;
        } else {
            line.push(segment);
        }
    }

    fn original_point(&self, offset: usize) -> OriginalPoint {
        let offset = offset.min(self.source.len());
        let line = self.line_starts.partition_point(|start| *start <= offset) - 1;
        let column = self.source[self.line_starts[line]..offset]
            .encode_utf16()
            .count();
        OriginalPoint {
            line: line as i64,
            column: column as i64,
        }
    }

    fn render_source_map(&self, identity: &PageIdentity, source: &str) -> String {
        let line_count = self.code.lines().count();
        let mut encoded = String::new();
        let mut previous_source = 0i64;
        let mut previous_original_line = 0i64;
        let mut previous_original_column = 0i64;
        for line_index in 0..line_count {
            if line_index > 0 {
                encoded.push(';');
            }
            let mut previous_generated_column = 0i64;
            for (index, segment) in self
                .mappings
                .get(line_index)
                .into_iter()
                .flatten()
                .enumerate()
            {
                if index > 0 {
                    encoded.push(',');
                }
                encode_vlq(segment.column - previous_generated_column, &mut encoded);
                previous_generated_column = segment.column;
                if let Some(original) = segment.original {
                    encode_vlq(-previous_source, &mut encoded);
                    previous_source = 0;
                    encode_vlq(original.line - previous_original_line, &mut encoded);
                    previous_original_line = original.line;
                    encode_vlq(original.column - previous_original_column, &mut encoded);
                    previous_original_column = original.column;
                }
            }
        }
        serde_json::to_string(&json!({
            "version": 3,
            "file": identity.generated_module,
            "sources": [identity.source_file],
            "sourcesContent": [source],
            "names": [],
            "mappings": encoded,
        }))
        .expect("serializable source map")
    }

    fn finish(self) -> String {
        self.code
    }
}

fn encode_vlq(value: i64, output: &mut String) {
    const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut value = if value < 0 {
        ((-value) as u64) * 2 + 1
    } else {
        value as u64 * 2
    };
    loop {
        let mut digit = value & 31;
        value >>= 5;
        if value > 0 {
            digit |= 32;
        }
        output.push(BASE64[digit as usize] as char);
        if value == 0 {
            break;
        }
    }
}

fn ensure_unique_routes(pages: &[(PathBuf, CompiledPage)]) -> Result<(), DocsError> {
    let mut routes = BTreeMap::new();
    for (path, page) in pages {
        if let Some(first) = routes.insert(page.route.slug.clone(), path.clone()) {
            return Err(DocsError::DuplicateRoute {
                slug: page.route.slug.clone(),
                first,
                second: path.clone(),
            });
        }
    }
    Ok(())
}

fn validate_react_dependencies(root: &Path) -> Result<(), DocsError> {
    let package_path = root.join("package.json");
    let source = fs::read_to_string(&package_path).map_err(|error| {
        DocsError::InvalidConfig(format!(
            "React {MINIMUM_REACT_MAJOR}+ docs require a project package.json: {error}"
        ))
    })?;
    let package: serde_json::Value = serde_json::from_str(&source).map_err(|error| {
        DocsError::InvalidConfig(format!("invalid project package.json: {error}"))
    })?;
    let lower_bound = Regex::new(r"(?:^|[|,\s@:])(?:\^|~|>=?|=)?\s*v?(\d+)")
        .expect("valid package version regex");
    for dependency in ["react", "react-dom"] {
        let requirement = ["dependencies", "peerDependencies", "devDependencies"]
            .into_iter()
            .find_map(|section| package.get(section)?.get(dependency)?.as_str())
            .ok_or_else(|| {
                DocsError::InvalidConfig(format!(
                    "Wake docs requires {dependency} {MINIMUM_REACT_MAJOR}+ in dependencies, peerDependencies, or devDependencies"
                ))
            })?;
        let declared_majors: Vec<u64> = lower_bound
            .captures_iter(requirement)
            .filter_map(|capture| capture[1].parse().ok())
            .collect();
        if declared_majors.is_empty()
            || declared_majors
                .iter()
                .any(|major| *major < MINIMUM_REACT_MAJOR)
        {
            return Err(DocsError::InvalidConfig(format!(
                "Wake docs requires {dependency} {MINIMUM_REACT_MAJOR}+; found requirement '{requirement}'"
            )));
        }
    }
    Ok(())
}

fn validate_options(options: &DocsOptions) -> Result<(), DocsError> {
    if options.locale.trim().is_empty() {
        return Err(DocsError::InvalidConfig(
            "locale must not be empty".to_string(),
        ));
    }
    if !matches!(options.default_theme.as_str(), "light" | "dark" | "system") {
        return Err(DocsError::InvalidConfig(
            "default_theme must be light, dark, or system".to_string(),
        ));
    }
    if let Some(accent_color) = &options.accent_color {
        let color = Regex::new(r"^#[0-9a-fA-F]{6}$").expect("valid color regex");
        if !color.is_match(accent_color) {
            return Err(DocsError::InvalidConfig(
                "accent_color must be a six-digit hex color".to_string(),
            ));
        }
    }
    Ok(())
}

fn scan_files(
    directory: &Path,
    mdx_files: &mut Vec<PathBuf>,
    demo_files: &mut Vec<PathBuf>,
) -> Result<(), DocsError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| DocsError::Io(directory.to_path_buf(), error.to_string()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| DocsError::Io(directory.to_path_buf(), error.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            scan_files(&path, mdx_files, demo_files)?;
        } else {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if name.ends_with(".demo.tsx") {
                demo_files.push(path);
            } else if path.extension().and_then(|value| value.to_str()) == Some("mdx") {
                mdx_files.push(path);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationManifest {
    files: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct GenerationSnapshot {
    files: BTreeMap<PathBuf, Vec<u8>>,
}

#[derive(Debug, Default)]
struct RenderedFileTreeBuilder {
    files: OwnedFileTreeBuilder,
    manifest_files: BTreeSet<String>,
}

impl RenderedFileTreeBuilder {
    fn new() -> Self {
        Self::default()
    }

    fn manifest_files(&self) -> Vec<String> {
        self.manifest_files.iter().cloned().collect()
    }

    fn seal(self) -> OwnedFileTree {
        self.files.seal()
    }
}

trait GenerationTransactionOps {
    fn rename(&self, source: &Path, destination: &Path) -> std::io::Result<()>;
    fn remove_tree(&self, path: &Path) -> std::io::Result<()>;
}

struct HostGenerationTransactionOps;

impl GenerationTransactionOps for HostGenerationTransactionOps {
    fn rename(&self, source: &Path, destination: &Path) -> std::io::Result<()> {
        fs::rename(source, destination)
    }

    fn remove_tree(&self, path: &Path) -> std::io::Result<()> {
        fs::remove_dir_all(path)
    }
}

/// Acquires the persistent cross-process lock for one generated Docs namespace.
///
/// The lock file lives at the project `.wake` root, outside every replaceable `.wake/**/docs/generated`
/// directory. It is deliberately retained after publication: unlinking a locked file would let a
/// later process open a different inode and enter the transaction concurrently. Operating-system
/// file locks are released automatically if a publisher exits unexpectedly.
fn acquire_generation_commit_lock(lock_root: &Path) -> Result<fs::File, DocsError> {
    let lock_path = lock_root.join(GENERATION_COMMIT_LOCK_FILE);
    validate_generation_commit_lock_shape(&lock_path)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| DocsError::Io(lock_path.clone(), error.to_string()))?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => {
                validate_generation_commit_lock_shape(&lock_path)?;
                let opened = same_file::Handle::from_file(
                    file.try_clone()
                        .map_err(|error| DocsError::Io(lock_path.clone(), error.to_string()))?,
                )
                .map_err(|error| DocsError::Io(lock_path.clone(), error.to_string()))?;
                let named = same_file::Handle::from_path(&lock_path)
                    .map_err(|error| DocsError::Io(lock_path.clone(), error.to_string()))?;
                if opened != named {
                    return Err(DocsError::InvalidConfig(format!(
                        "generated Docs commit lock identity changed while acquiring it: {}",
                        lock_path.display()
                    )));
                }
                return Ok(file);
            }
            Err(error) => {
                let error: std::io::Error = error.into();
                if error.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(DocsError::Io(lock_path.clone(), error.to_string()));
                }
                let remaining = GENERATION_COMMIT_LOCK_TIMEOUT.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(DocsError::Io(
                        lock_path,
                        "timed out waiting for the generated Docs commit lock".to_string(),
                    ));
                }
                thread::sleep(remaining.min(GENERATION_COMMIT_LOCK_RETRY));
            }
        }
    }
}

fn validate_generation_commit_lock_shape(lock_path: &Path) -> Result<(), DocsError> {
    match fs::symlink_metadata(lock_path) {
        Ok(metadata) => {
            if metadata_is_link_or_reparse_point(&metadata) || !metadata.is_file() {
                return Err(DocsError::InvalidConfig(format!(
                    "generated Docs commit lock must be a physical regular file: {}",
                    lock_path.display()
                )));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DocsError::Io(lock_path.to_path_buf(), error.to_string())),
    }
}

fn insert_generation_file(
    generation: &mut RenderedFileTreeBuilder,
    relative: PathBuf,
    content: &[u8],
) -> Result<(), DocsError> {
    let name = generation_relative_name(&relative)?;
    let relative = projected_generation_path(&relative)?;
    generation
        .files
        .insert(relative, content.to_vec())
        .map_err(|error| {
            DocsError::InvalidConfig(format!(
                "generated Docs output inventory is invalid: {error}"
            ))
        })?;
    let inserted = generation.manifest_files.insert(name);
    debug_assert!(inserted, "owned tree accepted a duplicate path identity");
    Ok(())
}

fn projected_generation_path(relative: &Path) -> Result<ProjectedRelativePath, DocsError> {
    ProjectedRelativePath::new(relative).map_err(|error| {
        DocsError::InvalidConfig(format!(
            "generated Docs output contains an unsafe path: {error}"
        ))
    })
}

fn rendered_file_map(files: &OwnedFileTree) -> BTreeMap<PathBuf, Vec<u8>> {
    files
        .iter()
        .map(|(relative, content)| (relative.as_path().to_path_buf(), content.to_vec()))
        .collect()
}

fn generation_relative_name(relative: &Path) -> Result<String, DocsError> {
    if relative.as_os_str().is_empty() {
        return Err(DocsError::InvalidConfig(
            "generated Docs output path must not be empty".to_string(),
        ));
    }
    let mut segments = Vec::new();
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(DocsError::InvalidConfig(format!(
                "generated Docs output contains an unsafe path: {}",
                relative.display()
            )));
        };
        let segment = segment.to_str().ok_or_else(|| {
            DocsError::InvalidConfig(format!(
                "generated Docs output path is not valid UTF-8: {}",
                relative.display()
            ))
        })?;
        if segment.is_empty()
            || segment.contains(['/', '\\', ':'])
            || segment.ends_with('.')
            || segment.ends_with(' ')
        {
            return Err(DocsError::InvalidConfig(format!(
                "generated Docs output contains an unsafe path: {}",
                relative.display()
            )));
        }
        segments.push(segment);
    }
    Ok(segments.join("/"))
}

fn manifest_relative_path(value: &str) -> Result<PathBuf, DocsError> {
    if value.is_empty()
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains(['\\', ':'])
        || value
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(DocsError::InvalidConfig(format!(
            "generated Docs manifest contains an unsafe path: {value}"
        )));
    }
    let relative = value.split('/').collect::<PathBuf>();
    if generation_relative_name(&relative)? != value {
        return Err(DocsError::InvalidConfig(format!(
            "generated Docs manifest contains a non-canonical path: {value}"
        )));
    }
    Ok(relative)
}

fn publish_generation(
    project_root: &Path,
    generated_dir: &Path,
    generation: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<Vec<PathBuf>, DocsError> {
    publish_generation_with_ops(
        project_root,
        generated_dir,
        generation,
        &HostGenerationTransactionOps,
    )
}

fn publish_generation_with_ops(
    project_root: &Path,
    generated_dir: &Path,
    generation: &BTreeMap<PathBuf, Vec<u8>>,
    ops: &dyn GenerationTransactionOps,
) -> Result<Vec<PathBuf>, DocsError> {
    validate_generation_namespace(project_root, generated_dir)?;
    validate_generation_files(generated_dir, generation)?;
    let _transaction_guard = GENERATION_TRANSACTION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    validate_existing_generation_ancestors(project_root, generated_dir)?;
    let lock_root = project_root.join(".wake");
    ensure_physical_generation_directory(project_root, &lock_root)?;
    let _commit_lock = acquire_generation_commit_lock(&lock_root)?;
    // Lock acquisition is an observable filesystem operation. Revalidate the complete physical
    // namespace while holding the project-wide cross-process guard before creating a possibly
    // nested parent, or inspecting and replacing any accepted generation.
    validate_existing_generation_ancestors(project_root, generated_dir)?;
    let parent = generated_dir.parent().ok_or_else(|| {
        DocsError::InvalidConfig(format!(
            "generated Docs directory has no parent: {}",
            generated_dir.display()
        ))
    })?;
    ensure_physical_generation_directory(project_root, parent)?;
    let previous = inspect_generation_tree(generated_dir)?;
    let changed_files = generation_changes(
        generated_dir,
        previous.as_ref().map(|snapshot| &snapshot.files),
        generation,
    );
    if changed_files.is_empty() {
        return Ok(changed_files);
    }

    let stage = tempfile::Builder::new()
        .prefix(".wake-docs-next-")
        .tempdir_in(parent)
        .map_err(|error| DocsError::Io(parent.to_path_buf(), error.to_string()))?;
    for (relative, content) in generation {
        let output = stage.path().join(relative);
        let output_parent = output.parent().unwrap_or(stage.path());
        ensure_physical_generation_directory(stage.path(), output_parent)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .map_err(|error| DocsError::Io(output.clone(), error.to_string()))?;
        file.write_all(content)
            .and_then(|()| file.flush())
            .map_err(|error| DocsError::Io(output.clone(), error.to_string()))?;
    }
    let staged = inspect_generation_tree(stage.path())?.ok_or_else(|| {
        DocsError::InvalidConfig("generated Docs staging tree disappeared".to_string())
    })?;
    if staged.files != *generation {
        return Err(DocsError::InvalidConfig(
            "generated Docs staging tree does not match its render plan".to_string(),
        ));
    }

    let current = inspect_generation_tree(generated_dir)?;
    if current != previous {
        return Err(DocsError::InvalidConfig(format!(
            "generated Docs tree changed while its replacement was being staged: {}",
            generated_dir.display()
        )));
    }

    let backup = if previous.is_some() {
        let path = vacant_generation_sibling(parent, ".wake-docs-previous-")?;
        ops.rename(generated_dir, &path)
            .map_err(|error| DocsError::Io(generated_dir.to_path_buf(), error.to_string()))?;
        Some(path)
    } else {
        None
    };
    if let Err(install_error) = ops.rename(stage.path(), generated_dir) {
        if let Some(backup) = &backup
            && let Err(restore_error) = ops.rename(backup, generated_dir)
        {
            return Err(DocsError::Io(
                generated_dir.to_path_buf(),
                format!(
                    "failed to install generated Docs tree ({install_error}); failed to restore previous generation ({restore_error})"
                ),
            ));
        }
        return Err(DocsError::Io(
            generated_dir.to_path_buf(),
            install_error.to_string(),
        ));
    }

    // The target now owns the committed generation. Cleanup must never turn a successful commit
    // into an error observed by callers; a unique sibling can be reclaimed by later maintenance.
    if let Some(backup) = &backup {
        let _ = ops.remove_tree(backup);
    }
    Ok(changed_files)
}

fn vacant_generation_sibling(parent: &Path, prefix: &str) -> Result<PathBuf, DocsError> {
    let reservation = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(parent)
        .map_err(|error| DocsError::Io(parent.to_path_buf(), error.to_string()))?;
    let path = reservation.path().to_path_buf();
    reservation
        .close()
        .map_err(|error| DocsError::Io(path.clone(), error.to_string()))?;
    Ok(path)
}

fn validate_existing_generation_ancestors(
    project_root: &Path,
    generated_dir: &Path,
) -> Result<(), DocsError> {
    let relative = strip_generation_prefix(project_root, generated_dir).ok_or_else(|| {
        DocsError::InvalidConfig(format!(
            "generated Docs directory must stay inside the project root: {}",
            generated_dir.display()
        ))
    })?;
    let mut current = project_root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(DocsError::InvalidConfig(format!(
                "generated Docs directory contains an unsafe component: {}",
                generated_dir.display()
            )));
        };
        validate_generation_directory_component(component, generated_dir)?;
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata_is_link_or_reparse_point(&metadata) || !metadata.is_dir() {
                    return Err(DocsError::InvalidConfig(format!(
                        "generated Docs directory must not traverse a symbolic link, reparse point, or non-directory: {}",
                        current.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(DocsError::Io(current.clone(), error.to_string())),
        }
    }
    Ok(())
}

fn inspect_generation_tree(directory: &Path) -> Result<Option<GenerationSnapshot>, DocsError> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) => {
            if metadata_is_link_or_reparse_point(&metadata) || !metadata.is_dir() {
                return Err(DocsError::InvalidConfig(format!(
                    "generated Docs tree must be a physical directory: {}",
                    directory.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(DocsError::Io(directory.to_path_buf(), error.to_string())),
    }
    let is_empty = fs::read_dir(directory)
        .map_err(|error| DocsError::Io(directory.to_path_buf(), error.to_string()))?
        .next()
        .is_none();
    if is_empty {
        return Ok(Some(GenerationSnapshot {
            files: BTreeMap::new(),
        }));
    }
    let mut files = BTreeMap::new();
    collect_generation_files(directory, directory, &mut files)?;
    validate_generation_files(directory, &files)?;
    Ok(Some(GenerationSnapshot { files }))
}

fn collect_generation_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), DocsError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| DocsError::Io(directory.to_path_buf(), error.to_string()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| DocsError::Io(directory.to_path_buf(), error.to_string()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| DocsError::Io(path.clone(), error.to_string()))?;
        if metadata_is_link_or_reparse_point(&metadata) {
            return Err(DocsError::InvalidConfig(format!(
                "generated Docs tree must not contain a symbolic link or reparse point: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            collect_generation_files(root, &path, files)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(DocsError::InvalidConfig(format!(
                "generated Docs tree must contain only directories and regular files: {}",
                path.display()
            )));
        }
        if metadata_has_multiple_links(&path, &metadata)? {
            return Err(DocsError::InvalidConfig(format!(
                "generated Docs file must not have multiple hard links: {}",
                path.display()
            )));
        }
        let relative = path.strip_prefix(root).map_err(|_| {
            DocsError::InvalidConfig(format!(
                "generated Docs file escaped its generation tree: {}",
                path.display()
            ))
        })?;
        generation_relative_name(relative)?;
        let content =
            fs::read(&path).map_err(|error| DocsError::Io(path.clone(), error.to_string()))?;
        if files.insert(relative.to_path_buf(), content).is_some() {
            return Err(DocsError::InvalidConfig(format!(
                "generated Docs tree contains a duplicate file identity: {}",
                relative.display()
            )));
        }
    }
    Ok(())
}

fn validate_generation_files(
    directory: &Path,
    files: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), DocsError> {
    let mut physical_identities = BTreeMap::new();
    for relative in files.keys() {
        let name = generation_relative_name(relative)?;
        let identity = generation_path_identity(&name);
        if let Some(first) = physical_identities.insert(identity, name.clone()) {
            return Err(DocsError::InvalidConfig(format!(
                "generated Docs tree contains case-equivalent paths `{first}` and `{name}`"
            )));
        }
    }
    let manifest_path = Path::new("manifest.json");
    let manifest_bytes = files.get(manifest_path).ok_or_else(|| {
        DocsError::InvalidConfig(format!(
            "generated Docs tree is missing its manifest: {}",
            directory.join(manifest_path).display()
        ))
    })?;
    let manifest: GenerationManifest = serde_json::from_slice(manifest_bytes).map_err(|error| {
        DocsError::InvalidConfig(format!(
            "generated Docs manifest is invalid at {}: {error}",
            directory.join(manifest_path).display()
        ))
    })?;
    let mut previous: Option<&str> = None;
    let mut declared = BTreeSet::new();
    let mut declared_identities = BTreeMap::new();
    for value in &manifest.files {
        if previous.is_some_and(|previous| previous >= value.as_str()) {
            return Err(DocsError::InvalidConfig(
                "generated Docs manifest file entries must be sorted and unique".to_string(),
            ));
        }
        previous = Some(value);
        let relative = manifest_relative_path(value)?;
        if relative == manifest_path {
            return Err(DocsError::InvalidConfig(
                "generated Docs manifest must not list itself".to_string(),
            ));
        }
        let identity = generation_path_identity(value);
        if let Some(first) = declared_identities.insert(identity, value.clone()) {
            return Err(DocsError::InvalidConfig(format!(
                "generated Docs manifest contains case-equivalent paths `{first}` and `{value}`"
            )));
        }
        declared.insert(relative);
    }
    let actual = files
        .keys()
        .filter(|relative| relative.as_path() != manifest_path)
        .cloned()
        .collect::<BTreeSet<_>>();
    if actual != declared {
        return Err(DocsError::InvalidConfig(format!(
            "generated Docs manifest does not exactly describe the physical file set at {}",
            directory.display()
        )));
    }
    Ok(())
}

fn generation_path_identity(value: &str) -> String {
    value.chars().flat_map(char::to_lowercase).collect()
}

fn generation_changes(
    generated_dir: &Path,
    previous: Option<&BTreeMap<PathBuf, Vec<u8>>>,
    next: &BTreeMap<PathBuf, Vec<u8>>,
) -> Vec<PathBuf> {
    let mut paths = previous
        .into_iter()
        .flat_map(BTreeMap::keys)
        .chain(next.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    paths.retain(|relative| previous.and_then(|files| files.get(relative)) != next.get(relative));
    paths
        .into_iter()
        .map(|relative| generated_dir.join(relative))
        .collect()
}

fn ensure_physical_generation_directory(root: &Path, directory: &Path) -> Result<(), DocsError> {
    let relative = strip_generation_prefix(root, directory).ok_or_else(|| {
        DocsError::InvalidConfig(format!(
            "generated Docs directory must stay inside the project root: {}",
            directory.display()
        ))
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(DocsError::InvalidConfig(format!(
                "generated Docs directory contains an unsafe component: {}",
                directory.display()
            )));
        };
        validate_generation_directory_component(component, directory)?;
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata_is_link_or_reparse_point(&metadata) || !metadata.is_dir() {
                    return Err(DocsError::InvalidConfig(format!(
                        "generated Docs directory must not traverse a symbolic link or reparse point: {}",
                        current.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let metadata = fs::symlink_metadata(&current)
                            .map_err(|error| DocsError::Io(current.clone(), error.to_string()))?;
                        if metadata_is_link_or_reparse_point(&metadata) || !metadata.is_dir() {
                            return Err(DocsError::InvalidConfig(format!(
                                "generated Docs directory creation raced with a symbolic link, reparse point, or non-directory: {}",
                                current.display()
                            )));
                        }
                    }
                    Err(error) => {
                        return Err(DocsError::Io(current.clone(), error.to_string()));
                    }
                }
            }
            Err(error) => return Err(DocsError::Io(current.clone(), error.to_string())),
        }
    }
    Ok(())
}

fn validate_generation_namespace(root: &Path, directory: &Path) -> Result<(), DocsError> {
    let relative = strip_generation_prefix(root, directory).ok_or_else(|| {
        DocsError::InvalidConfig(format!(
            "generated Docs directory must stay inside the project root: {}",
            directory.display()
        ))
    })?;
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(component) => {
                validate_generation_directory_component(component, directory)?;
                Ok(component)
            }
            _ => Err(DocsError::InvalidConfig(format!(
                "generated Docs directory contains an unsafe component: {}",
                directory.display()
            ))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let owns_namespace = components
        .first()
        .is_some_and(|value| generation_component_eq(value, OsStr::new(".wake")))
        && components.len() >= 3
        && generation_component_eq(components[components.len() - 2], OsStr::new("docs"))
        && generation_component_eq(components[components.len() - 1], OsStr::new("generated"))
        && components
            .windows(2)
            .filter(|pair| {
                generation_component_eq(pair[0], OsStr::new("docs"))
                    && generation_component_eq(pair[1], OsStr::new("generated"))
            })
            .count()
            == 1;
    if !owns_namespace {
        return Err(DocsError::InvalidConfig(format!(
            "generated Docs directory must be one non-nested Wake-owned `.wake/**/docs/generated` path: {}",
            directory.display()
        )));
    }
    Ok(())
}

fn strip_generation_prefix(root: &Path, path: &Path) -> Option<PathBuf> {
    if let Ok(relative) = path.strip_prefix(root) {
        return Some(relative.to_path_buf());
    }
    #[cfg(windows)]
    {
        let root_components = root.components().collect::<Vec<_>>();
        let path_components = path.components().collect::<Vec<_>>();
        if root_components.len() > path_components.len()
            || !root_components.iter().zip(&path_components).all(
                |(root_component, path_component)| {
                    generation_component_eq(root_component.as_os_str(), path_component.as_os_str())
                },
            )
        {
            return None;
        }
        Some(
            path_components[root_components.len()..]
                .iter()
                .map(|component| component.as_os_str())
                .collect(),
        )
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn generation_component_eq(left: &OsStr, right: &OsStr) -> bool {
    if left == right {
        return true;
    }
    #[cfg(windows)]
    {
        left.to_str()
            .zip(right.to_str())
            .is_some_and(|(left, right)| {
                windows_component_identity(left) == windows_component_identity(right)
            })
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
fn windows_component_identity(value: &str) -> String {
    let value = value.strip_prefix(r"\\?\").unwrap_or(value);
    let value = value.strip_prefix("UNC\\").unwrap_or(value);
    value
        .trim_start_matches(['\\', '/'])
        .chars()
        .flat_map(char::to_lowercase)
        .collect()
}

fn validate_generation_directory_component(
    component: &OsStr,
    directory: &Path,
) -> Result<(), DocsError> {
    let component = component.to_str().ok_or_else(|| {
        DocsError::InvalidConfig(format!(
            "generated Docs directory is not valid UTF-8: {}",
            directory.display()
        ))
    })?;
    if component.contains(['/', '\\', ':']) || component.ends_with('.') || component.ends_with(' ')
    {
        return Err(DocsError::InvalidConfig(format!(
            "generated Docs directory contains a non-portable component: {}",
            directory.display()
        )));
    }
    Ok(())
}

fn metadata_is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn metadata_has_multiple_links(path: &Path, metadata: &fs::Metadata) -> Result<bool, DocsError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let _ = path;
        Ok(metadata.nlink() > 1)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };

        let _ = metadata;
        let file = fs::File::open(path)
            .map_err(|error| DocsError::Io(path.to_path_buf(), error.to_string()))?;
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: the handle stays open for the call and `information` points to a valid,
        // writable `BY_HANDLE_FILE_INFORMATION` value.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
            return Err(DocsError::Io(
                path.to_path_buf(),
                std::io::Error::last_os_error().to_string(),
            ));
        }
        Ok(information.nNumberOfLinks > 1)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        let _ = metadata;
        Ok(false)
    }
}

fn atomic_write_if_changed(path: &Path, content: &[u8]) -> Result<bool, DocsError> {
    let _write_guard = ATOMIC_WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let changed = !fs::read(path).is_ok_and(|current| current == content);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| DocsError::Io(parent.to_path_buf(), error.to_string()))?;
    }

    let operation_id = NEXT_ATOMIC_WRITE_ID.fetch_add(1, Ordering::Relaxed);
    let sibling = |marker: &str| {
        path.with_extension(format!(
            "{}.{marker}-{}-{operation_id}",
            path.extension()
                .and_then(|value| value.to_str())
                .unwrap_or("tmp"),
            std::process::id()
        ))
    };
    let temporary = sibling("wake-next");
    fs::write(&temporary, content)
        .map_err(|error| DocsError::Io(temporary.clone(), error.to_string()))?;

    if path.exists() {
        let backup = sibling("wake-previous");
        if backup.exists() {
            fs::remove_file(&backup)
                .map_err(|error| DocsError::Io(backup.clone(), error.to_string()))?;
        }
        if let Err(error) = fs::rename(path, &backup) {
            let _ = fs::remove_file(&temporary);
            return Err(DocsError::Io(path.to_path_buf(), error.to_string()));
        }
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::rename(&backup, path);
            let _ = fs::remove_file(&temporary);
            return Err(DocsError::Io(path.to_path_buf(), error.to_string()));
        }
        fs::remove_file(&backup).map_err(|error| DocsError::Io(backup, error.to_string()))?;
    } else if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(DocsError::Io(path.to_path_buf(), error.to_string()));
    }
    Ok(changed)
}

/// Copy public after bundling, rejecting collisions with generated output.
pub fn copy_public_assets(root: &Path, outdir: &Path) -> Result<(), DocsError> {
    let public = root.join("public");
    if public.is_dir() {
        copy_public_directory(&public, &public, outdir)?;
    }
    Ok(())
}

fn copy_public_directory(base: &Path, directory: &Path, outdir: &Path) -> Result<(), DocsError> {
    for entry in fs::read_dir(directory)
        .map_err(|error| DocsError::Io(directory.to_path_buf(), error.to_string()))?
    {
        let entry =
            entry.map_err(|error| DocsError::Io(directory.to_path_buf(), error.to_string()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| DocsError::Io(path.clone(), error.to_string()))?;
        if metadata.file_type().is_symlink() {
            return Err(DocsError::InvalidConfig(format!(
                "public assets must not contain symbolic links: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            copy_public_directory(base, &path, outdir)?;
        } else if metadata.is_file() {
            let destination = outdir.join(path.strip_prefix(base).expect("public descendant"));
            if destination.exists() {
                return Err(DocsError::PublicCollision(destination));
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| DocsError::Io(parent.to_path_buf(), error.to_string()))?;
            }
            fs::copy(&path, &destination)
                .map_err(|error| DocsError::Io(destination, error.to_string()))?;
        } else {
            return Err(DocsError::InvalidConfig(format!(
                "unsupported public asset entry: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Replicate route-aware HTML shells for direct static-host access to every page.
pub fn write_route_shells(
    outdir: &Path,
    routes: &[RouteInfo],
    index_html: &str,
    site_title: &str,
    site_description: &str,
    locale: &str,
) -> Result<(), DocsError> {
    let root_route = routes.iter().find(|route| route.slug == "/");
    let root_title = root_route
        .map(|route| route_page_title(site_title, &route.title))
        .unwrap_or_else(|| site_title.to_string());
    let root_description = root_route
        .map(|route| route.description.as_str())
        .filter(|description| !description.is_empty())
        .unwrap_or(site_description);
    let root_html = route_html(index_html, &root_title, root_description, locale);
    atomic_write_if_changed(&outdir.join("index.html"), root_html.as_bytes())?;

    let not_found_description = if locale.to_ascii_lowercase().starts_with("zh") {
        "找不到请求的文档页面。"
    } else {
        "The requested documentation page could not be found."
    };
    let not_found_html = route_html(
        index_html,
        &format!("404 · {site_title}"),
        not_found_description,
        locale,
    );
    atomic_write_if_changed(&outdir.join("404.html"), not_found_html.as_bytes())?;

    for route in routes {
        let route_path = RoutePath::from_canonical_encoded(&route.slug)?;
        if route_path.encoded == "/" {
            continue;
        }
        let description = if route.description.is_empty() {
            site_description
        } else {
            &route.description
        };
        let html = route_html(
            index_html,
            &route_page_title(site_title, &route.title),
            description,
            locale,
        );
        atomic_write_if_changed(
            &outdir
                .join(route_path.decoded_relative_path())
                .join("index.html"),
            html.as_bytes(),
        )?;
    }
    Ok(())
}

fn route_page_title(site_title: &str, page_title: &str) -> String {
    if page_title.trim().is_empty() || page_title == site_title {
        site_title.to_string()
    } else {
        format!("{page_title} · {site_title}")
    }
}

fn route_html(index_html: &str, title: &str, description: &str, locale: &str) -> String {
    let mut html = index_html.to_string();
    let escaped_title = escape_html(title);
    if let Some(start) = html.find("<title>") {
        if let Some(relative_end) = html[start + 7..].find("</title>") {
            let end = start + 7 + relative_end + 8;
            html.replace_range(start..end, &format!("<title>{escaped_title}</title>"));
        }
    } else if let Some(head_end) = html.find("</head>") {
        html.insert_str(head_end, &format!("<title>{escaped_title}</title>\n"));
    }

    let metadata = format!(
        "<meta name=\"description\" content=\"{}\">\n",
        escape_html(description)
    );
    if let Some(head_end) = html.find("</head>") {
        html.insert_str(head_end, &metadata);
    }

    if let Some(start) = html.find("<html")
        && let Some(relative_end) = html[start..].find('>')
    {
        let end = start + relative_end + 1;
        let tag = &html[start..end];
        let language = format!(" lang=\"{}\"", escape_html(locale));
        let language_pattern =
            Regex::new(r#"(?i)\s+lang\s*=\s*(?:\"[^\"]*\"|'[^']*')"#).expect("valid lang regex");
        let updated = if language_pattern.is_match(tag) {
            language_pattern.replace(tag, language).into_owned()
        } else {
            tag.replacen("<html", &format!("<html{language}"), 1)
        };
        html.replace_range(start..end, &updated);
    }
    html
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
}

fn visit(node: &Node, visitor: &mut impl FnMut(&Node)) {
    visitor(node);
    if let Some(children) = node.children() {
        for child in children {
            visit(child, visitor);
        }
    }
}

fn literal_attribute(attributes: &[AttributeContent], name: &str) -> Option<String> {
    attributes.iter().find_map(|attribute| match attribute {
        AttributeContent::Property(property) if property.name == name => {
            static_string(property.value.as_ref())
        }
        _ => None,
    })
}

fn static_string(value: Option<&AttributeValue>) -> Option<String> {
    match value {
        Some(AttributeValue::Literal(value)) => Some(value.clone()),
        Some(AttributeValue::Expression(value)) => {
            let value = value.value.trim();
            ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
            .then(|| value[1..value.len() - 1].to_string())
        }
        None => None,
    }
}

fn is_static_value(value: Option<&AttributeValue>) -> bool {
    match value {
        None | Some(AttributeValue::Literal(_)) => true,
        Some(AttributeValue::Expression(value)) => {
            let value = value.value.trim();
            let quoted = (value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\''));
            quoted
                || matches!(value, "true" | "false" | "null" | "undefined")
                || value.parse::<f64>().is_ok()
        }
    }
}

fn normalize_base(value: &str) -> String {
    if value.trim().is_empty() || value == "/" {
        "/".to_string()
    } else {
        format!("/{}/", value.trim_matches('/'))
    }
}

fn slugify(value: &str) -> String {
    let mut result = String::new();
    let mut dash = false;
    for ch in value.trim().chars() {
        if ch.is_alphanumeric() || ch == '_' {
            if dash && !result.is_empty() {
                result.push('-');
            }
            dash = false;
            result.extend(ch.to_lowercase());
        } else {
            dash = true;
        }
    }
    if result.is_empty() {
        "section".to_string()
    } else {
        result
    }
}

fn title_case(value: &str) -> String {
    value
        .split(['-', '_', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or(String::new(), |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn js_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization")
}
fn js_braced(value: &str) -> String {
    format!("{{{}}}", js_string(value))
}
fn slash_path(path: impl AsRef<Path>) -> String {
    let value = path.as_ref().to_string_lossy().replace('\\', "/");
    if let Some(rest) = value.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = value.strip_prefix("//?/") {
        rest.to_string()
    } else {
        value
    }
}

fn root_relative_alias(root: &Path, path: &Path) -> Result<String, DocsError> {
    let normalized = normalize_path(path);
    let relative = normalized.strip_prefix(root).map_err(|_| {
        DocsError::InvalidConfig(format!("`{}` must be inside project root", path.display()))
    })?;
    Ok(format!("@@wake/docs-project/{}", slash_path(relative)))
}

fn absolute_from(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&root.join(path))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    result
}

fn canonical_dir(path: &Path) -> Result<PathBuf, DocsError> {
    fs::canonicalize(path).map_err(|error| DocsError::Io(path.to_path_buf(), error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct DecodedMapping {
        generated_column: i64,
        original: Option<(i64, i64)>,
    }

    fn decode_test_mappings(value: &str) -> Vec<Vec<DecodedMapping>> {
        let mut previous_source = 0;
        let mut previous_line = 0;
        let mut previous_column = 0;
        value
            .split(';')
            .map(|line| {
                let mut generated_column = 0;
                line.split(',')
                    .filter(|segment| !segment.is_empty())
                    .map(|segment| {
                        let values = decode_test_segment(segment);
                        generated_column += values[0];
                        let original = if values.len() >= 4 {
                            previous_source += values[1];
                            previous_line += values[2];
                            previous_column += values[3];
                            assert_eq!(previous_source, 0, "Docs pages have one source");
                            Some((previous_line, previous_column))
                        } else {
                            None
                        };
                        DecodedMapping {
                            generated_column,
                            original,
                        }
                    })
                    .collect()
            })
            .collect()
    }

    fn decode_test_segment(segment: &str) -> Vec<i64> {
        const BASE64: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes = segment.as_bytes();
        let mut index = 0;
        let mut values = Vec::new();
        while index < bytes.len() {
            let mut shift = 0;
            let mut encoded = 0u64;
            loop {
                let digit = BASE64
                    .as_bytes()
                    .iter()
                    .position(|candidate| *candidate == bytes[index])
                    .expect("valid base64 VLQ") as u64;
                index += 1;
                encoded |= (digit & 31) << shift;
                shift += 5;
                if digit & 32 == 0 {
                    break;
                }
            }
            let magnitude = (encoded >> 1) as i64;
            values.push(if encoded & 1 == 1 {
                -magnitude
            } else {
                magnitude
            });
        }
        values
    }

    #[test]
    fn component_preview_scopes_configured_accent_without_mutating_document_root() {
        assert!(
            RUNTIME_STYLE.contains(".workbench-shell { --wake-accent: var(--workbench-accent); }")
        );
        assert!(
            !RUNTIME_STYLE.contains(":root:has(.demo-frame-root)"),
            "Demo 不能在文档根节点重写语义颜色"
        );
        let demo_scope = RUNTIME_STYLE
            .split_once(".demo-frame-root {")
            .expect("Demo token scope")
            .1
            .split_once('}')
            .expect("Demo token scope end")
            .0;
        assert!(
            !demo_scope.contains("--wake-accent:"),
            "Demo 必须继承配置的 --wake-accent，不能重置为工作台中性色"
        );
        for expected in [
            "--token-semantic-color-background-hover-subtle: color-mix(in srgb, var(--wake-accent)",
            "--token-semantic-color-background-active-subtle: color-mix(in srgb, var(--wake-accent)",
            "--token-semantic-color-border-focus: var(--wake-accent);",
            "--token-semantic-color-brand-primary: var(--wake-accent);",
            "--token-semantic-color-brand-primary-hover: color-mix(in srgb, var(--wake-accent)",
            "--token-semantic-color-brand-primary-active: color-mix(in srgb, var(--wake-accent)",
            "--token-semantic-color-text-link: var(--wake-accent);",
            "--token-semantic-color-text-link-hover: color-mix(in srgb, var(--wake-accent)",
            "--token-semantic-shadow-focus-ring: 0 0 0 3px color-mix(in srgb, var(--wake-accent)",
        ] {
            assert!(
                demo_scope.contains(expected),
                "缺少 Demo 强调色映射: {expected}"
            );
        }
        for runtime in [RUNTIME_APP, RUNTIME_COMPONENTS] {
            assert!(
                runtime.contains("if (siteConfig.accentColor)"),
                "Docs runtime 必须只在显式配置时写入强调色"
            );
        }
    }

    #[test]
    fn embedded_components_runtime_only_renders_the_preview_surface() {
        assert!(RUNTIME_COMPONENTS.contains("siteConfig.presentation === \"embedded\""));
        assert!(RUNTIME_COMPONENTS.contains("workbench-embedded-preview"));
        assert!(RUNTIME_COMPONENT_STYLE.contains(".workbench-embedded-preview iframe"));
    }

    #[test]
    fn slash_paths_strip_windows_verbatim_prefixes() {
        assert_eq!(
            slash_path(Path::new(r"\\?\C:\proj\docs\index.mdx")),
            "C:/proj/docs/index.mdx"
        );
        assert_eq!(
            slash_path(Path::new(r"\\?\UNC\server\share\docs\index.mdx")),
            "//server/share/docs/index.mdx"
        );
    }

    #[test]
    fn concurrent_atomic_writes_do_not_share_staging_files() {
        let output = fixture().join(".wake/docs/generated/pages/shared.tsx");
        let workers = 16;
        let barrier = Arc::new(Barrier::new(workers));
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let output = output.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    for round in 0..16 {
                        let content = format!("worker={worker};round={round}");
                        atomic_write_if_changed(&output, content.as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let content = fs::read_to_string(&output).unwrap();
        assert!(content.starts_with("worker="), "{content}");
        let leftovers: Vec<_> = fs::read_dir(output.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name != "shared.tsx")
            .collect();
        assert!(leftovers.is_empty(), "staging files leaked: {leftovers:?}");
    }

    fn fixture() -> PathBuf {
        let root = loop {
            let id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let candidate =
                std::env::temp_dir().join(format!("wake-docs-{}-{id}", std::process::id()));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("fixture root: {error}"),
            }
        };
        fs::create_dir_all(root.join("docs/demos")).expect("fixture docs");
        fs::create_dir_all(root.join("src")).expect("fixture src");
        fs::write(
            root.join("package.json"),
            r#"{"dependencies":{"react":"^19.0.0","react-dom":"^19.0.0"}}"#,
        )
        .expect("fixture package");
        fs::write(root.join("src/button.tsx"), "export interface ButtonProps { /** Label. */ label: string; } export function Button({ label = \"Save\" }: ButtonProps) { return label; }").expect("props source");
        fs::write(root.join("docs/demos/basic.demo.tsx"), "export const meta = { title: \"Basic\" }; export default function Demo() { return <button>OK</button>; }").expect("demo");
        root
    }

    fn write_fixture_navigation(root: &Path) {
        let docs = root.join("docs");
        let mut mdx_files = Vec::new();
        let mut demo_files = Vec::new();
        scan_files(&docs, &mut mdx_files, &mut demo_files).expect("scan fixture pages");
        let mut pages = mdx_files
            .into_iter()
            .map(|path| {
                slash_path(
                    path.strip_prefix(&docs)
                        .expect("fixture page beneath docs")
                        .with_extension(""),
                )
            })
            .collect::<Vec<_>>();
        pages.sort();
        let pages = pages
            .iter()
            .map(|page| format!("\"{page}\""))
            .collect::<Vec<_>>()
            .join(", ");
        fs::write(
            docs.join("navigation.toml"),
            format!("[[group]]\nid = \"test\"\ntitle = \"Test\"\npages = [{pages}]\n"),
        )
        .expect("fixture navigation");
    }

    fn generate_fixture(
        root: &Path,
        options: &DocsOptions,
        mode: BuildMode,
    ) -> Result<GeneratedProject, DocsError> {
        write_fixture_navigation(root);
        generate(root, options, mode)
    }

    fn raw_file_snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn collect(root: &Path, directory: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
            let mut entries = fs::read_dir(directory)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            entries.sort_by_key(fs::DirEntry::file_name);
            for entry in entries {
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path).unwrap();
                if metadata.is_dir() && !metadata_is_link_or_reparse_point(&metadata) {
                    collect(root, &path, files);
                } else if metadata.is_file() {
                    files.insert(
                        slash_path(path.strip_prefix(root).unwrap()),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }

        let mut files = BTreeMap::new();
        collect(root, root, &mut files);
        files
    }

    fn rendered_file_snapshot(rendered: &RenderedProject) -> BTreeMap<String, Vec<u8>> {
        rendered
            .files
            .iter()
            .map(|(relative, content)| (slash_path(relative.as_path()), content.to_vec()))
            .collect()
    }

    #[test]
    fn render_is_independent_of_the_physical_generation_namespace() {
        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        write_fixture_navigation(&root);
        let wake_dir = root.join(".wake");

        let first = render_with_mode(
            &root,
            &DocsOptions::default(),
            BuildMode::Development,
            DocsMode::Site,
        )
        .unwrap();
        assert!(
            !wake_dir.exists(),
            "pure render created the publication root"
        );
        assert!(!first.files.is_empty());

        let generated_dir = wake_dir.join("docs/generated");
        fs::create_dir_all(&generated_dir).unwrap();
        fs::write(generated_dir.join("sentinel.txt"), "untouched").unwrap();
        fs::write(
            generated_dir.join("manifest.json"),
            b"not a valid generation manifest",
        )
        .unwrap();
        let before = raw_file_snapshot(&generated_dir);
        let second = render_with_mode(
            &root,
            &DocsOptions::default(),
            BuildMode::Development,
            DocsMode::Site,
        )
        .unwrap();

        assert_eq!(
            rendered_file_snapshot(&second),
            rendered_file_snapshot(&first)
        );
        assert_eq!(raw_file_snapshot(&generated_dir), before);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rendered_project_matches_legacy_physical_generation_and_metadata() {
        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n\nBody.\n").unwrap();
        write_fixture_navigation(&root);
        let rendered = render_with_mode(
            &root,
            &DocsOptions::default(),
            BuildMode::Development,
            DocsMode::Site,
        )
        .unwrap();
        let generated = generate_with_mode(
            &root,
            &DocsOptions::default(),
            BuildMode::Development,
            DocsMode::Site,
        )
        .unwrap();

        assert_eq!(
            raw_file_snapshot(&generated.generated_dir),
            rendered_file_snapshot(&rendered)
        );
        assert_eq!(generated.root, rendered.root);
        assert_eq!(
            generated.entry,
            generated
                .generated_dir
                .join(rendered.entry_relative.as_path())
        );
        assert_eq!(generated.watch_roots, rendered.watch_roots);
        assert_eq!(generated.routes, rendered.routes);
        assert_eq!(generated.mode, rendered.mode);
        assert_eq!(generated.demos, rendered.demos);
        assert_eq!(generated.warnings, rendered.warnings);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_render_leaves_the_previous_physical_generation_untouched() {
        let root = fixture();
        let page = root.join("docs/index.mdx");
        fs::write(&page, "# Accepted\n").unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let before = raw_file_snapshot(&generated.generated_dir);

        fs::write(&page, "+++\ntitle = [\n+++\n# Broken\n").unwrap();
        let error = render_with_mode(
            &root,
            &DocsOptions::default(),
            BuildMode::Development,
            DocsMode::Site,
        )
        .unwrap_err();

        assert!(matches!(error, DocsError::Frontmatter(_, _)), "{error}");
        assert_eq!(raw_file_snapshot(&generated.generated_dir), before);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn custom_generation_directory_rejects_user_and_external_trees_before_writing() {
        let root = fixture();
        write_fixture_navigation(&root);
        let sentinel = root.join("src/sentinel.txt");
        fs::write(&sentinel, "keep").unwrap();
        let external = root.with_extension("outside");
        let _ = fs::remove_dir_all(&external);

        for generated_dir in [&root, &root.join("src"), &external] {
            let error = generate_with_mode_in(
                &root,
                generated_dir,
                &DocsOptions::default(),
                BuildMode::Development,
                DocsMode::Site,
            )
            .unwrap_err();
            assert!(matches!(error, DocsError::InvalidConfig(_)), "{error}");
            assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
            assert!(!generated_dir.join("registry.ts").exists());
        }

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(external);
    }

    #[test]
    fn nested_generated_docs_namespace_is_rejected_before_writing() {
        let root = fixture();
        let outer = root.join(".wake/docs/generated");
        let nested = outer.join("candidate/docs/generated");

        let error = generate_with_mode_in(
            &root,
            &nested,
            &DocsOptions::default(),
            BuildMode::Development,
            DocsMode::Site,
        )
        .unwrap_err();

        assert!(matches!(error, DocsError::InvalidConfig(_)), "{error}");
        assert!(!outer.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_first_generation_directory_creation_is_idempotent() {
        let root = fixture();
        let wake_root = root.join(".wake");
        let barrier = Arc::new(Barrier::new(16));
        let workers = (0..16)
            .map(|_| {
                let root = root.clone();
                let wake_root = wake_root.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    ensure_physical_generation_directory(&root, &wake_root)
                })
            })
            .collect::<Vec<_>>();

        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        let metadata = fs::symlink_metadata(&wake_root).unwrap();
        assert!(metadata.is_dir());
        assert!(!metadata_is_link_or_reparse_point(&metadata));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_generation_validates_the_target_before_rendering_sources() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            "+++\ntitle = [\n+++\n# Broken\n",
        )
        .unwrap();

        let error = generate_with_mode_in(
            &root,
            root.join("src"),
            &DocsOptions::default(),
            BuildMode::Development,
            DocsMode::Site,
        )
        .unwrap_err();

        assert!(matches!(error, DocsError::InvalidConfig(_)), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn generation_namespace_uses_windows_path_identity_without_allowing_ads() {
        let root = Path::new(r"C:\Project");
        assert!(
            validate_generation_namespace(
                root,
                Path::new(r"\\?\c:\PROJECT\.WAKE\candidate\DOCS\GENERATED"),
            )
            .is_ok()
        );
        assert!(
            validate_generation_namespace(
                root,
                Path::new(r"C:\Project\.wake\candidate:stream\docs\generated"),
            )
            .is_err()
        );
        assert!(
            validate_generation_namespace(
                root,
                Path::new(r"C:\Project-other\.wake\docs\generated"),
            )
            .is_err()
        );
    }

    #[test]
    fn invalid_manifest_leaves_the_previous_generation_byte_identical() {
        let root = fixture();
        let generated = root.join(".wake/docs/generated");
        fs::create_dir_all(&generated).unwrap();
        let stale = generated.join("stale.ts");
        fs::write(&stale, "keep until validation succeeds").unwrap();
        fs::write(
            generated.join("manifest.json"),
            r#"{"files":["stale.ts","../../outside.txt"]}"#,
        )
        .unwrap();
        let before = raw_file_snapshot(&generated);

        let error =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(matches!(error, DocsError::InvalidConfig(_)), "{error}");
        assert_eq!(raw_file_snapshot(&generated), before);
        assert_eq!(
            fs::read_to_string(stale).unwrap(),
            "keep until validation succeeds"
        );
        let parent_entries = fs::read_dir(generated.parent().unwrap())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let parent_names = parent_entries
            .iter()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            parent_names,
            BTreeSet::from(["generated".to_string()]),
            "unexpected generated Docs transaction sibling"
        );
        assert!(
            root.join(".wake")
                .join(GENERATION_COMMIT_LOCK_FILE)
                .is_file()
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn empty_precreated_generation_directory_is_a_compatible_placeholder() {
        let root = fixture();
        let generated = root.join(".wake/docs/generated");
        fs::create_dir_all(&generated).unwrap();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();

        let result =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert_eq!(
            result.generated_dir,
            wake_common::fs::resolve_existing_prefix(&generated)
        );
        assert!(generated.join("manifest.json").is_file());
        assert!(generated.join("pages/index.tsx").is_file());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn manifest_schema_order_paths_and_physical_set_are_strict() {
        let directory = Path::new(".wake/docs/generated");
        let invalid = [
            (
                BTreeMap::from([(
                    PathBuf::from("manifest.json"),
                    br#"{"files":[],"owner":"user"}"#.to_vec(),
                )]),
                "unknown field",
            ),
            (
                BTreeMap::from([
                    (PathBuf::from("a.ts"), b"a".to_vec()),
                    (
                        PathBuf::from("manifest.json"),
                        br#"{"files":["z.ts","a.ts"]}"#.to_vec(),
                    ),
                    (PathBuf::from("z.ts"), b"z".to_vec()),
                ]),
                "sorted and unique",
            ),
            (
                BTreeMap::from([
                    (PathBuf::from("a.ts"), b"a".to_vec()),
                    (
                        PathBuf::from("manifest.json"),
                        br#"{"files":["a.ts","a.ts"]}"#.to_vec(),
                    ),
                ]),
                "sorted and unique",
            ),
            (
                BTreeMap::from([
                    (PathBuf::from("A.ts"), b"A".to_vec()),
                    (PathBuf::from("a.ts"), b"a".to_vec()),
                    (
                        PathBuf::from("manifest.json"),
                        br#"{"files":["A.ts","a.ts"]}"#.to_vec(),
                    ),
                ]),
                "case-equivalent",
            ),
            (
                BTreeMap::from([(
                    PathBuf::from("manifest.json"),
                    br#"{"files":["stream.ts:payload"]}"#.to_vec(),
                )]),
                "unsafe path",
            ),
        ];
        for (files, expected) in invalid {
            let error = validate_generation_files(directory, &files).unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "expected `{expected}` diagnostic, got: {error}"
            );
        }

        let extra = BTreeMap::from([
            (PathBuf::from("extra.ts"), b"extra".to_vec()),
            (PathBuf::from("manifest.json"), br#"{"files":[]}"#.to_vec()),
        ]);
        assert!(validate_generation_files(directory, &extra).is_err());
    }

    #[cfg(unix)]
    fn generated_test_file_identity(path: &Path) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = fs::metadata(path).unwrap();
        (metadata.dev(), metadata.ino())
    }

    #[cfg(windows)]
    fn generated_test_file_identity(path: &Path) -> (u32, u64) {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };

        let file = fs::File::open(path).unwrap();
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: the fixture handle stays open and the output pointer is valid for the call.
        assert_ne!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) },
            0
        );
        (
            information.dwVolumeSerialNumber,
            (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        )
    }

    #[test]
    fn identical_generation_is_a_true_no_op_without_a_directory_swap() {
        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        let first =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let tracked = first.generated_dir.join("registry.ts");
        let identity = generated_test_file_identity(&tracked);
        let second =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();

        assert!(second.changed_files.is_empty());
        assert_eq!(generated_test_file_identity(&tracked), identity);
        assert_eq!(second.generated_dir, first.generated_dir);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn publishing_a_new_generation_retires_stale_files_and_reports_logical_paths() {
        let root = fixture();
        let stale_source = root.join("docs/stale.mdx");
        fs::write(&stale_source, "# Stale\n").unwrap();
        let first =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let stale_module = first.generated_dir.join("pages/stale.tsx");
        let stale_map = first.generated_dir.join("pages/stale.tsx.map");
        assert!(stale_module.is_file());
        assert!(stale_map.is_file());

        fs::remove_file(stale_source).unwrap();
        let second =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert!(!stale_module.exists());
        assert!(!stale_map.exists());
        assert!(second.changed_files.contains(&stale_module));
        assert!(second.changed_files.contains(&stale_map));
        assert!(
            second
                .changed_files
                .iter()
                .all(|path| path.starts_with(&first.generated_dir)),
            "physical transaction names leaked into changed_files: {:?}",
            second.changed_files
        );
        let manifest = fs::read_to_string(first.generated_dir.join("manifest.json")).unwrap();
        assert!(!manifest.contains("stale"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn generated_docs_publication_waits_for_a_separate_process_commit_lock() {
        const PARENT_ENV: &str = "WAKE_TEST_DOCS_GENERATION_LOCK_PARENT";
        const READY_ENV: &str = "WAKE_TEST_DOCS_GENERATION_LOCK_READY";
        const RELEASE_ENV: &str = "WAKE_TEST_DOCS_GENERATION_LOCK_RELEASE";

        let root = fixture();
        let source = root.join("docs/index.mdx");
        fs::write(&source, "# Before\n").unwrap();
        let first =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let lock_root = root.join(".wake");
        let ready = root.join("generation-lock-ready");
        let release = root.join("generation-lock-release");

        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "tests::generation_commit_lock_process_helper",
            ])
            .env(PARENT_ENV, &lock_root)
            .env(READY_ENV, &ready)
            .env(RELEASE_ENV, &release)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.is_file() && Instant::now() < deadline {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("generated Docs lock helper exited before acquiring its lock: {status}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        if !ready.is_file() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("generated Docs lock helper did not acquire its lock");
        }

        // Prove that this is an operating-system lock rather than only the in-process mutex.
        let contender = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_root.join(GENERATION_COMMIT_LOCK_FILE))
            .unwrap();
        let lock_error: std::io::Error = contender.try_lock().unwrap_err().into();
        assert_eq!(lock_error.kind(), std::io::ErrorKind::WouldBlock);

        fs::write(source, "# After\n").unwrap();
        let writer_root = root.clone();
        let (completed_tx, completed_rx) = std::sync::mpsc::channel();
        let writer = thread::spawn(move || {
            let result = generate_fixture(
                &writer_root,
                &DocsOptions::default(),
                BuildMode::Development,
            );
            completed_tx.send(()).unwrap();
            result
        });
        let sibling_writer_root = root.clone();
        let sibling_target = root.join(".wake/candidate/docs/generated");
        let (sibling_completed_tx, sibling_completed_rx) = std::sync::mpsc::channel();
        let sibling_writer = thread::spawn(move || {
            let result = generate_with_mode_in(
                &sibling_writer_root,
                &sibling_target,
                &DocsOptions::default(),
                BuildMode::Development,
                DocsMode::Site,
            );
            sibling_completed_tx.send(()).unwrap();
            result
        });
        let publication_blocked = matches!(
            completed_rx.recv_timeout(Duration::from_millis(250)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        let sibling_publication_blocked = matches!(
            sibling_completed_rx.recv_timeout(Duration::from_millis(250)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );

        fs::write(&release, "release").unwrap();
        let helper_status = child.wait().unwrap();
        let second = writer.join().unwrap().unwrap();
        let sibling = sibling_writer.join().unwrap().unwrap();
        assert!(helper_status.success(), "generated Docs lock helper failed");
        assert!(
            publication_blocked,
            "generated Docs publication bypassed the separate-process commit lock"
        );
        assert!(
            sibling_publication_blocked,
            "a sibling generated Docs publication bypassed the project commit lock"
        );
        assert!(!second.changed_files.is_empty());
        assert_eq!(second.generated_dir, first.generated_dir);
        assert!(sibling.generated_dir.ends_with("docs/generated"));

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn generated_docs_rejects_a_symbolic_commit_lock() {
        let root = fixture();
        let lock_root = root.join(".wake");
        fs::create_dir_all(&lock_root).unwrap();
        let external = root.join("external-lock-target");
        fs::write(&external, "sentinel").unwrap();
        let lock_path = lock_root.join(GENERATION_COMMIT_LOCK_FILE);
        #[cfg(unix)]
        let link_result = std::os::unix::fs::symlink(&external, &lock_path);
        #[cfg(windows)]
        let link_result = std::os::windows::fs::symlink_file(&external, &lock_path);
        if link_result.is_err() {
            let _ = fs::remove_dir_all(root);
            return;
        }

        let error =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(matches!(error, DocsError::InvalidConfig(_)), "{error}");
        assert_eq!(fs::read_to_string(external).unwrap(), "sentinel");
        assert!(!root.join(".wake/docs/generated").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "invoked as a child process by the generated Docs commit lock regression"]
    fn generation_commit_lock_process_helper() {
        const PARENT_ENV: &str = "WAKE_TEST_DOCS_GENERATION_LOCK_PARENT";
        const READY_ENV: &str = "WAKE_TEST_DOCS_GENERATION_LOCK_READY";
        const RELEASE_ENV: &str = "WAKE_TEST_DOCS_GENERATION_LOCK_RELEASE";
        let Some(parent) = std::env::var_os(PARENT_ENV).map(PathBuf::from) else {
            return;
        };
        let ready = PathBuf::from(std::env::var_os(READY_ENV).expect("missing ready path"));
        let release = PathBuf::from(std::env::var_os(RELEASE_ENV).expect("missing release path"));
        let _lock = acquire_generation_commit_lock(&parent).unwrap();
        fs::write(ready, "ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !release.is_file() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(release.is_file(), "generated Docs lock helper timed out");
    }

    #[test]
    fn hardlinked_generated_file_is_rejected_without_touching_its_other_name() {
        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let external = root.join("external-sentinel.txt");
        fs::write(&external, "external sentinel").unwrap();
        let tracked = generated.generated_dir.join("registry.ts");
        fs::remove_file(&tracked).unwrap();
        fs::hard_link(&external, &tracked).unwrap();

        let error =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(matches!(error, DocsError::InvalidConfig(_)), "{error}");
        assert_eq!(fs::read_to_string(&external).unwrap(), "external sentinel");
        assert_eq!(fs::read_to_string(&tracked).unwrap(), "external sentinel");

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn symlinked_generated_file_is_rejected_without_touching_its_target() {
        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let external = root.join("external-symlink-sentinel.txt");
        fs::write(&external, "external sentinel").unwrap();
        let tracked = generated.generated_dir.join("registry.ts");
        fs::remove_file(&tracked).unwrap();
        #[cfg(unix)]
        let link_result = std::os::unix::fs::symlink(&external, &tracked);
        #[cfg(windows)]
        let link_result = std::os::windows::fs::symlink_file(&external, &tracked);
        if let Err(error) = link_result {
            #[cfg(windows)]
            {
                let _ = error;
                let _ = fs::remove_dir_all(root);
                return;
            }
            #[cfg(unix)]
            panic!("create fixture symlink: {error}");
        }

        let error =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(matches!(error, DocsError::InvalidConfig(_)), "{error}");
        assert_eq!(fs::read_to_string(external).unwrap(), "external sentinel");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_generation_install_restores_the_previous_tree() {
        struct FailSecondRename {
            calls: std::sync::atomic::AtomicUsize,
        }

        impl GenerationTransactionOps for FailSecondRename {
            fn rename(&self, source: &Path, destination: &Path) -> std::io::Result<()> {
                let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
                if call == 2 {
                    return Err(std::io::Error::other("injected install failure"));
                }
                fs::rename(source, destination)
            }

            fn remove_tree(&self, path: &Path) -> std::io::Result<()> {
                fs::remove_dir_all(path)
            }
        }

        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let before = raw_file_snapshot(&generated.generated_dir);
        let mut next = inspect_generation_tree(&generated.generated_dir)
            .unwrap()
            .unwrap()
            .files;
        next.insert(
            PathBuf::from("registry.ts"),
            b"export const injected = true;\n".to_vec(),
        );
        let ops = FailSecondRename {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };

        let error =
            publish_generation_with_ops(&generated.root, &generated.generated_dir, &next, &ops)
                .unwrap_err();
        assert!(matches!(error, DocsError::Io(_, _)), "{error}");
        assert_eq!(raw_file_snapshot(&generated.generated_dir), before);
        assert_eq!(
            ops.calls.load(Ordering::Relaxed),
            3,
            "restore was not attempted"
        );
        let leaked = fs::read_dir(generated.generated_dir.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| {
                name.starts_with(".wake-docs-next-") || name.starts_with(".wake-docs-previous-")
            })
            .collect::<Vec<_>>();
        assert!(leaked.is_empty(), "transaction siblings leaked: {leaked:?}");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cleanup_failure_after_commit_does_not_report_a_failed_generation() {
        struct FailCleanup;

        impl GenerationTransactionOps for FailCleanup {
            fn rename(&self, source: &Path, destination: &Path) -> std::io::Result<()> {
                fs::rename(source, destination)
            }

            fn remove_tree(&self, _path: &Path) -> std::io::Result<()> {
                Err(std::io::Error::other("injected cleanup failure"))
            }
        }

        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let mut next = inspect_generation_tree(&generated.generated_dir)
            .unwrap()
            .unwrap()
            .files;
        next.insert(
            PathBuf::from("registry.ts"),
            b"export const committed = true;\n".to_vec(),
        );

        let changed = publish_generation_with_ops(
            &generated.root,
            &generated.generated_dir,
            &next,
            &FailCleanup,
        )
        .unwrap();
        assert_eq!(
            fs::read(generated.generated_dir.join("registry.ts")).unwrap(),
            b"export const committed = true;\n"
        );
        assert_eq!(changed, vec![generated.generated_dir.join("registry.ts")]);
        for entry in fs::read_dir(generated.generated_dir.parent().unwrap()).unwrap() {
            let entry = entry.unwrap();
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".wake-docs-previous-")
            {
                fs::remove_dir_all(entry.path()).unwrap();
            }
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn leaves_imports_inside_fenced_code_as_documentation_text() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            "# Home\n\n```tsx\nimport \"./styles.css\";\nexport default function Example() {}\n```\n",
        )
        .unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();
        assert!(page.contains(r#"import \"./styles.css\";"#));
        assert!(!page.contains("@@wake/docs-project/docs/styles.css"));
    }

    #[test]
    fn mdx_esm_rewrites_only_typed_module_specifiers() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            r#"import {
  "../src/badge.tsx" as Badge,
} from /* source boundary */ "../src/badge.tsx"

import /* side-effect boundary */ "../src/side-effect.css"

import data from "../src/data.json" with { type: "../src/data.json" }

export {
  "../src/button.tsx" as Button,
} from /* export boundary */ "../src/button.tsx"

export const lazy = () => import(
  /* dynamic boundary */
  "../src/lazy.tsx",
  { with: { type: "../src/lazy.tsx" } }
)

export const computed = (name) => import("../src/" + name)
export const ordinary = "../src/not-a-module.ts"
export const template = `../src/not-a-template.ts`
export const commented = /* "../src/not-a-comment.ts" */ Badge

# Home

Body.
"#,
        )
        .unwrap();

        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();

        for rewritten in [
            "@@wake/docs-project/src/badge.tsx",
            "@@wake/docs-project/src/side-effect.css",
            "@@wake/docs-project/src/data.json",
            "@@wake/docs-project/src/button.tsx",
            "@@wake/docs-project/src/lazy.tsx",
        ] {
            assert!(
                page.contains(rewritten),
                "missing rewrite: {rewritten}\n{page}"
            );
        }
        for untouched in [
            "../src/",
            "../src/not-a-module.ts",
            "../src/not-a-template.ts",
            "../src/not-a-comment.ts",
        ] {
            assert!(
                page.contains(untouched),
                "ordinary JavaScript changed: {untouched}\n{page}"
            );
        }
        for bait in [
            r#""../src/badge.tsx" as Badge"#,
            r#"type: "../src/data.json""#,
            r#""../src/button.tsx" as Button"#,
            r#"type: "../src/lazy.tsx""#,
        ] {
            assert!(
                page.contains(bait),
                "same-value bait changed: {bait}\n{page}"
            );
        }
        assert!(
            page.contains("<h1"),
            "Markdown after multiline ESM was lost: {page}"
        );
    }

    #[test]
    fn duplicate_heading_metadata_matches_rendered_ids() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            "# Home\n\n## Repeat\n\nFirst.\n\n## Repeat\n\nSecond.\n",
        )
        .unwrap();

        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let route = &generated.routes[0];
        assert_eq!(
            route
                .headings
                .iter()
                .map(|heading| heading.id.as_str())
                .collect::<Vec<_>>(),
            vec!["home", "repeat", "repeat-1"]
        );
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();
        for id in ["home", "repeat", "repeat-1"] {
            assert!(
                page.contains(&format!("id={{\"{id}\"}}")),
                "missing DOM id {id}"
            );
        }
    }

    #[test]
    fn unicode_space_hash_and_percent_routes_have_one_canonical_identity() {
        let root = fixture();
        let file_name = "100% # 中文.mdx";
        fs::write(root.join("docs").join(file_name), "# Encoded route\n").unwrap();

        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let route = &generated.routes[0];
        let encoded = "/100%25%20%23%20%E4%B8%AD%E6%96%87";
        assert_eq!(route.slug, encoded);
        let registry = fs::read_to_string(generated.generated_dir.join("registry.ts")).unwrap();
        assert!(registry.contains(&format!(r#""encoded":"{encoded}""#)));
        assert!(registry.contains(r#""decoded":"/100% # 中文""#));
        assert!(RUNTIME_APP.contains("routePathFromLocation"));
        assert!(RUNTIME_ROUTES.contains("page.routePath.encoded"));

        let outdir = root.join("dist");
        fs::create_dir_all(&outdir).unwrap();
        write_route_shells(
            &outdir,
            &generated.routes,
            "<!doctype html><html><head><title>Docs</title></head><body></body></html>",
            "Docs",
            "Description",
            "en",
        )
        .unwrap();
        assert!(outdir.join("100% # 中文/index.html").is_file());
        assert!(
            !outdir
                .join("100%25%20%23%20%E4%B8%AD%E6%96%87/index.html")
                .exists()
        );
    }

    #[test]
    fn route_codec_is_segment_safe_uppercase_and_not_double_encoded() {
        let route = RoutePath::from_page_relative(
            Path::new("100% # 中文.mdx"),
            Path::new("docs/100% # 中文.mdx"),
        )
        .unwrap();
        assert_eq!(route.decoded, "/100% # 中文");
        assert_eq!(route.encoded, "/100%25%20%23%20%E4%B8%AD%E6%96%87");
        assert_eq!(
            RoutePath::from_canonical_encoded(&route.encoded).unwrap(),
            route
        );

        let literal_percent =
            RoutePath::from_page_relative(Path::new("100%25.mdx"), Path::new("docs/100%25.mdx"))
                .unwrap();
        assert_eq!(literal_percent.encoded, "/100%2525");
        assert_eq!(
            RoutePath::from_canonical_encoded(&literal_percent.encoded).unwrap(),
            literal_percent
        );

        for invalid in ["/%", "/%2f", "/%2F", "/%5C", "/../x", "/a//b"] {
            assert!(
                RoutePath::from_canonical_encoded(invalid).is_err(),
                "unsafe or non-canonical route was accepted: {invalid}"
            );
        }
    }

    #[test]
    fn backslash_never_becomes_a_lossy_identity_or_unmatchable_route_segment() {
        assert!(
            checked_identity_segment(OsStr::new(r"bad\name"), Path::new("docs/page.mdx")).is_err(),
            "a normal path component containing a backslash must be rejected"
        );

        match RoutePath::from_page_relative(
            Path::new(r"bad\name.mdx"),
            Path::new(r"docs/bad\name.mdx"),
        ) {
            Ok(route) => {
                assert_eq!(route.decoded, "/bad/name");
                assert_eq!(route.encoded, "/bad/name");
            }
            Err(DocsError::InvalidPagePath(_, message)) => {
                assert!(message.contains("separator"));
            }
            Err(error) => panic!("unexpected route error: {error}"),
        }

        match checked_slash_path(Path::new(r"bad\name.mdx"), Path::new(r"docs/bad\name.mdx")) {
            Ok(identity) => assert_eq!(identity, "bad/name.mdx"),
            Err(DocsError::InvalidPagePath(_, message)) => {
                assert!(message.contains("separator"));
            }
            Err(error) => panic!("unexpected identity error: {error}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn non_utf8_page_segments_are_diagnosed_instead_of_lossily_colliding() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        let root = PathBuf::from("project");
        let source_dir = root.join("docs");
        let invalid =
            OsString::from_wide(&[0xD800, b'.' as u16, b'm' as u16, b'd' as u16, b'x' as u16]);
        let error = PageIdentity::from_paths(&root, &source_dir, &source_dir.join(invalid))
            .expect_err("non-UTF-8 page path must be rejected");
        assert!(
            matches!(error, DocsError::InvalidPagePath(_, message) if message.contains("UTF-8"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_page_segments_are_diagnosed_instead_of_lossily_colliding() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let root = PathBuf::from("project");
        let source_dir = root.join("docs");
        let invalid = OsString::from_vec(vec![0xff, b'.', b'm', b'd', b'x']);
        let error = PageIdentity::from_paths(&root, &source_dir, &source_dir.join(invalid))
            .expect_err("non-UTF-8 page path must be rejected");
        assert!(
            matches!(error, DocsError::InvalidPagePath(_, message) if message.contains("UTF-8"))
        );
    }

    #[test]
    fn page_source_maps_are_relative_deterministic_and_honest() {
        let source = "import Badge from \"../src/badge.tsx\"\n\n# Home\n\nBody.\n";
        let build = |root: &Path| {
            fs::write(root.join("docs/index.mdx"), source).unwrap();
            let generated =
                generate_fixture(root, &DocsOptions::default(), BuildMode::Development).unwrap();
            let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();
            let map =
                fs::read_to_string(generated.generated_dir.join("pages/index.tsx.map")).unwrap();
            let registry = fs::read_to_string(generated.generated_dir.join("registry.ts")).unwrap();
            (page, map, registry)
        };
        let first_root = fixture();
        let second_root = fixture();
        let first = build(&first_root);
        let second = build(&second_root);
        assert_eq!(first, second, "generated docs depend on checkout root");

        let map: serde_json::Value = serde_json::from_str(&first.1).unwrap();
        assert_eq!(map["file"], "pages/index.tsx");
        assert_eq!(map["sources"], json!(["docs/index.mdx"]));
        assert!(!first.1.contains(&slash_path(&first_root)));
        assert!(!first.1.contains(&slash_path(&second_root)));

        let lines = first.0.lines().collect::<Vec<_>>();
        let mappings = map["mappings"]
            .as_str()
            .unwrap()
            .split(';')
            .collect::<Vec<_>>();
        let decoded = decode_test_mappings(map["mappings"].as_str().unwrap());
        assert_eq!(mappings.len(), lines.len());
        assert!(
            mappings[0].is_empty(),
            "synthetic runtime import was mapped"
        );
        assert!(
            mappings.last().is_some_and(|mapping| mapping.is_empty()),
            "synthetic sourceMappingURL/trailing newline must stay unmapped"
        );
        let esm_line = lines
            .iter()
            .position(|line| line.contains("src/badge.tsx"))
            .unwrap();
        assert!(
            !mappings[esm_line].is_empty(),
            "copied ESM has no source mapping"
        );
        assert_eq!(
            decoded[esm_line].first().copied(),
            Some(DecodedMapping {
                generated_column: 0,
                original: Some((0, 0)),
            }),
            "ESM must point to its actual MDX token"
        );
        let generated_specifier = lines[esm_line].find('"').unwrap() as i64;
        let original_specifier = source.find('"').unwrap() as i64;
        assert_eq!(
            decoded[esm_line]
                .iter()
                .find(|mapping| mapping.generated_column == generated_specifier)
                .and_then(|mapping| mapping.original),
            Some((0, original_specifier)),
            "rewritten module specifier must map to the original string token"
        );
        let metadata_line = lines
            .iter()
            .position(|line| line.starts_with("export const __wakeMeta"))
            .unwrap();
        assert!(
            mappings[metadata_line].is_empty(),
            "synthetic metadata was mapped"
        );
        let heading_line = lines.iter().position(|line| line.contains("<h1")).unwrap();
        assert!(
            mappings[heading_line].contains(','),
            "derived heading line must terminate with a generated-only segment"
        );
        assert_eq!(decoded[heading_line][0].generated_column, 4);
        assert_eq!(decoded[heading_line][0].original, Some((2, 0)));
        assert_eq!(
            decoded[heading_line].last().unwrap().original,
            None,
            "the synthetic remainder of a rendered node must not inherit its source"
        );
    }

    #[test]
    fn unicode_and_long_final_metadata_keep_source_maps_aligned() {
        let root = fixture();
        let description = "跨语言路线🚀".repeat(2_048);
        let source = format!(
            "+++\ntitle = \"最终页面🧭\"\ndescription = \"{description}\"\n+++\n# 标题🙂\n\n正文。\n"
        );
        fs::write(root.join("docs/index.mdx"), &source).unwrap();

        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();
        let raw_map =
            fs::read_to_string(generated.generated_dir.join("pages/index.tsx.map")).unwrap();
        let map: serde_json::Value = serde_json::from_str(&raw_map).unwrap();
        let mappings = map["mappings"]
            .as_str()
            .unwrap()
            .split(';')
            .collect::<Vec<_>>();
        let decoded = decode_test_mappings(map["mappings"].as_str().unwrap());
        let lines = page.lines().collect::<Vec<_>>();
        let metadata_line = lines
            .iter()
            .position(|line| line.starts_with("export const __wakeMeta"))
            .unwrap();
        let heading_line = lines.iter().position(|line| line.contains("<h1")).unwrap();
        let original_heading_line =
            source.lines().position(|line| line == "# 标题🙂").unwrap() as i64;

        assert_eq!(map["sourcesContent"], json!([source]));
        assert_eq!(mappings.len(), lines.len());
        assert!(mappings[metadata_line].is_empty());
        assert_eq!(
            decoded[heading_line]
                .first()
                .and_then(|mapping| mapping.original),
            Some((original_heading_line, 0))
        );
        assert!(lines[metadata_line].contains(&description));
    }

    #[test]
    fn lazy_search_corpus_comes_from_visible_markdown_ast_nodes() {
        let root = fixture();
        let page_path = root.join("docs/index.mdx");
        fs::write(
            &page_path,
            r#"+++
title = "Search"
description = "frontmatter-only-marker"
+++

import { Button as InvisibleImportMarker } from "../src/button.tsx"

# Search heading

Visible **body copy** with `--minify`.

```bash
wake build --release
```

<span>JSX visible text</span>

{invisibleExpressionMarker}
"#,
        )
        .unwrap();

        let compiled = compile_page(&root, &root.join("docs"), &page_path).unwrap();
        for expected in [
            "Search heading",
            "Visible",
            "body copy",
            "--minify",
            "wake build --release",
            "JSX visible text",
        ] {
            assert!(
                compiled.search_text.contains(expected),
                "missing search text fragment: {expected}"
            );
        }
        for excluded in [
            "frontmatter-only-marker",
            "InvisibleImportMarker",
            "invisibleExpressionMarker",
        ] {
            assert!(
                !compiled.search_text.contains(excluded),
                "non-visible AST content leaked into search text: {excluded}"
            );
        }
        assert!(
            !serde_json::to_string(&compiled.route)
                .unwrap()
                .contains("searchText")
        );

        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let registry = fs::read_to_string(generated.generated_dir.join("registry.ts")).unwrap();
        let corpus = fs::read_to_string(generated.generated_dir.join("search-corpus.ts")).unwrap();
        assert!(!registry.contains("searchText:"));
        assert!(!registry.contains("--minify"));
        assert!(corpus.contains("--minify"));
        assert_eq!(
            fs::read_to_string(generated.generated_dir.join("runtime/search.mjs")).unwrap(),
            RUNTIME_SEARCH
        );
    }

    #[test]
    fn rejects_react_versions_older_than_19() {
        let root = fixture();
        fs::write(
            root.join("package.json"),
            r#"{"dependencies":{"react":"^18.3.1","react-dom":"^19.0.0"}}"#,
        )
        .unwrap();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        let error =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(
            matches!(error, DocsError::InvalidConfig(message) if message.contains("react 19+") && message.contains("^18.3.1"))
        );
    }

    #[test]
    fn compiles_gfm_mdx_esm_macros_and_frontmatter() {
        let root = fixture();
        fs::write(
            root.join("docs/button.mdx"),
            r#"+++
title = "Button"
description = "Actions"
kind = "guide"
status = "stable"
+++

import Badge from "../src/badge.tsx"

# Button

| State | Meaning |
| --- | --- |
| stable | Ready |

<Badge>{1 + 1}</Badge>
<Demo src="./demos/basic.demo.tsx" />
<Demos glob="./demos/*.demo.tsx" columns={2} />
<API source="../src/button.tsx" symbol="ButtonProps" component="Button" />
"#,
        )
        .expect("mdx");
        fs::write(root.join("src/badge.tsx"), "export default () => null").expect("badge");
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert_eq!(generated.routes.len(), 1);
        assert_eq!(generated.routes[0].slug, "/button");
        let page = fs::read_to_string(generated.generated_dir.join("pages/button.tsx")).unwrap();
        assert!(page.contains("@@wake/docs-project/src/badge.tsx"));
        assert!(page.contains("<table>"));
        assert!(page.contains("<Badge>{1 + 1}</Badge>"));
        assert!(page.contains("__wakePage"));
        assert!(page.contains("sourceMappingURL=button.tsx.map"));
        let registry = fs::read_to_string(generated.generated_dir.join("registry.ts")).unwrap();
        assert!(registry.contains("basic.demo.tsx"));
        assert!(registry.contains("ButtonProps"));
        assert!(!registry.contains(&root.to_string_lossy().to_string()));
    }

    #[test]
    fn lifts_block_jsx_out_of_markdown_paragraphs() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            r#"# Blocks

<div className="cards">
  <div className="card">One</div>
  <div className="card">Two</div>
</div>

<div className="callout"><strong>Title</strong> Content</div>

<section>
Text <em>inline</em>

<div className="mixed-block">Block</div>

Tail
</section>
"#,
        )
        .unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();

        assert!(page.contains(r#"<div className={"cards"}><div className={"card"}>"#));
        assert!(page.contains(r#"<div className={"callout"}><strong>"#));
        assert!(page.contains(r#"<section><p>"#));
        assert!(page.contains(r#"</p><div className={"mixed-block"}>"#));
        for invalid in ["<p><div", "<p><section", "<p><article"] {
            assert!(
                !page.contains(invalid),
                "generated invalid JSX: {invalid}\n{page}"
            );
        }
    }

    #[test]
    fn keeps_markdown_children_but_lifts_standalone_components_and_fragments() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            r#"# Components

<Panel>

Markdown **strong**.

</Panel>

<Card />

<>
  <div>One</div>
  <article>Two</article>
</>

Inline <Badge>status</Badge> text.
"#,
        )
        .unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();

        assert!(page.contains("<Panel><p>"), "{page}");
        assert!(page.contains("<Card />"), "{page}");
        assert!(!page.contains("<p><Card />"), "{page}");
        assert!(page.contains("<><div>"), "{page}");
        assert!(!page.contains("<p><div"), "{page}");
        assert!(page.contains("<p>{\"Inline \"}<Badge>"), "{page}");
    }

    #[test]
    fn keeps_jsx_blocks_valid_next_to_markdown_flow_nodes() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            r#"# Adjacent

> Quote

<div>After quote</div>

- Item

<section>After list</section>

| A | B |
| --- | --- |
| 1 | 2 |

<article>After table</article>
"#,
        )
        .unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();

        assert!(page.contains("</blockquote>\n    <div>"), "{page}");
        assert!(page.contains("</ul>\n    <section>"), "{page}");
        assert!(page.contains("</div>\n    <article>"), "{page}");
        assert!(!page.contains("<p><div"), "{page}");
        assert!(!page.contains("<p><section"), "{page}");
        assert!(!page.contains("<p><article"), "{page}");
    }

    #[test]
    fn highlights_fenced_code_and_renders_semantic_tables() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            r####"# Code

## Example

```tsx title="button.tsx" {2}
import React from "react"
const count: number = 2
// Comment
```

| Name | Type |
| --- | --- |
| count | number |
"####,
        )
        .unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();
        assert!(page.contains("<CodeBlock"));
        assert!(page.contains("syntax-keyword"));
        assert!(page.contains("syntax-number"));
        assert!(page.contains("syntax-comment"));
        assert!(page.contains("code-line is-highlighted"));
        assert!(page.contains("button.tsx"));
        assert!(page.contains("heading-anchor"));
        assert!(page.contains("aria-label={\"Example\"}"));
        assert!(page.contains("<thead><tr><th>"));

        let windows_lines = highlight_code("const first = 1;\r\nconst second = 2;", "tsx");
        assert!(windows_lines.iter().all(|line| !line.contains('\r')));
        assert!(
            highlight_code("Get-ChildItem # Files", "powershell")[0].contains("syntax-comment")
        );
        assert!(highlight_code("SELECT * FROM docs -- Pages", "sql")[0].contains("syntax-keyword"));
        assert_eq!(
            code_highlighted_lines(Some("{1-999999}"), 2),
            BTreeSet::from([1, 2])
        );
    }

    #[test]
    fn renders_task_lists_with_stable_runtime_hooks() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            "# Checklist\n\n- [ ] Pending\n- [x] Complete\n",
        )
        .unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();
        assert!(page.contains("<ul className=\"task-list\">"));
        assert!(page.contains("<li className=\"task-list-item\">"));
        assert!(page.contains("checked={false} disabled"));
        assert!(page.contains("checked={true} disabled"));
    }

    #[test]
    fn rejects_dynamic_compile_time_attributes_with_location() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            "# Home\n\n<Demo src={chooseDemo()} />\n",
        )
        .expect("mdx");
        let error =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(matches!(error, DocsError::InvalidMacro { line: 3, .. }));
    }

    #[test]
    fn derives_routes_from_paths_and_excludes_production_drafts() {
        let root = fixture();
        fs::write(root.join("docs/a.mdx"), "# A").unwrap();
        fs::write(root.join("docs/b.mdx"), "+++\ndraft = true\n+++\n# B").unwrap();
        let development =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert_eq!(
            development
                .routes
                .iter()
                .map(|route| route.slug.as_str())
                .collect::<Vec<_>>(),
            vec!["/a", "/b"]
        );
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Production).unwrap();
        assert_eq!(generated.routes.len(), 1);
        assert_eq!(generated.routes[0].slug, "/a");
    }

    #[test]
    fn navigation_manifest_controls_group_section_and_page_order() {
        let root = fixture();
        fs::write(
            root.join("docs/install.mdx"),
            "+++\ntitle = \"安装\"\n+++\n# Install\n",
        )
        .expect("install");
        fs::write(
            root.join("docs/build.mdx"),
            "+++\ntitle = \"构建\"\n+++\n# Build\n",
        )
        .expect("build");
        fs::write(
            root.join("docs/navigation.toml"),
            r#"[[group]]
id = "app"
title = "应用开发"
pages = ["install"]

[[group.section]]
id = "release"
title = "构建与发布"
pages = ["build"]
"#,
        )
        .expect("navigation");
        let generated = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert_eq!(
            generated
                .routes
                .iter()
                .map(|route| (
                    route.title.as_str(),
                    route.group.as_str(),
                    route.section.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![("安装", "应用开发", ""), ("构建", "应用开发", "构建与发布")]
        );
        let build_page =
            fs::read_to_string(generated.generated_dir.join("pages/build.tsx")).unwrap();
        assert!(build_page.contains(r#""group":"应用开发""#));
        assert!(build_page.contains(r#""section":"构建与发布""#));
    }

    #[test]
    fn compiled_page_defers_module_render_until_navigation_is_final() {
        let root = fixture();
        let page_path = root.join("docs/index.mdx");
        let source = "+++\ntitle = \"首页\"\n+++\n# Home\n";
        fs::write(&page_path, source).unwrap();
        fs::write(
            root.join("docs/navigation.toml"),
            "[[group]]\nid = \"guide\"\ntitle = \"最终导航\"\npages = [\"index\"]\n",
        )
        .unwrap();

        let page = compile_page(&root, &root.join("docs"), &page_path).unwrap();
        assert_eq!(page.route.group, "");
        assert_eq!(page.module_plan.source, source);
        let pre_navigation_metadata = serde_json::to_string(&page.route).unwrap();
        let mut pages = vec![(page_path, page)];

        apply_navigation(&root.join("docs"), &mut pages).unwrap();
        let final_route = pages[0].1.route.clone();
        let rendered = pages[0].1.render_module();
        let registry = render_registry(&pages, &[], &[]);
        let routes = pages
            .iter()
            .map(|(_, page)| page.route.clone())
            .collect::<Vec<_>>();
        let metadata = serde_json::to_string(&final_route).unwrap();

        assert_eq!(routes, vec![final_route]);
        assert!(
            rendered
                .code
                .contains(&format!("export const __wakeMeta = {metadata};"))
        );
        assert!(!rendered.code.contains(&format!(
            "export const __wakeMeta = {pre_navigation_metadata};"
        )));
        assert!(registry.contains(&format!("{{ ...{metadata}, routePath:")));
        assert!(metadata.contains(r#""group":"最终导航""#));
    }

    #[test]
    fn user_esm_that_contains_the_metadata_prefix_is_never_rewritten() {
        let root = fixture();
        let marker = "export const __wakeMeta = {\"owner\":\"user\"};\n";
        fs::write(
            root.join("docs/index.mdx"),
            format!("export const userTemplate = `{marker}`;\n\n# Home\n"),
        )
        .unwrap();

        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();
        let route_metadata = serde_json::to_string(&generated.routes[0]).unwrap();

        assert!(page.contains(&format!("export const userTemplate = `{marker}`;")));
        assert!(page.contains(&format!("export const __wakeMeta = {route_metadata};")));
        assert_eq!(page.matches("export const __wakeMeta = ").count(), 2);
    }

    #[test]
    fn rejects_retired_frontmatter_fields() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            "+++\ntitle = \"Home\"\nslug = \"/legacy\"\n+++\n# Home\n",
        )
        .unwrap();
        let error =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(
            matches!(error, DocsError::Frontmatter(_, message) if message.contains("unknown field `slug`"))
        );
    }

    #[test]
    fn navigation_rejects_missing_duplicate_and_unlisted_pages() {
        let root = fixture();
        fs::write(root.join("docs/a.mdx"), "# A").unwrap();
        fs::write(root.join("docs/b.mdx"), "# B").unwrap();
        fs::write(
            root.join("docs/navigation.toml"),
            "[[group]]\nid = \"test\"\ntitle = \"Test\"\npages = [\"a\", \"a\", \"missing\"]\n",
        )
        .unwrap();
        let error = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(
            matches!(error, DocsError::Navigation(_, message) if message.contains("appears more than once"))
        );

        fs::write(
            root.join("docs/navigation.toml"),
            "[[group]]\nid = \"test\"\ntitle = \"Test\"\npages = [\"a\", \"missing\"]\n",
        )
        .unwrap();
        let error = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(
            matches!(error, DocsError::Navigation(_, message) if message.contains("missing page `missing`"))
        );

        fs::write(
            root.join("docs/navigation.toml"),
            "[[group]]\nid = \"test\"\ntitle = \"Test\"\npages = [\"a\"]\n",
        )
        .unwrap();
        let error = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(
            matches!(error, DocsError::Navigation(_, message) if message.contains("page `b` is not listed"))
        );
    }

    #[test]
    fn hidden_pages_do_not_need_navigation_entries() {
        let root = fixture();
        fs::write(root.join("docs/a.mdx"), "# A").unwrap();
        fs::write(
            root.join("docs/hidden.mdx"),
            "+++\nhidden = true\n+++\n# Hidden\n",
        )
        .unwrap();
        fs::write(
            root.join("docs/navigation.toml"),
            "[[group]]\nid = \"test\"\ntitle = \"Test\"\npages = [\"a\"]\n",
        )
        .unwrap();
        let generated = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert_eq!(generated.routes.len(), 2);
        assert!(generated.routes.iter().any(|route| route.hidden));
        let corpus = fs::read_to_string(generated.generated_dir.join("search-corpus.ts")).unwrap();
        assert!(!corpus.contains("Hidden"));
    }

    #[test]
    fn sidebar_keeps_status_in_the_page_header_instead_of_navigation_items() {
        let runtime = include_str!("../runtime/app.tsx");
        let sidebar_start = runtime.find("function Sidebar").expect("sidebar");
        let sidebar_end = runtime[sidebar_start..]
            .find("function TableOfContents")
            .map(|offset| sidebar_start + offset)
            .expect("table of contents");
        let sidebar = &runtime[sidebar_start..sidebar_end];

        assert!(!sidebar.contains("StatusBadge"));
        assert!(sidebar.contains("aria-current={active ? \"page\" : undefined}"));
        assert!(runtime.contains("className=\"breadcrumbs\""));
        assert!(runtime.contains("<StatusBadge status={meta.status} />"));
        assert!(runtime.contains("sessionStorage"));
        assert!(runtime.contains("wake-docs-user-expanded-sections"));
        assert!(!runtime.contains("wake-docs-expanded-sections"));
        assert!(!runtime.contains("setExpanded((current) => current.has(activeKey)"));
    }

    #[test]
    fn page_edit_rewrites_the_page_source_map_and_lazy_search_corpus() {
        let root = fixture();
        let page = root.join("docs/index.mdx");
        fs::write(&page, "# Home\n\nFirst paragraph.\n").unwrap();
        let first =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let unchanged =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert!(unchanged.changed_files.is_empty());

        fs::write(&page, "# Home\n\nSecond paragraph.\n").unwrap();
        let changed =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let relative: BTreeSet<_> = changed
            .changed_files
            .iter()
            .filter_map(|path| path.strip_prefix(&first.generated_dir).ok())
            .map(slash_path)
            .collect();
        assert_eq!(
            relative,
            BTreeSet::from([
                "pages/index.tsx".to_string(),
                "pages/index.tsx.map".to_string(),
                "search-corpus.ts".to_string(),
            ])
        );
    }

    #[test]
    fn demo_edit_only_rewrites_its_lazy_source_module() {
        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        let first =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        fs::write(
            root.join("docs/demos/basic.demo.tsx"),
            "export default function Demo() { return <button>Changed</button>; }",
        )
        .unwrap();
        let changed =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let relative: BTreeSet<_> = changed
            .changed_files
            .iter()
            .filter_map(|path| path.strip_prefix(&first.generated_dir).ok())
            .map(slash_path)
            .collect();
        assert_eq!(
            relative,
            BTreeSet::from(["demo-source/docs/demos/basic.demo.source.tsx".to_string()])
        );
        let source_module = fs::read_to_string(
            first
                .generated_dir
                .join("demo-source/docs/demos/basic.demo.source.tsx"),
        )
        .unwrap();
        assert!(source_module.contains("export const language = \"tsx\""));
        assert!(source_module.contains("export const highlighted = <>"));
        assert!(source_module.contains("syntax-keyword"));
        let registry = fs::read_to_string(first.generated_dir.join("registry.ts")).unwrap();
        assert!(registry.contains("loadSource"));
        assert!(!registry.contains("Changed</button>"));
    }

    #[test]
    fn static_route_shells_include_page_metadata_and_locale() {
        let root = fixture();
        let outdir = root.join("dist");
        let shell = "<!doctype html><html lang=\"en-US\"><head><title>wake app</title></head><body></body></html>";
        let route = |title: &str, description: &str, slug: &str| RouteInfo {
            id: slug.to_string(),
            file: format!("docs/{}.mdx", slug.trim_matches('/')),
            title: title.to_string(),
            description: description.to_string(),
            kind: "guide".to_string(),
            group: "指南".to_string(),
            group_id: "guide".to_string(),
            section: String::new(),
            section_id: String::new(),
            slug: slug.to_string(),
            status: "stable".to_string(),
            draft: false,
            hidden: false,
            headings: Vec::new(),
        };
        let routes = vec![
            route("Wake & Docs", "中文 <首页>", "/"),
            route(
                "创建 React 应用",
                "使用 \"Wake\" 构建",
                "/start/create-react-app",
            ),
        ];

        write_route_shells(&outdir, &routes, shell, "Wake & Docs", "默认描述", "zh-CN").unwrap();

        let root_html = fs::read_to_string(outdir.join("index.html")).unwrap();
        assert!(root_html.contains("<html lang=\"zh-CN\">"));
        assert!(root_html.contains("<title>Wake &amp; Docs</title>"));
        assert!(root_html.contains("content=\"中文 &lt;首页&gt;\""));
        let guide_html =
            fs::read_to_string(outdir.join("start/create-react-app/index.html")).unwrap();
        assert!(guide_html.contains("<title>创建 React 应用 · Wake &amp; Docs</title>"));
        assert!(guide_html.contains("content=\"使用 &quot;Wake&quot; 构建\""));
        let not_found_html = fs::read_to_string(outdir.join("404.html")).unwrap();
        assert!(not_found_html.contains("<title>404 · Wake &amp; Docs</title>"));
        assert!(not_found_html.contains("找不到请求的文档页面。"));
    }

    #[test]
    fn public_logo_follows_the_configured_base_path() {
        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        let options = DocsOptions {
            base_path: "/crab/".to_string(),
            logo: Some("/logo.svg".to_string()),
            ..DocsOptions::default()
        };
        let generated = generate_fixture(&root, &options, BuildMode::Production).unwrap();
        let config = fs::read_to_string(generated.generated_dir.join("config.tsx")).unwrap();
        assert!(config.contains(r#""basePath":"/crab/""#));
        assert!(config.contains(r#""logo":"/crab/logo.svg""#));
        assert!(config.contains(r#""locale":"zh-CN""#));
        assert_eq!(
            public_asset_url("/crab/", "https://cdn.example/logo.svg"),
            "https://cdn.example/logo.svg"
        );
    }

    #[test]
    fn docs_options_default_to_chinese_and_allow_explicit_english() {
        assert_eq!(DocsOptions::default().locale, "zh-CN");
        assert_eq!(
            DocsOptions::default().presentation,
            DocsPresentation::Standalone
        );

        let options = DocsOptions {
            locale: "en-US".to_string(),
            ..DocsOptions::default()
        };
        let config = render_config(Path::new("."), &options, DocsMode::Site).unwrap();
        assert!(config.contains(r#""locale":"en-US""#));
        assert!(config.contains(r#""presentation":"standalone""#));

        let embedded = render_config(
            Path::new("."),
            &DocsOptions {
                presentation: DocsPresentation::Embedded,
                ..DocsOptions::default()
            },
            DocsMode::Components,
        )
        .unwrap();
        assert!(embedded.contains(r#""presentation":"embedded""#));
    }

    #[test]
    fn default_preview_centers_demos_while_explicit_preview_owns_layout() {
        for docs_mode in [DocsMode::Site, DocsMode::Components] {
            let config =
                render_config(Path::new("project"), &DocsOptions::default(), docs_mode).unwrap();
            assert!(config.contains("function Preview({ children }"));
            assert!(config.contains("demo-default-preview"));
            assert!(!config.contains("React.Fragment"));
        }

        let explicit = render_config(
            Path::new("project"),
            &DocsOptions {
                preview: Some(PathBuf::from("docs/preview.tsx")),
                ..DocsOptions::default()
            },
            DocsMode::Components,
        )
        .unwrap();
        assert!(explicit.contains(
            r#"export { default as Preview } from "@@wake/docs-project/docs/preview.tsx";"#
        ));
        assert!(!explicit.contains("demo-default-preview"));
    }

    #[test]
    fn default_preview_safely_centers_runtime_content() {
        let default_preview = RUNTIME_STYLE
            .split_once(".demo-default-preview {")
            .expect("default Preview style")
            .1
            .split_once('}')
            .expect("default Preview style end")
            .0;
        assert!(default_preview.contains("display: grid;"));
        assert!(default_preview.contains("place-content: safe center;"));
        assert!(default_preview.contains("place-items: safe center;"));
    }

    #[test]
    fn accent_color_is_optional_and_serializes_explicit_overrides() {
        let default_config =
            render_config(Path::new("."), &DocsOptions::default(), DocsMode::Site).unwrap();
        assert!(default_config.contains(r#""accentColor":null"#));

        let options = DocsOptions {
            accent_color: Some("#7c3aed".to_string()),
            ..DocsOptions::default()
        };
        let explicit_config = render_config(Path::new("."), &options, DocsMode::Site).unwrap();
        assert!(explicit_config.contains(r##""accentColor":"#7c3aed""##));

        let invalid = DocsOptions {
            accent_color: Some("purple".to_string()),
            ..DocsOptions::default()
        };
        assert!(matches!(
            validate_options(&invalid),
            Err(DocsError::InvalidConfig(message))
                if message == "accent_color must be a six-digit hex color"
        ));
    }

    #[test]
    fn component_mode_builds_a_typed_demo_catalog_without_mdx() {
        let root = fixture();
        fs::write(
            root.join("docs/demos/basic.demo.tsx"),
            r#"
                import type { ButtonProps } from "../../src/button";
                export const meta = {
                    title: "Editable",
                    group: "Actions",
                    component: "Button",
                    order: 7,
                    args: { label: "Create" },
                };
                export default function Demo(props: ButtonProps) {
                    return <button>{props.label}</button>;
                }
            "#,
        )
        .unwrap();

        let generated = generate_with_mode(
            &root,
            &DocsOptions::default(),
            BuildMode::Development,
            DocsMode::Components,
        )
        .unwrap();

        assert_eq!(generated.mode, DocsMode::Components);
        assert!(generated.routes.is_empty());
        assert!(generated.warnings.is_empty());
        assert_eq!(generated.demos.len(), 1);
        let demo = &generated.demos[0];
        assert_eq!(demo.title, "Editable");
        assert_eq!(demo.group, "Actions");
        assert_eq!(demo.component, "Button");
        assert_eq!(demo.order, 7);
        assert_eq!(demo.control_count, 1);

        let registry = fs::read_to_string(generated.generated_dir.join("registry.ts")).unwrap();
        assert!(registry.contains(r#""kind":"string""#));
        assert!(registry.contains(r#""name":"label""#));
        let config = fs::read_to_string(generated.generated_dir.join("config.tsx")).unwrap();
        assert!(config.contains(r#""mode":"components""#));
        assert!(
            generated
                .generated_dir
                .join("runtime/components.tsx")
                .is_file()
        );
        let components_runtime =
            fs::read_to_string(generated.generated_dir.join("runtime/components.tsx")).unwrap();
        assert!(components_runtime.contains("@crab-dev/wake/internal/components-runtime"));
        assert!(!components_runtime.contains("from \"@crab-dev/rc-"));
        assert!(!components_runtime.contains("from \"lucide-react\""));
        assert!(
            generated
                .generated_dir
                .join("runtime/components-state.mjs")
                .is_file()
        );
        assert_eq!(
            fs::read_to_string(generated.generated_dir.join("runtime/search.mjs")).unwrap(),
            RUNTIME_SEARCH
        );
        assert!(generated.entry.ends_with("runtime/components-entry.tsx"));
        assert!(
            !generated
                .generated_dir
                .join("runtime/site-entry.tsx")
                .exists()
        );
    }

    #[test]
    fn site_mode_emits_only_the_site_runtime_surface() {
        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();

        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Production).unwrap();

        assert!(generated.entry.ends_with("runtime/site-entry.tsx"));
        let entry = fs::read_to_string(&generated.entry).unwrap();
        assert!(entry.contains("runtime/app.tsx"));
        assert!(!entry.contains("ComponentsApp"));
        assert!(!entry.contains("components.css"));
        assert_eq!(
            fs::read_to_string(generated.generated_dir.join("runtime/search.mjs")).unwrap(),
            RUNTIME_SEARCH
        );
        assert!(
            !generated
                .generated_dir
                .join("runtime/components.tsx")
                .exists()
        );
        assert!(
            !generated
                .generated_dir
                .join("runtime/components.css")
                .exists()
        );
        assert!(
            !generated
                .generated_dir
                .join("runtime/components-state.mjs")
                .exists()
        );
    }

    #[test]
    fn component_metadata_only_reads_top_level_overrides() {
        let source = r#"
            export const meta = {
                args: { group: "Nested", component: "Nested", order: 99 },
                // Only these top-level values describe the catalog entry.
                group: "Actions",
                component: "Button",
                order: 3,
            };
        "#;

        let meta = extract_static_demo_meta(source);
        assert_eq!(meta.group.as_deref(), Some("Actions"));
        assert_eq!(meta.component.as_deref(), Some("Button"));
        assert_eq!(meta.order, 3);
        assert_eq!(
            static_demo_args(source),
            Some(r#"{ group: "Nested", component: "Nested", order: 99 }"#)
        );
    }

    #[test]
    fn component_mode_warns_and_ignores_dynamic_meta_args() {
        let root = fixture();
        fs::write(
            root.join("docs/demos/basic.demo.tsx"),
            r#"
                import type { ButtonProps } from "../../src/button";
                const dynamicLabel = () => "Create";
                export const meta = { args: { label: dynamicLabel() } };
                export default function Demo(props: ButtonProps) {
                    return <button>{props.label}</button>;
                }
            "#,
        )
        .unwrap();

        let generated = generate_with_mode(
            &root,
            &DocsOptions::default(),
            BuildMode::Production,
            DocsMode::Components,
        )
        .unwrap();
        assert_eq!(generated.demos.len(), 1);
        assert!(
            generated
                .warnings
                .iter()
                .any(|warning| warning.contains("meta.args"))
        );
        assert_eq!(generated.demos[0].warnings, generated.warnings);
    }
}
