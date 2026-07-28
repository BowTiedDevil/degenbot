# In-process sim served pool storage slots + engine-state coverage map

> Spike result for ergo task `44IRFS` (epic `TR6GWT` — stale-state elimination).
> Enumerates the EXACT on-chain storage slots the in-process revm sim reads
> for V2/V3/V4 pools during a real `execute()`, which of those the engine's
> `BotState` tracked structs carry authoritatively, and the historical finding
> that reframes the epic.

## TL;DR — the epic's original premise does NOT hold as written

The epic assumed the fix is "serve the engine's typed pool state via
`DatabaseRef::storage_ref` (option B from `bot_state_db.rs`'s historical note)."

**That approach was already implemented and deliberately reverted.** The deleted
`bot_state_db.rs` (commit `c4d95424`) carried the full encoders — V2 reserves
slot 8, V3 `slot0`/`liquidity`/`ticks(i24)` — and `read_v2_slot`/`read_v3_slot`
**returned `None`**, routing every read to the RPC fallback. The doc comments
record WHY:

- **V2 slot 8 (reserves)**: the V2 pair's `swap()` K-invariant check mixes
  slot-8 `_reserve0/_reserve1` with `IERC20.balanceOf` (the token contracts,
  served from the RPC). Serving the engine's `reserve0/reserve1` (stale from
  `update_block`) against fresh RPC `balanceOf` makes K **fail** (stale
  reserves vs fresh balance) — the engine does NOT track per-pair token
  balances. The K check is `_reserve0 * _reserve1` vs `balance0 * balance1`
  (with the fee adjustment); a stale `_reserve` against fresh `balance`
  breaks the invariant.
- **V3 `slot0`/`ticks`**: a V3 `swap()` reads `slot0` (sqrtPrice/tick) AND the
  `tickBitmap` AND `ticks(tick).feeGrowthOutside` AND `feeGrowthGlobal` AND the
  observation array. Serving only `slot0`/`liquidity`/`ticks(gross,net)` from
  the engine against RPC-served bitmap/fee-growth/observation produces an
  **intra-sim inconsistency** → `LOK`/empty reverts on every cross-tick swap.

The engine's `V3PoolState`/`V4PoolState`/`TickInfo` carry ONLY:
`sqrt_price_x96`, `liquidity`, `tick`, and per-tick `liquidity_gross`/
`liquidity_net`. They do **NOT** carry `feeGrowthGlobal0/1`,
`feeGrowthOutside0/1` per tick, the `tickBitmap` words (only a
`known_bitmap_words` key-presence set for sparse fetch, NOT the bitmap values),
`observation` index/cardinality, or per-pair ERC-20 balances. Serving the
engine's partial state against RPC-served full state was the LOK revert.

So the fix is NOT "wire up `storage_ref` for the slots we have" — that's the
reverted bug. The real fix requires either:
1. **Extend the engine state** to carry the FULL slot set (`feeGrowthGlobal`,
   per-tick `feeGrowthOutside`, `tickBitmap` words, observation index/
   cardinality, per-pair token balances for V2) so the sim reads a
   fully-consistent engine state — a large lift, OR
2. **A different mechanism** that makes the solver + sim read the SAME state
   without partially-serving typed state.

The byte-exact encoders + serving seam (tasks NQ3FPV/V5W756/H3M6AH/PXQAEY) are
still VALUABLE artifacts (the encoders are re-derived + mainnet-verified here),
but serving them in production via `storage_ref` is GATED on the engine-state
extension (option 1) OR a different approach the user chooses at the checkpoint
below.

---

## On-chain storage slots the sim reads (per family)

### V2 pair (`UniswapV2Pair`)

| Slot | Field | On-chain layout | Engine field (`V2PoolState`) | Fallback policy |
|-----:|-------|-----------------|------------------------------|-----------------|
| 8 | `reserves` | packed `uint112 reserve0; uint112 reserve1; uint32 blockTimestampLast;` (reserve0 LOW 112, reserve1 bits 112..224, ts HIGH 32) | `reserve0: U112`, `reserve1: U112` (`update_block` → ts) | **rpc-fallback** today (K-invariant needs ERC20 `balanceOf` the engine lacks) |
| 6 | `price0CumulativeLast` | `uint256` | NOT tracked | rpc-fallback |
| 7 | `price1CumulativeLast` | `uint256` | NOT tracked | rpc-fallback |
| 9 | `kLast` | `uint256` | NOT tracked | rpc-fallback (rarely read in a swap) |

Pinned mainnet triple (`cast storage`, RPC at latest): pool
`0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc` (USDC/WETH 0.3% UniswapV2):
- `cast storage <pool> 8` = `0x6a680ee70000000000fde86778117bf0c05b000000000000000007fcdf150144`
  (reserve0=low112, reserve1=mid112, ts=high32)

### V3 pool (`UniswapV3Pool`)

| Slot | Field | On-chain layout | Engine field (`V3PoolState`) | Fallback policy |
|-----:|-------|-----------------|------------------------------|-----------------|
| 0 | `slot0` | packed `uint160 sqrtPriceX96` (low 160) `\| int24 tick` (bits 160..184) `\| uint16 observationIndex` `\| uint16 observationCardinality` `\| uint16 observationCardinalityNext` `\| uint8 feeProtocol` `\| bool unlocked` | `sqrt_price_x96: U256`, `tick: i32` | serve sqrtPrice/tick; rest rpc-fallback |
| 1 | `feeGrowthGlobal0X128` | `uint256` | **NOT tracked** | rpc-fallback (read by swap callback) |
| 2 | `feeGrowthGlobal1X128` | `uint256` | **NOT tracked** | rpc-fallback (read by swap callback) |
| 3 | *(V3 layout-dependent — see probe)* | — | — | rpc-fallback |
| 4 | `liquidity` | `uint128` (low 128, high 128 zero) | `liquidity: u128` | serve |
| 5 | `ticks` mapping base | `mapping(int24 => TickInfo)`; per-tick slot = `keccak256(sign_extend_24(tick) . 5)` | `tick_data: HashMap<i32, TickInfo>` (gross + net ONLY) | serve gross/net; feeGrowthOutside rpc-fallback |
| 6 | `tickBitmap` mapping base | `mapping(int16 => uint256)`; per-word slot = `keccak256(sign_extend_16(word) . 6)` | **NOT tracked** (only `known_bitmap_words` key-presence set) | rpc-fallback (read by swap) |
| 7 | `positions` mapping | `mapping(bytes32 => Position.Info)` | NOT tracked | rpc-fallback (not read by a swap) |
| 8+ | `observations` | `Observation[65535]` | NOT tracked | rpc-fallback (read on `observations` calls, not a plain swap) |

`TickInfo` on-chain (the `ticks(tick)` slot value, packed across 3 slots):
- slot+0: `uint128 liquidityGross` (low 128) `\| int128 liquidityNet` (high 128)
- slot+1: `uint256 feeGrowthOutside0X128` — **NOT in engine `TickInfo`**
- slot+2: `uint256 feeGrowthOutside1X128` — **NOT in engine `TickInfo`**

Pinned mainnet triple: pool `0xCBCdF9626bC03E24f779434178A73a0B4bad62eD`
(UniswapV3), decoded via `slot0()`/`liquidity()` views:
- sqrtPriceX96 = `46013657643641178635361266647644285`
- tick = `265455`
- liquidity = `30220394541582389`
- `cast storage <pool> 0` = `0x00016601680168000a040cef000000000008dca6028b78f02240aced87bb387d`
- `cast storage <pool> 4` = `0x000000000000000000000000000000000000000000000000006b5d49e99f8835`
- `cast storage <pool> 1` (feeGrowthGlobal0, NOT tracked) = `0x000000000000000000000000000000000000036add267a1dbae5770fead3dd1a`

### V4 `Pool.State` (PoolManager singleton at `0x000000000004444c5dc75cB358380D2e3De08A90`)

`_pools` mapping at top-level slot **6** (per `v4_poolmanager_storage_layout.md`).

| Pool.State offset | Field | On-chain layout | Engine field (`V4PoolState`) | Fallback policy |
|------------------:|-------|-----------------|------------------------------|-----------------|
| +0 | `slot0` | `type Slot0 is bytes32` (packed uint160 sqrtPriceX96 `\| int24 tick `\| ...`) | `sqrt_price_x96`, `tick` | serve sqrtPrice/tick |
| +1 | `feeGrowthGlobal0X128` | `uint256` | **NOT tracked** | rpc-fallback |
| +2 | `feeGrowthGlobal1X128` | `uint256` | **NOT tracked** | rpc-fallback |
| +3 | `liquidity` | `uint128` | `liquidity: u128` | serve |
| +4 | `ticks` mapping base | `mapping(int24 => TickInfo)` | `tick_data: HashMap<i32, TickInfo>` (gross + net ONLY) | serve gross/net; feeGrowthOutside rpc-fallback |
| +5 | `tickBitmap` mapping base | `mapping(int16 => uint256)` | **NOT tracked** | rpc-fallback |
| +6 | `positions` mapping | `mapping(bytes32 => Position.State)` | NOT tracked | rpc-fallback |

`poolId` = `keccak256(abi.encode(currency0, currency1, fee, tickSpacing, hooks))`.
`S_state = keccak256(abi.encode(poolId, uint256(6)))`. Per-tick slot =
`keccak256(abi.encode(sign_extend_24(tick), S_state + 4))`. Per-bitmap-word slot =
`keccak256(abi.encode(sign_extend_16(word), S_state + 5))`.

**Note**: the deleted `bot_state_db.rs` historical comment claimed "V4 has no
persistent on-chain storage at fixed slots; their swap state lives in transient
storage." That is INCORRECT — `Pool.State` is **persistent** at slot 6 (proven
by the pinned extsload below; `CurrencyDelta` is the transient part). The V4
slot0/liquidity/ticks are served via `storage_ref` against the PoolManager
address at `S_state + offset`, the same mechanism as V3.

Pinned mainnet triple: poolId
`0x21c67e77068de97969ba93d4aab21826d33ca12bb9f565d8496e8fda8a82ca27`,
`S_state = 0xda8cac368d67cd2f2d8aaa5cc531768e0fa3b1d205c5c5de60da078e1f59bdfc`:
- `cast storage PM S_state` (slot0) = `0x0000000001f407d07dfcef1000000000000000000002d6f3af955d0e737f29c9`
- `cast storage PM S_state+3` (liquidity) = `0x0000000000000000000000000000000000000000000000000130470b58738b1e`
- `cast storage PM S_state+1` (feeGrowthGlobal0, NOT tracked) = `0x000000000000000000000000000004c884997f7288ba1f071632550b2e0e7710`

---

## The divergence the `CurrencyNotSettled` failures are rooted in

The solver computes `hop_outputs` from the engine's event-applied typed state
(`V3PoolState.sqrt_price_x96`/`liquidity`/`tick`/`tick_data.gross,net`).
The sim's `BotStateDb.storage_ref` forwards EVERY read to the RPC. At `age=0`
(solve_block == sim_block), the divergence is **event-processing lag + the
RPC's freshest state** vs the engine's processed head — a razor-thin arb's
non-V4 hop under-delivers vs the solver's `hop_output`, the V4
`SETTLE`/`SETTLE_DELTA` exact-amount comes up short, and the unlock reverts
with `CurrencyNotSettled` (selector `0x5212cba1`). The composer is proven
structurally correct (42/288 V4-V3-V3 sims succeed). See ergo `RGT4DI` for the
mainnet inspector exercise that confirmed this.

## Slots the engine does NOT carry (must stay on RPC fallback, OR be added)

- **V2**: per-pair ERC-20 `balanceOf` (token0, token1) — needed for the K check
  alongside slot-8 reserves. `price*CumulativeLast`, `kLast`.
- **V3/V4**: `feeGrowthGlobal0X128`, `feeGrowthGlobal1X128` (slots +1, +2 of the
  Pool.State). Per-tick `feeGrowthOutside0X128`/`feeGrowthOutside1X128` (the
  `ticks(tick)` slot+1/+2). The `tickBitmap` words (slot +5 mapping). The
  `observation` index/cardinality/array.
- **V4 only**: `CurrencyDelta` is **transient** (TSTORE/TLOAD, EIP-1153) — not
  served via `storage_ref`; seeded via `journaled_state.transient_storage` if
  ever needed (the V4 swap settles deltas in the unlock callback; the sim's own
  swap writes them, so no pre-seed is needed for a forward sim).

## Checkpoint — material design pivot, user decision required

**Produce**: this doc (`docs/architecture/in_process_sim_served_slots.md`).

**Then ask**: the epic's original "serve engine state via `storage_ref`" plan is
the reverted bug — the engine doesn't carry fee-growth/bitmap/balances, so a
partial serve reintroduces the LOK/K-invariant reverts. Choose the fix path:

- **(A) Extend the engine state** to carry the full slot set (feeGrowthGlobal,
  per-tick feeGrowthOutside, tickBitmap words, V2 per-pair token balances, V3
  observation). Then serve ALL slots from the engine consistently. Large lift;
  touches `V3PoolState`/`V4PoolState`/`TickInfo` + the Mint/Burn/Swap/Transfer
  event application. Most correct.
- **(B) Shadow-RPC-read at sim time**: at sim time, for each tracked pool the
  solver read, do ONE batched `extsload`/`eth_call` for the FULL slot set at
  the sim block, seed the `CacheDB` from THAT, AND have the solver re-derive
  `hop_outputs` from the same batch. Eliminates the sim/solver divergence by
  making both read the same fresh RPC batch, without extending the engine.
  Adds one batched RPC per pool per sim — but that's already what the current
  `storage_ref` fallback does (cold RPC per slot). Net RPC cost may be LOWER
  (batched vs per-slot).
- **(C) Hybrid**: serve the engine's tracked slots (sqrtPrice/tick/liquidity/
  tick gross,net) via `storage_ref` AND accept the feeGrowth/bitmap RPC
  fallback, BUT only when the engine is caught up to the sim block (gate on
  `update_block == sim_block`); otherwise fall through to RPC entirely. Reduces
  the divergence surface without the full extension, but the LOK/K risk
  remains on cross-tick/fee-accruing swaps.

**Do not proceed to the encoder/serving tasks without approval** — the chosen
path determines whether the serving seam (`storage_ref`) is the mechanism at
all (B uses a CacheDB pre-seed instead).

## Validation Gates
This doc, committed. A reader can verify any row against the cited source / the
pinned `cast storage` triples (RPC endpoints + pool ids + slot derivations).
