# Backrun driver runbook (hosted, one process)

The backrun arm runs as the hosted `BackrunDriver` on the process
`StrategyHost` — the SAME `run_bot.sh` process as the settlement arm when both
facets are active (ADR-057). The standalone two-binary deployment is retired:
the pending-transaction arm is always a hosted driver now.

## 1. Activation (observe-only first — always)

```bash
# Stamp the pinned MEVBlocker searcher WS into the facet and activate it.
rust/target/debug/degenbot strategy activate backrun --endpoints-default

# Boot the hosted process (this is the settlement launcher too).
source bot.env
OPERATOR_PRIVATE_KEY="$PRIVATE_KEY" ./run_bot.sh foreground
# Expect: observe/drop lines per frame; zero "BID dispatched" lines.
```

No `bid_mode` => observe-only, never bids. The readiness gate refuses a live
boot with an activated-but-unsettled facet before any feed connection or
signing material loads.

## 2. Budget lines (bid mode)

Every knob is a typed `strategy.backrun` key: set it in the config.toml
`[strategy.backrun]` table (or its `DEGENBOT_STRATEGY_BACKRUN_*` env name). The
node/DB resolvers stay env-only (`DEGENBOT_RPC_HTTP_CHAINID_1`,
`DEGENBOT_RPC_WS_CHAINID_1`, `DEGENBOT_DB_PATH`).

| Typed key | Meaning | Hard behavior |
| --- | --- |
| `strategy.backrun.bid_mode` | Explicit bid flag | Off => `observe_only`, never bids |
| `strategy.backrun.budget_wei` | Cumulative cap (wei) | Zero => bid mode illegal; spent >= cap => `budget_exhausted` |
| `strategy.backrun.max_bundle_wei` | Per-bundle cap (wei) | Bid clamped to cap |
| `strategy.backrun.key_file` | Hex secp256k1 key path | Key never leaves `TxSigner` |
| `strategy.backrun.mevblocker_url` | Private-broadcast RPC | Set engages under the Public fan-out: the signed backrun is broadcast raw to this endpoint first, with the chain node as the public fallback relay (private-first). Inert under the Bundle (auction) arm, whose economics are unchanged |
| `strategy.backrun.dry_run` | Sign-nothing dispatch | All candidates skip as `DryRun` |
| `strategy.backrun.bribe_bips` | Bribe ceiling (bips, default 9800) | The wallet gate may compose LOWER bips |
| `strategy.backrun.bundle_gas_est` | Bundle gas estimate (default 300000) | Prices the net-of-gas bid gate |

Wallet economics (live defect, receipts 0xd41a1c35 / 0x8603039d): the
wallet funds ONLY the bundle's gas — the on-chain bribe is drawn from
flash proceeds (the executor config pays `bribe_bips` of the true
profit delta to `block.coinbase`) and the residue parks in executor
custody. A bid exists only when the solved gross profit covers the gas
burn plus 5%; the bribe then takes the surplus (capped by
`strategy.backrun.bribe_bips` and `strategy.backrun.max_bundle_wei`), and
the budget's `spent` accumulator tracks the wallet's gas burn, not the
bribe.

```toml
# $XDG_CONFIG_HOME/degenbot/config.toml (else $HOME/.config/degenbot/config.toml)
[strategy.backrun]
active = true
endpoints = "wss://searchers.mevblocker.io"   # stamped by --endpoints-default
bid_mode = true
budget_wei = "1000000000000000"   # wei values are quoted TOML strings
max_bundle_wei = "500000000000000"
priority_fee_gwei = 2
key_file = "/home/dev/.degenbot/operator.key"
```

## 3. STOP semantics

- Kill switch: `touch /tmp/degenbot-sidecar-STOP` (the `strategy.backrun.stop_file`
  knob's default path — historical name kept so operator muscle-memory and the
  key's documented default stay stable).
- In-loop: the decision layer drops every candidate (`kill_switch`), the feed
  drains, then the loop halts. Hosted: the driver's exit folds to a terminal
  FSM tombstone (`Stopped` on a clean stop, `Halted` on self-halt; no
  auto-restart), and the host keeps settling the other arms.
- Remove the file and restart the process to resume (the budget is not
  persisted and restarts at zero — keep the shell history for accounting;
  quarantine-parked frames ARE persisted, see section 6).

## 4. Landed-bundle forensics

For every `Bid dispatched` line record (append to `logs/backrun/bids.jsonl`):

```json
{"ts_unix_ms": 0, "tx_hash": "0x..", "target": "0x..", "bid_wei": "0",
 "submitted": 1, "skipped": 0, "block": 0}
```

After the block lands (`eth_getTransactionReceipt` via the driver's receipt
probe), append the landing record:

```json
{"tx_hash": "0x..", "block": 0, "status": 1, "gas_used": 0,
 "coinbase_delta_wei": "0", "net_wei": "0"}
```

- `coinbase_delta_wei`: the executor's sweeps land as the coinbase bribe
  (`block.coinbase` delta = sweep returnData, verified by the sim oracle).
- `net_wei` = `bid_wei` budget line minus the actual bribe the law of the
  returned bundle enforced.
- Cadence review (reference the 30-min soak baseline): landed/not ratio,
  median block-latency from `received_unix_ms`, and budget burn per hour.
  Two consecutive unlanded bids with a consumed budget line => STOP and
  inspect before re-arming.

## 5. Cancel/replacement hygiene

- Nonces come from the process `NonceAuthority`: `NonceLane::lease` at sign
  time, lowest-free above the chain nonce. The hosted head feed reconciles
  per head and delivers typed outcomes (`Landed`/`Stale`/`Orphaned`) to the
  owning strategy only.
- There is NO replacement ladder in the driver: one bid per decoded frame.
  A price-improved target simply reappears as a new frame; the target's own
  receipt probe on the bid path is what keeps a mined target from dispatching.

## 6. Run artifacts (driver namespace)

A hosted driver scopes its artifacts under the host state root:
`<state_root>/backrun/{session,quarantine}/`, so two strategies never collide.
The root is the typed `logging.runs_dir` key (default the XDG state home plus
`degenbot/logs`; a leading `~` expands against `HOME`).

| Typed key | Meaning |
| --- | --- |
| `logging.log_stderr` | Also mirror the session log to stderr (interactive runs). File-only otherwise. |
| `logging.trace_jsonl` | Explicit capture path. When set it wins; when absent the trace helpers append to the session's `trace.jsonl`. |

There is deliberately **no rotation, compression, or size cap**: run artifacts
are the forensics surface, and cleanup is the operator's (or an out-of-process
job's) call.
