import { execFileSync } from 'node:child_process'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

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

export function validatePolicy({ policy, packages, adrRecords, policyPath = 'engineering/architecture-boundaries.json' }) {
  const errors = []
  if (!policy || policy.schemaVersion !== 1) errors.push(`${policyPath}: schemaVersion must be 1`)
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
  const ruleIds = new Set()
  for (const rule of policy?.rules ?? []) {
    if (!rule.id || ruleIds.has(rule.id)) errors.push(`${policyPath}: every rule requires a unique id`)
    ruleIds.add(rule.id)
    if (!rule.description || !rule.suggestion) errors.push(`rule ${rule.id}: description and suggestion are required`)
    if (typeof rule.decision !== 'string') errors.push(`rule ${rule.id}: decision must reference an ADR`)
    else decisionPaths.add(rule.decision)
    const from = expandNames(rule, 'from', 'fromGroups', groups, errors)
    const denied = expandNames(rule, 'deny', 'denyGroups', groups, errors)
    const hasAllowOnly = Array.isArray(rule.allowOnly) || Array.isArray(rule.allowOnlyGroups)
    const allowOnly = hasAllowOnly
      ? expandNames(rule, 'allowOnly', 'allowOnlyGroups', groups, errors)
      : null
    for (const name of [...from, ...denied, ...(allowOnly ?? [])]) {
      if (!declaredSet.has(name)) errors.push(`rule ${rule.id}: references unknown crate ${name}`)
    }
    if (from.length === 0) errors.push(`rule ${rule.id}: requires at least one source crate`)
    if (denied.length === 0 && allowOnly === null) errors.push(`rule ${rule.id}: requires deny/denyGroups or allowOnly`)
    const deniedSet = new Set(denied)
    const allowedSet = allowOnly === null ? null : new Set(allowOnly)
    for (const source of from) {
      const dependencies = packages.get(source) ?? new Set()
      for (const dependency of dependencies) {
        if (!declaredSet.has(dependency)) continue
        const blocked = deniedSet.has(dependency) || (allowedSet !== null && !allowedSet.has(dependency))
        if (blocked) {
          errors.push(`[${rule.id}] ${source} -> ${dependency}: ${rule.description}. ${rule.suggestion}`)
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

function cargoPackages(repoRoot) {
  const output = execFileSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], {
    cwd: repoRoot,
    encoding: 'utf8',
    windowsHide: true,
  })
  const metadata = JSON.parse(output)
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

export function checkRepository(repoRoot = defaultRepoRoot) {
  const policyPath = join(repoRoot, 'engineering', 'architecture-boundaries.json')
  const decisionsDir = join(repoRoot, 'engineering', 'decisions')
  const errors = []
  const policy = readJson(policyPath, errors)
  const adrResult = validateAdrs({ repoRoot, decisionsDir })
  errors.push(...adrResult.errors)
  if (policy) {
    let packages = null
    try {
      packages = cargoPackages(repoRoot)
    } catch (error) {
      errors.push(`cargo metadata failed: ${error.message}`)
    }
    if (packages) {
      errors.push(...validatePolicy({
        policy,
        packages,
        adrRecords: adrResult.records,
        policyPath: display(repoRoot, policyPath),
      }))
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
    console.log('Architecture check passed: workspace boundaries and ADRs are valid.')
  }
}
