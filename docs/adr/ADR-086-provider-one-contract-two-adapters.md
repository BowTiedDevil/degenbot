# ADR-086: Provider sync/async — one contract, two call-convention adapters

## Status

Accepted (2026-10-27).

## Context

Recorded at the architecture-review grilling (scope: the Python companion's
provider trees) so future reviews do not re-suggest call-convention
unification. The review flagged the two provider trees
(`provider/sync.py`, 578 lines; `provider/async_provider.py`, 382 lines) as
shallow hand-kept mirrors: 23 shared method names kept in agreement by hand,
one method (`get_logs`) already answering two contracts under one name (sync
takes a validated `LogFilter`; async takes bare ints and validates nothing),
two values duplicated under two spellings each (`chain_id`/`get_chain_id`,
`block_number`/`get_block_number`), and 23 `cast()` calls re-typing the
stubs' `Any` returns across both copies.

Call-convention unification — one adapter parameterised over transport — was
the review's first suggestion and does not survive contact with the seam:

- The Rust seam itself is a pair: `_ffi.provider` exposes `AlloyProvider`
  (blocking) and `AsyncAlloyProvider` (awaitable, `feature = "async"`). The
  sync tree wraps genuinely blocking Rust calls; there is no event-loop
  bridging in Python to factor out.
- Both trees have production callers (`bot/_bot.py`, `runner/bot_runner.py`,
  `runner/_dispatch.py`, `dispatch/`, `contract/`). The deletion test keeps
  both: deleting either adapter breaks a call convention callers use rather
  than concentrating complexity. Two adapters = a real seam.

## Decision

**The two provider trees are two call-convention adapters over one contract.
The contract is unified; the call convention is not.**

1. Shared contract logic lives once in `degenbot.provider.surface` (the
   GLOSSARY's *Provider surface*): `get_logs` filter normalization and range
   validation, and the typed payload coercions both trees call.
   `block_helpers` (block-tag resolution) remains the precedent for the
   pattern and stays block-only.
2. `get_logs` converges upward: the async tree gains the sync tree's
   `LogFilter` contract; both trees shrink to delegation over the surface
   module.
3. One spelling per value: `get_chain_id()` / `get_block_number()` (the
   seam's naming, ADR-032); the sync-only properties are deleted. Breaking
   change, accepted for `0.6.0-alpha`.
4. The agreement is pinned by an offline parity test
   (`tests/provider/test_sync_async_parity.py`) that walks both public
   surfaces and asserts name/parameter/default agreement modulo `await`,
   against an explicit intentional-diff list (sync's `__enter__`/`__exit__`;
   async's `create`/`as_async_alloy`). Divergence fails red instead of
   shipping silently.
5. The 23 casts die at two homes: the surface module's coercion helpers
   (deterministic) and opportunistic `gen-stubs` primitive typing (the stub
   emits 25 `-> Any` returns, including primitives on the async pyclass;
   generator work never blocks the deepening).

## Rejected alternative

**One adapter parameterised over transport / one function serving both call
conventions.** Rejected because Python's `async` keyword makes one callable
body impossible, the underlying Rust seam is itself two pyclasses, and both
call conventions have production callers. Future architecture reviews: the
friction to attack is hand-kept agreement (fixed by the parity test and the
surface module), not the existence of two trees.

## Consequences

- A new provider method is added once per tree as a one-line delegation,
  with shared logic in the surface module; the parity test forces the pair
  to grow in lockstep.
- The property deletion breaks `provider.chain_id` / `provider.block_number`
  readers; in-tree sites flip to `get_*()` (`bot/_bot.py`,
  `updater/pool_updater_configs.py`, the package docstring examples).
- Rust-level pair parity (`AlloyProvider` vs `AsyncAlloyProvider` stub
  agreement) is explicitly deferred — the same agreement one tier down, a
  separate change.
