# YT3QOU result — HostedSources lifetime: engine-owned, deterministic head-watch teardown

## What shipped (v3 spec)

- **Ownership (criterion 1/2b).** `hosted_sources.rs`: `HostedSources` owns the
  shutdown `watch`, a head-edge completion flag, the head-watch WS transport Arc
  (`head_ws_provider`), and a **dedicated single-worker runtime**
  (`pubsub_runtime`). `strategy.rs`: `BootedHost.hosted: Option<HostedSources>`;
  `boot_hosted_arms` returns ownership (streams taken first, so no `&mut`).
  `register.rs`/`engine/mod.rs`: `PyArbEngine` stores it and owns the lifetime
  via an explicit `Drop` that takes the hosted sources first.
- **Deterministic teardown (criterion 2a/2c).** `build_pubsub_runtime` mirrors
  `AnvilFork::pubsub_runtime` (single worker, `enable_all`) and registers the
  worker thread in the `worker_census` (`hosted_pubsub_workers` /
  `degenbot-hosted-pubsub`). The head-watch WS connect and the
  `HeadWatch::subscribe` driving task run on that runtime
  (`head_watch.rs::subscribe` now takes a `spawn_on: Handle`; only caller is
  `hosted_sources.rs`). `Drop for HostedSources`: stop pumps -> fire the
  graceful `watch` signal -> drop the WS frontend -> tear the runtime down with
  the `AnvilFork` off-async split (`block_on` grace off-async; sleep + detached
  drop inside async).
- **Graceful loops (criterion 2d).** The head edge (`tokio::select!` against the
  stop signal) and the feed sampler both select on the `watch`; the head edge
  drops its transport clone and flips `head_edge_stopped` on exit.
- **Guard tests (criterion 3).** `hosted_sources.rs`: capture subscriber
  (alloy_pubsub-only) mirrors `degenbot-fork` node.rs; `dropping_the_owner_*`
  assert no `Reconnection attempt` / `Pubsub service backend error` post-drop
  plus the positive clean-shutdown flag; `a_dead_endpoint_ws_connect_leaks_*`
  pins criterion 2e.

## Criterion 2e empirical finding

The join provider takes the same `build_provider` WS path. A WS connect to a
dead endpoint FAILS at `WsConnect::connect` (eager `connect_async_with_config`)
BEFORE `PubSubService::connect` spawns its service (`alloy-pubsub` 2.5.0
`service.rs`: `connector.connect().await?` first, then `this.spawn()`), so a dead
endpoint leaks NOTHING — no dedicated-runtime reroute is needed for the join
provider. Pinned by `a_dead_endpoint_ws_connect_leaks_no_pubsub_service`.

## Gates (scoped, foreground)

- `cargo test --locked -p degenbot-bot` — OK (744 passed, 0 failed).
- `cargo test --locked -p degenbot-rpc` — OK (226 passed, 0 failed).
- `cargo test --locked -p degenbot_rs` — OK (165 passed, 0 failed).
- `rustfmt --check` on the 5 changed files — clean.
- `cargo clippy -p degenbot-bot -p degenbot-rpc -p degenbot_rs --all-targets` — clean.
- Negative probe: disabling the head-edge stop arm trips BOTH guard tests red;
  restored -> green.

## PENDING

- `uv run --no-sync pytest -q --no-header tests/test_rust_logging_bridge.py
  tests/arbitrage/test_engine_fake_parity.py` — NOT RUN: no Python-visible
  surface change (private field + internal teardown only), and it needs the
  rebuilt extension (`just dev`), which is the PM's ladder.
