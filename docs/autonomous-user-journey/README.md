# Autonomous user-journey exercise package

Stress test: an arbitrary user deploys an executor contract, wires the repo's
bot to it, runs live on mainnet, and attempts profit capture — run by a
hands-off worker/observer agent pair.

## Visibility matrix (the load-bearing property of this package)

| File | Worker | Observer | Contains |
|---|---|---|---|
| `HANDOFF.md` | ✅ deliver verbatim | ref | goal, rules, deliverables, grading expectations, scope freedom |
| `ENVIRONMENT_FINDINGS.md` | ✅ | ✅ | measured environment facts (neutral) |
| `RELAYS_AND_GUARDRAILS.md` | ✅ | ✅ | endpoint allowlist + trust model + integration guidance |
| `PRD.md` | ❌ | ✅ | requirements, policy decisions, success metrics |
| `ACCEPTANCE_CRITERIA.md` | ❌ (grading spirit is summarized in HANDOFF) | ✅ | phases, points model, gate definitions |
| `RUNBOOK.md` | ❌ | ✅ | orchestration, launch prompt, scoring, teardown |
| `SEALED.md` | ❌❌ (encrypt/stash at spawn) | ✅ | trap inventory (answer key), retired hint ladder, expectations |
| `OBSERVER_BRIEF.md` | ❌ (deleted at spawn after sealing) | ✅ | sealing + hands-off procedure |

## Launch sequence

1. Launcher runs `scripts/user_journey_preflight.sh` (blocks until green).
2. Launcher spawns OBSERVER with `OBSERVER_BRIEF.md` + `SEALED.md` in-context.
   Observer immediately seals (encrypt or stash outside the project root,
   delete repo copies) per the brief.
3. Launcher spawns WORKER with the RUNBOOK §2 verbatim prompt + HANDOFF.md.
4. Observer watches, enforces kill criteria, extends the window per the
   scarcity rule, then grades + audits for leakage.

## Postmortem tripwire

`scripts/user_journey_leak_check.sh` greps worker-visible docs and can be run
any time; the closure check is the observer's audit of the worker session log
for sealed markers (see OBSERVER_BRIEF §"At scoring time").