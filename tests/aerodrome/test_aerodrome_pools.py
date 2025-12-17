import pathlib
import pickle
from typing import Any

import pydantic_core
import pytest

from degenbot.aerodrome.abi import AERODROME_V2_POOL_ABI
from degenbot.aerodrome.functions import generate_aerodrome_v2_pool_address
from degenbot.aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from degenbot.aerodrome.types import (
    AerodromeV2PoolExternalUpdate,
    AerodromeV2PoolState,
    AerodromeV3PoolState,
)
from degenbot.anvil_fork import AnvilFork
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import set_web3
from degenbot.exceptions import DegenbotError
from degenbot.exceptions.liquidity_pool import ExternalUpdateError, LateUpdateError
from degenbot.uniswap.v3_libraries.tick_math import MAX_SQRT_RATIO, MIN_SQRT_RATIO

WETH_CONTRACT_ADDRESS = get_checksum_address("0x4200000000000000000000000000000000000006")
CBETH_CONTRACT_ADDRESS = get_checksum_address("0x2Ae3F1Ec7F1F5012CFEab0185bfc7aa3cf0DEc22")

AERODROME_V2_FACTORY_ADDRESS = get_checksum_address("0x420DD381b31aEf6683db6B902084cB0FFECe40Da")
AERODROME_V2_POOL_IMPLEMENTATION_ADDRESS = get_checksum_address(
    "0xA4e46b4f701c62e14DF11B48dCe76A7d793CD6d7"
)

AERODROME_V3_FACTORY_ADDRESS = get_checksum_address("0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A")
AERODROME_V3_QUOTER_ADDRESS = get_checksum_address("0x254cF9E1E6e233aa1AC962CB9B05b2cfeAaE15b0")
AERODROME_V3_TBTC_USDBC_POOL_ADDRESS = get_checksum_address(
    "0x723AEf6543aecE026a15662Be4D3fb3424D502A9"
)
AERODROME_V3_CBETH_WETH_POOL_ADDRESS = get_checksum_address(
    "0x47cA96Ea59C13F72745928887f84C9F52C3D7348"
)
AERODROME_V3_QUOTER_ABI = pydantic_core.from_json(
    """
    [{"inputs":[{"internalType":"address","name":"_factory","type":"address"},{"internalType":"address","name":"_WETH9","type":"address"}],"stateMutability":"nonpayable","type":"constructor"},{"inputs":[],"name":"WETH9","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"factory","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"bytes","name":"path","type":"bytes"},{"internalType":"uint256","name":"amountIn","type":"uint256"}],"name":"quoteExactInput","outputs":[{"internalType":"uint256","name":"amountOut","type":"uint256"},{"internalType":"uint160[]","name":"sqrtPriceX96AfterList","type":"uint160[]"},{"internalType":"uint32[]","name":"initializedTicksCrossedList","type":"uint32[]"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"internalType":"address","name":"tokenIn","type":"address"},{"internalType":"address","name":"tokenOut","type":"address"},{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"uint160","name":"sqrtPriceLimitX96","type":"uint160"}],"internalType":"struct IQuoterV2.QuoteExactInputSingleParams","name":"params","type":"tuple"}],"name":"quoteExactInputSingle","outputs":[{"internalType":"uint256","name":"amountOut","type":"uint256"},{"internalType":"uint160","name":"sqrtPriceX96After","type":"uint160"},{"internalType":"uint32","name":"initializedTicksCrossed","type":"uint32"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes","name":"path","type":"bytes"},{"internalType":"uint256","name":"amountOut","type":"uint256"}],"name":"quoteExactOutput","outputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"uint160[]","name":"sqrtPriceX96AfterList","type":"uint160[]"},{"internalType":"uint32[]","name":"initializedTicksCrossedList","type":"uint32[]"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"internalType":"address","name":"tokenIn","type":"address"},{"internalType":"address","name":"tokenOut","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"uint160","name":"sqrtPriceLimitX96","type":"uint160"}],"internalType":"struct IQuoterV2.QuoteExactOutputSingleParams","name":"params","type":"tuple"}],"name":"quoteExactOutputSingle","outputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"uint160","name":"sqrtPriceX96After","type":"uint160"},{"internalType":"uint32","name":"initializedTicksCrossed","type":"uint32"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"int256","name":"amount0Delta","type":"int256"},{"internalType":"int256","name":"amount1Delta","type":"int256"},{"internalType":"bytes","name":"path","type":"bytes"}],"name":"uniswapV3SwapCallback","outputs":[],"stateMutability":"view","type":"function"}]
    """  # noqa:E501
)


# This mark will be applied to ALL tests in this file.
pytestmark = pytest.mark.base


@pytest.fixture
def test_pools() -> Any:
    return pydantic_core.from_json(
        pathlib.Path("tests/aerodrome/first_200_aerodrome_v2_pools.json").read_bytes()
    )


def test_aerodrome_v2_address_generator():
    # Should generate address for Aerodrome V2 tBTC/USDBc pool
    # factory ref: https://basescan.org/address/0x420dd381b31aef6683db6b902084cb0ffece40da
    # pool ref: https://basescan.org/address/0x723AEf6543aecE026a15662Be4D3fb3424D502A9
    assert (
        generate_aerodrome_v2_pool_address(
            deployer_address=AERODROME_V2_FACTORY_ADDRESS,
            token_addresses=[
                "0x236aa50979D5f3De3Bd1Eeb40E81137F22ab794b",
                "0xd9aAEc86B65D86f6A7B5B1b0c42FFA531710b6CA",
            ],
            implementation_address=AERODROME_V2_POOL_IMPLEMENTATION_ADDRESS,
            stable=False,
        )
        == AERODROME_V3_TBTC_USDBC_POOL_ADDRESS
    )


def test_pickle_pool(
    fork_base_full: AnvilFork,
):
    set_web3(fork_base_full.w3)

    lp = AerodromeV2Pool(
        address=AERODROME_V3_TBTC_USDBC_POOL_ADDRESS,
    )
    pickle.dumps(lp)


def test_auto_update(
    fork_base_full: AnvilFork,
):
    set_web3(fork_base_full.w3)
    lp = AerodromeV2Pool(
        address=AERODROME_V3_TBTC_USDBC_POOL_ADDRESS,
    )
    lp.auto_update()

    # Hand-modify the state to force a positive update
    lp._state = AerodromeV2PoolState(
        address=lp.address,
        reserves_token0=lp.state.reserves_token0 - 1,
        reserves_token1=lp.state.reserves_token1 + 1,
        block=None,
    )
    lp.auto_update()

    with pytest.raises(LateUpdateError):
        lp.auto_update(block_number=lp.update_block - 10)


def test_external_update(
    fork_base_full: AnvilFork,
):
    set_web3(fork_base_full.w3)
    lp = AerodromeV2Pool(
        address=AERODROME_V3_TBTC_USDBC_POOL_ADDRESS,
    )

    current_state = lp.state

    lp.external_update(
        update=AerodromeV2PoolExternalUpdate(
            block_number=lp.update_block + 1,
            reserves_token0=int(1.1 * lp.reserves_token0),
            reserves_token1=int(0.9 * lp.reserves_token1),
        )
    )

    assert lp.state.reserves_token0 == int(current_state.reserves_token0 * 1.1)
    assert lp.state.reserves_token1 == int(current_state.reserves_token1 * 0.9)

    lp.external_update(
        update=AerodromeV2PoolExternalUpdate(
            block_number=lp.update_block + 1,
            reserves_token0=lp.reserves_token0,
            reserves_token1=lp.reserves_token1,
        )
    )

    with pytest.raises(ExternalUpdateError):
        lp.external_update(
            update=AerodromeV2PoolExternalUpdate(
                block_number=lp.update_block - 10,
                reserves_token0=lp.reserves_token0,
                reserves_token1=lp.reserves_token1,
            )
        )


def test_create_pool(fork_base_full: AnvilFork):
    set_web3(fork_base_full.w3)

    lp = AerodromeV2Pool(
        address=AERODROME_V3_TBTC_USDBC_POOL_ADDRESS,
    )
    assert lp.address == AERODROME_V3_TBTC_USDBC_POOL_ADDRESS
    assert lp.factory == AERODROME_V2_FACTORY_ADDRESS
    assert lp.deployer_address == AERODROME_V2_FACTORY_ADDRESS


def test_calculation_volatile(fork_base_full: AnvilFork, test_pools: list[Any]):
    set_web3(fork_base_full.w3)

    token_amount_multipliers = [
        0.000000001,
        0.00000001,
        0.0000001,
        0.000001,
        0.00001,
        0.0001,
        0.001,
        0.01,
        0.1,
        0.125,
        0.25,
        0.5,
        0.75,
    ]

    for pool_address in [pool["pool_address"] for pool in test_pools if pool["stable"] is False]:
        lp = AerodromeV2Pool(address=pool_address)

        max_reserves_token0 = lp.reserves_token0
        max_reserves_token1 = lp.reserves_token1

        w3_contract = fork_base_full.w3.eth.contract(
            address=pool_address, abi=AERODROME_V2_POOL_ABI
        )

        if max_reserves_token1 >= 2:
            for token_mult in token_amount_multipliers:
                token_in_amount = int(token_mult * max_reserves_token0)
                if token_in_amount == 0:
                    continue
                print(f"{token_in_amount=} with {token_mult=}")

                try:
                    helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                        token_in=lp.token0,
                        token_in_quantity=token_in_amount,
                    )
                    contract_amount_out = w3_contract.functions.getAmountOut(
                        token_in_amount,
                        lp.token0.address,
                    ).call()
                except Exception as e:
                    print(f"Failure {e} on pool {pool_address}")
                    raise
                else:
                    assert contract_amount_out == helper_amount_out, f"{pool_address=}"

        if max_reserves_token0 >= 2:
            for token_mult in token_amount_multipliers:
                token_in_amount = int(token_mult * max_reserves_token1)
                if token_in_amount == 0 or max_reserves_token1 <= 1:
                    continue
                print(f"{token_in_amount=} with {token_mult=}")
                try:
                    helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                        token_in=lp.token1,
                        token_in_quantity=token_in_amount,
                    )
                    contract_amount_out = w3_contract.functions.getAmountOut(
                        token_in_amount,
                        lp.token1.address,
                    ).call()
                except Exception as e:
                    print(f"Failure {e} on pool {pool_address}")
                    raise
                else:
                    assert contract_amount_out == helper_amount_out, f"{pool_address=}"


def test_calculation_stable(fork_base_full: AnvilFork, test_pools: list[Any]):
    set_web3(fork_base_full.w3)

    token_amount_multipliers = [
        0.000000001,
        0.00000001,
        0.0000001,
        0.000001,
        0.00001,
        0.0001,
        0.001,
        0.01,
        0.1,
        0.125,
        0.25,
        0.5,
        0.75,
    ]

    for pool_address in [pool["pool_address"] for pool in test_pools if pool["stable"] is True]:
        lp = AerodromeV2Pool(address=pool_address)

        max_reserves_token0 = lp.reserves_token0
        max_reserves_token1 = lp.reserves_token1

        w3_contract = fork_base_full.w3.eth.contract(
            address=pool_address, abi=AERODROME_V2_POOL_ABI
        )

        if max_reserves_token1 >= 2:
            for token_mult in token_amount_multipliers:
                token_in_amount = int(token_mult * max_reserves_token0)
                if token_in_amount == 0:
                    continue
                print(f"{token_in_amount=} with {token_mult=}")
                try:
                    helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                        token_in=lp.token0,
                        token_in_quantity=token_in_amount,
                    )
                    contract_amount_out = w3_contract.functions.getAmountOut(
                        token_in_amount,
                        lp.token0.address,
                    ).call()
                except DegenbotError as e:
                    print(f"Failure {e} on pool {pool_address}")
                else:
                    assert contract_amount_out == helper_amount_out, f"{pool_address=}"

        if max_reserves_token0 >= 2:
            for token_mult in token_amount_multipliers:
                token_in_amount = int(token_mult * max_reserves_token1)
                if token_in_amount == 0 or max_reserves_token1 <= 1:
                    continue
                print(f"{token_in_amount=} with {token_mult=}")
                try:
                    helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                        token_in=lp.token1,
                        token_in_quantity=token_in_amount,
                    )
                    contract_amount_out = w3_contract.functions.getAmountOut(
                        token_in_amount,
                        lp.token1.address,
                    ).call()
                except DegenbotError as e:
                    print(f"Failure {e} on pool {pool_address}")
                else:
                    assert contract_amount_out == helper_amount_out, f"{pool_address=}"


def test_aerodrome_v3_pool_creation(fork_base_full: AnvilFork) -> None:
    set_web3(fork_base_full.w3)
    AerodromeV3Pool(address=AERODROME_V3_CBETH_WETH_POOL_ADDRESS)


def test_aerodrome_v3_state(fork_base_full: AnvilFork) -> None:
    set_web3(fork_base_full.w3)

    lp = AerodromeV3Pool(address=AERODROME_V3_CBETH_WETH_POOL_ADDRESS)
    assert isinstance(lp.state, AerodromeV3PoolState), f"{type(lp.state)=}"


def test_aerodrome_v3_pool_calculation(fork_base_full: AnvilFork) -> None:
    set_web3(fork_base_full.w3)

    quoter = fork_base_full.w3.eth.contract(
        address=AERODROME_V3_QUOTER_ADDRESS, abi=AERODROME_V3_QUOTER_ABI
    )
    lp = AerodromeV3Pool(address="0x98c7A2338336d2d354663246F64676009c7bDa97")

    max_reserves_token0 = lp.token0.get_balance(lp.address)
    max_reserves_token1 = lp.token1.get_balance(lp.address)

    token_amount_multipliers = [
        0.000000001,
        0.00000001,
        0.0000001,
        0.000001,
        0.00001,
        0.0001,
        0.001,
        0.01,
        0.1,
        0.125,
        0.25,
        0.5,
        0.75,
    ]

    for token_mult in token_amount_multipliers:
        token_in_amount = int(token_mult * max_reserves_token0)
        if token_in_amount == 0:
            continue

        helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token0,
            token_in_quantity=token_in_amount,
        )
        quoter_amount_out, *_ = quoter.functions.quoteExactInputSingle(
            [
                lp.token0.address,  # tokenIn
                lp.token1.address,  # tokenOut
                token_in_amount,  # amountIn
                lp.tick_spacing,  # tickSpacing
                MIN_SQRT_RATIO + 1,  # sqrtPriceLimitX96
            ]
        ).call()

        assert helper_amount_out == quoter_amount_out

        token_in_amount = int(token_mult * max_reserves_token1)
        if token_in_amount == 0:
            continue

        helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token1,
            token_in_quantity=token_in_amount,
        )
        quoter_amount_out, *_ = quoter.functions.quoteExactInputSingle(
            [
                lp.token1.address,  # tokenIn
                lp.token0.address,  # tokenOut
                token_in_amount,  # amountIn
                lp.tick_spacing,  # tickSpacing
                MAX_SQRT_RATIO - 1,  # sqrtPriceLimitX96
            ]
        ).call()

        assert helper_amount_out == quoter_amount_out
