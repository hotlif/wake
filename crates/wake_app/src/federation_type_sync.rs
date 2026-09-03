//! Fail-closed host synchronization for build-bound Federation declarations.
//!
//! This module only performs explicit control-plane I/O. Callers decide when a remote
//! fetch is appropriate (development performs a startup sync and manifest-gated refreshes), and
//! an error never silently selects an older declaration package.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::digest::{SHA384, digest};
use wake_federation_contract::{
    BuildId, ContainerName, ErrorCode, FederationConfig, FederationLock, Manifest, RemoteConfig,
    RemoteRef,
};

use super::federation_types::{
    DeclarationBundle, FEDERATION_TYPE_BUNDLE_SCHEMA_VERSION, render_ambient_declaration,
    source_module_namespace,
};
use super::{WakeError, atomic_write};

const TYPES_ROOT: &str = ".wake/federation/types";
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_TYPE_BUNDLE_BYTES: usize = 16 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// One exact remote declaration build published by a synchronization pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedFederationTypes {
    pub remote: ContainerName,
    pub build_id: BuildId,
    /// SHA-384 content hash advertised by the exact build-bound type artifact.
    pub types_hash: String,
    pub declaration_file: PathBuf,
}

/// Deterministic output paths from one successful all-remotes synchronization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FederationTypeSyncResult {
    pub remotes: Vec<SyncedFederationTypes>,
    pub index_file: PathBuf,
}

/// Minimal manifest identity used by the development control-plane monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FederationTypeRevision {
    pub build_id: BuildId,
    pub types_hash: String,
}

pub(crate) type FederationTypeRevisions = BTreeMap<ContainerName, FederationTypeRevision>;

/// Fetch and publish declarations for every configured remote that has public exposes.
///
/// All manifests and declaration bundles are fetched and validated before the
/// stable `index.d.ts` is replaced. Existing build-scoped files are immutable:
/// the same `(remote, buildId)` may only resolve to byte-identical declarations. Shared-only
/// remotes are validated but contribute no declaration entry.
pub fn sync_federation_types(
    project_root: &Path,
    config: &FederationConfig,
) -> Result<FederationTypeSyncResult, WakeError> {
    sync_federation_types_with(project_root, config, &UreqResourceFetcher)
}

/// Synchronize editor declarations while enforcing production locks for development-pinned
/// remotes. Followed remotes intentionally remain independent of the lock and can advance.
pub(crate) fn sync_federation_types_for_development(
    project_root: &Path,
    config: &FederationConfig,
    lock: Option<&FederationLock>,
) -> Result<FederationTypeSyncResult, WakeError> {
    sync_federation_types_with_constraints(project_root, config, lock, &UreqResourceFetcher)
}

/// Fetch only followed manifests and return their build/type identities.
///
/// This is deliberately separate from [`sync_federation_types`]: an unchanged development
/// remote costs one small control-plane response and never downloads its declaration bundle.
pub(crate) fn probe_followed_federation_type_revisions(
    config: &FederationConfig,
) -> Result<FederationTypeRevisions, WakeError> {
    probe_followed_federation_type_revisions_with(config, &UreqResourceFetcher)
}

pub(crate) fn followed_type_revisions(
    config: &FederationConfig,
    result: &FederationTypeSyncResult,
) -> FederationTypeRevisions {
    result
        .remotes
        .iter()
        .filter(|output| {
            config
                .remotes
                .get(&output.remote)
                .is_some_and(|remote| remote.dev_follow)
        })
        .map(|output| {
            (
                output.remote.clone(),
                FederationTypeRevision {
                    build_id: output.build_id.clone(),
                    types_hash: output.types_hash.clone(),
                },
            )
        })
        .collect()
}

#[derive(Debug, Clone)]
struct ResourceResponse {
    status: u16,
    content_type: Option<String>,
    content_length: Option<u64>,
    body: Vec<u8>,
}

trait ResourceFetcher {
    fn fetch(&self, url: String, maximum_bytes: usize) -> Result<ResourceResponse, WakeError>;
}

struct UreqResourceFetcher;

impl ResourceFetcher for UreqResourceFetcher {
    fn fetch(&self, url: String, maximum_bytes: usize) -> Result<ResourceResponse, WakeError> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(FETCH_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .new_agent();
        let mut response = agent.get(&url).call().map_err(|error| {
            federation_error(
                ErrorCode::Network,
                format!("could not fetch federation resource `{url}`: {error}"),
            )
        })?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .map(|value| {
                value.to_str().map(str::to_owned).map_err(|error| {
                    federation_error(
                        ErrorCode::AssetMime,
                        format!("federation resource `{url}` has an invalid Content-Type: {error}"),
                    )
                })
            })
            .transpose()?;
        let content_length = response
            .headers()
            .get("content-length")
            .map(|value| {
                value
                    .to_str()
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .ok_or_else(|| {
                        federation_error(
                            ErrorCode::AssetSize,
                            format!("federation resource `{url}` has an invalid Content-Length"),
                        )
                    })
            })
            .transpose()?;
        if content_length.is_some_and(|size| size > maximum_bytes as u64) {
            return Err(resource_size_error(&url, maximum_bytes));
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(maximum_bytes.saturating_add(1) as u64)
            .read_to_vec()
            .map_err(|error| {
                federation_error(
                    ErrorCode::Network,
                    format!("could not read federation resource `{url}`: {error}"),
                )
            })?;
        Ok(ResourceResponse {
            status,
            content_type,
            content_length,
            body,
        })
    }
}

#[derive(Debug)]
struct PreparedTypes {
    remote: ContainerName,
    build_id: BuildId,
    types_hash: String,
    declaration_file: PathBuf,
    declaration_bytes: Vec<u8>,
}

fn sync_federation_types_with(
    project_root: &Path,
    config: &FederationConfig,
    fetcher: &impl ResourceFetcher,
) -> Result<FederationTypeSyncResult, WakeError> {
    sync_federation_types_with_constraints(project_root, config, None, fetcher)
}

fn sync_federation_types_with_constraints(
    project_root: &Path,
    config: &FederationConfig,
    lock: Option<&FederationLock>,
    fetcher: &impl ResourceFetcher,
) -> Result<FederationTypeSyncResult, WakeError> {
    config.validate().map_err(|error| {
        federation_error(
            ErrorCode::ConfigInvalid,
            format!("invalid federation configuration: {error}"),
        )
    })?;

    let mut prepared = Vec::with_capacity(config.remotes.len());
    for (expected_name, remote) in &config.remotes {
        let locked = locked_development_remote(expected_name, remote, lock)?;
        let Some(mut output) = fetch_remote_types(expected_name, remote, locked, fetcher)? else {
            continue;
        };
        output.declaration_file = project_root.join(&output.declaration_file);
        prepared.push(output);
    }

    // Detect immutable-path collisions for every remote before writing any file.
    for output in &prepared {
        validate_existing_declaration(output)?;
    }
    for output in &prepared {
        write_declaration_if_missing(output)?;
    }

    let index_file = project_root.join(TYPES_ROOT).join("index.d.ts");
    let index_bytes = render_type_index(&prepared).into_bytes();
    write_if_changed(&index_file, &index_bytes)?;

    Ok(FederationTypeSyncResult {
        remotes: prepared
            .into_iter()
            .map(|output| SyncedFederationTypes {
                remote: output.remote,
                build_id: output.build_id,
                types_hash: output.types_hash,
                declaration_file: output.declaration_file,
            })
            .collect(),
        index_file,
    })
}

fn probe_followed_federation_type_revisions_with(
    config: &FederationConfig,
    fetcher: &impl ResourceFetcher,
) -> Result<FederationTypeRevisions, WakeError> {
    config.validate().map_err(|error| {
        federation_error(
            ErrorCode::ConfigInvalid,
            format!("invalid federation configuration: {error}"),
        )
    })?;

    let mut revisions = BTreeMap::new();
    for (expected_name, remote) in &config.remotes {
        if !remote.dev_follow {
            continue;
        }
        let fetched = fetch_validated_manifest(expected_name, remote, None, fetcher)?;
        if fetched.manifest.exposes.is_empty() {
            continue;
        }
        let types = required_types(expected_name, &fetched.manifest)?;
        revisions.insert(
            expected_name.clone(),
            FederationTypeRevision {
                build_id: fetched.manifest.build_id.clone(),
                types_hash: types.content_hash.clone(),
            },
        );
    }
    Ok(revisions)
}

struct FetchedManifest {
    manifest: Manifest,
    manifest_url: HttpUrl,
    allowed_origins: BTreeSet<String>,
}

fn fetch_remote_types(
    expected_name: &ContainerName,
    remote: &RemoteConfig,
    locked: Option<&RemoteRef>,
    fetcher: &impl ResourceFetcher,
) -> Result<Option<PreparedTypes>, WakeError> {
    let fetched = fetch_validated_manifest(expected_name, remote, locked, fetcher)?;
    let manifest = fetched.manifest;
    let manifest_url = fetched.manifest_url;
    let allowed_origins = fetched.allowed_origins;
    if manifest.exposes.is_empty() {
        return Ok(None);
    }
    let types = required_types(expected_name, &manifest)?;
    if let Some(locked) = locked {
        let locked_integrity = locked.types_integrity.as_deref().ok_or_else(|| {
            federation_error(
                ErrorCode::TypeBuildMismatch,
                format!("development-pinned remote `{expected_name}` has no locked type artifact"),
            )
        })?;
        if types.integrity != locked_integrity {
            return Err(federation_error(
                ErrorCode::TypeBuildMismatch,
                format!(
                    "type artifact for development-pinned remote `{expected_name}` does not match the lock"
                ),
            ));
        }
    }
    let types_hash = types.content_hash.clone();
    let build_id = manifest.build_id.clone();

    let declared_size = usize::try_from(types.size)
        .map_err(|_| resource_size_error(&types.url, MAX_TYPE_BUNDLE_BYTES))?;
    if declared_size == 0 || declared_size > MAX_TYPE_BUNDLE_BYTES {
        return Err(resource_size_error(&types.url, MAX_TYPE_BUNDLE_BYTES));
    }
    let types_url = manifest_url.join(&types.url)?;
    if !allowed_origins.contains(&types_url.origin) {
        return Err(federation_error(
            ErrorCode::OriginDenied,
            format!(
                "federation types `{}` resolve outside the configured allowed origins",
                types_url.serialized
            ),
        ));
    }

    let types_response = fetcher.fetch(types_url.serialized.clone(), declared_size)?;
    validate_json_response(
        &types_url.serialized,
        &types_response,
        declared_size,
        Some(types.size),
    )?;
    let (content_hash, integrity) = sha384_digests(&types_response.body);
    if integrity != types.integrity || content_hash != types.content_hash {
        return Err(federation_error(
            ErrorCode::AssetIntegrity,
            format!(
                "federation declarations for `{expected_name}` do not match their SHA-384 metadata"
            ),
        ));
    }

    let bundle =
        serde_json::from_slice::<DeclarationBundle>(&types_response.body).map_err(|error| {
            federation_error(
                ErrorCode::ManifestSchema,
                format!("declaration bundle for `{expected_name}` is invalid JSON: {error}"),
            )
        })?;
    validate_declaration_bundle(expected_name, &manifest, &bundle)?;
    let declaration =
        render_ambient_declaration(&manifest.name, &manifest.build_id, &bundle.modules);

    let declaration_file = PathBuf::from(TYPES_ROOT)
        .join(expected_name.as_str())
        .join(manifest.build_id.as_str())
        .join("index.d.ts");
    Ok(Some(PreparedTypes {
        remote: expected_name.clone(),
        build_id,
        types_hash,
        declaration_file,
        declaration_bytes: declaration.into_bytes(),
    }))
}

fn fetch_validated_manifest(
    expected_name: &ContainerName,
    remote: &RemoteConfig,
    locked: Option<&RemoteRef>,
    fetcher: &impl ResourceFetcher,
) -> Result<FetchedManifest, WakeError> {
    let manifest_url = HttpUrl::parse(&remote.manifest_url, "manifestUrl")?;
    let allowed_origins = allowed_origins(remote, &manifest_url)?;
    let manifest_response = fetcher.fetch(remote.manifest_url.clone(), MAX_MANIFEST_BYTES)?;
    validate_json_response(
        &remote.manifest_url,
        &manifest_response,
        MAX_MANIFEST_BYTES,
        None,
    )?;
    if let Some(locked) = locked {
        let observed_integrity = sha384_digests(&manifest_response.body).1;
        if observed_integrity != locked.manifest_integrity {
            return Err(federation_error(
                ErrorCode::ManifestIntegrity,
                format!(
                    "manifest for development-pinned remote `{expected_name}` does not match the lock"
                ),
            ));
        }
    }
    let manifest =
        serde_json::from_slice::<Manifest>(&manifest_response.body).map_err(|error| {
            federation_error(
                ErrorCode::ManifestSchema,
                format!("federation manifest for `{expected_name}` is not valid v1 JSON: {error}"),
            )
        })?;
    manifest.validate().map_err(|error| {
        let code = if error
            .violations
            .iter()
            .any(|violation| violation.code == ErrorCode::RuntimeAbi)
        {
            ErrorCode::RuntimeAbi
        } else if error
            .violations
            .iter()
            .any(|violation| violation.code == ErrorCode::TypeBuildMismatch)
        {
            ErrorCode::TypeBuildMismatch
        } else {
            ErrorCode::ManifestSchema
        };
        federation_error(
            code,
            format!("invalid federation manifest for `{expected_name}`: {error}"),
        )
    })?;
    if &manifest.name != expected_name {
        return Err(federation_error(
            ErrorCode::ManifestSchema,
            format!(
                "configured remote `{expected_name}` returned manifest for `{}`",
                manifest.name
            ),
        ));
    }
    if let Some(locked) = locked
        && manifest.build_id != locked.build_id
    {
        return Err(WakeError::new(
            "FED_LOCK_MISMATCH",
            format!(
                "manifest buildId for development-pinned remote `{expected_name}` does not match the lock: `{}` != `{}`",
                manifest.build_id, locked.build_id
            ),
        ));
    }
    let manifest_has_exposes = !manifest.exposes.is_empty();
    if let Some(locked) = locked
        && locked.has_exposes != manifest_has_exposes
    {
        return Err(WakeError::new(
            "FED_LOCK_MISMATCH",
            format!(
                "manifest expose presence for development-pinned remote `{expected_name}` does not match the lock"
            ),
        ));
    }
    validate_filesystem_build_id(&manifest.build_id)?;
    Ok(FetchedManifest {
        manifest,
        manifest_url,
        allowed_origins,
    })
}

fn locked_development_remote<'a>(
    expected_name: &ContainerName,
    remote: &RemoteConfig,
    lock: Option<&'a FederationLock>,
) -> Result<Option<&'a RemoteRef>, WakeError> {
    if remote.dev_follow {
        return Ok(None);
    }
    let lock = lock.ok_or_else(|| {
        WakeError::new(
            "FED_LOCK_REQUIRED",
            format!("development-pinned remote `{expected_name}` requires wake-federation.lock"),
        )
    })?;
    let locked = lock.remotes.get(expected_name).ok_or_else(|| {
        WakeError::new(
            "FED_LOCK_MISMATCH",
            format!("development-pinned remote `{expected_name}` is missing from the lock"),
        )
    })?;
    if locked.manifest_url != remote.manifest_url {
        return Err(WakeError::new(
            "FED_LOCK_MISMATCH",
            format!(
                "locked manifest URL for development-pinned remote `{expected_name}` does not match configuration"
            ),
        ));
    }
    Ok(Some(locked))
}

fn required_types<'a>(
    expected_name: &ContainerName,
    manifest: &'a Manifest,
) -> Result<&'a wake_federation_contract::TypeArtifact, WakeError> {
    manifest.types.as_ref().ok_or_else(|| {
        federation_error(
            ErrorCode::TypeBuildMismatch,
            format!(
                "federation manifest for `{expected_name}` does not publish build-bound declarations"
            ),
        )
    })
}

fn validate_declaration_bundle(
    expected_name: &ContainerName,
    manifest: &Manifest,
    bundle: &DeclarationBundle,
) -> Result<(), WakeError> {
    if bundle.schema_version != FEDERATION_TYPE_BUNDLE_SCHEMA_VERSION {
        return Err(type_schema_error(
            expected_name,
            format!(
                "expected schema `{FEDERATION_TYPE_BUNDLE_SCHEMA_VERSION}`, got `{}`",
                bundle.schema_version
            ),
        ));
    }
    if bundle.name != manifest.name.as_str() || bundle.build_id != manifest.build_id.as_str() {
        return Err(federation_error(
            ErrorCode::TypeBuildMismatch,
            format!(
                "declaration identity `{}`@`{}` does not match manifest {}@{}",
                bundle.name, bundle.build_id, manifest.name, manifest.build_id
            ),
        ));
    }

    let expected_exposes = manifest
        .exposes
        .keys()
        .map(|expose| {
            (
                expose.as_str().to_owned(),
                format!(
                    "{}/{}",
                    manifest.name,
                    expose.as_str().trim_start_matches("./")
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if bundle.exposes != expected_exposes {
        return Err(federation_error(
            ErrorCode::TypeBuildMismatch,
            format!(
                "declaration exposes for `{expected_name}` do not exactly cover the manifest exposes"
            ),
        ));
    }
    if bundle.modules.is_empty() {
        return Err(type_schema_error(
            expected_name,
            "declaration bundle does not contain any modules",
        ));
    }
    for public_specifier in bundle.exposes.values() {
        if !bundle.modules.contains_key(public_specifier) {
            return Err(federation_error(
                ErrorCode::TypeBuildMismatch,
                format!(
                    "declaration bundle for `{expected_name}` is missing public module `{public_specifier}`"
                ),
            ));
        }
    }
    let public_specifiers = bundle.exposes.values().collect::<BTreeSet<_>>();
    let source_namespace = source_module_namespace(&manifest.name, &manifest.build_id);
    for (specifier, body) in &bundle.modules {
        if specifier.is_empty() || specifier.len() > 4096 || specifier.chars().any(char::is_control)
        {
            return Err(type_schema_error(
                expected_name,
                format!("invalid declaration module specifier `{specifier}`"),
            ));
        }
        if !public_specifiers.contains(specifier)
            && (!specifier.starts_with(&source_namespace)
                || specifier.len() == source_namespace.len())
        {
            return Err(type_schema_error(
                expected_name,
                format!(
                    "declaration module `{specifier}` is outside the build-owned module namespace"
                ),
            ));
        }
        match wake_tsdoc::validate_ambient_declaration_body(Path::new("remote-module.d.ts"), body) {
            Ok(facts) if facts.contains_forbidden_any() => {
                return Err(federation_error(
                    ErrorCode::TypeBuildMismatch,
                    format!(
                        "declaration module `{specifier}` contains the forbidden public `any` type"
                    ),
                ));
            }
            Ok(_) => {}
            Err(error) => {
                return Err(type_schema_error(
                    expected_name,
                    format!("declaration module `{specifier}` is invalid: {error}"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_json_response(
    url: &str,
    response: &ResourceResponse,
    maximum_bytes: usize,
    exact_size: Option<u64>,
) -> Result<(), WakeError> {
    if response.status != 200 {
        return Err(federation_error(
            ErrorCode::Network,
            format!(
                "federation resource `{url}` returned HTTP {} (expected 200)",
                response.status
            ),
        ));
    }
    let content_type = response.content_type.as_deref().ok_or_else(|| {
        federation_error(
            ErrorCode::AssetMime,
            format!("federation resource `{url}` is missing Content-Type"),
        )
    })?;
    if !is_json_content_type(content_type) {
        return Err(federation_error(
            ErrorCode::AssetMime,
            format!("federation resource `{url}` must use application/json, got `{content_type}`"),
        ));
    }
    let observed_size = response.body.len() as u64;
    if response.body.len() > maximum_bytes
        || response
            .content_length
            .is_some_and(|length| length != observed_size || length > maximum_bytes as u64)
        || exact_size.is_some_and(|size| size != observed_size)
    {
        return Err(resource_size_error(url, maximum_bytes));
    }
    Ok(())
}

fn is_json_content_type(value: &str) -> bool {
    value
        .split_once(';')
        .map_or(value, |(essence, _)| essence)
        .trim()
        .eq_ignore_ascii_case("application/json")
}

fn allowed_origins(
    remote: &RemoteConfig,
    manifest_url: &HttpUrl,
) -> Result<BTreeSet<String>, WakeError> {
    if remote.allowed_origins.is_empty() {
        return Ok(BTreeSet::from([manifest_url.origin.clone()]));
    }
    let mut origins = BTreeSet::new();
    for origin in &remote.allowed_origins {
        let parsed = HttpUrl::parse(origin, "allowedOrigins")?;
        if parsed.path != "/" || parsed.query.is_some() {
            return Err(federation_error(
                ErrorCode::OriginDenied,
                format!("allowed origin `{origin}` must not contain a path or query"),
            ));
        }
        origins.insert(parsed.origin);
    }
    if !origins.contains(&manifest_url.origin) {
        return Err(federation_error(
            ErrorCode::OriginDenied,
            format!(
                "manifest origin `{}` is not in the configured allowed origins",
                manifest_url.origin
            ),
        ));
    }
    Ok(origins)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpUrl {
    serialized: String,
    scheme: String,
    origin: String,
    path: String,
    query: Option<String>,
}

impl HttpUrl {
    fn parse(value: &str, field: &str) -> Result<Self, WakeError> {
        if value.trim() != value
            || value.is_empty()
            || value.contains(['\\', '#'])
            || value.chars().any(char::is_control)
        {
            return Err(invalid_http_url(field, value));
        }
        let uri = value
            .parse::<ureq::http::Uri>()
            .map_err(|_| invalid_http_url(field, value))?;
        let scheme = match uri.scheme_str() {
            Some("http") => "http",
            Some("https") => "https",
            _ => return Err(invalid_http_url(field, value)),
        };
        let authority = uri
            .authority()
            .ok_or_else(|| invalid_http_url(field, value))?;
        if authority.as_str().contains('@') {
            return Err(invalid_http_url(field, value));
        }
        let host = authority.host().to_ascii_lowercase();
        if !is_valid_host(&host) || (authority.port().is_some() && authority.port_u16().is_none()) {
            return Err(invalid_http_url(field, value));
        }
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        };
        let port = authority.port_u16();
        let default_port =
            (scheme == "https" && port == Some(443)) || (scheme == "http" && port == Some(80));
        let origin = match port {
            None => format!("{scheme}://{host}"),
            Some(_) if default_port => format!("{scheme}://{host}"),
            Some(port) => format!("{scheme}://{host}:{port}"),
        };
        let path = normalize_url_path(uri.path(), field, value)?;
        let query = uri.query().map(str::to_owned);
        let serialized = serialize_url(&origin, &path, query.as_deref());
        Ok(Self {
            serialized,
            scheme: scheme.to_owned(),
            origin,
            path,
            query,
        })
    }

    fn join(&self, value: &str) -> Result<Self, WakeError> {
        if value.trim() != value
            || value.is_empty()
            || value.contains(['\\', '#'])
            || value.chars().any(char::is_control)
        {
            return Err(invalid_http_url("types.url", value));
        }
        if value.starts_with("http://") || value.starts_with("https://") {
            return Self::parse(value, "types.url");
        }
        if value.starts_with("//") {
            return Self::parse(&format!("{}:{value}", self.scheme), "types.url");
        }
        if value.contains("://") {
            return Err(invalid_http_url("types.url", value));
        }
        let (raw_path, query) = value
            .split_once('?')
            .map_or((value, None), |(path, query)| {
                (path, Some(query.to_owned()))
            });
        if !raw_path.starts_with('/')
            && raw_path
                .split('/')
                .next()
                .is_some_and(|segment| segment.contains(':'))
        {
            return Err(invalid_http_url("types.url", value));
        }
        let path = if raw_path.is_empty() {
            self.path.clone()
        } else if raw_path.starts_with('/') {
            normalize_url_path(raw_path, "types.url", value)?
        } else {
            let directory = self
                .path
                .rsplit_once('/')
                .map_or("/", |(directory, _)| directory);
            normalize_url_path(&format!("{directory}/{raw_path}"), "types.url", value)?
        };
        let serialized = serialize_url(&self.origin, &path, query.as_deref());
        Ok(Self {
            serialized,
            scheme: self.scheme.clone(),
            origin: self.origin.clone(),
            path,
            query,
        })
    }
}

fn normalize_url_path(path: &str, field: &str, original: &str) -> Result<String, WakeError> {
    let path = if path.is_empty() { "/" } else { path };
    if !path.starts_with('/') {
        return Err(invalid_http_url(field, original));
    }
    let lowercase = path.to_ascii_lowercase();
    if lowercase.contains("%2e") || lowercase.contains("%2f") || lowercase.contains("%5c") {
        return Err(invalid_http_url(field, original));
    }
    let mut segments = Vec::new();
    for (index, segment) in path.split('/').enumerate() {
        match segment {
            "" if index == 0 => {}
            "." => {}
            ".." => {
                segments.pop();
            }
            value => segments.push(value),
        }
    }
    if (path.ends_with("/.") || path.ends_with("/.."))
        && segments.last().is_some_and(|last| !last.is_empty())
    {
        segments.push("");
    }
    Ok(format!("/{}", segments.join("/")))
}

fn is_valid_host(host: &str) -> bool {
    if let Some(ipv6) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        return !ipv6.is_empty()
            && ipv6
                .chars()
                .all(|character| character.is_ascii_hexdigit() || matches!(character, ':' | '.'));
    }
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
                && label
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_alphanumeric())
                && label
                    .chars()
                    .last()
                    .is_some_and(|character| character.is_ascii_alphanumeric())
        })
}

fn serialize_url(origin: &str, path: &str, query: Option<&str>) -> String {
    let mut value = format!("{origin}{path}");
    if let Some(query) = query {
        value.push('?');
        value.push_str(query);
    }
    value
}

fn validate_filesystem_build_id(build_id: &BuildId) -> Result<(), WakeError> {
    let value = build_id.as_str();
    if value.is_empty()
        || value.len() > 256
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(federation_error(
            ErrorCode::ManifestSchema,
            format!("federation buildId `{value}` is not a safe declaration path segment"),
        ));
    }
    Ok(())
}

fn validate_existing_declaration(output: &PreparedTypes) -> Result<(), WakeError> {
    match std::fs::read(&output.declaration_file) {
        Ok(existing) if existing == output.declaration_bytes => Ok(()),
        Ok(_) => Err(federation_error(
            ErrorCode::TypeBuildMismatch,
            format!(
                "immutable federation declarations at `{}` disagree with {}@{}",
                output.declaration_file.display(),
                output.remote,
                output.build_id
            ),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(WakeError::new("WAKE_IO", error.to_string()).at(&output.declaration_file))
        }
    }
}

fn write_declaration_if_missing(output: &PreparedTypes) -> Result<(), WakeError> {
    if output.declaration_file.is_file() {
        return Ok(());
    }
    atomic_write(&output.declaration_file, &output.declaration_bytes)
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<(), WakeError> {
    if std::fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(());
    }
    atomic_write(path, bytes)
}

fn render_type_index(outputs: &[PreparedTypes]) -> String {
    let mut index = String::from("// Generated by Wake Federation; do not edit.\n");
    for output in outputs {
        index.push_str("/// <reference path=\"./");
        index.push_str(output.remote.as_str());
        index.push('/');
        index.push_str(output.build_id.as_str());
        index.push_str("/index.d.ts\" />\n");
    }
    index
}

fn sha384_digests(bytes: &[u8]) -> (String, String) {
    let hash = digest(&SHA384, bytes);
    let raw = hash.as_ref();
    let mut hex = String::with_capacity(raw.len() * 2);
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for byte in raw {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    (hex, format!("sha384-{}", BASE64.encode(raw)))
}

fn resource_size_error(url: &str, maximum_bytes: usize) -> WakeError {
    federation_error(
        ErrorCode::AssetSize,
        format!("federation resource `{url}` violates the {maximum_bytes} byte size limit"),
    )
}

fn invalid_http_url(field: &str, value: &str) -> WakeError {
    federation_error(
        ErrorCode::OriginDenied,
        format!("{field} must be an unambiguous absolute HTTP(S) URL: `{value}`"),
    )
}

fn type_schema_error(remote: &ContainerName, message: impl Into<String>) -> WakeError {
    federation_error(
        ErrorCode::ManifestSchema,
        format!(
            "invalid declaration bundle for `{remote}`: {}",
            message.into()
        ),
    )
}

fn federation_error(code: ErrorCode, message: impl Into<String>) -> WakeError {
    WakeError::new(code.as_str(), message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use wake_federation_contract::{
        Asset, AssetKind, ExposeKey, ExposeMode, ExposedModule, ShadowMode, TypeArtifact,
        TypeArtifactFormat,
    };

    struct Fixture {
        root: tempfile::TempDir,
        config: FederationConfig,
        responses: BTreeMap<String, ResourceResponse>,
    }

    impl Fixture {
        fn new(remotes: &[&str]) -> Self {
            let root = tempfile::tempdir().unwrap();
            let mut config = FederationConfig {
                enabled: true,
                name: ContainerName::from("shell"),
                ..FederationConfig::default()
            };
            let mut responses = BTreeMap::new();
            for remote_name in remotes {
                let manifest_url = format!("https://{remote_name}.test/releases/manifest.json");
                let types_url = format!("https://{remote_name}.test/releases/types.json");
                let (manifest, bundle) = manifest_and_bundle(remote_name, "build-a");
                responses.insert(types_url, json_response(bundle));
                responses.insert(
                    manifest_url.clone(),
                    json_response(serde_json::to_vec(&manifest).unwrap()),
                );
                config.remotes.insert(
                    ContainerName::from(*remote_name),
                    RemoteConfig {
                        manifest_url,
                        allowed_origins: Vec::new(),
                        dev_follow: true,
                    },
                );
            }
            Self {
                root,
                config,
                responses,
            }
        }

        fn fetcher<'a>(&'a self, fetches: &'a Cell<usize>) -> impl ResourceFetcher + 'a {
            move |url: String, _maximum_bytes: usize| {
                fetches.set(fetches.get() + 1);
                self.responses.get(&url).cloned().ok_or_else(|| {
                    federation_error(ErrorCode::Network, format!("missing fixture URL `{url}`"))
                })
            }
        }
    }

    impl<F> ResourceFetcher for F
    where
        F: Fn(String, usize) -> Result<ResourceResponse, WakeError>,
    {
        fn fetch(&self, url: String, maximum_bytes: usize) -> Result<ResourceResponse, WakeError> {
            self(url, maximum_bytes)
        }
    }

    fn manifest_and_bundle(name: &str, build_id: &str) -> (Manifest, Vec<u8>) {
        let mut modules = BTreeMap::new();
        modules.insert(
            format!("{name}/Button"),
            "export interface ButtonProps { label: string; }\nexport const Button: (props: ButtonProps) => unknown;\n"
                .to_owned(),
        );
        let bundle = DeclarationBundle {
            schema_version: FEDERATION_TYPE_BUNDLE_SCHEMA_VERSION.to_owned(),
            name: name.to_owned(),
            build_id: build_id.to_owned(),
            exposes: BTreeMap::from([("./Button".to_owned(), format!("{name}/Button"))]),
            modules,
        };
        let mut bundle = serde_json::to_vec_pretty(&bundle).unwrap();
        bundle.push(b'\n');
        let (content_hash, integrity) = sha384_digests(&bundle);
        let mut manifest = Manifest::new(
            ContainerName::from(name),
            BuildId::from(build_id),
            "chrome120",
            Asset::new(
                AssetKind::JavaScript,
                "./remoteEntry.mjs",
                "remote-hash",
                format!("sha384-{}", "A".repeat(64)),
                "text/javascript",
                10,
            ),
        );
        manifest.exposes.insert(
            ExposeKey::from("./Button"),
            ExposedModule {
                mode: ExposeMode::Generic,
                scope: "default".to_owned(),
                shadow: ShadowMode::None,
                entry: Asset::new(
                    AssetKind::JavaScript,
                    "./button.mjs",
                    "button-hash",
                    format!("sha384-{}", "B".repeat(64)),
                    "text/javascript",
                    10,
                ),
                ..ExposedModule::default()
            },
        );
        manifest.types = Some(TypeArtifact {
            build_id: BuildId::from(build_id),
            url: "./types.json".to_owned(),
            content_hash,
            integrity,
            size: bundle.len() as u64,
            format: TypeArtifactFormat::DeclarationBundle,
        });
        (manifest, bundle)
    }

    fn json_response(body: Vec<u8>) -> ResourceResponse {
        ResourceResponse {
            status: 200,
            content_type: Some("application/json; charset=utf-8".to_owned()),
            content_length: Some(body.len() as u64),
            body,
        }
    }

    fn replace_manifest(fixture: &mut Fixture, remote: &str, manifest: &Manifest) {
        let url = fixture.config.remotes[&ContainerName::from(remote)]
            .manifest_url
            .clone();
        fixture
            .responses
            .insert(url, json_response(serde_json::to_vec(manifest).unwrap()));
    }

    fn replace_public_declaration_body(fixture: &mut Fixture, remote: &str, body: &str) {
        let types_url = format!("https://{remote}.test/releases/types.json");
        let mut bundle =
            serde_json::from_slice::<DeclarationBundle>(&fixture.responses[&types_url].body)
                .unwrap();
        bundle
            .modules
            .insert(format!("{remote}/Button"), body.to_owned());
        let mut bytes = serde_json::to_vec_pretty(&bundle).unwrap();
        bytes.push(b'\n');
        let (content_hash, integrity) = sha384_digests(&bytes);
        let manifest_url = fixture.config.remotes[&ContainerName::from(remote)]
            .manifest_url
            .clone();
        let mut manifest =
            serde_json::from_slice::<Manifest>(&fixture.responses[&manifest_url].body).unwrap();
        let types = manifest.types.as_mut().unwrap();
        types.content_hash = content_hash;
        types.integrity = integrity;
        types.size = bytes.len() as u64;
        replace_manifest(fixture, remote, &manifest);
        fixture.responses.insert(types_url, json_response(bytes));
    }

    fn lock_for_fixture(fixture: &Fixture, remote: &str) -> FederationLock {
        let name = ContainerName::from(remote);
        let manifest_url = fixture.config.remotes[&name].manifest_url.clone();
        let manifest_response = &fixture.responses[&manifest_url];
        let manifest = serde_json::from_slice::<Manifest>(&manifest_response.body).unwrap();
        let types = manifest.types.as_ref().unwrap();
        let mut lock = FederationLock::new();
        lock.remotes.insert(
            name,
            RemoteRef {
                manifest_url,
                build_id: manifest.build_id.clone(),
                manifest_integrity: sha384_digests(&manifest_response.body).1,
                has_exposes: true,
                types_integrity: Some(types.integrity.clone()),
                allowed_assets: BTreeMap::from([(
                    format!("https://{remote}.test/releases/remoteEntry.mjs"),
                    manifest.remote_entry.integrity.clone(),
                )]),
            },
        );
        lock
    }

    #[test]
    fn synchronizes_build_scoped_declarations_and_a_sorted_stable_index() {
        let fixture = Fixture::new(&["zeta", "catalog"]);
        let fetches = Cell::new(0);
        let fetcher = fixture.fetcher(&fetches);
        let first =
            sync_federation_types_with(fixture.root.path(), &fixture.config, &fetcher).unwrap();
        assert_eq!(fetches.get(), 4);
        assert_eq!(
            first
                .remotes
                .iter()
                .map(|remote| remote.remote.as_str())
                .collect::<Vec<_>>(),
            ["catalog", "zeta"]
        );
        let index = std::fs::read_to_string(&first.index_file).unwrap();
        assert_eq!(
            index,
            "// Generated by Wake Federation; do not edit.\n\
/// <reference path=\"./catalog/build-a/index.d.ts\" />\n\
/// <reference path=\"./zeta/build-a/index.d.ts\" />\n"
        );
        let catalog = std::fs::read_to_string(&first.remotes[0].declaration_file).unwrap();
        assert!(catalog.contains("declare module \"catalog/Button\""));
        wake_tsdoc::validate_declaration_body(Path::new("catalog.d.ts"), &catalog).unwrap();

        let index_bytes = std::fs::read(&first.index_file).unwrap();
        let second =
            sync_federation_types_with(fixture.root.path(), &fixture.config, &fetcher).unwrap();
        assert_eq!(
            fetches.get(),
            8,
            "idempotent sync still refreshes remote control data"
        );
        assert_eq!(first, second);
        assert_eq!(std::fs::read(&second.index_file).unwrap(), index_bytes);
    }

    #[test]
    fn shared_only_remotes_publish_no_editor_types() {
        let mut fixture = Fixture::new(&["catalog"]);
        let (mut manifest, _) = manifest_and_bundle("catalog", "build-a");
        manifest.exposes.clear();
        manifest.types = None;
        replace_manifest(&mut fixture, "catalog", &manifest);
        let fetches = Cell::new(0);

        let synchronized = sync_federation_types_with(
            fixture.root.path(),
            &fixture.config,
            &fixture.fetcher(&fetches),
        )
        .unwrap();

        assert_eq!(
            fetches.get(),
            1,
            "shared-only remotes fetch only their Manifest"
        );
        assert!(synchronized.remotes.is_empty());
        assert_eq!(
            std::fs::read_to_string(synchronized.index_file).unwrap(),
            "// Generated by Wake Federation; do not edit.\n"
        );
    }

    #[test]
    fn pinned_manifest_expose_presence_must_match_the_lock() {
        let mut fixture = Fixture::new(&["catalog"]);
        fixture
            .config
            .remotes
            .get_mut(&ContainerName::from("catalog"))
            .unwrap()
            .dev_follow = false;
        let mut lock = lock_for_fixture(&fixture, "catalog");
        lock.remotes
            .get_mut(&ContainerName::from("catalog"))
            .unwrap()
            .has_exposes = false;

        let error = sync_federation_types_with_constraints(
            fixture.root.path(),
            &fixture.config,
            Some(&lock),
            &fixture.fetcher(&Cell::new(0)),
        )
        .unwrap_err();
        assert_eq!(error.code, "FED_LOCK_MISMATCH");
    }

    #[test]
    fn revision_probe_fetches_only_followed_manifests_and_never_type_bundles() {
        let mut fixture = Fixture::new(&["catalog", "pinned"]);
        fixture
            .config
            .remotes
            .get_mut(&ContainerName::from("pinned"))
            .unwrap()
            .dev_follow = false;
        let fetches = Cell::new(0);

        let revisions = probe_followed_federation_type_revisions_with(
            &fixture.config,
            &fixture.fetcher(&fetches),
        )
        .unwrap();

        assert_eq!(fetches.get(), 1, "only the followed manifest is fetched");
        assert_eq!(
            revisions
                .keys()
                .map(ContainerName::as_str)
                .collect::<Vec<_>>(),
            ["catalog"]
        );
        assert_eq!(
            revisions[&ContainerName::from("catalog")].build_id.as_str(),
            "build-a"
        );
        assert_eq!(
            revisions[&ContainerName::from("catalog")].types_hash,
            manifest_and_bundle("catalog", "build-a")
                .0
                .types
                .unwrap()
                .content_hash
        );
    }

    #[test]
    fn development_pinned_types_require_and_match_the_reviewed_lock() {
        let mut fixture = Fixture::new(&["catalog"]);
        fixture
            .config
            .remotes
            .get_mut(&ContainerName::from("catalog"))
            .unwrap()
            .dev_follow = false;
        let fetches = Cell::new(0);
        let error = sync_federation_types_with_constraints(
            fixture.root.path(),
            &fixture.config,
            None,
            &fixture.fetcher(&fetches),
        )
        .unwrap_err();
        assert_eq!(error.code, "FED_LOCK_REQUIRED");
        assert_eq!(fetches.get(), 0, "a missing lock fails before network I/O");

        let lock = lock_for_fixture(&fixture, "catalog");
        let synchronized = sync_federation_types_with_constraints(
            fixture.root.path(),
            &fixture.config,
            Some(&lock),
            &fixture.fetcher(&fetches),
        )
        .unwrap();
        let old_index = std::fs::read(&synchronized.index_file).unwrap();

        let manifest_url = fixture.config.remotes[&ContainerName::from("catalog")]
            .manifest_url
            .clone();
        let mut tampered = fixture.responses[&manifest_url].body.clone();
        tampered.push(b' ');
        fixture
            .responses
            .insert(manifest_url, json_response(tampered));
        let error = sync_federation_types_with_constraints(
            fixture.root.path(),
            &fixture.config,
            Some(&lock),
            &fixture.fetcher(&fetches),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ManifestIntegrity.as_str());
        assert_eq!(std::fs::read(&synchronized.index_file).unwrap(), old_index);
    }

    #[test]
    fn development_pinned_manifest_build_and_type_integrity_match_lock() {
        let mut fixture = Fixture::new(&["catalog"]);
        fixture
            .config
            .remotes
            .get_mut(&ContainerName::from("catalog"))
            .unwrap()
            .dev_follow = false;
        let mut lock = lock_for_fixture(&fixture, "catalog");
        let (manifest, _) = manifest_and_bundle("catalog", "build-b");
        replace_manifest(&mut fixture, "catalog", &manifest);
        let manifest_url = fixture.config.remotes[&ContainerName::from("catalog")]
            .manifest_url
            .clone();
        lock.remotes
            .get_mut(&ContainerName::from("catalog"))
            .unwrap()
            .manifest_integrity = sha384_digests(&fixture.responses[&manifest_url].body).1;

        let error = sync_federation_types_with_constraints(
            fixture.root.path(),
            &fixture.config,
            Some(&lock),
            &fixture.fetcher(&Cell::new(0)),
        )
        .unwrap_err();
        assert_eq!(error.code, "FED_LOCK_MISMATCH");

        let (manifest, _) = manifest_and_bundle("catalog", "build-a");
        replace_manifest(&mut fixture, "catalog", &manifest);
        let locked = lock
            .remotes
            .get_mut(&ContainerName::from("catalog"))
            .unwrap();
        locked.manifest_integrity = sha384_digests(&fixture.responses[&manifest_url].body).1;
        locked.types_integrity = Some(format!("sha384-{}", "Z".repeat(64)));
        let error = sync_federation_types_with_constraints(
            fixture.root.path(),
            &fixture.config,
            Some(&lock),
            &fixture.fetcher(&Cell::new(0)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::TypeBuildMismatch.as_str());
    }

    #[test]
    fn rejects_integrity_mime_size_and_denied_origin_without_updating_the_index() {
        let mut fixture = Fixture::new(&["catalog"]);
        let index = fixture.root.path().join(TYPES_ROOT).join("index.d.ts");
        atomic_write(&index, b"previous-index\n").unwrap();

        let types_url = "https://catalog.test/releases/types.json";
        fixture
            .responses
            .get_mut(types_url)
            .unwrap()
            .body
            .push(b' ');
        let error = sync_federation_types_with(
            fixture.root.path(),
            &fixture.config,
            &fixture.fetcher(&Cell::new(0)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetSize.as_str());
        assert_eq!(std::fs::read(&index).unwrap(), b"previous-index\n");

        let (_, valid_bundle) = manifest_and_bundle("catalog", "build-a");
        let mut tampered_bundle = valid_bundle.clone();
        *tampered_bundle.last_mut().unwrap() = b' ';
        fixture
            .responses
            .insert(types_url.to_owned(), json_response(tampered_bundle));
        let error = sync_federation_types_with(
            fixture.root.path(),
            &fixture.config,
            &fixture.fetcher(&Cell::new(0)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetIntegrity.as_str());

        fixture
            .responses
            .insert(types_url.to_owned(), json_response(valid_bundle.clone()));
        fixture.responses.get_mut(types_url).unwrap().content_type = Some("text/plain".to_owned());
        let error = sync_federation_types_with(
            fixture.root.path(),
            &fixture.config,
            &fixture.fetcher(&Cell::new(0)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetMime.as_str());

        let (mut manifest, valid_bundle) = manifest_and_bundle("catalog", "build-a");
        manifest.types.as_mut().unwrap().url = "https://cdn.test/types.json".to_owned();
        replace_manifest(&mut fixture, "catalog", &manifest);
        fixture.responses.insert(
            "https://cdn.test/types.json".to_owned(),
            json_response(valid_bundle),
        );
        let error = sync_federation_types_with(
            fixture.root.path(),
            &fixture.config,
            &fixture.fetcher(&Cell::new(0)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::OriginDenied.as_str());
        assert_eq!(std::fs::read(&index).unwrap(), b"previous-index\n");
    }

    #[test]
    fn a_later_remote_failure_does_not_publish_earlier_remote_types() {
        let mut fixture = Fixture::new(&["catalog", "zeta"]);
        fixture
            .responses
            .get_mut("https://zeta.test/releases/types.json")
            .unwrap()
            .content_type = Some("text/plain".to_owned());

        let error = sync_federation_types_with(
            fixture.root.path(),
            &fixture.config,
            &fixture.fetcher(&Cell::new(0)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetMime.as_str());
        assert!(
            !fixture
                .root
                .path()
                .join(TYPES_ROOT)
                .join("catalog/build-a/index.d.ts")
                .exists()
        );
        assert!(
            !fixture
                .root
                .path()
                .join(TYPES_ROOT)
                .join("index.d.ts")
                .exists()
        );
    }

    #[test]
    fn rejects_wrong_identity_missing_exposes_and_public_any() {
        for mutation in ["identity", "exposes", "any"] {
            let mut fixture = Fixture::new(&["catalog"]);
            let types_url = "https://catalog.test/releases/types.json";
            let mut bundle =
                serde_json::from_slice::<DeclarationBundle>(&fixture.responses[types_url].body)
                    .unwrap();
            match mutation {
                "identity" => bundle.build_id = "build-b".to_owned(),
                "exposes" => bundle.exposes.clear(),
                "any" => {
                    bundle.modules.insert(
                        "catalog/Button".to_owned(),
                        "export const Button: any;\n".to_owned(),
                    );
                }
                _ => unreachable!(),
            }
            let mut body = serde_json::to_vec_pretty(&bundle).unwrap();
            body.push(b'\n');
            let (content_hash, integrity) = sha384_digests(&body);
            let (mut manifest, _) = manifest_and_bundle("catalog", "build-a");
            let types = manifest.types.as_mut().unwrap();
            types.content_hash = content_hash;
            types.integrity = integrity;
            types.size = body.len() as u64;
            replace_manifest(&mut fixture, "catalog", &manifest);
            fixture
                .responses
                .insert(types_url.to_owned(), json_response(body));

            let result = sync_federation_types_with(
                fixture.root.path(),
                &fixture.config,
                &fixture.fetcher(&Cell::new(0)),
            );
            assert!(result.is_err(), "{mutation} declaration was accepted");
            let error = result.unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::TypeBuildMismatch.as_str(),
                "{mutation}"
            );
            assert!(
                !fixture
                    .root
                    .path()
                    .join(TYPES_ROOT)
                    .join("index.d.ts")
                    .exists()
            );
        }
    }

    #[test]
    fn parser_owned_remote_validation_rejects_executable_or_invalid_bodies() {
        for (name, body) in [
            ("initializer", "export const value: string = run();\n"),
            ("function-body", "export function load(): void { run(); }\n"),
            ("invalid-syntax", "export interface Broken {\n"),
            (
                "balanced-interface-initializer",
                "export interface Broken { value: string = run(); }\n",
            ),
            (
                "balanced-type-method-body",
                "export type Broken = { load(): void { run(); } };\n",
            ),
            (
                "missing-generic-parameter",
                "export type Broken<,> = string;\n",
            ),
            (
                "const-type-alias-parameter",
                "export type Broken<const T> = T;\n",
            ),
            (
                "optional-index-signature",
                "export type Broken = { [key: string]?: boolean };\n",
            ),
            (
                "function-parameter-property",
                "export type Broken = (public value: string) => void;\n",
            ),
            (
                "invalid-readonly-mapped-modifier",
                "export type Broken = { -readonly value: string };\n",
            ),
            ("redundant-declare", "export declare const value: string;\n"),
        ] {
            let mut fixture = Fixture::new(&["catalog"]);
            let index = fixture.root.path().join(TYPES_ROOT).join("index.d.ts");
            atomic_write(&index, b"last-good-index\n").unwrap();
            replace_public_declaration_body(&mut fixture, "catalog", body);

            let result = sync_federation_types_with(
                fixture.root.path(),
                &fixture.config,
                &fixture.fetcher(&Cell::new(0)),
            );
            assert!(result.is_err(), "{name} declaration was accepted");
            let error = result.unwrap_err();

            assert_eq!(error.code, ErrorCode::ManifestSchema.as_str(), "{name}");
            assert_eq!(std::fs::read(&index).unwrap(), b"last-good-index\n");
        }
    }

    #[test]
    fn contextual_any_policy_uses_parser_type_facts() {
        let mut allowed = Fixture::new(&["catalog"]);
        replace_public_declaration_body(
            &mut allowed,
            "catalog",
            "export interface Named { any?: string; marker: 'any'; }\n",
        );
        sync_federation_types_with(
            allowed.root.path(),
            &allowed.config,
            &allowed.fetcher(&Cell::new(0)),
        )
        .unwrap();

        let mut rejected = Fixture::new(&["catalog"]);
        replace_public_declaration_body(
            &mut rejected,
            "catalog",
            "export type Unsafe = `${any}`;\n",
        );
        let error = sync_federation_types_with(
            rejected.root.path(),
            &rejected.config,
            &rejected.fetcher(&Cell::new(0)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::TypeBuildMismatch.as_str());

        let mut implicit = Fixture::new(&["catalog"]);
        replace_public_declaration_body(
            &mut implicit,
            "catalog",
            "export interface Unsafe { value; method(input); (input); new (input); } export function load(input);\n",
        );
        let error = sync_federation_types_with(
            implicit.root.path(),
            &implicit.config,
            &implicit.fetcher(&Cell::new(0)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::TypeBuildMismatch.as_str());
        assert!(error.message.contains("forbidden public `any`"));
    }

    #[test]
    fn rejects_modules_outside_the_remote_build_namespace() {
        let mut fixture = Fixture::new(&["catalog"]);
        let types_url = "https://catalog.test/releases/types.json";
        let mut bundle =
            serde_json::from_slice::<DeclarationBundle>(&fixture.responses[types_url].body)
                .unwrap();
        bundle.modules.insert(
            "foreign/Injected".to_owned(),
            "export interface Injected { value: string; }\n".to_owned(),
        );
        let mut body = serde_json::to_vec_pretty(&bundle).unwrap();
        body.push(b'\n');
        let (content_hash, integrity) = sha384_digests(&body);
        let (mut manifest, _) = manifest_and_bundle("catalog", "build-a");
        let types = manifest.types.as_mut().unwrap();
        types.content_hash = content_hash;
        types.integrity = integrity;
        types.size = body.len() as u64;
        replace_manifest(&mut fixture, "catalog", &manifest);
        fixture
            .responses
            .insert(types_url.to_owned(), json_response(body));

        let error = sync_federation_types_with(
            fixture.root.path(),
            &fixture.config,
            &fixture.fetcher(&Cell::new(0)),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::ManifestSchema.as_str());
        assert!(error.message.contains("build-owned module namespace"));
    }

    #[test]
    fn rejects_incompatible_manifest_identity_and_missing_type_artifacts() {
        for (mutation, expected_code) in [
            ("abi", ErrorCode::RuntimeAbi),
            ("name", ErrorCode::ManifestSchema),
            ("missing-types", ErrorCode::TypeBuildMismatch),
            ("unsafe-build-id", ErrorCode::ManifestSchema),
        ] {
            let mut fixture = Fixture::new(&["catalog"]);
            let (mut manifest, _) = manifest_and_bundle("catalog", "build-a");
            match mutation {
                "abi" => manifest.runtime_abi = "wake.federation.v2".to_owned(),
                "name" => manifest.name = ContainerName::from("checkout"),
                "missing-types" => manifest.types = None,
                "unsafe-build-id" => {
                    manifest.build_id = BuildId::from("../escape");
                    manifest.types.as_mut().unwrap().build_id = BuildId::from("../escape");
                }
                _ => unreachable!(),
            }
            replace_manifest(&mut fixture, "catalog", &manifest);

            let error = sync_federation_types_with(
                fixture.root.path(),
                &fixture.config,
                &fixture.fetcher(&Cell::new(0)),
            )
            .unwrap_err();
            assert_eq!(error.code, expected_code.as_str(), "{mutation}");
        }
    }

    #[test]
    fn build_scoped_declarations_are_immutable_and_http_dev_origins_are_supported() {
        let mut fixture = Fixture::new(&["catalog"]);
        let old_manifest_url = fixture.config.remotes[&ContainerName::from("catalog")]
            .manifest_url
            .clone();
        let manifest_response = fixture.responses.remove(&old_manifest_url).unwrap();
        let old_types_url = "https://catalog.test/releases/types.json";
        let types_response = fixture.responses.remove(old_types_url).unwrap();
        let manifest_url = "http://localhost:4173/releases/manifest.json".to_owned();
        let types_url = "http://localhost:4173/releases/types.json".to_owned();
        fixture
            .config
            .remotes
            .get_mut(&ContainerName::from("catalog"))
            .unwrap()
            .manifest_url = manifest_url.clone();
        fixture.responses.insert(manifest_url, manifest_response);
        fixture.responses.insert(types_url, types_response);

        let fetches = Cell::new(0);
        sync_federation_types_with(
            fixture.root.path(),
            &fixture.config,
            &fixture.fetcher(&fetches),
        )
        .unwrap();
        let declaration = fixture
            .root
            .path()
            .join(TYPES_ROOT)
            .join("catalog/build-a/index.d.ts");
        std::fs::write(&declaration, "different bytes").unwrap();
        let error = sync_federation_types_with(
            fixture.root.path(),
            &fixture.config,
            &fixture.fetcher(&fetches),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::TypeBuildMismatch.as_str());
    }
}
