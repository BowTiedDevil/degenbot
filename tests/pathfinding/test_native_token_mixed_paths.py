"""Public-NativeToken pathfinding across mixed V4 paths (ADR-097 acceptance).

The required matrix, driven with conceptual ``NativeToken`` public inputs —
never a raw ``ZeroAddress``:

- V4 -> non-V4 and non-V4 -> V4 paths in both orientations, with native-token
  start and end variants;
- V4/reserve-pair and V4/Curve native cases across DISTINCT sentinel database
  rows (the V4 ``address(0)`` currency row and the Curve all-Es coin row —
  never coalesced, ADR-097 D4);
- path connectivity, hop ordering, direction resolution, V4 pool identity,
  required wrap/unwrap bridges, and public ``NativeToken`` results.

The V4 zero sentinel appears ONLY in the seeding rows (protocol context: the
V4 wire spelling of its native currency) and in V4 execution internals
(``v4_currency``). Every public input names the asset with ``NativeToken``.

"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.curve.types import NATIVE_COIN_ALL_ES
from degenbot.db import db_resolve_native_token_ids
from degenbot.erc20 import NativeToken
from degenbot.exceptions.arbitrage import DirectionResolutionError
from degenbot.exceptions.base import DegenbotValueError
from degenbot.pathfinding import (
    POOL_KIND_BY_LABEL,
    PathfindingRequest,
    PathStep,
    PoolKind,
    find_paths,
    resolve_directions,
)
from degenbot.types.chain import ChainId
from degenbot.uniswap.v4_liquidity_pool import v4_currency
from tests.helpers.database import seed_v2_topology, seed_v4_topology
from tests.helpers.erc20_factory import make_native_token

if TYPE_CHECKING:
    import pathlib

    from tests.helpers.seam import Bot


CHAIN_ID = ChainId.ETH

WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
USDC_ADDRESS = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
POOL_MANAGER = get_checksum_address("0x" + "99" * 20)

# The V4 protocol's wire spelling of its native currency — SEEDING ONLY
# (protocol context, ADR-097 D3).
V4_NATIVE_SENTINEL = ZERO_ADDRESS

POOL_V4_NATIVE_USDC_A_HASH = "0x" + "0" * 63 + "1"
POOL_V4_NATIVE_USDC_B_HASH = "0x" + "0" * 63 + "2"
POOL_V2_USDC_WETH_A = get_checksum_address("0x" + "21" * 20)
POOL_V2_USDC_WETH_B = get_checksum_address("0x" + "22" * 20)
POOL_V2_USDC_CURVE_A = get_checksum_address("0x" + "23" * 20)
POOL_V2_USDC_CURVE_B = get_checksum_address("0x" + "24" * 20)
USDC2_ADDRESS = get_checksum_address("0x" + "bb" * 20)


@pytest.fixture
def native(py_bot) -> NativeToken:
    """The chain's conceptual native token (ADR-097)."""
    return make_native_token(py_bot)


def _seed_reserve_pair_db(db_path: pathlib.Path) -> pathlib.Path:
    """The V4/reserve-pair topology: two native/USDC V4 pools beside two
    reserve-pair legs (USDC/WETH and USDC2/WETH).

    Every token sits in two pools so the degree-2 candidate rule keeps the
    whole subgraph; the distinct intermediates keep each walk's pool pair
    unambiguous.
    """

    seed_v2_topology(
        db_path,
        [
            (POOL_V2_USDC_WETH_A, USDC_ADDRESS, WETH_ADDRESS),
            (POOL_V2_USDC_WETH_B, USDC2_ADDRESS, WETH_ADDRESS),
        ],
        chain_id=CHAIN_ID,
    )
    seed_v4_topology(
        db_path,
        [
            (POOL_V4_NATIVE_USDC_A_HASH, V4_NATIVE_SENTINEL, USDC_ADDRESS, ZERO_ADDRESS, 3000),
            (POOL_V4_NATIVE_USDC_B_HASH, V4_NATIVE_SENTINEL, USDC2_ADDRESS, ZERO_ADDRESS, 3000),
        ],
        pool_manager_address=POOL_MANAGER,
        chain_id=CHAIN_ID,
    )
    return db_path


def _seed_curve_db(db_path: pathlib.Path) -> pathlib.Path:
    """The V4/Curve topology: two native/USDC V4 pools beside two pairs
    carrying the Curve-context native leg's ROW — the all-Es sentinel row,
    DISTINCT from the V4 row. The graph's pair-edge vocabulary is V2/V3/V4;
    these edges stand in for the pool family that records the Curve coin, so
    every walk crosses the two distinct sentinel rows under the one
    conceptual native token.
    """

    seed_v2_topology(
        db_path,
        [
            (POOL_V2_USDC_CURVE_A, USDC_ADDRESS, NATIVE_COIN_ALL_ES),
            (POOL_V2_USDC_CURVE_B, USDC2_ADDRESS, NATIVE_COIN_ALL_ES),
        ],
        chain_id=CHAIN_ID,
    )
    seed_v4_topology(
        db_path,
        [
            (POOL_V4_NATIVE_USDC_A_HASH, V4_NATIVE_SENTINEL, USDC_ADDRESS, ZERO_ADDRESS, 3000),
            (POOL_V4_NATIVE_USDC_B_HASH, V4_NATIVE_SENTINEL, USDC2_ADDRESS, ZERO_ADDRESS, 3000),
        ],
        pool_manager_address=POOL_MANAGER,
        chain_id=CHAIN_ID,
    )
    return db_path


@pytest.fixture
def reserve_db(tmp_path: pathlib.Path) -> pathlib.Path:
    """The hermetic V4/reserve-pair topology (see ``_seed_reserve_pair_db``)."""
    return _seed_reserve_pair_db(tmp_path / "native_reserve.db")


@pytest.fixture
def curve_db(tmp_path: pathlib.Path) -> pathlib.Path:
    """The hermetic V4/Curve topology (see ``_seed_curve_db``)."""
    return _seed_curve_db(tmp_path / "native_curve.db")


def _steps(request: PathfindingRequest) -> set[tuple[str, ...]]:
    return {
        tuple((step.hash or step.address) for step in path) for path in find_paths(request=request)
    }


_FAMILY_LABEL = {int(kind): label for label, kind in POOL_KIND_BY_LABEL.items()}


def _families(path: tuple[PathStep, ...]) -> tuple[str, ...]:
    return tuple(_FAMILY_LABEL[int(step.type)] for step in path)


# ──────────────────────────────────────────────────────────────────
# The discovery matrix
# ──────────────────────────────────────────────────────────────────


def test_v4_led_mixed_paths_native_start_variant(
    reserve_db: pathlib.Path, native: NativeToken
) -> None:
    """V4 -> non-V4: the walk starts on the native token and lands on WETH."""

    paths = list(
        find_paths(
            request=PathfindingRequest(
                database_path=reserve_db,
                chain_id=CHAIN_ID,
                start_tokens=[native],
                end_tokens=[WETH_ADDRESS],
                pool_types=[PoolKind.V2, PoolKind.V4],
                max_depth=2,
            )
        )
    )

    assert {tuple((s.hash or s.address) for s in p) for p in paths} == {
        (POOL_V4_NATIVE_USDC_A_HASH, POOL_V2_USDC_WETH_A),
        (POOL_V4_NATIVE_USDC_B_HASH, POOL_V2_USDC_WETH_B),
    }
    for path in paths:
        assert len(path) == 2
        assert _families(path) == ("V4", "V2"), "hop ordering: the V4 native pool leads"
        assert path[1].address in {POOL_V2_USDC_WETH_A, POOL_V2_USDC_WETH_B}


def test_reserve_pair_led_mixed_paths_native_end_variant(
    reserve_db: pathlib.Path, native: NativeToken
) -> None:
    """non-V4 -> V4: the walk starts on WETH and ends on the native token."""

    paths = list(
        find_paths(
            request=PathfindingRequest(
                database_path=reserve_db,
                chain_id=CHAIN_ID,
                start_tokens=[WETH_ADDRESS],
                end_tokens=[native],
                pool_types=[PoolKind.V2, PoolKind.V4],
                max_depth=2,
            )
        )
    )

    assert {tuple((s.hash or s.address) for s in p) for p in paths} == {
        (POOL_V2_USDC_WETH_A, POOL_V4_NATIVE_USDC_A_HASH),
        (POOL_V2_USDC_WETH_B, POOL_V4_NATIVE_USDC_B_HASH),
    }
    for path in paths:
        assert _families(path) == ("V2", "V4"), "hop ordering: the reserve pair leads"
        assert path[0].address in {POOL_V2_USDC_WETH_A, POOL_V2_USDC_WETH_B}


def test_v4_curve_native_case_across_distinct_sentinel_rows(
    curve_db: pathlib.Path, native: NativeToken, py_bot: Bot
) -> None:
    """The V4/Curve native case: a walk from one sentinel row to the other.

    The two rows are DISTINCT database evidence (never coalesced), yet the
    boundary is the one conceptual ``NativeToken`` — and the session holds
    exactly one token object for it.
    """

    sentinel_ids = db_resolve_native_token_ids(str(curve_db), CHAIN_ID)
    assert len(sentinel_ids) == 2, "both protocol sentinel rows are recorded"
    assert len(set(sentinel_ids)) == 2, "the ids are distinct — no coalescing"

    # Session identity resolves both rows to the ONE conceptual token.
    assert make_native_token(py_bot) == native

    paths = list(
        find_paths(
            request=PathfindingRequest(
                database_path=curve_db,
                chain_id=CHAIN_ID,
                start_tokens=[native],
                end_tokens=[native],
                pool_types=[PoolKind.V2, PoolKind.V4],
                max_depth=2,
            )
        )
    )

    walked = {tuple((s.hash or s.address) for s in p) for p in paths}
    # native -> USDC -> native, in both orientations across the rows.
    assert walked == {
        (POOL_V4_NATIVE_USDC_A_HASH, POOL_V2_USDC_CURVE_A),
        (POOL_V4_NATIVE_USDC_B_HASH, POOL_V2_USDC_CURVE_B),
        (POOL_V2_USDC_CURVE_A, POOL_V4_NATIVE_USDC_A_HASH),
        (POOL_V2_USDC_CURVE_B, POOL_V4_NATIVE_USDC_B_HASH),
    }
    for path in paths:
        families = _families(path)
        assert set(families) == {"V4", "V2"}, "one V4 hop and the pair carrying the Curve leg"
        # V4 pool identity: the manager address + the seeded pool hash.
        v4_steps = [step for step in path if step.hash is not None]
        assert len(v4_steps) == 1
        assert v4_steps[0].address == POOL_MANAGER
        assert v4_steps[0].hash in {
            POOL_V4_NATIVE_USDC_A_HASH,
            POOL_V4_NATIVE_USDC_B_HASH,
        }


# ──────────────────────────────────────────────────────────────────
# Public inputs are exclusively NativeToken — raw sentinels refuse
# ──────────────────────────────────────────────────────────────────


@pytest.mark.parametrize(
    "sentinel",
    [ZERO_ADDRESS, NATIVE_COIN_ALL_ES],
    ids=["v4-zero-sentinel", "curve-all-es-sentinel"],
)
def test_raw_sentinel_boundary_is_refused_loudly(reserve_db: pathlib.Path, sentinel: str) -> None:
    """A raw protocol sentinel is one protocol's wire spelling (ADR-097 D1)."""

    with pytest.raises(DegenbotValueError, match="native-asset sentinel"):
        list(
            find_paths(
                request=PathfindingRequest(
                    database_path=reserve_db,
                    chain_id=CHAIN_ID,
                    start_tokens=[sentinel],
                    end_tokens=[WETH_ADDRESS],
                    pool_types=[PoolKind.V2, PoolKind.V4],
                    max_depth=2,
                )
            )
        )


def test_public_native_results_project_the_conceptual_token(native: NativeToken) -> None:
    """The public result surface names the asset, never an address (ADR-097)."""

    assert isinstance(native, NativeToken)
    assert "0x" not in repr(native), "the representation names the chain, never an address"
    assert native.chain_id == CHAIN_ID

    # Raw zero remains confined to V4 execution: the sanctioned V4
    # translation is the ONLY place the sentinel is spelled publicly.
    assert v4_currency(native) == ZERO_ADDRESS


# ──────────────────────────────────────────────────────────────────
# Direction resolution over mixed V4 paths (the Python adapter)
# ──────────────────────────────────────────────────────────────────


def test_direction_resolution_v4_led_matrix(native: NativeToken) -> None:
    """V4 -> non-V4 in both native-token profit variants.

    The value chain crosses the WETH/native representation gap at the seed
    side (WETH profit) or the capture side (native profit) — recorded as an
    explicit wrap/unwrap bridge, never aliased (ADR-097 D5).
    """

    hops = [
        (native, USDC_ADDRESS, "v4-native-usdc"),
        (USDC_ADDRESS, WETH_ADDRESS, "v2-usdc-weth"),
    ]

    zfos, bridges = resolve_directions(hops, WETH_ADDRESS, WETH_ADDRESS)
    assert zfos == [True, True], "hop ordering: the V4 hop leads"
    assert bridges == ["unwrap", "none", "none"], "the WETH seed unwraps to feed the V4 hop"

    zfos, bridges = resolve_directions(hops, native, WETH_ADDRESS)
    assert zfos == [True, True]
    assert bridges == ["none", "none", "unwrap"], "the native capture unwraps the WETH output"


def test_direction_resolution_non_v4_led_matrix(native: NativeToken) -> None:
    """non-V4 -> V4 in both native-token profit variants."""

    hops = [
        (WETH_ADDRESS, USDC_ADDRESS, "v2-weth-usdc"),
        (USDC_ADDRESS, native, "v4-usdc-native"),
    ]

    zfos, bridges = resolve_directions(hops, WETH_ADDRESS, WETH_ADDRESS)
    assert zfos == [True, True], "hop ordering: the reserve pair leads"
    assert bridges == ["none", "none", "wrap"], "the native capture wraps to the WETH profit"

    zfos, bridges = resolve_directions(hops, native, WETH_ADDRESS)
    assert zfos == [True, True]
    assert bridges == ["wrap", "none", "none"], "the native seed wraps to feed the WETH pair"


def test_direction_resolution_records_the_interior_representation_gap(
    native: NativeToken,
) -> None:
    """A mixed path whose interior boundary crosses the gap records it."""

    hops = [
        (USDC_ADDRESS, WETH_ADDRESS, "a"),
        (native, "0xdAC17F958D2ee523a2206206994597C13D831ec7", "b"),
        ("0xdAC17F958D2ee523a2206206994597C13D831ec7", USDC_ADDRESS, "c"),
    ]
    zfos, bridges = resolve_directions(hops, USDC_ADDRESS, WETH_ADDRESS)
    assert zfos == [True, True, True]
    assert bridges == ["none", "unwrap", "none", "none"]


def test_direction_resolution_native_weth_pair_hop_prefers_the_exact_side(
    native: NativeToken,
) -> None:
    """The native/WETH pair hop carries both representations; exact wins."""

    hops = [(native, WETH_ADDRESS, "v4-native-weth")]

    zfos, bridges = resolve_directions(hops, WETH_ADDRESS, WETH_ADDRESS)
    assert zfos == [False], "the WETH profit enters the WETH side without conversion"
    assert bridges == ["none", "wrap"]

    zfos, bridges = resolve_directions(hops, native, WETH_ADDRESS)
    assert zfos == [True], "the native profit enters the native side exactly"
    assert bridges == ["none", "unwrap"]


def test_direction_resolution_refuses_raw_sentinels(native: NativeToken) -> None:
    """Raw zero never crosses the resolver as a token spelling (ADR-097 D1)."""

    with pytest.raises(ValueError, match="native-asset sentinel"):
        resolve_directions([(ZERO_ADDRESS, USDC_ADDRESS, "v4")], WETH_ADDRESS, WETH_ADDRESS)
    with pytest.raises(ValueError, match="native-asset sentinel"):
        resolve_directions([(native, USDC_ADDRESS, "v4")], NATIVE_COIN_ALL_ES, WETH_ADDRESS)


def test_direction_resolution_still_refuses_a_non_closing_cycle(native: NativeToken) -> None:
    hops = [
        (WETH_ADDRESS, USDC_ADDRESS, "a"),
        (USDC_ADDRESS, "0xdAC17F958D2ee523a2206206994597C13D831ec7", "b"),
    ]
    with pytest.raises(DirectionResolutionError):
        resolve_directions(hops, WETH_ADDRESS, WETH_ADDRESS)
