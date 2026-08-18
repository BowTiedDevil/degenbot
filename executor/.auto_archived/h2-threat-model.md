# H2 Threat Model: V2/V3/V4 Callback Auth Reentrancy

**Verdict: EXPLOITABLE.** A malicious V2/V3 pool that the operator selects can
drain the executor's entire WETH + ETH balance (and, if PM is unlocked in the
same execution, its positive PM deltas too). The only preconditions are (a) the
operator encodes a path that swaps against the attacker pool, and (b) the
executor holds any WETH/ETH at the moment of the swap — both true for any
non-trivial arbitrage. The contract has **no reentrancy guard**, and
`POOL_MANAGER_ADDR` delta checks do **not** mitigate the executor-side theft.

Security boundary today: *the operator must only swap against canonical,
immutable, honest Uniswap V2/V3/V4 pools.* This is nowhere enforced by the
contract and is silently broken by pool-list poisoning (an attacker deploys a
bait-priced V2 pair; the operator's off-chain scanner picks it; the executor is
drained on first contact). The `POOL_MANAGER_ADDR` immutable is the only
trust-bounded external address; all V2/V3 pools come from `t_addresses`.

---

## Attack pattern (V2; V3 is structurally identical)

1. Operator encodes a path containing `V2_SWAP_COMPACT(pool_idx=K, ...)`.
   `_cmd_v2_swap_compact` executes:
   - `t_callback_packed = convert(pool_K, uint256) | (fee << 160)`
   - `extcall IUniswapV2Pair(pool_K).swap(out0, out1, recipient, forward_data)`.

2. `pool_K` is malicious. Inside its `swap()` it calls back into the executor:
   `executor.uniswapV2Call(sender, amount0Out, amount1Out, data=ATTACKER_STREAM)`.

3. Auth check in `uniswapV2Call`:
   `convert(msg.sender, uint256) == t_callback_packed % CALLBACK_FEE_SHIFT`.
   `msg.sender` == `pool_K` (it is the caller) == registered address. **Auth passes.**

4. `len(ATTACKER_STREAM) != 1`, so the auto-pay branch is skipped and
   `ATTACKER_STREAM` is processed as a command stream via `_execute_command_at`.

5. The attacker drives any command whose address arguments resolve to a
   populated `t_addresses` slot or a protocol sentinel. Crucially, `pool_K`'s
   own index `K` is in `t_addresses` (it was used to look up the pool).

## Drain primitives the attacker can invoke

| Command | Effect | Caught by PM? |
|---------|--------|----------------|
| `ERC20_TRANSFER(token=V4_WETH_SENTINEL, recipient=K, amount=full)` | Transfers executor's entire WETH balance to `pool_K`. **Direct WETH theft.** | No — executor-side ERC20 balance, outside PM delta view. |
| `WETH_WITHDRAW_ALL` then `SEND_ETH_ALL(recipient=K)` | Unwraps WETH→ETH, sends all ETH to `pool_K`. | No — ETH balance, outside PM view. |
| `WETH_WITHDRAW(amount)` | Unwraps any amount of WETH. | No. |
| `SEND_ETH(recipient=K, amount)` | Sends up to uint96 ETH to `pool_K`. | No. |
| `V4_TAKE(currency=WETH, recipient=K, amount)` (if PM unlocked in this execution) | Drains positive PM WETH delta to `pool_K`. | Partially — the now-missing delta may cause the outer `unlockCallback` to `CurrencyNotSettled`-revert, but only if the executor was supposed to settle it; if the attacker stole a delta the executor was about to `take` anyway, no revert and theft succeeds. |
| `V4_TAKE_DELTA`, `V4_SETTLE_DELTA`, `V4_MINT_COMPACT`, `V4_BURN_COMPACT` | Corrupt PM delta accounting / ERC6909 balances mid-path. | PM may catch some via `CurrencyNotSettled`, but executor-side ERC6909/WETH theft is not covered. |

The executor holds WETH/ETH at the attack moment because:
- V2's `swap()` does optimistic output transfer to `recipient` (often `SELF`)
  *before* invoking the callback — confirmed in `fake_uniswap_v2_pair.vy:212-230`.
- Accumulated profit from earlier swaps in the same `execute()` is still in the
  executor's WETH/ETH balance when later swaps fire.

## Why the existing auth does not prevent this

`t_callback_packed` is set to the pool address **before** the `extcall` to
`pool.swap()`. The whole point of V2/V3 flash callbacks is that the pool calls
back during that `extcall`. So "msg.sender == registered pool" is exactly the
condition the attacker satisfies — it cannot distinguish the legitimate single
callback from the attacker's callback, because they are literally the same call
(the pool is the caller, and it controls the `data` payload). A depth counter
does not help either: legitimate nested callbacks (V2 swap issued from inside
`unlockCallback`'s command loop) are structurally identical to the attack at
depth 2.

The **only** distinguisher between the legitimate callback and the attack is the
**content of `data`**: an honest pool echoes the `forward_data` the executor
passed to `swap()`; a malicious pool substitutes attacker commands.

---

## Recommended fix: pin the expected callback `data` via transient hash

Store `t_expected_cb_hash = keccak256(forward_data)` immediately before each
`extcall pool.swap()` (V2 and V3 compact/delta/direct paths). In every callback
entrypoint, assert `keccak256(msg.data_callback_field) == t_expected_cb_hash`
before processing.

This is the only robust distinguisher because it proves the pool returned the
data the executor gave it. An honest pool passes (it echoes `forward_data`); a
malicious pool fails (its `data` hashes differently). It also closes
**cross-callback reentry** (a V2 pool calling `uniswapV3SwapCallback`, etc.):
the stored hash is the V2 forward_data's hash, so the V3 callback's
`keccak256(attacker_data)` mismatches and reverts.

### Cost

- Hot path (every V2/V3 swap): +1 keccak256 (~30-60 gas) + 1 TSTORE (100) before
  the extcall; +1 keccak256 + 1 TLOAD (100) + 1 assert in the callback.
  ≈ **+260-320 gas per V2/V3 swap**. On a 3-hop (3 swaps) ≈ **+800-960 gas/path**.
- The auto-pay cases (V2 `data = 0xFE`, V3 `data = empty`) have a known hash that
  can be precomputed as a constant and stored without a runtime keccak — cuts
  ~30-60 gas per auto-pay swap. Worth doing since auto-pay dominates the
  benchmark paths.
- V4 `unlockCallback` is already auth'd to `POOL_MANAGER_ADDR` (trusted immutable)
  and needs **no** hash pin — V4 pool hooks cannot pass its `msg.sender == PM`
  check. Leave V4 as-is.

### Cheaper alternatives considered and rejected

- **Flat transient reentrancy lock** (`t_in_callback` set on entry, revert if set):
  rejected. Legitimate nested callbacks (V2 swap inside V4 `unlockCallback`) fire
  while a callback is already in flight, so a flat lock would block the canonical
  V4-unlock-then-V2-swap pattern. A *depth counter* also fails — legit nesting
  and the attack are both depth-2 callback-via-extcall.
- **Cross-callback-type guard** (track `t_expected_cb_type`; each callback
  asserts its own type): partially closes cross-callback reentry but does NOT
  close the same-type attack (V2 pool calling `uniswapV2Call` with attacker
  data). Not sufficient alone.
- **Re-derive continuation from stored offset** instead of trusting the pool's
  echoed `data`: impossible — `commands` is calldata of the `execute` frame; the
  callback is a separate call frame and cannot re-slice it. The pool-echoed
  `data` is a genuine design constraint, so the hash pin is the fix.

### Compatibility with H1

H1 (`withdraw()` reentrancy) is owner-gated and unrelated to callbacks; it
should get its own simple reentrancy lock (or restructure the WETH-withdraw +
raw_call ordering). Do NOT couple H1 and H2 to a shared primitive — they are
independent surfaces. (The H1 task body said "use the same lock mechanism if
introduced" — that suggestion is **withdrawn**: H1 should be fixed by
checks-effects ordering, H2 by the hash pin.)

---

## Reproduction notes for the H2 fix task

- The checksummed fake contracts (`fake_uniswap_v2_pair.vy`, etc.) are
  **OFF-LIMITS** and are honest-by-construction (they echo the passed `data`).
  They cannot reproduce this attack.
- The H2 reproduction test must create a **new, test-only malicious mock** V2
  pair (e.g. `tests/malicious_v2_pair.vy` or an in-test-deployed contract)
  whose `swap()` reenters `executor.uniswapV2Call` with an attacker command
  stream like `ERC20_TRANSFER(WETH=0xFE, recipient=its_own_idx, amount=full)`.
  Do not add this mock to `.auto/baseline_checksums.json`.
- Assert: pre-fix, the executor's WETH balance is drained to the malicious
  pool; post-fix (hash pin), the reentrant callback reverts and the legitimate
  swap path still completes on honest pools.

---

## Checkpoint — needs user input before H2 task is sized/fixed

1. **Pool universe guarantee**: Is the operator guaranteed to swap only against
   canonical, immutable Uniswap V2/V3/V4 pools (no PancakeSwap/Aerodrome/other
   forks, no upgradable or gauge-bearing pools)? If YES, the attack requires
   pool-list poisoning (still realistic for MEV) but the threat model narrows.
   If NO (operator uses forks), the attack is a direct operational risk.
2. **Gas budget**: The hash pin costs ~800-960 gas/path on the 27-path
   benchmark (auto-pay-optimized: less). Given AGENTS.md documents sub-100-gas
   fights, is this acceptable in exchange for closing a full-drain vector?
   Alternatives: (a) accept it; (b) apply the pin ONLY to non-auto-pay
   passthrough paths (cheaper, but leaves auto-pay reentry via cross-callback
   open — not recommended); (c) ship as-is and rely solely on operator pool
   hygiene (current state).
3. **V4**: confirm no hash pin is wanted on `unlockCallback` (already PM-gated).
