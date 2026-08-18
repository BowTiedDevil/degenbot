# Autoresearch: cmd_executor gas reduction

## Objective
Reduce execution gas for the `cmd_executor.vy` Vyper smart contract across the 27 optimized arbitrage paths. The contract is a compact command-stream executor for Uniswap V2/V3/V4 arbitrage. It uses `#pragma experimental-codegen` (Venom) and `#pragma optimize gas`. The primary target is the sum of cmd_executor optimal gas across all 27 paths.

## Metrics
- **Primary**: total_gas (gas units, lower is better) — sum of cmd_executor optimal gas across 27 three-hop paths
- **Secondary**: 
  - bytecode_size (bytes) 
  - per-path gas for all 27 permutations (gas units, lower is better):
    - v2_v2_v2,
    - v2_v2_v3, 
    - v2_v2_v4,
    - v2_v3_v2, 
    - v2_v3_v3,
    - v2_v3_v4, 
    - v2_v4_v2,
    - v2_v4_v3, 
    - v2_v4_v4, 
    - v3_v2_v2,   
    - v3_v2_v3, 
    - v3_v2_v4, 
    - v3_v3_v2,   
    - v3_v3_v3,   
    - v3_v3_v4,   
    - v3_v4_v2, 
    - v3_v4_v3,               
    - v3_v4_v4,   
    - v4_v2_v2, 
    - v4_v2_v3, 
    - v4_v2_v4, 
    - v4_v3_v2, 
    - v4_v3_v3,   
    - v4_v3_v4, 
    - v4_v4_v2, 
    - v4_v4_v3, 
    - v4_v4_v4, 

## How to Run
`./.auto/measure.sh` — outputs `METRIC name=number` lines.

## Files in Scope
- `contracts/cmd_executor.vy` — the main contract being optimized. This is the ONLY contract to modify.
  - Core dispatch loop: `_execute_command_at()` at line ~600
  - Command handlers: `_cmd_*` internal functions
  - Entrypoint: `execute(commands: Bytes[512], expected_balance: uint256 = 0)` — parses command stream, runs preprocessing, executes commands, performs profit check. `expected_balance` is packed: `(check_mode << 248) | expected_value` (mode 0=skip, 1=WETH+ETH, 2=ERC6909 WETH for V4V4V4).
- Test files (tests/) — may need updates when command encoding changes (e.g., transfer counts, field widths)

## Off Limits
- `contracts/fake_*.vy` — fake contracts used in tests; checksums must match baseline
- `contracts/tstore_executor.vy` — separate implementation, not the optimization target
- `contracts/interfaces/` — interface definitions, must not change
- `contracts/utility_functions.vy` — shared helpers
- `.auto/baseline_checksums.json` — baseline checksums for fake contracts

## Constraints
- All tests must pass (`uv run ape test tests/ -v`) — use sequential runs (`-j1`) if xdist races appear
- `#pragma optimize gas` must remain (do NOT switch to `optimize codesize`)
- `#pragma experimental-codegen` must remain (Venom backend is required)
- Vyper 0.5.0a2 — only features available in this version or later. **No while loops** (SyntaxException: unsupported 'While' Python AST node).
- Runtime bytecode must fit within EIP-170 24KB limit

## Architecture & Key Insights

### Profit Check Modes (Packed `expected_balance`)

The `expected_balance` parameter is packed: `(check_mode << 248) | expected_value`.

| check_mode | Check Performed | When to Use |
|------------|----------------|-------------|
| 0 | Skip (no check) | `expected_balance=0` — off-chain verification only |
| 1 | `WETH.balanceOf(self) + self.balance >= value` | Default for V2/V3/V4+other paths (WETH warm from transfers) |
| 2 | `PM.balanceOf(self, weth_id) >= value` | V4V4V4 with `V4_MINT_COMPACT` profit capture (ERC6909 slot warm from MINT, saves ~3,500 gas vs cold WETH) |

The operator constructs `expected_balance = (1 << 248) | pre_tx_weth_eth` for mode 1, or `(2 << 248) | pre_tx_erc6909_weth` for mode 2. Value 2^248 ≈ 3.4×10^74 wei — far exceeds any real balance, so the top byte is always available.

**Key insight for V4V4V4**: `WETH.balanceOf(self)` is cold (~2,600 gas) because V4 operations use delta accounting (no physical WETH transfers). But after `V4_MINT_COMPACT` writes to the ERC6909 slot, reading `PM.balanceOf(self, weth_id)` is warm (~100 gas). Mode 2 exploits this, saving ~3,500 gas on pure V4 paths.

Gas benchmarks must run with profit checks active to measure real-world cost. Skipping the check (`expected_balance=0`) saves gas but represents a degenerate case — it should never be the benchmark baseline.

### WETH/Ether-Only Custody
The executor only ever takes custody of WETH and Ether. All non-WETH/ETH tokens flow between pools via direct custody. In V2/V3 callbacks, `amount0 + amount1` is always the WETH inflow — no `token0()`/`token1()` needed.

### Contract Structure
The cmd_executor is a byte-stream VM. `execute()` parses a compact command stream (max 512 bytes), unconditionally calls `_preprocess()` (which reads SET_ADDRESS/SKIP_PROFIT_CHECK/BRIBE commands until 0xFF BEGIN_EXECUTION), then runs the main execution loop. Each command is dispatched via `_execute_command_at(offset) -> uint256` which reads the 1-byte opcode and delegates to one of 21 `_cmd_*` internal functions. The function extraction pattern was critical for Venom's liveness analysis — it reduced Venom's highest memory address from 22,976 to 8,544 by allowing mutually exclusive command handlers to share memory regions.

### Venom Codegen
Venom uses monotonic `alloca` allocation (no `deallocate_memory`). Its `ConcretizeMemLocPass` reclaims memory via liveness analysis — two allocas with non-overlapping liveness can share memory. The function extraction pattern works because when `_execute_command_at` dispatches to `_cmd_v4_swap_compact`, Venom only marks that function's `mems_used` as live. When it dispatches to `_cmd_v3_swap_compact` instead, a different set of allocas is live. Since the two `invoke` sites are in different basic blocks, the allocator can assign overlapping offsets.

**Critical corollary**: Named variables HELP Venom's liveness analysis. When a value is assigned to a named local, Venom can precisely track its liveness and potentially share its memory slot. Inlining a named variable into a complex expression can HURT because Venom may keep the inlined sub-expression live longer. This especially applies to `uint256`/`address` variables in struct constructors — do NOT inline them (each attempt regressed +242 gas). The exception is `Bytes` intermediate variables used once in an extcall — these SHOULD be inlined (saves Venom memory allocation).

### Command Encoding
Commands use compact binary encoding with variable-width fields. Many fields have been shrunk from their natural sizes:
- **Amounts**: uint128 → uint96 (max 7.9e28, covers all practical token amounts). Saves 4 bytes per amount.
- **V4 fee**: uint24 → uint16 (500/3000/10000 all fit). Saves 1 byte per V4 swap.
- **V4 tick_spacing**: int24 → int16 (10/60/200 all fit). Saves 1 byte per V4 swap.
- **forward_len**: uint16 → uint8 (max 255 bytes sufficient). Saves 1 byte per V2/V3/V4 command.
- **ERC20_TRANSFER amount**: uint256 → uint96. Saves 20 bytes per transfer.
- **Skip_profit_check**: default True; 0x01 inverts to ENABLE_PROFIT_CHECK.
- **0xFE prefix**: removed; `_preprocess()` is now called unconditionally.
- **0xFF separator**: still present; cheaper than else:break/return in _preprocess.

### Sentinel Address System
Address index fields in commands use sentinel bytes to represent special addresses without explicitly storing them in the address table:
- `0xFC` = PM (PoolManager address)
- `0xFD` = SELF (executor address)
- `0xFE` = WETH address (as immutable)
- `0xFF` = NATIVE (empty(address) for native ETH) / no-hooks indicator

Range-check optimization: `if idx >= 0xFC` (1 comparison) replaces 4 individual equality checks for sentinels. Each sentinel address in a command field eliminates a SET_ADDRESS command (~476 gas TLOAD+TSTORE saved per address). This was the single highest-impact optimization category.

## Proven Optimization Patterns (Priority Order)

### 1. Eliminate Transient Storage via Sentinel Address Values (~67,786 gas saved)
The single biggest category of wins. Before: every address required a SET_ADDRESS command that did TSTORE(t_addresses, addr). After: sentinel bytes (0xFC-0xFF) in command fields allow direct loading from immutable constants, skipping the TLOAD+TSTORE entirely. Range-check `if idx >= 0xFC` replaces 4 equality checks. Each eliminated SET_ADDRESS saves ~476 gas. **Highest-impact pattern for any new address field.**

### 2. Replace Checked Arithmetic with unsafe_* for Provably-Safe Operations (~19,997 gas)
Every `offset + constant` in 512-byte streams can never overflow uint256. Each `unsafe_add` saves an ADD+JUMPI overflow check. Also applies to balance sums, negation, and fee arithmetic. The pattern is systematic: find any `+` where the LHS is bounded (e.g., offset < 512, fee < 10000) and replace with `unsafe_add`. Similarly `unsafe_sub` when the subtrahend is provably ≤ the minuend.

### 3. Remove Defensive Guards/Checks (~5,000+ gas cumulative)
- **Assertions with string data** bloat bytecode hugely. Replace with empty assert or remove if provably unreachable.
- **`forward_len > 0` guards**: `slice(data, offset, 0)` returns `b""` which is equivalent to the guarded path — just remove the guard.
- **`len(commands) > 0` guard**: unnecessary in production.
- **`delta != 0` checks**: _v4_settle_currency handles delta=0 gracefully.
- **Dead else branches**: remove code paths that are never triggered (e.g., V4_SETTLE_DELTA PM/SELF branch).
- **need_balance condition**: skip WETH balanceOf when check_mode=0 and no bribe.

### 4. Merge Adjacent Slice Reads into Larger Reads + Bitwise Extraction (~15,000+ gas)
Replace multiple `slice(data, offset, N)` calls with one larger `slice(data, offset, N+M)` then extract fields with `>>` and `&`. Each merged pair saves one bounds check (~50-80 gas). Proven from 2-byte through 26-byte merges.

**WARNING**: Non-power-of-2 large merges (>18 bytes) are fragile — bit position calculations are error-prone. Multiple crashes occurred from wrong shift amounts (e.g., `>> 8` instead of `>> 136` for a 26-byte merged value). Always verify bit positions against the total merged width.

### 5. Use Local Memory Variables Instead of Per-Iteration TSTORE (~29,224 gas)
In `_preprocess()`, replace TLOAD+TSTORE per loop iteration with MLOAD+MSTORE for the counter variable, then write the transient storage once at the end. Memory access (~3 gas) vs transient storage (~100 gas TLOAD + ~100 gas TSTORE) per iteration. Pattern: accumulate in local variable, flush to transient once.

### 6. Eliminate Intermediate Bytes Variables Used Once in Extcall (~3,368 gas)
When a `Bytes[MAX_COMMANDS_LENGTH]` variable is constructed from a slice and immediately passed to an extcall, remove the variable and pass the slice directly. Saves Venom memory allocation. **Only works for Bytes types** — inlining uint256/address variables HURTS Venom's liveness analysis (each attempt regressed +242 gas). Replacing inline sentinel resolution with _lookup_address calls also regresses (runs 215, 220: INVOKE overhead exceeds savings).

### 7. Shrink Command Encoding Field Sizes (~3,686 gas)
Systematically review every field in every command encoding and check if the type is wider than needed:
- uint128 → uint96 for amounts (4 bytes saved each)
- uint24 → uint16 for fee (1 byte saved each)
- int24 → int16 for tick_spacing (1 byte saved each)
- uint16 → uint8 for forward_len (1 byte saved each)
- uint256 → uint96 for ERC20_TRANSFER amount (20 bytes saved each)
- uint8 for V2 fee: 1 byte saved but too small to affect execution gas (calldata-only)

Savings are proportional to bytes-saved × call-frequency. prioritize fields used in hot commands (V4_SWAP_COMPACT, V4_TAKE_COMPACT, ERC20_TRANSFER).

### 8. Replace DynArray with Fixed Array + Count (~4,434 gas)
DynArray does a bounds check (`idx < len`) on every indexed read, costing ~3-6 gas each. `address[32]` + `t_addr_count` eliminates all bounds checks. Key: the count is now a separate variable (was DynArray length), must be maintained manually. Do NOT bypass DynArray length tracking with direct writes (causes 27 test failures).

### 9. Invert Default for Preprocessing Commands (~9,849 gas)
- SKIP_PROFIT_CHECK removed: profit check now controlled by `expected_balance` packing. 0x01 opcode is unused/dead. Saves 1 byte calldata + 1 loop iteration per path.
- 0xFE prefix removed: `_preprocess()` is called unconditionally, saving 1 byte + overhead check.

### 10. Test Encoding Optimizations (Non-Contract Changes, ~26,504 gas)
These require only updating the command stream encoding in test files, not the contract:
- **V4V4V3**: Take WBTC directly to V3c in callback, eliminating executor intermediate custody (4→3 transfers, −20,795 gas). Pattern: V4_TAKE inside callback increases destination's balance, satisfying IIA check.
- **V4_MINT for V4V4V4 profit capture**: uses warm ERC6909 slot from initialize() (−5,709 gas). **Only works in pure V4V4V4** — regresses ~130 gas inside any V2/V3 callback context.
- **V4_TAKE_DELTA for V4V3V4**: takes net deltas directly, eliminating sync+transfer+settle round-trip (−5,866 gas). Only when takes exhaust all WETH delta.
- **Remove unnecessary V4_SETTLE_DELTA when delta=0**: after takes exhaust all WETH delta, settle_delta is a no-op (just exttload). PM.unlock() verifies all deltas zero (−2,172 gas).
- **V4V3V3**: Replace V4_SYNC+ERC20_TRANSFER+V4_SETTLE with V4_SETTLE_DELTA for WETH (−137 gas).
- **V2V3V4**: Replace V4_SYNC+V4_SETTLE with V4_SETTLE_DELTA when delta=0 (−3,724 gas).

### 11. Dispatch Optimizations
- **Reorder dispatch by frequency**: put most common opcodes first (e.g., V2_SWAP_DIRECT before V2_SWAP_COMPACT, saves ~464 gas).
- **Two-level dispatch via SHR 4**: compute high nibble (`command >> 4`) to group opcodes, reducing average comparison count by ~1.5 per command (saves ~471 gas).

### 12. Inline _lookup_address in Specific Hot Handlers (~3,452 gas)
Inline sentinel resolution in `_cmd_v4_swap_compact`, `_cmd_v4_take_compact`, `_cmd_erc20_transfer`, `_cmd_v3_swap_compact`, `_cmd_v2_swap_direct`, `_cmd_v2_swap_compact`. Saves function call overhead + enables Venom constant propagation for WETH/NATIVE sentinels. **Do NOT inline in**: V4_BATCH (loop body, no savings), V4_TAKE_DELTA (Venom already optimizes), _auto_settle_touched (loop, no savings), V4_SYNC (INVOKE overhead exceeds savings).

### 13. Inline Hooks via Ternary in V4_SWAP_COMPACT (~800 gas)
Replace `hooks_addr: address = empty(address)` + if/else with inline ternary: `hooks=self.t_addresses[hooks_idx] if hooks_idx != V4_NATIVE_SENTINEL else empty(address)`. Eliminates MSTORE initialization overhead. Only simple 2-way ternary saves gas; nested multi-branch ternary does not.

## What Definitively Doesn't Work

Do NOT re-explore these — they have been conclusively tested and proven ineffective or harmful:

### Venom Already Optimizes These (No Gas Change)
- `%` for power-of-2 → Venom converts to `&` internally
- Double `convert(convert(x, uint160), uint256)` → Venom folds both
- `@view` vs `@pure` distinction for read-only internal functions
- `staticcall` extcodesize check → Vyper 0.5.0a2 already skips it
- `shift()` vs `>>` → `>>` generates identical or better bytecode
- `skip_contract_check=True` on staticcall → redundant, already skipped
- Caching `len(data)` results → Venom's `len()` is already a single MLOAD
- Precomputed constants at deploy time → Venom already folds constant expressions
- `@pure` vs `@internal` for pure functions → Venom inlines both

### Regressions (Net Gas Increase)
- **Caching cheap values** (len(), need_balance, data_len): extra memory slot costs more than saved reads (+175 gas each)
- **Inline named uint256/address variables** into struct constructors: hurts Venom liveness analysis (+242 gas each)
- **Callback handler deduplication**: merging V2/V3 callback paths adds invoke overhead (+754-825 gas)
- **Conditional TSTORE** (skip when value unchanged): branch overhead exceeds savings (+4,404 gas)
- **Conditional V2 fee packing** (skip mul+OR for non-auto-pay): branch overhead +57 gas
- **Remove % SHIFT from callback assertions**: convert() bounds check is more expensive than the modulo mask (+373 gas). The mask actually HELPS Venom skip the bounds check.
- **Remove 0xFF separator**: else:break/return is more expensive than 0xFF BEGIN_EXECUTION path (+1,029 gas). The 0xFF enables cheaper Venom codegen.
- **V4_TAKE_SELF (0x5A) command**: new handler bytecode + dispatch expansion exceeds 1-byte calldata savings (+637 gas)
- **Replace inline sentinel with _lookup_address calls** in V4_SYNC/V2_SWAP_DIRECT: INVOKE overhead exceeds savings (+276-475 gas)
- **Top-level `_execute_command_at` dispatch elif reorder**: +1,132 gas — Venom bytecode layout is layout-sensitive; even >12× frequency diffs regress. Current dispatch order is at a local optimum.

### OVERFITTING — Do Not Do These Even If They Lower The Benchmark

The 27-path benchmark uses a small, specific address subset. A deployed executor has path-varying address sets. **An optimization that only helps the 27 permutations but regresses a realistic nested-swap path is a benchmark cheat.**

- **User sentinels (`USER0`/`USER1`) — REMOVED (commit 8c75fa6).** Originally the `else: USER1_ADDR` catch-all silently mis-resolved unbound reserved bytes (`0xF2`–`0xFB`) to `USER1` — a latent bug — and the savings were partly a benchmark artifact. Only the 4 protocol-role sentinels (PM/SELF/WETH/NATIVE) remain; user tokens resolve via `t_addresses` SET_ADDRESS like any other address. The rule below still applies to the 4 surviving sentinels.
- **Per-handler sentinel `elif` reorder chasing benchmark currency frequencies**: never reorder a sentinel chain to match *which* address a benchmark path uses. Order only by *protocol-role* frequency (WETH-first for currency, SELF-first for recipient), globally across handlers. Production paths legitimately need `V4_SYNC(WETH)`, `V4_SETTLE_DELTA(WETH)`, `V4_TAKE_COMPACT(WETH)` (e.g. WETH paid by a nested swap inside a V4 unlock); demoting WETH to fund a USER0-first benchmark saving is latent regression. (This was tested + committed as −529 gas then reverted as overfit: commit f525dd5, revert 7e85aef.)

### No Gas Change (Dead Ends)
- **V4_BATCH 26-byte merge**: loop body restructuring doesn't change Venom's internal optimization
- **Inline _v4_settle_currency in _auto_settle_touched**: Venom optimizes loop function calls differently despite INVOKE in IR (+578 bytes, 0 gas)
- **Inline WETH/NATIVE settle in _auto_settle_touched**: same as above
- **Inline _lookup_address in V4_TAKE_DELTA**: Venom already optimizes via constant propagation
- **Delta != 0 check in _auto_settle_touched**: benchmark paths always have nonzero deltas in table entries
- **V4_SETTLE_ETH command (without init_erc6909)**: +222 bytes bytecode, 0 gas
- **SEND_ETH slice merge (1+16→17)**: not in benchmark paths
- **Mint vs Take for profit capture**: mint is +8,136 gas MORE expensive for cold paths (double HashMap storage access). Only saves in compounding scenarios with warm ERC6909 slot from prior mint.

### Vyper Limitations
- **While loops**: Vyper 0.5.0a2 does not support `while` — only `for range` loops (SyntaxException)
- **DynArray direct writes**: bypassing `.append()` breaks length tracking (27 test failures)

### Structural Insights from Experiments
- **V4_MINT inside V2/V3 callback context**: regresses ~130 gas per path, regardless of callback type. Only pure V4V4V4 benefits.
- **V4_BATCH loop**: Venom optimizes function calls inside loops differently from non-loop code — inlining doesn't save gas.
- **_auto_settle_touched loop**: Same pattern — inlining _v4_settle_currency saves nothing.
- **Byte ordering in merged reads**: Non-power-of-2 merges >18 bytes are crash-prone. Always verify: field at bit position X within a merged N-byte slice = `merged_value >> (N-X-field_width)*8 & mask`.
- **CSE-hoisting cheap bitwise extracts into named locals**: REGRESSES. V2/V3_SWAP_COMPACT compute `all & 255` (forward_len) twice (slice arg + return); Venom does NOT CSE it across the intervening extcall. Hoisting a `_fwd_len` named local regressed +90 gas (per-path +3..+9) + 16 bytes — mstore + 2×mload (~9 gas) > 2×AND (~6 gas). RULE: never hoist sub-5-gas duplicated extracts; only keccak256/exttload/staticcall (>~10 gas) are worth hoisting (and those are already inlined). Rule #3 ("named vars help Venom") is about KEEPING existing named vars (esp. struct constructors), NOT introducing new ones for cheap expressions.

## What's Been Tried (Historical)
Cumulative savings from this autoresearch session: ~237K gas (4,416,991 → 4,179,191)

### Contractor-level changes (largest wins)
- ✅ Dispatch reorder: −11,625 gas (V4-first, then balanced reorder)
- ✅ Eliminate SET_ADDRESS via sentinel addresses: −67,786 gas cumulative
- ✅ Remove PM TSTORE for callback registration: −1,029 gas
- ✅ Remove t_v4_currencies_touched tracking: −1,685 gas
- ✅ Remove need_balance/WETH balanceOf: −24,986 gas
- ✅ Remove assertions with string data: −2,731 gas + •900 bytes
- ✅ Remove defensive guards (forward_len>0, delta!=0, len>0): •4,998 gas
- ✅ Replace + with unsafe_add for offset arithmetic: −19,997 gas
- ✅ Replace DynArray with fixed array + count: −4,434 gas
- ✅ Local t_addr_count in _preprocess (MLOAD/MSTORE vs TLOAD/TSTORE): −26,665 gas, then removed TSTORE entirely: −2,559 gas
- ✅ Pack t_expected_callback + t_callback_fee: −123 gas
- ✅ Inline _lookup_address in hot handlers: −3,452 gas
- ✅ Merge slice reads (2→10, 2+8→10, 2+16→18, 10+16→26, 18+3→21, 18+1→19, 18+5→23): −15,000+ gas cumulative
- ✅ Inline hooks_addr via ternary: −800 gas
- ✅ Inline _read_pm_delta_slot / _read_pm_delta: −635 gas
- ✅ Inline WETH/NATIVE settle in V4_SETTLE_DELTA: −682 gas
- ✅ Inline V2 callback handler: −90 gas
- ✅ Remove 0xFE prefix (unconditional _preprocess): −2,858 gas
- ✅ Invert SKIP_PROFIT_CHECK default: −6,991 gas
- ✅ Remove dead PM/SELF branch from V4_SETTLE_DELTA: -122 bytes bytecode
- ✅ Shrink encoding fields (amounts, fee, tick_spacing, forward_len, ERC20 amount): −3,686 gas
- ✅ Reduce forward_len to uint8: −475 gas
- ✅ V2 dispatch reorder (DIRECT before COMPACT): −464 gas
- ✅ Two-level dispatch via SHR 4: −471 gas
- ✅ Simplify V2 callback auto-pay check: −81 gas

### Test-encoding-only changes (no contract modifications)
- ✅ V4V4V3: Take WBTC directly to V3c in callback: −20,795 gas
- ✅ V4_MINT for V4V4V4 profit capture: −5,709 gas
- ✅ V4_TAKE_DELTA for V4V3V4: −5,866 gas
- ✅ Remove unnecessary V4_SETTLE_DELTA when delta=0: −2,172 gas
- ✅ V4V3V3: V4_SETTLE_DELTA replaces SYNC+TRANSFER+SETTLE: −137 gas
- ✅ V2V3V4: V4_SETTLE_DELTA replaces SYNC+SETTLE when delta=0: −3,724 gas
