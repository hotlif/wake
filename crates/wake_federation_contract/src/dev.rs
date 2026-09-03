use serde::{Deserialize, Serialize};

use crate::error::{ContractViolation, ErrorCode, ValidationErrors, finish_validation};
use crate::identity::{
    BuildId, ContainerName, ExposeKey, is_valid_container_name, is_valid_expose_key,
    is_valid_identity_token,
};
use crate::{
    FEDERATION_DEV_LEASE_SCHEMA_VERSION, FEDERATION_DEV_MAX_BUILD_LEASES,
    FEDERATION_DEV_UPDATE_SCHEMA_VERSION,
};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum DevUpdateAction {
    TypesOnly,
    IsolatedRemount,
    #[default]
    FullReload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DevLeaseReloadReason {
    BuildGone,
    InvalidLease,
    LeaseLimit,
    UpdateLagged,
}

/// Versioned browser-to-server snapshot lease protocol.
///
/// A `lease` frame replaces the complete build set owned by that WebSocket connection.
/// The server either applies it atomically and returns `lease-ack`, or leaves ownership
/// unchanged and directs only that connection to perform `full-reload`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DevLeaseMessage {
    Lease {
        schema_version: String,
        remote: ContainerName,
        build_ids: Vec<BuildId>,
    },
    LeaseAck {
        schema_version: String,
        remote: ContainerName,
        build_ids: Vec<BuildId>,
        current_build_id: BuildId,
        generation: u64,
    },
    FullReload {
        schema_version: String,
        remote: ContainerName,
        current_build_id: BuildId,
        generation: u64,
        expired_build_id: Option<BuildId>,
        reason: DevLeaseReloadReason,
    },
}

impl DevLeaseMessage {
    #[must_use]
    pub fn lease(remote: ContainerName, build_ids: Vec<BuildId>) -> Self {
        Self::Lease {
            schema_version: FEDERATION_DEV_LEASE_SCHEMA_VERSION.to_owned(),
            remote,
            build_ids,
        }
    }

    #[must_use]
    pub fn lease_ack(
        remote: ContainerName,
        build_ids: Vec<BuildId>,
        current_build_id: BuildId,
        generation: u64,
    ) -> Self {
        Self::LeaseAck {
            schema_version: FEDERATION_DEV_LEASE_SCHEMA_VERSION.to_owned(),
            remote,
            build_ids,
            current_build_id,
            generation,
        }
    }

    #[must_use]
    pub fn full_reload(
        remote: ContainerName,
        current_build_id: BuildId,
        generation: u64,
        expired_build_id: Option<BuildId>,
        reason: DevLeaseReloadReason,
    ) -> Self {
        Self::FullReload {
            schema_version: FEDERATION_DEV_LEASE_SCHEMA_VERSION.to_owned(),
            remote,
            current_build_id,
            generation,
            expired_build_id,
            reason,
        }
    }

    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let (schema_version, remote) = match self {
            Self::Lease {
                schema_version,
                remote,
                ..
            }
            | Self::LeaseAck {
                schema_version,
                remote,
                ..
            }
            | Self::FullReload {
                schema_version,
                remote,
                ..
            } => (schema_version, remote),
        };
        let mut violations = Vec::new();
        if schema_version != FEDERATION_DEV_LEASE_SCHEMA_VERSION {
            violations.push(ContractViolation::new(
                ErrorCode::ManifestSchema,
                "schemaVersion",
                format!("expected {FEDERATION_DEV_LEASE_SCHEMA_VERSION}"),
            ));
        }
        if !is_valid_container_name(remote.as_str()) {
            violations.push(ContractViolation::new(
                ErrorCode::ManifestSchema,
                "remote",
                "invalid remote container name",
            ));
        }

        match self {
            Self::Lease { build_ids, .. } | Self::LeaseAck { build_ids, .. } => {
                validate_canonical_build_ids(build_ids, &mut violations);
            }
            Self::FullReload {
                current_build_id,
                expired_build_id,
                ..
            } => {
                validate_build_id(current_build_id, "currentBuildId", &mut violations);
                if let Some(build_id) = expired_build_id {
                    validate_build_id(build_id, "expiredBuildId", &mut violations);
                }
            }
        }
        if let Self::LeaseAck {
            current_build_id, ..
        } = self
        {
            validate_build_id(current_build_id, "currentBuildId", &mut violations);
        }
        finish_validation(violations)
    }
}

fn validate_canonical_build_ids(build_ids: &[BuildId], violations: &mut Vec<ContractViolation>) {
    if build_ids.is_empty() {
        violations.push(ContractViolation::new(
            ErrorCode::ManifestSchema,
            "buildIds",
            "buildIds must contain at least one active build",
        ));
    }
    if build_ids.len() > FEDERATION_DEV_MAX_BUILD_LEASES {
        violations.push(ContractViolation::new(
            ErrorCode::ManifestSchema,
            "buildIds",
            format!("at most {FEDERATION_DEV_MAX_BUILD_LEASES} build leases are allowed"),
        ));
    }
    for (index, build_id) in build_ids.iter().enumerate() {
        validate_build_id(build_id, format!("buildIds[{index}]"), violations);
        if index > 0 && build_ids[index - 1] >= *build_id {
            violations.push(ContractViolation::new(
                ErrorCode::ManifestSchema,
                "buildIds",
                "buildIds must be sorted and unique",
            ));
            break;
        }
    }
}

fn validate_build_id(
    build_id: &BuildId,
    field: impl Into<String>,
    violations: &mut Vec<ContractViolation>,
) {
    if !build_id.is_valid() {
        violations.push(ContractViolation::new(
            ErrorCode::ManifestSchema,
            field,
            "build id must be a non-empty stable token",
        ));
    }
}

/// Versioned remote update broadcast owned by the federation dev coordinator.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DevUpdate {
    pub schema_version: String,
    pub remote: ContainerName,
    pub old_build_id: Option<BuildId>,
    pub new_build_id: BuildId,
    pub changed_exposes: Vec<ExposeKey>,
    pub types_hash: Option<String>,
    pub generation: u64,
    pub action: DevUpdateAction,
}

impl DevUpdate {
    #[must_use]
    pub fn new(
        remote: ContainerName,
        old_build_id: Option<BuildId>,
        new_build_id: BuildId,
        generation: u64,
        action: DevUpdateAction,
    ) -> Self {
        Self {
            schema_version: FEDERATION_DEV_UPDATE_SCHEMA_VERSION.to_owned(),
            remote,
            old_build_id,
            new_build_id,
            changed_exposes: Vec::new(),
            types_hash: None,
            generation,
            action,
        }
    }

    /// Sort and deduplicate set-like changed expose keys before transport.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.changed_exposes.sort();
        self.changed_exposes.dedup();
        self
    }

    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut violations = Vec::new();
        if self.schema_version != FEDERATION_DEV_UPDATE_SCHEMA_VERSION {
            violations.push(ContractViolation::new(
                ErrorCode::ManifestSchema,
                "schemaVersion",
                format!("expected {FEDERATION_DEV_UPDATE_SCHEMA_VERSION}"),
            ));
        }
        if !is_valid_container_name(self.remote.as_str()) {
            violations.push(ContractViolation::new(
                ErrorCode::ManifestSchema,
                "remote",
                "invalid remote container name",
            ));
        }
        if self
            .old_build_id
            .as_ref()
            .is_some_and(|build_id| !is_valid_identity_token(build_id.as_str(), 256))
        {
            violations.push(ContractViolation::new(
                ErrorCode::ManifestSchema,
                "oldBuildId",
                "oldBuildId must be a non-empty stable token when present",
            ));
        }
        if !is_valid_identity_token(self.new_build_id.as_str(), 256) {
            violations.push(ContractViolation::new(
                ErrorCode::ManifestSchema,
                "newBuildId",
                "newBuildId must be a non-empty stable token",
            ));
        }
        for (index, expose) in self.changed_exposes.iter().enumerate() {
            if !is_valid_expose_key(expose.as_str()) {
                violations.push(ContractViolation::new(
                    ErrorCode::ManifestSchema,
                    format!("changedExposes[{index}]"),
                    "changed expose keys must use canonical './path' form",
                ));
            }
        }
        if let Some(types_hash) = &self.types_hash
            && !is_valid_identity_token(types_hash, 256)
        {
            violations.push(ContractViolation::new(
                ErrorCode::ManifestSchema,
                "typesHash",
                "typesHash must be a non-empty stable token",
            ));
        }
        finish_validation(violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_normalization_is_stable() {
        let mut update = DevUpdate::new(
            "catalog".into(),
            Some("old".into()),
            "new".into(),
            2,
            DevUpdateAction::IsolatedRemount,
        );
        update.changed_exposes = vec!["./Z".into(), "./A".into(), "./Z".into()];
        let update = update.normalized();
        assert_eq!(
            update.changed_exposes,
            vec![ExposeKey::from("./A"), ExposeKey::from("./Z")]
        );
        update.validate().unwrap();
    }

    #[test]
    fn lease_messages_are_versioned_canonical_and_bounded() {
        let lease = DevLeaseMessage::lease(
            "catalog".into(),
            vec![BuildId::from("build-a"), BuildId::from("build-b")],
        );
        lease.validate().unwrap();
        assert_eq!(
            serde_json::to_string(&lease).unwrap(),
            r#"{"type":"lease","schemaVersion":"wake.federation.dev-lease.v1","remote":"catalog","buildIds":["build-a","build-b"]}"#
        );

        let duplicate = DevLeaseMessage::lease(
            "catalog".into(),
            vec![BuildId::from("build-a"), BuildId::from("build-a")],
        );
        assert!(duplicate.validate().is_err());
        let over_limit = DevLeaseMessage::lease(
            "catalog".into(),
            (0..=FEDERATION_DEV_MAX_BUILD_LEASES)
                .map(|index| BuildId::from(format!("build-{index:02}")))
                .collect(),
        );
        assert!(over_limit.validate().is_err());
    }

    #[test]
    fn full_reload_reason_is_a_closed_wire_enum() {
        let reload = DevLeaseMessage::full_reload(
            "catalog".into(),
            "current".into(),
            7,
            Some("expired".into()),
            DevLeaseReloadReason::BuildGone,
        );
        reload.validate().unwrap();
        assert_eq!(
            serde_json::to_string(&reload).unwrap(),
            r#"{"type":"full-reload","schemaVersion":"wake.federation.dev-lease.v1","remote":"catalog","currentBuildId":"current","generation":7,"expiredBuildId":"expired","reason":"build-gone"}"#
        );
    }
}
