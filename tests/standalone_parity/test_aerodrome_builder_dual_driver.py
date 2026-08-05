"""Tier-2 behavioral dual-driver parity — Aerodrome V2 builder identity+state (SSSXG6).

The behavioral companion to `rust/crates/degenbot/tests/parity_aerodrome_builder.rs`.
Proves the **same** canonical Aerodrome V2 identity+state fixture, driven through
the **Python consumer** (`PyBot`/`register_aerodrome_pool` — the PyO3 binding,
the same `RegisterAerodromeV2PoolParams` the Rust `PoolBuilder.build_aerodrome_v2`
emits after its on-chain `stable()`+`getFee()`+reserves I/O), registers the
**same deterministic pool id** and reports IDENTICAL identity + state as the
**Rust consumer** (`BotState` directly).

The fixture + expected outputs are loaded from the SHARED file
`tests/standalone_parity/fixtures/aerodrome_pool_builder.json`, which the Rust
parity test (`parity_aerodrome_builder.rs`) ALSO loads — a one-sided fixture
edit fails BOTH sides mechanically (the shared-fixture contract, HRT356).

The factory is a fixture (not in any shipped deployments), so the EIP-1167
CREATE2 verify in `register_aerodrome_pool` skips (ad-hoc path) — this parity
targets the registration/identity FFI seam, not CREATE2 verification.
"""

from __future__ import annotations

import json
from pathlib import Path

from degenbot.bot import PyBot

# ---- the shared canonical fixture (loaded, not copied) ----
_FIXTURE_PATH = Path(__file__).parent / "fixtures" / "aerodrome_pool_builder.json"
_FIXTURE = json.loads(_FIXTURE_PATH.read_text())
_F = _FIXTURE["fixture"]
_E = _FIXTURE["expected"]

_TOKEN0 = _F["token0"]
_TOKEN1 = _F["token1"]
_POOL = _F["pool"]
_FACTORY = _F["factory"]
_VARIANT = _F["variant"]
_STABLE = _F["stable"]
_FEE_NUMER = _F["fee_numer"]
_FEE_DENOM = _F["fee_denom"]
_RESERVE0 = int(_F["reserve0"])
_RESERVE1 = int(_F["reserve1"])
_UPDATE_BLOCK = _F["update_block"]
_EXPECTED_POOL_ID = _E["pool_id"]


def test_python_consumer_aerodrome_builder_identity_state_matches_fixture() -> None:
    """The PyBot Python driver reproduces the recorded Aerodrome identity+state.

    Python side of the Tier-2 dual-driver gate (pool builder identity+state).
    The Rust side (`parity_aerodrome_builder.rs`) loads the same fixture file
    and drives it through `BotState` directly; both MUST register the same pool
    id and report Identical identity (token0/token1/factory/variant/stable/fee)
    + state (reserve0/reserve1/update_block). Divergence = a lossy FFI seam on
    the registration/identity path.
    """
    py_bot = PyBot()
    # ADR-006: pool tokens must be registered in the same Bot as the pool for
    # `get_token0/get_token1` to resolve (names/symbols/decimals are not part
    # of the parity assertion — only the addresses are).
    py_bot.register_token(
        address=_TOKEN0, name="Token0", symbol="TK0", decimals=18, chain_id=1
    )
    py_bot.register_token(
        address=_TOKEN1, name="Token1", symbol="TK1", decimals=18, chain_id=1
    )
    pool_id = py_bot.register_aerodrome_pool(
        address=_POOL,
        token0=_TOKEN0,
        token1=_TOKEN1,
        factory=_FACTORY,
        variant=_VARIANT,
        stable=_STABLE,
        fee_numer=_FEE_NUMER,
        fee_denom=_FEE_DENOM,
        reserve0=_RESERVE0,
        reserve1=_RESERVE1,
        update_block=_UPDATE_BLOCK,
    )
    assert pool_id == _EXPECTED_POOL_ID, (
        "first registered pool must get the fixture's deterministic id — "
        f"both consumers MUST agree (got {pool_id}, want {_EXPECTED_POOL_ID})"
    )

    handle = py_bot.get_pool(pool_id)
    assert handle is not None, "registered pool must be retrievable by handle"

    # Identity (addresses are EIP-55 checksummed on the handle vs the lowercase
    # fixture — compare caselessly).
    assert handle.token0_address.lower() == _TOKEN0.lower()
    assert handle.token1_address.lower() == _TOKEN1.lower()
    assert handle.variant == _VARIANT
    assert handle.aerodrome_stable is _STABLE
    assert handle.aerodrome_fee == (_FEE_NUMER, _FEE_DENOM)

    # State.
    assert handle.aerodrome_reserve0 == _RESERVE0
    assert handle.aerodrome_reserve1 == _RESERVE1
