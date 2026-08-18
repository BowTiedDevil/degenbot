# Cmd Executor — User Guide

> **Comprehensive guide for constructing, encoding, and executing optimal arbitrage
> paths across Uniswap V2, V3, and V4 using the command-stream executor.**

This document teaches you everything needed to use the executor in production:
how it works, how to encode command streams, how to select the correct routing
pattern for every pool-permutation, and how to verify correctness.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Core Concepts](#2-core-concepts)
3. [Deployment & Initialization](#3-deployment--initialization)
4. [The `execute()` Entry Point](#4-the-execute-entry-point)
5. [Command Stream Format](#5-command-stream-format)
6. [Address Resolution & Sentinels](#6-address-resolution--sentinels)
7. [Command Reference](#7-command-reference)
8. [Callback Mechanics](#8-callback-mechanics)
9. [Pool Verification Rules](#9-pool-verification-rules)
10. [Direct Custody & Reverse-Order Execution](#10-direct-custody--reverse-order-execution)
11. [2-Hop Path Encyclopedia](#11-2-hop-path-encyclopedia)
12. [3-Hop Path Encyclopedia](#12-3-hop-path-encyclopedia)
13. [ERC6909 (Mint/Burn) Strategies](#13-erc6909-mintburn-strategies)
14. [Profit Checking](#14-profit-checking)
15. [Bribe System](#15-bribe-system)
16. [Putting It All Together: Encoding a Complete Transaction](#16-putting-it-all-together-encoding-a-complete-transaction)
17. [Troubleshooting](#17-troubleshooting)
18. [Appendix A: Opcode Quick Reference](#appendix-a-opcode-quick-reference)
19. [Appendix B: Sentinel Index Table](#appendix-b-sentinel-index-table)
20. [Appendix C: Command Size Table](#appendix-c-command-size-table)
21. [Appendix D: Transfer Count Summary](#appendix-d-transfer-count-summary)

---

## 1. Overview

The **Cmd Executor** is an on-chain arbitrage execution contract that processes
a compact binary command stream to execute multi-hop swap paths across Uniswap
V2, V3, and V4 — all in a single atomic transaction.

### Key Properties

| Property | Description |
|----------|-------------|
| **No prefunding** | All working capital is borrowed atomically via V2/V3 flash swaps and V4 `take()`. Deploy with zero balance. |
| **Compact encoding** | 1-byte opcodes + tightly-packed fields. Address indices instead of 20-byte addresses. Amounts as `uint96` (12 bytes) instead of `uint256` (32 bytes). |
| **Dynamic amounts** | V4 deltas read from PoolManager's own transient storage via `exttload()`. V2 output computed on-chain from excess balance. V3 auto-pay from callback parameters. |
| **Cross-protocol** | V2, V3, and V4 swaps compose freely in the same command stream. Callbacks continue execution via forwarded command data. |
| **ERC6909 support** | Profit can be captured as ERC6909 internal balance (V4_MINT) instead of physical WETH transfers, saving gas on pure-V4 paths. |

### When to Use This Executor

- You have identified a profitable arbitrage opportunity across 2 or 3 Uniswap pools.
- The pools may be any combination of V2, V3, and V4.
- You need to borrow capital atomically (no prefunding).
- You want minimum gas cost (optimal routing with direct custody).

---

## 2. Core Concepts

### 2.1 Command Stream

The executor processes a **byte stream** of commands. Each command is:

```
[opcode: 1 byte][parameters: variable bytes]
```

Commands execute **in order**. There is no branching or looping (except the
V4_BATCH inner loop). The stream is consumed until it is exhausted.

### 2.2 Execution Contexts

Commands execute in one of four contexts:

| Context | How Entered | Key Constraint |
|---------|------------|-----------------|
| **Top-level** | `execute()` is called directly | Can issue any command |
| **V2 callback** | `uniswapV2Call`/`hook`/`pancakeCall` fires | Inside a V2 swap; can issue any command |
| **V3 callback** | `uniswapV3SwapCallback`/`pancakeV3SwapCallback` fires | Inside a V3 swap; can issue any command |
| **V4 unlock** | `unlockCallback` fires (inside `PM.unlock()`) | Can issue V4 swap + settlement commands |

The **callback continuation** mechanism means the command stream can be split:
the top-level stream runs until a V2/V3/V4 swap triggers a callback, and the
callback data contains the **next** segment of commands. This is how multi-hop
paths chain through pool callbacks.

### 2.3 Address Table

Rather than repeating 20-byte addresses for every command, the executor uses an
**address lookup table** populated during preprocessing:

```
SET_ADDRESS [addr1]  →  t_addresses[0] = addr1
SET_ADDRESS [addr2]  →  t_addresses[1] = addr2
...
```

Subsequent commands reference addresses by **1-byte index** (0–251). Special
indices 0xFC–0xFF are **sentinels** that resolve to common addresses without
any table lookup (see §6).

### 2.4 Forward Data (Callback Continuation)

V2 and V3 swap commands embed **forward_data** — arbitrary bytes passed through
the pool's callback mechanism. The executor's callback handler treats this data
as a **new command stream** and processes it:

```
Top-level: V2_SWAP_COMPACT(..., forward_data=<inner commands>)
  → V2 calls uniswapV2Call on executor
  → executor processes <inner commands> from the callback data
```

This is the continuation mechanism: the top-level stream starts the outermost
swap, and forward_data contains the commands that should run inside the callback.

### 2.5 V4 Unlock as a Context

V4 operations must be inside a `PM.unlock()` call. The `V4_UNLOCK` command
enters this context, and its forward_data becomes the command stream for
`unlockCallback`:

```
V4_UNLOCK [forward_len][forward_data]
  → PM.unlock(forward_data) is called
  → PM calls executor.unlockCallback(forward_data)
  → executor processes forward_data as commands
```

Unlike V2/V3, V4 doesn't nest callbacks — all V4 operations for a single
transaction happen inside one `unlock()` call.

---

## 3. Deployment & Initialization

### 3.1 Constructor Parameters

```python
__init__(weth: address, pool_manager: address)
```

| Parameter | Purpose | Typical Value |
|-----------|---------|---------------|
| `weth` | WETH9 contract address | Network-specific (e.g., `0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2` on mainnet) |
| `pool_manager` | Uniswap V4 PoolManager address | Network-specific |

The deployer (`msg.sender`) becomes the **owner** — the only address that can
call `execute()`.

> **No path-specific tokens are baked in.** Earlier versions took `user0`/`user1`
> deploy-time sentinels (0xF0/0xF1) for two hot tokens (e.g. USDC/WBTC). These
> were removed (commit `8c75fa6`): their `else: USER1_ADDR` catch-all silently
> mis-resolved unbound reserved bytes, and the savings were partly a benchmark
> artifact. All path-specific tokens now resolve via `t_addresses` `SET_ADDRESS`,
> identically to every other address. Only the 4 protocol-role sentinels (PM/
> SELF/WETH/NATIVE) remain (see §6).

### 3.2 Warmup (ERC6909 Slot Initialization)

After deployment, call `initialize()` with 2 wei of ETH. This:

1. Deposits 1 wei WETH into the PoolManager
2. Mints 1 wei as ERC6909 to the executor (warming the storage slot)

**Why warmup matters**: The first V4_MINT on a cold `erc6909_balance_of` slot
costs ~22,100 gas (zero→non-zero SSTORE). After warmup, the slot is non-zero,
and subsequent mints cost only ~2,900 gas (warm dirty SSTORE). This saves
~17,000 gas on the first real arbitrage after warmup.

```python
# Call once after deployment (send 2 wei ETH)
executor.initialize(value=2)
```

### 3.3 ERC6909 Warmup Details

The `initialize()` function sends 1 wei WETH to the PoolManager, settles it
as a +1 WETH delta, then mints it as ERC6909 to the executor. After this:

- `pm.balanceOf(executor, weth_id) == 1` (the warmup wei)
- The storage slot `erc6909_balance_of[executor][weth_id]` is non-zero
- All future V4_MINT operations benefit from the warm SSTORE

This 1 wei of ERC6909 remains permanently and does not affect profit
calculations — it's purely a gas optimization.

---

## 4. The `execute()` Entry Point

```vyper
@external
@payable
def execute(commands: Bytes[288], config: uint256 = 0) -> uint256
```

### Parameters

| Parameter | Type | Description |
|-----------|------|-------------|
| `commands` | `Bytes[288]` | The binary command stream (SET_ADDRESS preprocessing + execution) |
| `config` | `uint256` | Packed check_mode + bribe_bips + bribe_recipient_idx + expected_value (see §14) |

### Return Value

Returns the executor's profit in wei (combined WETH + ETH balance increase, or
ERC6909 balance increase for mode 2).

### Access Control

**Owner-only.** Only the deployer address can call `execute()`.

### Execution Flow

```
1. _preprocess()        — parse SET_ADDRESS commands until 0xFF or non-preprocessing opcode
2. Command loop        — iterate _execute_command_at() until stream exhausted
3. Profit check         — verify WETH+ETH or ERC6909 balance ≥ expected (if check_mode ≠ 0)
4. Bribe                — if bribe_bips > 0, send profit × bips / 10000 ETH
5. Return profit
```

### Fast Path vs. Slow Path

When `check_mode == 0` AND `bribe_bips == 0`, the executor skips all balance
reads and bribe logic entirely (the "fast path"). This saves ~300 gas on paths
where the operator verifies profitability off-chain.

---

## 5. Command Stream Format

### 5.1 Stream Structure

```
┌─────────────────────────────────────────────────────────────┐
│ Preprocessing Section (optional)                            │
│   SET_ADDRESS commands, BRIBE commands                      │
│   (terminated by 0xFF or first non-preprocessing opcode)    │
├─────────────────────────────────────────────────────────────┤
│ Execution Section                                           │
│   Swap, transfer, settlement commands in execution order    │
│   (processed until stream exhausted)                        │
└─────────────────────────────────────────────────────────────┘
```

If the first byte is not a preprocessing opcode (0x00–0x03), the entire
stream is treated as execution — no preprocessing runs.

### 5.2 Preprocessing Commands

These run **before** execution to set up address table and bribe parameters.

| Opcode | Name | Format | Size |
|--------|------|--------|------|
| `0x00` | SET_ADDRESS | `[0x00][address:20]` | 21 bytes |
| `0xFF` | BEGIN_EXECUTION | (separator) | 1 byte |

Opcodes 0x01–0x03 are reserved (were SKIP_PROFIT_CHECK, BRIBE_COINBASE, BRIBE_ADDRESS; now packed into the `config` ABI parameter).

SET_ADDRESS commands populate `t_addresses[]` in order: the first SET_ADDRESS
puts the address at index 0, the second at index 1, etc.

### 5.3 Execution Commands

Execution commands are processed in strict order. Each returns the byte offset
of the next command. The loop terminates when the offset reaches `len(data)`.

Commands are grouped by protocol at 0x10 boundaries for dispatch efficiency.

---

## 6. Address Resolution & Sentinels

### 6.1 How Addresses Are Resolved

When a command references an "address index" (1-byte field), the executor
resolves it as follows:

```
if idx >= 0xFC (SENTINEL_THRESHOLD):
    resolve as sentinel (see table below)
else:
    lookup t_addresses[idx]
```

Any byte `>= 0xFC` that isn't one of the 4 sentinels below reverts with
`InvalidCommand` (fail-closed — no silent catch-all).

### 6.2 Sentinel Address Table

| Index | Name | Resolves To | When to Use |
|-------|------|-------------|-------------|
| `0xFC` | `V4_PM_SENTINEL` | PoolManager address (immutable) | V4 settlement commands, WETH transfers to PM |
| `0xFD` | `V4_SELF_SENTINEL` | `self` (executor address) | Self-referencing: V4_TAKE profit, V4_MINT profit |
| `0xFE` | `V4_WETH_SENTINEL` | WETH address (immutable) | V4 currency fields, ERC20 WETH transfers |
| `0xFF` | `V4_NATIVE_SENTINEL` | `address(0)` / NATIVE_ADDRESS | V4 native ETH currency, hooks="no hooks" flag |

Only these **4 protocol-role sentinels** exist. There are no path-specific token
sentinels — USDC, WBTC, DAI, etc. are always addressed via `t_addresses` +
`SET_ADDRESS`, exactly like any other address. (Earlier `USER0`/`USER1`
sentinels at 0xF0/0xF1 were removed; see §3.1.)

**Gas savings**: Each sentinel use saves 21 bytes of calldata (no SET_ADDRESS
needed) and ~476 gas per transaction by eliminating TLOAD.

### 6.3 Precomputed Delta Slots

For WETH and NATIVE only, the executor precomputes the V4 delta storage slot
(via `keccak256(abi.encodePacked(self, currency))`) at deploy time. This means:

- `V4_SETTLE_DELTA(0xFE)` (WETH) — reads `WETH_DELTA_SLOT` directly, no keccak256
- `V4_SETTLE_DELTA(0xFF)` (NATIVE) — reads `NATIVE_DELTA_SLOT` directly
- `V4_SETTLE_DELTA(0x05)` (table index, e.g. USDC) — computes keccak256 at runtime (~658 gas extra)

Use a sentinel only for the 4 protocol currencies (WETH/NATIVE) to skip the
keccak256 in `V4_SETTLE_DELTA`/`V4_TAKE_DELTA`. All other currencies (including
common tokens like USDC) pay the one-time `keccak256` cost — acceptable given
that per-tx `SET_ADDRESS` already touches the address table.

---

## 7. Command Reference

### 7.1 ERC20 / ETH Commands

#### ERC20_TRANSFER: `0x10`

```
[0x10][token_idx:1][recipient_idx:1][amount:12]  = 15 bytes
```

Transfer `amount` of `token` to `recipient`. Amount is `uint96` (max
7.9×10²⁸ — covers all practical token amounts).

**Use cases**: Explicit WETH transfers (profit to PM for settlement, excess
balance to V2 pairs), explicit USDC/WBTC transfers when direct custody isn't
possible.

#### ERC20_XFER_BALANCE: `0x11`

```
[0x11][token_idx:1][recipient_idx:1]  = 3 bytes
```

Transfer the executor's **entire balance** of `token` to `recipient`. Reads
`token.balanceOf(self)` (warm after any prior token operation).

**Use cases**: Sending all remaining WETH to PM for settlement, sweeping
residual balances.

#### WETH_DEPOSIT: `0x12` / WETH_DEPOSIT_ALL: `0x14`

```
[0x12][amount:32]  = 33 bytes    (explicit amount)
[0x14]             = 1 byte      (all available ETH)
```

Wrap native ETH to WETH. `WETH_DEPOSIT_ALL` wraps `self.balance`.

**Use case**: When the executor holds ETH (from V4_TAKE of native currency)
and needs WETH for V2/V3 payments or V4 settlement.

#### WETH_WITHDRAW: `0x13` / WETH_WITHDRAW_ALL: `0x15`

```
[0x13][amount:32]  = 33 bytes    (explicit amount)
[0x15]             = 1 byte      (all available WETH)
```

Unwrap WETH to native ETH. `WETH_WITHDRAW_ALL` unwraps `WETH.balanceOf(self)`.

**Use case**: When the bribe system needs ETH but the executor holds WETH.

#### SEND_ETH: `0x16` / SEND_ETH_ALL: `0x17`

```
[0x16][recipient_idx:1][amount:12]  = 14 bytes
[0x17][recipient_idx:1]             = 2 bytes
```

Send native ETH to a recipient. `SEND_ETH_ALL` sends `self.balance`.

**Use case**: After V4_BURN converts ERC6909 to +delta, then V4_TAKE sends
ETH to the executor, SEND_ETH can forward it to an external address.

### 7.2 V2 Swap Commands

#### V2_SWAP_COMPACT: `0x20`

```
[0x20][pool_idx:1][zfo:1][amount_out:12][recipient_idx:1][fee:2][fwd_len:1][fwd_data:N]
= 19 + N bytes
```

| Field | Size | Description |
|-------|------|-------------|
| `pool_idx` | 1 | Address table index of the V2 pair |
| `zfo` | 1 | Zero-for-one flag (1 = sell token0, 0 = sell token1) |
| `amount_out` | 12 | `uint96` desired output amount |
| `recipient_idx` | 1 | Address table index of output recipient |
| `fee` | 2 | V2 fee as fraction of 10000 (30 = 0.3% Uniswap, 25 = 0.25% PancakeSwap) |
| `fwd_len` | 1 | Length of forward data (1 = auto-pay via V2 callback, 0 = no data) |
| `fwd_data` | N | Callback continuation data |

**When to use**: Standard V2 swap that triggers a callback (flash borrow).

**Auto-pay**: When `fwd_len == 1`, the callback data is a single byte (conventionally
`0xFE`) and the V2 callback handler computes the owed amount from `getReserves() +
fee` and auto-transfers to the pair. No explicit payment encoding needed. When
`fwd_len > 1`, the callback data is a command stream for callback continuation.

#### V2_SWAP_CALC: `0x21`

```
[0x21][pool_idx:1][zfo:1][recipient_idx:1][fee:2]  = 6 bytes
```

Computes `amount_out` **on-chain** from excess balance:

```
amount_in = IERC20(input_token).balanceOf(pair) - reserve_in
amount_out = getAmountOut(amount_in, reserve_in, reserve_out, fee)
```

Calls `pair.swap(data=b"")` — **no callback**. The V2 pair already holds the
input tokens (excess balance from a prior V4_TAKE or ERC20_TRANSFER to the pair).

**When to use**: Direct-custody paths where V4_TAKE or ERC20_TRANSFER already
deposited input tokens at the V2 pair. Saves gas by skipping callback overhead.

**Important**: The V2 pair must have **excess balance** (tokens deposited but not
yet in reserves) before this command runs. If the pair's `balanceOf` equals its
`getReserves()`, the input amount will be 0 and the swap will fail.

#### V2_SWAP_DIRECT: `0x22`

```
[0x22][pool_idx:1][zfo:1][amount_out:12][recipient_idx:1]  = 16 bytes
```

V2 swap with explicit `amount_out` and no callback (`data=b""`). Like
V2_SWAP_CALC but the output amount is pre-computed off-chain.

**When to use**: When you know the exact swap amounts (off-chain simulation)
and the pair already holds the input tokens via excess balance. Saves ~4
staticcalls (getReserves, token0, token1, balanceOf) vs V2_SWAP_CALC, at the
cost of 10 extra calldata bytes.

**No fee field**: Unlike V2_SWAP_COMPACT, V2_SWAP_DIRECT omits the fee field.
The V2 pair applies its stored swap fee (set at pair creation) when checking
the K-invariant. Since the pair already holds the input tokens (excess balance),
the swap calls `data=b""` which means the K-invariant automatically uses the
pair's fee. Your off-chain `amount_out` must account for the pair's fee
correctly — an incorrect amount will cause the K-invariant to fail.

### 7.3 V3 Swap Commands

#### V3_SWAP_COMPACT: `0x30`

```
[0x30][pool_idx:1][zfo:1][amount_specified:12][recipient_idx:1][fwd_len:1][fwd_data:N]
= 17 + N bytes
```

| Field | Size | Description |
|-------|------|-------------|
| `pool_idx` | 1 | Address table index of the V3 pool |
| `zfo` | 1 | Zero-for-one flag |
| `amount_specified` | 12 | `uint96` exact-input amount (positive) |
| `recipient_idx` | 1 | Address table index of output recipient |
| `fwd_len` | 1 | Length of forward data (0 = auto-pay) |
| `fwd_data` | N | Callback continuation data |

**Amount convention**: V3 uses **exact-input** (positive `amount_specified`).
The contract negates it internally: `amount_specified = -int256(amount)`.

**Sqrt price limit**: Automatically set to `MIN_SQRT_PRICE_PLUS1` (for zfo=1)
or `MAX_SQRT_PRICE_MINUS1` (for zfo=0). These are the widest possible limits
that avoid the exact boundary values. You don't need to encode these.

**Auto-pay**: When `fwd_len == 0` (empty forward_data), the V3 callback
handler reads the owed amount from `amount0_delta`/`amount1_delta` and
auto-transfers to the pool. Zero calldata for the payment command.

**When to use**: Standard V3 swap with known input amount.

#### V3_SWAP_DELTA: `0x31`

```
[0x31][pool_idx:1][zfo:1][recipient_idx:1]  = 4 bytes
```

V3 swap where the **input amount is read from the PM's exttload**. The pool's
input token must have a positive delta in the PoolManager (from a prior V4 swap
or V4_TAKE that didn't settle).

**When to use**: When V4 has produced a positive delta for the V3 pool's input
token, and you want the V3 swap to consume exactly that amount. Saves 12 bytes
of calldata (no amount field).

**Caveat**: Reads the PM exttload delta for the V3 pool's input token — this
only has a meaningful value if a V4 swap has already created a positive delta
for that currency in the same unlock context. Unlike V3_SWAP_COMPACT, there is
no explicit amount encoding, so the swap amount is whatever the PM delta says.
Additionally, the V3 auto-pay (forward_data=b"") requires the executor to
physically hold the input ERC-20 tokens at callback time. In V4→V3 paths
where the tokens come from the PM, the standard approach is V4_TAKE (which
transfers physical tokens to the executor) + V3_SWAP_COMPACT with auto-pay.
V3_SWAP_DELTA skips the V4_TAKE step, but then the executor may not have the
physical tokens for auto-pay — use only when the executor already holds the
tokens or in paths where V3's callback payment is automatically satisfied.

### 7.4 V4 Swap Commands

#### V4_SWAP_COMPACT: `0x40`

```
[0x40][c0_idx:1][c1_idx:1][fee:2][tick_spacing:2][hooks_idx:1][zfo:1][amount:12]
= 21 bytes
```

| Field | Size | Description |
|-------|------|-------------|
| `c0_idx` | 1 | Address index of currency0 |
| `c1_idx` | 1 | Address index of currency1 |
| `fee` | 2 | Pool fee (e.g., 3000 = 0.3%, 500 = 0.05%, 10000 = 1%) |
| `tick_spacing` | 2 | Pool tick spacing (e.g., 10, 60, 200) — `int16` but encoded as unsigned |
| `hooks_idx` | 1 | Address index of hooks contract (0xFF = no hooks) |
| `zfo` | 1 | Zero-for-one flag |
| `amount` | 12 | `uint96` exact-input amount |

**Pool key identification**: The V4 pool is identified by the tuple
`(currency0, currency1, fee, tick_spacing, hooks)`. All five fields must match
an initialized pool in the PoolManager.

**No callback data**: V4 swaps don't have forward_data — they return the
`BalanceDelta` directly from `PM.swap()`. Continuation is via sequential
commands in the same unlock context.

**Amount sign**: As with V3, the amount is exact-input (positive). The contract
negates it: `amount_specified = -int256(amount)`.

**When to use**: Standard V4 swap with known input amount. The most common
V4 swap command.

#### V4_SWAP_DYNAMIC: `0x41`

```
[0x41][c0_idx:1][c1_idx:1][fee:2][tick_spacing:2][hooks_idx:1][zfo:1]
= 9 bytes
```

V4 swap where the **input amount is read from the PM's exttload delta** of the
input currency. No amount field — saves 12 bytes of calldata.

**When to use**: When a prior V4 swap or V4_TAKE has created a positive delta
for the input currency, and you want the next swap to consume exactly that
amount. Common in V4_BATCH sequences.

#### V4_BATCH: `0x42`

```
[0x42][num_swaps:1][entry_1:20]...[entry_N:20]
= 2 + 20×N bytes
```

Execute multiple V4 swaps in a tight loop with automatic amount chaining.
Maximum `num_swaps`: 8 (contract limit: `MAX_V4_BATCH_SWAPS`).
Each entry:

```
[c0_idx:1][c1_idx:1][fee:2][tick_spacing:2][hooks_idx:1][zfo:1][amount:12]
= 20 bytes per swap
```

If `amount == 0`, the swap reads its input amount from either:
- The PM exttload delta (first dynamic swap in the batch)
- The previous swap's `BalanceDelta` output (subsequent dynamic swaps)

After all swaps, auto-settles native ETH and WETH deltas.

**When to use**: Pure V4 paths (V4-V4-V4, V4-V4 paths) where all intermediate
deltas cancel. Extremely calldata-efficient.

### 7.5 V4 Settlement Commands

All V4 settlement commands must be called **inside the V4 unlock context**
(after `V4_UNLOCK`), except `V4_SYNC` which can be called anytime.

#### V4_UNLOCK: `0x50`

```
[0x50][fwd_len:1][fwd_data:N]
= 2 + N bytes
```

Enter the PoolManager's `unlock()` context. The `fwd_data` is passed to
`unlockCallback`, where the executor processes it as commands.

**No nesting**: You cannot call `V4_UNLOCK` inside another `V4_UNLOCK` context.
The PM enforces `AlreadyUnlocked`. All V4 operations in a single transaction
must be within one unlock block.

#### V4_TAKE: `0x51`

```
[0x51][currency_idx:1][recipient_idx:1][amount:32]
= 35 bytes
```

Take `amount` of `currency` from the PoolManager and send to `recipient`.
Creates a **negative delta** for `currency` (reduces the PM's balance by `amount`).

**When to use**: When you need a specific (non-compact) take amount. Rarely
used — `V4_TAKE_COMPACT` or `V4_TAKE_DELTA` are preferred.

#### V4_TAKE_COMPACT: `0x52`

```
[0x52][currency_idx:1][recipient_idx:1][amount:12]
= 15 bytes
```

Same as `V4_TAKE` but with `uint96` amount. Saves 20 bytes of calldata.

**When to use**: Taking a known amount from PM. The most common take command.

#### V4_TAKE_DELTA: `0x53`

```
[0x53][currency_idx:1][recipient_idx:1]
= 3 bytes
```

Takes the **entire positive PM delta** for `currency`. No amount encoding —
reads the delta from PM's exttload.

**When to use**: When you want to take whatever the PM owes you for a currency.
Common after a V4 swap that produces a known positive delta.

#### V4_SYNC: `0x54`

```
[0x54][currency_idx:1]
= 2 bytes
```

Snapshot the PoolManager's balance of `currency`. Must be called **before**
any external deposit to the PM (ERC20 transfers to PM, V2/V3 swaps sending
output to PM), and **before** `V4_SETTLE`.

**Critical ordering**: `sync()` → tokens arrive at PM → `settle()`. If `sync()`
is called after the deposit, `settle()` won't detect the balance increase.

**Can be called outside unlock**: This is important for V3→V4 paths where the
V3 swap (depositing to PM) happens before V4_UNLOCK. You call V4_SYNC in the
top-level stream before the V3 swap, and V4_SETTLE inside the unlock.

#### V4_SETTLE: `0x55`

```
[0x55]  = 1 byte
```

Credit the PM's balance increase (since the last `sync()`) as a positive delta
for the settled currency.

**Must be called after sync + token deposit**. The PM records the balance
delta since `sync()` and credits it to the caller's account.

#### V4_SETTLE_DELTA: `0x56`

```
[0x56][currency_idx:1]
= 2 bytes
```

Auto-settle a single currency. Reads the PM's exttload delta:
- **Positive delta**: calls `PM.take()` — PM sends tokens to executor
- **Negative delta**: calls `PM.sync()` + `ERC20.transfer(PM, owed)` + `PM.settle()` — executor sends tokens to PM

For WETH debts, if the executor lacks sufficient WETH, it wraps ETH first.

**When to use**: Settling a single currency after V4 operations. Extremely
compact (2 bytes) and handles both take and settle cases automatically.

#### V4_SETTLE_ALL: `0x57`

```
[0x57]  = 1 byte
```

Auto-settle **all** nonzero deltas. Iterates native ETH, WETH, and all
addresses in the address table.

**When to use**: End of a V4 operation when you want to sweep all remaining
deltas. Less gas-efficient than targeted `V4_SETTLE_DELTA` for specific
currencies, but convenient.

#### V4_MINT_COMPACT: `0x58`

```
[0x58][currency_idx:1][recipient_idx:1][amount:12]
= 15 bytes
```

Convert a **positive PM delta** into ERC6909 balance for `recipient`. No
physical token transfer — the asset stays inside the PoolManager as an
accounting entry.

**When to use**: Capturing profit as ERC6909 instead of physical WETH.
Saves 1 ERC20 transfer (~50,000 gas on production). Only useful when:
1. The delta is positive (PM owes the executor), AND
2. The recipient is the executor (profit capture), AND
3. The executor will compound or burn the ERC6909 later.

See §13 for the full ERC6909 strategy.

#### V4_BURN_COMPACT: `0x59`

```
[0x59][currency_idx:1][amount:12]
= 14 bytes
```

Convert ERC6909 balance into a **payable PM delta**. The executor always burns
its own ERC6909 tokens. This adds a positive delta, which can offset a
negative delta (debt to the PM).

**When to use**: Paying a V4 debt from accumulated ERC6909 instead of a
physical token transfer. Requires the executor to have ERC6909 from a prior
V4_MINT.

---

## 8. Callback Mechanics

### 8.1 V2 Callbacks

The executor implements three V2 callback variants:
- `uniswapV2Call` — Uniswap V2 and SushiSwap V2
- `hook` — Some V2 forks
- `pancakeCall` — PancakeSwap V2

All three work identically:

1. **Verify caller**: `assert msg.sender == t_callback_packed % (2^160), InvalidCallback(caller=msg.sender)` — reverts with `InvalidCallback` if the caller is not the registered V2 pair
2. **Check forward data length**:
   - `len(data) == 1` → auto-pay (compute owed amount from reserves + fee, transfer to pair)
   - `len(data) > 1` → process data as command stream (callback continuation)

**Auto-pay vs. forward_data**: For simple single-hop V2 swaps, use auto-pay
(`fwd_len=1`, `fwd_data=b"\xfe"`). The auto-pay computes the owed amount from
`getReserves()` plus the stored fee, then transfers the owed token from the
executor to the pair. The executor **must hold** the owed token. For multi-hop
paths, use forward_data to continue execution inside the callback.

### 8.2 V3 Callbacks

The executor implements two V3 callback variants:
- `uniswapV3SwapCallback` — Uniswap V3 and SushiSwap V3
- `pancakeV3SwapCallback` — PancakeSwap V3

Both work identically:

1. **Verify caller**: `assert msg.sender == t_callback_packed % (2^160), InvalidCallback(caller=msg.sender)`
2. **Check forward data length**:
   - `len(data) == 0` → auto-pay (read `amount0_delta`/`amount1_delta` from callback params, transfer owed token to pool)
   - `len(data) > 0` → process data as command stream

**Important**: V3 auto-pay is the **default mode**. When you call `V3_SWAP_COMPACT`
with `fwd_len=0` (empty forward_data), the callback automatically pays the pool
the owed token (the one with a positive delta). The executor **must hold** the
required ERC-20 tokens at callback time — the auto-pay transfers them from the
executor's balance to the pool. For multi-hop paths, the required tokens must
arrive at the executor before the V3 callback fires (e.g., from a V2 swap
output or a V4_TAKE that sent tokens to the executor).

### 8.3 V4 Unlock Callback

The `unlockCallback` is called by the PoolManager when `unlock()` is invoked:

1. **Verify caller**: `assert msg.sender == POOL_MANAGER_ADDR, InvalidCallback(caller=msg.sender)`
2. **Process data as command stream**: all V4 swap and settlement commands run here

No auto-pay — V4 uses delta accounting. All balances are settled explicitly
via V4_TAKE, V4_SETTLE, V4_SETTLE_DELTA, or V4_MINT.

### 8.4 Callback Packed Registration

Before a V2 or V3 swap, the executor writes `t_callback_packed` — a single
transient uint256 that packs:

```
bits 0-159:  callback address (the pool)
bits 160-175: V2 fee (uint16, 0 for V3)
```

This serves two purposes:
1. **Callback authentication**: The callback handler verifies `msg.sender` matches
2. **V2 fee access**: The auto-pay handler reads the fee to compute the owed amount

For V2: `packed = uint256(pool) | (fee << 160)`
For V3: `packed = uint256(pool)` (fee = 0)

---

## 9. Pool Verification Rules

Understanding how each pool verifies payment is **essential** for constructing
valid command streams. These rules determine the order of operations.

### 9.1 V2: K-invariant (Final Balance Check)

V2 checks:
```
balance0_adjusted × balance1_adjusted ≥ reserve0 × reserve1 × 10000²
```

- **When**: After the callback returns
- **What it means**: V2 only cares that the pair's **total** token balances satisfy
  the constant-product formula. It does NOT care WHEN or HOW tokens arrived.
- **Implication**: Tokens deposited BEFORE `swap()` (excess balance) work just
  as well as tokens sent DURING the callback. This is why V2_SWAP_DIRECT and
  V2_SWAP_CALC work — the pair already has the tokens.

### 9.2 V3: IIA Balance-Delta Check (Incremental)

V3 checks:
```
balance_before + amount_owed ≤ balance_after
```

- **When**: `balance_before` is read after the optimistic output transfer but
  before the callback. `balance_after` is read after the callback returns.
- **What it means**: Tokens must arrive **during the callback window** (between
  the two balance snapshots). Tokens deposited before `swap()` are already
  in `balance_before` and don't help.
- **Implication**: V3 ALWAYS requires a physical transfer during its callback.
  This is the single most important constraint for V3 routing.

### 9.3 V4: Delta Accounting (Transient Storage)

V4 checks:
```
t_deltas[currency] == 0 for all currencies at end of unlock()
```

- **When**: After `unlockCallback()` returns
- **What it means**: All token movements through the PM must net to zero.
  Positive deltas (PM owes you) must be taken. Negative deltas (you owe PM)
  must be settled.
- **Implication**: V4 delta netting eliminates internal transfers for V4→V4
  paths. Intermediate deltas cancel automatically.

### 9.4 Token Flow Windows Summary

```
         ┌─────────────────────────────────────────────────────┐
  V2:    │  Before swap()   During callback   After callback   │
         │  ─────────────────────────────────────────────✓────  │
         │  K-invariant checks TOTAL balances — timing irrelevant│
         └─────────────────────────────────────────────────────┘

         ┌─────────────────────────────────────────────────────┐
  V3:    │  Before swap()   During callback   After callback   │
         │  ─────────────── ✗ ──────── ✓ ──────── ✗ ──────────  │
         │  IIA: tokens must arrive between the two snapshots   │
         └─────────────────────────────────────────────────────┘

         ┌─────────────────────────────────────────────────────┐
  V4:    │  Inside unlock() — sync/settle ordering matters     │
         │  sync BEFORE deposit, settle AFTER deposit          │
         │  All deltas must net to zero by end of unlock()     │
         └─────────────────────────────────────────────────────┘
```

---

## 10. Direct Custody & Reverse-Order Execution

### 10.1 The Goal: Minimize ERC20 Transfers

Each ERC20 transfer costs ~50,000 gas in production. Direct custody sends
tokens **from one pool directly to the next pool** (or to the PoolManager),
bypassing the executor as intermediary. This saves 1 transfer per edge.

### 10.2 Direct Custody Rules Per Edge Type

| Edge | Direct? | Mechanism | Constraint |
|------|---------|-----------|------------|
| V2→V2 | ✓ | Reverse-order: V2_SWAP_DIRECT chains through excess balance | V2 K-invariant only checks final balances |
| V2→V3 | ✓ | V2 called inside V3 callback with `to=V3b` (IIA ✓) | V2's optimistic output hits V3b during its callback window |
| V2→V4 | ✓ | V2 callback sends to PM via sync+transfer+settle | Must sync before V2 sends to PM |
| V3→V2 | ✓ | V3 sends output directly to V2 pair (recipient=V2b) | V2 K-invariant ✓; V3 IIA ✓ (V3 checks its own balance, not V2's) |
| V3→V3 | ✓ | Reverse-order: outer V3 gets input during callback | Forward-order FAILS (tokens arrive before `balance_before`) |
| V3→V4 | ✓ | V3→PM (sync before swap, settle inside unlock) + V4_TAKE→V3a | V4_TAKE to V3a during V3a's callback satisfies IIA |
| V4→V2 | ✓ | V4_TAKE sends directly to V2 pair (excess balance) | V2 K-invariant ✓ for excess balance |
| V4→V3 | ✓ | V4_TAKE→V3 during V3's own callback (reverse-order) | Forward-order FAILS; must be during V3 callback window |
| V4→V4 | ✓ | Delta netting — 0 internal transfers | Same unlock: deltas cancel in transient storage |

### 10.3 Reverse-Order Execution

The most important pattern for multi-hops involving V3:

**Forward-order (fails for V3→V3)**:
```
V3a.swap(to=V3b)  →  USDC arrives at V3b, but V3b hasn't called swap() yet
                       ⟹ USDC is in V3b's balance_before, doesn't help IIA
V3b.swap(to=V3c)  →  FAILS: V3b IIA not satisfied
```

**Reverse-order (works)**:
```
Top-level: V3c.swap(to=executor)
  V3c callback:
    V3b.swap(to=V3c)     →  WBTC arrives at V3c DURING V3c's callback ✓
    V3b callback:
      V3a.swap(to=V3b)   →  USDC arrives at V3b DURING V3b's callback ✓
      V3a callback: auto-pay WETH
```

The same pattern applies whenever V3 appears as a middle or last hop:
start from the **last** pool, and work backwards.

### 10.4 The V2 Inside V3 Callback Pattern

When a path has a V2→V3 edge, call V2 from **inside V3's callback** with
`to=V3b`. V2's optimistic output lands at V3b in the IIA window:

```
V3b.swap(to=???):
  V3b callback:
    WETH→V2a (excess balance, via ERC20_TRANSFER)
    V2a.swap(to=V3b, data=b"")  →  USDC arrives at V3b DURING callback ✓
```

V2a uses V2_SWAP_DIRECT (no callback needed) because the excess WETH
pre-funds the pair. The K-invariant passes because the pair already has
the input tokens.

---

## 11. 2-Hop Path Encyclopedia

For two-hop arbitrage, there are 9 permutations (3²). The patterns are simpler
than three-hop because there are fewer callbacks and less nesting.

### 2-Hop Transfer Count Summary

| Path | Transfers | Pattern |
|------|-----------|---------|
| V2→V2 | 3 | Flash borrow from V2b, V2a via excess balance |
| V2→V3 | 3 | V2 inside V3 callback (to=V3b, IIA ✓), V3 outermost |
| V2→V4 | 2 | V4_TAKE→V2a direct, V2a→PM via sync+settle |
| V3→V2 | 3 | V3→V2b directly (recipient=V2b), V2c via V2_SWAP_DIRECT |
| V3→V3 | 2 | Reverse-order: V3b outermost, V3a→V3b direct |
| V3→V4 | 2 | V3→PM + V4_TAKE→V3a during callback (IIA ✓) |
| V4→V2 | 2 | V4_TAKE→V2b direct, V2c V2_SWAP_DIRECT |
| V4→V3 | 2 | V3b outermost, V4_TAKE→V3b during callback (IIA ✓) |
| V4→V4 | 0–1 | Pure delta netting; 0 with V4_MINT, 1 with V4_TAKE |

### 11.1 V2→V2

**Pattern**: Flash borrow WETH from V2b. Inside V2b callback:
1. ERC20_TRANSFER WETH→V2a (creates excess)
2. V2a V2_SWAP_DIRECT→V2b (sends USDC to V2b, satisfies K-invariant)

```
Commands:
  V2_SWAP_COMPACT(V2b, zfo, amount_out, SELF, fee=30,
    forward_data = [
      ERC20_TRANSFER(WETH, V2a, amount_weth),
      V2_SWAP_DIRECT(V2a, zfo, amount_out_usdc, V2b),
    ]
  )
```

### 11.2 V2→V3

**Pattern**: V3b outermost (reverse-order). Inside V3b callback:
1. ERC20_TRANSFER WETH→V2a (creates excess)
2. V2a V2_SWAP_DIRECT→V3b (USDC directly to V3b, IIA ✓)

```
Commands:
  V3_SWAP_COMPACT(V3b, zfo, amount_in, SELF,
    forward_data = [
      ERC20_TRANSFER(WETH, V2a, amount_weth),
      V2_SWAP_DIRECT(V2a, zfo, amount_out_usdc, V3b),
    ]
  )
```

### 11.3 V2→V4

**Pattern**: Inside V4 unlock:
1. V4b swap (produces WETH)
2. V4_TAKE WETH→V2a (direct custody, creates excess)
3. V2a V2_SWAP_DIRECT→PM (USDC to PM, delta netting)
4. V4_SYNC(USDC) + V4_SETTLE (credit delta)
   — OR —
   V4_SYNC(USDC), V2a→PM, V4_SETTLE (sync BEFORE deposit)

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4b, ...),
    V4_TAKE_COMPACT(WETH, V2a, amount),
    V4_SYNC(USDC),
    V2_SWAP_DIRECT(V2a, zfo, amount_usdc, PM),
    V4_SETTLE(),
    V4_SETTLE_DELTA(WETH),  # profit or settle remaining WETH
  ])
```

### 11.4 V3→V2

**Pattern**: V3a sends output directly to V2b (recipient=V2b). Inside V3a callback:
1. V2b V2_SWAP_DIRECT→executor (WETH profit)
2. ERC20_TRANSFER WETH→V3a (auto-pay, IIA ✓)

```
Commands:
  V3_SWAP_COMPACT(V3a, zfo, amount_in, V2b,
    forward_data = [
      V2_SWAP_DIRECT(V2b, zfo, amount_weth, SELF),
      ERC20_TRANSFER(WETH, V3a, amount_owed),
    ]
  )
```

### 11.5 V3→V3

**Pattern**: Reverse-order. V3b outermost, V3a→V3b direct custody.

```
Commands:
  V3_SWAP_COMPACT(V3b, zfo_b, amount_in_b, SELF,
    forward_data = [
      V3_SWAP_COMPACT(V3a, zfo_a, amount_in_a, V3b, forward_data=b"")
    ]
  )
```

V3a auto-pays (empty forward_data). V3b IIA: tokens arrive from V3a during
callback ✓.

### 11.6 V3→V4

**Pattern**: V3a→PM (delta netting). V4_SYNC before V3a swap, V4_SETTLE inside unlock.
V4_TAKE sends WETH back to V3a during V3a callback (IIA ✓).

```
Commands:
  V4_SYNC(USDC)                    # snapshot PM USDC balance
  V3_SWAP_COMPACT(V3a, zfo, amount_in, PM,
    forward_data = [
      V4_UNLOCK([
        V4_SETTLE(),                # credit +USDC delta
        V4_SWAP_COMPACT(V4b, ...),
        V4_TAKE_COMPACT(WETH, V3a, amount),  # V3a IIA ✓
        V4_TAKE_COMPACT(WETH, SELF, profit),
      ])
    ]
  )
```

### 11.7 V4→V2

**Pattern**: Inside V4 unlock:
1. V4a swap (produces USDC)
2. V4_TAKE USDC→V2b (direct custody, creates excess)
3. V2b V2_SWAP_DIRECT→executor (WETH profit)
4. V4_SETTLE_DELTA(WETH)

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4a, ...),
    V4_TAKE_COMPACT(USDC, V2b, amount),
    V2_SWAP_DIRECT(V2b, zfo, amount_weth, SELF),
    V4_SETTLE_DELTA(WETH),
  ])
```

### 11.8 V4→V3

**Pattern**: V3b runs inside V4 unlock context. V4_TAKE sends USDC directly to V3b
during V3b's callback window, satisfying IIA.

The key insight: the V3 swap runs inside `V4_UNLOCK`'s `unlockCallback`, which
is inside `PM.unlock()`. When V3b's callback fires, the V4_TAKE in V3b's
forward_data sends USDC directly to V3b **between `balance_before` and
`balance_after`** — satisfying V3's IIA.

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4a, ...),
    V3_SWAP_COMPACT(V3b, zfo, amount_in, SELF,
      forward_data = [
        V4_TAKE_COMPACT(USDC, V3b, amount),  # USDC→V3b directly, IIA ✓
      ]
    ),
    V4_SETTLE_DELTA(WETH),
  ])
```

**Transfers**: V3b→exec(WETH from swap), PM→V3b(USDC via V4_TAKE),
  exec→PM or PM→exec(WETH via V4_SETTLE_DELTA)

**Why it works**: V3b's IIA requires tokens during callback. V4_TAKE runs
inside V3b's forward_data, so the USDC deposit happens between the two
balance snapshots. "V4→V3 IIA ✗" only applies in **forward-order** (V4_TAKE
before V3.swap() starts). In reverse-order (V3 callback, V4_TAKE during
it), IIA is satisfied. ✓

### 11.9 V4→V4

**Pattern**: Pure delta netting. All swaps inside unlock. Intermediate deltas
cancel automatically. Only the net profit needs V4_MINT or V4_TAKE.

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4a, zfo_a, amount_a),
    V4_SWAP_COMPACT(V4b, zfo_b, amount_b),
    V4_MINT_COMPACT(WETH, SELF, profit),   # 0 transfers
  ])
```

---

## 12. 3-Hop Path Encyclopedia

All 27 permutations of V2/V3/V4 for three-hop paths. Each entry shows:
- The optimal routing pattern
- The command stream structure
- The transfer count
- Why the pattern works (which constraints are satisfied)

### 3-Hop Transfer Count Summary

| Path | Transfers | Technique |
|------|-----------|-----------|
| V2-V2-V2 | 4 | Reverse-order flash borrow, V2_SWAP_DIRECT chain |
| V2-V2-V3 | 4 | Reverse-order, V2a→V2b→V3c via V2_SWAP_DIRECT |
| V2-V2-V4 | 4 | V4_TAKE→V2a, V2b→PM delta netting |
| V2-V3-V2 | 4 | Reverse-order V2c, V3b→V2c, V2a→V3b during V3b callback |
| V2-V3-V3 | 4 | V3c outermost, V2a inside V3b callback (to=V3b, IIA ✓) |
| V2-V3-V4 | 4 | V3b outermost, V2a inside V3b callback, V3b→PM |
| V2-V4-V2 | 4 | Reverse-order V2c, V2a→PM, V4_TAKE→V2c |
| V2-V4-V3 | 4 | V3c-reverse, V2a→PM, V4_TAKE WBTC→V3c |
| V2-V4-V4 | 3 | V4_TAKE→V2a direct, V2a→PM, delta netting |
| V3-V2-V2 | 4 | V3a→V2b direct, V2b→V2c via V2_SWAP_DIRECT |
| V3-V2-V3 | 4 | V3c-reverse, V2b V2_SWAP_DIRECT→V3c, V3a→V2b direct |
| V3-V2-V4 | 4 | V3a→V2b direct, V2b→PM, V4_TAKE→V3a (IIA ✓) |
| V3-V3-V2 | 4 | V3b reverse-order, V3a→V3b direct, V2c V2_SWAP_DIRECT |
| V3-V3-V3 | 4 | Full reverse-order: V3c→V3b→V3a, all direct custody |
| V3-V3-V4 | 4 | V3a→V3b reverse, V3b→PM, V4_TAKE→V3a (IIA ✓) |
| V3-V4-V2 | 4 | V3a→PM, V4_TAKE→V2c direct |
| V3-V4-V3 | 4 | V3c-reverse, V3a auto-pay + V4_TAKE WBTC→V3c |
| V3-V4-V4 | 3 | V3a→PM, V4_TAKE→V3a directly (IIA ✓) |
| V4-V2-V2 | 4 | V4_TAKE→V2b, V2b→V2c, V2c→exec |
| V4-V2-V3 | 4 | V3c-reverse, V4_TAKE→V2b, V2b→V3c (IIA ✓) |
| V4-V2-V4 | 4 | Single unlock, V4_TAKE→V2b, V2_SWAP_DIRECT, delta netting |
| V4-V3-V2 | 4 | V4_TAKE USDC→V3b (IIA ✓), V3b→V2c direct |
| V4-V3-V3 | 4 | V4_TAKE USDC→V3b (IIA ✓), merged WETH settle |
| V4-V3-V4 | 3 | Single unlock, V4_TAKE_DELTA USDC→V3b (IIA ✓), V3b→PM, delta netting |
| V4-V4-V2 | 3 | Delta netting + V4_TAKE→V2c |
| V4-V4-V3 | 3 | Inside unlock, V4_TAKE WBTC→V3c during callback, delta netting |
| V4-V4-V4 | 0–1 | Pure delta netting + V4_MINT (0) or V4_TAKE (1) profit |

> **ALL 27 paths at ≤4 transfers** (vs 6 naive). Average savings: 35.9%.

---

### 12.1 V2-V2-V2 (4 transfers)

**Pattern**: Reverse-order flash borrow from V2c. Inside V2c callback:
1. ERC20_TRANSFER WETH→V2a (creates excess)
2. V2a V2_SWAP_DIRECT→V2b (USDC to V2b, creates excess)
3. V2b V2_SWAP_DIRECT→V2c (WBTC to V2c, satisfies K-invariant)

```
Commands:
  V2_SWAP_COMPACT(V2c, c_zfo, c_out, SELF, fee=30,
    forward_data = [
      ERC20_TRANSFER(WETH, V2a, AMOUNT_WETH),
      V2_SWAP_DIRECT(V2a, a_zfo, a_out, V2b),
      V2_SWAP_DIRECT(V2b, b_zfo, b_out, V2c),
    ]
  )

Transfers: V2c→exec(WETH), exec→V2a(WETH), V2a→V2b(USDC), V2b→V2c(WBTC)
Callback: V2c auto-pay (0xFE sentinel)
```

**Why it works**: V2c's callback goes to the executor (not V2b). V2a/V2b swap
with `data=b""` (no callback). Each V2 pair has its own reentrancy guard, so
V2a/V2b can swap while V2c is locked. K-invariant passes for each pair because
total balances are consistent.

---

### 12.2 V2-V2-V3 (4 transfers)

**Pattern**: V3c outermost (reverse-order). Inside V3c callback:
1. ERC20_TRANSFER WETH→V2a (creates excess)
2. V2a V2_SWAP_DIRECT→V2b (USDC to V2b, creates excess)
3. V2b V2_SWAP_DIRECT→V3c (WBTC to V3c, satisfies IIA)

```
Commands:
  V3_SWAP_COMPACT(V3c, c_zfo, b_out, SELF,
    forward_data = [
      ERC20_TRANSFER(WETH, V2a, AMOUNT_WETH),
      V2_SWAP_DIRECT(V2a, a_zfo, a_out, V2b),
      V2_SWAP_DIRECT(V2b, b_zfo, b_out, V3c),
    ]
  )

Transfers: V3c→exec(WETH), exec→V2a(WETH), V2a→V2b(USDC), V2b→V3c(WBTC)
```

**Why it works**: V3c's IIA is satisfied because V2b sends WBTC to V3c **during
V3c's callback** (between balance_before and balance_after). V2 pairs use
V2_SWAP_DIRECT (data=b""), so no V2 callback is triggered.

---

### 12.3 V2-V2-V4 (4 transfers)

**Pattern**: Inside V4 unlock:
1. V4c swap
2. V4_TAKE WETH→V2a (direct custody, creates excess)
3. V2a V2_SWAP_DIRECT→V2b (USDC, creates excess at V2b)
4. V4_SYNC(WBTC) + V2b V2_SWAP_DIRECT→PM (WBTC delta netting)
5. V4_SETTLE + V4_SETTLE_DELTA(WETH)

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4c, ..., WBTC_amount),
    V4_TAKE_COMPACT(WETH, V2a, AMOUNT_WETH),
    V2_SWAP_DIRECT(V2a, a_zfo, a_out, V2b),
    V4_SYNC(WBTC),
    V2_SWAP_DIRECT(V2b, b_zfo, b_out, PM),
    V4_SETTLE(),
    V4_SETTLE_DELTA(WETH),
  ])

Transfers: PM→V2a(WETH via take), V2a→V2b(USDC), V2b→PM(WBTC), PM→exec(WETH via settle_delta or take)
```

**Why it works**: V4_TAKE sends WETH directly to V2a, creating excess balance
for V2_SWAP_DIRECT (no WETH transfer from executor to V2a needed). V2b sends
WBTC to PM (delta netting via sync+settle). Only the WETH profit/loss is
handled by V4_SETTLE_DELTA.

---

### 12.4 V2-V3-V2 (4 transfers)

**Pattern**: Reverse-order from V2c. V2c fires first (flash borrow WETH profit
to executor). Inside V2c callback:
1. V3b swap (to=V2c) — WBTC goes directly to V2c (satisfies K-invariant)
2. V3b callback:
   a. ERC20_TRANSFER WETH→V2a (creates excess)
   b. V2a V2_SWAP_DIRECT→V3b (USDC to V3b during V3b callback, IIA ✓)

```
Commands:
  V2_SWAP_COMPACT(V2c, c_zfo, c_out, SELF, fee=30,
    forward_data = [
      V3_SWAP_COMPACT(V3b, b_zfo, a_out, V2c,
        forward_data = [
          ERC20_TRANSFER(WETH, V2a, AMOUNT_WETH),
          V2_SWAP_DIRECT(V2a, a_zfo, a_out, V3b),
        ]
      )
    ]
  )

Transfers: V2c→exec(WETH), V3b→V2c(WBTC), exec→V2a(WETH), V2a→V3b(USDC)
```

**Why it works**: V2c gets WBTC from V3b (K-invariant ✓). V3b gets USDC from
V2a during its callback (IIA ✓). V2a gets WETH from executor (excess, K-invariant ✓).
The profit WETH stays with the executor from the initial V2c flash borrow.

---

### 12.5 V2-V3-V3 (4 transfers)

**Pattern**: V3c outermost. Inside V3c callback: V3b swap. Inside V3b callback:
V2a with to=V3b (IIA ✓ during V3b's callback).

```
Commands:
  V3_SWAP_COMPACT(V3c, c_zfo, AMOUNT_WBTC, SELF,
    forward_data = [
      V3_SWAP_COMPACT(V3b, b_zfo, AMOUNT_USDC, V3c,
        forward_data = [
          ERC20_TRANSFER(WETH, V2a, AMOUNT_WETH),
          V2_SWAP_DIRECT(V2a, a_zfo, a_out, V3b),
        ]
      )
    ]
  )

Transfers: V3c→exec(WETH), V3b→V3c(WBTC), exec→V2a(WETH), V2a→V3b(USDC)
```

**Why it works**: V3b's IIA is satisfied because V2a's optimistic USDC
transfer hits V3b **between** `balance_before` and `balance_after`. V2a uses
V2_SWAP_DIRECT (data=b"") because excess WETH was pre-deposited.

---

### 12.6 V2-V3-V4 (4 transfers)

**Pattern**: V3b outermost. Inside V3b callback:
1. V4 unlock (provides WETH to V2a as excess)
2. V2a.swap(to=V3b) sends USDC directly (IIA ✓)
3. V3b sends WBTC to PM (delta netting)

```
Commands:
  V4_SYNC(WBTC)
  V3_SWAP_COMPACT(V3b, b_zfo, AMOUNT_USDC, PM,
    forward_data = [
      V4_UNLOCK([
        V4_SETTLE(),
        V4_SWAP_COMPACT(V4c, ..., WBTC_amount),
        V4_TAKE_COMPACT(WETH, V2a, AMOUNT_WETH),
        V4_TAKE_COMPACT(WETH, SELF, profit),
      ]),
      V2_SWAP_DIRECT(V2a, a_zfo, a_out, V3b),
    ]
  )

Transfers: V3b→PM(WBTC), PM→V2a(WETH via take), V2a→V3b(USDC), PM→exec(WETH via take)
```

**Why it works**: V3b IIA: USDC from V2a arrives during V3b callback ✓.
V2a K-invariant: excess WETH from V4_TAKE ✓. V3b→PM: sync before swap,
settle inside unlock ✓.

---

### 12.7 V2-V4-V2 (4 transfers)

**Pattern**: Reverse-order from V2c. V2c fires first (WETH profit to executor).
Inside V2c callback:
1. ERC20_TRANSFER WETH→V2a (creates excess)
2. V2a V2_SWAP_DIRECT→PM (USDC delta netting)
3. V4 unlock: sync+settle USDC, V4b swap, V4_TAKE WBTC→V2c

```
Commands:
  V2_SWAP_COMPACT(V2c, c_zfo, c_out, SELF, fee=30,
    forward_data = [
      ERC20_TRANSFER(WETH, V2a, AMOUNT_WETH),
      V4_UNLOCK([
        V4_SYNC(USDC),
        V2_SWAP_DIRECT(V2a, a_zfo, a_out, PM),
        V4_SETTLE(),
        V4_SWAP_COMPACT(V4b, ...),
        V4_TAKE_COMPACT(WBTC, V2c, AMOUNT_WBTC),
      ])
    ]
  )

Transfers: V2c→exec(WETH), exec→V2a(WETH), V2a→PM(USDC), PM→V2c(WBTC via take)
```

**Why it works**: V2c K-invariant: WBTC from V4_TAKE ✓. V2a K-invariant:
excess WETH ✓. USDC delta: V4_SYNC before V2a deposit, V4_SETTLE after ✓.

---

### 12.8 V2-V4-V3 (4 transfers)

**Pattern**: V3c-reverse. V3c fires first. Inside V3c callback:
1. ERC20_TRANSFER WETH→V2a (creates excess)
2. V4 unlock: sync USDC, V2a→PM, settle, V4b swap, V4_TAKE WBTC→V3c

```
Commands:
  V3_SWAP_COMPACT(V3c, c_zfo, AMOUNT_WBTC, SELF,
    forward_data = [
      ERC20_TRANSFER(WETH, V2a, AMOUNT_WETH),
      V4_UNLOCK([
        V4_SYNC(USDC),
        V2_SWAP_DIRECT(V2a, a_zfo, a_out, PM),
        V4_SETTLE(),
        V4_SWAP_COMPACT(V4b, ...),
        V4_TAKE_COMPACT(WBTC, V3c, AMOUNT_WBTC),
      ])
    ]
  )

Transfers: V3c→exec(WETH), exec→V2a(WETH), V2a→PM(USDC), PM→V3c(WBTC via take)
```

**Why it works**: V3c IIA: WBTC from V4_TAKE arrives during callback ✓.
V2a K-invariant: excess WETH ✓. USDC delta: sync before deposit ✓.

---

### 12.9 V2-V4-V4 (3 transfers)

**Pattern**: Inside V4 unlock:
1. V4_SYNC(USDC)
2. V4_TAKE WETH→V2a (direct custody, creates excess)
3. V2a V2_SWAP_DIRECT→PM (USDC delta netting)
4. V4_SETTLE()
5. V4b + V4c swaps (delta netting)
6. V4_TAKE WETH profit

```
Commands:
  V4_UNLOCK([
    V4_SYNC(USDC),
    V4_TAKE_COMPACT(WETH, V2a, AMOUNT_WETH),
    V2_SWAP_DIRECT(V2a, a_zfo, a_out, PM),
    V4_SETTLE(),
    V4_SWAP_COMPACT(V4b, ...),
    V4_SWAP_COMPACT(V4c, ...),
    V4_TAKE_COMPACT(WETH, SELF, profit),
  ])

Transfers: PM→V2a(WETH via take), V2a→PM(USDC), PM→exec(WETH via take)
```

**Why it works**: V4_TAKE→V2a eliminates the separate ERC20 WETH→V2a transfer.
V2a→PM eliminates both V2a→exec(USDC) and exec→PM(USDC). V4b+V4c swap via
delta netting (0 internal transfers). Only 3 transfers: PM→V2a, V2a→PM, PM→exec.

---

### 12.10 V3-V2-V2 (4 transfers)

**Pattern**: V3a sends USDC directly to V2b. Inside V3a callback:
1. V2b V2_SWAP_DIRECT→V2c (WBTC to V2c, creates excess)
2. V2c V2_SWAP_DIRECT→executor (WETH profit)
3. ERC20_TRANSFER WETH→V3a (auto-pay, IIA ✓)

```
Commands:
  V3_SWAP_COMPACT(V3a, a_zfo, AMOUNT_WETH, V2b,
    forward_data = [
      V2_SWAP_DIRECT(V2b, b_zfo, b_out, V2c),
      V2_SWAP_DIRECT(V2c, c_zfo, c_out, SELF),
      ERC20_TRANSFER(WETH, V3a, AMOUNT_WETH),
    ]
  )

Transfers: V3a→V2b(USDC), V2b→V2c(WBTC), V2c→exec(WETH), exec→V3a(WETH)
```

**Why it works**: V3a IIA: WETH from executor arrives during callback ✓.
V2b/V2c use V2_SWAP_DIRECT (excess balance from V3a's direct USDC output).

---

### 12.11 V3-V2-V3 (4 transfers)

**Pattern**: V3c-reverse. V3c fires first. Inside V3c callback: V3a swap
(recipient=V2b). Inside V3a callback: V2b V2_SWAP_DIRECT→V3c + WETH→V3a.

```
Commands:
  V3_SWAP_COMPACT(V3c, c_zfo, b_out, SELF,
    forward_data = [
      V3_SWAP_COMPACT(V3a, a_zfo, AMOUNT_WETH, V2b,
        forward_data = [
          V2_SWAP_DIRECT(V2b, b_zfo, b_out, V3c),
          ERC20_TRANSFER(WETH, V3a, AMOUNT_WETH),
        ]
      )
    ]
  )

Transfers: V3c→exec(WETH), V3a→V2b(USDC), V2b→V3c(WBTC), exec→V3a(WETH)
```

**Why it works**: V3c IIA: WBTC from V2b during callback ✓. V3a IIA: WETH
from executor during callback ✓. V2b K-invariant: excess USDC from V3a ✓.

---

### 12.12 V3-V2-V4 (4 transfers)

**Pattern**: V3a→V2b direct. V4_TAKE sends WETH directly to V3a (IIA ✓).
V2b→PM delta netting.

```
Commands:
  V4_SYNC(WBTC)
  V3_SWAP_COMPACT(V3a, a_zfo, AMOUNT_WETH, V2b,
    forward_data = [
      V4_UNLOCK([
        V4_SETTLE(),
        V4_SWAP_COMPACT(V4c, ...),
        V4_TAKE_COMPACT(WETH, V3a, AMOUNT_WETH),
        V4_TAKE_COMPACT(WETH, SELF, profit),
      ]),
      V2_SWAP_DIRECT(V2b, b_zfo, b_out, PM),
    ]
  )

Transfers: V3a→V2b(USDC), V2b→PM(WBTC), PM→V3a(WETH via take), PM→exec(WETH via take)
```

**Why it works**: V3a IIA: WETH from V4_TAKE during callback ✓. V2b K-invariant:
excess USDC from V3a ✓. WBTC delta: sync before V2b deposit ✓.

---

### 12.13 V3-V3-V2 (4 transfers)

**Pattern**: V3b reverse-order. V3b fires first (sends WBTC to V2c). Inside
V3b callback: V3a swap (recipient=V3b, USDC direct custody). Inside V3a callback:
V2c V2_SWAP_DIRECT→exec + WETH→V3a.

```
Commands:
  V3_SWAP_COMPACT(V3b, b_zfo, AMOUNT_USDC, V2c,
    forward_data = [
      V3_SWAP_COMPACT(V3a, a_zfo, AMOUNT_WETH, V3b,
        forward_data = [
          V2_SWAP_DIRECT(V2c, c_zfo, c_out, SELF),
          ERC20_TRANSFER(WETH, V3a, AMOUNT_WETH),
        ]
      )
    ]
  )

Transfers: V3b→V2c(WBTC), V3a→V3b(USDC), V2c→exec(WETH), exec→V3a(WETH)
```

---

### 12.14 V3-V3-V3 (4 transfers)

**Pattern**: Full reverse-order. V3c→exec, then V3b→V3c, then V3a→V3b, then
auto-pay V3a.

```
Commands:
  V3_SWAP_COMPACT(V3c, c_zfo, AMOUNT_WBTC, SELF,
    forward_data = [
      V3_SWAP_COMPACT(V3b, b_zfo, AMOUNT_USDC, V3c,
        forward_data = [
          V3_SWAP_COMPACT(V3a, a_zfo, AMOUNT_WETH, V3b, forward_data=b"")
        ]
      )
    ]
  )

Transfers: V3c→exec(WETH), V3b→V3c(WBTC), V3a→V3b(USDC), exec→V3a(WETH)
```

**Why it works**: Each V3 pool's IIA is satisfied by its inner pool's direct
output arriving during the callback window. The deepest (V3a) auto-pays.

---

### 12.15 V3-V3-V4 (4 transfers)

**Pattern**: V3a→V3b reverse-order. V3b→PM. V4_TAKE→V3a (IIA ✓).

```
Commands:
  V4_SYNC(WBTC)
  V3_SWAP_COMPACT(V3b, b_zfo, AMOUNT_USDC, PM,
    forward_data = [
      V3_SWAP_COMPACT(V3a, a_zfo, AMOUNT_WETH, V3b,
        forward_data = [
          V4_UNLOCK([
            V4_SETTLE(),
            V4_SWAP_COMPACT(V4c, ...),
            V4_TAKE_COMPACT(WETH, V3a, AMOUNT_WETH),
            V4_TAKE_COMPACT(WETH, SELF, profit),
          ])
        ]
      )
    ]
  )

Transfers: V3b→PM(WBTC), V3a→V3b(USDC), PM→V3a(WETH via take), PM→exec(WETH via take)
```

---

### 12.16 V3-V4-V2 (4 transfers)

**Pattern**: V3a→PM. V4_TAKE→V2c directly. V2c V2_SWAP_DIRECT→exec.

```
Commands:
  V4_SYNC(USDC)
  V3_SWAP_COMPACT(V3a, a_zfo, AMOUNT_WETH, PM,
    forward_data = [
      V4_UNLOCK([
        V4_SETTLE(),
        V4_SWAP_COMPACT(V4b, ...),
        V4_TAKE_COMPACT(WBTC, V2c, AMOUNT_WBTC),
        V2_SWAP_DIRECT(V2c, c_zfo, c_out, SELF),
      ]),
      ERC20_TRANSFER(WETH, V3a, AMOUNT_WETH),
    ]
  )

Transfers: V3a→PM(USDC), PM→V2c(WBTC via take), V2c→exec(WETH), exec→V3a(WETH)
```

---

### 12.17 V3-V4-V3 (4 transfers)

**Pattern**: V3c-reverse. V3c fires first. Inside V3c callback: V3a swap
(USDC→PM) with forward_data containing V4 unlock (V4_TAKE WBTC→V3c, IIA ✓).

```
Commands:
  V4_SYNC(USDC)
  V3_SWAP_COMPACT(V3c, c_zfo, AMOUNT_WBTC, SELF,
    forward_data = [
      V3_SWAP_COMPACT(V3a, a_zfo, AMOUNT_WETH, PM,
        forward_data = [
          ERC20_TRANSFER(WETH, V3a, AMOUNT_WETH),
          V4_UNLOCK([
            V4_SETTLE(),
            V4_SWAP_COMPACT(V4b, ...),
            V4_TAKE_COMPACT(WBTC, V3c, AMOUNT_WBTC),
          ])
        ]
      )
    ]
  )

Transfers: V3c→exec(WETH), V3a→PM(USDC), exec→V3a(WETH), PM→V3c(WBTC via take)
```

**Why it works**: V3c IIA: WBTC from V4_TAKE during callback ✓. V3a IIA:
WETH from ERC20_TRANSFER during callback ✓. USDC delta: sync before V3a→PM ✓.

---

### 12.18 V3-V4-V4 (3 transfers)

**Pattern**: V3a→PM. V4_TAKE→V3a directly (IIA ✓). V4b+V4c delta netting.

```
Commands:
  V4_SYNC(USDC)
  V3_SWAP_COMPACT(V3a, a_zfo, AMOUNT_WETH, PM,
    forward_data = [
      V4_UNLOCK([
        V4_SETTLE(),
        V4_SWAP_COMPACT(V4b, ...),
        V4_SWAP_COMPACT(V4c, ...),
        V4_TAKE_COMPACT(WETH, V3a, AMOUNT_WETH),
        V4_TAKE_COMPACT(WETH, SELF, profit),
      ])
    ]
  )

Transfers: V3a→PM(USDC), PM→V3a(WETH via take), PM→exec(WETH via take)
```

---

### 12.19 V4-V2-V2 (4 transfers)

**Pattern**: Inside V4 unlock: V4a swap, V4_TAKE USDC→V2b, V2b→V2c, V2c→exec.

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4a, ...),
    V4_TAKE_COMPACT(USDC, V2b, AMOUNT_USDC),
    V2_SWAP_DIRECT(V2b, b_zfo, b_out, V2c),
    V2_SWAP_DIRECT(V2c, c_zfo, c_out, SELF),
    V4_SETTLE_DELTA(WETH),
  ])

Transfers: PM→V2b(USDC via take), V2b→V2c(WBTC), V2c→exec(WETH), exec→PM or PM→exec(WETH)
```

---

### 12.20 V4-V2-V3 (4 transfers)

**Pattern**: V3c-reverse. V3c fires first. Inside V3c callback: V4 unlock
with V4_TAKE USDC→V2b, V2b V2_SWAP_DIRECT→V3c (IIA ✓ during callback).

```
Commands:
  V3_SWAP_COMPACT(V3c, c_zfo, b_out, SELF,
    forward_data = [
      V4_UNLOCK([
        V4_SWAP_COMPACT(V4a, ...),
        V4_TAKE_COMPACT(USDC, V2b, AMOUNT_USDC),
        V2_SWAP_DIRECT(V2b, b_zfo, b_out, V3c),
        V4_SETTLE_DELTA(WETH),
      ])
    ]
  )

Transfers: V3c→exec(WETH), PM→V2b(USDC via take), V2b→V3c(WBTC), WETH settle
```

**Key insight**: "V4→V3 IIA ✗" only applies in FORWARD-order. Here, V3c fires
first and V4_TAKE + V2b→V3c happen during V3c's callback, satisfying IIA.

---

### 12.21 V4-V2-V4 (4 transfers)

**Pattern**: Single unlock. V4_TAKE USDC→V2b, V2b V2_SWAP_DIRECT→exec,
V4c swap (consumes WBTC via delta), V4_SETTLE_DELTA both currencies.

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4a, ...),
    V4_TAKE_COMPACT(USDC, V2b, AMOUNT_USDC),
    V2_SWAP_DIRECT(V2b, b_zfo, b_out, SELF),
    V4_SWAP_COMPACT(V4c, ...),
    V4_SETTLE_DELTA(WBTC),
    V4_SETTLE_DELTA(WETH),
  ])

Transfers: PM→V2b(USDC via take), V2b→exec(WBTC), exec→PM(WBTC settle), PM→exec(WETH take or settle)
```

**Why it works**: V4_TAKE→V2b creates USDC excess at V2b. V2b sends WBTC to
executor (V2_SWAP_DIRECT output goes to executor). The executor then owes
WBTC to PM (V4c's negative WBTC delta). `V4_SETTLE_DELTA(WBTC)` handles
this: it detects the negative delta, syncs PM's WBTC balance, transfers
WBTC from executor to PM, and settles the debt.

---

### 12.22 V4-V3-V2 (4 transfers)

**Pattern**: Inside V4 unlock: V4a swap, V3b swap with V4_TAKE USDC→V3b in
forward_data (IIA ✓), V3b→V2c direct, V2c V2_SWAP_DIRECT→exec.

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4a, ...),
    V3_SWAP_COMPACT(V3b, b_zfo, AMOUNT_USDC, V2c,
      forward_data = [
        V4_TAKE_COMPACT(USDC, V3b, AMOUNT_USDC),
        V2_SWAP_DIRECT(V2c, c_zfo, c_out, SELF),
      ]
    ),
    V4_SETTLE_DELTA(WETH),
  ])

Transfers: V3b→V2c(WBTC), V2c→exec(WETH), PM→V3b(USDC via take), WETH settle
```

---

### 12.23 V4-V3-V3 (4 transfers)

**Pattern**: Inside V4 unlock: V4a swap, V3c→V3b reverse-order nested, V4_TAKE
USDC→V3b during V3b callback (IIA ✓). V4_SETTLE_DELTA WETH for merged profit+settle.

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4a, ...),
    V3_SWAP_COMPACT(V3c, c_zfo, AMOUNT_WBTC, SELF,
      forward_data = [
        V3_SWAP_COMPACT(V3b, b_zfo, AMOUNT_USDC, V3c,
          forward_data = [
            V4_TAKE_COMPACT(USDC, V3b, AMOUNT_USDC),
          ]
        )
      ]
    ),
    V4_SETTLE_DELTA(WETH),
  ])

Transfers: V3c→exec(WETH), V3b→V3c(WBTC), PM→V3b(USDC via take), WETH settle
```

**Why it works**: V3b's IIA is satisfied by V4_TAKE USDC→V3b during V3b's callback ✓.
V3c's IIA is satisfied by V3b→V3c during V3c's callback ✓. The WETH settle is
merged (profit capture + V4a debit in one V4_SETTLE_DELTA).

---

### 12.24 V4-V3-V4 (3 transfers)

**Pattern**: Single V4 unlock. V4a swap (WETH→USDC). V3b swap inside unlock with
V4_TAKE_DELTA USDC→V3b in its forward_data (IIA ✓ during V3b's callback). V3b
sends WBTC to PM (delta netting via sync+settle). V4c swap consumes WBTC delta.
V4_TAKE_DELTA WETH for profit. V4_SETTLE_DELTA resolves any remaining delta.

Only 3 transfers: V3b→PM (WBTC from swap), PM→V3b (USDC via V4_TAKE_DELTA),
PM→exec (WETH profit via V4_TAKE_DELTA). The V4a USDC delta, V3b WBTC deposit,
and V4c WBTC consumption all net to zero internally — no physical transfers.

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4a, ...),
    V4_SYNC(WBTC),
    V3_SWAP_COMPACT(V3b, b_zfo, AMOUNT_USDC, PM,
      forward_data = [
        V4_TAKE_DELTA(USDC, V3b),   # USDC→V3b during callback (IIA ✓)
        V4_SWAP_COMPACT(V4c, ...),  # consumes WBTC from V3b→PM delta
        V4_TAKE_DELTA(WETH, SELF),  # profit
      ]
    ),
    V4_SETTLE(),                   # settles WBTC from V3b→PM against V4c
  ])

Transfers: V3b→PM(WBTC), PM→V3b(USDC via take_delta), PM→exec(WETH via take_delta)
```

**Why it works**: V3b IIA: USDC from V4_TAKE_DELTA arrives during callback ✓.
V3b→PM: sync before V3b swap, settle after V3b sends WBTC ✓. V4a + V4c deltas:
USDC from V4a is consumed by V4_TAKE_DELTA (not a separate settle); WBTC from
V3b→PM is consumed by V4c swap (delta netting). Only the net profit WETH needs
a physical transfer.

---

### 12.25 V4-V4-V2 (3 transfers)

**Pattern**: Delta netting for V4a+V4b. V4_TAKE WBTC→V2c directly. V2c
V2_SWAP_DIRECT→exec. V4_SETTLE_DELTA WETH.

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4a, ...),
    V4_SWAP_COMPACT(V4b, ...),
    V4_TAKE_COMPACT(WBTC, V2c, AMOUNT_WBTC),
    V2_SWAP_DIRECT(V2c, c_zfo, c_out, SELF),
    V4_SETTLE_DELTA(WETH),
  ])

Transfers: PM→V2c(WBTC via take), V2c→exec(WETH), WETH settle
```

---

### 12.26 V4-V4-V3 (3 transfers)

**Pattern**: Inside V4 unlock. V4a+V4b delta netting (USDC cancels). V3c swap
with V4_TAKE WBTC→V3c in its forward_data (during V3c callback, IIA ✓).
V3c sends WETH to executor. V4_SETTLE_DELTA(WETH) resolves the V4a WETH
debit — the executor pays the net owed WETH to PM (or PM pays executor if
V3c's WETH output exceeds V4a's debit).

Only 3 transfers: V3c→exec (WETH from swap), PM→V3c (WBTC via V4_TAKE),
WETH settlement via V4_SETTLE_DELTA. Intermediate V4a/V4b USDC deltas cancel.

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4a, ...),
    V4_SWAP_COMPACT(V4b, ...),
    V3_SWAP_COMPACT(V3c, c_zfo, AMOUNT_WBTC, SELF,
      forward_data = [
        V4_TAKE_COMPACT(WBTC, V3c, AMOUNT_WBTC),  # WBTC→V3c (IIA ✓)
      ]
    ),
    V4_SETTLE_DELTA(WETH),
  ])
```

**Transfer analysis**:

1. PM→V3c (WBTC via V4_TAKE) — V4_TAKE sends WBTC directly to V3c during
   V3c's callback window, satisfying IIA.
2. V3c→exec (WETH from swap) — V3c's swap output goes to the executor.
   V3c auto-pays its input (WBTC) because the V4_TAKE deposited WBTC during
   the callback — V3's balance_delta check passes.
3. WETH settlement via V4_SETTLE_DELTA — this may be executor→PM (if V4a's
   WETH debit exceeds V3c's WETH credit) or PM→executor (if credit exceeds
   debit). Either way, it's exactly 1 transfer.

**Why it works**: V3c IIA: WBTC from V4_TAKE arrived during callback ✓.
V4a/V4b USDC deltas cancel (intermediate netting). Only WETH and WBTC have
nonzero deltas, handled by V4_TAKE and V4_SETTLE_DELTA.

---

### 12.27 V4-V4-V4 (0–1 transfers)

**Pattern**: Pure delta netting. All 3 V4 swaps inside unlock. Intermediate
deltas cancel. Only the net WETH profit is taken (or minted as ERC6909).

```
Commands:
  V4_UNLOCK([
    V4_SWAP_COMPACT(V4a, zfo_a, AMOUNT_WETH),
    V4_SWAP_COMPACT(V4b, zfo_b, AMOUNT_USDC),
    V4_SWAP_COMPACT(V4c, zfo_c, AMOUNT_WBTC),
    V4_MINT_COMPACT(WETH, SELF, profit),
  ])

Transfers: PM→exec (WETH via V4_TAKE) or 0 with V4_MINT_COMPACT
```

**With V4_TAKE**: 1 transfer (PM sends profit WETH to executor)
**With V4_MINT_COMPACT**: 0 transfers (profit as ERC6909 accounting entry)

The delta netting works because:
```
V4a: -1 WETH +2000 USDC  (sells WETH for USDC)
V4b: -2000 USDC +100 WBTC  (sells USDC for WBTC)
V4c: -100 WBTC +2 WETH    (sells WBTC for WETH)
─────────────────────────────────────────────────
Net: +1 WETH (profit)
```

All intermediate deltas cancel. Only the net profit WETH needs to be taken
or minted.

---

## 13. ERC6909 (Mint/Burn) Strategies

### 13.1 When to Use V4_MINT_COMPACT

**Only when the executor receives net profit from V4 operations and wants to
keep it as ERC6909 inside the PoolManager.**

| Scenario | Use V4_MINT? | Why |
|----------|-------------|-----|
| V4-V4-V4 profit capture | ✓ | Saves 1 transfer; profit stays in PM |
| V2-V4-V4 profit capture | ✓ | Saves 1 transfer; WETH profit as ERC6909 |
| V3-V4-V4 profit capture | ✓ | Saves 1 transfer; WETH profit as ERC6909 |
| V4_TAKE routing (to V2/V3) | ✗ | V2/V3 need physical ERC20 tokens |
| Any non-V4 profit capture | ✗ | WETH needs to physically exist at executor |

### 13.2 When to Use V4_BURN_COMPACT

**Only when the executor already holds ERC6909 from a prior V4_MINT** and wants
to use it to pay a V4 debt instead of a physical token transfer.

```
TX1: V4-V4-V4 → V4_MINT 1 WETH as ERC6909 [0 transfers]
TX2: V4-V2-V4 → V4_BURN to fund V4a's WETH input [0 transfers]
```

### 13.3 Multi-Transaction Compounding

The real savings from ERC6909 accumulate across multiple transactions:

```
TX1: V4-V4-V4 arbitrage
  → V4_MINT(WETH, executor, profit)  [0 transfers, profit as ERC6909]

TX2: V4-V4-V4 arbitrage
  → V4_BURN(WETH, needed_amount)    [0 transfers, converts ERC6909 → +delta]
  → V4_MINT(WETH, executor, profit)  [0 transfers, new profit as ERC6909]

...

TX_N: Withdraw
  → V4_BURN(WETH, total_erc6909)    [0 transfers]
  → V4_TAKE(WETH, executor, amount)  [1 transfer — final withdrawal]
```

Over N transactions: **1 transfer total** (vs N transfers with V4_TAKE each time).

### 13.4 Important Caveats

- **ERC6909 profit is "trapped" inside PM**: Must eventually V4_BURN + V4_TAKE to get
  physical WETH. Over a single cycle (mint → burn+take), total transfers equal
  V4_TAKE directly.
- **Cold SSTORE on first MINT**: The first V4_MINT in a transaction costs ~22,100
  gas extra (zero→non-zero SSTORE). After `initialize()`, this is mitigated.
- **Do NOT use V4_MINT for routing**: V2/V3 pools need physical ERC20, not
  accounting entries. Minting to a V2/V3 address gives them useless ERC6909.

---

## 14. Profit Checking & Bribe Configuration

### 14.1 The `config` Parameter

The `config` parameter packs **check mode**, **bribe bips**, **bribe recipient**, and an **expected value**
into a single `uint256`:

```
config = (expected_value << 32) | (bribe_recipient_idx << 24) | (bribe_bips << 8) | check_mode
```

| Bits | Field | Values |
|------|-------|--------|
| 0–7 | `check_mode` | 0=skip, 1=WETH+ETH, 2=ERC6909 |
| 8–23 | `bribe_bips` | 0–10000 (0=no bribe) |
| 24–31 | `bribe_recipient_idx` | 0=coinbase, 1–31=address table index |
| 32–255 | `expected_value` | Pre-tx balance for the selected mode |

| `check_mode` | Check Performed | Use Case |
|--------------|----------------|----------|
| 0 | Skip (no check) | `config=0` — off-chain verification only |
| 1 | `WETH.balanceOf(self) + self.balance >= value` | Default for V2/V3/V4+other paths (WETH warm from transfers) |
| 2 | `PM.balanceOf(self, weth_id) >= value` | V4V4V4 with V4_MINT_COMPACT profit (ERC6909 slot warm from MINT) |

### 14.2 Constructing `config`

In your off-chain code:

```python
# Mode 1: WETH + ETH combined balance check (standard)
pre_tx_weth_eth = weth.balanceOf(executor) + executor.balance
config = (pre_tx_weth_eth << 32) | 1

# Mode 2: ERC6909 WETH balance check (V4V4V4 with MINT)
pre_tx_erc6909_weth = pm.balanceOf(executor, weth_id)
config = (pre_tx_erc6909_weth << 32) | 2

# Mode 0: Skip check (off-chain verification)
config = 0

# With 5% coinbase bribe:
config = (pre_tx_weth_eth << 32) | (500 << 8) | 1

# With 10% bribe to address table entry 3:
config = (pre_tx_weth_eth << 32) | (1000 << 8) | (3 << 24) | 1
```

### 14.3 Mode Selection Guide

| Path | Recommended Mode | Reason |
|------|------------------|--------|
| V4-V4-V4 with V4_MINT profit | 2 | ERC6909 slot is warm (~100 gas); `WETH.balanceOf` is cold (~4,700 gas) |
| All V4-including paths with V4_TAKE | 1 | WETH is warm from V4_TAKE's `transfer()` |
| V2/V3-only paths | 1 | WETH is warm from swap callbacks |
| Any path with `config=0` | 0 | Operator verifies profitability off-chain (fast path) |

### 14.4 Safety

When `check_mode > 0`, the executor asserts `combined_after >=
expected_value` at the end of execution, reverting with `InsufficientProfit(actual, expected)`
if the arbitrage was not profitable (or the command stream is incorrect).

This is the main protection against unprofitable execution and command stream
errors. **Always use a non-zero `check_mode` in production** (mode 1 or 2).

---

## 15. Bribe System

Bribe configuration is packed into the `config` ABI parameter (bits 8–23 = bribe_bips,
bits 24–31 = bribe_recipient_idx). This replaced the old BRIBE_COINBASE (0x02)
and BRIBE_ADDRESS (0x03) command-stream opcodes, saving −53 gas per path by
eliminating the slice/convert/dispatch overhead in `_preprocess`.

### 15.1 Bribe Mechanics

- **Calculated after execution**: The bribe is based on the **profit** (balance
  increase), not the total amount moved.
- **Auto-wraps**: If the executor has insufficient ETH but holds WETH, it
  automatically unwraps WETH to cover the shortfall.
- **Never reverts**: If the executor can't afford the full bribe, it sends
  whatever is available. The transaction always succeeds.
- **Max bips**: 10,000 (= 100% of profit). No on-chain enforcement since the
  command-stream branch was removed; the operator is responsible for valid bips.

### 15.2 Example

```python
# Send 5% of profit to coinbase
config = (pre_tx_balance << 32) | (500 << 8) | 1  # 500 bips = 5%

# Send 10% of profit to a specific address (idx 3 in address table)
config = (pre_tx_balance << 32) | (1000 << 8) | (3 << 24) | 1  # 1000 bips = 10%
```

---

## 16. Putting It All Together: Encoding a Complete Transaction

### 16.1 End-to-End Workflow

```
1. Identify the opportunity (pools, amounts, direction)
2. Determine the optimal routing pattern (see §11/§12)
3. Populate the address table (minimize SET_ADDRESS commands via sentinels)
4. Encode preprocessing section (SET_ADDRESS + 0xFF)
5. Encode execution commands (swaps, transfers, settlement)
6. Compute config from pre-tx balance + check mode + bribe settings
7. Call execute(commands, config)
```

### 16.2 Example: V3-V3-V4 Arbitrage

**Setup**: WETH→USDC (V3a), USDC→WBTC (V3b), WBTC→WETH (V4c).

**Step 1–2**: From §12.15, use V3a→V3b reverse-order, V3b→PM delta netting,
V4_TAKE→V3a (IIA ✓).

**Step 3–4**: Address table

```python
at = AddressTable(
    weth_addr=WETH,
    executor_addr=EXECUTOR,
    pm_addr=POOL_MANAGER,
    user0_addr=USDC,
    user1_addr=WBTC,
)
weth_idx = at.add(WETH)       # → 0xFE (sentinel)
usdc_idx = at.add(USDC)       # → 0 (table)
wbtc_idx = at.add(WBTC)       # → 1 (table)
exec_idx = at.add(EXECUTOR)   # → 0xFD (sentinel)
pm_idx = at.add(POOL_MANAGER) # → 0xFC (sentinel)

v3a_idx = at.add(v3a.address) # → 2 (table)
v3b_idx = at.add(v3b.address) # → 3 (table)

# Only the 4 protocol roles are sentinels; USDC/WBTC go through the table.
preamble = enc_preamble(at)   # SET_ADDRESS(USDC..WBTC..v3a..v3b) + 0xFF
```

**Step 5**: Execution commands

```python
# V3a callback: V4 unlock (provides WETH to V3a + profit)
v4_inner = enc_v4_settle()                                              # 1 byte
v4_inner += enc_v4_swap_compact(usdc_idx, wbtc_idx, fee, ts, 0xFF, zfo_c, AMOUNT_WBTC)  # 21 bytes
v4_inner += enc_v4_take_compact(weth_idx, v3a_idx, AMOUNT_WETH)         # 15 bytes
v4_inner += enc_v4_take_compact(weth_idx, exec_idx, profit)             # 15 bytes

# V3b callback: V3a swap with V4 unlock inside
b_fwd = enc_v3_swap_compact(v3a_idx, a_zfo, AMOUNT_WETH, v3b_idx, forward_data=enc_v4_unlock(v4_inner))

# Top-level: V4_SYNC(WBTC) + V3b swap (→PM)
commands = enc_v4_sync(wbtc_idx)                                        # 2 bytes
commands += enc_v3_swap_compact(v3b_idx, b_zfo, AMOUNT_USDC, pm_idx, forward_data=b_fwd)

# Full calldata = preamble + execution commands
full_calldata = preamble + commands
```

**Step 6**: Compute config

```python
pre_tx_weth = weth.balanceOf(executor) + executor.balance
config = (pre_tx_weth << 32) | 1  # mode 1: WETH + ETH, no bribe
```

**Step 7**: Execute

```python
tx = executor.execute(full_calldata, config, sender=owner)
```

### 16.3 Example: V4-V4-V4 with V4_MINT (0 transfers)

```python
at = AddressTable(
    weth_addr=WETH,
    executor_addr=EXECUTOR,
    pm_addr=POOL_MANAGER,
    user0_addr=USDC,
    user1_addr=WBTC,
)

inner = enc_v4_swap_compact(weth_idx, usdc_idx, 3000, 60, 0xFF, zfo_a, AMOUNT_WETH)
inner += enc_v4_swap_compact(usdc_idx, wbtc_idx, 500, 10, 0xFF, zfo_b, AMOUNT_USDC)
inner += enc_v4_swap_compact(wbtc_idx, weth_idx, 10000, 200, 0xFF, zfo_c, AMOUNT_WBTC)
inner += enc_v4_mint_compact(weth_idx, exec_idx, profit)

commands = enc_preamble(at) + enc_v4_unlock(inner)

# ERC6909 check (mode 2) since we use V4_MINT
pre_tx_erc6909 = pm.balanceOf(executor, weth_id)
config = (pre_tx_erc6909 << 32) | 2

tx = executor.execute(commands, config, sender=owner)
```

### 16.4 Example: V2-V3-V2 with Bribe

```python
at = AddressTable(weth_addr=WETH, executor_addr=EXECUTOR, user0_addr=USDC, user1_addr=WBTC)

# ... (address setup, pool setup as per §12.4)

# V2c fires first. Callback: V3b swap → V3b callback: WETH→V2a + V2a→V3b
b_fwd = enc_erc20_transfer(weth_idx, v2a_idx, AMOUNT_WETH)
b_fwd += enc_v2_swap_direct(at, v2_a, a_zfo, a_out, v3b_idx)

c_fwd = enc_v3_swap_compact(v3b_idx, b_zfo, a_out, v2c_idx, forward_data=b_fwd)

execution = enc_v2_swap_compact(v2c_idx, c_zfo, c_out, exec_idx, fee=30, forward_data=c_fwd)

# Add 5% bribe to coinbase (packed in config, not command stream)
pre_tx_weth = weth.balanceOf(executor) + executor.balance
config = (pre_tx_weth << 32) | (500 << 8) | 1  # mode 1 + 500 bips = 5%

full_calldata = enc_preamble(at) + execution

tx = executor.execute(full_calldata, config, sender=owner)
```

---

## 17. Troubleshooting

### 17.1 Common Failure Modes

| Error | Cause | Fix |
|-------|-------|-----|
| `K-invariant` | V2 pair's post-swap balances don't satisfy constant-product formula | Check amount calculations; ensure excess balance is at the pair before V2_SWAP_DIRECT/CALC |
| `IIA` / balance-delta | V3 pool's input tokens didn't arrive during callback window | Use reverse-order execution; ensure V4_TAKE or V2 output arrives DURING V3 callback |
| `CurrencyNotSettled` | V4 delta non-zero at end of unlock | Add V4_SETTLE_DELTA or V4_TAKE_DELTA for all currencies with nonzero deltas |
| `InvalidCommand(opcode)` | Byte in the command stream doesn't match any dispatch case | Check command encoding; ensure no offset errors in the byte stream |
| `InvalidCallback(caller)` | `msg.sender` doesn't match `t_callback_packed` | The callback was received from an unexpected address; check that the V2/V3 swap was set up correctly |
| `InsufficientBalance(amount, available)` | Executor lacks tokens for withdrawal | Ensure the executor has enough WETH + ETH to cover the requested amount |

### 17.2 V3 IIA Failures (Most Common)

V3 IIA failures are by far the most common routing error. Symptoms:

- Transaction reverts during a V3 callback
- The revert message mentions "IIA" or "balance delta"

**Debugging steps**:

1. Check execution order: Is the V3 swap the **outermost** call (or nested
   inside a callback)? Tokens must arrive DURING the callback, not before.
2. Check `forward_data`: Does the V3 swap have `forward_data` that sends
   tokens to V3 during the callback?
3. Check direct custody: If another pool's output goes to V3, is it called
   from inside V3's callback (not before `swap()` starts)?
4. Check V4_TAKE timing: If V4 sends tokens to V3, the V4_TAKE must happen
   INSIDE V3's callback (V3c-reverse pattern).

### 17.3 V4 Sync/Settle Ordering

If `V4_SETTLE` doesn't detect a token deposit:

1. Was `V4_SYNC(currency)` called **before** the deposit?
2. Was the deposit to the PoolManager (not to a different address)?
3. Was `V4_SETTLE` called **after** the deposit (not between sync and deposit)?

### 17.4 Verifying Command Streams

Before submitting a transaction, verify your command stream by:

1. **Hex dump**: Print the command bytes and manually verify opcodes and fields
2. **Test against fake contracts**: Use the test suite's fake contracts for
   zero-cost verification
3. **Dry-run on fork**: Use Foundry's `cast call` or Anvil fork to simulate
   the transaction without spending gas

---

## Appendix A: Opcode Quick Reference

| Opcode | Name | Size | Context |
|--------|------|------|---------|
| `0x00` | SET_ADDRESS | 21 | Preprocess |
| `0x01`–`0x03` | *(reserved)* | — | Were in command stream, now in config param |
| `0xFF` | BEGIN_EXECUTION | 1 | Separator |
| `0x10` | ERC20_TRANSFER | 15 | Any |
| `0x11` | ERC20_XFER_BALANCE | 3 | Any |
| `0x12` | WETH_DEPOSIT | 33 | Any |
| `0x13` | WETH_WITHDRAW | 33 | Any |
| `0x14` | WETH_DEPOSIT_ALL | 1 | Any |
| `0x15` | WETH_WITHDRAW_ALL | 1 | Any |
| `0x16` | SEND_ETH | 14 | Any |
| `0x17` | SEND_ETH_ALL | 2 | Any |
| `0x20` | V2_SWAP_COMPACT | 19+N | Any |
| `0x21` | V2_SWAP_CALC | 6 | Any |
| `0x22` | V2_SWAP_DIRECT | 16 | Any |
| `0x30` | V3_SWAP_COMPACT | 17+N | Any |
| `0x31` | V3_SWAP_DELTA | 4 | Any |
| `0x40` | V4_SWAP_COMPACT | 21 | V4 unlock |
| `0x41` | V4_SWAP_DYNAMIC | 9 | V4 unlock |
| `0x42` | V4_BATCH | 2+20N | V4 unlock |
| `0x50` | V4_UNLOCK | 2+N | Any |
| `0x51` | V4_TAKE | 35 | V4 unlock |
| `0x52` | V4_TAKE_COMPACT | 15 | V4 unlock |
| `0x53` | V4_TAKE_DELTA | 3 | V4 unlock |
| `0x54` | V4_SYNC | 2 | Any |
| `0x55` | V4_SETTLE | 1 | V4 unlock* |
| `0x56` | V4_SETTLE_DELTA | 2 | V4 unlock* |
| `0x57` | V4_SETTLE_ALL | 1 | V4 unlock |
| `0x58` | V4_MINT_COMPACT | 15 | V4 unlock |
| `0x59` | V4_BURN_COMPACT | 14 | V4 unlock |

\* V4_SYNC can be called outside unlock. V4_SETTLE / V4_SETTLE_DELTA must be
inside unlock. V4_TAKE / V4_MINT / V4_BURN must be inside unlock.

---

## Appendix B: Sentinel Index Table

| Hex | Decimal | Name | Resolves To | When to Use |
|-----|---------|------|-------------|-------------|
| `0xFC` | 252 | `V4_PM_SENTINEL` | `POOL_MANAGER_ADDR` | PM as recipient for settlement, delta netting |
| `0xFD` | 253 | `V4_SELF_SENTINEL` | `self` (executor) | Taking profit, minting ERC6909 to self |
| `0xFE` | 254 | `V4_WETH_SENTINEL` | `WETH_ADDR` | WETH as currency, token, or settlement target |
| `0xFF` | 255 | `V4_NATIVE_SENTINEL` | `address(0)` | Native ETH as currency, "no hooks" flag |
| `0x00`–`0xFB` | 0–251 | Table index | `t_addresses[idx]` | All other addresses (incl. USDC/WBTC) via SET_ADDRESS |

**Resolution logic**: `if idx >= 0xFC → sentinel; else → t_addresses[idx]`.
Any byte `>= 0xFC` not matching a sentinel reverts (`InvalidCommand`).

**Savings per sentinel use**: ~476 gas per transaction (eliminates SET_ADDRESS
bytes and TLOAD at runtime).

---

## Appendix C: Command Size Table

Knowing command sizes helps minimize calldata:

| Command | Size (bytes) | Notes |
|---------|-------------|-------|
| V4_SETTLE | 1 | Minimal |
| V4_SETTLE_ALL | 1 | Minimal |
| WETH_DEPOSIT_ALL | 1 | Minimal |
| WETH_WITHDRAW_ALL | 1 | Minimal |
| V4_SYNC | 2 | |
| V4_SETTLE_DELTA | 2 | |
| SEND_ETH_ALL | 2 | |
| V2_SWAP_CALC | 6 | No amount, no forward_data |
| V3_SWAP_DELTA | 4 | No amount, no forward_data |
| V4_SWAP_DYNAMIC | 9 | No amount |
| ERC20_XFER_BALANCE | 3 | |
| V4_TAKE_DELTA | 3 | |
| V2_SWAP_DIRECT | 16 | |
| ERC20_TRANSFER | 15 | |
| V4_TAKE_COMPACT | 15 | |
| V4_MINT_COMPACT | 15 | |
| V4_BURN_COMPACT | 14 | |
| SEND_ETH | 14 | |
| V4_SWAP_COMPACT | 21 | |
| V4_TAKE | 35 | (rarely used — prefer V4_TAKE_COMPACT) |
| WETH_DEPOSIT | 33 | |
| WETH_WITHDRAW | 33 | |
| SET_ADDRESS | 21 | Preprocessing only |
| BEGIN_EXECUTION | 1 | Preprocessing only |

Variable-size commands (N = forward_data length):
| Command | Base Size | With Forward Data |
|---------|-----------|------------------|
| V2_SWAP_COMPACT | 19 | 19 + N |
| V3_SWAP_COMPACT | 17 | 17 + N |
| V4_UNLOCK | 2 | 2 + N |
| V4_BATCH (1 swap) | 22 | 2 + 20 × num_swaps |

---

## Appendix D: Transfer Count Summary

### 2-Hop Paths

| Path | Naive | Optimized | Savings |
|------|-------|-----------|---------|
| V2→V2 | 4 | 3 | 1 |
| V2→V3 | 4 | 3 | 1 |
| V2→V4 | 4 | 2 | 2 |
| V3→V2 | 4 | 3 | 1 |
| V3→V3 | 4 | 2 | 2 |
| V3→V4 | 4 | 2 | 2 |
| V4→V2 | 4 | 2 | 2 |
| V4→V3 | 4 | 2 | 2 |
| V4→V4 | 2 | 0–1 | 1–2 |

### 3-Hop Paths

| Path | Naive | Optimized | Savings | Technique |
|------|-------|-----------|---------|-----------|
| V2-V2-V2 | 6 | 4 | 2 | Reverse-order flash borrow, V2_SWAP_DIRECT chain |
| V2-V2-V3 | 6 | 4 | 2 | Reverse-order, V2a→V2b→V3c via V2_SWAP_DIRECT |
| V2-V2-V4 | 6 | 4 | 2 | V4_TAKE→V2a, V2b→PM delta netting |
| V2-V3-V2 | 6 | 4 | 2 | Reverse-order V2c, V3b→V2c, V2a→V3b during V3b callback |
| V2-V3-V3 | 6 | 4 | 2 | V3c outermost, V2a inside V3b callback (to=V3b, IIA ✓) |
| V2-V3-V4 | 6 | 4 | 2 | V3b outermost, V2a inside V3b callback, V3b→PM |
| V2-V4-V2 | 6 | 4 | 2 | Reverse-order V2c, V2a→PM, V4_TAKE→V2c |
| V2-V4-V3 | 6 | 4 | 2 | V3c-reverse, V2a→PM, V4_TAKE WBTC→V3c |
| V2-V4-V4 | 6 | 3 | 3 | V4_TAKE→V2a direct, V2a→PM, delta netting |
| V3-V2-V2 | 6 | 4 | 2 | V3a→V2b direct, V2b→V2c via V2_SWAP_DIRECT |
| V3-V2-V3 | 6 | 4 | 2 | V3c-reverse, V2b V2_SWAP_DIRECT→V3c, V3a→V2b direct |
| V3-V2-V4 | 6 | 4 | 2 | V3a→V2b direct, V2b→PM, V4_TAKE→V3a (IIA ✓) |
| V3-V3-V2 | 6 | 4 | 2 | V3b reverse-order, V3a→V3b direct, V2c V2_SWAP_DIRECT |
| V3-V3-V3 | 6 | 4 | 2 | Full reverse-order: V3c→V3b→V3a, all direct custody |
| V3-V3-V4 | 6 | 4 | 2 | V3a→V3b reverse, V3b→PM, V4_TAKE→V3a (IIA ✓) |
| V3-V4-V2 | 6 | 4 | 2 | V3a→PM, V4_TAKE→V2c direct |
| V3-V4-V3 | 6 | 4 | 2 | V3c-reverse, V3a auto-pay + V4_TAKE WBTC→V3c |
| V3-V4-V4 | 6 | 3 | 3 | V3a→PM, V4_TAKE→V3a directly (IIA ✓) |
| V4-V2-V2 | 6 | 4 | 2 | V4_TAKE→V2b, V2b→V2c, V2c→exec |
| V4-V2-V3 | 6 | 4 | 2 | V3c-reverse, V4_TAKE→V2b, V2b→V3c (IIA ✓) |
| V4-V2-V4 | 6 | 4 | 2 | Single unlock, V4_TAKE→V2b, V2_SWAP_DIRECT, delta netting |
| V4-V3-V2 | 6 | 4 | 2 | V4_TAKE USDC→V3b (IIA ✓), V3b→V2c direct |
| V4-V3-V3 | 6 | 4 | 2 | V4_TAKE USDC→V3b (IIA ✓), merged WETH settle |
| V4-V3-V4 | 6 | 3 | 3 | Single unlock, V4_TAKE_DELTA USDC→V3b (IIA ✓), V3b→PM, delta netting |
| V4-V4-V2 | 5 | 3 | 2 | Delta netting + V4_TAKE→V2c |
| V4-V4-V3 | 5 | 3 | 2 | Inside unlock, V4_TAKE WBTC→V3c during callback, delta netting |
| V4-V4-V4 | 2 | 0–1 | 1–2 | Pure delta netting + V4_MINT profit only |

**ALL 27 paths at ≤4 transfers.** Total savings: 56 transfers (35.9% from 156 naive).

---

## Further Reading

| Document | Subject |
|----------|---------|
| [`pool-mechanics.md`](pool-mechanics.md) | Detailed pool verification rules, sync/settle ordering, direct custody rules, per-edge transfer analysis |
| [`pm-as-bank.md`](pm-as-bank.md) | Using PoolManager as zero-fee flash-loan source, ERC6909 compounding, cross-protocol netting |
| [`erc6909-arbitrage.md`](erc6909-arbitrage.md) | When V4_MINT/V4_BURN save transfers, decision matrix, multi-transaction compounding |
| [`transfer-count-investigation.md`](transfer-count-investigation.md) | Why 4 transfers is the target, V3 IIA analysis, settle-merge optimization |
| [`arithmetic-profit-tracking-plan.md`](arithmetic-profit-tracking-plan.md) | Why arithmetic tracking doesn't save gas (TSTORE overhead > balanceOf savings) |
