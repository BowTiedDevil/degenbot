from eth_utils import to_checksum_address
from degenbot.erc20_token import Erc20Token


class MockErc20Token(Erc20Token):
    def __init__(self):
        pass


def test_erc20token_comparisons():
    token0 = MockErc20Token()
    token0.address = to_checksum_address(
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    )

    token1 = MockErc20Token()
    token1.address = to_checksum_address(
        "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
    )

    assert token0 == "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    assert token0 == "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".lower()
    assert token0 == to_checksum_address(
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    )

    assert token0 != token1

    assert token0 > token1
    assert token0 > "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
    assert token0 > "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599".lower()
    assert token0 > to_checksum_address(
        "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
    )

    assert token1 < token0
    assert token1 < "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    assert token1 < "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".lower()
    assert token1 < to_checksum_address(
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    )
