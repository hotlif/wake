import { existsSync, readFileSync, readdirSync } from 'node:fs'
import { resolve } from 'node:path'

const root = resolve(import.meta.dirname, '..')
const npmRoot = resolve(root, 'npm')
const workflowPath = resolve(root, '.github/workflows/release-npm.yml')
const workflow = readFileSync(workflowPath, 'utf8')
const vscodeWorkflow = readFileSync(
  resolve(root, '.github/workflows/vscode-css.yml'),
  'utf8',
)

const packages = readdirSync(npmRoot, { withFileTypes: true })
  .filter((entry) => entry.isDirectory())
  .map((entry) => {
    const manifestPath = resolve(npmRoot, entry.name, 'package.json')
    if (!existsSync(manifestPath)) return undefined
    const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'))
    return { directory: `npm/${entry.name}`, directoryName: entry.name, manifest }
  })
  .filter((entry) => entry && entry.manifest.private !== true)
  .sort((left, right) => left.manifest.name.localeCompare(right.manifest.name))

if (packages.length === 0) {
  throw new Error('No publishable npm packages were discovered under npm/*')
}
for (const required of [
  "- 'v*'",
  "- '!vscode-css-v*'",
  "if: github.ref_type == 'tag' && github.ref_name == format('v{0}', needs.verify.outputs.version)",
  'npm publish',
]) {
  if (!workflow.includes(required)) {
    throw new Error(`release-npm.yml is missing release contract marker ${required}`)
  }
}

const missing = []
for (const { directory, directoryName, manifest } of packages) {
  if (typeof manifest.name !== 'string' || typeof manifest.version !== 'string') {
    throw new Error(`${directory}/package.json must declare name and version`)
  }
  if (manifest.publishConfig?.access !== 'public') {
    throw new Error(`${manifest.name} must set publishConfig.access to public`)
  }
  if (manifest.publishConfig?.provenance !== true) {
    throw new Error(`${manifest.name} must enable publishConfig.provenance`)
  }

  const directoryCovered = workflow.includes(directory)
    || workflow.includes(`package_dir: ${directoryName}`)
  const nameCovered = workflow.includes(`'${manifest.name}'`)
    || workflow.includes(`"${manifest.name}"`)
  if (!directoryCovered || !nameCovered) {
    missing.push(
      `${manifest.name} (${directory}; build=${directoryCovered}; audit=${nameCovered})`,
    )
  }
}

if (missing.length > 0) {
  throw new Error(`npm packages missing automatic release coverage:\n${missing.join('\n')}`)
}

const extensionManifest = JSON.parse(
  readFileSync(resolve(root, 'editors/vscode-css/package.json'), 'utf8'),
)
if (extensionManifest.private !== true) {
  throw new Error(
    'editors/vscode-css is distributed as a GitHub VSIX and must be private for npm',
  )
}

const vscodeTargets = [
  'win32-x64',
  'linux-x64',
  'linux-arm64',
  'darwin-x64',
  'darwin-arm64',
]
for (const target of vscodeTargets) {
  if (!vscodeWorkflow.includes(`vsce_target: ${target}`)) {
    throw new Error(`VS Code automatic release is missing target ${target}`)
  }
}
for (const required of [
  "- 'vscode-css-v*'",
  'github-release:',
  'contents: write',
  'actions/attest-build-provenance@v3',
  'gh release',
  'needs.verify.outputs.version',
  'export WAKE_BIN="$PWD/${{ matrix.wake_binary }}"',
  '--binary "$PWD/${{ matrix.binary }}"',
  '--out "$PWD/artifacts"',
]) {
  if (!vscodeWorkflow.includes(required)) {
    throw new Error(`vscode-css.yml is missing release contract marker ${required}`)
  }
}
for (const forbidden of [
  'vsce publish',
  'VSCE_PAT',
  'vscode-marketplace-release',
  '--azure-credential',
  '--oidc',
]) {
  if (vscodeWorkflow.includes(forbidden)) {
    throw new Error(`vscode-css.yml must not publish to a marketplace: ${forbidden}`)
  }
}
const packageScript = readFileSync(
  resolve(root, 'editors/vscode-css/scripts/package-vsix.mjs'),
  'utf8',
)
if (!packageScript.includes('extensionManifest.version')) {
  throw new Error('VSIX archive names must use the extension manifest version')
}

console.log(
  `npm automatic release coverage: ${packages.length}/${packages.length} (${packages.map(({ manifest }) => manifest.name).join(', ')})`,
)
console.log(
  `GitHub VSIX automatic release coverage: ${vscodeTargets.length}/${vscodeTargets.length} (${vscodeTargets.join(', ')})`,
)
