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

function normalizedSlug(value) {
  const withLeadingSlash = value.startsWith('/') ? value : `/${value}`
  return withLeadingSlash === '/' ? '/' : withLeadingSlash.replace(/\/+$/, '')
}

const mdxFiles = walk(docsRoot, (path) => extname(path) === '.mdx')
const routes = new Map()
const pageSlugs = new Map()
const requiredFrontmatter = ['title', 'description', 'group', 'group_order', 'order', 'slug', 'status']

for (const file of mdxFiles) {
  const source = readFileSync(file, 'utf8')
  const frontmatter = source.match(/^\+\+\+\r?\n([\s\S]*?)\r?\n\+\+\+(?:\r?\n|$)/)
  if (!frontmatter) {
    errors.push(`${display(file)}:1 missing TOML frontmatter`)
    continue
  }

  const fields = new Map()
  for (const match of frontmatter[1].matchAll(/^([A-Za-z_][\w-]*)\s*=\s*(.+)$/gm)) {
    fields.set(match[1], match[2].trim())
  }
  for (const field of requiredFrontmatter) {
    if (!fields.has(field)) errors.push(`${display(file)}:1 missing frontmatter field ${field}`)
  }

  const rawSlug = fields.get('slug')
  const slugMatch = rawSlug?.match(/^"([^"]+)"$/)
  if (!slugMatch) {
    errors.push(`${display(file)}:1 slug must be a quoted string`)
    continue
  }

  const slug = normalizedSlug(slugMatch[1])
  const previous = routes.get(slug)
  if (previous) errors.push(`${display(file)}:1 duplicate slug ${slug} (also ${display(previous)})`)
  else routes.set(slug, file)
  pageSlugs.set(file, slug)
}

const requiredEngineeringFiles = [
  'README.md',
  'ARCHITECTURE.md',
  'DESIGN.md',
  'PLAN.md',
  'COMPATIBILITY.md',
  'TESTING.md',
  'PERFORMANCE.md',
  'AUDIT.md',
  'ROADMAP.md',
]
for (const name of requiredEngineeringFiles) {
  const path = join(repoRoot, 'engineering', name)
  if (!existsSync(path)) errors.push(`engineering/${name}: required engineering document is missing`)
}

const markdownFiles = [
  join(repoRoot, 'README.md'),
  join(repoRoot, 'fixtures', 'README.md'),
  join(repoRoot, 'npm', 'wake', 'README.md'),
  ...walk(docsRoot, (path) => ['.md', '.mdx'].includes(extname(path))),
  ...walk(join(repoRoot, 'engineering'), (path) => extname(path) === '.md'),
]

function validateFileTarget(file, target, line) {
  let decoded
  try {
    decoded = decodeURIComponent(target)
  } catch {
    errors.push(`${display(file)}:${line} invalid URL encoding in ${target}`)
    return
  }
  const path = resolve(dirname(file), decoded)
  if (!path.startsWith(`${repoRoot}${sep}`) && path !== repoRoot) {
    errors.push(`${display(file)}:${line} link escapes repository: ${target}`)
    return
  }
  if (!existsSync(path)) errors.push(`${display(file)}:${line} missing file target: ${target}`)
}

function validateRouteTarget(file, pageSlug, target, isImage, line) {
  let pathname
  try {
    pathname = decodeURIComponent(new URL(target, `https://wake.local${pageSlug}`).pathname)
  } catch {
    errors.push(`${display(file)}:${line} invalid route: ${target}`)
    return
  }
  const route = normalizedSlug(pathname)
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
  const links = /(!?)\[[^\]]*\]\(([^)]+)\)/g
  for (const match of source.matchAll(links)) {
    const isImage = match[1] === '!'
    const raw = match[2].trim().replace(/^<|>$/g, '')
    const target = raw.match(/^(\S+)/)?.[1]
    if (!target || target.startsWith('#')) continue
    if (/^[a-z][a-z\d+.-]*:/i.test(target) || target.startsWith('//')) continue

    const line = lineAt(source, match.index)
    const clean = target.split(/[?#]/, 1)[0]
    if (!clean) continue

    const extension = extname(clean).toLowerCase()
    if (pageSlug && (clean.startsWith('/') || !extension)) {
      validateRouteTarget(file, pageSlug, clean, isImage, line)
    } else {
      validateFileTarget(file, clean, line)
    }
  }
}

if (errors.length) {
  console.error(`Documentation check failed with ${errors.length} error(s):`)
  for (const error of errors) console.error(`- ${error}`)
  process.exitCode = 1
} else {
  console.log(`Documentation check passed: ${mdxFiles.length} routes, ${markdownFiles.length} Markdown files.`)
}
