# Plan 099: On-demand Rust engine state observability for simulation failures

## Overview

Add a Rust-side diagnostic state dump that captures the engine's current view
of every hop in a registered path and compares it to on-chain state at a known
block. The dump is triggered on demand, typically from the Python simulation
failure path, and written to a structured JSONL file. This gives us the
observability needed to decide whether simulation reverts are caused by stale
engine state or by encoder/command-stream bugs, without adding log spam to
normal runs.

## Problem

### Deletion test

If we deleted the existing liquidity-verification calls (`verify_on_register`,
`verify_snapshot_block`, `verify_backfill_block`) from the Rust engine, we would
still get simulation results and reverts, but we would have no way to know
whether a revert came from bad tick data, bad sqrt-price/liquidity values, stale
V2 reserves, or a bad encoder. The new observability is necessary because the
engine owns the canonical state and Python cannot inspect it today.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Engine state is opaque to Python after registration | `rust/src/optimizers/uniswap_engine/py_binding.rs` | We cannot diff the engine's view of a pool against on-chain without adding ad-hoc logging inside Rust event processing. |
| Simulation failure diagnostics only show on-chain via extra RPC calls | `examples/eth_backrun_v2_v3_v4_rust.py` `simulate_one` failure path | The current diagnostics query the chain but not the engine, so we cannot tell if the two diverged. |
| V2/V3/V4 state formats differ | `rust/src/optimizers/{v2,v3,v4}_block_engine.rs` | There is no unified "pool state" view exposed to callers, making per-hop comparison repetitive. |
| Liquidity verification exists only for tick data at registration | `rust/src/optimizers/uniswap_engine/py_binding.rs` `run_cl_verification` | sqrt-price, tick, in-range liquidity, and V2 reserves are never compared to the chain after backfill. |

## Solution

### 1. Diagnostic snapshot type

Define a serializable Rust value type that can represent a pool state for any
supported pool family. Keep it free of internal engine references so it can be
passed to Python and written to disk.

```rust
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "pool_family")]
enum DiagnosticPoolState {
    V2 {
        address: String,
        reserve0: U256,
        reserve1: U256,
        fee_numerator: u32,
        fee_denominator: u32,
    },
    V3 {
        address: String,
        token0: String,
        token1: String,
        fee: u32,
        tick_spacing: i32,
        sqrt_price_x96: U256,
        tick: i32,
        liquidity: U256,
    },
    V4 {
        pool_manager: String,
        pool_id: String,
        currency0: String,
        currency1: String,
        fee: u32,
        tick_spacing: i32,
        hook_flags: u16,
        sqrt_price_x96: U256,
        tick: i32,
        liquidity: U256,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct DiagnosticHop {
    position: usize,
    hop_type: String, // "V2", "V3", "V4"
    zero_for_one: bool,
    /// Engine-owned state (captured under the engine lock).
    engine_state: DiagnosticPoolState,
    /// On-chain state fetched after the lock is released (optional).
    onchain_state: Option<DiagnosticPoolState>,
    /// Simple string diff, empty when states match.
    diff: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct DiagnosticPathState {
    timestamp: String,
    path_id: u64,
    solve_block: u64,
    onchain_block: Option<u64>,
    path_type: String,
    hops: Vec<DiagnosticHop>,
    /// Calldata that produced the revert, if triggered from a sim failure.
    failed_calldata: Option<String>,
    /// Revert selector or decoded reason, if available.
    revert_info: Option<String>,
}
```

### 2. Bulk dump entry point on `PyUniswapArbEngine`

Expose a single Python-facing method that snapshots engine state, then fetches
on-chain values, then returns a `DiagnosticPathState`.

```rust
#[pymethods]
impl PyUniswapArbEngine {
    /// Snapshot the engine state for every hop in `path_id` and optionally
    /// compare it to on-chain state.
    ///
    /// Engine state is captured while the engine lock is held. The lock is
    /// released before any RPC calls so the pump is not stalled.
    fn diagnostic_inspect_path(
        &self,
        py: Python<'_>,
        path_id: usize,
        rpc_url: Option<String>,
    ) -> PyResult<PyObject> {
        // 1. Acquire lock, record current block, copy per-hop state.
        // 2. Release lock.
        // 3. If rpc_url is provided, fetch on-chain state at the recorded block
        //    (fall back to "latest"/"pending" and record the actual block).
        // 4. Compare and return a DiagnosticPathState as a Python dict.
    }
}
```

The method uses the existing engine path registry to map `path_id` to its hops,
so callers do not have to know engine-internal keys.

### 3. Lock discipline

- Acquire the `parking_lot::Mutex<UniswapEngine>` for the snapshot only.
- Copy every scalar field needed for the diff.
- Record the engine's current head block before releasing the lock.
- Release the lock.
- Fetch on-chain state with an `AlloyProvider` at the recorded block if
  possible.

This avoids stalling the WS pump while still giving a coherent engine view.
Because the on-chain fetch is at the same block the engine reported, the diff
is meaningful even though the lock is not held during the RPC.

### 4. Python integration and JSONL output

In `examples/eth_backrun_v2_v3_v4_rust.py`, add an opt-in helper:

```python
STATE_DUMP_ON_REVERT = os.environ.get("STATE_DUMP_ON_REVERT", "0") == "1"

async def dump_state_on_revert(
    *,
    path_id: int,
    path_type: str,
    solve_block: int,
    calldata: str,
    revert_info: str,
    engine_registry: EngineRegistry,
    dump_dir: Path,
    dedupe_key: tuple[int, str],
    dedupe_set: set[tuple[int, str]],
) -> None:
    """Write a JSONL diagnostic record after a simulation failure."""
    if dedupe_key in dedupe_set:
        return
    dedupe_set.add(dedupe_key)

    snapshot = await asyncio.to_thread(
        engine_registry.engine.diagnostic_inspect_path,
        path_id,
        # Use the same HTTP node the engine already uses for verification.
        os.environ.get("NODE_HTTP_URL"),
    )
    snapshot["path_type"] = path_type
    snapshot["failed_calldata"] = calldata
    snapshot["revert_info"] = revert_info

    dump_dir.mkdir(parents=True, exist_ok=True)
    dump_file = dump_dir / f"{datetime.utcnow().isoformat()}.jsonl"
    with dump_file.open("a", encoding="utf-8") as f:
        f.write(json.dumps(snapshot, default=str) + "\n")
```

Call this from `simulate_one` only when the simulation fails and
`STATE_DUMP_ON_REVERT` is enabled. Deduplicate by `(solve_block, path_type)` so
one bad pattern produces one artifact per block.

### 5. JSONL record metadata

Every line includes:

- `timestamp` — ISO-8601 UTC when the dump was taken.
- `path_id`, `path_type` — Rust path identity as seen by the engine.
- `solve_block` — the block the engine used to produce the result.
- `onchain_block` — the block at which on-chain state was fetched (may differ
  if the node does not support historical `eth_call`).
- `hops` — one object per hop with:
  - `position`, `hop_type`, `zero_for_one`
  - `engine_state` and `onchain_state`
  - `diff` — list of field-level mismatches (e.g. `reserve0: engine=..., chain=...`)
- `failed_calldata`, `revert_info` — simulation context.

### Design decisions

- **One bulk method vs per-family methods**: A single `diagnostic_inspect_path`
  matches how Python already thinks about paths and hides engine-internal keys.
- **Snapshot under lock, RPC after lock**: Avoids stalling the event pump.
  Historical `eth_call` at the recorded block makes the diff coherent.
- **JSONL output, not logger**: Lets us analyze reverts offline with `jq` or
  pandas without re-running the bot.
- **Opt-in via env var**: Keeps production runs quiet and avoids disk growth.
- **Dedupe by `(block, path_type)`**: Prevents a single systematic encoder bug
  from generating thousands of identical records.

## Files Involved

**Primary:**
- `rust/src/optimizers/uniswap_engine/py_binding.rs` — add
  `diagnostic_inspect_path` method and snapshot comparison logic.
- `rust/src/optimizers/uniswap_engine/mod.rs` — expose path-to-hop mapping and
  pool-state accessors needed by the new method.
- `examples/eth_backrun_v2_v3_v4_rust.py` — add `STATE_DUMP_ON_REVERT` helper,
  call it from `simulate_one`, manage the dedupe set.

**Secondary:**
- `rust/src/optimizers/{v2,v3,v4}_block_engine.rs` — add `diagnostic_state()`
  accessors that return the fields needed for `DiagnosticPoolState`.
- `rust/src/provider.rs` or `rust/src/provider_py.rs` — ensure we can make
  targeted `eth_call`/`getReserves`/`slot0`/V4 StateView calls from Rust.
- `examples/mainnet.env` — optionally document `STATE_DUMP_ON_REVERT`.

**No change needed:**
- `contracts/cmd_executor.vy` and `examples/eth_backrun_helpers.py` — state
  observability is upstream of encoding; encoder work is tracked separately in
  TODO-071d4791.

## Implementation Order

### Slice 1: Snapshot type and Rust path-to-hop access

1. Add `DiagnosticPoolState` and `DiagnosticPathState` types in
   `rust/src/optimizers/uniswap_engine/diagnostic.rs`.
2. Add accessors on `v2_block_engine::V2Pool`, `v3_block_engine::V3Pool`, and
   `v4_block_engine::V4Pool` that return the diagnostic state without exposing
   internals.
3. Expose a method on `UniswapEngine` that, given a `path_id`, returns the
   engine-side `DiagnosticPathState`.
4. Run `just test-rust` — expect existing tests to pass.

### Slice 2: Python-facing diagnostic method (engine state only)

1. Add `PyUniswapArbEngine.diagnostic_inspect_path(path_id, rpc_url=None)` that
   acquires the engine lock, captures state, and returns a Python dict.
2. Add a minimal Python script under `examples/` or a small call in the bot to
   exercise it for one path.
3. Run `just test-rust-python` — expect compilation and import to succeed.

### Slice 3: On-chain fetch and diff generation

1. Implement Rust RPC fetch for each pool family using the existing
   `AlloyProvider`:
   - V2: `getReserves`
   - V3: `slot0()` + `liquidity()`
   - V4: StateView `getSlot0` / `getLiquidity` or equivalent
2. Compare fetched values to the snapshot and populate `diff`.
3. Add a Rust unit test that compares a known fake pool state against the
   on-chain values fetched from a mock provider (if available) or at least
   validates the diff logic.
4. Run `just test-rust`.

### Slice 4: JSONL output from the Python failure path

1. Add `STATE_DUMP_ON_REVERT` env var and the JSONL writer in
   `examples/eth_backrun_v2_v3_v4_rust.py`.
2. Call the writer from `simulate_one` after a revert, passing `solve_block`,
   `path_type`, and revert data.
3. Deduplicate by `(solve_block, path_type)`.
4. Run the permutation runner on one problematic path type with the flag on
   and inspect the resulting JSONL file.

### Slice 5: Validate and clean up

1. Run `just lint` and `just test-all`.
2. Document the new env var in a comment near `INJECT_EXECUTOR_CODE` and in
   `examples/mainnet.env`.
3. Update `rust/CONTEXT.md` if any new domain terms are introduced.

## Testing

### Per-slice test runs

Each slice should leave `just test-rust` and `just test-python` green.

### New unit tests

- `rust/src/optimizers/uniswap_engine/tests.rs`:
  - `test_diagnostic_path_state` — build a small engine with one V2, one V3,
    and one V4 path, snapshot it, and assert every hop is captured.
  - `test_state_diff` — assert that a deliberate mismatch between snapshot and
    on-chain fetch is reported in the `diff` field.

### Integration test

- Add a one-off assertion in `examples/eth_backrun_v2_v3_v4_rust.py` dry-run
  path (or a small standalone script) that calls
  `engine_registry.engine.diagnostic_inspect_path(0, None)` and validates the
  returned dict shape. This ensures the Python binding does not drift.

## Benefits

- **Locality**: All per-version state access needed for diagnostics lives next
  to the block engines that own the state.
- **Seam**: `diagnostic_inspect_path` becomes a clean seam between the engine
  internals and the Python orchestration layer.
- **Leverage**: Once the dump exists, every simulation failure becomes a
  reproducible data point that can drive fixes in both the Rust solver and the
  Python encoders.
- **Depth**: Moves a shallow "log and guess" failure mode into a structured
  diff that narrows the root cause quickly.

## Risks

- **RPC overhead on failure path**: Mitigated by releasing the engine lock
  before RPC and deduplicating dumps.
- **Disk growth during long runs**: Mitigated by the env-var gate and the
  per-block/per-type dedupe. A future slice can add rotation.
- **Json serialization of U256**: We must use a consistent string encoding
  (e.g., `0x...`) so diffs are human-readable and deterministic.
- **Historical `eth_call` support**: Some providers do not support historical
  `eth_call`. The fallback to `latest`/`pending` and recording of
  `onchain_block` makes the limitation explicit in the artifact.

## Relationship to Other Plans

- **Plan 098** (snapshot transfer to Rust): This plan consumes the same
  engine-owned state and extends it with runtime diagnostics.
- **Plan 079** (Rust-owned bot core): The lock and state ownership model
  defined there is reused here.
- **Plan 084** (solver per-hop outputs): Simulation failures may be caused by
  stale per-hop outputs; being able to inspect engine state at `solve_block`
  helps validate those outputs.
- **TODO-071d4791** (encoder/permutation runner correctness): This plan is a
  prerequisite. Once engine state is trusted, TODO-071d4791 can be addressed
  without engine-state ambiguity.

## Status

- [x] Slice 1: Snapshot type and Rust path-to-hop access
- [x] Slice 2: Python-facing diagnostic method (engine state only)
- [x] Slice 3: On-chain fetch and diff generation
- [x] Slice 4: JSONL output from the Python failure path
- [x] Slice 5: Validate and clean up

Plan complete. Also fixed a pre-existing markdown link in `contracts/user-guide.md` so `just lint` passes cleanly.
