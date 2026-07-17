# degenbot — Domain Glossary

Ubiquitous language for the degenbot codebase. Terms here are the canonical
names used in architecture reviews (the `/improve-codebase-architecture`
skill), ADRs, and the three-layer transition. Keep this current as
deepening decisions crystallize.

## Architecture (from `/codebase-design` vocabulary)

- **Module** — anything with an interface and an implementation (function, crate, package, tier-spanning slice). Not "component"/"service."
- **Interface** — everything a caller must know to use a module: types, invariants, ordering, error modes, config, performance. Not "API"/"signature."
- **Depth** — leverage at the interface: behaviour per unit of interface. **Deep** = much behaviour behind a small interface; **shallow** = interface nearly as complex as the implementation.
- **Seam** — where a module's interface lives; a place to alter behaviour without editing in that place. Not "boundary."
- **Adapter** — a concrete thing satisfying an interface at a seam (role, not substance).
- **Leverage** — what callers get from depth (capability per unit of interface). **Locality** — what maintainers get (change/bugs/knowledge concentrate in one place).

## Three-layer system (ADR-005)

- **Rust core** — `rust/crates/degenbot-{core,-cl-math,-curve-math,-balancer-math,-abi,-decoders,-uniswap,-rpc,-bot,-pools,-db,-simulation,-submission,-price,-solvers,-executor,-fork,-pathfinding,-pool-updater,-aave-updater,-solidly-math,-evm-math,-v2-math}`. Zero `pyo3` (enforced by `just check-no-pyo3-in-cores`).
- **PyO3 wrapper** — `rust/crates/degenbot-python/src/<domain>/**`. `#[pyclass]`/`#[pyfunction]` only — arg extract → GIL release → core call → result wrap. No business logic.
- **Python companion** — `src/degenbot/**`. User-facing API, docstrings, I/O orchestration, immutable config dual-tracking, `Fraction`-based display.

### Construction-I/O executor

**`PyBotIo`** (Rust `#[pyclass]`, `degenbot.bot.PyBotIo`) is the sole
construction-I/O executor: every builder's `build()`/`update()` and the
type-resolution + tick-fetcher paths receive `io: PyBotIo` and call
`io.fetch_X()` / `io.probe_X()` directly (ADR-005 slice 14). The Python
`PoolIO`/`SyncPoolIO`/`AsyncPoolIO` protocols and the encode→call→decode
parity-gate fallbacks are deleted; `AsyncBot` and the async builders are
retired (sync `Bot` + `PyBotIo` is the only construction path). `PyBotIo`
also implements the 7-method generic RPC surface (`call`/`call_raw`/
`get_block*`/`get_code`/`get_balance`) used by the Curve detection modules.
`AsyncAlloyProvider` survives for the pump/subscribe/verify loop only —
never for construction.

## The `_ffi` seam (Pydantic barrier — DECIDED)

**Decision:** `degenbot._ffi` is **private** — a raw Rust extension imported by ONE barrier per domain, never by leaf code. Model: pydantic-core (`_pydantic_core` is imported only by `pydantic_core/__init__.py`; the companion `pydantic` never touches it). Replaces degenbot's prior mixed state (ban test + allowlist back-door + direct `_ffi.<sub>` leaf imports).

- **Ban rule (target):** "no file outside its domain's barrier module may contain `degenbot._ffi`." Mechanically enforceable; no allowlist, no submodule-vs-symbol distinction.
- **Home placement:** 1:1 mirror — every consumed `_ffi.<sub>` maps to a `degenbot.<domain>` home. Cross-cutting concerns are elevated to first-class domains (not a `common` junk-drawer), but homes are created lazily on first Python consumer (no empty pass-throughs for dead submodules).
- **Survey basis:** Polars (leaf-imports `_plr` freely, no ban), Pydantic (strict one-barrier, `_` truly private), cryptography (`bindings/_rust` namespaced), Ruff (thin CLI, N/A). Pydantic is the match because degenbot's ban test already signals "private" intent.

## Dispositions (per `_ffi.<sub>`)

### `deployments` — STAYS, correctly placed

- **Home:** `degenbot.uniswap::deployments` (Rust) → `degenbot.uniswap.deployments` (Python mirror).
- **Not eliminated.** The factory→identity lookup (`resolve_deployer`, `resolve_v2/v3_init_hash`, `verify_v2/v3`) is the standalone-Rust-core verification mechanism (ADR-005 / Fork A, JC6OFG): `register_v2/v3_pool` re-resolves `(deployer, init_hash)` from the embedded JSON and verifies the CREATE2 address at registration time. A standalone `Bot` verifies with no Python; if the builder carried identity in, Rust would trust rather than verify. Presets cannot replace it.
- **Not cross-cutting.** PancakeSwap/SushiSwap/Swapbased/Camelot/Aerodrome are Uniswap-V2/V3 protocol forks; their deployment identity is Uniswap-protocol-family data. One boundary handling all V2-style DEXes via `factory + variant_tag` is slice 7's deliberate collapse. `degenbot-uniswap::deployments` is the Uniswap family's identity module, not a cross-cutting registry.
- **Python work:** reroute `_ffi.deployments` leaf imports → `degenbot.uniswap.deployments`. No Rust structural change.
- **Deferred:** 11 Balancer factory rows in the JSON for "exhaustive lookup" — the one cross-family leak. Carve out to `degenbot.balancer.deployments` when Balancer gets an identity crate (today only `degenbot-balancer-math` exists).
- **Deferred:** whether `resolve_*` / `verify_*` free functions become methods on a typed `DeploymentRegistry` is a deepening question, not a mirror dependency.

### Cross-cutting submodules — 1:1 mirror, UNIFORM

**Decision (uniform):** every consumed `_ffi.<sub>` maps to a `degenbot.<sub>` home at the top level — including cross-cutting concerns, which are elevated to first-class domains (not a `common` junk-drawer). Homes are created lazily on first Python consumer; dead submodules stay un-homed. Top-level grab-bag `.py` files dissolve *into* their mirror home (the file's content moves into the package, not preserved as a floating peer).

- **`abi`** → `degenbot.abi`. The home bridges the Rust `degenbot-abi` core (encode/decode/decode_single) with EIP-55 checksumming. No `eth_abi` fallback — Rust is the only backend. Consumers: `contract/decoding.py`, plus `aerodrome`/`aave`/`builders/*` via `degenbot.abi`.
- **`contract`** → `degenbot.contract` (already a package). `crypto.py` reaches `_ffi.contract` for `get_function_selector`; reroute to `degenbot.contract.get_function_selector`.
- **`crypto`** → `degenbot.crypto`. Top-level `crypto.py` (81 lines: `function_selector`, `keccak256`, `event_topic`) becomes `degenbot.crypto`. Note: `function_selector` currently delegates to `_ffi.contract`; under the mirror it re-exports from `degenbot.contract`. `keccak256` is the one remaining non-Rust hashing surface (wraps `eth_utils.crypto.keccak`) — a follow-up exposes a Rust `keccak256` pyfunction.
- **`fork`** → `degenbot.fork`. Top-level `anvil_fork.py` (514 lines) becomes `degenbot.fork`, mirroring `_ffi.fork`.
- **`db`** → `degenbot.db` (genuine cross-cutting infrastructure — DB row types + ops, consumed by `database/`, `cli/`, `exceptions/`, `updater/`). Existing `database/_ffi.py` is the partial barrier; consolidate to `degenbot.db` as the single mirror home.
- **`deployments`** → `degenbot.uniswap.deployments` (see above — Uniswap-protocol-family identity, not cross-cutting).
- **`price`** → **not a single home.** `_ffi.price` exposes two distinct pyclasses consumed by two different domains: `PyChainlinkPriceFeed` → `degenbot.chainlink` (already re-exported from `chainlink/__init__.py`), `PyAavePriceOracle` → `degenbot.aave` (already re-exported from `aave/__init__.py`). The Rust crate `degenbot-price` is implementation; its pyclasses belong to their consuming domains, not a shared `degenbot.price`.

### Dead / test-only submodules (un-homed, lazy)

- **`executor`** — 0 callers anywhere (production or test). Truly dead surface. Stays un-homed; a `degenbot.executor` home appears only if a Python consumer lands. (The `contracts/` Vyper executor + `degenbot-executor` Rust crate exist, but no Python leaf reaches `_ffi.executor`.)
- **`subscriber`** — 0 production callers; test-only (`tests/fakes/subscribers.py`, `tests/test_pubsub_seam_parity.py` reach `_ffi.subscriber` for `PySubscription` / `register_subscriber`). Un-homed for production; when a production consumer appears it gets `degenbot.subscriber`. The test fakes import directly from `_ffi.subscriber` — under the strict Pydantic barrier, test code is leaf code and must import from the home once it exists; until then the test imports are the signal that a home is needed.

### Clean single-home submodules (reroute only)

`balancer_math` → `degenbot.balancer.math` · `cl_math` → `degenbot.uniswap.math` · `curve_math` → `degenbot.curve.math` · `solidly_math` → `degenbot.aerodrome.math` · `solady` → `degenbot.utils.solady` (existing subpackage `utils/solady/libzip.py` is the only consumer; mirrors 1:1) · `dex_identity` → `degenbot.uniswap.dex_identity` · `provider` → `degenbot.provider` (already a package) · `simulation` → `degenbot.dispatch` · `submission` → `degenbot.dispatch` (both under `dispatch/`) · `cancel` → `degenbot.updater` · `pool` → CLI-only consumer (`cli/pool.py`); `degenbot.pool` mirror home created when a non-CLI consumer appears, or `cli/pool.py` is the home itself if the CLI stays the sole consumer (verify during cutover).
