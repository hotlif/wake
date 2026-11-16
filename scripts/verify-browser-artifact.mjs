import { readFileSync } from 'node:fs'

/** A browser compiler must instantiate without filesystem, network, clock, or thread imports. */
export function verifyBrowserArtifact(path) {
  const bytes = readFileSync(path)
  if (bytes.length > 10 * 1024 * 1024) throw new Error('Wake browser WASM exceeds its 10 MiB unpacked budget')
  const module = new WebAssembly.Module(bytes)
  if (WebAssembly.Module.imports(module).length !== 0) throw new Error('Wake browser WASM must not require host imports')
  const { exports } = new WebAssembly.Instance(module, {})
  if (!(exports.memory instanceof WebAssembly.Memory) ||
      typeof exports.wake_browser_abi_version !== 'function' || exports.wake_browser_abi_version() !== 1 ||
      ['wake_browser_alloc', 'wake_browser_call', 'wake_browser_result_len'].some(key => typeof exports[key] !== 'function')) {
    throw new Error('Wake browser WASM ABI is incompatible with its packaged adapter')
  }
}
