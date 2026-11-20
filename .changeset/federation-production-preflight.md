---
"@crab-dev/wake": patch
---

Preflight locked Federation startup assets in parallel and share validation across isolated previews. Production remotes can opt into bounded byte verification with `assetPreflight: "verified-get"` for same-origin HTTPS assets. Native scripts and styles continue to enforce SRI.

Unmounting a Federation React bridge cancels its pending style attachment while keeping other previews' shared preflights active.
