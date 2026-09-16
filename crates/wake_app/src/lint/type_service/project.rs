//! One native compiler snapshot, explicit program membership and source-bound queries.
use super::{
    filesystem::TypeFileSystem,
    transport::{Limits, Session},
    wire::WireSource,
};
use crate::{CancellationToken, WakeError};
use base64::Engine as _;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use wake_common::fs::normalize;

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug)]
pub(super) struct ProjectId {
    session: u64,
    index: usize,
}

struct Project {
    id: String,
    config: PathBuf,
    files: BTreeMap<PathBuf, PathBuf>,
}

pub(super) struct TypeProject {
    process: Session,
    filesystem: TypeFileSystem,
    snapshot: u64,
    identity: u64,
    case_sensitive: bool,
    projects: Vec<Project>,
}

fn failure(message: impl std::fmt::Display) -> WakeError {
    WakeError::new("WAKE_LINT_ANALYSIS", format!("Type project: {message}"))
}

fn key(path: &Path, case_sensitive: bool) -> PathBuf {
    let mut value = normalize(path).into_os_string();
    if !case_sensitive {
        value.make_ascii_lowercase();
    }
    value.into()
}

fn absolute(value: &Value) -> Result<PathBuf, WakeError> {
    let path = value
        .as_str()
        .filter(|path| !path.contains('\0'))
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| failure("invalid absolute path in compiler response"))?;
    Ok(normalize(path))
}

impl TypeProject {
    pub(super) fn observed_paths(&self) -> Vec<PathBuf> {
        self.filesystem.observed_paths()
    }

    pub(super) fn start(
        mut filesystem: TypeFileSystem,
        root: &Path,
        compiler: &str,
        configurations: &[PathBuf],
        cancellation: CancellationToken,
    ) -> Result<Self, WakeError> {
        let root = normalize(root);
        let root = root.as_path();
        let configurations: Vec<_> = configurations.iter().map(|path| normalize(path)).collect();
        if configurations.is_empty() || configurations.len() > 64 {
            return Err(failure("expected 1–64 explicit tsconfig projects"));
        }
        for config in &configurations {
            if filesystem.callback("fileExists", &json!(config))? != true {
                return Err(failure("configuration does not exist").at(config));
            }
        }
        let backend = filesystem.backend(root, compiler)?;
        let mut process =
            Session::spawn(&backend.executable, root, cancellation, Limits::default())?;
        let initialized = process.request("initialize", Value::Null, |method, path| {
            filesystem.callback(method, path)
        })?;
        let case_sensitive = initialized["useCaseSensitiveFileNames"]
            .as_bool()
            .ok_or_else(|| failure("invalid initialize response"))?;
        if key(&absolute(&initialized["currentDirectory"])?, case_sensitive)
            != key(root, case_sensitive)
        {
            return Err(failure("compiler initialized in a different directory"));
        }
        let expected: BTreeSet<_> = configurations
            .iter()
            .map(|path| key(path, case_sensitive))
            .collect();
        if expected.len() != configurations.len() {
            return Err(failure("duplicate configuration paths"));
        }
        let response = process.request(
            "updateSnapshot",
            json!({"openProjects":configurations}),
            |method, path| filesystem.callback(method, path),
        )?;
        let snapshot = response["snapshot"]
            .as_u64()
            .ok_or_else(|| failure("invalid snapshot handle"))?;
        let entries = response["projects"]
            .as_array()
            .filter(|projects| !projects.is_empty() && projects.len() <= 64)
            .ok_or_else(|| failure("invalid or oversized project list"))?;
        let mut projects = Vec::new();
        let mut seen = BTreeSet::new();
        let mut files = 0usize;
        for entry in entries {
            let config = absolute(&entry["configFileName"])?;
            let id = absolute(&entry["id"])?;
            let config_key = key(&config, case_sensitive);
            if key(&id, case_sensitive) != config_key
                || !seen.insert(config_key)
                || !entry["compilerOptions"].is_object()
            {
                return Err(failure("invalid or duplicate project identity"));
            }
            let roots = entry["rootFiles"]
                .as_array()
                .filter(|paths| paths.len() <= 100_000)
                .ok_or_else(|| failure("invalid project root list"))?;
            for path in roots {
                absolute(path)?;
            }
            let id = entry["id"].as_str().expect("validated path").to_owned();
            for (method, label) in [
                ("getConfigFileParsingDiagnostics", "configuration"),
                ("getGlobalDiagnostics", "global environment"),
            ] {
                let diagnostics = process.request(
                    method,
                    json!({"snapshot":snapshot,"project":id}),
                    |method, path| filesystem.callback(method, path),
                )?;
                let diagnostics = diagnostics
                    .as_array()
                    .ok_or_else(|| failure("invalid diagnostics response"))?;
                for diagnostic in diagnostics {
                    let category = diagnostic["category"]
                        .as_u64()
                        .filter(|value| *value <= 3)
                        .ok_or_else(|| failure("invalid diagnostic category"))?;
                    if category == 1 {
                        let description: String =
                            diagnostic.to_string().chars().take(2048).collect();
                        return Err(failure(format!("{label} error: {description}")).at(&config));
                    }
                }
            }
            let names = process.request(
                "getSourceFileNames",
                json!({"snapshot":snapshot,"project":id}),
                |method, path| filesystem.callback(method, path),
            )?;
            let names = names
                .as_array()
                .ok_or_else(|| failure("invalid program file list"))?;
            files = files
                .checked_add(names.len())
                .filter(|count| *count <= 100_000)
                .ok_or_else(|| failure("program file identity budget exceeded"))?;
            let names = names
                .iter()
                .map(|value| absolute(value).map(|path| (key(&path, case_sensitive), path)))
                .collect::<Result<BTreeMap<_, _>, _>>()?;
            projects.push(Project {
                id,
                config,
                files: names,
            });
        }
        if !expected.is_subset(&seen) {
            return Err(failure("compiler omitted a requested configuration"));
        }
        let identity = NEXT_SESSION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| failure("session identity space exhausted"))?;
        Ok(Self {
            process,
            filesystem,
            snapshot,
            identity,
            case_sensitive,
            projects,
        })
    }

    pub(super) fn project_for(&self, file: &Path) -> Result<ProjectId, WakeError> {
        let file = key(file, self.case_sensitive);
        let mut candidates: Vec<_> = self
            .projects
            .iter()
            .enumerate()
            .filter(|(_, project)| project.files.contains_key(&file))
            .map(|(index, project)| {
                let directory = key(
                    project.config.parent().expect("absolute config"),
                    self.case_sensitive,
                );
                let depth = if file.starts_with(&directory) {
                    directory.components().count()
                } else {
                    0
                };
                (depth, index)
            })
            .collect();
        candidates.sort_unstable_by(|a, b| b.cmp(a));
        let Some(&(depth, index)) = candidates.first() else {
            return Err(failure("selected source belongs to no configured program").at(&file));
        };
        if candidates.get(1).is_some_and(|next| next.0 == depth) {
            return Err(failure("ambiguous program membership; select one tsconfig").at(&file));
        }
        Ok(ProjectId {
            session: self.identity,
            index,
        })
    }

    fn selected(&self, project: ProjectId) -> Result<&Project, WakeError> {
        if project.session != self.identity {
            return Err(failure("project belongs to a different type session"));
        }
        self.projects
            .get(project.index)
            .ok_or_else(|| failure("invalid project handle"))
    }

    pub(super) fn request(
        &mut self,
        project: ProjectId,
        method: &str,
        mut params: Value,
    ) -> Result<Value, WakeError> {
        let id = self.selected(project)?.id.clone();
        let fields = params
            .as_object_mut()
            .ok_or_else(|| failure("query parameters must be an object"))?;
        if fields.contains_key("snapshot") || fields.contains_key("project") {
            return Err(failure("query cannot replace its snapshot or project"));
        }
        fields.insert("snapshot".into(), json!(self.snapshot));
        fields.insert("project".into(), json!(id));
        self.process.request(method, params, |method, path| {
            self.filesystem.callback(method, path)
        })
    }

    pub(super) fn source<'source>(
        &mut self,
        project: ProjectId,
        file: &Path,
        source: &'source str,
    ) -> Result<WireSource<'source>, WakeError> {
        let result = (|| {
            if !self
                .selected(project)?
                .files
                .contains_key(&key(file, self.case_sensitive))
            {
                return Err(failure("source is outside the selected program").at(file));
            }
            let encoded =
                self.request(project, "getSourceFile", json!({"file":normalize(file)}))?;
            let encoded = encoded["data"]
                .as_str()
                .ok_or_else(|| failure("compiler omitted the selected source"))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(failure)?;
            WireSource::parse(&bytes, source, file, self.case_sensitive)
        })();
        if result.is_err() {
            self.process.shutdown();
        }
        result
    }

    pub(super) fn property_permissions(
        &mut self,
        project: ProjectId,
        file: &Path,
    ) -> Result<std::collections::BTreeMap<u32, (u32, bool)>, WakeError> {
        // Node handles use canonical case; the program's original spelling owns sparse overlays.
        let file = self
            .selected(project)?
            .files
            .get(&key(file, self.case_sensitive))
            .cloned()
            .ok_or_else(|| failure("declaration is outside the selected program").at(file))?;
        let text = self.filesystem.callback("readFile", &json!(file))?;
        let text = text["content"]
            .as_str()
            .ok_or_else(|| failure("property declaration source is unavailable").at(&file))?;
        // `source` validates membership and exact backend text before exposing any address facts.
        Ok(self.source(project, &file, text)?.property_permissions())
    }
}

#[cfg(test)]
mod tests {
    use super::super::filesystem::TypeFileSystem;
    use super::*;
    use crate::CancellationToken;
    use serde_json::json;
    use std::{collections::BTreeMap, path::Path, sync::Arc};
    use wake_common::{
        Span,
        fs::{OsFileSystem, normalize},
    };

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_member_queries_use_original_receiver_and_key_types_after_erasure() {
        use wake_lint_core::{
            LintOptions, RuleConfiguration, RuleLevel, RuleSetting, SourceType, TypeSource,
        };
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-member-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            r#"
export {};
declare const loose:any, safe:{field:string}, key:any;
loose.field.nested; loose?.field; safe[key as any];
(loose as {field:string}).field; (loose as any)!.field;
class C { #x=1; method(value:any){ value.#x; } }
new loose.Ctor(); loose[missingKey];
"#,
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"target":"es2022","types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let input = TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap();
        assert!(input.is_complete(), "{:?}", input.parse_diagnostics());
        let typed = project.type_source(&file, input).unwrap();
        let options = |allow_optional| LintOptions {
            recommended: false,
            rules: [(
                "ts/no-unsafe-member-access".into(),
                RuleSetting::Options(RuleConfiguration {
                    level: RuleLevel::Error,
                    options: [("allow_optional".into(), json!(allow_optional))].into(),
                }),
            )]
            .into(),
            ..Default::default()
        };
        let result = typed.lint(&options(false)).unwrap();
        let actual: Vec<_> = result
            .diagnostics
            .iter()
            .map(|d| {
                (
                    &source[d.start as usize..d.end as usize],
                    d.message_id.as_str(),
                )
            })
            .collect();
        assert_eq!(
            actual,
            [
                ("field", "unsafeMember"),
                ("nested", "unsafeMember"),
                ("field", "unsafeMember"),
                ("key as any", "unsafeKey"),
                ("field", "unsafeMember"),
                ("#x", "unsafeMember"),
                ("Ctor", "unsafeMember"),
                ("missingKey", "errorKey"),
                ("missingKey", "unsafeMember")
            ]
        );
        assert_eq!(typed.lint(&options(true)).unwrap().diagnostics.len(), 8);
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_template_types_preserve_unions_constraints_brands_and_standard_regexp_identity() {
        use wake_lint_core::{
            LintOptions, RuleConfiguration, RuleLevel, RuleSetting, SourceType, TypeSource,
        };
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-template-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            r#"
export {};
declare const object: {}, symbol:symbol, unknown:unknown, numbers:number[], union:string|{}, branded:string & {brand:true}, regexp:RegExp;
declare const tag:(strings:TemplateStringsArray, value:unknown)=>string;
`${object}${symbol}${unknown}${numbers}${union}${branded}${regexp}`;
tag`${object}`;
`${42}${true}${null}${undefined}${1n}${'text'}${/pattern/}`;
declare const any:any, never:never, voidValue:void;
`${any}${never}${voidValue}`;
function constrained<T extends string>(value:T) { return `${value}`; }
function loose<T>(looseValue:T) { return `${looseValue}`; }
declare const mapped:Uppercase<string>, template:`name${string}`;
`${mapped}${template}`;
class Regex<T> extends RegExp {}
`${new Regex<number>('x')}`;
{ class RegExp { private tag=0 }; const local = new RegExp(); `${local}`; }
"#,
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"target":"es2022","lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let input = TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap();
        assert!(input.is_complete(), "{:?}", input.parse_diagnostics());
        let typed = project.type_source(&file, input).unwrap();
        let options = |values| LintOptions {
            recommended: false,
            rules: [(
                "ts/restrict-template-expressions".into(),
                RuleSetting::Options(RuleConfiguration {
                    level: RuleLevel::Error,
                    options: serde_json::from_value(values).unwrap(),
                }),
            )]
            .into(),
            ..Default::default()
        };
        let ranges = |values| {
            typed
                .lint(&options(values))
                .unwrap()
                .diagnostics
                .iter()
                .map(|d| source[d.start as usize..d.end as usize].to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ranges(json!({})),
            [
                "object",
                "symbol",
                "unknown",
                "numbers",
                "union",
                "regexp",
                "/pattern/",
                "never",
                "voidValue",
                "looseValue",
                "new Regex<number>('x')",
                "local"
            ]
        );
        assert_eq!(
            ranges(
                json!({"allow_regexp":true,"allow_never":true,"allow_any":false,"allow_boolean":false,"allow_nullish":false,"allow_number":false})
            ),
            [
                "object",
                "symbol",
                "unknown",
                "numbers",
                "union",
                "42",
                "true",
                "null",
                "undefined",
                "1n",
                "any",
                "voidValue",
                "looseValue",
                "local"
            ]
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_call_facts_drive_core_rules_by_types_and_standard_symbol_identity() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-call-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            r#"
export {};
declare const loose: any, fn: Function;
loose(); loose?.(); new loose(); loose`tag`; missing();
fn(); new fn(); fn`tag`;
interface Derived extends Function {}
interface VoidCall extends Function {():void}
interface ValueCall extends Function {():number}
interface Construct extends Function {new():object}
declare const derived:Derived, voidCall:VoidCall, valueCall:ValueCall, construct:Construct;
derived(); new derived(); new voidCall(); voidCall();
valueCall(); new valueCall(); construct(); new construct();
function constrained<T extends Function>(value:T) { value(); }
function callable<T extends ()=>void>(value:T) { value(); }
interface Generic<T> extends Function { label:T }
declare const generic:Generic<string>; generic();
{ interface Function { label:string }; let local!:Function; local(); new local(); }
declare const both: Function | Derived, mixed: Function | {label:string}, part: Function & {label:string};
both(); mixed(); part();
import('not-installed');
// wake-lint-disable-next-line ts/no-unsafe-call
loose();
"#,
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let input = TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap();
        assert!(input.is_complete(), "{:?}", input.parse_diagnostics());
        let typed = project.type_source(&file, input).unwrap();
        let options = LintOptions {
            recommended: false,
            rules: [("ts/no-unsafe-call".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        };
        let result = typed.lint(&options).unwrap();
        assert!(result.parse_diagnostics.is_empty());
        let actual: Vec<_> = result
            .diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    &source[diagnostic.start as usize..diagnostic.end as usize],
                    diagnostic.message_id.as_str(),
                )
            })
            .collect();
        assert_eq!(
            actual,
            [
                ("loose", "unsafeCall"),
                ("loose?.", "unsafeCall"),
                ("new loose()", "unsafeNew"),
                ("loose", "unsafeTag"),
                ("missing", "errorCall"),
                ("fn", "unsafeCall"),
                ("new fn()", "unsafeNew"),
                ("fn", "unsafeTag"),
                ("derived", "unsafeCall"),
                ("new derived()", "unsafeNew"),
                ("new voidCall()", "unsafeNew"),
                ("value", "unsafeCall"),
                ("generic", "unsafeCall"),
                ("both", "unsafeCall"),
                ("part", "unsafeCall"),
            ]
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_await_facts_distinguish_standard_promises_from_non_thenables() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-await-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; async function run(value: number, promise: Promise<number>, thenable: PromiseLike<number>) { await value; await promise; await thenable; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let input = TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap();
        assert_eq!(input.awaits().len(), 3);
        let typed = project.type_source(&file, input).unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/await-thenable".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message_id, "notThenable");
        assert_eq!(
            &source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize],
            "value"
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assertion_facts_compare_structural_original_and_asserted_types() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-assertion-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> =
            Arc::from("export {}; const value = 'a' as string; const exact = 'a' as 'a';");
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/no-unnecessary-type-assertion".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message_id, "unnecessary");
        assert_eq!(
            &source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize],
            "'a' as 'a'"
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assertion_facts_compare_same_generic_reference_instances() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-generic-assertion-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; type Box<T> = { value: T }; declare const value: Box<string>; const exact = value as Box<string>;",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/no-unnecessary-type-assertion".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message_id, "unnecessary");
        assert_eq!(
            &source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize],
            "value as Box<string>"
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assertion_facts_compare_generic_class_reference_instances_with_bases() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-class-generic-assertion-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; class Box<T> extends Array<T> { value!: T }; declare const value: Box<string>; const exact = value as Box<string>;",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/no-unnecessary-type-assertion".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message_id, "unnecessary");
        assert_eq!(
            &source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize],
            "value as Box<string>"
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_call_return_facts_report_only_floating_promise_statements() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-floating-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; declare function task(): Promise<number>; declare function thenableTask(): PromiseLike<number>; function run() { task(); thenableTask(); import('./chunk'); const value = task(); void task(); }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-floating-promises".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 3);
        assert!(
            result
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.message_id == "floating")
        );
        assert_eq!(
            &source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize],
            "task()"
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_expression_statement_type_facts_report_bare_promise_values() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-floating-expression-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; declare const pending: Promise<number>; declare const holder: { pending: Promise<number> }; async function run() { pending; holder.pending; await pending; void pending; await holder.pending; void holder.pending; } (() => { pending; (pending as Promise<number>); })();",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-floating-promises".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 4);
        assert_eq!(
            &source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize],
            "pending"
        );
        assert_eq!(
            &source[result.diagnostics[1].start as usize..result.diagnostics[1].end as usize],
            "holder.pending"
        );
        assert_eq!(
            &source[result.diagnostics[2].start as usize..result.diagnostics[2].end as usize],
            "pending"
        );
        assert_eq!(
            &source[result.diagnostics[3].start as usize..result.diagnostics[3].end as usize],
            "(pending as Promise<number>)"
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assignment_value_facts_report_any_initializers_and_assignments() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-assignment-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; declare let input:any; let first = input; first = input; const safe = 1;",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-assignment".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 2);
        assert!(result.diagnostics.iter().all(|diagnostic| {
            &source[diagnostic.start as usize..diagnostic.end as usize] == "input"
        }));
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_cross_file_assignment_facts_follow_imported_any_and_structural_properties() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-cross-file-assignment-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let dependency = fixture.join("b.ts");
        let source: Arc<str> = Arc::from(
            "import { payload, holder } from './b'; const target: unknown = payload; function read(): unknown { return holder.value; }",
        );
        let dependency_source: Arc<str> = Arc::from(
            "export const payload: any = 1; export const holder: { value: any } = { value: 1 };",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({
                "compilerOptions":{"strict":true,"target":"es2022","module":"node16","moduleResolution":"node16","lib":["es2022"],"types":[],"skipLibCheck":true},
                "files":["a.ts","b.ts"]
            }).to_string())),
            (file.clone(), source.clone()),
            (dependency.clone(), dependency_source),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [
                    ("ts/no-unsafe-assignment".into(), RuleLevel::Error.into()),
                    ("ts/no-unsafe-return".into(), RuleLevel::Error.into()),
                ]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 2);
        assert!(result.diagnostics.iter().all(|diagnostic| {
            matches!(
                &source[diagnostic.start as usize..diagnostic.end as usize],
                "payload" | "holder.value"
            )
        }));
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_nested_type_argument_any_is_unsafe_for_assignment_and_return() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-nested-unsafe-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; declare const unsafe: Promise<any>; const target: Promise<unknown> = unsafe; function read(): Promise<unknown> { return unsafe; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let assignment = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-assignment".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(assignment.diagnostics.len(), 1);
        let returned = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-return".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(returned.diagnostics.len(), 1);
        assert!(assignment.diagnostics.iter().all(|diagnostic| {
            let text = &source[diagnostic.start as usize..diagnostic.end as usize];
            matches!(text, "unsafe" | "broken")
        }));
        assert!(returned.diagnostics.iter().all(|diagnostic| {
            let text = &source[diagnostic.start as usize..diagnostic.end as usize];
            matches!(text, "unsafe" | "broken")
        }));
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_structural_property_any_is_unsafe_for_assignment_and_return() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-structural-unsafe-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; declare const unsafe: { value: any }; const target: { value: string } = unsafe; function read(): { value: string } { return unsafe; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let assignment = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-assignment".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(assignment.diagnostics.len(), 1);
        let returned = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-return".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(returned.diagnostics.len(), 1);
        assert!(assignment.diagnostics.iter().all(|diagnostic| {
            &source[diagnostic.start as usize..diagnostic.end as usize] == "unsafe"
        }));
        assert!(returned.diagnostics.iter().all(|diagnostic| {
            &source[diagnostic.start as usize..diagnostic.end as usize] == "unsafe"
        }));
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_interface_property_any_is_unsafe_for_assignment_and_return() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-interface-unsafe-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; interface Unsafe { value: any } declare const unsafe: Unsafe; const target: { value: string } = unsafe; function read(): { value: string } { return unsafe; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let assignment = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-assignment".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(assignment.diagnostics.len(), 1);
        let returned = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-return".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(returned.diagnostics.len(), 1);
        assert!(assignment.diagnostics.iter().all(|diagnostic| {
            &source[diagnostic.start as usize..diagnostic.end as usize] == "unsafe"
        }));
        assert!(returned.diagnostics.iter().all(|diagnostic| {
            &source[diagnostic.start as usize..diagnostic.end as usize] == "unsafe"
        }));
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_class_property_any_is_unsafe_for_assignment_and_return() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-class-unsafe-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; class Unsafe { value: any = 1 } declare const unsafe: Unsafe; const target: { value: string } = unsafe; function read(): { value: string } { return unsafe; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let assignment = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-assignment".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(assignment.diagnostics.len(), 1);
        let returned = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-return".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(returned.diagnostics.len(), 1);
        assert!(assignment.diagnostics.iter().all(|diagnostic| {
            &source[diagnostic.start as usize..diagnostic.end as usize] == "unsafe"
        }));
        assert!(returned.diagnostics.iter().all(|diagnostic| {
            &source[diagnostic.start as usize..diagnostic.end as usize] == "unsafe"
        }));
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_mapped_property_any_is_unsafe_for_assignment_and_return() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-mapped-unsafe-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; type Unsafe = { [Key in \"value\"]: any }; declare const unsafe: Unsafe; const target: { value: string } = unsafe; function read(): { value: string } { return unsafe; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let assignment = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-assignment".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(assignment.diagnostics.len(), 1);
        let returned = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-return".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(returned.diagnostics.len(), 1);
        assert!(assignment.diagnostics.iter().all(|diagnostic| {
            &source[diagnostic.start as usize..diagnostic.end as usize] == "unsafe"
        }));
        assert!(returned.diagnostics.iter().all(|diagnostic| {
            &source[diagnostic.start as usize..diagnostic.end as usize] == "unsafe"
        }));
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_recursive_structural_properties_preserve_any_proofs() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-recursive-unsafe-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; interface A { b: B } interface B { a: A; value: any } declare const unsafe: A; const target: { b: { a: A; value: string } } = unsafe; function read(): { b: { a: A; value: string } } { return unsafe; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let assignment = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-assignment".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(assignment.diagnostics.len(), 1);
        let returned = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-return".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(returned.diagnostics.len(), 1);
        assert!(assignment.diagnostics.iter().all(|diagnostic| {
            &source[diagnostic.start as usize..diagnostic.end as usize] == "unsafe"
        }));
        assert!(returned.diagnostics.iter().all(|diagnostic| {
            &source[diagnostic.start as usize..diagnostic.end as usize] == "unsafe"
        }));
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_index_signature_any_is_unsafe_for_assignment_and_return() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-index-unsafe-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; type Unsafe = { [key: string]: any }; declare const unsafe: Unsafe; const target: { [key: string]: string } = unsafe; function read(): { [key: string]: string } { return unsafe; } type Broken = { [key: string]: Missing }; declare const broken: Broken; const brokenTarget: { [key: string]: string } = broken; function brokenRead(): { [key: string]: string } { return broken; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let assignment = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-assignment".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(assignment.diagnostics.len(), 2);
        assert!(
            assignment
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message_id == "unsafeAssignment")
        );
        assert!(
            assignment
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message_id == "errorAssignment")
        );
        let returned = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-return".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(returned.diagnostics.len(), 2);
        assert!(
            returned
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message_id == "unsafeReturn")
        );
        assert!(
            returned
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message_id == "errorReturn")
        );
        assert!(assignment.diagnostics.iter().all(|diagnostic| {
            let text = &source[diagnostic.start as usize..diagnostic.end as usize];
            matches!(text, "unsafe" | "broken")
        }));
        assert!(returned.diagnostics.iter().all(|diagnostic| {
            let text = &source[diagnostic.start as usize..diagnostic.end as usize];
            matches!(text, "unsafe" | "broken")
        }));
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_callable_property_return_any_is_unsafe_for_assignment_and_return() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-callable-property-unsafe-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; type Unsafe = { callback: () => any }; declare const unsafe: Unsafe; const target: { callback: () => string } = unsafe; function read(): { callback: () => string } { return unsafe; } type Broken = { callback: () => Missing }; declare const broken: Broken; const brokenTarget: { callback: () => string } = broken; function brokenRead(): { callback: () => string } { return broken; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let assignment = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-assignment".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(assignment.diagnostics.len(), 2);
        assert!(
            assignment
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message_id == "unsafeAssignment")
        );
        assert!(
            assignment
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message_id == "errorAssignment")
        );
        let returned = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-return".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(returned.diagnostics.len(), 2);
        assert!(
            returned
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message_id == "unsafeReturn")
        );
        assert!(
            returned
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message_id == "errorReturn")
        );
        assert!(assignment.diagnostics.iter().all(|diagnostic| {
            let text = &source[diagnostic.start as usize..diagnostic.end as usize];
            matches!(text, "unsafe" | "broken")
        }));
        assert!(returned.diagnostics.iter().all(|diagnostic| {
            let text = &source[diagnostic.start as usize..diagnostic.end as usize];
            matches!(text, "unsafe" | "broken")
        }));
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_return_value_facts_report_any_return_values() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-return-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; declare let input:any; function read() { return input; } function safe() { return 1; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-unsafe-return".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            &source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize],
            "input"
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_condition_facts_report_promise_truthiness() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-misused-promise-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; declare const promise: Promise<number>; if (promise) {} while (promise) {}",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 2);
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_call_argument_facts_report_promise_callbacks_at_void_parameters() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-misused-callback-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; declare function consume(callback: (value: number) => void): void; consume(async value => value); consume(value => value);",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message_id, "promiseCallback");
        assert!(
            source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize]
                .contains("async value")
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assignment_contextual_facts_report_promise_callbacks_at_void_variables() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-misused-assignment-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; let callback: (value: number) => void = value => value; callback = async value => value; let safe: (value: number) => void = value => value;",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message_id, "promiseCallback");
        assert!(
            source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize]
                .contains("async value")
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_return_contextual_facts_report_promise_callbacks_at_void_returns() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-misused-return-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; function factory(): () => void { return async () => 1; } function safe(): () => void { return () => 1; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message_id, "promiseCallback");
        assert!(
            source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize]
                .contains("async ()")
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_property_contextual_facts_report_promise_callbacks_at_void_properties() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-misused-property-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; type Props = { callback: (value: number) => void }; const value: Props = { callback: async value => value }; const safe: Props = { callback: value => value };",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message_id, "promiseCallback");
        assert!(
            source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize]
                .contains("async value")
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_jsx_contextual_facts_report_promise_callbacks_at_void_attributes() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-misused-jsx-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.tsx");
        let source: Arc<str> = Arc::from(
            "export {}; declare global { namespace JSX { interface IntrinsicElements { widget: { onClick: () => void } } } } const view = <widget onClick={async () => 1} />; const safe = <widget onClick={() => 1} />;",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"jsx":"preserve","lib":["es2022"],"types":[]},"files":["a.tsx"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.tsx", source.clone(), SourceType::Tsx).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message_id, "promiseCallback");
        assert!(
            source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize]
                .contains("async ()")
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_switch_facts_report_union_without_default() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-switch-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; declare const state: 'a' | 'b'; switch (state) { case 'a': break; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/switch-exhaustiveness-check".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message_id, "missingDefault");
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_switch_facts_prove_literal_union_cases() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-switch-literal-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; declare const state: 'a' | 1 | true | null | undefined; switch (state) { case 'a': break; case 1: break; case true: break; case null: break; case void 0: break; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/switch-exhaustiveness-check".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_switch_facts_prove_exact_bigint_union_cases() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-switch-bigint-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; declare const state: 9007199254740993n | 16n; switch (state) { case 0x20000000000001n: break; case 0x10n: break; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/switch-exhaustiveness-check".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_switch_facts_report_missing_string_enum_member() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-switch-enum-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; enum State { A = 'a', B = 'b' } declare const state: State; switch (state) { case State.A: break; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/switch-exhaustiveness-check".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assertion_facts_keep_distinct_enum_declaration_identity() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-enum-assertion-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; enum First { Ready = 1 } enum Second { Ready = 1 } enum FirstText { Ready = 'ready' } enum SecondText { Ready = 'ready' } declare const first: First, second: Second, firstText: FirstText; const firstCast = first as Second; const secondCast = second as First; const textCast = firstText as SecondText;",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({
                "compilerOptions":{"strict":true,"target":"es2022","lib":["es2022"],"types":[]},
                "files":["a.ts"]
            }).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/no-unnecessary-type-assertion".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assertion_facts_keep_distinct_unique_symbol_identity() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-unique-symbol-assertion-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; declare const first: unique symbol, second: unique symbol; const different = first as typeof second; const same = first as typeof first;",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({
                "compilerOptions":{"strict":true,"target":"es2022","lib":["es2022"],"types":[]},
                "files":["a.ts"]
            }).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/no-unnecessary-type-assertion".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            &source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize],
            "first as typeof first"
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assertion_facts_keep_distinct_class_declaration_identity() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-class-identity-assertion-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; class First { private value = 1 } class Second { private value = 1 } declare const first: First; const different = first as Second; const same = first as First;",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({
                "compilerOptions":{"strict":true,"target":"es2022","lib":["es2022"],"types":[]},
                "files":["a.ts"]
            }).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/no-unnecessary-type-assertion".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            &source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize],
            "first as First"
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assertion_facts_preserve_class_this_return_identity() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-class-this-assertion-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; class Box { method(): this { return this; } } declare const box: Box; const fromMethod = box.method() as Box; const same = box as Box;",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({
                "compilerOptions":{"strict":true,"target":"es2022","lib":["es2022"],"types":[]},
                "files":["a.ts"]
            }).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/no-unnecessary-type-assertion".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 2);
        let spans: Vec<_> = result
            .diagnostics
            .iter()
            .map(|diagnostic| &source[diagnostic.start as usize..diagnostic.end as usize])
            .collect();
        assert!(spans.contains(&"box.method() as Box"));
        assert!(spans.contains(&"box as Box"));
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assertion_facts_prove_structurally_equivalent_interfaces() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-structural-interface-assertion-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; interface Left { value: string } interface Right { value: string } declare const left: Left; const same = left as Right;",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({
                "compilerOptions":{"strict":true,"target":"es2022","lib":["es2022"],"types":[]},
                "files":["a.ts"]
            }).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/no-unnecessary-type-assertion".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            &source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize],
            "left as Right"
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assertion_facts_preserve_structural_property_optionalness() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-structural-optional-assertion-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; interface Required { value: string } interface Same { value: string } interface Optional { value?: string } declare const required: Required; const same = required as Same; const changed = required as Optional;",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({
                "compilerOptions":{"strict":true,"target":"es2022","lib":["es2022"],"types":[]},
                "files":["a.ts"]
            }).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/no-unnecessary-type-assertion".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            &source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize],
            "required as Same"
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assertions_require_complete_object_shapes_and_ordered_arguments() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-complete-assertion-shapes");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            r#"
export {};
interface Pair<A, B> { first: A; second: B }
declare const pair: Pair<string, number>;
pair as Pair<number, string>;
pair as Pair<string, number>;
(pair as Pair<string, number>);
((pair));
pair!;
interface Indexed { value: string; [key: string]: string }
interface Plain { value: string }
interface IndexedCopy { value: string; [key: string]: string }
interface WideIndex { value: string; [key: string]: string | number }
interface NumberIndex { value: string; [key: number]: string }
declare const indexed: Indexed;
indexed as Plain;
indexed as WideIndex;
indexed as NumberIndex;
indexed as IndexedCopy;
interface ReadIndex { readonly [key: string]: number }
interface ReadIndexCopy { readonly [key: string]: number }
interface WriteIndex { [key: string]: number }
declare const dictionary: ReadIndex;
dictionary as ReadIndexCopy;
dictionary as WriteIndex;
interface ReadProperty { readonly readonlyCheck: string }
interface ReadPropertyCopy { readonly readonlyCheck: string }
interface WriteProperty { readonlyCheck: string }
interface GetterProperty { get readonlyCheck(): string }
interface AccessorProperty { get readonlyCheck(): string; set readonlyCheck(value: string) }
declare const readonlyObject: ReadProperty;
readonlyObject as WriteProperty;
readonlyObject as ReadPropertyCopy;
readonlyObject as GetterProperty;
readonlyObject as AccessorProperty;
interface PropertyNamedReadonly { readonly: string }
interface PropertyNamedReadonlyCopy { readonly: string }
interface ReadonlyPropertyNamedReadonly { readonly readonly: string }
declare const namedReadonly: PropertyNamedReadonly;
namedReadonly as PropertyNamedReadonlyCopy;
namedReadonly as ReadonlyPropertyNamedReadonly;
import type { ExternalReadonly } from './shape';
declare const externalReadonly: ExternalReadonly;
externalReadonly as ReadPropertyCopy;
externalReadonly as WriteProperty;
type MappedRead = Readonly<WriteProperty>;
declare const mappedRead: MappedRead;
mappedRead as WriteProperty;
interface CallString { value: string; (input: string): number }
interface CallNumber { value: string; (input: number): number }
interface Construct { value: string; new(input: string): object }
declare const callable: CallString;
callable as Plain;
callable as CallNumber;
callable as Construct;
callable as CallString;
interface RequiredUndefined { value: undefined }
interface OptionalUndefined { value?: undefined }
declare const required: RequiredUndefined;
required as OptionalUndefined;
required as RequiredUndefined;
interface EmptyOne {}
interface EmptyTwo {}
declare const empty: EmptyOne;
empty as EmptyTwo;
interface RecursiveOne { next: RecursiveOne; value: string }
interface RecursiveTwo { value: string; next: RecursiveTwo }
interface RecursiveDifferent { next: RecursiveDifferent; value: number }
declare const recursive: RecursiveOne;
recursive as RecursiveTwo;
recursive as RecursiveDifferent;
"#,
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({
                "compilerOptions":{"strict":true,"exactOptionalPropertyTypes":true,"target":"es2022","lib":["es2022"],"types":[]},
                "files":["a.ts"]
            }).to_string())),
            (file.clone(), source.clone()),
            (fixture.join("shape.ts"), Arc::from("export interface ExternalReadonly { readonly readonlyCheck: string }")),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/no-unnecessary-type-assertion".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        let spans: Vec<_> = result
            .diagnostics
            .iter()
            .map(|diagnostic| &source[diagnostic.start as usize..diagnostic.end as usize])
            .collect();
        assert_eq!(
            spans,
            [
                "pair as Pair<string, number>",
                "pair as Pair<string, number>",
                "indexed as IndexedCopy",
                "dictionary as ReadIndexCopy",
                "readonlyObject as ReadPropertyCopy",
                "readonlyObject as GetterProperty",
                "namedReadonly as PropertyNamedReadonlyCopy",
                "externalReadonly as ReadPropertyCopy",
                "callable as CallString",
                "required as RequiredUndefined",
                "empty as EmptyTwo",
                "recursive as RecursiveTwo",
            ]
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assertions_preserve_effective_mapped_and_synthetic_permissions() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-mapped-permissions");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            r#"
export {};
interface Mutable { value: string }
interface Read { readonly value: string }
interface ReadOptional { readonly value?: string }
type Remove<T> = { -readonly [P in keyof T]: T[P] };
type Add<T> = { +readonly [P in keyof T]: T[P] };
type Remap<T> = { [P in keyof T as `renamed${Capitalize<P & string>}`]: T[P] };
declare const added: Readonly<Mutable>;
added as Read;
added as Mutable;
declare const plus: Add<Mutable>;
plus as Read;
plus as Mutable;
declare const removed: Remove<Read>;
removed as Mutable;
removed as Read;
declare const picked: Pick<Read, "value">;
picked as Read;
picked as Mutable;
declare const record: Record<"value", string>;
record as Mutable;
record as Read;
declare const remapped: Remap<Read>;
remapped as { readonly renamedValue: string };
remapped as { renamedValue: string };
remapped as Read;
declare const partial: Partial<Read>;
partial as ReadOptional;
partial as Read;
declare const required: Required<Partial<Read>>;
required as Read;
required as ReadOptional;
declare const readonlyObject: Read;
const spread = { ...readonlyObject };
spread as Mutable;
spread as Read;
const ordinary = { value: "ordinary" };
ordinary as Mutable;
ordinary as Read;
const literal = { value: "fixed" } as const;
literal as { readonly value: "fixed" };
literal as { value: "fixed" };
const angle = <const>{ value: "fixed" };
angle as { readonly value: "fixed" };
angle as { value: "fixed" };
"#,
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"exactOptionalPropertyTypes":true,"target":"es2022","lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/no-unnecessary-type-assertion".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        let spans: Vec<_> = result
            .diagnostics
            .iter()
            .map(|diagnostic| &source[diagnostic.start as usize..diagnostic.end as usize])
            .collect();
        assert_eq!(
            spans,
            [
                "added as Read",
                "plus as Read",
                "removed as Mutable",
                "picked as Read",
                "record as Mutable",
                "remapped as { readonly renamedValue: string }",
                "partial as ReadOptional",
                "required as Read",
                "spread as Mutable",
                "ordinary as Mutable",
                "literal as { readonly value: \"fixed\" }",
                "angle as { readonly value: \"fixed\" }",
            ]
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_switch_facts_prove_string_enum_members() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-switch-enum-complete-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; enum State { A = 'a', B = 'b' } declare const state: State; switch (state) { case State.A: break; case State.B: break; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/switch-exhaustiveness-check".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_string_literals_preserve_utf16_code_units() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-utf16-literals");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            r#"
export {};
declare const value: "\ud800" | "\udfff" | "\ufffd" | "\\ud800" | "\0" | "👍";
switch (value) {
  case "\ud800": break;
  case "\udfff": break;
  case "\ufffd": break;
  case "\\ud800": break;
  case "\0": break;
  case "\ud83d\udc4d": break;
}
declare const partial: "\ud800" | "\ufffd";
switch (partial) { case "\ufffd": break; }
declare const distinct: "\ud800" | "\ud801";
switch (distinct) { case "\ud800": break; }
declare const high: "\ud800";
high as "\ud800";
high as "\ufffd";
high as "\udfff";
high as "\ufffd\ufffd\ufffd";
type Alias = "\ud801";
declare const alias: Alias;
alias as "\ud800";
alias as "\ud801";
type Derived = `pre${Alias}`;
declare const derived: Derived;
derived as "pre\ud801";
derived as "pre\ud800";
"#,
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let input = TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap();
        let typed = project.type_source(&file, input).unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [
                    (
                        "ts/switch-exhaustiveness-check".into(),
                        RuleLevel::Error.into(),
                    ),
                    (
                        "ts/no-unnecessary-type-assertion".into(),
                        RuleLevel::Error.into(),
                    ),
                ]
                .into(),
                ..Default::default()
            })
            .unwrap();
        let spans: Vec<_> = result
            .diagnostics
            .iter()
            .map(|diagnostic| &source[diagnostic.start as usize..diagnostic.end as usize])
            .collect();
        assert_eq!(
            spans,
            [
                r#"switch (partial) { case "\ufffd": break; }"#,
                r#"switch (distinct) { case "\ud800": break; }"#,
                r#"high as "\ud800""#,
                r#"alias as "\ud801""#,
                r#"derived as "pre\ud801""#,
            ]
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_lossy_enum_values_and_property_names_stop_before_equivalence_proofs() {
        use wake_lint_core::{SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-utf16-unavailable");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        for source in [
            r#"export {}; interface A { "\ud800":string } interface B { "\udfff":string } declare const value:A; value as B;"#,
            r#"export {}; enum E { A="\ud800", B="\udfff" } declare const value:E; switch(value){case E.A:break;}"#,
        ] {
            let source: Arc<str> = Arc::from(source);
            let overlays = BTreeMap::from([
                (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
                (file.clone(), source.clone()),
            ]);
            let fs = TypeFileSystem::new(
                Arc::new(OsFileSystem),
                overlays,
                &root,
                CancellationToken::default(),
            )
            .unwrap();
            let mut project = TypeProject::start(
                fs,
                &root,
                "@typescript/native",
                std::slice::from_ref(&config),
                CancellationToken::default(),
            )
            .unwrap();
            let error = match project.type_source(
                &file,
                TypeSource::new("a.ts", source, SourceType::TypeScript).unwrap(),
            ) {
                Ok(_) => panic!("lossy semantic names/values must not reach equivalence proofs"),
                Err(error) => error,
            };
            assert_eq!(error.code, "WAKE_LINT_ANALYSIS");
            assert!(
                error.message.contains("UTF-16") || error.message.contains("lossless"),
                "{error}"
            );
        }
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_imported_long_string_literals_are_not_truncated_or_merged() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-utf16-imported");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let types = fixture.join("types.ts");
        let prefix = "x".repeat(1024);
        let definitions: Arc<str> = Arc::from(format!(
            r#"export type Value = "{prefix}\ud800"; export type Other = "{prefix}\ud801";"#
        ));
        let source: Arc<str> = Arc::from(
            r#"import type { Value, Other } from "./types"; declare const value:Value; value as Other; value as Value;"#,
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts","types.ts"]}).to_string())),
            (file.clone(), source.clone()),
            (types, definitions),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/no-unnecessary-type-assertion".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        let spans: Vec<_> = result
            .diagnostics
            .iter()
            .map(|diagnostic| &source[diagnostic.start as usize..diagnostic.end as usize])
            .collect();
        assert_eq!(spans, ["value as Value"]);
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_switch_facts_match_numeric_and_const_enum_members() {
        use wake_lint_core::{LintOptions, RuleLevel, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-switch-numeric-enum-rule");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source: Arc<str> = Arc::from(
            "export {}; const enum State { A = 1, B = 2 } enum Other { A = 4, B = 8 } declare const state: State, other: Other; switch (state) { case State.A: break; } switch (other) { case Other.A: break; case Other.B: break; }",
        );
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let typed = project
            .type_source(
                &file,
                TypeSource::new("a.ts", source.clone(), SourceType::TypeScript).unwrap(),
            )
            .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/switch-exhaustiveness-check".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message_id, "missingDefault");
        let switch =
            &source[result.diagnostics[0].start as usize..result.diagnostics[0].end as usize];
        assert!(switch.starts_with("switch (state)"));
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_child_queries_bind_core_owned_calls_assertions_and_template_substitutions() {
        use wake_lint_core::{SourceAssertionKind, SourceCallKind, SourceType, TypeSource};
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-children");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.tsx");
        let source: Arc<str> = Arc::from(
            "declare const obj:any, value:any, C:any, tag:any; const result = [obj?.<number>((value as string)!), new C(value), tag`x ${value as number} ${obj(value!)}`, `${value as number}${(obj(value) as string)!}`, import('pkg', {with:{type:'json'}}), <div>{obj(value!)}</div>];",
        );
        let input =
            TypeSource::new(file.to_str().unwrap(), source.clone(), SourceType::Tsx).unwrap();
        assert!(input.is_complete());
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"jsx":"preserve","lib":["es2022"],"types":[]},"files":["a.tsx"]}).to_string())),
            (file.clone(), source.clone()),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let owner = project.project_for(&file).unwrap();
        let wire = project.source(owner, &file, input.source()).unwrap();
        let mut locations = Vec::new();
        for call in input.calls() {
            let kind = match call.kind {
                SourceCallKind::Call | SourceCallKind::DynamicImport => 214,
                SourceCallKind::Construct => 215,
                SourceCallKind::TaggedTemplate => 216,
            };
            locations.push(wire.address(kind, call.span).unwrap());
            if call.kind != SourceCallKind::DynamicImport {
                locations.push(wire.head_address(kind, call.span, call.head).unwrap());
            }
            if call.kind != SourceCallKind::TaggedTemplate {
                for argument in &call.arguments {
                    locations.push(wire.child_address(kind, call.span, *argument).unwrap());
                }
            }
        }
        for assertion in input.assertions() {
            let kind = match assertion.kind {
                SourceAssertionKind::As => 235,
                SourceAssertionKind::Angle => 217,
                SourceAssertionKind::NonNull => 236,
                SourceAssertionKind::Satisfies => 239,
            };
            locations.push(
                wire.child_address(kind, assertion.span, assertion.operand)
                    .unwrap(),
            );
        }
        for template in input.templates() {
            for expression in &template.expressions {
                locations.push(wire.template_address(template.span, *expression).unwrap());
            }
        }
        let typed = project
            .request(owner, "getTypeAtLocations", json!({"locations":locations}))
            .unwrap();
        assert_eq!(typed.as_array().unwrap().len(), locations.len());
        assert!(
            typed
                .as_array()
                .unwrap()
                .iter()
                .all(|value| value["id"].is_u64()),
            "{typed}"
        );
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn native_assertion_addresses_match_the_original_grammar_ranges() {
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-assertions");
        let config = fixture.join("tsconfig.json");
        let file = fixture.join("a.ts");
        let source = "type Num=number; declare const a:Num,b:Num; a || b as Num; a + b as Num; a < b as Num; a as Num + b; const nested=((a as Num)!) satisfies Num; const angle=<Num>a; class C {} new C!();";
        let overlays = BTreeMap::from([
            (config.clone(), Arc::from(json!({"compilerOptions":{"strict":true,"lib":["es2022"],"types":[]},"files":["a.ts"]}).to_string())),
            (file.clone(), Arc::from(source)),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays,
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &[config],
            CancellationToken::default(),
        )
        .unwrap();
        let owner = project.project_for(&file).unwrap();
        let wire = project.source(owner, &file, source).unwrap();
        for (kind, expression) in [
            (235, "b as Num"),
            (235, "a + b as Num"),
            (235, "a < b as Num"),
            (235, "a as Num"),
            (236, "(a as Num)!"),
            (239, "((a as Num)!) satisfies Num"),
            (217, "<Num>a"),
            (236, "C!"),
        ] {
            let lo = source.find(expression).unwrap() as u32;
            assert!(
                wire.address(kind, Span::new(lo, lo + expression.len() as u32))
                    .is_ok(),
                "{expression}"
            );
        }
    }

    #[test]
    #[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
    fn project_membership_is_explicit_and_wire_queries_reject_wrong_source_versions() {
        let root = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let fixture = root.join(".tmp/virtual-type-projects");
        let configs = [
            fixture.join("one/tsconfig.json"),
            fixture.join("two/tsconfig.json"),
        ];
        let files = [fixture.join("one/a.ts"), fixture.join("two/a.ts")];
        let source = "import {read} from '../shared'; const value = read();";
        let config = json!({"compilerOptions":{"strict":true,"lib":["es2022"],"target":"es2022","module":"esnext","moduleResolution":"bundler","types":[]},"files":["a.ts"]}).to_string();
        let overlays = BTreeMap::from([
            (configs[0].clone(), Arc::from(config.clone())),
            (configs[1].clone(), Arc::from(config)),
            (files[0].clone(), Arc::from(source)),
            (files[1].clone(), Arc::from(source)),
            (
                fixture.join("shared.ts"),
                Arc::from("export async function read() { return 42; }"),
            ),
            (fixture.join("excluded.ts"), Arc::from("export {};")),
        ]);
        let fs = TypeFileSystem::new(
            Arc::new(OsFileSystem),
            overlays.clone(),
            &root,
            CancellationToken::default(),
        )
        .unwrap();
        let mut project = TypeProject::start(
            fs,
            &root,
            "@typescript/native",
            &configs,
            CancellationToken::default(),
        )
        .unwrap();
        for file in &files {
            let owner = project.project_for(file).unwrap();
            let wire = project.source(owner, file, source).unwrap();
            let location = wire
                .address(
                    214,
                    Span::new(
                        source.rfind("read()").unwrap() as u32,
                        (source.len() - 1) as u32,
                    ),
                )
                .unwrap();
            let typed = project
                .request(owner, "getTypeAtLocation", json!({"location":location}))
                .unwrap();
            assert_eq!(
                project
                    .request(owner, "typeToString", json!({"type":typed["id"]}))
                    .unwrap(),
                "Promise<number>"
            );
        }
        assert!(
            project
                .project_for(&fixture.join("excluded.ts"))
                .unwrap_err()
                .message
                .contains("program")
        );
        assert!(
            project
                .project_for(&fixture.join("shared.ts"))
                .unwrap_err()
                .message
                .contains("ambiguous")
        );
        let owner = project.project_for(&files[0]).unwrap();
        assert!(project.source(owner, &files[0], "changed").is_err());
        drop(project);
        for (config, expected) in [
            ("{broken", "configuration"),
            (
                "{\"compilerOptions\":{\"lib\":[\"missing-library\"]},\"files\":[\"a.ts\"]}",
                "configuration",
            ),
        ] {
            let mut overlays = overlays.clone();
            overlays.insert(configs[0].clone(), Arc::from(config));
            let fs = TypeFileSystem::new(
                Arc::new(OsFileSystem),
                overlays,
                &root,
                CancellationToken::default(),
            )
            .unwrap();
            let error = TypeProject::start(
                fs,
                &root,
                "@typescript/native",
                &configs[..1],
                CancellationToken::default(),
            )
            .err()
            .expect("invalid configuration must fail");
            assert!(error.message.contains(expected), "{error}");
        }
    }
}
