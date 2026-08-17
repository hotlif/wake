import { readFileSync, statSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

export function inspectVsix(path, target) {
  const archive = readFileSync(path)
  const entries = centralDirectoryEntries(archive)
  const serverEntries = entries.filter(
    entry =>
      entry.endsWith('/server/wake-css-language-server') ||
      entry.endsWith('/server/wake-css-language-server.exe'),
  )
  if (serverEntries.length !== 1) {
    throw new Error(`expected exactly one language server, found ${serverEntries.length}`)
  }
  const shouldUseExe = target.startsWith('win32')
  if (serverEntries[0].endsWith('.exe') !== shouldUseExe) {
    throw new Error(`language server executable does not match target ${target}`)
  }
  for (const forbidden of ['/node_modules/', '/src/', '/test/', '/scripts/']) {
    if (entries.some(entry => entry.includes(forbidden))) {
      throw new Error(`VSIX contains forbidden development path ${forbidden}`)
    }
  }
  const removedBundlerName = 'es' + 'build'
  for (const forbidden of [removedBundlerName, `@${removedBundlerName}/`]) {
    if (entries.some(entry => entry.toLowerCase().includes(forbidden))) {
      throw new Error(`VSIX contains forbidden build dependency ${forbidden}`)
    }
  }
  for (const forbidden of [
    'extension/wake',
    'extension/wake.exe',
    'extension/syntaxes/crab-css.injection.json',
    'extension/index.html',
    'extension/dist/index.html',
    'extension/dist/manifest.json',
  ]) {
    if (entries.includes(forbidden)) {
      throw new Error(`VSIX contains forbidden build artifact ${forbidden}`)
    }
  }
  for (const required of [
    'extension/package.json',
    'extension/dist/extension.js',
    'extension/THIRD_PARTY_NOTICES.md',
  ]) {
    if (!entries.includes(required)) throw new Error(`VSIX is missing ${required}`)
  }
  if (statSync(path).size > 15 * 1024 * 1024) {
    throw new Error('VSIX exceeds the 15 MiB package budget')
  }
  return entries
}

function centralDirectoryEntries(buffer) {
  const entries = []
  for (let offset = 0; offset + 46 <= buffer.length;) {
    const signature = buffer.readUInt32LE(offset)
    if (signature !== 0x02014b50) {
      offset += 1
      continue
    }
    const nameLength = buffer.readUInt16LE(offset + 28)
    const extraLength = buffer.readUInt16LE(offset + 30)
    const commentLength = buffer.readUInt16LE(offset + 32)
    entries.push(buffer.subarray(offset + 46, offset + 46 + nameLength).toString('utf8'))
    offset += 46 + nameLength + extraLength + commentLength
  }
  return entries
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [, , path, target] = process.argv
  if (!path || !target) throw new Error('usage: node scripts/check-vsix.mjs <path> <target>')
  const entries = inspectVsix(path, target)
  console.log(`Verified ${entries.length} VSIX entries for ${target}`)
}
