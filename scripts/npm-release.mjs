import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

const registry = 'https://registry.npmjs.org'
const run = (program, args) => execFileSync(program, args, { encoding: 'utf8' }).trim()
const manifest = () => JSON.parse(readFileSync('npm/wake/package.json', 'utf8'))

export function matchesVerifiedPush(ci, repository, checkout, contextSha) {
  return ci?.conclusion === 'success' && ci.event === 'push' && ci.head_branch === 'canary'
    && ci.head_repository?.full_name === repository && ci.head_sha === checkout && ci.head_sha === contextSha
}

function currentVerifiedHead() {
  const env = process.env
  if (env.GITHUB_ACTIONS !== 'true' || env.GITHUB_EVENT_NAME !== 'workflow_run') {
    throw new Error('npm release requires a successful canary CI run')
  }
  const { workflow_run: ci } = JSON.parse(readFileSync(env.GITHUB_EVENT_PATH, 'utf8'))
  if (!matchesVerifiedPush(ci, env.GITHUB_REPOSITORY, run('git', ['rev-parse', 'HEAD']), env.GITHUB_SHA)) {
    throw new Error('The checkout and Actions context must match the verified canary push')
  }
  const head = run('gh', ['api', `repos/${env.GITHUB_REPOSITORY}/git/ref/heads/canary`, '--jq', '.object.sha'])
  if (head === ci.head_sha) return true
  console.log(`Skipping stale release for ${ci.head_sha}. Current canary is ${head}.`)
  return false
}

export function isVersionPullRequest(pr, repository, commit) {
  return Boolean(pr.merged_at) && pr.merge_commit_sha === commit
    && pr.base.ref === 'canary' && pr.base.repo?.full_name === repository
    && pr.head.ref === 'changeset-release/canary' && pr.head.repo?.full_name === repository
}

function pendingVersion() {
  const { version } = manifest()
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`Invalid Wake release version: ${version}`)
  if (run('git', ['tag', '--list', `v${version}`])) return false
  // A newer verified push can pick up an unpublished version if canary moved during a build.
  const commit = run('git', ['log', '--first-parent', '-1', '--format=%H',
    `-S"version": "${version}"`, '--', 'npm/wake/package.json'])
  if (!commit) throw new Error(`Cannot find the commit introducing Wake ${version}`)
  const repo = process.env.GITHUB_REPOSITORY
  const prs = JSON.parse(run('gh', ['api', '--paginate', '--slurp', `repos/${repo}/commits/${commit}/pulls`])).flat()
  if (!prs.some(pr => isVersionPullRequest(pr, repo, commit))) {
    throw new Error(`Wake ${version} must be introduced by a merged Changesets Release PR`)
  }
  return true
}

export async function registryHasArchive(name, version, integrity) {
  const response = await fetch(`${registry}/${encodeURIComponent(name)}/${version}`)
  if (response.status === 404) return false
  if (!response.ok) throw new Error(`Registry lookup for ${name}@${version} returned ${response.status}`)
  if ((await response.json()).dist?.integrity !== integrity) {
    throw new Error(`Registry bytes for ${name}@${version} differ from the audited tarball`)
  }
  return true
}

export function publicationOrder(main) {
  return [...Object.keys(main.optionalDependencies), '@crab-dev/css', '@crab-dev/wake-lint-sdk', main.name]
}

async function publish() {
  if (!currentVerifiedHead()) throw new Error('A newer canary push must complete verification before publishing')
  const main = manifest()
  const { repository } = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'))
  for (const name of publicationOrder(main)) {
    const file = `./artifacts/${name.slice(1).replace('/', '-')}-${main.version}.tgz`
    const integrity = 'sha512-' + createHash('sha512').update(readFileSync(file)).digest('base64')
    if (await registryHasArchive(name, main.version, integrity)) {
      console.log(`Verified already published ${name}@${main.version}`)
      continue
    }
    execFileSync('npm', ['publish', file, '--ignore-scripts', '--access=public',
      `--registry=${registry}`, `--provenance=${repository.visibility === 'public'}`], { stdio: 'inherit' })
  }
}

async function main() {
  switch (process.argv[2]) {
    case 'head':
      appendFileSync(process.env.GITHUB_OUTPUT, `current=${currentVerifiedHead()}\n`)
      break
    case 'candidate':
      appendFileSync(process.env.GITHUB_ENV, `WAKE_RELEASE_CANDIDATE=${pendingVersion()}\n`)
      // Changesets' publish hook selects the matrix. The publish job owns registry writes and tagging.
      writeFileSync(process.env.CHANGESETS_OUTPUT, '')
      break
    case 'publish':
      await publish()
      break
    default:
      throw new Error('Expected head, candidate or publish')
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch(error => {
    console.error(error.message)
    process.exitCode = 1
  })
}
