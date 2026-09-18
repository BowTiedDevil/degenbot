# Sidecar live-bid runbook (epic 6ZOGIT, task FVIWDT)

The standalone backrun sidecar (`rust/crates/degenbot-submission/src/bin/backrun_sidecar.rs`)
is FORK-1: a separate binary with zero touches to the live engine block pump.
This runbook covers REAL bids: budget lines, cancel hygiene, forensics, and
STOP parity with the observer lineage (`observe_soak.py`).

## 1. Preflight (observe-only first -- always)

```bash
# Node + feed sanity, observe mode (no key file => no signing material loaded)
SIDECAR_RPC_URL="$DEGENBOT_RPC_HTTP_CHAINID_1" \
  cargo run -p degenbot-submission --bin backrun_sidecar
# Expect: observe/drop lines per frame; zero "BID dispatched" lines.
```

Gate the exact-sim oracle before any live bid attempt:

```bash
cargo test -p degenbot-rpc --test eth_simulate_v1_probe -- --ignored
cargo test -p degenbot-submission --test sidecar_solve_frame -- --ignored
```

## 2. Budget lines (bid mode)

| Env | Meaning | Hard behavior |
| --- | --- | --- |
| `SIDECAR_BID_MODE=1` | Explicit bid flag | Off => `observe_only`, never bids |
| `SIDECAR_BUDGET_WEI` | Cumulative cap (wei) | Zero => bid mode illegal; spent >= cap => `budget_exhausted` |
| `SIDECAR_MAX_BUNDLE_WEI` | Per-bundle cap (wei) | Bid clamped to cap |
| `SIDECAR_KEY_FILE` | Hex secp256k1 key path | Key never leaves `TxSigner` |
| `SIDECAR_MEVBLOCKER_URL` | Private-broadcast RPC | Adds a provider to `extra_broadcast` |
| `SIDECAR_DRY_RUN=1` | Sign-nothing dispatch | All candidates skip as `DryRun` |
| `SIDECAR_BRIBE_BIPS` | Bribe ceiling (bips, default 9800) | The wallet gate may compose LOWER bips |
| `SIDECAR_BUNDLE_GAS_EST` | Bundle gas estimate (default 300000) | Prices the net-of-gas bid gate |

Wallet economics (live defect, receipts 0xd41a1c35 / 0x8603039d): the
wallet funds ONLY the bundle's gas — the on-chain bribe is drawn from
flash proceeds (the executor config pays `bribe_bips` of the true
profit delta to `block.coinbase`) and the residue parks in executor
custody. A bid exists only when the solved gross profit covers the gas
burn plus 5%; the bribe then takes the surplus (capped by
`SIDECAR_BRIBE_BIPS` and `SIDECAR_MAX_BUNDLE_WEI`), and the budget's
`spent` accumulator tracks the wallet's gas burn, not the bribe.

```bash
SIDECAR_RPC_URL="$DEGENBOT_RPC_HTTP_CHAINID_1" \
SIDECAR_BID_MODE=1 SIDECAR_BUDGET_WEI=1000000000000000 \
SIDECAR_MAX_BUNDLE_WEI=500000000000000 \
SIDECAR_KEY_FILE="$HOME/.degenbot/operator.key" \
SIDECAR_MEVBLOCKER_URL="https://rpc.mevblocker.io/noreverts" \
SIDECAR_PRIORITY_FEE_GWEI=2 \
  cargo run -p degenbot-submission --bin backrun_sidecar
```

## 3. STOP semantics (parity with the soak observer)

- Kill switch: `touch /tmp/degenbot-sidecar-STOP`.
- In-loop: the decision layer drops every candidate (`kill_switch`), the feed
  drains, then the loop halts and the feed pump is stopped.
- The same STOP-criteria ladder as the soak run applies: manual STOP file,
  cumulative budget exhaustion, and (for the soak harness) wall-clock cap.
- Remove the file and restart the process to resume (state is not persisted;
  the budget restarts at zero -- keep the shell history for accounting).

## 4. Landed-bundle forensics

For every `Bid dispatched` line record (append to `logs/backrun/bids.jsonl`):

```json
{"ts_unix_ms": 0, "tx_hash": "0x..", "target": "0x..", "bid_wei": "0",
 "submitted": 1, "skipped": 0, "block": 0}
```

After the block lands (`eth_getTransactionReceipt` via the sidecar probe),
append the landing record:

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

- The dispatcher releases nonces/pools at confiration granularity
  (`Dispatcher::advance_block` per head); a submission that never lands voids
  its nonce after `blocks_before_nonce_expires` heads.
- There is NO replacement ladder in the sidecar: one bid per decoded frame.
  A price-improved target simply reappears as a new frame (stale ms bound
  keeps the request current) -- replacement storms are a mainloop concern,
  out of sidecar scope by design.

## 6. Run artifacts (session logs + JSONL trace)

Every sidecar process is one session. At boot it creates

```
<runs_dir>/backrun-sidecar/<yyyymmddTHHMMSSZ>-<pid>/
    stdout.log     # every fmt record (INFO and above by default)
    trace.jsonl    # the offline-review JSONL capture
```

and a best-effort `latest` symlink beside it:

```
<runs_dir>/backrun-sidecar/latest -> <yyyymmddTHHMMSSZ>-<pid>
```

The root is the typed `logging.runs_dir` key (TOML `[logging] runs_dir`, env
`DEGENBOT_RUNS_DIR`), defaulting to `~/.config/degenbot/logs`; a leading `~`
expands against `HOME`.

| Env | Meaning |
| --- | --- |
| `SIDECAR_LOG_STDERR=1` | Also mirror the session log to stderr (interactive runs). File-only otherwise. |
| `SIDECAR_TRACE_JSONL` | Explicit capture path. When set it wins; when absent the trace helpers append to the session's `trace.jsonl`. |

There is deliberately **no rotation, compression, or size cap**: run artifacts
are the forensics surface, and cleanup is the operator's (or an out-of-process
job's) call. `latest` exists precisely so scripts never need to scan the
engine directory.

If the run directory cannot be created (bad `DEGENBOT_RUNS_DIR`, permissions),
the sidecar logs to stderr and continues; a `latest` symlink failure is
swallowed. Dry-run/fixture mode is unchanged: an explicit capture path is
honored exactly as before, and a fixture run also writes its session
`trace.jsonl`.
