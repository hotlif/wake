import { mkdir, rm, writeFile } from 'node:fs/promises'
import { spawn } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

const root = resolve(import.meta.dirname, '..')
const fixture = join(tmpdir(), `wake-lint-lsp-${process.pid}`)
const binary = join(root, 'target', 'debug', `wake-lint-language-server${process.platform === 'win32' ? '.exe' : ''}`)
const source = 'const x = <C></C>;'

await rm(fixture, { recursive: true, force: true })
await mkdir(fixture, { recursive: true })
await writeFile(
  join(fixture, 'wake.config.toml'),
  '[lint]\nrecommended=false\n[lint.rules]\n"react/self-closing-comp"="error"\n',
)

const file = join(fixture, 'input.tsx')
const uri = pathToFileURL(file).href
const child = spawn(binary, [], { stdio: ['pipe', 'pipe', 'inherit'] })
let buffer = Buffer.alloc(0)
const messages = []
child.stdout.on('data', (chunk) => {
  buffer = Buffer.concat([buffer, chunk])
  while (true) {
    const separator = buffer.indexOf(Buffer.from('\r\n\r\n'))
    if (separator < 0) break
    const header = buffer.subarray(0, separator).toString('ascii')
    const length = Number(header.match(/Content-Length:\s*(\d+)/i)?.[1])
    if (!Number.isInteger(length)) throw new Error(`invalid LSP header: ${header}`)
    const bodyStart = separator + 4
    if (buffer.length < bodyStart + length) break
    messages.push(JSON.parse(buffer.subarray(bodyStart, bodyStart + length).toString('utf8')))
    buffer = buffer.subarray(bodyStart + length)
  }
})

const waitFor = async (predicate) => {
  const deadline = Date.now() + 15_000
  while (Date.now() < deadline) {
    const message = messages.find(predicate)
    if (message) return message
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 20))
  }
  throw new Error(`timed out waiting for LSP message: ${JSON.stringify(messages)}`)
}

const send = (message) => {
  const body = Buffer.from(JSON.stringify(message))
  child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`)
  child.stdin.write(body)
}

try {
  send({
    jsonrpc: '2.0',
    id: 1,
    method: 'initialize',
    params: { capabilities: {}, workspaceFolders: [{ uri: pathToFileURL(fixture).href, name: 'smoke' }] },
  })
  await waitFor((message) => message.id === 1 && message.result)
  send({ jsonrpc: '2.0', method: 'initialized', params: {} })
  send({
    jsonrpc: '2.0',
    method: 'textDocument/didOpen',
    params: { textDocument: { uri, languageId: 'typescriptreact', version: 1, text: source } },
  })
  const diagnostics = await waitFor((message) => message.method === 'textDocument/publishDiagnostics')
  if (diagnostics.params.diagnostics.length !== 1) throw new Error('expected one JSX lint diagnostic')
  send({
    jsonrpc: '2.0',
    id: 2,
    method: 'textDocument/codeAction',
    params: {
      textDocument: { uri },
      range: { start: { line: 0, character: 10 }, end: { line: 0, character: 17 } },
      context: { diagnostics: [] },
    },
  })
  const actions = await waitFor((message) => message.id === 2 && message.result)
  const edit = actions.result?.[0]?.edit?.changes?.[uri]?.[0]
  if (edit?.newText !== '/>') throw new Error('expected a self-closing JSX quick-fix')
  send({ jsonrpc: '2.0', id: 3, method: 'shutdown', params: null })
  await waitFor((message) => message.id === 3)
  send({ jsonrpc: '2.0', method: 'exit', params: null })
  await new Promise((resolvePromise) => child.once('exit', resolvePromise))
  console.log('wake lint LSP protocol smoke ok')
} finally {
  child.kill()
  await rm(fixture, { recursive: true, force: true })
}
