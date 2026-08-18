# Executor

On-chain arbitrage executor contracts for Uniswap V2/V3/V4, written in Vyper 0.5.0a3.

> **[`docs/user-guide.md`](docs/user-guide.md)** — Comprehensive guide for constructing, encoding, and executing optimal arbitrage paths. Includes the full 2-hop and 3-hop path encyclopedia with command stream examples for all 27 permutations.

> **[`docs/pool-mechanics.md`](docs/pool-mechanics.md)** — Pool timing constraints, sync/settle ordering, direct custody rules, and V2/V3 reverse-order execution. Read this before constructing multi-hop swap paths.

Two implementations — **tstore_executor** (static payload queue) and **cmd_executor** (compact command stream with dynamic amounts) — execute identical arbitrage paths using different dispatch mechanisms.

> **No prefunding required** — the executor borrows all working capital atomically within each transaction via V2/V3 flash swaps and V4 PoolManager `take()`. The contract can be deployed with zero balance and execute profitable arbitrage paths immediately. See [`docs/pm-as-bank.md`](docs/pm-as-bank.md) for the capital-sourcing design.

## Contracts

| Contract | Description | Runtime Bytecode |
|----------|-------------|-----------------|
| `cmd_executor.vy` | Compact command-stream executor with dynamic V4 delta tracking via PM exttload + ERC6909 mint/burn + excess-balance V2 swaps + packed ABI config param | 15,359 bytes |
| `tstore_executor.vy` | Static payload-queue executor with V4 auto-settle | 5,871 bytes |
| `fake_uniswap_v2_pair.vy` | Mock V2 pair with K-invariant check, reset(), getReserves (3 callback variants + configurable fee) | 4,462 bytes |
| `fake_uniswap_v3_pool.vy` | Mock V3 pool with balance-delta check (2 callback variants) | 3,005 bytes |
| `fake_uniswap_v4_pool_manager.vy` | Mock V4 PoolManager with exttload + ERC6909 | 14,458 bytes |

## Architecture

### tstore_executor (static)

Generic payload executor: an ordered list of ABI-encoded calls dispatched via `raw_call`. V4 swaps are invoked directly in `unlockCallback`, reading `BalanceDelta` return values to auto-settle with the PoolManager.

- **Payload queue**: each entry is `(target, calldata, value, allow_revert)`
- **V4 auto-settle**: reads swap return values, takes positive deltas, settles negatives
- **Explicit amounts**: every operation amount is pre-computed off-chain and ABI-encoded in the payload

### cmd_executor (compact command stream)

A single byte-stream of compact commands (1-byte opcode + tightly-packed parameters), with addresses referenced by index into a shared address table. Three execution modes are freely mixed:

1. **Explicit mode**: off-chain code pre-computes amounts (e.g., `V4_TAKE(currency, to, amount)`)
2. **Dynamic mode**: V4 deltas are read from the PoolManager's own transient storage via `exttload()` — the authoritative source. `V4_TAKE_DELTA`/`V4_SETTLE_DELTA`/`V4_SETTLE_ALL` derive amounts on-chain.
3. **Compact mode**: swap amounts use uint96 + default sqrt_price_limit, saving 20 bytes per swap

**Additional on-chain computation commands**:
- `V2_SWAP_CALC`: computes `amount_out` from `getReserves()` + stored fee + excess balance (`balanceOf(pair) - reserves`); direct custody (no callback)
- `V3 auto-pay`: when `forward_data` is empty, V3 callback reads owed amounts from parameters and auto-transfers to pool
- `V3_SWAP_DELTA`: amount from PM exttload + default sqrt + auto-pay (5 bytes encoding, ~89 bytes saved/swap)
- `ERC20_XFER_BALANCE`, `WETH_DEPOSIT_ALL`, `WETH_WITHDRAW_ALL`: read warm balances on-line instead of encoding 32-byte amounts

**Benefits over tstore_executor**:
- **No prefunding required**: all working capital sourced atomically via flash swaps and PM lending — zero balance at deploy is sufficient
- Compact encoding: 1-byte opcode + index-referenced addresses vs ABI-encoded calldata
- Dynamic amounts: eliminates 32-byte amount parameters from calldata
- Authoritative delta source: PM exttload eliminates tracker drift risk
- `V4_SETTLE_ALL`: replaces 3+ explicit commands per currency with a single auto-settle
- On-chain computation: V2/V3 auto-pay derived from callback parameters + warm storage

## Command Set

```
Control / Preprocessing (0x00–0x0F):
  0x00  SET_ADDRESS        Append address to lookup table
  0x01  (reserved)         Was SKIP_PROFIT_CHECK, now in config param
  0x02  (reserved)         Was BRIBE_COINBASE, now in config param
  0x03  (reserved)         Was BRIBE_ADDRESS, now in config param

ERC20 / ETH / Native (0x10–0x1F):
  0x10  ERC20_TRANSFER    ERC-20 transfer (any context)
  0x11  ERC20_XFER_BALANCE Transfer entire token balance (warm read, no amount)
  0x12  WETH_DEPOSIT      Wrap ETH to WETH
  0x13  WETH_WITHDRAW     Unwrap WETH to ETH
  0x14  WETH_DEPOSIT_ALL  Wrap all ETH to WETH (no amount)
  0x15  WETH_WITHDRAW_ALL Unwrap all WETH to ETH (no amount)
  0x16  SEND_ETH          Send uint96 ETH to address
  0x17  SEND_ETH_ALL      Send all ETH to address

V2 (0x20–0x2F):
  0x20  V2_SWAP_COMPACT   V2 swap (uint96 amount_out + per-swap fee + optional forward_data)
  0x21  V2_SWAP_CALC      V2 swap with on-chain amount calc from excess balance (per-swap fee)
  0x22  V2_SWAP_DIRECT    V2 swap with explicit amount_out, no callback (pair enforces K-invariant)

V3 (0x30–0x3F):
  0x30  V3_SWAP_COMPACT   V3 swap (uint96 amount + default sqrt + auto-pay)
  0x31  V3_SWAP_DELTA     V3 swap (amount from PM exttload + default sqrt + auto-pay)

V4 Swaps (0x40–0x4F):
  0x40  V4_SWAP_COMPACT   V4 swap (uint96 amount + default sqrt_price_limit)
  0x41  V4_SWAP_DYNAMIC   V4 swap (amount from PM exttload, default sqrt_price_limit)
  0x42  V4_BATCH          V4 multi-swap + auto-settle (tight loop)

V4 Settlement / ERC6909 (0x50–0x5F):
  0x50  V4_UNLOCK         Enter PoolManager unlock context
  0x51  V4_TAKE           Take from PoolManager (explicit amount)
  0x52  V4_TAKE_COMPACT   Take with uint96 amount
  0x53  V4_TAKE_DELTA     Take using PM exttload delta amount
  0x54  V4_SYNC           Sync at PoolManager (anytime)
  0x55  V4_SETTLE         Settle (after unlock, post sync+transfer)
  0x56  V4_SETTLE_DELTA   Auto-settle one currency from PM exttload delta
  0x57  V4_SETTLE_ALL     Auto-settle all nonzero deltas from PM exttload
  0x58  V4_MINT_COMPACT   Mint as ERC6909 with uint96 amount (no transfer)
  0x59  V4_BURN_COMPACT   Burn from ERC6909 with uint96 amount (no transfer)

Stream separators:
  0xFE  BEGIN_PREPROCESSING  First byte: signals a preprocessing section follows
  0xFF  BEGIN_EXECUTION      Marks end of preprocessing / start of execution
```

## Callbacks

Both executors support all major Uniswap fork callback types:

| Protocol | Callback | Forks |
|----------|----------|-------|
| V2 | `uniswapV2Call` | Uniswap, SushiSwap |
| V2 | `hook` | Velodrome, Aerodrome |
| V2 | `pancakeCall` | PancakeSwap |
| V3 | `uniswapV3SwapCallback` | Uniswap, SushiSwap |
| V3 | `pancakeV3SwapCallback` | PancakeSwap |
| V4 | `unlockCallback` | All V4 |

### Auto-pay behaviors

**V3 auto-pay** (empty `forward_data`): The callback receives exact signed deltas — `amount0_delta > 0` means we owe that amount of token0. The handler reads `token0()`/`token1()` from the pool (warm, ~100 gas each) and auto-transfers the positive delta. Saves ~37 bytes calldata per V3 swap.

**V2_SWAP_CALC excess balance**: Reads `balanceOf(pair) - reserves[input_index]` to determine the swap input amount — the tokens deposited to the pair but not yet reflected in reserves (e.g., from `V4_TAKE(currency, recipient=pair)`). Computes output on-chain from `getReserves() + fee + excess`. Calls `pair.swap()` with `data=b""` (no callback needed — the pair already holds the input). The V2 K-invariant passes because `_v2_get_amount_out` produces amounts consistent with the K-check. Saves ~69 bytes calldata per V2 swap (eliminates `ERC20_TRANSFER` + amount encoding), but adds ~15K gas for on-chain computation vs explicit. Best used when calldata cost is high relative to execution gas, or when you want on-chain adaptation to reserve changes.

**V2_SWAP_COMPACT auto-pay**: When called with `forward_data = V2_AUTO_PAY_SENTINEL` (0xFE), the V2 pair invokes the executor's callback, which reads the per-swap fee from transient storage (`t_v2_pair_fee[pool]`) and computes the owed input amount via `_v2_get_amount_in()`. This enables single-swap V2 paths without manually encoding `ERC20_TRANSFER` commands in the forward_data. For multi-hop V2 paths with explicit callback commands, the fee is written to `t_v2_pair_fee` only when `forward_len > 0` (callback fires); when `forward_data` is empty, the V2 pair enforces K-invariant without callback and the TSTORE is skipped.

All three V2 swap commands support dissimilar fees across pools in the same transaction:

| Command | Fee mechanism | Callback? | Use case |
|---------|-------------|-----------|----------|
| `V2_SWAP_CALC` (0x21) | Inline `fee:2` per command | No (`data=b""`) | On-chain computation from excess balance |
| `V2_SWAP_COMPACT` (0x20) | Inline `fee:2` → `t_v2_pair_fee[pool]` | Yes (if `forward_data` non-empty) | Flash swap with auto-pay or explicit callback commands |
| `V2_SWAP_DIRECT` (0x22) | None needed (pair enforces K) | No (`data=b""`) | Pre-computed amounts, pre-funded pair |

Both `V2_SWAP_COMPACT` and `V2_SWAP_CALC` validate the fee: `0 < fee < 10000` (reverts with `BipsTooHigh(bips)` on out-of-range values).

## Fake Contract Invariant Enforcement

The fake V2/V3/V4 contracts replicate the invariant checks of their real Uniswap counterparts. This ensures that tests only pass when the executor behaves correctly on mainnet — pre-funding pools before a swap does not mask missing callback payments.

| Invariant | Real Contract | Fake Contract |
|-----------|--------------|---------------|
| V3 balance-delta check | `balance0Before.add(uint256(amount0)) <= balance0()` (`IIA`) | ✅ Identical — snapshots balance before callback, checks delta after |
| V3 reentrancy guard | `slot0.unlocked` with `lock()` modifier | ✅ `self.unlocked` flag |
| V3 sqrtPriceLimitX96 validation | Range check against MIN/MAX_SQRT_RATIO (`SPL`) | ✅ Same range check |
| V3 amountSpecified ≠ 0 | `require(amountSpecified != 0, 'AS')` | ✅ Same assertion |
| V2 K-invariant check | `balance0Adjusted * balance1Adjusted >= reserve0 * reserve1 * 10000²` (with configurable fee) | ✅ Identical calculation with per-swap fee deduction |
| V2 balance-delta input computation | `amount0In = balance0 - (reserve0 - amount0Out)` | ✅ Identical delta computation |
| V2 reentrancy guard | `unlocked` flag with lock modifier | ✅ Same flag |
| V2 reserve update after swap | `_update(balance0, balance1, ...)` | ✅ Same update |
| V4 sync/settle | `reservesNow - reservesBefore` (balance delta) | ✅ Already correct |

See [`FAKE_CONTRACT_AUDIT.md`](FAKE_CONTRACT_AUDIT.md) for the full comparison between fake and real contracts.

## Gas Benchmarks

> **Note**: Benchmarks use fake contracts that enforce the same invariants as real
> Uniswap V2/V3 contracts — including balance-delta callback checks (V3's `IIA`),
> K-invariant (V2's constant product), and reentrancy guards. Previous benchmarks
> used absolute-balance checks that under-reported gas by ~8–26% on cross-protocol
> paths where the callback validation was skipped.

### cmd_executor explicit vs tstore_executor

`#pragma optimize gas`, Venom codegen.

| Path | tstore_executor | cmd_executor | Δ gas | Δ % |
|------|----------------|-------------|-------|-----|
| V4→V4 | 86,746 | 70,724 | **−16,022** | **−18.5%** |
| V4→V3 | 187,792 | 121,619 | **−66,173** | **−35.2%** |
| V3→V4 | 172,680 | 121,564 | **−51,116** | **−29.6%** |
| V4→V2 | 178,978 | 126,981 | **−51,997** | **−29.1%** |
| V2→V3 | 160,223 | 126,960 | **−33,263** | **−20.8%** |
| V3→V2 | 146,952 | 124,004 | **−22,948** | **−15.6%** |
| V2 direct | 107,276 | 87,400 | **−19,876** | **−18.5%** |

### cmd_executor optimal vs tstore_executor

Uses maximally efficient command selection: V3 auto-pay, V3_SWAP_DELTA, V4_SETTLE_DELTA, V4_TAKE_DELTA where applicable.

| Path | tstore_executor | cmd optimal | Δ gas | Δ % | Calldata saved |
|------|----------------|-------------|-------|-----|---------------|
| V4→V4 | 86,746 | 71,495 | **−15,251** | **−17.6%** | 32B |
| V4→V3 | 187,792 | 120,680 | **−67,112** | **−35.7%** | 35B |
| V3→V4 | 172,680 | 121,251 | **−51,429** | **−29.8%** | 103B |
| V4→V2 | 178,978 | 133,323 | **−45,655** | **−25.5%** | 53B (V2_SWAP_CALC) |
| V2→V3 | 160,223 | 126,020 | **−34,203** | **−21.3%** | 35B |
| V3→V2 | 146,952 | 123,046 | **−23,906** | **−16.3%** | 35B |
| V2 direct | 107,276 | 74,603 | **−32,673** | **−30.4%** | (same as explicit) |

### V4→V4 same-currency: explicit vs dynamic vs compact vs batch

| Approach | Gas | vs tstore | Calldata |
|----------|-----|-----------|----------|
| tstore_executor | 86,746 | — | — |
| cmd explicit | 70,724 | **−18.5%** | 192 bytes |
| cmd compact | 71,495 | −17.6% | 160 bytes (−32) |
| cmd dynamic | 72,741 | −16.1% | 144 bytes (−48) |
| cmd V4_BATCH | 72,112 | −16.9% | 157 bytes (−35) |

### V4→V4 cross-currency: explicit vs V4_BATCH

| Approach | Gas | vs tstore | Calldata |
|----------|-----|-----------|----------|
| tstore_executor | 114,726 | — | — |
| cmd explicit | 82,528 | **−28.0%** | 230 bytes |
| cmd V4_BATCH | 81,793 | **−28.7%** | 142 bytes (−88) |

### V4_BATCH analysis

V4_BATCH (opcode `0x42`) packs multiple V4 swaps into a single command with a tight loop and auto-settles at the end. It eliminates per-command dispatch overhead and replaces separate `V4_TAKE`/`V4_TAKE_DELTA` + `V4_SETTLE_DELTA` with a single auto-settle. Dynamic amounts are decoded from the previous swap's `BalanceDelta` return value (`slice(convert(swap_delta, bytes32), offset, 16)` → `int128`), avoiding exttload reads for subsequent swaps.

**Same-currency**: V4_BATCH is −16.9% vs tstore. BalanceDelta decode eliminates one exttload read per dynamic swap. The byte-stream parsing overhead is compensated by the tighter settlement path.

**Cross-currency**: V4_BATCH is **−28.7%** vs tstore — the best V4→V4 result. The auto-settle efficiently handles native ETH + WETH with just 2 exttload reads (for settlement only), while tstore's Phase 3 iterates deltas including intermediate ERC-20 entries that cancel to zero.

**When to use**: V4_BATCH is optimal for all V4→V4 paths. For same-currency paths, explicit `V4_SWAP_COMPACT + V4_TAKE` is also competitive (−18.5% vs tstore).

### Access List Impact (EIP-2930)

Production MEV searchers include access lists when submitting transactions. Benchmarks use Anvil's `eth_createAccessList` RPC to compute optimal access lists by tracing the transaction.

| Path | Without AL | With AL | AL Savings | AL Entries |
|------|-----------|---------|------------|------------|
| V4→V4 | 71,495 | 70,855 | −640 | 2 |
| V4→V3 | 134,452 | 119,240 | **−15,120** | 4 |
| V3→V4 | 134,931 | 119,811 | **−15,120** | 4 |
| V4→V2 explicit | 126,981 | 125,461 | **−15,200** | 4 |
| V4→V2 V2_SWAP_CALC | 148,062 | 131,643 | **−15,360** | 4 |
| V3→V2 | 124,302 | 122,173 | −1,840 | 4 |
| V2 direct | 87,400 | 79,453 | **−8,900** | 3 |

- **Cross-protocol paths save ~15K gas** with access lists — V4 PM + token balance slots are the biggest contributors
- **V2 direct saves the most** (13,400 gas) — the V2 pair's reserve/balance slots are heavily accessed
- **V4→V4 saves the least** (640 gas) — few storage slots touched (only PM deltas + executor WETH balance)
- **V2_SWAP_CALC benefits from AL**: the on-chain `getReserves()` reads are pre-warmed, reducing the ~15K overhead slightly

### Dynamic Commands (max calldata reduction)

Replaces every explicit amount in calldata with on-chain dynamic reads from warm storage:

- **V4_TAKE_DELTA** (0x53) replaces V4_TAKE — reads amount from PM exttload, saves 32B per take
- **V4_TAKE_COMPACT** (0x52) replaces V4_TAKE — uint96 amount, saves 20B per take when delta consumed
- **ERC20_XFER_BALANCE** (0x11) replaces ERC20_TRANSFER — reads warm token balance, saves 32B per transfer
- **V2_SWAP_CALC** (0x21) replaces ERC20_TRANSFER + V2_SWAP_COMPACT — reads excess balance + computes output, saves 52B per V2 swap
- **V3 auto-pay** — empty forward_data reads owed amounts from callback parameters, saves ~37B per V3 swap

| Path | Explicit | Dynamic | Δ gas | Calldata saved | Dynamic + AL |
|------|----------|---------|-------|----------------|-------------|
| V4→V4 | 70,724 | 71,495 | +771 | 32B (TAKE_DELTA) | 70,855 |
| V4→V3 | 121,619 | 134,452 | +12,833 | 73B (TAKE_COMPACT+auto-pay) | 119,240 |
| V3→V4 | 121,564 | 134,931 | +13,367 | 57B (auto-pay) | 119,811 |
| V4→V2 | 126,981 | 142,009* | +15,028 | 60B (TAKE_DELTA+XFER_BALANCE) | 125,461 |
| V4→V2 calc | 126,981 | 148,062 | +21,081 | 39B (TAKE_DELTA+SWAP_CALC) | 131,643 |
| V3→V2 | 124,004 | 124,302* | +298 | 82B (XFER_BALANCE) | 122,173 |
| V2 direct | 87,400 | 88,539* | +1,139 | 25B (XFER_BALANCE) | 79,453 |
| V2 direct calc | 87,400 | 105,847 | +18,447 | 4B (SWAP_CALC) | n/a |

*Best dynamic variant (one that doesn't use V2_SWAP_CALC).

**Key findings**:

1. **ERC20_XFER_BALANCE is a pure win** — saves 32B calldata AND reduces gas by 3,319 on V2 direct by eliminating the explicit amount encoding. The `balanceOf` read is ~200 gas (warm) but saves 32 bytes of calldata (512–2,048 gas equivalent).

2. **V4_TAKE_DELTA is a pure win** — saves 32B calldata per take with zero gas overhead (exttload reads are as fast as the explicit path).

3. **V4_TAKE_COMPACT saves 20B** — when V4_TAKE_DELTA isn't usable (delta consumed), uint96 amount saves 20 bytes per take with zero gas overhead.

4. **V2_SWAP_CALC has higher gas but saves calldata** — saves 52–80B calldata but costs 15–30K extra gas for on-chain computation. Best when calldata cost dominates (high gas prices, blob transactions), or when on-chain adaptation to reserve changes is desired (e.g., between simulation and execution, reserves shift slightly).

5. **V3→V2 with ERC20_XFER_BALANCE** is near-zero gas overhead (+243) for 32B calldata savings.

### Combined: cmd_executor optimal + access list

Best-case gas for each path (optimal/compact commands + EIP-2930 access list):

| Path | tstore (no AL) | cmd optimal + AL | Δ gas | Δ % |
|------|---------------|-----------------|-------|-----|
| V4→V4 | 86,746 | 70,855 | **−15,891** | **−18.3%** |
| V4→V3 | 187,792 | 119,240 | **−68,552** | **−36.5%** |
| V3→V4 | 172,680 | 119,811 | **−52,869** | **−30.6%** |
| V4→V2 | 178,978 | 125,461 | **−53,517** | **−29.9%** |
| V3→V2 | 146,952 | 122,173 | **−24,779** | **−16.9%** |
| V2 direct | 107,276 | 79,453 | **−27,823** | **−25.9%** |

### Sentinel Address Resolution

The address table indices 0xFC–0xFF are reserved for sentinel values that resolve to common addresses without `TLOAD` or `SET_ADDRESS` in the command stream:

| Index | Sentinel | Resolves To | Saves |
|-------|----------|-------------|-------|
| `0xFC` | `V4_PM_SENTINEL` | `POOL_MANAGER_ADDR` (immutable) | TLOAD + SET_ADDRESS per PM reference |
| `0xFD` | `V4_SELF_SENTINEL` | `self` (executor address) | TLOAD + SET_ADDRESS per executor reference |
| `0xFE` | `V4_WETH_SENTINEL` | `WETH_ADDR` (immutable) | TLOAD + SET_ADDRESS per WETH reference |
| `0xFF` | `V4_NATIVE_SENTINEL` | `NATIVE_ADDRESS` (`address(0)`) | TLOAD + SET_ADDRESS per native ETH reference |

`_lookup_address()` checks the sentinel range first (`idx >= 0xFC`), falling through to `t_addresses[idx]` for regular indices. Sentinel resolution is inlined in hot-path handlers (V4 swap compact, V4 take compact, ERC20 transfer, V3/V2 swap handlers) to avoid function call overhead. The sentinel pattern saved **−67,786 gas** across all 27 three-hop paths (WETH −19,824, executor −17,824, PM −16,062, no-hooks 0xFF −14,063).

Additionally, **V4 no-hooks sentinel** (`hooks_idx = 0xFF`) means "no hooks" — skips `TLOAD` for `address(0)` hook lookup and eliminates `SET_ADDRESS` for `ZERO_ADDRESS` in the command stream.

### Three-Hop Gas Optimization

After extensive optimization, the 27 three-hop permutations total **4,947,078 gas** (sum of per-path gas with profit checks active), with runtime bytecode at ~15,359 bytes. Historical measurements below may differ due to subsequent changes (notably the V3 real-math refactor and the removal of user sentinels — see commit `8c75fa6` and `.auto/ideas.md` Session 13).

**Top techniques by gas saved** (cumulative across all 27 paths):

| Technique | Gas Saved | Description |
|-----------|-----------|-------------|
| Sentinel address resolution | −67,786 | WETH/NATIVE/SELF/PM sentinels skip TLOAD + SET_ADDRESS |
| Dispatch reorder by frequency | −11,625 | Most common opcodes first in if/elif chain |
| Inline _lookup_address in hot handlers | −3,452 | Avoids function call overhead for sentinel resolution |
| Conditional balance reads | −24,986 | Skip balanceOf when skip_profit_check + no bribe |
| Preprocess reorder | −1,854 | SET_ADDRESS before BEGIN_EXECUTION in preprocessing |
| Remove forward_data variables | −3,368 | Eliminate intermediary Bytes vars used once |
| Single t_callback variable | −3,027 | Replace HashMap with one transient variable |
| Merged slice reads | −7,000 | Read 2/5/8 adjacent bytes in one slice |
| unsafe offset arithmetic | −26,339 | Skip overflow checks on offset increments |
| Sentinel-aware V4_SETTLE_DELTA | −246 | Skip exttload for sentinel currencies |

### Summary

- **cmd_executor dominates on all paths** (15.6–35.2% cheaper than tstore in explicit mode)
- **With optimal commands + access lists, savings reach 36.5%** on V4→V3 — the biggest gains come from V3 auto-pay and pre-warmed storage slots
- **`#pragma optimize gas`** saves 224–932 gas per path vs `optimize codesize` (avg ~640), at +3,821 bytes larger runtime bytecode
- **SET_ADDRESS + packed config param** eliminate ABI padding and command-stream dispatch — saves 128–160 bytes calldata per execution + −1,424 gas total from moving bribe/profit-check to ABI param
- **V4_MINT_COMPACT saves ~20K gas (−18.3%)** vs V4_TAKE by eliminating the physical ERC-20 transfer — the biggest single-command gas improvement
- **ERC20_XFER_BALANCE is a pure calldata win** — saves 25–82B per transfer with minimal gas overhead
- **V4_TAKE_DELTA is a pure calldata win** — saves 32B per take with zero gas overhead (exttload reads are as fast as the explicit path)
- **V4_TAKE_COMPACT** (0x52) saves 20B per take when delta is consumed — uint96 instead of uint256
- **V3 auto-pay** saves ~35 bytes calldata and 950–1,200 gas per cross-protocol path
- **V4_SETTLE_DELTA** eliminates V4_SYNC + ERC20_TRANSFER + V4_SETTLE (3 commands → 1, up to 103 bytes calldata on V3→V4)
- **V2_SWAP_CALC** saves calldata but costs more gas — best used when calldata cost dominates (e.g., blob transactions) or on-chain reserve adaptation is desired
- **V4_BATCH is optimal for V4→V4 paths** — −28.7% vs tstore on cross-currency with 88B less calldata than explicit
- **Sentinel address resolution** eliminates TLOAD + SET_ADDRESS for WETH, NATIVE, executor, and PM — −67,786 gas across all 27 three-hop paths

## Test Suite

276 passing, 0 skipped.

```bash
uv run ape test tests/ -v -s
```

| Test file | Tests | Coverage |
|-----------|-------|----------|
| `test_cmd_executor_compact.py` | 5 | V4_SWAP_COMPACT, V4_TAKE_COMPACT, ERC20_XFER_BALANCE, WETH_DEPOSIT_ALL, WETH_WITHDRAW_ALL |
| `test_cmd_executor_dynamic.py` | 6 | V4_TAKE_DELTA, V4_SETTLE_DELTA, V4_SETTLE_ALL, V4_SWAP_DYNAMIC |
| `test_cmd_executor_v4v4.py` | 7 | V4→V4 same-currency, cross-currency, V4_BATCH, V4→V4→V4 three-hop |
| `test_cmd_executor_v4v3.py` | 3 | V4→V3, V3→V4, V3 auto-pay |
| `test_cmd_executor_v4v2.py` | 3 | V4→V2, V2→V4, V2_SWAP_CALC with excess balance |
| `test_cmd_executor_v2v3.py` | 2 | V2→V3, V3→V2 |
| `test_cmd_executor_v2v2_v3v3.py` | 2 | V2→V2 nested callback, V3→V3 nested callback |
| `test_cmd_executor_v3_three_hop.py` | 14 | V3→V3→V3 (nested callbacks, auto-pay variants, reverse-order direct custody), V4→V3→V3, V3→V3→V4, PancakeSwap V3 callback |
| `test_cmd_executor_three_hop_optimized.py` | 27 | All 27 V2/V3/V4 three-hop permutations with optimal routing: **ALL 27 at ≤4 transfers** |
| `test_cmd_executor_edge_cases.py` | 2 | Native ETH, V2 direct swap |
| `test_cmd_executor_v2_swap_direct.py` | 8 | V2_SWAP_DIRECT with pre-funded pairs, explicit amounts |
| `test_cmd_executor_three_pool_v2.py` | 9 | Three-pool V2 paths, multi-hop V2 callbacks |
| `test_cmd_executor_inline_wrap_unwrap.py` | 8 | WETH_DEPOSIT/WITHDRAW inline, V4_V4 wrap/unwrap flows |
| `test_cmd_executor_v4v4_wrap_unwrap.py` | 4 | V4→V4 with WETH wrapping, native ETH handling |
| `test_cmd_executor_bribe_commands.py` | 5 | Bribe via config param, zero bips, gas overhead |
| `test_cmd_executor_bribe_transfer.py` | 4 | BRIBE actual ETH transfer, no-bribe baseline |
| `test_erc6909.py` | 5 | ERC6909 mint/burn view functions, mint test, burn test, gas savings |
| `test_erc6909_composability.py` | 6 | ERC6909 composability: V4_MINT profit, multi-tx BURN withdrawal, ETH-funded settlement |
| `test_v2_configurable_fee.py` | 7 | V2 configurable fee: Uniswap 0.3%, PancakeSwap 0.25%, sub-1% |
| `test_v2_swap_compact_fee.py` | 4 | V2_SWAP_COMPACT with inline fee: Uniswap, PancakeSwap, mixed-fee paths |
| `test_v2_fee_bounds.py` | 4 | V2 fee bounds: fee=0, fee=10000, out-of-range reverts |
| `test_v2_fee_denominator_fuzz.py` | 7 | Fuzz testing for V2 fee denominator edge cases |
| `test_v2_swap_calc_excess.py` | 7 | V2 excess-balance: direct, V4→V2, V2→V2, revert without deposit |
| `test_withdrawal.py` | 9 | ERC6909/ERC20 withdrawal, SEND_ETH/SEND_ETH_ALL, full round-trip |
| `test_conservation.py` | 4 | Token conservation invariants across paths |
| `test_events.py` | 6 | Event emission for commands and settlements |
| `test_pm_as_bank.py` | 11 | PoolManager as bank: lending, settlement, no-prefund verification |
| `test_fake_pool_manager_parity.py` | 30 | Fake vs real PoolManager parity checks (mainnet fork) |
| `test_v3_libraries.py` | 53 | Uniswap V3 TickMath/SqrtPriceMath/FullMath library parity (port + mainnet fork) |
| `test_v3_library_fuzz.py` | 10 | Hypothesis fuzz of V3 math libraries vs SolReference |
| `test_v3_pool.py` | 2 | V3 pool fixture sanity |
| `test_v3_v4_v3_residual_regression.py` | 2 | V3↔V4↔V3 overproduction residual regression guard |

### Three-Hop Comparison (WETH→USDC→WBTC→WETH)

**All 27** permutations reach 4 transfers or fewer.

| Protocol | Min Transfers | Mechanism | Direct Custody? | Key Constraint |
|----------|--------------|-----------|-----------------|-----------------|
| V2 | 4 | Reverse-order flash borrow via excess-balance V2 swap (last pool first, chain inside callback) | Reverse-order | Callback-to-recipient bypassed; K-invariant checks total balances |
| V3 | 4 | Reverse-order nested swaps: V3c→V3b→V3a, each sends output to the next outer pool | Reverse-order only | IIA checks balance delta during callback — tokens must arrive during callback, not before swap() |
| V4 | 2 PM ops | All swaps inside unlock(), deltas net out in transient storage | N/A (built-in) | Delta accumulation eliminates intermediate custody entirely |

**Key discoveries**:
- **V4→V3 IIA satisfied during V3's own callback**: V4_TAKE sends tokens to V3 *during* V3's callback (not before V3.swap() starts), so the balance increase appears in balance_after but not balance_before, satisfying IIA.
- **V2→V3 IIA satisfied during V3's callback**: V2a sends USDC to V3b during V3b's callback (reverse-order from V2c), satisfying V3's IIA.
- **Profit-capture decoupling**: Reverse-order from V2c sends WETH profit directly to the executor (via V2c's flash swap output), while a separate ERC20 transfer creates WETH excess at V2a for K-invariant.

## Build & Run

```bash
# Install dependencies
uv sync

# Run all tests (Ape compiles automatically)
uv run ape test tests/ -v -s

# Run only cmd_executor tests
uv run ape test tests/test_cmd_executor_*.py -v

# Run gas benchmarks
uv run ape test tests/test_gas_benchmark.py tests/test_gas_benchmark_optimal.py tests/test_gas_benchmark_dynamic.py tests/test_gas_benchmark_access_list.py -v -s
```

## Project Structure

```
contracts/
├── cmd_executor.vy                  # Compact command-stream executor
├── tstore_executor.vy               # Static payload-queue executor
├── fake_erc20.vy                    # Mock ERC-20
├── fake_weth.vy                     # Mock WETH (with deposit/withdraw)
├── fake_uniswap_v2_pair.vy         # Mock V2 pair (3 callback variants + getReserves + K-invariant + reset + configurable fee)
├── fake_uniswap_v3_pool.vy         # Mock V3 pool (2 callback variants + balance-delta check)
├── fake_uniswap_v4_pool_manager.vy  # Mock V4 PoolManager (with exttload + ERC6909)
├── fake_external_callback.vy        # Mock external callback contract for TSTORE_CONTINUATION tests
├── exttload_comparator.vy           # Exttload gas comparison utility
├── utility_functions.vy            # Address formatting helpers
└── interfaces/
    ├── IWETH.vyi                   # WETH deposit/withdraw interface
    ├── UniswapV2/                  # V2 pair/callee interfaces
    ├── UniswapV3/                   # V3 pool/callback interfaces
    └── UniswapV4/                  # V4 PoolManager/exttload/ERC6909/unlock interfaces

tests/
├── conftest.py                     # Shared fixtures
├── conftest_shared.py              # Shared encoding helpers, constants, AddressTable
├── test_cmd_executor_compact.py    # Compact/warm-balance command tests
├── test_cmd_executor_dynamic.py    # Dynamic command tests
├── test_cmd_executor_*.py          # cmd_executor path tests
├── test_gas_benchmark.py           # Gas comparison benchmarks (explicit)
├── test_gas_benchmark_optimal.py   # Optimal command selection benchmarks
├── test_gas_benchmark_dynamic.py  # Dynamic command + AL benchmarks (max calldata reduction)
├── test_gas_benchmark_access_list.py  # EIP-2930 access list benchmarks
├── test_tstore_executor_*.py      # tstore_executor path tests
```

## Configuration

See `ape-config.yaml`. The project uses:
- **Vyper 0.5.0a3** with `#pragma experimental-codegen` (Venom) and `#pragma optimize gas`
- **Foundry** (Anvil) for local test execution with mainnet fork
- **Custom test mnemonic** to avoid EIP-7702 delegation issues on forked networks

## Known Limitations

1. **V4-only paths beat tstore** (−18.5% explicit, −28.7% V4_BATCH cross-currency) — Venom codegen with function extraction and offset-based cursor enables memory reclamation that the default codegen's stack allocator already provides.
2. **V2_SWAP_CALC has higher gas cost** (~15–21K gas overhead for on-chain computation vs explicit amounts). Useful when calldata cost per byte exceeds execution gas cost, or when on-chain adaptation to reserve shifts between simulation and execution is desired.
3. **V3_SWAP_DELTA only works inside a V4 unlock context where a prior V4 swap has created a positive delta for the input currency** — it reads the amount from PM exttload. Additionally, the V3 auto-pay (when `forward_data=b""`) requires the executor to physically hold the input ERC-20 tokens at callback time. In V4→V3 paths, the standard approach is `V4_TAKE` or `V4_TAKE_DELTA` (which transfers physical tokens to the executor) + `V3_SWAP_COMPACT` with auto-pay. V3_SWAP_DELTA skips the V4_TAKE step, but then the executor may not have physical tokens for auto-pay. For the standard V4→V3 flow, use `V4_TAKE`/`V4_TAKE_DELTA` + `V3_SWAP_COMPACT` with auto-pay instead. See [`user-guide.md`](docs/user-guide.md) §7.3.
4. **V2 and V3 direct custody both require reverse-order execution** — but for different reasons. V3's IIA balance-delta check (`balance_before + amount_owed <= balance_after`) requires input tokens to arrive *during* the swap callback, not before `swap()` is called. Forward-order direct custody (V3a→V3b→V3c) fails because tokens arrive before the next pool's `balance_before` snapshot. Reverse-order (V3c→V3b→V3a) works because each nested swap's output arrives *during* the outer pool's callback window. V2's callback-to-recipient constraint (`uniswapV2Call(to)`) prevents forward-order V2→V2 direct custody — the callback lands on the output recipient, which another V2 pair can't process. Reverse-order (flash borrow from last pool, then V2a→V2b via excess-balance V2 swap inside the executor's callback) bypasses this because `V2_SWAP_CALC`/`V2_SWAP_DIRECT` use `data=b""` (no callback on the output recipient). See `docs/pool-mechanics.md` §5 and §5.1.
5. **V2_SWAP_CALC reads the pair's excess balance** — `amount_in = balanceOf(pair) - reserves`, so the entire excess at the pair is used as swap input. If the pair's excess includes tokens beyond the intended deposit (e.g., an oversized V4_TAKE, multiple deposits from different steps, or accumulated V2 fees from other swappers), the computed `amount_out` will be larger than expected. The V2 fee is configurable per-swap (fraction of 10000: 30=Uniswap, 25=PancakeSwap). `V2_SWAP_CALC` uses the fee inline; `V2_SWAP_COMPACT` writes the fee to transient `t_v2_pair_fee[pool]` for the callback handler (`_v2_auto_pay`) to compute the owed amount with `_v2_get_amount_in()`.
6. **`MAX_COMMANDS_LENGTH = 288`** — command streams are limited to 288 bytes (still 1.4× the typical ~200-byte arbitrage path). Command streams exceeding this will revert.
7. **TSTORE_CONTINUATION (planned)** — not yet implemented. When implemented, it will require a same-tx callback since transient storage is cleared at end of transaction.
8. **Sentinel indices (0xFC–0xFF) reduce the address table maximum to 252 entries** (indices 0x00–0xFB). In practice, arbitrage paths use 3–7 addresses, so this is not a practical limitation. (The prior user sentinels at 0xF0/0xF1 were removed — see commit `8c75fa6` — so user tokens like USDC/WBTC are now regular `t_addresses` entries.)
9. **Inline `_lookup_address` in V4 batch has no gas benefit** — Venom optimizes function calls inside tight loops differently. The batch handler uses the standard `_lookup_address` call.

## ERC6909 Internal Balances

The fake PoolManager now implements the full ERC6909 interface, enabling **internal balance holding** — assets stay inside the PM as accounting entries without physical ERC-20 transfers.

### New Commands

| Opcode | Name | Encoding | Replaces |
|--------|------|----------|----------|
| `0x58` | V4_MINT_COMPACT | `[currency_idx:1][recipient_idx:1][amount:12]` (15B) | take + sync + transfer + settle (4 ops) |
| `0x59` | V4_BURN_COMPACT | `[currency_idx:1][amount:12]` (14B) | sync + transfer + settle (3 ops) |
| `0x04` | TSTORE_CONTINUATION | `[len:2][commands:N]` (3+N) | Write commands to tstore for external callbacks |

### Gas Savings

V4_MINT_COMPACT saves **~20K gas (−18.3%)** vs V4_TAKE for consuming a positive delta, by eliminating the ERC-20 `transfer()` call. The token stays inside the PoolManager as an ERC6909 balance entry.

V4_BURN_COMPACT hardcodes `self` as the burn target (no `account_idx`), eliminating the risk of accidentally burning another account's ERC6909 tokens and saving 1 byte per burn. It enables settling a USDC (or other intermediate) debt from ERC6909 balance instead of a physical transfer — useful in V4→V4 reverse paths where the executor previously minted an intermediate token.

### Use Cases

1. **V4→V4 cross-currency rebalancing**: mint intermediate token as ERC6909 (saves 4 ops), later burn it to settle reverse-path debt (saves 3 ops)
2. **Keeper compounding**: mint output tokens inside PM, then use them as input for subsequent V4 swaps without physical transfers
3. **Atomic rebalancing**: hold multiple currencies inside PM, settle across pools without external ERC-20 round-trips

## Tstore Continuation (0x04) — Planned Feature

> **Note**: TSTORE_CONTINUATION is not yet implemented in the current cmd_executor contract.
> The documentation below describes the planned design.

For external protocols whose callback does NOT pass arbitrary data back to the executor (no `data` parameter), the executor stores continuation commands in transient storage before calling the external protocol. In the callback, the executor reads from tstore and processes the commands.

**TSTORE_CONTINUATION** (`0x04`): `[0x04][len:2][commands:N]` — writes N bytes of continuation commands to tstore.

**onExternalCallback()**: Generic callback entry point for external protocols. Reads the continuation from tstore and processes it. The caller must be a registered callback address.

**When to use**: V2/V3/V4 protocols already support data passthrough (via `forward_data` in callbacks), which is cheaper. Tstore continuation is only needed for:
- Protocols with fixed-parameter callbacks (no `data` field)
- Custom lending/liquidation callbacks with no arbitrary data passthrough
- Cross-protocol paths where the intermediate protocol doesn't carry forward_data

**Gas cost**: ~600-1,200 gas overhead per continuation level (2 TSTORE for metadata + N TSTORE for data + 1-2 TLOAD per chunk on read).

**Key constraint**: The external callback must happen within the **same transaction** as `TSTORE_CONTINUATION`, because transient storage is cleared at the end of the transaction.

## Preprocessing Section & Stream Separators

The command stream optionally starts with a preprocessing section, followed by execution:

```
[0xFE][preprocessing commands][0xFF][execution commands]
```

**0xFE (BEGIN_PREPROCESSING)** must be the first byte of the stream to signal that a preprocessing section follows. If the first byte is NOT 0xFE, the entire stream is treated as execution — no preprocessing pass runs, saving ~1,200 gas for pure-execution paths.

**Preprocessing commands** set up the execution environment. Only SET_ADDRESS remains in the stream; all other configuration (profit check, bribes) is packed into the `config` ABI parameter:

| Opcode | Name | Encoding | Purpose |
|--------|------|----------|---------|
| `0x00` | SET_ADDRESS | `[0x00][address:20]` (21 bytes) | Append address to lookup table |
| `0x01`–`0x03` | *(reserved)* | — | Were SKIP_PROFIT_CHECK, BRIBE_COINBASE, BRIBE_ADDRESS; now in config param |

**0xFF (BEGIN_EXECUTION)** marks the boundary between preprocessing and execution. If an execution opcode appears during preprocessing, preprocessing ends and execution starts from that byte.

This replaces the old ABI-encoded parameters:

| Old ABI parameter | New in-stream | Saved |
|-------------------|---------------|-------|
| `addresses: DynArray[address, 32]` | SET_ADDRESS commands | 12 padding bytes per address + DynArray overhead |
| `expected_balance: uint256` → `config: uint256` | Packed check_mode + bips + recipient + value | Eliminates bribe commands from stream + 2 elif branches |

**Calldata savings** (excluding 4-byte function selector):

| Addresses | Old ABI overhead | New preamble | Saved |
|-----------|-----------------|--------------|-------|
| 3 | 256 bytes | 64 + 1 bytes | **191 bytes** |
| 5 | 320 bytes | 106 + 1 bytes | **213 bytes** |
| 7 | 384 bytes | 148 + 1 bytes | **235 bytes** |

(The `+1` is the 0xFF separator byte; 0xFE prefix was already eliminated.)

## Bribe Configuration (in `config` param)

Built-in MEV bribe support: the executor can bribe block builders or arbitrary addresses with a percentage of the arbitrage profit. Bribe configuration is packed into the `config` ABI parameter (bits 8–23 = bribe_bips, bits 24–31 = bribe_recipient_idx).

- **Coinbase bribe**: `config = (pre_tx_balance << 32) | (bips << 8) | check_mode` — sends to `block.coinbase`. `bips=5000` = 50%, `bips=1000` = 10%.
- **Address bribe**: `config = (pre_tx_balance << 32) | (bips << 8) | (recipient_idx << 24) | check_mode` — sends to an address from the address table. `recipient_idx=0` = coinbase.

The bribe amount is `profit * bips / 10000`, auto-withdrawing WETH if ETH is insufficient, and capped at available balance (never reverts).

Moving bribe config from the command stream to the ABI parameter saved −1,424 gas total (−53 gas per path) by eliminating 2 elif branches in `_preprocess` and the slice/convert/dispatch overhead per field.

## Profit Check Modes

The `config` parameter of `execute()` is packed: `(expected_value << 32) | (bribe_recipient_idx << 24) | (bribe_bips << 8) | check_mode`.

| check_mode | Check Performed | When to Use |
|------------|----------------|-------------|
| 0 | Skip (no check) | `config=0` — off-chain verification only |
| 1 | `WETH.balanceOf(self) + self.balance >= value` | Default for V2/V3/V4+other paths (WETH warm from transfers) |
| 2 | `PM.balanceOf(self, weth_id) >= value` | V4V4V4 with `V4_MINT_COMPACT` profit capture |

**Why mode 2 exists**: On pure V4 paths (V4V4V4), the `WETH.balanceOf(self)` SLOAD is cold (~2,600 gas) because V4 operations use delta accounting — no physical WETH transfers. But after `V4_MINT_COMPACT` writes to the ERC6909 slot, reading `PM.balanceOf(self, weth_id)` is warm (~100 gas). Mode 2 saves ~3,500 gas on V4V4V4 by reading the warm ERC6909 slot instead of cold WETH.

The operator constructs:
- Mode 1: `(pre_tx_weth_eth_balance << 32) | 1`
- Mode 2: `(2 << 248) | (pre_tx_erc6909_weth_balance)`
- Skip: `0`

2^248 ≈ 3.4×10^74 wei — far exceeds any real token balance, so the top 8 bits are always zero and safe to use for the mode flag.

## `#pragma experimental-codegen` (Venom Backend) — Recommended

Vyper's experimental Venom codegen (enabled via `#pragma experimental-codegen` on 0.5.0a3) **beats the default codegen on all 7 paths** after function extraction enabled Venom's liveness analysis to reclaim memory across mutually exclusive command handlers. The progression from +5–9% regression to −0.6–1.9% improvement demonstrates that **contract structure dictates Venom performance** — the algorithm is correct, but monolithic dispatch loops defeat its liveness analysis.

### Three Key Structural Changes

1. **Offset-based cursor** (eliminated `Bytes[512]` returns → `uint256` returns): Removed 22 × 544-byte alloca return buffers that dominated Venom's memory footprint. Regression: +5–9% → +1.3–3.9%.

2. **Function extraction** (split monolithic `_execute_command_at` into 26 `@internal` `_cmd_*` functions): Allowed Venom's `ConcretizeMemLocPass` to reclaim memory across command handlers. Highest memory address: 22,976 → 8,544 (−62.8%). Result: Venom now beats default codegen on all paths.

3. **Thin dispatch** (`_execute_command_at` only reads opcode and delegates): The dispatch function has near-zero memory footprint, so Venom's `invoke` liveness tracking at the dispatch point sees minimal overlap.

### Why Function Extraction Works for Venom

The default codegen uses a stack-style `MemoryAllocator` with `deallocate_memory()` — it frees memory when variables go out of scope, regardless of control flow reachability. Mutually exclusive branches in the same function can share memory. This works perfectly for monolithic functions.

Venom uses monotonic alloca allocation (no `deallocate_memory()`). Its `ConcretizeMemLocPass` reclaims memory via **liveness analysis** — two allocas with non-overlapping liveness can share memory. But in a **monolithic dispatch loop**, all command handlers' variables are reachable from the loop header, so their liveness overlaps. The allocator cannot distinguish "reachable but mutually exclusive" from "concurrently live."

**With function extraction**, each `_cmd_*` handler is a separate function. When `_execute_command_at` dispatches to `_cmd_v4_swap_compact`, Venom's liveness analysis at the `invoke` site only marks that function's `mems_used` as live. When it dispatches to `_cmd_v3_swap_compact` instead, a different (and potentially overlapping) set of allocas is live. Since the two `invoke` sites are in different basic blocks of the dispatch function, `ConcretizeMemLocPass` can recognize that the two callees' memory regions are mutually exclusive and **assign them overlapping offsets**.

### Memory & Bytecode Comparison

| Metric | Default (extracted) | Venom (extracted) | Δ |
|--------|---------------------|-------------------|---|
| Memory allocator | Stack + deallocate | Monotonic + liveness reuse |
| Highest memory address | ~5,440 | ~8,544 | 1.6× (was 4.2× monolithic) |
| SWAP1 count | — | **724** | |
| Runtime bytecode | 18,676 | **15,106** (pre-SET_ADDRESS) | −3,570 (−19.1%) |

*With Venom + function extraction + `optimize gas`, the current cmd_executor is 15,359 bytes (includes sentinel address resolution, inline _lookup_address, SET_ADDRESS, packed config param for profit-check and bribes, ERC6909 commands, SEND_ETH, excess-balance V2 swaps, and the 0xFF preprocessing/execution partition).*

### Gas Impact Benchmarks

### `#pragma optimize gas` vs `optimize codesize`

Switching from `optimize codesize` to `optimize gas` saves **224–932 gas per path** (avg ~640) at the cost of **+3,821 bytes** larger runtime bytecode (+21.7%):

| Path | cmd (codesize) | cmd (gas) | Δ gas |
|------|---------------|-----------|-------|
| V4→V4 | 71,321 | 70,724 | **−597** |
| V4→V3 | 122,460 | 121,619 | **−841** |
| V3→V4 | 122,496 | 121,564 | **−932** |
| V4→V2 | 127,683 | 126,981 | **−702** |
| V2→V3 | 127,519 | 126,960 | **−559** |
| V3→V2 | 124,422 | 124,004 | **−418** |
| V2 direct | 87,624 | 87,400 | **−224** |

| Metric | codesize | gas | Δ |
|--------|----------|-----|---|
| Runtime bytecode | 17,598 B | 21,512 B | +3,914 B (+22.2%) |

**Verdict**: For an arbitrage executor where gas is the critical metric, `optimize gas` is the right choice. The bytecode increase (17.6 KB → 21.3 KB) is well within the 24 KB EIP-170 limit.

#### Venom extracted vs Default monolithic (the real-world comparison)

Users comparing "should I use Venom?" care about the default codegen baseline (monolithic, since default codegen doesn't need extraction) vs Venom with extraction:

| Path | Default (mono) | Venom (extracted) | Δ gas | Δ % |
|------|---------------|-------------------|-------|-----|
| V4→V4 | 72,523 | 71,124 | **−1,399** | **−1.9%** |
| V4→V3 | 119,164 | 117,531 | **−1,633** | **−1.4%** |
| V3→V4 | 119,369 | 117,506 | **−1,863** | **−1.6%** |
| V4→V2 | 115,464 | 113,685 | **−1,779** | **−1.5%** |
| V2→V3 | 110,132 | 109,381 | **−751** | **−0.7%** |
| V3→V2 | 106,780 | 105,853 | **−927** | **−0.9%** |
| V2 direct | 78,352 | 77,879 | **−473** | **−0.6%** |

#### Function extraction: net cost on default (+200–660 gas) vs net benefit on Venom (−2,162 to −5,009 gas)

| Path | Default Δ | Venom Δ | Net (Venom benefit − Default cost) |
|------|-----------|---------|-------------------------------------|
| V4→V4 | +332 | −3,372 | −3,704 |
| V4→V3 | +506 | −4,977 | −5,483 |
| V3→V4 | +662 | −4,914 | −5,576 |
| V4→V2 | +492 | −3,304 | −3,796 |
| V2→V3 | +351 | −5,009 | −5,360 |
| V3→V2 | +334 | −3,338 | −3,672 |
| V2 direct | +202 | −2,162 | −2,364 |

### Historical: How Each Change Affected Venom

| Change | Venom vs default | Venom memory | Key effect |
|--------|-----------------|--------------|------------|
| Baseline (Bytes[512] returns) | +5–9% | 36,224 | Monolithic dispatch + return buffers |
| + Offset-based cursor (uint256 returns) | +1.3–3.9% | 22,976 | Removed 22×544B return buffers |
| + Function extraction | **−0.6 to −1.9%** | **8,544** | Liveness can now reclaim across handlers |

### Verdict

**Venom with function extraction is the recommended codegen.** It beats the default codegen on all 7 paths by −0.6 to −1.9%, while also producing smaller bytecode. Both codegens pass all tests.

The default codegen still works (and is needed if you can't use `#pragma experimental-codegen`). Function extraction adds +200–660 gas overhead per path on the default codegen due to internal call dispatch, but this is more than offset by Venom's memory reclamation.

Note: The function extraction pattern is specific to **dispatch-heavy contracts** where mutually exclusive handlers are branched from a loop. For contracts with simple control flow, Venom's liveness analysis already works well without extraction.
