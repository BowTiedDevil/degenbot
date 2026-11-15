# Sim-anchor mirror coverage — measured overlap over real candidate read sets

**Source:** the retired `mirror_coverage_probe` example (`degenbot-simulation`), measured fully offline. **Standing regression:** `rust/crates/engine/degenbot-simulation/tests/mirror_anchor_coverage.rs` (asserts the projection-alive invariant and the exact tallies/classifications below; no RPC, no duplicated fixture bytes).

## The question

Of a real mainnet candidate's cold SLOAD set, how many keys does the sim anchor (`SimAnchorState`, the enumerated scalar projection of ADR-039) already carry — and is the word the anchor carries *serveable* (byte-identical to the chain word) or only *comparable* (tracked fields packed in the on-chain layout, untracked bits zeroed, per `degenbot_substrate::divergence_probe` module docs)?

## Why the serveable/comparable distinction is the point

The anchor's word is a *comparison* shape: it packs only the fields the engine tracks and **zeroes every untracked bit**. A serve has to hand the EVM the FULL chain word, so a comparable word cannot be served:

- a zeroed V2 `blockTimestampLast` (high 32 bits of pair slot 8) changes `UniswapV2Pair._update`'s elapsed-time math;
- zeroed V3 `slot0` bits — `observationIndex`, `feeProtocol`, `unlocked` — change protocol-fee math and trip `nonReentrant`'s `require(unlocked == 1)` (`LOK`).

The naive serve of these slots was the reverted K-invariant/`LOK` bug; the divergence probe exists to compare, not to serve. The measured table below is the evidence that the gap is real on live shapes: every covered V3 `slot0` and the V2 reserves word are comparable-but-not-serveable, and the tracked-bit diff is zero in every one of them — the engine's tracked fields are exact, and the *whole* divergence lives in the bits the anchor deliberately does not carry.

## Method

The read sets are measured, not invented: each key is a storage slot a real chain-1 transaction touched, taken from the node's `prestateTracer` over the transaction's parent state (the same pre/post sets `frame_oracle.rs` compares fork truth against). Two frames of the committed capture `rust/crates/engine/degenbot-strategy/tests/fixtures/frame_replay_capture.jsonl`:

- V3 frame `0xa2fe840bff879fee1393cfe553df2ba2bbd8ac5528cb2e00f53216f572c976ab` @ block **26303798** — SwapRouter02 `exactInput`, USDC → WETH → TKN (two canonical Uniswap V3 pools), 9 accounts / 25 storage slots.
- V2 frame `0xf2ca47766dfa8d9aeb280f19b6f8abc41e4479113229ad89311ef94c43e41bcf` @ block **26305173** — UniswapV2Router `swapExactETHForTokens` (one canonical V2 pair), 6 accounts / 10 storage slots.

The pools are registered in a real `BotState` at their measured pre-state (values read from the same parent block — exactly what a hydration read would have produced), snapshotted with the production `SimAnchorState::snapshot`, and every measured key is classified with the production projection (`probe_tracked_storage_slot`) plus the word comparison a serve would have to pass (`TrackedSlotKind::tracked_bit_mask`).

## Measured table (reproduced offline from the committed tree)

```
mirror_coverage_probe: the sim anchor vs REAL mainnet candidate read sets (debug_assertions=false)

== tally (storage slots only; every ERC20/EOA row is RPC-only by construction)
  V3 frame: 4/25 anchor-covered (16.0%); 2 of those serve the exact chain word (8.0% of all reads)
  V2 frame: 1/10 anchor-covered (10.0%); 0 of those serve the exact chain word (0.0% of all reads)
  BOTH: 5/35 covered (14.3%), 2/35 served verbatim (5.7%)
  account rows: 15 (9 + 6) — the mirror carries no balances/nonces, WarmCodeCache carries the contract rows cross-block
```

Every anchor-covered row, classified:

| Frame | Account | Slot | Kind | engine==chain | Tracked-bit diff | Untracked-bit diff | Class |
|---|---|---|---|---|---|---|---|
| V3 @ 26303798 | pool A `0x7261bb34…` | 0x0 | `V3Slot0` | **false** | `0x0` | `0x0001660002000200000000…0000` | comparable only |
| V3 @ 26303798 | pool A `0x7261bb34…` | 0x4 | `V3Liquidity` | **true** | `0x0` | `0x0` | **serveable** |
| V3 @ 26303798 | pool B `0x88e6a0c2…` | 0x0 | `V3Slot0` | **false** | `0x0` | `0x00014402d302d3016c0000…0000` | comparable only |
| V3 @ 26303798 | pool B `0x88e6a0c2…` | 0x4 | `V3Liquidity` | **true** | `0x0` | `0x0` | **serveable** |
| V2 @ 26305173 | pair `0xecfbe9b1…` | 0x8 | `V2Reserves` | **false** | `0x0` | `0x6ae478a300000000…0000` | comparable only |

Reading the rows: the untracked-bit diff of pool A's `slot0` is exactly the zeroed `observationIndex`/`feeProtocol`/`unlocked` tail of the chain word `0x000166000200020000fe84b7…` (the tracked low 184 bits agree), pool B's is the same shape, and the V2 pair's is exactly the high-32 `blockTimestampLast` word `0x6ae478a3` over agreeing reserves. The two `V3Liquidity` words serve verbatim because the full 256-bit chain word is tracked there (a `uint128` liquidity in the low half, zero high half on-chain).

The remaining 30 of 35 storage reads (V3: 21, V2: 9) are RPC-only by construction — fee-growth, observations, tick/tickBitmap words, cumulative prices, `unlocked`, and every ERC-20/proxy key. **Account-row caveat:** 15 of the 45 measured keys (9 + 6) are account rows (balances/nonces), which the mirror never carries; the `WarmCodeCache` carries the contract rows cross-block. The tallies above count storage slots only.

## Interpretation

- **Coverage is narrow and that is fine.** 14.3% of a real candidate's cold read set is anchor-covered; the anchor's value is the exactness of the covered rows, not breadth.
- **Exactness of the covered rows is the headline.** Every covered row has a zero tracked-bit diff — where the anchor carries a tracked field, it matches the chain's pre-state to the bit, and the two `V3Liquidity` words are byte-identical serves.
- **Comparable ≠ serveable is structural, not incidental.** Three of five covered rows (and the only covered V2 row) carry a correct tracked field but a zeroed untracked tail; serving them would hand the EVM a word the chain does not hold. This is why the anchor feeds the divergence *probe* and never a storage serve.

## The projection-alive invariant

The one assertion the example carried that a doc cannot: a **registered** pool must project its scalar rows into `SimAnchorState`. An empty projection over a registered pool means the registry's entry insert was compiled away (the id binding and the entry insert are separate writes; a build where the insert dies keeps the binding only), so every anchor-served read silently falls through to RPC with no error anywhere. `mirror_anchor_coverage.rs` asserts this for both frames — the V3 pools' scalar row and the V2 pair's reserves row — plus the exact tallies and per-row classifications above.
