# AILRM4 — hotpath validation: before/after the WJ57BO perf epic

Procedure: offline fallback per the task body (live-example hotpath profiling
needs the settlement-bot RPC flow; the sanctioned offline harness is the
degenbot-solvers boundary-scan example).

## Harness
`cargo build --release -p degenbot-solvers --example cl_boundary_scan`
run against the local mainnet-fork node
(`DEGENBOT_CLCAP_RPC=$DEGENBOT_RPC_HTTP_CHAINID_1`,
`DEGENBOT_SCAN_BLOCK=0x18a702e`, 8-pool V3 list incl. one DENSE pool with
888 solver ranges — identical inputs both sides, 3 reps each, solver-only
elapsed from the summary line).

## Results (solver-only elapsed, seconds)

| Run | Before (eager gen_ticks + SipHash maps + std Mutex + always-on lock diag) | After (lazy iterator + foldhash maps + parking_lot + gated diag) |
|-----|---------------------------------------------------------------------------:|------------------------------------------------------------------:|
| 1   | 1.479 | 1.607 |
| 2   | 1.589 | 1.486 |
| 3   | 1.620 | 1.493 |
| median | 1.589 | 1.493 |
| mean | 1.563 | 1.529 |

After-binaries carry all four leaf changes (YD3K3M gen_ticks_iter,
OPMQI3 gated StateLock diagnostics + leak fix, FTCG7Q hashbrown/foldhash maps,
VB72YG parking_lot PathTimesHeap).

## Verdict
- No measure regressed (worst run-by-run delta < ±10 %; the harness mean is
  -2.2 %, median -6.0 % in the good direction, within run-to-run noise).
- The workload is heavily RPC-fetch-weighted (250 tick-word fetches per
  pool across a local node), which dilutes the compute-side effects; the true
  unconditional wins (240 KB per-sim allocation removed, per-acquire registry
  traffic + leak removed, faster hashing) are structural, while the scan shows
  they cost nothing end-to-end.
- Acceptance criterion met: delta table recorded; no >10 % regression to
  report.
