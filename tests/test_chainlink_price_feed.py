from degenbot.anvil_fork import AnvilFork
from degenbot.bot import Bot
from degenbot.chainlink import ChainlinkPriceContract
from degenbot.checksum_cache import get_checksum_address


def test_chainlink_feed(fork_mainnet_full: AnvilFork, bot_mainnet_full: Bot):
    # Load WETH price feed
    # ref: https://data.chain.link/ethereum/mainnet/crypto-usd/eth-usd
    weth_price_feed = ChainlinkPriceContract(
        get_checksum_address("0x5f4ec3df9cbd43714fe2740f5e3616155c5b8419"),
        bot=bot_mainnet_full,
    )
    assert isinstance(weth_price_feed.price, float)
