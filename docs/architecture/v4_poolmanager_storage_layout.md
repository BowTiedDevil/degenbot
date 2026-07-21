# V4 PoolManager storage layout — on-chain-truth verification slots

> Source of truth: `contract_reference/uniswap/V4/PoolManager.sol` (canonical
> Uniswap V4-core `PoolManager`, the verified deployed source at
> `0x000000000004444c5dc75cB358380D2e3dE08A90`). This note records the storage
> slots the V4 on-chain-truth verifier reads via `PoolManager.extsload`.

## Why V4 differs from V3

V3 exposes `ticks(int24)` + `tickBitmap(int16)` as **public view functions** on
each `UniswapV3Pool` — the verifier calls them directly (selectors
`0xf30dba93` / `0x5339c296`).

V4's `PoolManager` is a **singleton** whose tick state lives **inside** the
per-pool `Pool.State` struct value, reachable only via raw storage reads
(`extsload`). It exposes `extsload(bytes32)`, `extsload(bytes32,uint256)`,
and the batched `extsload(bytes32[])` (selector `0xdbd035ff`). There is no
public `ticks()`/`tickBitmap()` view.

## Top-level `PoolManager` storage (slot assignment via `forge inspect`)

`contract PoolManager is IPoolManager, ProtocolFees, NoDelegateCall,
ERC6909Claims, Extsload, Exttload`.

Storage-bearing ancestors (C3-linearized, base→derived): `Exttload` (transient,
no permanent storage) → `Extsload` (no storage) → `ERC6909` (3 mappings) →
`ERC6909Claims` (none added) → `NoDelegateCall` (none) → `Owned` (`owner`) →
`ProtocolFees` (`protocolFeesAccrued`, `protocolFeeController`) → `PoolManager`.

| Slot | Variable               | Source contract |
|-----:|------------------------|-----------------|
| 0    | `owner`                | `Owned`         |
| 1    | `protocolFeesAccrued` | `ProtocolFees`  |
| 2    | `protocolFeeController`| `ProtocolFees`  |
| 3    | `isOperator`           | `ERC6909`       |
| 4    | `balanceOf`            | `ERC6909`       |
| 5    | `allowance`            | `ERC6909`       |
| **6**| **`_pools`**           | `PoolManager`   |

So **`_pools` (mapping `PoolId => Pool.State`) is at top-level base slot `6`.**

## `Pool.State` struct — nested-mapping slot offsets

```solidity
struct State {
    Slot0  slot0;                          // `type Slot0 is bytes32` — 1 slot
    uint256 feeGrowthGlobal0X128;          // +1
    uint256 feeGrowthGlobal1X128;          // +2
    uint128 liquidity;                     // +3 (mappings always start a new slot)
    mapping(int24 tick => TickInfo) ticks;                // +4
    mapping(int16 wordPos => uint256) tickBitmap;          // +5
    mapping(bytes32 => Position.State) positions;          // +6
}
```
`forge inspect` reports `State` occupies 224 bytes = 7 slots (4 value + 3
mapping), confirming the relative offsets.

## Slot-derivation math

For a pool `poolId` (the `bytes32` `PoolId`, derived from the `PoolKey` via
`keccak256(abi.encode(currency0, currency1, fee, tickSpacing, hooks))`):

1. `S_state = keccak256(abi.encode(poolId, uint256(6)))`  — the `Pool.State`
   value base.
2. `ticks` mapping base within State = `S_state + 4`.
3. `tickBitmap` mapping base within State = `S_state + 5`.

Per-tick / per-word (nested-mapping slot math):
- `TickInfo[tick]   = keccak256(abi.encode(int256(tick),   S_state + 4))`
- `BitmapWord[word] = keccak256(abi.encode(int256(word),   S_state + 5))`

`abi.encode` of an `int24`/`int16` key sign-extends to 256 bits (two's-complement
left-padding) — the slot-derive helper MUST sign-extend negative ticks/words
before hashing, or the slot diverges for the negative half of the tick range.

### `TickInfo` field layout (first slot)

```solidity
struct TickInfo {
    uint128 liquidityGross;  // bits 0..127   of slot+0
    int128  liquidityNet;    // bits 128..255 of slot+0
    uint256 feeGrowthOutside0X128;  // slot+1   (not read by the verifier)
    uint256 feeGrowthOutside1X128;  // slot+2   (not read by the verifier)
}
```
Reading the TickInfo's first storage slot yields a packed `bytes32`:
- `liquidityGross = uint128(uint256(word) & ((1<<128)-1))`
- `liquidityNet   = int128(int256(word >> 128))`  (sign-extend the high half)

## Read strategy (batched)

One `extsload(bytes32[])` call — selector `0xdbd035ff`, calldata
`selector || abi.encode(bytes32[] slots)` — covers every tick slot + bitmap-word
slot for the pool in a single round trip. Decode the returned `bytes32[]` in
order; tick slots yield the packed `(gross, net)` per the layout above, bitmap
slots yield the raw `uint256` word.

## On-chain cross-check status

The deployed `PoolManager` storage layout is exactly what `forge inspect`
computes from the verified deployed source (the vendored
`contract_reference/uniswap/V4/PoolManager.sol`). A runtime `extsload`
round-trip against a live V4 pool (`RPC_URL` available) is a nice-to-have
sanity check, not a layout-correctness gate — the layout is canonical by
construction.
