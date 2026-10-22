'use strict'

// This source is embedded by wake_app and run in a short-lived Node process.
// Keep the response writer separate from stdout so ordinary plugin logging
// cannot turn one JSON response into several protocol frames.
const fs = require('node:fs')
const path = require('node:path')
const { createRequire } = require('node:module')
const { pathToFileURL } = require('node:url')
const { createHash } = require('node:crypto')
const writeResponse = fs.writeSync.bind(fs)
const readFile = fs.readFileSync.bind(fs)
const statFile = fs.statSync.bind(fs)
const realPath = fs.realpathSync.bind(fs)
const fileExists = fs.existsSync.bind(fs)
const encodeResponse = JSON.stringify.bind(JSON)
const createRecord = Object.create.bind(Object)
const utf8Length = Buffer.byteLength.bind(Buffer)
process.stdout.write = process.stderr.write.bind(process.stderr)
fs.writeSync = (descriptor, ...arguments_) => {
  if (descriptor === 1) fail('extension attempted to write to protocol stdout')
  return writeResponse(descriptor, ...arguments_)
}

const PROTOCOL = 'wake.lint.extension.v1'
const SCHEMA = 'wake.lint.package.v1'
const PACKAGE_NAME = /^@[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*$/
const RULE_ID = /^[a-z][a-z0-9-]*(?:\/[a-z][a-z0-9-]*)+$/
const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$/
const CATEGORIES = new Set(['problem', 'suggestion', 'layout'])
const LANGUAGES = new Set(['js', 'jsx', 'ts', 'tsx'])
const MAX_REQUEST_BYTES = 20 * 1024 * 1024
const MAX_MANIFEST_BYTES = 256 * 1024
const MAX_MODULE_BYTES = 16 * 1024 * 1024
const MAX_SOURCE_BYTES = 16 * 1024 * 1024
const MAX_PACKAGES = 32
const MAX_RULES = 512
const MAX_DIAGNOSTICS = 1000
const MAX_RESPONSE_BYTES = 2 * 1024 * 1024
let responseNonce = ''

function fail(message) {
  throw new Error(message)
}

function object(value, label) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) fail(`${label} must be an object`)
  return value
}

function fields(value, required, label) {
  object(value, label)
  for (const key of required) if (!Object.hasOwn(value, key)) fail(`${label} is missing ${key}`)
  for (const key of Object.keys(value)) if (!required.includes(key)) fail(`unknown ${label} field: ${key}`)
}

function string(value, label, maximum = 4096) {
  if (typeof value !== 'string' || value.length === 0 || value.length > maximum) {
    fail(`${label} must be a non-empty string of at most ${maximum} characters`)
  }
  return value
}

function jsonFile(file, limit, label) {
  const stat = statFile(file)
  if (!stat.isFile() || stat.size > limit) fail(`${label} exceeds its size budget or is not a file`)
  const data = readFile(file)
  if (data.length > limit) fail(`${label} exceeds its size budget`)
  return data.toString('utf8')
}

function moduleHash(file, label) {
  const stat = statFile(file)
  if (!stat.isFile() || stat.size > MAX_MODULE_BYTES) fail(`${label} exceeds its size budget or is not a file`)
  const source = readFile(file)
  if (source.length > MAX_MODULE_BYTES) fail(`${label} exceeds its size budget`)
  return createHash('sha256').update(source).digest('hex')
}

function parseJson(text, label) {
  try {
    return JSON.parse(text)
  } catch (error) {
    fail(`${label} is invalid JSON: ${error.message}`)
  }
}

function inside(root, target) {
  const relative = path.relative(root, target)
  return relative === '' || (relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative))
}

function packageFile(packageRoot, relative, label) {
  string(relative, label)
  if (!relative.startsWith('./') || relative.includes('\0')) fail(`${label} must be a package-relative path`)
  const absolute = path.resolve(packageRoot, relative)
  if (!inside(packageRoot, absolute)) fail(`${label} escapes the package`)
  const realRoot = realPath(packageRoot)
  const realFile = realPath(absolute)
  if (!inside(realRoot, realFile)) fail(`${label} escapes the package`)
  if (!statFile(realFile).isFile()) fail(`${label} is not a file`)
  return absolute
}

function validateManifest(manifest, name, version) {
  fields(manifest, ['schema', 'name', 'version', 'protocol', 'sdkMajor', 'rules'], 'manifest')
  if (manifest.schema !== SCHEMA || manifest.protocol !== PROTOCOL || manifest.sdkMajor !== 1) {
    fail('extension manifest schema, protocol, or SDK major is incompatible')
  }
  if (manifest.name !== name || manifest.version !== version || !VERSION.test(manifest.version)) {
    fail(`extension manifest identity does not match ${name}`)
  }
  if (!Array.isArray(manifest.rules) || manifest.rules.length === 0 || manifest.rules.length > MAX_RULES) {
    fail('manifest rules must be a non-empty bounded array')
  }
  const ids = new Set()
  for (const rule of manifest.rules) {
    fields(rule, ['id', 'meta', 'messageIds', 'optionsSchema'], 'manifest rule')
    if (typeof rule.id !== 'string' || !RULE_ID.test(rule.id) || ids.has(rule.id)) fail(`invalid or duplicate rule id: ${rule.id}`)
    ids.add(rule.id)
    validateMeta(rule.meta)
    if (!Array.isArray(rule.messageIds) || rule.messageIds.length === 0 || rule.messageIds.length > 256) fail(`${rule.id} messageIds must be a non-empty bounded array`)
    const messageIds = new Set()
    for (const messageId of rule.messageIds) {
      string(messageId, `${rule.id} messageId`, 256)
      if (messageIds.has(messageId)) fail(`${rule.id} has a duplicate messageId`)
      messageIds.add(messageId)
    }
    fields(rule.optionsSchema, ['type', 'additionalProperties', 'properties'], `${rule.id} optionsSchema`)
    if (rule.optionsSchema.type !== 'object' || rule.optionsSchema.additionalProperties !== false) {
      fail(`${rule.id} optionsSchema must be a closed object`)
    }
    object(rule.optionsSchema.properties, `${rule.id} optionsSchema.properties`)
    if (Object.keys(rule.optionsSchema.properties).length !== 0) fail(`${rule.id} optionsSchema properties are unsupported`)
  }
  return manifest
}

function validateMeta(meta) {
  fields(meta, ['description', 'category', 'fixable'], 'rule meta')
  string(meta.description, 'rule meta.description')
  if (!CATEGORIES.has(meta.category) || meta.fixable !== false) fail('rule meta category or fixable value is unsupported')
}

function validatePackages(request, mode) {
  if (!Array.isArray(request.packages) || request.packages.length === 0 || request.packages.length > MAX_PACKAGES) {
    fail('packages must be a non-empty bounded array')
  }
  const names = new Set()
  for (const value of request.packages) {
    const name = mode === 'manifest' ? value : object(value, 'package request').name
    if (typeof name !== 'string' || !PACKAGE_NAME.test(name) || names.has(name)) fail(`invalid or duplicate extension package: ${name}`)
    names.add(name)
    if (mode === 'run') {
      fields(value, ['name', 'manifestText', 'modulePath', 'moduleHash'], 'package request')
      string(value.manifestText, `${name} manifestText`, MAX_MANIFEST_BYTES)
      if (!path.isAbsolute(string(value.modulePath, `${name} modulePath`))) fail(`${name} modulePath must be absolute`)
      if (typeof value.moduleHash !== 'string' || !/^[0-9a-f]{64}$/.test(value.moduleHash)) {
        fail(`${name} moduleHash must be a SHA-256 digest`)
      }
    }
  }
}

function locatePackage(start, name) {
  let directory = statFile(start).isDirectory() ? start : path.dirname(start)
  for (;;) {
    const candidate = path.join(directory, 'package.json')
    if (fileExists(candidate)) {
      const parsed = object(parseJson(jsonFile(candidate, MAX_MANIFEST_BYTES, 'extension package.json'), 'extension package.json'), 'extension package.json')
      if (parsed.name === name) return { packageRoot: directory, packageJson: parsed }
    }
    const parent = path.dirname(directory)
    if (parent === directory) fail(`cannot locate package.json for ${name}`)
    directory = parent
  }
}

function importOnlyPackage(root, name) {
  // CommonJS resolution cannot see a package exposing only an `import` export.
  // PnP gives us the package locator without importing any project module.
  if (fileExists(path.join(root, '.pnp.cjs'))) {
    const pnp = require('pnpapi')
    const location = pnp.resolveToUnqualified(name, path.join(root, 'package.json'))
    if (!location) fail(`cannot resolve extension package ${name}`)
    return locatePackage(location, name)
  }
  // Mirror Node's package search from the project issuer for node_modules.
  // The direct-dependency check is performed before reaching this fallback.
  let directory = root
  for (;;) {
    const candidate = path.join(directory, 'node_modules', ...name.split('/'))
    const metadata = path.join(candidate, 'package.json')
    if (fileExists(metadata)) return locatePackage(candidate, name)
    const parent = path.dirname(directory)
    if (parent === directory) fail(`cannot resolve extension package ${name}`)
    directory = parent
  }
}

function resolvePackage(root, rootPackage, name) {
  const dependencies = ['dependencies', 'devDependencies', 'optionalDependencies']
  if (!dependencies.some((key) => rootPackage[key] && typeof rootPackage[key] === 'object' && Object.hasOwn(rootPackage[key], name))) {
    fail(`${name} is not a direct dependency of the project`)
  }
  const issuer = createRequire(path.join(root, 'package.json'))
  let located
  try {
    located = locatePackage(issuer.resolve(name), name)
  } catch (error) {
    if (!['ERR_PACKAGE_PATH_NOT_EXPORTED', 'ERR_MODULE_NOT_FOUND', 'MODULE_NOT_FOUND'].includes(error?.code)) throw error
    located = importOnlyPackage(root, name)
  }
  const { packageRoot, packageJson } = located
  if (!VERSION.test(packageJson.version)) fail(`${name} has an invalid package version`)
  fields(packageJson.wakeLint, ['manifest', 'module'], `${name} wakeLint`)
  const manifestPath = packageFile(packageRoot, packageJson.wakeLint.manifest, `${name} wakeLint.manifest`)
  const modulePath = packageFile(packageRoot, packageJson.wakeLint.module, `${name} wakeLint.module`)
  const manifestText = jsonFile(manifestPath, MAX_MANIFEST_BYTES, `${name} manifest`)
  const manifest = validateManifest(parseJson(manifestText, `${name} manifest`), name, packageJson.version)
  return { name, manifestText, manifest, modulePath, moduleHash: moduleHash(modulePath, `${name} module`) }
}

function samePackageIdentity(left, right) {
  return left.name === right.name && left.manifestText === right.manifestText
    && left.modulePath === right.modulePath && left.moduleHash === right.moduleHash
}

function validateRuntime(plugin, manifest) {
  fields(plugin, ['protocol', 'sdkMajor', 'name', 'version', 'rules'], 'runtime plugin')
  if (plugin.protocol !== PROTOCOL || plugin.sdkMajor !== 1 || plugin.name !== manifest.name || plugin.version !== manifest.version) {
    fail(`${manifest.name} runtime plugin identity or protocol differs from its manifest`)
  }
  if (!Array.isArray(plugin.rules) || plugin.rules.length !== manifest.rules.length) fail(`${manifest.name} runtime rule list differs from its manifest`)
  const rules = new Map()
  for (let index = 0; index < plugin.rules.length; index += 1) {
    const rule = plugin.rules[index]
    const declared = manifest.rules[index]
    fields(rule, ['protocol', 'id', 'meta', 'create'], 'runtime rule')
    validateMeta(rule.meta)
    if (rule.protocol !== PROTOCOL || rule.id !== declared.id || typeof rule.create !== 'function'
      || rule.meta.description !== declared.meta.description || rule.meta.category !== declared.meta.category
      || rule.meta.fixable !== declared.meta.fixable) {
      fail(`${manifest.name} runtime rule differs from its manifest: ${declared.id}`)
    }
    rules.set(rule.id, { rule, declared })
  }
  return rules
}

function sourceBoundaries(source) {
  const boundaries = new Set([0])
  let offset = 0
  for (const character of source) {
    offset += Buffer.byteLength(character, 'utf8')
    boundaries.add(offset)
  }
  return boundaries
}

function validateDiagnostic(diagnostic, id, declared, boundaries) {
  object(diagnostic, 'diagnostic')
  for (const key of Object.keys(diagnostic)) {
    if (!['message', 'messageId', 'start', 'end', 'fix'].includes(key)) fail(`unknown diagnostic field: ${key}`)
  }
  if (Object.hasOwn(diagnostic, 'fix')) fail(`${id} diagnostic fix is not permitted`)
  const message = string(diagnostic.message, `${id} diagnostic.message`)
  const { start, end } = diagnostic
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || end < start
    || !boundaries.has(start) || !boundaries.has(end)) {
    fail(`${id} diagnostic range must use UTF-8 character boundaries`)
  }
  const messageId = string(diagnostic.messageId, `${id} diagnostic.messageId`, 256)
  if (!declared.messageIds.includes(messageId)) fail(`${id} diagnostic uses an undeclared messageId`)
  return { id, message, messageId, start, end }
}

function compareText(left, right) {
  return left < right ? -1 : left > right ? 1 : 0
}

async function run(request, packages) {
  const filePath = string(request.path, 'path')
  if (!LANGUAGES.has(request.language)) fail('unsupported source language')
  if (typeof request.source !== 'string' || Buffer.byteLength(request.source, 'utf8') > MAX_SOURCE_BYTES) fail('source exceeds its size budget')
  if (!Array.isArray(request.rules) || request.rules.length > MAX_RULES) fail('rules must be a bounded array')
  const boundaries = sourceBoundaries(request.source)
  const declaredRules = new Map()
  for (const entry of packages) {
    for (const declared of entry.manifest.rules) {
      if (declaredRules.has(declared.id)) fail(`duplicate extension rule id: ${declared.id}`)
      declaredRules.set(declared.id, { entry, declared })
    }
  }
  const requested = new Set()
  const activePackages = new Set()
  for (const setting of request.rules) {
    fields(setting, ['id', 'level', 'options'], 'rule setting')
    if (typeof setting.id !== 'string' || requested.has(setting.id)) fail(`invalid or duplicate requested rule: ${setting.id}`)
    requested.add(setting.id)
    if (!['off', 'warn', 'error'].includes(setting.level)) fail(`${setting.id} has an invalid level`)
    object(setting.options, `${setting.id} options`)
    if (Object.keys(setting.options).length !== 0) fail(`${setting.id} options are unsupported by the manifest`)
    const selected = declaredRules.get(setting.id)
    if (!selected) fail(`undeclared extension rule: ${setting.id}`)
    if (setting.level !== 'off') activePackages.add(selected.entry.name)
  }
  const runtimeRules = new Map()
  for (const entry of packages) {
    if (!activePackages.has(entry.name)) continue
    const imported = await import(pathToFileURL(entry.modulePath).href)
    const plugin = imported.default ?? imported.plugin ?? imported
    for (const [id, value] of validateRuntime(plugin, entry.manifest)) runtimeRules.set(id, value)
  }
  const diagnostics = []
  for (const setting of request.rules) {
    if (setting.level === 'off') continue
    const selected = runtimeRules.get(setting.id)
    const context = Object.freeze({
      source: request.source,
      path: filePath,
      language: request.language,
      syntax: Object.freeze({}),
      options: Object.freeze({}),
      report(diagnostic) {
        if (diagnostics.length >= MAX_DIAGNOSTICS) fail('extension diagnostic count exceeds its budget')
        diagnostics.push(validateDiagnostic(diagnostic, setting.id, selected.declared, boundaries))
      },
    })
    const returned = await selected.rule.create(context)
    if (Array.isArray(returned)) for (const diagnostic of returned) context.report(diagnostic)
  }
  const rootPackage = object(parseJson(jsonFile(path.join(request.root, 'package.json'), MAX_MANIFEST_BYTES, 'root package.json'), 'root package.json'), 'root package.json')
  for (const entry of packages) {
    if (moduleHash(entry.modulePath, `${entry.name} module`) !== entry.moduleHash
      || !samePackageIdentity(entry, resolvePackage(request.root, rootPackage, entry.name))) {
      fail(`${entry.name} identity changed while extension rules ran`)
    }
  }
  diagnostics.sort((a, b) => a.start - b.start || a.end - b.end || compareText(a.id, b.id)
    || compareText(a.message, b.message) || compareText(a.messageId ?? '', b.messageId ?? ''))
  return { ok: true, diagnostics }
}

async function main() {
  const input = readFile(0)
  if (input.length > MAX_REQUEST_BYTES) fail('extension request exceeds its size budget')
  const request = object(parseJson(input.toString('utf8'), 'extension request'), 'extension request')
  if (typeof request.nonce !== 'string' || !/^[0-9a-fA-F]{32}$/.test(request.nonce)) fail('invalid extension request nonce')
  responseNonce = request.nonce
  if (request.mode !== 'manifest' && request.mode !== 'run') fail('unsupported extension request mode')
  const root = string(request.root, 'root')
  if (!path.isAbsolute(root)) fail('extension root must be absolute')
  validatePackages(request, request.mode)
  const rootPackage = object(parseJson(jsonFile(path.join(root, 'package.json'), MAX_MANIFEST_BYTES, 'root package.json'), 'root package.json'), 'root package.json')
  const packages = request.packages.map((value) => {
    const name = request.mode === 'manifest' ? value : value.name
    const resolved = resolvePackage(root, rootPackage, name)
    if (request.mode === 'run' && !samePackageIdentity(resolved, value)) {
      fail(`${name} package identity changed since configuration was resolved`)
    }
    return resolved
  })
  return request.mode === 'manifest' ? { ok: true, packages } : run(request, packages)
}

main().then((result) => {
  const envelope = createRecord(null)
  envelope.ok = result.ok
  if (result.packages !== undefined) envelope.packages = result.packages
  if (result.diagnostics !== undefined) envelope.diagnostics = result.diagnostics
  envelope.nonce = responseNonce
  const response = encodeResponse(envelope)
  if (utf8Length(response, 'utf8') > MAX_RESPONSE_BYTES) fail('extension response exceeds its size budget')
  writeResponse(1, `${response}\n`)
}).catch((error) => {
  const message = error instanceof Error ? error.message : String(error)
  const envelope = createRecord(null)
  envelope.ok = false
  envelope.error = message.slice(0, 4096)
  envelope.nonce = responseNonce
  writeResponse(1, `${encodeResponse(envelope)}\n`)
})
