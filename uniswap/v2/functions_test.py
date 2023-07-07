import pytest

from degenbot.uniswap.v2.functions import generate_v2_pool_address


def test_v2_address_generator():
    # Should generate address for Uniswap V2 WETH/WBTC pool
    # factory ref: https://etherscan.io/address/0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f
    # WETH ref: https://etherscan.io/address/0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2
    # WBTC ref: https://etherscan.io/address/0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599
    # pool ref: https://etherscan.io/address/0xBb2b8038a1640196FbE3e38816F3e67Cba72D940
    wbtc_weth_address = generate_v2_pool_address(
        token_addresses=[
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
        ],
        factory_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
    )

    assert wbtc_weth_address == "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"

    # address generator returns a checksum address, so check against the lowered string
    with pytest.raises(AssertionError):
        assert (
            wbtc_weth_address
            == "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940".lower()
        )
