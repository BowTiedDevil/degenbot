# Migration Guide: The Sealed `_from_py_pool` Pool Seam

> **Purpose.** This is the rubric for migrating a pool companion to the
> Polars-style single-arg seam — `_from_py_pool(cls, py_pool) -> Self` — that
> completes ADR-005's companion-to-handle migration for every pool family.
> The seam is **complete** for V2/V3/V4/Balancer-weighted/Balancer-stable/
> Aerodrome/Curve; this guide records the discipline so a future family (or a
> regresssion review) follows the same shape, and records the layer-3
> follow-up epic (pump-decoded state) that supersedes parts of it.
>
> Sibling to `docs/migration-guides/three-layer-transition.md` (the
> stateful→stateless sweep rubric). That guide covers moving *logic* from
> Python to Rust; this one covers moving *construction* from kwargs to the
> handle.

## 1. The end state (the Polars `_from_pydf` rule)

A pool companion is constructed from **one argument** — the Rust handle — and
reads every identity field off it:

```python
@classmethod
def _from_py_pool(cls, py_pool: PyLiquidityPool) -> Self:
    self = object.__new__(cls)
    # family guard (§2)
    # read every identity field off py_pool (§3)
    # cross-pool references resolve via the handle (§4)
    return self
```

Direct construction is forbidden:

```python
def __init__(self, *args, **kwargs) -> None:  # noqa: ARG002
    raise TypeError(f"{type(self).__name__} cannot be constructed directly. "
                    "…call {type(self).__name__}._from_py_pool(handle)…")
```

The companion holds **nothing** an external caller can mutate; the only paths
to a pool instance are the ones that wire a handle (`Bot.build_pool()`
production, the test factory `make_<family>_pool`). This is the Polars
`DataFrame._from_pydf` discipline lifted to pools.

## 2. The rubric (one commit per family)

Each family migration is a single reviewable unit landing these changes in one
commit:

1. **Rust identity fields.** Add any identity field the companion still held
   Python-side to `VxPoolIdentity` + `Register<Family>PoolParams`
   (`rust/crates/degenbot-bot/src/bot_core/<family>_state.rs`), thread it
   through `BotState::register_<family>_pool` + the `register_<family>_pool`
   Py-binding (`rust/crates/degenbot-python/src/bot/mod.rs`), and resync the
   `.pyi` stub. **Apply the deletion test first**: an identity field that
   never varies (e.g. Curve's `state_cache_depth`, always 8) is *deleted* from
   the companion/factory/builder path rather than threaded — keep a default on
   the owned cache, stop accepting the kwarg.
2. **`PyLiquidityPool` getters.** Add `#[getter]`s (for scalars) or methods
   (for companion-resolution + read-throughs) on
   `rust/crates/degenbot-python/src/bot/pool.rs` exposing every identity field
   off the handle. The existing `address`/`update_block` getters gain a
   `"curve"`/family arm if not already present. Resync the `.pyi`.
3. **Forbidden `__init__`.** Replace the multi-kwarg `__init__` with the
   `TypeError`-raising stub above. Add **class-scope instance-attribute
   declarations** (the `_foo: T` set by `_from_py_pool`) so red-knot resolves
   reads in helper/calc methods without inline annotations (inline `self._x: T`
   annotations are *not* allowed in method bodies — declare at class scope).
4. **Single-arg `_from_py_pool`.** The new classmethod reads every field off
   the handle. Docstring must declare `Returns:` + `Raises:` (DOC201/DOC501).
5. **Factory + builder migration.** `make_<family>_pool` (tests) and the
   `<Family>PoolBuilder._register_handle` (production) register all identity
   in Rust, then return `<Family>Pool._from_py_pool(handle)`. The factory
   pre-registers tokens/underlying/lp in its bot (ADR-006) so the handle
   resolves companion handles via `get_<family>_tokens`.

### Guards and conventions

- **Variant-family guard.** The first thing `_from_py_pool` does is assert
  `py_pool.pool_family == "<family>"` — a V2 handle passed to a V3 companion
  raises `DegenbotValueError`, not a wrong-field crash. Required because the
  registry indexes all families by `pool_id`.
- **`self = object.__new__(cls)`.** Bypasses the forbidden `__init__`. Then
  assign every attribute and `return self`.
- **`Self` return type.** Use `typing.Self` so subclasses are handled (the
  factory's `pool_class` parameter relies on this).
- **`# noqa: SLF001`** on `_from_py_token` / `_from_py_pool` calls inside
  `_from_py_pool` — the seam intentionally crosses the private boundary.

## 3. Identity from the handle (the read pattern)

Read each category via the appropriate handle surface (mirror the Balancer /
Aerodrome / Curve seams):

| Category | How | Example |
|---|---|---|
| Scalar identity (A, fee, admin_fee, …) | `#[getter]` | `py_pool.curve_fee` |
| Tuple identity (a-ramp, crypto fees, …) | `#[getter]` returning a tuple | `py_pool.curve_a_ramp()` |
| Strategy enums | `#[getter]` returning `u8` discriminant; `Enum(value)` round-trips at the companion | `SwapStyle(py_pool.curve_swap_style)` |
| Token companions | method resolving `PyErc20Token` off the shared bot's token registry | `py_pool.get_curve_tokens()` → `Erc20Token._from_py_token(t)` |
| LP token | method; `None` ⇒ fall back to `tokens[0]` | `py_pool.get_curve_lp_token()` |

The factory's `_strategies_to_rust_enums` is the inverse of the enum
round-trip — both sides forward `.value` verbatim and `auto()`-based enums are
1-based.

## 4. Cross-pool references (the go-between)

A pool may depend on *another pool* (Curve metapool → base pool). This is the
only case that breaks single-arg naively. Resolve it with a **Rust
go-between**, not a Python registry:

1. The dependent pool's identity stores the referenced pool's **address**
   (`base_pool: Option<Address>`), storable at registration.
2. A ~6-line Rust method on `PyLiquidityPool` resolves it:
   ```rust
   fn curve_base_pool(&self) -> Option<PyLiquidityPool> {
       let core = self.core.read();
       let id = core.get_curve_identity(self.pool_id)?;
       let base_addr = id.base_pool?;
       let base_id = core.pool_id_by_address(&base_addr)?;
       drop(core);
       Some(PyLiquidityPool::new(Arc::clone(&self.core), base_id))
   }
   ```
   Same shared `Arc<RwLock<BotState>>` (ADR-006 D1) — no Python registry.
3. The companion wraps it **lazily** (`_LazyBasePool`): memoise the
   `cls._from_py_pool(handle)` on first use so a dependent that never takes
   the cross-pool path pays zero cost.

### Naming the interface

If the dependent invokes *calculation methods* on the referenced pool (not
just metadata), name the surface as a **`Protocol`** at the calculator
boundary and widen the calculator's input type to it:

```python
@runtime_checkable
class BasePoolPort(Protocol):
    # exactly the members the calculator calls — nothing more
    @property
    def tokens(self) -> tuple[Erc20Token, ...]: ...
    @property
    def balances(self) -> tuple[int, ...]: ...
    ...
```

This earns its keep on the **codebase-design seam gate**: one adapter (the
lazy go-between) is a *hypothetical* seam; **two** adapters make it real.
Ship a canned `Stub<Name>Pool` as the second adapter — it is what finally
lets the calculator be unit-tested without standing up a full pool (the
latent seam surfaces as a test that only `assert isinstance(...)` the
calculator instead of calling `.calculate()`).

## 5. Stored I/O trait objects (the read-through pattern)

The companion's per-block on-chain reads (Curve's 13-method data provider,
Balancer's rate provider, V3/V4's tick fetcher) are **not** Python callbacks.
They are pyo3-free `Arc<dyn Trait>` stored on the `VxPoolState`:

- **Rust core** (`rust/crates/degenbot-bot/`): the trait + a Py-adapter
  struct holding a `Bound<PyAny>` that re-enters via `Python::attach` (no
  `BotState` lock held across the call). Stored on the state struct as
  `data_provider: Option<Arc<dyn CurveDataProvider>>`.
- **`degenbot-python`** (`rust/crates/degenbot-python/src/bot/pool.rs`):
  `fetch_<family>_*` methods on `PyLiquidityPool` clone-out the trait object,
  drop the read guard, call the method, wrap the result. Use private helpers
  (`read_provider_vec` / `read_provider_opt` in the **non-`#[pymethods]`**
  `impl` block — `impl Trait` args aren't allowed in `#[pymethods]`).
- **Python companion** (`src/degenbot/<family>/`): a `_Handle<Name>Adapter`
  wrapping the handle, exposing the full read interface, raising
  `Missing<Family>Data` on a missing provider / fetch miss so the existing
  calc-path error handling applies unchanged. The companion holds *this
  adapter* (constructed in `_from_py_pool` when `py_pool.<family>_has_*`), not
  a Python object.

The FFI rule (ADR-005: no pyo3 in core crates) holds — the trait + the adapter
both live across the seam correctly.

## 6. Test discipline

- **Construction guard test.** `tests/<family>/test_<family>_pool_construction_guard.py`:
  no-arg `__init__` raises `TypeError` (mentions `Bot.build_pool` + the
  factory); old-kwarg shape raises `TypeError`; a wrong-family handle raises
  `DegenbotValueError`; plain-pool identity round-trips off the handle; the
  go-between resolves cross-pool references (sanity: the `BasePoolPort`
  instance delegates to the recovered companion); a `Stub<Name>Pool` is an
  `isinstance` of the `Protocol` and yields a calculator-input that
  type-checks.
- **Parity goldens unchanged.** The onchain-parity tests must stay green byte
  for byte — the seam is a construction refactor, not a math change. If a
  parity test builds a dependent + referenced pool in *separate* default bots,
  give it a single shared local `PyBot` (production-faithful: the builder
  shares `self._py_bot`); fresh per call keeps multiblock tests isolated.
- **`just lint` + `just test` green** before commit.

## 7. Layer-3 follow-up epic (TODO — not built here)

The stored trait-object I/O design is **not terminal** for on-chain feeds.
It is the layer-2 shape: the trait object hides *whether* the read is an
`eth_call` or a cache hit, and lets a companion read through a handle today.
The layer-3 move supersedes it for on-chain Curve/Balancer feeds:

- **Pump-decode per-block state directly into `BotState` slots.** The
  `LogDispatcher` (ADR-006 D4 helper) already decodes events for the pump's
  `apply_*` path; layer-3 extends that to the per-block values the
  data_provider/rate_provider callables currently fetch on-demand
  (`virtual_price`, `block_timestamp`, `redemption_price`, rate-provider
  rates, …) — decode them off the same log stream and write them into the
  `VxPoolState` slots. The on-chain data_provider/rate_provider callables are
  **eliminated** for pump-fed pools.
- **Balancer off-chain rate feeds remain trait objects terminally.** Rate
  providers that *aren't* on-chain log-driven (off-chain, push-fed) have no
  pump to decode from — their `Arc<dyn RateProvider>` stays a stored trait
  object permanently. The layer-3 epic scopes the elimination to
  log-decodable feeds only.

This epic is recorded here so the trait-object design of BQM2OA / JFGCHJ /
4UBHP6 / MLJT4V is not mistaken for terminal for on-chain Curve/Balancer
feeds. It is the layer-2 scaffolding the layer-3 pump-decode retires — in the
direction of *more* standalone-Rust coverage (ADR-006 D4), not less.

## References

- **ADR-005** — the three-layer architecture; "sealed `_from_py_pool` seam" footer.
- **ADR-006** — Bot as per-chain orchestrator; the shared `Arc<RwLock<BotState>>`
  is the go-between's enabler.
- **Seam commits** (per family): `15a8e2a5` + `f167db11` (V2, the template),
  `7ec458ad` (V3), `7bc78962` (V4), `11ead76a` (Balancer-weighted),
  `798247fd` (Balancer-stable), `1411a960` (Aerodrome), `938fb4d3` (Curve).
- **Rust foundation commits**: `755d1c7b` (pool_family discriminator),
  `bb2ee538` (tick fetcher trait, MLJT4V), `5b832a99` (Curve data-provider
  trait, JFGCHJ), `3dbf2f49` (Balancer rate-provider trait, 4UBHP6).
- **`docs/migration-guides/three-layer-transition.md`** — the sibling rubric
  for moving logic from Python to Rust (the stateless half of ADR-005).
- **`codebase-design`** skill — the "one adapter = hypothetical seam, two
  adapters = real seam" gate the `BasePoolPort` design satisfies.