# M6 — Hook sentinel fail-closed: RETRACTED (cosmetic, not a security fix)

## Original finding (M6)

`hooks_idx ∈ {0xFC, 0xFD, 0xFE}` falls into `self.t_addresses[252..254]` →
Vyper bounds revert (generic), instead of raising `InvalidCommand(opcode=...)`
consistent with the contract's fail-closed sentinel philosophy.

## Verdict: NOT A SECURITY FIX — contract already fails closed

`hooks` resolves via:
```vyper
hooks = self.t_addresses[hooks_idx] if hooks_idx != V4_NATIVE_SENTINEL else empty(address)
```

For `hooks_idx ∈ {0xFC=252, 0xFD=253, 0xFE=254}`: the ternary's condition
(`!= 0xFF`) is TRUE, so it evaluates `self.t_addresses[hooks_idx]`. Because
`t_addresses` is typed `address[32]` (MAX_INDEXED_ADDRESSES=32), Vyper emits a
runtime bounds check (`clampge idx, 32`) that **reverts** for any index ≥ 32.

So invalid hook sentinels ALREADY fail closed — the only effect of the proposed
guard was to substitute a clearer `InvalidCommand(opcode=hooks_idx)` error for
the generic Vyper array-bounds revert. That is a cosmetic error-message
improvement, not a correctness or security fix. No invalid hook sentinel can
pass through to `PM.swap(pool_key, ...)` with a wrong `hooks` value.

(Consistent with the H1/H2 retractions: the executor is owner-gated and the
operator controls the entire command stream, so a malformed `hooks_idx` is the
owner's own encoding bug — which the existing bounds check already catches.)

## Gas cost of the proposed guard

Measured on the 27-path benchmark (the canonical-Immutable operating model,
where every V4 pool has no hooks → `hooks_idx == 0xFF` on every V4 swap):

- `V4V4V4` (3 V4 swaps): 85,776 → 85,942 (**+166 gas**, ~+55/swap)
- `V4V4V2` (1 V4 swap): 131,106 → 131,244 (+138)
- `V4V2V2` (1 V4 swap): 166,673 → 166,742 (+69)

Even with short-circuit reordering (`if hooks_idx != V4_NATIVE_SENTINEL and
hooks_idx >= SENTINEL_THRESHOLD`), the common case (0xFF) still costs one
`ISZERO` + `JUMPI` per V4 swap. AGENTS.md flags >50 gas/path regressions as
material; the M6 guard exceeds that on every V4-involving path.

## Decision

**Do not ship M6.** Reverted the `raise InvalidCommand` guard from
`_cmd_v4_swap_compact`, `_cmd_v4_swap_dynamic`, and `_cmd_v4_batch`. The
existing ternary + Vyper bounds check is retained. The fail-closed behavior is
preserved (invalid hook sentinels still revert); only the error message is less
specific than `InvalidCommand`.

## Retained

`tests/test_sentinel_failclosed.py::TestM6HookSentinelFailClosed` — kept as a
regression guard asserting that invalid hook sentinels (0xFC/0xFD/0xFE) revert
(via the bounds check), regardless of which error selector is raised. The
0xFF (no-hooks) case is asserted to be the allowed sentinel.

## Contrast with M5 / M7 (which ARE kept)

- **M5 (V4_TAKE_DELTA delta<=0)**: KEPT. The guard prevents
  `convert(negative_delta, uint256)` wrapping to ~2²⁵⁵ and an opaque PM
  `take()` revert; converts it to a clean no-op. Real behavior improvement
  (zero gas on the hot path — V4_TAKE_DELTA is cold).
- **M7 (V4_SETTLE_DELTA PM/SELF sentinel)**: KEPT. The silent no-op for
  PM/SELF sentinels genuinely left a delta un-resolved that `PM.unlock` would
  later catch with a confusing `CurrencyNotSettled`; raising `InvalidCommand`
  at the source is clearer and the path is cold. Zero hot-path gas.
- Both M5 and M7 are on cold paths (V4_TAKE_DELTA and V4_SETTLE_DELTA are
  settlement commands, not the per-swap hot path), so their guards cost ~0 on
  the 27-path benchmark (confirmed: V4V4V4 85,776 → 85,7XX after M5+M7, no
  measurable regression).
