# Rust Extension Context

## Language

**GIL Discipline**: The set of rules governing when the Global Interpreter Lock is held vs released in PyO3-wrapped functions. Sub-μs pure-compute functions hold the GIL; I/O-bound functions release it via `py.detach()`.
_Avoid_: GIL management, thread safety, locking strategy

**SAFETY Comment**: A `// SAFETY:` comment on every `Python::attach()` call site documenting why no circular wait (deadlock) can occur — specifically that the sync path releases the GIL before `block_on()` and the async path's `attach()` runs on Tokio worker threads while Python releases the GIL during I/O polling.
_Avoid_: thread safety comment, lock comment

**Type Cache Key**: `Arc<[Arc<str>]>` — the lookup key for the two-level `CachedAbiTypes` intern. `Arc<str>` elements are interned via `TYPE_STR_INTERNER`; the outer `Arc<[...]>` is a thin pointer for cheap comparison and `Borrow` compatibility with `LruCache`.
_Avoid_: cache key, type key, intern key

**String Interner**: `TYPE_STR_INTERNER` — a global `LazyLock<Mutex<HashMap<String, Arc<str>>>>` that deduplicates the ~20 Solidity/EVM type strings (e.g., `"uint256"`, `"address"`) into `Arc<str>` values. Eliminates per-call `String` allocation.
_Avoid_: string cache, type string pool

**drain_raw()**: A pure-Rust method on `SubscriptionHandle` that swaps the double buffer and returns `RawDrainResult` without touching the GIL. The Python-facing `drain_buffer()` wraps it with `Python::attach()`.
_Avoid_: raw drain, buffer swap

**IntHopState**: The solver's integer hop state carrying pre-converted U512 fields (`fee_over_fee_scale`, `scaled_reserve_in`, `scaled_reserve_out`). Constructed once from U256 values; `swap()` and `compute_int_mobius_coefficients()` use U512 arithmetic directly, eliminating per-call conversions.
_Avoid_: hop state, integer state, mobius state

**PyPoolCache**: `parking_lot::Mutex<LruCache<u64, IntHopState>>` — a bounded (10K entry) LRU cache keyed by pool ID, replacing the former unbounded `HashMap`. Uses `parking_lot::Mutex` (no poisoning, returns `MutexGuard` directly from `lock()`) instead of `std::sync::Mutex` or `RefCell`.
_Avoid_: pool cache, solver cache, rust cache

## Relationships

- **String Interner** → **Type Cache Key**: The interner produces `Arc<str>` values that are collected into `Arc<[Arc<str>]>` keys for `LruCache` lookups
- **Type Cache Key** → **`Arc<CachedAbiTypes>`**: On cache hit, `Arc::clone` returns an O(1) reference instead of deep-cloning the `DynSolType` tree
- **`Arc<CachedAbiTypes>`** → **`FunctionSignature`**: `FunctionSignature` stores `Option<Arc<CachedAbiTypes>>` so cloning a signature is O(1) rather than O(tree depth)
- **`parking_lot::Mutex`** → **`PyPoolCache`**: Avoids poisoning (`lock()` returns `MutexGuard` directly, not `LockResult`), sidestepping `clippy::expect_used`/`clippy::unwrap_used` lints
- **`IntHopState`** → **`PyPoolCache`**: Pre-converted U512 values stored in the cache, eliminating per-swap U256→U512 conversions
- **`drain_raw()`** → **`drain_buffer()`**: Pure-Rust buffer mechanics extracted from the Python-touching method for testability without `with_embedded_python_interpreter` (which can only be called once per process)
- **`py.detach()`** → **async provider**: GIL released before `block_on()` calls; this is the no-circular-wait guarantee that makes `Python::attach()` safe in the async path
- **`auto-initialize` feature** → **concurrency tests**: Default Cargo feature that auto-initializes the Python interpreter; required for `concurrency_stress.rs` and integration tests

## Resolved Ambiguities

### GIL release vs GIL hold

**Ruling: Hold the GIL for sub-μs compute; release for I/O.** The threshold is empirical: GIL release/reacquire costs ~200ns. Any function completing in under 200ns (tick math ~20ns, address utils ~50ns) must hold the GIL. I/O-bound operations (async provider `block_on()`) must release it. The decision is documented per call site with `// SAFETY:` comments.

### parking_lot::Mutex vs std::sync::Mutex vs RefCell

**Ruling: `parking_lot::Mutex` for all Rust extension interior mutability.** `RefCell` is unsafe under free-threaded Python 3.14+ (no GIL guarantee). `std::sync::Mutex` poisons on panic, requiring `.expect()`/`.unwrap()` that violate strict clippy. `parking_lot::Mutex` avoids both issues: no poisoning, direct `MutexGuard` return.

### LruCache vs HashMap for PyPoolCache

**Ruling: `LruCache` with 10K capacity.** Unbounded `HashMap` causes memory leaks in long-running processes. LruCache evicts the least-recently-used entry when full. 10K entries is sufficient for typical arbitrage workloads (~100 pools) with headroom.

### f64_to_u256 decomposition

**Ruling: Iterative 4-limb decomposition.** The previous 2-limb decomposition (`hi * 2^64 + lo`) silently produced wrong results for values exceeding 128 bits. The 4-limb iterative decomposition correctly handles the full U256 range. f64's 52-bit mantissa limits round-trip precision to ~15-16 significant digits; the lower ~61 digits of a 77-digit U256 are lost in the float conversion (inherent to f64, not fixable).
