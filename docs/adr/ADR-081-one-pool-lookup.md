# ADR-081: One pool lookup at the state seam

## Status

Accepted.

## Context

A pool is addressed two ways: a V3 concentrated-liquidity pool by contract
address, a V4 pool by `(pool_manager, pool_id)`. The state owner grew a verb per
shape — `set_pool_live` beside `set_pool_live_by_key`, `apply_pump_buffer` beside
`apply_pump_buffer_by_key`, and six more — so every orchestration rule had two
entry points whose bodies could drift independently. The registration lifecycle
carried the same split as two ~100-line machines.

## Decision

One value, `PoolLookup` (`Address(Address) | V4 { pool_manager, pool_id }`),
owns the key shape. Orchestration verbs hang off it; the state owner keeps a
single interior per verb (`_by_key` for the V4-resolving arm, `coverage_by_pool_id`
for the id-resolving one), and the address-keyed twin is deleted rather than
kept as a shim.

`PoolLookup` lives in `degenbot-substrate` next to `BotState`, not in the bot
crate: it is the substrate's own vocabulary, and the PyO3 shell, the driver and
the standalone consumer all reach it through the same re-export.

A source-level census pins the invariant: no retired address-keyed verb may
reappear, and every retired verb must name its `PoolLookup` door. The census is
a pure function over `include_str!` sources so it carries a negative probe — a
fixture with the old shape fails the gate.

## Consequences

- The eight address-keyed twins are gone. Call sites name the pool once
  (`PoolLookup::from_address(addr).set_live(&mut state)`), and the resolve
  happens at the seam, not at each caller.
- `RegistrationKey`, the registration-lifecycle prototype, retired into
  `PoolLookup`: one key vocabulary, not two.
- The `_by_key` verbs remain public because they are the V4 door; a future
  caller holding a raw `pool_id` may collapse them into the same interior.
- Public core surface shrank; the `check-rust-consumer` and
  `check-rust-binding-default` lanes gate the change on both consumers.

## The memo identity is typed too

The same split lived one level down: a pool's *pre-build* identity — what the
negative memos and the verify-once fact key on — was text. The ledger formatted
`p:0x…` / `v4id:0x…`, the pipeline formatted `v3:…` / `v4:…`, the example
formatted a third variant, and the driver a fourth, so a verify fact written by
one path was invisible to the others and every insert allocated a
twice-lowercased string.

`MemoIdentity` (`Address(Address) | V4(V4PoolId)`) is the key type now:
`Display`/`FromStr` are the wire form (Python carries text without owning the
format), the ledger's doors take the typed key, and the driver builds it from
the `PoolLookup` it already holds. Three consequences are deliberate:

- **Family is not part of the identity.** One contract address holds one pool,
  so `Address` is the whole identity; the old `v3:`/`v4:` prefixes split facts
  that should have been shared.
- **A malformed identity is not memoizable.** `pool_memo_key` returns `None`
  rather than keying by whatever text arrived — an identity that is not 20 or 32
  bytes is a caller bug, and the Python doors raise `ValueError` naming it.
- **Text stays at the boundary.** `RegistrationLedger.address_memo_key` /
  `pool_id_memo_key` are the Python-side constructors; a source gate
  (`tests/test_pool_memo_key_seam.py`) fails on a hand-formatted key anywhere
  else in `src/`.
