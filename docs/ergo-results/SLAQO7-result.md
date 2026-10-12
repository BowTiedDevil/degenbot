# SLAQO7 — P3: PoolRef + `Bot.pools_for_tokens`

Agent: `p3-poolref` (dispatched worker). Tree left dirty; nothing committed.

Epic: `4ROQ6S` — "Bot deep, Python light, DB privileged". P1 (`a4eb5180c`) landed;
the tracker family is gone and `build_pool` / `build_managed_pool` are the only
construction doors.

## What changed (files + intent)

### New — `src/degenbot/discovery.py`

- `PoolRef` — a frozen, slotted identity view over the database's stamped rows:
  `address` / `manager` + `pool_hash`, `kind` (`PoolKind`), the pair
  (`token0`/`token1` + their ids), the raw `db_id`, and the `graph_id`. **No state
  accessors, no engine handle, no liveness promise.**
- `pools_for_tokens(database_path, chain_id, token_a, token_b, *, pool_kinds=None)`
  — the pair query: resolve the two token ids (`db_resolve_token_ids`), run ONE
  graph read (`build_path_graph`, the exposed wrapper over the Rust
  `degenbot_db::fetch_path_graph_edges` bulk read), and project the edges whose
  token pair matches into refs ordered by `graph_id`. Nothing is constructed.
- The module docstring documents the three-layer ladder (discovery finds / the
  registry remembers / construction commits) and the seam boundary below.

### Changed — `src/degenbot/bot/_bot.py`

- `Bot.pools_for_tokens(token_a, token_b, *, pool_kinds=None) -> list[PoolRef]` —
  delegates to the module function with `self.database_path` / `self.chain_id`,
  and carries the ladder in its docstring.

### Changed — `src/degenbot/__init__.py`

- `PoolRef` exported (public API) + added to `__all__`.

### P1 residue — delete the empty `src/degenbot/sushiswap` package

- Whole package removed (it was docstring-only with an empty `__all__`). Verified
  zero Python importers: remaining `sushiswap` hits are data/variant string
  literals (`types/pool_type.py` `PoolVariant.SUSHISWAP`, `registry/deployments.json`
  `variant`/`dex_variant`, golden fixtures, test data).
- The C8b documentation folded into `src/degenbot/registry/deployment_loader.py`
  above the `"sushiswap-v3": UniswapV3Pool` map entry (the hollow `SushiswapV3Pool`
  collapse: the fork is slot0/tick-identical, so the canonical `UniswapV3Pool` is
  registered and the variant lives only as deployment data).

### New — `tests/database/test_pool_discovery.py`

Seeds one V2 + one V3 + one V4 pool for one token pair through the Rust write seams
(`seed_v2/v3/v4_topology`) and asserts: one ref per family with the expected
identity; a ref carries no state accessor / engine handle; an unknown token yields
`[]`; a pool of another pair is excluded; discovery constructs nothing (spy on
`Bot.build_pool`, registry still empty); and a discovered `ref.address` is a valid
key into the construction ladder (`build_pool` registry short-circuit).

## Gates

| Gate | Result |
|---|---|
| `uv run --no-sync ruff check` on the 5 touched files | **pass** |
| `uv run --no-sync ruff format --check` on the 5 touched files | **pass** (already formatted) |
| `uv run --no-sync pytest tests/database/test_pool_discovery.py -q` | **6 passed** |
| `uv run --no-sync pytest tests/database tests/registry tests/bot tests/test_bot.py tests/pathfinding tests/types tests/pool_handle tests/test_db_public_mirror.py -q` | **1098 passed, 28 skipped** (160 deselected) |
| `import degenbot; degenbot.PoolRef` | resolves (`degenbot.discovery.PoolRef`) |

No PENDING gates. One `just rebuild-if-stale` was run once (the foreign Rust stream
had staled the extension; rebuild took 46s and the receipt matched afterwards).

## Seam-missing analysis (STOP-and-report per the brief)

The brief said to prefer the existing Python DB read surface and to stop-and-report
if a new Rust exposure is genuinely required. **It is, for the kind params.**

What the exposed seam **can** express, and this chunk therefore builds on:

- `degenbot.pathfinding.build_path_graph` → edges `(token0_id, token1_id, graph_id,
  PoolKind)`, `v2v3_addresses`, `v4_lookups` `(manager, pool_hash)`,
  `pool_id_to_kind`;
- `degenbot.db.db_resolve_token_ids` → the pair's token ids.

What it **cannot**, so `PoolRef`'s kind-param fields default to `None`:

| `PoolRef` field | Why `None` today |
|---|---|
| `kind_string` (e.g. `"uniswap_v3"`) | the Rust `PathGraphData` carries `pool_id_to_kind_string`, but the Python `PathGraph` pyclass does not expose it (only the collapsed `pool_id_to_kind`) |
| `factory` | needs the `exchanges` row per pool (`BotIo.fetch_exchange`), i.e. an N+1 open, and V4 has no `pools` row to reach it from |
| `fee`, `tick_spacing`, `stable` | per-family subclass columns (`uniswap_v3_pools`, `aerodrome_v2_pools`, …) — not on the graph read at all |
| `update_block` | the liquidity-update marker column — not on the graph read |

Two further limits of using `build_path_graph` as a discovery primitive (documented
in the docstring):

1. It applies a **degree >= 2** candidate-token filter (a cycle needs two pools), so
   a pair whose tokens each appear in fewer than two pools of the requested kinds
   yields no refs. A general discovery read must not require a cycle.
2. Requesting a single `pool_kinds` family can empty the result for the same reason.

The correct primitive already exists in Rust, unexposed:
`degenbot_db::discovery_read` (`DiscoveryPoolRow` — `DiscoveryV2Row` / `V3Row` /
`V4Row`). Each row carries the base pool + exchange + `token0`/`token1` **rows**
(addresses), `kind`, `fee_token0/1` + `fee_denominator`, `tick_spacing`, `stable`
(V2), and `liquidity_update_block/log_index` (V3/V4) — i.e. every `PoolRef` field.
**Recommended follow-up (manager schedules into a quiet Rust window):** expose
`DegenbotDb::fetch_discovery_rows(chain_id)` (and/or the per-family variants) as a
`db_fetch_discovery_rows` Python seam, then re-point `pools_for_tokens` at it. That
collapses discovery to a single DB read, fills every `None` field, and removes the
degree>=2 / cycle limitation. No Python-side rework is needed beyond the projection.

## Foreign tree activity observed (left alone)

An unrelated concurrent stream is active in the workspace (the P2b snapshot-retire
worker and the Rust solvers/aave session):

- Rust: `rust/crates/engine/degenbot-solvers/**` (incl. new `solidly_t.rs`),
  `rust/crates/integrations/degenbot-aave/**`, `rust/crates/shells/degenbot-cli*/**`,
  `rust/crates/shells/degenbot-python/src/aave_updater.rs`, `rust/crates/facade/degenbot/**`.
- Python/stubs/docs: `src/degenbot/_ffi/aave.pyi`, `docs/cli/aave.md`,
  `docs/rust-cli.md`, `src/degenbot/arbitrage/engine_registry.py`,
  `src/degenbot/runner/bot_runner.py`, `tests/arbitrage/**`, `tests/fakes/engine.py`,
  `tests/helpers/boot_actors.py`, `docs/ergo-results/gmdx5v-result.md`.

None of these were touched by this chunk. Note for the manager: the P2b worker owns
`engine_registry.py` / `bot_runner.py` (file-disjoint from this chunk — this chunk
touched `bot/_bot.py`, which the P2b brief was told to avoid).
