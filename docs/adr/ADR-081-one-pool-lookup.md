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
