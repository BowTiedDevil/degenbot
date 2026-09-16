# Validated Environment Findings (2026-09-16, empirically probed)

Everything on this page was **measured, not assumed**. Use it to calibrate the
PRD's open questions and the acceptance criteria's timing budgets.

## The RPC is a live-tip reth mainnet node — this is REAL mainnet

| Probe | Result |
|---|---|
| `cast client` | `reth/v2.5.2-5a6940e/x86_64-unknown-linux-gnu` |
| `cast chain-id` | `1` |
| Block cadence | New block every ~12 s (observed 25991892 → 25991895 over ~26 s) |
| Tip timestamp | **== wall clock** (block 25991895 timestamped within seconds of `date -u`) |
| Base fee at probe time | ~0.64 gwei |
| WS endpoint | Works (`cast block-number --rpc-url $DEGENBOT_RPC_WS_CHAINID_1` returns tip) |
| `eth_simulateV1` | Method exists (returned params-error on malformed input, not method-not-found) |
| Endpoints | OS env `DEGENBOT_RPC_HTTP_CHAINID_1` / `DEGENBOT_RPC_WS_CHAINID_1` → `host.containers.internal:8545/8546` (devcontainer-provided) |

**Consequence: any transaction the agent broadcasts becomes a REAL mainnet
transaction with REAL gas cost and REAL public-mempool exposure.** The "local"
of the RPC is about endpoint hosting, not chain isolation. Previous wording in
the PRD/handoff that implied an anvil-style isolated fork was wrong; the docs
now treat the environment as real mainnet. The chain-id-1 constraint is
satisfied by construction (no anvil `--chain-id` problem).

## The funded key

- `bot.env` key derives to `0x5c603b8a137A40426E0dDFA981EC10c245AF080e`
  (derived locally with `cast wallet address`; key never printed).
- At probe time: balance `0`, nonce `0` — **unfunded**. Funding is an Observer
  launch-checklist item; any earlier signed tx fails with insufficient funds.
- Zero balance is also a clean baseline for the profit ledger (Phase 4 starts
  from an exact zero at `<OP>`).

## Deployment cost (this matters for the gas budget)

- `contracts/cmd_executor_bytecode.txt`: creation bytecode **16,277 bytes**.
- `cast estimate --create` (WETH + PoolManager constructor args): **3,595,884 gas**.
- At 0.64 gwei: ≈ **0.0023 ETH**. At a 20 gwei spike: ≈ 0.072 ETH — this can
  exceed a cavalier budget. Mitigation: deploy when base fee is low, or set an
  explicit `max_fee` cap via the deploy command.
- Per-arb `execute()` ≈ 70–130k gas (repo gas benchmarks) ≈ 0.00005–0.0001 ETH
  at 0.64 gwei.

## DB snapshot posture: warm, gap ≈ 12k blocks

- `~/.config/degenbot/degenbot.db` exists, 344 MB, well-populated
  (73k Uniswap V3 pools, 136k V4, 522k V2, 615k tokens).
- Max `liquidity_update_block`: V3 25,979,637 / V4 25,979,645 vs. tip
  25,991,895 → **gap ≈ 12,250 blocks (~40 hours of chain time)**.
- Observed behavior: the engine backfills the gap itself
  (`backfill_from_snapshot: fetching events {total_blocks=12255, chunk_size=2000}`)
  — no manual DB work needed. The agent does NOT need to bootstrap the DB.

## Measured boot timeline (dry-run, September 2026)

| t | Milestone (from `logs/bot_run.log`) |
|---|---|
| 0–15 s | Config load, RPC cascade resolve, WS subscribe confirmed |
| ~20 s | Backfill starts (12,255 blocks in 2,000-block chunks) |
| ~40 s | Fleet worker census posts; engine state approaches Backfilled |
| ~60 s | Path registration running (16k → 85k paths by 100 s) |
| ~100–120 s | 160k+ paths registered and still growing |
| ~3.5 min | Steady state: continuous per-block sim flow |

**Steady-state behavior observed**: every block, small candidate batches
(`1 candidates: 1 ok (0 profitable, 1 below threshold)`). The sim pipeline,
threshold gates, and renderers all function. **Zero net-of-gas profitable
candidates in the observed ~1 min window** — consistent with live mainnet:
organic arb spread rarely clears gas + competition.

## Implications for the exercise

1. **FR-5 (net profit) is the hard bar.** In a 90–180 min timebox, count on
   fingers of one hand whether a net-of-gas-profitable submission even
   *appears*, let alone wins inclusion against competing searchers. The
   "Pass (partial)" grade exists precisely for this; the Observer may also
   consider a longer soak or a permissive `DEGENBOT_MIN_PROFIT_MARGIN_BPS=0`
   posture (already the default).
2. **Boot budget**: acceptance R1 should allow ≥ 5 minutes from launch to
   steady-state sim flow.
3. **Reverts on inclusion still cost gas.** Racing searchers are the
   realistic loss mode; the gas budget cap in the stop conditions is
   load-bearing, not ceremonial.
4. **No fork-injection needed.** The earlier PRD worry ("static fork ⇒ no
   opportunities") evaporates: blocks and organic flow arrive on their own.
