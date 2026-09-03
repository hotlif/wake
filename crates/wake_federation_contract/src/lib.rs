//! Stable, versioned data contracts for Wake Federation.
//!
//! This crate owns manifest/config DTOs, stable identities, error codes and dev
//! update messages. It deliberately owns no filesystem, network, resolver, AST,
//! Node, browser or runtime execution behavior.

#![forbid(unsafe_code)]

mod config;
mod dev;
mod error;
mod identity;
mod manifest;

pub use config::{
    ExposeConfig, ExposeMode, FederationConfig, FederationOptions, RemoteConfig, ShadowMode,
    SharedConfig,
};
pub use dev::{DevLeaseMessage, DevLeaseReloadReason, DevUpdate, DevUpdateAction};
pub use error::{ContractViolation, ErrorCode, ValidationErrors};
pub use identity::{BuildId, ContainerIdentity, ContainerName, ExposeKey, ModuleIdentity};
pub use manifest::{
    Asset, AssetKind, DevelopmentMetadata, ExposedModule, FederationLock, FederationManifest,
    Manifest, PackageKey, RemoteRef, SharedManifest, SharedOffer, SharedPolicy, SharedRequirement,
    TypeArtifact, TypeArtifactFormat,
};

pub const FEDERATION_SCHEMA_VERSION: &str = "wake.federation.manifest.v1";
pub const FEDERATION_RUNTIME_ABI: &str = "wake.federation.v1";
pub const FEDERATION_LOCK_SCHEMA_VERSION: &str = "wake.federation.lock.v1";
pub const FEDERATION_DEV_UPDATE_SCHEMA_VERSION: &str = "wake.federation.dev-update.v1";
pub const FEDERATION_DEV_LEASE_SCHEMA_VERSION: &str = "wake.federation.dev-lease.v1";
/// A single browser connection may conservatively retain this many build generations.
///
/// The bound prevents a malformed or stale page from turning snapshot retention into
/// unbounded server memory. Clients that exceed it must perform an explicit full reload.
pub const FEDERATION_DEV_MAX_BUILD_LEASES: usize = 8;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_error_codes_round_trip_as_public_strings() {
        let encoded = serde_json::to_string(&ErrorCode::ShareUnsatisfiable).unwrap();
        assert_eq!(encoded, "\"FED_SHARE_UNSATISFIABLE\"");
        assert_eq!(
            serde_json::from_str::<ErrorCode>(&encoded).unwrap(),
            ErrorCode::ShareUnsatisfiable
        );
        assert_eq!(ErrorCode::RuntimeAbi.as_str(), "FED_RUNTIME_ABI");
        for (code, expected) in [
            (ErrorCode::RemoteConflict, "FED_REMOTE_CONFLICT"),
            (
                ErrorCode::UnsupportedEnvironment,
                "FED_UNSUPPORTED_ENVIRONMENT",
            ),
            (
                ErrorCode::ContainerRegistration,
                "FED_CONTAINER_REGISTRATION",
            ),
            (ErrorCode::BridgeLifecycle, "FED_BRIDGE_LIFECYCLE"),
            (ErrorCode::BridgeProps, "FED_BRIDGE_PROPS"),
            (ErrorCode::StyleLoad, "FED_STYLE_LOAD"),
            (ErrorCode::LockRequired, "FED_LOCK_REQUIRED"),
            (ErrorCode::LockInvalid, "FED_LOCK_INVALID"),
            (ErrorCode::LockMismatch, "FED_LOCK_MISMATCH"),
            (ErrorCode::TypesInvalid, "FED_TYPES_INVALID"),
        ] {
            assert_eq!(code.as_str(), expected);
            assert_eq!(
                serde_json::from_str::<ErrorCode>(&format!("\"{expected}\"")).unwrap(),
                code
            );
        }
    }

    #[test]
    fn contract_has_no_platform_specific_identity_fields() {
        let identity = ModuleIdentity {
            container: "catalog".into(),
            build_id: "build-a".into(),
            expose: "./Button".into(),
            generation: 3,
        };
        assert_eq!(
            serde_json::to_string(&identity).unwrap(),
            r#"{"container":"catalog","buildId":"build-a","expose":"./Button","generation":3}"#
        );
    }
}
