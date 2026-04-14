# AGENTS.md

## Architecture

### Three-Layer Pattern

Every Rust-accelerated feature follows three layers with strict separation of concerns:

```
┌─────────────────────────────────────┐
│  Python Module (degenbot/foo.py)    │
│  - Public API, docstrings, types    │
│  - Imports from degenbot_rs         │
│  - Python-level convenience methods │
├─────────────────────────────────────┤
│  PyO3 Bindings (foo_py.rs)          │
│  - #[pyclass], #[pyfunction] only   │
│  - Arg extraction → GIL release     │
│  - Result wrapping → Python objects │
├─────────────────────────────────────┤
│  Rust Core (foo.rs)                 │
│  - Zero PyO3 imports                │
│  - Idiomatic Rust, Result<T, E>     │
│  - Independently testable           │
└─────────────────────────────────────┘
```

1. **Python convenience layer** - The user-facing API. Imports from `degenbot_rs` and adds Python-idiomatic methods (rich `__repr__`, `Fraction`-based prices, database lookups, publisher/subscriber). This layer knows nothing about Rust internals.

2. **Thin PyO3 wrappers** - Functions in `*_py.rs` files and `#[pyfunction]` entry points that convert between Rust and Python types. These contain **no business logic** — only argument extraction, GIL release, core function calls, and result wrapping.

3. **Pure Rust core** - Functions with `_internal` suffix or `pub fn name_rust()` that have zero Python dependencies. These are in files without `_py` suffix. They enable:
   - Unit testing without Python
   - Parallel processing without GIL
   - Reuse in non-Python Rust code

Example from `abi_decoder.rs`:
```rust
// Pure Rust core - no PyO3 dependencies
pub fn decode_rust(types: &[&str], data: &[u8]) -> Result<Vec<DecodedValue>, AbiDecodeError> { ... }

// Thin wrapper - GIL released during heavy work
#[pyfunction]
pub fn decode(py: Python<'_>, types: Vec<String>, data: &[u8]) -> PyResult<Py<PyAny>> {
    let values = py.detach(|| decode_rust(&type_refs, data))?;
    // Convert to Python in single pass
}
```

### Module Organization

| File | Purpose |
|------|---------|
| `lib.rs` | Python module entry point, re-exports, `pyo3_log::init()` |
| `errors.rs` | Centralized error types with `thiserror` + `From<->PyErr` conversions |
| `abi_types/` | Unified ABI type/value representation. Directory module with three submodules: `type_` (`AbiType`, `AbiTypeError`, type parsing), `value` (`AbiValue`, integer parsing, string-to-value conversion), `cached` (`CachedAbiTypes`, `value_to_alloy_for_type`, shared `TYPE_CACHE`). Shared canonical type system used by decoder, encoder, and contract modules |
| `abi_decoder.rs` | ABI decoding with pure Rust core + LRU type cache |
| `abi_encoder.rs` | ABI encoding with pure Rust core + LRU type cache |
| `alloy_py.rs` | Newtype wrappers (`PyU256`, `PyI256`) for zero-copy U256/I256 → Python int conversion via `int.from_bytes`; `extract_python_u256` for Python int/bytes → U256 extraction; `abi_value_from_python` for converting arbitrary Python objects into `AbiValue` enums for ABI encoding |
| `py_cache.rs` | Cached Python function/class references (`int.from_bytes`, `HexBytes`) via `PyOnceLock` |
| `tick_math.rs` | Uniswap V3 tick math — pure Rust core (no `pyo3` imports) |
| `tick_math_py.rs` | `PyO3` wrappers for tick math (`get_sqrt_ratio_at_tick`, `get_tick_at_sqrt_ratio`, `extract_u160`) |
| `hex_utils.rs` | Pure-Rust hex encoding/decoding (`decode_hex`, `encode_hex`) — no `PyO3` dependency |
| `py_converters.rs` | Python object converters for RPC types: `log_to_py_dict`, `block_to_py_dict`, `json_to_py_with_hexbytes`; field-aware `HexBytes`/address/int detection; block/transaction/log dict builders. All functions require the GIL (documented as accepted cost) |
| `address_utils.rs` | EIP-55 checksummed addresses |
| `provider.rs` | Ethereum RPC provider (sync, Alloy-based), retry logic, `LogFetcher` |
| `async_provider.rs` | Async provider wrapper for Python via `pyo3-async-runtimes` |
| `contract.rs` | Smart contract interface with `FunctionSignature` parsing |
| `async_contract.rs` | Async contract wrapper with `batch_call` via `join_all` |
| `provider_py.rs` | `PyO3` bindings for provider (`PyAlloyProvider`, `PyLogFilter`) |
| `contract_py.rs` | `PyO3` bindings for contract (`PyContract`, `encode_function_call`, `decode_return_data`, `get_function_selector`) |
| `signature_parser.rs` | Robust recursive-descent function signature parser |
| `runtime.rs` | Shared Tokio runtime singleton |

### Key Design Patterns

- **Shared Multi-Threaded Runtime**: `runtime.rs` provides a singleton Tokio runtime using `Runtime::new()` (multi-threaded scheduler). This is intentional: with Python 3.13+ free-threading, multiple threads can call into Rust provider/contract methods simultaneously. A multi-threaded Tokio runtime enables true parallelism for concurrent I/O-bound RPC calls, while a current-thread runtime would serialize them. Thread count is tunable via `TOKIO_WORKER_THREADS`. The runtime is lazily initialized — pure Rust functions (`tick_math`, `abi_decoder`, `address_utils`) never trigger it.
- **Arc Sharing**: Providers use `Arc<AlloyProvider>` for thread-safe sharing across Python objects. `Contract::clone()` shares the `Arc<RwLock<HashMap>>` signature cache across all clones.
- **Signature Caching**: `Contract` uses `Arc<RwLock<HashMap<String, Arc<FunctionSignature>>>>` for parsed function signatures. The `Arc<FunctionSignature>` value allows cheap returns from the cache without copying the parsed data.
- **GIL Release**: See [GIL Release Protocol](#gil-release-protocol) below.

### Module Naming Convention

Files follow a strict naming convention that signals their `PyO3` dependency:

| Pattern | Meaning | Example |
|---------|---------|--------|
| `foo.rs` | Pure Rust core — zero `pyo3` imports | `tick_math.rs`, `hex_utils.rs`, `provider.rs` |
| `foo_py.rs` | `PyO3` wrappers only — `#[pyfunction]`, `#[pyclass]`, type conversion | `tick_math_py.rs`, `provider_py.rs`, `contract_py.rs` |
| `foo_py.rs` (conversion) | `PyO3`-dependent converters (no `#[pyfunction]`, but creates Python objects) | `py_converters.rs`, `alloy_py.rs`, `py_cache.rs` |

**Rule**: If `pyo3` appears in a file that isn't named `*_py.rs` or isn't a `PyO3`-only conversion module, it's a code smell. Consider splitting the pure Rust logic into a separate file.

**Exception**: `alloy_py.rs` is named with `_py` suffix because its entire purpose is `PyO3` type conversion, even though it doesn't contain `#[pyfunction]` entries.

### ABI Types: String Interface Decision

`AbiType` and `AbiValue` are pure Rust enums that are **not** exposed as `#[pyclass]` types to Python. Python users interact with ABI encoding/decoding through string-based function signatures (e.g., `"transfer(address,uint256)"`) and string result values. This is intentional:
- The string interface matches web3.py convention — Python users expect to pass ABI types as strings
- Exposing `AbiType` as a `#[pyclass]` would add API surface without clear benefit
- Invalid type strings (e.g., `"uint56"`) surface errors at encoding/decoding time, which is the expected behavior

If type-safe ABI construction becomes valuable later, add it as a separate `abi_types_py.rs` module.

## Design Principles for New Rust Modules

These principles distill lessons from Polars, Pydantic, and this codebase's own evolution. Apply them when designing any new Rust-accelerated feature.

### The Nine Rules

1. **Write nice Python** — The Python-facing API should feel native, not like a Rust wrapper
2. **Write nice Rust** — The Rust core should be idiomatic Rust, not Python-accommodating
3. **Thin translator** — The PyO3 layer extracts args → calls Rust → wraps result. Nothing else.
4. **No Python in Rust core** — If `pyo3` appears in a file that isn't `*_py.rs`, it's a code smell
5. **Release the GIL** — Every non-trivial operation should release the GIL
6. **Own your data at the boundary** — Data crossing the boundary must be owned (copied) or Arc-shared, never borrowed
7. **Map errors at the boundary** — Rust errors become Python exceptions in the PyO3 layer, not the core
8. **Cache Python references** — Use `PyOnceLock` for frequently-used Python types/functions
9. **Profile the boundary** — The conversion layer is where overhead lives; optimize it

### The Boundary Mental Model

The PyO3 boundary is not an afterthought — it **is** the architecture. Two fundamentally different memory models meet here:

| Aspect | Python (CPython) | Rust |
|--------|-----------------|------|
| Memory management | Reference counting + GC | Ownership + borrow checker |
| Thread safety | GIL serializes all object access | Send/Sync traits, no global lock |
| Object lifecycle | Live until refcount = 0 | Compile-time determined |
| FFI boundary | Must hold GIL to access Py objects | Frees the GIL for parallelism |

**The key insight**: Every Rust function called from Python enters with the GIL held. The GIL is both a constraint (Python objects can only be touched with it) and an opportunity (releasing it allows true parallelism).

### Bound vs Py — When to Use Each

| Type | GIL Required? | Ownership | When to Use |
|------|--------------|-----------|-------------|
| `Bound<'py, T>` | Yes (lifetime tied to `'py`) | Borrowed reference | Inside GIL-held code, function arguments |
| `Py<T>` | No (independent of GIL) | Owned reference (increments refcount) | Storing across GIL release, struct fields |

**Critical rule**: If you release the GIL (`py.detach()`, `py.allow_threads()`), any `Bound<'py, _>` references become invalid. Extract owned data first:

```rust
// CORRECT: Extract owned data before releasing GIL
fn call(&self, py: Python<'_>, address: &str) -> PyResult<String> {
    let address = address.to_string();  // Copy to owned String
    let provider = Arc::clone(&self.provider);
    let result = py.detach(|| {
        // No Python objects here — only Rust types
        get_runtime().block_on(async { provider.eth_call(&address).await })
    })?;
    Ok(result)
}

// WRONG: Using Bound<'py, _> after detach
fn bad_call(&self, py: Python<'_>, data: &Bound<'_, PyBytes>) -> PyResult<()> {
    let result = py.detach(|| {
        let bytes = data.as_bytes();  // UNDEFINED BEHAVIOR! GIL is released!
    });
}
```

### Own Your Data at the Boundary

Every byte that crosses from Python to Rust must be copied into an owned Rust type. Every Rust result that goes to Python must be constructed into a Python object. This copying is the cost of safety. The specific conversion patterns for each type are documented in [Python↔Rust Type Conversion Protocol](#pythonrust-type-conversion-protocol).

The general rule: **before releasing the GIL, extract all borrowed Python data into owned Rust types. The detached closure must not reference any `Bound<'_, PyAny>`.**

### Arc Sharing Patterns

`Arc<T>` is the sharing mechanism between Python and Rust for long-lived state. Different patterns apply to different use cases:

| Pattern | When to Use | Example | Reference |
|---------|-------------|---------|-----------|
| `Arc<T>` (simple shared handle) | Immutable state shared across PyO3 wrappers — cheap clone, thread-safe | `Arc<AlloyProvider>` in `PyAlloyProvider` | degenbot, Pydantic `Arc<CombinedValidator>` |
| `Arc<RwLock<T>>` (interior mutability) | State that must be updated from Rust without Python (caches, state updates) | `Arc<RwLock<HashMap<String, Arc<FunctionSignature>>>>` in `Contract` | Polars `RwLock<DataFrame>` |
| `Weak<OnceLock<T>>` (recursive references) | Recursive structures where strong references would create cycles | Not yet used — applicable if recursive ABI struct trees are built | Pydantic `DefinitionRef<T>` |
| `ForeignOwner` (Python-owned memory) | Zero-copy access to Python-managed memory (advanced) | Not yet used — applicable if sharing large byte arrays without copy | Polars `SharedStorage::ForeignOwner` |

**degenbot's current `Arc<AlloyProvider>` pattern is exactly right** — it matches Pydantic's `Arc<CombinedValidator>`: shared, immutable, thread-safe, cheap to clone. No need for `RwLock` on providers since they aren't mutated. No cycles, so no need for `Weak`.

### Reference Project Lessons

**Polars** — Zero-copy Arrow bridge:
- Arrow arrays live in Rust-managed memory; Python gets views via the C Data Interface, not copies
- `RwLock<DataFrame>` at the wrapper level allows mutation from Rust methods while `frozen` pyclass prevents Python mutation
- The Rust core crate (`polars-core`) has zero PyO3 imports — all Python interop isolated in `polars-python`
- `SharedStorage` (custom Arc) wraps raw buffers so that cloning/slicing is O(1) and foreign-owned memory (from Python) is kept alive by holding a `Py<PyAny>` reference inside the Rust storage
- **Lesson for degenbot**: If you ever need to share large memory regions without copy (e.g., raw transaction data), Polars' `ForeignOwner` pattern is the blueprint

**Pydantic** — Schema-as-contract + build/run separation:
- Schema compilation (Python → Rust) happens once; validation (Rust → Python) happens many times. Build phase can be slower; run phase must be as fast as possible
- `Arc<CombinedValidator>` shares compiled validator trees; `Weak<OnceLock<T>>` breaks reference cycles for recursive schemas
- `enum_dispatch` provides zero-cost abstraction — the `CombinedValidator` enum dispatches without virtual method overhead
- `CoreSchema` dict as the interface contract avoids tight coupling between Python schema changes and Rust type definitions
- **Lesson for degenbot**: The `Contract` signature cache (`parse once, use many times`) already follows this pattern. Apply the same build/run separation to any new module where setup is expensive but execution is hot-path (e.g., pool state initialization vs. swap calculation)

### Porting Decision Framework

Not everything should be in Rust. Use this framework:

| Criterion | Keep in Python | Port to Rust |
|-----------|---------------|-------------|
| **Hot path?** | Called infrequently | Called in tight loops, per-block, per-tx |
| **GIL bottleneck?** | Already releases GIL via I/O | CPU-bound, holds GIL during computation |
| **Type complexity?** | Heavy Python object manipulation | Pure numeric / byte manipulation |
| **Async?** | Needs deep Python async integration | Network I/O with simple result types |
| **Testability?** | Complex mock setup | Clear input → output functions |
| **Python ecosystem coupling?** | Tightly coupled to SQLAlchemy, web3.py, WeakSet | Standalone logic, no Python object identity |

**What stays in Python**: Database lookups (SQLAlchemy), token identity objects (`Erc20Token`), publisher/subscriber (`WeakSet[Subscriber]`), `Fraction`-based price calculations, registry lookups, Python protocol implementations.

**What ports to Rust**: Pure arithmetic (constant product math, tick math), ABI encoding/decoding, address checksumming, keccak256 operations, RPC I/O with simple result types, byte manipulation.

## Coding Standards

### Error Handling

All error types use `thiserror` with explicit variants and `#[non_exhaustive]`:

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SomeError {
    #[error("Invalid value: {0}")]
    InvalidValue(String),
}

impl From<SomeError> for PyErr {
    fn from(err: SomeError) -> Self {
        PyValueError::new_err(err.to_string())
    }
}
```

#### Error Type Hierarchy

Error types form conversion chains. Follow these rules for new errors:

```
AbiDecodeError ──→ ContractError ──→ ProviderError ──→ PyErr
                  ─────────────────────────────────────→ PyErr (direct, from decoder/encoder)
```

- `AbiDecodeError` → `PyErr` directly when the decoder/encoder is called standalone (maps to `PyValueError`)
- `AbiDecodeError` → `ContractError` when errors flow through contract operations
- `ContractError` → `ProviderError` via the `Other` variant
- `ProviderError` → `PyErr` with **Python-exception-type mapping**:
  - `ProviderError::Timeout` → `PyTimeoutError`
  - `ProviderError::ConnectionFailed` → `PyConnectionError`
  - `ProviderError::RateLimited`, `RpcError`, `InvalidResponse`, `AnvilError`, `Other`, `SerializationError` → `PyRuntimeError`
  - All others → `PyValueError`

When adding a new error type, decide which chain it belongs to. If it can arise in both standalone and contract contexts, implement `From<NewError> for PyErr` directly AND `From<NewError> for ContractError`.

### Linting Rules

From `Cargo.toml`:
- `warnings = "deny"` - All Rust warnings are errors
- `unwrap_used = "deny"` - No `.unwrap()` in production code
- `expect_used = "deny"` - No `.expect()` in production code
- `pedantic = "warn"` - Clippy pedantic lints (warnings, not errors)
- `nursery = "warn"` - Clippy nursery lints (warnings, not errors)
- `missing_errors_doc = "allow"` - Allowed since error docs are covered by `# Errors` sections

In test code, allow explicitly:
```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests { ... }
```

For property-based test modules, use the inner attribute form:
```rust
#[cfg(test)]
mod proptests {
    #![allow(clippy::unwrap_used)]
    // ...
}
```

### Documentation

Module-level docs with `//!`:
```rust
//! Module description.
//!
//! Additional detail on architecture or usage.
```

Function docs with structured sections:
```rust
/// Brief description.
///
/// # Arguments
///
/// * `param` - Description
///
/// # Returns
///
/// Description of return value
///
/// # Errors
///
/// Returns `ErrorType` when...
///
/// # Example
///
/// ```
/// use crate::module::function;
/// let result = function(arg)?;
/// ```
```

### Dependencies

| Crate | Purpose |
|-------|---------|
| `alloy` | Ethereum primitives, RPC types, keccak256 (`full` feature) |
| `pyo3` | Python bindings (`abi3-py312` + `serde` features) |
| `pyo3-async-runtimes` | Async Python interop with Tokio (`tokio-runtime` feature) |
| `pyo3-log` | Rust `log` → Python `logging` bridge, initialized in `lib.rs` via `pyo3_log::init()` |
| `tokio` | Async runtime (`rt-multi-thread` + `time` features) |
| `parking_lot` | High-performance `RwLock` and `Mutex` (no poisoning) |
| `lru` | Bounded LRU caches for ABI type parsing in decoder/encoder |
| `rand` | Jitter for retry backoff (`random_range`) |
| `thiserror` | Error type derivation |
| `serde` | Serialization (`derive` feature) |
| `serde_json` | JSON conversion for RPC responses |
| `futures` | `join_all` for async batch operations |
| `log` | Rust logging facade (emitted to Python via `pyo3-log`) |

**Dev dependencies:**

| Crate | Purpose |
|-------|---------|
| `proptest` | Property-based testing |

**Removed:** `num-bigint` was previously used for U256/I256 → Python int conversion. It has been replaced by `PyU256`/`PyI256` newtype wrappers in `alloy_py.rs` that use `int.from_bytes` for zero-copy conversion.

### Caching Strategy & Global State

The codebase uses three caching patterns. Use this decision framework for new caches:

| Pattern | When to use | Example |
|---------|-------------|---------|
| `LazyLock<parking_lot::Mutex<LruCache<K, V>>>` | Bounded cache of Rust data, accessed from multiple threads without GIL | `abi_types::cached::TYPE_CACHE` (shared by decoder and encoder) |
| `PyOnceLock<Py<PyAny>>` | Python object references (require GIL to create, then cached) | `py_cache::INT_FROM_BYTES`, `py_cache::HEXBYTES_CLASS` |
| `OnceLock<T>` | One-time initialization, never evicted, never resized | `runtime::RUNTIME` |
| `LazyLock<HashSet<&'static str>>` | Immutable constant sets built once | `py_converters::HEXBYTES_FIELDS`, `py_converters::ADDRESS_FIELDS` |

**Capacity policy:** LRU caches use 10,000 entries as the standard capacity (`const XXX_CAPACITY: NonZeroUsize`). New caches should follow this unless there's a measured reason to differ.

**Thread safety model:**
- `parking_lot::Mutex`-protected caches are GIL-independent: safe under Python 3.13+ free-threading
- `PyOnceLock` values are GIL-aware: initialization requires GIL, reads after init are safe from any thread
- All global state must be safe to access from multiple Tokio threads simultaneously

### GIL Release Protocol

The rule is simple: **release the GIL for any I/O-bound work or long CPU work; hold the GIL for Python object construction.**

| Operation | GIL held? | Pattern |
|-----------|-----------|---------|
| Provider RPC calls (sync) | Released | `py.detach(\|\| get_runtime().block_on(async { ... }))` |
| ABI encode/decode (pure Rust) | Released | `py.detach(\|\| decode_rust(...))` |
| Python object creation (PyList, PyDict, HexBytes) | Held | Direct PyO3 API calls |
| RPC type conversion (block/tx/log dicts) | Held | `py_converters` module (inherently requires GIL) |
| Async operations | Released | `future_into_py(py, async { ... })` |
| Post-async Python object construction | Re-acquired | `Python::attach(\|py\| ...)` inside async futures |

**Important:** Copy any borrowed Python data (strings, bytes) into owned Rust types *before* releasing the GIL. The detached closure must not reference any `Bound<'_, PyAny>`.

#### `Python::attach()` Risk

`Python::attach()` is used in async futures (e.g., `async_provider.rs`) to re-acquire the GIL after an `.await` completes, so that Python objects can be constructed from the result. This is the standard pattern for `pyo3-async-runtimes`, but carries a **deadlock risk**: if the Python event loop is in a state that prevents GIL acquisition, the call will hang. Mitigate this by:
- Ensuring the Tokio runtime is sized appropriately via `TOKIO_WORKER_THREADS`
- Never calling sync provider methods from within an async Python context

#### `py_converters` GIL Holding

The `py_converters` module (`log_to_py_dict`, `block_to_py_dict`, `json_to_py_with_hexbytes`) creates Python objects and therefore must hold the GIL. For large blocks with many transactions, this holds the GIL for the duration of object construction. This is an **accepted cost** — Python objects cannot be constructed without the GIL. The actual RPC I/O that precedes these calls already releases the GIL via `py.detach()` in the provider layer.

### PyO3 API Design Conventions

- **`#[pyclass]`** for stateful objects that Python holds references to (e.g., `PyAlloyProvider`, `PyContract`, `PyLogFilter`). Name with `Py` prefix.
- **`#[pyfunction]`** for stateless operations (e.g., `decode`, `encode`, `get_function_selector`). No `Py` prefix.
- **`#[pyo3(signature = (...))]** with defaults for optional parameters. Always specify `signature` explicitly when there are `Option<T>` parameters to control keyword-only vs positional.
- **Sync wrappers** use `py.detach()` + `get_runtime().block_on()`. This blocks a Tokio worker thread — do not hold this pattern for operations that might deadlock with other Tokio tasks on the same runtime.
- **Async wrappers** use `pyo3_async_runtimes::tokio::future_into_py()`. Preferred for any long-running I/O operation.

### Python↔Rust Type Conversion Protocol

#### U256/I256 → Python int
Use `PyU256`/`PyI256` from `alloy_py.rs` or the convenience functions `u256_to_py()` / `i256_to_py()`. These use `int.from_bytes` via `py_cache` for zero-copy conversion without `num-bigint` intermediate allocations.

#### Python int → U256/I256
Use `abi_value_from_python()` in `alloy_py.rs` which handles:
- Small integers via `i128` extraction
- Large integers via Python's `to_bytes()` method
- Bool before int (since `bool` subclasses `int` in Python)

#### Bytes → Python HexBytes
Use `py_cache::create_hexbytes()` which caches the `HexBytes` class reference. This ensures compatibility with web3.py and eth_abi.

#### JSON → Python dict
Use `py_converters::json_to_py_with_hexbytes()` for RPC responses. This does field-aware conversion:
- `HEXBYTES_FIELDS` (hash, input, data, topics, etc.) → `HexBytes` objects
- `ADDRESS_FIELDS` (address, miner, from, to) → EIP-55 checksummed strings
- `NUMERIC_FIELDS` (gas, value, nonce, etc.) → Python `int` via `u256_to_py`
- All other strings → plain `str`
- Field names use **snake_case** for Python consistency

This field mapping is a de facto API contract with Python consumers. Changes to `HEXBYTES_FIELDS`, `ADDRESS_FIELDS`, or `NUMERIC_FIELDS` may break downstream code.

### Sync vs Async API Contract

| Pattern | Module | When to use |
|---------|--------|-------------|
| Sync (blocks calling Python thread) | `provider_py.rs`, `contract_py.rs` | Simple scripts, Jupyter notebooks, code that doesn't need concurrency |
| Async (returns coroutine to Python) | `async_provider.rs`, `async_contract.rs` | Async Python applications, high-throughput batch operations |

**Sync path:** `py.detach()` → `get_runtime().block_on()` — blocks a Tokio worker thread. Do not call sync methods from within an async context or from Tokio tasks, as this can deadlock.

**Async path:** `future_into_py()` — returns a Python awaitable. Use for all long-running I/O in async Python code.

**Batch operations:** The async `batch_call` in `PyAsyncContract` uses `futures::future::join_all` to execute multiple contract calls in parallel. Follow this pattern for new batch operations.

### Logging Bridge

`pyo3_log::init()` is called once in the `#[pymodule]` function in `lib.rs`. This bridges Rust's `log` crate to Python's `logging` module:
- Rust `log::info!()`, `log::warn!()`, etc. → Python logger named after the Rust module
- The bridge is initialized once at import time
- Use `log::debug!()` for high-frequency tracing, `log::warn!()` for recoverable issues

## Performance Guidelines

- Use `const` for compile-time constants (e.g., tick math tables)
- Use `#[inline]` for small, frequently-called functions
- Use `py.detach()` to release GIL during CPU-intensive work
- Pre-allocate vectors with `Vec::with_capacity()` when size is known
- Use `Arc::clone()` (not `.clone()`) to make reference-counting explicit
- Use `CachedAbiTypes` for batch encode/decode operations (avoids repeated type string parsing)
- Use `AbiType::type_str()` which returns `Cow<'static, str>` to avoid allocation for common types like "address", "bool"

## Testing

- Unit tests in `#[cfg(test)]` modules within each file
- Property-based tests using `proptest` for mathematical invariants and roundtrip encode/decode
- Integration tests in `rust/tests/python_integration.rs` (requires `--features auto-initialize`)
- **Roundtrip encode→decode is the standard invariant** for property-based testing of ABI operations
- Run tests: `just test-rust` (runs `cargo test --features auto-initialize -- --test-threads=1`)
- Run linter: `just lint-rust` (runs `cargo clippy --all-targets --all-features -- -D warnings`)
- Property-based test example from `tick_math.rs`:
```rust
#[cfg(test)]
mod proptests {
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn roundtrip_any_valid_tick(tick in -887_272_i32..=887_272_i32) {
            let ratio = get_sqrt_ratio_at_tick_internal(tick)?;
            let tick_back = get_tick_at_sqrt_ratio_internal(ratio)?;
            prop_assert_eq!(tick_back.as_i32(), tick);
        }
    }
}
```

## Build Commands

Uses `just` from the project root (see justfile):

```bash
just test-rust           # Run Rust tests (with auto-initialize feature, single-threaded)
just lint-rust           # Run cargo clippy (all targets, all features, deny warnings)
just build-rust-debug    # Build release library (links Python, no extension-module feature)
just build-rust-extension # Build Python extension (--features extension-module, for distribution)
just dev                 # Build and install Python extension in dev mode (maturin develop)
just test-all            # Run all Rust and Python tests
```

**Note:** `build-rust-debug` uses `cargo build --release` but without the `extension-module` feature. It's "debug" in the sense that it doesn't produce a standalone Python extension — use `build-rust-extension` or `dev` for that.

## Release Profile

`Cargo.toml` `[profile.release]`:
- `lto = "thin"` — cross-crate inlining for smaller/faster binaries
- `strip = true` — strip debug symbols
- `codegen-units = 1` — maximum optimization at cost of compile time

These trade compile time for runtime performance. The `bench` profile inherits `release` with `debug = true` for profiling.

## Solidity/EVM Notes

- All arithmetic matches EVM behavior (256-bit words, wrapping for unsigned)
- Use `alloy::primitives::{U256, I256, Address, B256}` for EVM-native types
- Function selectors: first 4 bytes of `keccak256(signature)`
- ABI encoding uses `abi_encode_params()` (not `abi_encode()`) for correct parameter encoding without extra tuple offset
