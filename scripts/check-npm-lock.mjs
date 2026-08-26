import { checkNpmRepository } from './check-architecture.mjs'

const errors = checkNpmRepository()
if (errors.length > 0) {
  console.error(`npm lock check failed with ${errors.length} error(s):`)
  for (const error of errors) console.error(`- ${error}`)
  process.exitCode = 1
} else {
  console.log('npm lock check passed: workspace, internal optional, registry and integrity records are valid.')
}
