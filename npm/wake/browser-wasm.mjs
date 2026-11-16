/** Private Worker-side adapter for the version-matched Wake WASM artifact. */
export async function createWasmBuildContext(wasmUrl, options) {
  const response = await fetch(wasmUrl, { credentials: 'same-origin', redirect: 'error' })
  if (!response.ok) throw new Error(`Wake WASM could not be loaded: ${response.status}`)
  const { instance } = await WebAssembly.instantiate(await response.arrayBuffer(), {})
  const api = instance.exports
  if (typeof api.wake_browser_abi_version !== 'function' || api.wake_browser_abi_version() !== 1 ||
      typeof api.wake_browser_alloc !== 'function' || typeof api.wake_browser_call !== 'function' ||
      typeof api.wake_browser_result_len !== 'function' || !(api.memory instanceof WebAssembly.Memory)) {
    throw new Error('Wake browser compiler ABI is incompatible')
  }
  const encoder = new TextEncoder()
  const decoder = new TextDecoder('utf-8', { fatal: true })
  let disposed = false
  function call(request) {
    if (disposed) throw Object.assign(new Error('Wake browser context is disposed'), { code: 'WAKE_CANCELLED' })
    const input = encoder.encode(JSON.stringify(request))
    const pointer = api.wake_browser_alloc(input.length)
    if (!pointer) throw Object.assign(new Error('Wake browser request exceeds its memory limit'), { code: 'WAKE_CONFIG' })
    new Uint8Array(api.memory.buffer, pointer, input.length).set(input)
    const resultPointer = api.wake_browser_call(pointer, input.length)
    const result = JSON.parse(decoder.decode(new Uint8Array(api.memory.buffer, resultPointer, api.wake_browser_result_len())))
    if (!result.ok) throw Object.assign(new Error(result.error.message), result.error)
    return result.value
  }
  call({ operation: 'initialize', options })
  return {
    build(files) {
      const value = call({ operation: 'build', files })
      for (const file of value.files) file.bytes = new Uint8Array(file.bytes)
      return value
    },
    dispose() {
      if (disposed) return
      call({ operation: 'dispose' })
      disposed = true
    },
  }
}
