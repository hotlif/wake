//! Production lock generation for Wake Federation.
//!
//! This is deliberately a build-time control-plane boundary: it downloads and
//! validates immutable manifests, but it never downloads or executes remote code.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::digest::{SHA384, digest};
use wake_federation_contract::{
    Asset, AssetKind, ErrorCode, FederationConfig, FederationLock, Manifest, RemoteConfig,
    RemoteRef,
};

use super::WakeError;

const LOCK_FILE: &str = "wake-federation.lock";
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MANIFEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Fetch every configured production manifest and atomically publish the exact
/// immutable closure as `<project_root>/wake-federation.lock`.
///
/// A failure always leaves an existing lock untouched. In particular, network,
/// schema and integrity failures never fall back to an older lock.
pub fn generate_federation_lock(
    project_root: &Path,
    config: &FederationConfig,
) -> Result<FederationLock, WakeError> {
    generate_federation_lock_with(project_root, config, &UreqManifestFetcher)
}

/// Resolve an enabled Federation project without making a shell depend on `wake_config`.
pub fn federation_project_root(start: &Path) -> Result<PathBuf, WakeError> {
    load_federation_project(start).map(|(root, _)| root)
}

/// Resolve project configuration and generate its reviewed production lock.
pub fn generate_project_federation_lock(
    start: &Path,
) -> Result<(PathBuf, FederationLock), WakeError> {
    let (project_root, config) = load_federation_project(start)?;
    if config.remotes.is_empty() {
        return Err(WakeError::new(
            "FED_CONFIG_INVALID",
            "federation lock generation requires at least one configured remote",
        )
        .at(&project_root.join(wake_config::CONFIG_FILE)));
    }
    let lock = generate_federation_lock(&project_root, &config)?;
    Ok((project_root, lock))
}

fn load_federation_project(start: &Path) -> Result<(PathBuf, FederationConfig), WakeError> {
    let absolute = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?
            .join(start)
    };
    let absolute = absolute.canonicalize().map_err(|error| {
        WakeError::new(
            "WAKE_IO",
            format!("cannot resolve project directory: {error}"),
        )
        .at(&absolute)
    })?;
    if !absolute.is_dir() {
        return Err(WakeError::new("WAKE_IO", "project path is not a directory").at(&absolute));
    }
    let project_root = wake_config::find_root(&absolute);
    let config_path = project_root.join(wake_config::CONFIG_FILE);
    if !config_path.is_file() {
        return Err(WakeError::new(
            "FED_CONFIG_INVALID",
            format!(
                "no {} was found from `{}`",
                wake_config::CONFIG_FILE,
                absolute.display()
            ),
        )
        .at(&config_path));
    }
    let config = wake_config::load(&project_root).map_err(|error| {
        WakeError::new("FED_CONFIG_INVALID", error.to_string()).at(&config_path)
    })?;
    if !config.federation.enabled {
        return Err(WakeError::new(
            "FED_CONFIG_INVALID",
            format!(
                "`{}` must set `federation.enabled = true`",
                config_path.display()
            ),
        )
        .at(&config_path));
    }
    Ok((project_root, config.federation))
}

#[derive(Debug)]
struct ManifestResponse {
    status: u16,
    content_type: Option<String>,
    content_length: Option<u64>,
    body: Vec<u8>,
}

trait ManifestFetcher {
    fn fetch(&self, url: String) -> Result<ManifestResponse, WakeError>;
}

impl<F> ManifestFetcher for F
where
    F: Fn(String) -> Result<ManifestResponse, WakeError>,
{
    fn fetch(&self, url: String) -> Result<ManifestResponse, WakeError> {
        self(url)
    }
}

struct UreqManifestFetcher;

impl ManifestFetcher for UreqManifestFetcher {
    fn fetch(&self, url: String) -> Result<ManifestResponse, WakeError> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(MANIFEST_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .new_agent();
        let mut response = agent.get(&url).call().map_err(|error| {
            federation_error(
                ErrorCode::ManifestFetch,
                format!("could not fetch federation manifest `{url}`: {error}"),
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
                        format!("federation manifest `{url}` has a non-text Content-Type: {error}"),
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
                            ErrorCode::ManifestFetch,
                            format!("federation manifest `{url}` has an invalid Content-Length"),
                        )
                    })
            })
            .transpose()?;
        if content_length.is_some_and(|size| size > MAX_MANIFEST_BYTES as u64) {
            return Err(manifest_size_error(&url));
        }
        let body = response
            .body_mut()
            .with_config()
            .limit((MAX_MANIFEST_BYTES + 1) as u64)
            .read_to_vec()
            .map_err(|error| {
                federation_error(
                    ErrorCode::ManifestFetch,
                    format!("could not read federation manifest `{url}`: {error}"),
                )
            })?;
        Ok(ManifestResponse {
            status,
            content_type,
            content_length,
            body,
        })
    }
}

fn generate_federation_lock_with(
    project_root: &Path,
    config: &FederationConfig,
    fetcher: &impl ManifestFetcher,
) -> Result<FederationLock, WakeError> {
    config.validate().map_err(|error| {
        federation_error(
            ErrorCode::ConfigInvalid,
            format!("invalid federation configuration: {error}"),
        )
    })?;

    let mut lock = FederationLock::new();
    for (expected_name, remote) in &config.remotes {
        let manifest_url = HttpsUrl::parse(&remote.manifest_url, "manifestUrl")?;
        let allowed_origins = production_origins(remote, &manifest_url)?;
        let response = fetcher.fetch(remote.manifest_url.clone())?;
        validate_manifest_response(&remote.manifest_url, &response)?;
        let manifest = serde_json::from_slice::<Manifest>(&response.body).map_err(|error| {
            federation_error(
                ErrorCode::ManifestSchema,
                format!("federation manifest for `{expected_name}` is not valid v1 JSON: {error}"),
            )
        })?;
        manifest.validate_for_production_lock().map_err(|error| {
            federation_validation_error(
                ErrorCode::ManifestSchema,
                format!("invalid federation manifest for `{expected_name}`"),
                &error,
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
        let (allowed_assets, types_integrity) =
            resolve_asset_closure(&manifest_url, &allowed_origins, &manifest)?;
        lock.remotes.insert(
            expected_name.clone(),
            RemoteRef {
                // Keep the normalized configuration spelling. The host's production
                // lock check intentionally compares this field byte-for-byte.
                manifest_url: remote.manifest_url.clone(),
                build_id: manifest.build_id,
                manifest_integrity: sha384_integrity(&response.body),
                has_exposes: !manifest.exposes.is_empty(),
                types_integrity,
                allowed_assets,
            },
        );
    }
    lock.validate().map_err(|error| {
        federation_error(
            ErrorCode::ManifestSchema,
            format!("generated federation lock is invalid: {error}"),
        )
    })?;

    let mut bytes = serde_json::to_vec_pretty(&lock).map_err(|error| {
        WakeError::new(
            "WAKE_INTERNAL",
            format!("could not serialize federation lock: {error}"),
        )
    })?;
    bytes.push(b'\n');
    let path = project_root.join(LOCK_FILE);
    if std::fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
        super::atomic_write(&path, &bytes)?;
    }
    Ok(lock)
}

fn validate_manifest_response(url: &str, response: &ManifestResponse) -> Result<(), WakeError> {
    if response.status != 200 {
        return Err(federation_error(
            ErrorCode::ManifestFetch,
            format!(
                "federation manifest `{url}` returned HTTP {} (expected 200)",
                response.status
            ),
        ));
    }
    let content_type = response.content_type.as_deref().ok_or_else(|| {
        federation_error(
            ErrorCode::AssetMime,
            format!("federation manifest `{url}` is missing Content-Type"),
        )
    })?;
    if !is_json_content_type(content_type) {
        return Err(federation_error(
            ErrorCode::AssetMime,
            format!("federation manifest `{url}` must use application/json, got `{content_type}`"),
        ));
    }
    if response
        .content_length
        .is_some_and(|size| size > MAX_MANIFEST_BYTES as u64)
        || response.body.len() > MAX_MANIFEST_BYTES
    {
        return Err(manifest_size_error(url));
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

fn manifest_size_error(url: &str) -> WakeError {
    federation_error(
        ErrorCode::AssetSize,
        format!(
            "federation manifest `{url}` exceeds the {MAX_MANIFEST_BYTES} byte production limit"
        ),
    )
}

fn production_origins(
    remote: &RemoteConfig,
    manifest_url: &HttpsUrl,
) -> Result<BTreeSet<String>, WakeError> {
    if remote.allowed_origins.is_empty() {
        return Ok(BTreeSet::from([manifest_url.origin.clone()]));
    }

    let mut origins = BTreeSet::new();
    for origin in &remote.allowed_origins {
        let parsed = HttpsUrl::parse(origin, "allowedOrigins")?;
        if parsed.path != "/" || parsed.query.is_some() {
            return Err(federation_error(
                ErrorCode::OriginDenied,
                format!("production allowed origin `{origin}` must not contain a path or query"),
            ));
        }
        origins.insert(parsed.origin);
    }
    if !origins.contains(&manifest_url.origin) {
        return Err(federation_error(
            ErrorCode::OriginDenied,
            format!(
                "manifest origin `{}` is not in the configured production allowed origins",
                manifest_url.origin
            ),
        ));
    }
    Ok(origins)
}

fn resolve_asset_closure(
    manifest_url: &HttpsUrl,
    allowed_origins: &BTreeSet<String>,
    manifest: &Manifest,
) -> Result<(BTreeMap<String, String>, Option<String>), WakeError> {
    let mut allowed_assets = BTreeMap::<String, String>::new();
    let mut identities = BTreeMap::<String, LockedAssetIdentity>::new();
    for asset in manifest_assets(manifest) {
        insert_asset(
            &mut allowed_assets,
            &mut identities,
            manifest_url,
            allowed_origins,
            &asset.url,
            LockedAssetIdentity::from_asset(asset),
        )?;
    }
    let types_integrity = manifest.types.as_ref().map(|types| types.integrity.clone());
    if let Some(types) = &manifest.types {
        insert_asset(
            &mut allowed_assets,
            &mut identities,
            manifest_url,
            allowed_origins,
            &types.url,
            LockedAssetIdentity {
                kind: AssetKind::Other,
                content_hash: types.content_hash.clone(),
                integrity: types.integrity.clone(),
                mime: "application/json".to_owned(),
                size: types.size,
            },
        )?;
    }
    Ok((allowed_assets, types_integrity))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LockedAssetIdentity {
    kind: AssetKind,
    content_hash: String,
    integrity: String,
    mime: String,
    size: u64,
}

impl LockedAssetIdentity {
    fn from_asset(asset: &Asset) -> Self {
        Self {
            kind: asset.kind,
            content_hash: asset.content_hash.clone(),
            integrity: asset.integrity.clone(),
            mime: asset.mime.clone(),
            size: asset.size,
        }
    }
}

fn manifest_assets(manifest: &Manifest) -> Vec<&Asset> {
    let mut assets = Vec::new();
    assets.push(&manifest.remote_entry);
    assets.extend(manifest.remote_entry_source_map.iter());
    for expose in manifest.exposes.values() {
        assets.push(&expose.entry);
        assets.extend(&expose.css);
        assets.extend(expose.source_map.iter());
        assets.extend(&expose.synchronous_assets);
        assets.extend(&expose.asynchronous_assets);
    }
    for offer in &manifest.shared.offers {
        assets.extend(offer.asset.iter());
    }
    for requirement in &manifest.shared.requirements {
        assets.extend(requirement.fallback.iter());
    }
    assets
}

fn insert_asset(
    allowed_assets: &mut BTreeMap<String, String>,
    identities: &mut BTreeMap<String, LockedAssetIdentity>,
    manifest_url: &HttpsUrl,
    allowed_origins: &BTreeSet<String>,
    location: &str,
    identity: LockedAssetIdentity,
) -> Result<(), WakeError> {
    let resolved = manifest_url.join(location)?;
    if !allowed_origins.contains(&resolved.origin) {
        return Err(federation_error(
            ErrorCode::OriginDenied,
            format!(
                "federation asset `{}` resolves outside the production allowed origins",
                resolved.serialized
            ),
        ));
    }
    if let Some(previous) = identities.get(&resolved.serialized)
        && previous != &identity
    {
        return Err(federation_error(
            ErrorCode::AssetIntegrity,
            format!(
                "federation asset `{}` has conflicting manifest metadata",
                resolved.serialized
            ),
        ));
    }
    identities.insert(resolved.serialized.clone(), identity.clone());
    match allowed_assets.get(&resolved.serialized) {
        Some(_) => Ok(()),
        None => {
            allowed_assets.insert(resolved.serialized, identity.integrity);
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpsUrl {
    serialized: String,
    origin: String,
    path: String,
    query: Option<String>,
}

impl HttpsUrl {
    fn parse(value: &str, field: &str) -> Result<Self, WakeError> {
        if value.trim() != value
            || value.is_empty()
            || value.contains(['\\', '#'])
            || value.chars().any(char::is_control)
        {
            return Err(invalid_https_url(field, value));
        }
        let uri = value
            .parse::<ureq::http::Uri>()
            .map_err(|_| invalid_https_url(field, value))?;
        if uri.scheme_str() != Some("https") {
            return Err(invalid_https_url(field, value));
        }
        let authority = uri
            .authority()
            .ok_or_else(|| invalid_https_url(field, value))?;
        if authority.as_str().contains('@') {
            return Err(invalid_https_url(field, value));
        }
        let host = authority.host().to_ascii_lowercase();
        if !is_valid_host(&host) || (authority.port().is_some() && authority.port_u16().is_none()) {
            return Err(invalid_https_url(field, value));
        }
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        };
        let port = authority.port_u16();
        let origin = match port {
            Some(443) | None => format!("https://{host}"),
            Some(port) => format!("https://{host}:{port}"),
        };
        let path = normalize_url_path(uri.path(), field, value)?;
        let query = uri.query().map(str::to_owned);
        let serialized = serialize_url(&origin, &path, query.as_deref());
        Ok(Self {
            serialized,
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
            return Err(invalid_https_url("asset.url", value));
        }
        if value.starts_with("https://") {
            return Self::parse(value, "asset.url");
        }
        if value.starts_with("//") {
            return Self::parse(&format!("https:{value}"), "asset.url");
        }
        if value.contains("://") {
            return Err(invalid_https_url("asset.url", value));
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
            return Err(invalid_https_url("asset.url", value));
        }
        let path = if raw_path.is_empty() {
            self.path.clone()
        } else if raw_path.starts_with('/') {
            normalize_url_path(raw_path, "asset.url", value)?
        } else {
            let directory = self
                .path
                .rsplit_once('/')
                .map_or("/", |(directory, _)| directory);
            normalize_url_path(&format!("{directory}/{raw_path}"), "asset.url", value)?
        };
        let serialized = serialize_url(&self.origin, &path, query.as_deref());
        Ok(Self {
            serialized,
            origin: self.origin.clone(),
            path,
            query,
        })
    }
}

fn normalize_url_path(path: &str, field: &str, original: &str) -> Result<String, WakeError> {
    let path = if path.is_empty() { "/" } else { path };
    if !path.starts_with('/') {
        return Err(invalid_https_url(field, original));
    }
    let lowercase = path.to_ascii_lowercase();
    if lowercase.contains("%2e") || lowercase.contains("%2f") || lowercase.contains("%5c") {
        // Browsers normalize encoded traversal and separators differently from
        // generic URI parsers. Reject the ambiguous spelling so lock keys exactly
        // match `new URL(...).href` in every supported browser.
        return Err(invalid_https_url(field, original));
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
    let normalized = format!("/{}", segments.join("/"));
    // Unlike filesystem paths, WHATWG URLs preserve duplicate slash bytes.
    debug_assert!(normalized.starts_with('/'));
    Ok(normalized)
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

fn invalid_https_url(field: &str, value: &str) -> WakeError {
    federation_error(
        ErrorCode::OriginDenied,
        format!("production {field} must be an unambiguous absolute HTTPS URL: `{value}`"),
    )
}

fn sha384_integrity(bytes: &[u8]) -> String {
    format!("sha384-{}", BASE64.encode(digest(&SHA384, bytes).as_ref()))
}

fn federation_error(code: ErrorCode, message: impl Into<String>) -> WakeError {
    WakeError::new(code.as_str(), message)
}

fn federation_validation_error(
    default_code: ErrorCode,
    context: impl Into<String>,
    error: &wake_federation_contract::ValidationErrors,
) -> WakeError {
    let code = error
        .violations
        .iter()
        .find(|violation| violation.code != ErrorCode::ManifestSchema)
        .map_or(default_code, |violation| violation.code);
    federation_error(code, format!("{}: {error}", context.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use wake_federation_contract::{
        AssetKind, BuildId, ContainerName, ExposeKey, ExposeMode, ExposedModule, PackageKey,
        ShadowMode, SharedOffer, SharedPolicy, SharedRequirement, TypeArtifact, TypeArtifactFormat,
    };

    fn sri(character: char) -> String {
        format!("sha384-{}", character.to_string().repeat(64))
    }

    fn asset(kind: AssetKind, url: &str, character: char) -> Asset {
        Asset::new(
            kind,
            url,
            format!("hash-{character}"),
            sri(character),
            match kind {
                AssetKind::JavaScript => "text/javascript",
                AssetKind::Css => "text/css",
                AssetKind::SourceMap => "application/json",
                AssetKind::Other => "application/octet-stream",
            },
            17,
        )
    }

    fn complete_manifest(name: &str) -> Manifest {
        let mut manifest = Manifest::new(
            ContainerName::from(name),
            BuildId::from("build-a"),
            "chrome120",
            asset(AssetKind::JavaScript, "./remoteEntry.mjs", 'A'),
        );
        manifest.remote_entry_source_map =
            Some(asset(AssetKind::SourceMap, "./remoteEntry.mjs.map", 'B'));
        manifest.exposes.insert(
            ExposeKey::from("./Button"),
            ExposedModule {
                mode: ExposeMode::Generic,
                scope: "default".to_owned(),
                shadow: ShadowMode::None,
                entry: asset(AssetKind::JavaScript, "chunks/button.js", 'C'),
                css: vec![asset(AssetKind::Css, "/styles/button.css", 'D')],
                source_map: Some(asset(AssetKind::SourceMap, "chunks/button.js.map", 'E')),
                synchronous_assets: vec![asset(AssetKind::Other, "assets/icon.svg", 'F')],
                asynchronous_assets: vec![asset(AssetKind::JavaScript, "chunks/lazy.js", 'G')],
            },
        );
        manifest.shared.offers.push(SharedOffer {
            share_key: "react".to_owned(),
            package: PackageKey {
                name: "react".to_owned(),
                version: "18.2.0".to_owned(),
                package_context: "workspace-a".to_owned(),
                build_variant: "browser-production".to_owned(),
            },
            provider: ContainerName::from(name),
            policy: SharedPolicy::default(),
            asset: Some(asset(AssetKind::JavaScript, "shared/react.js", 'H')),
        });
        manifest.shared.requirements.push(SharedRequirement {
            share_key: "scheduler".to_owned(),
            required_version: "0.23.0".to_owned(),
            package_context: "workspace-a".to_owned(),
            build_variant: "browser-production".to_owned(),
            policy: SharedPolicy::default(),
            fallback: Some(asset(AssetKind::JavaScript, "shared/scheduler.js", 'I')),
        });
        manifest.types = Some(TypeArtifact {
            build_id: BuildId::from("build-a"),
            url: "types/declarations.json".to_owned(),
            content_hash: "types-hash".to_owned(),
            integrity: sri('J'),
            size: 42,
            format: TypeArtifactFormat::DeclarationBundle,
        });
        manifest
    }

    fn shared_only_manifest(name: &str) -> Manifest {
        let mut manifest = complete_manifest(name);
        manifest.exposes.clear();
        manifest.types = None;
        manifest
    }

    fn config(url: &str) -> FederationConfig {
        let mut config = FederationConfig {
            enabled: true,
            name: ContainerName::from("shell"),
            ..FederationConfig::default()
        };
        config.remotes.insert(
            ContainerName::from("catalog"),
            RemoteConfig {
                manifest_url: url.to_owned(),
                allowed_origins: Vec::new(),
                dev_follow: false,
            },
        );
        config
    }

    fn response(manifest: &Manifest) -> ManifestResponse {
        ManifestResponse {
            status: 200,
            content_type: Some("application/json; charset=utf-8".to_owned()),
            content_length: None,
            body: serde_json::to_vec(manifest).unwrap(),
        }
    }

    #[test]
    fn resolves_the_complete_asset_closure_to_exact_https_urls() {
        let root = tempfile::tempdir().unwrap();
        let manifest = complete_manifest("catalog");
        let lock = generate_federation_lock_with(
            root.path(),
            &config("https://catalog.test/releases/wake-federation.json"),
            &|_| Ok(response(&manifest)),
        )
        .unwrap();
        let remote = &lock.remotes[&ContainerName::from("catalog")];
        assert_eq!(remote.build_id, BuildId::from("build-a"));
        assert!(remote.has_exposes);
        let expected_types_integrity = sri('J');
        assert_eq!(
            remote.types_integrity.as_deref(),
            Some(expected_types_integrity.as_str())
        );
        assert_eq!(remote.allowed_assets.len(), 10);
        assert_eq!(
            remote.allowed_assets["https://catalog.test/releases/chunks/button.js"],
            sri('C')
        );
        assert_eq!(
            remote.allowed_assets["https://catalog.test/styles/button.css"],
            sri('D')
        );
        assert_eq!(
            remote.allowed_assets["https://catalog.test/releases/types/declarations.json"],
            sri('J')
        );
        assert!(root.path().join(LOCK_FILE).is_file());
    }

    #[test]
    fn shared_only_remote_lock_and_production_build_do_not_require_types() {
        let root = tempfile::tempdir().unwrap();
        let manifest_url = "https://catalog.test/releases/wake-federation.json";
        let manifest = shared_only_manifest("catalog");
        let lock = generate_federation_lock_with(root.path(), &config(manifest_url), &|_| {
            Ok(response(&manifest))
        })
        .unwrap();
        let remote = &lock.remotes[&ContainerName::from("catalog")];
        assert!(!remote.has_exposes);
        assert!(remote.types_integrity.is_none());
        assert!(!remote.allowed_assets.is_empty());

        std::fs::write(
            root.path().join("wake.config.toml"),
            format!(
                r#"[federation]
enabled = true
name = "shell"

[federation.remotes.catalog]
manifest_url = "{manifest_url}"
"#
            ),
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("src/main.ts"),
            "globalThis.__sharedOnlyHost = true;\n",
        )
        .unwrap();

        let result = crate::build(
            crate::BuildOptions {
                project: crate::ProjectOptions {
                    cwd: Some(root.path().to_path_buf()),
                    config_path: None,
                },
                entry: Some(PathBuf::from("src/main.ts")),
                outdir: Some(PathBuf::from("dist")),
                ..crate::BuildOptions::default()
            },
            &crate::CancellationToken::default(),
        )
        .unwrap();
        assert!(result.success);
    }

    #[test]
    fn production_build_rejects_exposed_lock_without_types_or_expose_presence() {
        let root = tempfile::tempdir().unwrap();
        let manifest_url = "https://catalog.test/releases/wake-federation.json";
        let manifest = complete_manifest("catalog");
        let mut lock = generate_federation_lock_with(root.path(), &config(manifest_url), &|_| {
            Ok(response(&manifest))
        })
        .unwrap();
        lock.remotes
            .get_mut(&ContainerName::from("catalog"))
            .unwrap()
            .types_integrity = None;
        std::fs::write(
            root.path().join(LOCK_FILE),
            serde_json::to_vec_pretty(&lock).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.path().join("wake.config.toml"),
            format!(
                r#"[federation]
enabled = true
name = "shell"

[federation.remotes.catalog]
manifest_url = "{manifest_url}"
"#
            ),
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/main.ts"), "export {};\n").unwrap();
        let build = || {
            crate::build(
                crate::BuildOptions {
                    project: crate::ProjectOptions {
                        cwd: Some(root.path().to_path_buf()),
                        config_path: None,
                    },
                    entry: Some(PathBuf::from("src/main.ts")),
                    outdir: Some(PathBuf::from("dist")),
                    ..crate::BuildOptions::default()
                },
                &crate::CancellationToken::default(),
            )
        };
        let error = build().unwrap_err();
        assert_eq!(error.code, ErrorCode::TypeBuildMismatch.as_str());

        let mut value = serde_json::to_value(&lock).unwrap();
        value["remotes"]["catalog"]
            .as_object_mut()
            .unwrap()
            .remove("hasExposes");
        std::fs::write(
            root.path().join(LOCK_FILE),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
        let error = build().unwrap_err();
        assert_eq!(error.code, "FED_LOCK_INVALID");
        assert!(error.message.contains("hasExposes"), "{}", error.message);
    }

    #[test]
    fn production_lock_rejects_ownerless_singleton_offers_and_requirements() {
        let root = tempfile::tempdir().unwrap();
        let configuration = config("https://catalog.test/wake-federation.json");
        for target in ["offer", "requirement"] {
            let mut manifest = complete_manifest("catalog");
            {
                let policy = if target == "offer" {
                    &mut manifest.shared.offers[0].policy
                } else {
                    &mut manifest.shared.requirements[0].policy
                };
                policy.singleton = true;
                policy.owner = None;
            }
            let error = generate_federation_lock_with(root.path(), &configuration, &|_| {
                Ok(response(&manifest))
            })
            .unwrap_err();
            assert_eq!(error.code, ErrorCode::ConfigInvalid.as_str());
            assert!(error.message.contains(target), "{}", error.message);
            assert!(error.message.contains("policy.owner"), "{}", error.message);

            let policy = if target == "offer" {
                &mut manifest.shared.offers[0].policy
            } else {
                &mut manifest.shared.requirements[0].policy
            };
            policy.owner = Some(ContainerName::from("catalog"));
            generate_federation_lock_with(root.path(), &configuration, &|_| {
                Ok(response(&manifest))
            })
            .expect("an explicit singleton owner is production-lockable");
        }
    }

    #[test]
    fn enforces_manifest_and_asset_origin_allowlists() {
        let root = tempfile::tempdir().unwrap();
        let mut manifest = complete_manifest("catalog");
        manifest.remote_entry.url = "https://cdn.test/remoteEntry.mjs".to_owned();
        let mut allowed = config("https://catalog.test/wake-federation.json");
        allowed
            .remotes
            .get_mut(&ContainerName::from("catalog"))
            .unwrap()
            .allowed_origins = vec![
            "https://catalog.test".to_owned(),
            "https://cdn.test".to_owned(),
        ];
        generate_federation_lock_with(root.path(), &allowed, &|_| Ok(response(&manifest))).unwrap();

        allowed
            .remotes
            .get_mut(&ContainerName::from("catalog"))
            .unwrap()
            .allowed_origins
            .pop();
        let error =
            generate_federation_lock_with(root.path(), &allowed, &|_| Ok(response(&manifest)))
                .unwrap_err();
        assert_eq!(error.code, ErrorCode::OriginDenied.as_str());

        allowed
            .remotes
            .get_mut(&ContainerName::from("catalog"))
            .unwrap()
            .allowed_origins = vec!["https://cdn.test".to_owned()];
        let fetched = Cell::new(false);
        let error = generate_federation_lock_with(root.path(), &allowed, &|_| {
            fetched.set(true);
            Ok(response(&manifest))
        })
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::OriginDenied.as_str());
        assert!(
            !fetched.get(),
            "a denied manifest origin must not be fetched"
        );
    }

    #[test]
    fn rejects_http_ambiguous_urls_and_conflicting_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let manifest = complete_manifest("catalog");
        let error = generate_federation_lock_with(
            root.path(),
            &config("http://catalog.test/wake-federation.json"),
            &|_| Ok(response(&manifest)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::OriginDenied.as_str());

        let mut ambiguous = complete_manifest("catalog");
        ambiguous.remote_entry.url = "./%2e%2e/remoteEntry.mjs".to_owned();
        let error = generate_federation_lock_with(
            root.path(),
            &config("https://catalog.test/wake-federation.json"),
            &|_| Ok(response(&ambiguous)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::OriginDenied.as_str());

        let mut duplicate = complete_manifest("catalog");
        duplicate
            .exposes
            .get_mut(&ExposeKey::from("./Button"))
            .unwrap()
            .entry
            .url = "./remoteEntry.mjs".to_owned();
        let error = generate_federation_lock_with(
            root.path(),
            &config("https://catalog.test/wake-federation.json"),
            &|_| Ok(response(&duplicate)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetIntegrity.as_str());

        let mut aliased_duplicate = complete_manifest("catalog");
        let entry = &mut aliased_duplicate
            .exposes
            .get_mut(&ExposeKey::from("./Button"))
            .unwrap()
            .entry;
        *entry = aliased_duplicate.remote_entry.clone();
        entry.url = "remoteEntry.mjs".to_owned();
        entry.content_hash = "conflicting-hash".to_owned();
        let error = generate_federation_lock_with(
            root.path(),
            &config("https://catalog.test/wake-federation.json"),
            &|_| Ok(response(&aliased_duplicate)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetIntegrity.as_str());
    }

    #[test]
    fn strict_url_join_preserves_browser_visible_slashes_and_rejects_schemes() {
        let base = HttpsUrl::parse(
            "https://CATALOG.test:443/releases//wake-federation.json",
            "manifestUrl",
        )
        .unwrap();
        assert_eq!(base.origin, "https://catalog.test");
        assert_eq!(
            base.join("./chunks//button.js").unwrap().serialized,
            "https://catalog.test/releases//chunks//button.js"
        );
        assert_eq!(
            base.join("../chunks/button.js").unwrap().serialized,
            "https://catalog.test/releases/chunks/button.js"
        );
        assert!(base.join("data:text/javascript,alert(1)").is_err());
        assert!(base.join("https://bad_host.test/chunk.js").is_err());
    }

    #[test]
    fn rejects_bad_status_mime_size_schema_and_remote_name() {
        let root = tempfile::tempdir().unwrap();
        let manifest = complete_manifest("catalog");
        let configuration = config("https://catalog.test/wake-federation.json");

        for (mut bad, code) in [
            (
                ManifestResponse {
                    status: 503,
                    ..response(&manifest)
                },
                ErrorCode::ManifestFetch,
            ),
            (
                ManifestResponse {
                    content_type: Some("text/plain".to_owned()),
                    ..response(&manifest)
                },
                ErrorCode::AssetMime,
            ),
            (
                ManifestResponse {
                    content_length: Some(MAX_MANIFEST_BYTES as u64 + 1),
                    ..response(&manifest)
                },
                ErrorCode::AssetSize,
            ),
        ] {
            // Keep the actual body small: both the declared and observed limits
            // are independently enforced.
            bad.body.shrink_to_fit();
            let error = generate_federation_lock_with(root.path(), &configuration, &|_| {
                Ok(ManifestResponse {
                    status: bad.status,
                    content_type: bad.content_type.clone(),
                    content_length: bad.content_length,
                    body: bad.body.clone(),
                })
            })
            .unwrap_err();
            assert_eq!(error.code, code.as_str());
        }

        let error = generate_federation_lock_with(root.path(), &configuration, &|_| {
            Ok(ManifestResponse {
                body: b"{not-json".to_vec(),
                ..response(&manifest)
            })
        })
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ManifestSchema.as_str());

        let wrong_name = complete_manifest("checkout");
        let error = generate_federation_lock_with(root.path(), &configuration, &|_| {
            Ok(response(&wrong_name))
        })
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ManifestSchema.as_str());

        let mut missing_types = complete_manifest("catalog");
        missing_types.types = None;
        let error = generate_federation_lock_with(root.path(), &configuration, &|_| {
            Ok(response(&missing_types))
        })
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::TypeBuildMismatch.as_str());
    }

    #[test]
    fn raw_manifest_tampering_changes_the_locked_sha384() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let manifest = complete_manifest("catalog");
        let compact = serde_json::to_vec(&manifest).unwrap();
        let pretty = serde_json::to_vec_pretty(&manifest).unwrap();
        let configuration = config("https://catalog.test/wake-federation.json");
        let lock_a = generate_federation_lock_with(root_a.path(), &configuration, &|_| {
            Ok(ManifestResponse {
                body: compact.clone(),
                ..response(&manifest)
            })
        })
        .unwrap();
        let lock_b = generate_federation_lock_with(root_b.path(), &configuration, &|_| {
            Ok(ManifestResponse {
                body: pretty.clone(),
                ..response(&manifest)
            })
        })
        .unwrap();
        assert_ne!(
            lock_a.remotes[&ContainerName::from("catalog")].manifest_integrity,
            lock_b.remotes[&ContainerName::from("catalog")].manifest_integrity
        );
    }

    #[test]
    fn output_is_deterministic_idempotent_and_never_uses_an_old_lock() {
        let root = tempfile::tempdir().unwrap();
        let manifest = complete_manifest("catalog");
        let configuration = config("https://catalog.test/wake-federation.json");
        let fetches = Cell::new(0_u8);
        let fetch = |_| {
            fetches.set(fetches.get() + 1);
            Ok(response(&manifest))
        };
        let first = generate_federation_lock_with(root.path(), &configuration, &fetch).unwrap();
        let path = root.path().join(LOCK_FILE);
        let first_bytes = std::fs::read(&path).unwrap();
        let second = generate_federation_lock_with(root.path(), &configuration, &fetch).unwrap();
        assert_eq!(first, second);
        assert_eq!(first_bytes, std::fs::read(&path).unwrap());
        assert_eq!(
            fetches.get(),
            2,
            "idempotence must still re-fetch manifests"
        );

        let error = generate_federation_lock_with(root.path(), &configuration, &|_| {
            Err(federation_error(
                ErrorCode::ManifestFetch,
                "network unavailable",
            ))
        })
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ManifestFetch.as_str());
        assert_eq!(first_bytes, std::fs::read(&path).unwrap());
    }
}
