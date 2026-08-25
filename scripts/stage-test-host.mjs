import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from 'node:fs'
import { dirname, resolve } from 'node:path'

import {
  NATIVE_MANIFEST_FILE,
  NATIVE_SBOM_FILE,
  PLATFORM_CONTRACTS,
  THIRD_PARTY_LICENSE_FILE,
  artifactRecord,
  createNativeManifest,
  createNativeSbom,
  createThirdPartyLicenses,
  platformContract,
  readEngineProvenance,
  reproducibleTimestamp,
  verifyRustyV8Archive,
} from './native-package-contract.mjs'

const root = resolve(import.meta.dirname, '..')

function parseArguments(values) {
  const supported = new Set([
    'binding',
    'host',
    'package-dir',
    'suffix',
    'target',
    'v8-archive',
  ])
  const parsed = new Map()
  for (let index = 0; index < values.length; index += 1) {
    const argument = values[index]
    if (!argument.startsWith('--')) {
      throw new Error(`Unexpected positional argument ${argument}`)
    }
    const [rawName, inlineValue] = argument.slice(2).split('=', 2)
    if (!supported.has(rawName)) {
      throw new Error(`Unknown option --${rawName}`)
    }
    const value = inlineValue ?? values[++index]
    if (!value || value.startsWith('--')) {
      throw new Error(`--${rawName} requires a value`)
    }
    if (parsed.has(rawName)) {
      throw new Error(`--${rawName} may only be provided once`)
    }
    parsed.set(rawName, value)
  }
  return parsed
}

function currentContract() {
  const suffix = `${process.platform}-${process.arch}`
  const contract = Object.values(PLATFORM_CONTRACTS).find((candidate) => (
    candidate.suffix.startsWith(`${suffix}-`) || candidate.suffix === suffix
  ))
  if (!contract) {
    throw new Error(`Wake does not publish a native package for ${suffix}`)
  }
  return contract
}

function copyArtifact(source, destination, executable) {
  mkdirSync(dirname(destination), { recursive: true })
  if (resolve(source) !== resolve(destination)) copyFileSync(source, destination)
  if (executable && process.platform !== 'win32') chmodSync(destination, 0o755)
}

const argumentsMap = parseArguments(process.argv.slice(2))
const packageDirectory = resolve(
  root,
  argumentsMap.get('package-dir') ?? 'npm/wake',
)
const packageManifest = JSON.parse(
  readFileSync(resolve(packageDirectory, 'package.json'), 'utf8'),
)
const isPlatformPackage = Object.hasOwn(
  PLATFORM_CONTRACTS,
  packageManifest.name,
)
const contract = isPlatformPackage
  ? platformContract(packageManifest.name)
  : currentContract()
const target = argumentsMap.get('target') ?? contract.target
const suffix = argumentsMap.get('suffix') ?? contract.suffix
if (target !== contract.target) {
  throw new Error(
    `${packageManifest.name} requires target ${contract.target}; received ${target}`,
  )
}
if (suffix !== contract.suffix) {
  throw new Error(
    `${packageManifest.name} requires suffix ${contract.suffix}; received ${suffix}`,
  )
}

if (isPlatformPackage) {
  const rustyV8Archive = argumentsMap.get('v8-archive')
    ?? process.env.RUSTY_V8_ARCHIVE
  if (!rustyV8Archive) {
    throw new Error(
      'Platform staging requires a checksum-verified Rusty V8 archive via --v8-archive or RUSTY_V8_ARCHIVE',
    )
  }
  verifyRustyV8Archive(resolve(root, rustyV8Archive), contract)
}

const hostName = contract.hostPath.endsWith('.exe')
  ? 'wake-test-host.exe'
  : 'wake-test-host'
const targetedBuild = argumentsMap.has('target')
const hostSource = resolve(
  root,
  argumentsMap.get('host') ?? (
    targetedBuild
      ? `target/${target}/release/${hostName}`
      : `target/release/${hostName}`
  ),
)
const bindingSource = resolve(
  root,
  argumentsMap.get('binding') ?? `npm/wake/wake.${suffix}.node`,
)
const bindingRelativePath = `wake.${suffix}.node`
const bindingDestination = resolve(packageDirectory, bindingRelativePath)
const hostDestination = resolve(packageDirectory, contract.hostPath)

if (isPlatformPackage) {
  copyArtifact(bindingSource, bindingDestination, false)
}
copyArtifact(hostSource, hostDestination, true)

if (isPlatformPackage) {
  const engine = readEngineProvenance(root, contract)
  const artifacts = [
    artifactRecord('node-binding', bindingRelativePath, bindingDestination),
    artifactRecord('test-host', contract.hostPath, hostDestination),
  ]
  const nativeManifest = createNativeManifest({
    packageManifest,
    contract,
    artifacts,
    engine,
  })
  const sbom = createNativeSbom(nativeManifest, reproducibleTimestamp())
  writeFileSync(
    resolve(packageDirectory, NATIVE_MANIFEST_FILE),
    `${JSON.stringify(nativeManifest, null, 2)}\n`,
  )
  writeFileSync(
    resolve(packageDirectory, NATIVE_SBOM_FILE),
    `${JSON.stringify(sbom, null, 2)}\n`,
  )
  writeFileSync(
    resolve(packageDirectory, THIRD_PARTY_LICENSE_FILE),
    createThirdPartyLicenses(root, engine),
  )
  console.log(
    `Staged ${packageManifest.name} ${nativeManifest.build.id} at ${packageDirectory}`,
  )
} else {
  console.log(`Staged Wake test host at ${hostDestination}`)
}
