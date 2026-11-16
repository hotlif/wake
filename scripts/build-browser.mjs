import { execFileSync } from 'node:child_process'
import { copyFileSync, mkdirSync } from 'node:fs'
import { resolve } from 'node:path'
import { verifyBrowserArtifact } from './verify-browser-artifact.mjs'

const root = resolve(import.meta.dirname, '..')
execFileSync('cargo', ['+1.95.0', 'build', '--locked', '--offline', '--release', '--target', 'wasm32-unknown-unknown', '-p', 'wake_browser'], {
  cwd: root, stdio: 'inherit',
})
const source = resolve(root, process.env.CARGO_TARGET_DIR ?? 'target', 'wasm32-unknown-unknown/release/wake_browser.wasm')
verifyBrowserArtifact(source)
const directory = resolve(root, 'npm/wake/browser')
mkdirSync(directory, { recursive: true })
copyFileSync(source, resolve(directory, 'wake_browser.wasm'))
console.log('Staged version-matched Wake browser compiler for npm packaging.')
