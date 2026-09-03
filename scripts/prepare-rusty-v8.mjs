import { createWriteStream, existsSync, mkdirSync, renameSync, rmSync } from 'node:fs'
import { appendFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { Readable, Transform } from 'node:stream'
import { pipeline } from 'node:stream/promises'

import {
  PLATFORM_CONTRACTS,
  verifyRustyV8Archive,
} from './native-package-contract.mjs'
import { materializeRustyV8ArchiveForEnvironment } from './rusty-v8-archive-path.mjs'

const root = resolve(import.meta.dirname, '..')

function parseTarget(argumentsList) {
  if (
    argumentsList.length !== 2
    || argumentsList[0] !== '--target'
    || !argumentsList[1]
  ) {
    throw new Error('Usage: node scripts/prepare-rusty-v8.mjs --target <rust-target>')
  }
  return argumentsList[1]
}

const target = parseTarget(process.argv.slice(2))
const contract = Object.values(PLATFORM_CONTRACTS).find((candidate) => (
  candidate.target === target
))
if (!contract) throw new Error(`Unsupported Rusty V8 target ${target}`)

const directory = resolve(root, 'target', 'rusty-v8-prebuilt')
const destination = resolve(directory, contract.rustyV8Archive.name)
mkdirSync(directory, { recursive: true })

if (!existsSync(destination)) {
  const temporary = `${destination}.${process.pid}.tmp`
  try {
    const response = await fetch(contract.rustyV8Archive.url, {
      headers: { 'user-agent': 'wake-release-builder' },
      redirect: 'follow',
    })
    if (!response.ok || !response.body) {
      throw new Error(
        `Unable to download ${contract.rustyV8Archive.url}: HTTP ${response.status}`,
      )
    }
    const contentLength = response.headers.get('content-length')
    if (
      contentLength !== null
      && Number(contentLength) !== contract.rustyV8Archive.size
    ) {
      throw new Error(
        `${contract.rustyV8Archive.name} HTTP size ${contentLength} does not match pinned size ${contract.rustyV8Archive.size}`,
      )
    }
    let received = 0
    const sizeLimit = new Transform({
      transform(chunk, _encoding, callback) {
        received += chunk.length
        if (received > contract.rustyV8Archive.size) {
          callback(new Error(`${contract.rustyV8Archive.name} exceeded its pinned size`))
        } else {
          callback(null, chunk)
        }
      },
    })
    await pipeline(
      Readable.fromWeb(response.body),
      sizeLimit,
      createWriteStream(temporary, { flags: 'wx' }),
    )
    verifyRustyV8Archive(temporary, contract)
    renameSync(temporary, destination)
  } catch (error) {
    rmSync(temporary, { force: true })
    throw error
  }
}

verifyRustyV8Archive(destination, contract)
const environmentArchive = materializeRustyV8ArchiveForEnvironment(
  destination,
  process.env,
)
verifyRustyV8Archive(environmentArchive, contract)
if (process.env.GITHUB_ENV) {
  await appendFile(
    process.env.GITHUB_ENV,
    `RUSTY_V8_ARCHIVE=${environmentArchive}\n`,
  )
}
console.log(
  `Verified Rusty V8 ${contract.rustyV8Archive.sha256} at ${environmentArchive}`,
)
