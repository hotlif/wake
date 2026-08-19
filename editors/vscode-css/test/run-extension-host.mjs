import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { runTests } from '@vscode/test-electron'

const extensionDevelopmentPath = resolve(fileURLToPath(new URL('..', import.meta.url)))
const extensionTestsPath = resolve(extensionDevelopmentPath, '.test-dist/suite/index.js')
const fixture = resolve(extensionDevelopmentPath, 'test/fixture')
const vscodeExecutablePath = process.env.VSCODE_TEST_EXECUTABLE_PATH

await runTests({
  ...(vscodeExecutablePath
    ? { vscodeExecutablePath }
    : { version: process.env.VSCODE_TEST_VERSION ?? '1.96.4' }),
  extensionDevelopmentPath,
  extensionTestsPath,
  launchArgs: [fixture, '--disable-extensions'],
})
