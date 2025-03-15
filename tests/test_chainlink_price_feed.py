from degenbot.cache import get_checksum_address
from degenbot.chainlink import ChainlinkPriceContract
from degenbot.config import set_web3


def test_chainlink_feed(ethereum_archive_node_web3):
    set_web3(ethereum_archive_node_web3)

    # Load WETH price feed
    # ref: https://data.chain.link/ethereum/mainnet/crypto-usd/eth-usd
    weth_price_feed = ChainlinkPriceContract(
        get_checksum_address("0x5f4ec3df9cbd43714fe2740f5e3616155c5b8419")
    )
    assert isinstance(weth_price_feed.price, float)
