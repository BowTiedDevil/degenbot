from degenbot import AnvilFork, ChainlinkPriceContract, get_checksum_address, set_web3


def test_chainlink_feed(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)

    # Load WETH price feed
    # ref: https://data.chain.link/ethereum/mainnet/crypto-usd/eth-usd
    weth_price_feed = ChainlinkPriceContract(
        get_checksum_address("0x5f4ec3df9cbd43714fe2740f5e3616155c5b8419")
    )
    assert isinstance(weth_price_feed.price, float)
