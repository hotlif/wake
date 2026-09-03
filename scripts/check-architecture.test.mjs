import assert from 'node:assert/strict'
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from '@crab-dev/wake/test'
import {
  parseCargoTreePackages,
  parseCargoLock,
  validateAdrs,
  validateCargoManifestSources,
  validateCargoProvenance,
  validateCargoTreeRules,
  validateYarnProvenance,
  validatePolicy,
  validateRepositorySources,
} from './check-architecture.mjs'

const decision = 'engineering/decisions/0001-architecture-evolution-loop.md'
const activeAdrRecords = new Map([
  ['0001-architecture-evolution-loop.md', { status: 'accepted' }],
  ['0003-compiler-and-shell-boundaries.md', { status: 'proposed' }],
  ['0010-shared-css-syntax-tree.md', { status: 'accepted' }],
  ['0020-react-browser-test-runtime.md', { status: 'proposed' }],
  ['0021-local-platform-package-links.md', { status: 'superseded' }],
  ['0022-yarn-pnp-ownership.md', { status: 'accepted' }],
  ['0025-wake-native-federation-contract.md', { status: 'accepted' }],
  ['0027-build-session-ownership-and-lifetime.md', { status: 'accepted' }],
  ['0028-build-generation-ownership-and-observation-cache.md', { status: 'accepted' }],
  ['0029-node-contract-and-federation-control-ownership.md', { status: 'accepted' }],
  ['0030-live-reload-capability-boundary.md', { status: 'accepted' }],
  ['0031-docs-page-identity-and-source-provenance.md', { status: 'accepted' }],
  ['0032-federation-development-snapshot-leases.md', { status: 'accepted' }],
  ['0033-structured-module-emit-provenance.md', { status: 'accepted' }],
  ['0034-transactional-persistent-cache-boundary.md', { status: 'accepted' }],
  ['0035-parser-owned-crab-runtime-resolution.md', { status: 'accepted' }],
  ['0036-input-disjoint-exact-output-transactions.md', { status: 'accepted' }],
  ['0037-typed-development-watch-and-candidate-generations.md', { status: 'accepted' }],
  ['0038-docs-generation-transaction.md', { status: 'accepted' }],
  ['0039-owned-immutable-filesystem-overlay.md', { status: 'accepted' }],
  ['0040-parser-owned-frozen-declaration-graph.md', { status: 'accepted' }],
  ['0043-react-module-compiler-boundary.md', { status: 'accepted' }],
])

function rustSources(directory, root = directory) {
  const sources = []
  for (const entry of readdirSync(directory)) {
    const path = join(directory, entry)
    const stats = statSync(path)
    if (stats.isDirectory()) {
      sources.push(...rustSources(path, root))
    } else if (stats.isFile() && entry.endsWith('.rs')) {
      sources.push({
        path,
        repositoryPath: relative(root, path).replaceAll('\\', '/'),
      })
    }
  }
  return sources
}

function beforeCfgTestModule(source, repositoryPath) {
  const marker = /^[ \t]*#\[cfg\(test\)\][ \t]*\r?\n[ \t]*(?:pub(?:\(crate\))?[ \t]+)?mod[ \t]+tests[ \t]*\{/m.exec(source)
  assert.ok(marker, `${repositoryPath} must retain an explicit cfg(test) module boundary`)
  return source.slice(0, marker.index)
}

test('generated JavaScript remains output-only at the bundler emit boundary', () => {
  const incremental = readFileSync(
    new URL('../crates/wake_bundler/src/incremental.rs', import.meta.url),
    'utf8',
  )
  const cache = readFileSync(new URL('../crates/wake_cache/src/lib.rs', import.meta.url), 'utf8')
  const typedCodegen = readFileSync(
    new URL('../crates/wake_ecma_codegen/src/typed.rs', import.meta.url),
    'utf8',
  )
  const typedModules = readFileSync(
    new URL('../crates/wake_ecma_minify/src/typed_modules.rs', import.meta.url),
    'utf8',
  )
  const optimizer = readFileSync(
    new URL('../crates/wake_ecma_minify/src/optimizer.rs', import.meta.url),
    'utf8',
  )

  for (const helper of [
    'is_pure_reg_body',
    'for_each_require',
    'redirect_require_targets',
    'strip_hoisted_requires_and_barrels',
    'compact_body_names',
    'exported_names',
    'reassigns_module_exports',
    'strip_standalone_requires',
    'replace_eager_discarded_static_requests',
    'bind_runtime_remote_imports',
  ]) {
    assert.doesNotMatch(
      incremental,
      new RegExp(`\\bfn\\s+${helper}\\b`),
      `${helper} must not recreate semantic facts from emitted JavaScript (ADR 0033)`,
    )
  }
  for (const spelling of ['module\\.exports', 'exports', '__wake_require__']) {
    assert.doesNotMatch(
      incremental,
      new RegExp(`\\.replace\\(\\s*["']${spelling}`),
      `factory bindings must be selected before codegen, not replaced in generated bodies`,
    )
  }
  assert.doesNotMatch(
    incremental,
    /\.contains\(\s*["']__wake_(?:interop_default|interop_star|require__\.metaUrl)/,
    'runtime capabilities must come from typed body metadata, never generated token scans',
  )

  for (const graphOwner of ['cyclic_module_ids', 'concat_cycle_source_ids', 'topo_sort_modules']) {
    assert.match(
      incremental,
      new RegExp(`fn\\s+${graphOwner}\\s*\\([\\s\\S]{0,320}retained_edges:\\s*&FxHashMap<u32, ModuleEdges>`),
      `${graphOwner} must consume optimizer-retained ModuleEdges`,
    )
  }
  assert.match(
    incremental,
    /struct EmitLinkerData\s*\{[\s\S]*?runtime_import_expose:\s*Option<String>/,
    'Federation expose identity must participate in typed body-linker identity',
  )
  assert.match(
    typedCodegen,
    /generated_module_requests\.push\(GeneratedModuleRequest\s*\{/,
    'the same typed token walk must emit exact request ranges',
  )
  assert.match(
    typedCodegen,
    /meta_url:\s*plan\.runtime_capabilities\(\)\.meta_url/,
    'metaUrl runtime installation must be a sealed typed capability',
  )
  assert.match(
    typedCodegen,
    /GeneratedModuleRuntimeNames\s*\{[\s\S]*?module:\s*emitted\(plan\.runtime\(\)\.module\.symbol\)[\s\S]*?exports:\s*emitted\(plan\.runtime\(\)\.exports\.symbol\)[\s\S]*?require:\s*emitted\(plan\.runtime\(\)\.internal_require\.symbol\)/,
    'wrapper parameters must come from the finalized typed symbol table',
  )
  assert.match(
    typedModules,
    /fn bind_bundled_wrapper_references\s*\([\s\S]{0,1800}occurrence\.symbol\(\)\.is_none\(\)[\s\S]{0,1800}set_name_symbol/,
    'only unresolved source references may bind to collision-free wrapper symbols',
  )
  assert.match(
    incremental,
    /runtime_names_by_id[\s\S]{0,600}GeneratedModuleRuntimeNames::is_canonical/,
    'concat must conservatively retain modules whose typed wrapper names are non-canonical',
  )
  assert.match(cache, /const SCHEMA:\s*u32\s*=\s*13;/)
  const requestDto = /pub struct CachedModuleRequest\s*\{[\s\S]*?\n\}/.exec(cache)
  assert.ok(requestDto, 'cache must publish the stable request DTO')
  assert.match(requestDto[0], /pub specifier:\s*String/)
  assert.doesNotMatch(
    requestDto[0], /target_module_id/,
    'persistent request DTOs must not carry traversal-local module IDs',
  )
  const runtimeNamesDto = /pub struct CachedModuleRuntimeNames\s*\{[\s\S]*?\n\}/.exec(cache)
  assert.ok(runtimeNamesDto, 'cache must persist typed wrapper names with body metadata')
  assert.match(runtimeNamesDto[0], /pub module:\s*String/)
  assert.match(runtimeNamesDto[0], /pub exports:\s*String/)
  assert.match(runtimeNamesDto[0], /pub require:\s*String/)
  assert.match(runtimeNamesDto[0], /pub capabilities:\s*CachedModuleRuntimeCapabilities/)
  const runtimeCapabilitiesDto =
    /pub struct CachedModuleRuntimeCapabilities\s*\{[\s\S]*?\n\}/.exec(cache)
  assert.ok(runtimeCapabilitiesDto, 'cache must persist the closed typed runtime capability set')
  for (const capability of [
    'meta_url',
    'external_require',
    'promise_resolve',
    'object_assign',
    'object_keys',
    'object_define_property',
    'runtime_import',
    'shared',
  ]) {
    assert.match(runtimeCapabilitiesDto[0], new RegExp(`pub ${capability}:\\s*bool`))
  }
  assert.match(
    cache,
    /pub struct CachedModuleMappings\s*\{[\s\S]*?pub runtime_names:\s*CachedModuleRuntimeNames/,
    'runtime names and generated request facts must share the body metadata owner',
  )
  assert.match(
    optimizer,
    /pub const PIPELINE_VERSION:\s*&str\s*=\s*"wake-closure-minifier-v15"/,
    'changing retained request identity must invalidate optimized artifacts',
  )
  assert.doesNotMatch(
    typedModules,
    /TypedModuleRequestKind::DynamicImport\s*\)[\s\S]{0,240}TypedModuleRequestKind::StaticImport/,
    'bundled request resolution must not fall back from dynamic to static request identity',
  )
  assert.doesNotMatch(
    typedModules,
    /specifier_rewrites:\s*BTreeMap<String,\s*String>/,
    'bundled specifier rewrites must retain request kind',
  )
  assert.doesNotMatch(
    typedModules,
    /factory\.global\(\s*["'](?:require|Promise|Object)["']\s*\)/,
    'bundled compiler intrinsics must use typed runtime services instead of capturable globals',
  )
  assert.match(
    typedModules,
    /fn preserved_host_global\s*\([\s\S]{0,520}plan\.mode\s*==\s*TypedModuleMode::BundledCommonJs[\s\S]{0,520}factory\.global\(name\)/,
    'the only host-global escape hatch must reject bundled finalization',
  )
  assert.doesNotMatch(
    incremental,
    /body\.is_empty\(\)\s*\}\s*else\s*\{\s*"function/,
    'factory async classification must never be inferred from generated body emptiness',
  )
  const chunk = readFileSync(new URL('../crates/wake_bundler/src/chunk.rs', import.meta.url), 'utf8')
  assert.match(
    chunk,
    /struct ModuleEdges\s*\{[\s\S]*?requests:\s*Vec<ResolvedModuleRequest>/,
    'the final graph owner must retain source-ordered request kind and target identity',
  )
  for (const graphConsumer of [
    'live_modules',
    'css_emission_order',
    'namespace_identity_module_ids',
    'async_module_ids_with_edges',
    'retained_module_edges',
    'dyn_chunks_of',
  ]) {
    assert.match(
      incremental,
      new RegExp(`fn\\s+${graphConsumer}\\s*\\([\\s\\S]{0,420}ModuleEdges`),
      `${graphConsumer} must consume the final typed ModuleEdges owner`,
    )
  }
})

test('persistent cache remains derived, transactional, and non-fatal', () => {
  const cache = readFileSync(
    new URL('../crates/wake_cache/src/lib.rs', import.meta.url),
    'utf8',
  )
  const incremental = readFileSync(
    new URL('../crates/wake_bundler/src/incremental.rs', import.meta.url),
    'utf8',
  )
  const loader = readFileSync(
    new URL('../crates/wake_bundler/src/loader.rs', import.meta.url),
    'utf8',
  )
  const app = readFileSync(new URL('../crates/wake_app/src/lib.rs', import.meta.url), 'utf8')
  const cli = readFileSync(new URL('../crates/wake_cli/src/main.rs', import.meta.url), 'utf8')

  assert.match(cache, /const SCHEMA:\s*u32\s*=\s*13;/)
  assert.match(cache, /const HEADER_LEN:\s*usize\s*=\s*32;/)
  for (const outcome of ['Loaded', 'Missing', 'Incompatible', 'Corrupt', 'Io']) {
    assert.match(cache, new RegExp(`\\b${outcome}\\b`), `cache load must retain ${outcome}`)
  }
  assert.match(cache, /fn envelope_checksum[\s\S]{0,320}hasher\.update\(prefix\)[\s\S]{0,160}hasher\.update\(payload\)/)
  assert.match(cache, /fn acquire_lock[\s\S]{0,360}try_lock\(\)[\s\S]{0,520}timed out waiting for persistent cache lock/)
  assert.match(cache, /NamedTempFile::new_in\(parent\)[\s\S]{0,520}sync_all\(\)[\s\S]{0,520}persist\(path\)/)
  assert.match(cache, /fn put_emission\s*\([\s\S]{0,900}authored_bodies\.insert\(key\)[\s\S]{0,160}authored_mappings\.insert\(key\)/)
  assert.doesNotMatch(cache, /pub fn put_(?:body|mappings)\b/)
  assert.doesNotMatch(
    cache,
    /\b(?:FileStamp|CachedSource|PathEntry)\b|pub fn (?:cached_source|put_source)\b/,
    'persistent storage must not own source/path snapshots',
  )

  assert.match(
    incremental,
    /CacheLoadOutcome::Missing\s*\|\s*CacheLoadOutcome::Incompatible[\s\S]{0,160}BuildCache::new\(\),\s*None/,
    'missing and incompatible cache files must remain silent misses',
  )
  assert.match(incremental, /Diagnostic::warning\([\s\S]{0,240}with_code\("WAKE_CACHE"\)/)
  assert.match(incremental, /self\.one_shot\s*&&\s*!self\.load_cache_enabled/)
  assert.match(incremental, /cache\.put_emission\(/)
  assert.doesNotMatch(incremental, /cache\.put_(?:body|mappings)\(/)
  assert.doesNotMatch(
    `${incremental}\n${loader}`,
    /\b(?:FileStamp|CachedSource|persistent_source_variant|cached_source_type|put_source)\b/,
    'fresh processes must always read and hash real loader output',
  )

  const cachePath = /fn persistent_cache_path\s*\([\s\S]{0,420}\n\}/.exec(app)?.[0]
  assert.ok(cachePath, 'wake_app must retain one cache-path helper')
  assert.match(cachePath, /enabled\.then\(/)
  assert.doesNotMatch(cachePath, /create_dir/)

  const rebuildStart = cli.indexOf('fn execute_build_watch_rebuild(')
  const rebuildEnd = cli.indexOf('fn next_watch_retry_delay(', rebuildStart)
  assert.ok(
    rebuildStart >= 0 && rebuildEnd > rebuildStart,
    'build watch success/diagnostic boundary must exist',
  )
  const watch = cli.slice(rebuildStart, rebuildEnd)
  assert.equal(
    [...watch.matchAll(/record_successful_build_diagnostics\(/g)].length,
    1,
    'the shared TUI success branch must retain diagnostics for every rebuild',
  )
  assert.match(watch, /if dashboard\.is_some\(\)[\s\S]*?record_successful_build_diagnostics/)
  assert.match(watch, /else if initial[\s\S]*?ui\.build_result\("Initial build completed"/)
  assert.match(watch, /for diagnostic in &result\.diagnostics[\s\S]{0,120}ui\.diagnostic\(diagnostic\)/)
})

test('Rust product, integration, and benchmark sources use only the BuildSession owner', () => {
  const cratesRoot = fileURLToPath(new URL('../crates/', import.meta.url))
  const violations = []
  for (const { path, repositoryPath } of rustSources(cratesRoot)) {
    // The engine implementation and its unit-level proofs remain inside the owning crate. Public
    // integration tests, examples, and benches are deliberately outside this sole exemption.
    if (repositoryPath.startsWith('wake_bundler/src/')) continue
    const source = readFileSync(path, 'utf8')
    for (const [contract, pattern] of [
      ['IncrementalBundler', /\bIncrementalBundler\b/],
      ['BuildSession::from_incremental', /\bBuildSession\s*::\s*from_incremental\b/],
    ]) {
      const match = pattern.exec(source)
      if (match) {
        const line = source.slice(0, match.index).split(/\r?\n/).length
        violations.push(`${repositoryPath}:${line}: ${contract}`)
      }
    }
  }
  assert.deepEqual(
    violations,
    [],
    'product callers must construct typed retained/one-shot BuildSession values (ADR 0027)',
  )
})

test('wake_app Federation production candidates use one BuildGeneration owner', () => {
  const generationOwner = readFileSync(
    new URL('../crates/wake_bundler/src/generation.rs', import.meta.url),
    'utf8',
  )
  const bundlerRoot = readFileSync(
    new URL('../crates/wake_bundler/src/lib.rs', import.meta.url),
    'utf8',
  )
  const app = readFileSync(new URL('../crates/wake_app/src/lib.rs', import.meta.url), 'utf8')
  const federationPath = 'crates/wake_app/src/federation.rs'
  const federation = beforeCfgTestModule(
    readFileSync(new URL(`../${federationPath}`, import.meta.url), 'utf8'),
    federationPath,
  )

  assert.match(generationOwner, /\bpub struct BuildGeneration\s*\{/)
  assert.match(generationOwner, /\bpub fn retained_session\s*\(/)
  assert.match(generationOwner, /\bpub fn build_once\s*\(\s*&mut self\b/)
  assert.match(
    bundlerRoot,
    /pub\s+use\s+generation::(?:BuildGeneration|\{[^}]*\bBuildGeneration\b[^}]*\})\s*;/,
    'wake_bundler must expose the sole BuildGeneration product owner (ADR 0028)',
  )
  assert.match(
    app,
    /struct\s+ProjectBuildSession\s*\{[\s\S]*?\bgeneration:\s*BuildGeneration\s*,[\s\S]*?\bapplication:\s*BuildSession\s*,[\s\S]*?\}/,
    'a retained application context must store its BuildGeneration owner beside its session',
  )
  assert.match(
    app,
    /session\s*\.\s*generation\s*\.\s*advance_generation\s*\(\s*\)\s*;[\s\S]*?session\s*\.\s*application\s*\.\s*invalidate/,
    'a watcher batch must advance the observation generation before invalidating the application',
  )
  assert.match(federation, /\bgeneration:\s*&mut\s+BuildGeneration\b/)
  const transientBuilds = federation.match(/\bgeneration\s*\.\s*build_once\s*\(/g) ?? []
  assert.ok(
    transientBuilds.length >= 2,
    'container and shared-provider production views must be generation-owned one-shot builds',
  )
  assert.doesNotMatch(
    federation,
    /\bBuildSession\s*::\s*(?:new|new_one_shot|from_incremental)\s*\(/,
    'production Federation code must not construct a BuildSession outside BuildGeneration',
  )
  const binder = /pub\(super\) fn bind_production_generation\s*\([\s\S]*?\n\}/.exec(federation)?.[0]
  assert.ok(binder, 'Federation must expose one read-only generation binder')
  assert.match(binder, /generation_fs:\s*Arc<dyn FileSystem>/)
  assert.match(binder, /load_production_lock_from_fs\(prepared, generation_fs\)/)
  const artifacts = /pub\(super\) fn build_artifacts\s*\([\s\S]*?\n\}/.exec(federation)?.[0]
  assert.ok(artifacts, 'Federation artifact materialization must remain inspectable')
  assert.doesNotMatch(
    artifacts,
    /load_production_lock(?:_from_fs)?\s*\(/,
    'artifact materialization must consume the lock captured by the generation binder',
  )
})

test('wake_dev_server generation owns Federation and diagnostic source views', () => {
  const devServerPath = 'crates/wake_dev_server/src/lib.rs'
  const devServer = beforeCfgTestModule(
    readFileSync(new URL(`../${devServerPath}`, import.meta.url), 'utf8'),
    devServerPath,
  )
  const app = readFileSync(new URL('../crates/wake_app/src/lib.rs', import.meta.url), 'utf8')
  const pnpFileSystem = readFileSync(
    new URL('../crates/wake_resolver/src/pnpfs.rs', import.meta.url),
    'utf8',
  )

  assert.match(
    devServer,
    /struct\s+MountBuildSession\s*\{[\s\S]*?\bgeneration:\s*BuildGeneration\s*,[\s\S]*?\bsession:\s*BuildSession\s*,[\s\S]*?\}/,
    'a development mount must retain its BuildGeneration owner beside its BuildSession',
  )
  assert.match(
    devServer,
    /fn\s+invalidate_filesystem\s*\([\s\S]{0,500}?generation\.advance_generation\(\)[\s\S]{0,300}?session\.invalidate_filesystem\(\)/,
    'full development invalidation must advance the filesystem epoch and retained session together',
  )
  assert.match(
    devServer,
    /fn\s+invalidate_paths\s*\([\s\S]{0,500}?generation\.advance_generation\(\)[\s\S]{0,300}?session\.invalidate_paths\(/,
    'path development invalidation must advance the filesystem epoch and retained session together',
  )
  assert.match(
    devServer,
    /fn\s+build_current_generation\s*\([\s\S]{0,700}?session\.file_system_view\(\)[\s\S]{0,300}?session\.build_current_ref\([\s\S]{0,200}?\(output, file_system\)/,
    'the mount owner must return the runtime output with its exact decorated generation filesystem view',
  )
  assert.match(
    devServer,
    /let\s*\(out, generation_fs\)\s*=\s*session\.build_current_generation\([\s\S]{0,4000}?FederationSnapshot::assemble\(\s*&spec\.federation,\s*out,\s*generation_fs,/,
    'Federation assembly must receive the same generation view paired with the runtime build',
  )
  assert.match(
    pnpFileSystem,
    /fn\s+read_to_string\s*\([\s\S]{0,900}?None\s*=>\s*self\.inner\.read_to_string\(&physical\)/,
    'ordinary PnP text reads must preserve the generation read_to_string query family',
  )
  assert.match(
    devServer,
    /pub\s+struct\s+DiagnosticSource\s*\{\s*pub\s+path:\s*PathBuf,\s*pub\s+text:\s*String,\s*\}/,
    'development diagnostics must carry source bytes across the event boundary',
  )
  assert.match(
    devServer,
    /capture_diagnostic_sources\(&diagnostics,\s*generation_fs\.as_ref\(\)\)[\s\S]{0,1000}?ServerEvent::Diagnostics\s*\{\s*diagnostics,\s*sources,\s*\}/,
    'build failures must capture sources through the runtime build generation view',
  )
  assert.match(
    app,
    /ServerEvent::Diagnostics\s*\{\s*diagnostics,\s*sources,\s*\}\s*=>\s*\{[\s\S]{0,300}?diagnostic_infos_from_captured_sources\(&diagnostics,\s*sources\)/,
    'the application event adapter must consume generation-captured diagnostic sources',
  )
  const capturedDiagnosticAdapter =
    /fn\s+diagnostic_infos_from_captured_sources\s*\([\s\S]*?\n\}\r?\n\r?\n#\[derive/.exec(app)?.[0]
  assert.ok(capturedDiagnosticAdapter, 'the captured diagnostic adapter must remain inspectable')
  assert.doesNotMatch(
    capturedDiagnosticAdapter,
    /\bread_to_string\s*\(|\bstd::fs\b|\bOsFileSystem\b/,
    'event consumers must not reopen a mutable filesystem path for diagnostic locations',
  )
})

test('Rust and npm expose exact output-kind and Federation error-code sets', () => {
  const outputOwner = readFileSync(
    new URL('../crates/wake_app/src/output.rs', import.meta.url),
    'utf8',
  )
  const nodeTypes = readFileSync(new URL('../npm/wake/index.d.ts', import.meta.url), 'utf8')
  const rustOutputKinds = [
    ...outputOwner.matchAll(/Self::[A-Za-z]+\s*=>\s*"([a-z-]+)"/g),
  ].map((match) => match[1])
  const outputType = /export type OutputFileKind\s*=([\s\S]*?)\n\nexport interface OutputFile/.exec(nodeTypes)
  assert.ok(outputType, 'index.d.ts must publish the closed OutputFileKind union')
  const typescriptOutputKinds = [...outputType[1].matchAll(/'([^']+)'/g)].map((match) => match[1])
  assert.equal(new Set(rustOutputKinds).size, rustOutputKinds.length, 'Rust output wire names must be unique')
  assert.equal(
    new Set(typescriptOutputKinds).size,
    typescriptOutputKinds.length,
    'TypeScript output wire names must be unique',
  )
  assert.deepEqual(
    [...typescriptOutputKinds].sort(),
    [...rustOutputKinds].sort(),
    'OutputFileKind must not drift across Rust serialization and the npm declaration',
  )

  const rustErrors = readFileSync(
    new URL('../crates/wake_federation_contract/src/error.rs', import.meta.url),
    'utf8',
  )
  const federationRuntime = readFileSync(
    new URL('../npm/wake/federation.mjs', import.meta.url),
    'utf8',
  )
  const federationTypes = readFileSync(
    new URL('../npm/wake/federation.d.mts', import.meta.url),
    'utf8',
  )
  const rustErrorCodes = [...rustErrors.matchAll(/Self::[A-Za-z]+\s*=>\s*"(FED_[A-Z_]+)"/g)]
    .map((match) => match[1])
  const runtimeBlock = /const FEDERATION_ERROR_CODES\s*=\s*Object\.freeze\(\{([\s\S]*?)\}\)/.exec(federationRuntime)
  const typeBlock = /FEDERATION_ERROR_CODES:\s*Readonly<\{([\s\S]*?)\}>/.exec(federationTypes)
  assert.ok(runtimeBlock, 'Federation runtime must publish one literal error-code table')
  assert.ok(typeBlock, 'Federation declarations must publish one literal error-code table')
  const runtimeErrorCodes = [...runtimeBlock[1].matchAll(/:\s*'(FED_[A-Z_]+)'/g)]
    .map((match) => match[1])
  const typeErrorCodes = [...typeBlock[1].matchAll(/:\s*'(FED_[A-Z_]+)'/g)]
    .map((match) => match[1])
  for (const [owner, values] of [
    ['Rust', rustErrorCodes],
    ['runtime', runtimeErrorCodes],
    ['TypeScript', typeErrorCodes],
  ]) {
    assert.equal(new Set(values).size, values.length, `${owner} Federation error codes must be unique`)
  }
  assert.deepEqual([...runtimeErrorCodes].sort(), [...rustErrorCodes].sort())
  assert.deepEqual([...typeErrorCodes].sort(), [...rustErrorCodes].sort())
  assert.match(nodeTypes, /\|\s*FederationContractErrorCode/)
  for (const code of ['WAKE_FED_INIT_CONFIG', 'WAKE_FED_INIT_IO', 'WAKE_FED_INIT_CONFLICT']) {
    assert.match(nodeTypes, new RegExp(`\\|\\s*'${code}'`))
  }
  const applicationErrors = [
    readFileSync(new URL('../crates/wake_app/src/lib.rs', import.meta.url), 'utf8'),
    readFileSync(new URL('../crates/wake_app/src/output.rs', import.meta.url), 'utf8'),
  ].join('\n')
  for (const code of ['WAKE_OUTPUT_COLLISION', 'WAKE_WATCH_SNAPSHOT_CHANGED']) {
    assert.match(applicationErrors, new RegExp(`WakeError::new\\(\\s*"${code}"`))
    assert.match(nodeTypes, new RegExp(`\\|\\s*'${code}'`))
  }
})

test('Federation declarations stay parser-owned, frozen, and bind without text rescans', () => {
  const parser = readFileSync(
    new URL('../crates/wake_ecma_parser/src/declaration.rs', import.meta.url),
    'utf8',
  )
  const parserCore = readFileSync(
    new URL('../crates/wake_ecma_parser/src/lib.rs', import.meta.url),
    'utf8',
  )
  const tsdoc = beforeCfgTestModule(
    readFileSync(new URL('../crates/wake_tsdoc/src/lib.rs', import.meta.url), 'utf8'),
    'crates/wake_tsdoc/src/lib.rs',
  )
  const federationTypes = beforeCfgTestModule(
    readFileSync(new URL('../crates/wake_app/src/federation_types.rs', import.meta.url), 'utf8'),
    'crates/wake_app/src/federation_types.rs',
  )
  const federationSync = beforeCfgTestModule(
    readFileSync(new URL('../crates/wake_app/src/federation_type_sync.rs', import.meta.url), 'utf8'),
    'crates/wake_app/src/federation_type_sync.rs',
  )
  const devFederation = beforeCfgTestModule(
    readFileSync(new URL('../crates/wake_dev_server/src/federation.rs', import.meta.url), 'utf8'),
    'crates/wake_dev_server/src/federation.rs',
  )

  assert.match(parserCore, /declaration:\s*Option<declaration::DeclarationCollector/)
  assert.match(parser, /pub fn parse_declaration_facts[\s\S]{0,500}parse_with_collector/)
  assert.match(parser, /pub struct DeclarationFacts\b/)
  assert.match(parser, /pub enum DeclarationImportUsage\s*\{/)
  for (const usage of ['TypeOnly', 'ReferencedValue', 'RuntimeSideEffect']) {
    assert.match(parser, new RegExp(`\\b${usage}\\b`))
  }
  assert.doesNotMatch(parser, /\bstruct\s+Analyzer\b|\bfn\s+tokenize\b/)

  assert.match(tsdoc, /pub struct FrozenDeclarationGraph\b/)
  assert.match(tsdoc, /pub trait DeclarationFileSystem\b/)
  assert.match(tsdoc, /pub fn render_ambient_with\b/)
  assert.match(tsdoc, /let facts = parse_declaration_facts\(&source, source_type\)/)
  assert.match(tsdoc, /DeclarationImportUsage::RuntimeSideEffect/)
  assert.match(
    tsdoc,
    /if runtime_side_effect && !is_declaration_file_path\(&path\)/,
    'runtime resource imports from implementation sources must not enter the declaration graph',
  )

  assert.match(federationTypes, /struct FrozenFederationTypes\b/)
  assert.match(federationTypes, /prepare_library_declarations_with_file_system/)
  assert.match(federationTypes, /\.render_ambient_with\(/)
  assert.match(federationSync, /wake_tsdoc::validate_ambient_declaration_body/)
  for (const retired of [
    'emit_federation_types',
    'rebind_federation_type_output',
    'rewrite_relative_specifiers',
    'is_module_string_context',
    'quoted_end',
    'contains_any_keyword',
    'normalize_ambient_body',
    'validate_module_body',
  ]) {
    const declaration = new RegExp('\\bfn\\s+' + retired + '\\b')
    assert.doesNotMatch(federationTypes, declaration)
    assert.doesNotMatch(federationSync, declaration)
  }

  assert.match(devFederation, /pub struct FederationTypeGeneration\b/)
  assert.doesNotMatch(
    devFederation,
    /canonical_type_identity|type_identity_placeholder|rebind_federation_type_output/,
  )
})

test('filesystem canonical identity follows the active immutable generation', () => {
  const common = beforeCfgTestModule(
    readFileSync(new URL('../crates/wake_common/src/fs.rs', import.meta.url), 'utf8'),
    'crates/wake_common/src/fs.rs',
  )
  const generation = beforeCfgTestModule(
    readFileSync(new URL('../crates/wake_bundler/src/generation.rs', import.meta.url), 'utf8'),
    'crates/wake_bundler/src/generation.rs',
  )
  const pnp = beforeCfgTestModule(
    readFileSync(new URL('../crates/wake_resolver/src/pnpfs.rs', import.meta.url), 'utf8'),
    'crates/wake_resolver/src/pnpfs.rs',
  )
  const federationTypes = beforeCfgTestModule(
    readFileSync(new URL('../crates/wake_app/src/federation_types.rs', import.meta.url), 'utf8'),
    'crates/wake_app/src/federation_types.rs',
  )

  assert.match(
    common,
    /pub trait FileSystem[\s\S]{0,320}fn canonicalize\(&self, path: &Path\) -> io::Result<PathBuf>;/,
    'logical canonicalization must be required of every filesystem implementation',
  )
  const overlayCanonicalize =
    /impl FileSystem for OwnedOverlayFileSystem\s*\{\s*fn canonicalize[\s\S]*?\n    fn read_to_string/.exec(common)?.[0]
  assert.ok(overlayCanonicalize, 'the owned overlay canonical identity boundary must remain inspectable')
  assert.match(overlayCanonicalize, /if let Some\(relative\) = self\.owned_relative\(&normalized\)/)
  assert.match(overlayCanonicalize, /return Ok\(normalized\)/)
  assert.match(overlayCanonicalize, /return Err\(path_error\(/)
  assert.match(overlayCanonicalize, /match self\.base\.canonicalize\(path\)/)
  assert.ok(
    overlayCanonicalize.indexOf('return Err(path_error(') <
      overlayCanonicalize.indexOf('match self.base.canonicalize(path)'),
    'owned-root misses must fail closed before the base filesystem can be consulted',
  )

  assert.match(generation, /struct GenerationState\s*\{[\s\S]*?canonical_paths:\s*QueryCache<CachedIo<PathBuf>>/)
  assert.match(
    generation,
    /impl FileSystem for GenerationFileSystem[\s\S]{0,520}state\.canonical_paths\.cell\(path\)[\s\S]{0,220}self\.shared\.source\.canonicalize\(path\)/,
    'canonical identities must be replayed from the same generation epoch as file contents',
  )
  assert.match(
    pnp,
    /impl FileSystem for PnpFileSystem[\s\S]{0,900}if physical == logical\s*\{\s*Ok\(canonical\)\s*\}\s*else\s*\{\s*Ok\(logical\)/,
    'PnP projections must validate physical content without leaking its path identity',
  )
  assert.match(
    federationTypes,
    /impl wake_tsdoc::DeclarationFileSystem for GenerationDeclarationFileSystem[\s\S]{0,300}self\.file_system\s*\.canonicalize\(path\)/,
    'Federation declarations must canonicalize through their generation filesystem',
  )
})

test('Node owns exact closed request and Docs response DTOs', () => {
  const node = readFileSync(new URL('../crates/wake_node/src/lib.rs', import.meta.url), 'utf8')
  const nodeTypes = readFileSync(new URL('../npm/wake/index.d.ts', import.meta.url), 'utf8')
  const nodeCli = readFileSync(new URL('../npm/wake/bin/wake.mjs', import.meta.url), 'utf8')

  const rustFields = (name) => {
    const block = new RegExp(`struct ${name}\\s*\\{([\\s\\S]*?)\\n\\}`).exec(node)
    assert.ok(block, `wake_node must own ${name}`)
    assert.match(
      node.slice(Math.max(0, block.index - 100), block.index),
      /#\[serde\(default, rename_all = "camelCase", deny_unknown_fields\)\]/,
      `${name} must be a closed camelCase boundary`,
    )
    return [...block[1].matchAll(/^\s+([a-z_]+):/gm)].map((match) => match[1]).sort()
  }
  const expectedRustFields = new Map([
    ['RawBuildOptions', ['cache', 'config_path', 'cwd', 'entry', 'federation', 'outdir', 'source_map']],
    ['RawBundleOptions', ['cache', 'config_path', 'cwd', 'entry', 'external', 'format', 'minify', 'outfile', 'platform', 'source_map', 'target']],
    ['RawGenerateCssTokenOptions', ['config_path', 'cwd']],
    ['RawGenerateDocgenOptions', ['cwd', 'entry']],
    ['RawLibraryBuildOptions', ['cwd', 'entry']],
    ['RawDocsOptions', ['base_path', 'config_path', 'cwd', 'mode', 'outdir']],
    ['RawApplicationDevServerOptions', ['config_path', 'cwd', 'entry', 'federation', 'host', 'open', 'port']],
    ['RawDocsDevServerOptions', ['config_path', 'cwd', 'host', 'mode', 'open', 'port']],
    ['RawTestOptions', [
      'allow_no_tests',
      'bail',
      'browser_path',
      'changed',
      'coverage',
      'environment',
      'headful',
      'name_pattern',
      'patterns',
      'projects',
      'related',
      'root',
      'seed',
      'serial',
      'shard',
      'shuffle',
      'update_snapshots',
      'workers',
    ]],
  ])
  for (const [name, fields] of expectedRustFields) {
    assert.deepEqual(rustFields(name), [...fields].sort(), `${name} field set drifted`)
  }

  const typeFields = (name) => {
    const block = new RegExp(`export interface ${name}[^\\{]*\\{([\\s\\S]*?)\\n\\}`).exec(nodeTypes)
    assert.ok(block, `index.d.ts must publish ${name}`)
    return [...block[1].matchAll(/^\s+([A-Za-z][A-Za-z0-9]*)\??:/gm)]
      .map((match) => match[1])
      .sort()
  }
  assert.deepEqual(typeFields('DevServerOptions'), [
    'entry',
    'federation',
    'host',
    'open',
    'port',
  ])
  assert.deepEqual(typeFields('DocsDevServerOptions'), ['host', 'mode', 'open', 'port'])
  const testOptionFields = [
    'allowNoTests',
    'bail',
    'browserPath',
    'changed',
    'coverage',
    'environment',
    'headful',
    'namePattern',
    'patterns',
    'projects',
    'related',
    'root',
    'seed',
    'serial',
    'shard',
    'shuffle',
    'updateSnapshots',
    'workers',
  ].sort()
  assert.deepEqual(typeFields('TestOptions'), testOptionFields)

  const nativeOptions = /const nativeOptions = \{([\s\S]*?)\n    \}/.exec(nodeCli)?.[1]
  assert.ok(nativeOptions, 'the CLI native test request must remain inspectable')
  const cliNativeOptionFields = [...nativeOptions.matchAll(/^\s+([A-Za-z][A-Za-z0-9]*)(?:,|:\s)/gm)]
    .map((match) => match[1])
    .sort()
  assert.deepEqual(
    cliNativeOptionFields,
    testOptionFields,
    'CLI-only watch/reporter/output state must not cross the closed native test boundary',
  )

  assert.match(node, /struct NodeDocsRoute\b/)
  assert.match(node, /struct NodeDocsBuildResult\b/)
  const buildDocs = /pub fn build_docs\s*\([\s\S]*?\n\}/.exec(node)?.[0]
  assert.ok(buildDocs, 'wake_node must retain an inspectable buildDocs boundary')
  assert.match(buildDocs, /node_docs_result_value\(&result\)/)
  assert.doesNotMatch(buildDocs, /node_result_value\(&result/)
})

test('Node boundary unions, runtime exports, and event contracts remain exact', () => {
  const node = readFileSync(new URL('../crates/wake_node/src/lib.rs', import.meta.url), 'utf8')
  const testContract = readFileSync(
    new URL('../crates/wake_test_contract/src/lib.rs', import.meta.url),
    'utf8',
  )
  const testProtocol = readFileSync(
    new URL('../crates/wake_test_contract/src/protocol.rs', import.meta.url),
    'utf8',
  )
  const app = readFileSync(new URL('../crates/wake_app/src/lib.rs', import.meta.url), 'utf8')
  const commonJs = readFileSync(new URL('../npm/wake/index.cjs', import.meta.url), 'utf8')
  const esm = readFileSync(new URL('../npm/wake/index.mjs', import.meta.url), 'utf8')
  const nodeTypes = readFileSync(new URL('../npm/wake/index.d.ts', import.meta.url), 'utf8')
  const nodeErrors = readFileSync(new URL('../npm/wake/errors.cjs', import.meta.url), 'utf8')
  const nodeRequestBoundary = beforeCfgTestModule(node, 'crates/wake_node/src/lib.rs')

  const structFields = (source, name) => {
    const block = new RegExp(`struct ${name}\\s*\\{([\\s\\S]*?)\\n\\}`).exec(source)
    assert.ok(block, `${name} must remain inspectable`)
    return [...block[1].matchAll(/^\s+([a-z_]+):/gm)].map((match) => match[1]).sort()
  }
  const federationDtos = new Map([
    ['RawNodeFederationEnabledOptions', ['enabled', 'exposes', 'name', 'remotes', 'shared']],
    ['RawNodeFederationDisabledOptions', ['enabled']],
    ['RawNodeFederationRemoteOptions', ['allowed_origins', 'dev_follow', 'manifest_url']],
    ['RawNodeFederationExposeOptions', ['allow_global_css', 'entry', 'mode', 'scope', 'shadow']],
    ['RawNodeFederationSharedOptions', [
      'coherence_group',
      'fallback',
      'owner',
      'required_version',
      'scope',
      'singleton',
      'strict',
    ]],
  ])
  for (const [name, fields] of federationDtos) {
    assert.deepEqual(structFields(node, name), [...fields].sort(), `${name} field set drifted`)
    const declaration = new RegExp(
      `#\\[serde\\([^\\]]*rename_all = "camelCase"[^\\]]*deny_unknown_fields[^\\]]*\\)\\]\\s*struct ${name}`,
    )
    assert.match(node, declaration, `${name} must reject unknown and snake_case fields`)
  }
  assert.match(
    node,
    /federation:\s*Option<RawNodeFederationOptions>/,
    'Node build and development inputs must use the Node-owned Federation union',
  )
  assert.doesNotMatch(node, /federation:\s*Option<wake_app::FederationOptions>/)
  assert.doesNotMatch(
    node,
    /struct RawNodeFederation(?:Remote|Expose|Shared)Options[\s\S]*?#\[serde\(alias/,
    'Node Federation DTOs must not inherit TOML snake_case aliases',
  )
  assert.match(
    node,
    /#\[serde\(rename_all = "camelCase", deny_unknown_fields\)\]\s*struct RawNodeWatchControlFields/,
  )
  assert.match(
    testProtocol,
    /#\[serde\(rename_all = "camelCase", deny_unknown_fields\)\]\s*struct RawWatchControl/,
  )

  assert.equal(
    [...nodeRequestBoundary.matchAll(/serde_json::from_str/g)].length,
    1,
    'all Node-owned JSON requests must enter through the single non-null deserializer',
  )
  assert.match(nodeRequestBoundary, /let value = serde_json::from_str::<Value>\(json\)/)
  assert.match(
    nodeRequestBoundary,
    /if let Some\(pointer\) = explicit_null_pointer\(&value, &mut String::new\(\)\)/,
  )
  assert.match(nodeRequestBoundary, /serde_json::from_value\(value\)/)
  assert.match(
    nodeRequestBoundary,
    /explicit null is not allowed in a Node request at \{pointer\}; omit the field to use its default/,
  )
  for (const owner of [
    'RawFederationProjectOptions',
    'RawBuildOptions',
    'RawGenerateCssTokenOptions',
    'RawGenerateDocgenOptions',
    'RawLibraryBuildOptions',
    'RawBundleOptions',
    'RawApplicationDevServerOptions',
    'RawDocsDevServerOptions',
    'RawDocsOptions',
  ]) {
    assert.match(
      nodeRequestBoundary,
      new RegExp(
        `impl ${owner}\\s*\\{\\s*fn parse\\([^)]*\\)[^{]*\\{\\s*parse_optional_node_request\\(value, "WAKE_CONFIG"\\)`,
      ),
      `${owner} must use the unified Node request deserializer`,
    )
  }
  assert.match(
    nodeRequestBoundary,
    /fn parse_test_options\([\s\S]{0,320}parse_optional_node_request::<RawTestOptions>\(options_json, "WAKE_TEST_CONFIG"\)/,
  )
  assert.match(
    nodeRequestBoundary,
    /deserialize_node_request::<RawNodeWatchControl>\(&control_json\)/,
  )
  assert.match(nodeErrors, /if \(options === undefined\) return \[\{\}, undefined\]/)
  assert.match(nodeErrors, /options === null \|\| typeof options !== 'object'/)
  assert.match(nodeErrors, /if \(signal === null\)/)
  assert.doesNotMatch(nodeErrors, /options == null/)
  assert.match(nodeErrors, /options\.cause !== undefined \? \{ cause: options\.cause \} : undefined/)
  assert.match(nodeTypes, /export class WakeError extends Error \{\s+constructor\(code: WakeErrorCode, message: string, options\?: WakeErrorOptions\)/)
  assert.match(nodeTypes, /export function bundle\(options\?: BundleOptions\): Promise<BundleResult>/)
  assert.match(commonJs, /const INTERNAL_CONTEXT_CONSTRUCTOR = Symbol\(/)
  assert.match(commonJs, /function assertInternalContextConstructor\(token, name, factory\)/)
  for (const className of ['BuildContext', 'DevServer', 'TestContext']) {
    const declaration = new RegExp(
      `export class ${className}(?: extends EventEmitter)? \\{([\\s\\S]*?)^\\}`,
      'm',
    ).exec(nodeTypes)
    assert.ok(declaration, `${className} declaration must remain inspectable`)
    assert.match(declaration[1], /^\s+private constructor\(\)/m, `${className} must remain factory-owned`)
    assert.match(
      commonJs,
      new RegExp(`class ${className}[\\s\\S]*?constructor\\(handle, token\\) \\{[\\s\\S]{0,120}assertInternalContextConstructor\\(token, '${className}'`),
      `${className} must reject direct JavaScript construction`,
    )
  }

  const expectedExports = [
    'BuildContext',
    'DevServer',
    'TestContext',
    'WakeError',
    'build',
    'buildDocs',
    'buildLibrary',
    'bundle',
    'createBuildContext',
    'createTestContext',
    'generateCssToken',
    'generateDocgen',
    'generateFederationLock',
    'initializeFederation',
    'runTests',
    'startDevServer',
    'startDocsDevServer',
    'version',
  ].sort()
  const objectKeys = (block, label) => {
    assert.ok(block, `${label} export block must remain inspectable`)
    return [...block[1].matchAll(/^\s+([A-Za-z][A-Za-z0-9]*)(?:\s*:\s*typeof\s+[A-Za-z][A-Za-z0-9]*)?,?\s*$/gm)]
      .map((match) => match[1])
      .sort()
  }
  assert.deepEqual(
    objectKeys(/module\.exports\s*=\s*\{([\s\S]*?)\n\}/.exec(commonJs), 'CommonJS'),
    expectedExports,
  )
  assert.deepEqual(
    objectKeys(/export const\s*\{([\s\S]*?)\}\s*=\s*api/.exec(esm), 'ESM'),
    expectedExports,
  )
  assert.deepEqual(
    objectKeys(/declare const wake:\s*\{([\s\S]*?)\n\}/.exec(nodeTypes), 'TypeScript default'),
    expectedExports,
  )

  const rustVariants = (source, name) => {
    const block = new RegExp(`pub enum ${name}\\s*\\{([\\s\\S]*?)^\\}`, 'm').exec(source)
    assert.ok(block, `${name} must remain inspectable`)
    return [...block[1].matchAll(/^ {4}([A-Z][A-Za-z0-9]+)(?:\s*\{|\s*,)\s*$/gm)]
      .map((match) => match[1][0].toLowerCase() + match[1].slice(1))
      .sort()
  }
  const typeEvents = (className) => {
    const block = new RegExp(`export class ${className} extends EventEmitter \\{([\\s\\S]*?)^\\}`, 'm')
      .exec(nodeTypes)
    assert.ok(block, `${className} event overloads must remain inspectable`)
    return [...block[1].matchAll(/on\(event: '([^']+)'/g)].map((match) => match[1]).sort()
  }
  const testDispatch = /#dispatchEvents\(events\)\s*\{([\s\S]*?)^  \}\s*^\s*#startEventPoll/m
    .exec(commonJs)
  assert.ok(testDispatch, 'TestContext event dispatch must remain inspectable')
  const commonTestEvents = [...testDispatch[1].matchAll(/case '([^']+)'/g)]
    .map((match) => match[1])
    .sort()
  const expectedTestEvents = [
    'closed',
    'diagnostic',
    'runComplete',
    'runStart',
    'suiteResult',
    'testCaseResult',
  ].sort()
  assert.deepEqual(rustVariants(app, 'TestSessionEvent'), expectedTestEvents)
  assert.deepEqual(commonTestEvents, expectedTestEvents)
  assert.deepEqual(typeEvents('TestContext'), expectedTestEvents)

  const devDrain = /#drainEvents\(\)\s*\{([\s\S]*?)^  \}\s*^\s*async close/m.exec(commonJs)
  assert.ok(devDrain, 'DevServer event dispatch must remain inspectable')
  const commonDevEvents = [...devDrain[1].matchAll(/event\.type === '([^']+)'/g)]
    .map((match) => match[1])
    .sort()
  const expectedDevEvents = [
    'closed',
    'diagnostic',
    'federationUpdated',
    'rebuildStart',
    'rebuilt',
    'workspaceState',
  ].sort()
  assert.deepEqual(rustVariants(app, 'DevServerEvent'), expectedDevEvents)
  assert.deepEqual(commonDevEvents, expectedDevEvents)
  assert.deepEqual(typeEvents('DevServer'), expectedDevEvents)

  const camelCase = (value) => value.replace(/_([a-z])/g, (_, letter) => letter.toUpperCase())
  const rustVariantFields = (enumName, variantName) => {
    const enumBlock = new RegExp(`pub enum ${enumName}\\s*\\{([\\s\\S]*?)^\\}`, 'm').exec(app)
    assert.ok(enumBlock, `${enumName} must remain inspectable`)
    const variant = new RegExp(`^    ${variantName} \\{([\\s\\S]*?)^    \\},?$`, 'm')
      .exec(enumBlock[1])
    assert.ok(variant, `${enumName}::${variantName} must remain inspectable`)
    return [...variant[1].matchAll(/^        ([a-z_]+):/gm)]
      .map((match) => camelCase(match[1]))
      .sort()
  }
  const interfaceFields = (name) => {
    const block = new RegExp(`export interface ${name} \\{([\\s\\S]*?)^\\}`, 'm').exec(nodeTypes)
    assert.ok(block, `${name} must remain inspectable`)
    return [...block[1].matchAll(/^\s+(?:readonly )?([A-Za-z][A-Za-z0-9]*)\??:/gm)]
      .map((match) => match[1])
      .sort()
  }
  const testEventProjections = new Map([
    ['RunStart', ['TestRunStartEvent', 'runStart']],
    ['TestCaseResult', ['TestCaseResultEvent', 'testCaseResult']],
    ['SuiteResult', ['TestSuiteResultEvent', 'suiteResult']],
  ])
  for (const [variant, [typeName, eventName]] of testEventProjections) {
    const expectedFields = rustVariantFields('TestSessionEvent', variant)
    assert.deepEqual(interfaceFields(typeName), expectedFields, `${typeName} field set drifted`)
    const projection = new RegExp(
      `(?:case '${eventName}':|case '${eventName}':\\s*)[\\s\\S]*?this\\.emit\\('${eventName}', \\{([\\s\\S]*?)\\}\\)`,
    ).exec(testDispatch[1])
    assert.ok(projection, `${eventName} JavaScript projection must remain inspectable`)
    const projectedFields = [...projection[1].matchAll(/([A-Za-z][A-Za-z0-9]*):\s*event\./g)]
      .map((match) => match[1])
      .sort()
    assert.deepEqual(projectedFields, expectedFields, `${eventName} JavaScript field set drifted`)
  }
  assert.match(testDispatch[1], /case 'diagnostic':[\s\S]{0,100}this\.emit\('diagnostic', event\.diagnostic\)/)
  assert.match(testDispatch[1], /case 'runComplete':[\s\S]{0,100}this\.emit\('runComplete', event\.result\)/)

  const devEventProjections = new Map([
    ['RebuildStart', ['DevServerRebuildStartEvent', 'rebuildStart']],
    ['Rebuilt', ['DevServerRebuiltEvent', 'rebuilt']],
    ['WorkspaceState', ['DevServerWorkspaceStateEvent', 'workspaceState']],
    ['FederationUpdated', ['DevServerFederationUpdatedEvent', 'federationUpdated']],
  ])
  for (const [variant, [typeName, eventName]] of devEventProjections) {
    const expectedFields = ['type', ...rustVariantFields('DevServerEvent', variant)].sort()
    assert.deepEqual(interfaceFields(typeName), expectedFields, `${typeName} field set drifted`)
    assert.match(
      devDrain[1],
      new RegExp(`event\\.type === '${eventName}'[\\s\\S]{0,100}this\\.emit\\('${eventName}', event\\)`),
      `${eventName} must forward the exact typed Rust payload`,
    )
  }

  assert.deepEqual(rustVariants(testContract, 'TestSuiteStatus'), ['failed', 'passed', 'skipped'])
  assert.deepEqual(rustVariants(testContract, 'TestEnvironmentKind'), ['browser', 'dom'])
  assert.deepEqual(rustVariants(testContract, 'TestLeakKind'), [
    'listener',
    'network',
    'other',
    'socket',
    'task',
    'timer',
  ])
  assert.match(nodeTypes, /export type TestSuiteStatus = 'passed' \| 'failed' \| 'skipped'/)
  assert.match(nodeTypes, /kind: 'dom' \| 'browser'/)
  assert.match(nodeTypes, /kind: 'timer' \| 'listener' \| 'task' \| 'socket' \| 'network' \| 'other'/)
})

test('Federation types distinguish canonical wire nulls from normalized absence', () => {
  const contract = beforeCfgTestModule(
    readFileSync(new URL('../crates/wake_federation_contract/src/manifest.rs', import.meta.url), 'utf8'),
    'crates/wake_federation_contract/src/manifest.rs',
  )
  const types = readFileSync(new URL('../npm/wake/federation.d.mts', import.meta.url), 'utf8')
  const runtime = readFileSync(new URL('../npm/wake/federation.mjs', import.meta.url), 'utf8')

  const serializedOptionFields = [...contract.matchAll(/^\s+pub ([a-z_]+): Option<[^>]+>,/gm)]
    .map((match) => match[1])
    .sort()
  assert.deepEqual(serializedOptionFields, [
    'asset',
    'coherence_group',
    'development',
    'fallback',
    'owner',
    'remote_entry_source_map',
    'source_map',
    'types',
    'types_integrity',
  ])
  assert.match(
    contract,
    /#\[serde\(skip_serializing_if = "Option::is_none"\)\]\s+pub types_integrity: Option<String>/,
    'the lock is the sole Option field omitted rather than serialized as null',
  )

  for (const [wireType, fieldPattern] of [
    ['FederationExposeWire', 'sourceMap: FederationAsset \\| null'],
    ['SharedPolicyWire', 'coherenceGroup: string \\| null[\\s\\S]*?owner: string \\| null'],
    ['SharedOfferWire', 'asset: FederationAsset \\| null'],
    ['SharedRequirementWire', 'fallback: FederationAsset \\| null'],
    ['FederationManifestWire', 'remoteEntrySourceMap: FederationAsset \\| null[\\s\\S]*?types: FederationTypeArtifact \\| null[\\s\\S]*?development:[\\s\\S]*?\\| null'],
  ]) {
    assert.match(types, new RegExp(`export type ${wireType}[\\s\\S]*?${fieldPattern}`))
  }
  const normalizedPolicy = /export interface SharedPolicy \{([\s\S]*?)^\}/m.exec(types)
  const normalizedManifest = /export interface FederationManifest \{([\s\S]*?)^\}/m.exec(types)
  assert.ok(normalizedPolicy && normalizedManifest)
  assert.doesNotMatch(normalizedPolicy[1], /\b(?:coherenceGroup|owner)\??:[^\n]*\bnull\b/)
  assert.doesNotMatch(normalizedManifest[1], /\b(?:remoteEntrySourceMap|types|development)\??:[^\n]*\bnull\b/)
  assert.match(types, /interface FederationTransportManifestResult \{[\s\S]*?readonly manifest: unknown/)

  for (const marker of [
    /rawManifest\.remoteEntrySourceMap === undefined \|\| rawManifest\.remoteEntrySourceMap === null/,
    /rawExpose\.sourceMap === undefined \|\| rawExpose\.sourceMap === null/,
    /offer\.asset === undefined \|\| offer\.asset === null/,
    /requirement\.fallback === undefined \|\| requirement\.fallback === null/,
    /rawManifest\.types !== undefined && rawManifest\.types !== null/,
    /rawManifest\.development !== undefined && rawManifest\.development !== null/,
    /const coherenceGroup = rawPolicy\.coherenceGroup \?\? undefined/,
    /const owner = rawPolicy\.owner \?\? undefined/,
  ]) assert.match(runtime, marker)
})

test('Federation development snapshots are retained only by bounded typed leases', () => {
  const contractRoot = readFileSync(
    new URL('../crates/wake_federation_contract/src/lib.rs', import.meta.url),
    'utf8',
  )
  const contractDev = readFileSync(
    new URL('../crates/wake_federation_contract/src/dev.rs', import.meta.url),
    'utf8',
  )
  const snapshotOwner = beforeCfgTestModule(
    readFileSync(new URL('../crates/wake_dev_server/src/federation.rs', import.meta.url), 'utf8'),
    'crates/wake_dev_server/src/federation.rs',
  )
  const server = beforeCfgTestModule(
    readFileSync(new URL('../crates/wake_dev_server/src/lib.rs', import.meta.url), 'utf8'),
    'crates/wake_dev_server/src/lib.rs',
  )
  const runtime = readFileSync(new URL('../npm/wake/federation.mjs', import.meta.url), 'utf8')
  const embeddedRuntime = readFileSync(
    new URL('../crates/wake_app/assets/federation-runtime.mjs', import.meta.url),
    'utf8',
  )
  const types = readFileSync(new URL('../npm/wake/federation.d.mts', import.meta.url), 'utf8')

  for (const source of [contractRoot, runtime, types]) {
    assert.match(source, /wake\.federation\.dev-lease\.v1/)
  }
  assert.match(contractRoot, /FEDERATION_DEV_MAX_BUILD_LEASES:\s*usize\s*=\s*8/)
  assert.match(runtime, /FEDERATION_DEV_MAX_BUILD_LEASES\s*=\s*8/)
  assert.match(types, /FEDERATION_DEV_MAX_BUILD_LEASES:\s*8/)

  const rustReasons = /enum DevLeaseReloadReason\s*\{([\s\S]*?)\}/.exec(contractDev)
  const runtimeReasons = /DEV_LEASE_RELOAD_REASONS\s*=\s*new Set\(\[([^\]]+)\]\)/.exec(runtime)
  const typeReasons = /type FederationDevLeaseReloadReason\s*=([^\n]+)/.exec(types)
  assert.ok(rustReasons && runtimeReasons && typeReasons)
  const toKebab = (value) => value.replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase()
  const rustReasonValues = [...rustReasons[1].matchAll(/^\s*([A-Z][A-Za-z0-9]+),/gm)]
    .map((match) => toKebab(match[1]))
    .sort()
  const runtimeReasonValues = [...runtimeReasons[1].matchAll(/'([^']+)'/g)]
    .map((match) => match[1])
    .sort()
  const typeReasonValues = [...typeReasons[1].matchAll(/'([^']+)'/g)]
    .map((match) => match[1])
    .sort()
  assert.deepEqual(runtimeReasonValues, rustReasonValues)
  assert.deepEqual(typeReasonValues, rustReasonValues)

  assert.match(snapshotOwner, /FEDERATION_SNAPSHOT_GRACE_GENERATIONS:\s*u64\s*=\s*2/)
  assert.match(snapshotOwner, /history:\s*BTreeMap<BuildId,\s*RetiredFederationRoutes>/)
  assert.match(snapshotOwner, /lease_counts:\s*BTreeMap<BuildId,\s*usize>/)
  assert.doesNotMatch(snapshotOwner, /history:\s*BTreeMap<String,\s*FederationSnapshot>/)
  assert.match(snapshotOwner, /pub fn replace_leases\s*\(/)
  assert.match(snapshotOwner, /pub fn release_leases\s*\(/)
  assert.match(snapshotOwner, /FederationRouteLookup::Gone/)
  assert.match(snapshotOwner, /FederationRouteLookup::NotFederation/)

  assert.match(server, /HttpResponse::Gone\(\)/)
  assert.match(server, /Access-Control-Expose-Headers/)
  assert.match(server, /FederationRouteLookup::Missing[\s\S]*?HttpResponse::NotFound\(\)/)
  assert.match(server, /duplicate enabled Federation container name/)
  assert.match(server, /mount\.federation_tx\.subscribe\(\)/)
  const appState = /struct AppState\s*\{([\s\S]*?)\n\}/.exec(server)
  const mountedState = /struct MountedAppState\s*\{([\s\S]*?)\n\}/.exec(server)
  assert.ok(appState && mountedState)
  assert.doesNotMatch(appState[1], /federation_tx/)
  assert.match(mountedState[1], /federation_tx:\s*broadcast::Sender<String>/)
  assert.match(server, /collect::<BTreeMap<_,\s*_>>\(\)/)
  const goneResponse = /fn federation_gone_response\s*\([\s\S]*?\n\}/.exec(server)
  assert.ok(goneResponse)
  assert.doesNotMatch(
    goneResponse[0],
    /federation_tx|\.send\s*\(/,
    'public HTTP 410 lookup must remain a read-only, non-broadcast boundary',
  )
  assert.match(server, /Err\(broadcast::error::RecvError::Lagged\(_\)\)[\s\S]*?DevLeaseReloadReason::UpdateLagged/)

  assert.equal(runtime, embeddedRuntime, 'published and embedded Federation runtimes must be byte-identical')
  assert.match(runtime, /activeDevBuildIds:\s*new Set\(\)/)
  assert.doesNotMatch(runtime, /retiredBuildIds/)
  assert.match(runtime, /response\.status === 410/)
  assert.match(runtime, /identity\?\.development === true/)
  assert.match(runtime, /control\.remote === identity\.name/)
  assert.match(runtime, /control\.expiredBuildId === identity\.buildId/)
  assert.match(runtime, /control\.generation > identity\.generation/)
  assert.match(runtime, /control\.currentBuildId !== accepted\.currentBuildId/)
  assert.match(runtime, /control\.generation !== accepted\.generation/)
  assert.match(runtime, /JSON\.stringify\(\[lease\.buildIds, cursor\.currentBuildId, cursor\.generation\]\)/)
  assert.match(runtime, /development: remote\.mode === 'development'/)
  assert.doesNotMatch(runtime, /development:\s*true|development:\s*manifest\.development/)
})

test('Federation init and lock stay application-owned across Rust and npm frontends', () => {
  const app = readFileSync(
    new URL('../crates/wake_app/src/federation_init.rs', import.meta.url),
    'utf8',
  )
  const rustCliPath = 'crates/wake_cli/src/main.rs'
  const rustCli = beforeCfgTestModule(
    readFileSync(new URL(`../${rustCliPath}`, import.meta.url), 'utf8'),
    rustCliPath,
  )
  const node = readFileSync(new URL('../crates/wake_node/src/lib.rs', import.meta.url), 'utf8')
  const commonJs = readFileSync(new URL('../npm/wake/index.cjs', import.meta.url), 'utf8')
  const npmCli = readFileSync(new URL('../npm/wake/bin/wake.mjs', import.meta.url), 'utf8')

  assert.match(app, /pub fn initialize_federation_types\s*\(/)
  assert.match(app, /persist_noclobber/)
  assert.match(rustCli, /wake_app::initialize_federation_types\s*\(/)
  assert.match(rustCli, /wake_app::generate_project_federation_lock\s*\(/)
  assert.doesNotMatch(rustCli, /struct FederationInitError|FEDERATION_DECLARATION|tempfile::Builder/)

  for (const [exportName, appCall] of [
    ['initializeFederation', 'initialize_federation_types'],
    ['generateFederationLock', 'generate_project_federation_lock'],
  ]) {
    assert.match(node, new RegExp(`#\\[napi\\(js_name = "${exportName}"\\)\\]`))
    assert.match(node, new RegExp(`wake_app::${appCall}\\s*\\(`))
    assert.match(commonJs, new RegExp(`native\\.${exportName}\\s*\\(`))
    assert.match(npmCli, new RegExp(`await ${exportName}\\s*\\(`))
  }
  assert.match(npmCli, /command === 'federation'/)
  assert.match(npmCli, /action !== 'init' && action !== 'lock'/)
})

test('ordinary browser updates expose only the ADR 0030 Live Reload contract', () => {
  const devServerPath = 'crates/wake_dev_server/src/lib.rs'
  const devServer = beforeCfgTestModule(
    readFileSync(new URL(`../${devServerPath}`, import.meta.url), 'utf8'),
    devServerPath,
  )
  const app = readFileSync(new URL('../crates/wake_app/src/lib.rs', import.meta.url), 'utf8')
  const config = readFileSync(new URL('../crates/wake_config/src/lib.rs', import.meta.url), 'utf8')
  const node = readFileSync(new URL('../crates/wake_node/src/lib.rs', import.meta.url), 'utf8')
  const nodeTypes = readFileSync(new URL('../npm/wake/index.d.ts', import.meta.url), 'utf8')
  const navigation = readFileSync(new URL('../docs/navigation.toml', import.meta.url), 'utf8')

  assert.match(
    devServer,
    /const LIVE_RELOAD_ENDPOINT:\s*&str\s*=\s*"\/__wake_live_reload"\s*;/,
    'wake_dev_server must own the one ordinary browser-update endpoint',
  )
  assert.match(devServer, /#\[serde\(tag\s*=\s*"type"\)\]\s*enum LiveReloadMessage/)
  for (const variant of ['Ready', 'Reload', 'Error']) {
    assert.match(devServer, new RegExp(`\\b${variant}\\b`))
  }
  for (const encoder of ['msg_ready', 'msg_reload', 'msg_error']) {
    assert.match(
      devServer,
      new RegExp(`fn ${encoder}\\b[\\s\\S]*?encode_live_reload\\(LiveReloadMessage::`),
      `${encoder} must encode a typed LiveReloadMessage`,
    )
  }
  assert.doesNotMatch(
    devServer,
    /\{\\?"type\\?"\s*:\s*\\?"(?:ready|reload|error)\\?"/,
    'ordinary Live Reload frames must not be assembled from JSON string literals',
  )
  assert.match(devServer, /m\.type === "reload"\) \{ clearError\(\); location\.reload\(\); \}/)
  assert.doesNotMatch(devServer, /\/__wake_hmr/)

  assert.match(
    app,
    /\("import\.meta\.hot"\.to_string\(\),\s*"false"\.to_string\(\)\)/,
    'application builds must lower the unsupported module-hot API to false',
  )
  assert.match(app, /if key == "import\.meta\.hot" \{\s*continue;\s*\}/)
  assert.match(config, /fn deserialize_defines\b[\s\S]*?contains_key\("import\.meta\.hot"\)/)
  assert.match(
    config,
    /#\[serde\(default, deny_unknown_fields\)\]\s*pub struct DevServer/,
    'wake.config.toml must reject fake development-server capabilities',
  )
  assert.match(
    node,
    /#\[serde\(default, rename_all = "camelCase", deny_unknown_fields\)\]\s*struct RawApplicationDevServerOptions/,
    'the Node bridge must reject fake development-server capabilities',
  )
  const devServerOptions = /export interface DevServerOptions[^\{]*\{([\s\S]*?)\n\}/.exec(nodeTypes)
  assert.ok(devServerOptions, 'index.d.ts must retain DevServerOptions')
  assert.doesNotMatch(devServerOptions[1], /\b(?:hmr|hot|liveReload)\??\s*:/)

  assert.equal(
    existsSync(fileURLToPath(new URL('../docs/app/hmr.mdx', import.meta.url))),
    false,
    'the old HMR documentation route must not survive the atomic rename',
  )
  assert.equal(
    existsSync(fileURLToPath(new URL('../docs/app/live-reload.mdx', import.meta.url))),
    true,
  )
  assert.match(navigation, /app\/live-reload/)
  assert.doesNotMatch(navigation, /app\/hmr/)

  const currentCapabilitySurfaces = [
    'README.md',
    'crates/wake_cli/src/main.rs',
    'crates/wake_cli/src/dashboard.rs',
    'npm/wake/bin/wake.mjs',
    'npm/wake/bin/terminal.mjs',
    'docs/index.mdx',
    'docs/app/dev-server.mdx',
    'docs/app/live-reload.mdx',
    'docs/reference/cli/dev.mdx',
    'docs/reference/configuration/dev-server.mdx',
    'docs/reference/node-api/dev-server.mdx',
    'engineering/ARCHITECTURE.md',
    'engineering/DESIGN.md',
    'engineering/PLAN.md',
  ].map((path) => [path, readFileSync(new URL(`../${path}`, import.meta.url), 'utf8')])
  for (const [path, source] of currentCapabilitySurfaces) {
    assert.match(source, /Live [Rr]eload/, `${path} must name the current browser capability honestly`)
    assert.doesNotMatch(
      source,
      /HMR on|HMR ·|HMR WebSocket|可 HMR|利于 HMR|获得开发服务器、HMR|保持组件状态|保留组件状态/,
      `${path} must not restore the removed module-HMR or state-preservation promise`,
    )
  }
})

const publicTestContractFiles = [
  '.github/workflows/release-npm.yml',
  'crates/wake_test/Cargo.toml',
  'docs/reference/cli/test.mdx',
  'docs/reference/compatibility.mdx',
  'docs/reference/configuration/test.mdx',
  'docs/reference/errors.mdx',
  'docs/reference/node-api/test.mdx',
  'npm/wake/CHANGELOG.md',
  'npm/wake/bin/wake.mjs',
  'npm/wake/index.cjs',
  'npm/wake/index.d.ts',
  'npm/wake/index.mjs',
  'npm/wake/package.json',
  'npm/wake/test.cjs',
  'npm/wake/test.d.ts',
  'npm/wake/test.mjs',
  'npm/wake/test-react.cjs',
  'npm/wake/test-react.d.ts',
  'npm/wake/test-react.mjs',
]

const removedTestContracts = [
  ['Jest compatibility surface', /\bJest\b/i],
  ['Boa engine promise', /\bBoa(?:_engine|_gc)?\b/i],
  ['jsdom environment promise', /\bjsdom\b/i],
  ['test config initializer', /\binitTestConfig\b/],
  ['old runtime filename', /jest-runtime\.js/i],
  ['camelCase name flag', /\btestNamePattern\b/],
  ['run-in-band compatibility field', /\brunInBand\b/],
  ['singular snapshot update field', /\bupdateSnapshot\b/],
  ['legacy no-tests field', /\bpassWithNoTests\b/],
  ['legacy watch flag', /\bwatchAll\b/],
  ['legacy randomization field', /\brandomize\b/],
  ['legacy init flag', /--init\b/],
  ['legacy JSON flag', /--json\b/],
  ['legacy dashed name flag', /--test-name-pattern\b/],
  ['legacy dashed serial flag', /--run-in-band\b/],
  ['legacy dashed snapshot flag', /--update-snapshot\b/],
  ['legacy dashed no-tests flag', /--pass-with-no-tests\b/],
  ['legacy dashed watch flag', /--watch-all\b/],
  ['legacy flattened failure field', /\bfailureMessages\b/],
  ['legacy flattened result count', /\bnumPassedTestSuites\b/],
  ['inline source snapshot matcher', /\btoMatchInlineSnapshot\b/],
  ['intra-DOM concurrent test API', /readonly\s+concurrent\s*:/],
  ['legacy fake timer promise', /legacy\s+(?:fake\s+)?timer/i],
  ['Babel coverage promise', /Babel\s+coverage/i],
]

test('public test surfaces contain only the ADR 0020 Wake-native contract', () => {
  for (const path of publicTestContractFiles) {
    const source = readFileSync(new URL(`../${path}`, import.meta.url), 'utf8')
    for (const [contract, pattern] of removedTestContracts) {
      assert.doesNotMatch(source, pattern, `${path} still exposes ${contract}`)
    }
  }
})

test('active test kernel contains one versioned Wake result wire', () => {
  for (const path of [
    'crates/wake_test/src/lib.rs',
    'crates/wake_test/runtime/wake-test-runtime.js',
  ]) {
    const source = readFileSync(new URL(`../${path}`, import.meta.url), 'utf8')
    assert.match(source, /wake\.test\.runtime\.v1/, `${path} is missing the private result schema`)
    for (const [contract, pattern] of removedTestContracts) {
      assert.doesNotMatch(source, pattern, `${path} retains ${contract}`)
    }
    assert.doesNotMatch(source, /#\[cfg\(any\(\)\)\]/, `${path} retains a disabled compatibility path`)
    assert.doesNotMatch(source, /\b(?:ancestorTitles|numPassingAsserts)\b/, `${path} retains an old result field`)
  }
})

test('embedded V8 conformance uses one immutable selected Test262 ES2024 manifest', () => {
  const manifest = JSON.parse(
    readFileSync(new URL('../engineering/test262-es2024.json', import.meta.url), 'utf8'),
  )
  assert.equal(manifest.contract, 'ADR-0020')
  assert.equal(manifest.target, 'ES2024')
  assert.match(manifest.commit, /^[0-9a-f]{40}$/)
  assert.match(manifest.sha256, /^[0-9a-f]{64}$/)
  assert.ok(manifest.selectedRoots.length > 0)
  assert.equal(new Set(manifest.selectedRoots).size, manifest.selectedRoots.length)
  assert.deepEqual(
    new Set(manifest.excludedTests),
    new Set(Object.keys(manifest.exclusionReasons)),
  )

  const runner = readFileSync(new URL('../scripts/run-test262.mjs', import.meta.url), 'utf8')
  assert.match(runner, /createHash\('sha256'\)/)
  assert.match(runner, /wake_ecma_vm/)
  assert.match(runner, /'--locked'/)
  assert.match(runner, /'--offline'/)
  const ci = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8')
  assert.match(ci, /corepack yarn test262:es2024/)
  assert.match(ci, /prepare-rusty-v8\.mjs --target x86_64-unknown-linux-gnu/)
  assert.match(ci, /browser-conformance:/)
  for (const platform of [
    'windows-latest',
    'ubuntu-24.04',
    'ubuntu-24.04-arm',
    'macos-15',
    'macos-15-intel',
  ]) {
    assert.match(ci, new RegExp(platform.replaceAll('.', '\\.')))
  }
  assert.match(ci, /cargo test --locked --offline -p wake_test_browser --lib -- --ignored/)
  assert.match(ci, /cargo test --locked --offline -p wake_test --lib -- --ignored/)
})

test('system browser evidence separates experimental publication from stable readiness', () => {
  const manifest = JSON.parse(
    readFileSync(
      new URL('../engineering/system-browser-conformance.json', import.meta.url),
      'utf8',
    ),
  )
  assert.equal(manifest.schemaVersion, 3)
  assert.equal(manifest.contract, 'ADR-0020')
  assert.equal(manifest.scope, 'ci-release-browser-evidence')
  assert.equal(manifest.versionSource, 'cdp-browser-get-version')
  assert.equal(manifest.requiredHeadless, true)
  assert.equal(manifest.browserBinaryPolicy, 'system-only-no-download')
  assert.deepEqual(manifest.acceptedKinds, ['chrome', 'edge', 'chromium'])
  assert.equal(manifest.stableReadiness.policy, 'shared-exact-major')
  assert.equal(manifest.stableReadiness.major, 151)
  assert.deepEqual(
    Object.keys(manifest.targets).sort(),
    [
      'darwin-arm64',
      'darwin-x64',
      'linux-arm64-gnu',
      'linux-x64-gnu',
      'win32-x64-msvc',
    ],
  )
  assert.equal(manifest.targets['win32-x64-msvc'].experimental.mode, 'exact-major-conformance')
  assert.equal(manifest.targets['win32-x64-msvc'].experimental.major, 151)
  assert.equal(manifest.targets['linux-x64-gnu'].experimental.mode, 'exact-major-conformance')
  assert.equal(manifest.targets['linux-x64-gnu'].experimental.major, 151)
  assert.equal(manifest.targets['linux-arm64-gnu'].experimental.mode, 'unavailable')
  assert.deepEqual(
    manifest.targets['linux-arm64-gnu'].reviewedRunnerEvidence[0].browserVersions,
    {},
  )
  assert.equal(manifest.targets['darwin-x64'].experimental.mode, 'reviewed-major-smoke')
  assert.deepEqual(manifest.targets['darwin-x64'].experimental.majors, [150, 151])
  assert.equal(manifest.targets['darwin-arm64'].experimental.mode, 'exact-major-smoke')
  assert.equal(manifest.targets['darwin-arm64'].experimental.major, 150)
  for (const policy of Object.values(manifest.targets)) {
    assert(Array.isArray(policy.reviewedRunnerEvidence))
    for (const evidence of policy.reviewedRunnerEvidence) {
      assert.match(
        evidence.source,
        /^https:\/\/github\.com\/actions\/runner-images\/blob\/[0-9a-f]{40}\//,
      )
      assert.match(evidence.imageVersion, /^\d+\.\d+\.\d+$/)
    }
  }

  const checker = readFileSync(
    new URL('./check-system-browser-conformance.mjs', import.meta.url),
    'utf8',
  )
  const identityExample = readFileSync(
    new URL(
      '../crates/wake_test_browser/examples/system_browser_identity.rs',
      import.meta.url,
    ),
    'utf8',
  )
  const ci = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8')
  const release = readFileSync(
    new URL('../.github/workflows/release-npm.yml', import.meta.url),
    'utf8',
  )
  assert.match(identityExample, /BrowserDriver::launch/)
  assert.match(identityExample, /driver\.installation/)
  assert.match(ci, /check-system-browser-conformance\.mjs/)
  assert.match(ci, /--identity/)
  assert.match(ci, /--unavailable true/)
  assert.match(ci, /--stable-readiness blocked/)
  assert.match(ci, /browser-stable-readiness:/)
  assert.match(release, /--reporter json/)
  assert.match(release, /--result browser-result\.json/)
  assert.match(release, /--unavailable true/)
  assert.match(release, /--stable-readiness blocked/)
  assert.ok(
    release.indexOf('--result browser-result.json') <
      release.indexOf('  publish:'),
    'the pinned browser result must be checked before publish',
  )
  assert.doesNotMatch(checker, /from ['"]node:(?:http|https|net)['"]|\bfetch\s*\(/)
  for (const source of [ci, release]) {
    assert.doesNotMatch(
      source,
      /playwright|puppeteer|chrome-for-testing|setup-chrome|browser-actions/i,
    )
  }
})

test('architecture CI fetches the complete lock graph before its offline target-all check', () => {
  const ci = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8')
  const start = ci.indexOf('  architecture:')
  const end = ci.indexOf('\n  fmt:', start)
  assert.notEqual(start, -1)
  assert.notEqual(end, -1)
  const job = ci.slice(start, end)
  const markers = [
    'corepack yarn install --immutable --check-cache',
    'cargo fetch --locked',
    'node scripts/prepare-rusty-v8.mjs --target x86_64-unknown-linux-gnu',
    'cargo build -p wake_test_host -p wake_cli --locked --offline',
    'corepack yarn release:check',
    './target/debug/wake test scripts/check-architecture.test.mjs --serial',
    'corepack yarn architecture:check',
  ]
  let previous = -1
  for (const marker of markers) {
    const index = job.indexOf(marker)
    assert.ok(index > previous, `${marker} must follow the preceding clean-cache gate`)
    previous = index
  }
  assert.doesNotMatch(job, /cargo fetch --locked --target/)
  assert.match(job, /cargo tree --target all --offline/)

  const checker = readFileSync(
    new URL('./check-architecture.mjs', import.meta.url),
    'utf8',
  )
  assert.match(checker, /'--offline',[\s\S]*?'--target',[\s\S]*?'all'/)
})

test('Node CI stages the complete local platform package before testing and packing', () => {
  const ci = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8')
  const manifest = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'))
  const startupCheck = readFileSync(new URL('./check-startup.mjs', import.meta.url), 'utf8')
  const nodeJobStart = ci.search(/\r?\n  node:\r?\n/)
  assert.notEqual(nodeJobStart, -1)
  const nodeJob = ci.slice(nodeJobStart)
  const markers = [
    'cargo fetch --locked',
    'node scripts/prepare-rusty-v8.mjs --target x86_64-pc-windows-msvc',
    'corepack yarn native:build',
    'git diff --exit-code -- Cargo.lock',
    'node scripts/stage-test-host.mjs --package-dir npm/wake-win32-x64-msvc',
    'corepack yarn npm:test:wake',
    'corepack yarn npm:pack:check',
  ]
  let previous = -1
  for (const marker of markers) {
    const index = nodeJob.indexOf(marker)
    assert(index > previous, `${marker} must follow the preceding Node CI stage`)
    previous = index
  }
  assert.doesNotMatch(nodeJob, /cargo fetch --locked --target/)
  assert.doesNotMatch(nodeJob, /Copy-Item .*\.node/)
  assert.equal(
    manifest.scripts['npm:test:wake'],
    'node --import ./npm/wake/test/select-built-native.mjs ./npm/wake/bin/wake.mjs test npm/wake/test/cli.test.mjs npm/wake/test/components-state.test.mjs npm/wake/test/console.test.mjs npm/wake/test/terminal.test.mjs && yarn npm:test:wake:addon',
  )
  assert.equal(
    manifest.scripts['npm:test:wake:addon'],
    'node --import ./npm/wake/test/select-built-native.mjs --test npm/wake/test/api.test.mjs npm/css/test/realm.node.mjs && yarn npm:test:wake:federation',
  )
  const nativeAddonSelection = readFileSync(
    new URL('../npm/wake/test/select-built-native.mjs', import.meta.url),
    'utf8',
  )
  assert.match(nativeAddonSelection, /new URL\(`\.\.\/wake\.\$\{suffix\}\.node`, import\.meta\.url\)/)
  assert.match(nativeAddonSelection, /process\.env\.WAKE_NATIVE_PATH\s*=\s*nativePath/)
  assert.doesNotMatch(nativeAddonSelection, /wake-(?:win32|linux|darwin)-[^/]*\//)
  for (const scriptName of ['npm:test:wake', 'npm:test:wake:addon']) {
    assert.match(
      manifest.scripts[scriptName],
      /node --import \.\/npm\/wake\/test\/select-built-native\.mjs/,
      `${scriptName} must select the addon emitted by native:build before importing Wake`,
    )
  }
  assert.equal(
    manifest.scripts['npm:test:wake:federation'],
    'node --test npm/wake/test/federation.test.mjs npm/wake/test/federation-react.test.mjs',
  )
  for (const federationTest of [
    'npm/wake/test/federation.test.mjs',
    'npm/wake/test/federation-react.test.mjs',
  ]) {
    assert.match(manifest.scripts['npm:test:wake:federation'], new RegExp(federationTest.replaceAll('.', '\\.')))
  }
  for (const marker of ['.pnp.cjs', '.pnp.loader.mjs', 'pathToFileURL', "'--experimental-loader'"]) {
    assert.match(startupCheck, new RegExp(marker.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  }
})

test('npm consumers are built from local tarballs and tested outside the PnP source tree', () => {
  const ci = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8')
  const release = readFileSync(
    new URL('../.github/workflows/release-npm.yml', import.meta.url),
    'utf8',
  )
  const consumer = readFileSync(new URL('./check-npm-consumer.mjs', import.meta.url), 'utf8')
  const artifactStart = ci.search(/\r?\n  npm-package-artifacts:\r?\n/)
  const consumerStart = ci.search(/\r?\n  npm-consumer:\r?\n/)
  const consumerEnd = ci.search(/\r?\n  architecture:\r?\n/)
  assert.notEqual(artifactStart, -1)
  assert(consumerStart > artifactStart)
  assert(consumerEnd > consumerStart)
  const artifactJob = ci.slice(artifactStart, consumerStart)
  const consumerJob = ci.slice(consumerStart, consumerEnd)

  for (const marker of [
    'platform: win32-x64-msvc',
    'platform: linux-x64-gnu',
    'corepack yarn install --immutable --check-cache',
    'corepack yarn native:build',
    'npm pack ./npm/wake --ignore-scripts --pack-destination artifacts',
    'npm pack ./npm/css --ignore-scripts --pack-destination artifacts',
    'npm pack ./npm/${{ matrix.package_dir }} --ignore-scripts --pack-destination artifacts',
    'node scripts/pack-npm-lock-platforms.mjs --artifacts artifacts --exclude ${{ matrix.platform }}',
  ]) assert.match(artifactJob, new RegExp(marker.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))

  assert.match(consumerJob, /needs: npm-package-artifacts/)
  assert.match(consumerJob, /node: '22\.14\.0'/)
  assert.match(consumerJob, /node: '26'/)
  assert.match(consumerJob, /node scripts\/check-npm-consumer\.mjs/)
  assert.match(consumerJob, /WAKE_NPM_PROJECT: \$\{\{ runner\.temp \}\}\/wake-npm-consumer/)
  assert.doesNotMatch(consumerJob, /corepack|yarn install|cargo |npm pack|WAKE_NATIVE_PATH/)

  assert.match(consumer, /\['install', '--package-lock-only'/)
  assert.match(consumer, /\['ci', \.\.\.ciArguments\]/)
  assert.match(consumer, /optionalPlatformArchives/)
  assert.match(consumer, /assertNoPnpAncestor\(project\)/)
  assert.match(consumer, /WAKE_NPM_WORKSPACE_CLASSIC/)
  assert.match(consumer, /node_modules\/wake-npm-consumer-shared/)
  assert.match(consumer, /'@crab-dev\/wake\/federation'/)
  assert.match(consumer, /'@crab-dev\/wake\/federation\/react'/)
  assert.match(consumer, /join\(project, 'consumer\.ts'\)/)
  assert.match(consumer, /join\(project, 'tsconfig\.json'\)/)
  assert.match(consumer, /skipLibCheck: false/)
  assert.match(consumer, /\['run', 'typecheck'\]/)

  const packCheck = readFileSync(new URL('./check-npm-packs.mjs', import.meta.url), 'utf8')
  assert.match(packCheck, /publicPackageFiles\(packageManifest\)/)
  assert.match(packCheck, /'\.\/federation'/)
  assert.match(packCheck, /'\.\/federation\/react'/)
  assert.match(packCheck, /The main package tarball is missing public target/)

  const prepublishStart = release.search(/\r?\n  prepublish-smoke:\r?\n/)
  const publishStart = release.search(/\r?\n  publish:\r?\n/)
  const prepublish = release.slice(prepublishStart, publishStart)
  assert.match(prepublish, /Select external npm consumer project/)
  assert.match(prepublish, /WAKE_NPM_PROJECT=%s\\n/)
  assert.match(prepublish, /\$RUNNER_TEMP\/wake-npm-consumer/)
  assert.match(prepublish, /node scripts\/check-npm-consumer\.mjs/)
  assert.doesNotMatch(prepublish, /--package-lock=false|mkdir local-smoke|cd local-smoke/)

  const verifyStart = release.search(/\r?\n  verify:\r?\n/)
  const buildNativeStart = release.search(/\r?\n  build-native:\r?\n/)
  const verify = release.slice(verifyStart, buildNativeStart)
  for (const marker of [
    'corepack yarn release:check',
    'corepack yarn npm:test:wake:federation',
    'corepack yarn npm:typecheck:wake',
    'corepack yarn npm:typecheck:css',
    'corepack yarn npm:pack:check',
    'WAKE_PACK_TARGETS: npm/css,npm/wake',
  ]) assert.match(verify, new RegExp(marker.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  for (const required of [
    'federation.mjs federation.d.mts',
    'federation-react.mjs federation-react.d.ts',
  ]) assert.match(release, new RegExp(required.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
})

test('Crab CSS editor tests use only the freshly built Wake CLI and sibling host', () => {
  const workflow = readFileSync(new URL('../.github/workflows/vscode-css.yml', import.meta.url), 'utf8')
  const yarnConfig = readFileSync(new URL('../.yarnrc.yml', import.meta.url), 'utf8')
  const architecturePolicy = JSON.parse(
    readFileSync(new URL('../engineering/architecture-boundaries.json', import.meta.url), 'utf8'),
  )
  const manifest = JSON.parse(
    readFileSync(new URL('../editors/vscode-css/package.json', import.meta.url), 'utf8'),
  )
  const build = readFileSync(
    new URL('../editors/vscode-css/scripts/build.mjs', import.meta.url),
    'utf8',
  )
  const launcher = readFileSync(
    new URL('../editors/vscode-css/scripts/run-wake-tests.mjs', import.meta.url),
    'utf8',
  )
  const binaryResolver = readFileSync(
    new URL('../editors/vscode-css/scripts/wake-binary.mjs', import.meta.url),
    'utf8',
  )
  const packageScript = readFileSync(
    new URL('../editors/vscode-css/scripts/package-vsix.mjs', import.meta.url),
    'utf8',
  )
  assert.equal(
    manifest.scripts.check,
    'yarn compile && node scripts/run-wake-tests.mjs test/manifest.test.mjs',
  )
  assert.deepEqual(
    architecturePolicy.dependencyProvenance.networkFreeBuild.offlineCargoBuildFiles,
    ['.github/workflows/release-npm.yml', '.github/workflows/vscode-css.yml'],
  )
  assert.match(binaryResolver, /process\.env\.WAKE_BIN/)
  assert.match(binaryResolver, /isAbsolute\(wakeBinary\)/)
  assert.match(build, /spawnSync\(wakeBinary, args/)
  assert.match(launcher, /spawnSync\(wakeBinary, \['test', \.\.\.testFiles, '--serial'\]/)
  assert.match(packageScript, /'--no-dependencies'/)
  assert.match(yarnConfig, /"@secretlint\/resolver@10\.2\.2":/)
  assert.match(yarnConfig, /"@secretlint\/secretlint-formatter-sarif": "10\.2\.2"/)
  assert.match(yarnConfig, /"@secretlint\/secretlint-rule-no-dotenv": "10\.2\.2"/)
  assert.match(yarnConfig, /"@secretlint\/secretlint-rule-preset-recommend": "10\.2\.2"/)
  for (const source of [manifest.scripts.check, build, launcher, binaryResolver]) {
    assert.doesNotMatch(source, /npm\/wake\/bin\/wake\.mjs|node_modules|releaseBinary|['"]cargo['"]|https?:\/\//)
  }

  const verifyStart = workflow.search(/\r?\n  verify:\r?\n/)
  const verifyEnd = workflow.search(/\r?\n  extension-host:\r?\n/)
  assert.notEqual(verifyStart, -1)
  assert(verifyEnd > verifyStart)
  const verify = workflow.slice(verifyStart, verifyEnd)
  const markers = [
    'corepack yarn install --immutable --check-cache',
    'cargo fetch --locked',
    'node scripts/prepare-rusty-v8.mjs --target x86_64-unknown-linux-gnu',
    'cargo build --release -p wake_test_host -p wake_cli --locked --offline',
    'corepack yarn release:check',
    'corepack yarn vscode:css:check',
    'WAKE_BIN: ${{ github.workspace }}/target/release/wake',
  ]
  let previous = -1
  for (const marker of markers) {
    const index = verify.indexOf(marker)
    assert(index > previous, `${marker} must follow the preceding VSIX verify stage`)
    previous = index
  }
  assert.match(verify, /CARGO_NET_OFFLINE: "true"/)
  assert.doesNotMatch(verify, /npm run native:build|napi build|stage-test-host/)
  assert.doesNotMatch(verify, /cargo fetch --locked --target/)

  const jobSource = (name, nextName) => {
    const start = workflow.search(new RegExp(`\\r?\\n  ${name}:\\r?\\n`))
    const end = workflow.search(new RegExp(`\\r?\\n  ${nextName}:\\r?\\n`))
    assert.notEqual(start, -1)
    assert(end > start)
    return workflow.slice(start, end)
  }
  const buildJobs = [
    {
      name: 'extension-host',
      source: jobSource('extension-host', 'package-native'),
      fetch: 'cargo fetch --locked --target x86_64-unknown-linux-gnu',
      build: 'cargo build --release -p wake_css_lsp -p wake_cli --locked --offline',
    },
    {
      name: 'package-native',
      source: jobSource('package-native', 'package-linux'),
      fetch: 'cargo fetch --locked --target ${{ matrix.rust_target }}',
      build: 'cargo build --release -p wake_css_lsp -p wake_cli --target ${{ matrix.rust_target }} --locked --offline',
    },
    {
      name: 'package-linux',
      source: jobSource('package-linux', 'github-release'),
      fetch: 'cargo fetch --locked --target ${{ matrix.rust_target }}',
      build: 'cargo build --release -p wake_css_lsp -p wake_cli --target ${{ matrix.rust_target }} --locked --offline',
    },
  ]
  for (const { name, source, fetch, build: buildMarker } of buildJobs) {
    const fetchIndex = source.indexOf(fetch)
    const buildIndex = source.indexOf(buildMarker)
    const offlineIndex = source.indexOf('CARGO_NET_OFFLINE: "true"')
    assert(fetchIndex >= 0, `${name} is missing its target-scoped Cargo fetch`)
    assert(buildIndex > fetchIndex, `${name} must build only after its Cargo fetch`)
    assert(offlineIndex > buildIndex, `${name} must force Cargo offline during its build`)
    assert.equal(
      source.split(/\r?\n/).filter((line) => line.includes('- run: cargo build')).length,
      1,
      `${name} must contain one Cargo build`,
    )
    assert.doesNotMatch(source, /prepare-rusty-v8\.mjs/)
  }
})

function policy(overrides = {}) {
  return {
    schemaVersion: 3,
    decision,
    dependencyProvenance: {
      decision,
      forbiddenTrackedPaths: ['vendor/**'],
      forbiddenTrackedBinaryExtensions: [],
      cargo: {
        allowedRegistrySources: ['registry+https://github.com/rust-lang/crates.io-index'],
        pathDependencies: 'workspace-members-only',
      },
      yarn: {
        decision: 'engineering/decisions/0022-yarn-pnp-ownership.md',
        packageManager: 'yarn@4.16.0',
        allowedResolutionProtocols: ['npm:', 'workspace:', 'patch:'],
        workspaceLocators: 'declared-workspaces-only',
        internalWorkspacePackages: {
          '@crab-dev/wake-win32-x64-msvc': 'npm/wake-win32-x64-msvc',
        },
      },
    },
    crates: ['wake_common', 'wake_ecma_parser', 'wake_app'],
    groups: { compiler: ['wake_common', 'wake_ecma_parser'] },
    cargoTreeRules: [{
      id: 'app-no-engine',
      description: 'app closure cannot contain the engine',
      from: ['wake_app'],
      denyPackages: ['deno_core'],
      decision,
      suggestion: 'spawn the isolated host',
    }],
    rules: [{
      id: 'compiler-no-app',
      description: 'compiler cannot depend on app',
      fromGroups: ['compiler'],
      deny: ['wake_app'],
      decision,
      suggestion: 'invert the dependency',
    }],
    ...overrides,
  }
}

test('rejects a forbidden compiler to app dependency', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common', 'wake_app'])],
    ['wake_app', new Set()],
  ])
  const errors = validatePolicy({ policy: policy(), packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('[compiler-no-app] wake_ecma_parser -> wake_app')))
})

test('expands allow-only groups and rejects dependencies outside the declared layer', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common'])],
    ['wake_app', new Set(['wake_ecma_parser'])],
  ])
  const layered = policy({
    groups: { compiler: ['wake_common', 'wake_ecma_parser'] },
    rules: [{
      id: 'app-only-compiler',
      description: 'app test boundary',
      from: ['wake_app'],
      allowOnlyGroups: ['compiler'],
      decision,
      suggestion: 'use the compiler layer',
    }],
  })
  assert.deepEqual(validatePolicy({ policy: layered, packages, adrRecords: activeAdrRecords }), [])

  packages.get('wake_app').add('wake_app')
  const errors = validatePolicy({ policy: layered, packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('[app-only-compiler] wake_app -> wake_app')))
})

test('repository policy rejects foundation, parser, and shell boundary regressions', () => {
  const repositoryPolicy = JSON.parse(readFileSync(new URL('../engineering/architecture-boundaries.json', import.meta.url), 'utf8'))
  const packages = new Map(repositoryPolicy.crates.map((name) => [name, new Set()]))
  packages.get('wake_common').add('wake_css')
  packages.get('wake_ecma_parser').add('wake_ecma_semantic')
  packages.get('wake_cli').add('wake_bundler')

  const errors = validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  })
  assert(errors.some((error) => error.includes('[common-is-workspace-foundation] wake_common -> wake_css')))
  assert(errors.some((error) => error.includes('[parser-does-not-own-semantic] wake_ecma_parser -> wake_ecma_semantic')))
  assert(errors.some((error) => error.includes('[shells-use-app-or-compiler] wake_cli -> wake_bundler')))
})

test('repository policy keeps the public compiler facade above one pure backend', () => {
  const repositoryPolicy = JSON.parse(readFileSync(new URL('../engineering/architecture-boundaries.json', import.meta.url), 'utf8'))
  const packages = new Map(repositoryPolicy.crates.map((name) => [name, new Set()]))
  packages.get('wake_compiler_core').add('wake_ecma_parser')
  packages.get('wake_compiler_core').add('wake_ecma_minify')
  packages.get('wake_compiler_core').add('wake_ecma_codegen')
  packages.get('wake_compiler').add('wake_compiler_core')
  packages.get('wake_tsdoc').add('wake_ecma_parser')
  packages.get('wake_test').add('wake_test_contract')
  packages.get('wake_test_host').add('wake_test_contract')
  packages.get('wake_test_host').add('wake_test')
  packages.get('wake_app').add('wake_test_contract')

  assert.deepEqual(validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  }), [])

  packages.get('wake_compiler_core').add('wake_bundler')
  packages.get('wake_compiler').add('wake_ecma_parser')
  const errors = validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  })
  assert(errors.some((error) => error.includes('[compiler-core-owns-pure-module-phases] wake_compiler_core -> wake_bundler')))
  assert(errors.some((error) => error.includes('[compiler-facade-depends-on-core-only] wake_compiler -> wake_ecma_parser')))
})

test('repository policy keeps browser policy above the driver', () => {
  const repositoryPolicy = JSON.parse(readFileSync(new URL('../engineering/architecture-boundaries.json', import.meta.url), 'utf8'))
  const packages = new Map(repositoryPolicy.crates.map((name) => [name, new Set()]))
  packages.get('wake_test').add('wake_test_browser')
  packages.get('wake_test').add('wake_test_contract')
  packages.get('wake_test_host').add('wake_test_contract')
  packages.get('wake_test_host').add('wake_test')
  packages.get('wake_app').add('wake_test_contract')
  packages.get('wake_tsdoc').add('wake_ecma_parser')
  packages.get('wake_compiler_core').add('wake_ecma_parser')
  packages.get('wake_compiler_core').add('wake_ecma_minify')
  packages.get('wake_compiler_core').add('wake_ecma_codegen')
  packages.get('wake_compiler').add('wake_compiler_core')

  assert.deepEqual(validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  }), [])

  packages.get('wake_test_browser').add('wake_test')
  const errors = validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  })
  assert(errors.some((error) => error.includes('[browser-driver-does-not-own-tests] wake_test_browser -> wake_test')))
})

test('repository policy separates the test contract, runner, host, and app', () => {
  const repositoryPolicy = JSON.parse(readFileSync(new URL('../engineering/architecture-boundaries.json', import.meta.url), 'utf8'))
  const packages = new Map(repositoryPolicy.crates.map((name) => [name, new Set()]))
  packages.get('wake_test').add('wake_test_contract')
  packages.get('wake_test_host').add('wake_test_contract')
  packages.get('wake_test_host').add('wake_test')
  packages.get('wake_app').add('wake_test_contract')
  packages.get('wake_tsdoc').add('wake_ecma_parser')
  packages.get('wake_compiler_core').add('wake_ecma_parser')
  packages.get('wake_compiler_core').add('wake_ecma_minify')
  packages.get('wake_compiler_core').add('wake_ecma_codegen')
  packages.get('wake_compiler').add('wake_compiler_core')

  assert.deepEqual(validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  }), [])

  packages.get('wake_app').add('wake_test')
  packages.get('wake_test_contract').add('wake_common')
  packages.get('wake_test_host').delete('wake_test_contract')
  const errors = validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  })
  assert(errors.some((error) => error.includes('[app-uses-test-contract-not-runner] wake_app -> wake_test')))
  assert(errors.some((error) => error.includes('[test-contract-is-data-only] wake_test_contract -> wake_common')))
  assert(errors.some((error) => error.includes('[test-host-owns-session-isolation-only] wake_test_host must directly depend on wake_test_contract')))
})

test('parses prefix-free Cargo tree output without depending on platform paths', () => {
  const packages = parseCargoTreePackages([
    'wake_cli v0.1.21 (C:\\repo\\crates\\wake_cli)',
    'wake_app v0.1.21 (/repo/crates/wake_app)',
    'deno_core v0.410.0',
    'v8 v150.4.0 (*)',
    '[build-dependencies]',
    '',
  ].join('\n'))
  assert.deepEqual(packages, new Set(['wake_cli', 'wake_app', 'deno_core', 'v8']))
})

test('Cargo tree rules reject engine leakage and require the authoritative host path', () => {
  const treePolicy = {
    groups: { shells: ['wake_cli', 'wake_node'] },
    cargoTreeRules: [
      {
        id: 'shells-no-engine',
        description: 'shell closure is engine-free',
        fromGroups: ['shells'],
        denyPackages: ['wake_test', 'deno_core', 'v8'],
        suggestion: 'spawn the host',
      },
      {
        id: 'host-has-runner',
        description: 'host owns execution',
        from: ['wake_test_host'],
        requirePackages: ['wake_test_contract', 'wake_test', 'deno_core', 'v8'],
        suggestion: 'link the authoritative runner',
      },
    ],
  }
  const packageTrees = new Map([
    ['wake_cli', new Set(['wake_cli', 'wake_app', 'wake_test_contract'])],
    ['wake_node', new Set(['wake_node', 'wake_app', 'wake_test_contract'])],
    ['wake_test_host', new Set(['wake_test_host', 'wake_test_contract', 'wake_test', 'deno_core', 'v8'])],
  ])
  assert.deepEqual(validateCargoTreeRules({ policy: treePolicy, packageTrees }), [])

  packageTrees.get('wake_cli').add('deno_core')
  packageTrees.get('wake_test_host').delete('wake_test_contract')
  const errors = validateCargoTreeRules({ policy: treePolicy, packageTrees })
  assert(errors.some((error) => error.includes('[shells-no-engine] wake_cli transitive cargo tree contains forbidden package deno_core')))
  assert(errors.some((error) => error.includes('[host-has-runner] wake_test_host transitive cargo tree is missing required package wake_test_contract')))
})

test('rejects malformed Cargo tree rules before invoking Cargo', () => {
  const malformed = policy({
    cargoTreeRules: [{
      id: 'bad-tree-rule',
      description: 'invalid tree rule',
      from: ['missing_crate'],
      denyPackages: ['v8', 'v8'],
      requirePackages: ['v8'],
      decision,
      suggestion: 'fix the rule',
    }],
  })
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set()],
    ['wake_app', new Set()],
  ])
  const errors = validatePolicy({ policy: malformed, packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('references unknown crate missing_crate')))
  assert(errors.some((error) => error.includes('denyPackages contains duplicates')))
  assert(errors.some((error) => error.includes('package v8 cannot be both denied and required')))
})

const cratesIo = 'registry+https://github.com/rust-lang/crates.io-index'
const npmIntegrity = 'sha512-PWaYA1L/q9u2u7xYQi+Y3L3Yfnie7XyLeaJICV1MGD6LprsBxcAqGjYyr0eY3p+QdsA+x/Irkt4Qif8D63+Sbw=='

function cargoFixture(root) {
  const commonId = 'wake_common 0.1.0'
  const vmId = 'wake_ecma_vm 0.1.0'
  return {
    metadata: {
      workspace_members: [commonId, vmId],
      packages: [
        {
          id: commonId,
          name: 'wake_common',
          version: '0.1.0',
          manifest_path: join(root, 'crates', 'wake_common', 'Cargo.toml'),
          dependencies: [],
        },
        {
          id: vmId,
          name: 'wake_ecma_vm',
          version: '0.1.0',
          manifest_path: join(root, 'crates', 'wake_ecma_vm', 'Cargo.toml'),
          dependencies: [
            { name: 'wake_common', source: null, req: '*', path: join(root, 'crates', 'wake_common') },
            { name: 'deno_core', source: cratesIo, req: '=0.410.0', path: null },
          ],
        },
      ],
    },
    lockText: `# generated\nversion = 4\n\n[[package]]\nname = "wake_common"\nversion = "0.1.0"\n\n[[package]]\nname = "wake_ecma_vm"\nversion = "0.1.0"\n\n[[package]]\nname = "deno_core"\nversion = "0.410.0"\nsource = "${cratesIo}"\nchecksum = "${'a'.repeat(64)}"\n`,
    policy: {
      lockfileVersion: 4,
      allowedRegistrySources: [cratesIo],
      exactPackages: { deno_core: '0.410.0' },
      exclusiveOwners: { deno_core: ['wake_ecma_vm'] },
    },
  }
}

function clone(value) {
  return JSON.parse(JSON.stringify(value))
}

test('Cargo provenance accepts crates.io locks and first-party workspace paths', () => {
  const root = join(tmpdir(), 'wake-provenance-cargo')
  const fixture = cargoFixture(root)
  assert.equal(parseCargoLock(fixture.lockText).packages.length, 3)
  assert.deepEqual(validateCargoProvenance({ ...fixture, repoRoot: root }), [])
  assert.deepEqual(validateCargoManifestSources({
    repoRoot: root,
    workspacePaths: [join(root, 'crates', 'wake_common')],
    manifests: new Map([[
      join(root, 'fuzz', 'Cargo.toml'),
      '[dependencies]\nwake_common = { path = "../crates/wake_common" }\n',
    ]]),
  }), [])
})

test('Cargo provenance rejects external paths, git sources, missing checksums, and wrong owners', () => {
  const root = join(tmpdir(), 'wake-provenance-cargo-invalid')
  const base = cargoFixture(root)

  const externalPath = clone(base.metadata)
  externalPath.packages[0].dependencies.push({
    name: 'third_party',
    source: null,
    req: '*',
    path: join(root, 'vendor', 'third_party'),
  })
  assert(validateCargoProvenance({ ...base, metadata: externalPath, repoRoot: root })
    .some((error) => error.includes('cargo-path')))

  const wrongOwner = clone(base.metadata)
  wrongOwner.packages[0].dependencies.push({ name: 'deno_core', source: cratesIo, req: '=0.410.0', path: null })
  assert(validateCargoProvenance({ ...base, metadata: wrongOwner, repoRoot: root })
    .some((error) => error.includes('cargo-owner')))

  const gitSource = clone(base.metadata)
  gitSource.packages[1].dependencies[1].source = 'git+https://example.invalid/deno_core'
  assert(validateCargoProvenance({ ...base, metadata: gitSource, repoRoot: root })
    .some((error) => error.includes('cargo-source')))

  const missingChecksum = base.lockText.replace(`checksum = "${'a'.repeat(64)}"`, '')
  assert(validateCargoProvenance({ ...base, lockText: missingChecksum, repoRoot: root })
    .some((error) => error.includes('SHA-256 checksum')))

  const sourceFreeThirdParty = base.lockText
    .replace(`source = "${cratesIo}"\n`, '')
    .replace(`checksum = "${'a'.repeat(64)}"\n`, '')
  assert(validateCargoProvenance({ ...base, lockText: sourceFreeThirdParty, repoRoot: root })
    .some((error) => error.includes('is not a workspace member')))

  const manifestErrors = validateCargoManifestSources({
    repoRoot: root,
    workspacePaths: [join(root, 'crates', 'wake_common')],
    manifests: new Map([[
      join(root, 'fuzz', 'Cargo.toml'),
      '[dependencies]\nthird_party = { path = "../vendor/third_party" }\nremote = { git = "https://example.invalid/repo" }\n',
    ]]),
  })
  assert(manifestErrors.some((error) => error.includes('cargo-path')))
  assert(manifestErrors.some((error) => error.includes('cargo-source')))
})

function yarnFixture() {
  const checksum = `10c0/${'a'.repeat(128)}`
  const platformName = '@crab-dev/wake-win32-x64-msvc'
  const platformPath = 'npm/wake-win32-x64-msvc'
  return {
    rootManifest: {
      name: 'wake-workspace',
      version: '0.1.0',
      packageManager: 'yarn@4.16.0',
      workspaces: ['npm/*'],
      dependencies: { react: '19.2.8', other: '^1.0.0' },
    },
    workspaceManifests: new Map([
      ['npm/wake', {
        name: '@crab-dev/wake',
        version: '0.1.0',
        peerDependencies: { react: '>=19.2.0 <20' },
        optionalDependencies: { [platformName]: '0.1.0' },
      }],
      [platformPath, {
        name: platformName,
        version: '0.1.0',
        os: ['win32'],
        cpu: ['x64'],
      }],
    ]),
    internalManifests: new Map([[platformPath, {
      name: platformName,
      version: '0.1.0',
      os: ['win32'],
      cpu: ['x64'],
    }]]),
    lock: {
      __metadata: { version: '10', cacheKey: '10c0' },
      '@crab-dev/wake@workspace:npm/wake': {
        version: '0.0.0-use.local',
        resolution: '@crab-dev/wake@workspace:npm/wake',
        linkType: 'soft',
      },
      [`${platformName}@workspace:${platformPath}`]: {
        version: '0.0.0-use.local',
        resolution: `${platformName}@workspace:${platformPath}`,
        linkType: 'soft',
      },
      'react@npm:19.2.8': {
        version: '19.2.8',
        resolution: 'react@npm:19.2.8',
        checksum,
      },
      'other@npm:^1.0.0': {
        version: '1.2.3',
        resolution: 'other@npm:1.2.3',
        checksum,
      },
    },
    policy: {
      lockfileVersion: 10,
      packageManager: 'yarn@4.16.0',
      allowedResolutionProtocols: ['npm:', 'workspace:', 'patch:'],
      internalWorkspacePackages: { [platformName]: platformPath },
      exactPackages: { react: '19.2.8' },
    },
  }
}

test('Yarn provenance allows manifest ranges while the lock owns exact npm artifacts', () => {
  assert.deepEqual(validateYarnProvenance(yarnFixture()), [])
})

test('Yarn provenance owns platform packages through workspace locators', () => {
  const mismatchedPin = yarnFixture()
  mismatchedPin.workspaceManifests.get('npm/wake').optionalDependencies[
    '@crab-dev/wake-win32-x64-msvc'
  ] = '0.1.1'
  assert(validateYarnProvenance(mismatchedPin).some((error) => error.includes('must equal internal')))

  const retiredRootBridge = yarnFixture()
  retiredRootBridge.rootManifest.optionalDependencies = {}
  retiredRootBridge.rootManifest.optionalDependencies[
    '@crab-dev/wake-win32-x64-msvc'
  ] = 'file:npm/wake-win32-x64-msvc'
  assert(validateYarnProvenance(retiredRootBridge)
    .some((error) => error.includes('retired file: bridge')))

  const missingManifest = yarnFixture()
  missingManifest.internalManifests.clear()
  assert(validateYarnProvenance(missingManifest).some((error) => error.includes('must define')))
})

test('Yarn provenance rejects non-registry locators, corrupt locks, and false workspace locators', () => {
  const invalidLocator = yarnFixture()
  invalidLocator.rootManifest.dependencies.other = 'file:../other'
  assert(validateYarnProvenance(invalidLocator).some((error) => error.includes('yarn-source')))

  const badResolved = yarnFixture()
  badResolved.lock['react@npm:19.2.8'].resolution = 'react@git:https://example.invalid/react'
  assert(validateYarnProvenance(badResolved).some((error) => error.includes('yarn-resolution')))

  const badChecksum = yarnFixture()
  badChecksum.lock['react@npm:19.2.8'].checksum = 'sha1-deadbeef'
  assert(validateYarnProvenance(badChecksum).some((error) => error.includes('yarn-checksum')))

  const rangedLock = yarnFixture()
  rangedLock.lock['react@npm:19.2.8'].version = '^19.2.8'
  assert(validateYarnProvenance(rangedLock).some((error) => error.includes('exact SemVer')))

  const falseLocator = yarnFixture()
  falseLocator.lock['@crab-dev/wake@workspace:npm/wake'].linkType = 'hard'
  assert(validateYarnProvenance(falseLocator).some((error) => error.includes('yarn-workspace')))

  const missingExactPin = yarnFixture()
  missingExactPin.rootManifest.dependencies.react = '^19.2.8'
  assert(validateYarnProvenance(missingExactPin).some((error) => error.includes('yarn-pin')))
})

test('repository provenance rejects vendor trees, checked-in binaries, and networked build hooks', () => {
  const policy = {
    forbiddenTrackedPaths: ['vendor/**', 'crates/**/vendor/**'],
    forbiddenTrackedBinaryExtensions: ['.node'],
    networkFreeBuild: {
      forbiddenRustBuildScriptTokens: ['https://', 'reqwest::'],
      forbiddenNpmLifecycleScripts: ['preinstall', 'install', 'postinstall'],
      offlineCargoBuildFiles: ['.github/workflows/release-npm.yml'],
    },
  }
  const validFiles = [
    'crates/wake_node/build.rs',
    'package.json',
    '.github/workflows/release-npm.yml',
  ]
  const validSources = new Map([
    ['crates/wake_node/build.rs', 'fn main() { napi_build::setup(); }'],
    ['package.json', '{"scripts":{"build":"node build.mjs"}}'],
    [
      '.github/workflows/release-npm.yml',
      [
        '- run: cargo build --locked --offline',
        '- run: cargo test --locked --offline',
        '- run: cargo clippy --locked --offline',
        '  env:',
        '    CARGO_NET_OFFLINE: "true"',
      ].join('\n'),
    ],
  ])
  assert.deepEqual(validateRepositorySources({ files: validFiles, sources: validSources, policy }), [])

  const files = [
    ...validFiles,
    'vendor/deno_core-0.410.0/lib.rs',
    'crates/wake_js_runtime/vendor/happy-dom-20.11.6/index.js',
    'npm/wake/native.node',
  ]
  const sources = new Map(validSources)
  sources.set('crates/wake_node/build.rs', 'const URL: &str = "https://example.invalid/archive";')
  sources.set('package.json', '{"scripts":{"install":"node download.mjs"}}')
  sources.set(
    '.github/workflows/release-npm.yml',
    [
      '- run: cargo build --release',
      '- run: cargo test --workspace',
      '- run: cargo clippy --workspace',
    ].join('\n'),
  )
  const errors = validateRepositorySources({ files, sources, policy })
  assert(errors.some((error) => error.includes('vendor/deno_core')))
  assert(errors.some((error) => error.includes('happy-dom')))
  assert(errors.some((error) => error.includes('native.node')))
  assert(errors.some((error) => error.includes('cargo build must include --locked --offline')))
  assert(errors.some((error) => error.includes('cargo test must include --locked --offline')))
  assert(errors.some((error) => error.includes('cargo clippy must include --locked --offline')))
})

test('rejects an unregistered workspace crate', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common'])],
    ['wake_app', new Set()],
    ['wake_new', new Set()],
  ])
  const errors = validatePolicy({ policy: policy(), packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('workspace crate wake_new is not registered')))
})

test('rejects boundary decisions that are not active', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common'])],
    ['wake_app', new Set()],
  ])
  const rejected = new Map([['0001-architecture-evolution-loop.md', { status: 'rejected' }]])
  const errors = validatePolicy({ policy: policy(), packages, adrRecords: rejected })
  assert(errors.some((error) => error.includes('must be proposed or accepted')))
})

test('rejects a boundary policy without an ADR', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common'])],
    ['wake_app', new Set()],
  ])
  const withoutDecision = policy({ decision: undefined, rules: [] })
  const errors = validatePolicy({ policy: withoutDecision, packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('decision must reference an ADR')))
})

test('rejects invalid ADR status and a missing supersedes target', () => {
  const root = join(tmpdir(), `wake-architecture-${Date.now()}-${Math.random().toString(16).slice(2)}`)
  const decisionsDir = join(root, 'engineering', 'decisions')
  mkdirSync(decisionsDir, { recursive: true })
  writeFileSync(join(decisionsDir, '0001-first.md'), `# ADR 0001: First\n\n- Status: invalid\n\n${sections('None.')}`)
  writeFileSync(join(decisionsDir, '0002-second.md'), `# ADR 0002: Second\n\n- Status: proposed\n\n${sections('[ADR 0099](0099-missing.md)')}`)
  try {
    const result = validateAdrs({ repoRoot: root, decisionsDir })
    assert(result.errors.some((error) => error.includes('status must be proposed')))
    assert(result.errors.some((error) => error.includes('Supersedes target 0099-missing.md does not exist')))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('rejects duplicate ADR numbers', () => {
  const root = join(tmpdir(), `wake-architecture-${Date.now()}-${Math.random().toString(16).slice(2)}`)
  const decisionsDir = join(root, 'engineering', 'decisions')
  mkdirSync(decisionsDir, { recursive: true })
  const body = `- Status: proposed\n\n${sections('None.')}`
  writeFileSync(join(decisionsDir, '0001-first.md'), `# ADR 0001: First\n\n${body}`)
  writeFileSync(join(decisionsDir, '0001-second.md'), `# ADR 0001: Second\n\n${body}`)
  try {
    const result = validateAdrs({ repoRoot: root, decisionsDir })
    assert(result.errors.some((error) => error.includes('ADR number 0001 duplicates')))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('Crab legacy CSS compatibility is a parser-owned resolution target', () => {
  const loader = readFileSync(
    new URL('../crates/wake_bundler/src/loader.rs', import.meta.url),
    'utf8',
  )
  const incremental = readFileSync(
    new URL('../crates/wake_bundler/src/incremental.rs', import.meta.url),
    'utf8',
  )

  assert.doesNotMatch(loader, /\bmigrate_crab_component_css_runtime\b/)
  assert.doesNotMatch(
    loader,
    /\.replace\s*\(/,
    'loader source bytes must not be rewritten for dependency compatibility (ADR 0035)',
  )
  assert.match(loader, /pub\(crate\) fn crab_component_package_dir/)
  assert.match(
    incremental,
    /fn crab_component_dependency_resolution_target[\s\S]*?DependencyKind::Import[\s\S]*?DependencyKind::ExportFrom[\s\S]*?DependencyKind::Require/,
  )
  assert.match(
    incremental,
    /crab_component_package_dir\(self\.fs\.as_ref\(\), &path\)\.is_some\(\)/,
    'style discovery and resolution compatibility must share one manifest-backed entry predicate',
  )
  assert.match(
    incremental,
    /resolve_internal_package_with_profile\(\s*&request\.resolution_specifier,/,
    'the host-owned bridge target must bypass source aliases while retaining issuer package policy',
  )
  assert.match(
    incremental,
    /dep_ids\.push\(ResolvedModuleRequest\s*\{\s*request:\s*ModuleRequestKey::new\(\s*dep\.specifier\.clone\(\),\s*dep\.kind\.into\(\),?\s*\),\s*module_id:\s*did,?\s*\}\)/,
    'ModuleRec/codegen identity must retain the source specifier',
  )
})

test('Docs page metadata is rendered from one finalized typed plan', () => {
  const docs = readFileSync(new URL('../crates/wake_docs/src/lib.rs', import.meta.url), 'utf8')
  const production = beforeCfgTestModule(docs, 'crates/wake_docs/src/lib.rs')

  assert.match(
    production,
    /struct CompiledPage\s*\{[\s\S]{0,420}route:\s*RouteInfo,[\s\S]{0,420}module_plan:\s*PageModulePlan/,
    'a compiled page must retain typed route state and an unrendered module plan',
  )
  assert.match(
    production,
    /apply_navigation\(&source_dir,\s*&mut pages\)\?[\s\S]{0,1800}for \(_, page\) in &pages \{[\s\S]{0,240}page\.render_module\(\)/,
    'page code and maps must render only after navigation has finalized route metadata',
  )
  assert.doesNotMatch(production, /\bfn\s+sync_page_metadata\b/)
  assert.doesNotMatch(
    production,
    /\.find\(\s*["']export const __wakeMeta/,
    'generated page modules must never be searched for a metadata text marker (ADR 0038)',
  )
})

test('generated Docs generations serialize one complete transaction across processes', () => {
  const docs = readFileSync(new URL('../crates/wake_docs/src/lib.rs', import.meta.url), 'utf8')
  const lockStart = docs.indexOf('fn acquire_generation_commit_lock(')
  const lockEnd = docs.indexOf('\nfn insert_generation_file(', lockStart)
  assert.ok(lockStart >= 0 && lockEnd > lockStart, 'the generated Docs lock must remain inspectable')
  const lock = docs.slice(lockStart, lockEnd)
  assert.match(docs, /GENERATION_COMMIT_LOCK_FILE:\s*&str\s*=\s*"\.wake-docs-generation\.lock"/)
  assert.match(lock, /\.create\(true\)[\s\S]*?\.truncate\(false\)/)
  assert.match(lock, /file\.try_lock\(\)/, 'the generation transaction must use an OS lock')
  assert.match(lock, /GENERATION_COMMIT_LOCK_TIMEOUT/)
  assert.match(lock, /validate_generation_commit_lock_shape\(&lock_path\)/)
  assert.match(lock, /same_file::Handle::from_file[\s\S]*?same_file::Handle::from_path/)

  const publishStart = docs.indexOf('fn publish_generation_with_ops(')
  const publishEnd = docs.indexOf('\nfn vacant_generation_sibling(', publishStart)
  assert.ok(publishStart >= 0 && publishEnd > publishStart, 'Docs publication must remain inspectable')
  const publish = docs.slice(publishStart, publishEnd)
  const processLockAt = publish.indexOf('GENERATION_TRANSACTION_LOCK')
  const projectLockRootAt = publish.indexOf('let lock_root = project_root.join(".wake")')
  const osLockAt = publish.indexOf('acquire_generation_commit_lock(&lock_root)?')
  const inspectAt = publish.indexOf('let previous = inspect_generation_tree(generated_dir)?')
  const stageAt = publish.indexOf('.prefix(".wake-docs-next-")')
  const backupAt = publish.indexOf('ops.rename(generated_dir, &path)')
  const installAt = publish.indexOf('ops.rename(stage.path(), generated_dir)')
  const cleanupAt = publish.indexOf('ops.remove_tree(backup)')
  assert.ok(
    processLockAt >= 0
      && projectLockRootAt > processLockAt
      && osLockAt > projectLockRootAt
      && inspectAt > osLockAt
      && stageAt > inspectAt
      && backupAt > stageAt
      && installAt > backupAt
      && cleanupAt > installAt,
    'inspect, stage, replace, rollback, and cleanup must remain inside both commit guards',
  )
  assert.match(docs, /fn generated_docs_publication_waits_for_a_separate_process_commit_lock\s*\(/)
  assert.match(docs, /let sibling_target = root\.join\("\.wake\/candidate\/docs\/generated"\)/)
  assert.match(docs, /\.windows\(2\)[\s\S]{0,360}\.count\(\)[\s\S]{0,40}== 1/)
  assert.match(docs, /fn nested_generated_docs_namespace_is_rejected_before_writing\s*\(/)
  assert.match(
    docs,
    /fs::create_dir\(&current\)[\s\S]{0,260}ErrorKind::AlreadyExists[\s\S]{0,320}metadata_is_link_or_reparse_point/,
    'concurrent first publication must join a safely created project lock directory',
  )
  assert.match(docs, /fn concurrent_first_generation_directory_creation_is_idempotent\s*\(/)
})

test('exact-file products use one input-disjoint publication transaction', () => {
  const output = readFileSync(
    new URL('../crates/wake_app/src/output.rs', import.meta.url),
    'utf8',
  )
  const app = readFileSync(new URL('../crates/wake_app/src/lib.rs', import.meta.url), 'utf8')
  const library = readFileSync(
    new URL('../crates/wake_app/src/library.rs', import.meta.url),
    'utf8',
  )
  const tsdoc = readFileSync(
    new URL('../crates/wake_tsdoc/src/lib.rs', import.meta.url),
    'utf8',
  )

  assert.match(output, /pub\(super\) fn publish_exact_outputs\s*\(/)
  assert.match(output, /\.prefix\("\.wake-exact-stage-"\)[\s\S]*?\.flush\(\)[\s\S]*?sync_all\(\)/)
  assert.match(output, /\.prefix\("\.wake-exact-backup-"\)/)
  assert.match(output, /fn rollback_exact_outputs\s*\(/)
  assert.match(output, /same_file::Handle::from_path/)
  assert.match(output, /left\.lexical == right\.lexical[\s\S]*?left\.physical == right\.physical/)
  assert.doesNotMatch(
    output,
    /fn remove\s*\(/,
    'an exact outfile cannot infer ownership of a stale companion from its filename',
  )

  const finishBundle = /fn finish_bundle\s*\([\s\S]*?\n}\r?\n\r?\nfn rewrite_source_map_file/.exec(app)?.[0]
  assert.ok(finishBundle, 'finish_bundle must remain a bounded application-layer operation')
  assert.match(finishBundle, /let mut candidates = vec!\[ExactOutput::write\(/)
  assert.match(finishBundle, /candidates\.push\(ExactOutput::write\(map_path, map\.as_bytes\(\)\)\)/)
  assert.match(
    finishBundle,
    /cancellation\.commit\(\|\| publish_exact_outputs\(&candidates, protected_inputs\)\)/,
    'bundle code and source-map publication must remain inside the cancellation commit fence',
  )
  assert.doesNotMatch(finishBundle, /\batomic_write\s*\(/)
  assert.match(app, /RecordingFileSystem::new\(prepared\.generation\.file_system\(\)\)/)
  assert.match(
    app,
    /filter\(\|path\| !prepared\.generation\.owns_logical_file\(path\)\)/,
    'host identity checks must exclude immutable logical inputs that deliberately do not exist on disk',
  )
  assert.match(
    app,
    /validate_not_reserved\([\s\S]{0,180}"Bundle output"/,
    'exact bundle output must remain disjoint from the logical generated-input namespace',
  )

  const token = /pub fn generate_css_token\s*\([\s\S]*?\n}\r?\n\r?\nfn validate_build/.exec(library)?.[0]
  assert.ok(token, 'token generation must remain inspectable as one exact product')
  assert.match(
    token,
    /cancellation\.commit\(\|\|\s*\{[\s\S]*?publish_exact_outputs\s*\(/,
    'token publication must remain inside the cancellation commit fence',
  )
  assert.doesNotMatch(token, /\batomic_write\s*\(/)
  assert.match(
    library,
    /ResolutionEnvironment::new\(Arc::new\(recording_fs\.clone\(\)\)\)/,
    'token resolution must record resolver, PnP, archive, and token content reads',
  )

  const docgen = /pub fn generate_docgen\s*\([\s\S]*?\n}\r?\n\r?\npub fn build_library/.exec(library)?.[0]
  assert.ok(docgen, 'Docgen must remain inspectable as one exact product')
  assert.match(docgen, /extract_component_api_with_provenance/)
  assert.match(
    docgen,
    /cancellation\.commit\(\|\|\s*\{[\s\S]*?publish_exact_outputs\s*\(/,
    'Docgen publication must remain inside the cancellation commit fence',
  )
  assert.doesNotMatch(docgen, /\batomic_write\s*\(/)
  assert.match(tsdoc, /pub fn extract_component_api_with_provenance\s*\(/)
  assert.match(tsdoc, /inputs:\s*resolver\.inputs\.into_iter\(\)\.collect\(\)/)

  const buildLibrary = /pub fn build_library\s*\([\s\S]*?\n\}\r?\n\r?\nfn write_library_file/.exec(library)?.[0]
  assert.ok(buildLibrary, 'library generation must remain inspectable as one directory product')
  assert.match(
    buildLibrary,
    /cancellation\.commit\(\|\| commit_library_outputs\(&root, staging\.path\(\)\)\)/,
    'library directory publication must remain inside the cancellation commit fence',
  )
})

test('directory and exact products share one reserved cross-process commit lock', () => {
  const output = readFileSync(
    new URL('../crates/wake_app/src/output.rs', import.meta.url),
    'utf8',
  )
  const app = readFileSync(new URL('../crates/wake_app/src/lib.rs', import.meta.url), 'utf8')

  const lockStart = output.indexOf('pub(super) fn acquire_output_commit_lock(')
  const lockEnd = output.indexOf('\n#[derive(', lockStart)
  assert.ok(lockStart >= 0 && lockEnd > lockStart, 'the shared output commit lock must remain inspectable')
  const commitLock = output.slice(lockStart, lockEnd)
  assert.match(output, /static OUTPUT_COMMIT:\s*Mutex<\(\)>/)
  assert.match(output, /OUTPUT_COMMIT_LOCK_NAMESPACE:\s*&str\s*=\s*"wake-output-publication-v1"/)
  assert.match(output, /OUTPUT_COMMIT_LOCK_FILE:\s*&str\s*=\s*"\.wake-output\.lock"/)
  assert.match(output, /OUTPUT_COMMIT_LOCK_PATH:\s*&str\s*=\s*"\/tmp\/wake-output-publication-v1\.lock"/)
  assert.match(output, /OUTPUT_COMMIT_MUTEX_NAME:\s*&str\s*=\s*"Global\\\\wake-output-publication-v1"/)
  assert.match(output, /fn open_unix_output_commit_lock[\s\S]{0,2200}create_new\(true\)[\s\S]{0,2200}libc::O_NOFOLLOW/)
  assert.match(commitLock, /OUTPUT_COMMIT\.try_lock\(\)/)
  assert.match(commitLock, /TryLockError::Poisoned\(poisoned\)/)
  assert.match(commitLock, /file\.try_lock\(\)/, 'Unix publication must use a crash-released advisory lock')
  assert.match(commitLock, /CreateMutexW/)
  assert.match(commitLock, /WaitForSingleObject/)
  assert.match(commitLock, /WAIT_OBJECT_0\s*\|\s*WAIT_ABANDONED/)
  assert.match(commitLock, /OUTPUT_COMMIT_LOCK_TIMEOUT/)
  assert.doesNotMatch(
    commitLock,
    /temp_dir|TMPDIR|TEMP|\.ancestors\(\)/,
    'the global lock namespace must not depend on process environment or write every ancestor',
  )
  assert.doesNotMatch(output, /EXACT_OUTPUT_COMMIT|acquire_output_target_lock/)
  assert.doesNotMatch(output, /exact_output_lock_anchors|directory_output_lock_anchors/)

  const exactStart = output.indexOf('fn publish_exact_outputs_inner(')
  const exactEnd = output.indexOf('\nfn rollback_exact_outputs(', exactStart)
  assert.ok(exactStart >= 0 && exactEnd > exactStart, 'exact publication must remain inspectable')
  const exact = output.slice(exactStart, exactEnd)
  const exactLockAt = exact.indexOf('acquire_output_commit_lock("exact-file")')
  const exactPreStageRevalidateAt = exact.indexOf('validate_exact_output_set(candidates, protected_inputs)?', exactLockAt)
  const exactParentAt = exact.indexOf('std::fs::create_dir_all(parent)', exactPreStageRevalidateAt)
  const exactPreStageReservationAt = exact.indexOf('validate_exact_output_commit_scope(', exactPreStageRevalidateAt)
  const exactStageAt = exact.indexOf('.prefix(".wake-exact-stage-")')
  const exactHookAt = exact.indexOf('after_staging()', exactStageAt)
  const exactFinalRevalidateAt = exact.indexOf('validate_exact_output_set(candidates, protected_inputs)?', exactHookAt)
  const exactFinalReservationAt = exact.indexOf('validate_exact_output_commit_scope(', exactFinalRevalidateAt)
  const exactMutationAt = exact.indexOf('std::fs::rename(&path, &backup)', exactFinalReservationAt)
  assert.ok(
    exactLockAt >= 0
      && exactPreStageRevalidateAt > exactLockAt
      && exactParentAt > exactPreStageRevalidateAt
      && exactPreStageReservationAt > exactParentAt
      && exactStageAt > exactPreStageReservationAt
      && exactHookAt > exactStageAt
      && exactFinalRevalidateAt > exactHookAt
      && exactFinalReservationAt > exactFinalRevalidateAt
      && exactMutationAt > exactFinalReservationAt,
    'exact outputs must lock before staging, then revalidate the complete set and reservation before mutation',
  )
  assert.match(output, /locks\.iter\(\)\.any\(\|lock\| identities_alias\(&output, lock\)\)/)

  const publishStart = app.indexOf('fn publish_staged_output<T>(')
  const publishEnd = app.indexOf('\nfn absolute_from(', publishStart)
  assert.ok(publishStart >= 0 && publishEnd > publishStart, 'staged publication must remain inspectable')
  const publish = app.slice(publishStart, publishEnd)
  const directoryStageAt = publish.indexOf('.tempdir_in(project_root)')
  const directoryMaterializeAt = publish.indexOf('materialize(&stage_root)?', directoryStageAt)
  const directoryCommitAt = publish.indexOf('cancellation.commit(', directoryMaterializeAt)
  assert.ok(
    directoryStageAt >= 0
      && directoryMaterializeAt > directoryStageAt
      && directoryCommitAt > directoryMaterializeAt,
    'application and Docs candidates must materialize in the protected project domain before commit',
  )
  assert.doesNotMatch(publish, /tempdir_in\(parent\)/)
  assert.match(publish, /commit_staged_output_with\s*\(/)
  assert.match(
    publish,
    /\|\|\s*\{[\s\S]*?resolve_safe_output_directory\s*\([\s\S]*?locked_target\s*!=\s*target/,
    'application and Docs outputs must repeat path and ownership validation after locking',
  )

  const commitStart = app.indexOf('fn commit_staged_output_with(')
  const commitEnd = app.indexOf('\nfn commit_staged_output_locked(', commitStart)
  assert.ok(commitStart >= 0 && commitEnd > commitStart, 'the locking commit wrapper must remain inspectable')
  const commit = app.slice(commitStart, commitEnd)
  const acquireAt = commit.indexOf('acquire_output_commit_lock(')
  const identityAt = commit.indexOf('resolve_physical_output_path(target)?', acquireAt)
  const reservationAt = commit.indexOf('validate_directory_output_commit_scope(', identityAt)
  const revalidateAt = commit.indexOf('revalidate()?', reservationAt)
  const mutateAt = commit.indexOf('commit_staged_output_locked(', revalidateAt)
  assert.ok(
    acquireAt >= 0
      && identityAt > acquireAt
      && reservationAt > identityAt
      && revalidateAt > reservationAt
      && mutateAt > revalidateAt,
    'global locking, physical/reservation/ownership validation, and directory mutation must stay ordered',
  )
  assert.doesNotMatch(commit, /drop\s*\(\s*commit_lock\s*\)/)

  const reservationStart = app.indexOf('fn validate_directory_output_commit_scope(')
  const reservationEnd = app.indexOf('\nfn commit_staged_output_locked(', reservationStart)
  assert.ok(reservationStart >= 0 && reservationEnd > reservationStart)
  const reservation = app.slice(reservationStart, reservationEnd)
  assert.match(reservation, /lock_path\.starts_with\(scope\)/)
  assert.match(reservation, /scope\.starts_with\(&lock_path\)/)

  const lockedStart = commitEnd + 1
  const lockedEnd = app.indexOf('\nfn rollback_output_tree(', lockedStart)
  assert.ok(lockedEnd > lockedStart, 'the locked commit and rollback boundary must remain inspectable')
  const lockedCommit = app.slice(lockedStart, lockedEnd)
  assert.match(
    lockedCommit,
    /rollback_output_tree\s*\(/,
    'failure rollback must execute before the target lock guard leaves its wrapper scope',
  )
  assert.equal(
    [...app.matchAll(/commit_staged_output_locked\s*\(/g)].length,
    2,
    'the locked mutation core must only be defined and called by the target-lock wrapper',
  )
  assert.match(app, /fn nested_directory_outputs_share_one_commit_lock\s*\(/)
  assert.match(app, /fn directory_output_preserves_existing_reserved_lock_metadata\s*\(/)
  assert.match(app, /fn exact_staging_inside_a_directory_target_is_locked_before_creation\s*\(/)
  assert.match(app, /fn child_directory_materialization_stays_outside_the_parent_output_tree\s*\(/)
  assert.match(app, /fn output_commit_lock_blocks_directory_and_exact_publishers_in_a_separate_process\s*\(/)
  assert.match(app, /fn output_commit_lock_recovers_after_a_holder_process_exits\s*\(/)
  assert.match(output, /fn exact_output_cannot_replace_the_shared_commit_lock_inode\s*\(/)
  assert.match(output, /fn exact_output_rejects_the_reserved_migration_lock_name\s*\(/)
})

function sections(supersedes) {
  return [
    '## Context\n\nContext.',
    '## Decision\n\nDecision.',
    '## Invariants\n\nInvariant.',
    '## Evidence\n\nEvidence.',
    '## Consequences\n\nConsequences.',
    '## Validation\n\nValidation.',
    `## Supersedes\n\n${supersedes}`,
    '## Removal plan\n\nNone.',
  ].join('\n\n')
}
