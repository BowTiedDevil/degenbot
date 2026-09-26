# ADR-064: The session object registry is the cutover — identity is minted once, and the Python side is an adapter

**Status: accepted.** Records the completion of the session object registry
epic: what the registry is, the order the migration ran in, the two
structural decisions a future reader could mistake for mistakes, what
deliberately did NOT move into it, and what stays process-level.
Companion to the design note
[docs/architecture/session-object-registry.md](../architecture/session-object-registry.md),
which carries the reasoning for the target; predecessors ADR-006 D5 (one `Bot`
per chain, hence per-session identity), ADR-022 (registration verify lifecycle
is core-owned), ADR-045 (`PathRegistry` is the identity-only precedent this
generalizes), ADR-057 (`StrategyHost` stays process-level governance), and
ADR-059 (the pool family kernel).

## Context

Four separate owners each carried a private notion of "the same pool":
`BotState`, the Python `PoolRegistry` / `TokenRegistry`, the `EngineRegistry`
key maps (`_v2_keys` / `_v3_keys` / `_v4_keys`), and the per-strategy token
joins in `StrategyKit` / `MarketContext`. ADR-061 recorded the same drift
shape in the tick-map staging implementations, where four private notions of
"the same V3 pool" each had to be reconciled. The cost was not a bug but a
standing possibility: two owners can disagree about which route is which, and
nothing in the type system objects.

The fix is not a new map. It is to have exactly one place that answers "is
this the same object?", so that a second opinion is not expressible.

## Decision

**One session owns object identity, in `degenbot-bot::bot_core::session_registry`.**
Pools and tokens are typed `DashMap` + `Arc` entries behind a get-or-create
that inserts under the map's own shard lock. Paths and positions are
collection-free modules reached through a handle installed once, because for
those two kinds the identity owner already existed elsewhere or must not exist
at all.

The public Python interface is unchanged by this. What changed underneath is
that Python stopped *being* an identity owner and became an adapter: the
`PoolRegistry` / `TokenRegistry` and the `EngineRegistry` key maps are now thin
delegates that ask Rust to resolve a name and keep only the Python
presentation object. The key invariants that make that delegation honest are
recorded where they are enforced rather than restated here — an entry can only
exist for a key Rust minted, and no Python code re-derives a key from an
address, a chain id, or a family tag.

## Migration order, and why this order

Sequenced so each step was independently shippable and no step stranded an
owner.

1. **Path kind — reach, do not move.** `PathRegistry` could not be relocated
   without inverting the layering, so the registry reached it through
   `PathObjectAdapter`. It was already identity-only with a
   registration-only mutating surface, which made it the cheapest possible
   proof of the get-or-create contract: no live state to disentangle. The one
   owner and the layer order both survive.
2. **Pool and token identity in core.** The registry landed beside `BotState`,
   keyed by the identity `BotState` already used. `BotState` kept live state
   and became a consumer. Only once both kinds were core-owned could the
   Python maps be *shown* to be mirrors rather than argued to be mirrors.
3. **Retire the mirrors.** The `EngineRegistry` key maps and the verification
   claim tables went. This is the subtle step: the claims carried an
   invariant, not a cache. The at-most-once registration invariant is KEPT, but
   it stops being a parallel table and becomes a consequence of get-or-create —
   a racing second registration joins the first rather than duplicating it.
4. **Position kind.** Identity settled as `(chain, market, account)`; the
   reading is deliberately not an object (below).
5. **Cutover and gates.** The FFI construction choreography was deleted rather
   than left alongside a core path, and the architecture gates below were added
   so a second identity owner cannot quietly reappear.

Deleting before the gates existed would have been the mistake: the gates are
what make the deletion durable.

## Decision 1 — the two binding sites differ structurally, and that is not an inconsistency

The path owner is bound in `EngineDriver::assemble`. The position observer is
bound in the PyO3 bot boot, in `PyBot::attach_construction_io`, behind
`aave-updater`. Two different sites for the same kind of operation looks like
an oversight, and a future reader may "fix" it by unifying them.

The difference is that the two values have different owners. The path registry
lives INSIDE the engine, so the composition point that already holds both the
session and that owner — driver assembly — is the only place that can bind it.
The position reader is backed by database state whose connection lifetime the
boot owns, and no engine component opens a database at all; the engine performs
no database I/O. So the bot boot is the composition root that holds both
halves.

The two lines in the shell only construct an observer from a handle the boot
already owns and install it. The shell decides nothing about *reading*: which
rows answer, how fresh they are, and which faults refuse are the integration's
own, implemented and tested in `degenbot-aave`. Composition is what a binding
layer is for; the reading is not.

## Decision 2 — a position is not a canonical session object

A position is a fresh read-model projection. Only its identity is
session-canonical. The reason is that the value DECAYS: caching it would hide
its age from a strategy about to act on it, and constructing one on a failed
read would fabricate a position that does not exist.

The type makes fabrication impossible rather than merely discouraged.
`Freshness::accepts(Option<u64>)` takes the observed block as an `Option`, and
a never-advanced source satisfies nothing but `Any` — `AtOrAfter` and `AtMost`
both require a `Some` that compares, so a source that never advanced cannot
pass as current. There is no failure path that returns a value.

The position modules are therefore collection-free, and the architecture gates
assert it, because a map here would be exactly the cache the seam exists to
prevent.

## What stayed out, on purpose

- **Process-level arbitration.** `StrategyHost` remains the per-process owner
  of shared strategy services — the hub, the route registry, the nonce
  authority (ADR-057). The session registry is a per-session structure that the
  host may reach; it is never a host competitor, and it is not where
  cross-driver arbitration belongs. This is deferred deliberately, not
  overlooked.
- **`src/degenbot/runner/_registration_ledger.py` SURVIVES as an adapter.**
  It looks like superseded state and is not. The taxonomy is core-owned: the
  negative memos, the typed build-refusal classification, and the outcome
  vocabulary live in `degenbot_bot::bot_core::registration_ledger`, and the
  pure-Rust driver reads the same implementation, so a tag cannot drift between
  them. What the module adds is pure translation Python owns: the label enum is
  BUILT FROM the core's exported tag list rather than re-declared, and a Python
  exception TYPE is mapped onto a core failure kind. Do not sweep this away as
  dead code.
### The five Python-side address-keyed maps that are NOT session identity

They are collected here, in one place, because "a map keyed by an address is a
second identity owner" is the wrong question and the answer differs per map.
The decisive test is **minting**: can this map decide that two references are
the same object? A map that can only ever STORE an identity somebody else
minted is a memo, and a memo is not a second authority. The first two were
ruled on by earlier reviews; the third group was raised later, is ruled on here,
carries a limitation of its own, and is why all five are written down together.

1. **`src/degenbot/erc20/erc20.py` — `_cached_balance` /
   `_cached_approval` / `_cached_total_supply`.** Instance-scoped read caches
   bounded by a block depth (`state_cache_depth`, default 8), on a companion
   that is dropped and re-minted. They do not answer "is this the same token?"
   — the token identity is core-owned and reached through the handle — and a
   cache's lifetime is a block window, not a session. Migrating them would have
   moved a bounded read cache into an identity registry and given the registry
   a stale-value fallback it must not have.
2. **`src/degenbot/aave/analysis/orchestrator.py` — `price_map`.** A
   FUNCTION-LOCAL built per invocation from a per-call RPC fetch and discarded
   on return. It is a local of one analysis pass, not state at all.
3. **The per-DEX pool-tracker memos — `_tracked_pools` in
   `src/degenbot/types/abstract/pool_tracker.py` (the base that declares the
   field and `_add_tracked_pool`), `src/degenbot/uniswap/trackers.py`,
   `src/degenbot/curve/trackers.py`, and
   `src/degenbot/aerodrome/trackers.py`.** KEPT, deliberately.

   Why keeping them is correct. Each is keyed by checksummed pool address and
   is consulted only on a miss of the registry: `get_pool` checks
   `self._bot.pools.get(...)` — the Rust-backed registry — and the tracker's
   memo is written only from what that call returned. The memo is therefore
   incapable of minting: it cannot invent a pool object, and it cannot answer
   "are these the same pool?" for a pool the registry has never seen, because
   the lookup that fills it IS the registry. A memo of registry-issued handles
   is a derived view, not a competing id space, and deleting it would only move
   work without removing a class of bug.

   **Known limitation — the residual risk is real and is owned here, not
   forgotten.** A memo can outlive a companion the registry has dropped: the
   tracker's `_tracked_pools` is cleared only by
   `Bot.release_python_state` / `Bot.close` (`src/degenbot/bot_lifecycle.py`),
   and `remove()` pops one address, so between a registry-side eviction and the
   next teardown a tracker can hand back a pool object the registry no longer
   holds. That is the same "stale value presented as current" class this epic
   has been policing everywhere else — and the position seam exists precisely
   to prevent it for positions (Decision 2). It is NOT fixed here: it is
   pre-existing, it predates the cutover, and the correct fix (an epoch or
   generation stamp on the memo, or a registry-handle validity check on read) is
   a separate change with its own design. It is recorded so the next reader who
   touches the trackers inherits a known hazard instead of rediscovering it.

## The gates

- **No new private dedup map outside the session registry.** Scoped to the
  registry's OWN FOUR identity types: a map keyed by `PoolIdentity`,
  `TokenIdentity`, `PathIdentity`, or `PositionIdentity` is a second id space,
  and it is invisible to the type system because both maps would share a key
  type. The rule is deliberately NOT "no address-keyed map": the core holds 48
  pre-existing ones that are caches, ledgers, and indices with a different
  lifetime, and a gate flagging those would be reporting the codebase rather
  than this rule.

  **This gate polices the RUST CORE, and only the Rust core.** It was verified
  to have a hole and was closed: it shipped listing only two of the four
  identity types, and a second `HashMap<PathIdentity, _>` planted outside the
  registry — the exact failure the path chunk was built to prevent — passed it.
  The key list is now all four, and the match compares the key type's final
  path segment, so a fully-qualified key
  (`HashMap<crate::bot_core::session_registry::PathIdentity, u8>`) is caught
  rather than slipping through on the qualifier.

  **The Python side is NOT policed by this gate, and the task's acceptance
  criterion is therefore only half-automated.** The gate walks `*.rs`; the
  Python surface, where address-keyed dedup state actually still lives, is
  policed by REVIEW plus the removal of the maps that were authority. A
  general Python text gate cannot decide the real rule — the five maps listed
  above are all address-keyed and all legitimate, because the test is minting,
  which a text scan cannot see. What is decidable and false-positive-free is
  narrower: `retired_python_dedup_maps_do_not_reappear` forbids the five
  RETIRED names (`_v2_keys`, `_v3_keys`, `_v4_keys`, `_v3_inflight`,
  `_v4_inflight`) from reappearing anywhere under `src/degenbot/`. Those names
  must never return, so there is no judgment in that check to tune — and it
  does not pretend to cover new maps under fresh names, which remain review's
  job. State the limit rather than let the gate name imply more than it holds.
- **No PyO3 in the registry or the core.** The pyo3-free charter, already
  policed by import.
- **A core public item may not define its contract through a Python-side
  symbol.** This one exists because the import-level gate was not sufficient: a
  public core API once documented its contract as "the 1-based wire code the
  Python `PoolProbe` enum uses", which passes an import gate because nothing
  writes `use pyo3`. The gate polices the decidable form — a `pub` item whose
  doc makes a Python symbol the agent of a definitional verb — and it
  deliberately exempts Python TEST artifacts, because a test suite is a witness
  that a value agrees, never the definition of one.

  That gate is a SUBSET of the underlying rule, and this record says so rather
  than implying full coverage. The real rule is a judgment about which entity
  holds a definition, and no purely lexical check can decide it: the core docs
  contain ~331 legitimate `Python`-qualified mentions, and one of them — "the
  tolerance the Python `test_core.py` suite uses" — is grammatically
  indistinguishable from the rejected case while being entirely legitimate. The
  call-graph alternative (no public core API consumed only by the binding
  shell) was measured and fails on 48 pre-existing legitimate APIs. What ships
  catches the definitional shape; a contract-source comment that avoids those
  verbs will still pass.
