# Plan 084: Solver Per-Hop Outputs

## Overview

Extend the Rust solver to expose per-hop output amounts alongside `(optimal_input, profit)`, and refactor the Python encoder to use those amounts instead of calling `pool.calculate_tokens_out_from_tokens_in()` on stale Python pool objects. Replace the single `HopInfo` type with typed variants (`V2HopInfo`, `V3HopInfo`, `V4HopInfo`) carrying only the immutable metadata each encoder needs, plus a Rust-assigned `pool_key` for mutual exclusivity tracking. Migrate `pending_pools`/`committed_pools` from `set[object]` (Python pool identity) to `set[int]` (Rust pool keys). This eliminates all runtime dependencies on Python pool state after startup, completing the thin-bootstrap architecture: Python builds pools and paths, Rust owns all runtime data.

## Problem

### Deletion test

If you deleted every `pool.calculate_tokens_out_from_tokens_in()` call in the 9 encoder functions, every `path_info.hops[i].pool` reference in the dispatch path, and the `{h.pool for h in path_info.hops}` mutual exclusivity check — the encoders would have no way to determine amounts, token addresses, or pool identity. But the Rust solver already computes the amounts internally, addresses are immutable metadata available at registration, and the Rust engine assigns unique `u64` keys. The deletion test reveals that Python pool objects are pass-throughs for data already available elsewhere.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| `IncompleteSwap` on stale Python pools | `encode_v3v4_payloads()`, `encode_v3v3_payloads()`, etc. | After backfill, the Rust pump updates engine state autonomously. Python pools are frozen. When the encoder calls `calculate_tokens_out_from_tokens_in()` on the stale Python pool, tick data divergence causes `IncompleteSwap` — the path is dropped even though the Rust solver correctly found it profitable. Same paths fail every dispatch cycle. |
| Encoder re-derives what the solver already computed | Every `encode_*_payloads()` function | The solver computes `forward_out` and `output_b` internally (via `int_simulate_v3_v3_path`, `int_simulate_path`, etc.) then throws them away. The encoder re-derives the same amounts from different (stale) data. This is redundant work that produces wrong answers. |
| Python pool objects in the hot loop | `path_info.hops[].pool`, `{h.pool for h in hops}`, `SubmittedTx.pools`, `pending_pools: set[object]` | Pool objects are kept alive solely for the encoder and mutual exclusivity. Their state is stale after backfill. They create a coupling between the Python and Rust object lifecycles. |
| Broad `except Exception` in encoders swallows real bugs | All 9 `encode_*_payloads()` functions | Because `IncompleteSwap` is expected (stale state), encoders catch all exceptions and return `None`. Real bugs are silently swallowed. Catching `IncompleteSwap` separately only masks the symptom — the root cause is that the encoder shouldn't call Python pool methods at all. |
| Mutual exclusivity uses Python pool identity | `pending_pools: set[object]`, `committed_pools: set[object]`, `SubmittedTx.pools` | Object identity is fragile: two pool objects for the same contract could compare as different. The Rust engine already assigns unique `u64` keys per pool — those are the authoritative identifiers. |

## Solution

### Step 1: Extend Rust simulation functions to return per-hop outputs

Modify the simulation functions in-place to return per-hop output amounts alongside the final output. No backward-compatible variants — internal callers are updated in the same change.

**`int_simulate_v3_v3_path`** — currently returns `U256` (final output). Change to:
```rust
struct SimulationResult {
    final_output: U256,
    hop_outputs: Vec<U256>,  // [output1, output2]
}

fn int_simulate_v3_v3_path(...) -> SimulationResult
```

**`int_simulate_mixed_path_with_crossing`** — same pattern:
```rust
fn int_simulate_mixed_path_with_crossing(...) -> SimulationResult
```

**`int_simulate_path`** (V2-V2 constant-product) — currently returns `U256`. Change to collect each hop's output:
```rust
fn int_simulate_path(x: U256, hops: &[IntHopState]) -> SimulationResult {
    let mut amount = x;
    let mut hop_outputs = Vec::with_capacity(hops.len());
    for hop in hops {
        if amount.is_zero() {
            return SimulationResult { final_output: U256::ZERO, hop_outputs };
        }
        amount = hop.swap(amount);
        hop_outputs.push(amount);
    }
    SimulationResult { final_output: amount, hop_outputs }
}
```

For a 2-hop path: `hop_outputs[0]` = intermediate output, `hop_outputs[1]` = final output. `hop_outputs[1] - optimal_input = profit`.

### Step 2: Thread per-hop outputs through solve_path → solve_all → PyO3 binding

**`solve_path`** — currently returns `Option<(U256, U256)>` (optimal_input, profit). Change to:
```rust
struct SolvePathResult {
    optimal_input: U256,
    profit: U256,
    hop_outputs: Vec<U256>,
}
// solve_path returns Option<SolvePathResult>
```

**`solve_all`** — stores `Vec<(u64, SolvePathResult)>`.

**`latest_results`** (PyO3 binding) — return a **structured format** instead of the current flat list:

```python
# Current: flat list with implicit stride-3
[path_id_0, opt_input_0, profit_0, path_id_1, ...]

# After: list of tuples — self-describing, no stride assumption
[
    (path_id_0, opt_input_0, profit_0, (hop1_out_0, hop2_out_0)),
    (path_id_1, opt_input_1, profit_1, (hop1_out_1, hop2_out_1)),
    ...
]
```

The tuple of per-hop outputs is a Python tuple, matching the path's hop count. No implicit stride — each result is self-describing. This won't break if 3-hop paths are added later.

### Step 3: Typed HopInfo variants with immutable metadata

Replace the single `HopInfo` dataclass with three typed variants, each carrying exactly the immutable fields its encoder needs plus a Rust-assigned `pool_key`:

```python
@dataclasses.dataclass(frozen=True)
class V2HopInfo:
    pool_key: int          # Rust engine u64 key — used for mutual exclusivity
    pool_address: str
    token0_address: str
    token1_address: str
    zfo: bool


@dataclasses.dataclass(frozen=True)
class V3HopInfo:
    pool_key: int
    pool_address: str
    token0_address: str
    token1_address: str
    fee: int               # e.g. 3000 for 0.3%
    zfo: bool


@dataclasses.dataclass(frozen=True)
class V4HopInfo:
    pool_key: int
    pool_manager_address: str
    pool_id_hex: str
    currency0_address: str
    currency1_address: str
    fee: int
    tick_spacing: int
    hook_address: str
    zfo: bool


HopInfo = V2HopInfo | V3HopInfo | V4HopInfo
```

All fields are immutable and populated at construction time from the Python pool object (which is still available during the bootstrap phase). After this plan, the hot loop never dereferences a Python pool object.

The `pool_key` field holds the `u64` key assigned by the Rust engine at registration (`_v2_keys[address]`, `_v3_keys[address]`, `_v4_keys[pool_id_hex]`).

`PathInfo.hops` becomes `list[HopInfo]` — a heterogeneous list of the union type. The encoder dispatch still uses `path_info.path_type` (string like `"V3-V4"`) to select the right encoder, and pattern-matches on `isinstance(hop, V3HopInfo)` to access type-specific fields.

### Step 4: Refactor encoders to use Rust per-hop outputs and typed HopInfo

Replace every `pool.calculate_tokens_out_from_tokens_in()` call with the corresponding value from `hop_outputs`. Replace every `pool.token0.address` / `pool.fee` / etc. with the corresponding `HopInfo` field.

**Encoder signature change** (example: V3-V4):
```python
# Before
def encode_v3v3_payloads(
    path_info: PathInfo,
    optimal_input: int,
    executor_address: str,
) -> ...:

# After
def encode_v3v3_payloads(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],  # (hop1_out, hop2_out)
    executor_address: str,
) -> ...:
```

Example transformation (V3-V4 encoder):
```python
# BEFORE — stale Python pool call
hop_a = path_info.hops[0]  # was: v3_a = path_info.hops[0].pool
hop_b = path_info.hops[1]  # was: v3_b = path_info.hops[1].pool
forward_out = v3_a.calculate_tokens_out_from_tokens_in(
    token_in=token_in_v3, token_in_quantity=optimal_input,
)

# AFTER — from Rust solver, consistent with its state
hop_a = path_info.hops[0]  # V3HopInfo
hop_b = path_info.hops[1]  # type depends on path
forward_out = hop_outputs[0]
token_in = hop_a.token0_address if hop_a.zfo else hop_a.token1_address
```

V4-specific fields come from `V4HopInfo`:
```python
# BEFORE — _v4_pool_key_salient dereferences Python pool
key = _v4_pool_key_salient(v4_pool)

# AFTER — all fields are on V4HopInfo
hop = path_info.hops[1]  # V4HopInfo
key = (hop.currency0_address, hop.currency1_address, hop.fee, hop.tick_spacing, hop.hook_address)
```

### Step 5: Migrate mutual exclusivity to Rust pool keys

Replace `set[object]` (Python pool identity) with `set[int]` (Rust `u64` pool keys):

```python
# BEFORE
pending_pools: set[object] = set()
path_pools = {h.pool for h in path_info.hops}  # pool objects

# AFTER
pending_pools: set[int] = set()
path_pools = {h.pool_key for h in path_info.hops}  # Rust keys
```

Update `SubmittedTx.pools`:
```python
# BEFORE
class SubmittedTx:
    pools: set[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool]

# AFTER
class SubmittedTx:
    pools: set[int]
```

The `difference_update`, intersection, and union operations work identically on `set[int]`. Pool keys are assigned at registration and are stable for the engine's lifetime.

### Step 6: Remove stale exception handlers and narrow catches

With stale-pool calls eliminated, `IncompleteSwap` can no longer occur in the encoder. Remove all `except IncompleteSwap` handlers. Replace broad `except Exception` with specific catches for expected failure modes:

- `ValueError` — ABI encoding failures, invalid amount conversions
- `OverflowError` — int128 overflow in V4 encoding

Let unexpected exceptions propagate — they indicate real bugs that need immediate attention, not silent swallowing.

### Design decisions

- **Structured result format (list of tuples)**: Self-describing and won't break if hop counts change. Each result is `(path_id, optimal_input, profit, (hop1_out, hop2_out, ...))`. No implicit stride — the per-hop outputs are a nested tuple whose length matches the path's hop count. Marginal Python object construction overhead is negligible vs. the RPC calls in the dispatch path.
- **Frozen dataclasses for HopInfo**: Immutable after construction. Prevents accidental mutation. Matches the project pattern for value objects.
- **`pool_key: int` on every HopInfo**: The Rust-assigned `u64` key uniquely identifies a pool across V2/V3/V4. Used for mutual exclusivity and potential future Rust API calls. No need for a separate `pool_type` field — `isinstance(hop, V2HopInfo)` is the structural check.
- **V4-V4 `dynamic_amount` is encoder-specific**: The V4-V4 encoder uses `dynamic_amount=True` for the second swap, meaning the on-chain `BalanceDelta` from the first swap is forwarded as `amountSpecified` for the second. This is a contract-specific optimization — the Rust solver still provides `hop_outputs[1]` unconditionally, but the V4-V4 encoder doesn't use it for calldata. It may use it for the int128 overflow guard and logging.
- **Per-hop outputs are expected to be accurate**: The Rust solver's state is updated by the pump, which processes events nearly in real-time. Per-hop outputs derived from the solver's state should match on-chain execution for the current block. The `eth_simulateV1` step validates against any residual discrepancy (latency, state bugs, encoding issues).
- **No backward compatibility layers**: Internal development, no external callers. In-place changes to simulation functions, no parallel `_with_hop_outputs` variants.
- **`V4PoolInfo.pool` reference kept in backfill code**: The backfill phase (pre-pump) still uses Python pool objects for event processing. This is bootstrap-only, not the hot loop. `V4PoolInfo` can be updated in future work.
- **`zfo` stays on HopInfo**: Zero-for-one determines the swap direction. It's set at path construction time and doesn't change.

## Files Involved

**Primary:**
- `rust/src/optimizers/mobius_v3_int.rs` — Return per-hop outputs from simulation functions in-place
- `rust/src/optimizers/mobius_int_exact.rs` — Return per-hop outputs from `exact_mobius_solve` in-place; add `hop_outputs` field to `ExactMobiusResult`
- `rust/src/optimizers/mobius_int.rs` — Return per-hop outputs from `int_simulate_path` in-place
- `rust/src/optimizers/uniswap_engine.rs` — `SolvePathResult` struct, thread through `solve_path` → `solve_all` → `latest_results`; structured result format in PyO3 binding
- `examples/eth_backrun_v2_v3_v4_rust.py` — Typed `HopInfo` variants, refactor all 9 encoders, `profitable_results` parse, mutual exclusivity migration, `SubmittedTx` update, remove `_v4_pool_key_salient` (replaced by `V4HopInfo` fields), remove `IncompleteSwap` import

**Secondary:**
- `rust/src/optimizers/v3_block_engine.rs` — No change (pool state unchanged)
- `rust/src/optimizers/v4_block_engine.rs` — No change (pool state unchanged)

**No change needed:**
- `src/degenbot/uniswap/v3_pool_calc.py` — Encoder no longer calls it at runtime
- `src/degenbot/uniswap/v4_liquidity_pool.py` — Encoder no longer calls it at runtime
- `src/degenbot/exceptions/pool.py` — `IncompleteSwap` stays (used elsewhere), just no longer caught in encoders

## Implementation Order

### Slice 1: Extend Rust simulation functions to return per-hop outputs

1. Define `SimulationResult { final_output: U256, hop_outputs: Vec<U256> }` in `mobius_int.rs` (or a shared types module)
2. Modify `int_simulate_path` to collect per-hop outputs into `SimulationResult`
3. Modify `int_simulate_v3_v3_path` to collect `output1`, `output2` into `SimulationResult`
4. Modify `int_simulate_mixed_path_with_crossing` to collect per-stage outputs into `SimulationResult`
5. Update all internal callers (`int_solve_v3_v3`, `solve_mixed_path_int`, `exact_mobius_solve`) to consume `SimulationResult`
6. Add `hop_outputs: Vec<U256>` to `ExactMobiusResult`
7. Run: `just test-rust` — expect all existing tests to pass (new fields are additive)

**New Rust unit tests:**
```rust
#[test]
fn test_int_simulate_v3_v3_path_hop_outputs() {
    // Set up a 2-hop V3-V3 path with known tick ranges.
    // Verify hop_outputs[0] = output from hop 1 (intermediate).
    // Verify hop_outputs[1] = final output.
    // Invariant: hop_outputs[1] - optimal_input = profit.
}

#[test]
fn test_int_simulate_path_hop_outputs_v2_v2() {
    // Set up a V2-V2 path.
    // Verify hop_outputs has length 2.
    // Invariant: IntHopState::swap(optimal_input, hop0) == hop_outputs[0].
    // Invariant: IntHopState::swap(hop_outputs[0], hop1) == hop_outputs[1].
}

#[test]
fn test_mixed_path_hop_outputs() {
    // Set up a V2-V3 mixed path.
    // Verify per-hop outputs are consistent with the mixed simulation.
    // Invariant: hop_outputs.last() - optimal_input = profit.
}
```

### Slice 2: Expose per-hop outputs through Python binding

1. Define `SolvePathResult { optimal_input: U256, profit: U256, hop_outputs: Vec<U256> }` in `uniswap_engine.rs`
2. Update `solve_path` to return `Option<SolvePathResult>`
3. Update `solve_all` to store `Vec<(u64, SolvePathResult)>`
4. Update `latest_results` PyO3 binding to return structured format:
   ```python
   [(path_id, opt_input, profit, (hop1_out, hop2_out)), ...]
   ```
5. Run: `just test-rust` — expect pass
6. Run: `just test-rust-python` — expect pass (existing tests use `latest_results` — update parsing)

**New Rust unit tests:**
```rust
#[test]
fn test_latest_results_structured_format() {
    // Register a 2-hop path, process a block, solve.
    // Verify latest_results returns list of tuples with per-hop outputs.
    // Invariant: hop2_out - optimal_input == profit.
}
```

### Slice 3: Typed HopInfo variants and profitable_results update in Python

1. Define `V2HopInfo`, `V3HopInfo`, `V4HopInfo` frozen dataclasses
2. Define `HopInfo = V2HopInfo | V3HopInfo | V4HopInfo` union type
3. Populate in `build_paths()` (or equivalent) where `HopInfo` objects are created — extract immutable fields from Python pool objects during bootstrap
4. Store `pool_key` from the Rust registry dicts (`_v2_keys`, `_v3_keys`, `_v4_keys`)
5. Update `ProfitableResult` / `profitable_results()` to parse the structured format
6. Update `dispatch_profitable_results()` to pass `hop_outputs` through the dispatch chain
7. Run: `just test-python` — expect pass (encoders not yet changed)

**New Python unit tests:**
```python
def test_v2_hop_info_fields():
    """V2HopInfo carries pool_key, pool_address, token addresses."""
    hop = V2HopInfo(pool_key=42, pool_address="0xabc...", token0_address="0xt0", token1_address="0xt1", zfo=True)
    assert hop.pool_key == 42

def test_v3_hop_info_fields():
    """V3HopInfo includes fee."""
    hop = V3HopInfo(pool_key=7, pool_address="0xabc...", token0_address="0xt0", token1_address="0xt1", fee=3000, zfo=True)
    assert hop.fee == 3000

def test_v4_hop_info_fields():
    """V4HopInfo includes fee, tick_spacing, hook_address, pm address, pool_id."""
    hop = V4HopInfo(pool_key=99, pool_manager_address="0xpm", pool_id_hex="0xpid",
                     currency0_address="0xc0", currency1_address="0xc1",
                     fee=3000, tick_spacing=60, hook_address="0xhook", zfo=True)
    assert hop.hook_address == "0xhook"

def test_hop_info_isinstance_dispatch():
    """PathInfo with heterogeneous hops can dispatch by isinstance."""
    hops = [V3HopInfo(...), V4HopInfo(...)]
    assert isinstance(hops[0], V3HopInfo)
    assert isinstance(hops[1], V4HopInfo)

def test_profitable_results_parses_structured_format():
    """profitable_results() correctly parses the structured result list."""
    ...
```

### Slice 4: Refactor V3-V3 and V3-V4 encoders (highest-impact path types)

1. Add `hop_outputs: tuple[int, ...]` parameter to `encode_v3v3_payloads()` and `encode_v3v4_payloads()`
2. Replace `v3_pool.calculate_tokens_out_from_tokens_in()` with `hop_outputs[0]` for `forward_out`
3. Replace token address lookups: `pool.token0.address` → `hop.token0_address` (via `V3HopInfo`)
4. Replace pool address: `pool.address` → `hop.pool_address`
5. Remove `IncompleteSwap` catch from these two encoders
6. Narrow exception handlers: catch `ValueError` and `OverflowError` only
7. Update `encode_payloads()` to pass `hop_outputs` through
8. Run: `just test-python` — expect pass

**New Python unit tests:**
```python
def test_encode_v3v3_uses_hop_outputs():
    """encode_v3v3_payloads uses hop_outputs instead of calculate_tokens_out."""
    hop_a = V3HopInfo(pool_key=1, pool_address="0xA", token0_address=WETH, token1_address=USDC, fee=3000, zfo=True)
    hop_b = V3HopInfo(pool_key=2, pool_address="0xB", token0_address=USDC, token1_address=WETH, fee=3000, zfo=False)
    path_info = PathInfo(hops=[hop_a, hop_b])
    hop_outputs = (1_000_000, 1_050_000)
    result = encode_v3v3_payloads(path_info, optimal_input=1_000_000, hop_outputs=hop_outputs, executor_address=EXECUTOR)
    # Verify calldata encodes hop_outputs[0] as V3_B's amountSpecified
    # Verify no call to any pool's calculate_tokens_out_from_tokens_in
    ...

def test_encode_v3v4_uses_hop_outputs():
    """encode_v3v4_payloads uses hop_outputs instead of calculate_tokens_out."""
    # Similar pattern for V3→V4 path
    ...
```

### Slice 5: Migrate mutual exclusivity to Rust pool keys

1. Change `pending_pools: set[object]` → `pending_pools: set[int]`
2. Change `committed_pools: set[object]` → `committed_pools: set[int]`
3. Change `SubmittedTx.pools` from `set[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool]` → `set[int]`
4. Change `path_pools` construction: `{h.pool for h in path_info.hops}` → `{h.pool_key for h in path_info.hops}`
5. Run: `just test-python` — expect pass

### Slice 6: Refactor remaining encoders (V4-V4, V4-V3, V2-V3, V3-V2, V2-V2, V4-V2, V2-V4)

1. Apply the same pattern from Slice 4 to all 7 remaining encoders
2. Replace `_v4_pool_key_salient(pool)` with direct `V4HopInfo` field access
3. Remove all `IncompleteSwap` catches
4. Remove the `from degenbot.exceptions.pool import IncompleteSwap` import
5. Narrow remaining `except Exception` handlers to `ValueError`/`OverflowError`
6. Remove the `_v4_pool_key_salient` function (now dead code)
7. Run: `just test-python` — expect pass

**New Python unit tests:**
```python
def test_encode_v4v4_uses_hop_outputs():
    """encode_v4v4_payloads uses hop_outputs for int128 guard; second swap uses dynamic_amount=True."""
    hop_a = V4HopInfo(pool_key=5, ...)
    hop_b = V4HopInfo(pool_key=6, ...)
    path_info = PathInfo(hops=[hop_a, hop_b])
    hop_outputs = (1_000_000, 1_050_000)
    result = encode_v4v4_payloads(path_info, optimal_input=1_000_000, hop_outputs=hop_outputs, executor_address=EXECUTOR)
    # V4-V4: first swap uses -optimal_input, second uses dynamic_amount=True
    # int128 guard uses hop_outputs
    ...

def test_encode_v4v3_uses_hop_outputs():
    ...

def test_encode_v2v3_uses_hop_outputs():
    ...
# ... etc for all 7
```

### Slice 7: Validation and cleanup

1. Run the bot in dry-run mode and verify:
   - No "encoding failed" messages for `IncompleteSwap` (no longer possible)
   - Profitable paths encode and simulate correctly
   - Simulation results match expected values (same profit = same on-chain outcome)
   - Mutual exclusivity correctly prevents double-spending the same pool via Rust keys
2. Run `just test-all` + `just lint` — expect green
3. Verify `import IncompleteSwap` removed from example bot
4. Verify `_v4_pool_key_salient` removed
5. Verify no hot-loop references to Python pool objects remain in the dispatch path (grep for `h.pool` or `.pool` in encoder/dispatch functions)
6. Run: `just test-all` + `just lint` — expect green

## Testing

### Per-slice test runs

| Slice | Test command | Expect |
|-------|-------------|--------|
| 1 | `just test-rust` | Green (additive changes) |
| 2 | `just test-rust` + `just test-rust-python` | Green (update existing result parsing) |
| 3 | `just test-python` | Green (encoders not yet changed) |
| 4 | `just test-python` | Green |
| 5 | `just test-python` | Green |
| 6 | `just test-python` | Green |
| 7 | `just test-all` + `just lint` | Green |

### New unit tests

**Rust** — see Slice 1 and Slice 2 for specific test functions.

**Python** — new test file `tests/arbitrage/test_solver_per_hop_outputs.py`:
- `test_v2_hop_info_fields` — immutable metadata
- `test_v3_hop_info_fields` — includes fee
- `test_v4_hop_info_fields` — includes fee, tick_spacing, hook_address, pool_manager, pool_id
- `test_hop_info_isinstance_dispatch` — heterogeneous list dispatch
- `test_profitable_results_parses_structured_format` — new format parsing
- `test_encode_v3v3_uses_hop_outputs` — hop_outputs replace pool calculation
- `test_encode_v3v4_uses_hop_outputs`
- `test_encode_v4v4_uses_hop_outputs` — dynamic_amount=True for second swap
- `test_encode_v4v3_uses_hop_outputs`
- `test_encode_v2v3_uses_hop_outputs`
- `test_encode_v3v2_uses_hop_outputs`
- `test_encode_v2v2_uses_hop_outputs`
- `test_encode_v4v2_uses_hop_outputs`
- `test_encode_v2v4_uses_hop_outputs`
- `test_mutual_exclusivity_uses_pool_keys` — `set[int]` operations

### Integration tests

The existing Rust engine tests in `rust/src/optimizers/uniswap_engine.rs` cover end-to-end path registration, solving, and result extraction. These must be updated for the new structured result format. The V3-V3 accuracy tests in `tests/arbitrage/test_optimizers/` remain valid — they test the solver, not the encoder.

## Benefits

- **Depth (thin bootstrap)**: Eliminates ALL runtime dependencies on Python pool state. Python becomes a true bootstrap layer: build pools → register with Rust → Rust owns everything. This was the original intent of Plans 079/082.
- **Correctness**: `IncompleteSwap` from stale Python pools becomes impossible. The encoder uses amounts consistent with the solver's state.
- **Locality**: Per-hop outputs travel with the solve result to the encoder in a single data flow. No cross-referencing to separate Python pool state.
- **Simplicity**: Removes ~18 `calculate_tokens_out_from_tokens_in()` calls across 9 encoders, replacing each with a tuple index. Removes the need for `IncompleteSwap` exception handling in the encoder layer. Removes `_v4_pool_key_salient` (replaced by `V4HopInfo` fields).
- **Type safety**: `V2HopInfo`/`V3HopInfo`/`V4HopInfo` are frozen dataclasses with exactly the fields each encoder needs. No accessing `pool.fee` on a V2 pool by mistake. `isinstance` dispatch is structural and unambiguous.

## Risks

- **Rust simulation function signature changes are breaking internally**: `int_simulate_v3_v3_path`, `int_simulate_path`, `int_simulate_mixed_path_with_crossing` all change return type. All internal callers must be updated. **Mitigation**: `#[must_use]` and compile-time type checking ensure no callers are missed. The Rust compiler catches all missing updates.
- **Structured format adds Python object construction**: Each result is now a Python tuple containing a nested tuple. The overhead is O(1) per result (a few tuple allocations) vs. the existing overhead of `calculate_tokens_out_from_tokens_in()` (~5-50µs each, called twice per path). **Mitigation**: Negligible vs. the RPC calls in the dispatch path.
- **V4PoolInfo.pool reference remains in backfill code**: The pre-pump backfill phase still uses `V4PoolInfo.pool` to apply liquidity updates and extract tick data. This is bootstrap-only, not the hot loop. **Mitigation**: Out of scope for this plan. Can be addressed when backfill is fully Rust-owned.
- **pool_key must stay synchronized between Python and Rust**: If the Python `HopInfo.pool_key` diverges from the Rust engine's internal key, mutual exclusivity breaks silently. **Mitigation**: Keys are assigned once at registration and stored in Python's `EngineRegistry` lookup dicts. The `HopInfo` reads from those dicts. No separate source of truth.

## Relationship to Other Plans

- **Plan 079** (Rust-Owned Bot Core): This plan completes the encoder item from Plan 079's friction table: "Swap encoding crosses back into Python". It's the last piece needed for "Python is the cockpit, Rust is the engine".
- **Plan 082** (Rust-Owned State Pipeline): Plan 082 activated the Rust pumps. This plan fixes the follow-on issue: after the pump takes over, Python pool objects become stale, but the encoder still calls them. This is the "Rust engine state → Python encoder" seam that Plan 082 opened but didn't close.
- **Plan 081** (V4 Extension): Added V4 pool support to the Rust engine. This plan's changes to `int_solve_v3_v3` and the encoder apply equally to V4-V4 and V3-V4 paths since they use the same `IntV3TickRangeSequence` type.
- **Arbitrage optimizer** (plans/arbitrage-optimizer/): Orthogonal. The optimizer's solver dispatch, accuracy tests, and benchmarking are unchanged. This plan only affects how solver results flow to the encoder.

## Status

[x] Slice 1: Extend Rust simulation functions to return per-hop outputs
[x] Slice 2: Expose per-hop outputs through Python binding (structured format)
[x] Slice 3: Typed HopInfo variants and profitable_results update in Python
[x] Slice 4: Refactor V3-V3 and V3-V4 encoders
[x] Slice 5: Migrate mutual exclusivity to Rust pool keys
[x] Slice 6: Refactor remaining 7 encoders
[x] Slice 7: Validate and clean up
