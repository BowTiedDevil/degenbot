## Result: FIRST crates.io PUBLISH — 22/22 LIVE at 0.6.0-alpha.5

**Date:** 2026-08-19/20 (UTC). **All 22 publishable workspace members live**, verified via the registry API (`cargo search` per name) + crates.io index.

### What shipped
- 22 crates @ 0.6.0-alpha.5: core, decoders, order-index, pathfinding, abi, fork, math, db, executor, uniswap, pools, rpc, aave, pool-updater, price, solvers, submission, bot, execution, simulation, arbitrage, and the umbrella `degenbot`.
- Published from clean tree; every publish passed the verified dry-run (no --no-verify anywhere).
- Token: local file `.crates-io-token` (gitignored in 6fdeeb1bd; chmod 600). CI uses the `CRATES_IO_TOKEN` repo secret (user action).

### Consumer smoke (pure-Rust pillar, proven from the registry)
```
cargo new /tmp/db-consumer-smoke --lib
cd /tmp/db-consumer-smoke && cargo add "degenbot@0.6.0-alpha.5" && cargo build
```
Full 22-crate dep tree resolved from crates.io and compiled (66 degenbot artifacts, ~47s, exit 0). `cargo search degenbot --limit 30` shows all 22 with descriptions.

### Hard-won mechanics (for the next release + CI) — see also ADR-035 follow-up
1. **cargo publish is self-sufficient for ordering + waits.** Modern cargo (1.97.1) implements a `PublishPlan`: `cargo publish --workspace --allow-dirty --exclude <live>` publishes in dependency order and *polls the registry itself* until each crate (and each local dependency) is available before proceeding ("waiting for X to be available at crates-io. N remaining crates to be published"). The `Published ... at registry` line = cargo-confirmed index availability. No hand-rolled curl index-polling is needed (my earlier loop's raw 404 probes were a side channel with no semantics — never used as a gate again).
2. **New-crate rate limit (crates.io source `src/rate_limiter.rs`, token bucket per uploader):** PublishNew = 1 per 10 minutes sustained, burst 5. PublishUpdate (new version of existing crate) = 1/min, burst 30. YankUnyank = 1/min, burst 100. Also a daily cap on new versions (MAX_NEW_VERSIONS_DAILY). **Consequence:** a first release of N brand-new names takes ~N×10 min minimum; the 429 body includes the exact retry-after timestamp. Version-bump releases of these 22 crates are fast (30 burst + 1/min, no 429 expected).
3. **Proven release recipe (used to finish this release):** loop per round: (a) audit live set via `cargo search <name>` (registry API, authoritative), (b) `cargo publish --workspace --allow-dirty --exclude <each live one>`, (c) on 429, parse `try again after <DATE>` from the error, sleep until then + 120s, repeat.
4. Account gate: crates.io requires a **verified email** before any publish (400 on first attempt).

### Failure/timeline log (for the record)
- /tmp/publish.log (run 1: core; 400 verified-email), /tmp/publish2.log (decoders "already exists" — run 1 cancel race), /tmp/publish3.log (order-index, pathfinding, abi, fork; killed mid-wait), /tmp/publish4.log (math; user kill), /tmp/publish_ws.log (db, executor, uniswap; 429), /tmp/publish_ws2.log (pools; 429), /tmp/ws3.log ×3 (rpc, aave, pool-updater, price, solvers, submission, bot, execution, simulation, arbitrage, degenbot; each round landing 1-2 on the 10-min mark). Total new-crate tax paid: ~2.5h of registry-side waits.

### Follow-ups (new tasks, not this one)
- CI workflow `.github/workflows/publish-to-crates-io.yml` still contains the old hand-rolled 22×dry-run/22×publish loop — replace with the `--workspace` + retry-after recipe above (needed the first time a *new* crate joins the workspace; version-bump releases would work either way).
- Consider adding the retry-after loop as a justfile recipe (e.g. `just publish-crates`) so humans and CI share the runbook.
