# Executor facts-driven Plan walker — spike spec (`v3_v4_v3`)

> ADR-031 · epic `62V6Q5`. Status: **superseded by A3** — the spike's prototype was generalized (A2) and made the sole producer (A3): the 30 `build_*_plan` bodies and the `build_for`/`AxisSupport` rows are deleted, and `family_axis_support` is facts-derived. The spike spec below records the A1 proof; see ADR-031 for the accepted end state.

## Why this spike

ADR-031 deepens the executor by replacing the 30 hand-written per-family `build_*_plan` producers (a ~7,000-line adversarial surface) with **per-protocol hop facts** (data) + **per-protocol mechanics** (code) + **one enclosure-deriving walker**. The single risk was whether the facts schema could express the *worst* enclosure — `v3_v4_v3`'s 3-level `V3c → V3a → V4_UNLOCK` nesting. This spike proves that on that family, byte-identical to the hand-authored producer, before generalizing (A2).

## What was built (behind `--features walk`)

`rust/crates/degenbot-executor/src/grammar_walker.rs`:

- **`HopFacts`** — the per-protocol declarative data half (ADR-031 D4). Fields: `prot`, `zfo` (direction), `swap_fee`, `tick_spacing`, `out_currency`, `in_currency`, `out_dest` (Executor / PoolManager / take-to-pool-repay), `repay` (`SelfRefund` / `Offstream` / `NetZero`).
- **`mod mechanics`** — the per-protocol *code* half: `v3_flash` builds a V3 `FlashSwap` from facts (`out_dest` picks the recipient routing). A2 adds the V4/V2 mechanics.
- **`build_v3v4v3_walk`** — the walker. Reads the three hops' facts + solver amounts, derives the enclosure from the `Repay`/`OutDest` facts, and emits a single `Plan` (the one representation: `plan_to_bytes` + `LedgerValidator` are untouched pure functions of it).
- **`derive_v3v4v3_walk`** — the tri-state public entry (ADR-030): build → `LedgerValidator` gate → bytes.

## The enclosure-derivation rule (the novel core)

For `v3_v4_v3` (leading V3, V4 middle, terminal V3), the walker derives the nesting from the facts rather than hardcoding indices:

1. **`SelfFund` funding** — entry capital (a funding axis value).
2. **`V4Sync`** — the leading V3's `out_dest = PoolManager` seeds the PM ledger the V4 unlock reads.
3. The **`Offstream`** hop (terminal V3 `c`) is the **outermost** `FlashSwap` (repaid by a downstream take).
4. The **`SelfRefund`** hop (leading V3 `a`) is **inner**; its callback runs the WETH self-refund `Erc20Transfer` then the `V4Unlock`.
5. The **`NetZero`** V4 (`b`) lives inside the unlock: `V4Settle` (the synced forward) → `V4Swap` → `V4TakeCompact` to `c` (repaying `c`'s borrow) → `V4SettleAll` (PM-net-zero).

This is D3's "enclosure is derived, not chosen": the author states *facts* (where each hop's output goes, how it's repaid); the walker states the nesting.

## Evidence (the spike's gate)

- **Byte-identity**: `derive_v3v4v3_walk(...)`'s bytes equal `build_v3v4v3_plan`'s bytes on representative amount sets (`walker_matches_hand_authored_v3v4v3`). Byte-identity ⇒ execution-identity: both share `plan_to_bytes`, and the revm matrix already runs `build_v3v4v3_plan` GREEN.
- **Validator gate**: the walked Plan passes `LedgerValidator::validate_full`.
- **Default unchanged**: `cargo test -p degenbot-executor` (no `walk`) — 109 lib + all integration suites green; `grammar_walker` is `#[cfg(feature = "walk")]`, absent from default builds.
- **Lint**: `cargo clippy -p degenbot-executor --features walk --all-targets` clean.

## The facts schema (frozen for A2)

```rust
struct HopFacts {
    prot: Prot,          // V2 | V3 | V4 | ...
    zfo: bool,           // swap direction
    swap_fee: u16,
    tick_spacing: i16,   // V4 only
    out_currency: Address,
    in_currency: Address,
    out_dest: OutDest,   // Executor | PoolManager | Repay(pool)
    repay: Repay,        // SelfRefund | Offstream | NetZero
}
```

A2 generalizes by: (1) adding the V2/V4 mechanics modules (the swap/`unlock` step constructors), (2) extending `OutDest`/`Repay` to any new coupling a family exercises (native bridging, V2 pre-fund, capture terminals), (3) widening the walker to walk an arbitrary hop sequence reading each hop's facts, and (4) proving the **full 25-family revm contract-matrix** (not byte-parity against the suspect builders) before hard cutover (A3).

## Non-goals (checked)

No other family, no default-build output change, no DEX rollout. The two real latent defects B surfaced (the losing InPathFlash stream; `v4_v4_v4` no-slack configs) are the *motivation*: A2/A3 make them unrepresentable at the schema level.