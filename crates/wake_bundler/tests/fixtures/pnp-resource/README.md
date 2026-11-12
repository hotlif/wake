# PnP archive replacement fixtures

`fixture.rs` generates two deterministic STORE archives containing a package manifest,
an `index.js` that publishes its revision and a static URL to `payload.data`, and
the payload (`payload-v1` / `payload-v2`). They test replacement of the *same*
physical archive path while a build session retains module and archive caches.

No binary archive is checked in. These are small synthetic test inputs shared by
bundler and app watch tests; the real Monaco package is tested separately by
`fixtures/monaco-worker-yarn-pnp`.
