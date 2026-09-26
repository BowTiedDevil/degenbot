# Session object registry — design note

**Status: pools, tokens, and paths implemented; the position seam is declared and the
bot boot binds a reader (see *Who installs it, and where*); the Python/FFI
cutover is outstanding.** This note records the *target* agreed for epic `3CYYH3`
(task `ZL2JKC`) before any interface work. It deliberately specifies no Python method
and no FFI signature — those are separate tasks. Terminology is the settled set in
[CONTEXT.md § Session objects](../CONTEXT.md#session-objects); this note is the
reasoning behind it.

**The path kind landed as a reach, not a move.** `PathRegistry` could not be
relocated without inverting the layering, so it stayed where it was and the
registry reached it: `bot_core::session_registry::PathObjectAdapter` is the
registry's side of the boundary, implemented once in
`arb_engine::path_objects::EnginePathObjects`. The session registry holds no path
map, and the engine keeps the id space, the dedup index, and the cap. Every other
row below that says "the registry owns it" is a claim about the registry struct;
the path row is a claim about a handle.

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
| **Path** | ordered `(pool, direction)` hop signature over validated session pool objects, plus the id the path-identity owner allocated | none — `PathRegistry` is already identity-only | Cheapest to fold: the current owner already holds *only* identity, so it is the natural first proof. A path is SHARED across strategies: strategies borrow the canonical object and derive their own plan, so solver, dispatch, and submission stay out of it. |
| **Position** | chain + market (pool contract) + account | none — a reading is a fresh projection, not an entry | **Identity only, and the value is NOT an object.** The key is settled; the value decays while a caller holds it, so the registry names the position and reaches the value through an observer that can refuse. See *The position kind*. |

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

## The position kind: an identity, and a read that refuses

A position is the one candidate whose **value decays**, so it is not a canonical
session object and get-or-create is the wrong verb for it: constructing one on a
failed read would fabricate a position, and caching one would hide its age from a
strategy that is about to act on it. What the session owns is the position's
**identity** — `(chain, market, account)`, stamped with the session's own chain —
and nothing else. The **reading** comes from a `PositionObserver` the session holds
as a handle, and the seam is collection-free on both sides: no position map, no
cached value, no second authority that could disagree with the lending integration
about what a position is.

The caller states the freshness it requires (`Freshness::Any` / `AtOrAfter` /
`AtMost`) and the refusal vocabulary distinguishes the ways a read can fail: no
observer installed, an identity for another chain, an unserved market, an account
with no position, a retryable transport fault, rows that cannot be interpreted, and
an observation too old for the caller's requirement. Only the transport fault is
retryable. The freshness check is the **session's**, so a lax observer cannot pass a
decaying snapshot through as current, and no failure path returns a value.

**Where the seam lives, and why.** The trait, the identity, the refusal vocabulary
and the reading are declared in `degenbot-core`
(`degenbot_core::session_positions`), not in the engine's session registry. The
observer is implemented by a lending integration (`degenbot-aave`), which must not
depend on the engine, and the engine must not depend on a lending integration.
`degenbot-core` is the one layer both already depend on, so the seam sits there and
**no new dependency edge is added in either direction** — a property the
architecture gates assert mechanically. What the re-export facade and the `PyO3`
shell may hold is not the adapter but the act of composing one: a shell is a
composition root, so binding an integration's reader to a session is assembly, and
assembly is what a binding layer is for. The reading itself stays in the integration.

**Who installs it, and where.** The install is one line,
`install_position_observer` on the session registry, and the SITE it belongs to
differs from the path owner's for a structural reason rather than by accident:

| | Path owner | Position observer |
|---|---|---|
| The value lives | in the **engine** (`arb_engine::PathRegistry`) | in the **database** the Aave updater maintains |
| Who holds it | the engine driver, from construction | the bot boot, which opened the connection |
| Bound at | `EngineDriver::assemble`, beside the session | the bot boot's database-attach step |

The path owner is INSIDE the engine, so the composition point that already holds both
the session and that owner — driver assembly — binds it, and has since the path kind
landed. The position reader's backing is database state whose *connection lifetime* the
bot boot owns, and no engine component opens it: the engine performs no database I/O
at all (see *Non-I/O scope*). So the bot boot is the composition root that holds both
halves, and it binds there. The seam's TYPES stay in `degenbot-core`; only the two
binding lines sit in a shell, and they are delegation — the boot constructs the
integration's reader from a connection it already owns and hands it to the session.
Which rows answer a read, how fresh they are, and which faults refuse are the
reader's own, implemented and tested in `degenbot-aave`.

**The census, and why the seam splits across layers.** Walking every manifest's
transitive closure, exactly three crates can name both the engine's registry and a
lending reader: the re-export facade `degenbot`, the `PyO3` shell `degenbot-python`,
and `degenbot-cli` — and the CLI never builds a session (it takes only telemetry and
stance config), so the real set is the facade and the binding shell. That census is
what forces the seam to cross a layer boundary at all: the two halves are in crates
that are forbidden from depending on each other, so nothing in either domain layer can
compose them. Given that split, each remaining surface is settled rather than open:

- **The Python-driven bot binds it, in `degenbot-python`.** The binding shell is that
  bot's composition root, so it is where the install belongs — at the step where the
  boot opens the session's database, because that is where the session and the
  database handle are both in hand.
- **A pure-Rust consumer installs its own, in its own `main`.** The umbrella has no
  Aave updater, hence no database handle and no session of its own to bind against,
  so there is nothing there to wire and no wiring layer should be invented for it. A
  consumer that runs a lending updater composes its own session and installs its own
  reader in two lines — the same two lines, at its own composition point.
  `rust/crates/facade/degenbot/tests/session_position_reachability.rs` is that
  consumer path, written out.

Because the install can now arrive both from the bot boot and from a consumer's own
code, a **second** install is a hazard the session can genuinely see, so the registry
refuses it AND reports it at ERROR rather than leaving the diagnostic to a call site
that cannot be assumed to exist. The observer is handed back so a caller can release
it, and the bot boot — which installs unconditionally — has no branch of its own to
get wrong.

## Relationship to Bot, EngineDriver, and StrategyHost

- **`Bot` (Python)** is a transitional session facade that today acts as factory, registry,
  and I/O boundary at once. The object registry is the part of that role which is *not*
  I/O. Post-migration `Bot` remains the I/O boundary and the public surface (ADR-005
  layer 3); it stops being an identity owner.
- **`EngineDriver`** ([ADR-049](../adr/ADR-049-engine-stage-driver-seam.md),
  [ADR-050](../adr/ADR-050-rust-native-engine-driver.md)) is the engine's single public
  seam. It *consumes* the object registry; it does not own it. The engine's path identity
  moves under the registry, but the engine keeps admission, solve, and per-event reorg
  coordination. Driver composition is where the path owner is BOUND: `assemble` — the one
  point every driver constructor (`new`, `from_stages`, `from_stages_with_hub`) reaches
  holding both the session `Bot` and its `EngineStages` — installs the engine's path
  registry as the session's path-identity owner, so a booted session answers path asks
  rather than refusing them for want of one. A second driver over one session is a second
  path-id space, so the install is once-per-session and a second one is reported rather
  than absorbed.
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
| `PathRegistry` (`degenbot-bot::arb_engine::path_registry.rs`) | Path identity: paths, reverse index, signatures, id allocator, cap | **Reached, not moved** as the path kind: it stays the single owner of the id space and the dedup/cap state, and the session asks for a canonical path object through `PathObjectAdapter`. Already identity-only, so this is the lowest-risk step; relocating the struct would have made the lower layer name engine machinery. |
| `StrategyKit` / `MarketContext` (`degenbot-strategy`) | Per-strategy boot-resolved composition and process-lifetime caches (connector index, DFS graph, token joins, warm code cache) | **Coordinated.** These are composition and cache surfaces, not object stores (ADR-061 D3). They keep the cache role but stop owning object identity — the token id/address joins are a per-strategy copy of token identity. |

## Migration order

Sequenced so each step is independently shippable and no step strands an owner:

1. **Path kind.** Reach `PathRegistry` rather than fold it — it is already identity-only
   with a registration-only mutating surface, so it validates the get-or-create contract
   with no live-state entanglement, and an adapter keeps the one owner and the layer order
   intact. Landed.
2. **Pool + token identity in the Rust core.** Introduce the registry beside `BotState`,
   keyed by the identity `BotState` already uses. `BotState` keeps live state and becomes
   a consumer of the registry; the `EngineRegistry` key maps are then provable mirrors of
   a single source.
3. **Retire the `EngineRegistry` key maps and the Python `PoolRegistry` / `TokenRegistry`
   identity roles.** Registration claims are re-expressed as get-or-create (ADR-022's
   at-most-once invariant preserved), then the claim tables go.
4. **Repoint `StrategyKit` / `MarketContext`.** Replace the per-strategy token joins with
   borrowed references; caches remain, identity does not.
5. **Position kind.** Landed. The identity is settled, the seam is declared
   (`degenbot_core::session_positions`, reached from the session registry's
   `position` submodule, implemented by `degenbot-aave`'s observer), and the bot boot
   binds the reader where it opens the session's database — the composition point
   that holds both halves, in the same role `EngineDriver::assemble` plays for the
   path owner. What remains is the user-facing Python/FFI surface, which is the same
   kind of step as pool/token identity above: the seam is a working boot path before
   it is a public API.

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
4. **Position kind** key construction — settled: `(chain, market, account)` with the
   market named by its pool contract, so the key stays family-agnostic.
