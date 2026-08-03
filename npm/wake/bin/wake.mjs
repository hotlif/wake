#!/usr/bin/env node

import { readFile } from 'node:fs/promises'
import {
  build,
  buildDocs,
  startDevServer,
  startDocsDevServer,
  version,
} from '../index.mjs'
import { parse, tokenize } from '../experimental.mjs'

const HELP = `Wake ${version()}

Usage:
  wake build [entry] [--outdir DIR] [--cache] [--sourcemap]
  wake dev [root] [--entry FILE] [--host HOST] [--port PORT] [--open]
  wake docs build [root] [--outdir DIR] [--base PATH]
  wake docs dev [root] [--host HOST] [--port PORT] [--open]
  wake parse <file>
  wake tokenize <file>
  wake --version
`

function takeOption(args, name) {
  const index = args.indexOf(name)
  if (index === -1) return undefined
  if (index + 1 >= args.length) throw new Error(`${name} requires a value`)
  const [value] = args.splice(index + 1, 1)
  args.splice(index, 1)
  return value
}

function takeFlag(args, name) {
  const index = args.indexOf(name)
  if (index === -1) return false
  args.splice(index, 1)
  return true
}

function commonOptions(args) {
  return {
    configPath: takeOption(args, '--config'),
  }
}

function printResult(result) {
  const files = result.files?.length || 0
  console.log(
    `wake: built ${result.moduleCount} modules and ${files} files in ${result.durationMs.toFixed(1)}ms`,
  )
  if (result.outputDir) console.log(`wake: output ${result.outputDir}`)
}

async function runServer(factory, options) {
  const controller = new AbortController()
  const server = await factory({ ...options, signal: controller.signal })
  console.log(`wake: listening on ${server.url}`)

  let stopping = false
  const stop = async (signal) => {
    if (stopping) return
    stopping = true
    controller.abort()
    try {
      await server.close()
    } finally {
      process.exitCode = signal === 'SIGINT' ? 130 : 143
    }
  }
  process.once('SIGINT', () => void stop('SIGINT'))
  process.once('SIGTERM', () => void stop('SIGTERM'))
  await server.waitUntilClosed()
}

export async function runCli(argv = process.argv.slice(2)) {
  const args = [...argv]
  if (takeFlag(args, '--version') || takeFlag(args, '-V')) {
    console.log(version())
    return 0
  }
  if (args.length === 0 || takeFlag(args, '--help') || takeFlag(args, '-h')) {
    console.log(HELP)
    return 0
  }

  const command = args.shift()
  if (command === 'build') {
    const options = commonOptions(args)
    options.outdir = takeOption(args, '--outdir')
    options.cache = takeFlag(args, '--cache')
    options.sourceMap = takeFlag(args, '--sourcemap')
    if (args[0]) options.entry = args.shift()
    if (args.length) throw new Error(`unknown build arguments: ${args.join(' ')}`)
    printResult(await build(options))
    return 0
  }

  if (command === 'dev') {
    const options = commonOptions(args)
    options.entry = takeOption(args, '--entry')
    options.host = takeOption(args, '--host')
    const port = takeOption(args, '--port')
    if (port) options.port = Number(port)
    options.open = takeFlag(args, '--open')
    if (args[0]) options.cwd = args.shift()
    if (args.length) throw new Error(`unknown dev arguments: ${args.join(' ')}`)
    await runServer(startDevServer, options)
    return process.exitCode || 0
  }

  if (command === 'docs') {
    const action = args.shift()
    const options = commonOptions(args)
    options.outdir = takeOption(args, '--outdir')
    options.basePath = takeOption(args, '--base')
    options.host = takeOption(args, '--host')
    const port = takeOption(args, '--port')
    if (port) options.port = Number(port)
    options.open = takeFlag(args, '--open')
    if (args[0]) options.cwd = args.shift()
    if (args.length) throw new Error(`unknown docs arguments: ${args.join(' ')}`)
    if (action === 'build') {
      printResult(await buildDocs(options))
      return 0
    }
    if (action === 'dev') {
      await runServer(startDocsDevServer, options)
      return process.exitCode || 0
    }
    throw new Error('docs requires build or dev')
  }

  if (command === 'parse' || command === 'tokenize') {
    const file = args.shift()
    if (!file || args.length) throw new Error(`${command} requires one source file`)
    const source = await readFile(file, 'utf8')
    if (command === 'tokenize') {
      console.log(JSON.stringify(tokenize(source), null, 2))
    } else {
      const module = parse(source, {
        sourceType: file.endsWith('.cjs') ? 'script' : 'module',
      })
      try {
        console.log(JSON.stringify(module.summary, null, 2))
      } finally {
        module.dispose()
      }
    }
    return 0
  }

  throw new Error(`unknown command: ${command}`)
}

try {
  process.exitCode = await runCli()
} catch (error) {
  const code = error?.code ? `[${error.code}] ` : ''
  console.error(`wake: ${code}${error?.message || error}`)
  for (const diagnostic of error?.diagnostics || []) {
    console.error(`  ${diagnostic.severity}: ${diagnostic.message}`)
  }
  process.exitCode = 1
}
