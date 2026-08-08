import pytest

from degenbot.chainlink import ChainlinkPriceContract
from degenbot.checksum_cache import get_checksum_address
from degenbot.fork import AnvilFork
from tests.helpers.bot_factory import make_bot_with_provider
from tests.standalone_anvil import seed as seed_catalog


def test_chainlink_feed(standalone_anvil: AnvilFork):
    """A Chainlink feed loads from a mock aggregator seeded on a standalone anvil (no upstream)."""
    bot = make_bot_with_provider(standalone_anvil.provider)
    weth_price_feed = ChainlinkPriceContract(
        get_checksum_address(seed_catalog.CHAINLINK),
        bot=bot,
    )
    assert isinstance(weth_price_feed.price, float)
    assert weth_price_feed.price == pytest.approx(3720.38)
