import { statSync } from 'node:fs'
import { isAbsolute } from 'node:path'

export function requireWakeBinary(owner) {
  const wakeBinary = process.env.WAKE_BIN
  if (!wakeBinary) {
    throw new Error(`${owner} requires WAKE_BIN to name the freshly built Wake CLI`)
  }
  if (!isAbsolute(wakeBinary)) {
    throw new Error(`${owner} requires an absolute WAKE_BIN path; received ${wakeBinary}`)
  }
  let stats
  try {
    stats = statSync(wakeBinary)
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error)
    throw new Error(`${owner} could not inspect WAKE_BIN ${wakeBinary}: ${detail}`)
  }
  if (!stats.isFile()) {
    throw new Error(`${owner} requires WAKE_BIN to name a file; received ${wakeBinary}`)
  }
  return wakeBinary
}
