# AGENTS.md — Executor Project Context

> **Read this first** when picking up this project. It distills the critical context that isn't obvious from reading the code alone.

## What This Project Is

An on-chain arbitrage executor for Uniswap V2/V3/V4, written in **Vyper 0.5.0a3** with the experimental Venom codegen backend. The primary contract (`cmd_executor.vy`) is a compact byte-stream VM that executes multi-hop swap paths across all three Uniswap protocol versions in a single atomic transaction.

**No prefunding required** — the executor borrows all working capital atomically via V2/V3 flash swaps and V4 PoolManager `take()`. Can be deployed with zero balance.

## Quick Orientation

| Item | Location | Notes |
|------|----------|-------|
| Main contract | `contracts/cmd_executor.vy` | 1931 lines, **only file to optimize** |
| Legacy contract | `contracts/tstore_executor.vy` | Older payload-queue executor, not the target |
| Fake V2 pair | `contracts/fake_uniswap_v2_pair.vy` | K-invariant + configurable fee + 3 callback variants |
| Fake V3 pool | `contracts/fake_uniswap_v3_pool.vy` | Balance-delta IIA check + 2 callback variants |
| Fake V4 PM | `contracts/fake_uniswap_v4_pool_manager.vy` | exttload + ERC6909 + delta accounting |
| Fake ERC20 | `contracts/fake_erc20.vy` | Standard mock with mint |
| Fake WETH | `contracts/fake_weth.vy` | deposit/withdraw wrapping |
| Test suite | `tests/` | ~276 tests, run with `uv run ape test tests/ -v -s` |
| Benchmark | `tests/test_cmd_executor_three_hop_optimized.py` | 27 three-hop permutations — the gas benchmark |
| Gas results | `.gas-results` | Written by test suite, consumed by `.auto/measure.sh` |
| Autoresearch | `.auto/` | Config, logs, ideas from gas optimization sessions |

## Build & Run

```bash
uv run ape test tests/ -v -s              # All tests
uv run ape test tests/test_cmd_executor_*.py -v   # cmd_executor only
uv run ape test tests/test_cmd_executor_three_hop_optimized.py -v -s  # Gas benchmark
```

Uses **Foundry (Anvil)** for local test execution. `ape-config.yaml` sets Vyper 0.5.0a3, mainnet-fork default, and a custom test mnemonic (to avoid EIP-7702 delegation issues).

Sequential runs (`-j1`) if xdist races appear. The test suite uses Hypothesis for fuzz testing — `.hypothesis/` contains cached examples.

## Contract Architecture: cmd_executor

### Core Flow

1. **`execute(commands: Bytes[MAX_COMMANDS_LENGTH], config: uint256 = 0)`** — Owner-only entry point. `config` is packed: `(expected_value << 32) | (bribe_recipient_idx << 24) | (bribe_bips << 8) | check_mode` (low byte = mode: 0=skip, 1=WETH+ETH, 2=ERC6909 WETH; bits 8-23 = bribe bips; bits 24-31 = bribe recipient address table index; bits 32-255 = expected value). Calls `_preprocess()` unconditionally (parses SET_ADDRESS until `0xFF` or first non-preprocessing opcode), then iterates `_execute_command_at()` until the stream is exhausted.

2. **`_execute_command_at(data, offset) → uint256`** — Reads a 1-byte opcode, dispatches to one of 26 `_cmd_*` internal functions via two-level dispatch (high nibble first, then exact match). Returns the offset of the next command.

3. **Callback handlers** — `uniswapV2Call`, `hook`, `pancakeCall` (V2), `uniswapV3SwapCallback`, `pancakeV3SwapCallback` (V3), `unlockCallback` (V4) — each processes commands from the callback data by iterating `_execute_command_at()` until the stream is exhausted.

### Why Function Extraction Matters

The 26 `_cmd_*` functions are extracted from a monolithic dispatch loop. This is **critical for Venom's liveness analysis**: Venom uses monotonic alloca allocation, and `ConcretizeMemLocPass` reclaims memory only when liveness proves two allocas are mutually exclusive. In a monolithic function, all handlers' variables are reachable → no sharing. With extraction, each `_cmd_*` is a separate invoke → Venom can overlap their memory regions.

**Result**: Highest memory address dropped from 22,976 to 8,544 (−62.8%). This makes Venom beat the default codegen on all paths.

### Command Encoding (Compact Binary)

Commands use 1-byte opcodes + tightly-packed fields (no ABI encoding). Key field sizes:
- **Amounts**: `uint96` (max 7.9×10²⁸, 12 bytes) — covers all practical token amounts
- **V4 fee**: `uint16` (500/3000/10000 fit, 2 bytes)
- **V4 tick_spacing**: `int16` (10/60/200 fit, 2 bytes)
- **Indices**: `uint8` (1 byte) — refer to address table or sentinel values
- **V2 fee**: `uint16` inline per-swap (2 bytes)

### Sentinel Address System (CRITICAL — biggest gas win: −67,786 gas)

Address indices `0xFC`–`0xFF` resolve to the **4 protocol-role sentinels** without TLOAD or SET_ADDRESS:

| Index | Sentinel | Resolves To |
|-------|----------|-------------|
| `0xFC` | `V4_PM_SENTINEL` | `POOL_MANAGER_ADDR` (immutable) |
| `0xFD` | `V4_SELF_SENTINEL` | `self` (executor address) |
| `0xFE` | `V4_WETH_SENTINEL` | `WETH_ADDR` (immutable) |
| `0xFF` | `V4_NATIVE_SENTINEL` | `NATIVE_ADDRESS` / no-hooks indicator |

**Only protocol roles are sentinels — no path-specific tokens are baked into the contract.**
User tokens (USDC, WBTC, DAI, …) are *never* sentinels; they go through `t_addresses` via `SET_ADDRESS` per transaction, exactly like every other address. The prior `USER0`/`USER1` sentinels were removed because their savings were partly a benchmark artifact (the 27-path suite uses exactly two hot user tokens in fixed roles) and their `else: USER1_ADDR` catch-all silently mis-resolved unbound reserved bytes (`0xF2`–`0xFB`) — a latent bug. See `.auto/prompt.md` OVERFITTING section and commit `8c75fa6`.

Range-check optimization: `if idx >= SENTINEL_THRESHOLD (0xFC)` dispatches sentinel resolution; `< 0xFC` does direct `t_addresses[idx]` array lookup. Any byte `>= 0xFC` that isn't PM/SELF/WETH/NATIVE raises `InvalidCommand` (fail-closed, no silent catch-all).

**Inline sentinel resolution** is used in hot handlers (V4_SWAP_COMPACT, V4_TAKE_COMPACT, ERC20_TRANSFER, V2/V3 swaps) to avoid `_lookup_address` INVOKE overhead. With only 4 protocol sentinels, branch ordering within these chains is by *protocol-role* frequency (WETH first for currency fields; SELF first for recipient fields) — a defensible global ordering, not a per-handler benchmark-frequency tuning.

### Transient State

| Variable | Type | Purpose |
|----------|------|---------|
| `t_callback_packed` | `transient(uint256)` | Low 160 bits = callback address, bits 160-175 = V2 fee. Single TSTORE per V2 swap (address + fee packed). |
| `t_addresses` | `transient(address[32])` | Address lookup table populated by SET_ADDRESS commands during preprocessing. |

V4 deltas are **NOT** tracked locally — read from the PoolManager's own transient storage via `exttload()`. This eliminates tracker drift risk.

Precomputed WETH/NATIVE delta slots are stored as immutables (`WETH_DELTA_SLOT`, `NATIVE_DELTA_SLOT`) to skip `keccak256` on hot paths.

### Opcodes (grouped by protocol at 0x10 boundaries)

```
0x00-0x03  Control: SET_ADDRESS; 0x01-0x03 reserved (were SKIP_PROFIT_CHECK, BRIBE_COINBASE, BRIBE_ADDRESS — now in config param)
0x10-0x17  ERC20/ETH: ERC20_TRANSFER, XFER_BALANCE, WETH_DEPOSIT/WITHDRAW (±ALL), SEND_ETH (±ALL)
0x20-0x22  V2: V2_SWAP_COMPACT, V2_SWAP_CALC, V2_SWAP_DIRECT
0x30-0x31  V3: V3_SWAP_COMPACT, V3_SWAP_DELTA
0x40-0x42  V4 Swaps: V4_SWAP_COMPACT, V4_SWAP_DYNAMIC, V4_BATCH
0x50-0x59  V4 Settlement: V4_UNLOCK, V4_TAKE, V4_TAKE_COMPACT, V4_TAKE_DELTA,
           V4_SYNC, V4_SETTLE, V4_SETTLE_DELTA, V4_SETTLE_ALL,
           V4_MINT_COMPACT, V4_BURN_COMPACT
0xFF       BEGIN_EXECUTION (end of preprocessing / start of execution)
0xFF       BEGIN_EXECUTION (separator between preprocessing and execution)
```

### Callback Behavior

| Protocol | Auto-pay trigger | Mechanism |
|----------|-----------------|-----------|
| V2 | `len(data) == 1` (0xFE sentinel) | Computes owed amount from `getReserves() + fee`, auto-transfers |
| V3 | `len(data) == 0` (empty forward_data) | Reads positive delta from callback params, auto-transfers `token0()`/`token1()` |
| V4 | unlockCallback | Processes command stream inside PM unlock context |

## Fake Contract Invariants

The fake contracts replicate **the same invariant checks** as real Uniswap. Tests that pass here will pass on mainnet:

- **V2**: K-invariant (`balance0Adjusted * balance1Adjusted >= reserve0 * reserve1 * 10000²`), configurable fee (0.3% Uniswap, 0.25% PancakeSwap), reentrancy guard, `getReserves()`
- **V3**: Balance-delta IIA check (`balance_before + amount_owed <= balance_after`), reentrancy guard, sqrtPriceLimitX96 validation, `amountSpecified ≠ 0`
- **V4**: sync/settle via balance delta, CurrencyNotSettled on unlock exit, NonzeroDeltaCount tracking, full ERC6909 mint/burn/transfer/allowance

**Fake contracts are OFF-LIMITS for optimization** — their checksums are verified by `.auto/measure.sh` against `.auto/baseline_checksums.json`.

## Vyper/Venom Constraints

1. **No `while` loops** — Vyper 0.5.0a3 only supports `for range()`. `while` causes `SyntaxException`.
2. **`#pragma experimental-codegen`** (Venom) and `#pragma optimize gas` are required. Do NOT switch to `optimize codesize`.
3. **Named variables help Venom** — they give Venom better liveness info for memory allocation. Inlining named `uint256`/`address` variables into struct constructors **regresses +242 gas** each.
4. **Bytes intermediaries should be inlined** — if a `Bytes[N]` variable is constructed from a slice and used once in an extcall, remove the variable and pass the slice directly. Saves Venom memory allocation.
5. **`unsafe_add`/`unsafe_sub`** — Every offset arithmetic (`offset + constant`) in a 512-byte stream can never overflow. Use `unsafe_add` to skip ADD+JUMPI overflow checks. Same for balance sums, delta negation (with care).
6. **Venom already optimizes**: `%` for power-of-2 → bitwise AND, double-convert folding, constant expression folding, `@pure` vs `@internal` inlining, `len()` caching, `shift()` vs `>>`.
7. **Custom errors** — Vyper 0.5.0a3 adds module-level `error` declarations. The contract uses 8 custom errors (`Unauthorized`, `InvalidCallback`, `InsufficientBalance`, `InsufficientProfit`, `InvalidCommand`, `BipsTooHigh`, `InvalidMsgValue`, `NotPlainEthTransfer`) with `raise` and `assert ..., ErrorName(args)`. Error arguments are evaluated on the failure path only, so successful assertions have zero gas overhead. Bytecode cost is +231 bytes for the error selector + encoding logic.

### Inspecting Venom IR with `vyper -f ir_runtime`

Before committing to a code change, you can inspect Venom's intermediate representation to see exactly how it transforms your code — without waiting for a full test run.

```bash
# Generate the Venom IR (after Venom passes, before EVM code generation)
# This reveals: inlining decisions, control-flow structure, alloca placement,
# redundant operation elimination, and why certain patterns compile well or badly.
uv run vyper -f ir_runtime contracts/cmd_executor.vy > /tmp/venom_ir.vy
```

**How to use this in practice:**

1. **Confirm inlining** — Search for your function name (e.g., `_preprocess`, `_lookup_address`). If Venom has inlined it, you won't see an `invoke` or `call` to it in the IR; its body will be expanded at the call site.

2. **Check alloca overlap** — Look for `alloca` instructions at function entry. Two functions with non-overlapping liveness will reuse the same memory offsets. If allocas are piling up (high offsets), liveness isn't overlapping — consider further extraction or named intermediates.

3. **Verify bounds-check elimination** — After `slice` calls, look for `assert`/`clamp` instructions. Venom should remove provably-safe bounds checks (e.g., when slicing from a known-calldata buffer with a compile-time-valid offset). If clamps remain, your `unsafe_add`/`unsafe_sub` may not have propagated far enough.

4. **Spot redundant masks** — Search for `and` with constants like `255` or `65535` immediately after a `shr`. If they appear after a shift from a merged slice read where the high bits are already zero, Venom failed to eliminate them — that's a candidate for manual removal.

5. **Compare before/after** — Diff the IR of two versions of the contract to see exactly what changed in Venom's output. This is faster and more precise than running the full 27-path benchmark for speculative tweaks.

**Key IR patterns to look for:**
- `invoke <func_name>` — function was NOT inlined (INVOKE overhead = ~8-30 gas)
- `alloca <size> @ <offset>` — memory allocation; high offsets = poor overlap
- `clample` / `clampge` — bounds checks Venom could not eliminate
- `store` → `load` pairs with no intervening side-effects — candidate for CSE
- `and X, 255` after `shr X, N` — redundant mask that may be removable

## Gas Optimization: What Works (Priority Order)

These are proven patterns — see `.auto/ideas.md` for exhaustive details:

1. **Sentinel addresses** (−67,786 gas) — Eliminate SET_ADDRESS + TLOAD for the 4 protocol-role addresses (PM/SELF/WETH/NATIVE). This is the figure in the table below for the 4 surviving sentinels; the now-removed user sentinels were a *separate, additional* win (segments 2–3, see dead-ends table + commit `8c75fa6`).
2. **`unsafe_add`/`unsafe_sub` for offset arithmetic** (−19,997 gas) — Skip overflow checks on provably-safe additions
3. **Merged slice reads** (−15,800+ gas) — Read N+M bytes in one `slice()`, extract with `>>` and `&`. Saves one bounds check per merge.
4. **Local memory vars instead of per-iteration TSTORE** (−29,224 gas) — Accumulate in locals, flush transient once
5. **Remove defensive guards** (−5,000+ gas) — Assertions with strings bloat bytecode; `forward_len > 0` guards are unnecessary (`slice(offset, 0) == b""`)
6. **Dispatch reorder by frequency** (−11,625 gas) — Put most common opcodes first in if/elif chain
7. **Packed ABI config parameter** (−1,424 gas) — Move bribe/profit-check config from command stream to ABI uint256 parameter. ABI decoding is free; command stream requires slice/convert/dispatch.
8. **Inline `_lookup_address` in hot handlers** (−3,452 gas) — Avoids INVOKE overhead for sentinel resolution
9. **Shrink encoding fields** (−3,686 gas) — `uint128→uint96`, `uint24→uint16`, `uint16→uint8`
10. **Fixed array + count instead of DynArray** (−4,434 gas) — Eliminates bounds checks
11. **Invert defaults for preprocessing** (−9,849 gas) — SKIP_PROFIT_CHECK defaults True, 0xFE prefix removed. (Note: Opcodes 0x01-0x03 are now reserved — profit check and bribe config are packed into the ABI `config` parameter.) The 0xFE stream prefix was removed entirely; `_preprocess` runs unconditionally from offset 0. The 0xFE byte is now only the V2 auto-pay sentinel and the `V4_WETH_SENTINEL` constant — never a stream prefix.

## What DEFINITELY Doesn't Work

Do NOT re-explore:

| Pattern | Why |
|---------|-----|
| Inline named `uint256`/`address` into struct constructors | Hurts Venom liveness (+242 gas each) |
| Callback handler deduplication | +754-825 gas regression |
| Conditional TSTORE (skip when unchanged) | +4,404 gas — branch overhead exceeds TSTORE |
| `V4_TAKE_DELTA` replacing `V4_TAKE_COMPACT` | +12,913 gas — `_lookup_address` + `_read_pm_delta` overhead exceeds 12-byte calldata savings |
| Forward_data expansion for callback data | ~147 gas/byte in extcall forward_data vs ~16 gas/byte in top-level calldata |
| Remove `0xFF` separator | +1,029 gas — `0xFF` enables cheaper Venom code path |
| New compound commands | Bytecode expansion + dispatch overhead exceed calldata savings |
| `_lookup_address` mass inlining | +134 gas, +1,360 bytes — only specific handlers benefit |
| V4_BATCH for V4→V4 | +2,639 gas — auto-settle overhead exceeds dispatch savings |
| Auto-settle inside unlockCallback | +62,040 gas — keccak256 + exttload per callback too expensive |
| Caching `len(data)` | +1,152 gas — Venom's `len()` is already just MLOAD |
| Loop bound reduction | No gas change — Venom handles via break conditions |
| `expected_balance` as command stream (SET_EXPECTED_BALANCE) | +8,183 gas — ABI parameter decoding is free; command stream requires slice/convert/dispatch |
| Address table as ABI parameter | Fixed `address[32]` costs ~4,000-5,200 gas vs ~1,000 gas for stream-encoded SET_ADDRESS; dynamic `bytes` adds offset+length overhead |
| `msg.value` for config (full expected_value) | `msg.value` persists in `self.balance`, contaminating profit check. Refund costs ~9,000 gas (CALL). Small flags only saves ~60 gas per path — not worth complexity + balance leak |
| Top-level `_execute_command_at` dispatch elif reorder | +1,132 gas — Venom bytecode layout is layout-sensitive; even >12× frequency diffs regress. Current dispatch order is at a local optimum. |
| User sentinels (`USER0`/`USER1`, bytes `0xF0`/`0xF1`) | **Removed (commit 8c75fa6).** Their `else: USER1_ADDR` catch-all silently mis-resolved unbound reserved bytes `0xF2`–`0xFB` to `USER1` (latent bug), and their savings were partly a benchmark artifact (the suite uses exactly two hot user tokens in fixed roles). Kept only the 4 protocol-role sentinels (PM/SELF/WETH/NATIVE); user tokens now go through `t_addresses` via `SET_ADDRESS`. Honest +40,577 gas regression accepted in exchange for correctness + generalizability. |
| Per-handler sentinel `elif` reorder to match *benchmark* token frequencies | **OVERFITTING.** Reordering a handler's chain so `USER0`/`USER1` precede `WETH` saves gas only because the synthetic tests use USDC/WBTC; production nested-swap paths (WETH paid inside a V4 unlock) regress. With user sentinels now removed this class is structurally closed, but the rule stands for the 4 protocol sentinels: order by *protocol-role* frequency (WETH-first currency, SELF-first recipient) globally, never by which address a particular benchmark path happens to use. |

## Autoresearch History

The `.auto/log.jsonl` contains 253+ experiments across 3 compaction segments. Key milestones:

| Segment | Baseline → Best | Savings | Key Wins |
|---------|----------------|---------|----------|
| 0 | 4,416,991 → 4,374,918 | −42,073 | Dispatch reorder, unsafe arithmetic, merged slices, function extraction |
| 1 | 4,374,918 → 4,298,800 | −76,118 | V4_TAKE→V4_TAKE_COMPACT, sentinel addresses (NATIVE/WETH/SELF/PM), merged idx reads, no-hooks sentinel |
| 2 | 4,298,800 → 4,179,191 | −119,609 | WETH/NATIVE delta slot precomputation, shift→>> replacement, forward_len guard removal, merged multi-byte reads, user sentinels (0xF0/0xF1)†, encoding size reductions |
| 3 | 4,507,479 → 4,468,322 | −39,157 | User sentinels (USDC/WBTC)†, sentinel branch reorder by frequency |

† The user-sentinel wins in segments 2–3 (the `0xF0`/`0xF1` bytes mapping to deploy-time `USER0`/`USER1` immutables) were **subsequently removed** (commit `8c75fa6`, Session 13). Their `else: USER1_ADDR` catch-all silently mis-resolved unbound reserved bytes `0xF2`–`0xFB` (latent bug) and their savings were partly a benchmark artifact. Removing them cost +40,577 gas (+~1,503/path) — accepted as an intentional correctness-for-gas trade — and −1,123 bytes bytecode.

**Current state**: ~4,947,078 gas across 27 paths, ~15,359 bytes bytecode (post user-sentinel removal). Diminishing returns — all code-level optimizations exhausted. Remaining savings require protocol-level changes (off-limits).

## Key Domain Concepts

### WETH/Ether-Only Custody Invariant

**The executor only ever takes custody of WETH and Ether.** All non-WETH/ETH tokens (USDC, WBTC, etc.) flow directly between pools via direct custody — they never pass through the executor's ERC20 balance.

This is a structural property of valid arbitrage paths: the operator designs paths so that intermediate assets are WETH/ETH exclusively. Non-WETH/ETH tokens are always sent from one pool directly to the next pool (or the PM) via direct custody. A path that delivers USDC or WBTC to the executor would be suboptimal — it would require an extra transfer to forward it onward.

**Implications:**
- In V2/V3 callbacks, `amount0 + amount1` is always the WETH (or ETH) inflow — the other amount is zero, or both are the same token. No `token0()`/`token1()` calls needed to identify which token was received.
- The profit check only needs to track WETH + ETH balance changes — no other ERC20 balances matter.
- This invariant enables arithmetic profit tracking: accumulate WETH/ETH flows in transient storage instead of reading `WETH.balanceOf(self)` + `self.balance` at start and end.

### Profit Check & Bribe Configuration (Packed `config`)

**The steady-state operating mode is to always verify profitability.** The `config` parameter is packed: `(expected_value << 32) | (bribe_recipient_idx << 24) | (bribe_bips << 8) | check_mode`. The low byte (bits 0-7) selects the check mode; bits 8-23 are bribe bips (0=no bribe); bits 24-31 are the bribe recipient address table index (0=coinbase); bits 32-255 are the expected pre-tx balance.

| check_mode | Check Performed | When to Use |
|------------|----------------|-------------|
| 0 | Skip (no check) | `config=0` — off-chain verification |
| 1 | `WETH.balanceOf(self) + self.balance >= value` | Default for V2/V3/V4+other paths (WETH warm from transfers) |
| 2 | `PM.balanceOf(self, weth_id) >= value` | V4V4V4 with `V4_MINT_COMPACT` profit capture (ERC6909 slot warm from MINT, saves ~3,500 gas vs cold WETH) |

The operator constructs `config = (pre_tx_balance << 32) | 1` for mode 1, or `(pre_tx_balance << 32) | 2` for mode 2. With bribes: `(pre_tx_balance << 32) | (bribe_bips << 8) | check_mode`.

**Key insight for V4V4V4**: `WETH.balanceOf(self)` is cold (~2,600 gas) because V4 operations use delta accounting (no physical WETH transfers). But after `V4_MINT_COMPACT` writes to the ERC6909 slot, reading `PM.balanceOf(self, weth_id)` is warm (~100 gas). Mode 2 exploits this, saving ~3,500 gas on pure V4 paths.

Gas benchmarks must run with profit checks active to measure real-world cost. Skipping the check (`config=0`) saves gas but represents a degenerate case — it should never be the benchmark baseline.

### Direct Custody & Reverse-Order Execution
- **V3 IIA check** requires tokens to arrive *during* the callback, not before. This forces **reverse-order** execution for V3 chains: inner pools fire first (V3c→V3b→V3a), output sent directly to outer pool.
- **V2 callback-to-recipient** constraint means `uniswapV2Call(to=X)` where `X` is another V2 pair fails. V2 chains also use reverse-order (V2c flash borrow first, then V2a→V2b via V2_SWAP_CALC).
- **V4** has no custody issues — deltas accumulate in transient storage, no intermediate transfers needed.

### V2_SWAP_CALC Excess Balance
`amount_in = balanceOf(pair) - reserves` — tokens deposited to pair but not yet reflected in reserves (e.g., from `V4_TAKE(currency, recipient=pair)`). Computes output on-chain, calls `pair.swap()` with `data=b""` (no callback). Saves calldata but costs ~15-21K gas overhead for on-chain math.

### ERC6909 Internal Balances (V4)
- **V4_MINT_COMPACT** (0x58) — convert positive PM delta to ERC6909 balance (no physical transfer). Saves ~20K gas for profit capture in V4→V4 paths.
- **V4_BURN_COMPACT** (0x59) — convert ERC6909 balance to PM delta. Hardcodes `self` as burn target.
- Only V4V4V4 benefits from MINT for profit capture — regresses ~130 gas inside any V2/V3 callback context.

### Bribe System
Bribe configuration is packed into the ABI `config` parameter (bits 8-23 = bribe_bips, bits 24-31 = bribe_recipient_idx). Bribe = `profit × bips / 10000`. Auto-withdraws WETH if ETH insufficient. Capped at available balance (never reverts).

## Files NOT to Modify

| File | Reason |
|------|--------|
| `contracts/fake_*.vy` | Fake contracts — checksums verified by measure.sh |
| `contracts/tstore_executor.vy` | Separate implementation, not optimization target |
| `contracts/interfaces/` | Interface definitions |
| `contracts/utility_functions.vy` | Shared helpers |
| `contracts/ExttloadComparator.vy` | Verification utility |
| `.auto/baseline_checksums.json` | Baseline checksums for fake contracts |

## Additional Documentation

| Document | Subject |
|----------|---------|
| `docs/pool-mechanics.md` | V2/V3/V4 timing constraints, sync/settle ordering, reverse-order execution |
| `docs/pm-as-bank.md` | PoolManager as flash-loan source, MINT vs TAKE analysis |
| `docs/erc6909-arbitrage.md` | ERC6909 internal balance arbitrage strategies |
| `docs/transfer-count-investigation.md` | Minimum transfer analysis per path |
| `README.md` | Comprehensive project documentation (gas benchmarks, architecture, command set) |
| `SECURITY_REVIEW.md` | Security audit findings and fixes |
| `FAKE_CONTRACT_AUDIT.md` | Fake vs real contract invariant comparison |
| `OPTIMIZATION_ANALYSIS.md` | Callback invariant analysis and optimization proposals |
| `docs/arithmetic-profit-tracking-plan.md` | Arithmetic profit tracking analysis (TSTORE overhead vs balanceOf savings) |
| `.auto/ideas.md` | Exhaustive optimization log with proven patterns and dead ends |
