# H3 — Donation inflation & stream truncation

## Donation half: REAL — fixed

An attacker who sends plain ETH to the executor via a plain value-transfer CALL
(triggering `__default__`) inflates `combined_after` in mode-1 profit check
(`WETH.balanceOf(self) + self.balance`). This can:
- mask a losing arbitrage (the `InsufficientProfit` assert passes on a donation-
  inflated `combined_after`), and
- trigger a fake bribe payout (`profit = combined_after − combined_before` includes
  the donation, `bribe_amount = bips/10000 × profit` is paid out of the donated ETH
  plus any real balance — net loss to the operator on a fake-profit).

This is the **only** finding from the review that is genuinely exploitable to
make `execute` return success on a losing path (all other "exploit" claims — H1,
H2 — were retracted because the profit check reads *after* the loop and balances
honestly). The donation defeats that precisely because it lands *before* the
`combined_after` read but *after* `combined_before` (= `expected_value` from
config, the operator's pre-tx balance).

### Fix

`__default__()` now rejects any sender except `POOL_MANAGER_ADDR` and
`WETH_ADDR` — the only two contracts that legitimately deliver ETH to the
executor via `raw_call`:

1. **PoolManager** — `take(NATIVE_ADDRESS, executor, amt)` does
   `raw_call(to=executor, value=amt)`. Required for the V4 native-take
   withdrawal path.
2. **WETH** — `withdraw(amt)` does `raw_call(msg.sender=executor, value=amt)` to
   credit the withdrawer. Required for any WETH→ETH unwrap (WETH_WITHDRAW,
   WETH_WITHDRAW_ALL, `withdraw()`, bribe shortfall unwrap).

Every other plain-ETH transfer to the executor is a donation and is rejected
with `Unauthorized(caller=msg.sender)`. Unknown-function calls continue to be
rejected with `NotPlainEthTransfer`.

### Residual (folded in from H2 retraction)

A malicious pool *outside the operator's stated model* could theoretically
deliver a donation via a reentrant `ERC20_TRANSFER(WETH→executor)` (ERC20
transfer, not plain ETH — `__default__` is not involved). Canonical immutable
Uniswap V2/V3 pairs never make inbound `ERC20_TRANSFER`s to the executor during a
callback, so this residual is closed under the stated model. Mode-2 (ERC6909
WETH) profit reads are unaffected by ERC20 donations of physical WETH.

## Truncation half: NON-EXPLOITABLE — no code fix (guard retained)

L3 claimed every `for _ in range(MAX_COMMANDS_LENGTH)` loop silently drops
trailing commands when `offset < len(data)` at loop exit. **Unreachable** given
the type system:

- All command-stream `Bytes` parameters are typed `Bytes[MAX_COMMANDS_LENGTH]`
  (288 bytes).
- Smallest command is 1 byte (V4_SETTLE, V4_SETTLE_ALL, WETH_DEPOSIT_ALL,
  WETH_WITHDRAW_ALL — all `SIZE_* = 1`).
- Max command count in any well-typed stream = 288, which exactly consumes the
  288-iteration loop cap. The `if offset >= len: break` always fires from the
  length check before the loop counter can exhaust.

So a well-typed input can never silently truncate. The concern would only
materialize if `MAX_COMMANDS_LENGTH` were raised without widening the loop cap,
or if a sub-1-byte command were added — neither is the case. Kept the
truncation test as a structural-invariant guard.

## Artifacts

- Fix: `contracts/cmd_executor.vy` `__default__()` — added sender allowlist.
- `tests/test_donation_profit_inflation.py` — donation reverts; balance untainted.
- `tests/test_stream_truncation.py` — structural-invariant guard (288-byte
  stream, 288-iteration cap, no silent truncation).

## Validation

- `tests/test_donation_profit_inflation.py` ✓
- `tests/test_stream_truncation.py` ✓
- `tests/test_withdrawal.py` ✓ (regression: PM native-take + WETH withdraw still
  deliver ETH to the executor — `__default__` allowlist preserves these).
- `tests/test_cmd_executor_three_hop_optimized.py` — 27/27 paths green, gas
  unchanged (the `__default__` body is never hit on these paths).

## Status

H3 task (7VAA46): DONE — donation fixed; truncation analyzed as non-exploitable
with invariant guard retained.
