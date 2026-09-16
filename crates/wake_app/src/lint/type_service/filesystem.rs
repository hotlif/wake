//! Frozen, read-only filesystem callbacks with resolver-owned PnP installation queries.
use super::super::module_snapshot::SnapshotFs;
use crate::{CancellationToken, WakeError};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wake_common::{FileSystem, fs::normalize};
use wake_resolver::{NodeModulesPath, NodeModulesView, ResolutionContext, ResolutionEnvironment};

struct Projection {
    context: ResolutionContext,
    volume: PathBuf,
    prefix: PathBuf,
}

pub(super) struct TypeFileSystem {
    snapshot: Arc<SnapshotFs>,
    environment: ResolutionEnvironment,
    view: NodeModulesView,
    namespace: tempfile::TempDir,
    projections: Vec<Projection>,
}

fn failure(message: impl std::fmt::Display) -> WakeError {
    WakeError::new("WAKE_LINT_ANALYSIS", format!("Type filesystem: {message}"))
}

fn missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory | io::ErrorKind::IsADirectory
    )
}

fn key(path: &Path) -> PathBuf {
    let path = normalize(path).into_os_string();
    #[cfg(windows)]
    let path = {
        let mut path = path;
        path.make_ascii_lowercase();
        path
    };
    path.into()
}

impl TypeFileSystem {
    pub(super) fn new(
        base: Arc<dyn FileSystem>,
        overlays: BTreeMap<PathBuf, Arc<str>>,
        root: &Path,
        cancellation: CancellationToken,
    ) -> Result<Self, WakeError> {
        let snapshot = Arc::new(SnapshotFs::new(base, overlays, cancellation)?);
        let environment = ResolutionEnvironment::new(snapshot.clone());
        let view = environment.node_modules_view(root).map_err(failure)?;
        snapshot.check()?;
        Ok(Self {
            snapshot,
            environment,
            view,
            namespace: tempfile::Builder::new()
                .prefix("wake-lint-types-")
                .tempdir()
                .map_err(failure)?,
            projections: Vec::new(),
        })
    }

    pub(super) fn backend(
        &self,
        root: &Path,
        compiler: &str,
    ) -> Result<super::backend::Backend, WakeError> {
        self.snapshot.check()?;
        let result = super::backend::Backend::locate(&self.environment, root, compiler);
        self.snapshot.check()?;
        result
    }

    pub(super) fn observed_paths(&self) -> Vec<PathBuf> {
        self.snapshot
            .observed_paths()
            .into_iter()
            .map(|path| self.environment.watch_path(&path))
            .collect()
    }

    fn resolve(&self, path: &Path) -> Result<(NodeModulesPath, NodeModulesView), WakeError> {
        let normalized = normalize(path);
        let path_key = key(&normalized);
        for projection in &self.projections {
            if path_key.starts_with(key(&projection.prefix)) {
                let suffix: PathBuf = normalized
                    .components()
                    .skip(projection.prefix.components().count())
                    .collect();
                let actual = projection.volume.join(suffix);
                let view = self.view.in_context(projection.context.clone());
                return Ok((view.resolve(&actual).map_err(failure)?, view));
            }
        }
        let view = self.view.clone();
        if path_key.starts_with(key(self.namespace.path())) {
            return Ok((NodeModulesPath::Missing, view));
        }
        Ok((view.resolve(path).map_err(failure)?, view))
    }

    fn canonical(
        &mut self,
        actual: PathBuf,
        context: ResolutionContext,
    ) -> Result<PathBuf, WakeError> {
        let physical = self
            .environment
            .context_for_issuer(&actual)
            .map_err(failure)?;
        if context == physical {
            return Ok(actual);
        }
        let volume = actual
            .ancestors()
            .last()
            .filter(|path| path.is_absolute())
            .ok_or_else(|| failure("expected an absolute canonical path"))?
            .to_owned();
        let index = match self.projections.iter().position(|projection| {
            projection.context == context && key(&projection.volume) == key(&volume)
        }) {
            Some(index) => index,
            None => {
                if self.projections.len() >= 256 {
                    return Err(failure("authority/volume projection budget exceeded"));
                }
                let index = self.projections.len();
                self.projections.push(Projection {
                    context,
                    volume: volume.clone(),
                    prefix: self.namespace.path().join(index.to_string()),
                });
                index
            }
        };
        Ok(self.projections[index]
            .prefix
            .join(actual.strip_prefix(&volume).map_err(failure)?))
    }

    pub(super) fn callback(&mut self, method: &str, params: &Value) -> Result<Value, WakeError> {
        self.snapshot.check()?;
        let result = self.query(method, params);
        // Boolean PnP/filesystem methods cannot hide latched cancellation, budgets or I/O.
        self.snapshot.check()?;
        result
    }

    fn query(&mut self, method: &str, params: &Value) -> Result<Value, WakeError> {
        if !matches!(
            method,
            "readFile" | "fileExists" | "directoryExists" | "getAccessibleEntries" | "realpath"
        ) {
            return Err(failure("unsupported callback"));
        }
        let text = params
            .as_str()
            .filter(|text| !text.contains('\0'))
            .ok_or_else(|| failure("expected a path string"))?;
        let requested = normalize(Path::new(text));
        if !requested.is_absolute() {
            return Err(failure("callback paths must be absolute"));
        }
        let (resolved, view) = self.resolve(&requested)?;
        let (path, context) = match resolved {
            NodeModulesPath::Missing => {
                return Ok(match method {
                    "readFile" => json!({"content":null}),
                    "getAccessibleEntries" => json!({"files":[],"directories":[]}),
                    "realpath" => json!(requested),
                    _ => json!(false),
                });
            }
            NodeModulesPath::Directory(names) => {
                return Ok(match method {
                    "readFile" => json!({"content":null}),
                    "getAccessibleEntries" => json!({"files":[],"directories":names}),
                    "realpath" => json!(requested),
                    "directoryExists" => json!(true),
                    _ => json!(false),
                });
            }
            NodeModulesPath::Native(path) => {
                let context = view.context_for_issuer(&path).map_err(failure)?;
                (path, context)
            }
            NodeModulesPath::Projected { path, context } => (path, context),
        };
        let fs = self.environment.file_system();
        Ok(match method {
            "readFile" => match fs.read_to_string(&path) {
                Ok(text) => json!({"content":text}),
                Err(error) if missing(&error) => json!({"content":null}),
                Err(error) => return Err(failure(error).at(&path)),
            },
            "fileExists" | "directoryExists" => {
                // PnP's legacy boolean probes suppress ZIP decoding errors. A checked lookup
                // must distinguish a missing entry from an unreadable installation snapshot.
                match fs.canonicalize(&path) {
                    Ok(_) => json!(if method == "fileExists" {
                        fs.is_file(&path)
                    } else {
                        fs.is_dir(&path)
                    }),
                    Err(error) if missing(&error) => json!(false),
                    Err(error) => return Err(failure(error).at(&path)),
                }
            }
            "realpath" => {
                let canonical = match fs.canonicalize(&path) {
                    Ok(canonical) => normalize(&canonical),
                    Err(error) if missing(&error) => path,
                    Err(error) => return Err(failure(error).at(&path)),
                };
                let context = view
                    .in_context(context)
                    .context_for_issuer(&canonical)
                    .map_err(failure)?;
                json!(self.canonical(canonical, context)?)
            }
            "getAccessibleEntries" => {
                let entries = match fs.read_dir(&path) {
                    Ok(entries) => entries,
                    Err(error) if missing(&error) => Vec::new(),
                    Err(error) => return Err(failure(error).at(&path)),
                };
                let mut files = Vec::new();
                let mut directories = Vec::new();
                for child in entries {
                    self.snapshot.check()?;
                    let name = child
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or_else(|| failure("directory entry is not UTF-8"))?;
                    if fs.is_dir(&child) {
                        directories.push(name.to_owned());
                    } else if fs.is_file(&child) {
                        files.push(name.to_owned());
                    }
                }
                files.sort();
                files.dedup();
                directories.sort();
                directories.dedup();
                json!({"files":files,"directories":directories})
            }
            _ => unreachable!(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CancellationToken;
    use serde_json::{Value, json};
    use std::{collections::BTreeMap, path::Path, sync::Arc};
    use wake_common::{MemoryFileSystem, fs::normalize};

    fn query(fs: &mut TypeFileSystem, method: &str, path: &Path) -> Value {
        fs.callback(method, &json!(path)).unwrap()
    }

    fn stored_zip(name: &str, text: &str) -> Vec<u8> {
        let crc = !text.bytes().fold(!0u32, |crc, byte| {
            (0..8).fold(crc ^ byte as u32, |crc, _| {
                (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)))
            })
        });
        let mut bytes = vec![0; 30];
        bytes[0..4].copy_from_slice(&0x0403_4b50u32.to_le_bytes());
        bytes[4..6].copy_from_slice(&20u16.to_le_bytes());
        bytes[14..18].copy_from_slice(&crc.to_le_bytes());
        bytes[18..22].copy_from_slice(&(text.len() as u32).to_le_bytes());
        bytes[22..26].copy_from_slice(&(text.len() as u32).to_le_bytes());
        bytes[26..28].copy_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(text.as_bytes());
        let offset = bytes.len() as u32;
        let mut central = vec![0; 46];
        central[0..4].copy_from_slice(&0x0201_4b50u32.to_le_bytes());
        central[4..6].copy_from_slice(&20u16.to_le_bytes());
        central[6..8].copy_from_slice(&20u16.to_le_bytes());
        central[16..20].copy_from_slice(&crc.to_le_bytes());
        central[20..24].copy_from_slice(&(text.len() as u32).to_le_bytes());
        central[24..28].copy_from_slice(&(text.len() as u32).to_le_bytes());
        central[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(name.as_bytes());
        let mut end = vec![0; 22];
        end[0..4].copy_from_slice(&0x0605_4b50u32.to_le_bytes());
        end[8..10].copy_from_slice(&1u16.to_le_bytes());
        end[10..12].copy_from_slice(&1u16.to_le_bytes());
        end[12..16].copy_from_slice(&(central.len() as u32).to_le_bytes());
        end[16..20].copy_from_slice(&offset.to_le_bytes());
        bytes.extend(central);
        bytes.extend(end);
        bytes
    }

    #[test]
    #[ignore = "requires WAKE_LINT_TYPESCRIPT_EXE pointing to installed native TypeScript 7.0.2"]
    fn native_type_queries_read_virtual_zip_peers_and_watch_the_physical_archive() {
        use super::super::transport::{Limits, Session};
        let executable =
            std::env::var_os("WAKE_LINT_TYPESCRIPT_EXE").expect("explicit native compiler fixture");
        let temporary = tempfile::tempdir().unwrap();
        let root = normalize(temporary.path());
        let disk = Arc::new(MemoryFileSystem::new());
        let location =
            |peer| format!("./.yarn/__virtual__/pkg-{peer}/0/cache/pkg.zip/node_modules/pkg/");
        disk.insert(
            root.join(".pnp.cjs"),
            "module.exports = require('./.pnp.data.json');",
        );
        disk.insert(root.join(".pnp.data.json"), json!({"enableTopLevelFallback":false,"packageRegistryData":[
            [null,[[null,{"packageLocation":"./","packageDependencies":[["first",["pkg","virtual:first"]],["second",["pkg","virtual:second"]]]}]]],
            ["pkg",[
                ["virtual:first",{"packageLocation":location("first"),"packageDependencies":[["peer","npm:1"]]}],
                ["virtual:second",{"packageLocation":location("second"),"packageDependencies":[["peer","npm:2"]]}]
            ]],
            ["peer",[
                ["npm:1",{"packageLocation":"./peers/one/","packageDependencies":[]}],
                ["npm:2",{"packageLocation":"./peers/two/","packageDependencies":[]}]
            ]]
        ]}).to_string());
        let archive = root.join(".yarn/cache/pkg.zip");
        disk.insert(
            &archive,
            stored_zip(
                "node_modules/pkg/index.d.ts",
                "export { value } from 'peer';",
            ),
        );
        disk.insert(
            root.join("peers/one/index.d.ts"),
            "export declare const value: number;",
        );
        disk.insert(
            root.join("peers/two/index.d.ts"),
            "export declare const value: string;",
        );
        disk.insert(root.join("tsconfig.json"), json!({"compilerOptions":{"strict":true,"noLib":true,"module":"preserve","moduleResolution":"bundler"},"files":["a.ts"]}).to_string());
        let source = "import {value as first} from 'first'; import {value as second} from 'second'; first; second;";
        disk.insert(root.join("a.ts"), source);
        let mut fs =
            TypeFileSystem::new(disk, BTreeMap::new(), &root, CancellationToken::default())
                .unwrap();
        let mut service = Session::spawn(
            Path::new(&executable),
            &root,
            CancellationToken::default(),
            Limits::default(),
        )
        .unwrap();
        service
            .request("initialize", Value::Null, |method, path| {
                fs.callback(method, path)
            })
            .unwrap();
        let snapshot = service
            .request(
                "updateSnapshot",
                json!({"openProjects":[root.join("tsconfig.json")]}),
                |method, path| fs.callback(method, path),
            )
            .unwrap();
        for (name, expected) in [("first", "number"), ("second", "string")] {
            let typed = service.request("getTypeAtPosition", json!({"snapshot":snapshot["snapshot"],"project":snapshot["projects"][0]["id"],"file":root.join("a.ts"),"position":source.rfind(name).unwrap()}), |method,path| fs.callback(method,path)).unwrap();
            let kind = service.request("typeToString", json!({"snapshot":snapshot["snapshot"],"project":snapshot["projects"][0]["id"],"type":typed["id"]}), |method,path| fs.callback(method,path)).unwrap();
            assert_eq!(kind, expected);
            let logical = normalize(&root.join(location(name)).join("index.d.ts"));
            assert_eq!(
                query(
                    &mut fs,
                    "realpath",
                    &root.join(format!("node_modules/{name}/index.d.ts"))
                ),
                json!(logical)
            );
            assert_eq!(
                query(&mut fs, "getAccessibleEntries", logical.parent().unwrap()),
                json!({"files":["index.d.ts"],"directories":[]})
            );
        }
        assert!(fs.observed_paths().contains(&archive));
        // Discovery also retains negative physical .pnp.cjs witnesses above the virtual
        // projection boundary. Only archive members must collapse to the archive itself.
        assert!(
            !fs.observed_paths()
                .iter()
                .any(|path| path.to_string_lossy().contains("pkg.zip") && path != &archive)
        );
        service.shutdown();
    }

    #[test]
    fn corrupted_archives_and_invalid_source_bytes_cannot_be_reported_as_absent() {
        let temporary = tempfile::tempdir().unwrap();
        let root = normalize(temporary.path());
        let disk = Arc::new(MemoryFileSystem::new());
        disk.insert(root.join("bad.zip"), b"broken archive".as_slice());
        disk.insert(root.join("bad.ts"), vec![255, 254, 0]);
        for method in [
            "fileExists",
            "directoryExists",
            "readFile",
            "getAccessibleEntries",
            "realpath",
        ] {
            let mut fs = TypeFileSystem::new(
                disk.clone(),
                BTreeMap::new(),
                &root,
                CancellationToken::default(),
            )
            .unwrap();
            assert!(
                fs.callback(
                    method,
                    &json!(root.join("bad.zip/node_modules/pkg/index.d.ts"))
                )
                .is_err(),
                "{method} hid the archive failure"
            );
        }
        let mut fs =
            TypeFileSystem::new(disk, BTreeMap::new(), &root, CancellationToken::default())
                .unwrap();
        assert!(
            fs.callback("readFile", &json!(root.join("bad.ts")))
                .is_err()
        );
    }

    #[test]
    #[ignore = "requires WAKE_LINT_TYPESCRIPT_EXE pointing to installed native TypeScript 7.0.2"]
    fn native_type_queries_keep_shared_external_packages_separate_between_projects() {
        use super::super::transport::{Limits, Session};
        let executable =
            std::env::var_os("WAKE_LINT_TYPESCRIPT_EXE").expect("explicit native compiler fixture");
        let temporary = tempfile::tempdir().unwrap();
        let base = normalize(temporary.path());
        let one = base.join("one");
        let two = base.join("two");
        let disk = Arc::new(MemoryFileSystem::new());
        let source = "import { value } from 'alias'; import { phantom } from 'undeclared'; value;";
        for (root, peer, kind) in [(&one, "peer-one", "number"), (&two, "peer-two", "string")] {
            disk.insert(
                root.join(".pnp.cjs"),
                "module.exports = require('./.pnp.data.json');",
            );
            disk.insert(root.join(".pnp.data.json"), json!({"enableTopLevelFallback":false,"packageRegistryData":[
                [null,[[null,{"packageLocation":"./","packageDependencies":[["alias",["pkg","npm:1"]]]}]]],
                ["pkg",[["npm:1",{"packageLocation":"../cache/pkg/","packageDependencies":[["peer","npm:1"]]}]]],
                ["peer",[["npm:1",{"packageLocation":format!("../cache/{peer}/"),"packageDependencies":[]}]]]
            ]}).to_string());
            disk.insert(root.join("tsconfig.json"), json!({"compilerOptions":{"strict":true,"noLib":true,"module":"preserve","moduleResolution":"bundler"},"files":["a.ts"]}).to_string());
            disk.insert(root.join("a.ts"), source);
            disk.insert(
                base.join(format!("cache/{peer}/index.d.ts")),
                format!("export declare const value: {kind};"),
            );
        }
        disk.insert(
            base.join("cache/pkg/index.d.ts"),
            "export { value } from 'peer';",
        );
        disk.insert(
            base.join("node_modules/undeclared/index.d.ts"),
            "export declare const phantom: boolean;",
        );
        let mut fs =
            TypeFileSystem::new(disk, BTreeMap::new(), &one, CancellationToken::default()).unwrap();
        let mut service = Session::spawn(
            Path::new(&executable),
            &base,
            CancellationToken::default(),
            Limits::default(),
        )
        .unwrap();
        service
            .request("initialize", Value::Null, |method, path| {
                fs.callback(method, path)
            })
            .unwrap();
        let snapshot = service
            .request(
                "updateSnapshot",
                json!({"openProjects":[one.join("tsconfig.json"),two.join("tsconfig.json")]}),
                |method, path| fs.callback(method, path),
            )
            .unwrap();
        assert_eq!(
            snapshot["projects"].as_array().unwrap().len(),
            2,
            "{snapshot}"
        );
        for (root, expected) in [(&one, "number"), (&two, "string")] {
            let project = snapshot["projects"]
                .as_array()
                .unwrap()
                .iter()
                .find(|project| {
                    key(Path::new(project["configFileName"].as_str().unwrap()))
                        == key(&root.join("tsconfig.json"))
                })
                .unwrap();
            let params = json!({"snapshot":snapshot["snapshot"],"project":project["id"],"file":root.join("a.ts"),"position":source.rfind("value").unwrap()});
            let typed = service
                .request("getTypeAtPosition", params.clone(), |method, path| {
                    fs.callback(method, path)
                })
                .unwrap();
            let kind = service.request("typeToString", json!({"snapshot":snapshot["snapshot"],"project":project["id"],"type":typed["id"]}), |method,path| fs.callback(method,path)).unwrap();
            assert_eq!(kind, expected, "{project}");
            let diagnostics = service
                .request("getSemanticDiagnostics", params, |method, path| {
                    fs.callback(method, path)
                })
                .unwrap();
            let missing: Vec<_> = diagnostics
                .as_array()
                .unwrap()
                .iter()
                .filter(|diagnostic| diagnostic["code"] == 2307)
                .collect();
            assert_eq!(missing.len(), 1, "{diagnostics}");
            assert!(
                missing[0].to_string().contains("undeclared"),
                "{diagnostics}"
            );
        }
        service.shutdown();
    }

    #[test]
    fn callbacks_freeze_overlays_and_keep_pnp_authority_and_relative_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let base = normalize(temporary.path());
        let one = base.join("one");
        let two = base.join("two");
        let cache = base.join("cache");
        let disk = Arc::new(MemoryFileSystem::new());
        for (root, peer) in [(&one, "peer-one"), (&two, "peer-two")] {
            disk.insert(
                root.join(".pnp.cjs"),
                "module.exports = require('./.pnp.data.json');",
            );
            disk.insert(root.join(".pnp.data.json"), json!({
                "enableTopLevelFallback":false,
                "packageRegistryData":[
                    [null,[[null,{"packageLocation":"./","packageDependencies":[["alias",["pkg","npm:1"]]]}]]],
                    ["pkg",[["npm:1",{"packageLocation":"../cache/pkg/","packageDependencies":[["peer","npm:1"]]}]]],
                    ["peer",[["npm:1",{"packageLocation":format!("../cache/{peer}/"),"packageDependencies":[]}]]]
                ]
            }).to_string());
        }
        disk.insert(
            cache.join("pkg/index.d.ts"),
            "export { value } from 'peer';",
        );
        disk.insert(
            cache.join("peer-one/index.d.ts"),
            "export declare const value: number;",
        );
        disk.insert(
            cache.join("peer-two/index.d.ts"),
            "export declare const value: string;",
        );
        disk.insert(cache.join("shared.d.ts"), "sibling");
        disk.insert(one.join("node_modules/undeclared/index.d.ts"), "shadow");
        disk.insert(
            base.join("node_modules/undeclared/index.d.ts"),
            "ancestor shadow",
        );
        disk.insert(
            cache.join("node_modules/undeclared/index.d.ts"),
            "cache shadow",
        );
        disk.insert(one.join("a.ts"), "disk");
        let cancellation = CancellationToken::default();
        let mut fs = TypeFileSystem::new(
            disk.clone(),
            BTreeMap::from([
                (one.join("a.ts"), Arc::from("overlay")),
                (one.join("new/virtual.ts"), Arc::from("virtual")),
            ]),
            &one,
            cancellation.clone(),
        )
        .unwrap();
        assert_eq!(
            query(&mut fs, "readFile", &one.join("a.ts")),
            json!({"content":"overlay"})
        );
        assert_eq!(query(&mut fs, "directoryExists", &one.join("new")), true);
        assert_eq!(
            query(&mut fs, "getAccessibleEntries", &one.join("new")),
            json!({"files":["virtual.ts"],"directories":[]})
        );
        assert_eq!(
            query(&mut fs, "getAccessibleEntries", &one.join("node_modules")),
            json!({"files":[],"directories":["alias"]})
        );
        assert_eq!(
            query(
                &mut fs,
                "readFile",
                &one.join("node_modules/undeclared/index.d.ts")
            ),
            json!({"content":null})
        );
        assert_eq!(
            query(
                &mut fs,
                "fileExists",
                &one.join("node_modules/undeclared/index.d.ts")
            ),
            false
        );
        assert_eq!(
            query(
                &mut fs,
                "fileExists",
                &base.join("node_modules/undeclared/index.d.ts")
            ),
            false
        );
        let first = query(
            &mut fs,
            "realpath",
            &one.join("node_modules/alias/index.d.ts"),
        );
        let second = query(
            &mut fs,
            "realpath",
            &two.join("node_modules/alias/index.d.ts"),
        );
        assert_eq!(
            query(&mut fs, "realpath", &one.join("node_modules/alias")),
            json!(Path::new(first.as_str().unwrap()).parent().unwrap())
        );
        assert_eq!(
            query(&mut fs, "realpath", &two.join("node_modules/alias")),
            json!(Path::new(second.as_str().unwrap()).parent().unwrap())
        );
        assert_ne!(
            first, second,
            "the same package under distinct PnP authorities cannot merge"
        );
        for (canonical, peer) in [(&first, "number"), (&second, "string")] {
            let canonical = Path::new(canonical.as_str().unwrap());
            assert_eq!(
                query(&mut fs, "readFile", canonical),
                json!({"content":"export { value } from 'peer';"})
            );
            assert_eq!(query(&mut fs, "realpath", canonical), json!(canonical));
            let directory = canonical.parent().unwrap();
            assert_eq!(query(&mut fs, "realpath", directory), json!(directory));
            assert_eq!(
                query(
                    &mut fs,
                    "fileExists",
                    &directory.join("../node_modules/undeclared/index.d.ts")
                ),
                false
            );
            assert_eq!(
                query(&mut fs, "readFile", &directory.join("../shared.d.ts")),
                json!({"content":"sibling"})
            );
            assert_eq!(
                query(
                    &mut fs,
                    "readFile",
                    &directory.join("node_modules/peer/index.d.ts")
                ),
                json!({"content":format!("export declare const value: {peer};")})
            );
        }
        disk.insert(cache.join("pkg/index.d.ts"), "changed after snapshot");
        assert_eq!(
            query(&mut fs, "readFile", Path::new(first.as_str().unwrap())),
            json!({"content":"export { value } from 'peer';"})
        );
        let observed = fs.observed_paths();
        assert!(observed.contains(&cache.join("pkg/index.d.ts")));
        assert!(!observed.contains(&normalize(Path::new(first.as_str().unwrap()))));
        assert!(fs.callback("writeFile", &json!(one.join("a.ts"))).is_err());
        assert!(fs.callback("fileExists", &json!("relative.ts")).is_err());
        cancellation.cancel();
        assert_eq!(
            fs.callback("fileExists", &json!(one.join("a.ts")))
                .unwrap_err()
                .code,
            "WAKE_CANCELLED"
        );
    }
}
