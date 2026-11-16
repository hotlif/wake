/** Standalone preview worker. Existing application workers should import the handler module instead. */
import { cleanupBrowserPreviews, handleBrowserPreviewFetch, handleBrowserPreviewMessage } from './browser-service-worker-handler.mjs'

self.addEventListener('install', event => event.waitUntil(self.skipWaiting()))
self.addEventListener('activate', event => event.waitUntil((async () => {
  await cleanupBrowserPreviews()
  await self.clients.claim()
})()))
self.addEventListener('message', handleBrowserPreviewMessage)
self.addEventListener('fetch', handleBrowserPreviewFetch)
