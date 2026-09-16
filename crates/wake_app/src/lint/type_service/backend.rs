//! Locate a declared, versioned native compiler without executing JavaScript package loaders.
use crate::WakeError;
use serde_json::Value;
use std::path::{Path, PathBuf};
use wake_common::{FileSystem, fs::normalize};
use wake_resolver::{ResolutionEnvironment, ResolveOptions};

const VERSION: &str = "7.0.2";

#[derive(Debug)]
pub(super) struct Backend {
    pub executable: PathBuf,
}

fn failure(message: impl std::fmt::Display) -> WakeError {
    WakeError::new("WAKE_LINT_ANALYSIS", format!("Type backend: {message}"))
}

fn manifest(fs: &dyn FileSystem, path: &Path) -> Result<Value, WakeError> {
    let bytes = fs.read(path).map_err(|error| failure(error).at(path))?;
    if bytes.len() > 1024 * 1024 {
        return Err(failure("manifest exceeds the 1 MiB budget").at(path));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| failure(error).at(path))?;
    if !value.is_object() {
        return Err(failure("manifest must be an object").at(path));
    }
    Ok(value)
}

impl Backend {
    pub(super) fn locate(
        environment: &ResolutionEnvironment,
        root: &Path,
        compiler: &str,
    ) -> Result<Self, WakeError> {
        Self::locate_for(
            environment,
            root,
            compiler,
            std::env::consts::OS,
            std::env::consts::ARCH,
        )
    }

    fn locate_for(
        environment: &ResolutionEnvironment,
        root: &Path,
        compiler: &str,
        os: &str,
        cpu: &str,
    ) -> Result<Self, WakeError> {
        let npm_os = match os {
            "windows" => "win32",
            "macos" => "darwin",
            "linux" => "linux",
            _ => return Err(failure(format!("unsupported native platform: {os}/{cpu}"))),
        };
        let npm_cpu = match cpu {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            _ => return Err(failure(format!("unsupported native platform: {os}/{cpu}"))),
        };
        let root = normalize(root);
        if !root.is_absolute() {
            return Err(failure("expected an absolute project root"));
        }
        let fs = environment.file_system();
        let owner = root
            .ancestors()
            .map(|directory| directory.join("package.json"))
            .find(|path| fs.is_file(path))
            .ok_or_else(|| failure("project has no package.json declaring its compiler"))?;
        let owner_metadata = manifest(fs.as_ref(), &owner)?;
        let declared = [
            "dependencies",
            "devDependencies",
            "optionalDependencies",
            "peerDependencies",
        ]
        .iter()
        .any(|field| {
            owner_metadata[*field][compiler]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        });
        if !declared {
            return Err(failure(format!(
                "{compiler} must be declared in the project's package.json"
            ))
            .at(&owner));
        }
        let compiler_root = environment
            .resolver()
            .resolve_package_root(compiler, &root)
            .map_err(failure)?;
        let compiler_manifest = compiler_root.join("package.json");
        let metadata = manifest(fs.as_ref(), &compiler_manifest)?;
        if metadata["name"] != "typescript" || metadata["version"] != VERSION {
            return Err(failure(format!(
                "requires typescript@{VERSION}, found {}@{}",
                metadata["name"], metadata["version"]
            ))
            .at(&compiler_manifest));
        }
        let package = format!("@typescript/typescript-{npm_os}-{npm_cpu}");
        if metadata["optionalDependencies"][&package] != VERSION {
            return Err(
                failure(format!("compiler must declare {package}@{VERSION}"))
                    .at(&compiler_manifest),
            );
        }
        let context = environment
            .node_modules_view(&root)
            .map_err(failure)?
            .context_for_issuer(&compiler_root)
            .map_err(failure)?;
        let installed = ResolutionEnvironment::with_context(
            environment.base_file_system(),
            ResolveOptions::default(),
            context,
        );
        let platform_root = installed
            .resolver()
            .resolve_package_root(&package, &compiler_root)
            .map_err(failure)?;
        let platform_manifest = platform_root.join("package.json");
        let metadata = manifest(installed.file_system().as_ref(), &platform_manifest)?;
        if metadata["name"] != package
            || metadata["version"] != VERSION
            || metadata["os"] != serde_json::json!([npm_os])
            || metadata["cpu"] != serde_json::json!([npm_cpu])
        {
            return Err(failure(format!(
                "requires {package}@{VERSION} for {npm_os}/{npm_cpu}"
            ))
            .at(&platform_manifest));
        }
        let name = if os == "windows" { "tsc.exe" } else { "tsc" };
        let logical = platform_root.join("lib").join(name);
        let physical = installed.watch_path(&logical);
        // Archive entries cannot be directly launched. A Yarn virtual path may still map to
        // an ordinary unpacked executable, in which case its physical basename is preserved.
        if physical.file_name().and_then(|value| value.to_str()) != Some(name)
            || !installed.base_file_system().is_file(&physical)
        {
            return Err(failure("native executable must be installed as an accessible physical file (unplug the platform package when using Yarn PnP)").at(&logical));
        }
        let executable = installed
            .base_file_system()
            .canonicalize(&physical)
            .map(|path| normalize(&path))
            .map_err(|error| failure(error).at(&physical))?;
        Ok(Self { executable })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use wake_common::{MemoryFileSystem, fs::normalize};

    #[test]
    fn backend_location_validates_declared_alias_version_platform_and_compiler_issuer() {
        let temporary = tempfile::tempdir().unwrap();
        let root = normalize(temporary.path());
        for (os, cpu, npm_os, npm_cpu) in [
            ("windows", "x86_64", "win32", "x64"),
            ("windows", "aarch64", "win32", "arm64"),
            ("macos", "x86_64", "darwin", "x64"),
            ("macos", "aarch64", "darwin", "arm64"),
            ("linux", "x86_64", "linux", "x64"),
            ("linux", "aarch64", "linux", "arm64"),
        ] {
            for fault in [
                "none",
                "undeclared",
                "compiler-version",
                "compiler-name",
                "platform-version",
                "platform-name",
                "platform-cpu",
                "platform-os",
                "dependency-version",
                "missing-executable",
                "oversized",
            ] {
                let disk = Arc::new(MemoryFileSystem::new());
                let compiler = root.join("node_modules/compiler-alias");
                let package = format!("@typescript/typescript-{npm_os}-{npm_cpu}");
                let platform = compiler.join("node_modules").join(&package);
                let executable = platform.join(if os == "windows" {
                    "lib/tsc.exe"
                } else {
                    "lib/tsc"
                });
                disk.insert(
                    root.join("package.json"),
                    if fault == "undeclared" {
                        "{}".to_owned()
                    } else {
                        json!({"devDependencies":{"compiler-alias":"npm:typescript@7.0.2"}})
                            .to_string()
                    },
                );
                let compiler_metadata = json!({"name":if fault == "compiler-name" {"other"}else{"typescript"},"version":if fault == "compiler-version" {"6.0.2"}else{"7.0.2"},"optionalDependencies":{&package:if fault == "dependency-version"{"7.0.1"}else{"7.0.2"}}});
                disk.insert(
                    compiler.join("package.json"),
                    if fault == "oversized" {
                        format!("{}{}", " ".repeat(1024 * 1024), compiler_metadata)
                    } else {
                        compiler_metadata.to_string()
                    },
                );
                disk.insert(platform.join("package.json"), json!({"name":if fault == "platform-name" {"wrong"}else{&package},"version":if fault == "platform-version"{"7.0.1"}else{"7.0.2"},"os":[if fault=="platform-os"{"wrong"}else{npm_os}],"cpu":[if fault=="platform-cpu"{"wrong"}else{npm_cpu}]}).to_string());
                if fault != "missing-executable" {
                    disk.insert(&executable, "native executable fixture");
                }
                // A root-level platform package must not override the compiler's own dependency.
                disk.insert(
                    root.join("node_modules")
                        .join(&package)
                        .join("package.json"),
                    "{\"version\":\"wrong\"}",
                );
                let environment = ResolutionEnvironment::new(disk);
                let result = Backend::locate_for(&environment, &root, "compiler-alias", os, cpu);
                if fault == "none" {
                    assert_eq!(result.unwrap().executable, executable);
                } else {
                    let error = result
                        .err()
                        .unwrap_or_else(|| panic!("{os}/{cpu}/{fault} was accepted"));
                    assert_eq!(error.code, "WAKE_LINT_ANALYSIS", "{error}");
                }
            }
        }
        let environment = ResolutionEnvironment::new(Arc::new(MemoryFileSystem::new()));
        assert!(Backend::locate_for(&environment, &root, "typescript", "other", "x86_64").is_err());
        assert!(Backend::locate_for(&environment, &root, "typescript", "linux", "other").is_err());
    }

    #[test]
    #[ignore = "requires the repository's installed @typescript/native alias and matching native platform package"]
    fn frozen_backend_and_library_files_support_unsaved_type_queries() {
        use super::super::{
            filesystem::TypeFileSystem,
            transport::{Limits, Session},
            wire::WireSource,
        };
        use crate::CancellationToken;
        use base64::Engine as _;
        use std::collections::BTreeMap;
        use wake_common::{Span, fs::OsFileSystem};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-lint-type-program");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source = "import {read} from './b'; /* 😀 */ const result = read();";
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"target":"es2022","module":"esnext","moduleResolution":"bundler","types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), Arc::from(source)),
            (fixture.join("b.ts"), Arc::from("export async function read() { return 42; }")),
        ]);
        let mut fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let backend = fs.backend(&root, "@typescript/native").unwrap();
        let mut service = Session::spawn(
            &backend.executable,
            &root,
            CancellationToken::default(),
            Limits::default(),
        )
        .unwrap();
        let initialized = service
            .request("initialize", Value::Null, |method, path| {
                fs.callback(method, path)
            })
            .unwrap();
        let snapshot = service
            .request(
                "updateSnapshot",
                json!({"openProjects":[config]}),
                |method, path| fs.callback(method, path),
            )
            .unwrap();
        let params =
            json!({"snapshot":snapshot["snapshot"],"project":snapshot["projects"][0]["id"]});
        for method in [
            "getConfigFileParsingDiagnostics",
            "getGlobalDiagnostics",
            "getSemanticDiagnostics",
        ] {
            let diagnostics = service
                .request(method, params.clone(), |method, path| {
                    fs.callback(method, path)
                })
                .unwrap();
            assert!(
                diagnostics.as_array().is_some_and(Vec::is_empty),
                "{method}: {diagnostics}"
            );
        }
        let encoded = service.request("getSourceFile", json!({"snapshot":snapshot["snapshot"],"project":snapshot["projects"][0]["id"],"file":file}), |method,path| fs.callback(method,path)).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded["data"].as_str().unwrap())
            .unwrap();
        let wire = WireSource::parse(
            &bytes,
            source,
            &file,
            initialized["useCaseSensitiveFileNames"].as_bool().unwrap(),
        )
        .unwrap();
        let address = wire
            .address(
                214,
                Span::new(
                    source.rfind("read()").unwrap() as u32,
                    (source.len() - 1) as u32,
                ),
            )
            .unwrap();
        let typed = service.request("getTypeAtLocation", json!({"snapshot":snapshot["snapshot"],"project":snapshot["projects"][0]["id"],"location":address}), |method,path| fs.callback(method,path)).unwrap();
        assert_eq!(service.request("typeToString", json!({"snapshot":snapshot["snapshot"],"project":snapshot["projects"][0]["id"],"type":typed["id"]}), |method,path| fs.callback(method,path)).unwrap(), "Promise<number>");
        assert!(
            fs.observed_paths()
                .iter()
                .any(|path| path.ends_with("lib.es5.d.ts"))
        );
        assert!(
            fs.observed_paths()
                .iter()
                .any(|path| path.ends_with("package.json"))
        );
        service.shutdown();
    }

    #[test]
    #[ignore = "requires the repository's installed @typescript/native alias and matching native platform package"]
    fn installed_pnp_backend_location_starts_the_actual_native_process() {
        use super::super::transport::{Limits, Session};
        use crate::CancellationToken;
        use wake_common::fs::OsFileSystem;
        let root = normalize(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let environment = ResolutionEnvironment::new(Arc::new(OsFileSystem));
        let backend = Backend::locate(&environment, &root, "@typescript/native").unwrap();
        assert!(backend.executable.is_file());
        let mut service = Session::spawn(
            &backend.executable,
            &root,
            CancellationToken::default(),
            Limits::default(),
        )
        .unwrap();
        let initialized = service
            .request("initialize", serde_json::Value::Null, |_, _| unreachable!())
            .unwrap();
        assert_eq!(
            normalize(std::path::Path::new(
                initialized["currentDirectory"].as_str().unwrap()
            )),
            root
        );
        service.shutdown();
        assert!(
            Backend::locate(&environment, &root, "typescript")
                .unwrap_err()
                .message
                .contains("7.0.2")
        );
    }
}
