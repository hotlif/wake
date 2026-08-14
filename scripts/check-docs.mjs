import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, extname, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const docsRoot = join(repoRoot, 'docs')
const errors = []

function walk(directory, predicate) {
  const files = []
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) files.push(...walk(path, predicate))
    else if (predicate(path)) files.push(path)
  }
  return files
}

function display(path) {
  return relative(repoRoot, path).split(sep).join('/')
}

function withoutFencedCode(source) {
  const lines = source.split(/(?<=\n)/)
  let fence = null
  return lines.map((line) => {
    const marker = line.match(/^\s*(`{3,}|~{3,})/)
    if (marker && fence === null) {
      fence = marker[1][0]
      return line.replace(/[^\r\n]/g, ' ')
    }
    if (marker && marker[1][0] === fence) {
      fence = null
      return line.replace(/[^\r\n]/g, ' ')
    }
    return fence === null ? line : line.replace(/[^\r\n]/g, ' ')
  }).join('')
}

function lineAt(source, offset) {
  return source.slice(0, offset).split('\n').length
}

function pageId(file) {
  return relative(docsRoot, file).split(sep).join('/').replace(/\.mdx$/, '')
}

function routeFor(id) {
  const route = id === 'index' ? '' : id.replace(/\/index$/, '')
  return `/${route}`
}

function parseQuoted(value) {
  const match = value?.match(/^"([\s\S]*)"$/)
  return match?.[1]
}

function headings(source, level) {
  const body = withoutFencedCode(source)
  const marker = '#'.repeat(level)
  return [...body.matchAll(new RegExp(`^${marker}\\s+(.+)$`, 'gm'))].map((match) => match[1].trim())
}

function hasHeading(source, pattern) {
  return headings(source, 2).some((heading) => pattern.test(heading))
}

function validatePageContract(file, source, kind) {
  const h1 = headings(source, 1)
  if (h1.length !== 1) errors.push(`${display(file)}: page requires exactly one H1 heading`)

  const h2 = headings(source, 2)
  if (kind !== 'overview' && h2.length === 0) {
    errors.push(`${display(file)}: ${kind ?? 'page'} requires at least one H2 section`)
  }

  if (kind === 'overview') {
    if (!/<(?:PageActions|HomeLead|OverviewLead)\b/.test(source)) errors.push(`${display(file)}: overview requires a primary task entry`)
    if (h2.length < 2) errors.push(`${display(file)}: overview requires at least two task-oriented sections`)
    return
  }
  if (kind === 'tutorial') {
    if (!/<ResultPanel\b/.test(source) && !hasHeading(source, /完成.*后|完成本页后/)) {
      errors.push(`${display(file)}: tutorial requires an explicit completion result`)
    }
    if (!/^(?:```|~~~)/m.test(source)) errors.push(`${display(file)}: tutorial requires complete runnable code`)
    if (!hasHeading(source, /验证|验收/)) errors.push(`${display(file)}: tutorial requires a verification section`)
    if (!hasHeading(source, /常见错误/)) errors.push(`${display(file)}: tutorial requires a common-errors section`)
  }
  if (kind === 'guide' && !hasHeading(source, /验证|测量|检查/)) {
    errors.push(`${display(file)}: guide requires a verification or measurement section`)
  }
  if ((kind === 'tutorial' || kind === 'guide') && !hasHeading(source, /^下一步$/)) {
    errors.push(`${display(file)}: ${kind} requires a 下一步 section`)
  }
}

function publicRustFields(source, structName) {
  const match = source.match(new RegExp(`pub struct ${structName}\\s*\\{([\\s\\S]*?)\\n\\}`))
  if (!match) {
    errors.push(`crates/wake_config/src/lib.rs: public config struct ${structName} was not found`)
    return []
  }
  return [...match[1].matchAll(/^\s*pub\s+([a-z][a-z0-9_]*):/gm)].map((field) => field[1])
}

function validateConfigReference() {
  const configSource = readFileSync(join(repoRoot, 'crates', 'wake_config', 'src', 'lib.rs'), 'utf8')
  const coverage = [
    { page: 'project.mdx', structs: ['Config', 'BrowserslistOptions', 'TransformControl', 'TypeScript', 'React', 'ComponentScan', 'Html', 'Hooks'] },
    { page: 'dev-server.mdx', structs: ['DevServer', 'Proxy'] },
    { page: 'docs.mdx', structs: ['Docs'] },
  ]
  for (const item of coverage) {
    const path = join(docsRoot, 'reference', 'configuration', item.page)
    const source = readFileSync(path, 'utf8')
    for (const structName of item.structs) {
      for (const field of publicRustFields(configSource, structName)) {
        if (!source.includes(`\`${field}\``)) {
          errors.push(`${display(path)}: missing public ${structName} field \`${field}\` from wake_config`)
        }
      }
    }
  }
}

function parseNavigation(path) {
  if (!existsSync(path)) {
    errors.push(`${display(path)}: required navigation manifest is missing`)
    return []
  }
  const source = readFileSync(path, 'utf8')
  const entries = []
  let current = null
  for (const [index, line] of source.split(/\r?\n/).entries()) {
    const trimmed = line.trim()
    if (!trimmed || trimmed.startsWith('#')) continue
    if (trimmed === '[[group]]' || trimmed === '[[group.section]]') {
      current = { kind: trimmed === '[[group]]' ? 'group' : 'section', line: index + 1 }
      entries.push(current)
      continue
    }
    const assignment = trimmed.match(/^([a-z_]+)\s*=\s*(.+)$/)
    if (!current || !assignment) {
      errors.push(`${display(path)}:${index + 1} unsupported navigation syntax`)
      continue
    }
    const [, key, raw] = assignment
    if (key === 'pages') {
      const values = [...raw.matchAll(/"([^"]+)"/g)].map((match) => match[1])
      if (!/^\[.*\]$/.test(raw) || values.length === 0) {
        errors.push(`${display(path)}:${index + 1} pages must be a non-empty string array`)
      }
      current.pages = values
    } else if (key === 'id' || key === 'title') {
      current[key] = parseQuoted(raw)
      if (current[key] === undefined) errors.push(`${display(path)}:${index + 1} ${key} must be a quoted string`)
    } else {
      errors.push(`${display(path)}:${index + 1} unknown navigation field ${key}`)
    }
  }

  const placements = []
  const groupIds = new Set()
  let group = null
  let sectionIds = new Set()
  for (const entry of entries) {
    if (!entry.id || !entry.title) {
      errors.push(`${display(path)}:${entry.line} navigation entry requires id and title`)
      continue
    }
    if (!/^[a-z0-9-]+$/.test(entry.id)) errors.push(`${display(path)}:${entry.line} invalid id ${entry.id}`)
    if (entry.kind === 'group') {
      if (groupIds.has(entry.id)) errors.push(`${display(path)}:${entry.line} duplicate group id ${entry.id}`)
      groupIds.add(entry.id)
      group = entry
      sectionIds = new Set()
    } else if (!group) {
      errors.push(`${display(path)}:${entry.line} section appears before a group`)
      continue
    } else {
      if (sectionIds.has(entry.id)) errors.push(`${display(path)}:${entry.line} duplicate section id ${group.id}/${entry.id}`)
      sectionIds.add(entry.id)
    }
    for (const page of entry.pages ?? []) placements.push({ page, line: entry.line })
  }
  return placements
}

const allowedFrontmatter = new Set([
  'title',
  'description',
  'kind',
  'status',
  'draft',
  'hidden',
])

function fixtureFrontmatter(file) {
  const source = readFileSync(file, 'utf8')
  const frontmatter = source.match(/^\+\+\+\r?\n([\s\S]*?)\r?\n\+\+\+(?:\r?\n|$)/)
  if (!frontmatter) {
    errors.push(`${display(file)}:1 missing TOML frontmatter`)
    return new Map()
  }
  const fields = new Map()
  for (const match of frontmatter[1].matchAll(/^([A-Za-z_][\w-]*)\s*=\s*(.+)$/gm)) {
    fields.set(match[1], match[2].trim())
  }
  for (const field of fields.keys()) {
    if (!allowedFrontmatter.has(field)) {
      errors.push(`${display(file)}:1 unsupported frontmatter field ${field}`)
    }
  }
  for (const field of ['title', 'description', 'kind', 'status']) {
    if (!fields.has(field)) errors.push(`${display(file)}:1 missing frontmatter field ${field}`)
  }
  const kind = parseQuoted(fields.get('kind'))
  const status = parseQuoted(fields.get('status'))
  if (kind && !validKinds.has(kind)) errors.push(`${display(file)}:1 unknown kind ${kind}`)
  if (status && !validStatuses.has(status)) errors.push(`${display(file)}:1 unknown status ${status}`)
  if (!/^"[^"\r\n]+"$/.test(fields.get('title') ?? '')) {
    errors.push(`${display(file)}:1 title must be a non-empty quoted string`)
  }
  if (!/^"[^"\r\n]+"$/.test(fields.get('description') ?? '')) {
    errors.push(`${display(file)}:1 description must be a non-empty quoted string`)
  }
  return fields
}

function validateFixtureDocs() {
  const fixturesRoot = join(repoRoot, 'fixtures')
  const fixtureDocsRoots = readdirSync(fixturesRoot, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && existsSync(join(fixturesRoot, entry.name, 'docs')))
    .map((entry) => join(fixturesRoot, entry.name, 'docs'))

  const fieldsByFile = new Map()
  let fixturePageCount = 0
  for (const fixtureDocsRoot of fixtureDocsRoots) {
    for (const file of walk(fixtureDocsRoot, (path) => extname(path) === '.mdx')) {
      fieldsByFile.set(file, fixtureFrontmatter(file))
      fixturePageCount += 1
    }
  }

  const siteDocsRoot = join(fixturesRoot, 'react-docs', 'docs')
  const sitePages = new Map(
    walk(siteDocsRoot, (path) => extname(path) === '.mdx')
      .map((file) => [
        relative(siteDocsRoot, file).split(sep).join('/').replace(/\.mdx$/, ''),
        fieldsByFile.get(file),
      ]),
  )
  const placements = parseNavigation(join(siteDocsRoot, 'navigation.toml'))
  const listed = new Set()
  for (const { page, line } of placements) {
    const id = page.replace(/^\/+|\/+$/g, '').replace(/\.mdx$/, '')
    if (listed.has(id)) {
      errors.push(`${display(join(siteDocsRoot, 'navigation.toml'))}:${line} duplicate page ${id}`)
    }
    listed.add(id)
    if (!sitePages.has(id)) {
      errors.push(`${display(join(siteDocsRoot, 'navigation.toml'))}:${line} missing page ${id}`)
    }
  }
  for (const [id, fields] of sitePages) {
    if (fields?.get('hidden') !== 'true' && !listed.has(id)) {
      errors.push(`${display(join(siteDocsRoot, `${id}.mdx`))}: page is not listed in navigation.toml`)
    }
  }
  return fixturePageCount
}

const mdxFiles = walk(docsRoot, (path) => extname(path) === '.mdx')
const routes = new Map()
const pageSlugs = new Map()
const pages = new Map()
const hiddenPages = new Set()
const requiredFrontmatter = ['title', 'description', 'kind', 'status']
const retiredFrontmatter = ['group', 'group_order', 'order', 'slug', 'package']
const validKinds = new Set(['overview', 'tutorial', 'guide', 'reference', 'component'])
const validStatuses = new Set(['stable', 'beta', 'experimental', 'deprecated'])

for (const file of mdxFiles) {
  const source = readFileSync(file, 'utf8')
  const frontmatter = source.match(/^\+\+\+\r?\n([\s\S]*?)\r?\n\+\+\+(?:\r?\n|$)/)
  if (!frontmatter) {
    errors.push(`${display(file)}:1 missing TOML frontmatter`)
    continue
  }
  const fields = new Map()
  for (const match of frontmatter[1].matchAll(/^([A-Za-z_][\w-]*)\s*=\s*(.+)$/gm)) fields.set(match[1], match[2].trim())
  for (const field of requiredFrontmatter) if (!fields.has(field)) errors.push(`${display(file)}:1 missing frontmatter field ${field}`)
  for (const field of retiredFrontmatter) if (fields.has(field)) errors.push(`${display(file)}:1 retired frontmatter field ${field} is not allowed`)

  const kind = parseQuoted(fields.get('kind'))
  const status = parseQuoted(fields.get('status'))
  if (kind && !validKinds.has(kind)) errors.push(`${display(file)}:1 unknown kind ${kind}`)
  if (status && !validStatuses.has(status)) errors.push(`${display(file)}:1 unknown status ${status}`)
  if (!/^"[^"\r\n]+"$/.test(fields.get('title') ?? '')) errors.push(`${display(file)}:1 title must be a non-empty quoted string`)
  if (!/^"[^"\r\n]+"$/.test(fields.get('description') ?? '')) errors.push(`${display(file)}:1 description must be a non-empty quoted string`)

  const id = pageId(file)
  const slug = routeFor(id)
  if (routes.has(slug)) errors.push(`${display(file)}:1 duplicate route ${slug}`)
  else routes.set(slug, file)
  pages.set(id, file)
  pageSlugs.set(file, slug)
  if (fields.get('hidden') === 'true') hiddenPages.add(id)

  const body = source.slice(frontmatter[0].length)
  validatePageContract(file, body, kind)
}

validateConfigReference()
const fixturePageCount = validateFixtureDocs()

const placements = parseNavigation(join(docsRoot, 'navigation.toml'))
const listed = new Map()
for (const { page, line } of placements) {
  const id = page.replace(/^\/+|\/+$/g, '').replace(/\.mdx$/, '')
  if (listed.has(id)) errors.push(`docs/navigation.toml:${line} duplicate page ${id}`)
  else listed.set(id, line)
  if (!pages.has(id)) errors.push(`docs/navigation.toml:${line} missing page ${id}`)
}
for (const [id, file] of pages) {
  if (!hiddenPages.has(id) && !listed.has(id)) errors.push(`${display(file)}: page is not listed in navigation.toml`)
}

const requiredEngineeringFiles = ['README.md', 'ARCHITECTURE.md', 'DESIGN.md', 'PLAN.md', 'COMPATIBILITY.md', 'TESTING.md', 'PERFORMANCE.md', 'AUDIT.md', 'ROADMAP.md']
for (const name of requiredEngineeringFiles) {
  const path = join(repoRoot, 'engineering', name)
  if (!existsSync(path)) errors.push(`engineering/${name}: required engineering document is missing`)
}

const markdownFiles = [join(repoRoot, 'README.md'), join(repoRoot, 'fixtures', 'README.md'), join(repoRoot, 'npm', 'wake', 'README.md'), ...walk(docsRoot, (path) => ['.md', '.mdx'].includes(extname(path))), ...walk(join(repoRoot, 'engineering'), (path) => extname(path) === '.md')]

function validateFileTarget(file, target, line) {
  let decoded
  try { decoded = decodeURIComponent(target) } catch { errors.push(`${display(file)}:${line} invalid URL encoding in ${target}`); return }
  const path = resolve(dirname(file), decoded)
  if (!path.startsWith(`${repoRoot}${sep}`) && path !== repoRoot) { errors.push(`${display(file)}:${line} link escapes repository: ${target}`); return }
  if (!existsSync(path)) errors.push(`${display(file)}:${line} missing file target: ${target}`)
}

function validateRouteTarget(file, pageSlug, target, isImage, line) {
  let pathname
  try { pathname = decodeURIComponent(new URL(target, `https://wake.local${pageSlug}`).pathname) } catch { errors.push(`${display(file)}:${line} invalid route: ${target}`); return }
  const route = pathname === '/' ? '/' : pathname.replace(/\/+$/, '')
  if (routes.has(route)) return
  if (isImage) {
    const asset = pathname.replace(/^\/+/, '')
    if (existsSync(join(repoRoot, 'public', asset)) || existsSync(join(docsRoot, 'public', asset))) return
  }
  errors.push(`${display(file)}:${line} unknown documentation route: ${target}`)
}

for (const file of markdownFiles) {
  const source = withoutFencedCode(readFileSync(file, 'utf8'))
  const pageSlug = pageSlugs.get(file)
  for (const match of source.matchAll(/(!?)\[[^\]]*\]\(([^)]+)\)/g)) {
    const isImage = match[1] === '!'
    const raw = match[2].trim().replace(/^<|>$/g, '')
    const target = raw.match(/^(\S+)/)?.[1]
    if (!target || target.startsWith('#') || /^[a-z][a-z\d+.-]*:/i.test(target) || target.startsWith('//')) continue
    const line = lineAt(source, match.index)
    const clean = target.split(/[?#]/, 1)[0]
    if (!clean) continue
    const extension = extname(clean).toLowerCase()
    if (pageSlug && (clean.startsWith('/') || !extension)) validateRouteTarget(file, pageSlug, clean, isImage, line)
    else validateFileTarget(file, clean, line)
  }
}

if (errors.length) {
  console.error(`Documentation check failed with ${errors.length} error(s):`)
  for (const error of errors) console.error(`- ${error}`)
  process.exitCode = 1
} else {
  console.log(`Documentation check passed: ${mdxFiles.length} routes, ${markdownFiles.length} Markdown files, ${fixturePageCount} fixture pages.`)
}
