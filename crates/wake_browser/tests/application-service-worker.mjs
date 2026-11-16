import { cleanupBrowserPreviews, handleBrowserPreviewFetch, handleBrowserPreviewMessage } from '/browser-service-worker-handler.mjs'

// An existing application's lifecycle and unrelated fetch behavior remain application-owned.
self.addEventListener('install', event => event.waitUntil(self.skipWaiting()))
self.addEventListener('activate', event => event.waitUntil((async () => {
  await cleanupBrowserPreviews()
  await self.clients.claim()
})()))
self.addEventListener('message', event => { handleBrowserPreviewMessage(event) })
self.addEventListener('fetch', event => {
  if (handleBrowserPreviewFetch(event)) return
  if (new URL(event.request.url).pathname === '/application-owned') {
    event.respondWith(new Response('application handler'))
  }
})
