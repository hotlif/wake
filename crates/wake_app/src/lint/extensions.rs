//! Explicitly authorized, one-shot JavaScript lint extensions. The native app owns the project
//! result and validates all worker output; the worker never writes Wake source files.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use wake_common::{Diagnostic, Severity, SourceFile, Span};
use wake_config::{LintRuleLevel, LintRuleSetting};

use super::{LintDiagnosticInfo, failure};
use crate::{CancellationToken, DiagnosticInfo, WakeError};

const MAX_WORKER_OUTPUT: usize = 2 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_DIAGNOSTICS: usize = 1000;
const WORKER_TIMEOUT: Duration = Duration::from_secs(10);

fn extension_error(message: impl Into<String>) -> WakeError {
    WakeError::new("WAKE_LINT_EXTENSION", message)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema: String,
    name: String,
    version: String,
    protocol: String,
    sdk_major: u32,
    rules: Vec<ManifestRule>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestRule {
    id: String,
    meta: RuleMeta,
    message_ids: Vec<String>,
    options_schema: OptionsSchema,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleMeta {
    description: String,
    category: String,
    fixable: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OptionsSchema {
    #[serde(rename = "type")]
    kind: String,
    additional_properties: bool,
    properties: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Package {
    name: String,
    manifest_text: String,
    #[serde(skip_serializing)]
    manifest: serde_json::Value,
    module_path: String,
    module_hash: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawDiagnostic {
    id: String,
    message: String,
    message_id: String,
    start: u64,
    end: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkerResponse {
    ok: bool,
    nonce: String,
    #[serde(default)]
    packages: Option<Vec<Package>>,
    #[serde(default)]
    diagnostics: Option<Vec<RawDiagnostic>>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct Catalog {
    root: PathBuf,
    packages: Vec<Package>,
    rules: BTreeMap<String, ManifestRule>,
}

impl Catalog {
    pub(super) fn load(
        root: &Path,
        names: &[String],
        cancellation: &CancellationToken,
    ) -> Result<Option<Self>, WakeError> {
        if names.is_empty() {
            return Ok(None);
        }
        if names.len() > 32 {
            return Err(extension_error("Too many lint extension packages"));
        }
        let mut unique = BTreeSet::new();
        for name in names {
            if !valid_package_name(name) || !unique.insert(name) {
                return Err(extension_error(format!(
                    "Invalid or duplicate lint extension package: {name}"
                )));
            }
        }
        let response = call_worker(
            root,
            &serde_json::json!({"mode":"manifest", "root":root, "packages":names}),
            cancellation,
        )?;
        let packages = response
            .packages
            .ok_or_else(|| extension_error("Lint extension host omitted package manifests"))?;
        if packages.len() != names.len() {
            return Err(extension_error(
                "Lint extension host returned a different package set",
            ));
        }
        let mut rules = BTreeMap::new();
        for (name, package) in names.iter().zip(&packages) {
            if &package.name != name || package.manifest_text.len() > 256 * 1024 {
                return Err(extension_error("Lint extension package identity changed"));
            }
            let manifest: Manifest =
                serde_json::from_str(&package.manifest_text).map_err(|error| {
                    extension_error(format!("Invalid lint extension manifest: {error}"))
                })?;
            let manifest_value: serde_json::Value = serde_json::from_str(&package.manifest_text)
                .map_err(|error| {
                    extension_error(format!("Invalid lint extension manifest: {error}"))
                })?;
            if manifest.schema != "wake.lint.package.v1"
                || manifest.protocol != "wake.lint.extension.v1"
                || manifest.sdk_major != 1
                || manifest.name != *name
                || manifest.version.is_empty()
                || manifest_value != package.manifest
                || package.module_path.is_empty()
                || package.module_hash.len() != 64
                || !package
                    .module_hash
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(extension_error(format!(
                    "Incompatible lint extension manifest: {name}"
                )));
            }
            if manifest.rules.is_empty() || manifest.rules.len() > 512 {
                return Err(extension_error(format!(
                    "Invalid lint extension rule count: {name}"
                )));
            }
            for rule in manifest.rules {
                if rule.id.is_empty()
                    || !rule.id.contains('/')
                    || wake_lint_core::rule_catalog()
                        .iter()
                        .any(|native| native.id == rule.id)
                    || rule.meta.description.is_empty()
                    || !matches!(
                        rule.meta.category.as_str(),
                        "problem" | "suggestion" | "layout"
                    )
                    || rule.meta.fixable
                    || rule.message_ids.is_empty()
                    || rule.message_ids.iter().any(String::is_empty)
                    || rule.message_ids.iter().collect::<BTreeSet<_>>().len()
                        != rule.message_ids.len()
                    || rule.options_schema.kind != "object"
                    || rule.options_schema.additional_properties
                    || !rule.options_schema.properties.is_empty()
                {
                    return Err(extension_error(format!(
                        "Invalid lint extension rule manifest: {}",
                        rule.id
                    )));
                }
                let id = rule.id.clone();
                if rules.insert(id.clone(), rule).is_some() {
                    return Err(extension_error(format!(
                        "Duplicate lint extension rule: {id}"
                    )));
                }
            }
        }
        Ok(Some(Self {
            root: root.to_path_buf(),
            packages,
            rules,
        }))
    }

    pub(super) fn has_rule(&self, id: &str) -> bool {
        self.rules.contains_key(id)
    }

    pub(super) fn verify_identity(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), WakeError> {
        let names = self
            .packages
            .iter()
            .map(|package| package.name.clone())
            .collect::<Vec<_>>();
        let current = Self::load(&self.root, &names, cancellation)?
            .expect("nonempty extension packages reload to a catalog");
        if current.packages != self.packages {
            return Err(extension_error(
                "Lint extension package identity changed during the check",
            ));
        }
        Ok(())
    }

    pub(super) fn all_rules(&self) -> impl Iterator<Item = &String> {
        self.rules.keys()
    }

    pub(super) fn validate_setting(
        &self,
        id: &str,
        setting: &LintRuleSetting,
    ) -> Result<(), WakeError> {
        if !self.has_rule(id) {
            return Err(failure(format!("Unknown lint rule: {id}")));
        }
        if let LintRuleSetting::Options(value) = setting
            && !value.options.is_empty()
        {
            return Err(extension_error(format!(
                "Lint extension rule {id} does not accept options in package schema v1"
            )));
        }
        Ok(())
    }

    pub(super) fn run(
        &self,
        path: &str,
        source: &str,
        language: &str,
        settings: &BTreeMap<String, LintRuleSetting>,
        cancellation: &CancellationToken,
    ) -> Result<Vec<LintDiagnosticInfo>, WakeError> {
        if source.len() > MAX_SOURCE_BYTES {
            return Err(extension_error("Lint extension source exceeds 16 MiB"));
        }
        let rules = settings
            .iter()
            .filter(|(_, setting)| !matches!(setting.level(), LintRuleLevel::Off))
            .map(|(id, setting)| {
                serde_json::json!({"id":id, "level":setting.level().as_str(), "options":{}})
            })
            .collect::<Vec<_>>();
        if rules.is_empty() {
            return Ok(Vec::new());
        }
        let response = call_worker(
            &self.root,
            &serde_json::json!({
                "mode":"run", "root":self.root, "packages":self.packages,
                "path":path, "language":language, "source":source, "rules":rules,
            }),
            cancellation,
        )?;
        let raw = response
            .diagnostics
            .ok_or_else(|| extension_error("Lint extension host omitted diagnostics"))?;
        if raw.len() > MAX_DIAGNOSTICS {
            return Err(extension_error("Lint extension diagnostic budget exceeded"));
        }
        let source_file = SourceFile::new_ecmascript(path.to_owned(), source.to_owned());
        let mut diagnostics = Vec::with_capacity(raw.len());
        for value in raw {
            let rule = self.rules.get(&value.id).ok_or_else(|| {
                extension_error(format!(
                    "Lint extension reported an unknown rule: {}",
                    value.id
                ))
            })?;
            let level = settings
                .get(&value.id)
                .ok_or_else(|| {
                    extension_error(format!(
                        "Lint extension reported a disabled rule: {}",
                        value.id
                    ))
                })?
                .level();
            if matches!(level, LintRuleLevel::Off)
                || !rule.message_ids.contains(&value.message_id)
                || value.message.is_empty()
                || value.message.len() > 4096
                || value.start > value.end
                || value.end > source.len() as u64
                || value.end > u32::MAX as u64
                || !source.is_char_boundary(value.start as usize)
                || !source.is_char_boundary(value.end as usize)
            {
                return Err(extension_error(
                    "Lint extension returned an invalid diagnostic",
                ));
            }
            let mut diagnostic = Diagnostic::new(
                if matches!(level, LintRuleLevel::Error) {
                    Severity::Error
                } else {
                    Severity::Warning
                },
                value.message,
            )
            .with_code(value.id)
            .with_primary(
                Span::new(value.start as u32, value.end as u32),
                value.message_id.clone(),
            );
            diagnostic.path = Some(path.to_owned());
            diagnostics.push(LintDiagnosticInfo {
                diagnostic: DiagnosticInfo::from_diagnostic(&diagnostic, Some(&source_file)),
                message_id: Some(value.message_id),
                fix: None,
            });
        }
        Ok(diagnostics)
    }
}

fn valid_package_name(name: &str) -> bool {
    let Some((scope, package)) = name.strip_prefix('@').and_then(|name| name.split_once('/'))
    else {
        return false;
    };
    !scope.is_empty()
        && !package.is_empty()
        && [scope, package].iter().all(|part| {
            part.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.')
            })
        })
}

fn node_root(root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let canonical = root.to_string_lossy();
        if let Some(unc) = canonical.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        if let Some(path) = canonical.strip_prefix(r"\\?\") {
            return PathBuf::from(path);
        }
    }
    root.to_path_buf()
}

fn call_worker(
    root: &Path,
    request: &serde_json::Value,
    cancellation: &CancellationToken,
) -> Result<WorkerResponse, WakeError> {
    cancellation.check()?;
    let mut nonce_bytes = [0u8; 16];
    getrandom::fill(&mut nonce_bytes).map_err(|error| {
        extension_error(format!("Could not start lint extension host: {error}"))
    })?;
    let nonce = format!("{:032x}", u128::from_le_bytes(nonce_bytes));
    let mut request = request.clone();
    let host_root = node_root(root);
    request
        .as_object_mut()
        .expect("lint extension request is an object")
        .insert("nonce".into(), serde_json::Value::String(nonce.clone()));
    request
        .as_object_mut()
        .expect("lint extension request is an object")
        .insert(
            "root".into(),
            serde_json::Value::String(host_root.to_string_lossy().into_owned()),
        );
    let input = serde_json::to_vec(&request).map_err(|error| {
        extension_error(format!("Could not encode lint extension request: {error}"))
    })?;
    let mut command = Command::new("node");
    command.arg("--max-old-space-size=128");
    let pnp = host_root.join(".pnp.cjs");
    if pnp.is_file() {
        command.arg("--require").arg(&pnp);
        let loader = host_root.join(".pnp.loader.mjs");
        if loader.is_file() {
            // Node treats an absolute Windows drive path as an unsupported URL scheme here.
            command
                .arg("--experimental-loader")
                .arg("./.pnp.loader.mjs");
        }
    }
    // Node evaluates `-e` top-level bindings in a context visible to `vm.runInThisContext`.
    // Keep the response nonce and writer in a function-local scope before importing package code.
    let worker_source = format!(
        "(function () {{\n{}\n}})();",
        include_str!("extension_host.cjs")
    );
    let mut output_file = tempfile::NamedTempFile::new().map_err(|error| {
        extension_error(format!("Could not create lint extension output: {error}"))
    })?;
    // The worker only receives an append-only handle. Its package code cannot read
    // the response nonce back from stdout through the inherited descriptor.
    let output_writer = OpenOptions::new()
        .append(true)
        .open(output_file.path())
        .map_err(|error| {
            extension_error(format!("Could not open lint extension output: {error}"))
        })?;
    let mut child = command
        .arg("-e")
        .arg(worker_source)
        .current_dir(&host_root)
        .env_remove("NODE_OPTIONS")
        .stdin(Stdio::piped())
        .stdout(Stdio::from(output_writer))
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            extension_error(format!("Could not start lint extension host: {error}"))
        })?;
    let mut stdin = child.stdin.take().expect("piped extension input");
    let (writer_tx, writer_rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = writer_tx.send(stdin.write_all(&input).map_err(|error| error.to_string()));
    });
    let started = Instant::now();
    let mut status = None;
    let mut written = None;
    loop {
        if cancellation.is_cancelled() || started.elapsed() > WORKER_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            if cancellation.is_cancelled() {
                return Err(WakeError::cancelled());
            }
            return Err(extension_error("Lint extension host timed out"));
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(value) => status = value,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(extension_error(format!(
                        "Lint extension host failed: {error}"
                    )));
                }
            }
        }
        let output_len = match output_file.as_file().metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(extension_error(format!(
                    "Could not inspect lint extension output: {error}"
                )));
            }
        };
        if output_len > MAX_WORKER_OUTPUT as u64 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(extension_error(
                "Lint extension host output budget exceeded",
            ));
        }
        if written.is_none() {
            match writer_rx.try_recv() {
                Ok(value) => written = Some(value),
                Err(mpsc::TryRecvError::Disconnected) => {
                    written = Some(Err("Lint extension host input thread failed".into()));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(status) = status {
            if !status.success() {
                return Err(extension_error(
                    "Lint extension host exited without a valid response",
                ));
            }
            if let Some(written) = written {
                written.map_err(|error| {
                    extension_error(format!("Lint extension host input failed: {error}"))
                })?;
                let output = read_worker_output(&mut output_file)?;
                let response: WorkerResponse = serde_json::from_slice(&output)
                    .map_err(|_| extension_error("Lint extension host returned invalid JSON"))?;
                if response.nonce != nonce {
                    return Err(extension_error(
                        "Lint extension host response identity changed",
                    ));
                }
                if !response.ok {
                    return Err(extension_error(
                        response
                            .error
                            .unwrap_or_else(|| "Lint extension host failed".into()),
                    ));
                }
                return Ok(response);
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_worker_output(output_file: &mut tempfile::NamedTempFile) -> Result<Vec<u8>, WakeError> {
    let length = output_file
        .as_file()
        .metadata()
        .map_err(|error| {
            extension_error(format!("Could not inspect lint extension output: {error}"))
        })?
        .len();
    if length > MAX_WORKER_OUTPUT as u64 {
        return Err(extension_error(
            "Lint extension host output budget exceeded",
        ));
    }
    let mut output = vec![0; length as usize];
    output_file
        .as_file_mut()
        .seek(SeekFrom::Start(0))
        .and_then(|_| output_file.as_file_mut().read_exact(&mut output))
        .map_err(|error| {
            extension_error(format!("Could not read lint extension output: {error}"))
        })?;
    if output_file
        .as_file()
        .metadata()
        .map_err(|error| {
            extension_error(format!("Could not inspect lint extension output: {error}"))
        })?
        .len()
        != length
    {
        return Err(extension_error("Lint extension host output changed"));
    }
    let Some(end) = output.iter().position(|byte| *byte == b'\n') else {
        return Err(extension_error(
            "Lint extension host closed stdout before its response",
        ));
    };
    if end + 1 != output.len() {
        return Err(extension_error(
            "Lint extension host returned trailing output",
        ));
    }
    output.truncate(end);
    Ok(output)
}
