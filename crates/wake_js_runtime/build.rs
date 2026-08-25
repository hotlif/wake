use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const NPM_CI_HINT: &str =
    "run `npm ci` from the Wake workspace root; build.rs never downloads JavaScript packages";

struct PackageSpec {
    name: &'static str,
    version: &'static str,
    embedded_roots: &'static [(&'static str, &'static str)],
}

const PACKAGES: &[PackageSpec] = &[
    PackageSpec {
        name: "happy-dom",
        version: "20.11.6",
        embedded_roots: &[("lib", "lib")],
    },
    PackageSpec {
        name: "entities",
        version: "7.0.1",
        embedded_roots: &[("dist/esm", "npm/entities/dist/esm")],
    },
    PackageSpec {
        name: "whatwg-mimetype",
        version: "3.0.0",
        embedded_roots: &[("lib", "npm/whatwg-mimetype/lib")],
    },
    PackageSpec {
        name: "buffer-image-size",
        version: "0.6.4",
        embedded_roots: &[("lib", "npm/buffer-image-size/lib")],
    },
    // Happy DOM imports `ws`, but Wake's trusted-test network policy denies WebSocket creation
    // through a first-party adapter. We still validate the npm dependency selected by the lockfile
    // so dependency drift cannot silently change the embedded substrate.
    PackageSpec {
        name: "ws",
        version: "8.21.3",
        embedded_roots: &[],
    },
];

fn main() {
    if let Err(message) = generate() {
        panic!("{message}");
    }
}

fn generate() -> Result<(), String> {
    let manifest = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .ok_or_else(|| "missing CARGO_MANIFEST_DIR".to_string())?,
    );
    let workspace = manifest.parent().and_then(Path::parent).ok_or_else(|| {
        format!(
            "could not locate the Wake workspace above {}",
            manifest.display()
        )
    })?;
    let node_modules = workspace.join("node_modules");
    let mut files = BTreeMap::new();

    for package in PACKAGES {
        let root = node_modules.join(package.name);
        validate_package(&root, package)?;
        for (relative_root, key_prefix) in package.embedded_roots {
            let source_root = root.join(relative_root);
            println!("cargo:rerun-if-changed={}", source_root.display());
            collect_javascript(
                &source_root,
                &source_root,
                Path::new(key_prefix),
                &mut files,
            )?;
        }
    }

    let wake_runtime = manifest.join("runtime/happy-dom");
    println!("cargo:rerun-if-changed={}", wake_runtime.display());
    collect_javascript(&wake_runtime, &wake_runtime, Path::new(""), &mut files)?;

    let mut generated = String::from(
        "pub(crate) fn source(path: &str) -> Option<&'static str> {\n    match path {\n",
    );
    for (key, source) in files {
        let key = key.to_string_lossy().replace('\\', "/");
        let source = source.to_string_lossy().replace('\\', "/");
        generated.push_str(&format!(
            "        {key:?} => Some(include_str!({source:?})),\n"
        ));
    }
    generated.push_str("        _ => None,\n    }\n}\n");
    let output =
        PathBuf::from(env::var_os("OUT_DIR").ok_or_else(|| "missing Cargo OUT_DIR".to_string())?)
            .join("wake_happy_dom_sources.rs");
    fs::write(&output, generated)
        .map_err(|error| format!("could not write {}: {error}", output.display()))?;
    Ok(())
}

fn validate_package(root: &Path, expected: &PackageSpec) -> Result<(), String> {
    let metadata_path = root.join("package.json");
    println!("cargo:rerun-if-changed={}", metadata_path.display());
    let metadata = fs::read_to_string(&metadata_path).map_err(|error| {
        format!(
            "Wake requires npm package {}@{} at {} ({error}); {NPM_CI_HINT}",
            expected.name,
            expected.version,
            metadata_path.display()
        )
    })?;
    let name = json_string_field(&metadata, "name").ok_or_else(|| {
        format!(
            "{} does not contain a string `name`; {NPM_CI_HINT}",
            metadata_path.display()
        )
    })?;
    let version = json_string_field(&metadata, "version").ok_or_else(|| {
        format!(
            "{} does not contain a string `version`; {NPM_CI_HINT}",
            metadata_path.display()
        )
    })?;
    if name != expected.name || version != expected.version {
        return Err(format!(
            "Wake expected npm package {}@{} at {}, found {name}@{version}; {NPM_CI_HINT}",
            expected.name,
            expected.version,
            metadata_path.display()
        ));
    }
    Ok(())
}

fn json_string_field(source: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\"");
    let after_name = source.split_once(&needle)?.1.trim_start();
    let value = after_name
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let mut output = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            output.push(match character {
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                'b' => '\u{0008}',
                'f' => '\u{000c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => return None,
            });
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return Some(output);
        } else {
            output.push(character);
        }
    }
    None
}

fn collect_javascript(
    root: &Path,
    directory: &Path,
    key_prefix: &Path,
    files: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|error| {
        format!(
            "could not read JavaScript package directory {} ({error}); {NPM_CI_HINT}",
            directory.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "could not read an entry beneath {}: {error}",
                directory.display()
            )
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_javascript(root, &path, key_prefix, files)?;
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("js" | "mjs" | "cjs")
        ) {
            let relative = path.strip_prefix(root).map_err(|error| {
                format!("{} escaped {}: {error}", path.display(), root.display())
            })?;
            let key = key_prefix.join(relative);
            if let Some(previous) = files.insert(key.clone(), path.clone()) {
                return Err(format!(
                    "embedded JavaScript key {} is provided by both {} and {}",
                    key.display(),
                    previous.display(),
                    path.display()
                ));
            }
        }
    }
    Ok(())
}
