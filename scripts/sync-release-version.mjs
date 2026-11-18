import { readFileSync, writeFileSync } from 'node:fs'

// Changesets owns the public package group. These consumers sit outside it.
const { version } = JSON.parse(readFileSync('npm/wake/package.json', 'utf8'))
const workspace = JSON.parse(readFileSync('package.json', 'utf8'))
workspace.version = version
workspace.dependencies['@crab-dev/css'] = version
writeFileSync('package.json', JSON.stringify(workspace, null, 2) + '\n')

function updatePin(path, pattern) {
  const source = readFileSync(path, 'utf8')
  if (!pattern.test(source)) throw new Error(`Missing Wake version in ${path}`)
  writeFileSync(path, source.replace(pattern, (_, before, after) => before + version + after))
}
updatePin('Cargo.toml', /(\[workspace\.package\]\s+version = ")[^"]+(")/)
updatePin('.yarnrc.yml', /(\s+"@crab-dev\/css": ")[^"]+(")/)
