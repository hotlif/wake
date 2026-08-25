import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

import {
  NATIVE_MANIFEST_FILE,
  NATIVE_SBOM_FILE,
  THIRD_PARTY_LICENSE_FILE,
  artifactRecord,
  createNativeManifest,
  createNativeSbom,
  createThirdPartyLicenses,
  expectedPlatformFiles,
  platformContract,
  readEngineProvenance,
} from './native-package-contract.mjs'

const root = resolve(import.meta.dirname, '..')

function readJson(path, description) {
  try {
    return JSON.parse(readFileSync(path, 'utf8'))
  } catch (error) {
    throw new Error(`Unable to read ${description} at ${path}`, { cause: error })
  }
}

function stableJson(value) {
  return JSON.stringify(value, null, 2)
}

function assertEqual(actual, expected, description) {
  if (stableJson(actual) !== stableJson(expected)) {
    throw new Error(`${description} does not match the staged native artifacts`)
  }
}

function normalizedText(path) {
  return readFileSync(path, 'utf8').replaceAll('\r\n', '\n')
}

export function verifyNativePackage(packageDirectory) {
  const directory = resolve(packageDirectory)
  const packageManifest = readJson(
    resolve(directory, 'package.json'),
    'platform package manifest',
  )
  const contract = platformContract(packageManifest.name)
  const bindingPath = `wake.${contract.suffix}.node`
  if (packageManifest.main !== `./${bindingPath}`) {
    throw new Error(`${packageManifest.name} main must be ./${bindingPath}`)
  }
  for (const field of ['dependencies', 'devDependencies', 'optionalDependencies', 'scripts']) {
    if (Object.keys(packageManifest[field] ?? {}).length !== 0) {
      throw new Error(`${packageManifest.name} must not declare ${field}`)
    }
  }
  const expectedManifestFiles = expectedPlatformFiles(packageManifest, contract)
    .filter((path) => path !== 'package.json')
    .sort()
  const actualManifestFiles = (packageManifest.files ?? []).slice().sort()
  assertEqual(
    actualManifestFiles,
    expectedManifestFiles,
    `${packageManifest.name} files whitelist`,
  )

  const engine = readEngineProvenance(root, contract)
  const artifacts = [
    artifactRecord('node-binding', bindingPath, resolve(directory, bindingPath)),
    artifactRecord(
      'test-host',
      contract.hostPath,
      resolve(directory, contract.hostPath),
    ),
  ]
  const expectedNativeManifest = createNativeManifest({
    packageManifest,
    contract,
    artifacts,
    engine,
  })
  const actualNativeManifest = readJson(
    resolve(directory, NATIVE_MANIFEST_FILE),
    'native manifest',
  )
  assertEqual(
    actualNativeManifest,
    expectedNativeManifest,
    `${packageManifest.name} native manifest`,
  )

  const actualSbom = readJson(
    resolve(directory, NATIVE_SBOM_FILE),
    'native SPDX SBOM',
  )
  const created = actualSbom.creationInfo?.created
  if (
    typeof created !== 'string'
    || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(created)
    || Number.isNaN(Date.parse(created))
  ) {
    throw new Error(`${packageManifest.name} SBOM has an invalid creation timestamp`)
  }
  assertEqual(
    actualSbom,
    createNativeSbom(expectedNativeManifest, created),
    `${packageManifest.name} SPDX SBOM`,
  )

  const expectedThirdPartyLicenses = createThirdPartyLicenses(root, engine)
  const actualThirdPartyLicenses = normalizedText(
    resolve(directory, THIRD_PARTY_LICENSE_FILE),
  )
  if (actualThirdPartyLicenses !== expectedThirdPartyLicenses) {
    throw new Error(`${packageManifest.name} third-party licenses are stale`)
  }
  for (const licenseFile of ['LICENSE-MIT', 'LICENSE-APACHE']) {
    if (
      normalizedText(resolve(directory, licenseFile))
      !== normalizedText(resolve(root, 'npm/wake', licenseFile))
    ) {
      throw new Error(`${packageManifest.name} ${licenseFile} is stale`)
    }
  }

  return {
    buildId: expectedNativeManifest.build.id,
    contract,
    packageManifest,
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const packageDirectory = process.argv[2]
  if (!packageDirectory || process.argv.length !== 3) {
    throw new Error('Usage: node scripts/verify-native-package.mjs <package-directory>')
  }
  const result = verifyNativePackage(packageDirectory)
  console.log(
    `${result.packageManifest.name}: verified ${result.buildId}`,
  )
}
