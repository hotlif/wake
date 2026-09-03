import { constants, copyFileSync, linkSync, mkdirSync } from 'node:fs'
import { basename, dirname, join } from 'node:path'

function readGitHubRunIdentity(environment) {
  const runId = environment.GITHUB_RUN_ID
  const runAttempt = environment.GITHUB_RUN_ATTEMPT
  if (runId === undefined && runAttempt === undefined) return null
  if (
    !/^[1-9][0-9]*$/.test(runId ?? '')
    || !/^[1-9][0-9]*$/.test(runAttempt ?? '')
    || typeof environment.RUNNER_TEMP !== 'string'
    || environment.RUNNER_TEMP.length === 0
  ) {
    throw new Error(
      'Invalid GitHub run identity: GITHUB_RUN_ID and GITHUB_RUN_ATTEMPT must be positive integers and RUNNER_TEMP must be set',
    )
  }
  return { runId, runAttempt, runnerTemp: environment.RUNNER_TEMP }
}

export function selectRustyV8ArchivePath(canonicalArchive, environment) {
  const identity = readGitHubRunIdentity(environment)
  if (!identity) return canonicalArchive
  // rust-cache can restore Cargo's v8 build-script fingerprint without its
  // generated native library. A per-attempt path changes the tracked env value,
  // forcing upstream v8/build.rs to materialize the archive again.
  return join(
    identity.runnerTemp,
    'wake-rusty-v8',
    `run-${identity.runId}-attempt-${identity.runAttempt}`,
    basename(canonicalArchive),
  )
}

export function materializeRustyV8ArchiveForEnvironment(
  canonicalArchive,
  environment,
) {
  const selectedArchive = selectRustyV8ArchivePath(canonicalArchive, environment)
  if (selectedArchive === canonicalArchive) return canonicalArchive

  mkdirSync(dirname(selectedArchive), { recursive: true })
  try {
    linkSync(canonicalArchive, selectedArchive)
  } catch (error) {
    if (error?.code !== 'EEXIST') {
      try {
        copyFileSync(canonicalArchive, selectedArchive, constants.COPYFILE_EXCL)
      } catch (copyError) {
        if (copyError?.code !== 'EEXIST') throw copyError
      }
    }
  }
  return selectedArchive
}
