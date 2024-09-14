import json
from typing import Any

import pytest
import web3
from eth_utils.address import to_checksum_address

from degenbot import set_web3
from degenbot.fork.anvil_fork import AnvilFork
from degenbot.solidly.solidly_functions import generate_aerodrome_v2_pool_address
from degenbot.solidly.solidly_liquidity_pool import AerodromeV2LiquidityPool

TBTC_USDBC_POOL_ADDRESS = to_checksum_address("0x723AEf6543aecE026a15662Be4D3fb3424D502A9")
AERODROME_V2_FACTORY_ADDRESS = to_checksum_address("0x420DD381b31aEf6683db6B902084cB0FFECe40Da")
AERODROME_IMPLEMENTATION_ADDRESS = to_checksum_address("0xA4e46b4f701c62e14DF11B48dCe76A7d793CD6d7")


@pytest.fixture
def test_pools() -> list:
    with open("tests/solidly/first_200_aerodrome_v2_pools.json") as file:
        return json.load(file)


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
            implementation_address=AERODROME_IMPLEMENTATION_ADDRESS,
            stable=False,
        )
        == TBTC_USDBC_POOL_ADDRESS
    )


def test_create_pool(
    base_full_node_web3: web3.Web3,
):
    set_web3(base_full_node_web3)

    lp = AerodromeV2LiquidityPool(
        address=TBTC_USDBC_POOL_ADDRESS,
    )
    assert lp.address == TBTC_USDBC_POOL_ADDRESS
    assert lp.factory == AERODROME_V2_FACTORY_ADDRESS
    assert lp.deployer_address == AERODROME_V2_FACTORY_ADDRESS


def test_calculation_volatile(fork_base: AnvilFork, test_pools: list[Any]):
    TEST_BLOCK = None
    if TEST_BLOCK:
        fork_base.reset(block_number=TEST_BLOCK)
    set_web3(fork_base.w3)

    TOKEN_AMOUNT_MULTIPLIERS = [
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
        lp = AerodromeV2LiquidityPool(address=pool_address)

        max_reserves_token0 = lp.reserves_token0
        max_reserves_token1 = lp.reserves_token1

        if max_reserves_token1 >= 2:
            for token_mult in TOKEN_AMOUNT_MULTIPLIERS:
                token_in_amount = int(token_mult * max_reserves_token0)
                if token_in_amount == 0:
                    continue
                print(f"{token_in_amount=} with {token_mult=}")
                try:
                    helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                        token_in=lp.token0,
                        token_in_quantity=token_in_amount,
                    )
                    contract_amount_out = lp.w3_contract.functions.getAmountOut(
                        token_in_amount,
                        lp.token0.address,
                    ).call()
                except Exception as e:
                    print(f"Failure {e} on pool {pool_address}")
                    raise
                else:
                    assert contract_amount_out == helper_amount_out, f"{pool_address=}"

        if max_reserves_token0 >= 2:
            for token_mult in TOKEN_AMOUNT_MULTIPLIERS:
                token_in_amount = int(token_mult * max_reserves_token1)
                if token_in_amount == 0 or max_reserves_token1 <= 1:
                    continue
                print(f"{token_in_amount=} with {token_mult=}")
                try:
                    helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                        token_in=lp.token1,
                        token_in_quantity=token_in_amount,
                    )
                    contract_amount_out = lp.w3_contract.functions.getAmountOut(
                        token_in_amount,
                        lp.token1.address,
                    ).call()
                except Exception as e:
                    print(f"Failure {e} on pool {pool_address}")
                    raise
                else:
                    assert contract_amount_out == helper_amount_out, f"{pool_address=}"


def test_calculation_stable(fork_base: AnvilFork, test_pools: list[Any]):
    TEST_BLOCK = None
    if TEST_BLOCK:
        fork_base.reset(block_number=TEST_BLOCK)
    set_web3(fork_base.w3)

    TOKEN_AMOUNT_MULTIPLIERS = [
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
        lp = AerodromeV2LiquidityPool(address=pool_address)

        max_reserves_token0 = lp.reserves_token0
        max_reserves_token1 = lp.reserves_token1

        if max_reserves_token1 >= 2:
            for token_mult in TOKEN_AMOUNT_MULTIPLIERS:
                token_in_amount = int(token_mult * max_reserves_token0)
                if token_in_amount == 0:
                    continue
                print(f"{token_in_amount=} with {token_mult=}")
                try:
                    helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                        token_in=lp.token0,
                        token_in_quantity=token_in_amount,
                    )
                    contract_amount_out = lp.w3_contract.functions.getAmountOut(
                        token_in_amount,
                        lp.token0.address,
                    ).call()
                except Exception as e:
                    print(f"Failure {e} on pool {pool_address}")
                else:
                    assert contract_amount_out == helper_amount_out, f"{pool_address=}"

        if max_reserves_token0 >= 2:
            for token_mult in TOKEN_AMOUNT_MULTIPLIERS:
                token_in_amount = int(token_mult * max_reserves_token1)
                if token_in_amount == 0 or max_reserves_token1 <= 1:
                    continue
                print(f"{token_in_amount=} with {token_mult=}")
                try:
                    helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                        token_in=lp.token1,
                        token_in_quantity=token_in_amount,
                    )
                    contract_amount_out = lp.w3_contract.functions.getAmountOut(
                        token_in_amount,
                        lp.token1.address,
                    ).call()
                except Exception as e:
                    print(f"Failure {e} on pool {pool_address}")
                else:
                    assert contract_amount_out == helper_amount_out, f"{pool_address=}"
