import { createHash } from 'node:crypto'
import {
  createReadStream,
  createWriteStream,
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
} from 'node:fs'
import { spawnSync } from 'node:child_process'
import { resolve } from 'node:path'
import { pipeline } from 'node:stream/promises'
import { Readable } from 'node:stream'

const workspace = resolve(import.meta.dirname, '..')
const manifestPath = resolve(workspace, 'engineering/test262-es2024.json')
const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'))
const cache = resolve(workspace, 'target/test262-conformance')
const archive = resolve(cache, `${manifest.commit}.tar.gz`)
const extracted = resolve(cache, `test262-${manifest.commit}`)

mkdirSync(cache, { recursive: true })
async function sha256(path) {
  const digest = createHash('sha256')
  await pipeline(createReadStream(path), digest)
  return digest.digest('hex')
}

if (existsSync(archive) && await sha256(archive) !== manifest.sha256) {
  rmSync(archive, { force: true })
}
if (!existsSync(archive)) {
  const partial = `${archive}.${process.pid}.part`
  rmSync(partial, { force: true })
  try {
    const response = await fetch(manifest.archiveUrl, {
      redirect: 'follow',
      signal: AbortSignal.timeout(120_000),
    })
    if (!response.ok || !response.body) {
      throw new Error(`Unable to download Test262: HTTP ${response.status}`)
    }
    await pipeline(Readable.fromWeb(response.body), createWriteStream(partial, { flags: 'wx' }))
    const actual = await sha256(partial)
    if (actual !== manifest.sha256) {
      throw new Error(`Test262 archive checksum mismatch: expected ${manifest.sha256}, received ${actual}`)
    }
    renameSync(partial, archive)
  } finally {
    rmSync(partial, { force: true })
  }
}

// Always expand the verified archive into a clean, commit-named directory. This prevents a stale
// or locally edited extraction from becoming the conformance input on a warm CI worker.
rmSync(extracted, { recursive: true, force: true })
const unpack = spawnSync('tar', ['-xf', archive, '-C', cache], { stdio: 'inherit' })
if (unpack.error) throw unpack.error
if (unpack.status !== 0) throw new Error(`tar exited with ${unpack.status}`)

const cargo = process.platform === 'win32' ? 'cargo.exe' : 'cargo'
const result = spawnSync(
  cargo,
  [
    'run',
    '--locked',
    '--offline',
    '--quiet',
    '-p',
    'wake_ecma_vm',
    '--example',
    'test262',
    '--',
    extracted,
    manifestPath,
  ],
  { cwd: workspace, stdio: 'inherit' },
)
if (result.error) throw result.error
process.exitCode = result.status ?? 1
