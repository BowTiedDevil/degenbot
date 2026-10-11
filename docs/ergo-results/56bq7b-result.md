# 56BQ7B result — rpc_env holder governance + non-vacuous bot storm guard + helper promotion + start-contract docs

Agent: f1-rpc-env-and-storm-guard. Tree left dirty (no commit).

## Item 1 — `tests/helpers/rpc_env.py` governed (HIGHEST PRIORITY)

### Current-behavior demonstration (the trap was LIVE, not benign-today)

The chain-1 resolution path is HOLDER-FED: `ArbitrageConfig.build` delegates to
`degenbot.config.resolve_rpc_uris`, which reads the installed `resolved_config()`
verdict, and the holder is first-wins (installed once at FFI module init). A bare
`monkeypatch.setenv("DEGENBOT_RPC_*", ...)` after import therefore does NOT move
the holder.

Demonstration (guard run against the OLD helper, before the fix):

```
$ uv run --no-sync pytest tests/test_rpc_env_guard.py -q -p no:randomly
FF
E  AssertionError: assert 'ws://host.co...internal:8546' == 'ws://localhost:8546'
   - ws://localhost:8546
   + ws://host.containers.internal:8546
```

So the old helper's loopback override was a SILENT NO-OP and the ambient
snapshot (`ws://host.containers.internal:8546`, from the shell env at FFI init)
kept governing — exactly the "never falls back to ambient shell state" promise
broken. Benign only because the consumers never assert the URI and never connect;
trapped the moment any consumer depends on the loopback.

### What changed

- `tests/helpers/rpc_env.py`: `rpc_env(monkeypatch)` -> a governed
  `@contextmanager rpc_env()`. It saves `os.environ`, applies the loopback
  `DEGENBOT_RPC_HTTP_CHAINID_1` / `DEGENBOT_RPC_WS_CHAINID_1` overrides,
  re-seams the holder via `reinstall_config_holder_for_tests()`, and on exit
  restores the environment EXACTLY and re-governs the ambient snapshot — the
  same contract as `tests/conftest.py::governed_holder` (mirrors it; no consumer
  uses `governed_holder`, so there is no composition conflict).
- 7 consumers (`tests/arbitrage/test_diag_config.py`,
  `test_cockpit_session.py`, `test_cockpit_session_owner.py`,
  `test_cockpit_phase.py`, `test_engine_fake_parity.py`,
  `test_posture_driven_boot.py`, `test_registration_lifecycle_value.py`): their
  autouse `_rpc_env` fixture bodies change to
  `def _rpc_env(): with rpc_env(): yield` (call-shape only; no other body edits).
- NEW `tests/test_rpc_env_guard.py`: red-capable guard — inside `rpc_env()` the
  request + subscription scopes resolve the loopback WS twin via
  `_ffi.resolved_node_uri_for_tests` and `resolve_http_rpc_uri` /
  `resolve_ws_rpc_uri`; the holder-fed consumer step (`get_provider_from_config`)
  sees the override; and exit restores the ambient snapshot.

Deferred-start safety: construction dials nothing (E4 deferral), and the guard's
consumer step injects a recording provider factory, so a governed chain-1
endpoint causes ZERO eager I/O.

## Item 2 — non-vacuous bot-side storm guard (connection-count observable)

### What changed

`rust/crates/engine/degenbot-bot/src/hosted_sources.rs`:
- NEW test helper `accept_then_die_ws_server()`: a minimal WS server that serves
  the head watch's INITIAL connect (handshake + `eth_subscribe` reply, then
  closes) and FAILS every later connection (accept then drop, no handshake), so
  the alloy pubsub service spawns, the watch goes live, the backend dies, and the
  service's `reconnect_with_retries` loop is driven. It returns the ws URL and an
  accepted-connection counter.
- NEW guard `dropping_the_owner_mid_storm_leaves_no_new_pubsub_reconnect`:
  starts the owner against that server, waits for a SECOND accepted connection
  (non-vacuity: the reconnect loop is observed RUNNING), drops the owner mid-storm,
  then asserts NO further accepted connection lands.
- `rust/crates/engine/degenbot-bot/Cargo.toml` + `rust/Cargo.lock`: new
  dev-dependency `tokio-tungstenite ^0.30` (already in the lockfile via
  alloy-transport-ws / degenbot-rpc; the lock diff is one added edge).

### Why the accepted-connection count, not the capture subscriber

The sibling guard's capture subscriber is BEST-EFFORT: the process-wide
`tracing` subscriber slot is first-wins, and another test in the binary owns it
(the sibling's own doc says so). A first implementation asserted the storm via
`capture::events()`; it PASSED in isolation but FAILED in the full parallel suite
("vacuous guard: no pubsub reconnect observed") precisely because the capture
layer never installed. Counting accepted connections is independent of tracing,
so the guard is genuinely non-vacuous in the full binary. This is a deliberate
deviation from the design sketch's literal "capture subscriber" wording, keeping
its intent (observe the storm before the drop; no new reconnect after).

### Red-capability demonstration (the PM's E1 lever)

Neutralizing the `OwnedPubsubRuntime` teardown in `Drop for HostedSources`
(`std::mem::forget(self.pubsub_runtime.take())` instead of dropping it) leaves
the service's reconnect loop alive past the drop:

```
assertion `left == right` failed: dropping the owner mid-storm left 1 new reconnect connections
  left: 3
 right: 2
```

Restored -> green. The old dead-endpoint guard stays as the positive-half
(shutdown-flag) guard.

## Item 3 — promote `counting_listener`

`counting_listener` promoted to `pub(crate)` and the new
`accept_then_die_ws_server` / `serve_initial_connect` family added `pub(crate)`
beside it, matching the crate-local test-helper convention
(`arb_engine::tests::test_keys` — `pub(crate)` items in a `#[cfg(test)]` module).
No new machinery.

## Item 4 — `start` half-failure contract

Added an explicit "Half-failure contract" paragraph on BOTH sides, stating the
latch is consumed BEFORE launching, a failed `start` is NOT retryable, the owner
is left HALF-STARTED (fail-closed), and the caller MUST drop the engine (no retry
path by design):
- `HostedSources::start` docs (`hosted_sources.rs`).
- `start_hosted_sources` error-path docs
  (`rust/crates/shells/degenbot-python/src/bot/engine/strategy.rs`).

## Gates run (scoped)

- `uv run --no-sync pytest <7 consumers> tests/test_rpc_env_guard.py tests/test_config_holder_seam.py` -> **69 passed**.
- `uv run --no-sync pytest tests/test_config_rpc.py` -> **23 passed**.
- `cargo test -p degenbot-bot` -> **735 passed, 0 failed** (3 ignored).
- `cargo clippy -p degenbot-bot --tests` -> clean.
- `cargo check -p degenbot_rs` -> clean; `just dev` rebuilt + installed the
  extension (needed to run pytest after the Rust rebuilds).
- `ruff check` on the helper + guard + registration file -> clean.
- `cargo fmt -p degenbot-bot -p degenbot_rs` applied.

Not run (dispatcher owns): whole-workspace ladders (`just test-rust-nextest`,
`just test-python`, `just test-rust`), whole-tree lint.

## Foreign / pre-existing notes

- `tests/arbitrage/test_cockpit_phase.py` and `test_posture_driven_boot.py`
  carry PRE-EXISTING ruff errors in unchanged regions (unused
  `FakeEngine` import, `async` `_empty`, trailing whitespace, typing-only
  `BotRunner` import, two generator-dicts); not introduced here. `cockpit_phase`'s
  import block was re-sorted as a side effect of the isort pass (latent unsorted
  import at HEAD) — cosmetic.
- No foreign-dirty files observed or touched.

## Red-capability summary

- Item 1 guard: neutralizing the `reinstall_config_holder_for_tests()` call in
  `rpc_env` -> all 4 guard tests red (holder falls back to ambient).
- Item 2 guard: neutralizing the `OwnedPubsubRuntime` teardown -> red (new
  reconnect connection after the drop).
