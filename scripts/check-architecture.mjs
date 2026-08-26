import { execFileSync } from 'node:child_process'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { parseSyml } from '@yarnpkg/parsers'

const scriptPath = fileURLToPath(import.meta.url)
const defaultRepoRoot = resolve(dirname(scriptPath), '..')
const validAdrStatuses = new Set(['proposed', 'accepted', 'superseded', 'rejected'])
const activeAdrStatuses = new Set(['proposed', 'accepted'])
const requiredAdrSections = [
  'Context',
  'Decision',
  'Invariants',
  'Evidence',
  'Consequences',
  'Validation',
  'Supersedes',
  'Removal plan',
]

function display(root, path) {
  return relative(root, path).split(sep).join('/')
}

function unique(values) {
  return [...new Set(values)]
}

const npmDependencyFields = [
  'dependencies',
  'devDependencies',
  'optionalDependencies',
  'peerDependencies',
]

function repositoryPath(path) {
  return path.replaceAll('\\', '/').replace(/^\.\//, '')
}

function pathIdentity(path) {
  const normalized = repositoryPath(resolve(path))
  return process.platform === 'win32' ? normalized.toLowerCase() : normalized
}

function globRegex(pattern) {
  let source = '^'
  for (let index = 0; index < pattern.length; index += 1) {
    const character = pattern[index]
    if (character === '*' && pattern[index + 1] === '*') {
      source += '.*'
      index += 1
    } else if (character === '*') {
      source += '[^/]*'
    } else if ('\\.^$+?()[]{}|'.includes(character)) {
      source += `\\${character}`
    } else {
      source += character
    }
  }
  return new RegExp(`${source}$`)
}

function exactSemver(version) {
  return /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/.test(version)
}

function canonicalSha512(integrity) {
  const match = /^sha512-([A-Za-z0-9+/]{86})==$/.exec(integrity ?? '')
  if (!match) return false
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
  return alphabet.indexOf(match[1].at(-1)) % 16 === 0
}

function readJson(path, errors) {
  try {
    return JSON.parse(readFileSync(path, 'utf8'))
  } catch (error) {
    errors.push(`${path}: invalid JSON (${error.message})`)
    return null
  }
}

function expandNames(rule, field, groupField, groups, errors) {
  const direct = Array.isArray(rule[field]) ? rule[field] : []
  const groupNames = Array.isArray(rule[groupField]) ? rule[groupField] : []
  const expanded = [...direct]
  for (const groupName of groupNames) {
    if (!Array.isArray(groups[groupName])) {
      errors.push(`rule ${rule.id}: unknown group ${groupName} in ${groupField}`)
      continue
    }
    expanded.push(...groups[groupName])
  }
  return unique(expanded)
}

function parseAdr(path, root, errors) {
  const source = readFileSync(path, 'utf8')
  const filename = path.split(/[\\/]/).at(-1)
  const filenameMatch = filename.match(/^(\d{4})-[a-z0-9][a-z0-9-]*\.md$/)
  if (!filenameMatch) {
    errors.push(`${display(root, path)}: ADR filename must be NNNN-kebab-case.md`)
    return null
  }
  const number = filenameMatch[1]
  const heading = source.match(/^# ADR (\d{4}):\s+\S.+$/m)
  if (!heading || heading[1] !== number) {
    errors.push(`${display(root, path)}: H1 must start with # ADR ${number}:`)
  }
  const status = source.match(/^- Status:\s*([a-z]+)\s*$/m)?.[1]
  if (!status || !validAdrStatuses.has(status)) {
    errors.push(`${display(root, path)}: status must be proposed, accepted, superseded, or rejected`)
  }
  const headings = [...source.matchAll(/^##\s+(.+?)\s*$/gm)].map((match) => match[1])
  for (const section of requiredAdrSections) {
    if (!headings.includes(section)) errors.push(`${display(root, path)}: missing ## ${section}`)
  }
  const supersedesHeading = source.match(/^## Supersedes\s*$/m)
  const supersedesStart = supersedesHeading ? supersedesHeading.index + supersedesHeading[0].length : -1
  const supersedesTail = supersedesStart >= 0 ? source.slice(supersedesStart) : ''
  const nextHeading = supersedesTail.search(/^##\s/m)
  const supersedesBody = (nextHeading >= 0 ? supersedesTail.slice(0, nextHeading) : supersedesTail).trim()
  const supersedes = [...supersedesBody.matchAll(/\((\d{4}-[a-z0-9][a-z0-9-]*\.md)\)/g)].map((match) => match[1])
  if (supersedesBody && !/^None\.?$/i.test(supersedesBody) && supersedes.length === 0) {
    errors.push(`${display(root, path)}: Supersedes must be None or link to an ADR file`)
  }
  if (status === 'superseded' && !/^- Superseded by:\s*\[[^\]]+\]\([^)]+\)\s*$/m.test(source)) {
    errors.push(`${display(root, path)}: superseded ADR must include a Superseded by link`)
  }
  return { filename, number, path, source, status, supersedes }
}

export function validateAdrs({ repoRoot, decisionsDir }) {
  const errors = []
  if (!existsSync(decisionsDir)) return [`${display(repoRoot, decisionsDir)}: decisions directory is missing`]
  const files = readdirSync(decisionsDir)
    .filter((name) => /^\d{4}-.*\.md$/.test(name) && name !== '0000-template.md')
    .sort()
  const records = new Map()
  const numbers = new Map()
  for (const name of files) {
    const record = parseAdr(join(decisionsDir, name), repoRoot, errors)
    if (!record) continue
    if (numbers.has(record.number)) {
      errors.push(`${display(repoRoot, record.path)}: ADR number ${record.number} duplicates ${numbers.get(record.number)}`)
    } else {
      numbers.set(record.number, record.filename)
    }
    records.set(record.filename, record)
  }
  for (const record of records.values()) {
    for (const target of record.supersedes) {
      if (!records.has(target)) errors.push(`${display(repoRoot, record.path)}: Supersedes target ${target} does not exist`)
    }
  }
  return { errors, records }
}

function cargoLockString(block, key) {
  const match = block.match(new RegExp(`^${key} = ("(?:[^"\\\\]|\\\\.)*")\\s*$`, 'm'))
  return match ? JSON.parse(match[1]) : null
}

export function parseCargoLock(source) {
  const version = Number(source.match(/^version = (\d+)\s*$/m)?.[1])
  if (!Number.isSafeInteger(version)) throw new Error('Cargo.lock is missing an integer lockfile version')
  const packages = source.split(/^\[\[package\]\]\s*$/m).slice(1).map((block, index) => {
    const name = cargoLockString(block, 'name')
    const packageVersion = cargoLockString(block, 'version')
    if (!name || !packageVersion) throw new Error(`Cargo.lock package ${index + 1} is missing name/version`)
    return {
      name,
      version: packageVersion,
      source: cargoLockString(block, 'source'),
      checksum: cargoLockString(block, 'checksum'),
    }
  })
  return { version, packages }
}

function workspacePackages(metadata) {
  const workspaceIds = new Set(metadata.workspace_members ?? [])
  return (metadata.packages ?? []).filter((pkg) => workspaceIds.has(pkg.id))
}

function packageIdentity(name, version) {
  return `${name}@${version}`
}

export function validateCargoProvenance({ metadata, lockText, policy, repoRoot = '.' }) {
  const errors = []
  const members = workspacePackages(metadata)
  const memberPaths = new Set(members.map((pkg) => pathIdentity(dirname(pkg.manifest_path))))
  const memberIdentities = new Set(members.map((pkg) => packageIdentity(pkg.name, pkg.version)))
  const allowedSources = new Set(policy.allowedRegistrySources ?? [])
  const exactPackages = policy.exactPackages ?? {}
  const exclusiveOwners = policy.exclusiveOwners ?? {}
  const exactReferences = new Map(Object.keys(exactPackages).map((name) => [name, 0]))

  for (const pkg of members) {
    for (const dependency of pkg.dependencies ?? []) {
      if (dependency.path && !memberPaths.has(pathIdentity(dependency.path))) {
        errors.push(`[dependency-provenance:cargo-path] ${pkg.name} -> ${dependency.name}: ${repositoryPath(relative(repoRoot, dependency.path))} is not a workspace member`)
      }
      if (dependency.source && !allowedSources.has(dependency.source)) {
        errors.push(`[dependency-provenance:cargo-source] ${pkg.name} -> ${dependency.name}: source ${dependency.source} is not an allowed crates.io registry`)
      }
      const owners = exclusiveOwners[dependency.name]
      if (Array.isArray(owners) && !owners.includes(pkg.name)) {
        errors.push(`[dependency-provenance:cargo-owner] ${dependency.name} may only be declared by ${owners.join(', ')}; found ${pkg.name}`)
      }
      if (Object.hasOwn(exactPackages, dependency.name)) {
        exactReferences.set(dependency.name, (exactReferences.get(dependency.name) ?? 0) + 1)
        if (dependency.req !== `=${exactPackages[dependency.name]}`) {
          errors.push(`[dependency-provenance:cargo-pin] ${pkg.name} must pin ${dependency.name} to =${exactPackages[dependency.name]}; found ${dependency.req}`)
        }
      }
    }
  }

  let lock
  try {
    lock = parseCargoLock(lockText)
  } catch (error) {
    return [...errors, `[dependency-provenance:cargo-lock] ${error.message}`]
  }
  if (lock.version !== policy.lockfileVersion) {
    errors.push(`[dependency-provenance:cargo-lock] Cargo.lock version must be ${policy.lockfileVersion}; found ${lock.version}`)
  }
  const sourceFree = new Set()
  for (const pkg of lock.packages) {
    const identity = packageIdentity(pkg.name, pkg.version)
    if (pkg.source === null) {
      sourceFree.add(identity)
      if (!memberIdentities.has(identity)) {
        errors.push(`[dependency-provenance:cargo-lock] ${identity} has no registry source and is not a workspace member`)
      }
      if (pkg.checksum !== null) {
        errors.push(`[dependency-provenance:cargo-lock] workspace package ${identity} must not carry a registry checksum`)
      }
      continue
    }
    if (!allowedSources.has(pkg.source)) {
      errors.push(`[dependency-provenance:cargo-lock] ${identity} uses forbidden source ${pkg.source}`)
    }
    if (!/^[0-9a-f]{64}$/.test(pkg.checksum ?? '')) {
      errors.push(`[dependency-provenance:cargo-lock] ${identity} must carry a lowercase SHA-256 checksum`)
    }
  }
  for (const identity of memberIdentities) {
    if (!sourceFree.has(identity)) {
      errors.push(`[dependency-provenance:cargo-lock] workspace package ${identity} is missing from Cargo.lock as a source-free package`)
    }
  }
  for (const [name, version] of Object.entries(exactPackages)) {
    const matches = lock.packages.filter((pkg) => pkg.name === name && pkg.version === version && allowedSources.has(pkg.source))
    if (matches.length !== 1) {
      errors.push(`[dependency-provenance:cargo-pin] Cargo.lock must contain exactly one crates.io ${name}@${version}; found ${matches.length}`)
    }
    if ((exactReferences.get(name) ?? 0) === 0) {
      errors.push(`[dependency-provenance:cargo-pin] no workspace crate declares required ${name} =${version}`)
    }
  }
  return errors
}

function dependencyTable(section) {
  return /(?:^|\.)(?:dependencies|dev-dependencies|build-dependencies)(?:\.|$)/.test(section)
}

export function validateCargoManifestSources({ manifests, workspacePaths, repoRoot = '.' }) {
  const errors = []
  const allowedPaths = new Set([...workspacePaths].map(pathIdentity))
  for (const [manifestPath, source] of manifests) {
    let section = ''
    for (const [lineIndex, line] of source.split(/\r?\n/).entries()) {
      const heading = line.match(/^\s*\[+([^\]]+)\]+\s*(?:#.*)?$/)
      if (heading) {
        section = heading[1].trim()
        continue
      }
      if (!dependencyTable(section)) continue
      for (const match of line.matchAll(/\bpath\s*=\s*"([^"]+)"/g)) {
        const target = resolve(dirname(manifestPath), match[1])
        if (!allowedPaths.has(pathIdentity(target))) {
          errors.push(`[dependency-provenance:cargo-path] ${repositoryPath(relative(repoRoot, manifestPath))}:${lineIndex + 1} resolves outside a workspace member: ${repositoryPath(relative(repoRoot, target))}`)
        }
      }
      if (/\bgit\s*=\s*"/.test(line) || /\bregistry\s*=\s*"/.test(line)) {
        errors.push(`[dependency-provenance:cargo-source] ${repositoryPath(relative(repoRoot, manifestPath))}:${lineIndex + 1} uses a git or alternate-registry dependency`)
      }
    }
  }
  return errors
}

function workspacePatterns(manifest) {
  if (Array.isArray(manifest.workspaces)) return manifest.workspaces
  if (Array.isArray(manifest.workspaces?.packages)) return manifest.workspaces.packages
  return []
}

export function validateYarnProvenance({
  lock,
  rootManifest,
  workspaceManifests,
  internalManifests = new Map(),
  policy,
}) {
  const errors = []
  const internalWorkspacePackages = new Map(
    Object.entries(policy.internalWorkspacePackages ?? {})
      .map(([name, configuredPath]) => [name, repositoryPath(configuredPath)]),
  )
  if (lock.__metadata?.version !== String(policy.lockfileVersion)) {
    errors.push(`[dependency-provenance:yarn-lock] yarn.lock version must be ${policy.lockfileVersion}; found ${lock.__metadata?.version ?? 'missing'}`)
  }
  if (rootManifest.packageManager !== policy.packageManager) {
    errors.push(`[dependency-provenance:yarn-manager] package.json packageManager must be ${policy.packageManager}; found ${rootManifest.packageManager ?? 'missing'}`)
  }

  const manifests = new Map([['', rootManifest], ...workspaceManifests])
  for (const [manifestPath, manifest] of manifests) {
    for (const field of npmDependencyFields) {
      for (const [name, locator] of Object.entries(manifest[field] ?? {})) {
        if (/^(?:file|link|portal|git(?:\+[^:]*)?|https?):/i.test(locator) || /^git@/i.test(locator)) {
          errors.push(`[dependency-provenance:yarn-source] ${manifestPath || 'package.json'} ${field}.${name} uses a forbidden source locator ${locator}`)
        }
        if (locator.startsWith('workspace:') && ![...workspaceManifests.values()].some((workspace) => workspace.name === name)) {
          errors.push(`[dependency-provenance:yarn-workspace] ${manifestPath || 'package.json'} ${field}.${name} points to an undeclared workspace`)
        }
      }
    }
  }

  const workspaceNames = new Set()
  for (const [workspacePath, manifest] of workspaceManifests) {
    if (!manifest.name) {
      errors.push(`[dependency-provenance:yarn-workspace] ${workspacePath}/package.json is missing name`)
      continue
    }
    workspaceNames.add(manifest.name)
    const lockedWorkspace = Object.values(lock).find((entry) => entry?.resolution === `${manifest.name}@workspace:${workspacePath}`)
    if (!lockedWorkspace || lockedWorkspace.linkType !== 'soft') {
      errors.push(`[dependency-provenance:yarn-workspace] yarn.lock is missing ${manifest.name}@workspace:${workspacePath}`)
    }
  }

  const mainWorkspace = [...workspaceManifests]
    .find(([, manifest]) => manifest.name === '@crab-dev/wake')
  for (const [name, manifestPath] of internalWorkspacePackages) {
    const internalManifest = internalManifests.get(manifestPath)
    if (
      !internalManifest ||
      internalManifest.name !== name ||
      !exactSemver(internalManifest.version ?? '')
    ) {
      errors.push(`[dependency-provenance:yarn-internal-workspace] ${manifestPath}/package.json must define ${name} at one exact version`)
      continue
    }
    if (!mainWorkspace) {
      errors.push('[dependency-provenance:yarn-internal-workspace] @crab-dev/wake must be a declared workspace')
      continue
    }
    const [mainWorkspacePath, mainManifest] = mainWorkspace
    const requested = mainManifest.optionalDependencies?.[name]
    if (requested !== internalManifest.version) {
      errors.push(`[dependency-provenance:yarn-internal-workspace] ${mainWorkspacePath}/package.json optionalDependencies.${name} must equal internal ${internalManifest.version}; found ${requested ?? 'missing'}`)
    }
    if (Object.hasOwn(rootManifest.optionalDependencies ?? {}, name)) {
      errors.push(`[dependency-provenance:yarn-internal-workspace] package.json must not contain the retired file: bridge for ${name}`)
    }
    if (!workspaceNames.has(name)) {
      errors.push(`[dependency-provenance:yarn-internal-workspace] ${name} must be a declared workspace`)
    }
  }

  for (const [descriptor, entry] of Object.entries(lock)) {
    if (descriptor === '__metadata') continue
    if (!entry || typeof entry !== 'object') {
      errors.push(`[dependency-provenance:yarn-lock] ${descriptor} is not a valid lock entry`)
      continue
    }
    if (entry.resolution?.includes('@workspace:')) {
      if (entry.linkType !== 'soft') errors.push(`[dependency-provenance:yarn-workspace] ${descriptor} must be a soft workspace locator`)
      continue
    }
    if (!exactSemver(entry.version ?? '')) {
      errors.push(`[dependency-provenance:yarn-lock] ${descriptor} must resolve to an exact SemVer; found ${entry.version ?? 'missing'}`)
    }
    if (!entry.resolution?.includes('@npm:') && !entry.resolution?.includes('@patch:')) {
      errors.push(`[dependency-provenance:yarn-resolution] ${descriptor} must use an npm or audited builtin patch resolution; found ${entry.resolution ?? 'missing'}`)
    }
    if (!entry.conditions && !/^10c0\/[0-9a-f]{128}$/.test(entry.checksum ?? '')) {
      errors.push(`[dependency-provenance:yarn-checksum] ${descriptor} must carry one canonical Yarn checksum`)
    }
  }

  for (const [name, version] of Object.entries(policy.exactPackages ?? {})) {
    const declarations = []
    for (const [manifestPath, manifest] of manifests) {
      for (const field of ['dependencies', 'devDependencies', 'optionalDependencies']) {
        if (Object.hasOwn(manifest[field] ?? {}, name)) declarations.push([manifestPath || 'package.json', field, manifest[field][name]])
      }
    }
    if (declarations.length === 0) {
      errors.push(`[dependency-provenance:yarn-pin] no install-bearing manifest pins ${name}@${version}`)
    }
    for (const [manifestPath, field, locator] of declarations) {
      if (locator !== version) {
        errors.push(`[dependency-provenance:yarn-pin] ${manifestPath} ${field}.${name} must equal ${version}; found ${locator}`)
      }
    }
    const locked = Object.values(lock).find((entry) => entry?.resolution === `${name}@npm:${version}`)
    if (!locked) {
      errors.push(`[dependency-provenance:yarn-pin] yarn.lock must pin ${name}@npm:${version}`)
    }
  }
  return errors
}

export function validateRepositorySources({ files, sources, policy }) {
  const errors = []
  const forbiddenPaths = (policy.forbiddenTrackedPaths ?? []).map((pattern) => [pattern, globRegex(pattern)])
  const forbiddenExtensions = (policy.forbiddenTrackedBinaryExtensions ?? []).map((extension) => extension.toLowerCase())
  const paths = files.map(repositoryPath)
  for (const [pattern, regex] of forbiddenPaths) {
    const matches = paths.filter((path) => regex.test(path))
    if (matches.length > 0) {
      errors.push(`[dependency-provenance:repository] ${matches[0]} matches forbidden third-party source path ${pattern} (${matches.length} file${matches.length === 1 ? '' : 's'})`)
    }
  }
  for (const path of paths) {
    const lowercase = path.toLowerCase()
    const extension = forbiddenExtensions.find((candidate) => lowercase.endsWith(candidate))
    if (extension) errors.push(`[dependency-provenance:repository] ${path} is a forbidden checked-in binary/archive (${extension})`)
  }

  const network = policy.networkFreeBuild ?? {}
  const forbiddenTokens = (network.forbiddenRustBuildScriptTokens ?? []).map((token) => token.toLowerCase())
  for (const path of files.filter((candidate) => /(?:^|\/)build\.rs$/.test(repositoryPath(candidate)))) {
    const source = sources.get(repositoryPath(path)) ?? ''
    const lowercase = source.toLowerCase()
    for (const token of forbiddenTokens) {
      if (lowercase.includes(token)) {
        errors.push(`[dependency-provenance:build-network] ${repositoryPath(path)} contains forbidden network token ${token}`)
      }
    }
  }
  for (const path of files.filter((candidate) => repositoryPath(candidate).endsWith('package.json'))) {
    const source = sources.get(repositoryPath(path))
    if (source === undefined) continue
    let manifest
    try {
      manifest = JSON.parse(source)
    } catch {
      continue
    }
    for (const lifecycle of network.forbiddenNpmLifecycleScripts ?? []) {
      if (Object.hasOwn(manifest.scripts ?? {}, lifecycle)) {
        errors.push(`[dependency-provenance:build-network] ${repositoryPath(path)} must not define npm lifecycle script ${lifecycle}`)
      }
    }
  }
  for (const path of network.offlineCargoBuildFiles ?? []) {
    const source = sources.get(path)
    if (source === undefined) {
      errors.push(`[dependency-provenance:build-network] required build command file ${path} is missing`)
      continue
    }
    for (const [index, line] of source.split(/\r?\n/).entries()) {
      const command = /\bcargo\s+(build|test|clippy)\b/.exec(line)?.[1]
      if (!command) continue
      if (!line.includes('--locked') || !line.includes('--offline')) {
        errors.push(`[dependency-provenance:build-network] ${path}:${index + 1} cargo ${command} must include --locked --offline`)
      }
    }
    if (!/CARGO_NET_OFFLINE:\s*["']?true["']?/i.test(source)) {
      errors.push(`[dependency-provenance:build-network] ${path} must set CARGO_NET_OFFLINE=true for formal builds`)
    }
  }
  return errors
}

export function parseCargoTreePackages(source) {
  const packages = new Set()
  for (const line of source.split(/\r?\n/)) {
    const match = /^([A-Za-z0-9][A-Za-z0-9_-]*)\s+v\S+(?:\s|$)/.exec(line.trim())
    if (match) packages.add(match[1])
  }
  return packages
}

function cargoTreeRuleSources(rule, groups) {
  const sources = Array.isArray(rule.from) ? [...rule.from] : []
  for (const groupName of Array.isArray(rule.fromGroups) ? rule.fromGroups : []) {
    if (Array.isArray(groups[groupName])) sources.push(...groups[groupName])
  }
  return unique(sources)
}

export function validateCargoTreeRules({ policy, packageTrees }) {
  const errors = []
  const groups = policy?.groups ?? {}
  for (const rule of policy?.cargoTreeRules ?? []) {
    const denied = new Set(Array.isArray(rule.denyPackages) ? rule.denyPackages : [])
    const required = new Set(Array.isArray(rule.requirePackages) ? rule.requirePackages : [])
    for (const source of cargoTreeRuleSources(rule, groups)) {
      const packages = packageTrees.get(source)
      if (!(packages instanceof Set)) continue
      for (const dependency of denied) {
        if (packages.has(dependency)) {
          errors.push(`[${rule.id}] ${source} transitive cargo tree contains forbidden package ${dependency}: ${rule.description}. ${rule.suggestion}`)
        }
      }
      for (const dependency of required) {
        if (!packages.has(dependency)) {
          errors.push(`[${rule.id}] ${source} transitive cargo tree is missing required package ${dependency}: ${rule.description}. ${rule.suggestion}`)
        }
      }
    }
  }
  return errors
}

export function validatePolicy({ policy, packages, adrRecords, policyPath = 'engineering/architecture-boundaries.json' }) {
  const errors = []
  if (!policy || policy.schemaVersion !== 3) errors.push(`${policyPath}: schemaVersion must be 3`)
  const declared = Array.isArray(policy?.crates) ? policy.crates : []
  const declaredSet = new Set(declared)
  if (declared.length !== declaredSet.size) errors.push(`${policyPath}: crates contains duplicate names`)
  const packageNames = new Set(packages.keys())
  for (const name of packageNames) if (!declaredSet.has(name)) errors.push(`${policyPath}: workspace crate ${name} is not registered`)
  for (const name of declaredSet) if (!packageNames.has(name)) errors.push(`${policyPath}: registered crate ${name} does not exist in workspace`)

  const groups = policy?.groups ?? {}
  for (const [groupName, members] of Object.entries(groups)) {
    if (!Array.isArray(members)) {
      errors.push(`${policyPath}: group ${groupName} must be an array`)
      continue
    }
    if (members.length !== new Set(members).size) errors.push(`${policyPath}: group ${groupName} contains duplicates`)
    for (const name of members) if (!declaredSet.has(name)) errors.push(`${policyPath}: group ${groupName} references unknown crate ${name}`)
  }

  const decisionPaths = new Set()
  if (typeof policy?.decision === 'string') decisionPaths.add(policy.decision)
  else errors.push(`${policyPath}: decision must reference an ADR`)
  const provenance = policy?.dependencyProvenance
  if (!provenance || typeof provenance !== 'object') {
    errors.push(`${policyPath}: dependencyProvenance is required`)
  } else {
    if (typeof provenance.decision === 'string') decisionPaths.add(provenance.decision)
    else errors.push(`${policyPath}: dependencyProvenance.decision must reference an ADR`)
    if (!Array.isArray(provenance.forbiddenTrackedPaths) || provenance.forbiddenTrackedPaths.length === 0) {
      errors.push(`${policyPath}: dependencyProvenance.forbiddenTrackedPaths must be a non-empty array`)
    }
    if (!Array.isArray(provenance.forbiddenTrackedBinaryExtensions)) {
      errors.push(`${policyPath}: dependencyProvenance.forbiddenTrackedBinaryExtensions must be an array`)
    }
    if (!Array.isArray(provenance.cargo?.allowedRegistrySources) || provenance.cargo.allowedRegistrySources.length !== 1) {
      errors.push(`${policyPath}: dependencyProvenance.cargo must declare exactly one registry source`)
    }
    if (provenance.cargo?.pathDependencies !== 'workspace-members-only') {
      errors.push(`${policyPath}: dependencyProvenance.cargo.pathDependencies must be workspace-members-only`)
    }
    if (!Array.isArray(provenance.yarn?.allowedResolutionProtocols) || !provenance.yarn.allowedResolutionProtocols.includes('npm:')) {
      errors.push(`${policyPath}: dependencyProvenance.yarn must allow npm: resolutions`)
    }
    if (provenance.yarn?.workspaceLocators !== 'declared-workspaces-only') {
      errors.push(`${policyPath}: dependencyProvenance.yarn.workspaceLocators must be declared-workspaces-only`)
    }
    if (provenance.yarn?.packageManager !== 'yarn@4.16.0') {
      errors.push(`${policyPath}: dependencyProvenance.yarn.packageManager must be yarn@4.16.0`)
    }
    if (typeof provenance.yarn?.decision === 'string') decisionPaths.add(provenance.yarn.decision)
    else errors.push(`${policyPath}: dependencyProvenance.yarn.decision must reference an ADR`)
    if (
      !provenance.yarn?.internalWorkspacePackages ||
      typeof provenance.yarn.internalWorkspacePackages !== 'object' ||
      Array.isArray(provenance.yarn.internalWorkspacePackages) ||
      Object.keys(provenance.yarn.internalWorkspacePackages).length === 0
    ) {
      errors.push(`${policyPath}: dependencyProvenance.yarn.internalWorkspacePackages must be a non-empty object`)
    }
    for (const [dependency, owners] of Object.entries(provenance.cargo?.exclusiveOwners ?? {})) {
      if (!Array.isArray(owners) || owners.length === 0) {
        errors.push(`${policyPath}: exclusive owners for ${dependency} must be a non-empty array`)
      } else {
        for (const owner of owners) if (!declaredSet.has(owner)) errors.push(`${policyPath}: exclusive owner ${owner} for ${dependency} is not a workspace crate`)
      }
    }
  }
  const ruleIds = new Set()
  const cargoTreeRules = policy?.cargoTreeRules
  if (!Array.isArray(cargoTreeRules) || cargoTreeRules.length === 0) {
    errors.push(`${policyPath}: cargoTreeRules must be a non-empty array`)
  }
  for (const rule of Array.isArray(cargoTreeRules) ? cargoTreeRules : []) {
    if (!rule.id || ruleIds.has(rule.id)) errors.push(`${policyPath}: every rule requires a unique id`)
    ruleIds.add(rule.id)
    if (!rule.description || !rule.suggestion) errors.push(`rule ${rule.id}: description and suggestion are required`)
    if (typeof rule.decision !== 'string') errors.push(`rule ${rule.id}: decision must reference an ADR`)
    else decisionPaths.add(rule.decision)
    const from = expandNames(rule, 'from', 'fromGroups', groups, errors)
    for (const name of from) {
      if (!declaredSet.has(name)) errors.push(`rule ${rule.id}: references unknown crate ${name}`)
    }
    if (from.length === 0) errors.push(`rule ${rule.id}: requires at least one source crate`)
    for (const field of ['denyPackages', 'requirePackages']) {
      if (rule[field] !== undefined && !Array.isArray(rule[field])) {
        errors.push(`rule ${rule.id}: ${field} must be an array`)
      }
      const values = Array.isArray(rule[field]) ? rule[field] : []
      if (values.length !== new Set(values).size) errors.push(`rule ${rule.id}: ${field} contains duplicates`)
      for (const value of values) {
        if (typeof value !== 'string' || value.length === 0) errors.push(`rule ${rule.id}: ${field} must contain non-empty package names`)
      }
    }
    const denied = Array.isArray(rule.denyPackages) ? rule.denyPackages : []
    const required = Array.isArray(rule.requirePackages) ? rule.requirePackages : []
    if (denied.length === 0 && required.length === 0) {
      errors.push(`rule ${rule.id}: requires denyPackages or requirePackages`)
    }
    const requiredSet = new Set(required)
    for (const name of denied) {
      if (requiredSet.has(name)) errors.push(`rule ${rule.id}: package ${name} cannot be both denied and required`)
    }
  }
  for (const rule of policy?.rules ?? []) {
    if (!rule.id || ruleIds.has(rule.id)) errors.push(`${policyPath}: every rule requires a unique id`)
    ruleIds.add(rule.id)
    if (!rule.description || !rule.suggestion) errors.push(`rule ${rule.id}: description and suggestion are required`)
    if (typeof rule.decision !== 'string') errors.push(`rule ${rule.id}: decision must reference an ADR`)
    else decisionPaths.add(rule.decision)
    const from = expandNames(rule, 'from', 'fromGroups', groups, errors)
    const denied = expandNames(rule, 'deny', 'denyGroups', groups, errors)
    const required = expandNames(rule, 'require', 'requireGroups', groups, errors)
    const hasAllowOnly = Array.isArray(rule.allowOnly) || Array.isArray(rule.allowOnlyGroups)
    const allowOnly = hasAllowOnly
      ? expandNames(rule, 'allowOnly', 'allowOnlyGroups', groups, errors)
      : null
    for (const name of [...from, ...denied, ...required, ...(allowOnly ?? [])]) {
      if (!declaredSet.has(name)) errors.push(`rule ${rule.id}: references unknown crate ${name}`)
    }
    if (from.length === 0) errors.push(`rule ${rule.id}: requires at least one source crate`)
    if (denied.length === 0 && allowOnly === null && required.length === 0) {
      errors.push(`rule ${rule.id}: requires deny/denyGroups, allowOnly, or require/requireGroups`)
    }
    const deniedSet = new Set(denied)
    const allowedSet = allowOnly === null ? null : new Set(allowOnly)
    for (const dependency of required) {
      if (deniedSet.has(dependency)) {
        errors.push(`rule ${rule.id}: ${dependency} cannot be both denied and required`)
      }
      if (allowedSet !== null && !allowedSet.has(dependency)) {
        errors.push(`rule ${rule.id}: required dependency ${dependency} is outside allowOnly`)
      }
    }
    for (const source of from) {
      const dependencies = packages.get(source) ?? new Set()
      for (const dependency of dependencies) {
        if (!declaredSet.has(dependency)) continue
        const blocked = deniedSet.has(dependency) || (allowedSet !== null && !allowedSet.has(dependency))
        if (blocked) {
          errors.push(`[${rule.id}] ${source} -> ${dependency}: ${rule.description}. ${rule.suggestion}`)
        }
      }
      for (const dependency of required) {
        if (!dependencies.has(dependency)) {
          errors.push(`[${rule.id}] ${source} must directly depend on ${dependency}: ${rule.description}. ${rule.suggestion}`)
        }
      }
    }
  }

  for (const decisionPath of decisionPaths) {
    const filename = decisionPath.split('/').at(-1)
    const record = adrRecords.get(filename)
    if (!record) errors.push(`${policyPath}: decision ${decisionPath} does not exist`)
    else if (!activeAdrStatuses.has(record.status)) {
      errors.push(`${policyPath}: decision ${decisionPath} must be proposed or accepted, found ${record.status}`)
    }
  }
  return errors
}

function cargoMetadata(repoRoot) {
  const output = execFileSync('cargo', ['metadata', '--locked', '--offline', '--no-deps', '--format-version', '1'], {
    cwd: repoRoot,
    encoding: 'utf8',
    windowsHide: true,
  })
  return JSON.parse(output)
}

function cargoTreePackages(repoRoot, packageName) {
  const output = execFileSync('cargo', [
    'tree',
    '--locked',
    '--offline',
    '--all-features',
    '--edges',
    'normal,build',
    '--target',
    'all',
    '--prefix',
    'none',
    '--format',
    '{p}',
    '-p',
    packageName,
  ], {
    cwd: repoRoot,
    encoding: 'utf8',
    maxBuffer: 32 * 1024 * 1024,
    windowsHide: true,
  })
  return parseCargoTreePackages(output)
}

function cargoPackages(metadata) {
  const packages = new Map()
  const workspaceIds = new Set(metadata.workspace_members)
  const workspaceNames = new Set(metadata.packages.filter((pkg) => workspaceIds.has(pkg.id)).map((pkg) => pkg.name))
  for (const pkg of metadata.packages) {
    if (!workspaceIds.has(pkg.id)) continue
    packages.set(pkg.name, new Set(pkg.dependencies
      .filter((dependency) => dependency.kind !== 'dev')
      .map((dependency) => dependency.name)
      .filter((name) => workspaceNames.has(name))))
  }
  return packages
}

function repositoryFiles(repoRoot) {
  const output = execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard', '-z'], {
    cwd: repoRoot,
    encoding: 'utf8',
    maxBuffer: 32 * 1024 * 1024,
    windowsHide: true,
  })
  return unique(output.split('\0').filter(Boolean).map(repositoryPath)).sort()
}

function npmWorkspaces({ repoRoot, rootManifest, files, errors }) {
  const patterns = workspacePatterns(rootManifest).map((pattern) => [pattern, globRegex(`${repositoryPath(pattern)}/package.json`)])
  const workspaces = new Map()
  for (const [pattern, regex] of patterns) {
    const matches = files.filter((path) => regex.test(path))
    if (matches.length === 0) errors.push(`package.json workspace pattern ${pattern} matches no package manifest`)
    for (const path of matches) {
      const manifest = readJson(join(repoRoot, path), errors)
      if (manifest) workspaces.set(repositoryPath(dirname(path)), manifest)
    }
  }
  return workspaces
}

export function checkYarnRepository(
  repoRoot = defaultRepoRoot,
  { policy: providedPolicy, files: providedFiles } = {},
) {
  const errors = []
  const policy = providedPolicy
    ?? readJson(join(repoRoot, 'engineering', 'architecture-boundaries.json'), errors)
  const yarnPolicy = policy?.dependencyProvenance?.yarn
  if (!yarnPolicy) {
    errors.push('engineering/architecture-boundaries.json: dependencyProvenance.yarn is required')
    return errors
  }
  let files = providedFiles
  if (!files) {
    try {
      files = repositoryFiles(repoRoot)
    } catch (error) {
      errors.push(`git ls-files failed: ${error.message}`)
      return errors
    }
  }
  const rootManifest = readJson(join(repoRoot, yarnPolicy.manifest), errors)
  let yarnLock = null
  try {
    yarnLock = parseSyml(readFileSync(join(repoRoot, yarnPolicy.lockfile), 'utf8'))
  } catch (error) {
    errors.push(`${yarnPolicy.lockfile}: ${error.message}`)
  }
  if (!rootManifest || !yarnLock) return errors

  const workspaceManifests = npmWorkspaces({ repoRoot, rootManifest, files, errors })
  const internalManifests = new Map()
  for (const configuredPath of Object.values(yarnPolicy.internalWorkspacePackages ?? {})) {
    const manifestPath = repositoryPath(configuredPath)
    const manifest = readJson(join(repoRoot, manifestPath, 'package.json'), errors)
    if (manifest) internalManifests.set(manifestPath, manifest)
  }
  errors.push(...validateYarnProvenance({
    lock: yarnLock,
    rootManifest,
    workspaceManifests,
    internalManifests,
    policy: yarnPolicy,
  }))
  return errors
}

function repositorySources(repoRoot, files, provenance) {
  const required = new Set((provenance.networkFreeBuild?.offlineCargoBuildFiles ?? []).map(repositoryPath))
  for (const path of files) {
    if (/(?:^|\/)build\.rs$/.test(path) || path.endsWith('package.json')) required.add(path)
  }
  const sources = new Map()
  for (const path of required) {
    const absolute = join(repoRoot, path)
    if (existsSync(absolute)) sources.set(path, readFileSync(absolute, 'utf8'))
  }
  return sources
}

export function checkRepository(repoRoot = defaultRepoRoot) {
  const policyPath = join(repoRoot, 'engineering', 'architecture-boundaries.json')
  const decisionsDir = join(repoRoot, 'engineering', 'decisions')
  const errors = []
  const policy = readJson(policyPath, errors)
  const adrResult = validateAdrs({ repoRoot, decisionsDir })
  errors.push(...adrResult.errors)
  if (policy) {
    let metadata = null
    try {
      metadata = cargoMetadata(repoRoot)
    } catch (error) {
      errors.push(`cargo metadata failed: ${error.message}`)
    }
    if (metadata) {
      const packages = cargoPackages(metadata)
      errors.push(...validatePolicy({
        policy,
        packages,
        adrRecords: adrResult.records,
        policyPath: display(repoRoot, policyPath),
      }))
      const packageTrees = new Map()
      const configuredCargoTreeRules = Array.isArray(policy.cargoTreeRules)
        ? policy.cargoTreeRules
        : []
      const cargoTreeRoots = unique(configuredCargoTreeRules
        .flatMap((rule) => cargoTreeRuleSources(rule, policy.groups ?? {})))
      for (const packageName of cargoTreeRoots) {
        try {
          packageTrees.set(packageName, cargoTreePackages(repoRoot, packageName))
        } catch (error) {
          errors.push(`cargo tree failed for ${packageName}: ${error.message}`)
        }
      }
      errors.push(...validateCargoTreeRules({ policy, packageTrees }))
      const provenance = policy.dependencyProvenance
      if (provenance) {
        let files = null
        try {
          files = repositoryFiles(repoRoot)
        } catch (error) {
          errors.push(`git ls-files failed: ${error.message}`)
        }
        if (files) {
          const sources = repositorySources(repoRoot, files, provenance)
          errors.push(...validateRepositorySources({ files, sources, policy: provenance }))

          const cargoLockPath = join(repoRoot, provenance.cargo.lockfile)
          if (!existsSync(cargoLockPath)) {
            errors.push(`${provenance.cargo.lockfile}: required Cargo lockfile is missing`)
          } else {
            errors.push(...validateCargoProvenance({
              metadata,
              lockText: readFileSync(cargoLockPath, 'utf8'),
              policy: provenance.cargo,
              repoRoot,
            }))
          }
          const manifestSources = new Map(files
            .filter((path) => path.endsWith('Cargo.toml'))
            .map((path) => [join(repoRoot, path), readFileSync(join(repoRoot, path), 'utf8')]))
          errors.push(...validateCargoManifestSources({
            manifests: manifestSources,
            workspacePaths: workspacePackages(metadata).map((pkg) => dirname(pkg.manifest_path)),
            repoRoot,
          }))

          errors.push(...checkYarnRepository(repoRoot, { policy, files }))
        }
      }
    }
  }
  return errors
}

const invokedPath = process.argv[1] ? pathToFileURL(resolve(process.argv[1])).href : null
if (import.meta.url === invokedPath) {
  const errors = checkRepository()
  if (errors.length) {
    console.error(`Architecture check failed with ${errors.length} error(s):`)
    for (const error of errors) console.error(`- ${error}`)
    process.exitCode = 1
  } else {
    console.log('Architecture check passed: workspace boundaries, transitive Cargo closures, ADRs, and dependency provenance are valid.')
  }
}
