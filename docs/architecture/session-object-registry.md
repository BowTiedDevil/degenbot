# Session object registry — design note

**Status: contract frozen, nothing implemented.** This note records the *target* agreed for
epic `3CYYH3` (task `ZL2JKC`) before any interface work. It deliberately specifies no Rust
trait, no Python method, and no FFI signature — those are separate tasks. Terminology is
the settled set in [CONTEXT.md § Session objects](../CONTEXT.md#session-objects); this note
is the reasoning behind it.

## The decision, in one paragraph

There should be **one session-scoped object registry** holding the pools, tokens, and paths
of a session, keyed by canonical identity, with a single get-or-create semantic. Every
consumer — the Python `Bot` facade, the engine, each strategy — asks that registry for an
object and gets the one canonical entry back; consumers hold borrowed references and never
register through a private path. The registry is deliberately shallow: it owns **identity
only**. It performs no chain or database I/O, runs no solver, and decides nothing about
submission.

## Session scope

Scoped to **one session** — the lifetime of one `Bot` on one chain, per
[ADR-006](../adr/ADR-006-bot-as-per-chain-orchestrator.md) D5 ("one `Bot` per chain").
Consequences that follow from that scope and are settled here:

- **Identity is session-local.** A canonical key is meaningful only inside its session; it
  is not a cross-process or cross-run identifier, and two sessions may key the same pool
  differently without either being wrong.
- **It sits below the process.** It is *not* the process-level governance surface.
  [ADR-057](../adr/ADR-057-strategy-host.md) keeps `StrategyHost` as the per-process
  owner of shared strategy services (hub, route registry, nonce authority); the object
  registry is a per-session structure that the host may reach, never a host competitor.
- **It is chain-scoped, not multi-chain.** The chain discriminator stays part of the
  canonical key only for as long as the transitional Python `Bot` layer is in play (see
  *Migration order*).

## First-cut object kinds

| Kind | Canonical identity | Live state today | First-cut rationale |
|---|---|---|---|
| **Pool** | family + chain + pool address (V4 adds `PoolManager` + `PoolId`) | `BotState` | The dominant kind; four separate owners exist today. |
| **Token** | chain + address | `BotState` token entry | Pools reference tokens; identity is already address-keyed in every owner. |
| **Path** | ordered `(pool, direction)` hop signature | none — `PathRegistry` is already identity-only | Cheapest to fold: the current owner already holds *only* identity, so it is the natural first proof. |
| **Position** | — | — | **Explicitly deferred.** Named as the next kind, not designed here. |

The first cut is deliberately the set for which a canonical key is *already* unambiguous
in every existing owner. No kind is admitted whose key construction is still contested.

## Canonical identity vs. live state

This is the load-bearing distinction of the design, and it is the one the current code
mostly already observes.

- **Identity** is stable for the life of the session: which pool this is, which token,
  which route. It is what the registry owns, and get-or-create is the only way to grow it.
- **Live state** is the mutable, block-advanced copy of on-chain state. It stays with its
  existing owner.

The registry therefore does **not** absorb `BotState`. It answers "which pool is this?" and
hands back a reference; reading or advancing the live state remains a separate concern,
exactly as [ADR-045](../adr/ADR-045-solve-cycle-extraction.md) already split
`ArbitrageEngine` into `registry: PathRegistry` (identity) + `cycle: SolveCycle` (behavior).
`PathRegistry` is the precedent this design generalizes: an identity-only registry whose
mutating surface is registration and whose read surface is shared.

The two failure modes this rule exists to prevent:

1. **Absorbing live state**, which would make the registry a second `BotState` and put two
   writers on reorg-rollback journals ([ADR-003](../adr/ADR-003-botcore-state-layer.md),
   [ADR-014](../adr/ADR-014-pool-state-deepening-layer.md)).
2. **Re-deriving identity per consumer** — precisely the drift ADR-061 documented when
   four tick-map staging implementations each carried a private notion of "the same V3
   pool" ([ADR-061](../adr/ADR-061-pool-ingress-plane-capability.md), Context).

## Relationship to Bot, EngineDriver, and StrategyHost

- **`Bot` (Python)** is a transitional session facade that today acts as factory, registry,
  and I/O boundary at once. The object registry is the part of that role which is *not*
  I/O. Post-migration `Bot` remains the I/O boundary and the public surface (ADR-005
  layer 3); it stops being an identity owner.
- **`EngineDriver`** ([ADR-049](../adr/ADR-049-engine-stage-driver-seam.md),
  [ADR-050](../adr/ADR-050-rust-native-engine-driver.md)) is the engine's single public
  seam. It *consumes* the object registry; it does not own it. The engine's path identity
  moves under the registry, but the engine keeps admission, solve, and per-event reorg
  coordination.
- **`StrategyHost`** stays process-level governance (ADR-057): it registers and drives
  drivers and owns the nonce authority. Strategies attached to the host borrow object
  references from the session registry.

## Non-I/O scope (what the registry must never own)

Stated as prohibitions, because each is a place a registry-shaped struct is tempted to
grow:

- **No chain or database I/O.** No provider, no RPC call, no DB handle, no tick fetch.
  Pool-state provisioning stays with `PoolIngress` (ADR-061 D1) and its `Db → Chain`
  precedence.
- **No solver behavior.** No resolve, no solve, no envelope. `PathRegistry`'s
  "deliberately holds no resolve or solve state" is the inherited rule.
- **No submission policy.** No nonce, no relay posture, no fees — those are
  `StrategyHost` / `NonceAuthority` (ADR-057).
- **No per-consumer caches.** It is not a warm cache for a strategy's private use; a
  consumer that wants a cache caches *its own* derived values, not objects.

## Current owners and what replaces or coordinates with each

| Current owner | What it holds today | Disposition |
|---|---|---|
| `BotState` (`degenbot-bot::bot_core`) | Live pool/token state keyed by `pool_id`; reorg journals | **Coordinated, not replaced.** Stays the live-state owner. Its `pool_id` space becomes the canonical identity the registry keys on, and the registry never becomes a second writer. |
| Python `PoolRegistry` (`degenbot/registry/pool.py`) | Python pool objects by `(chain_id, address)`, plus a V4 `ManagedPoolRegistry`; propagates removal to `BotState` (ADR-007) | **Replaced** as the identity owner. It is already a partial mirror of `BotState` — the same mirror class ADR-003 retired for the legacy solver path. |
| Python `TokenRegistry` (`degenbot/registry/token.py`) | Token objects by `(chain_id, address)` | **Replaced** as the identity owner. |
| `EngineRegistry` (`degenbot/arbitrage/engine_registry.py`) key maps | `_v2_keys` / `_v3_keys` / `_v4_keys`: Python address → Rust `pool_id` mirrors | **Replaced.** This map *is* the cross-layer identity join the registry absorbs. |
| `EngineRegistry` verify claims | `_v3_inflight` / `_v4_inflight` + `VerifyClaims` — at-most-once claims closing a check-then-act TOCTOU | **Replaced in meaning.** The at-most-once invariant is *kept* — the registration verify lifecycle is core-owned per [ADR-022](../adr/ADR-022-registration-verify-lifecycle-core-ownership.md) — but it becomes a consequence of get-or-create rather than a parallel claim table. This is the subtlest row: the claim machinery carries an invariant, not a cache, and must not simply be deleted. |
| `PathRegistry` (`degenbot-bot::arb_engine::path_registry.rs`) | Path identity: paths, reverse index, signatures, id allocator, cap | **Folded in** as the path kind. Already identity-only, so this is the lowest-risk move. |
| `StrategyKit` / `MarketContext` (`degenbot-strategy`) | Per-strategy boot-resolved composition and process-lifetime caches (connector index, DFS graph, token joins, warm code cache) | **Coordinated.** These are composition and cache surfaces, not object stores (ADR-061 D3). They keep the cache role but stop owning object identity — the token id/address joins are a per-strategy copy of token identity. |

## Migration order

Sequenced so each step is independently shippable and no step strands an owner:

1. **Path kind.** Fold `PathRegistry` in first. It is already identity-only with a
   registration-only mutating surface, so it validates the get-or-create contract with no
   live-state entanglement.
2. **Pool + token identity in the Rust core.** Introduce the registry beside `BotState`,
   keyed by the identity `BotState` already uses. `BotState` keeps live state and becomes
   a consumer of the registry; the `EngineRegistry` key maps are then provable mirrors of
   a single source.
3. **Retire the `EngineRegistry` key maps and the Python `PoolRegistry` / `TokenRegistry`
   identity roles.** Registration claims are re-expressed as get-or-create (ADR-022's
   at-most-once invariant preserved), then the claim tables go.
4. **Repoint `StrategyKit` / `MarketContext`.** Replace the per-strategy token joins with
   borrowed references; caches remain, identity does not.
5. **Position kind**, once its key construction is settled.

Each step follows the repository's stated posture: parallel implementation behind a
feature flag where needed, then a hard cutover, with no permanent backwards-compatibility
layer.

## Open questions for supervisor review

1. **V4 canonical key.** Is the pool key `(chain, PoolManager, PoolId)` while
   `PoolManager` stays reserved as the V4 contract role, or does the first cut defer V4
   pools? The Python side already special-cases V4 by `PoolId` hex, and `PoolManager`
   cannot serve as a key component name without being re-read as the registry.
2. **Key scope across steps 2 and 3.** If Rust keys are family+chain+address but Python
   keeps `chain_id` in its key for one more release, is a temporary dual-key state
   acceptable, or must step 2 land the chain discriminator immediately?
3. **Engine `pool_id` provenance.** The engine currently *receives* ids through
   `EngineRegistry`'s mirrors. Does the registry allocate the id space, or does `BotState`
   keep the allocator and the registry only index it? ADR-045's `PathRegistry` allocates
   its own ids, which argues for the former; `pool_id` is load-bearing in `BotState`'s
   journal and undo surface, which argues for the latter.
4. **Position kind** key construction, deliberately left open.
