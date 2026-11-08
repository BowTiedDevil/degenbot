# PoolManager as Universal Flash-Loan Bank

> **For the full user guide** with command encoding examples and process descriptions,
> see [`user-guide.md`](user-guide.md). This document focuses on the PM-as-bank
> strategy and its trade-offs.

Can Uniswap V4's PoolManager (PM) act as a zero-fee source of **any**
token for **any** arbitrage path — even paths that don't include a V4
pool? Mechanically yes, and it's zero-fee, but it doesn't reduce
transfer counts for non-V4 paths. It does, however, enable novel
strategies when the executor operates frequently on V4.

Test results (see `tests/test_pm_as_bank.py`):

| Path | MINT xf | MINT gas | TAKE xf | TAKE gas | Gas diff |
|------|---------|----------|---------|----------|----------|
| V2-V2-V2 | 4 | 246,327 | 5 | 225,009 | +21,318 |
| V2-V3-V2 | 4 | 225,014 | 5 | 203,842 | +21,172 |
| V3-V2-V2 | 4 | 224,985 | 5 | 203,795 | +21,190 |
| V3-V3-V2 | 4 | 203,892 | 5 | 182,574 | +21,318 |
| V2-V2-V3 | 5† | 198,716 | n/a | n/a | n/a |

† V3-ending paths: profit is physical WETH at executor (not ERC6909).

**MINT is ~21k gas more expensive than TAKE** (ERC6909 bookkeeping
overhead), but saves 1 ERC20 transfer. Net savings depend on whether
the saved transfer would exceed 21k gas.

---

## 0. The 21k MINT Premium: Cold SSTORE on `erc6909_balance_of`

### Why MINT costs more

`PM.mint()` writes to `erc6909_balance_of[executor][weth_id]` — a
**cold storage slot** (never accessed in the transaction). A cold
SSTORE from zero to non-zero costs **22,100 gas**.

`PM.take()` calls `WETH.transfer()`, which writes to
`WETH.balanceOf(PM)` and `WETH.balanceOf(executor)` — both **warm**
(touched by sync/settle earlier in the transaction). Each warm
SSTORE costs ~5,000 gas.

The remaining ~4.2k premium after accounting for the cold SSTORE
penalty is from: 1 extra log event (`IERC6909Claims.Transfer`
at ~3k each) minus the `WETH.transfer` extcall overhead that TAKE
pays (~1.8k).

### Measured gas comparison (V2-V2-V2)

| Config | MINT | TAKE | MINT premium |
|--------|------|------|-------------|
| Cold (first MINT) | 246,442 | 225,136 | +21,306 |
| Cold + access list | 292,854 | 288,736 | +4,118 |
| Warm (compound MINT) | 229,354 | 225,136 | +4,218 |
| Warm + access list | 292,854 | 288,736 | +4,118 |

### Why compounding eliminates the premium

The MINT premium shrinks from **+21,306 → +4,218 gas** when
`erc6909_balance_of` already holds a non-zero value from a prior
MINT (the "compounding" scenario). The slot transitions from
non-zero → larger-non-zero (warm dirty SSTORE at 2,900 gas) instead
of zero → non-zero (cold SSTORE at 22,100 gas).

**Savings: 17,088 gas from compounding alone.**

Access lists achieve a similar premium reduction (+4,118) by
pre-warming the slot, but the net TX cost is **46k gas HIGHER** due
to the 33 storage keys at 1,900 gas each in the access list
overhead. Access lists only win when bundled across multiple
transactions sharing the AL cost.

### Why access lists cost more than they save

The access list includes 33 storage keys from 7 contracts:

| Contract | Keys | Slots |
|----------|------|-------|
| V2 pairs (×3) | 8 each = 24 | `token0`, `token1`, `callback_variant`, `amount_in`, `amount_out`, `unlocked`, packed `reserve0+reserve1+timestamp`, +1 |
| WETH | 4 | `balanceOf` for PM, executor, V2a, V2c |
| USDC | 2 | `balanceOf` for V2a, V2b |
| WBTC | 2 | `balanceOf` for V2b, V2c |
| V4_PM | 1 | `erc6909_balance_of` (the hot slot!) |
| Executor | 0 | Pure transient (tstore) — no storage slots |
| **Total** | **33** | |

At 2,400/address + 1,900/key, the upfront cost is 79,500 gas. These
keys are genuinely touched during execution (V2 reserves, token
balances, the erc6909 slot), so the AL does save the equivalent
cold-access gas. But the savings are **within the same transaction**
where the EVM would have auto-loaded them anyway (they become warm
on first touch). The AL converts cold (2,100 gas) to warm (100 gas)
on first touch, saving ~2,000 per key. For 33 keys, that's ~66,000
gas saved — but the 79,500 upfront cost exceeds it by 13,500 gas.

Access lists win when: (a) the same AL is reused across multiple
transactions in a bundle (amortizing the upfront cost), or (b) the
transaction includes many delegate calls that would redundantly
cold-load the same slots.

### Practical implications

- **First MINT**: pays the 21k cold-SSTORE penalty. Still saves 1
  ERC20 transfer (~50k gas in production), netting ~29k saved.
- **Subsequent MINTs** (compounding): only 4.2k premium over TAKE.
  MINT saves 1 transfer (~50k) at 4.2k cost → **net ~46k saved**.
- **Access lists**: not worthwhile for single transactions.
  Compounding captures the same savings without the AL overhead.

See `tests/test_gas_mint_vs_take.py` for the full measurement code.

---

## 1. No Prefunding Required

The executor requires **zero initial capital**. Every token needed for an
arbitrage path is borrowed atomically within the same transaction:

- **V2/V3 paths**: flash swaps provide the input tokens inside the swap
callback; repayment is enforced by the K-invariant (V2) or IIA
balance-delta check (V3). No prefunded balance needed.
- **V4 paths**: the PoolManager's `take()` lends tokens from its own
reserves (aggregated across all V4 pools) at zero fee, enforced by
the `CurrencyNotSettled` revert at end of `unlock()`. No prefunded
balance needed.
- **Mixed paths**: the same mechanisms compose — PM `take()` can fund
the first V2/V3 swap, and the swap chain's output repays the PM
before `unlock()` returns. No prefunded balance needed.

This is a deliberate design property, not an accident. An executor
that requires prefunding carries capital-at-rest risk (stuck funds,
misrouted profits, griefing via forced ETH deposits). The self-capitalizing
architecture means the contract can be deployed with zero balance and
execute profitable arbitrage paths immediately.

**The one command that breaks this property is `V3_SWAP_DELTA`**, which
requires the executor to hold ERC-20 tokens before the auto-pay callback
fires. This is documented as a known limitation; the standard V4→V3 flow
uses `V4_TAKE_DELTA` + `V3_SWAP_COMPACT` with auto-pay instead, which
preserves the no-prefunding guarantee.

---

## 2. The Core Mechanism: `take()` Without a V4 Swap

`take(currency, recipient, amount)` requires only that `t_unlocked`
is true (i.e., we're inside `unlock()`). It does **not** require a V4
swap to have occurred. This means:

```
unlock():
  take(WETH, V2a, 1e18)       ← borrow from PM, send directly to V2a
  V2a V2_SWAP_CALC(to=V2b)    ← normal V2 swap
  V2b V2_SWAP_CALC(to=V2c)    ← normal V2 swap
  V2c V2_SWAP_COMPACT(fee=30, to=PM)  ← output goes to PM (settlement path)
  sync(WETH)
  settle()                     ← +AMOUNT_WETH_PROFIT delta, nets with -1e18
  V4_TAKE(WETH, executor, Δ)  ← or V4_MINT (0 transfers)
```

The PM lends 1 WETH (via `take`) and receives AMOUNT_WETH_PROFIT from
V2c (via `settle`). The net delta is +PROFIT WETH — the executor's
arbitrage profit, which it extracts via `V4_TAKE` or `V4_MINT`.

**No V4 swap in the entire path.** PM is purely a bank.

---

## 3. Transfer Count Comparison

### V2-V2-V2 (no V4 pools)

| Step | Current (V2 flash swap) | PM-as-bank |
|------|-------------------------|------------|
| 1 | V2c→executor (WETH flash swap) | PM→V2a (take, WETH) |
| 2 | executor→V2a (WETH, excess) | V2a→V2b (USDC, swap) |
| 3 | V2a→V2b (USDC, swap) | V2b→V2c (WBTC, swap) |
| 4 | V2b→V2c (WBTC, swap) | V2c→PM (WETH, repayment) |
| 5 | — | PM→executor (take profit) or mint (0) |
| **Total** | **4** | **4–5** |

PM-as-bank ties at 4 (with V4_MINT for profit) but requires ERC6909
profit capture. With V4_TAKE for physical profit, it's 5 (worse).

**Why it can't beat 4**: V2's flash swap conveys two purposes in one
transfer — it creates the executor's profit *and* starts the chain.
PM-as-bank separates these: `take()` creates the input for V2a, and
a separate action captures the profit.

### V2-V3-V2 (with IIA constraint)

| Step | Current | PM-as-bank |
|------|---------|------------|
| 1 | V2c→executor (WETH flash) | PM→V2a (take) |
| 2 | V3b→V2c (WBTC, direct) | V2a→V3b (USDC, in callback) |
| 3 | V2a→V3b (USDC, in callback) | V3b→V2c (WBTC, direct) |
| 4 | executor→V2a (WETH, explicit) | V2c→PM (WETH, repayment) |
| 5 | — | PM→executor (profit) or mint |
| **Total** | **4** | **4–5** |

Same story — tie with V4_MINT, loss with V4_TAKE.

### General pattern

For any non-V4 path optimized with reverse-order and direct custody:

| Component | V2/V3 flash swap | PM-as-bank |
|-----------|-----------------|------------|
| Source token input | 1 transfer (flash swap output) | 1 transfer (take) |
| Internal swaps | N transfers | N transfers (same) |
| Repayment | Implicit (in K-invariant) | 1 transfer (to PM) |
| Profit extraction | 1 transfer (already from executor) | 0–1 (V4_MINT or V4_TAKE) |

The flash swap merges "source input" and "profit extraction" into
a single transfer. PM-as-bank separates them, adding an extra
transfer for repayment unless V4_MINT is used (tying at the same
count but with ERC6909 profit).

---

## 4. The Key Advantage: Zero-Fee Borrowing

### Flash swap fees

| Source | Fee |
|--------|-----|
| V2 flash swap | 0.30% (300 bps) |
| V3 flash swap | 0.01%–1.00% (fee tier) |
| V4 PM `take()` | **0.00%** |
| Aave flash loan | 0.05% (or 0% for some assets) |
| Uniswap V3 flash | 0.00% (but callback overhead) |

PM `take()` is a pure delta operation. The PM does not charge a swap
fee because there is **no swap** — the PM is just lending tokens. The
"repayment" is the user's obligation to zero the delta by end of
`unlock()`, but the PM doesn't take a cut.

**Capital impact**: For a 1000 WETH arbitrage:
- V2 flash swap fee: 3 WETH (0.3%)
- PM take fee: 0 WETH

The executor can use the saved fee as additional capital, improving
the swap route's profitability.

### Why doesn't the PM charge a fee?

The PM is not a lending protocol — it's a **swap venue**. The `take()`
operation exists so that users can withdraw positive deltas (tokens
owed to them by the PM). Using it as a flash-loan source is a
second-order effect. The PM's fee model is at the **swap level**
(pool-specific fees per swap), not at the delta level.

When you `take()` without a swap, you're creating a negative delta
that you must settle. The PM's security model guarantees this by
requiring all deltas to be zero at end of unlock (reverting otherwise).
The PM is collateralized by the caller's obligation to return the
tokens — enforced by the `CurrencyNotSettled` revert.

---

## 5. Liquidity Depth

The PM's token balance is the **aggregate** of all V4 pool reserves
for that token. For popular tokens:

| Token | V2 pair liquidity | V3 pool liquidity | V4 PM liquidity |
|-------|------------------|-------------------|-----------------|
| WETH | Per-pair (say 500 WETH) | Per-fee-tier (say 2000 WETH) | **All V4 WETH pools combined** |
| USDC | Per-pair | Per-fee-tier | **All V4 USDC pools combined** |

If V2a has 500 WETH in reserves but the executor needs 5000 WETH,
V2a's flash swap can only provide ~500 WETH (after fee). The PM might
hold 50,000 WETH across all V4 WETH pools — enough to supply the
entire input.

**This is the dominant use case for PM-as-bank**: accessing deep
liquidity from V4 without using V4 pools as swap venues.

### But: PM liquidity is also V4 pool liquidity

Withdrawing 5000 WETH via `take()` means the PM's WETH balance drops
by 5000 WETH until settlement. If V4 swaps are happening concurrently
(in the same block, in other transactions), they could fail due to
insufficient PM balance.

In practice, this is the same concern as with any lending protocol:
the lender's liquidity is also being used by others. But since
`take()` + `settle()` happen atomically within one transaction, the
PM's balance is only temporarily reduced (within the same unlock
callback). Other transactions in the same block are not affected —
they see the PM's balance *after* the transaction completes.

---

## 6. Novel Strategies Enabled by PM-as-Bank

### 6.1 Cross-protocol netting

If the executor runs multiple arbitrage paths in the same `unlock()`
call, the PM deltas from all paths net against each other:

```
unlock():
  # Path 1: V2-V2-V2 (borrow WETH from PM, repay via V2c output)
  take(WETH, V2a, 1e18)
  V2a→V2b→V2c      # V2c sends 2e18 WETH to PM
  settle()           # delta: -1e18 + 2e18 = +1e18

  # Path 2: V4-V4-V4 (internal delta netting)
  V4a swap           # delta: -1e18 WETH + USDC
  V4b swap           # delta: -USDC + WBTC
  V4c swap           # delta: -WBTC + 2e18 WETH
  # Net delta from path 2: +1e18 WETH

  # Combined net delta: +2e18 WETH
  V4_MINT(WETH, executor, 2e18)  # 0 transfers (ERC6909)
```

**2 paths, 4 transfers total**: take(1) + V2a→V2b(1) +
V2b→V2c(1) + V2c→PM(1) = 4 transfers for path 1, plus 0 for path 2,
plus mint(0) = **4 transfers for 2 paths**.

Running them separately would be 4 (path 1) + 1 (path 2: take profit) =
5 transfers. **Cross-protocol netting saves 1 transfer** by eliminating
the separate profit extraction for path 2.

### 6.2 ERC6909 compounding across transaction boundaries

The executor's ERC6909 balance from prior operations can fund the PM
borrow instead of physical token transfers:

```
TX1: V4-V4-V4 → V4_MINT profit WETH as ERC6909 (0 transfers)

TX2: V2-V2-V2 using ERC6909 as collateral:
  unlock():
    V4_BURN(WETH, 1e18)     # Convert ERC6909 → +delta (0 transfers)
                              # This funds V2a's WETH input
    V2a V2_SWAP_CALC(...)
    V2b V2_SWAP_CALC(...)
    ...
    settle()                  # V2c output repays the delta
```

`V4_BURN` does: `erc6909_balance -= amount; delta += amount`. It produces
a **positive** delta, which offsets the **negative** delta from a prior
`take()`. The pair nets to zero:

```
TX2:
  unlock():
    take(WETH, V2a, 1e18)     # -1e18 delta (borrow)
    V4_BURN(WETH, 1e18)       # +1e18 delta (from ERC6909)
    # Net delta for WETH: 0 — the borrow is "funded" by ERC6909
    V2a swap, V2b swap, V2c swap
    # V2c sends 2e18 WETH to executor (profit)
    # No settle needed for WETH — delta is already 0
```

**0-transfer WETH funding!** The executor uses its accumulated
ERC6909 to fund the borrow without any physical transfer. The V2 chain
runs normally, and the executor keeps the V2 output as profit.

This only works if the ERC6909 balance is from a PRIOR transaction. In
the zero-balance scenario, there's nothing to burn. The compounding
benefit only manifests over multiple transactions.

### 6.3 Token substitution (borrow any token)

PM can lend tokens that aren't in the arbitrage path at all. Example:
the path is V2(WETH→USDC) → V3(USDC→WBTC), but the executor needs
DAI for some external reason (e.g., a CLOB trade). The PM can lend
DAI via `take()` inside the same `unlock()`:

```
unlock():
  take(DAI, executor, 1e18)      # borrow DAI for external use
  take(WETH, V2a, 1e18)          # borrow WETH for the swap chain
  V2a→V3b, V3b→executor          # swap chain
  [use DAI externally]
  transfer(DAI, PM, 1e18) + settle(DAI)  # repay DAI
  transfer(WETH, PM, 1e18) + settle(WETH)  # repay WETH
```

This is a flash loan of DAI + WETH in a single atomic transaction.
The PM doesn't care what the tokens are used for — it only requires
that all deltas are zero at end of unlock.

---

## 7. Constraints and Risks

### 7.1 PM balance must be sufficient

`take()` is a physical `IERC20.transfer()`. If the PM doesn't hold
enough of the requested token, the transfer fails (revert). This
means PM-as-bank only works for tokens that V4 pools actually hold.

In production, V4 PM holds massive reserves of major tokens (WETH,
USDC, USDT, WBTC). Niche tokens may not have sufficient V4 liquidity.

### 7.2 Settlement timing: sync before deposit, settle after

When the swap chain's output is used to repay the PM:

```
take(WETH, V2a, 1e18)  # -1e18 delta
V2c sends WETH to PM    # but settle must see this!
```

The `sync()` must be called **before** V2c sends WETH to PM, and
`settle()` must be called **after**. This adds ordering constraints
when the swap chain is inside the unlock callback:

```
unlock():
  sync(WETH)              # snapshot PM's WETH balance (before V2c sends)
  take(WETH, V2a, 1e18)  # PM→V2a (takes from balance AFTER sync — OK because sync is pre-settlement)
  V2a→V2b→V2c            # swap chain, V2c sends WETH to PM
  settle()                # reads new balance, credits delta
```

The ordering is sensitive. `sync()` snapshots the PM's current
balance. Then `take()` reduces PM's balance (physical transfer out).
Then V2c sends WETH to PM (increasing PM's balance). Then `settle()`
reads the new balance and credits the difference.

The delta credited by `settle()` = `balance_now - balance_at_sync`.
After sync: balance drops by `take` (1e18 out), then increases by V2c
output (2e18 in). Net change: +1e18. So `settle()` credits +1e18,
which together with the -1e18 from `take` gives net delta 0. ✓

But if sync is called AFTER `take()`, the snapshot would be of the
lower balance (post-take), and settle would see a larger increase.
The result is the same because the net delta is what matters — but
the intermediate accounting differs.

**Best practice**: call `sync()` BEFORE any operations that affect
PM's balance, so the snapshot captures the "before" state correctly.

### 7.3 The `CurrencyNotSettled` revert is the security guarantee

If the swap chain fails to produce enough tokens to repay the PM,
the `unlock()` callback's post-check will revert with
`CurrencyNotSettled`. This is the PM's equivalent of a flash-loan
repayment check — no tokens can leave without being returned.

This means PM-as-bank is **safe by construction**: the only way for
the executor to keep tokens is if the swap chain generates enough
profit to cover the borrow + generate a surplus (the executor's
profit).

---

## 8. Comparison to Alternative Flash-Loan Sources

| Feature | V2 Flash Swap | V3 Flash | Aave | PM `take()` |
|---------|---------------|----------|------|-------------|
| Fee | 0.30% | 0.01–1.00% | 0.00–0.05% | **0.00%** |
| Liquidity | Per-pair | Per-fee-tier | Per-asset reserve | **All V4 reserves** |
| Atomic repayment | K-invariant | IIA | Callback | **Delta accounting** |
| Multi-asset | No (one pair) | No (one pool) | Yes | **Yes** |
| Callback required | Yes | Yes | Yes | **No (delta auto-settles)** |
| ERC6909 compounding | No | No | No | **Yes** |
| Transfer count | Best (merged) | Baseline | Baseline+1 | Same or +1 |

The PM's unique advantage is **zero-fee, multi-asset, callback-free
flash loans with ERC6909 compounding**. The disadvantage is the same
or slightly higher transfer count compared to V2 flash swaps.

---

## 9. Production Architecture

For an executor that operates across multiple venues (V2, V3, V4) in
the same block:

```
class ArbExecutor:
    """Uses PM as universal flash-loan source."""

    def execute_arbitrage(path, amount):
        # 1. Borrow input token from PM (zero fee)
        pm.take(path.input_token, path.first_pool, amount)

        # 2. Execute swap chain (V2/V3/V4 — doesn't matter)
        for pool in path.pools:
            pool.swap(...)

        # 3. Repay PM from last pool's output (direct to PM)
        #    or from executor if last pool can't send to PM
        pm.sync(path.input_token)
        # last pool output goes to PM via swap recipient=PM
        pm.settle()

        # 4. Extract profit: V4_MINT (ERC6909) or V4_TAKE
        if executor.has_erc6909_use:
            pm.mint_erc6909(executor, profit)  # 0 transfers
        else:
            pm.take(path.output_token, executor, profit)  # 1 transfer
```

### Multi-tx lifecycle with ERC6909 compounding

```
TX1: V4-V4-V4 → V4_MINT 1 WETH ERC6909 (0 transfers, 0 fees)
TX2: V2-V3-V2 → V4_BURN funds the borrow (0 transfers)
                   V2/V3 swaps (4 transfers)
                   Profit via V4_MINT (0 transfers)
TX3: V4-V4-V4 → V4_BURN funds the borrow (0 transfers)
                   V4 swaps (0 internal transfers)
                   More V4_MINT profit (0 transfers)
...
TX_N: Withdraw → V4_BURN all ERC6909 + V4_TAKE physical WETH (1 transfer)
```

**N transactions, 4 + 1 transfers** (4 for the one V2-V3-V2 path,
1 for the final withdrawal). Without PM-as-bank compounding, each
additional V4 path would add 1 transfer for profit extraction. With
compounding, all intermediate paths are zero-transfer.

---

## 10. Summary

| Question | Answer |
|----------|--------|
| Can PM lend tokens for non-V4 paths? | **Yes** — `take()` only requires `t_unlocked` |
| Does it reduce transfer counts? | **No** — same or +1 vs. V2 flash swap |
| Does it save fees? | **Yes** — 0% vs. V2's 0.3% and V3's fee tier |
| Does it provide deeper liquidity? | **Yes** — aggregates all V4 pool reserves |
| Does it require callback handling? | **No** — delta auto-settles at end of unlock |
| Can it compound with ERC6909? | **Yes** — V4_MINT + V4_BURN cycle (0 transfers) |
| Is it safe? | **Yes** — `CurrencyNotSettled` revert guarantees repayment |
| Does it require prefunding? | **No** — all capital sourced atomically within each transaction |

**The PM-as-bank pattern does not reduce per-path transfer counts,
but it dramatically reduces the cost and complexity of sourcing
capital for arbitrage.** The zero-fee borrow is the primary benefit —
for a 1000 WETH arb, the PM saves 3 WETH in flash-swap fees compared
to V2. Over many operations, the savings compound with ERC6909
(V4_MINT/V4_BURN) to near-zero transfer costs for frequent operators.

No prefunding is required at any point: the contract can be deployed
with zero balance and execute profitable arbitrage paths immediately.
