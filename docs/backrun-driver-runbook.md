# Backrun driver runbook (hosted, one process)

The backrun reaction runs as two per-ecosystem hosted drivers on the process
`StrategyHost` — the SAME `run_bot.sh` process as the settlement arm when any
facet is active (ADR-057). The standalone two-binary deployment is retired.

- `mevblocker_backrun`: the MEVBlocker-ecosystem composition. Its submission
  slot anchors an `eth_sendBundle` auction on the MEVBlocker searcher
  WebSocket and leads the raw-broadcast fan-out with the private endpoint.
- `txpool_backrun`: the public-mempool composition. Its submission slot fans the
  signed bytes over the public relay allowlist with the read provider as the
  fallback relay.

Both share the same reaction machinery (feed, anchored discovery, decide gate,
sim, dispatch); they differ only in their config facet and submission slot.
They are independently activatable and MAY run together in one process.

## 1. Activation (observe-only first — always)

```bash
# Stamp the pinned MEVBlocker searcher WS into the facet and activate it.
rust/target/debug/degenbot strategy activate mevblocker_backrun --endpoints-default

# Or activate the public-mempool composition instead (pinned relay allowlist).
rust/target/debug/degenbot strategy activate txpool_backrun --endpoints-default

# Boot the hosted process (this is the settlement launcher too).
source bot.env
OPERATOR_PRIVATE_KEY="$PRIVATE_KEY" ./run_bot.sh foreground
# Expect: observe/drop lines per frame; zero "BID dispatched" lines.
```

No `bid_mode` => observe-only, never bids. The readiness gate refuses a live
boot with an activated-but-unsettled facet before any feed connection or
signing material loads. The mevblocker facet's bid mode additionally requires
`key_file` and `mevblocker_url` (the private endpoint); the peer facet submits
publicly and has no private URL.

## 2. Budget lines (bid mode)

Every knob is a typed per-ecosystem key: set it in the config.toml
`[strategy.mevblocker_backrun]` / `[strategy.txpool_backrun]` table (or its
`DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_*` / `DEGENBOT_STRATEGY_PEER_BACKRUN_*`
env name). The node/DB resolvers stay env-only (`DEGENBOT_RPC_HTTP_CHAINID_1`,
`DEGENBOT_RPC_WS_CHAINID_1`, `DEGENBOT_DB_PATH`).

| Typed key | Meaning | Hard behavior |
| --- | --- | --- |
| `<facet>.bid_mode` | Explicit bid flag | Off => `observe_only`, never bids |
| `<facet>.budget_wei` | Cumulative cap (wei) | Zero => bid mode illegal; spent >= cap => `budget_exhausted` |
| `<facet>.max_bundle_wei` | Per-submission cap (wei) | Bid clamped to cap |
| `<facet>.key_file` | Hex secp256k1 key path | Key never leaves `TxSigner` |
| `mevblocker_backrun.mevblocker_url` | Private-broadcast RPC | Required for mevblocker bid mode; the signed backrun is broadcast raw to this endpoint first, with the chain node as the public fallback relay (private-first) |
| `<facet>.dry_run` | Sign-nothing dispatch | All candidates skip as `DryRun` |
| `<facet>.bribe_bips` | Bribe ceiling (bips, default 9800) | The wallet gate may compose LOWER bips |
| `<facet>.bundle_gas_est` | Bundle gas estimate (default 300000) | Prices the net-of-gas bid gate |
| `<facet>.endpoints` | Submission endpoint set | mevblocker: the searcher WS; peer: the public relay fan-out |

Wallet economics (live defect, receipts 0xd41a1c35 / 0x8603039d): the
wallet funds ONLY the bundle's gas — the on-chain bribe is drawn from
flash proceeds (the executor config pays `bribe_bips` of the true
profit delta to `block.coinbase`) and the residue parks in executor
custody. A bid exists only when the solved gross profit covers the gas
burn plus 5%; the bribe then takes the surplus (capped by the facet's
`bribe_bips` and `max_bundle_wei`), and the budget's `spent` accumulator
tracks the wallet's gas burn, not the bribe.

```toml
# $XDG_CONFIG_HOME/degenbot/config.toml (else $HOME/.config/degenbot/config.toml)
[strategy.mevblocker_backrun]
active = true
endpoints = "wss://searchers.mevblocker.io"   # stamped by --endpoints-default
bid_mode = true
budget_wei = "1000000000000000"   # wei values are quoted TOML strings
max_bundle_wei = "500000000000000"
priority_fee_gwei = 2
key_file = "/home/dev/.degenbot/operator.key"
mevblocker_url = "https://rpc.mevblocker.io/fast"
```

## 3. STOP semantics

- Kill switch: `touch /tmp/degenbot-sidecar-STOP` (the facet's `stop_file`
  knob's default path — historical name kept so operator muscle-memory and the
  key's documented default stay stable).
- In-loop: the decision layer drops every candidate (`kill_switch`), the feed
  drains, then the loop halts. Hosted: each driver's exit folds to a terminal
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
  time, lowest-free above the chain nonce. Each strategy owns its own lane;
  the hosted head feed reconciles per head and delivers typed outcomes
  (`Landed`/`Stale`/`Orphaned`) to the owning strategy only.
- There is NO replacement ladder in the driver: one bid per decoded frame.
  A price-improved target simply reappears as a new frame; the target's own
  receipt probe on the bid path is what keeps a mined target from dispatching.

## 6. Run artifacts (driver namespace)

A hosted driver scopes its artifacts under the host state root:
`<state_root>/<strategy>/{session,quarantine}/`, so two strategies never
collide. The root is the typed `logging.runs_dir` key (default the XDG state
home plus `degenbot/logs`; a leading `~` expands against `HOME`).

| Typed key | Meaning |
| --- | --- |
| `logging.log_stderr` | Also mirror the session log to stderr (interactive runs). File-only otherwise. |
| `logging.trace_jsonl` | Explicit capture path. When set it wins; when absent the trace helpers append to the session's `trace.jsonl`. |

There is deliberately **no rotation, compression, or size cap**: run artifacts
are the forensics surface, and cleanup is the operator's (or an out-of-process
job's) call.
