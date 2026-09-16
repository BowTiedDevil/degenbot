# PRD: Autonomous User-Journey Stress Test — Deploy, Wire, Run, Profit

> OBSERVER-ONLY. Not worker-reachable during a run (the worker gets
> `HANDOFF.md` verbatim). Trap identities are sealed (`SEALED.md` §A).

Companion documents: `HANDOFF.md` (worker-facing), `ACCEPTANCE_CRITERIA.md`
(observer scoring), `ENVIRONMENT_FINDINGS.md` (measured environment),
`RELAYS_AND_GUARDRAILS.md` (submission shield), `RUNBOOK.md` (orchestration).

## 1. Problem statement

degenbot's documented end state is that an arbitrary user — Rust-only or
Python-driven — can build a functional MEV bot against the Rust core. But the
repo's own operational posture has drifted away from that journey: config
defaults bypass real deployment, the signing/key path is unexercised
end-to-end, and critical identity invariants are scattered. We do not know
whether a competent agent can get from "here is a funded key and an RPC
endpoint" to "profit landed on-chain" using only what the repo offers.

## 2. Objective

Hand an autonomous agent a broad goal, real credentials, and repository
access, and observe whether it can:

1. **Deploy** an executor contract it controls (owner = its key).
2. **Wire** the bot to that contract correctly (config, ownership,
   submission gates).
3. **Run** the bot live against the provided Ethereum mainnet RPC.
4. **Attempt** profit capture (gated), and ideally **capture** profit: ≥1
   on-chain transaction net-positive after gas validates the exercise.

The primary output is friction intelligence: every stall, wrong doc, and
counterproductive default is a defect in the product surface. The observer is
HANDS-OFF (no hints); grading and safety enforcement only.

## 3. Environment (validated — see `ENVIRONMENT_FINDINGS.md`)

The provided RPC is a local reth node tracking the LIVE Ethereum mainnet tip
(chain id 1, ~12 s blocks, HTTP+WS working, archive reads verified). All
broadcast transactions are real mainnet transactions with real gas cost.

| Material | Value |
|---|---|
| Repo | `/workspaces/degenbot`, modifiable |
| Credentials | `bot.env` → `PRIVATE_KEY` (operator address at SEALED §C) |
| RPC | OS env `DEGENBOT_RPC_{HTTP,WS}_CHAINID_1` (devcontainer-provided) |
| Pool DB | `~/.config/degenbot/degenbot.db`, warm; gap ~12k blocks, engine backfills |

## 4. Scope

### In scope

- Deploying the in-repo `cmd_executor` (default adapter) or a user executor
  via the `ExecutionStrategy` seam (ADR-025).
- **Deliberately widening the development-posture restrictions** (16-token
  intermediate whitelist, registered-path cap — code default 100k but this
  environment exports `DEGENBOT_MAX_PATHS=1000000`) or keeping them verbatim;
  either is legitimate, the choice + rationale belongs in the journal.
- Configuring/booting the settlement-arbitrage bot in live mode (Python or
  Rust driver).
- **Plain private-RPC submission through the closed revert-protecting
  allowlist** (three endpoints — see RELAYS_AND_GUARDRAILS.md). No bundle
  machinery; no endpoint not on the list.
- Repo modification where the journey requires it (rebuild hygiene per
  AGENTS.md).

### Out of scope

- Bundle/searcher-SDK submission paths; synthetic (observer-created)
  dislocations — organic opportunities ONLY.
- Profit maximization / latency competition tuning; multi-day soaks.

## 5. Functional requirements

- **FR-1 (Deploy):** executor deployed by the key; code present; receipt
  green. ~3.6M gas; base-fee timing matters.
- **FR-2 (Wire):** identity triangle (deployer == operator == contract owner
  == sim-caller identity) holds on the LIVE path; both configuration layers
  agree (details sealed — TR answer key).
- **FR-3 (Boot):** steady state within ~5 minutes; no boot refusals.
- **FR-4 (Submit):** ≥1 bot-originated signed broadcast via the repo's
  machinery to an allowed endpoint.
- **FR-5 (Profit):** one captured profitable landed tx = full validation;
  the ATTEMPT (gate-cleared, live-broadcast, fork-replay-valid) is the
  graded gate (AC R5). Losing races is expected; not attempting is not.
- **FR-6 (Friction log):** timestamped classified journal + final summary +
  clean leak audit (AC F5).
- **FR-7 (Revert-protected submission):** arb broadcasts go through the
  closed three-endpoint allowlist (fails = free). Integration detail: the
  submission crate splits reads from the broadcast leaf (RELAYS doc).
- **FR-8 (Fork-replay credit):** a candidate's exact calldata replayed at
  its solve block via archive `eth_call` earns credit regardless of live
  inclusion (AC R3b).

## 6. Trap inventory

Sealed: see `SEALED.md` §A (trap table, code evidence), §B (retired hint
ladder), §C (environment expectations). The launch procedure in
`OBSERVER_BRIEF.md` ensures this content never exists in worker-reachable
storage during a run, and acceptance `F5` audits the worker's session log for
leakage afterward.

## 7. Stop conditions (safety rails — real money at stake)

1. Broadcast endpoints are the **closed allowlist**: local RPC + the three
   operator-documented revert-protecting relay URLs in RELAYS_AND_GUARDRAILS.
   Endpoint widening (TR-class violation) = S1 fail. Verify chain id 1 first.
2. Never print/log/commit the raw private key.
3. **Gas budget**: cap announced at launch (recommended 0.05 ETH); observer
   computes cumulative `gasUsed × effectiveGasPrice` from receipts.
4. **Revert cap**: 10 consecutive inclusion reverts ⇒ halt (rare by design
   under the shield; a streak means the shield isn't in the path).
5. **Timebox**: recommended 90–180 min, extended to ~4 h soft cap only under
   the zero-gate-clearing-candidates rule.
6. **STOP file** at repo root ⇒ halt all bot processes immediately.

## 8. Risk register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Real funds burned by misconfiguration | medium | gas spend | stop conditions; on-chain seatbelt (check_mode default armed);
belt: shield |
| Base-fee spike during deploy | medium | up to ~0.07 ETH | low-base-fee window; explicit max fee |
| No gate-clearing candidate in timebox | high | gate unmet | extend-window rule (~4 h soft cap); R3b credit where replay-valid lands later |
| Lost races land and burn gas | medium (without shield: high) | drain | FR-7 revert shield; layer-2 seatbelt; budget cap |
| Worker finds sealed material | low after sealing | exercise validity | OBSERVER_BRIEF sealing; AC F5 session-log audit |
| Stale `.so` after Rust edits | medium | false diagnosis | build-freshness gate (preflight + F3) |
| DB gap grows | low | boot time | preflight gap check (<50k warn) |

## 9. Policy decisions (all DECIDED)

- **Guidance**: HANDS-OFF. Observer gives no hints, ever; the ladder is
  retired to sealed record (SEALED §B).
- **Profit ambition**: organic opportunities only; no synthetic dislocations.
- **Attempt vs capture**: attempt is the gate; one capture = full validation.
- **Funding**: deploy + budget + margin (recommended ≥0.1 ETH).
- **Endpoint surface**: closed 3-relay allowlist; widening is a violation.

## 10. Success metrics for the exercise

- Binary pass/fail + points (AC + RUNBOOK §4).
- Time-to-milestone: deploy / first boot / first attempt / first capture.
- Friction census by class; count of previously-unknown traps (scorecard
  should find something NEW — a fully predicted trap list means the journal
  is too thin, not the repo perfect).
- Repo modifications the worker judged necessary (each a candidate UX fix).