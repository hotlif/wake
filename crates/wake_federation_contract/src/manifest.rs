use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{
    ExposeMode, ReactSharePolicyView, ShadowMode, is_valid_bare_specifier,
    is_valid_coherence_group, is_valid_scope, validate_host_rendered_react_scope,
    validate_render_boundary, validate_scope,
};
use crate::error::{ContractViolation, ErrorCode, ValidationErrors, finish_validation};
use crate::identity::{
    BuildId, ContainerName, ExposeKey, is_non_empty_token, is_valid_container_name,
    is_valid_expose_key, is_valid_identity_token,
};
use crate::{FEDERATION_LOCK_SCHEMA_VERSION, FEDERATION_RUNTIME_ABI, FEDERATION_SCHEMA_VERSION};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum AssetKind {
    #[default]
    #[serde(rename = "javascript")]
    JavaScript,
    Css,
    SourceMap,
    Other,
}

/// One immutable resource in the manifest asset closure.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asset {
    pub kind: AssetKind,
    pub url: String,
    pub content_hash: String,
    pub integrity: String,
    pub mime: String,
    pub size: u64,
}

impl Asset {
    #[must_use]
    pub fn new(
        kind: AssetKind,
        url: impl Into<String>,
        content_hash: impl Into<String>,
        integrity: impl Into<String>,
        mime: impl Into<String>,
        size: u64,
    ) -> Self {
        Self {
            kind,
            url: url.into(),
            content_hash: content_hash.into(),
            integrity: integrity.into(),
            mime: mime.into(),
            size,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExposedModule {
    pub mode: ExposeMode,
    pub scope: String,
    pub shadow: ShadowMode,
    pub entry: Asset,
    pub css: Vec<Asset>,
    pub source_map: Option<Asset>,
    pub synchronous_assets: Vec<Asset>,
    pub asynchronous_assets: Vec<Asset>,
}

/// Resolver-stable package identity used by the share registry.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageKey {
    pub name: String,
    pub version: String,
    pub package_context: String,
    pub build_variant: String,
}

/// Runtime selection policy attached to an offer or requirement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedPolicy {
    pub scope: String,
    pub singleton: bool,
    pub strict: bool,
    pub fallback: bool,
    pub coherence_group: Option<String>,
    pub owner: Option<ContainerName>,
}

impl Default for SharedPolicy {
    fn default() -> Self {
        Self {
            scope: "default".to_owned(),
            singleton: false,
            strict: false,
            fallback: true,
            coherence_group: None,
            owner: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedOffer {
    pub share_key: String,
    pub package: PackageKey,
    pub provider: ContainerName,
    pub policy: SharedPolicy,
    pub asset: Option<Asset>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedRequirement {
    pub share_key: String,
    pub required_version: String,
    pub package_context: String,
    /// Build-time implementation variant (browser conditions, transforms and runtime ABI).
    pub build_variant: String,
    pub policy: SharedPolicy,
    pub fallback: Option<Asset>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedManifest {
    pub offers: Vec<SharedOffer>,
    pub requirements: Vec<SharedRequirement>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum TypeArtifactFormat {
    #[default]
    DeclarationBundle,
}

/// Declaration bundle bound to the exact JavaScript build.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypeArtifact {
    pub build_id: BuildId,
    pub url: String,
    pub content_hash: String,
    pub integrity: String,
    pub size: u64,
    pub format: TypeArtifactFormat,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DevelopmentMetadata {
    pub updates_url: String,
    pub generation: u64,
}

/// Wake-native federation manifest v1.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: String,
    pub runtime_abi: String,
    pub name: ContainerName,
    pub build_id: BuildId,
    pub browser_target: String,
    pub remote_entry: Asset,
    pub remote_entry_source_map: Option<Asset>,
    pub exposes: BTreeMap<ExposeKey, ExposedModule>,
    pub shared: SharedManifest,
    pub types: Option<TypeArtifact>,
    pub development: Option<DevelopmentMetadata>,
}

pub type FederationManifest = Manifest;

impl Manifest {
    #[must_use]
    pub fn new(
        name: ContainerName,
        build_id: BuildId,
        browser_target: impl Into<String>,
        remote_entry: Asset,
    ) -> Self {
        Self {
            schema_version: FEDERATION_SCHEMA_VERSION.to_owned(),
            runtime_abi: FEDERATION_RUNTIME_ABI.to_owned(),
            name,
            build_id,
            browser_target: browser_target.into(),
            remote_entry,
            remote_entry_source_map: None,
            exposes: BTreeMap::new(),
            shared: SharedManifest::default(),
            types: None,
            development: None,
        }
    }

    /// Validate all fail-closed v1 shape invariants in deterministic field order.
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut violations = Vec::new();
        if self.schema_version != FEDERATION_SCHEMA_VERSION {
            violation(
                &mut violations,
                ErrorCode::ManifestSchema,
                "schemaVersion",
                format!("expected {FEDERATION_SCHEMA_VERSION}"),
            );
        }
        if self.runtime_abi != FEDERATION_RUNTIME_ABI {
            violation(
                &mut violations,
                ErrorCode::RuntimeAbi,
                "runtimeAbi",
                format!("expected {FEDERATION_RUNTIME_ABI}"),
            );
        }
        if !is_valid_container_name(self.name.as_str()) {
            violation(
                &mut violations,
                ErrorCode::ManifestSchema,
                "name",
                "invalid container name",
            );
        }
        if !is_valid_identity_token(self.build_id.as_str(), 256) {
            violation(
                &mut violations,
                ErrorCode::ManifestSchema,
                "buildId",
                "buildId must be a non-empty stable token",
            );
        }
        if !is_non_empty_token(&self.browser_target, 512) {
            violation(
                &mut violations,
                ErrorCode::ManifestSchema,
                "browserTarget",
                "browserTarget must be non-empty",
            );
        }
        validate_asset(
            &mut violations,
            "remoteEntry",
            &self.remote_entry,
            Some(AssetKind::JavaScript),
        );
        if let Some(source_map) = &self.remote_entry_source_map {
            validate_asset(
                &mut violations,
                "remoteEntrySourceMap",
                source_map,
                Some(AssetKind::SourceMap),
            );
        }

        for (key, expose) in &self.exposes {
            let base = format!("exposes[{key}]");
            if !is_valid_expose_key(key.as_str()) {
                violation(
                    &mut violations,
                    ErrorCode::ManifestSchema,
                    base.clone(),
                    "expose keys must use the canonical './path' form",
                );
            }
            validate_scope(
                &mut violations,
                ErrorCode::ManifestSchema,
                &format!("{base}.scope"),
                &expose.scope,
            );
            validate_render_boundary(
                &mut violations,
                ErrorCode::ManifestSchema,
                &format!("{base}.shadow"),
                expose.mode,
                expose.shadow,
            );
            if expose.mode == ExposeMode::Isolated && expose.scope == "default" {
                violation(
                    &mut violations,
                    ErrorCode::CoherenceConflict,
                    format!("{base}.scope"),
                    "isolated exposes require a non-default share scope",
                );
            }
            validate_asset(
                &mut violations,
                &format!("{base}.entry"),
                &expose.entry,
                Some(AssetKind::JavaScript),
            );
            for (index, asset) in expose.css.iter().enumerate() {
                validate_asset(
                    &mut violations,
                    &format!("{base}.css[{index}]"),
                    asset,
                    Some(AssetKind::Css),
                );
            }
            if let Some(source_map) = &expose.source_map {
                validate_asset(
                    &mut violations,
                    &format!("{base}.sourceMap"),
                    source_map,
                    Some(AssetKind::SourceMap),
                );
            }
            for (field, assets) in [
                ("synchronousAssets", &expose.synchronous_assets),
                ("asynchronousAssets", &expose.asynchronous_assets),
            ] {
                for (index, asset) in assets.iter().enumerate() {
                    validate_asset(
                        &mut violations,
                        &format!("{base}.{field}[{index}]"),
                        asset,
                        None,
                    );
                }
            }
        }

        for (index, offer) in self.shared.offers.iter().enumerate() {
            let base = format!("shared.offers[{index}]");
            validate_share_key(
                &mut violations,
                &format!("{base}.shareKey"),
                &offer.share_key,
            );
            validate_package_key(&mut violations, &format!("{base}.package"), &offer.package);
            if !is_valid_container_name(offer.provider.as_str()) {
                violation(
                    &mut violations,
                    ErrorCode::ManifestSchema,
                    format!("{base}.provider"),
                    "provider must be a valid container name",
                );
            } else if offer.provider != self.name {
                violation(
                    &mut violations,
                    ErrorCode::ManifestSchema,
                    format!("{base}.provider"),
                    "a manifest may only offer dependencies from its own container",
                );
            }
            validate_shared_policy(&mut violations, &format!("{base}.policy"), &offer.policy);
            if let Some(asset) = &offer.asset {
                validate_asset(
                    &mut violations,
                    &format!("{base}.asset"),
                    asset,
                    Some(AssetKind::JavaScript),
                );
            }
        }

        for (index, requirement) in self.shared.requirements.iter().enumerate() {
            let base = format!("shared.requirements[{index}]");
            validate_share_key(
                &mut violations,
                &format!("{base}.shareKey"),
                &requirement.share_key,
            );
            if !is_non_empty_token(&requirement.required_version, 128) {
                violation(
                    &mut violations,
                    ErrorCode::ManifestSchema,
                    format!("{base}.requiredVersion"),
                    "requiredVersion must be a non-empty range",
                );
            }
            if !is_non_empty_token(&requirement.package_context, 512) {
                violation(
                    &mut violations,
                    ErrorCode::ManifestSchema,
                    format!("{base}.packageContext"),
                    "packageContext must be a non-empty stable token",
                );
            }
            if !is_non_empty_token(&requirement.build_variant, 256) {
                violation(
                    &mut violations,
                    ErrorCode::ManifestSchema,
                    format!("{base}.buildVariant"),
                    "buildVariant must be a non-empty stable token",
                );
            }
            validate_shared_policy(
                &mut violations,
                &format!("{base}.policy"),
                &requirement.policy,
            );
            if let Some(asset) = &requirement.fallback {
                validate_asset(
                    &mut violations,
                    &format!("{base}.fallback"),
                    asset,
                    Some(AssetKind::JavaScript),
                );
            }
        }

        let host_rendered_scopes = self
            .exposes
            .iter()
            .filter(|(_, expose)| {
                expose.mode == ExposeMode::HostRendered && is_valid_scope(&expose.scope)
            })
            .map(|(key, expose)| (expose.scope.as_str(), format!("exposes[{key}].scope")))
            .collect::<BTreeMap<_, _>>();
        let react_policies = self
            .shared
            .requirements
            .iter()
            .enumerate()
            .map(|(index, requirement)| ReactSharePolicyView {
                share_key: &requirement.share_key,
                scope: &requirement.policy.scope,
                singleton: requirement.policy.singleton,
                coherence_group: requirement.policy.coherence_group.as_deref(),
                owner: requirement.policy.owner.as_ref(),
                policy_path: format!("shared.requirements[{index}].policy"),
            })
            .collect::<Vec<_>>();
        for (scope, expose_path) in host_rendered_scopes {
            validate_host_rendered_react_scope(
                &mut violations,
                ErrorCode::CoherenceConflict,
                &expose_path,
                scope,
                &react_policies,
            );
        }

        if let Some(types) = &self.types {
            if types.build_id != self.build_id {
                violation(
                    &mut violations,
                    ErrorCode::TypeBuildMismatch,
                    "types.buildId",
                    "type artifacts must be bound to the manifest buildId",
                );
            }
            validate_location(&mut violations, "types.url", &types.url);
            validate_content_hash(&mut violations, "types.contentHash", &types.content_hash);
            validate_integrity(&mut violations, "types.integrity", &types.integrity);
        }

        if let Some(development) = &self.development
            && !is_non_empty_token(&development.updates_url, 4096)
        {
            violation(
                &mut violations,
                ErrorCode::ManifestSchema,
                "development.updatesUrl",
                "updatesUrl must be non-empty",
            );
        }

        validate_unique_asset_metadata(&mut violations, self);

        finish_validation(violations)
    }

    /// Validate the additional policy required before a Manifest may enter a production lock.
    ///
    /// Development may intentionally leave singleton ownership open while iterating. Production
    /// cannot: navigation order must never select a singleton owner, and public exposes must carry
    /// build-bound declarations.
    pub fn validate_for_production_lock(&self) -> Result<(), ValidationErrors> {
        self.validate()?;
        let mut violations = Vec::new();
        if !self.exposes.is_empty() && self.types.is_none() {
            violation(
                &mut violations,
                ErrorCode::TypeBuildMismatch,
                "types",
                "manifests with public exposes require a build-bound declaration artifact",
            );
        }
        for (index, offer) in self.shared.offers.iter().enumerate() {
            if offer.policy.singleton && offer.policy.owner.is_none() {
                violation(
                    &mut violations,
                    ErrorCode::ConfigInvalid,
                    format!("shared.offers[{index}].policy.owner"),
                    "production singleton offers require a deterministic owner",
                );
            }
        }
        for (index, requirement) in self.shared.requirements.iter().enumerate() {
            if requirement.policy.singleton && requirement.policy.owner.is_none() {
                violation(
                    &mut violations,
                    ErrorCode::ConfigInvalid,
                    format!("shared.requirements[{index}].policy.owner"),
                    "production singleton requirements require a deterministic owner",
                );
            }
        }
        finish_validation(violations)
    }

    /// Stable JSON bytes for signing or storing a complete manifest.
    ///
    /// Set-like collections are sorted before serialization and maps use `BTreeMap`.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ValidationErrors> {
        self.validate()?;
        let normalized = self.normalized();
        Ok(serde_json::to_vec(&normalized)
            .expect("the federation manifest contains only JSON-serializable DTOs"))
    }

    /// Stable build-identity material without performing a hash.
    ///
    /// Deployment URLs, `buildId`, type `buildId`, and development metadata are
    /// excluded. This prevents deployment location and the resulting identifier
    /// from becoming circular inputs. Callers own the hash algorithm and spelling.
    pub fn canonical_build_identity_bytes(&self) -> Result<Vec<u8>, ValidationErrors> {
        self.validate()?;
        let mut value = serde_json::to_value(self.normalized_for_identity())
            .expect("the federation manifest contains only JSON-serializable DTOs");
        if let Value::Object(root) = &mut value {
            root.remove("buildId");
            root.remove("development");
        }
        strip_deployment_fields(&mut value);
        Ok(serde_json::to_vec(&value)
            .expect("canonical federation identity contains only JSON values"))
    }

    fn normalized(&self) -> Self {
        let mut normalized = self.clone();
        normalized.shared.offers.sort();
        normalized.shared.requirements.sort();
        normalized
    }

    fn normalized_for_identity(&self) -> Self {
        let mut normalized = self.clone();
        normalized.development = None;
        normalized.remote_entry.url.clear();
        if let Some(source_map) = &mut normalized.remote_entry_source_map {
            source_map.url.clear();
        }
        for expose in normalized.exposes.values_mut() {
            expose.entry.url.clear();
            for asset in &mut expose.css {
                asset.url.clear();
            }
            if let Some(source_map) = &mut expose.source_map {
                source_map.url.clear();
            }
            for asset in expose
                .synchronous_assets
                .iter_mut()
                .chain(&mut expose.asynchronous_assets)
            {
                asset.url.clear();
            }
        }
        for offer in &mut normalized.shared.offers {
            if let Some(asset) = &mut offer.asset {
                asset.url.clear();
            }
        }
        for requirement in &mut normalized.shared.requirements {
            if let Some(asset) = &mut requirement.fallback {
                asset.url.clear();
            }
        }
        normalized.shared.offers.sort();
        normalized.shared.requirements.sort();
        if let Some(types) = &mut normalized.types {
            types.url.clear();
        }
        normalized
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AssetIdentity {
    kind: AssetKind,
    content_hash: String,
    integrity: String,
    mime: String,
    size: u64,
}

fn validate_unique_asset_metadata(violations: &mut Vec<ContractViolation>, manifest: &Manifest) {
    let mut seen = BTreeMap::<String, (AssetIdentity, String)>::new();
    let mut register = |path: String, url: &str, identity: AssetIdentity| {
        if url.is_empty() {
            return;
        }
        if let Some((previous, previous_path)) = seen.get(url) {
            if previous != &identity {
                violation(
                    violations,
                    ErrorCode::AssetIntegrity,
                    format!("{path}.url"),
                    format!(
                        "asset URL conflicts with metadata already declared at {previous_path}"
                    ),
                );
            }
            return;
        }
        seen.insert(url.to_owned(), (identity, path));
    };
    let mut register_asset = |path: String, asset: &Asset| {
        register(
            path,
            &asset.url,
            AssetIdentity {
                kind: asset.kind,
                content_hash: asset.content_hash.clone(),
                integrity: asset.integrity.clone(),
                mime: asset.mime.clone(),
                size: asset.size,
            },
        );
    };

    register_asset("remoteEntry".to_owned(), &manifest.remote_entry);
    if let Some(asset) = &manifest.remote_entry_source_map {
        register_asset("remoteEntrySourceMap".to_owned(), asset);
    }
    for (key, expose) in &manifest.exposes {
        let base = format!("exposes[{key}]");
        register_asset(format!("{base}.entry"), &expose.entry);
        for (index, asset) in expose.css.iter().enumerate() {
            register_asset(format!("{base}.css[{index}]"), asset);
        }
        if let Some(asset) = &expose.source_map {
            register_asset(format!("{base}.sourceMap"), asset);
        }
        for (field, assets) in [
            ("synchronousAssets", &expose.synchronous_assets),
            ("asynchronousAssets", &expose.asynchronous_assets),
        ] {
            for (index, asset) in assets.iter().enumerate() {
                register_asset(format!("{base}.{field}[{index}]"), asset);
            }
        }
    }
    for (index, offer) in manifest.shared.offers.iter().enumerate() {
        if let Some(asset) = &offer.asset {
            register_asset(format!("shared.offers[{index}].asset"), asset);
        }
    }
    for (index, requirement) in manifest.shared.requirements.iter().enumerate() {
        if let Some(asset) = &requirement.fallback {
            register_asset(format!("shared.requirements[{index}].fallback"), asset);
        }
    }
    if let Some(types) = &manifest.types {
        register(
            "types".to_owned(),
            &types.url,
            AssetIdentity {
                kind: AssetKind::Other,
                content_hash: types.content_hash.clone(),
                integrity: types.integrity.clone(),
                mime: "application/json".to_owned(),
                size: types.size,
            },
        );
    }
}

/// One production lock entry. `allowed_assets` maps exact URL to exact SRI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteRef {
    pub manifest_url: String,
    pub build_id: BuildId,
    pub manifest_integrity: String,
    /// Whether the integrity-bound Manifest publishes any public exposes.
    ///
    /// This is required lock metadata: consumers cannot infer it from optional declarations or
    /// the asset closure without making an exposed remote indistinguishable from a shared-only
    /// remote after tampering.
    pub has_exposes: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types_integrity: Option<String>,
    pub allowed_assets: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationLock {
    pub schema_version: String,
    pub remotes: BTreeMap<ContainerName, RemoteRef>,
}

impl FederationLock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema_version: FEDERATION_LOCK_SCHEMA_VERSION.to_owned(),
            remotes: BTreeMap::new(),
        }
    }

    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut violations = Vec::new();
        if self.schema_version != FEDERATION_LOCK_SCHEMA_VERSION {
            violation(
                &mut violations,
                ErrorCode::ManifestSchema,
                "schemaVersion",
                format!("expected {FEDERATION_LOCK_SCHEMA_VERSION}"),
            );
        }
        for (name, remote) in &self.remotes {
            let base = format!("remotes[{name}]");
            if !is_valid_container_name(name.as_str()) {
                violation(
                    &mut violations,
                    ErrorCode::ManifestSchema,
                    base.clone(),
                    "invalid remote container name",
                );
            }
            if !is_https_url(&remote.manifest_url) {
                violation(
                    &mut violations,
                    ErrorCode::OriginDenied,
                    format!("{base}.manifestUrl"),
                    "production lock manifests require HTTPS",
                );
            }
            if !is_valid_identity_token(remote.build_id.as_str(), 256) {
                violation(
                    &mut violations,
                    ErrorCode::ManifestSchema,
                    format!("{base}.buildId"),
                    "buildId must be non-empty",
                );
            }
            validate_integrity_with_code(
                &mut violations,
                &format!("{base}.manifestIntegrity"),
                &remote.manifest_integrity,
                ErrorCode::ManifestIntegrity,
            );
            if let Some(integrity) = &remote.types_integrity {
                validate_integrity(
                    &mut violations,
                    &format!("{base}.typesIntegrity"),
                    integrity,
                );
            }
            if remote.has_exposes && remote.types_integrity.is_none() {
                violation(
                    &mut violations,
                    ErrorCode::TypeBuildMismatch,
                    format!("{base}.typesIntegrity"),
                    "remotes with public exposes require a locked declaration artifact",
                );
            }
            for (url, integrity) in &remote.allowed_assets {
                if !is_https_url(url) {
                    violation(
                        &mut violations,
                        ErrorCode::OriginDenied,
                        format!("{base}.allowedAssets[{url}]"),
                        "production asset closure requires HTTPS",
                    );
                }
                validate_integrity(
                    &mut violations,
                    &format!("{base}.allowedAssets[{url}]"),
                    integrity,
                );
            }
        }
        finish_validation(violations)
    }
}

fn validate_shared_policy(
    violations: &mut Vec<ContractViolation>,
    base: &str,
    policy: &SharedPolicy,
) {
    validate_scope(
        violations,
        ErrorCode::ManifestSchema,
        &format!("{base}.scope"),
        &policy.scope,
    );
    if let Some(group) = &policy.coherence_group
        && !is_valid_coherence_group(group)
    {
        violation(
            violations,
            ErrorCode::ManifestSchema,
            format!("{base}.coherenceGroup"),
            "coherenceGroup must be a non-empty stable token",
        );
    }
    if policy.coherence_group.is_some() && !policy.singleton {
        violation(
            violations,
            ErrorCode::CoherenceConflict,
            format!("{base}.singleton"),
            "coherenceGroup requires singleton=true",
        );
    }
    if let Some(owner) = &policy.owner
        && !is_valid_container_name(owner.as_str())
    {
        violation(
            violations,
            ErrorCode::ManifestSchema,
            format!("{base}.owner"),
            "owner must be a valid container name",
        );
    }
    if policy.owner.is_some() && !policy.singleton {
        violation(
            violations,
            ErrorCode::ShareSingletonConflict,
            format!("{base}.owner"),
            "only singleton policies may pin an owner",
        );
    }
}

fn validate_package_key(violations: &mut Vec<ContractViolation>, base: &str, package: &PackageKey) {
    for (field, value, maximum) in [
        ("version", package.version.as_str(), 128),
        ("packageContext", package.package_context.as_str(), 512),
        ("buildVariant", package.build_variant.as_str(), 256),
    ] {
        if !is_valid_identity_token(value, maximum) {
            violation(
                violations,
                ErrorCode::ManifestSchema,
                format!("{base}.{field}"),
                format!("{field} must be a non-empty stable token"),
            );
        }
    }
    if !is_valid_bare_specifier(&package.name) {
        violation(
            violations,
            ErrorCode::ManifestSchema,
            format!("{base}.name"),
            "name must be a valid bare package specifier",
        );
    }
}

fn validate_share_key(violations: &mut Vec<ContractViolation>, path: &str, value: &str) {
    if !is_valid_bare_specifier(value) {
        violation(
            violations,
            ErrorCode::ManifestSchema,
            path,
            "shareKey must be a valid bare package specifier",
        );
    }
}

fn validate_asset(
    violations: &mut Vec<ContractViolation>,
    base: &str,
    asset: &Asset,
    expected_kind: Option<AssetKind>,
) {
    if expected_kind.is_some_and(|expected| expected != asset.kind) {
        violation(
            violations,
            ErrorCode::ManifestSchema,
            format!("{base}.kind"),
            format!("unexpected asset kind {:?}", asset.kind),
        );
    }
    validate_location(violations, &format!("{base}.url"), &asset.url);
    validate_content_hash(
        violations,
        &format!("{base}.contentHash"),
        &asset.content_hash,
    );
    validate_integrity(violations, &format!("{base}.integrity"), &asset.integrity);
    let mime_matches = match asset.kind {
        AssetKind::JavaScript => matches!(
            asset.mime.as_str(),
            "text/javascript" | "application/javascript"
        ),
        AssetKind::Css => asset.mime == "text/css",
        AssetKind::SourceMap => matches!(
            asset.mime.as_str(),
            "application/json" | "application/source-map+json"
        ),
        AssetKind::Other => is_non_empty_token(&asset.mime, 256),
    };
    if !mime_matches {
        violation(
            violations,
            ErrorCode::AssetMime,
            format!("{base}.mime"),
            format!("MIME '{}' does not match {:?}", asset.mime, asset.kind),
        );
    }
}

fn validate_location(violations: &mut Vec<ContractViolation>, path: &str, value: &str) {
    if !is_non_empty_token(value, 4096) {
        violation(
            violations,
            ErrorCode::ManifestSchema,
            path,
            "asset locations must be non-empty and contain no control characters",
        );
    }
}

fn validate_content_hash(violations: &mut Vec<ContractViolation>, path: &str, value: &str) {
    if !is_valid_identity_token(value, 256) {
        violation(
            violations,
            ErrorCode::ManifestSchema,
            path,
            "content hashes must be non-empty stable tokens",
        );
    }
}

fn validate_integrity(violations: &mut Vec<ContractViolation>, path: &str, value: &str) {
    validate_integrity_with_code(violations, path, value, ErrorCode::AssetIntegrity);
}

fn validate_integrity_with_code(
    violations: &mut Vec<ContractViolation>,
    path: &str,
    value: &str,
    code: ErrorCode,
) {
    let valid = value
        .strip_prefix("sha384-")
        .is_some_and(|digest| digest.len() == 64 && digest.bytes().all(is_base64_byte));
    if !valid {
        violation(
            violations,
            code,
            path,
            "integrity must be one SHA-384 SRI token",
        );
    }
}

const fn is_base64_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/')
}

fn is_https_url(value: &str) -> bool {
    value
        .strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty() && !rest.chars().any(char::is_control))
}

fn violation(
    violations: &mut Vec<ContractViolation>,
    code: ErrorCode,
    path: impl Into<String>,
    message: impl Into<String>,
) {
    violations.push(ContractViolation::new(code, path, message));
}

fn strip_deployment_fields(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                strip_deployment_fields(value);
            }
        }
        Value::Object(object) => {
            object.remove("url");
            object.remove("buildId");
            for value in object.values_mut() {
                strip_deployment_fields(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::REACT_COHERENCE_MEMBERS;

    const SRI: &str = "sha384-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    fn js(url: &str, hash: &str) -> Asset {
        Asset::new(AssetKind::JavaScript, url, hash, SRI, "text/javascript", 42)
    }

    fn react_requirement(share_key: &str) -> SharedRequirement {
        SharedRequirement {
            share_key: share_key.to_owned(),
            required_version: "^18.0.0".to_owned(),
            package_context: format!("{share_key}@18"),
            build_variant: "browser-import".to_owned(),
            policy: SharedPolicy {
                scope: "react18".to_owned(),
                singleton: true,
                strict: true,
                fallback: false,
                coherence_group: Some("react18".to_owned()),
                owner: Some("shell".into()),
            },
            fallback: None,
        }
    }

    fn manifest() -> Manifest {
        let mut manifest = Manifest::new(
            "catalog".into(),
            "build-a".into(),
            "chrome>=120",
            js("https://cdn.test/a/remote.js", "remote-hash"),
        );
        manifest.exposes.insert(
            "./Button".into(),
            ExposedModule {
                mode: ExposeMode::HostRendered,
                scope: "react18".to_owned(),
                shadow: ShadowMode::None,
                entry: js("https://cdn.test/a/button.js", "button-hash"),
                ..ExposedModule::default()
            },
        );
        manifest.shared.requirements = REACT_COHERENCE_MEMBERS
            .into_iter()
            .map(react_requirement)
            .collect();
        manifest.types = Some(TypeArtifact {
            build_id: "build-a".into(),
            url: "https://cdn.test/a/types.tgz".to_owned(),
            content_hash: "types-hash".to_owned(),
            integrity: SRI.to_owned(),
            size: 128,
            format: TypeArtifactFormat::DeclarationBundle,
        });
        manifest
    }

    #[test]
    fn manifest_uses_the_versioned_camel_case_shape() {
        let value = serde_json::to_value(manifest()).unwrap();
        assert_eq!(value["schemaVersion"], FEDERATION_SCHEMA_VERSION);
        assert_eq!(value["runtimeAbi"], FEDERATION_RUNTIME_ABI);
        assert_eq!(value["remoteEntry"]["kind"], "javascript");
        assert_eq!(value["exposes"]["./Button"]["mode"], "host-rendered");
        assert!(value.get("schema_version").is_none());
    }

    #[test]
    fn shared_requirements_carry_the_build_variant_on_the_wire() {
        let requirement = SharedRequirement {
            share_key: "react".to_owned(),
            required_version: "^18.0.0".to_owned(),
            package_context: "react@18".to_owned(),
            build_variant: "browser-import".to_owned(),
            policy: SharedPolicy::default(),
            fallback: None,
        };
        let mut value = serde_json::to_value(&requirement).unwrap();
        assert_eq!(value["buildVariant"], "browser-import");
        value.as_object_mut().unwrap().remove("buildVariant");
        assert!(serde_json::from_value::<SharedRequirement>(value).is_err());
    }

    #[test]
    fn production_manifest_requires_types_and_singleton_owners() {
        manifest().validate_for_production_lock().unwrap();
        let base = || {
            let mut manifest = manifest();
            let expose = manifest
                .exposes
                .get_mut(&ExposeKey::from("./Button"))
                .unwrap();
            expose.mode = ExposeMode::Generic;
            expose.scope = "default".to_owned();
            manifest.shared.requirements.clear();
            manifest
        };

        let mut missing_types = base();
        missing_types.types = None;
        missing_types.validate().unwrap();
        let error = missing_types.validate_for_production_lock().unwrap_err();
        assert!(error.violations.iter().any(|violation| {
            violation.code == ErrorCode::TypeBuildMismatch && violation.path == "types"
        }));

        let mut ownerless_offer = base();
        ownerless_offer.shared.offers.push(SharedOffer {
            share_key: "react".to_owned(),
            package: PackageKey {
                name: "react".to_owned(),
                version: "18.3.0".to_owned(),
                package_context: "react@18".to_owned(),
                build_variant: "browser-import".to_owned(),
            },
            provider: "catalog".into(),
            policy: SharedPolicy {
                singleton: true,
                ..SharedPolicy::default()
            },
            asset: None,
        });
        ownerless_offer.validate().unwrap();
        let error = ownerless_offer.validate_for_production_lock().unwrap_err();
        assert!(error.violations.iter().any(|violation| {
            violation.code == ErrorCode::ConfigInvalid
                && violation.path == "shared.offers[0].policy.owner"
        }));

        let mut ownerless_requirement = base();
        ownerless_requirement
            .shared
            .requirements
            .push(SharedRequirement {
                share_key: "react".to_owned(),
                required_version: "^18.0.0".to_owned(),
                package_context: "react@18".to_owned(),
                build_variant: "browser-import".to_owned(),
                policy: SharedPolicy {
                    singleton: true,
                    ..SharedPolicy::default()
                },
                fallback: None,
            });
        ownerless_requirement.validate().unwrap();
        let error = ownerless_requirement
            .validate_for_production_lock()
            .unwrap_err();
        assert!(error.violations.iter().any(|violation| {
            violation.code == ErrorCode::ConfigInvalid
                && violation.path == "shared.requirements[0].policy.owner"
        }));
    }

    #[test]
    fn host_rendered_manifest_requires_all_five_react_members() {
        let mut manifest = manifest();
        manifest
            .shared
            .requirements
            .retain(|requirement| requirement.share_key != "react-dom/client");

        let error = manifest.validate().unwrap_err();
        assert!(error.violations.iter().any(|violation| {
            violation.code == ErrorCode::CoherenceConflict
                && violation.path == "exposes[./Button].scope"
                && violation.message.contains("react-dom/client")
        }));
    }

    #[test]
    fn host_rendered_manifest_requires_one_singleton_group_and_owner() {
        let mut manifest = manifest();
        manifest.shared.requirements[0].policy.singleton = false;
        manifest.shared.requirements[1].policy.coherence_group = Some("other-react".to_owned());
        manifest.shared.requirements[2].policy.owner = Some("catalog".into());
        manifest.shared.requirements[3].policy.coherence_group = None;

        let error = manifest.validate().unwrap_err();
        for expected_path in [
            "shared.requirements[0].policy.singleton",
            "shared.requirements[1].policy.coherenceGroup",
            "shared.requirements[2].policy.owner",
            "shared.requirements[3].policy.coherenceGroup",
        ] {
            assert!(
                error
                    .violations
                    .iter()
                    .any(|violation| violation.path == expected_path),
                "missing diagnostic at {expected_path}: {error}"
            );
        }
    }

    #[test]
    fn generic_and_isolated_manifest_exposes_do_not_require_react_group() {
        for (mode, scope, shadow) in [
            (ExposeMode::Generic, "default", ShadowMode::None),
            (ExposeMode::Isolated, "react17", ShadowMode::Open),
        ] {
            let mut manifest = manifest();
            let expose = manifest
                .exposes
                .get_mut(&ExposeKey::from("./Button"))
                .unwrap();
            expose.mode = mode;
            expose.scope = scope.to_owned();
            expose.shadow = shadow;
            manifest.shared.requirements.clear();

            manifest.validate().unwrap();
        }
    }

    #[test]
    fn type_artifacts_fail_when_build_ids_drift() {
        let mut manifest = manifest();
        manifest.types.as_mut().unwrap().build_id = "build-b".into();
        let error = manifest.validate().unwrap_err();
        assert!(error.violations.iter().any(|violation| {
            violation.code == ErrorCode::TypeBuildMismatch && violation.path == "types.buildId"
        }));
    }

    #[test]
    fn build_identity_is_set_order_and_deployment_independent() {
        let mut left = manifest();
        let mut right = manifest();
        left.exposes
            .get_mut(&ExposeKey::from("./Button"))
            .unwrap()
            .css = vec![
            Asset::new(AssetKind::Css, "/a.css", "z", SRI, "text/css", 2),
            Asset::new(AssetKind::Css, "/z.css", "a", SRI, "text/css", 1),
        ];
        right
            .exposes
            .get_mut(&ExposeKey::from("./Button"))
            .unwrap()
            .css = vec![
            Asset::new(
                AssetKind::Css,
                "https://other.test/z.css",
                "z",
                SRI,
                "text/css",
                2,
            ),
            Asset::new(
                AssetKind::Css,
                "https://other.test/a.css",
                "a",
                SRI,
                "text/css",
                1,
            ),
        ];
        right.build_id = "different-output-id".into();
        right.remote_entry.url = "https://other.test/remote.js".to_owned();
        right.types.as_mut().unwrap().build_id = right.build_id.clone();
        right.types.as_mut().unwrap().url = "https://other.test/types.tgz".to_owned();
        assert_eq!(
            left.canonical_build_identity_bytes().unwrap(),
            right.canonical_build_identity_bytes().unwrap()
        );
    }

    #[test]
    fn integrity_and_mime_fail_closed() {
        let mut manifest = manifest();
        manifest.remote_entry.integrity = "sha256-weak".to_owned();
        manifest.remote_entry.mime = "text/html".to_owned();
        let error = manifest.validate().unwrap_err();
        assert!(
            error
                .violations
                .iter()
                .any(|violation| violation.code == ErrorCode::AssetIntegrity)
        );
        assert!(
            error
                .violations
                .iter()
                .any(|violation| violation.code == ErrorCode::AssetMime)
        );
    }

    #[test]
    fn one_asset_url_cannot_claim_conflicting_metadata() {
        let mut manifest = manifest();
        let expose = manifest
            .exposes
            .get_mut(&ExposeKey::from("./Button"))
            .unwrap();
        let mut conflicting = expose.entry.clone();
        conflicting.kind = AssetKind::Css;
        conflicting.mime = "text/css".to_owned();
        expose.css.push(conflicting);

        let error = manifest.validate().unwrap_err();
        assert!(error.violations.iter().any(|violation| {
            violation.code == ErrorCode::AssetIntegrity
                && violation.path == "exposes[./Button].css[0].url"
        }));

        let expose = manifest
            .exposes
            .get_mut(&ExposeKey::from("./Button"))
            .unwrap();
        expose.css.clear();
        expose.synchronous_assets.push(expose.entry.clone());
        manifest.validate().unwrap();
    }

    #[test]
    fn production_lock_requires_https_and_exact_sri() {
        let mut lock = FederationLock::new();
        lock.remotes.insert(
            "catalog".into(),
            RemoteRef {
                manifest_url: "http://catalog.test/manifest.json".to_owned(),
                build_id: "build-a".into(),
                manifest_integrity: SRI.to_owned(),
                ..RemoteRef::default()
            },
        );
        let error = lock.validate().unwrap_err();
        assert_eq!(error.violations[0].code, ErrorCode::OriginDenied);
    }

    #[test]
    fn production_lock_requires_explicit_expose_presence_and_types_for_exposed_remotes() {
        let missing_presence = serde_json::json!({
            "schemaVersion": FEDERATION_LOCK_SCHEMA_VERSION,
            "remotes": {
                "catalog": {
                    "manifestUrl": "https://catalog.test/manifest.json",
                    "buildId": "build-a",
                    "manifestIntegrity": SRI,
                    "typesIntegrity": SRI,
                    "allowedAssets": {}
                }
            }
        });
        assert!(
            serde_json::from_value::<FederationLock>(missing_presence).is_err(),
            "hasExposes is required so a tampered exposed lock cannot masquerade as shared-only"
        );

        let mut lock = FederationLock::new();
        lock.remotes.insert(
            "catalog".into(),
            RemoteRef {
                manifest_url: "https://catalog.test/manifest.json".to_owned(),
                build_id: "build-a".into(),
                manifest_integrity: SRI.to_owned(),
                has_exposes: true,
                types_integrity: None,
                allowed_assets: BTreeMap::new(),
            },
        );
        let error = lock.validate().unwrap_err();
        assert!(error.violations.iter().any(|violation| {
            violation.code == ErrorCode::TypeBuildMismatch
                && violation.path == "remotes[catalog].typesIntegrity"
        }));

        lock.remotes.get_mut(&"catalog".into()).unwrap().has_exposes = false;
        lock.validate().unwrap();
        let serialized = serde_json::to_value(&lock).unwrap();
        assert_eq!(
            serialized,
            serde_json::json!({
                "schemaVersion": FEDERATION_LOCK_SCHEMA_VERSION,
                "remotes": {
                    "catalog": {
                        "manifestUrl": "https://catalog.test/manifest.json",
                        "buildId": "build-a",
                        "manifestIntegrity": SRI,
                        "hasExposes": false,
                        "allowedAssets": {}
                    }
                }
            }),
            "the runtime fixture for a shared-only remote must omit typesIntegrity"
        );

        let legacy_null = serde_json::json!({
            "schemaVersion": FEDERATION_LOCK_SCHEMA_VERSION,
            "remotes": {
                "catalog": {
                    "manifestUrl": "https://catalog.test/manifest.json",
                    "buildId": "build-a",
                    "manifestIntegrity": SRI,
                    "hasExposes": false,
                    "typesIntegrity": null,
                    "allowedAssets": {}
                }
            }
        });
        let legacy_lock = serde_json::from_value::<FederationLock>(legacy_null).unwrap();
        assert_eq!(
            legacy_lock.remotes[&"catalog".into()].types_integrity,
            None,
            "v1 locks written before None omission remain readable"
        );
        legacy_lock.validate().unwrap();
    }
}
