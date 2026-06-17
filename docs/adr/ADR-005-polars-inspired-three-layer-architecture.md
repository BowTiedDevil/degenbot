# ADR-005: Polars-Inspired Three-Layer Architecture

**Status: accepted.** Implemented for the `Bot`/`PyBot`/`PyLiquidityPool`/`PyErc20Token` family
(`BotCore`→`Bot`, `PyBotCore`→`PyBot` rename + `Mutex`→`RwLock` in the PyO3-handle tier).
The `UniswapEngine` unification is deferred — see "Deferred".

## Context

degenbot mixes Python and Rust via PyO3 across two distinct questions:

1. **(ADR-003)** *What owns runtime pool/token state?* — answered: the Rust `Bot`, as a
   single owner peer to `UniswapEngine`.
2. **(this ADR)** *How do Python callers reach that Rust-owned state across the FFI
   without copying, while staying thread-safe under Python 3.13+ free-threading and the
   per-block hot loop?* — previously unanswered. ADR-003 mentions "thin `PyO3` handles
   over `Arc<Mutex<BotCore>>`" in passing but never canonizes the lock type or the
   session-owns-wrapper topology. The "Polars model" was referenced scattered across
   Plan 079, ADR-003 implications, and `rust/AGENTS.md`, but never recorded as a
   decision for the *stateful* middle-layer case.

`rust/AGENTS.md` already documents the **generic** PyO3 module convention — Python
convenience layer / thin PyO3 wrapper (`*_py.rs`) / pure Rust core (`*.rs`, no `pyo3`
imports) — and lists Polars as one of two reference projects (Polars + Pydantic). That
convention covers the *stateless* case (free `#[pyfunction]`s like `decode`/`encode`/
`tick_math`). This ADR is the **stateful specialization**: when the Rust core holds
long-lived mutable state that many Python objects must reference.

## Decision

Adopt the **Polars-inspired three-layer architecture** for stateful Rust-owned
resources, with a **standalone Rust core as a first-class concern** (the crate split
that lets the core be consumed without Python, like Polars). Three layers with strict
separation (mirroring `rust/AGENTS.md`'s generic convention, specialized for shared
state), realized across two Rust crates:

1. **Rust Core** (`Bot`, `V2PoolState`/`V3PoolState`/`V4PoolState`, `DexIdentity`,
   reorg journal, event decoders, swap math) — pure Rust, **zero `pyo3` imports**.
   Owns all data + state-machine logic + the `DexIdentity` preset registry. This is
   the crate a standalone Rust consumer `cargo add`s; the Python-binding crate is a
   sibling, not a parent. Already largely true: `rust/src/bot_core/mod.rs` core
   structs have no `pyo3` imports today — the split is a packaging decision, not a
   rewrite.
2. **PyO3 Wrapper** (`PyBot`, `#[pyclass]` in `py_bot.rs`) — holds
   `Arc<parking_lot::RwLock<Bot>>`. **The wrapper is the sharing mechanism.** Thin
   stateful handles (`PyLiquidityPool` carrying a `pool_id` key, `PyErc20Token` carrying an `Address`
   key) clone the *same* `Arc`, so N Python objects reference one Rust-owned `Bot`.
   Read methods take a read guard (`calculate_tokens_out`, `encode_swap`, getters,
   journal-length queries); write methods take a write guard (`register_*`, `update_*`,
   `restore_*`, `discard_*`). All `#[pyclass]`/`#[pyfunction]` surface lives in the
   binding crate; the Rust core never names them.
3. **Python Session** (`bot.py:Bot`) — the public orchestrator. Constructs
   `self._py_bot = PyBot()` in `__init__`. Owns registries, config, DB, and all I/O;
   delegates Rust-owned state through the wrapper.

The **crate split target** mirrors Polars' topology:

| Crate | Contents | `pyo3`? | Consumers |
|---|---|---|---|
| `degenbot-core` | `Bot`, `V2/V3/V4PoolState`, `DexIdentity` + presets, calc math, reorg, decoders | none | Rust users *and* the binding crate |
| `degenbot-python` (name TBD) | `PyBot`, `PyLiquidityPool`, `PyErc20Token`, `PyBotIo` (future), all `#[pyclass]`/`#[pyfunction]` | all | Python only |
| `degenbot` (umbrella Python package) | the Python `Bot` companion + the `degenbot_rs` extension built from `degenbot-python` | n/a | Python users |

This is exactly `polars-core` / `polars-python` / `polars` (the umbrella `polars`
Rust crate re-exports `polars_core::{DataFrame, Series, ...}` with zero `pyo3`; all
`Py*` wrappers live exclusively in `polars-python`, which Rust consumers never touch).

### Placement of DEX identity

The standalone constraint **settles where DEX identity lives**: in `degenbot-core`
(Rust), not in a Python module. A Rust consumer constructing a Sushiswap-on-Arbitrum V2
pool needs the Sushiswap factory address, deployer, init hash, and fees — without a
Python import. If those presets lived in `degenbot.dex_presets` (Python), the
standalone claim breaks. `DexIdentity` is therefore a frozen value object in
`degenbot-core` (factory, deployer, init hash, fee params, variant string, ABI struct
shapes), with `pub` DEX presets (`UNISWAP_V2`, `SUSHISW2`, `CAMELOT_V2_STABLE`, etc.)
— the exact shape Polars gives format codecs in `polars-io` (Rust), not Python.

**`DexIdentity` is *not* a field on `V2PoolState`.** A pool's swap-math inputs
(reserves, sqrt_price) don't need the DEX identity to apply a Sync event or solve a
swap — that's invariant math. The identity is needed only at *encoding* (factory goes
into calldata) and *registration* (which preset constructed this). So it's a
construction/encoding parameter, not state. Both the Python `Pool` companion (via the
`Py*` wrapper) and the standalone Rust consumer read `DexIdentity` presets from
`degenbot-core`.

### Grounding in Polars

`polars-python`'s `DataFrame` wrapper holds `RwLock<DataFrame>` over `polars-core`;
slicing/cloning shares the underlying buffers via a custom `Arc` (`SharedStorage`), so
many Python `DataFrame` views reference one Rust-owned buffer set. degenbot mirrors this
exactly: `PyBot` holds `RwLock<Bot>`; `PyLiquidityPool`/`PyErc20Token` share via `Arc::clone`. The
difference is granularity — Polars shares large Arrow buffers; degenbot shares a single
state struct and keys into it.

The **crate split mirrors Polars exactly too** (verified against the Polars source):
`polars-core` has zero `pyo3` imports in any of its 231 source files (the `pyo3` line
in its `Cargo.toml` is declared-but-unused); all `#[pyclass]` `Py*` wrappers live
exclusively in `polars-python`; the umbrella `polars` Rust crate `pub use`s
`polars_core::{DataFrame, Series, ...}` with no `pyo3`, and is what Rust consumers
`cargo add`. degenbot's target topology (`degenbot-core` / `degenbot-python` /
`degenbot` umbrella) is the same shape — Rust core consumable standalone, Python
bindings a sibling crate.

### Layer naming

Naming follows the Polars rule **unconditionally**: the `Py` prefix is kept on the
PyO3 wrapper both as the Rust struct name *and* as the Python-exposed name; the bare
noun is reserved for the Python companion class. No `#[pyclass(name = "...")]`
override drops the prefix.

| Layer | Name | Example |
|---|---|---|
| Rust core (data + state-machine logic, no I/O) | bare noun, no `pyo3` | `Bot`, `DexIdentity` |
| Rust core internal storage (dispatch key, not public) | terse `<version>PoolState` | `V2PoolState`, `V3PoolState`, `V4PoolState` + `PoolEntry::V2/V3/V4` |
| PyO3 wrapper (`#[pyclass]`, keeps `Py`) | `Py` + companion name | `PyBot`, `PyLiquidityPool`, `PyErc20Token` |
| Python companion (orchestration + I/O) | bare noun matching the wrapper minus `Py` | `Bot` ↔ `PyBot`; `Erc20Token` ↔ `PyErc20Token`; `LiquidityPool` ↔ `PyLiquidityPool` |
| Future Rust I/O struct | `Py` + `*Io`/`*Reader` | `PyBotIo` (stateful, holds provider/DB) |
| Stateful Rust free functions | no `Py` prefix | per `rust/AGENTS.md` (`#[pyfunction]`) |

**Generalized wrapper noun, variant is internal.** The wrapper noun is *generalized* —
`PyLiquidityPool` (and the standalone-Rust `LiquidityPool` reference), not a
per-variant `PyV2PoolState`/`PyV3PoolState`/`PyV4PoolState`. The `V2`/`V3`/`V4`
variant vocabulary lives **only** as internal Rust-core storage dispatch
(`PoolEntry::V2(V2PoolState)` + the terse `V2PoolState` structs are match-arm targets,
not a public API surface). This matches degenbot's user-facing ergonomics: a user knows
they want "a Uniswap V2 pool" (which the frontend shows), not the constant-product
invariant name; `Bot` investigates the identity under the hood (pool key, address,
token pair, fee, factory) and resolves the variant internally. The standalone-Rust
`Bot` (under the crate-split target) does the same — a Rust consumer constructs via
`Bot::register_pool(addr, dex=...)`, never `V2PoolState::new(...)`. This is the
`pl.DataFrame` precedent: users construct `DataFrame`, never `ChunkedArray<T>`.

**Stance B — collapsed DEX companions.** Under stance B, the hollow DEX-class
hierarchy (`SushiswapV2Pool`, `PancakeswapV2Pool`, `SwapbasedV2Pool` — all of which
add only a `variant` ClassVar + static fee constants, verified during grilling)
collapses into the generalized `LiquidityPool` companion, with DEX identity carried as
`DexIdentity` data (stance II — identity is deployment data, not behavior) and DEX
*behavioral* divergence carried as strategy mixins (only `CamelotPoolCalc`'s
stable-swap branch and `AerodromeV2Pool`'s log decoder are genuine behavior, neither
of which earns a class hierarchy — both are already mixin/decoder-shaped). DEX presets
live in `degenbot-core` as `pub` values (`UNISWAP_V2`, `SUSHISWAP_V2`, `CAMELOT_V2`,
etc.), used as construction parameters (`LiquidityPool(addr, dex=UNISWAP_V2)` or
named constructors `LiquidityPool.uniswap_v2(addr)`), not as subclasses. Public-API
breakage is acceptable (0.x major refactor underway).

**Precedent set by this ADR's slices.** The `#[pyclass(name = "Pool")]` /
`name = "Token"` overrides (which exposed the original structs under the bare names
`Pool`/`Token`) were dropped (slice 1), and the structs renamed to
`PyLiquidityPool`/`PyErc20Token` (slice 2) — every wrapper now keeps the `Py` prefix
unconditionally, with no `name=` override. This is the template for future wrappers.

## Considered options (rejected alternatives)

- **Mutex everywhere (engine parity).** Keep `Arc<Mutex<Bot>>` on `PyBot` to match
  `UniswapEngine`. **Rejected**: the Python-facing access pattern is read-heavy
  (per-pool calc reads, tick-data reads during solves, `PyLiquidityPool`/`PyErc20Token` property
  reads); a single write mutex would serialize all of them. `RwLock` allows concurrent
  readers under Python 3.13+ free-threading. Cost — marginally larger guard, slightly
  slower writes — is justified by read dominance. (`UniswapEngine` retains `Mutex` today
  because its access pattern is engine-then-core under a pump, a different shape; see
  Deferred.)
- **The Python `Bot` class *is* the `#[pyclass]`.** Drop the wrapper, make `Bot` itself
  a PyO3 class. **Rejected**: couples session orchestration (SQLAlchemy, web3.py,
  publisher/subscriber, RPC I/O) to PyO3 lifetime/GIL semantics — `Bot` could no longer
  be constructed without the GIL or unit-tested without the extension built. Breaks the
  clean separation the generic `rust/AGENTS.md` three-layer rule mandates ("if `pyo3`
  appears in a file that isn't `*_py.rs`, it's a code smell") — a Python class can't be
  `*_py.rs`.
- **Handles re-resolve via a global registry.** `PyLiquidityPool`/`PyErc20Token` hold only a key and
  call a global `get_pool(id)` each access. **Rejected**: forces a lock + `HashMap`
  lookup per property read, and reintroduces a process-global singleton (the deprecated
  pattern root `AGENTS.md` warns against) as the state authority. Loses the O(1)
  `Arc`-shared reference that is the whole point of the Polars analogy.
- **Status quo (`PyBotCore` + `Mutex`).** **Rejected**: this was the ad-hoc,
  uncanonicalized state this ADR replaces. No recorded rationale existed for the lock
  type or the topology; the next contributor could not tell whether the choices were
  deliberate.
- **DEX identity on the Python companion (B-i).** Place `DexIdentity` presets in a
  Python module (`degenbot.dex_presets`), with the Python `Pool` companion as the
  single source. **Rejected by the standalone-core constraint**: a Rust consumer of
  `degenbot-core` (a pure-Rust bot, or a Python-alternative runtime) constructing a
  Sushiswap V2 pool would need the Sushiswap factory/init-hash/fees — unreachable
  without a Python import. The standalone claim is first-class, so identity must be
  Rust-side (see "Placement of DEX identity" above). The Python companion *holds* a
  `DexIdentity` at runtime (resolved through the `Py*` wrapper); it is not the
  *source* of truth for it.
- **Everything in Rust upfront (B-iii).** Move all DEX calc strategies into Rust as
  part of this ADR. **Rejected**: violates the cutover property — each DEX's calc
  strategy is independently portable to Rust and must be tested against the existing
  Python behavior before cutover. `ConstantProductCalc` ports first; `CamelotStableCalc`
  follows independently; DEX identity (data, not behavior) is Rust-side from day one
  regardless. See Deferred.

## Consequences

- **Multiple Python handles share one Rust-owned `Bot` thread-safely** — the goal. The
  Python `Bot` can hand out `PyLiquidityPool`/`PyErc20Token` handles that stay live and consistent
  with the session's state for their lifetime.
- **The `RwLock` read/write split is now an invariant.** New stateful `#[pyclass]`
  wrappers in this tier must classify each method as read (`.read()`) or write
  (`.write()`). The `rust/CONTEXT.md` {PyBot} term records the classification.
- **Two locking disciplines coexist until unification** (see Deferred): `RwLock` on the
  Python-facing wrapper tier, `Mutex` on the engine-internal tier. A future slice
  collapses this.
- **The lock-ordering rule from ADR-003 is preserved**: Python-facing wrapper methods
  take the core lock alone and never nest the `UniswapEngine` lock — the rule that
  keeps the deadlock surface empty. This ADR does not change lock *order*, only the
  *type* of lock on the Python-facing tier.
- **The standalone-core constraint is first-class.** Any new state, identity, or
  preset that a standalone Rust consumer would need must land in `degenbot-core` from
  day one — placing it on the Python side first and "moving it later" strands it
  across the future crate boundary. `DexIdentity` (above) is the operative precedent:
  Rust-side at introduction, even though the Python `Pool` companion is its first
  consumer. This is the rule that prevents the crate split (Deferred) from becoming
  a rewrite.

## Related

- **ADR-003** (Bot as state layer) — **complementary, not overlapping.** ADR-003 answers
  *what owns state* (the Rust `Bot`, peer to `UniswapEngine`); this ADR answers *how
  Python reaches that state across FFI*. ADR-003's "thin `PyO3` handles over
  `Arc<Mutex<BotCore>>`" mentioned the handles + Arc but never canonized the lock type
  or the session-owns-wrapper topology — that canonization is this ADR.
- **`rust/AGENTS.md` "Three-Layer Pattern"** — this ADR is the **stateful
  specialization** of that generic convention. The generic convention says "pure core /
  thin wrapper / Python convenience"; this ADR adds the stateful topology (shared
  `Arc<RwLock<Core>>`, the-wrapper-is-the-sharing-mechanism, Python session owns the
  wrapper, read/write guard split).
- **`docs/architecture/rust-owned-bot.md` §13** — topology description and code mapping.
- **`rust/CONTEXT.md`** — glossary term {Polars-Inspired Three-Layer Architecture};
  inline mentions in {Bot}/{PyBot}/{PyLiquidityPool}/{PyErc20Token} de-duplicated to point here.
- **Plan 079** ("Rust-Owned Bot Core") — first articulated the "Polars model" goal
  ("Python is the cockpit, Rust is the engine"). This ADR records the FFI-topology
  decision 079 implied but never specified, and **sharpens 079's framing**: the
  standalone-Rust-core target means Rust isn't merely Python's engine — it's a
  consumable library in its own right. "Cockpit/engine" survives as the *Python-bound*
  reading; the crate split (Deferred) is the standalone reading 079 didn't name.

## Deferred

Two targets are deferred, both consequences of this ADR's standalone-core direction:

- **Crate split (`degenbot-core` / `degenbot-python` / `degenbot` umbrella).** Today
  `rust/` is one crate with `pyo3` permeating (the `*_py.rs` convention). The split
  peels the `*_py.rs` files into `degenbot-python`, leaving `degenbot-core` with zero
  `pyo3` — a packaging change, not a rewrite (core structs like `Bot`/`V3PoolState`
  already import no `pyo3`). Triggered when either (a) a standalone Rust consumer
  wants `cargo add degenbot-core`, or (b) the Python-binding surface grows large
  enough to deserve its own release cadence. The `DexIdentity` preset registry lands in
  `degenbot-core` *now* (regardless of split timing) per the standalone-core
  consequence above, so the split never has to relocate it.

- **`UniswapEngine` lock unification.** `UniswapEngine` holds its own
  `Arc<Mutex<Bot>>` (engine-then-core order, ADR-003). Unifying the engine onto the
  *shared* `Arc<RwLock<Bot>>` — so the engine and Python share one handle to one `Bot`
  — is a later slice. Requires resolving the nested lock-ordering question (currently
  engine-`Mutex`-then-core-`Mutex`; collapsing to a single core lock, or a different
  discipline) and is deferred until the engine's access pattern is ready to give up
  its independent lock. Until then, the Python `Bot`'s `PyBot` and the
  `UniswapEngine`'s `Bot` are separate Rust-owned instances of the same struct.
