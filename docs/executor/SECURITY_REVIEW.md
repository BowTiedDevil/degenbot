# Security & Performance Review: cmd_executor.vy

**Date**: 2026-05-29  
**Reviewer**: AI-assisted audit  
**Contract**: `contracts/cmd_executor.vy` (18,956 bytes runtime, Venom codegen, `optimize gas`)  
**Scope**: Security vulnerabilities, logical correctness, performance regressions, edge-case handling  

---

## Executive Summary

The contract is well-architected for its purpose — a compact command-stream VM for on-chain arbitrage execution. The owner-only guard, transient callback registration, and deterministic command sizes are solid foundations. However, I found **3 critical**, **3 high**, **6 medium**, and **several low/informational** issues. The critical issues involve bribe caps, reentrancy via callback registration, and a consistency bug in `_command_size` for variable-length commands.

**Design property: no prefunding required.** The executor requires zero initial capital — all working capital is borrowed atomically via V2/V3 flash swaps and V4 PoolManager `take()`. This is a deliberate security feature: no capital at rest means no stuck-funds risk, no griefing via forced ETH deposits, and no capital lockup before the contract can operate. The self-capitalizing architecture means the contract can be deployed with zero balance and execute profitable arbitrage paths immediately. The one command that breaks this property (`V3_SWAP_DELTA`) is documented as a known limitation.

---

## CRITICAL

### C1: Bribe should only transfer a portion of THIS TRANSACTION's profit

**Location**: `execute()`, bribe logic

**Previous bug**: `bribe_amount` was capped by `min(msg.value, ...)` (original) or `min(self.balance, ...)` (first fix). Both caps are wrong — they allow sending ETH that isn't part of the current transaction's profit.

**Fix**: Removed artificial caps entirely. The bribe is now purely `profit * bips / 10000` where `profit = combined_after - combined_before`. If the executor doesn't hold enough native ETH to cover it, the bribe logic auto-withdraws WETH (up to current WETH balance) before sending. This ensures:

1. Only profit from THIS transaction is used for the bribe
2. WETH is automatically unwrapped if ETH is insufficient  
3. If neither ETH nor WETH covers the bribe, `raw_call` reverts (fail-safe)

Additionally, `bips` is validated <= 10,000 in both BRIBE command handlers, preventing `unsafe_mul` overflow.

---

### C2: Reentrancy via t_allowed_callback_addresses persists across unlock boundary

**Location**: `t_allowed_callback_addresses` is set by swap commands and checked by callback handlers, but is NOT cleared between V4_UNLOCK calls or after callbacks complete.

**Issue**: If a command stream contains two V4_UNLOCK blocks (e.g., `V4_UNLOCK(inner_1) + V4_UNLOCK(inner_2)`), the callback addresses registered by inner_1 remain valid for inner_2. More importantly, if a malicious callback target is somehow registered (e.g., a compromised V2 pair), it remains valid for the entire transaction.

**This is by design**: The callback addresses are pool addresses, set by the swap commands themselves. Only the pools we actually swap with get registered. A malicious pool can only be registered by the owner's command stream, and the owner controls which pools to target.

**However**: There's a subtler issue. Consider a command stream that:
1. `V2_SWAP_COMPACT(pool=A, forward_data=<commands with V2_SWAP_COMPACT(pool=B, ...)>)`

Inside the callback from pool A, pool B gets registered. But pool B's callback will call `_process_commands(data)` where `data` comes from pool B. If pool B is maliciously constructed to send crafted `data`, it could execute arbitrary commands on the executor. BUT — the callback address check prevents this: pool B must be registered, and only the owner's command stream registers pools.

**Residual risk**: If the same pool is used for TWO separate swaps in one command stream (once in the outer stream, once in a callback), the second callback could use stale transient state (e.g., `tAddresses`, `t_v4_currencies_touched`) that was modified by the first callback. This is actually safe because each callback's `_process_commands` creates its own execution context — the state from the outer stream persists, which is exactly the intended behavior for cross-protocol arbitrage.

**Verdict**: Design is safe given the owner-only constraint. No change needed.

---

### C3: `_command_size` returns WRONG sizes for V2/V3_SWAP_COMPACT and V4_UNLOCK with forward_data

**Location**: `_command_size()`, lines ~1250–1280

```vyper
elif opcode == COMMAND_V3_SWAP_COMPACT:
    return 22  # WITHOUT forward_data — use V3_SWAP_DELTA for auto-pay
elif opcode == COMMAND_V2_SWAP_COMPACT:
    return 22  # WITHOUT forward_data — use V2_SWAP_CALC for auto-pay
```

**Issue**: V2_SWAP_COMPACT and V3_SWAP_COMPACT ARE variable-length commands. Their actual encoding is `22 + forward_len` bytes. The `_command_size` function returns a FIXED 22, ignoring the forward_data. This is **correct** for the `_process_commands` path because the command handlers return `offset + 22 + forward_len`, so `_command_size` is never consulted in that code path.

**But**: `_command_size` IS used by `_read_tstore_continuation`. If a V2/V3_SWAP_COMPACT command is stored in tstore continuation data, `_command_size` would return 22, causing the offset to skip only 22 bytes instead of `22 + forward_len`. The command dispatch would be correct (it reads `forward_len` from the data and returns the right offset), but the tstore continuation reader's `offset += cmd_size` would advance by 22, not by `22 + forward_len`, causing it to mis-parse all subsequent commands.

**Impact**: If V2_SWAP_COMPACT or V3_SWAP_COMPACT with forward_data is placed inside a TSTORE_CONTINUATION, the continuation reader will desync and either revert or execute garbage commands.

**Fix**: `_command_size` needs access to the data to read the `forward_len` field for variable-size commands. This requires changing its signature to accept data + offset:

```vyper
@internal
@view
def _command_size(opcode: bytes1, data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
```

Then for V2/V3_SWAP_COMPACT:
```vyper
elif opcode == COMMAND_V2_SWAP_COMPACT:
    forward_len: uint256 = convert(slice(data, offset + 22, 2), uint256)
    return 24 + forward_len
```

Similarly for V4_UNLOCK and V4_BATCH. Alternatively, document that these commands MUST NOT be placed in tstore continuation data (which is the current intended use — tstore continuations are for "simple" continuation like V4_SETTLE_ALL after external callbacks).

**Severity**: Critical if variable-length commands are ever used in tstore continuations. Low in practice because the typical continuation is a short fixed-size command, but the contract has no runtime enforcement of this constraint.

---

## HIGH

### H1: Double profit check is redundant and wastes gas

**Location**: `execute()`, lines ~1398 and ~1427

```vyper
if not self.t_skip_profit_check:
    assert combined_after >= combined_before, "balance reduction"  # First check

# ... bribe logic ...

if not self.t_skip_profit_check:
    assert combined_after >= combined_before, "balance reduction"  # Second check (redundant!)
```

The profit check is performed TWICE. The first check (before bribe) and the second check (after bribe) assert the same condition. The bribe logic does NOT modify `combined_after` — it just sends ETH via `raw_call`, which reduces `self.balance` but that's after `combined_after` was captured.

`combined_after` is read BEFORE the bribe. The bribe sends ETH from `self.balance`, so after the bribe, `combined_after` is stale. The second assert checks the same stale value, so it's truly redundant.

If the intent was to verify the state AFTER the bribe, `combined_after` should be re-read. Currently the second check is dead code — it always passes if the first one passed.

**Fix**: Remove the second check, or re-read balances after the bribe if post-bribe verification is desired.

**Gas impact**: One redundant `assert` ≈ 3 gas (just an opcode check since the condition is already known true after the first assert). Negligible, but the code is confusing.

---

### H2: V4_MINT_COMPACT does not verify the delta is sufficient

**Location**: `_cmd_v4_mint_compact()`

```vyper
extcall IPoolManager(POOL_MANAGER_ADDR).mint(
    self.t_addresses[recipient_idx],
    convert(convert(currency, uint160), uint256),
    amount,
)
```

**Issue**: The executor mints `amount` of ERC6909 tokens, but does NOT verify that the PM's delta for that currency covers the mint. In the real PoolManager, `mint()` calls `_accountDelta(currency, -(amount.toInt128()), msg.sender)` which will revert if the delta is insufficient (negative delta exceeds the positive credit). So the PM itself enforces this invariant.

After minting, the delta for that currency is reduced (some/all of the positive delta is consumed). `V4_SETTLE_ALL` will still iterate over the currency, and if the delta is zero (fully minted), exttload returns 0 — correct behavior.

The concern is with partial mints: if the delta is only partially consumed, the residual should still be settled. `V4_SETTLE_ALL` reads exttload (which returns the post-mint residual delta), so this is also handled correctly — **except** that `_cmd_v4_mint_compact` does NOT add the currency to `t_v4_currencies_touched`. This means `V4_SETTLE_ALL` will NOT settle any residual delta for a partially-minted currency. Example:

1. V4 swap creates +100 USDC delta for the executor
2. V4_MINT_COMPACT(USDC, 80) — mints 80, leaving +20 delta
3. V4_SETTLE_ALL — USDC is NOT in `t_v4_currencies_touched`, so the +20 is NOT taken
4. V4_UNLOCK ends → CurrencyNotSettled revert

**Fix**: Add `self.t_v4_currencies_touched[currency] = True` in `_cmd_v4_mint_compact`.

**Similarly**: `_cmd_v4_burn_compact` does NOT mark currency as touched. After burning, a positive delta is created (the PM owes the executor). If V4_SETTLE_ALL is called, it needs to know about this currency to settle the new delta. Same fix needed.

---

### H3: V4_BURN_COMPACT can burn from any account without approval check in the executor — ✅ FIXED

**Location**: `_cmd_v4_burn_compact()`

**Previous issue**: The `account_idx` parameter allowed specifying which account's ERC6909 tokens to burn. A mis-encoded `account_idx` could accidentally burn another user's ERC6909 tokens if the executor has operator approval.

**Fix**: Removed `account_idx` parameter entirely. The handler now always burns from `self`:

```vyper
extcall IPoolManager(POOL_MANAGER_ADDR).burn(
    self,  # Always burn own tokens
    convert(convert(currency, uint160), uint256),
    amount,
)
```

This saves 1 byte per encoding (19→18 bytes) and eliminates the risk. Encoding changed from `[0x59][currency_idx:1][account_idx:1][amount:16]` to `[0x59][currency_idx:1][amount:16]`.

---

## Offset Consistency Bug (Post-Audit Fix)

### Handler return values vs `_command_size` vs encoding sizes — ✅ FIXED

When `V4_BURN_COMPACT` was changed from 19→18 bytes (removing `account_idx` per H3), the return values in two handlers got swapped — `_cmd_v4_mint_compact` (19-byte encoding) was returning `offset + 18` while `_cmd_v4_burn_compact` (18-byte encoding) was returning `offset + 19`. This caused `InvalidCommand` errors in any multi-command stream containing a mint or burn, because the offset would be off by 1 after the mismatched command, causing the next command's opcode byte to be read from the wrong position.

A systematic audit of ALL handler return values vs `_command_size` values vs actual encoding sizes revealed 4 total mismatches:

| Command | Encoding | Handler return | `_command_size` | Mismatch source | Fix |
|---------|----------|---------------|-----------------|----------------|-----|
| V4_MINT_COMPACT (0x58) | 19 bytes | 18 ✗ | 19 | Handler → 19 | ✅ Fixed |
| V4_BURN_COMPACT (0x59) | 18 bytes | 19 ✗ | 18 | Handler → 18 | ✅ Fixed |
| V4_TAKE_COMPACT (0x52) | 19 bytes | 18 ✗ | 19 | Handler → 19 | ✅ Fixed |
| SEND_ETH (0x16) | 18 bytes | 19 ✗ | 18 | Handler → 18 | ✅ Fixed |
| V4_SWAP_DYNAMIC (0x41) | 11 bytes | 11 | 10 ✗ | `_command_size` → 11 | ✅ Fixed |
| V4_SWAP_COMPACT (0x40) | 27 bytes | 27 | 26 ✗ | `_command_size` → 26→27 | ✅ Fixed |

**Impact**: Any multi-command stream containing V4_MINT_COMPACT, V4_BURN_COMPACT, V4_TAKE_COMPACT, or SEND_ETH would have failed with `InvalidCommand` due to byte misalignment. The `_command_size` mismatches only affected `TSTORE_CONTINUATION` validation, which was already blocked by runtime assertions for variable-size commands.

**Root cause**: When the V4_BURN_COMPACT encoding was changed from 19→18 bytes, the handler return values were updated but accidentally swapped — mint's `offset + 19` became `offset + 18` and burn's `offset + 18` became `offset + 19`. The other mismatches were pre-existing (likely from when opcodes were originally added).

**Recommendation**: Add a build-time check that verifies every `_command_size` return value matches the corresponding handler's `return offset + N` value, and that both match the encoding documentation.

---

## MEDIUM

### M1: `_v2_auto_pay` uses `elif` for amount0_out/amount1_out — cannot handle both-positive case

**Location**: `_v2_auto_pay()`

```vyper
if amount0_out > 0:
    # pay token1
elif amount1_out > 0:
    # pay token0
```

In real Uniswap V2, it's possible for BOTH `amount0Out` and `amount1Out` to be positive in a single swap (flash borrow of both tokens). The `elif` means if both are positive, only token0's owed amount is paid, and token1's payment is silently skipped. This would cause the V2 K-invariant check to fail.

However, in practice, arbitrage executors never swap both tokens out of a V2 pair. The V2 fee math in `_v2_get_amount_in` also doesn't handle dual-output swaps (it computes the owed amount for one output only). So this is a theoretical issue, but worth noting for correctness.

**Fix**: Change `elif` to `if` and support both positive amounts:
```vyper
if amount0_out > 0:
    owed1: uint256 = self._v2_get_amount_in(amount0_out, ...)
    extcall IERC20(token1).transfer(pool, owed1, ...)
if amount1_out > 0:
    owed0: uint256 = self._v2_get_amount_in(amount1_out, ...)
    extcall IERC20(token0).transfer(pool, owed0, ...)
```

---

### M2: `_v2_get_amount_in` can underflow when `amount_out >= reserve_out`

**Location**: `_v2_get_amount_in()`

```vyper
denominator: uint256 = (reserve_out - amount_out) * fee_multiplier
```

If `amount_out >= reserve_out`, this subtraction underflows. In Vyper, this reverts. In practice, the V2 pair's `INSUFFICIENT_LIQUIDITY` check prevents `amount_out > reserve`, but `amount_out == reserve` is allowed (draining one side completely).

When `amount_out == reserve_out`, the denominator is 0, and `numerator // 0` also reverts. So the function is safe (reverts instead of producing wrong results), but the error message won't be helpful.

**Fix**: Add an explicit check:
```vyper
assert amount_out < reserve_out, "V2: INSUFFICIENT_LIQUIDITY"
```

---

### M3: `V2_SWAP_CALC` uses `balanceOf(pair) - reserves` as `amount_in` — excess may include unintended tokens

**Location**: `_cmd_v2_swap_calc()`

```vyper
pair_balance: uint256 = staticcall IERC20(input_token).balanceOf(pool)
amount_in: uint256 = pair_balance - reserve_in
```

This reads the pair's ENTIRE excess balance (tokens at the pair not yet in
reserves) as the swap amount. The excess is normally created by a prior
V4_TAKE or ERC20_TRANSFER sending tokens directly to the pair. But the
excess can also include:
- Accumulated V2 swap fees from other swappers between the last `_update()`
  and this call (negligible for same-block arbitrage)
- An oversized V4_TAKE that sent more tokens to the pair than intended
- Multiple deposits to the same pair from different steps in the command
  stream

If any of these inflate the excess beyond the intended swap input, the
computed `amount_out` and `amount_in` will be larger than expected,
potentially causing:
- Slippage beyond what the arbitrage opportunity requires
- The executor spending more tokens than the path needs
- Profit check failure if the oversized swap consumes tokens needed later

**This is by design** for V2_SWAP_CALC — it's "swap everything at the pair."
But the pair's excess balance at the time of the V2_SWAP_CALC command IS
the swap amount. If unintended excess is present, the behavior may be
unexpected.

**Note**: The V2 fee is now configurable per-swap (fraction of 10000:
30=Uniswap, 25=PancakeSwap) via the `fee:2` field in both V2_SWAP_CALC
and V2_SWAP_COMPACT encoding. V2_SWAP_CALC uses the fee inline in
`_v2_get_amount_out`; V2_SWAP_COMPACT writes it to `t_v2_pair_fee[pool]`
transient storage for the callback handler's `_v2_auto_pay()` →
`_v2_get_amount_in()` computation. Both commands assert `0 < fee < 10000`
on decode (reverting with `BipsTooHigh` if exceeded). The formula uses `10000 - fee`.

---

### M4: SEND_ETH_ALL sends `self.balance` which includes msg.value

**Location**: `_cmd_send_eth_all()`

```vyper
raw_call(self.t_addresses[recipient_idx], b'', value=self.balance)
```

If `execute()` was called with `msg.value > 0`, `self.balance` includes that value. SEND_ETH_ALL would then forward the sent ETH along with any earned ETH. This may not be intended — the owner might send ETH for WETH wrapping or V4 settlement, not for forwarding.

**Fix**: Consider tracking "earned ETH" separately, or documenting that SEND_ETH_ALL sends the entire balance including any attached ETH.

---

### M5: Bribe amount uses `unsafe_mul` — can overflow for extreme bips values

**Location**: `execute()`

```vyper
bribe_amount: uint256 = min(
    msg.value,
    unsafe_mul(self.t_bribe_bips, profit) // 10_000,
)
```

`unsafe_mul(bips, profit)` can overflow if `bips * profit > 2^256 - 1`. With `bips` stored as `uint256` (not `uint16`), an attacker who sets `bips` to a very large value could cause overflow. Since `bips` is only ever set by the BRIBE commands which read 2 bytes, the maximum encoded value is 65535, and `65535 * profit` is safe for any realistic profit. But `t_bribe_bips` is `transient(uint256)`, so a bug in the encoding could write a larger value.

**Fix**: Either:
1. Validate `bips <= 10_000` in the BRIBE command handlers
2. Change `t_bribe_bips` to `transient(uint16)`
3. Use `mul(bips, profit)` (safe mul) instead of `unsafe_mul`

Option 1 is simplest and adds meaningful validation (bips > 10000 makes no sense).

---

### M6: Multiple V4_BATCH calls in one stream share `t_v4_currencies_touched`

**Location**: `_cmd_v4_batch()` + `_auto_settle_touched()`

If two V4_BATCH commands are in the same stream, currencies touched by the first batch remain in `t_v4_currencies_touched` for V4_SETTLE_ALL to iterate. This is correct behavior (all currencies from all batches need settling). However, `_v4_batch_settle()` only settles native+WETH, which is correct ONLY if the batches form a V4-only path where intermediate deltas cancel.

If two V4_BATCH commands handle different paths (e.g., V4→V3 and V4→V2), currencies from the first batch's intermediate steps may have nonzero deltas that the second batch's `_v4_batch_settle()` won't settle.

**Fix**: This is a user error (V4_BATCH should only be used for V4-only paths), but worth documenting.

---

## LOW

### L1: `__default__()` accepts all ETH transfers silently

**Location**: `__default__()`

```vyper
if len(msg.data) == 0:
    return
else:
    raise NotPlainEthTransfer()
```

Any address can send ETH to the executor. This is needed for receiving native ETH from V4_TAKE_DELTA, but it also means anyone can force the executor to hold ETH, which could interfere with the profit check (inflating `combined_before` at an unexpected time).

**Impact**: Negligible. The profit check uses `combined_after >= combined_before`, so extra ETH only increases the baseline, making the check harder to pass (beneficial for the owner). This is consistent with the no-prefunding design: since the contract never requires a funded balance to operate, any ETH present is from transaction operations (not required capital).

---

### L2: `withdraw()` can unwrap WETH and send ETH even if not needed

**Location**: `withdraw()`

```vyper
if amount > self.balance:
    extcall IWETH(WETH_ADDR).withdraw(amount - self.balance)
```

If `amount > self.balance + WETH_balance`, the `raw_call` on the next line will revert. But the WETH has already been unwrapped, leaving the contract in a state where WETH was burned but ETH wasn't sent. The executor loses the WETH.

**Fix**: Check that `self.balance + WETH_balance >= amount` before unwrapping. Or use a try/catch pattern.

---

### L3: Fee integer division in `_v2_get_amount_out/_in` truncates toward zero

The V2 math uses integer division which truncates. This matches the real UniswapV2Library behavior and is correct — the pair's K-invariant check ensures the truncation favors the pair (never the swapper).

---

### L4: `_read_tstore_continuation` reads `t_continuation_data` slots without bounds check

**Location**: `_read_tstore_continuation()`

```vyper
for i: uint256 in range(MAX_TSTORE_CONTINUATION_SLOTS):
    if i >= num_chunks:
        break
    chunks[i] = self.t_continuation_data[i]
```

If `num_chunks > MAX_TSTORE_CONTINUATION_SLOTS` (i.e., `length > 512`), the loop silently truncates. But `length` is already asserted `<= MAX_COMMANDS_LENGTH (512)`, and `MAX_TSTORE_CONTINUATION_SLOTS = 16`, so `16 * 32 = 512`. If `length = 512`, `num_chunks = 16`, which is exactly `MAX_TSTORE_CONTINUATION_SLOTS`. This is safe.

---

### L5: `_cmd_tstore_continuation` byte-by-byte reconstruction of partial chunks is gas-expensive

**Location**: `_cmd_tstore_continuation()`, the inner `for j in range(32)` loop

Reading partial chunks byte-by-byte costs ~32 * (slice + convert + shift + OR) ≈ ~1,500 gas per partial chunk. For a 512-byte continuation, the last chunk is partial, adding ~1,500 gas overhead.

**Optimization**: Ensure that continuation data is always padded to a 32-byte boundary externally, so the partial-chunk path is never taken. The code comment mentions this contract but doesn't enforce it.

---

## PERFORMANCE

### P1: Redundant second `assert combined_after >= combined_before` — remove it

~3 gas saved (trivial), but improves code clarity.

---

### P2: `_auto_settle_touched()` iterates ALL `t_addresses` even if most aren't V4-touched

The loop iterates 32 addresses max. For each, it checks `t_v4_currencies_touched[addr]` (TLOAD, 100 gas). If only 2 addresses are V4-touched out of 10, that's 8 * (TLOAD + branch) ≈ 800 gas wasted.

**Fix**: Maintain a transient `DynArray[address, MAX_V4_CURRENCIES]` of touched currencies (like `t_currencies_used` in the fake PM) and iterate that instead. This replaces the O(N) scan with O(V4_currencies).

Estimated savings: 200–1,000 gas depending on address table size.

---

### P3: `_v4_settle_currency` for WETH does unnecessary `balanceOf` check

```vyper
weth_balance: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self)
if weth_balance < owed:
    extcall IWETH(WETH_ADDR).deposit(value=owed - weth_balance)
```

This adds a warm SLOAD (~100 gas) + external call overhead. In most arb paths, the executor has WETH from V4 take or prior operations. The check is correct, but if the executor always needs to deposit (common case when settling after a V4 take into WETH), the balance check is wasted.

**Alternative pattern**: Always deposit the full owed amount, then transfer. This costs more gas when WETH is already available but saves the balance check when it's not.

Verdict: Keep current behavior — the optimization depends on the specific path.

---

### P4: Sentinel address resolution bypasses bounds check for indices ≥ 0xFC

`_lookup_address(idx)` checks `idx >= 0xFC` first (sentinel range), falling through to `self.t_addresses[idx]` for regular indices. The sentinel path avoids the TLOAD + array bounds check. The regular path still performs the full bounds check. This is safe because sentinel indices 0xFC–0xFF are reserved and cannot appear in the address table (the table is populated sequentially from index 0). Inlined versions in hot handlers replicate this same pattern.

---

### P5: `_command_size` uses a linear chain of `if/elif` — jump table would be faster

The function has ~25 branches. Each `elif` is an unconditional jump, so the average case is ~12 comparisons. A jump table (using the opcode as an index into a precomputed array) would be O(1), but Vyper doesn't support this pattern.

**Workaround**: Order the branches from most-common to least-common opcode. Currently the order seems arbitrary.

---

### P6: `_read_tstore_continuation` allocates a 512-byte `chunks` array on the stack

```vyper
chunks: bytes32[MAX_TSTORE_CONTINUATION_SLOTS] = [...]
```

This is 16 * 32 = 512 bytes of zero-initialization in memory. On Venom, this may be optimized away for partially-written slots, but the explicit zero-initialization forces all 16 slots to be written.

**Optimization**: Only initialize `num_chunks` slots instead of all 16. But Vyper doesn't support partial initialization of arrays.

---

## CODE QUALITY

### Q1: Inconsistent opcode numbering — RESOLVED

Opcodes have been reorganized into clean protocol-grouped ranges (0x00=Control, 0x10=ERC20/ETH, 0x20=V2, 0x30=V3, 0x40=V4 Swaps, 0x50=V4 Settlement). No gaps remain.

**Fix**: Update the docstring to only list active opcodes, and remove comments about removed opcodes.

---

### Q2: `_cmd_tstore_continuation` partial-chunk reconstruction is overly complex

The byte-by-byte `for j in range(32)` loop could be simplified by requiring the caller to pad the data to a 32-byte boundary. The encoding documentation says `len = actual bytes`, but the implementation tries to handle partial chunks. This adds ~100 bytes of bytecode and a rare path that's hard to test.

---

### Q3: Three identical V2 callback functions (uniswapV2Call, hook, pancakeCall)

These are identical implementations with only the function name differing. They could share logic via an `@internal` function, but Vyper already generates separate ABI entries for each. The shared logic IS already in `_v2_auto_pay` and `_process_commands` — the callback functions are thin wrappers. The duplication is unavoidable due to different selector requirements.

---

## SUMMARY TABLE

| ID | Severity | Category | Issue | Fix Effort |
|----|----------|----------|-------|------------|
| C1 | Critical | Security | Bribe cap used `msg.value` instead of profit-derived amount | ✅ Fixed: removed artificial cap; bribe = `profit * bips / 10000` purely, reverts if insufficient ETH |
| C2 | Critical | Security | Reentrancy via callback registration | N/A (safe by design) |
| C3 | Critical | Correctness | `_command_size` wrong for variable-length commands in tstore | ✅ Fixed: removed V2/V3_SWAP_COMPACT from _command_size, added runtime assertions in _read_tstore_continuation |
| H1 | High | Performance | Double profit check is redundant | ✅ Fixed: removed second redundant check |
| H2 | High | Correctness | V4_MINT/BURN_COMPACT don't mark currency in `t_v4_currencies_touched` | ✅ Fixed: both now set t_v4_currencies_touched[currency] = True |
| H3 | High | Security | V4_BURN_COMPACT `account_idx` allows burning from arbitrary accounts | ✅ Fixed: removed `account_idx`, always burns from `self`; encoding 19→18 bytes |
| M1 | Medium | Correctness | `_v2_auto_pay` `elif` can't handle dual-output V2 swaps | ✅ Fixed: changed elif to if |
| M2 | Medium | Robustness | `_v2_get_amount_in` underflows when `amount_out >= reserve_out` | ✅ Fixed: added explicit assert |
| M3 | Medium | Semantics | V2_SWAP_CALC uses full pair excess `balanceOf(pair) - reserves` as `amount_in` | Doc only |
| M4 | Medium | Semantics | SEND_ETH_ALL includes `msg.value` | Doc only |
| M5 | Medium | Security | `unsafe_mul` for bribe with `uint256` bips | ✅ Fixed: bips validated ≤ 10000 in BRIBE command handlers |
| M6 | Medium | Semantics | Multiple V4_BATCH + V4_SETTLE_ALL interaction | Doc only |
| L1 | Low | Security | `__default__` accepts arbitrary ETH | N/A |
| L2 | Low | Correctness | `withdraw()` can unwrap WETH then fail to send | ✅ Fixed: added total balance check before unwrapping |
| L3 | Low | Semantics | Fee truncation matches V2 | N/A |
| L4 | Low | Correctness | Tstore continuation bounds check | N/A |
| L5 | Low | Performance | Byte-by-byte partial chunk reconstruction | Low |
| P1 | Perf | Gas | Remove redundant second profit check | 3 gas |
| P2 | Perf | Gas | `_auto_settle_touched` iterates all addresses | 200–1,000 gas |
| P3 | Perf | Gas | WETH settle extra balance check | Path-dependent |
| P4 | Perf | Gas | Double TLOAD for `pool` | ~12 gas |
| P5 | Perf | Gas | Linear `_command_size` dispatch | N/A (Vyper) |
| P6 | Perf | Memory | 512-byte chunks array initialization | N/A (Vyper) |
