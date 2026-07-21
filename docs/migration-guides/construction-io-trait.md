# Migration guide: Construction-I/O core trait (slice A)

Architecture review 2025-07-18 / candidate 1. This documents the first slice of
the `ConstructionIo` deepening — the core trait + native adapters landed in
`degenbot-bot`/`bot_core/construction_io/`, with `PyBotIo`'s 19 atomic methods
(7 generic RPC + 12 DB reads/writes) delegating through it. The 27 choreographed
encode→call→decode wrappers stay on `PyBotIo` this slice — they move core-side
in a follow-up (the builder-choreography port).

## What changed

### `Bot` owns a `ConstructionIo` handle (ADR-003)

`Bot` gains an interior-mutable `construction_io: RwLock<Option<Arc<ConstructionIo>>>`
field, attached post-construction via `Bot::set_construction_io(io)`. The
Python `Bot.__init__` path calls `PyBot.attach_construction_io(provider,
database_path=…)` at construction time; the standalone-Rust path calls
`Bot::set_construction_io` directly. The bare `Bot::new(chain_id)` test-fixture
path leaves the handle `None` (the 19 atomic methods degrade to the no-I/O shape).

### Two async core traits + a composite handle

`rust/crates/degenbot-bot/src/bot_core/construction_io/`:

- `trait DbConstruction` — the 12 construction-time DB reads/writes (async,
  `Send + Sync`). Returns `degenbot_db::rows::*` core row types directly (no
  `Py*` mirror at the seam). Propagates `DbError` **loudly** (Decision 8 (A) —
  the trait never swallows; the choreography decides whether to degrade).
- `trait RpcConstruction` — the 7 generic RPC methods (`get_block_number`,
  `get_block`, `get_block_timestamp`, `get_code`, `get_balance`, `call`,
  `call_raw`). Propagates `ProviderError`.
- `struct ConstructionIo` — composite handle (`Arc<dyn DbConstruction + Send +
  Sync>` + `Arc<dyn RpcConstruction + Send + Sync>`), held by `Bot`.

The traits are `#[async_trait]` (desugared to `Pin<Box<dyn Future + Send>>`) so
they're dyn-compatible; the PyO3 boundary does the `block_on` (release GIL →
drive the future → wrap result).

### Three adapters

- `NoDb` — every `DbConstruction` method returns the empty/`None` shape. The
  no-DB path (so `ConstructionIo.db` is always `Some`) AND the first in-memory
  test fake.
- `DegenbotDbConstruction` — holds a **persistent** `DegenbotDb` (held
  connection, not per-call `DegenbotDb::open`). Deletes the 12×-open
  boilerplate that lived on `PyBotIo`. Opens with `open_for_writes` (the
  construction executor does reads AND the `update_erc20_token_metadata`
  write-back). To stay within the 0.6.x Alembic-retention invariant, the
  adapter no-ops DB-open failures (file missing → `NoDb`) and removes the
  `AlembicCurrent` query_read-only marker only as missing-file degradation —
  none of the forbidden-until-0.7 seams are touched.
- `AlloyRpcConstruction` — wraps `degenbot-rpc`'s `AlloyProvider`; alloy-only.

### `PyBotIo` cutover (narrow)

`PyBotIo`'s **12 DB methods + 7 generic RPC methods** delegate through
`construction_io` when attached; the **27 choreography wrappers**
(`fetch_v2_reserves`, `fetch_erc20_metadata`, `fetch_factory_address`, …)
**stay unchanged**. `PyBotIo` retains its `provider: Py<PyAny>` field +
`forward_call_to_provider` for the choreography path — **temporary**, deleted
with the builder-choreography port.

## Breaking changes

### 1. The `RpcConstruction` trait is alloy-only (Q6-narrow)

Production construction-I/O's generic RPC surface requires a
`PyAlloyProvider`-backed provider. **Non-alloy Python providers are no longer
supported for the 7 generic RPC methods** when a `ConstructionIo` is attached —
supply a `PyAlloyProvider` (live or the offline-from-JSON shell).

`PyBot.attach_construction_io` is a **soft skip** for non-alloy providers: the
handle is left `None`, and `PyBotIo`'s 19 atomic methods fall back to the
inlined `self.alloy` path (and the Python-provider fallback for the 7 RPC). This
preserves the legacy test-double path (Mock/MagicMock providers in the
choreography suite); the fallback is temporary and slated for deletion with the
choreography port. **Production drivers MUST supply an alloy provider** to get
the trait delegation.

### 2. DB errors propagate loudly (Decision 8 (A) unified)

`DbConstruction` propagates `DbError`. Construction-time DB failures that
previously degraded silently (the old `contextlib.suppress(Exception)`-style
behavior on `PyBotIo`'s per-call `DegenbotDb::open`) now surface as the trait's
`DbError`, wrapped at the PyO3 boundary into `ValueError`.

Handle transient SQLite locks at the `DegenbotDb` layer (longer
`busy_timeout`), not by silent degrade. A missing DB **file** at
`attach_construction_io` time is still tolerated (→ `NoDb`), matching the
prior "file not yet created by SQLAlchemy" cold-start semantics; a corrupt or
schema-mismatched DB surfaces loudly.

### 3. Held DB connection (no per-call `DegenbotDb::open`)

`DegenbotDbConstruction` holds a persistent `DegenbotDb`. Construction reads see
the latest committed snapshot at statement start (WAL); the write-back
(`update_erc20_token_metadata`) uses the same held connection. The 12×
`DegenbotDb::open` / `open_for_writes` calls that lived in `PyBotIo`'s method
bodies are gone — one open at attach time, one held for the `Bot`'s lifetime.

Standalone-Rust consumers construct `DegenbotDbConstruction::new(db)` directly
(per the standalone-Rust constraint, no `pyo3` in the core crate; the adapter
is `pub` from `degenbot-bot`).

## Standalone-Rust usage

```rust
use degenbot_bot::bot_core::construction_io::{
    AlloyRpcConstruction, ConstructionIo, DegenbotDbConstruction, NoDb,
};
use degenbot_rpc::AlloyProvider;

let bot = Bot::new(chain_id);
let db = DegenbotDbConstruction::new(/* DegenbotDb */);
let rpc = AlloyRpcConstruction::new(AlloyProvider::builder().build().await?);
bot.set_construction_io(ConstructionIo::new(
    std::sync::Arc::new(db),
    std::sync::Arc::new(rpc),
));
// bot.construction_io_arc() → Option<Arc<ConstructionIo>> for the builders.
```

The `NoDb` adapter is the default for a cold-start standalone `Bot` (no
`database_path`) — the 12 DB methods return the empty/`None` shape, the 7 RPC
methods go through `AlloyRpcConstruction`.

## Migration steps (Python driver)

No code changes required for drivers already using `Bot(provider=…)` with an
alloy-backed provider — `Bot.__init__` calls `attach_construction_io`
automatically.

If you previously passed a non-alloy provider to `Bot(provider=…)` and relied on
the 7 generic RPC methods reaching it via `attach_construction_io`: supply a
`PyAlloyProvider` instead. The choreography wrappers continue to work with any
provider for now (temporary).

## Files

- `rust/crates/degenbot-bot/src/bot_core/construction_io/{mod,handle,adapters,tests}.rs`
  — the trait + adapters + tests.
- `rust/crates/degenbot-bot/src/bot_core/mod.rs` — `Bot.construction_io` field +
  `set_construction_io` / `construction_io_arc` accessors.
- `rust/crates/degenbot-python/src/bot/mod.rs` — `PyBot::attach_construction_io`.
- `rust/crates/degenbot-python/src/bot/py_bot_io.rs` — 12 DB + 7 RPC delegation,
  `PyBotIo::attach_construction_io`, `PyBotIo::new` eager-build path.
- `src/degenbot/bot/_bot.py` — `Bot.__init__` attach wiring.

## Follow-ups (the rest of candidate 1)

- The 27 choreography wrappers move core-side (the builder-choreography port) —
  after which `PyBotIo.provider` + `forward_call_to_provider` + the
  non-alloy fallback are deleted.
- ADR-014 lands after the slice (per the slice authorization): the formal
  record of the `ConstructionIo` trait + adapter pattern.
- `degenbot-rpc` is promoted to a non-optional dep (candidate 1 step Q5) once
  the choreography adapters land.
