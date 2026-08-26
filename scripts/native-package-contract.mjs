import { createHash } from 'node:crypto'
import { readFileSync, statSync } from 'node:fs'
import { createRequire } from 'node:module'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { parseSyml } from '@yarnpkg/parsers'

const require = createRequire(import.meta.url)
const pnpapi = require('pnpapi')

export const MEBIBYTE = 1024 * 1024
export const PLATFORM_PACKED_WARNING = 56 * MEBIBYTE
export const PLATFORM_PACKED_LIMIT = 64 * MEBIBYTE
export const PLATFORM_UNPACKED_LIMIT = 192 * MEBIBYTE

export const NATIVE_MANIFEST_FILE = 'native-manifest.json'
export const NATIVE_SBOM_FILE = 'sbom.spdx.json'
export const THIRD_PARTY_LICENSE_FILE = 'THIRD_PARTY_LICENSES.txt'

const RUSTY_V8_RELEASE_BASE = 'https://github.com/denoland/rusty_v8/releases/download/v150.4.0'

function rustyV8Archive(name, sha256, size) {
  return Object.freeze({
    name,
    sha256,
    size,
    url: `${RUSTY_V8_RELEASE_BASE}/${name}`,
  })
}

export const PLATFORM_CONTRACTS = Object.freeze({
  '@crab-dev/wake-win32-x64-msvc': Object.freeze({
    directory: 'npm/wake-win32-x64-msvc',
    target: 'x86_64-pc-windows-msvc',
    suffix: 'win32-x64-msvc',
    hostPath: 'test-host/wake-test-host.exe',
    rustyV8Archive: rustyV8Archive(
      'rusty_v8_simdutf_release_x86_64-pc-windows-msvc.lib.gz',
      'f231f82cbacb9aefe6d9af57e6df2e8959a40e001f79306485133e3c075b98f0',
      39_225_458,
    ),
  }),
  '@crab-dev/wake-linux-x64-gnu': Object.freeze({
    directory: 'npm/wake-linux-x64-gnu',
    target: 'x86_64-unknown-linux-gnu',
    suffix: 'linux-x64-gnu',
    hostPath: 'test-host/wake-test-host',
    rustyV8Archive: rustyV8Archive(
      'librusty_v8_simdutf_release_x86_64-unknown-linux-gnu.a.gz',
      'f48762ca10d1f1fc605a441c5ae430ec8ce1e9e80f14d78fbc42cb878c30b476',
      38_799_023,
    ),
  }),
  '@crab-dev/wake-linux-arm64-gnu': Object.freeze({
    directory: 'npm/wake-linux-arm64-gnu',
    target: 'aarch64-unknown-linux-gnu',
    suffix: 'linux-arm64-gnu',
    hostPath: 'test-host/wake-test-host',
    rustyV8Archive: rustyV8Archive(
      'librusty_v8_simdutf_release_aarch64-unknown-linux-gnu.a.gz',
      '539e283815a396a5796f32858b42e517b858ebaaeaaad05d03290ee8c864a527',
      37_577_326,
    ),
  }),
  '@crab-dev/wake-darwin-x64': Object.freeze({
    directory: 'npm/wake-darwin-x64',
    target: 'x86_64-apple-darwin',
    suffix: 'darwin-x64',
    hostPath: 'test-host/wake-test-host',
    rustyV8Archive: rustyV8Archive(
      'librusty_v8_simdutf_release_x86_64-apple-darwin.a.gz',
      'a750271fec6b211457ed0a5cf7d2eab1924b265621a82da86ab959d6ff0823e4',
      39_073_447,
    ),
  }),
  '@crab-dev/wake-darwin-arm64': Object.freeze({
    directory: 'npm/wake-darwin-arm64',
    target: 'aarch64-apple-darwin',
    suffix: 'darwin-arm64',
    hostPath: 'test-host/wake-test-host',
    rustyV8Archive: rustyV8Archive(
      'librusty_v8_simdutf_release_aarch64-apple-darwin.a.gz',
      '5aeffd8d5a0c1b79ac1d70af83d5b19099655fd9c645a794dc43f101f779838c',
      39_039_034,
    ),
  }),
})

const TEST_RUNTIME_SOURCES = 'engineering/test-runtime-sources.json'

function requiredMatch(source, expression, description) {
  const value = source.match(expression)?.[1]
  if (!value) throw new Error(`Unable to read ${description}`)
  return value
}

function packageBlock(lock, name) {
  const blocks = lock.split(/\r?\n(?=\[\[package\]\])/)
  const block = blocks.find((candidate) => (
    candidate.match(/^name = "([^"]+)"$/m)?.[1] === name
  ))
  if (!block) throw new Error(`Cargo.lock is missing ${name}`)
  return block
}

export function sha256(value) {
  return createHash('sha256').update(value).digest('hex')
}

export function sha256File(path) {
  return sha256(readFileSync(path))
}

export function platformContract(packageName) {
  const contract = PLATFORM_CONTRACTS[packageName]
  if (!contract) throw new Error(`Unsupported native package ${packageName}`)
  return contract
}

export function readEngineProvenance(root, contract) {
  const cargoLock = readFileSync(resolve(root, 'Cargo.lock'), 'utf8')
  const packageManifest = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf8'))
  const yarnLock = parseSyml(readFileSync(resolve(root, 'yarn.lock'), 'utf8'))
  const testRuntimeSources = JSON.parse(
    readFileSync(resolve(root, TEST_RUNTIME_SOURCES), 'utf8'),
  )
  if (
    testRuntimeSources.schemaVersion !== 2 ||
    testRuntimeSources.contract !== 'ADR-0020'
  ) {
    throw new Error('Test runtime provenance must use ADR-0020 schema v2')
  }
  const runtimeCrate = (name) => {
    const provenance = testRuntimeSources.rustCrates?.find((entry) => entry.name === name)
    if (!provenance) throw new Error(`Test runtime provenance is missing crate ${name}`)
    const locked = packageBlock(cargoLock, name)
    const version = requiredMatch(locked, /^version = "([^"]+)"$/m, `${name} version`)
    const source = requiredMatch(locked, /^source = "([^"]+)"$/m, `${name} source`)
    const checksum = requiredMatch(locked, /^checksum = "([0-9a-f]{64})"$/m, `${name} checksum`)
    if (
      provenance.registry !== 'https://crates.io' ||
      version !== provenance.version ||
      source !== 'registry+https://github.com/rust-lang/crates.io-index' ||
      checksum !== provenance.checksum
    ) {
      throw new Error(`${name} must come from the checksummed crates.io Cargo.lock entry`)
    }
    return Object.freeze({
      name,
      version,
      license: provenance.license,
      crateSha256: checksum,
      vcsCommit: provenance.vcsCommit ?? null,
    })
  }
  const denoCore = runtimeCrate('deno_core')
  const denoV8 = runtimeCrate('deno_v8')
  const rustyV8 = runtimeCrate('v8')
  if (
    testRuntimeSources.engine?.denoRelease !== '2.9.5' ||
    testRuntimeSources.engine?.v8Version !== '15.0.245.2' ||
    !/^[0-9a-f]{40}$/.test(denoCore.vcsCommit ?? '') ||
    denoV8.vcsCommit !== denoCore.vcsCommit
  ) {
    throw new Error('Deno/V8 release and crate VCS provenance require an audited update')
  }
  const happyDom = testRuntimeSources.sources?.find(({ name }) => name === 'happy-dom')
  if (
    !happyDom ||
    happyDom.version !== '20.11.6' ||
    !/^sha512-[A-Za-z0-9+/]+=*$/.test(happyDom.integrity) ||
    !/^[0-9a-f]{40}$/.test(happyDom.gitHead) ||
    happyDom.license !== 'MIT'
  ) {
    throw new Error('Happy DOM provenance must pin version, integrity, gitHead, and MIT license')
  }
  const installedDomPackages = (testRuntimeSources.sources ?? [])
    .filter(({ embedded }) => embedded === true)
    .map((source) => {
      const requested = packageManifest.dependencies?.[source.name] ??
        packageManifest.devDependencies?.[source.name]
      const locked = Object.values(yarnLock).find((entry) => (
        entry?.resolution === `${source.name}@npm:${source.version}`
      ))
      if (
        locked?.version !== source.version ||
        !/^10c0\/[0-9a-f]{128}$/.test(locked?.checksum ?? '')
      ) {
        throw new Error(`${source.name} must come from the checksummed Yarn npm resolution`)
      }
      if (source.name === 'happy-dom' && requested !== source.version) {
        throw new Error(`happy-dom must be exactly pinned to ${source.version} in package.json`)
      }
      return Object.freeze({
        name: source.name,
        version: source.version,
        license: source.license,
        licenseFile: source.licenseFile,
        tarball: source.tarball,
        integrity: source.integrity,
        gitHead: source.gitHead ?? null,
      })
    })
  if (!installedDomPackages.some(({ name }) => name === 'happy-dom')) {
    throw new Error('Embedded DOM package closure must contain happy-dom')
  }
  const expectedDomPackages = [
    'buffer-image-size',
    'entities',
    'happy-dom',
    'whatwg-mimetype',
  ]
  const actualDomPackages = installedDomPackages.map(({ name }) => name).sort()
  if (JSON.stringify(actualDomPackages) !== JSON.stringify(expectedDomPackages)) {
    throw new Error(`Embedded DOM package closure must be exactly ${expectedDomPackages.join(', ')}`)
  }
  if (rustyV8.version !== '150.4.0') {
    throw new Error(
      `Rusty V8 ${rustyV8.version} requires an audited provenance update`,
    )
  }

  return Object.freeze({
    runtime: 'V8',
    deno: Object.freeze({
      version: testRuntimeSources.engine?.denoRelease,
      commit: denoCore.vcsCommit,
    }),
    crates: Object.freeze({
      denoCore,
      denoV8,
      rustyV8: Object.freeze({
        ...rustyV8,
        prebuiltArchive: contract.rustyV8Archive,
      }),
    }),
    v8: Object.freeze({
      version: testRuntimeSources.engine?.v8Version,
      license: 'BSD-3-Clause',
    }),
    dom: Object.freeze({
      happyDom: installedDomPackages.find(({ name }) => name === 'happy-dom'),
      embeddedPackages: Object.freeze(installedDomPackages),
    }),
  })
}

export function artifactRecord(kind, relativePath, absolutePath) {
  return Object.freeze({
    kind,
    path: relativePath.replaceAll('\\', '/'),
    size: statSync(absolutePath).size,
    sha256: sha256File(absolutePath),
  })
}

export function verifyRustyV8Archive(path, contract) {
  const archive = contract.rustyV8Archive
  const actualSize = statSync(path).size
  if (actualSize !== archive.size) {
    throw new Error(
      `${archive.name} size ${actualSize} does not match pinned size ${archive.size}`,
    )
  }
  const actualSha256 = sha256File(path)
  if (actualSha256 !== archive.sha256) {
    throw new Error(
      `${archive.name} SHA-256 ${actualSha256} does not match pinned ${archive.sha256}`,
    )
  }
  return archive
}

export function computeNativeBuildId({
  packageName,
  version,
  target,
  artifacts,
  engine,
}) {
  const identity = [
    'wake-native-build-id-v1',
    packageName,
    version,
    target,
    ...artifacts
      .slice()
      .sort((left, right) => left.path.localeCompare(right.path))
      .flatMap((artifact) => [
        artifact.kind,
        artifact.path,
        String(artifact.size),
        artifact.sha256,
      ]),
    engine.runtime,
    engine.deno.version,
    engine.deno.commit,
    engine.crates.denoCore.version,
    engine.crates.denoCore.crateSha256,
    engine.crates.denoV8.version,
    engine.crates.denoV8.crateSha256,
    engine.crates.rustyV8.version,
    engine.crates.rustyV8.crateSha256,
    engine.crates.rustyV8.prebuiltArchive.name,
    engine.crates.rustyV8.prebuiltArchive.sha256,
    String(engine.crates.rustyV8.prebuiltArchive.size),
    engine.v8.version,
    ...engine.dom.embeddedPackages
      .slice()
      .sort((left, right) => left.name.localeCompare(right.name))
      .flatMap((dependency) => [
        dependency.name,
        dependency.version,
        dependency.integrity,
      ]),
  ].join('\n')
  return `sha256:${sha256(identity)}`
}

export function createNativeManifest({
  packageManifest,
  contract,
  artifacts,
  engine,
}) {
  const buildId = computeNativeBuildId({
    packageName: packageManifest.name,
    version: packageManifest.version,
    target: contract.target,
    artifacts,
    engine,
  })
  return {
    schemaVersion: 1,
    package: {
      name: packageManifest.name,
      version: packageManifest.version,
      target: contract.target,
    },
    build: {
      id: buildId,
      algorithm: 'sha256-v1',
    },
    artifacts,
    engine,
    browser: {
      bundled: false,
      runtimeDownload: false,
      selection: 'explicit-path-or-system-discovery',
    },
  }
}

function spdxId(value) {
  return value.replace(/[^A-Za-z0-9.-]+/g, '-')
}

function spdxPackage({
  name,
  version,
  license,
  downloadLocation,
  checksum,
  checksumAlgorithm = 'SHA256',
}) {
  return {
    name,
    SPDXID: `SPDXRef-Package-${spdxId(name)}`,
    versionInfo: version,
    downloadLocation,
    filesAnalyzed: false,
    licenseConcluded: license,
    licenseDeclared: license,
    copyrightText: 'NOASSERTION',
    ...(checksum
      ? { checksums: [{ algorithm: checksumAlgorithm, checksumValue: checksum }] }
      : {}),
  }
}

function spdxFile(artifact) {
  return {
    fileName: `./${artifact.path}`,
    SPDXID: `SPDXRef-File-${spdxId(artifact.path)}`,
    checksums: [{ algorithm: 'SHA256', checksumValue: artifact.sha256 }],
    licenseConcluded: 'NOASSERTION',
    copyrightText: 'NOASSERTION',
  }
}

export function createNativeSbom(nativeManifest, created) {
  const buildHash = nativeManifest.build.id.slice('sha256:'.length)
  const engine = nativeManifest.engine
  const wakePackage = spdxPackage({
    name: nativeManifest.package.name,
    version: nativeManifest.package.version,
    license: 'MIT OR Apache-2.0',
    downloadLocation: 'NOASSERTION',
  })
  const denoCore = spdxPackage({
    name: engine.crates.denoCore.name,
    version: engine.crates.denoCore.version,
    license: engine.crates.denoCore.license,
    downloadLocation: `https://crates.io/api/v1/crates/deno_core/${engine.crates.denoCore.version}/download`,
    checksum: engine.crates.denoCore.crateSha256,
  })
  const denoV8 = spdxPackage({
    name: engine.crates.denoV8.name,
    version: engine.crates.denoV8.version,
    license: engine.crates.denoV8.license,
    downloadLocation: `https://crates.io/api/v1/crates/deno_v8/${engine.crates.denoV8.version}/download`,
    checksum: engine.crates.denoV8.crateSha256,
  })
  const rustyV8 = spdxPackage({
    name: 'rusty_v8',
    version: engine.crates.rustyV8.version,
    license: engine.crates.rustyV8.license,
    downloadLocation: `https://crates.io/api/v1/crates/v8/${engine.crates.rustyV8.version}/download`,
    checksum: engine.crates.rustyV8.crateSha256,
  })
  const rustyV8Archive = spdxPackage({
    name: `rusty_v8-prebuilt-${nativeManifest.package.target}`,
    version: engine.crates.rustyV8.version,
    license: 'MIT AND BSD-3-Clause',
    downloadLocation: engine.crates.rustyV8.prebuiltArchive.url,
    checksum: engine.crates.rustyV8.prebuiltArchive.sha256,
  })
  const v8 = spdxPackage({
    name: 'V8',
    version: engine.v8.version,
    license: engine.v8.license,
    downloadLocation: 'https://chromium.googlesource.com/v8/v8',
  })
  const domPackages = engine.dom.embeddedPackages.map((dependency) => spdxPackage({
    name: dependency.name,
    version: dependency.version,
    license: dependency.license,
    downloadLocation: dependency.tarball,
    checksum: Buffer.from(
      dependency.integrity.slice('sha512-'.length),
      'base64',
    ).toString('hex'),
    checksumAlgorithm: 'SHA512',
  }))
  const happyDom = domPackages.find(({ name }) => name === 'happy-dom')
  if (!happyDom) throw new Error('Native SBOM requires Happy DOM')
  const packages = [
    wakePackage,
    denoCore,
    denoV8,
    rustyV8,
    rustyV8Archive,
    v8,
    ...domPackages,
  ]
  const files = nativeManifest.artifacts.map(spdxFile)
  return {
    spdxVersion: 'SPDX-2.3',
    dataLicense: 'CC0-1.0',
    SPDXID: 'SPDXRef-DOCUMENT',
    name: `${nativeManifest.package.name}-${nativeManifest.package.version}-native`,
    documentNamespace: `https://github.com/hotlif/wake/sbom/${spdxId(nativeManifest.package.name)}/${nativeManifest.package.version}/${buildHash}`,
    creationInfo: {
      created,
      creators: ['Tool: Wake native package staging v1'],
    },
    packages,
    files,
    relationships: [
      {
        spdxElementId: 'SPDXRef-DOCUMENT',
        relationshipType: 'DESCRIBES',
        relatedSpdxElement: wakePackage.SPDXID,
      },
      ...packages.slice(1).map((dependency) => ({
        spdxElementId: wakePackage.SPDXID,
        relationshipType: 'DEPENDS_ON',
        relatedSpdxElement: dependency.SPDXID,
      })),
      ...files.map((file) => ({
        spdxElementId: wakePackage.SPDXID,
        relationshipType: 'CONTAINS',
        relatedSpdxElement: file.SPDXID,
      })),
      {
        spdxElementId: denoCore.SPDXID,
        relationshipType: 'DEPENDS_ON',
        relatedSpdxElement: denoV8.SPDXID,
      },
      {
        spdxElementId: denoV8.SPDXID,
        relationshipType: 'DEPENDS_ON',
        relatedSpdxElement: rustyV8.SPDXID,
      },
      {
        spdxElementId: rustyV8.SPDXID,
        relationshipType: 'DEPENDS_ON',
        relatedSpdxElement: v8.SPDXID,
      },
      {
        spdxElementId: rustyV8Archive.SPDXID,
        relationshipType: 'GENERATED_FROM',
        relatedSpdxElement: rustyV8.SPDXID,
      },
      {
        spdxElementId: rustyV8Archive.SPDXID,
        relationshipType: 'GENERATED_FROM',
        relatedSpdxElement: v8.SPDXID,
      },
      ...domPackages
        .filter(({ SPDXID }) => SPDXID !== happyDom.SPDXID)
        .map((dependency) => ({
          spdxElementId: happyDom.SPDXID,
          relationshipType: 'DEPENDS_ON',
          relatedSpdxElement: dependency.SPDXID,
        })),
    ],
  }
}

const DENO_LICENSE = `MIT License

Copyright 2018-2026 the Deno authors

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software is furnished to do so,
subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.`

const V8_LICENSE = `Copyright 2014 the V8 project authors. All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

* Redistributions of source code must retain the above copyright notice,
  this list of conditions and the following disclaimer.
* Redistributions in binary form must reproduce the above copyright notice,
  this list of conditions and the following disclaimer in the documentation
  and/or other materials provided with the distribution.
* Neither the name of Google Inc. nor the names of its contributors may be
  used to endorse or promote products derived from this software without
  specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.`

export function createThirdPartyLicenses(root, engine) {
  const domLicenses = engine.dom.embeddedPackages.map((dependency) => {
    if (!dependency.licenseFile) {
      throw new Error(`${dependency.name} provenance must declare its npm license file`)
    }
    const licensePath = pnpapi.resolveToUnqualified(
      `${dependency.name}/${dependency.licenseFile}`,
      fileURLToPath(import.meta.url),
    )
    let license
    try {
      license = readFileSync(licensePath, 'utf8').replaceAll('\r\n', '\n').trim()
    } catch (error) {
      throw new Error(
        `Unable to read Yarn PnP license ${licensePath}; run yarn install --immutable --check-cache before staging (${error.message})`,
      )
    }
    return `${dependency.name} ${dependency.version} — ${dependency.license}
${'-'.repeat(dependency.name.length + dependency.version.length + dependency.license.length + 5)}

${license}`
  }).join('\n\n')
  return `Wake native test engine third-party licenses
================================================

This platform package embeds the following audited engine components:

- deno_core ${engine.crates.denoCore.version} (MIT)
- deno_v8 ${engine.crates.denoV8.version} (MIT)
- Rusty V8 ${engine.crates.rustyV8.version} (MIT)
- V8 ${engine.v8.version} (BSD-3-Clause)
${engine.dom.embeddedPackages.map((dependency) => `- ${dependency.name} ${dependency.version} (${dependency.license})`).join('\n')}

The target-specific Rusty V8 archive is ${engine.crates.rustyV8.prebuiltArchive.name}
(SHA-256 ${engine.crates.rustyV8.prebuiltArchive.sha256}). It is verified before
the release build and is not downloaded by the installed package.

Deno and Rusty V8 — MIT
-----------------------

${DENO_LICENSE}

Embedded npm DOM packages
-------------------------

${domLicenses}

V8 — BSD-3-Clause
-----------------

${V8_LICENSE}
`
}

export function reproducibleTimestamp() {
  const sourceDateEpoch = process.env.SOURCE_DATE_EPOCH ?? '0'
  if (!/^\d+$/.test(sourceDateEpoch)) {
    throw new Error('SOURCE_DATE_EPOCH must be a non-negative integer')
  }
  const milliseconds = Number(sourceDateEpoch) * 1000
  const date = new Date(milliseconds)
  if (!Number.isFinite(milliseconds) || Number.isNaN(date.valueOf())) {
    throw new Error('SOURCE_DATE_EPOCH is outside the supported date range')
  }
  return date.toISOString().replace('.000Z', 'Z')
}

export function expectedPlatformFiles(packageManifest, contract) {
  if (PLATFORM_CONTRACTS[packageManifest.name] !== contract) {
    throw new Error(`${packageManifest.name} does not match its platform contract`)
  }
  return [
    'LICENSE-APACHE',
    'LICENSE-MIT',
    'README.md',
    THIRD_PARTY_LICENSE_FILE,
    NATIVE_MANIFEST_FILE,
    'package.json',
    NATIVE_SBOM_FILE,
    `wake.${contract.suffix}.node`,
    contract.hostPath,
  ].sort()
}
