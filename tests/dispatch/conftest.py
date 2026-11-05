from __future__ import annotations

import pytest

from degenbot.arbitrage.engine_registry import ArbitrageEngine
from tests.helpers.v2_cycle_engine import build_two_hop_v2_engine


# The batch executor's path resolver is the engine's stage projection
# (`EngineStages::path_info_for`), so seam tests must hand the executor a real
# engine + a registered path_id. The engine's `register_and_solve_path`
# requires a >=2-hop cycle; `build_two_hop_v2_engine` builds that offline (Bot +
# make_erc20 + make_v2_pool, no RPC / no DB) — the same fixture the
# session-consumption oracle drives through the `PyO3` consumer seam.


@pytest.fixture
def nxm2bf_v2_engine_and_path() -> tuple[ArbitrageEngine, int]:
    """A `PyArbitrageEngine` + a registered 2-hop V2 path (0.3% fee cycle).

    `path_info_for(path_id)` projects a 2-hop `PathInfo` (both V2, `fee=30`,
    `zfo=(True, False)`).
    """
    return build_two_hop_v2_engine()
