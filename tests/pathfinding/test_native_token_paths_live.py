"""Live mixed-path ``NativeToken`` cases — the ``just test-fork`` lane.

Runs on the mainnet fork tier (``online_rpc``): REAL token and pool
construction against the node, then the public-``NativeToken`` pathfinding
and direction-resolution matrix over the mixed V4 topology the offline
module pins. The live leg proves the matrix holds with live-derived token
objects — public results project ``NativeToken``, while the raw zero
sentinel stays confined to V4 execution (``v4_currency``).

"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.pathfinding import PathfindingRequest, PoolKind, find_paths, resolve_directions
from degenbot.types.chain import ChainId
from degenbot.uniswap.v4_liquidity_pool import v4_currency
from tests.helpers.database import seed_v2_topology, seed_v4_topology

if TYPE_CHECKING:
    import pathlib

    from degenbot import Bot


CHAIN_ID = ChainId.ETH

USDC_WETH_V2 = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
USDC_ADDRESS = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
POOL_MANAGER = get_checksum_address("0x" + "99" * 20)

# The V4 wire spelling of its native currency — SEEDING ONLY (protocol
# context); public inputs name the asset with `NativeToken` (ADR-097).
V4_NATIVE_SENTINEL = ZERO_ADDRESS
POOL_V4_NATIVE_USDC_A_HASH = "0x" + "0" * 63 + "1"
POOL_V4_NATIVE_USDC_B_HASH = "0x" + "0" * 63 + "2"
USDC2_ADDRESS = get_checksum_address("0x" + "bb" * 20)
SYNTH_PAIR_USDC2_WETH = get_checksum_address("0x" + "22" * 20)


@pytest.mark.online_rpc
def test_live_mixed_v4_paths_with_native_token_boundaries(
    bot_mainnet_full: Bot,
    tmp_path: pathlib.Path,
) -> None:
    native = bot_mainnet_full.get_native_token()
    assert native == bot_mainnet_full.get_native_token(), "one conceptual token per chain"
    assert native.chain_id == 1
    assert native.symbol == "ETH"

    # REAL pool construction against the node: the reserve-pair leg of the
    # matrix is a live-built pool with live-read token metadata.
    pair = bot_mainnet_full.build_pool(USDC_WETH_V2, silent=True)
    pair_tokens = {pair.token0.address, pair.token1.address}
    assert pair_tokens == {USDC_ADDRESS, WETH_ADDRESS}

    db = tmp_path / "live_mixed.db"
    # The live-built pair's row beside a second leg pair, and two V4
    # native/USDC pools: every token sits in two pools so the degree-2
    # candidate rule keeps the whole subgraph.
    seed_v2_topology(
        db,
        [
            (USDC_WETH_V2, USDC_ADDRESS, WETH_ADDRESS),
            (SYNTH_PAIR_USDC2_WETH, USDC2_ADDRESS, WETH_ADDRESS),
        ],
        chain_id=CHAIN_ID,
    )
    seed_v4_topology(
        db,
        [
            (POOL_V4_NATIVE_USDC_A_HASH, V4_NATIVE_SENTINEL, USDC_ADDRESS, ZERO_ADDRESS, 3000),
            (POOL_V4_NATIVE_USDC_B_HASH, V4_NATIVE_SENTINEL, USDC2_ADDRESS, ZERO_ADDRESS, 3000),
        ],
        pool_manager_address=POOL_MANAGER,
        chain_id=CHAIN_ID,
    )

    def paths(start: list, end: list) -> set[tuple[str, ...]]:
        return {
            tuple((step.hash or step.address) for step in path)
            for path in find_paths(
                request=PathfindingRequest(
                    database_path=db,
                    chain_id=CHAIN_ID,
                    start_tokens=start,
                    end_tokens=end,
                    pool_types=[PoolKind.V2, PoolKind.V4],
                    max_depth=2,
                )
            )
        }

    # V4 -> non-V4 with a native-token start, and non-V4 -> V4 with a
    # native-token end: both orientations of the mixed path.
    assert paths([native], [WETH_ADDRESS]) == {
        (POOL_V4_NATIVE_USDC_A_HASH, USDC_WETH_V2),
        (POOL_V4_NATIVE_USDC_B_HASH, SYNTH_PAIR_USDC2_WETH),
    }
    assert paths([WETH_ADDRESS], [native]) == {
        (USDC_WETH_V2, POOL_V4_NATIVE_USDC_A_HASH),
        (SYNTH_PAIR_USDC2_WETH, POOL_V4_NATIVE_USDC_B_HASH),
    }

    # Direction resolution over the same mixed hops, driven with the
    # live-built token spellings and the conceptual native token.
    hops = [
        (native, USDC_ADDRESS, "v4-native-usdc"),
        (USDC_ADDRESS, WETH_ADDRESS, "v2-usdc-weth"),
    ]
    zfos, bridges = resolve_directions(hops, WETH_ADDRESS, WETH_ADDRESS)
    assert zfos == [True, True]
    assert bridges == ["unwrap", "none", "none"]

    zfos, bridges = resolve_directions(hops, native, WETH_ADDRESS)
    assert zfos == [True, True]
    assert bridges == ["none", "none", "unwrap"]

    # Raw zero remains confined to V4 execution.
    assert v4_currency(native) == ZERO_ADDRESS
