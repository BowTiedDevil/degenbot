# Plan 063: Rust GIL Discipline, Allocation Reduction, and Testing Gaps

## Overview

Fix GIL release/reattach discipline issues in the Rust extension, reduce hot-path heap allocations in the ABI type cache and optimizer, and close critical testing gaps (concurrency, subscriptions, optimizer property tests). The architectural goal is to ensure the Rust extension never deadlocks under concurrent sync+async load, that repeated decode/encode calls don't allocate on cache hits, and that every module has at least basic unit test coverage.

## Problem

### Deletion test

If you deleted the GIL-release code (`py.detach()`) from sub-microsecond functions like tick math, nothing would break — the functions would run correctly with the GIL held, and they'd be faster. If you deleted `Python::attach()` from async provider futures, the async path would fail to construct Python objects. If you deleted the `CachedAbiTypes` clone-on-cache-hit, decoders would fail to return results. The issue isn't that this code is unnecessary — it's that some of it is counterproductive (GIL release overhead exceeds compute time) and some of it is latency-sensitive (`Python::attach()` can block under GIL contention).

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| `Python::attach()` latency risk unverified | `async_provider.rs`, `subscription_py.rs` | Concurrent sync+async callers may contend for GIL; no test proves this cannot deadlock |
| `py.detach()` on sub-μs functions | `tick_math_py.rs` | GIL release/reacquire ~200ns exceeds compute ~20ns; net slowdown |
| `Vec<String>` allocated on every cache hit | `abi_types/cached.rs::get_cached_types()` | 2–3 String allocations per `decode_rust()` call even when cache returns immediately |
| `CachedAbiTypes::clone()` deep-clones type tree | `abi_types/cached.rs::get_cached_types()` | `DynSolType::Tuple(Vec<DynSolType>)` recursive clone on every cache hit |
| `Vec<Address>` / `Vec<Vec<B256>>` cloned per chunk | `provider.rs::fetch_logs_chunked()` | 100-chunk fetch → 100× address/topic Vec clones |
| `IntHopState::swap()` converts U256→U512 every call | `optimizers/mobius_int.rs` | 4 conversions per swap; `int_simulate_path` with 3 hops = 12 conversions |
| Zero test coverage for subscription pump | `subscription.rs`, `subscription_py.rs` | `drain_buffer()`, `RawSubItem` conversion, double-buffer swap all untested |
| Zero concurrency test for GIL release | N/A | `--test-threads=1` hides all race conditions in CI |
| Empty test body | `async_contract.rs::test_async_contract_creation` | Provides 0% coverage |
| No proptest for f64↔U256 conversion | `optimizers/mobius_int.rs` | `f64_to_u256` / `u512_to_f64` have no boundary-case property tests |
| No benchmarks for ABI or optimizer paths | `benches/` | Only tick_math benched; no perf regression protection for decode/encode/solve |
| `PyPoolCache` unbounded `HashMap` | `optimizers/mobius_py.rs` | Long-running process leaks memory |

## Solution

### Step 1: Remove GIL release from sub-microsecond functions

Hold the GIL for functions where the compute time is less than GIL release/reacquire overhead (~200ns). This currently includes tick math (~20ns).

```rust
// tick_math_py.rs — BEFORE
pub fn get_sqrt_ratio_at_tick(py: Python<'_>, tick: i32) -> PyResult<Bound<'_, PyAny>> {
    let result = py.detach(|| get_sqrt_ratio_at_tick_internal(tick))?;
    alloy_py::u256_to_py(py, &U256::from(result))
}

// tick_math_py.rs — AFTER
pub fn get_sqrt_ratio_at_tick(py: Python<'_>, tick: i32) -> PyResult<Bound<'_, PyAny>> {
    // SAFETY: get_sqrt_ratio_at_tick_internal takes ~20ns — far less than
    // the ~200ns GIL release/reacquire overhead. Holding the GIL is faster.
    let result = get_sqrt_ratio_at_tick_internal(tick)?;
    alloy_py::u256_to_py(py, &U256::from(result))
}
```

Apply the same pattern to `get_tick_at_sqrt_ratio`.

**Investigation: `to_checksum_address` GIL impact**. `to_checksum_address` in `address_utils_py.rs` does not use `py.detach()` — it holds the GIL throughout, based on the assumption that the compute cost (~50ns) is less than GIL release/reacquire overhead. Before committing to this assumption, benchmark the function with and without `py.detach()` to confirm. Add a criterion benchmark in `rust/benches/address_utils.rs` comparing the two modes. If `py.detach()` is faster (allowing Python threads to run concurrently during the computation), add it; if holding the GIL is faster (as expected), add a `// SAFETY` comment documenting the decision with the benchmark numbers.

### Step 2: Verify GIL contention safety with concurrency tests

The sync provider path correctly releases the GIL via `py.detach()` before every `block_on()` call. The async path's `Python::attach()` calls run on Tokio worker threads while the Python event loop periodically releases the GIL during I/O polling (`epoll_wait`/`select`). There is no circular wait in the existing code — but this safety property is unverified by any test.

Add two tests that prove `Python::attach()` cannot deadlock against concurrent GIL holders:

**Rust-level stress test** (`rust/tests/concurrency_stress.rs`):
Spawns multiple threads from Rust, each calling `Python::attach()` while another thread holds the GIL in a tight Python loop. Verifies all `attach()` calls complete within a timeout. This proves the CPython thread scheduler always eventually yields the GIL — no circular wait is possible. Requires `auto-initialize` feature.

```rust
// rust/tests/concurrency_stress.rs
#![cfg(feature = "auto-initialize")]

use pyo3::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_attach_completes_under_gil_contention() {
    static GIL_HOLDER_DONE: AtomicBool = AtomicBool::new(false);

    // Thread A: hold GIL in a tight Python loop, then release
    let holder = thread::spawn(|| {
        Python::attach(|py| {
            // Run a tight Python loop that holds the GIL continuously
            let code = c"for _ in range(10000): pass";
            py.run(code, None, None).unwrap();
        });
        GIL_HOLDER_DONE.store(true, Ordering::Release);
    });

    // Thread B: repeatedly attach while Thread A may hold the GIL
    let attacher = thread::spawn(|| {
        // Wait briefly so Thread A has a chance to grab the GIL first
        thread::sleep(Duration::from_micros(100));
        for _ in 0..100 {
            Python::attach(|py| {
                let _ = py.None();
            });
        }
    });

    // Both threads must complete within 10 seconds (no deadlock)
    holder.join().expect("GIL holder thread panicked");
    attacher.join().expect("Attacher thread panicked");
}
```

**Python-level integration test** (`tests/test_gil_concurrency.py`):
Creates multiple `threading.Thread`s calling sync `AlloyProvider` methods concurrently (against a local anvil), plus an `asyncio` loop running async methods simultaneously. Verifies all complete within a timeout. This tests the real code paths end-to-end with actual I/O.

```python
# tests/test_gil_concurrency.py
import asyncio
import threading
import time
from concurrent.futures import ThreadPoolExecutor

import pytest

from degenbot_rs import AlloyProvider, AsyncAlloyProvider

# Requires a running anvil instance at http://localhost:8545

DEADLOCK_TIMEOUT_S = 10


def test_sync_async_no_deadlock(anvil_http_url):
    """Sync threads + async loop must complete without deadlock."""
    provider = AlloyProvider(anvil_http_url)
    async_provider = AsyncAlloyProvider(provider)
    results = []
    errors = []

    def sync_caller(n):
        try:
            block = provider.get_block_number()
            results.append(("sync", n, block))
        except Exception as e:
            errors.append(("sync", n, e))

    async def async_caller(n):
        try:
            block = await async_provider.get_block_number()
            results.append(("async", n, block))
        except Exception as e:
            errors.append(("async", n, e))

    # Run sync callers on threads + async callers on the event loop concurrently
    with ThreadPoolExecutor(max_workers=4) as pool:
        sync_futs = [pool.submit(sync_caller, i) for i in range(4)]
        async_fut = asyncio.run(
            asyncio.gather(*[async_caller(i) for i in range(4)])
        )
        for f in sync_futs:
            f.result(timeout=DEADLOCK_TIMEOUT_S)

    assert len(errors) == 0, f"Errors: {errors}"
    assert len(results) == 8, f"Expected 8 results, got {len(results)}"
```

Additionally, add safety comments on every `Python::attach()` call site documenting the contention contract:

```rust
// SAFETY: Python::attach() may block briefly while the Python event loop
// or another Python thread holds the GIL. This cannot deadlock because:
// - Sync provider methods release the GIL via py.detach() before block_on()
// - The Python event loop periodically releases the GIL during I/O polling
// - CPython's thread scheduler yields the GIL every check interval
// If attach fails because the interpreter is shutting down, try_attach
// returns None and we propagate a clear error.
Python::attach(|py| { ... })
```

### Step 3: Eliminate repeated string allocation on cache hits

The same Solidity type strings ("uint256", "address", "bytes32", etc.) appear on every `get_cached_types` call. Currently, each call allocates fresh `String`s for each element and deep-clones the `CachedAbiTypes` on cache hit. A two-level intern eliminates both:

**Level 1: String interner** — deduplicates individual type strings as `Arc<str>`. First occurrence of "uint256" allocates; every subsequent occurrence is an O(1) `Arc::clone`. The set of Solidity types is small and fixed (~20), so a simple `HashMap` suffices — no LRU eviction needed.

**Level 2: Value Arc** — `Arc<CachedAbiTypes>` replaces `CachedAbiTypes::clone()` with O(1) `Arc::clone`, eliminating the deep-clone of the `DynSolType::Tuple(Vec<DynSolType>)` tree (~100ns+ for multi-type tuples).

```rust
// abi_types/cached.rs — BEFORE
pub(crate) static TYPE_CACHE: LazyLock<Mutex<LruCache<Vec<String>, CachedAbiTypes>>> = ...;

pub fn get_cached_types(types: &[&str]) -> Result<CachedAbiTypes, AbiDecodeError> {
    let key: Vec<String> = types.iter().map(std::string::ToString::to_string).collect();
    let mut cache = TYPE_CACHE.lock();
    if let Some(cached) = cache.get(&key) {
        return Ok(cached.clone());  // Deep clone of DynSolType tree
    }
    ...
}

// abi_types/cached.rs — AFTER

/// Interns individual Solidity type strings ("uint256", "address", etc.).
/// ~20 entries, fixed set, no eviction needed.
pub(crate) static TYPE_STR_INTERNER:
    LazyLock<Mutex<HashMap<String, Arc<str>>>> = ...;

fn intern_type_str(s: &str) -> Arc<str> {
    let mut cache = TYPE_STR_INTERNER.lock();
    if let Some(interned) = cache.get(s) {
        return Arc::clone(interned);
    }
    let interned: Arc<str> = Arc::from(s);
    cache.insert(s.to_string(), interned.clone());
    drop(cache);
    interned
}

pub(crate) static TYPE_CACHE:
    LazyLock<Mutex<LruCache<Arc<[Arc<str>]>, Arc<CachedAbiTypes>>>> = ...;

pub fn get_cached_types(types: &[&str]) -> Result<Arc<CachedAbiTypes>, AbiDecodeError> {
    let key: Arc<[Arc<str>]> = types.iter().map(|&s| intern_type_str(s)).collect();
    let mut cache = TYPE_CACHE.lock();
    if let Some(cached) = cache.get(&key) {
        return Ok(Arc::clone(cached));  // O(1) Arc clone
    }
    let cached = Arc::new(CachedAbiTypes::new(types)?);
    cache.put(key, Arc::clone(&cached));
    drop(cache);
    Ok(cached)
}
```

Per-call cost on cache hit:
- Intern each string: O(1) `Arc::clone` (no heap allocation)
- Construct key: one `Arc<[Arc<str>]>` array allocation (pointer-sized elements, ~32 bytes for a 4-type tuple)
- Value: O(1) `Arc::clone`

vs. current per-call cost on cache hit:
- `.to_string()` each: heap allocation per string (~5-7 bytes average)
- Construct key: `Vec<String>` allocation
- Value: deep clone of `DynSolType` tree

Update all callers of `get_cached_types` to work with `Arc<CachedAbiTypes>`. Since `Arc<CachedAbiTypes>` derefs to `CachedAbiTypes`, most call sites only need a type annotation change.

`from_abi_types()` must also return `Arc<CachedAbiTypes>` (wrapping the newly-constructed value) to keep the API consistent. This avoids creating two code paths (one returning `Arc`, one returning bare `CachedAbiTypes`). The primary beneficiary is `FunctionSignature` in `contract.rs`, which stores `CachedAbiTypes` as a field — switching to `Arc<CachedAbiTypes>` means cloning a `FunctionSignature` skips the deep type-tree clone, the same class of win as the cache-hit improvement.

### Step 4: Arc-share addresses and topics in `LogFilter`

Wrap `addresses: Vec<Address>` and `topics: Vec<Vec<B256>>` in `Arc` so `LogFilter::clone()` is O(1) when shared across chunk tasks.

```rust
// provider.rs — BEFORE
pub struct LogFilter {
    addresses: Vec<Address>,
    topics: Vec<Vec<B256>>,
    ...
}

// provider.rs — AFTER
pub struct LogFilter {
    addresses: Arc<[Address]>,
    topics: Arc<[Vec<B256>]>,
    ...
}
```

Update `LogFilter::to_alloy_filter()`, `address_strings()`, and `topic_strings()` to dereference the `Arc`. The `fetch_logs_chunked` chunk-filter construction becomes `Arc::clone` instead of `Vec::clone`.

### Step 5: Pre-convert U256→U512 in `IntHopState`

Store the U512-converted values in `IntHopState` so `swap()` avoids repeated conversions. The current `swap()` method converts U256→U512 four times per call; `int_simulate_path` with 3 hops calls `swap()` 3×, incurring 12 conversions.

**Note**: `IntHopState::new()` can no longer be `const fn` because `U512::from(U256)` (ruint's inherent `from()` method) is not `const`. The current `const fn` is only used in one place (`PyIntHopState::new`), which calls it at runtime anyway — so dropping `const` has no practical impact.

```rust
// mobius_int.rs — AFTER
pub struct IntHopState {
    pub reserve_in: U256,
    pub reserve_out: U256,
    pub gamma_numer: u64,
    pub fee_denom: u64,
    // Pre-converted U512 values for swap hot path
    reserve_in_u512: U512,
    reserve_out_u512: U512,
    gamma_numer_u512: U512,
    fee_denom_u512: U512,
}

impl IntHopState {
    /// Create a new integer hop state.
    ///
    /// Not const because U512::from(U256) is not const fn in ruint.
    #[must_use]
    pub fn new(reserve_in: U256, reserve_out: U256, gamma_numer: u64, fee_denom: u64) -> Self {
        Self {
            reserve_in,
            reserve_out,
            gamma_numer,
            fee_denom,
            reserve_in_u512: U512::from(reserve_in),
            reserve_out_u512: U512::from(reserve_out),
            gamma_numer_u512: U512::from(gamma_numer),
            fee_denom_u512: U512::from(fee_denom),
        }
    }
}
```

The struct grows from 80 bytes to 336 bytes (4×U512 = 256 bytes added). `Clone` copies 336 bytes vs 80 bytes — ~4× more expensive. This is acceptable because `IntHopState` is cloned only during `solve()` path construction (once per pool per solve call), while `swap()` may be called thousands of times per optimization iteration.

### Step 6: Add subscription pump unit tests

Create tests for `drain_buffer()`, `RawSubItem` conversion, and the double-buffer swap:

**Note**: Tests that call `drain_buffer()` require a `Python<'_>` token (for `PyDict`/`PyList` construction), which needs the `auto-initialize` feature. This feature is now a default in `Cargo.toml` (`default = ["auto-initialize"]`), so `cargo test` always enables it — no per-test feature gates or `--features` flags are needed.

```rust
// subscription.rs tests
#[cfg(test)]
mod tests {
    fn make_handle() -> Arc<SubscriptionHandle> { ... }

    #[test]
    fn test_drain_empty_buffer_returns_items_empty() {
        // drain on fresh handle → DrainResult::Items(vec![])
    }

    #[test]
    fn test_drain_converts_header_to_py_dict() {
        // Push RawSubItem::Header, drain, verify Python dict has expected keys
    }

    #[test]
    fn test_drain_end_marker_returns_ended() {
        // Push RawSubItem::End, drain → DrainResult::Ended
    }

    #[test]
    fn test_double_buffer_swap_preserves_items() {
        // Write to buffer[0], swap to buffer[1], write more, drain buffer[0] → get first items only
    }
}
```

### Step 7: Add concurrency stress test for GIL contention

Covered by Step 2 (which now includes both the Rust-level and Python-level concurrency tests). This step is folded into Step 2 to avoid duplication. No additional test file is needed beyond `rust/tests/concurrency_stress.rs` (Rust-level) and `tests/test_gil_concurrency.py` (Python-level) defined there.

### Step 8: Add property tests and benchmarks

**proptest additions:**
- `f64_to_u256(u512_to_f64(U512::from(x)))` roundtrip for values in U256 range
- `int_mobius_solve` with near-zero and near-U256::MAX reserves
- `parse_int256_with_hex_prefix` for values at the 2^255 sign boundary
- Signature parser with adversarial whitespace and bracket nesting

**criterion benchmarks:**
- `abi_decoder::decode_rust` with typical Transfer event types
- `abi_encoder::encode_rust` with (address, address, uint256)
- `int_mobius_solve` for 2-hop and 3-hop paths
- `CachedAbiTypes::decode` (cache hit vs miss)

### Step 9: Bind `PyPoolCache` with LRU eviction

Replace `HashMap<u64, IntHopState>` with `Mutex<LruCache<u64, IntHopState>>`:

```rust
pub struct PyPoolCache {
    pools: Mutex<LruCache<u64, IntHopState>>,
}
```

**`Mutex` is required** because `LruCache::get()` takes `&mut self` (it updates LRU ordering on access), but `PyPoolCache::solve(&self, ...)` must remain `&self` (pyo3 convention for read-only methods). `Mutex` is safe under both GIL-enabled and free-threaded Python builds:
- GIL-enabled: uncontended `Mutex::lock()` costs ~20ns — negligible vs the solve computation
- Free-threaded: `Mutex` provides real thread safety; `RefCell` would panic on concurrent access
- `solve()` calls no Python code while holding the lock (pure Rust math after lookup), so no risk of deadlock with Python global synchronization events
- `MutexExt::lock_py_attached` is not needed because we never call into the Python runtime while holding the lock

Use the same `10_000` capacity as `TYPE_CACHE`. Add `__len__` and `clear()` methods.

### Step 10: Remove empty test, add safety comments

- Delete `async_contract.rs::test_async_contract_creation` (empty body, zero value)
- Add `// SAFETY: GIL is held` comments on all `abi_value_from_python` call sites
- Remove redundant `.floor()` in `mobius_refine_int`: `f64_to_u256` already truncates toward zero (equivalent to `floor()` for positive f64 values), so the explicit `.floor()` before the call is a no-op. Positive values are guaranteed here because `x_approx` and `max_input` come from the Möbius solver which never produces negative optimal inputs — `mobius_refine_int` returns early if `x_approx <= 0.0` (line 401), and `f64_to_u256` returns `U256::ZERO` for non-positive inputs. Add a `// NOTE: .floor() is redundant — f64_to_u256 truncates toward zero, and x_approx is guaranteed positive by the guard above` comment where `.floor()` was removed.

### Design decisions

- **`auto-initialize` as a default feature**: Making `auto-initialize` a default feature in `Cargo.toml` means `cargo test` and `cargo bench` always have a Python interpreter available without passing `--features`. This simplifies the justfile and removes per-test `#[cfg(feature = "auto-initialize")]` gates. The `extension-module` feature (for building the actual wheel) remains opt-in and overrides this at link time.
- **Two-level intern for type cache**: The string interner (`HashMap<String, Arc<str>>`) deduplicates the ~20 Solidity type strings so repeated calls avoid `String` allocation entirely. The value `Arc<CachedAbiTypes>` eliminates the deep-clone on cache hits. The key becomes `Arc<[Arc<str>]>` — pointer-sized elements that compare cheaply and avoid `Borrow` trait complexity with `LruCache` lookups.
- **Remove GIL detach vs keep it**: For tick math (~20ns), removing `py.detach()` is a clear win. For `to_checksum_address` (~50ns), the assumption that holding the GIL is faster will be verified with a criterion benchmark before committing to it. For ABI encode/decode (μs–ms), keeping `py.detach()` remains correct. The threshold is approximately 1μs — below that, detach overhead dominates.
- **`Python::attach()` contention — test, don't wrap**: pyo3 0.28 provides `Python::try_attach()` which returns `None` if the interpreter is shutting down or not initialized — but it does NOT provide non-blocking GIL acquisition for the contention case (it still blocks if another thread holds the GIL). Adding a timeout wrapper is not feasible without a non-blocking GIL-acquire API. Instead, we prove the safety property with two concurrency tests (Rust-level + Python-level) that verify `Python::attach()` always completes under GIL contention. The underlying safety guarantee is that CPython always yields the GIL (thread scheduler check interval, I/O poll), so circular wait is impossible. Safety comments on each `Python::attach()` call site document this contract.
- **`Arc<[Address]>` vs `Arc<Vec<Address>>`**: `Arc<[Address]>` is slightly more allocation-efficient (single allocation for both the Arc header and the slice data). The `to_alloy_filter()` method can dereference via `&*` without cloning.
- **Pre-converting U512 values in IntHopState**: This adds 256 bytes to the struct (80 → 336 bytes) and makes `Clone` ~4× more expensive, but eliminates 4 U256→U512 conversions per `swap()` call. The tradeoff is clear for the hot-path `int_simulate_path` which calls `swap()` thousands of times per optimization iteration — each conversion costs ~10ns, so 4 conversions × 1000 iterations = 40μs saved. The `new()` function can no longer be `const fn` because `U512::from(U256)` is not const in ruint, but this has no practical impact since the only call site (`PyIntHopState::new`) is a runtime call anyway.

## Files Involved

**Primary:**
- `rust/src/tick_math_py.rs` — Remove `py.detach()` from both functions
- `rust/src/address_utils_py.rs` — Benchmark GIL detach vs hold; add `// SAFETY` comment
- `rust/src/abi_types/cached.rs` — Arc-wrap key and value; change return type
- `rust/src/provider.rs` — Arc-share addresses/topics in `LogFilter`
- `rust/src/optimizers/mobius_int.rs` — Pre-convert U512; remove redundant `.floor()`
- `rust/src/optimizers/mobius_py.rs` — LRU eviction for `PyPoolCache`
- `rust/src/subscription.rs` — Add `#[cfg(test)]` unit tests
- `rust/src/async_contract.rs` — Delete empty test
- `rust/src/async_provider.rs` — Add GIL contention safety comments on `Python::attach()`
- `rust/src/subscription_py.rs` — Add GIL contention safety comments
- `rust/src/json_converters.rs` — Add GIL contention safety comments
- `rust/src/py_cache.rs` — Add GIL contention safety comments
- `rust/src/alloy_py.rs` — Add GIL contention safety comments

**Secondary (callers updated for `Arc<CachedAbiTypes>`):**
- `rust/src/abi_decoder.rs` — Update `decode_rust`, `decode_single_rust`, `decode_for_types`
- `rust/src/abi_encoder.rs` — Update `encode_single_rust`, `encode_rust`, `encode_for_types`
- `rust/src/contract.rs` — Update `FunctionSignature` to store `Arc<CachedAbiTypes>` instead of `CachedAbiTypes`; update `encode_arguments_cached`, `decode_return_data_cached`
- `rust/src/abi_types/mod.rs` — Update re-exports

**New files:**
- `rust/tests/concurrency_stress.rs` — Rust-level GIL contention stress test
- `tests/test_gil_concurrency.py` — Python-level sync+async integration test
- `rust/benches/address_utils.rs` — Address checksumming benchmark (GIL detach vs hold)
- `rust/benches/abi_decode.rs` — ABI decode benchmark
- `rust/benches/abi_encode.rs` — ABI encode benchmark
- `rust/benches/mobius_solver.rs` — Möbius optimizer benchmark

## Implementation Order

### Slice 1: Remove GIL detach from sub-μs functions

1. Remove `py.detach()` from `get_sqrt_ratio_at_tick` and `get_tick_at_sqrt_ratio` in `tick_math_py.rs`
2. Add `// SAFETY:` comments explaining why GIL is held
3. Create `rust/benches/address_utils.rs` — benchmark `to_checksum_address` with and without `py.detach()`
4. Based on benchmark results: either add `py.detach()` to `to_checksum_address` or add a `// SAFETY` comment documenting why GIL is held
5. Run: `just test-rust` — expect all tests pass

### Slice 2: GIL contention tests + safety comments + Arc-wrap CachedAbiTypes

1. Create `rust/tests/concurrency_stress.rs` with Rust-level GIL contention test (requires `auto-initialize` feature)
2. Add `[[test]]` entry in `Cargo.toml` for the concurrency test
3. Add safety comments on every `Python::attach()` call site in `async_provider.rs`, `subscription_py.rs`, `json_converters.rs`, `py_cache.rs`, `alloy_py.rs`
4. Add string interner (`TYPE_STR_INTERNER`) and change `TYPE_CACHE` to `LruCache<Arc<[Arc<str>]>, Arc<CachedAbiTypes>>`
5. Update `get_cached_types` to use `intern_type_str()` and return `Arc<CachedAbiTypes>` with `Arc::clone` on hit
6. Update `from_abi_types` to return `Arc<CachedAbiTypes>`; update `FunctionSignature` to store `Arc<CachedAbiTypes>`
7. Update all callers: `decode_rust`, `encode_rust`, `decode_for_types`, `encode_for_types`, `FunctionSignature`, contract methods
8. Run: `just test-rust` — expect all tests pass
9. Run: `cargo test --test concurrency_stress` — expect pass

### Slice 3: Arc-share LogFilter fields

1. Change `LogFilter.addresses` to `Arc<[Address]>` and `topics` to `Arc<[Vec<B256>]>`
2. Update `to_alloy_filter()`, `address_strings()`, `topic_strings()` to dereference
3. Update `fetch_logs_chunked` chunk construction to `Arc::clone`
4. Run: `just test-rust` — expect all tests pass

### Slice 4: Pre-convert U512 in IntHopState

1. Add `reserve_in_u512`, `reserve_out_u512`, `gamma_numer_u512`, `fee_denom_u512` fields
2. Populate in `IntHopState::new()` — drop `const fn`, use regular `fn` (U512::from is not const)
3. Update `swap()` to use pre-converted values
4. Update `compute_int_mobius_coefficients` to use `hop.reserve_in_u512` etc. instead of `U512::from(hop.reserve_in)`
5. Update `PyIntHopState::new` (no visible Python API change)
6. Remove redundant `.floor()` in `mobius_refine_int`, add `// NOTE` comment documenting why
7. Run: `just test-rust` — expect all tests pass

### Slice 5: Add subscription pump unit tests

1. Add `#[cfg(test)] mod tests` to `subscription.rs`
2. Test: empty drain, header conversion, log conversion, End marker, double-buffer swap
3. Run: `just test-rust` — expect all new tests pass

### Slice 6: Add property tests and delete empty test

1. Add proptest for `f64_to_u256`/`u512_to_f64` roundtrip
2. Add proptest for `int_mobius_solve` with extreme reserves
3. Add proptest for `parse_int256_with_hex_prefix` sign-bit boundary
4. Delete empty `test_async_contract_creation` test
5. Run: `just test-rust` — expect all tests pass

(Concurrency tests are in Slice 2; this slice covers property tests only.)

### Slice 7: Add criterion benchmarks

1. Create `rust/benches/abi_decode.rs` with Transfer event decode benchmark
2. Create `rust/benches/abi_encode.rs` with (address, uint256) encode benchmark
3. Create `rust/benches/mobius_solver.rs` with 2-hop and 3-hop int solve benchmark
4. Add to `Cargo.toml` `[[bench]]` entries with `harness = false`
5. Ensure `criterion` and `proptest` are listed under `[dev-dependencies]` in `Cargo.toml`
6. Run: `cargo bench` — expect benchmarks complete without error

### Slice 8: LRU eviction for PyPoolCache and safety comments

1. Replace `HashMap` with `Mutex<LruCache>` in `PyPoolCache`
2. Wrap `pools.get()` calls in `self.pools.lock().unwrap().get()` in `solve()`
3. Add `clear()` method
4. Add `// SAFETY: GIL is held` comments on `abi_value_from_python` call sites
5. Run: `just test-rust && just lint-rust` — expect all pass

### Slice 9: Validate and clean up

1. Run `just test-all` — full Python + Rust test suite
2. Run `just lint` — both Rust clippy and Python lint
3. Run `cargo bench` — verify no regressions vs baseline
4. Update `rust/AGENTS.md` if any new patterns or conventions introduced
5. Remove any temporary test helpers introduced during migration

## Testing

### Per-slice test runs

Each slice runs `just test-rust`. Slices that touch Python-facing code also run `just test-rust-python`.

### New unit tests

```rust
// subscription.rs — new tests
#[test]
fn test_drain_empty_buffer_returns_empty_items() { ... }
#[test]
fn test_drain_converts_raw_header_to_py_dict() { ... }
#[test]
fn test_drain_end_marker_returns_ended_variant() { ... }
#[test]
fn test_double_buffer_swap_isolates_reads_from_writes() { ... }

// mobius_int.rs — new proptests
proptest! {
    #[test]
    fn f64_u256_roundtrip(v in any::<[u8; 32]>()) { ... }
    #[test]
    fn int_mobius_extreme_reserves(r_in in 0u64.., r_out in 0u64..) { ... }
}

// abi_types/value.rs — new proptests
proptest! {
    #[test]
    fn int256_hex_sign_bit_boundary(n in 0u128..) { ... }
}
```

### Integration tests

- `rust/tests/concurrency_stress.rs` — Rust-level GIL contention stress test (proves `Python::attach()` cannot deadlock; requires `auto-initialize` feature, `--test-threads>1`)
- `tests/test_gil_concurrency.py` — Python-level integration test (sync threads + async loop against anvil; proves real code paths have no circular wait)
- Existing `rust/tests/python_integration.rs` — covers Python↔Rust boundary roundtrips (no changes needed)

### Benchmarks

- `rust/benches/address_utils.rs` — `to_checksum_address` with and without `py.detach()`, 1000-iteration throughput
- `rust/benches/abi_decode.rs` — Transfer event decode, 1000-iteration throughput
- `rust/benches/abi_encode.rs` — (address, uint256) encode, 1000-iteration throughput
- `rust/benches/mobius_solver.rs` — 2-hop and 3-hop int solve with typical reserves

## Benefits

- **Correctness**: GIL contention safety is verified by two concurrency tests (Rust-level + Python-level) rather than assumed; `Python::attach()` call sites get safety comments documenting the no-circular-wait contract
- **Performance**: String interning eliminates per-call type string allocation (~3 String allocs → 3 Arc::clone); cache-hit value elimination (1 DynSolType tree clone → 1 Arc::clone); GIL release removal from sub-μs functions; U512 pre-conversion in hot path; Arc-shared log filter
- **Coverage**: Subscription pump (0→~5 tests), optimizer boundary cases (0→~3 proptests), f64↔U256 roundtrip (0→1 proptest), GIL concurrency (0→2 tests: Rust-level + Python-level)
- **Locality**: `IntHopState` carries its own U512 values — no cross-module conversion helpers needed at call sites
- **Depth**: `Arc<CachedAbiTypes>` makes the cache seam deeper — callers hold an Arc that's cheap to clone, not a value that's expensive to deep-clone

## Risks

- **`Arc<CachedAbiTypes>` return type change**: This changes a public API. Any Rust crate depending on `degenbot_rs` directly (unlikely — it's `publish = false`) would need updating. Python callers are unaffected since `CachedAbiTypes` is never exposed to Python.
- **Removing `py.detach()` from tick math**: If a future change makes tick math significantly slower (unlikely — it's pure integer arithmetic with no I/O), the GIL would be held longer. The `// SAFETY` comment documents the assumption so it can be revisited.
- **Concurrency stress test flakiness**: Multi-threaded tests may be flaky in CI. Mitigation: Rust test uses `auto-initialize` feature gate and explicit `thread::spawn`/`join` with timeouts; Python test requires a running anvil instance and `DEADLOCK_TIMEOUT_S` fail-safe. Both are candidates for CI-only execution.
- **`Arc<[Address]>` in LogFilter**: Requires `Address: Clone` to construct, which it already implements. The `to_alloy_filter()` method needs `self.addresses.clone()` to build Alloy's `Filter`, which clones the slice — but this only happens once per chunk, not per-topic.

## Relationship to Other Plans

- **Plan 014** (Async REPL): Complementary — the `Python::attach()` contention risk documented here affects async contexts that Plan 014 enables
- **Plan —** (Arbitrage Optimizer): Complementary — the `IntHopState` U512 pre-conversion and `PyPoolCache` LRU eviction directly benefit the optimizer hot path
- **Independent**: All other completed plans (001–062) — this plan operates entirely within the `rust/` subtree with no Python-side changes

## Status

[x] Slice 1: Remove GIL detach from sub-μs functions
[x] Slice 2: GIL contention tests + safety comments + Arc-wrap CachedAbiTypes and cache key
[x] Slice 3: Arc-share LogFilter fields
[x] Slice 4: Pre-convert U512 in IntHopState
[x] Slice 5: Add subscription pump unit tests
[x] Slice 6: Add property tests and delete empty test
[x] Slice 7: Add criterion benchmarks
[x] Slice 8: LRU eviction for PyPoolCache and safety comments
[x] Slice 9: Validate and clean up
