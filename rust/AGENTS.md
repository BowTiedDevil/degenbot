# AGENTS.md

## Architecture

### Two-Layer Pattern
The codebase uses a clean separation between pure Rust logic and Python bindings:

1. **Pure Rust core** - Functions with `_internal` suffix or `pub fn name_rust()` that have zero Python dependencies. These enable:
   - Unit testing without Python
   - Parallel processing without GIL
   - Reuse in non-Python Rust code

2. **Thin PyO3 wrappers** - Functions in `*_py.rs` files and `#[pyfunction]` entry points that convert between Rust and Python types

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
| `lib.rs` | Python module entry point, re-exports |
| `errors.rs` | Centralized error types with `thiserror` |
| `abi_types.rs` | Unified ABI type representation (shared by decoder and contract) |
| `abi_decoder.rs` | ABI decoding with pure Rust core |
| `tick_math.rs` | Uniswap V3 tick math calculations |
| `address_utils.rs` | EIP-55 checksummed addresses |
| `provider.rs` | Ethereum RPC provider (sync, Alloy-based) |
| `async_provider.rs` | Async provider wrapper for Python |
| `contract.rs` | Smart contract interface |
| `async_contract.rs` | Async contract wrapper for Python |
| `provider_py.rs` | PyO3 bindings for provider |
| `contract_py.rs` | PyO3 bindings for contract |
| `fast_hexbytes.rs` | High-performance hex/bytes type with pre-computed hex |
| `runtime.rs` | Shared Tokio runtime singleton |
| `signature_parser.rs` | Robust function signature parsing |
| `utils.rs` | JSON-to-Python conversion utilities |

### Key Design Patterns

- **Shared Multi-Threaded Runtime**: `runtime.rs` provides a singleton Tokio runtime using `Runtime::new()` (multi-threaded scheduler). This is intentional: with Python 3.13+ free-threading, multiple threads can call into Rust provider/contract methods simultaneously. A multi-threaded Tokio runtime enables true parallelism for concurrent I/O-bound RPC calls, while a current-thread runtime would serialize them. Thread count is tunable via `TOKIO_WORKER_THREADS`. The runtime is lazily initialized — pure Rust functions (`tick_math`, `abi_decoder`, `address_utils`) never trigger it.
- **Arc Sharing**: Providers use `Arc<AlloyProvider>` for thread-safe sharing across Python objects
- **Signature Caching**: `Contract` uses `parking_lot::RwLock<HashMap>` for parsed function signatures
- **GIL Release**: Use `py.detach(|| ...)` for CPU-intensive work to allow Python parallelism

## Coding Standards

### Error Handling

All error types use `thiserror` with explicit variants:

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

### Linting Rules

From `Cargo.toml`:
- `warnings = "deny"` - All Rust warnings are errors
- `unwrap_used = "deny"` - No `.unwrap()` in production code
- `expect_used = "deny"` - No `.expect()` in production code
- `pedantic` and `nursery` lints enabled

In test code, allow explicitly:
```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests { ... }
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
| `alloy` | Ethereum primitives, RPC types, keccak256 |
| `pyo3` | Python bindings (`abi3-py312` feature) |
| `pyo3-async-runtimes` | Async Python interop with Tokio |
| `tokio` | Async runtime (multi-threaded) |
| `parking_lot` | High-performance RwLock |
| `num-bigint` | Arbitrary precision integers (Python int interop) |
| `thiserror` | Error type derivation |
| `serde_json` | JSON conversion for RPC responses |
| `proptest` | Property-based testing |

### Performance Guidelines

- Use `const` for compile-time constants (e.g., tick math tables)
- Use `#[inline]` for small, frequently-called functions
- Use `py.detach()` to release GIL during CPU-intensive work
- Pre-allocate vectors with `Vec::with_capacity()` when size is known
- Use `Arc::clone()` (not `.clone()`) to make reference-counting explicit

### Testing

- Unit tests in `#[cfg(test)]` modules within each file
- Property-based tests using `proptest` for mathematical invariants
- Run tests with `cargo test` (no flags needed)
- Example from `tick_math.rs`:
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
just test-rust           # Run Rust tests
just lint-rust           # Run Rust linter (clippy)
just build-rust-debug    # Build release library (links Python)
just build-rust-extension # Build Python extension (correct for distribution)
just dev                 # Build and install Python extension in dev mode
just test-all            # Run all Rust and Python tests
```

## Solidity/EVM Notes

- All arithmetic matches EVM behavior (256-bit words, wrapping for unsigned)
- Use `alloy::primitives::{U256, I256, Address, B256}` for EVM-native types
- Function selectors: first 4 bytes of `keccak256(signature)`
