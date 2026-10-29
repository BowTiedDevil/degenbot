# ADR-078: The family write door — one `PoolRegistration` request, one verdict, family as data

**Status: accepted (2026-10-16).** Drafted at the architecture-review grilling (candidate C3; rounds 1–4, Q5/Q13/Q14/Q21/Q27). Extends the ADR-073 family read door to the write side; builds on ADR-077 (the door's refusal arm is the refusal vocabulary).

> **Amendment (2026-10-29): the write door's additive hedge is consumed — pool identity has one owner.** D3 sequenced the door additively ("the substrate door lands
> additively; no existing entry is rewired onto it"), which left three duplicate
> policies standing: a typed `AlreadyRegistered` error on the families whose entry
> returns `Result`, an `assert!` panic on Curve and both Balancer arms, and — written
> only to keep that panic from firing through the door — the door's own
> `duplicate_address` pre-check. All three are gone.
> `degenbot_substrate::pool_table::PoolTable` now owns the entry table, the address
> index, the `(PoolManager, pool_id)` index, the id counter, the reorg journal depth,
> and the ADR-040 quarantine set (the shared `RegistryCore` view dissolves into
> `BotState.pools`, with the token table beside it), and it owns the operations that
> change identity:
> `allocate` (one monotonic id allocation and key binding in one step), `register`
> (the one duplicate refusal, answered in the shared `RegisterRefusal` vocabulary),
> and the one quarantine transition that bumps the pool's `state_nonce` in either
> direction. Every family arm constructs its entry through `from_params` and hands it
> to the table; the door dispatches and folds verdicts, and no longer pre-checks.
> Curve and both Balancer arms answer `RegisterRefusal` instead of panicking (their
> arity checks ride the same vocabulary as `SpecViolation`), so a duplicate can
> neither panic nor be decided twice. Two consequences follow: a duplicate is now
> detected after the winning family's construction rather than before it, and a typed
> arm's refusal `detail` stays that family's rendering (the field is log-only). The
> per-family tests that pinned each arm's own duplicate behaviour now pin the table's
> invariants once, over every family (`degenbot_substrate::tests::pool_table`). Read
> D1/D3 below against this note: the per-family *mechanics* stay behind the door, the
> *identity* does not.

## Context

GLOSSARY's "Pool construction" already names the intent: *the core's one entry from a requested pool to a constructed, registered pool*. The reality is a family×verb matrix hand-fanned in **three** places:

- **The PyO3 wrapper** (`degenbot-python/src/bot/mod.rs`, 3,801 lines): ~60 `#[pymethods]` — `register_v2_pool` / `build_v3_pool` / `register_curve_pool` / … — plus per-family payload helpers (`registered_v2_payload`, `registered_v3_payload`, `registered_v4_payload`, `registered_family_pool_id`) compiled into the binding. A new pool family lands as N new pymethods plus payload builders.
- **The core bot** (`bot_core/build_register.rs`): `build_and_register_v2`, `_aerodrome_v2`, `_balancer_weighted`, `_balancer_stable`, `_curve_pool`, `_v3`, `_v4` — the same matrix one seam down.
- **The pure-Rust consumer** (`examples/settlement_bot/src/live.rs`): hand-rolled `build_and_register` / `v2_params` mirroring the Python path.

Meanwhile the pieces of the door already exist, unassembled: the write-side `register_*` matrix sits in `degenbot-substrate`'s orchestration modules split by structural family axis (`reserve_pair_orchestration.rs`, `balance_vector_orchestration.rs`, `concentrated_liquidity_orchestration.rs`); the params types live in `degenbot-pools`; `pool_builder/route.rs` is already door-shaped *depth* for construction (get-or-register half, typed route walk, identity resolution, `AlreadyRegistered` race fold, refusal classification into `ConstructionRefusal`) with exactly one production caller (the V3 arm via Python's `construct_pool`) and tests-only V2/V4 arms; and `families/door.rs` demonstrates the one-door pattern on the read side — ONE `SwapReadRequest`, design bits riding as additive variants, never forks.

Three consumers (FFI, bot_core, settlement_bot) cross this seam, so the seam is real, not hypothetical.

## Decision

**D1 — ONE write door on `BotState`, in `degenbot-substrate`.** `register_pool(PoolRegistration) -> RegistrationVerdict`: one request enum whose per-family params (the existing `degenbot-pools` params types) ride as additive variants; one verdict speaking ADR-077's refusal vocabulary. The orchestration modules keep the per-family mechanics behind the door.

**D2 — The door absorbs the route layer's construction depth and generalizes it.** `construct` becomes a door verb serving every family: the get-or-register half, identity resolution, the race fold, and refusal classification move inside; `route.rs` becomes the door's construction interior. The tests-only V2/V4 arms become real door paths.

**D3 — Migration: door → bot_core → FFI.** The substrate door lands additively; the `bot_core` `Bot::register_*` / `build_and_register_*` fan-out converges onto it; the FFI collapses last — pymethods become verb dispatch translating Python kwargs into the request enum, the `registered_*_payload` helpers and `assemble_*_tick_map` move core-side, and the generated stub surface (`_ffi/__init__.pyi`) shrinks to the verbs. Per-family, byte-identical relocations (the ADR-071/073 playbook); hard cutover per phase.

**D4 — Journal verbs ride the same door.** `v2/v3_journal_len`, `discard_before_block`, `restore_before_block` stop being per-family methods and ride the family-as-data door.

## Consequences

A new pool family becomes one params variant and one family module — zero new pymethods, zero new errmap arms. The PyO3 wrapper returns to its glossary definition (translation, not co-implementation), and the pure-Rust consumer and the Python driver cross the same seam with the same vocabulary.
