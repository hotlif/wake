import { spawnSync } from 'node:child_process'
import { statSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

export function assertUnixTestHostMode(mode) {
  if ((mode & 0o111) !== 0o111) {
    throw new Error(`Installed Unix test host must have owner, group and other executable bits; found ${
      (mode & 0o777).toString(8)
    }`)
  }
}

export function checkInstalledTestHost(project, platformPackage, hostPlatform = process.platform) {
  const hostName = hostPlatform === 'win32' ? 'wake-test-host.exe' : 'wake-test-host'
  const path = join(project, 'node_modules', platformPackage, 'test-host', hostName)
  const host = statSync(path)
  if (!host.isFile()) throw new Error(`Installed test host is not a file: ${path}`)
  if (hostPlatform !== 'win32') assertUnixTestHostMode(host.mode)
}

function run(command, args, project, label) {
  const env = { ...process.env }
  delete env.NODE_OPTIONS
  delete env.npm_execpath
  const result = spawnSync(command, args, {
    cwd: project,
    env,
    encoding: 'utf8',
    windowsHide: true,
  })
  if (result.error) throw new Error(`${label} failed to start: ${result.error.message}`)
  if (result.signal) throw new Error(`${label} ended with signal ${result.signal}`)
  return result
}

function parseLintResult(runResult, expectedStatus, label) {
  if (runResult.status !== expectedStatus) {
    throw new Error(`${label} lint smoke exited ${runResult.status}; expected ${expectedStatus}: ${runResult.stderr}`)
  }
  let result
  try {
    result = JSON.parse(runResult.stdout)
  } catch (error) {
    throw new Error(`${label} lint smoke did not return JSON: ${error.message}`)
  }
  const diagnostics = result.files?.flatMap(file => file.diagnostics ?? [])
  if (result.exitCode !== 1 || result.errorCount !== 1 || diagnostics?.length !== 1
    || diagnostics[0].code !== 'js/no-debugger') {
    throw new Error(`${label} lint smoke did not report the expected js/no-debugger error: ${runResult.stdout}`)
  }
  return diagnostics[0]
}

export function runInstalledLintSmoke(project, wakeCli) {
  writeFileSync(join(project, 'lint-smoke.js'), 'debugger;\n')
  const cli = parseLintResult(run(process.execPath, [
    wakeCli, 'lint', '--root', project, 'lint-smoke.js', '--format', 'json',
    '--rule', 'js/no-debugger=error',
  ], project, 'CLI'), 1, 'CLI')
  const options = "{root:process.cwd(),paths:['lint-smoke.js'],rules:{'js/no-debugger':'error'}}"
  const commonjs = parseLintResult(run(process.execPath, ['-e', [
    "const {lint}=require('@crab-dev/wake')",
    `lint(${options}).then(result=>process.stdout.write(JSON.stringify(result))).catch(error=>{console.error(error);process.exitCode=1})`,
  ].join(';')], project, 'CommonJS'), 0, 'CommonJS')
  const esm = parseLintResult(run(process.execPath, ['--input-type=module', '-e', [
    "const {lint}=await import('@crab-dev/wake')",
    `const result=await lint(${options})`,
    'process.stdout.write(JSON.stringify(result))',
  ].join(';')], project, 'ESM'), 0, 'ESM')
  if (JSON.stringify(commonjs) !== JSON.stringify(cli) || JSON.stringify(esm) !== JSON.stringify(cli)) {
    throw new Error('CLI, CommonJS and ESM lint smoke diagnostics differ')
  }
}
