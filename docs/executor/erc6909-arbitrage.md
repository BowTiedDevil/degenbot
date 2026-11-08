# ERC6909 in Arbitrage Paths

> **For the full user guide** with V4_MINT/V4_BURN encoding examples and the
> decision matrix, see [`user-guide.md`](user-guide.md) §13. This document focuses
> on the transfer-count analysis and compounding strategies.

How V4_MINT and V4_BURN reduce ERC20 transfers by keeping tokens inside
the PoolManager as accounting entries, and when to use them.

---

## 1. What ERC6909 Gives You

The V4 PoolManager tracks all token movements as signed deltas in
transient storage. At the end of `unlock()`, every delta must be zero.

Two operations resolve deltas **without** physical ERC20 transfers:

| Operation | Replaces | ERC20 xfer cost | Mechanism |
|-----------|----------|-----------------|-----------|
| `V4_MINT` | `V4_TAKE` (positive delta → executor) | 0 vs 1 | Converts +delta into ERC6909 balance entry inside PM |
| `V4_BURN` | `sync + transfer + settle` (negative delta) | 0 vs 1 | Converts ERC6909 balance back into +delta |

**V4_MINT** saves 1 transfer when the executor would have taken tokens
out of PM (profit extraction).

**V4_BURN** saves 1 transfer when the executor would have sent tokens to
PM to settle a debit — but only if the executor already holds ERC6909
balance from a prior V4_MINT.

---

## 2. When V4_MINT Saves Transfers

### Only when V4_TAKE goes to the executor for profit

V4_TAKE has two uses in three-hop paths:

1. **Routing**: Send intermediate tokens to V2/V3 pools
   (e.g., `V4_TAKE(USDC, V2b)`, `V4_TAKE(WBTC, V3c)`)
   → **Cannot** use V4_MINT. V2/V3 need physical ERC20.

2. **Profit extraction**: Send WETH profit to executor
   (e.g., `V4_TAKE(WETH, executor, profit)`)
   → **Can** use V4_MINT. Executor holds profit as ERC6909 inside PM.

### Affected paths (zero-balance, single-tx)

| Path | Standard | V4_MINT | Savings |
|------|----------|---------|---------|
| V4-V4-V4 | 1 | **0** | 1 (profit as ERC6909) |
| V2-V4-V4 | 3 | **2** | 1 (profit as ERC6909) |
| V3-V4-V4 | 3 | **2** | 1 (profit as ERC6909) |
| All others | 3–4 | 3–4 | 0 (V4_TAKE is for routing, not profit) |

Only 3 of 27 paths benefit from V4_MINT in a single-tx scenario.
The other 24 paths use V4_TAKE for intermediate routing to V2/V3,
which requires physical ERC20 transfers.

---

## 3. When V4_BURN Saves Transfers

### Only when the executor already holds ERC6909

V4_BURN converts an existing ERC6909 balance into a +delta, which can
offset a -delta (debit). This saves the `sync + transfer + settle`
sequence that would normally fund the debit.

But the executor must ALREADY hold ERC6909 from a prior V4_MINT. In a
zero-balance single-tx scenario, the executor has no ERC6909 to burn.

### Multi-transaction compounding

The real power of V4_BURN emerges across multiple transactions:

```
TX1: V4-V4-V4 arbitrage
  → V4_MINT(WETH, executor, profit)    [0 xfers]
  → Executor holds 1 WETH as ERC6909

TX2: V4-V4-V4 arbitrage (or any V4 path with WETH debit)
  → V4_BURN(WETH, AMOUNT_WETH)         [0 xfers]
     Converts ERC6909 → +delta, covering V4a's WETH debit
  → V4_MINT(WETH, executor, profit)    [0 xfers]
  → Executor now holds ~2 WETH as ERC6909

TX3: Withdraw profits
  → V4_BURN(WETH, total_erc6909)       [0 xfers]
  → V4_TAKE(WETH, executor, amount)   [1 xfer — only the take]
```

**3 transactions, 1 ERC20 transfer** (vs 3 with V4_TAKE each time).

Over N V4 operations, an executor that uses MINT+BURN only needs
1 transfer for the final withdrawal, not 1 per transaction.

### Cross-path composability

ERC6909 WETH from a V4-V4-V4 operation (TX1) can fund a V4-V2-V4
operation (TX2):

```
TX1: V4-V4-V4 → V4_MINT profit WETH as ERC6909 [0 xfers]
TX2: V4-V2-V4 → V4_BURN to fund V4a's WETH input [0 xfers]
                   (saves the sync+transfer+settle that would
                    normally cost 1 ERC20 transfer)
```

---

## 4. WETH_DEPOSIT for Native ETH Funding

WETH_DEPOSIT wraps the executor's native ETH at WETH9:

```
WETH9.deposit{value=amount}()    → 0 ERC20 transfers
```

But the executor must still `sync + transfer(WETH→PM) + settle` to
credit the delta at PM. The `transfer(WETH→PM)` IS an ERC20 transfer.

**Net**: WETH_DEPOSIT doesn't reduce ERC20 transfer count — it reduces
the *cost of sourcing WETH*. Instead of swapping for WETH (gas + slippage),
the executor wraps its own ETH.

WETH_DEPOSIT is valuable when:
- The executor holds native ETH from MEV rewards or user deposits
- ETH is cheaper to obtain than WETH (no swap needed)
- The path requires multiple WETH settlements across transactions

---

## 5. Decision Matrix

```
                   ┌──────────────────────────────────────────┐
                   │ Where does V4_TAKE send its tokens?      │
                   ├──────────────┬─────────────┬─────────────┤
                   │  Executor    │  V2 pool     │  V3 pool    │
                   │  (profit)    │  (routing)   │  (routing)  │
  ┌────────────────┼──────────────┼──────────────┼─────────────┤
  │ Single tx,     │ V4_MINT ✓   │ Must be ✗    │ Must be ✗   │
  │ zero balance   │ Saves 1     │ physical      │ physical    │
  ├────────────────┼──────────────┼──────────────┼─────────────┤
  │ Multi-tx,      │ V4_MINT ✓   │ Must be ✗    │ Must be ✗   │
  │ has ERC6909    │ Saves 1     │ physical      │ physical    │
  │ from prior     │              │               │             │
  ├────────────────┼──────────────┼──────────────┼─────────────┤
  │ Need to settle │ V4_BURN ✓   │ N/A           │ N/A         │
  │ WETH debit,   │ Saves 1     │               │             │
  │ hold ERC6909   │ (vs settle) │               │             │
  ├────────────────┼──────────────┼──────────────┼─────────────┤
  │ Executor has   │ WETH_DEPOSIT│ N/A           │ N/A         │
  │ native ETH    │ Gas savings │               │             │
  │                │ (0 vs swap)  │               │             │
  └────────────────┴──────────────┴──────────────┴─────────────┘
```

---

## 6. Transfer Count Summary with ERC6909

| Path | Standard | +MINT profit | Multi-tx +BURN |
|------|----------|-------------|----------------|
| V4-V4-V4 | 1 | **0** | **0** (compound indefinitely, 1 at withdrawal) |
| V2-V4-V4 | 3 | **2** | **1** (burn settles USDC or WETH debit) |
| V3-V4-V4 | 3 | **2** | **1** (same) |
| V4-V*-V* | 3–4 | 3–4 | **2–3** (burn settles WETH input) |
| V2-V*-V* (no V4) | 4 | 4 | N/A |
| V3-V*-V* (no V4) | 4 | 4 | N/A |

---

## 7. Important Nuances

### ETH-funded V4-V4-V4 is NOT 0 transfers

A common misconception: WETH_DEPOSIT wraps native ETH → 0 ERC20
transfers for the whole path. But the executor still needs to
`transfer(WETH→PM) + settle` to credit the delta at PM. That's 1
ERC20 transfer.

In V4-V4-V4 specifically, delta netting already resolves the WETH
without any external payment. WETH_DEPOSIT + settle is UNNECESSARY
for V4-V4-V4 — the -1WETH from V4a is offset by +2WETH from V4c
within the delta accounting.

### V4_MINT amount = net delta, not profit

When the executor also deposits WETH for V4a's input (e.g., via
WETH_DEPOSIT + settle + V4a swap), the net delta includes both the
principal return and the profit. V4_MINT must consume the ENTIRE
net delta to zero it out.

Example with ETH funding:
```
Deposit + settle: +1 WETH delta
V4a swap:         -1 WETH delta (canceled by deposit)
V4c swap:         +2 WETH delta
Net:              +2 WETH delta
V4_MINT(2 WETH):  delta = 0 ✓    (includes 1 principal + 1 profit)
```

### ERC6909 is not a free lunch

Profit stored as ERC6909 is "trapped" inside PM. The executor must
eventually `V4_BURN + V4_TAKE` to get physical WETH (1 ERC20
transfer). Over a single arbitrage + withdrawal cycle, the total
ERC20 transfers are the same as using V4_TAKE directly.

The savings compound when the executor operates multiple V4 paths
between withdrawals: each intermediate operation saves 1-2 transfers
through MINT/BURN, and only pays the 1-transfer cost once at
withdrawal.

**The compounding effect extends beyond V4 paths.** When the PM is
used as a flash-loan source for non-V4 paths (see
[pm-as-bank.md](pm-as-bank.md)), ERC6909 from prior V4 operations
can fund the borrow via `V4_BURN` — eliminating the take+settle
transfer pair entirely. This creates a cross-protocol compounding
cycle where V4 profits fund V2/V3 operations and vice versa.

### Conservation checks and ERC6909

The balance conservation checks (see §9 of
[pool-mechanics.md](pool-mechanics.md)) track `token.balanceOf(account)`
for ERC20 tokens and `chain.provider.get_balance()` for native ETH.
They do **not** currently track `pm.balanceOf(account, currency_id)`
for ERC6909 internal balances.

This means:
- **Conservation remains correct**: When V4_MINT captures profit as
  ERC6909, no physical WETH is created or destroyed. The PM keeps
  the WETH (no transfer event), and the executor's ERC20 WETH
  balance doesn't change. ERC20 conservation still passes.
- **Profit is invisible**: The executor's gain exists as `pm.balanceOf()`,
  which neither `snapshot_balances` nor `_verify_conservation` tracks.
  The `expected_weth_delta` profit check will report 0 gain when
  the actual gain is in ERC6909.
- **Dedicated tests cover this**: The ERC6909 composability tests
  explicitly assert `pm.balanceOf(executor, weth_id)` changes,
  ensuring V4_MINT/V4_BURN work correctly even though the
  conservation framework doesn't see them.

---

## 8. Production Recommendations

1. **Use V4_MINT for profit** when the executor operates frequently
   in V4 pools. The 1-transfer savings per transaction compounds
   across many trades.

2. **Use V4_BURN for settlement** when the executor holds ERC6909
   from a prior V4_MINT. This eliminates sync+transfer+settle.

3. **Use WETH_DEPOSIT for funding** when the executor holds native
   ETH. It's cheaper (gas) than sourcing WETH from an external pool,
   even though it still requires 1 ERC20 transfer to PM.

4. **Don't use V4_MINT for routing** — V2/V3 pools need physical
   ERC20 tokens. Minting to a V2/V3 address gives them useless
   ERC6909 balance inside PM.

5. **Batch withdrawals** — Accumulate ERC6909 across many V4
   operations, then do a single V4_BURN + V4_TAKE for the entire
   accumulated balance.
