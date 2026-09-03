use std::fmt;

use serde::{Deserialize, Serialize};

/// Stable machine-readable failures shared by build-time and browser consumers.
///
/// Variant names are intentionally decoupled from their serialized spellings. The
/// `FED_*` values form part of the federation v1 public contract and must not be
/// repurposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ErrorCode {
    #[serde(rename = "FED_CONFIG_INVALID")]
    ConfigInvalid,
    #[serde(rename = "FED_INVALID_SPECIFIER")]
    InvalidSpecifier,
    #[serde(rename = "FED_UNKNOWN_REMOTE")]
    UnknownRemote,
    #[serde(rename = "FED_MANIFEST_FETCH")]
    ManifestFetch,
    #[serde(rename = "FED_MANIFEST_SCHEMA")]
    ManifestSchema,
    #[serde(rename = "FED_RUNTIME_ABI")]
    RuntimeAbi,
    #[serde(rename = "FED_ORIGIN_DENIED")]
    OriginDenied,
    #[serde(rename = "FED_MANIFEST_INTEGRITY")]
    ManifestIntegrity,
    #[serde(rename = "FED_ASSET_INTEGRITY")]
    AssetIntegrity,
    #[serde(rename = "FED_ASSET_MIME")]
    AssetMime,
    #[serde(rename = "FED_ASSET_SIZE")]
    AssetSize,
    #[serde(rename = "FED_UNKNOWN_EXPOSE")]
    UnknownExpose,
    #[serde(rename = "FED_CONTAINER_INIT")]
    ContainerInit,
    #[serde(rename = "FED_CONTAINER_GET")]
    ContainerGet,
    #[serde(rename = "FED_SHARE_UNSATISFIABLE")]
    ShareUnsatisfiable,
    #[serde(rename = "FED_SHARE_SINGLETON_CONFLICT")]
    ShareSingletonConflict,
    #[serde(rename = "FED_COHERENCE_CONFLICT")]
    CoherenceConflict,
    #[serde(rename = "FED_TYPE_BUILD_MISMATCH")]
    TypeBuildMismatch,
    #[serde(rename = "FED_TIMEOUT")]
    Timeout,
    #[serde(rename = "FED_NETWORK")]
    Network,
    #[serde(rename = "FED_STATIC_REMOTE_UNSUPPORTED")]
    StaticRemoteUnsupported,
    #[serde(rename = "FED_REMOTE_CYCLE")]
    RemoteCycle,
    #[serde(rename = "FED_REMOTE_CONFLICT")]
    RemoteConflict,
    #[serde(rename = "FED_UNSUPPORTED_ENVIRONMENT")]
    UnsupportedEnvironment,
    #[serde(rename = "FED_CONTAINER_REGISTRATION")]
    ContainerRegistration,
    #[serde(rename = "FED_BRIDGE_LIFECYCLE")]
    BridgeLifecycle,
    #[serde(rename = "FED_BRIDGE_PROPS")]
    BridgeProps,
    #[serde(rename = "FED_STYLE_LOAD")]
    StyleLoad,
    #[serde(rename = "FED_LOCK_REQUIRED")]
    LockRequired,
    #[serde(rename = "FED_LOCK_INVALID")]
    LockInvalid,
    #[serde(rename = "FED_LOCK_MISMATCH")]
    LockMismatch,
    #[serde(rename = "FED_TYPES_INVALID")]
    TypesInvalid,
}

impl ErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConfigInvalid => "FED_CONFIG_INVALID",
            Self::InvalidSpecifier => "FED_INVALID_SPECIFIER",
            Self::UnknownRemote => "FED_UNKNOWN_REMOTE",
            Self::ManifestFetch => "FED_MANIFEST_FETCH",
            Self::ManifestSchema => "FED_MANIFEST_SCHEMA",
            Self::RuntimeAbi => "FED_RUNTIME_ABI",
            Self::OriginDenied => "FED_ORIGIN_DENIED",
            Self::ManifestIntegrity => "FED_MANIFEST_INTEGRITY",
            Self::AssetIntegrity => "FED_ASSET_INTEGRITY",
            Self::AssetMime => "FED_ASSET_MIME",
            Self::AssetSize => "FED_ASSET_SIZE",
            Self::UnknownExpose => "FED_UNKNOWN_EXPOSE",
            Self::ContainerInit => "FED_CONTAINER_INIT",
            Self::ContainerGet => "FED_CONTAINER_GET",
            Self::ShareUnsatisfiable => "FED_SHARE_UNSATISFIABLE",
            Self::ShareSingletonConflict => "FED_SHARE_SINGLETON_CONFLICT",
            Self::CoherenceConflict => "FED_COHERENCE_CONFLICT",
            Self::TypeBuildMismatch => "FED_TYPE_BUILD_MISMATCH",
            Self::Timeout => "FED_TIMEOUT",
            Self::Network => "FED_NETWORK",
            Self::StaticRemoteUnsupported => "FED_STATIC_REMOTE_UNSUPPORTED",
            Self::RemoteCycle => "FED_REMOTE_CYCLE",
            Self::RemoteConflict => "FED_REMOTE_CONFLICT",
            Self::UnsupportedEnvironment => "FED_UNSUPPORTED_ENVIRONMENT",
            Self::ContainerRegistration => "FED_CONTAINER_REGISTRATION",
            Self::BridgeLifecycle => "FED_BRIDGE_LIFECYCLE",
            Self::BridgeProps => "FED_BRIDGE_PROPS",
            Self::StyleLoad => "FED_STYLE_LOAD",
            Self::LockRequired => "FED_LOCK_REQUIRED",
            Self::LockInvalid => "FED_LOCK_INVALID",
            Self::LockMismatch => "FED_LOCK_MISMATCH",
            Self::TypesInvalid => "FED_TYPES_INVALID",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One deterministic contract validation failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContractViolation {
    pub code: ErrorCode,
    pub path: String,
    pub message: String,
}

impl ContractViolation {
    pub(crate) fn new(
        code: ErrorCode,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Ordered validation failures. Validation never depends on hash-map iteration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidationErrors {
    pub violations: Vec<ContractViolation>,
}

impl ValidationErrors {
    #[must_use]
    pub fn new(mut violations: Vec<ContractViolation>) -> Self {
        violations.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.code.cmp(&right.code))
                .then_with(|| left.message.cmp(&right.message))
        });
        violations.dedup();
        Self { violations }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.violations.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.violations.len()
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.violations.is_empty() {
            return formatter.write_str("federation contract validation failed");
        }
        write!(
            formatter,
            "federation contract validation failed with {} violation(s): ",
            self.violations.len()
        )?;
        for (index, violation) in self.violations.iter().enumerate() {
            if index != 0 {
                formatter.write_str("; ")?;
            }
            write!(
                formatter,
                "{} at {}: {}",
                violation.code, violation.path, violation.message
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

pub(crate) fn finish_validation(
    violations: Vec<ContractViolation>,
) -> Result<(), ValidationErrors> {
    if violations.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::new(violations))
    }
}
