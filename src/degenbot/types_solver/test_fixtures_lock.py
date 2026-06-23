"""Cross-language ABI lock — Python encoders MUST match `fixtures.json`.

The TypeScript side (``coordinator/src/types/fixtures.gen.ts``) is the
canonical emitter; viem's ``encodeFunctionData`` produces the ``calldata``
hex stored in ``coordinator/src/types/fixtures.json``. This test loads
each fixture, runs the Python wire-decoder + encoder, and asserts the
resulting hex is byte-identical.

If this test fails, the Python encoder has diverged — investigate the
encoder, NOT the fixtures. The fixtures file is the contract.

Run
---
    pytest solver/driver/types/test_fixtures_lock.py
"""

from __future__ import annotations

import json
import operator
from pathlib import Path
from typing import Any

import pytest

from degenbot.types_solver.codec import (
    COMPOSE_FOUR_LEG_SELECTOR,
    COMPOSE_FOUR_LEG_SIGNATURE,
    EXECUTE_NATIVE_ARB_SELECTOR,
    EXECUTE_NATIVE_ARB_SIGNATURE,
    MATCH_INTERNAL_SELECTOR,
    MATCH_INTERNAL_SIGNATURE,
    _sig_keccak,
    encode_compose_four_leg,
    encode_match_internal,
    encode_native_arb,
    selectors_match_signatures,
)
from degenbot.types_solver.executor import (
    DexKind,
    FlashProtocol,
)
from degenbot.types_solver.wire import (
    compose_params_from_wire,
    from_wire_json,
    match_params_from_wire,
    native_arb_from_wire,
)

# ---------------------------------------------------------------------------
# Locked fixtures location. Resolves up to repo root, then to the canonical
# coordinator path. The pyproject keeps `solver/` and `coordinator/` as
# sibling top-level workspaces, so the relative jump is stable.
# ---------------------------------------------------------------------------


def _resolve_fixtures_path() -> Path:
    """Locate ``coordinator/src/types/fixtures.json`` by walking up from this file.

    The canonical fixtures live in the parent monorepo's ``coordinator/`` workspace.
    A hardcoded ``parents[N]`` jump silently broke when degenbot was vendored under
    ``vendor/degenbot/`` (the index no longer pointed at the repo root, so the lock
    test errored-out at collection and stopped guarding selector drift). Walk up
    instead so the anchor survives relocation.
    """
    rel = Path("coordinator") / "src" / "types" / "fixtures.json"
    for parent in Path(__file__).resolve().parents:
        candidate = parent / rel
        if candidate.is_file():
            return candidate
    # Fall back to the historical sibling-workspace layout for a clear error message.
    return Path(__file__).resolve().parents[3] / rel


_FIXTURES_PATH: Path = _resolve_fixtures_path()


def _load_fixtures() -> list[dict[str, Any]]:
    """Read fixtures.json and return the flat fixture list.

    Schema mirror of ``coordinator/src/types/fixtures.ts``:
    ``{version, note, fixtures: [{name, fn, params, calldata}, ...]}``.
    """
    raw: dict[str, Any] = from_wire_json(_FIXTURES_PATH.read_text(encoding="utf-8"))
    fixtures: list[dict[str, Any]] = list(raw["fixtures"])
    return fixtures


def _fixtures_for(fn: str) -> list[dict[str, Any]]:
    """Filter fixtures by the discriminator field used in fixtures.json."""
    return [f for f in _load_fixtures() if f["fn"] == fn]


# ---------------------------------------------------------------------------
# Sanity guards — fail loud if the fixture file is missing or malformed.
# ---------------------------------------------------------------------------


def test_fixtures_file_exists() -> None:
    assert _FIXTURES_PATH.exists(), f"fixtures missing: {_FIXTURES_PATH}"


def test_fixtures_envelope_shape() -> None:
    raw: dict[str, Any] = from_wire_json(_FIXTURES_PATH.read_text(encoding="utf-8"))
    assert raw["version"] == 1, "fixtures.json: unexpected version field"
    assert isinstance(raw["fixtures"], list)
    assert len(raw["fixtures"]) >= 6, "expected at least the 6 baseline fixtures"


# ---------------------------------------------------------------------------
# Selector + signature regression guards.
# ---------------------------------------------------------------------------


def test_selectors_match_canonical_signatures() -> None:
    """Locked selectors == keccak256(canonical_signature)[:4]."""
    assert selectors_match_signatures(), (
        "selector / signature drift detected — one of the locked 4-byte "
        "selectors no longer matches the canonical Solidity signature it claims"
    )


def test_execute_native_arb_selector_is_locked() -> None:
    """``executeNativeArb`` selector is the locked value 0xf6f6add1."""
    assert bytes.fromhex("f6f6add1") == EXECUTE_NATIVE_ARB_SELECTOR
    assert _sig_keccak(EXECUTE_NATIVE_ARB_SIGNATURE) == EXECUTE_NATIVE_ARB_SELECTOR


def test_match_internal_selector_is_locked() -> None:
    """``matchInternal`` selector is the locked value 0x210ea473 (12-field MatchParams)."""
    assert bytes.fromhex("210ea473") == MATCH_INTERNAL_SELECTOR
    assert _sig_keccak(MATCH_INTERNAL_SIGNATURE) == MATCH_INTERNAL_SELECTOR


def test_compose_four_leg_selector_is_locked() -> None:
    """``composeFourLeg`` selector is the locked value 0x2e1977d2 (12-field ComposeParams)."""
    assert bytes.fromhex("2e1977d2") == COMPOSE_FOUR_LEG_SELECTOR
    assert _sig_keccak(COMPOSE_FOUR_LEG_SIGNATURE) == COMPOSE_FOUR_LEG_SELECTOR


# ---------------------------------------------------------------------------
# Enum-ordinal pinning — guards against accidental Python-side reordering.
# ---------------------------------------------------------------------------


def test_flash_protocol_ordinals() -> None:
    """``FlashProtocol`` integer values match the Solidity enum ordinals exactly."""
    assert int(FlashProtocol.AAVE_V3) == 0
    assert int(FlashProtocol.MORPHO) == 1
    assert int(FlashProtocol.ERC3156) == 2
    assert int(FlashProtocol.UNI_V3) == 3
    assert int(FlashProtocol.UNI_V2) == 4
    assert int(FlashProtocol.UNI_V4) == 5


def test_dex_kind_ordinals() -> None:
    """``DexKind`` integer values match the Solidity enum ordinals exactly.

    Phase E appended ordinals 7..10, Phase F.3 appended 11..13, and the
    registry POC appended 14..28. The enum is locked across Solidity, TS,
    Rust, and Python. A drift here breaks ABI parity.
    """
    assert int(DexKind.UNI_V2_STYLE) == 0
    assert int(DexKind.UNI_V3_POOL) == 1
    assert int(DexKind.UNI_V4_POOL_MANAGER) == 2
    assert int(DexKind.CURVE) == 3
    assert int(DexKind.RESERVED) == 4
    assert int(DexKind.AGGREGATOR_V6) == 5
    assert int(DexKind.MORPHO_BLUE_ACTION) == 6
    # Phase E additions — see docs/architecture/dex-kind-expansion.md.
    assert int(DexKind.ALGEBRA) == 7
    assert int(DexKind.SOLIDLY) == 8
    assert int(DexKind.CURVE_NG) == 9
    assert int(DexKind.BALANCER_V2) == 10
    # Phase F.3 additions.
    assert int(DexKind.MAVERICK_V2) == 11
    assert int(DexKind.DODO_PMM) == 12
    assert int(DexKind.FLUID_DEX) == 13
    # Registry POC additions.
    assert int(DexKind.BALANCER_V3) == 14
    assert int(DexKind.KYBER_ELASTIC) == 15
    assert int(DexKind.LFJ_LIQUIDITY_BOOK) == 16
    assert int(DexKind.GMX_V2) == 17
    assert int(DexKind.WOMBAT) == 18
    assert int(DexKind.BEBOP) == 19
    assert int(DexKind.HASHFLOW) == 20
    assert int(DexKind.WOOFI) == 21
    assert int(DexKind.OKX_DEX) == 22
    assert int(DexKind.ENSO) == 23
    assert int(DexKind.SQUID) == 24
    assert int(DexKind.LIFI) == 25
    assert int(DexKind.RANGO) == 26
    assert int(DexKind.RUBIC) == 27
    assert int(DexKind.NATIVE) == 28


# ---------------------------------------------------------------------------
# Per-function fixture lock — the load-bearing equality of this whole phase.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "fixture", _fixtures_for("executeNativeArb"), ids=operator.itemgetter("name")
)
def test_native_arb_fixtures_lock(fixture: dict[str, Any]) -> None:
    """Each NativeArbParams fixture must round-trip through wire+encode unchanged."""
    params = native_arb_from_wire(fixture["params"])
    actual = encode_native_arb(params)
    assert actual == fixture["calldata"], (
        f"calldata mismatch for {fixture['name']}\nexpected: {fixture['calldata']}\nactual:   {actual}"
    )


@pytest.mark.parametrize("fixture", _fixtures_for("matchInternal"), ids=operator.itemgetter("name"))
def test_match_internal_fixtures_lock(fixture: dict[str, Any]) -> None:
    """Each MatchParams fixture must round-trip through wire+encode unchanged."""
    params = match_params_from_wire(fixture["params"])
    actual = encode_match_internal(params)
    assert actual == fixture["calldata"], (
        f"calldata mismatch for {fixture['name']}\nexpected: {fixture['calldata']}\nactual:   {actual}"
    )


@pytest.mark.parametrize(
    "fixture", _fixtures_for("composeFourLeg"), ids=operator.itemgetter("name")
)
def test_compose_four_leg_fixtures_lock(fixture: dict[str, Any]) -> None:
    """Each ComposeParams fixture must round-trip through wire+encode unchanged."""
    params = compose_params_from_wire(fixture["params"])
    actual = encode_compose_four_leg(params)
    assert actual == fixture["calldata"], (
        f"calldata mismatch for {fixture['name']}\nexpected: {fixture['calldata']}\nactual:   {actual}"
    )


# ---------------------------------------------------------------------------
# Coverage of every fn discriminator we expect in fixtures.json — guards
# against silently dropping a function family from the lock.
# ---------------------------------------------------------------------------


def test_all_three_function_families_have_fixtures() -> None:
    """Every Executor strategy entry point is represented in fixtures.json."""
    fixtures = _load_fixtures()
    fns = {f["fn"] for f in fixtures}
    assert "executeNativeArb" in fns
    assert "matchInternal" in fns
    assert "composeFourLeg" in fns


# ---------------------------------------------------------------------------
# Wire decoder edge cases — exercised here so the lock test alone gives full
# branch coverage for wire.py.
# ---------------------------------------------------------------------------


def test_from_wire_json_rejects_non_object_top_level() -> None:
    """A bare list at the top level is not a fixtures envelope."""
    with pytest.raises(TypeError, match="expected JSON object"):
        from_wire_json("[]")


def test_wire_decimal_string_bigint_round_trip() -> None:
    """``flashAmount`` decimal-string survives as an exact int."""
    fixtures = _fixtures_for("executeNativeArb")
    fixture = next(f for f in fixtures if f["name"] == "NativeArbParams.1swap")
    params = native_arb_from_wire(fixture["params"])
    # Locked from fixtures.json — 1e9 = 1 USDC unit's worth of base units.
    assert params.flash_amount == 1_000_000_000
    assert params.swaps[0].amount_out_min == 250_000_000_000_000_000


def test_wire_empty_calldata_decodes_to_empty_bytes() -> None:
    """``"0x"`` must round-trip to empty ``bytes`` through encode + lock."""
    fixtures = _fixtures_for("composeFourLeg")
    fixture = next(f for f in fixtures if f["name"] == "ComposeParams.arbOnly")
    params = compose_params_from_wire(fixture["params"])
    # acrossFillCalldata / cowFillCalldata / uniswapxRebalanceCalldata are all "0x".
    assert params.across_fill_calldata == "0x"
    assert params.cow_fill_calldata == "0x"
    assert params.uniswapx_rebalance_calldata == "0x"
    # And the lock test parametrize above already proves the encode side
    # also produces the right bytes for "0x" — but assert again here to
    # exercise the early-return branch in `_hex_to_bytes`.
    actual = encode_compose_four_leg(params)
    assert actual == fixture["calldata"]


def test_dataclasses_are_frozen() -> None:
    """Mutating any field on the strategy params must raise."""
    fixtures = _fixtures_for("executeNativeArb")
    params = native_arb_from_wire(fixtures[0]["params"])
    with pytest.raises((AttributeError, TypeError)):  # frozen dataclass
        params.flash_amount = 0  # type: ignore[misc]


def test_swaps_are_tuple_not_list() -> None:
    """SwapStep collections are immutable tuples to keep the dataclass hashable."""
    fixtures = _fixtures_for("executeNativeArb")
    params = native_arb_from_wire(fixtures[0]["params"])
    assert isinstance(params.swaps, tuple)


def test_inflows_are_tuple_not_list() -> None:
    """Both inflow arrays are immutable tuples, not lists."""
    fixtures = _fixtures_for("matchInternal")
    params = match_params_from_wire(fixtures[0]["params"])
    assert isinstance(params.expected_token_inflows, tuple)
    assert isinstance(params.expected_token_inflow_min, tuple)


# ---------------------------------------------------------------------------
# Manual round-trip — fully synthetic (no fixture dependency) so the test
# covers the encode path even if fixtures.json is unreadable for any reason.
# ---------------------------------------------------------------------------


def test_synthetic_native_arb_encode_starts_with_selector() -> None:
    """A minimal hand-rolled NativeArbParams must produce calldata prefixed
    with the locked selector, even with zero swaps."""
    payload = {
        "flashLender": "0x" + "11" * 20,
        "flashProtocol": 0,
        "flashToken": "0x" + "22" * 20,
        "flashAmount": "1000",
        "swaps": [],
        "minProfit": "10",
        "deadline": "1730000000",
    }
    params = native_arb_from_wire(payload)
    out = encode_native_arb(params)
    assert out.startswith("0xf6f6add1")


def test_synthetic_match_internal_encode_starts_with_selector() -> None:
    """Hand-rolled empty MatchParams must produce calldata prefixed with the locked selector."""
    payload = {
        "cowSettlementCalldata": "0x",
        "uniswapxBatchCalldata": "0x",
        "expectedTokenInflows": [],
        "expectedTokenInflowMin": [],
        "flashLender": "0x" + "33" * 20,
        "flashProtocol": 1,
        "flashToken": "0x" + "44" * 20,
        "flashAmount": "0",
        "minProfit": "0",
        "deadline": "0",
        "settlementFundingToken": "0x" + "00" * 20,
        "settlementFundingMax": "0",
    }
    params = match_params_from_wire(payload)
    out = encode_match_internal(params)
    assert out.startswith("0x210ea473")


def test_synthetic_compose_four_leg_encode_starts_with_selector() -> None:
    """Hand-rolled empty ComposeParams must produce calldata prefixed with the locked selector."""
    payload = {
        "acrossFillCalldata": "0x",
        "arbSwaps": [],
        "cowFillCalldata": "0x",
        "uniswapxRebalanceCalldata": "0x",
        "flashLender": "0x" + "55" * 20,
        "flashProtocol": 2,
        "flashToken": "0x" + "66" * 20,
        "flashAmount": "0",
        "minProfit": "0",
        "deadline": "0",
        "settlementFundingToken": "0x" + "00" * 20,
        "settlementFundingMax": "0",
    }
    params = compose_params_from_wire(payload)
    out = encode_compose_four_leg(params)
    assert out.startswith("0x2e1977d2")


# ---------------------------------------------------------------------------
# JSON helper coverage.
# ---------------------------------------------------------------------------


def test_from_wire_json_parses_simple_object() -> None:
    """``from_wire_json`` round-trips a trivial JSON object losslessly."""
    parsed = from_wire_json(json.dumps({"a": 1, "b": [2, 3]}))
    assert parsed == {"a": 1, "b": [2, 3]}
