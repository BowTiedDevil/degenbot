import pytest
import web3

from degenbot.token import Erc20Token


class MockErc20Token(Erc20Token):
    def __init__(self):
        pass


def test_erc20token_comparisons():
    token0 = MockErc20Token()
    token0.address = web3.Web3.toChecksumAddress(
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    )

    token1 = MockErc20Token()
    token1.address = web3.Web3.toChecksumAddress(
        "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
    )

    assert token0 == "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    assert token0 == "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".lower()
    assert token0 == web3.Web3.toChecksumAddress(
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    )

    assert token0 != token1

    assert token0 > token1
    assert token0 > "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
    assert token0 > "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599".lower()
    assert token0 > web3.Web3.toChecksumAddress(
        "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
    )

    assert token1 < token0
    assert token1 < "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    assert token1 < "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".lower()
    assert token1 < web3.Web3.toChecksumAddress(
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    )
