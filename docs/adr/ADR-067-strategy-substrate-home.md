# ADR-067: The strategy substrate gets one home — ADR-061's placement reopens

**Status: accepted (decision); extraction is an implementation task sequenced after the pool-construction work.**

## Context

ADR-061 made pool-state provisioning a plane capability and placed the substrate —
pool ingress, the planning workspace, executor hop views, the connector index — in
`degenbot-bot::bot_core`, with strategies composing it (`StrategyKit`, per the
strategy-seams substrate map). The architecture review measured the cost of that
placement across the crate line:

- At review start: five `degenbot-strategy` files imported `degenbot_bot::bot_core`.
- After the frame un-split (pass B): **eight files, eighteen import statements**
  (`backrun.rs`, `backrun_driver/{driver_boot,tests}.rs`, `backrun_engine.rs`,
  `candidate_projection.rs`, `frame_pipeline.rs`, `market_context.rs`,
  `strategy_kit.rs`). The direction is growth: strategy-plane work pulls more
  substrate across the crate line, not less.

The substrate's own upstream neighbors set the pattern — pool identity lives in
`degenbot-pools`, path enumeration in `degenbot-pathfinding`; the substrate layer is
where shared mechanics belong. Holding it inside the arbitrage *application* crate
inverts the leverage: every `PoolIngress` signature change ripples into a sibling
crate's eight files, and a Rust-only strategy author reaches core composition
through the arb engine's internals.

## Decision

**D1 — ADR-061's capability claim stands; its placement half reopens.** State
provisioning remains a plane capability consumed by `StrategyKit`; the *files* move
to a home both consumers compose (a `degenbot-bot`-independent crate or module at
the substrate layer, beside pools/pathfinding).

**D2 — The extraction follows the pool-construction work.** The construction entry
(card 2) is reworking the same core-adjacent surface; sequencing avoids fabricating
a second rippling change.

**D3 — Acceptance is measured.** Post-extraction `degenbot-strategy` carries ZERO
`use degenbot_bot::bot_core` imports; the strategy seam map and GLOSSARY.md point at
the new home; `degenbot-bot` composes it as a peer, and the pure-Rust consumer path
(`just check-rust-consumer`) proves a strategy can be authored without the
application crate.

## Consequences

- `StrategyKit`'s provision cell keeps its interface; only its join address changes.
- ADR-061 remains the record of the capability decision; this ADR owns the placement.
- If the extraction measures WORSE (e.g. the substrate proves entangled with
  `BotState` pointers), the evidence returns here and the placement stands — this
  ADR then records why.