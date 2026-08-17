# T6 Analysis: Can a single rule reproduce the 3-hop grammar-walker arms?

Spike (analysis only, no code changes). Source: `rust/crates/degenbot-executor/src/grammar_walker/shapes/three_hop.rs` (2253 lines). Mechanics: `grammar_walker.rs::{v3_flash_to, v2_swap, v2_swap_direct, v2_flash}`. Facts: `HopFacts` (prot, zfo, swap_fee, tick_spacing, in/out_currency, out_dest:{Executor,PoolManager,Repay(addr)}, repay:{SelfRefund,Offstream,NetZero}, pool_address, currency0/1_address, terminal_form:{None,DirectHandoff,UnlockInternal}}).

The digest's arm-name headers are mangled; verdicts below trust the family `── …` comments + line ranges, cross-checked against the actual arm bodies.

## Verdict table

| # | family (lines) | topology (outer→inner) | verdict |
|---|---|---|---|
| 1 | v3v3v3 (17–87) | `SelfFund, fc[fb[fa[empty cb]]]`; fa auto_repay | DERIVABLE |
| 2 | v3v3v2 (88–155) | `SelfFund, fb[fa[c_swap, WETH→fa]]` (reverse) | DERIVABLE |
| 3 | v3v2v3 (156–222) | `SelfFund, fc[fa[b_swap(repays fc), WETH→fa]]` (reverse) | DERIVABLE |
| 4 | v3v2v2 (223–276) | `SelfFund, fa[b, c, WETH→fa]` (hop0 outer) | DERIVABLE |
| 5 | v2v2v3 (277–335) | `SelfFund, fc[prefund→v2a, a_swap, b_swap(repays fc)]` | DERIVABLE |
| 6 | v2v3v3 (336–412) | `SelfFund, fc[fb[prefund→v2a, a_direct(repays fb)]]` | DERIVABLE |
| 7 | v2v3v2 (336–480) | `SelfFund, v2_flash(fc)[fb[prefund→v2a, a_direct(repays fb)]]` | DERIVABLE |
| 8 | v4v4v4 (481–675) | `V4Unlock[V4Batch\|V4Swap×3, capture, V4SettleAll]` | DERIVABLE |
| 9 | v4v2v4 (743–824) | `V4Unlock[V4Swap(a), Take→v2b, b_swap, V4Swap(c), V4SettleAll]` | DERIVABLE |
| 10 | v4v4v2 (1015–1095) | `V4Unlock[V4Swap(a), V4Swap(b), Take→v2c, c_swap, V4SettleAll]` | DERIVABLE |
| 11 | v4v3v3 (825–913) | `V4Unlock[V4Swap(a), fc[fb[Take(repays fb)]], V4SettleΔ(WETH)]` | DERIVABLE |
| 12 | v4v3v4 (914–1014) | `V4Unlock[V4Swap(a), V4Sync, fb[Take(repays fb)], V4Settle, V4Swap(c), V4SettleAll]` | DERIVABLE |
| 13 | v4v4v3 (1096–1194) | `V4Unlock[V4Swap(a), V4Swap(b), fc(recipient SELF)[Take(repays fc)], V4SettleAll]` | DERIVABLE |
| 14 | v4v3v2 (1275–1358) | `V4Unlock[V4Swap(a), fb[Take(repays fb), c_swap(→SELF)], V4SettleΔ(WETH)]` | DERIVABLE |
| 15 | v4v2v3 (1195–1274) | `fc(SELF)[V4Unlock[V4Swap(a), Take→v2b, b_swap(repays fc), V4SettleΔ(WETH)]]` | DERIVABLE |
| 16 | v2v2v4 (1359–1437) | `SelfFund, V4Unlock[V4Sync, prefund→v2a, a_swap, b_swap→PM, V4Settle, V4Swap(c), V4SettleAll]` | DERIVABLE |
| 17 | v2v4v4 (1438–1533) | `SelfFund, V4Unlock[V4Sync, prefund→v2a, a_swap→PM, V4Settle, V4Swap(b), V4Swap(c), V4SettleAll]` | DERIVABLE |
| 18 | v2v3v4 (1534–1633) | `V4Sync(fwd_b), fb(PM)[V4Unlock[V4Settle, V4Swap(c), V4TakeCompact→v2a(seeds), V4TakeCompact→SELF(profit), V4Sync(WETH), V4SettleAll], a_direct(repays fb)]` | **needs-fact: SEED-MECHANISM** |
| 19 | v3v2v4 (1634–1744) | `SelfFund, fa[fb(V2-flash, auto_repay=true)[V4Unlock[V4Swap(c), V4TakeCompact→SELF(out_c), V4SettleΔ(fwd_b)]], WETH→fa]` — **forward** nest a(b) | **BESPOKE / needs-fact: REPAY-TIMING** |
| 20 | v3v3v4 (1745–1839) | `V4Sync(fwd_b), fb(PM)[fa(recipient=fb, rpr=true)[V4Unlock[V4Settle, V4Swap(c), V4TakeCompact→v3a(repays fa), V4SettleAll]]]` — reverse | DERIVABLE |
| 21 | v2v4v2 (1840–1933) | `SelfFund, v2_flash(fc)[prefund→v2a, V4Unlock[V4Sync, a_swap→PM, V4Settle, V4Swap(b), V4TakeCompact→v2c(repays fc), V4SettleΔ]]` | DERIVABLE |
| 22 | v2v4v3 (1934–2036) | `SelfFund, fc(V3-flash, SELF)[prefund→v2a, V4Unlock[a_swap→PM, V4Settle, V4Swap(b), V4TakeCompact→v3c(repays fc), V4SettleΔ]]` | DERIVABLE |
| 23a | v3v4v2 (2037–2201, DirectHandoff) | `SelfFund, V4Sync(fwd_a), fa(PM)[V4Unlock[V4Settle, V4Swap(b), V4TakeDelta→v2c(seeds), V4SettleAll], c_swap(→SELF), WETH→fa]` | DERIVABLE (terminal_form fact) |
| 23b | v3v4v4 (2037–2201, UnlockInternal) | `SelfFund, V4Sync(fwd_a), fa(PM)[WETH→fa, V4Unlock[V4Settle, V4Swap(b), V4Swap(c), V4TakeDelta→SELF, V4SettleAll]]` | DERIVABLE (terminal_form fact) |

**Tally: 21 DERIVABLE · 1 needs-fact (v2v3v4 seed-mechanism) · 1 needs-fact/bespoke (v3v2v4 repay-timing).**

## (a) Can ONE extended rule reproduce all 21 byte-identically?

**No — but a SMALL SET (3 rules) gets 21 of 23 arm-instances; 2 remain holdouts needing one new fact each.** The literal H1 ("flashes nest in REVERSE swap order") is an oversimplification and **fails v3v2v4**: that arm nests the leading V3 flash OUTSIDE the V2 flash (forward `a(b)`), not reverse. The reason it's reverse in every other multi-flash arm is that a flash whose repay currency is delivered by an inner flash must wrap that inner flash — and in those arms the inner flash happens to be the upstream one. v3v2v4 breaks the coincidence because its V2 flash uses `auto_repay=true` (repay drawn at borrow, pre-callback), which forces the *seeder* flash to be outer instead. H1 therefore does not generalize; the **repay-graph**, not swap-order, drives nesting.

## Winning rule set (3 rules)

**R1 — Enclosure selection (root frame).**
- All-V4, or `hop0.prot == V4` → root enclosure is a `V4Unlock`; flashes (if any) nest *inside* its `inner`.
- `hop0` ∈ {V2-nonflash, V3-flash, V2-flash} with a V4 terminal but no V4 leading hop, and ≥1 flash present → flash is the root; any V4 hop's `V4Unlock` becomes a **sub-enclosure** folded into the flash callback that contains it.
- Leading V2/V3 hops, no flash, V4 terminal → `[SelfFund, V4Unlock[flat: V4Sync, prefund, swaps, V4Settle…]]`.

**R2 — Flash nest order is REPAY-GRAPH-driven (extended H1).**
- A flash whose repay currency is delivered by flash X has X nested inside its callback (`X` inner). ⇒ produces reverse nesting for V3↔V3 chains (the common case).
- The terminal flash (recipient SELF) is outermost.
- A V2 flash with `auto_repay=true` draws repay at borrow (pre-callback), so the flash that *seeds* its pool must run first ⇒ that seeder is OUTER (forward). This is the v3v2v4 case; it is the only forward-nested arm and is **not derivable without a repay-timing fact** (see below).
- Non-flash hops fold as plain steps into the callback of the first flash in swap order (the innermost), threaded in swap order; the last one whose output exits to SELF or repays an enclosing flash does so via its `repays_flash`/`seeds_pool` flags.

**R3 — Leaf + WETH-seed placement.**
- Non-flash swap steps and the WETH seed/repay attach to the **first-flash-in-swap-order's callback** (the deepest), in swap order.
- WETH seed: V3-led → repay the first flash in its own callback (`Erc20Transfer repays_flash=fa`); V2-led → prefund the leading non-flash pool (`Erc20Transfer seeds_pool=v2a, repays_flash=None`); inside a V4Unlock's delta accounting → emit as `V4TakeCompact`/`V4TakeDelta` instead of `Erc20Transfer` (this is the v2v3v4 holdout).
- V4 delta threading (Sync/Settle/Take/SettleAll) per currency boundary is **already** factored into `v4_bridge_steps`, `v4_terminal_capture_steps`, `v4_scaffold_table` — a generic walker reuses them, it does not re-derive them.
- The merged v3v4{v2,v4} split is already driven by the existing `terminal_form` fact; no new fact needed for it.

## (b) Missing facts (what's absent from `HopFacts`)

Existing fields cover routing (`out_dest`) and obligation-category (`repay`), but **not** (i) the *mechanism* of repayment nor (ii) the *timing* of the borrow-draw relative to the callback. Those two gaps are exactly what the 2 holdouts expose.

1. **`repay_mechanism` per flash hop** (vocabulary: `AutoFromExecutor`, `TransferInCallback`, `V4TakeInUnlock`, `DownstreamFlashDelivery`, `DownstreamTakeSeeds`).
   - Unlocks arm **#19 v3v2v4** (forward `a(b)` + `auto_repay=true`): only `AutoFromExecutor`/`TransferInCallback` with a *pre-callback draw* verdict on the V2 flash reproduces the forward nest byte-identically. Current `repay: SelfRefund` is identical for fa in both v3v2v4 (forward) and v3v3v4 (reverse) — not distinguishing; the timing sub-fact is.
   - Also disambiguates the hand-set `auto_repay` flag (only set in v3v2v4, line 1710 — verified) and the `rpr` (recipient_pool_repays) hand-flag across all arms.

2. **`seed_delivery` per WETH seed** (vocabulary: `Erc20Transfer`, `V4TakeCompact`), or equivalently a boolean "is the seed target inside the active V4Unlock's delta ledger".
   - Unlocks arm **#18 v2v3v4**: the optimal-WETH prefund to `v2a` is emitted as a `V4TakeCompact(seeds_pool=v2a)` *inside* the V4Unlock plus a profit take `V4TakeCompact→SELF`, because the seed currency is a V4-managed WETH delta. Without this fact the walker would emit an `Erc20Transfer` and the byte stream diverges. (The profit take itself is already the existing `v4_terminal_capture_steps` pattern.)

No other arms require new facts. Everything else — including the AddressTable "golden-ordered" staging per terminal form, which is orthogonal to nesting topology — is derivable from existing facts + inputs + the helpers above.

## Line-count estimate

- Arm bodies occupy **~1,600 lines** (17–2201 ≈ 2,184 raw; minus ~580 of `derive()` scaffold/comments/blank).
- A generic walker implementing R1–R3 + reusing `v4_bridge_steps`/`v4_terminal_capture_steps`/`v4_scaffold_table`:
  - enclosure dispatch + builder: ~150
  - flash-nest builder (repay-graph driven): ~120
  - leaf + WETH-seed placement: ~100
  - `V4Swap`/`V2Swap*`/`FlashSwap` step-literal helpers (the bulk of the current arm code): ~80
  - special-case for the 2 holdouts (v3v2v4 forward/auto, v2v3v4 V4TakeCompact-seed + profit take): ~60
  - AddressTable staging per shape: ~80
  - **≈ 590–650 lines**.
- ⇒ **~2.5–2.7× reduction** (not 10×). The compression comes mostly from collapsing the repeated `V4Swap{…}`/`FlashSwap{…}` literal blocks into per-protocol step helpers and from folding the 9 V4-led variants into one enclosure rule; the genuinely irreducible residue is the 2 holdouts above plus the per-boundary V4 delta sequencing that already lives in shared helpers.

## Bottom line

A single rule (H1) does **not** suffice — it fails v3v2v4. A 3-rule set (enclosure selection · repay-graph nest order · leaf+seed placement) reproduces 21 of 23 arm-instances byte-identically. Two arms remain: **v3v2v4** (needs a `repay_mechanism`/timing fact, or equivalently an `auto_repay` flag, to justify forward nesting) and **v2v3v4** (needs a `seed_delivery` mechanism fact to emit V4TakeCompact-prefund inside the unlock). Adding both facts to `HopFacts` would let a generic walker subsume all 23 instances in ~600 lines vs the current ~1,600.
