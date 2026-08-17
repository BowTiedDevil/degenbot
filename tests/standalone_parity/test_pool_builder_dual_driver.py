"""Tier-2 behavioral dual-driver parity — Rust pool builder identity+state (A2QRWO).

The behavioral companion to `rust/crates/degenbot/tests/parity_pool_builder.rs`.
Proves the **same** canonical V3 identity+state fixture, driven through the
**Python consumer** (`RustBot`/`register_v3_pool` — the PyO3 binding, the same
`RegisterV3PoolParams` the Rust `PoolBuilder.build_v3` emits after its I/O),
registers the **same deterministic pool id** and reports IDENTICAL identity +
state as the **Rust consumer** (`BotState` directly).

The fixture + expected outputs are loaded from the SHARED file
`tests/standalone_parity/fixtures/pool_builder.json`, which the Rust parity
test (`parity_pool_builder.rs`) ALSO loads — a one-sided fixture edit fails
BOTH sides mechanically (the shared-fixture contract, HRT356).
"""

from __future__ import annotations

import json
from pathlib import Path

from degenbot.bot import RustBot

# ---- the shared canonical fixture (loaded, not copied) ----
_FIXTURE_PATH = Path(__file__).parent / "fixtures" / "pool_builder.json"
_FIXTURE = json.loads(_FIXTURE_PATH.read_text())
_F = _FIXTURE["fixture"]
_E = _FIXTURE["expected"]

_TOKEN0 = _F["token0"]
_TOKEN1 = _F["token1"]
_POOL = _F["pool"]
_FACTORY = _F["factory"]
_FEE = _F["fee"]
_TICK_SPACING = _F["tick_spacing"]
_SQRT_PRICE_X96 = int(_F["sqrt_price_x96"])
_LIQUIDITY = int(_F["liquidity"])
_TICK = _F["tick"]
_UPDATE_BLOCK = _F["update_block"]
_EXPECTED_POOL_ID = _E["pool_id"]


def test_python_consumer_pool_builder_identity_state_matches_fixture() -> None:
    """The RustBot Python driver reproduces the recorded pool identity+state.

    Python side of the Tier-2 dual-driver gate (pool builder identity+state).
    The Rust side (`parity_pool_builder.rs`) loads the same fixture file and
    drives it through `BotState` directly; both MUST register the same pool id
    and report Identical identity (token0/token1/fee/tick_spacing) + state
    (sqrt_price_x96/liquidity/tick) + Tracked coverage. Divergence = a lossy
    FFI seam on the registration/identity path.
    """
    py_bot = RustBot()
    # ADR-006: pool tokens must be registered in the same Bot as the pool for
    # `get_token0/get_token1` to resolve (names/symbols/decimals are not part
    # of the parity assertion — only the addresses are).
    py_bot.register_token(address=_TOKEN0, name="Token0", symbol="TK0", decimals=18, chain_id=1)
    py_bot.register_token(address=_TOKEN1, name="Token1", symbol="TK1", decimals=18, chain_id=1)
    pool_id = py_bot.register_v3_pool(
        address=_POOL,
        token0=_TOKEN0,
        token1=_TOKEN1,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        factory=_FACTORY,
        sqrt_price_x96=_SQRT_PRICE_X96,
        liquidity=_LIQUIDITY,
        tick=_TICK,
        tick_data={_TICK: (_LIQUIDITY, 0, 0)},
        update_block=_UPDATE_BLOCK,
        coverage="tracked",
    )
    assert pool_id == _EXPECTED_POOL_ID, (
        "first registered pool must get the fixture's deterministic id — "
        f"both consumers MUST agree (got {pool_id}, want {_EXPECTED_POOL_ID})"
    )

    py_pool = py_bot.get_pool(pool_id)
    assert py_pool is not None, "registered pool must be retrievable by id"

    # Identity (the builder's on-chain/DB-resolved immutable values).
    assert py_pool.fee == _FEE, f"fee identity diverged (got {py_pool.fee}, want {_FEE})"
    assert py_pool.tick_spacing == _TICK_SPACING, (
        f"tick_spacing identity diverged (got {py_pool.tick_spacing}, want {_TICK_SPACING})"
    )
    assert py_pool.get_token0().address.lower() == _TOKEN0, "token0 identity diverged"
    assert py_pool.get_token1().address.lower() == _TOKEN1, "token1 identity diverged"

    # State (the builder's live scalars).
    assert py_pool.sqrt_price_x96 == _SQRT_PRICE_X96, "sqrt_price state diverged"
    assert py_pool.liquidity == _LIQUIDITY, "liquidity state diverged"
    assert py_pool.tick == _TICK, "tick state diverged"

    # Coverage: a Tracked (dense) registration carries the assembled tick map —
    # the sparse fallback returns an empty snapshot. Proves the builder's
    # DB-tracked decision is identical across consumers.
    assert len(py_pool.tick_data_snapshot()) != 0, "Tracked pool must carry its tick data"
