import { createWasmBuildContext } from './browser-wasm.mjs'

let context
self.onmessage = async event => {
  const request = event.data
  try {
    if (request.operation === 'initialize') {
      context = await createWasmBuildContext(request.wasmUrl, request.options)
      self.postMessage({ id: request.id, ok: true })
      return
    }
    if (request.operation !== 'build' || context === undefined) throw new Error('Wake browser Worker is not initialized')
    const started = performance.now()
    const files = Object.fromEntries(Object.entries(request.files).map(([path, bytes]) => [path, Array.from(bytes)]))
    const output = context.build(files)
    output.durationMs = performance.now() - started
    self.postMessage({ id: request.id, ok: true, value: output }, output.files.map(file => file.bytes.buffer))
  } catch (error) {
    self.postMessage({ id: request.id, ok: false, error: {
      code: error.code ?? 'WAKE_INTERNAL', message: error.message ?? String(error), diagnostics: error.diagnostics ?? [],
    } })
  }
}
