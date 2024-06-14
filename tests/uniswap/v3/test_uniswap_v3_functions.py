from fractions import Fraction

import pytest
from degenbot.uniswap.v3_functions import (
    decode_v3_path,
    exchange_rate_from_sqrt_price_x96,
    generate_v3_pool_address,
)
from hexbytes import HexBytes

MAINNET_WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
MAINNET_WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
MAINNET_UNISWAP_V3_WBTC_WETH_LP_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"
MAINNET_UNISWAP_V3_WBTC_WETH_LP_FEE = 3000

BASE_PANCAKESWAP_V3_WETH_CBETH_ADDRESS = "0x257FCbAE4Ac6B26A02E4FC5e1a11e4174B5ce395"
BASE_PANCAKESWAP_V3_FACTORY_ADDRESS = "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"
BASE_PANCAKESWAP_V3_DEPLOYER_ADDRESS = "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"

BASE_CBETH_WETH_V3_POOL_FEE = 100
BASE_CBETH_ADDRESS = "0x2ae3f1ec7f1f5012cfeab0185bfc7aa3cf0dec22"
BASE_WETH_ADDRESS = "0x4200000000000000000000000000000000000006"


def test_v3_address_generator() -> None:
    # Should generate address for Uniswap V3 WETH/WBTC pool
    # factory ref: https://etherscan.io/address/0x1F98431c8aD98523631AE4a59f267346ea31F984
    # WETH ref: https://etherscan.io/address/0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2
    # WBTC ref: https://etherscan.io/address/0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599
    # pool ref: https://etherscan.io/address/0xcbcdf9626bc03e24f779434178a73a0b4bad62ed
    wbtc_weth_address = generate_v3_pool_address(
        token_addresses=[MAINNET_WBTC_ADDRESS, MAINNET_WETH_ADDRESS],
        fee=MAINNET_UNISWAP_V3_WBTC_WETH_LP_FEE,
        factory_or_deployer_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    )
    assert wbtc_weth_address == MAINNET_UNISWAP_V3_WBTC_WETH_LP_ADDRESS

    # address generator returns a checksum address, so check against the lowered string
    with pytest.raises(AssertionError):
        assert wbtc_weth_address == MAINNET_UNISWAP_V3_WBTC_WETH_LP_ADDRESS.lower()

    assert (
        generate_v3_pool_address(
            token_addresses=[
                BASE_WETH_ADDRESS,
                BASE_CBETH_ADDRESS,
            ],
            fee=BASE_CBETH_WETH_V3_POOL_FEE,
            factory_or_deployer_address=BASE_PANCAKESWAP_V3_DEPLOYER_ADDRESS,
            init_hash="0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2",
        )
        == BASE_PANCAKESWAP_V3_WETH_CBETH_ADDRESS
    )


def test_v3_decode_path() -> None:
    path = (
        HexBytes(MAINNET_WBTC_ADDRESS)
        + HexBytes(
            (MAINNET_UNISWAP_V3_WBTC_WETH_LP_FEE).to_bytes(length=3, byteorder="big")
        )  # pad to 3 bytes
        + HexBytes(MAINNET_WETH_ADDRESS)
    )
    assert decode_v3_path(path) == [
        MAINNET_WBTC_ADDRESS,
        MAINNET_UNISWAP_V3_WBTC_WETH_LP_FEE,
        MAINNET_WETH_ADDRESS,
    ]

    for fee in (100, 500, 3000, 10000):
        path = (
            HexBytes(MAINNET_WBTC_ADDRESS)
            + HexBytes((fee).to_bytes(length=3, byteorder="big"))  # pad to 3 bytes
            + HexBytes(MAINNET_WETH_ADDRESS)
        )
        assert decode_v3_path(path) == [MAINNET_WBTC_ADDRESS, fee, MAINNET_WETH_ADDRESS]


def test_v3_exchange_rates_from_sqrt_price_x96() -> None:
    PRICE = 2018382873588440326581633304624437
    assert (
        exchange_rate_from_sqrt_price_x96(PRICE)
        == Fraction(2018382873588440326581633304624437, 2**96) ** 2
    )
