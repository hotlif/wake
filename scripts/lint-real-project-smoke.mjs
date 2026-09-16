import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const root = resolve(import.meta.dirname, '..')
const binary = resolve(root, 'target', 'debug', process.platform === 'win32' ? 'wake.exe' : 'wake')
const project = mkdtempSync(join(tmpdir(), 'wake-lint-real-project-'))

function runAt(projectRoot, args) {
  const result = spawnSync(binary, ['lint', '--root', projectRoot, '--format', 'json', ...args], {
    cwd: root,
    encoding: 'utf8',
  })
  assert.equal(result.error, undefined, result.error?.message)
  let json
  try {
    json = JSON.parse(result.stdout)
  } catch (error) {
    throw new Error(`wake lint returned non-JSON (${result.status}): ${result.stderr}`, { cause: error })
  }
  return { status: result.status, json }
}

function run(args) {
  return runAt(project, args)
}

try {
  mkdirSync(join(project, 'src'), { recursive: true })
  mkdirSync(join(project, 'docs'), { recursive: true })
  writeFileSync(join(project, 'wake.config.toml'), `[lint]
files = ["src/**/*.js", "src/**/*.mjs", "src/**/*.cjs", "src/**/*.jsx", "src/**/*.ts", "src/**/*.mts", "src/**/*.cts", "src/**/*.tsx", "docs/**/*.md"]
[lint.processors]
"docs/**/*.md" = "markdown"
[lint.rules]
"js/no-debugger" = "warn"
"react/self-closing-comp" = "error"
"ts/no-explicit-any" = "error"
`)
  writeFileSync(join(project, 'src/index.js'), 'debugger;\n')
  writeFileSync(join(project, 'src/module.mjs'), 'debugger;\n')
  writeFileSync(join(project, 'src/legacy.cjs'), 'debugger;\n')
  writeFileSync(join(project, 'src/module.mts'), 'debugger;\n')
  writeFileSync(join(project, 'src/legacy.cts'), 'debugger;\n')
  writeFileSync(join(project, 'src/Widget.jsx'), 'const Widget = () => <Button></Button>;\n')
  writeFileSync(join(project, 'src/types.ts'), 'const value: any = 1;\n')
  writeFileSync(join(project, 'src/entry.js'), "import './missing.js';\nimport './cycle-a.js';\n")
  writeFileSync(join(project, 'src/cycle-a.js'), "import './cycle-b.js';\n")
  writeFileSync(join(project, 'src/cycle-b.js'), "import './cycle-a.js';\n")
  writeFileSync(join(project, 'src/App.tsx'), 'const App = () => <Panel></Panel>;\ndebugger;\n')
  const markdown = '# Guide\n\n```js\ndebugger;\n```\n'
  writeFileSync(join(project, 'docs/guide.md'), markdown)

  const checked = run([])
  assert.equal(checked.status, 1)
  assert.equal(checked.json.files.length, 12)
  const diagnostics = checked.json.files.flatMap((file) => file.diagnostics)
  assert.equal(diagnostics.filter((item) => item.code === 'js/no-debugger').length, 7)
  assert.equal(diagnostics.filter((item) => item.code === 'react/self-closing-comp').length, 2)
  assert.equal(diagnostics.filter((item) => item.code === 'ts/no-explicit-any').length, 1)
  assert.equal(readFileSync(join(project, 'docs/guide.md'), 'utf8'), markdown)

  const config = run(['--print-config', 'docs/guide.md'])
  assert.equal(config.status, 0)
  assert.equal(config.json.config.processor, 'markdown')

  const preview = run(['src/App.tsx', '--fix-dry-run'])
  assert.equal(preview.status, 0)
  assert.equal(preview.json.files[0].changed, true)
  assert.match(preview.json.files[0].output, /<Panel\/>/)
  assert.equal(readFileSync(join(project, 'src/App.tsx'), 'utf8'), 'const App = () => <Panel></Panel>;\ndebugger;\n')

  const markdownFix = spawnSync(binary, ['lint', '--root', project, '--format', 'json', 'docs/guide.md', '--fix-dry-run'], {
    cwd: root,
    encoding: 'utf8',
  })
  assert.equal(markdownFix.status, 2)
  assert.match(markdownFix.stderr, /WAKE_LINT_CONFIG/)

  writeFileSync(join(project, 'wake.config.toml'), `[lint]
files = ["src/**/*.js"]
[lint.rules]
"import/no-unresolved" = "error"
"import/no-cycle" = "error"
`)
  const modules = run([])
  assert.equal(modules.status, 1)
  assert.equal(modules.json.files.length, 4)
  const moduleDiagnostics = modules.json.files.flatMap((file) => file.diagnostics)
  assert.equal(moduleDiagnostics.filter((item) => item.code === 'import/no-unresolved').length, 1)
  assert.ok(moduleDiagnostics.filter((item) => item.code === 'import/no-cycle').length >= 1)

  const matrixProjects = [
    {
      name: 'npm-package',
      setup(rootDir) {
        mkdirSync(join(rootDir, 'src'), { recursive: true })
        writeFileSync(join(rootDir, 'package.json'), JSON.stringify({ name: 'wake-smoke-package', type: 'module' }))
        writeFileSync(join(rootDir, 'wake.config.toml'), `[lint]
files = ["src/**/*.js"]
environments = ["node"]
[lint.rules]
"js/no-debugger" = "error"
"import/no-unresolved" = "error"
"js/no-undef" = "error"
`)
        writeFileSync(join(rootDir, 'src/index.js'), "import fs from 'node:fs';\nprocess;\ndebugger;\n")
      },
      check(rootDir, result) {
        assert.equal(result.status, 1)
        assert.equal(result.json.files.length, 1)
        const diagnostics = result.json.files[0].diagnostics
        assert.equal(diagnostics.filter((item) => item.code === 'js/no-debugger').length, 1)
        assert.equal(diagnostics.filter((item) => item.code === 'import/no-unresolved').length, 0)
        assert.equal(diagnostics.filter((item) => item.code === 'js/no-undef').length, 0)
      },
    },
    {
      name: 'workspace-monorepo',
      setup(rootDir) {
        for (const packageName of ['app', 'shared']) mkdirSync(join(rootDir, 'packages', packageName, 'src'), { recursive: true })
        writeFileSync(join(rootDir, 'package.json'), JSON.stringify({ name: 'wake-smoke-workspace', workspaces: ['packages/*'] }))
        writeFileSync(join(rootDir, 'wake.config.toml'), `[lint]
files = ["packages/*/src/**/*.js"]
[lint.rules]
"js/no-debugger" = "warn"
`)
        writeFileSync(join(rootDir, 'packages/app/package.json'), JSON.stringify({ name: '@smoke/app' }))
        writeFileSync(join(rootDir, 'packages/shared/package.json'), JSON.stringify({ name: '@smoke/shared' }))
        writeFileSync(join(rootDir, 'packages/app/src/index.js'), 'debugger;\n')
        writeFileSync(join(rootDir, 'packages/shared/src/index.js'), 'debugger;\n')
      },
      check(rootDir, result) {
        assert.equal(result.status, 0)
        assert.equal(result.json.files.length, 2)
        assert.equal(result.json.files.flatMap((file) => file.diagnostics).filter((item) => item.code === 'js/no-debugger').length, 2)
      },
    },
    {
      name: 'pnp-manifest',
      setup(rootDir) {
        mkdirSync(join(rootDir, 'src'), { recursive: true })
        mkdirSync(join(rootDir, '.yarn', 'cache', 'dep-npm-1.0.0', 'node_modules', 'dep'), { recursive: true })
        writeFileSync(join(rootDir, 'package.json'), JSON.stringify({ name: 'wake-smoke-pnp', packageManager: 'yarn@4.16.0' }))
        writeFileSync(join(rootDir, 'wake.config.toml'), `[lint]
files = ["src/**/*.js"]
[lint.rules]
"import/no-unresolved" = "error"
`)
        writeFileSync(join(rootDir, 'src/index.js'), "import dep from 'dep';\nvoid dep;\n")
        writeFileSync(join(rootDir, '.yarn', 'cache', 'dep-npm-1.0.0', 'node_modules', 'dep', 'index.js'), 'export default 1;\n')
        writeFileSync(join(rootDir, '.pnp.cjs'), "module.exports = require('./.pnp.data.json');\n")
        writeFileSync(join(rootDir, '.pnp.data.json'), JSON.stringify({
          enableTopLevelFallback: false,
          fallbackExclusionList: [],
          fallbackPool: [],
          packageRegistryData: [
            [null, [[null, { packageLocation: './', packageDependencies: [['dep', 'npm:1.0.0']], linkType: 'SOFT' }]]],
            ['dep', [['npm:1.0.0', { packageLocation: './.yarn/cache/dep-npm-1.0.0/node_modules/dep/', packageDependencies: [['dep', 'npm:1.0.0']], linkType: 'HARD' }]]],
          ],
        }))
      },
      check(rootDir, result) {
        assert.equal(result.status, 0)
        assert.equal(result.json.files.length, 1)
        assert.equal(result.json.files[0].diagnostics.length, 0)
      },
    },
  ]
  for (const fixture of matrixProjects) {
    const matrixRoot = mkdtempSync(join(tmpdir(), `wake-lint-${fixture.name}-`))
    try {
      fixture.setup(matrixRoot)
      fixture.check(matrixRoot, runAt(matrixRoot, []))
    } finally {
      rmSync(matrixRoot, { recursive: true, force: true })
    }
  }

  const repositoryFixtures = [
    {
      name: 'react-ts-app',
      paths: ['src'],
      expectedFiles: ['src/index.tsx'],
    },
    {
      name: 'react-docs',
      paths: ['src', 'docs'],
      ruleArgs: [
        '--rule',
        'import/no-unresolved=error',
        '--rule',
        'import/no-extraneous-dependencies=error',
      ],
      expectedFiles: [
        'docs/Badge.tsx',
        'docs/components/demos/basic.demo.tsx',
        'docs/components/demos/states.demo.tsx',
        'docs/preview.tsx',
        'src/button.tsx',
      ],
    },
    {
      name: 'react-components-yarn-pnp',
      paths: ['docs', 'workspaces/rc-pnp/docs'],
      expectedFiles: [
        'docs/components/demos/basic.demo.tsx',
        'workspaces/rc-pnp/docs/components/demos/basic.demo.tsx',
      ],
    },
    {
      name: 'react-ts-app-yarn-pnp',
      paths: ['src'],
      expectedFiles: ['src/App.tsx', 'src/index.tsx', 'src/types.ts', 'src/utils.ts'],
    },
    {
      name: 'react-docs-workspaces-alpha',
      root: resolve(root, 'fixtures', 'react-docs-workspaces', 'components', 'rc-alpha'),
      paths: ['docs'],
      expectedFiles: ['docs/components/demos/basic.demo.tsx'],
    },
    {
      name: 'react-docs-workspaces-beta',
      root: resolve(root, 'fixtures', 'react-docs-workspaces', 'components', 'rc-beta'),
      paths: ['docs'],
      expectedFiles: ['docs/components/demos/basic.demo.tsx'],
    },
  ]
  for (const fixture of repositoryFixtures) {
    const fixtureRoot = fixture.root ?? resolve(root, 'fixtures', fixture.name)
    const result = runAt(fixtureRoot, [...(fixture.ruleArgs ?? []), ...fixture.paths])
    assert.equal(result.status, 0, fixture.name)
    assert.deepEqual(result.json.files.map((file) => file.path), fixture.expectedFiles, fixture.name)
    assert.equal(result.json.errorCount, 0, fixture.name)
  }
  console.log('wake lint real-project smoke ok')
} finally {
  rmSync(project, { recursive: true, force: true })
}
