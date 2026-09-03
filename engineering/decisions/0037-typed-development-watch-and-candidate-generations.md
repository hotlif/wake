# ADR 0037: Typed development watches and isolated candidate generations

- Status: accepted
- Date: 2026-09-02

## Context

Development and `build --watch` previously inferred ownership from a directory plus a private file
extension list. Extra watch roots could replace the default `src` ownership, public files without a
recognized extension were invisible, and controls such as an external `wake.config.toml` or
`.browserslistrc` were confused with source files. Resolver aliases, component scans, Federation
exposes, and a configured entry may also live outside `src` or the project root.

A retained `BuildSession` copied resolve, define, target, JSX, entry, Docs, and Federation inputs at
construction. Re-reading source files without reconstructing that immutable plan silently retained
old configuration. Reconstructing eagerly was also unsafe: virtual entries and generated scan,
Docs, and Federation modules were written into fixed `.wake` paths before a candidate build had
succeeded. A bad candidate could therefore mutate the accepted session's inputs even when the last
published browser generation remained visible.

## Decision

1. `wake_app` owns project discovery, the exact configuration source, control classification,
   derived compile plans, candidate generations, and the accepted/pending/blocked refresh state.
   `wake_dev_server` owns filesystem event routing, watcher registrations, mount session lifetime,
   and atomic publication of bundle, HTML, and Federation state. The dependency remains
   `wake_app -> wake_dev_server`; the server never loads project configuration.
2. Watch ownership is a typed `WatchInterest`: an exact file or a tree with either source-loader
   filtering or all-file filtering. Exact controls bypass extension filtering and match only their
   declared/resolved identity, never a global basename. Tree registrations use the nearest existing
   ancestor and structural create/remove events match both descendants and a missing interest's
   ancestors.
3. The project plan is the union of the default `src` tree (or root fallback), configured extra
   roots, the logical user entry, public all-file tree, root `index.html`, exact config/browser
   controls, project package/install markers, resolver-equivalent nearest ancestor PnP/install
   witnesses (including an external explicit entry's discovery chain), component-scan roots,
   configured alias targets, and Federation expose or resolved runtime source targets. Generated
   `.wake` inputs do not become source or control owners. The implicit configuration-discovery
   chain is a permanent floor: it remains in the retained `BuildContext` plan through preliminary
   candidates, failed candidates, successful commits, and bootstrap handoff, so creating a closer
   project marker is always a topology invalidation.
4. A control event reloads the exact configuration source and derives a complete immutable compile
   plan. Compile-only changes, including aliases, defines, browser target/transforms, and JSX import
   source, create a candidate `BuildSession`. The candidate plan, session, and visible generation
   commit only after a successful build. Failure retains the last published generation and keeps
   candidate interests so a missing or broken new source can trigger a retry.
5. Project/config root, logical entry, output directory for build watch, public URL or Docs mount
   topology, component-scan topology, development server/proxy settings, Docs source/preview/theme,
   Federation topology, and Federation lock changes are restart-required. They report
   `WAKE_DEV_RESTART_REQUIRED`; they never continue with a mixed old/new topology.
6. Invalid configuration and restart-required state are sticky control blockers. Ordinary source
   events do not erase their diagnostic or publish a misleading successful generation. A later
   control event replaces the blocker; once configuration/topology is valid, candidate source
   interests remain retryable until compilation succeeds.
7. A lazy Docs workspace is represented by a typed deferred mount: immutable URL ownership, a pure
   probe, preliminary interests, and a refresh policy, but no placeholder compile plan. Startup
   watcher registration and its authoritative Rescan may replace the pending probe without
   allocating a generation or `BuildSession`. The first request materializes and builds only the
   requested workspace. A failed first build retains retry interests and does not commit mixed Docs
   callback state or HTML.
8. Every long-lived dev/watch plan owns an RAII candidate generation. Generated module identities
   remain stable logical paths under `<project>/.wake`, while a leased projected filesystem maps
   them to an isolated physical tree. The filesystem view itself owns the lease, reverses directory
   entries to logical identities, and hides the staging namespace. One-shot production keeps stable
   logical output behavior. `.wake` is a reserved source namespace.
9. `BuildContext` is the application-owned retained-build implementation. Its strict
   `BuildContext::create` constructor remains eager for manual and Node callers, whose synchronous
   contract has no watcher-registration capability. CLI watch startup uses the separate
   `BuildWatchBootstrap` transaction described below; frontends consume application-owned plans
   rather than process cwd or another extension table.
10. Watch invalidation is a closed sum: `Paths` carries observed identities and `Rescan` means that
    watcher coverage was absent or cannot be trusted. A Rescan rereads authoritative configuration,
    supersedes an uncommitted candidate when its probe identity changed, regenerates derived inputs
    whose source set may have changed, and fully invalidates the accepted retained session. A
    same-identity failed candidate may retain its materialized session for retry. Rescan is never
    encoded as an empty path list inside the invalidation pipeline. Watch routing and cache
    invalidation retain both declared and resolved identities when they differ. The public
    `RebuildStart.changedPaths` observation projects those identities to canonical, sorted,
    deduplicated paths; an authoritative Rescan is observable there as an empty list, but that
    presentation value never re-enters the pipeline as `Paths([])`.
11. `BuildContext` publishes one atomic `WatchPlanSnapshot` containing its root, typed interests,
    and a monotonic revision. Interests changing is the only operation which advances the revision.
    `build --watch` installs a complete revision before its first build, performs a Rescan, and
    repeats install-then-Rescan whenever the post-build revision differs. This closes both startup
    and configuration-widening windows. A coverage capability attests one live watcher generation,
    the exact plan root and revision, and a superset of its interests; a snapshot from another
    root, revision, or backend generation cannot authorize work. After a successful commit, a
    same-root, monotonically newer context plan may shrink an installed superset as cleanup without
    scheduling a duplicate build.
12. Confirmed watcher registrations, not the desired plan, are the registration state. Additions
    and recursive promotions precede cleanup. Failed additions remain absent, failed rollbacks do
    not resurrect a fictional registration, and failed obsolete cleanup remains recorded as
    degraded over-coverage. Runtime registration failures retry with bounded backoff; backend loss
    reconstructs the watcher and a successful coverage recovery always causes a Rescan. Each
    watcher generation owns a cancellation token. Its callback cancels that token and atomically
    revokes the generation before enqueueing a backend diagnostic. Application output is written
    to staging and takes the same token's commit gate after the ownership marker is complete, so
    cancellation and visible-tree replacement have one linear order. A retired callback cannot
    cancel or diagnose its successor generation. The development server applies the same rule to
    mount publication: every backend generation owns a lease and commit gate; `Error` and
    backend-requested Rescan revoke it synchronously before enqueue, and every queued notification
    carries its generation. Bundle/HTML/Federation installation, reload and diagnostics events,
    worker session adoption, and candidate completion share the final publication gate. Backend
    loss rejects that transition as retryable; shutdown rejects it as aborted. Lazy readiness is
    not itself claimed as part of that gate: after a successful gated transaction returns, its
    exact `Building(epoch) -> Loaded` completion is the final release to HTTP waiters. Because the
    publication linearized first, a later backend loss or stop cannot roll that accepted result
    back; a rejected gate never reaches the readiness release.
13. A source-tree interest rejects Wake's generated/staging namespaces without rejecting similar
    user names such as `.wakeful`. A writing `BuildContext` additionally excludes its owned output
    tree from every tree interest, including the project-root fallback, so publication cannot feed
    the watch loop back into itself.
14. Refresh is a move-only candidate transaction. It exposes preliminary interests separately from
    a one-shot materializer and a one-shot completion. The only terminal outcomes are `Committed`,
    `RetryableFailure`, `Superseded`, and `Aborted`; dropping an unfinished candidate emits
    `Aborted` exactly once. A cloneable boolean callback is not a valid transaction capability.
15. Candidate processing is ordered: pure probe and preliminary coverage declaration, confirmed
    watcher registration, side-effecting materialization, refined coverage registration, build,
    then commit. Effective coverage is accepted union every uncommitted or rejected candidate and
    may shrink only after commit. Registration, probe, materialization, and build failures retain
    over-coverage and pending/blocked evidence. Watched `BuildContext` calls carry the exact
    installed `WatchPlanRevision`; stale, future, or newly widened revisions stop before reads,
    materialization, build, or publication. Manual callers pass no watcher capability and retain
    their explicit-invalidation contract.
16. A watcher backend recovery Rescan is a load fence. Lazy readiness has one mutex-owned state
    machine—`Pending`, `Queued(epoch)`, `Building(epoch)`, `Loaded`, `Failed`, or `Stopped`—and a
    `watch::Sender<()>` used only to notify asynchronous HTTP waiters; phase payload is never copied
    into the notification channel. A waiter subscribes before
    its first state read; only the first waiter in `Pending` creates a `MountLoadTicket { index,
    epoch }`, and the worker must atomically claim that exact queued epoch. Duplicate, stale, and
    overflowed epochs cannot authorize work or complete a newer attempt. Recovery preserves a
    queued ticket, moves an in-flight build back to `Pending`, and allows only the next epoch to
    retry after complete registration plus the mandatory Rescan. Rescan may replace the pending
    candidate without overwriting `Queued` or `Building`. The gated publication installs visible
    state, adopts the worker session, and completes the candidate; only after that gate returns
    success does exact completion of the matching attempt release waiters as `Loaded`. Failure,
    backend loss, and stop likewise complete only their matching epoch. A wakeable stop signal and
    worker finalization release every asynchronous waiter without blocking an Actix worker thread.
17. `build --watch` starts through a typed, probe-only `BuildWatchBootstrap`. Its public state is
    `Waiting { plan, error }`, `Activatable { plan }`, or terminal `Activated { plan }`. Recoverable
    disk facts—including invalid/missing configuration, root, or entry—retain a registration plan
    without allocating `.wake`, an output, or a session. `activate_at(revision)` rejects stale
    capability before materialization, reprobes the control snapshot, materializes once, and fences
    any refined widening before constructing `BuildContext` from that already prepared generation.
    A bootstrap cannot activate twice. CLI retains the bootstrap/context interest union through
    the first successful publication; removing bootstrap-only coverage is cleanup-only and does
    not schedule a duplicate build. Waiting startup and topology restart remain inside the command
    loop rather than terminating the process. Backend loss clears every capability and requires
    reconstruction plus Rescan before activation or rebuild. None of this changes the eager,
    caller-managed invalidation contract of manual or Node `BuildContext::create`.

## Invariants

- Adding an extra root never removes the default source, entry, public, or control interests.
- A nested file merely named `wake.config.toml` cannot reload another project's configuration.
- Public extensionless files and every loader-supported source/asset extension can trigger the
  intended refresh.
- An unsuccessful candidate cannot change the accepted session, logical module identity, visible
  bundle/HTML/Federation generation, or the bytes observed through the accepted projected view.
- Random physical generation paths never enter resolver aliases, source maps, module identities,
  cache keys, or Federation build identity.
- Lazy mounts and eager mounts use the same accept/retry/restart policy; laziness changes only when
  candidate materialization and the session are allocated. An unrequested lazy workspace performs
  zero generated writes and owns no accepted compile plan.
- A candidate completion is consumed once, and every candidate replacement, failure, shutdown, or
  drop has an explicit terminal outcome.
- Accepted plus pending/rejected coverage never narrows because a probe, materializer, build, or
  backend registration failed.
- No build reads inputs until the corresponding watch-plan revision has complete confirmed
  coverage. Any interval without complete coverage is closed by a subsequent Rescan.
- A backend-loss or shutdown watermark visible before the final development publication gate
  prevents mount mutation, reload/diagnostic broadcast, accepted-session replacement, and a
  `Committed` candidate outcome; stale notifications from that generation cannot affect its
  successor.
- Watched startup creates no generation, session, cache, or output before bootstrap coverage is
  confirmed. Manual `BuildContext::create` remains eager, and a consumed bootstrap cannot create a
  second retained context.
- A queued lazy load cannot pass a pending recovery Rescan. At most one exact-epoch ticket owns an
  attempt, and no asynchronous mount waiter survives server stop or worker termination.
- Recursive coverage satisfies a non-recursive request for the same backend path and is never
  downgraded merely to match a weaker desired mode.
- An owned output or Wake staging write cannot match a source interest and trigger a self-rebuild.
- One physical input appears at most once in public `RebuildStart.changedPaths`, including Windows
  verbatim-prefix aliases; internal routing and invalidation still retain every required declared
  and resolved identity. An empty public list identifies Rescan observation, not an ordinary file
  batch or an internal `Paths([])` invalidation.

## Evidence

- `crates/wake_dev_server/src/lib.rs`: typed interests, nearest-ancestor registration, explicit
  Rescan, truthful backend state with bounded recovery, per-mount routing/reconciliation,
  move-only candidate transactions, typed deferred mounts, recovery load fences, asynchronous
  readiness notification, exact-epoch load tickets, wakeable shutdown, lazy retry,
  generation-tagged notifications, backend publication leases, and atomic visible generation.
  Its event projection preserves internal declared/resolved invalidation identities while exposing
  canonical deduplicated changed paths; the Windows verbatim-alias test locks that boundary.
  Deterministic unit tests hold the commit gate across revocation, verify retryable/aborted
  exactly-once completion and unchanged prior publication, prove a successor generation can
  publish while stale notifications are ignored, and exercise a check-to-await race, 32 concurrent
  waiters sharing one ticket, duplicate/stale epochs, queued/building recovery, loader disconnect,
  epoch overflow, and shutdown wakeup.
- `crates/wake_app/src/lib.rs`: exact controls, complete project watch derivation, topology policy,
  probe-owned fixed-size control snapshots, accepted/pending/blocked refresh state, deferred Docs
  workspace assembly, probe-only `BuildWatchBootstrap`, permanent discovery-floor propagation,
  cancellation-linearized staged publication, `BuildContext`, and leased logical candidate plans.
- `crates/wake_common/src/fs.rs`: typed logical-to-physical projection, reverse directory mapping,
  hidden staging namespace, and filesystem-owned lease capability.
- `crates/wake_app/src/federation.rs` and `crates/wake_docs/src/lib.rs`: physical candidate
  materialization returned as stable logical paths, with Docs generated-directory containment and
  manifest validation.
- `crates/wake_cli/src/main.rs`: CLI build watch installs bootstrap coverage before activation,
  hands off through the bootstrap/context union, and consumes revisioned
  `BuildContext::watch_plan()` snapshots through install-then-Rescan instead of maintaining cwd and
  extension heuristics. Backend-generation capabilities and generation-owned cancellation prevent
  queued or in-flight work from publishing across watcher loss; unit tests cover wrong roots,
  wrong revisions, retired generations, monotonic shrink, and delayed stale diagnostics.

## Consequences

Configuration refresh is more expensive than source invalidation because it derives and validates a
new immutable plan and may build a fresh retained session. That cost buys a clear commit boundary.
Candidate physical trees live for as long as any projected filesystem/session clone can observe
them and are reclaimed when the final lease is dropped.

Watcher recovery may temporarily retain obsolete over-coverage when the backend refuses cleanup;
that state is diagnosed and retried, but it does not block a build whose desired coverage is
complete. Deeper platform filesystem race/case/reparse hardening remains separate enforcement work
and must preserve this ownership and commit protocol rather than exposing physical candidate paths.
Bundler materialization and compilation remain synchronous and are not preempted mid-call; stop
prevents their later publication and wakes HTTP waiters, but a hard close deadline for a blocked
third-party filesystem or compiler call is a separate scheduling concern.

## Validation

- `cargo +1.95.0 test -p wake_common --lib`
- `cargo +1.95.0 test -p wake_dev_server --lib`
- `cargo +1.95.0 test -p wake_docs --lib`
- `cargo +1.95.0 test -p wake_app --lib`
- `cargo +1.95.0 test -p wake_cli --bin wake`
- `cargo +1.95.0 test -p wake_cli --test cli_output`
- `cargo +1.95.0 check -p wake_common -p wake_docs -p wake_dev_server -p wake_app -p wake_cli`
- `cargo +1.95.0 clippy -p wake_common -p wake_docs -p wake_dev_server -p wake_app -p wake_cli --all-targets -- -D warnings`
- `cargo +1.95.0 fmt --all -- --check`
- `corepack yarn architecture:test`
- `corepack yarn architecture:check`
- `corepack yarn docs:check`
- `git diff --check`

## Supersedes

None.

## Removal plan

Remove directory-only/cwd watch roots, frontend extension allowlists, basename control matching,
fixed development generated inputs, and in-place mutation of accepted compile options. No fallback
may silently retain a changed configuration field or expose candidate physical paths.
