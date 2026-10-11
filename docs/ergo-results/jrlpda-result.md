# JRLPDA result — OwnedPubsubRuntime extraction

## What shipped

- `rust/crates/foundation/degenbot-core/src/pubsub_runtime.rs` (NEW): the ONE home of the dedicated-runtime teardown discipline. `OwnedPubsubRuntime::build(OwnedPubsubRuntimeSpec{resource,kind,thread_name,sizing,binding})` builds a single-worker multi-thread runtime with a named thread, census-registers, and counts live instances; `.handle()`/`.spawn()` expose the runtime; `Drop` implements the AnvilFork split verbatim (off-async `block_on` grace / inside-async sleep + detached drop, 2ms `SHUTDOWN_GRACE`).
- **Census live-count fix**: `worker_census::register` upserts by resource, so N owners each registering a fixed count left the census frozen at the last writer's value. The type keeps a process-global per-resource counter (`BTreeMap` behind a `parking_lot::Mutex`), +1 on build / saturating -1 on Drop, and re-publishes the CURRENT count on every change — the census snapshot now reports the true fleet size at dump time.
- Migrations: `AnvilFork` (degenbot-fork/node.rs, census row `fork_pubsub_workers`) and `HostedSources` (degenbot-bot/hosted_sources.rs, `hosted_pubsub_workers`) now build via the shared type; both private `PUBSUB_SHUTDOWN_GRACE` constants and both hand-rolled Drop splits are deleted.
- Zero assertion changes in the guard tests (PM-verified via diff).

## PM review

- Cold diff: verbatim discipline consolidation; both call sites delegate; the duplicated constants and splits removed.
- Gates re-run by PM: degenbot-core / degenbot-fork / degenbot-bot all green; scoped clippy clean; fmt fixed in-review (the new test module + the node.rs migration); comment-hygiene green.
- Negative probe (PM-run): leaking the shared runtime (`std::mem::forget`) trips the fork guard `leaked_provider_clone_emits_no_reconnect_storm` RED — the shared type's Drop is the cancellation mechanism for the real-backend site. The bot guard stays green by design (its dead-loopback endpoint means no service exists to storm; its red lever is the head-edge clean-shutdown flag, proven red-capable in the chunk-B review).
- Known limitation carried from chunk B, not a regression: the bot-side storm guard cannot observe runtime cancellation without a live backend (a live-backend variant would need an AnvilFork-backed endpoint — noted as future work, not blocking).
