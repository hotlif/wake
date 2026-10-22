# Independent Yarn PnP type consumer

`scripts/check-pnp-conformance.mjs` copies this fixture to a temporary project,
packs the current CSS npm source and `shared-config`, and installs them under
Yarn 4 strict PnP. It also packs a Wake type projection: the current package's
exact export map is retained, and each exported declaration is copied and
checked byte for byte. Runtime dependencies and platform binaries are omitted
from this type only installation. The consumer checks that TypeScript reads the shared
tsconfig from a zip archive and resolves Wake and CSS declaration entries
from the packed ZIP files.

The temporary project is separate from the source workspace so workspace
links and its root type declarations cannot satisfy these imports. Runtime
dependency installation is covered by the Components PnP gate.

The full type consumer starts with the repository Yarn lockfile to pin the
compiler and pnpify dependency graph. Yarn replaces its workspace entry with
this independent project's three packed local dependencies during install.

For offline diagnosis, `node scripts/check-pnp-conformance.mjs --local-entries-only`
installs just the three local tarballs and checks their ZIP paths and declaration
entries. The default gate also installs TypeScript and typechecks `consumer.ts`.
