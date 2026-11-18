import assert from 'node:assert/strict'
import { readdirSync, readFileSync } from 'node:fs'
import { parseSyml } from '@yarnpkg/parsers'
import { isVersionPullRequest, matchesVerifiedPush, publicationOrder, registryHasArchive } from './npm-release.mjs'
import { test } from 'node:test'

const read = (path) => readFileSync(new URL(path, import.meta.url), 'utf8')

test('npm release consumes successful canary CI and only uses the tested checkout', () => {
  const workflow = parseSyml(read('../.github/workflows/release-npm.yml'))
  assert.deepEqual(workflow.on, {
    workflow_run: { workflows: ['CI'], types: ['completed'], branches: ['canary'] },
    workflow_dispatch: null,
  })
  const plan = workflow.jobs['prepare-release']
  for (const guard of [
    "github.event.workflow_run.event == 'push'",
    "github.event.workflow_run.conclusion == 'success'",
    "github.event.workflow_run.head_branch == 'canary'",
    'github.event.workflow_run.head_repository.full_name == github.repository',
    'github.event.workflow_run.head_sha == github.sha',
  ]) assert.ok(plan.if.includes(guard), guard)
  for (const job of Object.values(workflow.jobs)) {
    for (const step of job.steps.filter(step => step.uses?.startsWith('actions/checkout@'))) {
      assert.match(step.with.ref, /^\$\{\{ github\.event\.workflow_run\.head_sha(?: \|\| github\.sha)? \}\}$/)
    }
  }
  const guard = plan.steps.findIndex(step => step.run === 'node scripts/npm-release.mjs head')
  const action = plan.steps.find(step => step.uses === 'changesets/action@v2')
  assert.ok(guard >= 0 && plan.steps.indexOf(action) > guard)
  assert.equal(action.if, "steps.verified.outputs.current == 'true'")
  assert.equal(action.with['pr-base-branch'], 'canary')
  assert.equal(action.with['version-script'], 'corepack yarn changeset:version')
  assert.equal(action.with['publish-script'], 'node scripts/npm-release.mjs candidate')
  assert.ok(plan.outputs.build.includes("outputs.has-changesets == 'false' && env.WAKE_RELEASE_CANDIDATE == 'true'"))
  assert.equal(workflow.jobs.verify.if, "needs.prepare-release.outputs.build == 'true'")
  const { publish, tag } = workflow.jobs
  assert.equal(publish.if, "github.event_name == 'workflow_run'")
  assert.deepEqual(publish.needs, ['verify', 'audit-tarballs', 'prepublish-smoke'])
  assert.ok(publish.steps.some(step => step.run === 'node scripts/npm-release.mjs publish'))
  assert.equal(tag.needs, 'publish')
})

test('Changesets and the publisher cover exactly the public Wake npm packages', () => {
  const packages = readdirSync(new URL('../npm/', import.meta.url), { withFileTypes: true })
    .filter(entry => entry.isDirectory())
    .map(entry => JSON.parse(read(`../npm/${entry.name}/package.json`)))
  const names = packages.filter(pkg => !pkg.private).map(pkg => pkg.name).sort()
  const config = JSON.parse(read('../.changeset/config.json'))
  assert.deepEqual(config.fixed.map(group => group.toSorted()), [names])
  assert.equal(config.baseBranch, 'canary')
  assert.deepEqual(config.privatePackages, { version: false, tag: false })
  const main = JSON.parse(read('../npm/wake/package.json'))
  const order = publicationOrder(main)
  assert.deepEqual(order.toSorted(), names)
  assert.deepEqual(order.slice(-3), ['@crab-dev/css', '@crab-dev/wake-lint-sdk', main.name])
})

test('the Actions context, checkout and trusted CI push must agree', () => {
  const repo = 'owner/wake'
  const ci = { conclusion: 'success', event: 'push', head_branch: 'canary', head_repository: { full_name: repo }, head_sha: 'verified' }
  assert.ok(matchesVerifiedPush(ci, repo, 'verified', 'verified'))
  for (const change of [{ conclusion: 'failure' }, { event: 'pull_request' }, { head_branch: 'other' },
    { head_repository: { full_name: 'fork/wake' } }, { head_sha: 'other' }]) {
    assert.equal(matchesVerifiedPush({ ...ci, ...change }, repo, 'verified', 'verified'), false)
  }
  assert.equal(matchesVerifiedPush(ci, repo, 'other', 'verified'), false)
  assert.equal(matchesVerifiedPush(ci, repo, 'verified', 'other'), false)
})

test('only the merged Changesets PR for the version commit authorizes the release', () => {
  const repo = 'owner/wake'
  const pr = { merged_at: '2026-11-18', merge_commit_sha: 'merge', base: { ref: 'canary', repo: { full_name: repo } },
    head: { ref: 'changeset-release/canary', repo: { full_name: repo } } }
  assert.ok(isVersionPullRequest(pr, repo, 'merge'))
  for (const change of [{ merged_at: null }, { merge_commit_sha: 'old' }, { base: { ref: 'main' } },
    { head: { ...pr.head, ref: 'feature' } }, { head: { ...pr.head, repo: { full_name: 'fork/wake' } } }]) {
    assert.equal(isVersionPullRequest({ ...pr, ...change }, repo, 'merge'), false)
  }
})

test('publication skips only identical registry bytes and fails closed on lookup errors', async (t) => {
  const lookup = t.mock.method(globalThis, 'fetch')
  for (const [status, integrity, expected] of [[404, null, false], [200, 'sha512-same', true], [200, 'sha512-other'], [403], [503]]) {
    lookup.mock.mockImplementation(async () => Response.json({ dist: { integrity } }, { status }))
    const result = registryHasArchive('@crab-dev/wake', '1.0.0', 'sha512-same')
    if (expected === undefined) await assert.rejects(result)
    else assert.equal(await result, expected)
  }
})
