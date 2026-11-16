/** Explicit handlers; importing the module never installs listeners or claims clients. */
export function handleBrowserPreviewMessage(event: {
  data: unknown
  source: unknown
  ports: readonly MessagePort[]
  waitUntil(promise: Promise<unknown>): void
}): boolean
/** Returns true when respondWith was called; otherwise the application's handler should run. */
export function handleBrowserPreviewFetch(event: {
  request: Request
  clientId: string
  respondWith(response: Response | Promise<Response>): void
}): boolean
export function cleanupBrowserPreviews(): Promise<void>
