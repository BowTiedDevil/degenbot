"""Pool construction route — the construction context as resolved policy values.

The construction route (GLOSSARY.md, pool-construction card) owns ONE core entry
from a requested pool to a constructed, registered pool. The driver supplies
resolved policy VALUES only: the route rungs and the generic-builder arm. The
per-unit build path that used to run these steps in Python is the engine's now
(``degenbot_bot::arb_engine::crawl::unit``), and its refusal taxonomy is pinned
by the core's own tests.

The retired driver-side fallback chain (three V3 trackers + the generic builder,
bare except-and-continue between rungs) is the bug this context exists to make
impossible: the route lives core-side.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from degenbot.runner.build_paths import ConstructionContext
from degenbot.runner.identity import (
    PANCAKESWAP_V3_MAINNET_FACTORY,
    SUSHISWAP_V3_MAINNET_FACTORY,
    UNISWAP_V3_MAINNET_FACTORY,
    WETH_ADDRESS,
)

if TYPE_CHECKING:
    from pathlib import Path


class _RouteRecordingBot:
    """A fake construction bot: records the build call, raises on demand."""

    def __init__(self, *, database_path: Path, exc: BaseException | None = None) -> None:
        self.chain_id = 1
        self.database_path = database_path
        self.exc = exc
        self.build_calls: list[dict[str, Any]] = []

    def registration_fleet_hosted(self) -> bool:
        return True

    def build_erc20token(self, address: str) -> Any:
        return f"weth:{address}"

    def build_pool(self, address: str, *, silent: bool = False, **kwargs: Any) -> Any:
        self.build_calls.append({"address": address, **kwargs})
        if self.exc is not None:
            raise self.exc
        return f"pool:{address}"

    def build_managed_pool(self, pool_manager: str, request: Any) -> Any:
        if self.exc is not None:
            raise self.exc
        return f"pool:{getattr(request, 'pool_id', '?')}"


class TestConstructionContext:
    def test_for_bot_resolves_policy_values(self, tmp_path: Path) -> None:
        """The context shrinks to resolved policy values: the core route owns
        the construction order, and the resolved route policy enters."""

        ctx = ConstructionContext.for_bot(_RouteRecordingBot(database_path=tmp_path / "unused.db"))
        assert ctx.bot.chain_id == 1
        assert ctx.weth == f"weth:{WETH_ADDRESS}"
        route = ctx.construction_route
        assert route.generic is True
        assert set(route.factories) == {
            UNISWAP_V3_MAINNET_FACTORY,
            SUSHISWAP_V3_MAINNET_FACTORY,
            PANCAKESWAP_V3_MAINNET_FACTORY,
        }
        assert ctx.session is None, "the session is parked here once the crawl boots"
