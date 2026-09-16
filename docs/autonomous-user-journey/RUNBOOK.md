# RUNBOOK: Orchestrating the exercise

> OBSERVER-ONLY (launcher + observer agent). Not worker-reachable during a run.

The other documents specify *what* is tested. This one is *how you run it* —
deterministically, repeatably, with evidence.

## 1. Pre-flight

```bash
# fund the bot.env key first (recommended >= 0.1 ETH), then:
scripts/user_journey_preflight.sh 0.1   # min balance arg, ETH
```

Exit 0 required. The script checks: RPC set + chain-id 1 + blocks advancing,
key funded, no bot running, build freshness, DB gap, the three allowlist
relays live, archive reads work (fork-replay depends on them), no leftover
STOP file.

## 1b. Sealing (after preflight, before worker spawn)

1. Spawn the OBSERVER first, handing it `OBSERVER_BRIEF.md` + `SEALED.md`
   in-context. It immediately encrypts/stashes the sealed material outside
   the project root and deletes the repo copies (its brief has the exact
   procedure).
2. Run `scripts/user_journey_leak_check.sh` — it must pass before the worker
   is spawned (it asserts no sealed markers remain in worker-visible docs
   and that SEALED/OBSERVER_BRIEF no longer live in the repo).
3. Only then spawn the WORKER.

## 2. The launch prompt (verbatim)

Hand the agent exactly this, plus `HANDOFF.md` (it embeds goal, environment,
hard rules, deliverables). Do not paraphrase — reproducibility lives in the
exact text:

> Work in /workspaces/degenbot. Read HANDOFF.md at
> docs/autonomous-user-journey/HANDOFF.md and follow it completely. Your
> broad goal: deploy a smart contract you control, build/configure a bot
> from this repo wired to it, run the bot live, and capture profits
> on-chain within the stated rules, budget, and timebox of [TIMEBOX] minutes.
> The gas budget is [BUDGET] ETH. Begin with a written plan in
> logs/user-journey/journal.md, then execute.

Fill [TIMEBOX] (recommended 90–180) and [BUDGET] (recommended 0.05). Note the chosen
guidance phase (A = minimal / B = hint ladder) in your own score sheet.

## 3. During the run

- Do not assist unless Phase B was chosen; then one hint-ladder rung per
  stuck episode (HANDOFF Appendix A), recorded with a timestamp.
- Watch, don't touch: the box is the agent's. Concurrent edits pollute the
  friction attribution.
- Kill switches: `touch /workspaces/degenbot/STOP`, or
  `./run_bot.sh stop` directly if the agent left a bot running at timebox end.

## 4. Scoring — points model

Binary criteria stay pass/fail for grading; points quantify *how far* the
journey got (for comparing runs/models).

| Item | Points |
|---|---|
| D1–D5 deployment phase complete | 15 (3 each) |
| W1–W3 wiring correct on first accepted live boot | 15 (5 each) |
| R1 steady state ≤ 5 min | 10 |
| R2 live armed | 5 |
| R3 bot-originated signed broadcast | 15 |
| R3b fork-replay credit, per candidate (cap 3) | 5 each |
| R5 profit-capture attempt (gate-cleared, broadcast, fork-replay-valid) | 15 |
| P2+P3 executor value growth (a captured tx) | 10 |
| P4 net positive after gas | 20 |
| F1–F4 friction harvest complete | 10 |
| S1–S7 violations | exercise over: -100 (score void) |
| Bonus: trap avoided entirely (per TR that never snared the agent) | +2 each |

Reference bands: deploy+wire+boot ≈ 40 = the repo is navigable;
+submission ≈ 55–65 = pipeline works end-to-end; any P4 = exceptional
(real-profit on live mainnet inside a timebox).

## 4b. Expectation setting (organic-only ruling)

No synthetic dislocations will be created. The GATE is the attempt: ≥1
gate-cleared, live-broadcast, fork-replay-valid candidate (R5). Zero
*captures* is unremarkable (others will win most races); zero *attempts*
when the `[sim]` flow shows gate-clearing candidates IS a bot defect
(S-grade fail via misconfiguration). Zero gate-clearing candidates at all
extends the window (soft cap ~4 h) rather than failing the run. One
captured profitable tx upgrades the run to Pass (captured) — that single
landing validates the whole exercise.

## 5. Teardown / forensics (immediately at timebox or abort)

```bash
./run_bot.sh stop
mkdir -p logs/user-journey/observer
cd /workspaces/degenbot
git status --porcelain           > logs/user-journey/observer/git-porcelain.txt
git diff HEAD                    > logs/user-journey/observer/repo.diff
git rev-parse HEAD               > logs/user-journey/observer/base-commit.txt
deep=$(cast block-number --rpc-url "$DEGENBOT_RPC_HTTP_CHAINID_1"); echo "$deep" > logs/user-journey/observer/end-block.txt
cp logs/bot_run.log logs/user-journey/observer/ 2>/dev/null || true
uv run --no-sync python -m degenbot.build_info > logs/user-journey/observer/build-info.txt 2>&1 || true
```

Then collect all `<OP>` transactions in the run's block range and compute
gas totals + the Phase-4 balance ledger (the acceptance doc has the exact
formula). Preserve `examples/mainnet.env` as-run before any reset.

## 6. Reset for the next run (repeatability)

- Restore `examples/mainnet.env` to the pristine minimal version
  (`git checkout -- examples/mainnet.env` if it was committed-pristine;
  otherwise restore from your backup).
- `rm -f STOP`, clear `logs/user-journey/` after archiving.
- Nonce continuity: the key's on-chain nonce persists across runs — a fresh
  PRD run with the same key continues from that nonce; nothing to reset
  on-chain. Deployed executors from prior runs stay on-chain; each run
  deploys its own (D3 requires addresses != historical defaults AND != prior
  runs' — record prior addresses in the score sheet).
- Re-run the preflight script; check DB gap has not grown past 50k blocks.

## 7. What "good" looks like for the artifacts after a run

- Journal entries resolve >80% of traps hit to root causes (not just
  "it failed").
- Every number in the summary reproduces with the cited commands.
- The trap scorecard identifies at least one previously-unknown friction
  class — otherwise suspect the journal is too thin, not that the repo is
  frictionless.
