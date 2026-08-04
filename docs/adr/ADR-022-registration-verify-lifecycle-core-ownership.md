# ADR-022: Registration verify-lifecycle is core-owned — one provider per bot, tracked always verified

**Status: accepted (architecture).** Recorded during the verify-lifecycle
grilling for epic `Z5CNPB` (task `IKGQ6F`), settling the D4 decision **D4
(lifecycle)** and its dependencies: who owns the per-pool registration
verify-lifecycle, what the verify provider seam is, and what "no pool becomes
solvable on unverified state" means. Built on the feasibility verdict in
`docs/migration-guides/verify-lifecycle-core-ownership.md` (spike `A4YORC`).

## Context

The registration verify-lifecycle — the per-pool
`set_quarantined → verify seed (RPC) → drain+pin → verify post-drain (RPC) →
set_live` sequence plus its block-resolution + config-gating policy — lived in
Python (`src/degenbot/arbitrage/engine_registry.py::register_v3/v4_pool`) as an
async choreography over four Rust primitives. Two of the three D4 semantics were
already core-side by construction (coverage-aware `registration_lifecycle` on the
state structs; `set_*_pool_quarantined` already a Sparse no-op; the coverage_
aware orphan sweep), so the undecided core was *who owns the async
choreography*, *what provider the verify RPC uses*, and *what happens when
verify config is absent*.

Investigation surfaced three facts that drove the decisions:

1. `AlloyProvider` is `Arc<dyn Provider<Ethereum>>` with a manual `Clone`
   (`Arc::clone`) — cheap to clone, shares the transport.
2. Core pool/`BotState` structs are deliberately I/O-free (ADR-001) — they must
   not hold a provider.
3. ADR-021's `solver_state_verifier` is a **solve-time** scalar-state tripwire
   with a whole-bot shutdown reaction — no registration analogue. The word
   "tripwire" was being overloaded across the two.

## Decision

### D1 — The verify-lifecycle choreography is core-owned, in `bot_core/registration_lifecycle.rs`.

A sibling of `liquidity_verifier.rs` / `snapshot_verify.rs` (state-hygiene
concern per ADR-003), NOT inside `pool_builder` (construction) and NOT inside
the sync `register_v3/v4_pool` (the pool is already registered by `build_pool`;
the lifecycle is a post-registration orchestration). It is a **runtime-agnostic
async fn** that interleaves `&mut core` transitions
(`set_quarantined → drain+pin → set_live`) with lock-free verify RPC, preserving
the rolling-start invariants: drain+pin as a single `core.write()` hold; no
guard across the RPC `.await` (take-pin-then-drop); step-1 verifies the pinned
snapshot seed @ snapshot block; step-2 verifies the pin's own captured block.
Python `register_v3/v4_pool` becomes a thin delegating shell.

### D2 (D-A) — The registration tripwire is the verification mismatch; ADR-021's solver verifier is out of registration scope.

"State tripwire as the final gate" before `Live` means: the verification
`MismatchError` is raised as a typed `VerificationMismatchError` at the terminal
step so `Live` is unreachable on unverified state — never auto-repair. ADR-021's
`solver_state_verifier::verify_solver_hop_states` is a distinct, solve-time,
per-hop scalar diff with a whole-bot shutdown reaction; it has no registration
analogue and is explicitly NOT part of this lifecycle. The two "tripwires" must
not be conflated.

### D3 (D-B) — One provider per bot/chain; the verify RPC reuses the bot's single `AlloyProvider`.

All operations on a chain (construction, verify, pump) share the bot's one
`AlloyProvider`. The core lifecycle receives a **clone passed-in** as
`&AlloyProvider` from the outer owner (engine/Bot) — never stored on `BotState`
(ADR-001 I/O-free pools keep the provider off core state). The separate
`verify_rpc_url`/`verify_provider`/`set_verify_rpc_url` plumbing is **retired**
as a deliberate simplification: verifying against a node distinct from
construction is no longer supported (one node per bot/chain). The `state_view`
contract address stays a chain-scoped value (the V4 `eth_call` target).

### D4 (D-C) — There is NO "verify disabled" mode for tracked: tracked pools are always verified.

Because D3 makes the verify provider always present, "verify config absent"
reduces to a missing V4 `state_view` address (V3 per-pool verify reads
`pool.ticks()` directly). The core lifecycle always requires verify for tracked
(sparse skips) and raises a typed error if a V4 tracked pool needs `state_view`
and it's absent. **Enforced in core** so a standalone Rust consumer gets the
same guarantee (AGENTS.md standalone-Rust-core constraint), with Python
`start()` surfacing the missing-V4-target condition early as a loud failure.
No vacuous-pass (the prior Python behavior), no silent permanent quarantine.

### D5 — Sparse pools stay `Live`, unverified and un-quarantined (DFQYM5, unchanged).

A Sparse pool is immediately `Live`, receives no verification deferral, and no
verify RPC is invoked. The lifecycle's sparse branch asserts the RPC is not
called, not merely that the pool ends `Live`.

### D6 — Tracked pools need a production producer (cross-task, 4GQWZ4).

The Rust `PoolBuilder` (`3FVZF4`) is chain-arm/Sparse-only; the only Tracked
producer today is the Python builders' DB-arm (`PyBot.assemble_*_tick_map`),
retired by `4GQWZ4`. The tracked lifecycle must not become dead code: `4GQWZ4`
must wire the DB-arm full-tick-map assembly into the Rust `PoolBuilder` (DB-hit
→ `coverage=Tracked` → Quarantined → two-step verify → Live), falling back to
chain-arm/Sparse only on a DB miss.

## Sequencing / boundary

- This ADR scopes the **core lifecycle** (runtime-agnostic). Driving
  registration from the pump's single tokio runtime — the single-runtime
  unification — is Part 2 (`AF6OCC`/`6VZN7H`), which spawns this cron-driven
  lifecycle on the pump runtime and reuses the one `AlloyProvider`.
- `release_all_v3_v4_quarantined` remains only an orphan sweep (pools built but
  whose path never registered); per-path release is the productivity gate
  (`WSLCD2`).
- ADR-001 (I/O-free pools), ADR-003 (Bot owns state; verify is state hygiene),
  ADR-005 (three-layer), ADR-006 D4 (registry as orchestration), ADR-021
  (solve-time tripwire) all preserved.

## Implementation notes (landed, IKGQ6F)

Refinements made while implementing that sharpen (not contradict) the decisions
above:

1. **Sparse still drains; it only skips verify (no RPC).** "No verification
   deferral / no RPC" for Sparse is not "no drain": a Sparse pool must still
   apply backfill/pump events that were buffered while it was unregistered
   (the perm-V2-V2-V3 class). The lifecycle's Sparse branch drains
   (`apply_*_buffer`) but quarantines/verifies/pins nothing, and never invokes
   a verify closure.

2. **Provider is closure-resolved, not pre-gated (D-C scoped to where it
   matters).** The concrete adapters take `Option<&AlloyProvider>` (the bot's
   single provider, passed-in); `MissingProvider`/`MissingStateView` fire only
   when a **Tracked** pool actually reaches a verify step. A Sparse /
   unregistered / no-pin no-op never needs one, so a fresh
   `EngineRegistry(bot=bot)` that has not run `start()` can still register such
   pools — the fail-fast is only for the unverifiable-tracked case it protects.

3. **Read-guard deadlock (test-caught):** acquiring `core.write()` inside a
   `match` arm while the match-scrutinee `core.read()` temporary is still alive
   deadlocks parking_lot (read-held-then-write). Bind the coverage to a `let`
   so the read guard drops before the write.

The offline Python drain-reproduction test moved Rust-side
(`tracked_v3_lifecycle_drains_buffered_backfill`) because tracked registration
now requires live on-chain verification (D-C always-verify); the Python seam
test asserts the D-C fail-fast instead. The `verify_rpc_url` field is retained
as the single provider source until the full D-B field-retire (a follow-up).
