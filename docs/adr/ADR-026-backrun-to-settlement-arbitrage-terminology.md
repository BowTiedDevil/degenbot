# ADR-026: Retire the "backrun" label — "settlement arbitrage" (strategy) and "arbitrage" (umbrella layer)

**Status: accepted.** Supersedes the "backrun" naming used by ADR-019, ADR-025,
and the `degenbot-backrun-strategy` crate / `eth_backrun_*` example for all
*forward-looking* writing. It does **not** rewrite those historical records —
they keep "backrun" as it read at the time; this ADR is the canonical reference
the codebase moves toward (tracked by ergo epic `R2BXPV`).

## Context

The repository's strategy has been named "backrun" since ADR-019 (the crate
`degenbot-backrun-strategy`, the example `examples/eth_backrun_v2_v3_v4_rust.py`,
and the ADR-019/025 bodies). That label is **architecturally misleading** and
kept requiring re-explanation across sessions:

- **"Backrun" (classic MEV)** means positioning a transaction/bundle immediately
  *after a specific, identified victim transaction* (mempool-ordered), profiting
  from that victim's price impact — a *named victim* and ordering relative to it
  are essential.
- **This bot's strategy** reads the *settled* block's resulting pool state,
  solves cross-pool price discrepancies (V2/V3/V4 paths) with **no labeled
  victim**, and dispatches a single transaction at the head of the *next* block.
  It has no victim-tx targeting and no tx-to-tx ordering.

Because "backrun" is an overloaded MEV term that does not describe this
mechanism, every session's implicit reading had to be corrected by hand. The
decision below gives the codebase a durable, unambiguous vocabulary.

A secondary driver: reviewers repeatedly had to decode the abbreviation "arb".
"arbitrage" is a real, look-up-able word; the repo standardizes on the verbose,
unabbreviated form.

## Decision

### D1 — Canonical terms (supersede "backrun" everywhere forward-looking)

- **"settlement arbitrage"** — the bot's *current strategy*: arbitrage the
  cross-pool price discrepancies a settled block leaves in pool state, executing
  one transaction at the next-block head. The defining properties are (1) the
  opportunity source is a settled **pool-state discrepancy** (not a victim's
  flow) and (2) execution is a single tx at the **next-block head** (not ordered
  against a specific tx).
- **"arbitrage"** — the *umbrella strategy layer*. Users may run any arbitrage
  strategy (liquidation, sandwich, frontrun, backrun, multi-market, statistical,
  CEX/DEX, …); the layer is deliberately neutral (Q6).
- **No "arb" abbreviation** anywhere — "arbitrage" spelled out.

### D2 — Identifier mapping (crunch/epic `R2BXPV`)

| Layer | Old | New |
|---|---|---|
| crate | `degenbot-backrun-strategy` | `degenbot-arbitrage` |
| logger | `degenbot_backrun_strategy` | `degenbot_arbitrage` |
| example | `examples/eth_backrun_v2_v3_v4_rust.py` | `examples/eth_settlement_arbitrage_v2_v3_v4_rust.py` |
| public CLI fn | `build_backrun_arg_parser` | `build_arbitrage_arg_parser` |
| internal fn | `_make_backrun_config` | `_make_arbitrage_config` |
| tests | `test_backrun_*` / `test_eth_backrun_*` | `test_arbitrage_*` / `test_eth_arbitrage_*` |
| CLI/config prose | "backrun chain/session" | "arbitrage chain/session" |
| mechanism prose | "backrun bot/strategy" | "settlement arbitrage" / "arbitrage" |

- Consistent naming **across all layers** so the decision is durable.
- **No backwards-compat stub/alias** for any renamed symbol (internal features;
  no public callers — AGENTS.md standalone-feature rule).

### D3 — History is frozen (this ADR supersedes, it does not rewrite)

- ADR-019, ADR-025 titles + bodies keep "backrun" as they read historically.
- `docs/investigations/*`, `docs/plans/*`, `docs/results/*`,
  `docs/migration-guides/*`, and other dated extraction records keep the label.
- Future work uses the canonical terms via this ADR.

## Consequences

- **Forward-looking code and living docs** (README, CONTEXT.md, living
  `docs/architecture/*`) move to the canonical terms; done under ergo epic
  `R2BXPV`, staged non-breaking prose first (B1–B3), then breaking identifier
  renames (A1–A5).
- **Historical records** intentionally retain the old label and become the
  proof of what was named at the time; a stale identifier reference in a frozen
  record to a renamed crate/symbol is expected and not corrected.
- Search tooling/`rg` are the source of truth for any residual hit; the final
  audit gate (task A5) asserts the only remaining "backrun" hits are frozen.
