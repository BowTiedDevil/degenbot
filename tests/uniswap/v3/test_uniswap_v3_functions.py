import pytest
from degenbot.uniswap.v3_functions import decode_v3_path, generate_v3_pool_address
from hexbytes import HexBytes

WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
WBTC_WETH_LP_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"
WBTC_WETH_LP_FEE = 3000


def test_v3_address_generator() -> None:
    # Should generate address for Uniswap V3 WETH/WBTC pool
    # factory ref: https://etherscan.io/address/0x1F98431c8aD98523631AE4a59f267346ea31F984
    # WETH ref: https://etherscan.io/address/0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2
    # WBTC ref: https://etherscan.io/address/0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599
    # pool ref: https://etherscan.io/address/0xcbcdf9626bc03e24f779434178a73a0b4bad62ed
    wbtc_weth_address = generate_v3_pool_address(
        token_addresses=[WBTC_ADDRESS, WETH_ADDRESS],
        fee=WBTC_WETH_LP_FEE,
        factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    )
    assert wbtc_weth_address == WBTC_WETH_LP_ADDRESS

    # address generator returns a checksum address, so check against the lowered string
    with pytest.raises(AssertionError):
        assert wbtc_weth_address == WBTC_WETH_LP_ADDRESS.lower()


def test_v3_decode_path() -> None:
    path = (
        HexBytes(WBTC_ADDRESS)
        + HexBytes((WBTC_WETH_LP_FEE).to_bytes(3))  # pad to 3 bytes
        + HexBytes(WETH_ADDRESS)
    )
    assert decode_v3_path(path) == [WBTC_ADDRESS, WBTC_WETH_LP_FEE, WETH_ADDRESS]
