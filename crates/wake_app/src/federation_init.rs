//! Shared product service for initializing Federation editor declarations.
//!
//! Frontends only parse arguments and present the result. The ownership and
//! no-clobber publication rules live here so the Rust and npm CLIs cannot drift.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{WakeError, federation_project_root};

const FEDERATION_DECLARATION: &str =
    "/// <reference path=\"./.wake/federation/types/index.d.ts\" />\n";
const FEDERATION_TYPES_INDEX: &str =
    "// Managed by `wake dev`; remote federation declarations are synchronized here.\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FederationInitFileStatus {
    Created,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FederationInitResult {
    pub project_root: PathBuf,
    pub declaration_path: PathBuf,
    pub types_index_path: PathBuf,
    pub declaration: FederationInitFileStatus,
    pub types_index: FederationInitFileStatus,
}

/// Create the stable declaration entry used by Federation type synchronization.
///
/// Every visible target is inspected before any filesystem mutation. Wake never
/// overwrites a user-owned file, and each newly created file is published with a
/// same-directory no-clobber rename.
pub fn initialize_federation_types(start: &Path) -> Result<FederationInitResult, WakeError> {
    let start = absolute_existing_directory(start)?;
    let project_root = federation_project_root(&start).map_err(map_project_error)?;
    let declaration_path = project_root.join("wake-federation.d.ts");
    let types_index_path = project_root.join(".wake/federation/types/index.d.ts");

    let declaration = inspect_target(&declaration_path, FEDERATION_DECLARATION.as_bytes())?;
    let types_index = inspect_target(&types_index_path, FEDERATION_TYPES_INDEX.as_bytes())?;

    let types_directory = types_index_path
        .parent()
        .expect("the Federation types index always has a parent");
    std::fs::create_dir_all(types_directory)
        .map_err(|error| init_io("create Federation types directory", types_directory, &error))?;

    // The referenced hidden index is installed first. The public root declaration
    // is the commit marker, so a late failure cannot expose a dangling reference.
    let types_index = publish_if_missing(
        &types_index_path,
        FEDERATION_TYPES_INDEX.as_bytes(),
        types_index,
    )?;
    let declaration = publish_if_missing(
        &declaration_path,
        FEDERATION_DECLARATION.as_bytes(),
        declaration,
    )?;

    Ok(FederationInitResult {
        project_root,
        declaration_path,
        types_index_path,
        declaration,
        types_index,
    })
}

fn absolute_existing_directory(path: &Path) -> Result<PathBuf, WakeError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| init_io("resolve the current directory", Path::new("."), &error))?
            .join(path)
    };
    let metadata = std::fs::metadata(&absolute)
        .map_err(|error| init_io("inspect project directory", &absolute, &error))?;
    if !metadata.is_dir() {
        return Err(WakeError::new(
            "WAKE_FED_INIT_CONFIG",
            format!("project path `{}` is not a directory", absolute.display()),
        )
        .at(&absolute));
    }
    absolute
        .canonicalize()
        .map_err(|error| init_io("resolve project directory", &absolute, &error))
}

fn map_project_error(error: WakeError) -> WakeError {
    let code = if error.code == "FED_CONFIG_INVALID" {
        "WAKE_FED_INIT_CONFIG"
    } else {
        "WAKE_FED_INIT_IO"
    };
    WakeError {
        code: code.to_owned(),
        message: error.message,
        path: error.path,
        diagnostics: error.diagnostics,
    }
}

fn init_io(operation: &str, path: &Path, error: &std::io::Error) -> WakeError {
    WakeError::new(
        "WAKE_FED_INIT_IO",
        format!("failed to {operation} `{}`: {error}", path.display()),
    )
    .at(path)
}

fn inspect_target(path: &Path, expected: &[u8]) -> Result<FederationInitFileStatus, WakeError> {
    match std::fs::read(path) {
        Ok(existing) if existing == expected => Ok(FederationInitFileStatus::Unchanged),
        Ok(_) => Err(WakeError::new(
            "WAKE_FED_INIT_CONFLICT",
            format!(
                "refusing to overwrite `{}` because its content is not Wake's Federation initializer output",
                path.display()
            ),
        )
        .at(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(FederationInitFileStatus::Created)
        }
        Err(error) => Err(init_io("read", path, &error)),
    }
}

fn publish_if_missing(
    path: &Path,
    contents: &[u8],
    status: FederationInitFileStatus,
) -> Result<FederationInitFileStatus, WakeError> {
    if status == FederationInitFileStatus::Unchanged {
        return Ok(status);
    }
    let parent = path
        .parent()
        .expect("Federation initializer output always has a parent");
    let mut temporary = tempfile::Builder::new()
        .prefix(".wake-federation-")
        .tempfile_in(parent)
        .map_err(|error| init_io("create temporary file beside", path, &error))?;
    temporary
        .write_all(contents)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| init_io("write", path, &error))?;

    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(FederationInitFileStatus::Created),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            inspect_target(path, contents)
        }
        Err(error) => Err(init_io("atomically publish", path, &error.error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("wake.config.toml"),
            "[federation]\nenabled = true\nname = 'shell'\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn initialization_is_reusable_and_idempotent() {
        let root = project();
        let nested = root.path().join("packages/catalog/src");
        std::fs::create_dir_all(&nested).unwrap();

        let first = initialize_federation_types(&nested).unwrap();
        assert_eq!(first.declaration, FederationInitFileStatus::Created);
        assert_eq!(first.types_index, FederationInitFileStatus::Created);
        assert_eq!(
            std::fs::read_to_string(&first.declaration_path).unwrap(),
            FEDERATION_DECLARATION
        );
        assert_eq!(
            std::fs::read_to_string(&first.types_index_path).unwrap(),
            FEDERATION_TYPES_INDEX
        );

        let second = initialize_federation_types(root.path()).unwrap();
        assert_eq!(second.declaration, FederationInitFileStatus::Unchanged);
        assert_eq!(second.types_index, FederationInitFileStatus::Unchanged);
        assert_eq!(second.project_root, first.project_root);
        assert_eq!(
            serde_json::to_value(FederationInitFileStatus::Created).unwrap(),
            "created"
        );
        assert_eq!(
            serde_json::to_value(FederationInitFileStatus::Unchanged).unwrap(),
            "unchanged"
        );
    }

    #[test]
    fn every_target_is_checked_before_mutation() {
        let root = project();
        let types = root.path().join(".wake/federation/types");
        std::fs::create_dir_all(&types).unwrap();
        let index = types.join("index.d.ts");
        std::fs::write(&index, "declare module 'owned-by-user';\n").unwrap();

        let error = initialize_federation_types(root.path()).unwrap_err();
        assert_eq!(error.code, "WAKE_FED_INIT_CONFLICT");
        assert_eq!(
            std::fs::read_to_string(index).unwrap(),
            "declare module 'owned-by-user';\n"
        );
        assert!(!root.path().join("wake-federation.d.ts").exists());
    }

    #[test]
    fn disabled_projects_use_the_frontend_independent_init_error() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("wake.config.toml"), "[federation]\n").unwrap();

        let error = initialize_federation_types(root.path()).unwrap_err();
        assert_eq!(error.code, "WAKE_FED_INIT_CONFIG");
        assert!(!root.path().join(".wake").exists());
    }
}
