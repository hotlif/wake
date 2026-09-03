use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{ContractViolation, ErrorCode, ValidationErrors, finish_validation};
use crate::identity::{
    ContainerName, ExposeKey, is_non_empty_token, is_valid_config_expose_key,
    is_valid_container_name,
};

fn default_scope() -> String {
    "default".to_owned()
}

const fn default_true() -> bool {
    true
}

/// React and its renderer must be selected as one implementation whenever an expose joins the
/// host's React tree. Subpath exports are separate module identities at runtime, so sharing only
/// `react` is not sufficient to prevent a split Hooks/JSX runtime.
pub(crate) const REACT_COHERENCE_MEMBERS: [&str; 5] = [
    "react",
    "react/jsx-runtime",
    "react/jsx-dev-runtime",
    "react-dom",
    "react-dom/client",
];

pub(crate) struct ReactSharePolicyView<'a> {
    pub share_key: &'a str,
    pub scope: &'a str,
    pub singleton: bool,
    pub coherence_group: Option<&'a str>,
    pub owner: Option<&'a ContainerName>,
    pub policy_path: String,
}

/// Rendering boundary of an exposed module.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ExposeMode {
    #[default]
    Generic,
    HostRendered,
    Isolated,
}

/// Shadow-root contract for an exposed module.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ShadowMode {
    #[default]
    None,
    Open,
}

/// Project-level federation configuration shared by config, bundler and product edges.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationConfig {
    pub enabled: bool,
    pub name: ContainerName,
    pub remotes: BTreeMap<ContainerName, RemoteConfig>,
    pub exposes: BTreeMap<ExposeKey, ExposeConfig>,
    pub shared: BTreeMap<String, SharedConfig>,
}

/// Public API spelling used by Node and other programmatic callers.
pub type FederationOptions = FederationConfig;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteConfig {
    #[serde(alias = "manifest_url")]
    pub manifest_url: String,
    #[serde(alias = "allowed_origins")]
    pub allowed_origins: Vec<String>,
    #[serde(default = "default_true", alias = "dev_follow")]
    pub dev_follow: bool,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            manifest_url: String::new(),
            allowed_origins: Vec::new(),
            dev_follow: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExposeConfig {
    pub entry: String,
    pub mode: ExposeMode,
    #[serde(default = "default_scope")]
    pub scope: String,
    pub shadow: ShadowMode,
    /// Host-rendered components must opt in before unscoped CSS may enter the host document.
    #[serde(default, alias = "allow_global_css")]
    pub allow_global_css: bool,
}

impl<'de> Deserialize<'de> for ExposeConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(default, rename_all = "camelCase", deny_unknown_fields)]
        struct RawExposeConfig {
            entry: String,
            mode: ExposeMode,
            scope: Option<String>,
            shadow: Option<ShadowMode>,
            #[serde(alias = "allow_global_css")]
            allow_global_css: bool,
        }

        impl Default for RawExposeConfig {
            fn default() -> Self {
                Self {
                    entry: String::new(),
                    mode: ExposeMode::Generic,
                    scope: None,
                    shadow: None,
                    allow_global_css: false,
                }
            }
        }

        let raw = RawExposeConfig::deserialize(deserializer)?;
        Ok(Self {
            entry: raw.entry,
            mode: raw.mode,
            scope: raw.scope.unwrap_or_else(|| match raw.mode {
                ExposeMode::Generic => default_scope(),
                ExposeMode::HostRendered | ExposeMode::Isolated => String::new(),
            }),
            shadow: raw.shadow.unwrap_or(match raw.mode {
                ExposeMode::Isolated => ShadowMode::Open,
                ExposeMode::Generic | ExposeMode::HostRendered => ShadowMode::None,
            }),
            allow_global_css: raw.allow_global_css,
        })
    }
}

impl Default for ExposeConfig {
    fn default() -> Self {
        Self {
            entry: String::new(),
            mode: ExposeMode::Generic,
            scope: default_scope(),
            shadow: ShadowMode::None,
            allow_global_css: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedConfig {
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(alias = "required_version")]
    pub required_version: Option<String>,
    pub singleton: bool,
    pub strict: bool,
    #[serde(default = "default_true")]
    pub fallback: bool,
    #[serde(alias = "coherence_group")]
    pub coherence_group: Option<String>,
    /// Deterministic singleton owner. Production builds require this for singleton shares.
    pub owner: Option<ContainerName>,
}

impl Default for SharedConfig {
    fn default() -> Self {
        Self {
            scope: default_scope(),
            required_version: None,
            singleton: false,
            strict: false,
            fallback: true,
            coherence_group: None,
            owner: None,
        }
    }
}

impl FederationConfig {
    /// Normalize set-like inputs and contextual expose defaults, then validate.
    pub fn validate_and_normalize(mut self) -> Result<Self, ValidationErrors> {
        for remote in self.remotes.values_mut() {
            if let Ok(normalized) = normalize_http_url(&remote.manifest_url, false) {
                remote.manifest_url = normalized;
            }
            remote.allowed_origins = std::mem::take(&mut remote.allowed_origins)
                .into_iter()
                .map(|origin| normalize_http_url(&origin, true).unwrap_or(origin))
                .collect();
            remote.allowed_origins.sort();
            remote.allowed_origins.dedup();
        }

        let mut exposes = BTreeMap::new();
        let mut normalization_violations = Vec::new();
        for (key, mut expose) in self.exposes {
            let canonical = if key.as_str().starts_with("./") {
                key
            } else {
                ExposeKey::new(format!("./{}", key.as_str()))
            };
            if expose.mode == ExposeMode::Generic && expose.scope.is_empty() {
                expose.scope = default_scope();
            }
            if exposes.insert(canonical.clone(), expose).is_some() {
                push(
                    &mut normalization_violations,
                    format!("exposes[{canonical}]"),
                    "expose keys collide after canonical './path' normalization",
                );
            }
        }
        self.exposes = exposes;
        if !normalization_violations.is_empty() {
            return Err(ValidationErrors::new(normalization_violations));
        }
        self.validate()?;
        Ok(self)
    }

    /// Validate normalized configuration without reading files or resolving packages.
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut violations = Vec::new();
        let has_configuration = self.enabled
            || !self.name.as_str().is_empty()
            || !self.remotes.is_empty()
            || !self.exposes.is_empty()
            || !self.shared.is_empty();
        if has_configuration && !self.enabled {
            push(
                &mut violations,
                "enabled",
                "federation configuration requires enabled=true",
            );
        }
        if self.enabled && !is_valid_container_name(self.name.as_str()) {
            push(
                &mut violations,
                "name",
                "container names must be 1-64 ASCII letters, digits, '_' or '-'",
            );
        }

        for (name, remote) in &self.remotes {
            let base = format!("remotes[{name}]");
            if !is_valid_container_name(name.as_str()) {
                push(
                    &mut violations,
                    base.clone(),
                    "remote names must use the container-name grammar",
                );
            }
            if self.enabled && name == &self.name {
                push(
                    &mut violations,
                    base.clone(),
                    "a remote name must differ from the local page container name",
                );
            }
            if !is_http_url(&remote.manifest_url) {
                push(
                    &mut violations,
                    format!("{base}.manifestUrl"),
                    "manifestUrl must be an absolute HTTP(S) URL",
                );
            }
            for (index, origin) in remote.allowed_origins.iter().enumerate() {
                if !is_http_origin(origin) {
                    push(
                        &mut violations,
                        format!("{base}.allowedOrigins[{index}]"),
                        "allowed origins must be HTTP(S) origins without a path",
                    );
                }
            }
        }

        for (key, expose) in &self.exposes {
            let base = format!("exposes[{key}]");
            if !is_valid_config_expose_key(key.as_str()) {
                push(
                    &mut violations,
                    base.clone(),
                    "expose keys must use the canonical './path' form",
                );
            }
            if !is_valid_federation_entry(&expose.entry) {
                push(
                    &mut violations,
                    format!("{base}.entry"),
                    "entry must be a trimmed project-relative path without traversal, query or fragment",
                );
            }
            validate_scope(
                &mut violations,
                ErrorCode::ConfigInvalid,
                &format!("{base}.scope"),
                &expose.scope,
            );
            validate_render_boundary(
                &mut violations,
                ErrorCode::ConfigInvalid,
                &format!("{base}.shadow"),
                expose.mode,
                expose.shadow,
            );
            if expose.mode == ExposeMode::Isolated && expose.scope == "default" {
                push(
                    &mut violations,
                    format!("{base}.scope"),
                    "isolated exposes require a non-default share scope",
                );
            }
            if expose.allow_global_css && expose.mode != ExposeMode::HostRendered {
                push(
                    &mut violations,
                    format!("{base}.allowGlobalCss"),
                    "allowGlobalCss is only valid for host-rendered exposes",
                );
            }
        }

        for (package, shared) in &self.shared {
            let base = format!("shared[{package}]");
            if !is_valid_bare_specifier(package) {
                push(
                    &mut violations,
                    base.clone(),
                    "shared keys must be valid bare package specifiers",
                );
            }
            validate_scope(
                &mut violations,
                ErrorCode::ConfigInvalid,
                &format!("{base}.scope"),
                &shared.scope,
            );
            if let Some(version) = &shared.required_version
                && (!is_non_empty_token(version, 128) || version.trim() != version)
            {
                push(
                    &mut violations,
                    format!("{base}.requiredVersion"),
                    "requiredVersion must be a non-empty range",
                );
            }
            if shared.strict && shared.required_version.is_none() {
                push(
                    &mut violations,
                    format!("{base}.requiredVersion"),
                    "strict shared dependencies require requiredVersion",
                );
            }
            if let Some(group) = &shared.coherence_group
                && !is_valid_coherence_group(group)
            {
                push(
                    &mut violations,
                    format!("{base}.coherenceGroup"),
                    "coherenceGroup must be a non-empty stable token",
                );
            }
            if shared.coherence_group.is_some() && !shared.singleton {
                push(
                    &mut violations,
                    format!("{base}.singleton"),
                    "coherenceGroup requires singleton=true",
                );
            }
            if let Some(owner) = &shared.owner {
                if !is_valid_container_name(owner.as_str()) {
                    push(
                        &mut violations,
                        format!("{base}.owner"),
                        "owner must be a valid container name",
                    );
                }
                if !shared.singleton {
                    push(
                        &mut violations,
                        format!("{base}.singleton"),
                        "owner requires singleton=true",
                    );
                }
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
            .iter()
            .map(|(share_key, shared)| ReactSharePolicyView {
                share_key,
                scope: &shared.scope,
                singleton: shared.singleton,
                coherence_group: shared.coherence_group.as_deref(),
                owner: shared.owner.as_ref(),
                policy_path: format!("shared[{share_key}]"),
            })
            .collect::<Vec<_>>();
        for (scope, expose_path) in host_rendered_scopes {
            validate_host_rendered_react_scope(
                &mut violations,
                ErrorCode::ConfigInvalid,
                &expose_path,
                scope,
                &react_policies,
            );
        }

        finish_validation(violations)
    }
}

pub(crate) fn validate_host_rendered_react_scope(
    violations: &mut Vec<ContractViolation>,
    code: ErrorCode,
    expose_path: &str,
    scope: &str,
    policies: &[ReactSharePolicyView<'_>],
) {
    let matching = policies
        .iter()
        .filter(|policy| {
            policy.scope == scope && REACT_COHERENCE_MEMBERS.contains(&policy.share_key)
        })
        .collect::<Vec<_>>();

    for share_key in REACT_COHERENCE_MEMBERS {
        if !matching.iter().any(|policy| policy.share_key == share_key) {
            violations.push(ContractViolation::new(
                code,
                expose_path,
                format!(
                    "host-rendered scope '{scope}' must declare shared dependency '{share_key}'"
                ),
            ));
        }
    }

    for policy in &matching {
        if !policy.singleton {
            violations.push(ContractViolation::new(
                code,
                format!("{}.singleton", policy.policy_path),
                format!(
                    "host-rendered React dependency '{}' requires singleton=true in scope '{scope}'",
                    policy.share_key
                ),
            ));
        }
        if policy.coherence_group.is_none() {
            violations.push(ContractViolation::new(
                code,
                format!("{}.coherenceGroup", policy.policy_path),
                format!(
                    "host-rendered React dependency '{}' requires a non-empty coherenceGroup",
                    policy.share_key
                ),
            ));
        }
    }

    let coherence_groups = matching
        .iter()
        .filter_map(|policy| policy.coherence_group)
        .filter(|group| is_valid_coherence_group(group))
        .collect::<BTreeSet<_>>();
    if coherence_groups.len() > 1 {
        for policy in &matching {
            if policy.coherence_group.is_some_and(is_valid_coherence_group) {
                violations.push(ContractViolation::new(
                    code,
                    format!("{}.coherenceGroup", policy.policy_path),
                    format!(
                        "all host-rendered React dependencies in scope '{scope}' must use the same coherenceGroup"
                    ),
                ));
            }
        }
    }

    let owners = matching
        .iter()
        .map(|policy| policy.owner.map(ContainerName::as_str))
        .collect::<BTreeSet<_>>();
    if owners.len() > 1 {
        for policy in matching {
            violations.push(ContractViolation::new(
                code,
                format!("{}.owner", policy.policy_path),
                format!(
                    "all host-rendered React dependencies in scope '{scope}' must omit owner together or use the same owner"
                ),
            ));
        }
    }
}

pub(crate) fn validate_render_boundary(
    violations: &mut Vec<ContractViolation>,
    code: ErrorCode,
    path: &str,
    mode: ExposeMode,
    shadow: ShadowMode,
) {
    let valid = match mode {
        ExposeMode::Generic => shadow == ShadowMode::None,
        ExposeMode::HostRendered => shadow == ShadowMode::None,
        ExposeMode::Isolated => shadow == ShadowMode::Open,
    };
    if !valid {
        let message = match mode {
            ExposeMode::HostRendered => "host-rendered exposes cannot create a shadow root",
            ExposeMode::Isolated => "isolated exposes require an open shadow root",
            ExposeMode::Generic => "generic exposes cannot create a shadow root",
        };
        violations.push(ContractViolation::new(code, path, message));
    }
}

pub(crate) fn validate_scope(
    violations: &mut Vec<ContractViolation>,
    code: ErrorCode,
    path: &str,
    scope: &str,
) {
    if !is_valid_scope(scope) {
        violations.push(ContractViolation::new(
            code,
            path,
            "share scopes must be non-empty stable tokens",
        ));
    }
}

fn push(violations: &mut Vec<ContractViolation>, path: impl Into<String>, message: &'static str) {
    violations.push(ContractViolation::new(
        ErrorCode::ConfigInvalid,
        path,
        message,
    ));
}

fn is_http_url(value: &str) -> bool {
    normalize_http_url(value, false).is_ok()
}

fn is_http_origin(value: &str) -> bool {
    normalize_http_url(value, true).is_ok()
}

fn normalize_http_url(value: &str, origin_only: bool) -> Result<String, ()> {
    if value.trim() != value || value.is_empty() {
        return Err(());
    }
    let (scheme, rest) = value.split_once("://").ok_or(())?;
    if !matches!(scheme, "http" | "https")
        || rest.contains('\\')
        || rest.contains('#')
        || rest.chars().any(char::is_whitespace)
    {
        return Err(());
    }
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let suffix = &rest[authority_end..];
    if authority.is_empty() || authority.contains('@') || !is_valid_url_authority(authority) {
        return Err(());
    }
    if origin_only && !matches!(suffix, "" | "/") {
        return Err(());
    }
    if origin_only {
        Ok(format!("{scheme}://{authority}"))
    } else {
        Ok(value.to_owned())
    }
}

fn is_valid_url_authority(authority: &str) -> bool {
    if let Some(ipv6) = authority.strip_prefix('[') {
        let Some(close) = ipv6.find(']') else {
            return false;
        };
        let host = &ipv6[..close];
        let tail = &ipv6[close + 1..];
        return !host.is_empty()
            && host
                .chars()
                .all(|character| character.is_ascii_hexdigit() || matches!(character, ':' | '.'))
            && is_valid_url_port(tail);
    }
    if authority.contains(['[', ']']) || authority.matches(':').count() > 1 {
        return false;
    }
    let (host, port) = authority
        .rsplit_once(':')
        .map_or((authority, None), |(host, port)| (host, Some(port)));
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
        && port.is_none_or(|port| is_valid_url_port(&format!(":{port}")))
}

fn is_valid_url_port(value: &str) -> bool {
    value.is_empty()
        || value
            .strip_prefix(':')
            .filter(|port| {
                !port.is_empty() && port.chars().all(|character| character.is_ascii_digit())
            })
            .and_then(|port| port.parse::<u16>().ok())
            .is_some_and(|port| port > 0)
}

fn is_valid_federation_entry(value: &str) -> bool {
    let path = value.strip_prefix("./").unwrap_or(value);
    let looks_absolute = value.starts_with(['/', '\\'])
        || value
            .as_bytes()
            .get(1)
            .is_some_and(|character| *character == b':');
    value.trim() == value
        && !looks_absolute
        && !path.is_empty()
        && path.len() <= 1024
        && !path.contains(['\\', '?', '#', '\0'])
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && !matches!(segment, "." | ".."))
}

pub(crate) fn is_valid_bare_specifier(value: &str) -> bool {
    if value.starts_with(['.', '/']) || !is_valid_path_specifier(value) {
        return false;
    }
    let mut segments = value.split('/');
    let first = segments.next().unwrap_or_default();
    if first.starts_with('@') {
        first.len() > 1 && segments.next().is_some()
    } else {
        !first.contains('@')
    }
}

fn is_valid_path_specifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.contains(['\\', '?', '#'])
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '/' | '@' | '_' | '-' | '.')
        })
        && value
            .split('/')
            .all(|segment| !segment.is_empty() && !matches!(segment, "." | ".."))
}

pub(crate) fn is_valid_scope(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '@' | '/')
        })
        && value
            .split('/')
            .all(|segment| !segment.is_empty() && !matches!(segment, "." | ".."))
}

pub(crate) fn is_valid_coherence_group(value: &str) -> bool {
    let mut characters = value.chars();
    value.len() <= 128
        && characters
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_rendered_config() -> FederationConfig {
        let mut config = FederationConfig {
            enabled: true,
            name: "shell".into(),
            ..FederationConfig::default()
        };
        config.exposes.insert(
            "./Button".into(),
            ExposeConfig {
                entry: "src/button.tsx".to_owned(),
                mode: ExposeMode::HostRendered,
                scope: "react18".to_owned(),
                shadow: ShadowMode::None,
                allow_global_css: false,
            },
        );
        for share_key in REACT_COHERENCE_MEMBERS {
            config.shared.insert(
                share_key.to_owned(),
                SharedConfig {
                    scope: "react18".to_owned(),
                    singleton: true,
                    coherence_group: Some("react18".to_owned()),
                    owner: Some("shell".into()),
                    ..SharedConfig::default()
                },
            );
        }
        config
    }

    #[test]
    fn config_accepts_toml_aliases_and_serializes_camel_case() {
        let remote: RemoteConfig = serde_json::from_value(serde_json::json!({
            "manifest_url": "https://example.test/wake-federation.json",
            "allowed_origins": ["https://example.test"],
            "dev_follow": false
        }))
        .unwrap();
        assert!(!remote.dev_follow);
        let value = serde_json::to_value(remote).unwrap();
        assert!(value.get("manifestUrl").is_some());
        assert!(value.get("manifest_url").is_none());

        let expose: ExposeConfig = serde_json::from_value(serde_json::json!({
            "entry": "src/button.tsx",
            "mode": "host-rendered",
            "scope": "react18",
            "allow_global_css": true
        }))
        .unwrap();
        assert!(expose.allow_global_css);
        let value = serde_json::to_value(expose).unwrap();
        assert_eq!(value["allowGlobalCss"], true);
        assert!(value.get("allow_global_css").is_none());
    }

    #[test]
    fn global_css_opt_in_is_only_valid_for_host_rendered_exposes() {
        for mode in [ExposeMode::Generic, ExposeMode::Isolated] {
            let mut config = FederationConfig {
                enabled: true,
                name: "shell".into(),
                ..FederationConfig::default()
            };
            config.exposes.insert(
                "./Styles".into(),
                ExposeConfig {
                    entry: "src/styles.ts".to_owned(),
                    mode,
                    scope: if mode == ExposeMode::Isolated {
                        "isolated".to_owned()
                    } else {
                        "default".to_owned()
                    },
                    shadow: if mode == ExposeMode::Isolated {
                        ShadowMode::Open
                    } else {
                        ShadowMode::None
                    },
                    allow_global_css: true,
                },
            );
            let error = config.validate().unwrap_err();
            assert!(error.violations.iter().any(|violation| {
                violation.path == "exposes[./Styles].allowGlobalCss"
                    && violation.code == ErrorCode::ConfigInvalid
            }));
        }
    }

    #[test]
    fn react_render_boundaries_fail_closed() {
        let mut config = FederationConfig {
            enabled: true,
            name: "shell".into(),
            ..FederationConfig::default()
        };
        config.exposes.insert(
            "./Legacy".into(),
            ExposeConfig {
                entry: "src/legacy.tsx".to_owned(),
                mode: ExposeMode::Isolated,
                shadow: ShadowMode::None,
                ..ExposeConfig::default()
            },
        );
        let error = config.validate().unwrap_err();
        assert!(error.violations.iter().any(|violation| {
            violation.code == ErrorCode::ConfigInvalid
                && violation.path == "exposes[./Legacy].shadow"
        }));
    }

    #[test]
    fn normalization_canonicalizes_exposes_and_origins() {
        let mut config = FederationConfig {
            enabled: true,
            name: "shell".into(),
            ..FederationConfig::default()
        };
        config.remotes.insert(
            "catalog".into(),
            RemoteConfig {
                manifest_url: "https://catalog.test/wake-federation.json".to_owned(),
                allowed_origins: vec![
                    "https://catalog.test/".to_owned(),
                    "https://catalog.test".to_owned(),
                ],
                dev_follow: true,
            },
        );
        config.exposes.insert(
            "Button".into(),
            ExposeConfig {
                entry: "src/button.tsx".to_owned(),
                ..ExposeConfig::default()
            },
        );
        let config = config.validate_and_normalize().unwrap();
        assert!(config.exposes.contains_key(&ExposeKey::from("./Button")));
        assert_eq!(
            config.remotes[&ContainerName::from("catalog")].allowed_origins,
            ["https://catalog.test".to_owned()]
        );
    }

    #[test]
    fn a_remote_cannot_reuse_the_local_container_name() {
        let mut config = FederationConfig {
            enabled: true,
            name: "shell".into(),
            ..FederationConfig::default()
        };
        config.remotes.insert(
            "shell".into(),
            RemoteConfig {
                manifest_url: "https://shell.test/wake-federation.json".to_owned(),
                ..RemoteConfig::default()
            },
        );

        let error = config.validate().unwrap_err();
        assert!(error.violations.iter().any(|violation| {
            violation.code == ErrorCode::ConfigInvalid && violation.path == "remotes[shell]"
        }));
    }

    #[test]
    fn strict_config_grammar_rejects_ambiguous_inputs() {
        let invalid_remotes = [
            "https://user@catalog.test/manifest.json",
            "https:///manifest.json",
            "https://catalog.test/manifest.json#fragment",
            " https://catalog.test/manifest.json",
        ];
        for manifest_url in invalid_remotes {
            let mut config = FederationConfig {
                enabled: true,
                name: "shell".into(),
                ..FederationConfig::default()
            };
            config.remotes.insert(
                "catalog".into(),
                RemoteConfig {
                    manifest_url: manifest_url.to_owned(),
                    ..RemoteConfig::default()
                },
            );
            assert!(config.validate().is_err(), "accepted {manifest_url}");
        }

        let mut config = FederationConfig {
            enabled: true,
            name: "shell".into(),
            ..FederationConfig::default()
        };
        config.exposes.insert(
            "./Button".into(),
            ExposeConfig {
                entry: "../button.tsx".to_owned(),
                shadow: ShadowMode::Open,
                ..ExposeConfig::default()
            },
        );
        config
            .shared
            .insert("./react".to_owned(), SharedConfig::default());
        let error = config.validate().unwrap_err();
        assert!(error.violations.iter().any(|violation| {
            violation.path.ends_with(".entry") && violation.code == ErrorCode::ConfigInvalid
        }));
        assert!(error.violations.iter().any(|violation| {
            violation.path.ends_with(".shadow") && violation.message.contains("generic")
        }));
        assert!(
            error
                .violations
                .iter()
                .any(|violation| violation.path == "shared[./react]")
        );
    }

    #[test]
    fn strict_and_coherence_require_complete_shared_policy() {
        let mut config = FederationConfig {
            enabled: true,
            name: "shell".into(),
            ..FederationConfig::default()
        };
        config.shared.insert(
            "react".to_owned(),
            SharedConfig {
                strict: true,
                coherence_group: Some("react18".to_owned()),
                ..SharedConfig::default()
            },
        );
        let error = config.validate().unwrap_err();
        assert!(
            error
                .violations
                .iter()
                .any(|violation| violation.path.ends_with("requiredVersion"))
        );
        assert!(
            error
                .violations
                .iter()
                .any(|violation| violation.path.ends_with("singleton"))
        );
    }

    #[test]
    fn host_rendered_react_scope_requires_all_five_members() {
        let mut config = host_rendered_config();
        config.shared.remove("react-dom/client");

        let error = config.validate().unwrap_err();
        assert!(error.violations.iter().any(|violation| {
            violation.code == ErrorCode::ConfigInvalid
                && violation.path == "exposes[./Button].scope"
                && violation.message.contains("react-dom/client")
        }));
    }

    #[test]
    fn host_rendered_react_scope_requires_one_singleton_group_and_owner() {
        let mut config = host_rendered_config();
        config.shared.get_mut("react").unwrap().singleton = false;
        config
            .shared
            .get_mut("react/jsx-runtime")
            .unwrap()
            .coherence_group = Some("other-react".to_owned());
        config
            .shared
            .get_mut("react/jsx-dev-runtime")
            .unwrap()
            .owner = Some("catalog".into());
        config.shared.get_mut("react-dom").unwrap().coherence_group = None;

        let error = config.validate().unwrap_err();
        for expected_path in [
            "shared[react].singleton",
            "shared[react/jsx-runtime].coherenceGroup",
            "shared[react/jsx-dev-runtime].owner",
            "shared[react-dom].coherenceGroup",
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
    fn generic_and_isolated_exposes_do_not_require_a_react_coherence_group() {
        let mut config = FederationConfig {
            enabled: true,
            name: "shell".into(),
            ..FederationConfig::default()
        };
        config.exposes.insert(
            "./Data".into(),
            ExposeConfig {
                entry: "src/data.ts".to_owned(),
                ..ExposeConfig::default()
            },
        );
        config.exposes.insert(
            "./Legacy".into(),
            ExposeConfig {
                entry: "src/legacy.tsx".to_owned(),
                mode: ExposeMode::Isolated,
                scope: "react17".to_owned(),
                shadow: ShadowMode::Open,
                allow_global_css: false,
            },
        );
        config.shared.insert(
            "react".to_owned(),
            SharedConfig {
                scope: "react17".to_owned(),
                ..SharedConfig::default()
            },
        );

        config.validate().unwrap();
    }
}
