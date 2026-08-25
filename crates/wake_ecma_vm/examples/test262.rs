use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde::Deserialize;
use wake_ecma_vm::{ScriptSource, Vm, VmErrorKind, VmOptions};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    contract: String,
    suite: String,
    target: String,
    commit: String,
    archive_url: String,
    sha256: String,
    license: String,
    expected_files: usize,
    expected_variants: usize,
    selected_roots: Vec<String>,
    excluded_tests: Vec<String>,
    exclusion_reasons: BTreeMap<String, String>,
}

#[derive(Debug, Default)]
struct Metadata {
    flags: BTreeSet<String>,
    includes: Vec<String>,
    negative_phase: Option<String>,
    negative_type: Option<String>,
}

#[derive(Debug)]
struct Variant {
    name: &'static str,
    strict: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Test262 conformance failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args_os().skip(1);
    let root = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "usage: test262 <suite-root> <manifest.json>".to_string())?;
    let manifest_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "usage: test262 <suite-root> <manifest.json>".to_string())?;
    if args.next().is_some() {
        return Err("usage: test262 <suite-root> <manifest.json>".to_string());
    }

    let manifest: Manifest = serde_json::from_str(
        &fs::read_to_string(&manifest_path)
            .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?,
    )
    .map_err(|error| format!("invalid {}: {error}", manifest_path.display()))?;
    validate_manifest(&manifest)?;

    let excluded: BTreeSet<_> = manifest.excluded_tests.iter().cloned().collect();
    let mut tests = Vec::new();
    for selected in &manifest.selected_roots {
        let selected_path = root.join(selected);
        if !selected_path.is_dir() {
            return Err(format!("selected Test262 root is missing: {selected}"));
        }
        collect_javascript_files(&root, &selected_path, &excluded, &mut tests)?;
    }
    tests.sort();
    tests.dedup();
    if tests.len() != manifest.expected_files {
        return Err(format!(
            "manifest expected {} Test262 files but selected {}",
            manifest.expected_files,
            tests.len()
        ));
    }

    let harness_root = root.join("harness");
    let assert_harness = read(&harness_root.join("assert.js"))?;
    let sta_harness = read(&harness_root.join("sta.js"))?;
    let mut failures = Vec::new();
    let mut variants_run = 0usize;

    for relative in &tests {
        let source = read(&root.join(relative))?;
        let metadata =
            parse_metadata(&source).map_err(|error| format!("{}: {error}", slash(relative)))?;
        reject_unsupported_contract(relative, &metadata, &source)?;
        let variants = variants(&metadata);
        variants_run += variants.len();

        let mut includes = String::new();
        if !metadata.flags.contains("raw") {
            includes.push_str(&assert_harness);
            includes.push('\n');
            includes.push_str(&sta_harness);
            includes.push('\n');
            for include in &metadata.includes {
                let include_path = harness_root.join(include);
                includes.push_str(&read(&include_path)?);
                includes.push('\n');
            }
        }

        for variant in variants {
            if let Err(error) = execute_variant(relative, &source, &includes, &metadata, &variant) {
                failures.push(format!("{} [{}]: {error}", slash(relative), variant.name));
            }
        }
    }

    if variants_run != manifest.expected_variants {
        return Err(format!(
            "manifest expected {} variants but executed {variants_run}",
            manifest.expected_variants
        ));
    }
    if !failures.is_empty() {
        for failure in failures.iter().take(50) {
            eprintln!("FAIL {failure}");
        }
        if failures.len() > 50 {
            eprintln!("... {} additional failures", failures.len() - 50);
        }
        return Err(format!(
            "{} of {variants_run} Test262 variants failed",
            failures.len()
        ));
    }

    println!(
        "Test262 {} selected manifest passed: {} files, {} variants, V8 {}",
        manifest.target,
        tests.len(),
        variants_run,
        Vm::engine_version()
    );
    Ok(())
}

fn validate_manifest(manifest: &Manifest) -> Result<(), String> {
    if manifest.schema_version != 1
        || manifest.contract != "ADR-0020"
        || manifest.suite != "test262"
        || manifest.target != "ES2024"
        || manifest.license != "BSD-3-Clause"
    {
        return Err("manifest identity does not match the ADR-0020 ES2024 contract".to_string());
    }
    if manifest.commit.len() != 40
        || !manifest.commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        || manifest.sha256.len() != 64
        || !manifest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !manifest.archive_url.contains(&manifest.commit)
    {
        return Err("manifest source commit/archive/checksum is not immutable".to_string());
    }
    if manifest.selected_roots.is_empty() || manifest.expected_files == 0 {
        return Err("manifest must select at least one exact Test262 root".to_string());
    }
    let excluded: BTreeSet<_> = manifest.excluded_tests.iter().cloned().collect();
    let explained: BTreeSet<_> = manifest.exclusion_reasons.keys().cloned().collect();
    if excluded != explained
        || manifest
            .exclusion_reasons
            .values()
            .any(|reason| reason.trim().is_empty())
    {
        return Err("every excluded Test262 file must have one non-empty reason".to_string());
    }
    Ok(())
}

fn collect_javascript_files(
    root: &Path,
    directory: &Path,
    excluded: &BTreeSet<String>,
    output: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot enumerate {}: {error}", directory.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            collect_javascript_files(root, &path, excluded, output)?;
        } else if file_type.is_file() && path.extension().is_some_and(|extension| extension == "js")
        {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| format!("{} escaped the Test262 root", path.display()))?
                .to_path_buf();
            if !excluded.contains(&slash(&relative)) {
                output.push(relative);
            }
        }
    }
    Ok(())
}

fn parse_metadata(source: &str) -> Result<Metadata, String> {
    let start = source
        .find("/*---")
        .ok_or_else(|| "missing Test262 frontmatter".to_string())?
        + "/*---".len();
    let tail = &source[start..];
    let end = tail
        .find("---*/")
        .ok_or_else(|| "unterminated Test262 frontmatter".to_string())?;
    let lines: Vec<_> = tail[..end].lines().collect();
    let mut metadata = Metadata::default();
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index].trim();
        if let Some(value) = line.strip_prefix("flags:") {
            metadata.flags = parse_list(value, &lines, &mut index)?.into_iter().collect();
        } else if let Some(value) = line.strip_prefix("includes:") {
            metadata.includes = parse_list(value, &lines, &mut index)?;
        } else if line == "negative:" {
            index += 1;
            while index < lines.len() {
                let nested = lines[index];
                if !nested.starts_with(' ') && !nested.starts_with('\t') {
                    index = index.saturating_sub(1);
                    break;
                }
                let nested = nested.trim();
                if let Some(value) = nested.strip_prefix("phase:") {
                    metadata.negative_phase = Some(value.trim().to_string());
                } else if let Some(value) = nested.strip_prefix("type:") {
                    metadata.negative_type = Some(value.trim().to_string());
                }
                index += 1;
            }
        }
        index += 1;
    }
    if metadata.negative_phase.is_some() != metadata.negative_type.is_some() {
        return Err("negative metadata requires both phase and type".to_string());
    }
    Ok(metadata)
}

fn parse_list(value: &str, lines: &[&str], index: &mut usize) -> Result<Vec<String>, String> {
    let value = value.trim();
    if value.starts_with('[') {
        let body = value
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .ok_or_else(|| "frontmatter inline list must close on the same line".to_string())?;
        return Ok(body
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(trim_yaml_scalar)
            .collect());
    }
    let mut values = Vec::new();
    while *index + 1 < lines.len() {
        let next = lines[*index + 1];
        let Some(item) = next.trim().strip_prefix('-') else {
            break;
        };
        if !next.starts_with(' ') && !next.starts_with('\t') {
            break;
        }
        values.push(trim_yaml_scalar(item.trim()));
        *index += 1;
    }
    Ok(values)
}

fn trim_yaml_scalar(value: &str) -> String {
    value
        .trim_matches(|character| character == '\'' || character == '"')
        .to_string()
}

fn reject_unsupported_contract(
    relative: &Path,
    metadata: &Metadata,
    source: &str,
) -> Result<(), String> {
    for unsupported in ["module", "CanBlockIsFalse"] {
        if metadata.flags.contains(unsupported) {
            return Err(format!(
                "selected test {} requires unsupported flag {unsupported}; remove it from the manifest or implement the host contract",
                slash(relative)
            ));
        }
    }
    for unsupported in ["$262.agent", "$262.createRealm", "$262.IsHTMLDDA"] {
        if source.contains(unsupported) {
            return Err(format!(
                "selected test {} requires unsupported host API {unsupported}",
                slash(relative)
            ));
        }
    }
    Ok(())
}

fn variants(metadata: &Metadata) -> Vec<Variant> {
    if metadata.flags.contains("raw") || metadata.flags.contains("noStrict") {
        vec![Variant {
            name: "default",
            strict: false,
        }]
    } else if metadata.flags.contains("onlyStrict") {
        vec![Variant {
            name: "strict",
            strict: true,
        }]
    } else {
        vec![
            Variant {
                name: "default",
                strict: false,
            },
            Variant {
                name: "strict",
                strict: true,
            },
        ]
    }
}

fn execute_variant(
    relative: &Path,
    source: &str,
    includes: &str,
    metadata: &Metadata,
    variant: &Variant,
) -> Result<(), String> {
    let mut vm = Vm::with_options(VmOptions {
        execution_timeout: Some(Duration::from_secs(10)),
        ..VmOptions::default()
    });
    vm.register_array_buffer_detach_function("__wakeTest262DetachArrayBuffer")
        .map_err(|error| format!("cannot install detach host: {error}"))?;
    let strict = if variant.strict {
        "\"use strict\";\n"
    } else {
        ""
    };
    let async_test = metadata.flags.contains("async");
    let prelude = format!(
        r#"
Object.defineProperty(globalThis, "__wakeTest262Started", {{ value: false, writable: true }});
Object.defineProperty(globalThis, "__wakeTest262Async", {{ value: {{ state: "pending", error: "" }}, writable: false }});
Object.defineProperty(globalThis, "print", {{ configurable: true, writable: true, value(message) {{
  if (String(message) === "Test262:AsyncTestComplete") __wakeTest262Async.state = "passed";
}} }});
Object.defineProperty(globalThis, "$DONE", {{ configurable: true, writable: true, value(error) {{
  if (error === undefined) __wakeTest262Async.state = "passed";
  else {{ __wakeTest262Async.state = "failed"; __wakeTest262Async.error = String(error && error.stack || error); }}
}} }});
Object.defineProperty(globalThis, "$262", {{ configurable: true, writable: true, value: {{
  global: globalThis,
  evalScript(source) {{ return (0, eval)(String(source)); }},
  detachArrayBuffer(buffer) {{ return __wakeTest262DetachArrayBuffer(buffer); }},
  gc() {{ throw new Test262Error("Wake Test262 manifest does not provide explicit GC"); }}
}} }});
{strict}{includes}
globalThis.__wakeTest262Started = true;
{source}
"#
    );
    let path = slash(relative);
    let result = if async_test {
        vm.execute_and_read(
            &ScriptSource::new(path.clone(), prelude),
            "JSON.stringify(globalThis.__wakeTest262Async)",
        )
    } else {
        vm.execute(&ScriptSource::new(path.clone(), prelude))
    };

    match (&metadata.negative_phase, &metadata.negative_type, result) {
        (None, None, Ok(async_state)) if async_test => {
            let state: serde_json::Value = serde_json::from_str(&async_state)
                .map_err(|error| format!("invalid async completion state: {error}"))?;
            match state["state"].as_str() {
                Some("passed") => Ok(()),
                Some("failed") => Err(format!(
                    "async completion failed: {}",
                    state["error"].as_str().unwrap_or("unknown error")
                )),
                _ => Err("async test did not call $DONE".to_string()),
            }
        }
        (None, None, Ok(_)) => Ok(()),
        (None, None, Err(error)) => Err(format!("unexpected {:?}: {}", error.kind, error.message)),
        (Some(phase), Some(expected), Err(error)) => {
            if !error.message.contains(expected) {
                return Err(format!(
                    "expected {expected} during {phase}, received {:?}: {}",
                    error.kind, error.message
                ));
            }
            let started = vm
                .execute(&ScriptSource::new(
                    "<test262-phase-check>",
                    "String(globalThis.__wakeTest262Started === true)",
                ))
                .unwrap_or_else(|_| "false".to_string())
                == "true";
            match phase.as_str() {
                "parse" if !started && error.kind == VmErrorKind::Exception => Ok(()),
                "runtime" if started && error.kind == VmErrorKind::Exception => Ok(()),
                other => Err(format!(
                    "negative phase mismatch: expected {other}, test-started={started}, kind={:?}",
                    error.kind
                )),
            }
        }
        (Some(phase), Some(expected), Ok(_)) => Err(format!(
            "expected {expected} during {phase}, but the test passed"
        )),
        _ => Err("invalid negative metadata".to_string()),
    }
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_and_nested_metadata_without_a_yaml_runtime() {
        let metadata = parse_metadata(
            r#"/*---
flags: [onlyStrict, async]
includes:
  - compareArray.js
negative:
  phase: runtime
  type: TypeError
---*/"#,
        )
        .unwrap();
        assert_eq!(
            metadata.flags,
            BTreeSet::from(["async".to_string(), "onlyStrict".to_string()])
        );
        assert_eq!(metadata.includes, ["compareArray.js"]);
        assert_eq!(metadata.negative_phase.as_deref(), Some("runtime"));
        assert_eq!(metadata.negative_type.as_deref(), Some("TypeError"));
    }
}
