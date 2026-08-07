# Chain (RPC bootstrap) tick-map assembly — design

> **Scope spike** for epic `5NT2OC` ("Chain (RPC bootstrap) tick-map assembly —
> `TickBootstrapRpc` trait + `TickWordFetcher` consolidation"). This is the design
> document the spike ships; it is the canonical pre-implementation read for the
> epic's downstream implementation tasks.
>
> **Status:** design (no code yet). Last updated 2026-07-17.
>
> **Post-`XEANMB` update (2026-07-17):** the `SnapshotStore` arm this design
> describes as running first in the `Store → Db → Chain` precedence is now
> RETIRED (`XEANMB` landed a WAL held read transaction that replaces the
> Store's boot-time freeze; `assemble_*_tick_map` is `Db → Chain`). The
> forward-looking consolidation notes in §6 (which reference the Store arm)
> are historical — they were correct at design time; the post-removal path
> (single `TickRpc` trait keyed by address that both the registration-time
> bootstrap + the live-pump miss path use) remains a valid future
> consolidation but no longer requires removing a Store arm. See
> [`docs/architecture/snapshot-store-removal-scoping.md`](../architecture/snapshot-store-removal-scoping.md)
> §7 for the removal's final task dispositions.

## 0. Problem statement

Candidate 1 (`UHPXSD`) shipped `assemble_v3_tick_map` / `assemble_v4_tick_map` —
Rust helpers implementing `Store → Db` precedence. The Python V3/V4 builders
were cut over (task `YPRFQ2`) to call them before `register_*_pool`. On a miss,
the builder runs Branch 3 sparse RPC (the current tick of the current word's
bitmask + per-active-tick liquidity fetch) **inline in Python**.

The Chain arm is the follow-up that moves Branch 3 into Rust so the helper
becomes `Store → Db → Chain` (full precedence) and a `cargo add degenbot`
standalone Rust consumer can get a sparse-from-RPC tick map without going
through Python — closing the "Rust is the engine" end-state for the assemble
helper.

The two Python-side Branch 3 paths still live in:

- `src/degenbot/builders/v3_pool_builder.py::V3PoolBuilder.build` —
  `else:` block after `assembled = self._py_bot.assemble_v3_tick_map(pool_address)`.
- `src/degenbot/builders/v4_pool_builder.py::V4PoolBuilder.build` —
  `else:` block after the V4 twin `assemble_v4_tick_map` call.

Both branches do the same choreography, parametrized by an `io`:
1. `word, _ = cl_get_tick_word_and_bit_position(tick=tick, tick_spacing=spacing)`
2. `bitmap_at_word = io.fetch_tick_bitmap(pool_address, word, block=state_block)`
3. `active_ticks = [(word << 8 + i) * spacing for i in range(256) if bitmap & (1<<i)]`
4. For each active tick: `io.fetch_tick_data(pool_address, active_tick, block=…)`
5. Build `{tick: (liquidity_gross, liquidity_net, block)}` for `register_*_pool`.

(V4 substitutes the state-view address + `pool_id_bytes` for the pool address and
`fetch_v4_tick_bitmap` / `fetch_v4_tick_data` for the V3 variants.)

---

## 1. The `TickBootstrapRpc` trait signature

### Decision: one new trait in `degenbot-pools/src/tick_fetch.rs`, sibling to `TickWordFetcher`

The trait lives in `degenbot-pools` because the assemble helper (in
`degenbot-bot::bot_core::tick_assembly`) is the consumer and `degenbot-bot`
depends on `degenbot-pools`; the trait type must be visible to both crates.
Following the `std::io::Read` precedent (defining an interface pulls no I/O),
the trait *definition* + its value-only error/return types live here; the
RPC/pyo3 *implementations* live in `degenbot-rpc` and `degenbot-python`
respectively.

```rust
//! In `rust/crates/degenbot-pools/src/tick_fetch.rs` —

/// Sparse tick-map bootstrap RPC seam — Chain arm of the
/// `assemble_{v3,v4}_tick_map` precedence (`Store → Db → Chain`).
///
/// The callable answers the **whole-word** sparse read performed once per pool
/// at registration: the tick-bitmap word containing the pool's current `tick`,
/// plus the `(gross, net)` for every initialized tick in that word. Returns
/// `None` for a word whose bitmap is all-zero (no initialized ticks — matches
/// the Python builder's "don't seed empty" rule).
///
/// **Distinct from [`TickWordFetcher`]:** this trait is keyed by *address*
/// (the pool's contract address — known BEFORE `register_*_pool` assigns the
/// internal `pool_id`), whereas `TickWordFetcher` is keyed by *pool_id*
/// (the live-pump miss path operates on already-registered pools). The
/// orchestration choreography is identical between the two (compute word,
/// fetch bitmap, enumerate bits, fetch ticks) but the key type differs — see
/// §3 for why they are kept separate for now, not consolidated.
pub trait TickBootstrapRpc: Send + Sync + std::fmt::Debug {
    /// Fetch the tick-bitmap word containing `tick` for the V3 pool at
    /// `pool_address`, plus the liquidity for every initialized tick in that
    /// word. `block` is the snapshot state block.
    ///
    /// Returns `None` for an all-zero bitmap (no initialized ticks).
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapTickError::Rpc`] on RPC failure,
    /// [`BootstrapTickError::InvalidReturn`] on a malformed return.
    fn bootstrap_v3_tick_word(
        &self,
        pool_address: &str,
        tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> Result<Option<BootstrapTickWord>, BootstrapTickError>;

    /// Fetch the tick-bitmap word containing `tick` for the V4 pool identified
    /// by (`pool_manager`, `pool_id`), plus the liquidity for every tick in
    /// that word.
    fn bootstrap_v4_tick_word(
        &self,
        pool_manager: &str,
        pool_id: &[u8; 32],
        tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> Result<Option<BootstrapTickWord>, BootstrapTickError>;
}

/// One word's bootstrap result. Mirrors `FetchedTickWord` from
/// `TickWordFetcher` but lives separately for the call-site distinction
/// (registration-time vs live-pump). Both can be unified later (§3).
#[derive(Debug, Clone)]
pub struct BootstrapTickWord {
    /// The tick-bitmap word position (`get_tick_word_and_bit_position(tick, spacing).0`).
    pub word: i32,
    /// Initialized ticks in this word. `liquidity_gross` / `liquidity_net` /
    /// `block` — the same shape `register_*_pool` accepts as `tick_data`.
    pub ticks: HashMap<i32, TickInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapTickError {
    /// RPC failure (timeout, reverting call, transport error).
    Rpc,
    /// Malformed return data (short bytes, bad ABI decode).
    InvalidReturn,
}
```

### Rationale (one trait with two methods, not two traits)

- The two methods differ only in their pool identification (V3 address vs V4
  pool-manager + pool-id). They share the same choreography, return type, and
  error union — one trait keeps the pyo3 adapter + alloy impl in one place
  (one struct, two method bodies).
- Splitting into `V3TickBootstrapRpc` + `V4TickBootstrapRpc` would force two
  pyo3 adapter structs and two alloy impls for ~20 lines of divergent code each
  — net cost worse than one trait with two methods.
- The trait takes the whole-word choreography as a single call (`bootstrap_*`
  returns `BootstrapTickWord` with bitmap + ticks) rather than the finer
  `fetch_tick_bitmap` + `fetch_tick_data` calls Python does inline today. This
  is deliberate — **the choreography moves into the trait impl** (§4 Outcome
  (a)), so a `cargo add` consumer gets the whole-word patch from one call.

### Why the trait is async-internal but sync from the caller's perspective

`AlloyProvider::eth_call` is `async`. The trait method signature is `fn ... ->
Result<...>` (synchronous). The alloy impl in `degenbot-rpc` handles this with
`py.detach(|| get_runtime().block_on(async { ... }))` — exactly the pattern
`PyBotIo::forward_call_to_provider` uses today (see
`rust/crates/degenbot-python/src/bot/py_bot_io.rs:2128`). For the standalone
consumer (no GIL), the alloy impl can be `async fn` on a sibling trait OR
provide a blocking wrapper that uses `tokio::runtime::Runtime::block_on` — the
`degenbot-rpc` provider already exposes a synchronous-looking `eth_call`
wrapper (see `rust/crates/degenbot-rpc/src/provider.rs:623`'s `pub async fn
eth_call` + the runtime pattern in py_bot_io).

**Deferred decision:** whether `TickBootstrapRpc` should be `async fn`-based
(an `async trait`) is a 5NT2OC implementation concern — the spike recommends
NOT making it `async` yet, since the only consumer (`assemble_*_tick_map`) runs
in a synchronous callsite already wrapped in `py.detach` (A4YUYJ's GIL release).
A future pure-async standalone `Bot` API can introduce a sibling `async` trait
that shares the same value types.

---

## 2. The pyo3 adapter design

### Decision: `PyTickBootstrapRpc` in `rust/crates/degenbot-python/src/bot/py_bot_io.rs` (sibling to `PyTickWordFetcher`)

The adapter holds a `pyo3::Py<pyo3::PyAny>` reference to a Python object that
exposes `fetch_tick_bitmap` / `fetch_tick_data` (for V3) and
`fetch_v4_tick_bitmap` / `fetch_v4_tick_data` (for V4). The natural holder is a
`PyBotIo` instance, but the adapter accepts any Python object exposing those
methods (matching the duck-typed `provider` field on `PyBotIo`).

```rust
//! In `rust/crates/degenbot-python/src/bot/py_bot_io.rs` (or a new sibling
//! module `tick_bootstrap.rs` if the file is getting big — see §6).

use degenbot_pools::tick_fetch::{BootstrapTickError, BootstrapTickWord, TickBootstrapRpc};
use std::collections::HashMap;

/// `#[pyo3]` adapter wrapping a Python `PyBotIo` (or any object exposing its
/// four `fetch_*` methods) as a stored `Arc<dyn TickBootstrapRpc>`. Mirrors
/// `PyTickWordFetcher`'s structure (a stored `Py<PyAny>` handle, re-entering
/// the GIL synchronously per call). Used by `assemble_*_tick_map`'s Chain arm.
#[derive(Debug)]
pub(crate) struct PyTickBootstrapRpc {
    /// A Python object exposing `fetch_tick_bitmap`, `fetch_tick_data`,
    /// `fetch_v4_tick_bitmap`, `fetch_v4_tick_data`. Typically a `PyBotIo`.
    io: pyo3::Py<pyo3::PyAny>,
    /// The block identifier Python expects — `None` for latest, `int` for a
    /// specific block. Kept on the adapter so every method uses the same one.
    block: Option<u64>,
}

pub(crate) fn make_tick_bootstrap_rpc(
    io: pyo3::Py<pyo3::PyAny>,
    block: Option<u64>,
) -> std::sync::Arc<dyn TickBootstrapRpc> {
    std::sync::Arc::new(PyTickBootstrapRpc { io, block })
}

impl TickBootstrapRpc for PyTickBootstrapRpc {
    fn bootstrap_v3_tick_word(
        &self,
        pool_address: &str,
        tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> Result<Option<BootstrapTickWord>, BootstrapTickError> {
        use degenbot_cl_math::cl_lib::get_tick_word_and_bit_position;

        let (word, _) = get_tick_word_and_bit_position(tick, tick_spacing);
        pyo3::Python::attach(|py| {
            // 1. fetch_tick_bitmap(pool_address, word, block) -> U256
            let bitmap_obj = self.io.bind(py)
                .call_method1("fetch_tick_bitmap", (pool_address, word, self.block_arg(py)?))
                .map_err(|_| BootstrapTickError::Rpc)?;
            let bitmap: u128 = bitmap_obj.extract().map_err(|_| BootstrapTickError::InvalidReturn)?;
            if bitmap == 0 {
                return Ok(None);
            }
            // 2. enumerate set bits + fetch_tick_data per active tick
            let mut ticks = HashMap::new();
            for i in 0..u32::try_from(TICKS_PER_WORD).unwrap() {  // 256
                if bitmap & (1u128 << i) > 0 {
                    let active_tick = ((word << 8) + i as i32) * tick_spacing;
                    let pair = self.io.bind(py)
                        .call_method1("fetch_tick_data", (pool_address, active_tick, self.block_arg(py)?))
                        .map_err(|_| BootstrapTickError::Rpc)?;
                    let (gross, net): (u128, i128) = pair.extract().map_err(|_| BootstrapTickError::InvalidReturn)?;
                    ticks.insert(active_tick, TickInfo {
                        liquidity_gross: alloy::primitives::U128::from(gross),
                        liquidity_net: alloy::primitives::I256::try_from(net).unwrap_or(I256::ZERO),
                        block,
                    });
                }
            }
            Ok(Some(BootstrapTickWord { word, ticks }))
        })
    }

    // bootstrap_v4_tick_word — identical structure, V4 methods + pool_id_bytes
    // argument; see §0 for the call signature diff.
}
```

### Why the adapter delegates to Python (not directly to alloy)

Three plausible implementations of `TickBootstrapRpc` exist:

1. **`PyTickBootstrapRpc`** — wraps `PyBotIo`, calls `fetch_tick_bitmap` /
   `fetch_tick_data` via GIL re-entry (above). Lives in `degenbot-python`.
   Used by Python-driven consumers.
2. **`AlloyTickBootstrapRpc`** — pure-Rust, holds an `Arc<AlloyProvider>` from
   `degenbot-rpc`, does the eth_call choreography directly (no GIL). Lives in
   `degenbot-rpc` (or `degenbot-bot`). Used by standalone Rust consumers.
3. **`PyBotIo`'s existing native fast path** — `PyBotIo` already holds an
   `Option<Arc<AlloyProvider>>` and routes `forward_call_to_provider` to the
   native Rust client when present (see `py_bot_io.rs:2127-2147`'s `if let
   Some(alloy) = &self.alloy { ... }` block). This means **option (1) already
   gets the alloy fast path for free** when `PyBotIo.alloy` is `Some`.

So the spike recommends option (1) for the Python path (it's the thinnest seam
+ inherits the alloy fast path) AND option (2) for the standalone path. Both
implement the same trait; the assemble helper takes `Option<&dyn
TickBootstrapRpc>`.

**Adapter scope:** the adapter stores `block: Option<u64>` so every call uses
the snapshot state block consistently. (PyObject's `block=state_block` arg is
passed as int or `None` per `PyBotIo::fetch_tick_bitmap`'s `Option<&Bound<...>>`
signature.)

---

## 3. The `TickWordFetcher` consolidation decision

### Decision: KEEP two traits for now; consolidate after `XEANMB` lands

`TickWordFetcher` and `TickBootstrapRpc` look similar but serve DISTINCT
callsites with different key types:

| Aspect | `TickWordFetcher` | `TickBootstrapRpc` |
|---|---|---|
| Trait location | `degenbot-pools/src/tick_fetch.rs` | (new) `degenbot-pools/src/tick_fetch.rs` |
| Key | `pool_id: u64` (Rust-internal) | pool address (V3) / pool-manager+pool-id (V4) |
| Call site | Live-pump miss path during swap simulation (`v3_simulate_swap` → `MissingTickWord`) | Registration-time Chain arm of `assemble_*_tick_map` (before `register_*_pool`) |
| When | AFTER `register_*_pool` (pool_id assigned) | BEFORE `register_*_pool` (pool_id not yet assigned) |
| Return shape | `FetchedTickWord { word, ticks }` | `BootstrapTickWord { word, ticks }` (structurally identical) |
| Frequency | On-demand per miss, ~once per swap crossing a word boundary | Once per pool registration |

**Why not consolidate to one trait now:** consolidating would require either
(a) keying both by address (forcing the live-pump miss path to resolve
address from `pool_id` — extra `BotState` read per miss) or (b) keying both
by `pool_id` (forcing the Chain arm to do `register_*_pool` with an empty tick
map, then come back and fix it up — contradicts ADR-006's inline-seed
rolling-start race closure).

**Consolidation path (post-XEANMB):** once `SnapshotStore` is gone (the
`XEANMB` epic) and `assemble_*_tick_map`'s Store arm is collapsed, the Chain
arm is the LAST remaining registration-time sparse read. At that point a
follow-up could introduce a single `TickRpc` trait keyed by address that both
callsites use, with the live-pump miss path looking up the address via the
existing `BotState` read (already needed there for the swap states anyway).
That consolidation is a separate, post-XEANMB concern — **not in scope** for
`5NT2OC`.

**`tick_fetch.rs` module restructure:** when the new trait lands, split the
file into two submodules — `tick_fetch/bootstrap.rs` (new
`TickBootstrapRpc` + `BootstrapTickWord` + `BootstrapTickError`) and
`tick_fetch/miss.rs` (existing `TickWordFetcher` + `FetchedTickWord` +
`FetchTickWordError`) — keeping `tick_fetch/mod.rs` as the re-export root.
Keeps the two-keyed traits visible but namespace-separated.

---

## 4. Chain arm port target — port into `assemble_*_tick_map`

### Decision: port the whole-word choreography into the Chain arm of `assemble_*_tick_map` (Outcome (a))

The `assemble_*_tick_map` helper gains a `chain: Option<&dyn TickBootstrapRpc>`
parameter (after the existing `probe`/`db` params). When Store and Db both
miss, the helper calls `chain.bootstrap_*_tick_word(...)` instead of returning
`None`.

```rust
pub fn assemble_v3_tick_map(
    store_probe: impl FnOnce() -> (HashMap<i32, TickInfo>, PoolTickCoverage),
    db: Option<&DegenbotDb>,
    address: &str,           // NEW — needed by the Chain arm
    tick: i32,               // NEW
    tick_spacing: i32,       // NEW
    block: u64,              // NEW
    chain: Option<&dyn TickBootstrapRpc>,  // NEW
) -> TickMapAssemblyResult { … }
```

The Store arm runs first (brief `Mutex` lock, takes the entry); the Db arm
runs second (if present); the Chain arm runs third (if neither hit). After a
Chain hit, the helper returns `Some((ticks, PoolTickCoverage::Sparse))` —
Sparse, NOT Tracked, because the Chain arm only seeds ONE tick-bitmap word
(current word's active ticks) and a `sparse` coverage signals to Rust's
miss-detection that neighbouring words must still be backfilled on demand.
This preserves Branch 3's existing `coverage = "sparse"` semantics.

**The Builder cutover** (last task of this epic): `V3PoolBuilder.build` /
`V4PoolBuilder.build` replace their `else:` Branch 3 blocks with a single
call: `py_bot.assemble_v3_tick_map(address, tick, spacing, block, rpc=io)`.
The `io` (PyBotIo) is wrapped via `make_tick_bootstrap_rpc(io, block)` into
an `Arc<dyn TickBootstrapRpc>` once at builder construction time.

### Rejected alternatives

- **(b) Port into a standalone pure-Rust helper in `degenbot-cl-math`:** rejected
  — `degenbot-cl-math` is the pure-math crate (no I/O, no DB, no RPC per the
  existing crate boundary). The choreography does RPC → it doesn't belong here.
  The existing `get_tick_word_and_bit_position` helper in
  `degenbot-cl-math/src/cl_lib/liquidity_mapping.rs` (used by the Chain arm)
  already gives the math crate its due role.
- **(c) Keep it in Python forever:** rejected — contradicts the "Rust is
  the engine, Python is the cockpit" end-state. Branch 3 is currently the
  LAST Python-side sparse-orchestration path; this epic is its port.

---

## 5. Sequencing vs `XEANMB` (snapshot-store removal)

### Decision: `5NT2OC` (this epic) ships BEFORE `XEANMB`; `XEANMB`'s collapse to `Db → Chain` then has a sparse fallback

**Reasoning:**

- If `XEANMB` ships first (collapsing `assemble_*`'s `Store` arm), the helper
  becomes `Db → Chain`... but the Chain arm doesn't exist yet. The Python
  builder would have to keep Branch 3 inline (the current state) AND the Rust
  helper would return `None` on every miss. That's the status quo — no
  regression, but no progress either, and `XEANMB` alone would make the Rust
  helper WORSE (no Store fallback) without giving it the Chain fallback.
- If `5NT2OC` ships first (Chain arm lands), the helper is `Store → Db →
  Chain`. Python's Branch 3 then becomes dead code (cleaned up in the last
  task of this epic). `XEANMB` can then collapse the Store arm safely — the
  resulting helper is `Db → Chain`, with the Chain arm providing the
  sparse-from-RPC fallback the Store arm used to provide.

**So:** `5NT2OC` is a hard prerequisite for `XEANMB`. Add a `depends_on` edge
in the ergo graph: `XEANMB depends_on 5NT2OC`.

(or, mathematically: Chain arm adds a fallback → Store arm removal is safe;
without Chain, store removal would strand the helper without a sparse
fallback at all, defeating the point of moving precedence to Rust.)

A sub-case: the `XEANMB` scoping spike (HKJ7VR) currently says "blocks on
Candidate 1's Db arm" — Candidate 1's Db arm HAS landed (tasks A6J5HG, ME7I5P,
A4YUYJ, YPRFQ2). Add the new edge `XEANMB depends_on 5NT2OC` to express the
Chain constraint; this keeps the existing Candidate-1 dependency satisfied while
adding the new hard prerequisite.

---

## 6. Implementation plan (3-5 tasks)

Drafted here for review; formal task creation is a downstream step after this
spike is accepted. The plan is **5 tasks**:

### Task 1 — `TickBootstrapRpc` trait + value types in `degenbot-pools`

- New submodule `rust/crates/degenbot-pools/src/tick_fetch/bootstrap.rs` with
  `TickBootstrapRpc`, `BootstrapTickWord`, `BootstrapTickError`. Re-export from
  `tick_fetch/mod.rs` (restructure the module: split `tick_fetch.rs` into
  `tick_fetch/{mod,miss,bootstrap}.rs`).
- No impls yet — just trait + value types. One fake-impl test (`FakeRpc`)
  mirrors `FakeFetcher` in `bot_core/mod.rs` to exercise the trait shape.
- **Validation gates:** `just test-rust`, `just lint-rust`,
  `just check-no-pyo3-in-cores`.

### Task 2 — `AlloyTickBootstrapRpc` pure-Rust impl in `degenbot-rpc` (or `degenbot-bot`)

- New impl of `TickBootstrapRpc` for an `AlloyProvider`-backed struct. Does the
  eth_call choreography (selector encode + sign-extend + call + ABI decode) —
  can extract the bit-extraction + decode helpers from `PyBotIo`'s
  `fetch_tick_bitmap` / `fetch_tick_data` (which become largely duplicate code
  paths — factor a `decode_tick_bitmap_word` + `decode_tick_data` helper).
- Standalone-consumer-facing: this is what `cargo add degenbot` users get.
- Tests: against `degenbot-rpc`'s existing fixture harness or
  `OfflineProvider` mock — verify Chain arm returns the same word + ticks
  Python Branch 3 would.
- **Validation gates:** `just test-rust`, `just lint-rust`,
  `just check-no-pyo3-in-cores`.

### Task 3 — `assemble_*_tick_map` Chain arm: add `chain` param, wire Store → Db → Chain precedence

- Modify `rust/crates/degenbot-bot/src/bot_core/tick_assembly.rs`:
  `assemble_v3_tick_map` + `assemble_v4_tick_map` gain the `chain:
  Option<&dyn TickBootstrapRpc>` param + the address/tick/spacing/block
  params. After Store + Db miss, call `chain.bootstrap_*_tick_word(...)` →
  return `Some((ticks, Sparse))` on hit; `None` if the bitmap was zero.
- The closure-based Store arm (Decision 4 (A) from UHPXSD) is **unchanged** —
  the Chain arm runs AFTER the Store arm's closure has returned + dropped the
  read guard, satisfying A4YUYJ's two-phase lock protocol (no `BotState` guard
  across the RPC read). Document this in the docstring.
- Tests: extend the existing 13 `tick_assembly` tests with Chain-arm variants
  (Chain hit after Store+Db miss; Chain returns zero-bitmap → None; Chain
  error propagation).
- **Validation gates:** `just test-rust`, `just lint-rust`,
  `just check-no-pyo3-in-cores`.

### Task 4 — PyO3 exposure: `PyBotIo`-backed `PyTickBootstrapRpc` adapter + `assemble_*` method signature update

- New `make_tick_bootstrap_rpc(io: Py<PyAny>, block: Option<u64>) ->
  Arc<dyn TickBootstrapRpc>` in `rust/crates/degenbot-python/src/bot/py_bot_io.rs`
  (or a new sibling `tick_bootstrap.rs` module — keep `py_bot_io.rs` focused).
- Update `PyBot::assemble_v3_tick_map` / `assemble_v4_tick_map` `#[pymethods]`
  to accept an optional `rpc: Option<PyBotIo>` arg (or a `block: int | None` +
  an `io: PyBotIo` — whatever the simplest binding is). The wrapper constructs
  `make_tick_bootstrap_rpc(io, block)` and passes `Some(&rpc)` to the core
  helper.
- The GIL release for the Chain arm's RPC read happens inside `py.detach`
  (same pattern as A4YUYJ's Db read + PyTickWordFetcher's callback hop).
- Tests: extend `bot::tests` with a Chain-arm end-to-end test — same shape as
  `assemble_v3_tick_map_returns_tracked_after_snapshot_load` but exercising
  the Chain path with a fake RPC.
- **Validation gates:** `just test`, `just lint`,
  `just check-no-pyo3-in-cores`.

### Task 5 — Python builder cutover: pass `rpc=io` to `assemble_*_tick_map`, remove inline Branch 3

- `src/degenbot/builders/v3_pool_builder.py` + `v4_pool_builder.py`: remove
  the `else:` Branch 3 block; the `assembled = self._py_bot.assemble_*` call
  now passes the `io` (wrapped as `rpc`) + `block`; never returns `None` in
  practice (cold-start has Db=None + Store empty + Chain=Some → Chain arm
  fires). The `coverage` variable always comes from the helper now.
- Delete the now-dead Branch 3 code in both builders.
- **Validation gates:** `just test-python` (snapshot tests still `Tracked`,
  sparse-RPC pools now come back from Rust as `Sparse` — same semantics, no
  visible behavior change), `just lint`.

### Dependency graph for the 5 tasks

```
Task 1 (trait)
  └─ Task 2 (alloy impl) ─┐
                          ├─ Task 3 (helper Chain arm)
                          └─ Task 4 (pyo3 adapter + wrapper)
                                └─ Task 5 (Python cutover)
```

(Task 3 and Task 4 are independent given 1+2; Task 5 requires 4.)

### After `5NT2OC` lands:

- `XEANMB` snapshot-store removal can start (its scoping spike `HKJ7VR`
  blocks on 5NT2OC; update `XEANMB depends_on 5NT2OC` in ergo).
- Branch 3 in Python is dead code post-Task 5; its removal is part of Task 5
  (not a separate follow-up).

---

## 7. Invariants re-affirmed

- **No `pyo3` in core crates** (`degenbot-pools`, `degenbot-bot`,
  `degenbot-rpc`, `degenbot-cl-math`): the new `TickBootstrapRpc` trait +
  `BootstrapTickWord` / `BootstrapTickError` value types live in
  `degenbot-pools`; the alloy impl lives in `degenbot-rpc` / `degenbot-bot`;
  the pyo3 adapter lives in `degenbot-python`. `just check-no-pyo3-in-cores`
  continues to pass.
- **A4YUYJ two-phase lock:** the Store arm's closure acquires + drops the
  `BotState` read guard before the Db read AND before the Chain arm RPC read
  — no `BotState` guard held across any I/O.
- **GIL released across RPC:** the Chain arm's alloy eth_call runs under
  `py.detach`, and `PyTickBootstrapRpc`'s GIL re-entry is the same brief
  per-call hop pattern as `PyTickWordFetcher` (no GIL re-entrancy deadlock
  risk — the Python-side `PyBotIo.fetch_tick_data` doesn't re-enter
  `BotState`).

---

## 8. Verification of spike acceptance criteria

- [x] Section 1 covers the `TickBootstrapRpc` trait signature.
- [x] Section 2 covers the pyo3 adapter design (concrete file path + struct
      shape + how it bridges `PyBotIo` to the trait).
- [x] Section 3 covers the `TickWordFetcher` consolidation decision.
- [x] Section 4 covers the Chain arm port target (a / b / c decision +
      rationale for (a)).
- [x] Section 5 covers the sequencing-vs-XEANMB decision (`5NT2OC` ships first;
      `XEANMB` depends_on `5NT2OC`).
- [x] Section 6 drafts a 5-task implementation plan.
- [x] Section 7 re-affirms the three-layer invariants (no-pyo3-in-cores +
      two-phase lock + GIL release).
