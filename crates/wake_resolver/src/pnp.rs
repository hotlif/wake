//! # Yarn PnP（Plug'n'Play）解析
//!
//! Yarn Berry 的 PnP 模式**不铺 `node_modules`**：所有包由一份 `.pnp.cjs` 里内嵌的依赖图直接定位，
//! 包体以**无压缩 zip**（见 [`wake_common::zip`]）留在全局缓存。本模块负责：
//!
//! 1. **提取 + 解析** `.pnp.cjs` 内嵌的 `RAW_RUNTIME_STATE` JSON（或旁边的 `.pnp.data.json`）。
//! 2. **PnP 解析算法**：给定 issuer（导入方目录）与裸说明符，
//!    经 `findPackageLocator`（最长路径前缀）→ 查 issuer 包的 `packageDependencies`
//!    → 得目标 locator → 得该包位置 → 拼子路径，产出「未限定」路径（unqualified）。
//!    随后由现有的 [`crate::Resolver`] 文件/目录解析补扩展名/main/index。
//! 3. **`resolveVirtual`**：把 `.yarn/__virtual__/<hash>/<depth>/…` 虚拟路径映射回真实 zip 路径
//!    （虚拟目录不物理存在，仅承载 peer 依赖的不同解析）。
//!
//! `exports`/`imports` 字段、alias、patch 协议的真实补丁应用留后续；当前覆盖 zip-backed
//! 常规依赖（React/lodash 等）——它们的入口都是真实文件，走 main/index 即可命中。

use std::path::{Component, Path, PathBuf};

use wake_common::{FileSystem, FxHashMap, FxHashSet, fs::normalize};

use crate::split_package;

/// 包定位符 `(ident, reference)`。顶层包用 `(None, None)`。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Locator {
    ident: Option<String>,
    reference: Option<String>,
}

/// 一条依赖的解析目标 locator（处理 `[realName, ref]` 别名式）。
#[derive(Clone, Debug)]
struct DepTarget {
    ident: String,
    reference: String,
}

/// 单个包的信息。
struct PackageInfo {
    /// 包位置（已归一化、相对 cwd；可能是虚拟路径、可能指向 zip 内部）。
    location: PathBuf,
    /// 依赖名 → 解析目标；`None` 表示未满足的 peer 依赖（数据里为 `null`）。
    dependencies: FxHashMap<String, Option<DepTarget>>,
}

/// 一份解析好的 PnP 清单。
pub struct PnpManifest {
    root: PathBuf,
    enable_top_level_fallback: bool,
    /// 被排除出「顶层 fallback」的 issuer locator 集合。
    fallback_exclusion: FxHashSet<Locator>,
    /// 顶层 fallback 依赖池（`fallbackPool`）。
    fallback_pool: FxHashMap<String, Option<DepTarget>>,
    packages: FxHashMap<Locator, PackageInfo>,
    /// `findPackageLocator` 用：（归一化位置, locator），按最长前缀匹配；已剔除 discardFromLookup。
    locations: Vec<(PathBuf, Locator)>,
}

/// PnP 解析失败原因。
#[derive(Debug, PartialEq, Eq)]
pub enum PnpError {
    /// issuer 不属于任何已知包（不该发生，除非 issuer 在项目外）。
    IssuerNotFound,
    /// issuer 包未声明对该依赖的依赖（且 fallback 也没有）。
    Undeclared,
    /// 依赖是未满足的 peer（数据里为 `null`）。
    UnfulfilledPeer,
    /// 目标 locator 在清单中缺失（清单不一致）。
    MissingPackage,
    /// issuer 的物理位置同时对应多个 locator，且它们将当前依赖解析到不同目标。
    AmbiguousIssuer,
}

impl PnpManifest {
    /// Return the normalized project root that owns this manifest.
    ///
    /// Runtime consumers use this only as a physical invalidation witness when a resolution
    /// fails; package resolution itself remains owned by [`crate::Resolver`].
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 查找 `dir` 所属的 Yarn PnP 项目根目录。
    ///
    /// 根发现与清单解析分离，使需要缓存解析上下文的产品边缘可以用稳定的项目根作为 key，
    /// 同时仍由 resolver 唯一拥有 `.pnp.cjs` 的发现规则。
    pub fn discover_root(fs: &dyn FileSystem, start_dir: &Path) -> Option<PathBuf> {
        let mut dir = normalize(start_dir);
        loop {
            if fs.is_file(&dir.join(".pnp.cjs")) || fs.is_file(&dir.join(".pnp.data.json")) {
                return Some(dir);
            }
            if !dir.pop() {
                return (fs.is_file(Path::new(".pnp.cjs"))
                    || fs.is_file(Path::new(".pnp.data.json")))
                .then(PathBuf::new);
            }
        }
    }

    /// 若 `dir`（含各祖先目录）存在 `.pnp.cjs`/`.pnp.data.json`，加载并返回（清单, pnp_root 相对 cwd 路径）。
    ///
    /// `start_dir` 应为入口文件所在目录（相对 cwd）。逐级上溯查找。
    pub fn discover(fs: &dyn FileSystem, start_dir: &Path) -> Option<PnpManifest> {
        let root = PnpManifest::discover_root(fs, start_dir)?;
        PnpManifest::load(fs, &root)
    }

    /// 从 `pnp_root` 目录（相对 cwd）加载清单。
    pub fn load(fs: &dyn FileSystem, pnp_root: &Path) -> Option<PnpManifest> {
        // 优先 `.pnp.data.json`（纯 JSON，较新 Yarn 可选产出）；否则从 `.pnp.cjs` 提取内嵌 JSON。
        let json = {
            let data_path = pnp_root.join(".pnp.data.json");
            if let Ok(s) = fs.read_to_string(&data_path) {
                s
            } else {
                let cjs = fs.read_to_string(&pnp_root.join(".pnp.cjs")).ok()?;
                extract_pnp_data(&cjs)?
            }
        };
        let value: serde_json::Value = serde_json::from_str(&json).ok()?;
        PnpManifest::from_value(&value, pnp_root)
    }

    fn from_value(v: &serde_json::Value, pnp_root: &Path) -> Option<PnpManifest> {
        let enable_top_level_fallback = v
            .get("enableTopLevelFallback")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);

        // fallbackExclusionList: [[ident, [ref, ...]], ...]
        let mut fallback_exclusion = FxHashSet::default();
        if let Some(list) = v.get("fallbackExclusionList").and_then(|x| x.as_array()) {
            for pair in list {
                let arr = pair.as_array()?;
                let ident = arr.first().and_then(|x| x.as_str());
                if let Some(refs) = arr.get(1).and_then(|x| x.as_array()) {
                    for r in refs {
                        if let Some(reference) = r.as_str() {
                            fallback_exclusion.insert(Locator {
                                ident: ident.map(str::to_string),
                                reference: Some(reference.to_string()),
                            });
                        }
                    }
                }
            }
        }

        // fallbackPool: [[name, ref|null|[name,ref]], ...]
        let mut fallback_pool = FxHashMap::default();
        if let Some(pool) = v.get("fallbackPool").and_then(|x| x.as_array()) {
            parse_dependencies(pool, &mut fallback_pool);
        }

        // packageRegistryData: [[ident|null, [[ref|null, info], ...]], ...]
        let mut packages: FxHashMap<Locator, PackageInfo> = FxHashMap::default();
        let mut locations: Vec<(PathBuf, Locator)> = Vec::new();
        let registry = v.get("packageRegistryData")?.as_array()?;
        for ident_entry in registry {
            let pair = ident_entry.as_array()?;
            let ident = pair.first().and_then(|x| x.as_str()).map(str::to_string);
            let refs = pair.get(1)?.as_array()?;
            for ref_entry in refs {
                let rp = ref_entry.as_array()?;
                let reference = rp.first().and_then(|x| x.as_str()).map(str::to_string);
                let info = rp.get(1)?;
                let raw_loc = info.get("packageLocation").and_then(|x| x.as_str())?;
                // 位置相对 pnp_root；拼到 pnp_root 上再归一化 → 相对 cwd。
                let location = normalize(&pnp_root.join(raw_loc));
                let mut dependencies = FxHashMap::default();
                if let Some(deps) = info.get("packageDependencies").and_then(|x| x.as_array()) {
                    parse_dependencies(deps, &mut dependencies);
                }
                let discard = info
                    .get("discardFromLookup")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false);
                let locator = Locator {
                    ident: ident.clone(),
                    reference: reference.clone(),
                };
                if !discard {
                    locations.push((location.clone(), locator.clone()));
                }
                packages.insert(
                    locator,
                    PackageInfo {
                        location,
                        dependencies,
                    },
                );
            }
        }

        // 最长前缀优先。同一物理位置的 locator 必须保留为候选集，不能在这里
        // 任意优先 virtual/base；实际取舍取决于当前要解析的依赖。
        locations.sort_by(|a, b| {
            b.0.components()
                .count()
                .cmp(&a.0.components().count())
                .then_with(|| a.0.cmp(&b.0))
        });

        Some(PnpManifest {
            root: normalize(pnp_root),
            enable_top_level_fallback,
            fallback_exclusion,
            fallback_pool,
            packages,
            locations,
        })
    }

    /// 找 issuer 所属的所有 locator：只保留最长位置前缀下的同路径候选。
    ///
    /// Yarn 的 unplugged 包可能让 base/virtual locator 共用一个 `packageLocation`。
    /// 候选不能靠清单顺序或 virtual 标记决胜，必须结合当前依赖消歧。
    fn find_package_locators(&self, issuer_dir: &Path) -> Vec<&Locator> {
        let issuer = normalize(issuer_dir);
        let Some((matched_location, _)) = self
            .locations
            .iter()
            .find(|(loc, _)| path_has_prefix(&issuer, loc))
        else {
            return Vec::new();
        };
        self.locations
            .iter()
            .filter(|(location, _)| location == matched_location)
            .map(|(_, locator)| locator)
            .collect()
    }

    /// 返回 issuer 候选集一致的包名，供上层做严格限定的依赖 fallback。
    pub fn issuer_package_name(&self, issuer_dir: &Path) -> Result<Option<&str>, PnpError> {
        let locators = self.find_package_locators(issuer_dir);
        let Some(first) = locators.first() else {
            return Err(PnpError::IssuerNotFound);
        };
        let ident = first.ident.as_deref();
        if locators
            .iter()
            .all(|locator| locator.ident.as_deref() == ident)
        {
            Ok(ident)
        } else {
            Err(PnpError::AmbiguousIssuer)
        }
    }

    /// 解析裸说明符到「未限定」路径（相对 cwd；可能虚拟 / 指向 zip）。
    ///
    /// 返回的路径需再经 [`crate::Resolver`] 的文件/目录解析补 main/index/扩展名。
    pub fn resolve_bare(&self, specifier: &str, issuer_dir: &Path) -> Result<PathBuf, PnpError> {
        let (ident, subpath) = split_package(specifier);
        let issuer_locators = self.find_package_locators(issuer_dir);
        if issuer_locators.is_empty() {
            return Err(PnpError::IssuerNotFound);
        }

        // 先在全部候选中尝试 issuer 自身的直接依赖。只要有直接依赖成功，
        // 就不应让另一候选的顶层 fallback 改变结果。
        let mut direct_failures = Vec::new();
        let mut resolved_target = None;
        for issuer_locator in &issuer_locators {
            match self.resolve_direct_dependency_locator(issuer_locator, &ident) {
                Ok(target_locator) => merge_resolved_target(&mut resolved_target, target_locator)?,
                Err(error) => direct_failures.push((*issuer_locator, error)),
            }
        }
        if let Some(target_locator) = resolved_target {
            return self.unqualified_path(&target_locator, &subpath);
        }

        // 所有直接依赖都失败后，再逐候选尝试 Yarn 顶层 fallback。
        // `MissingPackage`/`IssuerNotFound` 表示清单损坏而非依赖边界；绝不能用顶层
        // 同名包掩盖。只有未声明依赖和未满足 peer 符合 Yarn fallback 语义。
        if direct_failures
            .iter()
            .any(|(_, error)| !matches!(error, PnpError::Undeclared | PnpError::UnfulfilledPeer))
        {
            return Err(direct_failures
                .into_iter()
                .map(|(_, error)| error)
                .fold(PnpError::Undeclared, preferred_failure));
        }
        let mut fallback_target = None;
        let mut failure = PnpError::Undeclared;
        for (issuer_locator, direct_error) in direct_failures {
            let result = self
                .fallback_lookup(issuer_locator, &ident)
                .ok_or(direct_error)
                .and_then(|target| self.locator_for_target(target));
            match result {
                Ok(target_locator) => merge_resolved_target(&mut fallback_target, target_locator)?,
                Err(error) => failure = preferred_failure(failure, error),
            }
        }
        let target_locator = fallback_target.ok_or(failure)?;
        self.unqualified_path(&target_locator, &subpath)
    }

    fn unqualified_path(
        &self,
        target_locator: &Locator,
        subpath: &str,
    ) -> Result<PathBuf, PnpError> {
        let pkg = self
            .packages
            .get(target_locator)
            .ok_or(PnpError::MissingPackage)?;
        let unqualified = if subpath.is_empty() {
            pkg.location.clone()
        } else {
            normalize(&pkg.location.join(subpath))
        };
        Ok(unqualified)
    }

    /// 用一个确定 issuer locator 解析它自身声明的依赖。
    fn resolve_direct_dependency_locator(
        &self,
        issuer_locator: &Locator,
        ident: &str,
    ) -> Result<Locator, PnpError> {
        let issuer_pkg = self
            .packages
            .get(issuer_locator)
            .ok_or(PnpError::IssuerNotFound)?;
        let target = match issuer_pkg.dependencies.get(ident) {
            Some(Some(target)) => target,
            Some(None) => return Err(PnpError::UnfulfilledPeer),
            None => return Err(PnpError::Undeclared),
        };
        self.locator_for_target(target)
    }

    fn locator_for_target(&self, target: &DepTarget) -> Result<Locator, PnpError> {
        let locator = Locator {
            ident: Some(target.ident.clone()),
            reference: Some(target.reference.clone()),
        };
        self.packages
            .contains_key(&locator)
            .then_some(locator)
            .ok_or(PnpError::MissingPackage)
    }

    /// 顶层 fallback：`enableTopLevelFallback` 且 issuer 未被排除时，
    /// 依次查顶层包依赖与 `fallbackPool`。
    fn fallback_lookup(&self, issuer: &Locator, ident: &str) -> Option<&DepTarget> {
        if !self.enable_top_level_fallback || self.fallback_exclusion.contains(issuer) {
            return None;
        }
        // 顶层包 = (None, None)。
        let top = self.packages.get(&Locator {
            ident: None,
            reference: None,
        });
        if let Some(Some(t)) = top.and_then(|p| p.dependencies.get(ident)) {
            return Some(t);
        }
        if let Some(Some(t)) = self.fallback_pool.get(ident) {
            return Some(t);
        }
        None
    }
}

fn merge_resolved_target(
    resolved: &mut Option<Locator>,
    candidate: Locator,
) -> Result<(), PnpError> {
    match resolved {
        Some(previous) if previous != &candidate => Err(PnpError::AmbiguousIssuer),
        Some(_) => Ok(()),
        None => {
            *resolved = Some(candidate);
            Ok(())
        }
    }
}

/// 多个同路径 locator 都失败时，保留最能描述清单问题的错误。
fn preferred_failure(current: PnpError, candidate: PnpError) -> PnpError {
    fn rank(error: &PnpError) -> u8 {
        match error {
            PnpError::IssuerNotFound => 0,
            PnpError::Undeclared => 1,
            PnpError::UnfulfilledPeer => 2,
            PnpError::MissingPackage => 3,
            PnpError::AmbiguousIssuer => 4,
        }
    }
    if rank(&candidate) > rank(&current) {
        candidate
    } else {
        current
    }
}

/// 解析 `packageDependencies`/`fallbackPool` 条目 `[name, ref|null|[realName, realRef]]`。
fn parse_dependencies(arr: &[serde_json::Value], out: &mut FxHashMap<String, Option<DepTarget>>) {
    for entry in arr {
        let Some(pair) = entry.as_array() else {
            continue;
        };
        let Some(name) = pair.first().and_then(|x| x.as_str()) else {
            continue;
        };
        let rref = pair.get(1);
        let target = match rref {
            Some(serde_json::Value::String(s)) => Some(DepTarget {
                ident: name.to_string(),
                reference: s.clone(),
            }),
            // 别名式 [realName, realRef]。
            Some(serde_json::Value::Array(a)) => {
                let real_name = a.first().and_then(|x| x.as_str());
                let real_ref = a.get(1).and_then(|x| x.as_str());
                match (real_name, real_ref) {
                    (Some(n), Some(r)) => Some(DepTarget {
                        ident: n.to_string(),
                        reference: r.to_string(),
                    }),
                    _ => None,
                }
            }
            // null / 缺失 → 未满足 peer。
            _ => None,
        };
        out.insert(name.to_string(), target);
    }
}

/// 组件级前缀判定：`path` 的组件序列是否以 `prefix` 的组件序列开头。
///
/// 用组件而非字符串前缀，避免 `react` 误配 `react-dom`。空 `prefix`（顶层 `./`）是任何路径的前缀。
fn path_has_prefix(path: &Path, prefix: &Path) -> bool {
    path.starts_with(prefix)
}

/// 把 `.yarn/__virtual__/<hash>/<depth>/<rest>` 虚拟路径映射回真实物理路径。
///
/// 虚拟目录不物理存在；`<depth>` 指明从 `__virtual__` 的父目录再上溯多少级即到 `<rest>` 的真实根。
/// 见 `@yarnpkg/fslib` 的 `VirtualFS.resolveVirtual`。非虚拟路径原样返回。可能嵌套，故循环。
pub fn resolve_virtual(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    // 防御性上限：现实至多一层虚拟；给足冗余避免异常数据死循环。
    for _ in 0..8 {
        let comps: Vec<Component> = current.components().collect();
        let Some(vi) = comps.iter().position(
            |c| matches!(c, Component::Normal(s) if *s == "__virtual__" || *s == "$$virtual"),
        ) else {
            return current;
        };
        // 需要 <hash>=comps[vi+1]、<depth>=comps[vi+2]（数字）。
        let depth = match comps.get(vi + 2) {
            Some(Component::Normal(s)) => match s.to_str().and_then(|s| s.parse::<usize>().ok()) {
                Some(d) => d,
                None => return current, // 非数字 → 非法虚拟，原样返回
            },
            _ => return current,
        };
        // target = __virtual__ 的父目录（comps[0..vi]）。
        let mut result = PathBuf::new();
        for c in &comps[0..vi] {
            result.push(c.as_os_str());
        }
        for _ in 0..depth {
            result.push("..");
        }
        for c in &comps[vi + 3..] {
            result.push(c.as_os_str());
        }
        let next = normalize(&result);
        if next == current {
            return next;
        }
        current = next;
    }
    current
}

/// 从 `.pnp.cjs` 源码提取内嵌的 `RAW_RUNTIME_STATE` JSON 文本（JS 单引号字符串 → 解码）。
pub fn extract_pnp_data(source: &str) -> Option<String> {
    // 定位 `RAW_RUNTIME_STATE` 赋值后的首个单引号。
    let key = source.find("RAW_RUNTIME_STATE")?;
    let after = &source[key..];
    let q = after.find('\'')?;
    let body = &after[q + 1..];
    Some(decode_js_single_quoted(body))
}

/// 解码一个 JS 单引号字符串字面量（到首个未转义的 `'`）。处理续行与常见转义。
fn decode_js_single_quoted(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => break,
            '\\' => match chars.next() {
                Some('\n') => {} // 续行：反斜杠+换行 → 无
                Some('\r') => {
                    // 反斜杠 + CRLF 续行：吞掉紧随的 \n（若有）。
                    // 无法回退迭代器，这里保守地不额外处理——真实 .pnp.cjs 为 LF。
                }
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{0008}'),
                Some('f') => out.push('\u{000C}'),
                Some('v') => out.push('\u{000B}'),
                Some('0') => out.push('\0'),
                Some('\\') => out.push('\\'),
                Some('\'') => out.push('\''),
                Some('"') => out.push('"'),
                Some('/') => out.push('/'),
                Some('x') => {
                    let h: String = chars.by_ref().take(2).collect();
                    if let Ok(n) = u32::from_str_radix(&h, 16)
                        && let Some(ch) = char::from_u32(n)
                    {
                        out.push(ch);
                    }
                }
                Some('u') => {
                    // \uHHHH 或 \u{H..}
                    let mut rest = chars.clone();
                    if rest.next() == Some('{') {
                        let hex: String = rest.by_ref().take_while(|&c| c != '}').collect();
                        // 推进原迭代器：{ + hex + }
                        for _ in 0..hex.len() + 2 {
                            chars.next();
                        }
                        if let Ok(n) = u32::from_str_radix(&hex, 16)
                            && let Some(ch) = char::from_u32(n)
                        {
                            out.push(ch);
                        }
                    } else {
                        let hex: String = chars.by_ref().take(4).collect();
                        if let Ok(n) = u32::from_str_radix(&hex, 16)
                            && let Some(ch) = char::from_u32(n)
                        {
                            out.push(ch);
                        }
                    }
                }
                Some(other) => out.push(other),
                None => break,
            },
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::MemoryFileSystem;

    #[test]
    fn discovers_data_only_pnp_roots() {
        let fs = MemoryFileSystem::new();
        fs.insert(
            "project/.pnp.data.json",
            r#"{
                "packageRegistryData": [[null, [[null, {
                    "packageLocation": "./",
                    "packageDependencies": [],
                    "linkType": "SOFT"
                }]]]]
            }"#,
        );
        assert_eq!(
            PnpManifest::discover_root(&fs, Path::new("project/src")),
            Some(PathBuf::from("project"))
        );
        assert!(PnpManifest::discover(&fs, Path::new("project/src")).is_some());
    }

    #[test]
    fn decodes_js_string_with_continuations() {
        // 模拟 .pnp.cjs：反斜杠续行（\ + 换行 → 无）。
        let body = "{\\\n  \"a\": \"b\"\\\n}'; more";
        let decoded = decode_js_single_quoted(body);
        assert_eq!(decoded, "{  \"a\": \"b\"}");
        let v: serde_json::Value = serde_json::from_str(&decoded).unwrap();
        assert_eq!(v["a"], "b");
    }

    #[test]
    fn decodes_escaped_backslashes() {
        // .pnp.cjs 里 JSON 值含反斜杠：源码 4 个反斜杠 → JS 解码 2 个 → JSON 解码 1 个。
        // 这里用原始字符串给出 JS 字符串体 `{"a":"x\\\\y"}`（4 反斜杠）。
        let body = r#"{"a":"x\\\\y"}'"#;
        let decoded = decode_js_single_quoted(body);
        assert_eq!(decoded, r#"{"a":"x\\y"}"#);
        let v: serde_json::Value = serde_json::from_str(&decoded).unwrap();
        assert_eq!(v["a"], r"x\y");
    }

    #[test]
    fn extract_finds_raw_runtime_state() {
        let src = "const RAW_RUNTIME_STATE =\n'{\\\n  \"enableTopLevelFallback\": true\\\n}';\n";
        let json = extract_pnp_data(src).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["enableTopLevelFallback"], true);
    }

    #[test]
    fn resolve_virtual_strips_virtual_segment() {
        // <root>/.yarn/__virtual__/<hash>/7/AppData/x/pkg.zip/node_modules/pkg/index.js
        // depth 7，父目录 = .yarn（1 段），净上溯 6 级。
        let p = Path::new(
            ".yarn/__virtual__/react-dom-virtual-1cb40a477a/7/AppData/Local/Yarn/Berry/cache/react-dom.zip/node_modules/react-dom/index.js",
        );
        let real = resolve_virtual(p);
        assert_eq!(
            real,
            PathBuf::from(
                "../../../../../../AppData/Local/Yarn/Berry/cache/react-dom.zip/node_modules/react-dom/index.js"
            )
        );
    }

    #[test]
    fn resolve_virtual_passthrough_non_virtual() {
        let p = Path::new("../../AppData/cache/react.zip/node_modules/react/index.js");
        assert_eq!(resolve_virtual(p), normalize(p));
    }

    /// 用一段合成的 PnP 数据端到端验证解析算法（含虚拟依赖 + 子路径 + fallback）。
    fn sample_manifest() -> PnpManifest {
        let json = r#"{
            "enableTopLevelFallback": true,
            "fallbackExclusionList": [],
            "fallbackPool": [],
            "packageRegistryData": [
                [null, [[null, {
                    "packageLocation": "./",
                    "packageDependencies": [
                        ["react", "npm:19.0.0"],
                        ["react-dom", "virtual:abc#npm:19.0.0"]
                    ],
                    "linkType": "SOFT"
                }]]],
                ["react", [["npm:19.0.0", {
                    "packageLocation": "../../cache/react.zip/node_modules/react/",
                    "packageDependencies": [["react", "npm:19.0.0"]],
                    "linkType": "HARD"
                }]]],
                ["react-dom", [
                    ["npm:19.0.0", {
                        "packageLocation": "../../cache/react-dom.zip/node_modules/react-dom/",
                        "packageDependencies": [["react-dom", "npm:19.0.0"]],
                        "linkType": "SOFT"
                    }],
                    ["virtual:abc#npm:19.0.0", {
                        "packageLocation": "./.yarn/__virtual__/react-dom-virtual-x/5/cache/react-dom.zip/node_modules/react-dom/",
                        "packageDependencies": [
                            ["react-dom", "virtual:abc#npm:19.0.0"],
                            ["react", "npm:19.0.0"],
                            ["scheduler", "npm:0.25.0"]
                        ],
                        "linkType": "HARD"
                    }]
                ]],
                ["scheduler", [["npm:0.25.0", {
                    "packageLocation": "../../cache/scheduler.zip/node_modules/scheduler/",
                    "packageDependencies": [["scheduler", "npm:0.25.0"]],
                    "linkType": "HARD"
                }]]]
            ]
        }"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        PnpManifest::from_value(&v, Path::new("")).unwrap()
    }

    #[test]
    fn resolves_bare_from_top_level() {
        let m = sample_manifest();
        // 顶层 import "react" → react 包位置。
        let u = m.resolve_bare("react", Path::new("src")).unwrap();
        assert_eq!(u, PathBuf::from("../../cache/react.zip/node_modules/react"));
        // import "react-dom/client" → 虚拟位置 + 子路径 client。
        let u = m
            .resolve_bare("react-dom/client", Path::new("src"))
            .unwrap();
        assert_eq!(
            u,
            PathBuf::from(
                ".yarn/__virtual__/react-dom-virtual-x/5/cache/react-dom.zip/node_modules/react-dom/client"
            )
        );
    }

    #[test]
    fn resolves_transitive_scheduler_via_virtual_issuer() {
        let m = sample_manifest();
        // issuer 是 react-dom 虚拟包内的文件 → 应看到虚拟包的依赖（含 scheduler）。
        let issuer = Path::new(
            ".yarn/__virtual__/react-dom-virtual-x/5/cache/react-dom.zip/node_modules/react-dom",
        );
        let u = m.resolve_bare("scheduler", issuer).unwrap();
        assert_eq!(
            u,
            PathBuf::from("../../cache/scheduler.zip/node_modules/scheduler")
        );
        // 而非虚拟 react-dom（npm:19.0.0）不依赖 scheduler → 从它那里解析应失败。
        let issuer_nonvirtual = Path::new("../../cache/react-dom.zip/node_modules/react-dom");
        assert_eq!(
            m.resolve_bare("scheduler", issuer_nonvirtual),
            Err(PnpError::Undeclared)
        );
    }

    #[test]
    fn shared_unplugged_location_uses_the_only_locator_that_resolves_dependency() {
        let json = r#"{
            "enableTopLevelFallback": false,
            "fallbackExclusionList": [],
            "fallbackPool": [],
            "packageRegistryData": [
                [null, [[null, {
                    "packageLocation": "./",
                    "packageDependencies": [
                        ["wake", "virtual:peer-hash#file:../wake"]
                    ],
                    "linkType": "SOFT"
                }]]],
                ["wake", [
                    ["file:../wake", {
                        "packageLocation": "./.yarn/unplugged/wake/node_modules/wake/",
                        "packageDependencies": [
                            ["wake", "file:../wake"]
                        ],
                        "linkType": "SOFT"
                    }],
                    ["virtual:peer-hash#file:../wake", {
                        "packageLocation": "./.yarn/unplugged/wake/node_modules/wake/",
                        "packageDependencies": [
                            ["wake", "virtual:peer-hash#file:../wake"],
                            ["ui", "npm:1.0.0"]
                        ],
                        "linkType": "SOFT"
                    }]
                ]],
                ["ui", [["npm:1.0.0", {
                    "packageLocation": "../../cache/ui.zip/node_modules/ui/",
                    "packageDependencies": [["ui", "npm:1.0.0"]],
                    "linkType": "HARD"
                }]]]
            ]
        }"#;
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let manifest = PnpManifest::from_value(&value, Path::new("")).unwrap();
        let issuer = Path::new(".yarn/unplugged/wake/node_modules/wake/internal");

        assert_eq!(
            manifest.resolve_bare("ui", issuer).unwrap(),
            PathBuf::from("../../cache/ui.zip/node_modules/ui")
        );
    }

    fn shared_location_manifest(instances: serde_json::Value) -> PnpManifest {
        shared_location_manifest_with_top_level(instances, None)
    }

    fn shared_location_manifest_with_top_level(
        instances: serde_json::Value,
        top_level_ui: Option<&str>,
    ) -> PnpManifest {
        let mut top_dependencies = vec![serde_json::json!(["wake", "file:../wake"])];
        if let Some(reference) = top_level_ui {
            top_dependencies.push(serde_json::json!(["ui", reference]));
        }
        let value = serde_json::json!({
            "enableTopLevelFallback": top_level_ui.is_some(),
            "fallbackExclusionList": [],
            "fallbackPool": [],
            "packageRegistryData": [
                [null, [[null, {
                    "packageLocation": "./",
                    "packageDependencies": top_dependencies,
                    "linkType": "SOFT"
                }]]],
                ["wake", instances],
                ["ui", [
                    ["npm:1.0.0", {
                        "packageLocation": "../../cache/ui-v1/node_modules/ui/",
                        "packageDependencies": [["ui", "npm:1.0.0"]],
                        "linkType": "HARD"
                    }],
                    ["npm:2.0.0", {
                        "packageLocation": "../../cache/ui-v2/node_modules/ui/",
                        "packageDependencies": [["ui", "npm:2.0.0"]],
                        "linkType": "HARD"
                    }]
                ]]
            ]
        });
        PnpManifest::from_value(&value, Path::new("")).unwrap()
    }

    fn wake_instance(reference: &str, ui_reference: Option<&str>) -> serde_json::Value {
        let mut dependencies = vec![serde_json::json!(["wake", reference])];
        if let Some(ui_reference) = ui_reference {
            dependencies.push(serde_json::json!(["ui", ui_reference]));
        }
        serde_json::json!([reference, {
            "packageLocation": "./.yarn/unplugged/wake/node_modules/wake/",
            "packageDependencies": dependencies,
            "linkType": "SOFT"
        }])
    }

    #[test]
    fn shared_location_base_locator_resolves_normally() {
        let manifest = shared_location_manifest(serde_json::json!([wake_instance(
            "file:../wake",
            Some("npm:1.0.0")
        )]));

        assert_eq!(
            manifest
                .resolve_bare(
                    "ui",
                    Path::new(".yarn/unplugged/wake/node_modules/wake/internal")
                )
                .unwrap(),
            PathBuf::from("../../cache/ui-v1/node_modules/ui")
        );
    }

    #[test]
    fn shared_location_virtual_locators_with_same_target_are_not_ambiguous() {
        let manifest = shared_location_manifest(serde_json::json!([
            wake_instance("file:../wake", None),
            wake_instance("virtual:first#file:../wake", Some("npm:1.0.0")),
            wake_instance("virtual:second#file:../wake", Some("npm:1.0.0"))
        ]));

        assert_eq!(
            manifest
                .resolve_bare(
                    "ui",
                    Path::new(".yarn/unplugged/wake/node_modules/wake/internal")
                )
                .unwrap(),
            PathBuf::from("../../cache/ui-v1/node_modules/ui")
        );
    }

    #[test]
    fn shared_location_virtual_locators_with_different_targets_are_ambiguous() {
        let manifest = shared_location_manifest(serde_json::json!([
            wake_instance("file:../wake", None),
            wake_instance("virtual:first#file:../wake", Some("npm:1.0.0")),
            wake_instance("virtual:second#file:../wake", Some("npm:2.0.0"))
        ]));

        assert_eq!(
            manifest.resolve_bare(
                "ui",
                Path::new(".yarn/unplugged/wake/node_modules/wake/internal")
            ),
            Err(PnpError::AmbiguousIssuer)
        );
    }

    #[test]
    fn shared_location_direct_dependency_wins_before_top_level_fallback() {
        let manifest = shared_location_manifest_with_top_level(
            serde_json::json!([
                wake_instance("file:../wake", None),
                wake_instance("virtual:peer#file:../wake", Some("npm:1.0.0"))
            ]),
            Some("npm:2.0.0"),
        );

        assert_eq!(
            manifest
                .resolve_bare(
                    "ui",
                    Path::new(".yarn/unplugged/wake/node_modules/wake/internal")
                )
                .unwrap(),
            PathBuf::from("../../cache/ui-v1/node_modules/ui")
        );
    }

    #[test]
    fn missing_direct_locator_is_not_hidden_by_top_level_fallback() {
        let manifest = shared_location_manifest_with_top_level(
            serde_json::json!([wake_instance("file:../wake", Some("npm:missing"))]),
            Some("npm:2.0.0"),
        );

        assert_eq!(
            manifest.resolve_bare(
                "ui",
                Path::new(".yarn/unplugged/wake/node_modules/wake/internal")
            ),
            Err(PnpError::MissingPackage)
        );
    }

    #[test]
    fn undeclared_dependency_errors() {
        let m = sample_manifest();
        assert_eq!(
            m.resolve_bare("left-pad", Path::new("src")),
            Err(PnpError::Undeclared)
        );
    }
}
