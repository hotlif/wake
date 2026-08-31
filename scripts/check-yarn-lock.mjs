import { existsSync, readFileSync, readdirSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { parseSyml } from '@yarnpkg/parsers'

const root = resolve(import.meta.dirname, '..')
const manifest = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'))
const lock = parseSyml(readFileSync(join(root, 'yarn.lock'), 'utf8'))
const yarnrc = parseSyml(readFileSync(join(root, '.yarnrc.yml'), 'utf8'))

if (manifest.packageManager !== 'yarn@4.16.0') {
  throw new Error(`packageManager must be yarn@4.16.0; found ${manifest.packageManager}`)
}
if (manifest.devDependencies?.corepack !== '0.34.6') {
  throw new Error('corepack must be exactly pinned to 0.34.6')
}
for (const [key, expected] of Object.entries({
  nodeLinker: 'pnp',
  pnpMode: 'strict',
  pnpFallbackMode: 'none',
  pnpEnableInlining: 'false',
  enableGlobalCache: 'false',
  compressionLevel: '0',
  enableScripts: 'false',
})) {
  if (yarnrc[key] !== expected) {
    throw new Error(`.yarnrc.yml ${key} must equal ${JSON.stringify(expected)}`)
  }
}
const componentExtensions = [
  'alert', 'button', 'card', 'checkbox', 'dialog', 'drawer', 'dropdown-container', 'empty',
  'hooks', 'line-edit', 'number-edit', 'prose', 'segmented', 'select', 'skeleton', 'spin',
  'switch', 'tag', 'text-edit', 'token-global', 'token-semantic', 'tooltip', 'tree', 'virtual',
]
for (const suffix of componentExtensions) {
  const selector = `@crab-dev/rc-${suffix}@*`
  const dependencies = yarnrc.packageExtensions?.[selector]?.dependencies
  for (const [name, version] of Object.entries({
    '@crab-dev/css': '0.1.25',
    '@types/react': '^19.2.18',
    'lucide-react': '^1.23.0',
    react: '19.2.8',
    'react-dom': '19.2.8',
  })) {
    if (dependencies?.[name] !== version) {
      throw new Error(`.yarnrc.yml ${selector} must extend ${name}@${version}`)
    }
  }
}
if (yarnrc.packageExtensions?.['lucide-react@*']?.dependencies?.['@types/react'] !== '^19.2.18') {
  throw new Error('.yarnrc.yml must extend lucide-react with @types/react')
}
if (lock.__metadata?.version !== '10' || lock.__metadata?.cacheKey !== '10c0') {
  throw new Error('yarn.lock must use Yarn 4 lock format v10 and cache key 10c0')
}
for (const path of ['package-lock.json', 'editors/vscode-css/package-lock.json']) {
  if (existsSync(join(root, path))) throw new Error(`${path} must be removed; yarn.lock is authoritative`)
}

const workspacePaths = [
  ...readdirSync(join(root, 'npm'), { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && existsSync(join(root, 'npm', entry.name, 'package.json')))
    .map((entry) => `npm/${entry.name}`),
  'editors/vscode-css',
]
const workspaces = new Map(workspacePaths.map((path) => {
  const workspace = JSON.parse(readFileSync(join(root, path, 'package.json'), 'utf8'))
  return [workspace.name, { path, manifest: workspace }]
}))

for (const [name, workspace] of workspaces) {
  const locked = Object.values(lock).find((entry) => (
    entry?.resolution === `${name}@workspace:${workspace.path}`
  ))
  if (!locked || locked.linkType !== 'soft') {
    throw new Error(`yarn.lock is missing ${name}@workspace:${workspace.path}`)
  }
}

for (const [descriptor, entry] of Object.entries(lock)) {
  if (descriptor === '__metadata') continue
  if (!entry || typeof entry !== 'object') throw new Error(`invalid yarn.lock entry ${descriptor}`)
  if (entry.resolution?.includes('@workspace:')) continue
  if (!/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(entry.version ?? '')) {
    throw new Error(`${descriptor} must resolve to one exact version; found ${entry.version}`)
  }
  if (!entry.conditions && !/^10c0\/[0-9a-f]{128}$/.test(entry.checksum ?? '')) {
    throw new Error(`${descriptor} must carry a canonical Yarn checksum`)
  }
  if (!entry.resolution?.includes('@npm:') && !entry.resolution?.startsWith('typescript@patch:')) {
    throw new Error(`${descriptor} must use an npm or audited builtin patch resolution`)
  }
}

const allManifests = [['package.json', manifest], ...[...workspaces.values()].map((workspace) => (
  [`${workspace.path}/package.json`, workspace.manifest]
))]
for (const [path, current] of allManifests) {
  for (const field of ['dependencies', 'devDependencies', 'optionalDependencies']) {
    for (const [name, locator] of Object.entries(current[field] ?? {})) {
      if (/^(?:file|link|portal|git(?:\+[^:]*)?|https?):/i.test(locator)) {
        throw new Error(`${path} ${field}.${name} uses forbidden source locator ${locator}`)
      }
      if (locator.startsWith('workspace:') && !workspaces.has(name)) {
        throw new Error(`${path} ${field}.${name} points to an unknown workspace`)
      }
    }
  }
}

for (const [name, version] of Object.entries({
  'happy-dom': '20.11.6',
  'entities': '7.0.1',
  'whatwg-mimetype': '3.0.0',
  'buffer-image-size': '0.6.4',
  'ws': '8.21.3',
  'react': '19.2.8',
  'react-dom': '19.2.8',
})) {
  const requested = manifest.dependencies?.[name] ?? manifest.devDependencies?.[name]
  if (requested !== version) throw new Error(`package.json must exactly pin ${name}@${version}`)
  const locked = Object.values(lock).find((entry) => entry?.resolution === `${name}@npm:${version}`)
  if (!locked) throw new Error(`yarn.lock must contain ${name}@npm:${version}`)
}

console.log(`Yarn 4.16 PnP lock validated (${workspaces.size} workspaces).`)
