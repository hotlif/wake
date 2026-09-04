import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs'
import { dirname, extname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { parseSyml } from '@yarnpkg/parsers'

const scriptPath = fileURLToPath(import.meta.url)
const defaultRepoRoot = resolve(dirname(scriptPath), '..')
const allowedFrontmatterKeys = new Set([
  'name',
  'description',
  'license',
  'allowed-tools',
  'metadata',
])
const architectWakeEvalCoverage = [
  'implicit-positive',
  'implicit-negative',
  'concurrency',
  'publication-transaction',
  'test-first',
  'baseline-only',
  'docs-no-red',
  'read-only',
  'dynamic-consumers',
  'machine-boundary',
  'mixed-scope',
  'local-bug-negative',
  'private-refactor-negative',
  'proposed-not-authority',
  'accepted-no-duplicate-adr',
  'unspecified-accepted-adr',
]

function display(root, path) {
  return relative(root, path).split(sep).join('/')
}

function filesUnder(directory) {
  if (!existsSync(directory)) return []
  const files = []
  for (const name of readdirSync(directory)) {
    const path = join(directory, name)
    const stats = statSync(path)
    if (stats.isDirectory()) files.push(...filesUnder(path))
    else if (stats.isFile()) files.push(path)
  }
  return files
}

function parseYaml(path, source, errors) {
  try {
    const value = parseSyml(source)
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
      errors.push(`${path}: YAML root must be a mapping`)
      return null
    }
    return value
  } catch (error) {
    errors.push(`${path}: invalid YAML (${error.message})`)
    return null
  }
}

function withoutFencedCode(source) {
  const output = []
  let fence = null
  for (const line of source.split(/\r?\n/)) {
    const match = /^[ \t]*(?:(?:[-+*]|\d+[.)])[ \t]+)?(`{3,}|~{3,})(.*)$/.exec(line)
    if (fence === null && match) {
      fence = match[1]
      output.push('')
    } else if (fence !== null) {
      if (match && match[1][0] === fence[0] && match[1].length >= fence.length && match[2].trim() === '') {
        fence = null
      }
      output.push('')
    } else {
      output.push(line)
    }
  }
  return output.join('\n')
}

function topLevelMappingBlocks(source, key) {
  const escaped = key.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const occurrence = new RegExp(`^${escaped}[ \\t]*:`)
  const header = new RegExp(`^${escaped}[ \\t]*:[ \\t]*(?:#.*)?$`)
  const lines = source.split(/\r?\n/)
  const blocks = []
  for (let index = 0; index < lines.length; index += 1) {
    if (!occurrence.test(lines[index])) continue
    if (!header.test(lines[index])) {
      blocks.push(null)
      continue
    }
    const body = []
    for (let bodyIndex = index + 1; bodyIndex < lines.length; bodyIndex += 1) {
      if (/^\S/.test(lines[bodyIndex]) && !/^\s*#/.test(lines[bodyIndex])) break
      body.push(lines[bodyIndex])
    }
    blocks.push(body.join('\n'))
  }
  return blocks
}

function directMappingValues(block, key) {
  const escaped = key.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const indents = block.split(/\r?\n/)
    .filter((line) => line.trim() !== '' && !/^\s*#/.test(line))
    .map((line) => /^ +/.exec(line)?.[0].length ?? 0)
    .filter((indent) => indent > 0)
  if (indents.length === 0) return []
  const directIndent = Math.min(...indents)
  return [...block.matchAll(new RegExp(`^ {${directIndent}}${escaped}[ \\t]*:[ \\t]*(.*?)[ \\t]*$`, 'gm'))]
    .map((match) => match[1])
}

function unsupportedMappingKey(source) {
  let blockScalarIndent = null
  for (const line of source.split(/\r?\n/)) {
    if (blockScalarIndent !== null) {
      if (line.trim() === '') continue
      const indent = /^ */.exec(line)[0].length
      if (indent > blockScalarIndent) continue
      blockScalarIndent = null
    }
    if (/^\s*(?:#.*)?$/.test(line)) continue
    const indent = /^ */.exec(line)[0].length
    let candidate = line.trimStart()
    candidate = candidate.replace(/^(?:[-+*]|\d+[.)])[ \t]+/, '')
    if (/^(?:"(?:[^"\\]|\\.)*"|'(?:[^']|'')*')[ \t]*:/.test(candidate)) return line.trim()
    if (/^\?(?:[ \t]|$)/.test(candidate)) return line.trim()
    if (/^(?:!|&|\*)\S*(?:[ \t]+[^:]*)?[ \t]*:/.test(candidate)) return line.trim()
    if (/^<<[ \t]*:/.test(candidate)) return line.trim()
    if (
      /:[ \t]*[>|](?:[1-9][+-]?|[+-][1-9]?)?[ \t]*(?:#.*)?$/.test(candidate)
      || /^[>|](?:[1-9][+-]?|[+-][1-9]?)?[ \t]*(?:#.*)?$/.test(candidate)
    ) {
      blockScalarIndent = indent
    }
  }
  return null
}

function topLevelScalar(source, key) {
  const escaped = key.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const matches = [...source.matchAll(new RegExp(`^${escaped}[ \\t]*:[ \\t]*(.*?)[ \\t]*$`, 'gm'))]
  return matches.length === 1 ? matches[0][1] : null
}

function yamlStringScalar(value) {
  if (value === null || value === '') return false
  if (/^(?:["']|[>|])/.test(value)) return true
  if (/^(?:~|null|true|false|yes|no|on|off)$/i.test(value)) return false
  if (/^[+-]?(?:\.(?:inf|nan)|0[xob][0-9a-f_]+|(?:\d[\d_]*(?:\.[\d_]*)?|\.[\d_]+)(?:e[+-]?\d+)?)$/i.test(value)) return false
  if (/^[+-]?\d[\d_]*(?::[0-5]?\d)+(?:\.\d*)?$/.test(value)) return false
  if (/^\d{4}-\d{1,2}-\d{1,2}(?:[Tt ]|$)/.test(value)) return false
  if (/^[{[]/.test(value)) return false
  return true
}

function frontmatter(source, path, errors) {
  const match = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(source)
  if (!match) {
    errors.push(`${path}: SKILL.md must start with YAML frontmatter delimited by ---`)
    return null
  }
  const metadata = parseYaml(path, match[1], errors)
  return metadata ? { metadata, source: match[1] } : null
}

function isInside(root, target) {
  const path = relative(root, target)
  return path === '' || (!path.startsWith(`..${sep}`) && path !== '..' && !isAbsolute(path))
}

function markdownTargets(source) {
  const targets = []
  for (const match of source.matchAll(/!?\[[^\]]*\]\((<[^>]+>|[^)\s]+)(?:\s+["'][^"']*["'])?\)/g)) {
    const raw = match[1].replace(/^<|>$/g, '')
    if (!raw || raw.startsWith('#') || /^[a-z][a-z0-9+.-]*:/i.test(raw)) continue
    const withoutFragment = raw.split('#', 1)[0].split('?', 1)[0]
    if (withoutFragment) targets.push(withoutFragment)
  }
  return targets
}

function validateOpenAiYaml({ repoRoot, skillDir, name, errors }) {
  const path = join(skillDir, 'agents', 'openai.yaml')
  const shown = display(repoRoot, path)
  if (!existsSync(path)) {
    errors.push(`${shown}: skill UI metadata is required`)
    return
  }
  const source = readFileSync(path, 'utf8')
  const config = parseYaml(shown, source, errors)
  if (!config) return
  const unsupportedKey = unsupportedMappingKey(source)
  if (unsupportedKey !== null) {
    errors.push(`${shown}: mapping keys must use unquoted plain scalars; found ${unsupportedKey}`)
  }

  const interfaceBlocks = topLevelMappingBlocks(source, 'interface')
  if (interfaceBlocks.length !== 1 || interfaceBlocks[0] === null) {
    errors.push(`${shown}: interface must appear exactly once as a top-level mapping`)
  } else {
    for (const field of ['display_name', 'short_description', 'default_prompt']) {
      const values = directMappingValues(interfaceBlocks[0], field)
      if (values.length !== 1) {
        errors.push(`${shown}: interface.${field} must appear exactly once`)
      } else if (!/^(?:"(?:[^"\\]|\\.)*"|'(?:[^']|'')*')$/.test(values[0])) {
        errors.push(`${shown}: interface.${field} must be a quoted string`)
      }
    }
  }

  const ui = config.interface
  if (!ui || typeof ui !== 'object' || Array.isArray(ui)) {
    errors.push(`${shown}: interface must be a mapping`)
  } else {
    if (typeof ui.display_name !== 'string' || ui.display_name.trim() === '') {
      errors.push(`${shown}: interface.display_name must be a non-empty string`)
    }
    if (typeof ui.short_description !== 'string' || ui.short_description.length < 25 || ui.short_description.length > 64) {
      errors.push(`${shown}: interface.short_description must contain 25-64 characters`)
    }
    if (typeof ui.default_prompt !== 'string' || !ui.default_prompt.includes(`$${name}`)) {
      errors.push(`${shown}: interface.default_prompt must mention $${name}`)
    }
  }

  const policyBlocks = topLevelMappingBlocks(source, 'policy')
  let implicit = null
  if (policyBlocks.length !== 1 || policyBlocks[0] === null) {
    errors.push(`${shown}: policy must appear exactly once as a top-level mapping`)
  } else {
    const values = directMappingValues(policyBlocks[0], 'allow_implicit_invocation')
    if (values.length !== 1) {
      errors.push(`${shown}: policy.allow_implicit_invocation must appear exactly once`)
    } else if (!/^(?:true|false)$/.test(values[0])) {
      errors.push(`${shown}: policy.allow_implicit_invocation must be a boolean`)
    } else {
      implicit = values[0]
    }
  }

  if (!config.policy || typeof config.policy !== 'object' || Array.isArray(config.policy)) {
    errors.push(`${shown}: policy must be a mapping`)
  }
  if (name === 'architect-wake' && implicit !== null && implicit !== 'true') {
    errors.push(`${shown}: architect-wake must enable implicit invocation`)
  }
}

function nonEmptyStringArray(value) {
  return Array.isArray(value)
    && value.length > 0
    && value.every((entry) => typeof entry === 'string' && entry.trim() !== '')
}

function validateBehaviorEvals({ repoRoot, skillDir, name, errors }) {
  if (name !== 'architect-wake') return
  const path = join(skillDir, 'references', 'behavior-evals.json')
  const shown = display(repoRoot, path)
  if (!existsSync(path)) {
    errors.push(`${shown}: architect-wake behavior evaluation corpus is required`)
    return
  }

  let document
  try {
    document = JSON.parse(readFileSync(path, 'utf8'))
  } catch (error) {
    errors.push(`${shown}: invalid JSON (${error.message})`)
    return
  }
  if (!document || typeof document !== 'object' || Array.isArray(document)) {
    errors.push(`${shown}: root must be an object`)
    return
  }
  if (document.version !== 1) errors.push(`${shown}: version must be 1`)

  const protocol = document.protocol
  if (!protocol || typeof protocol !== 'object' || Array.isArray(protocol)) {
    errors.push(`${shown}: protocol must be an object`)
  } else {
    if (protocol.isolation !== 'fresh-agent-per-case') {
      errors.push(`${shown}: protocol.isolation must be fresh-agent-per-case`)
    }
    if (protocol.withholdExpectedUntilAfterRun !== true) {
      errors.push(`${shown}: protocol.withholdExpectedUntilAfterRun must be true`)
    }
    if (protocol.selectionEvidence !== 'host-event-or-unsupported') {
      errors.push(`${shown}: protocol.selectionEvidence must be host-event-or-unsupported`)
    }
    if (protocol.workspaceIsolation !== 'enforced-read-only-or-disposable') {
      errors.push(`${shown}: protocol.workspaceIsolation must be enforced-read-only-or-disposable`)
    }
    if (protocol.grading !== 'observable-actions-and-order') {
      errors.push(`${shown}: protocol.grading must be observable-actions-and-order`)
    }
    if (protocol.ordinaryCi !== 'schema-only') {
      errors.push(`${shown}: protocol.ordinaryCi must be schema-only`)
    }
  }

  const cases = document.cases
  if (!Array.isArray(cases) || cases.length < 5) {
    errors.push(`${shown}: cases must contain at least five scenarios`)
    return
  }

  const ids = new Set()
  const coverage = new Set()
  const enums = {
    invocation: new Set(['implicit', 'explicit']),
    selection: new Set(['selected', 'not-selected']),
    route: new Set(['ordinary', 'architecture', 'adr-review', 'none']),
    preImplementation: new Set(['red', 'baseline', 'none']),
    evidenceScope: new Set(['focused', 'architecture', 'dynamic-contract', 'direct-adr', 'none']),
    mutation: new Set(['allowed', 'forbidden']),
  }

  for (const [index, entry] of cases.entries()) {
    const prefix = `${shown}: cases[${index}]`
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      errors.push(`${prefix} must be an object`)
      continue
    }
    if (typeof entry.id !== 'string' || !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(entry.id)) {
      errors.push(`${prefix}.id must use lowercase hyphen-case`)
    } else if (ids.has(entry.id)) {
      errors.push(`${prefix}: case id must be unique (${entry.id})`)
    } else {
      ids.add(entry.id)
    }
    if (typeof entry.prompt !== 'string' || entry.prompt.trim() === '') {
      errors.push(`${prefix}.prompt must be a non-empty string`)
    }
    if (!enums.invocation.has(entry.invocation)) {
      errors.push(`${prefix}.invocation must be implicit or explicit`)
    }
    if (!nonEmptyStringArray(entry.coverage)) {
      errors.push(`${prefix}.coverage must be a non-empty array of strings`)
    } else {
      for (const tag of entry.coverage) {
        if (!architectWakeEvalCoverage.includes(tag)) errors.push(`${prefix}: unknown coverage ${tag}`)
        coverage.add(tag)
      }
    }
    if (!nonEmptyStringArray(entry.assertions)) {
      errors.push(`${prefix}.assertions must be a non-empty array of strings`)
    }
    if (!nonEmptyStringArray(entry.forbidden)) {
      errors.push(`${prefix}.forbidden must be a non-empty array of strings`)
    }

    const expected = entry.expected
    if (!expected || typeof expected !== 'object' || Array.isArray(expected)) {
      errors.push(`${prefix}.expected must be an object`)
      continue
    }
    for (const key of ['selection', 'route', 'preImplementation', 'evidenceScope', 'mutation']) {
      if (!enums[key].has(expected[key])) {
        errors.push(`${prefix}.expected.${key} is invalid`)
      }
    }
    if (typeof expected.architectureGates !== 'boolean') {
      errors.push(`${prefix}.expected.architectureGates must be a boolean`)
    }

    const tags = new Set(Array.isArray(entry.coverage) ? entry.coverage : [])
    if (entry.invocation === 'explicit' && expected.selection !== 'selected') {
      errors.push(`${prefix}: explicit cases must select the skill`)
    }
    if (expected.selection === 'not-selected'
      && (expected.route !== 'none' || expected.preImplementation !== 'none')) {
      errors.push(`${prefix}: not-selected cases must have no skill route or implementation precondition`)
    }
    if (expected.route !== 'none' && expected.selection !== 'selected') {
      errors.push(`${prefix}: routed cases must select the skill`)
    }
    if (tags.has('implicit-positive')
      && (entry.invocation !== 'implicit' || expected.selection !== 'selected')) {
      errors.push(`${prefix}: implicit-positive cases must implicitly select the skill`)
    }
    if (tags.has('implicit-negative')
      && (entry.invocation !== 'implicit' || expected.selection !== 'not-selected')) {
      errors.push(`${prefix}: implicit-negative cases must not select the skill`)
    }
    if (tags.has('test-first') && expected.preImplementation !== 'red') {
      errors.push(`${prefix}: test-first cases must establish an expected Red before production implementation`)
    }
    if (tags.has('baseline-only') && expected.preImplementation !== 'baseline') {
      errors.push(`${prefix}: baseline-only cases must establish a baseline first`)
    }
    if (tags.has('docs-no-red') && expected.preImplementation !== 'none') {
      errors.push(`${prefix}: docs-no-red cases must not manufacture a Red`)
    }
    if (tags.has('read-only')
      && (expected.mutation !== 'forbidden' || expected.preImplementation !== 'none')) {
      errors.push(`${prefix}: read-only cases must forbid mutation and have no implementation precondition`)
    }
    if (tags.has('dynamic-consumers') && expected.evidenceScope !== 'dynamic-contract') {
      errors.push(`${prefix}: dynamic-consumers cases must expand to dynamic-contract evidence`)
    }
    if (tags.has('machine-boundary')
      && (expected.route !== 'architecture' || expected.architectureGates !== true)) {
      errors.push(`${prefix}: machine-boundary cases must use architecture routing and gates`)
    }
    if ((tags.has('concurrency') || tags.has('publication-transaction'))
      && (expected.selection !== 'selected' || expected.route !== 'architecture')) {
      errors.push(`${prefix}: concurrency and publication-transaction cases must use architecture routing`)
    }
    if ((tags.has('local-bug-negative') || tags.has('private-refactor-negative'))
      && (entry.invocation !== 'implicit' || expected.selection !== 'not-selected' || expected.route !== 'none')) {
      errors.push(`${prefix}: local bug and private refactor negative cases must remain outside implicit architecture routing`)
    }
    if (tags.has('proposed-not-authority')
      && (expected.route !== 'adr-review' || expected.mutation !== 'forbidden'
        || expected.preImplementation !== 'none')) {
      errors.push(`${prefix}: proposed-not-authority cases must be read-only ADR reviews`)
    }
    if (tags.has('accepted-no-duplicate-adr')
      && (expected.selection !== 'selected' || expected.route !== 'architecture')) {
      errors.push(`${prefix}: accepted ADR cases must use architecture routing without creating a duplicate decision`)
    }
    if (tags.has('unspecified-accepted-adr')
      && (expected.preImplementation !== 'none' || expected.mutation !== 'forbidden')) {
      errors.push(`${prefix}: unspecified accepted ADR cases must not mutate before the target is identified`)
    }
    if (tags.has('mixed-scope')
      && (expected.route !== 'architecture' || expected.preImplementation !== 'red'
        || expected.architectureGates !== true)) {
      errors.push(`${prefix}: mixed-scope cases must establish Red and use architecture routing and gates`)
    }
  }

  for (const tag of architectWakeEvalCoverage) {
    if (!coverage.has(tag)) errors.push(`${shown}: missing required coverage ${tag}`)
  }
}

function validateLinks({ repoRoot, skillDir, entryPath, errors }) {
  const reachable = new Set()
  const visitedMarkdown = new Set()
  const pending = [entryPath]
  reachable.add(resolve(entryPath))
  while (pending.length > 0) {
    const sourcePath = pending.pop()
    const identity = resolve(sourcePath)
    if (visitedMarkdown.has(identity)) continue
    visitedMarkdown.add(identity)
    if (extname(sourcePath).toLowerCase() !== '.md') continue

    const source = withoutFencedCode(readFileSync(sourcePath, 'utf8'))
    for (const link of markdownTargets(source)) {
      const target = resolve(dirname(sourcePath), link)
      if (!isInside(skillDir, target)) {
        errors.push(`${display(repoRoot, sourcePath)}: relative link escapes the skill directory: ${link}`)
        continue
      }
      if (!existsSync(target)) {
        errors.push(`${display(repoRoot, sourcePath)}: relative link does not exist: ${link}`)
        continue
      }
      reachable.add(resolve(target))
      if (extname(target).toLowerCase() === '.md') pending.push(target)
    }
  }

  const referencesDir = join(skillDir, 'references')
  for (const path of filesUnder(referencesDir)) {
    if (!reachable.has(resolve(path))) {
      errors.push(`${display(repoRoot, path)}: reference is not reachable from SKILL.md`)
    }
  }
}

export function validateSkill({ repoRoot, skillDir }) {
  const errors = []
  const entryPath = join(skillDir, 'SKILL.md')
  const shownEntry = display(repoRoot, entryPath)
  if (!existsSync(entryPath)) return [`${shownEntry}: required skill entrypoint is missing`]

  const source = readFileSync(entryPath, 'utf8')
  const parsedFrontmatter = frontmatter(source, shownEntry, errors)
  const folderName = skillDir.split(/[\\/]/).at(-1)
  let name = folderName
  if (parsedFrontmatter) {
    const { metadata, source: frontmatterSource } = parsedFrontmatter
    const unsupportedKey = unsupportedMappingKey(frontmatterSource)
    if (unsupportedKey !== null) {
      errors.push(`${shownEntry}: mapping keys must use unquoted plain scalars; found ${unsupportedKey}`)
    }
    for (const key of Object.keys(metadata)) {
      if (!allowedFrontmatterKeys.has(key)) errors.push(`${shownEntry}: unsupported frontmatter key ${key}`)
    }
    name = metadata.name
    if (typeof name !== 'string' || !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(name) || name.length > 64) {
      errors.push(`${shownEntry}: name must be lowercase hyphen-case with at most 64 characters`)
      name = folderName
    } else if (name !== folderName) {
      errors.push(`${shownEntry}: name ${name} must match directory ${folderName}`)
      name = folderName
    }
    const description = metadata.description
    if (typeof description !== 'string' || description.trim() === '' || description.length > 1024) {
      errors.push(`${shownEntry}: description must be a non-empty string with at most 1024 characters`)
    } else if (/[<>]/.test(description)) {
      errors.push(`${shownEntry}: description must not contain angle brackets`)
    }
    for (const key of ['name', 'description']) {
      if (!yamlStringScalar(topLevelScalar(frontmatterSource, key))) {
        errors.push(`${shownEntry}: ${key} must be a YAML string`)
      }
    }
  }
  if (/\[TODO:[^\]]*\]/i.test(withoutFencedCode(source))) errors.push(`${shownEntry}: unfinished TODO placeholder`)

  validateOpenAiYaml({ repoRoot, skillDir, name, errors })
  validateBehaviorEvals({ repoRoot, skillDir, name, errors })
  validateLinks({ repoRoot, skillDir, entryPath, errors })
  return errors
}

export function validateSkills({ repoRoot, skillsDir = join(repoRoot, '.agents', 'skills') }) {
  if (!existsSync(skillsDir)) return [`${display(repoRoot, skillsDir)}: skills directory is missing`]
  const skillDirs = readdirSync(skillsDir)
    .map((name) => join(skillsDir, name))
    .filter((path) => statSync(path).isDirectory())
    .sort()
  if (skillDirs.length === 0) return [`${display(repoRoot, skillsDir)}: no repository skills found`]
  return skillDirs.flatMap((skillDir) => validateSkill({ repoRoot, skillDir }))
}

const invokedPath = process.argv[1] ? pathToFileURL(resolve(process.argv[1])).href : null
if (import.meta.url === invokedPath) {
  const errors = validateSkills({ repoRoot: defaultRepoRoot })
  if (errors.length) {
    console.error(`Skill check failed with ${errors.length} error(s):`)
    for (const error of errors) console.error(`- ${error}`)
    process.exitCode = 1
  } else {
    console.log('Skill check passed: repository skills, UI metadata, and local references are valid.')
  }
}
