# Pool Mechanics & Ordering Constraints

A cheat-sheet for the timing and ordering rules that govern how
V2, V3, and V4 pools accept tokens. Violating any of these produces
reverts (K-invariant, IIA, CurrencyNotSettled, etc.) that are
cryptic to diagnose without understanding the underlying mechanism.

> **For a complete user guide** with command encoding examples and the full
> 2-hop and 3-hop path encyclopedia, see [`user-guide.md`](user-guide.md).

---

## 5.1 Reverse-Order Execution for V2 Chains

The same reverse-order principle that enables V3→V3 direct custody
also resolves the V2 callback-to-recipient constraint for V2→V2 edges.

**The problem**: V2's `swap(to=X)` calls `uniswapV2Call(X)`. When `X`
is another V2 pair, the callback lands on a contract that can't handle
it. Forward-order flash swaps with `to=V2b` fail.

**The solution**: Start with a flash borrow from the **last** pool
(V2c, V3c, or V4), then chain V2a→V2b via excess-balance V2 swaps
inside the callback. Both `V2_SWAP_CALC` and `V2_SWAP_DIRECT` achieve
this — they call `pair.swap(data=b"")` with no callback, relying on
excess balance from the pre-deposited tokens. The examples below use
`V2_SWAP_CALC` for clarity (computation on-chain), but the production
test suite uses `V2_SWAP_DIRECT` (pre-computed off-chain amounts) to
save ~4 staticcalls per swap.

```
V2c.swap(to=executor) → callback on executor:
  1. ERC20_TRANSFER WETH to V2a     (creates excess balance)
  2. V2_SWAP_CALC V2a → V2b         (sends USDC directly, no callback)
  3. V2_SWAP_CALC V2b → V2c         (sends WBTC directly, pays the flash borrow)
Return from callback → V2c K-invariant ✓
```

**Why this works**:
- V2c's callback goes to the **executor** (not V2b), so the
  callback-to-recipient constraint is never triggered.
- V2_SWAP_CALC calls `swap(to=V2b, data=b"")` — no callback on V2b.
  The output goes directly to V2b, creating excess USDC balance.
- The next V2_SWAP_CALC reads V2b's excess USDC as its input amount
  and sends WBTC directly to V2c (no callback on V2c either).
- Each V2 pair has its own reentrancy guard — V2a/V2b can swap while
  V2c is locked.
- V2's K-invariant only checks **total balances after the swap**, so
  tokens arriving via excess balance (not callback) satisfy K.

**Pool setup**: V2a/V2b must use `_setup_v2_for_calc` (reserves at
the correct price ratio) so that V2_SWAP_CALC computes amounts that
cascade correctly through the chain. V2c can use either `_setup_v2`
or `_setup_v2_for_calc` — the K-invariant passes regardless.

**Chain amount computation**: Since V2_SWAP_CALC computes output
on-chain from `getReserves()` + fee + excess balance, the actual
output amounts (a_out, b_out) are determined by V2 math. The last
pool's output (c_out) must be computed from the chain rather than
hardcoded, because the reserves and fee calculations may produce a
different amount than a constant like `AMOUNT_WETH_PROFIT`.

**Extensions**:
- **V2-V2-V3**: V3c fires first, V2a→V2b→V3c via V2_SWAP_CALC inside
  V3c's callback. V3c's IIA check passes because V2b deposits WBTC
  directly into V3c during the callback window.
- **V2-V2-V4**: V4 unlock fires first (take WETH profit), then
  V2a→V2b→executor via V2_SWAP_CALC inside unlock callback.
  Executor receives WBTC to settle the PM debit.

---

## 1. How Each Pool Verifies Payment

### V2 — K-invariant (total balances)

```
balance0Adjusted × balance1Adjusted ≥ reserve0 × reserve1 × 10000²
```

- **What it checks**: The product of the pair's *total token balances*
  (including any excess deposits not yet in reserves) satisfies the
  constant-product formula with fee deduction.
- **When it checks**: At the end of `swap()`, after the callback returns.
- **Implication**: V2 does not care *when* or *how* tokens arrived —
  only that the final balances are consistent. Tokens can be deposited
  before `swap()` (excess balance, no callback) or during the callback
  (flash swap). Both paths satisfy the K-invariant.

**⚠️ Callback-to-recipient constraint**: V2's `swap()` invokes
`uniswapV2Call` on the `to` address (the output recipient). This means
a flash swap with `to=V2b` sends the callback to V2b, which can't
process it. **V2→V2 direct custody via forward-order flash swap is
impossible.** However, reverse-order execution with excess-balance V2 swaps
bypasses this constraint entirely (see §5.1 below).

### V3 — IIA balance-delta check (incremental)

```
balance_before + amount_owed ≤ balance_after
```

- **What it checks**: The pool's input-token balance increased by at
  least the owed amount *between the two snapshots*.
- **When it checks**: `balance_before` is read at the start of
  `swap()`, *after* the optimistic output transfer but *before* the
  callback. `balance_after` is read after the callback returns.
- **Implication**: Tokens must arrive **during the callback window**
  (between the two balance reads). Tokens deposited *before* `swap()`
  are already in `balance_before` and do not help satisfy the check.
  This is the single most important constraint for V3 routing.

### V4 — Delta accounting (transient storage)

```
t_deltas[currency] must be zero for all currencies at end of unlock()
```

- **What it checks**: All token movements through the PoolManager are
  tracked as signed deltas in transient storage. At the end of
  `unlock()`, every currency's delta must be zero (all debts settled,
  all credits withdrawn).
- **When it checks**: After `unlockCallback()` returns.
- **Settlement sequence**: To resolve a **negative** delta (you owe PM):
  `sync(currency)` → send tokens to PM → `settle()`.
  - `sync()` snapshots `balanceOf(PM)` *before* the transfer.
  - You then transfer tokens to PM (or they arrive via another path).
  - `settle()` reads the new `balanceOf(PM)` and credits the delta.
- **Take sequence**: To resolve a **positive** delta (PM owes you):
  `take(currency, recipient, amount)` — physically transfers tokens
  from PM to `recipient`. Creates a negative delta to offset the
  positive one.

---

## 2. Sync/Settle Ordering

The V4 PoolManager uses a two-phase balance snapshot for settling:

```
sync(currency)       →  snapshots balanceOf(PM)
  ... tokens arrive at PM ...
settle()             →  delta = balanceOf(PM) - snapshot
```

| Step | Must happen | Must NOT happen |
|------|-------------|-----------------|
| `sync(currency)` | Before the token deposit to PM | After the deposit (would snapshot the high balance → delta=0) |
| Token deposit | After `sync()`, before `settle()` | Before `sync()` (deposit invisible to settle) |
| `settle()` | After the deposit | N/A |

**`sync()` may be called outside `unlock()`** — this is a critical
enabler for V3→PM direct custody (see §4 below).

**`V4_SETTLE_DELTA`** (opcode `0x56`) handles the full sequence
internally: reads the PM delta via exttload, then:
- Positive delta → `take()`
- Negative delta → `sync() + transfer(PM) + settle()`

This means `V4_SETTLE_DELTA` transfers tokens **from the executor**
to PM. If the executor doesn't hold the tokens (because V3 sent
directly to PM), `V4_SETTLE_DELTA` will fail. In that case, use
the separate `V4_SYNC` + `V4_SETTLE` commands instead, with `sync`
called before the V3 swap.

---

## 3. Token Flow Windows

The three pool types have fundamentally different windows for when
input tokens must arrive:

```
         ┌─────────────────────────────────────────────────────┐
  V2:    │  Before swap()   During callback   After callback   │
         │  ─────────────────────────────────────────────✓────  │
         │  K-invariant checks TOTAL balances — timing irrelevant│
         └─────────────────────────────────────────────────────┘

         ┌─────────────────────────────────────────────────────┐
  V3:    │  Before swap()   During callback   After callback   │
         │  ─────────────── ✗ ──────── ✓ ──────── ✗ ──────────  │
         │  IIA: tokens in balance_before don't count           │
         │  Tokens must arrive between the two balance reads    │
         └─────────────────────────────────────────────────────┘

         ┌─────────────────────────────────────────────────────┐
  V4:    │  Inside unlock() — sync/settle ordering matters     │
         │  sync BEFORE deposit, settle AFTER deposit          │
         │  All deltas must net to zero by end of unlock()     │
         └─────────────────────────────────────────────────────┘
```

---

## 4. Direct Custody Rules (Per Edge Type)

"Direct custody" = a pool sends its output directly to the next
pool (or PM), bypassing the executor. This saves 1 ERC20 transfer
per edge (no executor intermediary hop).

### WETH/Ether-Only Custody Invariant

A consequence of direct custody is that the executor **only ever
takes custody of WETH and Ether**. All non-WETH/ETH tokens (USDC,
WBTC, etc.) flow directly between pools — they never pass through
the executor's ERC20 balance.

This is a structural property of valid arbitrage paths: intermediate
assets are WETH/ETH exclusively because any path that delivered a
non-WETH/ETH token to the executor would require an extra transfer
to forward it onward, making it suboptimal.

**Implications for profit checking:** In V2/V3 callbacks,
`amount0 + amount1` is always the WETH (or ETH) inflow — the other
amount is zero, or both are the same token. No `token0()`/`token1()`
calls needed. The profit check only needs to account for WETH + ETH
balance changes, enabling arithmetic tracking via transient storage
instead of `balanceOf` reads.

| Edge | Direct? | Path | Reason |
|------|---------|------|--------|
| V2 → V2 | ✓ | Reverse-order: flash borrow from last pool, V2a→V2b via excess-balance V2 swap inside callback | Bypasses callback-to-recipient; swap with data=b"" sends directly, no callback |
| V2 → V3 | ✓‡ | V2a called inside V3b's callback: V2a.swap(to=V3b) sends output directly during callback (IIA ✓) | V3's IIA: tokens arriving before swap() are in balance_before; during callback ✓. See §4.4 |
| V2 → V4 | ✓ | V2 callback: transfer(USDC, PM) + sync + settle + V4 swap(s) | V2 callback has time to sync+send+settle before V4 |
| V3 → V2 | ✓ | V3 sends to V2 pair (recipient), V2 uses excess-balance swap | V3's IIA checks V3's own balance, not V2's; V2 K-invariant ✓ |
| V3 → V3 | ✓* | **Reverse-order only**: V3b→V3c during V3c callback (IIA ✓). Forward-order fails. | IIA: tokens must arrive during callback. Reverse-order guarantees this. |
| V3 → V4 | ✓** | V3a→PM + V4_TAKE→V3a during V3a callback (IIA ✓) | V4_TAKE is an ERC20 transfer that arrives during V3a's callback window |
| V4 → V2 | ✓ | V4_TAKE sends to V2 pair (recipient), V2 uses excess-balance swap | V2 K-invariant ✓ for excess balance |
| V4 → V3 | ✓† | **Reverse-order from V3**: V4_TAKE→V3b during V3b's **own callback** (IIA ✓). Forward-order fails. | V3's IIA satisfied when tokens arrive during its own callback window, even from V4_TAKE |
| V4 → V4 | ✓ | Delta netting — 0 internal transfers | Same unlock: deltas cancel in transient storage |

\* V3→V3 reverse-order: top-level call is the **last** pool (V3c),
inner pools execute inside callbacks. Each pool's output goes to the
pool that needs it as input *during that pool's callback*. See §5.

\*\* V3→V4 with V4_TAKE-to-V3a: the `sync` for the V3→PM deposit
must be called **before** the V3 swap starts (top-level commands),
so that `settle()` inside the V4 unlock correctly credits the delta.
See §2.

† V4→V3 **reverse-order callback IIA**: V3's IIA only blocks
V4→V3 in **forward order** (V4_TAKE sends tokens to V3 *before*
V3.swap() starts, so they're captured in balance_before and don't
help). But if V3's swap has already started (we're in V3's callback),
V4_TAKE→V3 deposits tokens *during* the callback window — they
appear in balance_after but not balance_before, satisfying IIA. ✓

This is the key insight that unlocked V4-V3-V2, V4-V3-V3, V4-V3-V4,
V2-V4-V3, and V4-V2-V3 from 6→4 or 5→4 transfers. The same principle
applies to V2/V4_TAKE→V3c during V3c's callback (V3c-reverse pattern).

‡ V2→V3 **V2 inside V3 callback**: V2's `swap(to=recipient)` can
send output to V3b directly, but only if V2 is called from *inside*
V3b's callback (so the output arrives in the IIA window). V2 uses
an excess-balance swap with `data=b""` (no callback) because the executor pre-deposits
excess balance before calling V2. This eliminates the executor
intermediary on the V2→V3 edge. See §4.4.

### 4.1 Multi-Edge Optimizations

Some three-hop paths combine direct-custody edges in ways that
eliminate **multiple** intermediary transfers:

**V3-V2-V3 (6→4)**: Reverse-order from V3c. V3c→exec (WETH profit),
then inside V3c's callback: V3a→V2b direct (USDC, creating excess
at V2b), then V2b excess-balance swap→V3c (WBTC, satisfying V3c IIA).
The V2b→V3c direct custody eliminates executor intermediary on
the B→C edge, and the V3a→V2b direct eliminates it on the A→B edge.

**V4-V2-V4 (6→4)**: Single V4 unlock. V4a swap (0 internal) →
V4_TAKE USDC→V2b direct (creating excess) → V2b excess-balance V2 swap→exec
(sends WBTC to executor) → V4c swap (consumes WBTC via delta after
settle) → V4_SETTLE_DELTA for remaining currencies. The V4_TAKE→V2b
direct custody + V2b excess-balance V2 swap (no callback) eliminates both the
V2 flash swap callback overhead and the WBTC executor intermediary.

**V3-V3-V2 (4 transfers)**: V3b fires first with V3a→V3b direct (reverse-order
skips USDC auto-pay). V2c V2_SWAP_DIRECT reads excess WBTC from V3b.
Same reverse-order pattern as V3→V3 direct custody, extended to a
terminal V2 pool.

**V4-V3-V4 (6→3)**: Single V4 unlock. V4a swap → V3b with
V4_TAKE_DELTA USDC→V3b in V3b's forward_data (IIA ✓ during callback).
V3b sends WBTC to PM (delta netting via sync+settle). V4c swap
consumes WBTC via delta. V4_TAKE_DELTA WETH→exec (profit). The V4_TAKE_DELTA→V3b
inside the callback eliminates V3b's separate IIA payment, and V3b→PM delta
netting eliminates the WBTC executor intermediary. USDC and WBTC deltas
net to zero internally (no physical transfers).

**V4-V2-V3 (6→4)**: V3c-reverse. V3c fires first. Inside V3c
callback: V4 unlock with V4a swap → V4_TAKE USDC→V2b (direct
custody, creating excess) → V2b excess-balance V2 swap→V3c (sends WBTC
to V3c during V3c's callback — IIA ✓). V4_SETTLE_DELTA WETH.
The V3c-reverse pattern converts both IIA blockers (V4→V3, V2→V3)
into satisfiable constraints during V3c's own callback.

**V2-V4-V3 (6→4)**: V3c-reverse. V3c fires first. Inside V3c
callback: ERC20 WETH→V2a (creates excess) → V4 unlock:
V4_SYNC(USDC) → V2a V2_SWAP_DIRECT→PM (USDC delta netting,
with V4_SETTLE after deposit to credit delta)
→ V4b swap → V4_TAKE WBTC→V3c (IIA ✓). Same
V3c-reverse insight as V4-V2-V3.

**V3-V4-V3 (6→4)**: V3c-reverse. V3c fires first. Inside V3c
callback: V3a swap (USDC→PM) with V3a forward_data containing
ERC20 WETH→V3a (explicit auto-pay, IIA ✓) + V4 unlock (V4b swap
+ V4_TAKE WBTC→V3c during V3c callback, IIA ✓). V4_SYNC(USDC)
before V3c swap to capture V3a→PM deposit for delta netting.

**V4-V3-V2 (6→4)**: Single V4 unlock. V4a swap → V3b with
V4_TAKE USDC→V3b in V3b's forward_data (IIA ✓ during callback).
V3b sends WBTC to V2c (direct custody). V2c excess-balance V2 swap→exec.
V4_SETTLE_DELTA WETH.

**V4-V3-V3 (6→4)**: Single V4 unlock. V4a swap → V3c→V3b
reverse-order nested inside V3c's callback. V3b's forward_data
contains V4_TAKE USDC→V3b (IIA ✓ during V3b's callback, eliminating
auto-pay). Merged WETH profit+settle: sync, then single
ERC20_TRANSFER of full AMOUNT_WETH to PM, then settle (credits
both profit principal and flash-loan repayment in one transfer).

**V2-V3-V2 (6→4)**: Reverse-order from V2c. V2c fires first
(flash swap, WETH profit to executor). Inside V2c callback:
V3b swap (WBTC→V2c direct custody), V3b callback: ERC20
WETH→V2a + V2a V2_SWAP_DIRECT→V3b (USDC arrives during V3b callback,
satisfying IIA). V2c K-invariant satisfied by WBTC from V3b.

**V2-V4-V2 (6→4)**: Reverse-order from V2c. V2c fires first
(flash swap, WETH profit to executor). Inside V2c callback:
ERC20 WETH→V2a + V2a V2_SWAP_DIRECT→PM (USDC delta netting for V4b)
+ V4 unlock: V4_SYNC+V4_SETTLE+V4b swap+V4_TAKE WBTC→V2c.
V2a→PM eliminates both V2a→executor USDC and V4_SETTLE_DELTA USDC.

### 4.2 V4_TAKE Direct-to-Recipient

When a V4 pool's output token is needed as input by another pool,
V4_TAKE can send tokens directly to the next pool's address instead
of routing through the executor. This saves 1 ERC20_TRANSFER.

| V4_TAKE recipient | When it works | Constraint |
|--------------------|---------------|------------|
| V2 pair | Always (V2 K-invariant checks total balances) | Creates excess balance → excess-balance V2 swap reads it |
| V3 pool (during callback) | V3's IIA satisfied because tokens arrive during callback | Must happen during V3's own callback (not before swap()) |
| PM | Delta netting — credits +delta consumed by next swap | Must sync before the external deposit, settle after |
| Executor | General fallback for profit capture | No optimization, baseline path |

### 4.3 Profit-Capture and Reverse-Order Decoupling

For on-chain arbitrage, the executor contract must end up holding
the profit token. Initially, V2-V3-V2 and V2-V4-V2 seemed stuck at
5 because the WETH profit seemed trapped:

- **V2-V3-V2**: V2c→V2a via excess-balance V2 swap sends all WETH to V2a,
  but the executor needs the profit WETH.
- **V2-V4-V2**: Same — V2c→V2a traps profit.

However, **reverse-order from V2c** decouples profit capture from
V2a's K-invariant:

- V2c fires first as a **flash swap** (to=executor), sending WETH
  profit directly to the executor.
- Inside V2c's callback, a **separate** ERC20_TRANSFER sends WETH
  to V2a, creating excess for the V2 swap.
- V2a's K-invariant is satisfied by the excess WETH, and V2a's
  The V2 swap output (USDC) goes to the next pool (V3b or PM).

This pattern works because V2's K-invariant checks **total balances**
after the swap — it doesn't care whether the WETH arrived as flash-swap
repayment or as an excess deposit. The executor gets the profit WETH
from V2c's flash swap, while V2a gets its own WETH from the separate
transfer. Two different sources, two different purposes.

### 4.4 V2 Inside V3 Callback: Bypassing the Executor Intermediary

When a path has a V2→V3 edge, the naive approach sends V2's output
to the executor, then the executor forwards it to V3 (2 transfers).
But V2's `swap(to=recipient)` can send output to **any address** —
including V3b. The challenge is V3's IIA: tokens must arrive *during*
V3b's callback, not before `swap()` starts.

**The insight**: if V2 is called from *inside* V3b's callback,
V2's optimistic output lands at V3b in the IIA window.

```
Naive (2 transfers):                  Optimized (1 transfer):
  V2a.swap(to=executor)  [T1]          V3c.swap(to=executor)  [T1]
    V3c callback:                         V3c callback:
      V3b callback:                         V3b callback:
        ERC20 executor→V3b   [T2]             ERC20 WETH→V2a     [T2] (excess)
                                              V2a.swap(to=V3b)   [T3] ← IIA ✓
```

V2a is called with `V2_SWAP_DIRECT` (no callback needed) because
the executor pre-deposits excess WETH at V2a. V2a's optimistic
USDC transfer to V3b happens *during V3b's callback* — between
`balance_before` and `balance_after` — satisfying IIA. ✓

This applies whenever V2 appears **before** V3 in the hop sequence:

| Path | Before | After | Key restructure |
|------|--------|-------|-----------------|
| V2-V3-V3 | 5 (V2a→exec + exec→V3b) | **4** (V2a→V3b during callback) | V3c outermost, V2a inside V3b callback |
| V2-V3-V4 | 5 (V2a→exec + exec→V3b) | **4** (V2a→V3b during callback) | V3b outermost, V4 provides WETH to V2a, V2a→V3b |
| V4-V3-V3 | 5 (profit→PM + settle_delta→PM) | **4** (merged single WETH→PM) | Sync before transfer; settle once for full amount |

The V4-V3-V3 case is slightly different: V4_TAKE already satisfies
V3b's IIA. The extra transfer came from **two separate WETH→PM
transfers** (profit capture via `ERC20_TRANSFER`, then `settle_delta`
for the remaining debt). Merging into one `ERC20_TRANSFER` of the
full `AMOUNT_WETH` + `sync` + `settle` eliminates the second
transfer. The `sync` must precede the `ERC20_TRANSFER` so that
`settle()` sees the balance delta correctly.

**General principle**: whenever V2's output feeds V3's input, call
V2 from inside V3's callback with `to=V3`. V2's optimistic transfer
becomes V3's IIA payment — one transfer serves two purposes.

---

## 5. Reverse-Order Execution for V3 Chains

When V3 pools are chained (V3a→V3b→V3c), forward-order direct
custody fails because V3a's output arrives at V3b *before* V3b.swap()
is called — it's captured in V3b's `balance_before` and doesn't help
the IIA check.

**Reverse-order** solves this:

```
Top-level: V3c.swap(recipient=executor)
  V3c callback:
    V3b.swap(recipient=V3c)       ← WBTC arrives at V3c during callback → IIA ✓
    V3b callback:
      V3a.swap(recipient=V3b)     ← USDC arrives at V3b during callback → IIA ✓
      V3a callback: auto-pay WETH  ← executor pays WETH to V3a
```

As callbacks unwind, each pool's IIA check passes because the inner
pool's output arrived *during* the outer pool's callback window.

**Transfers: 4** (V3c→executor, V3b→V3c, V3a→V3b, executor→V3a)
vs **6** in the naive (all-via-executor) approach.

**Extension to mixed V3-V3 paths**: The same reverse-order pattern
works when the V3 chain is part of a larger path:
- **V2-V3-V3**: V3c outermost, V2a called inside V3b callback with to=V3b (IIA ✓)
- **V3-V3-V4**: V3b sends WBTC to PM, V4_TAKE sends WETH to V3a (IIA ✓)
- **V3-V3-V2**: V3b sends WBTC to V2c (excess + V2_SWAP_DIRECT ✓)

For V3-V3-V2 and V3-V3-V4, the final non-V3 pool (V2c or PM)
receives tokens from the outermost V3 pool, and the V4_TAKE or
V2c output can be directed to V3a during its callback.

---

## 6. V4 Delta Netting

When multiple V4 swaps execute inside a single `unlock()`, their
deltas cancel in transient storage:

```
V4a: WETH→USDC    delta: -1WETH +2000USDC
V4b: USDC→WBTC    delta: +1WETH -2000USDC +100WBTC -100WBTC
V4c: WBTC→WETH    delta: ... -100WBTC +2WETH
─────────────────────────────────────────────
Net:  +1WETH (profit)
```

Only the net profit needs `V4_TAKE`. All intermediate deltas cancel
automatically. This is why V4-V4-V4 needs only 1 transfer total
(V4_TAKE profit + V4_SETTLE WETH).

---

## 7. V4 Reentrancy Constraints

- `unlock()` acquires a lock. Calling `unlock()` again inside the
  callback raises `AlreadyUnlocked` (the guard checks `t_unlocked`).
- **Implication**: For paths where V4 is both the first and last pool
  (e.g., V4-V2-V4), all V4 swaps must be in the **same** unlock block.
  The V2 swap happens inside the unlock callback. The second V4 swap
  is a direct `V4_SWAP` command (not wrapped in another `V4_UNLOCK`).

---

## 9. Balance Conservation Verification

### What the checks assert

After each three-hop swap, the test suite runs three conservation
invariants that catch bugs invisible to the K-invariant/IIA/delta
checks:

1. **ERC20 conservation** (USDC, WBTC): The sum of balance changes
   across all tracked accounts is zero. No token is created or
   destroyed by the swap.

2. **WETH+ETH conservation**: WETH and native ETH are fungible
   (wrapping/unwrapping moves between them). Their combined balance
   changes across all tracked accounts must sum to zero.

3. **Executor profit** (optional): When `expected_weth_delta` is
   provided, the executor's combined WETH+ETH balance must have
   changed by exactly that amount. This checks correct distribution,
   not just conservation.

### What they catch vs. what they don't

| Bug type | Conservation | Profit check |
|----------|-------------|--------------|
| Tokens sent to untracked address (leak) | ✓ | ✗ |
| Tokens double-counted (same transfer twice) | ✓ | ✗ |
| Tokens lost in transit (no recipient) | ✓ | ✗ |
| Tokens went to wrong tracked account | ✗ | ✓ |
| V3 IIA unsatisfied (revert) | ✗ (tx reverted) | ✗ (tx reverted) |
| V4 delta not settled (revert) | ✗ (tx reverted) | ✗ (tx reverted) |

**Key insight**: Conservation can only fail when tokens move to an
**untracked** address. If all tokens stay between tracked accounts,
conservation always holds — no ERC20 token is created or destroyed
by legitimate swap operations. The conservation check catches "leaks"
(tokens sent to addresses the test wasn't monitoring), while the
profit check catches "wrong distribution" (tokens went to the wrong
tracked account).

On-chain reverts (K-invariant, IIA, CurrencyNotSettled) are the
**first line of defense** — they catch most structural bugs before
the conservation checks even run. The conservation checks are the
**second line** — they catch bugs where the transaction succeeds but
tokens end up in the wrong place.

### Negative tests

`tests/test_conservation.py` deliberately injects bugs into command
streams and verifies the conservation check catches them:

| Test | Bug injected | Conservation result |
|------|-------------|-------------------|
| V4_TAKE to untracked address | WETH profit sent to `owner_account` | WETH+ETH conservation violated ✓ |
| Stray ERC20_TRANSFER to untracked | Extra WETH transfer to `owner_account` inside unlock callback | WETH+ETH conservation violated ✓ |
| V4_TAKE to wrong tracked address | WETH profit sent to V2 pair (tracked) | Conservation passes; **profit check fails** ✓ |
| Stray WETH transfer in V2 callback | WETH transfer to `owner_account` after swap chain | WETH+ETH conservation violated ✓ |

### ERC6909 tracking gap

When profit is captured via `V4_MINT_COMPACT` (ERC6909) instead of
`V4_TAKE`, the executor's WETH balance doesn't change — the value
exists as `pm.balanceOf(executor, weth_id)` inside the PoolManager.
The current `snapshot_balances` does not track ERC6909 balances.

This is **not** a false negative for conservation: no physical WETH
is created or destroyed, so conservation still passes correctly.
However, the executor's profit is invisible to both conservation
and profit checks. A future enhancement should add `pm.balanceOf()`
to `snapshot_balances` for full ERC6909 coverage.

ERC6909 composability tests (see
[docs/erc6909-arbitrage.md](erc6909-arbitrage.md)) verify
correct V4_MINT/V4_BURN behavior through separate dedicated tests
that explicitly check `pm.balanceOf()`.

For PM-as-bank strategies (using take() as a zero-fee flash-loan
source for non-V4 paths), see
[docs/pm-as-bank.md](pm-as-bank.md).

---

## 8. Transfer Count Summary

> See also [`user-guide.md`](user-guide.md) Appendix D for the complete 2-hop and
> 3-hop transfer count tables with technique descriptions.

### Per-edge costs

| Edge | Naive (via executor) | Optimized (direct custody) | Savings |
|------|---------------------|---------------------------|---------|
| V2→V2 | 2 (out+in) | 1 (excess-balance V2 swap, reverse-order) | 1 |
| V2→V3 | 2 | 1 (V2 inside V3 callback, to=V3b) ‡ | 1 |
| V2→V4 | 2 + sync+settle | 1 + sync+send+settle | 1 |
| V3→V2 | 2 | 1 (excess-balance V2 swap) | 1 |
| V3→V3 | 2 | 0 (reverse-order) | 2 |
| V3→V4 | 2 + take+settle | 0 (V3→PM, V4_TAKE→V3a) | 2 |
| V4→V2 | 1 (take) + 1 (pay-in) | 1 (V4_TAKE→V2, excess-balance V2 swap) | 1 |
| V4→V3 | 1 (take) + 1 (pay-in) | 1 (V4_TAKE→V3 during callback) † | 1 |
| V4→V4 | 0 (delta netting) | 0 | 0 |

† V4→V3 direct custody is possible via **reverse-order callback IIA**:
V4_TAKE sends tokens to V3 during V3's own callback (not before
V3.swap() starts). This satisfies V3's IIA because the balance
increase happens between balance_before and balance_after. The
"V4→V3 IIA ✗" constraint only applies in **forward-order**.

### Full three-hop transfer counts

| Path | Naive | Optimized | Savings | Techniques |
|------|-------|-----------|---------|------------|
| V2-V2-V2 | 6 | **4** | 2 | Reverse-order flash borrow, excess-balance V2 swap chain |
| V2-V2-V3 | 6 | **4** | 2 | Reverse-order, V2a→V2b→V3c via excess-balance V2 swap |
| V2-V2-V4 | 6 | **4** | 2 | V4_TAKE WETH→V2a direct, V2b→PM delta netting |
| V2-V3-V2 | 6 | **4** | 2 | Reverse-order V2c: V3b→V2c + V2a→V3b during V3b callback (IIA ✓) |
| V2-V3-V3 | 6 | **4** | 2 | V2a inside V3b callback (to=V3b), IIA ✓ during callback |
| V2-V3-V4 | 6 | **4** | 2 | V3b outermost, V2a→V3b during callback (IIA ✓), V3b→PM |
| V2-V4-V2 | 6 | **4** | 2 | Reverse-order V2c: V2a→PM + V4 delta netting + V4_TAKE→V2c |
| V2-V4-V3 | 6 | **4** | 2 | V3c-reverse: V2a→PM, V4_TAKE WBTC→V3c during callback |
| V2-V4-V4 | 6 | **3** | 3 | V4_TAKE→V2a direct, V2a→PM via V2_SWAP_DIRECT |
| V3-V2-V2 | 6 | **4** | 2 | Reverse-order, V3a→V2b, V2b→USDC calc |
| V3-V2-V3 | 6 | **4** | 2 | V3c-reverse: V3a→V2b, V2b V2_SWAP_DIRECT→V3c |
| V3-V2-V4 | 6 | **4** | 2 | V4_TAKE WETH→V3a direct, V2b→PM inside unlock |
| V3-V3-V2 | 6 | **4** | 2 | V3b reverse-order, V3a→V3b direct |
| V3-V3-V3 | 6 | **4** | 2 | V3c→V3b→V3a reverse-order (all direct custody) |
| V3-V3-V4 | 6 | **4** | 2 | V3a→V3b reverse, V3b→PM, V4_TAKE→V3a |
| V3-V4-V2 | 6 | **4** | 2 | V3a→PM, V3→PM + settle |
| V3-V4-V3 | 6 | **4** | 2 | V3c-reverse: V4_TAKE WBTC→V3c during V3c callback |
| V3-V4-V4 | 6 | **3** | 3 | V3a→PM, V4_TAKE→V3a directly |
| V4-V2-V2 | 6 | **4** | 2 | Reverse-order, V4_TAKE→V2a |
| V4-V2-V3 | 6 | **4** | 2 | V3c-reverse: V4_TAKE→V2b + V2b→V3c during callback |
| V4-V2-V4 | 6 | **4** | 2 | V4_TAKE→V2b direct, V2b V2_SWAP_DIRECT, delta netting |
| V4-V3-V2 | 6 | **4** | 2 | V4_TAKE USDC→V3b during V3b callback, V3b→V2c |
| V4-V3-V3 | 6 | **4** | 2 | V4_TAKE USDC→V3b (IIA ✓), merged WETH profit+settle |
| V4-V3-V4 | 6 | **3** | 3 | V4_TAKE_DELTA USDC→V3b during callback, V3b→PM delta netting |
| V4-V4-V2 | 5 | **3** | 2 | V4_TAKE→V2 directly, delta netting |
| V4-V4-V3 | 5 | **3** | 2 | V4_TAKE→V3 during V3 callback |
| V4-V4-V4 | 2 | **1** | 1 | Delta netting + V4_TAKE net profit only |

**Summary**: ALL 27 paths at **4 transfers or fewer**.
Total savings: 56 transfers from 156 naive (35.9% reduction).
ERC6909 can reduce this further for V4-ending paths — see
[docs/erc6909-arbitrage.md](erc6909-arbitrage.md).
