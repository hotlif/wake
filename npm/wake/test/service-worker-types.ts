import { handleBrowserPreviewFetch, handleBrowserPreviewMessage, cleanupBrowserPreviews } from '@crab-dev/wake/browser/service-worker-handler'

declare const worker: ServiceWorkerGlobalScope
worker.addEventListener('message', event => { handleBrowserPreviewMessage(event) })
worker.addEventListener('fetch', event => { handleBrowserPreviewFetch(event) })
worker.addEventListener('activate', event => { event.waitUntil(cleanupBrowserPreviews()) })
