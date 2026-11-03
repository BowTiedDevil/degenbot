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

from typing import TYPE_CHECKING

from degenbot.runner.build_paths import ConstructionContext
from degenbot.runner.identity import (
    PANCAKESWAP_V3_MAINNET_FACTORY,
    SUSHISWAP_V3_MAINNET_FACTORY,
    UNISWAP_V3_MAINNET_FACTORY,
)

if TYPE_CHECKING:
    from pathlib import Path


class _ConstructionBot:
    """A fake construction bot: the two bot facts the context resolves."""

    def __init__(self, *, database_path: Path) -> None:
        self.chain_id = 1
        self.database_path = database_path


class TestConstructionContext:
    def test_for_bot_resolves_policy_values(self, tmp_path: Path) -> None:
        """The context shrinks to resolved policy values: the core route owns
        the construction order, and the resolved route policy enters."""

        ctx = ConstructionContext.for_bot(_ConstructionBot(database_path=tmp_path / "unused.db"))
        assert ctx.bot.chain_id == 1
        route = ctx.construction_route
        assert route.generic is True
        assert set(route.factories) == {
            UNISWAP_V3_MAINNET_FACTORY,
            SUSHISWAP_V3_MAINNET_FACTORY,
            PANCAKESWAP_V3_MAINNET_FACTORY,
        }
        assert ctx.session is None, "the session is parked here once the crawl boots"
