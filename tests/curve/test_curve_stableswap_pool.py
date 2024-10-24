import itertools
from typing import TYPE_CHECKING, cast

import eth_abi.abi
import pytest
from eth_utils.address import to_checksum_address
from web3 import Web3

if TYPE_CHECKING:
    from web3.contract.contract import Contract
from web3.types import Timestamp

from degenbot import AnvilFork
from degenbot.config import connection_manager, set_web3
from degenbot.curve.abi import CURVE_V1_FACTORY_ABI, CURVE_V1_POOL_ABI, CURVE_V1_REGISTRY_ABI
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.exceptions import BrokenPool, InvalidSwapInputAmount, NoLiquidity

FRXETH_WETH_CURVE_POOL_ADDRESS = to_checksum_address("0x9c3B46C0Ceb5B9e304FCd6D88Fc50f7DD24B31Bc")
CURVE_V1_FACTORY_ADDRESS = to_checksum_address("0x127db66E7F0b16470Bec194d0f496F9Fa065d0A9")
CURVE_V1_REGISTRY_ADDRESS = to_checksum_address("0x90E00ACe148ca3b23Ac1bC8C240C2a7Dd9c2d7f5")
TRIPOOL_ADDRESS = to_checksum_address("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")


@pytest.fixture
def tripool(ethereum_archive_node_web3: Web3) -> CurveStableswapPool:
    set_web3(ethereum_archive_node_web3)
    return CurveStableswapPool(TRIPOOL_ADDRESS)


def _test_calculations(lp: CurveStableswapPool):
    state_block = lp._update_block
    w3_contract = connection_manager.get_web3(lp.chain_id).eth.contract(
        address=lp.address,
        abi=CURVE_V1_POOL_ABI,
    )

    for token_in_index, token_out_index in itertools.permutations(range(len(lp.tokens)), 2):
        token_in = lp.tokens[token_in_index]
        token_out = lp.tokens[token_out_index]

        for amount_multiplier in [0.01, 0.05, 0.25]:
            amount = int(amount_multiplier * lp.balances[lp.tokens.index(token_in)])

            try:
                calc_amount = lp.calculate_tokens_out_from_tokens_in(
                    token_in=token_in,
                    token_out=token_out,
                    token_in_quantity=amount,
                    block_identifier=state_block,
                )
            except (InvalidSwapInputAmount, NoLiquidity):
                continue
            except Exception:
                print(f"Failure simulating swap (in-pool) at block {state_block} for {lp.address}:")
                raise

            if lp.address == "0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5":
                tx = {
                    "to": lp.address,
                    "data": Web3.keccak(text="get_dy(uint256,uint256,uint256)")[:4]
                    + eth_abi.abi.encode(
                        types=["uint256", "uint256", "uint256"],
                        args=[token_in_index, token_out_index, amount],
                    ),
                }
                contract_amount, *_ = eth_abi.abi.decode(
                    data=connection_manager.get_web3(lp.chain_id).eth.call(
                        transaction=tx,  # type: ignore[arg-type]
                        block_identifier=state_block,
                    ),
                    types=["uint256"],
                )
            else:
                contract_amount = w3_contract.functions.get_dy(
                    token_in_index,
                    token_out_index,
                    amount,
                ).call(block_identifier=state_block)

            assert (
                calc_amount == contract_amount
            ), f"Failure simulating swap (in-pool) at block {state_block} for {lp.address}: {amount} {token_in} for {token_out}"  # noqa:E501

    if lp.is_metapool:
        assert lp.base_pool is not None
        for token_in, token_out in itertools.permutations(lp.tokens_underlying, 2):
            token_in_index = lp.tokens_underlying.index(token_in)
            token_out_index = lp.tokens_underlying.index(token_out)

            for amount_multiplier in [0.10, 0.25, 0.50]:
                if token_in in lp.tokens:
                    amount = int(amount_multiplier * lp.balances[lp.tokens.index(token_in)])
                else:
                    amount = int(
                        amount_multiplier
                        * lp.base_pool.balances[lp.base_pool.tokens.index(token_in)]
                    )

                try:
                    calc_amount = lp.calculate_tokens_out_from_tokens_in(
                        token_in=token_in,
                        token_out=token_out,
                        token_in_quantity=amount,
                        block_identifier=state_block,
                    )
                except (InvalidSwapInputAmount, NoLiquidity):
                    continue

                contract_amount = w3_contract.functions.get_dy_underlying(
                    token_in_index,
                    token_out_index,
                    amount,
                ).call(block_identifier=state_block)

                assert calc_amount == contract_amount, (
                    f"Failure simulating swap (metapool) at block {state_block} for {lp.address}: "
                    f"{amount} {token_in} for {token_out}"
                )


def test_create_pool(ethereum_archive_node_web3: Web3):
    set_web3(ethereum_archive_node_web3)
    CurveStableswapPool(address=TRIPOOL_ADDRESS)


def test_tripool(tripool: CurveStableswapPool):
    _test_calculations(tripool)


def test_auto_update(fork_mainnet: AnvilFork):
    # Build the pool at a known historical block
    block_number = 18849427 - 1
    fork_mainnet.reset(block_number=block_number)
    set_web3(fork_mainnet.w3)

    _tripool = CurveStableswapPool(TRIPOOL_ADDRESS)

    assert fork_mainnet.w3.eth.get_block_number() == block_number
    assert _tripool._update_block == block_number

    expected_balances = [75010632422398781503259123, 76382820384826, 34653521595900]
    assert _tripool.balances == expected_balances

    fork_mainnet.reset(block_number=block_number + 1)
    assert fork_mainnet.w3.eth.get_block_number() == block_number + 1
    _tripool.auto_update()
    assert _tripool._update_block == block_number + 1
    assert _tripool.balances == [
        75010632422398781503259123,
        76437030384826,
        34599346168546,
    ]


def test_a_ramping(fork_mainnet: AnvilFork):
    # A range:      5000 -> 2000
    # A time :      1653559305 -> 1654158027
    initial_a = 5000
    final_a = 2000

    initial_a_time = 1653559305
    final_a_time = 1654158027

    fork_mainnet.reset(block_number=14_900_000)
    set_web3(fork_mainnet.w3)

    tripool = CurveStableswapPool(address=TRIPOOL_ADDRESS)
    tripool._create_timestamp = cast(Timestamp, 0)  # defeat the timestamp optimization

    assert tripool._a(timestamp=initial_a_time) == initial_a
    assert tripool._a(timestamp=final_a_time) == final_a
    assert tripool._a(timestamp=(initial_a_time + final_a_time) // 2) == (initial_a + final_a) // 2


def test_single_pool(
    fork_mainnet: AnvilFork,
):
    single_pool_address = "0x663aC72a1c3E1C4186CD3dCb184f216291F4878C"

    if not single_pool_address:
        return

    _block_identifier = 21030935

    set_web3(fork_mainnet.w3)
    if _block_identifier:
        fork_mainnet.reset(block_number=_block_identifier)

    lp = CurveStableswapPool(address=single_pool_address)
    _test_calculations(lp)


def test_tricrypto_pool(fork_mainnet: AnvilFork):
    """
    Tricrypto (WETH-wBTC-USDT) has a lot of one-off functions, so always test it
    """
    pool_address = "0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5"
    set_web3(fork_mainnet.w3)
    lp = CurveStableswapPool(address=pool_address)
    _test_calculations(lp)


def test_metapool_over_multiple_blocks_to_verify_cache_behavior(fork_mainnet: AnvilFork):
    pool_address = "0x618788357D0EBd8A37e763ADab3bc575D54c2C7d"
    start_block = 18_850_000
    end_block = 18_850_500

    # Pool has a 10 minute base rate cache expiry, so choose a 30 block interval (5 minutes)
    # to capture calcs at both cached and cache-expired states
    span = 30

    fork_mainnet.reset(block_number=start_block)
    fork_mainnet.w3.provider.timeout = 60  # type: ignore[attr-defined]
    set_web3(fork_mainnet.w3)

    lp = CurveStableswapPool(address=pool_address)

    for block in range(start_block + span, end_block, span):
        fork_mainnet.reset(block_number=block)
        lp.auto_update()
        _test_calculations(lp)


def test_base_pool(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)

    basepool = CurveStableswapPool(address=TRIPOOL_ADDRESS, silent=True)

    # Compare withdrawal calc for all tokens in the pool
    for token_index, token in enumerate(basepool.tokens):
        print(f"Testing {token} withdrawal")
        for amount_multiplier in [0.01, 0.10, 0.25]:
            token_in_amount = int(amount_multiplier * basepool.balances[token_index])
            print(f"Withdrawing {token_in_amount} {token}")
            calc_amount, *_ = basepool._calc_withdraw_one_coin(
                _token_amount=token_in_amount, i=token_index
            )

            amount_contract, *_ = eth_abi.abi.decode(
                types=["uint256"],
                data=fork_mainnet.w3.eth.call(
                    transaction={
                        "to": basepool.address,
                        "data": Web3.keccak(text="calc_withdraw_one_coin(uint256,int128)")[:4]
                        + eth_abi.abi.encode(
                            types=["uint256", "int128"],
                            args=[token_in_amount, token_index],
                        ),
                    }
                ),
            )
            assert calc_amount == amount_contract

    for token_index, token in enumerate(basepool.tokens):
        print(f"Testing {token} calc token amount")

        amount_array = [0] * len(basepool.tokens)

        for amount_multiplier in [0.01, 0.10, 0.25]:
            token_in_amount = int(amount_multiplier * basepool.balances[token_index])
            amount_array[token_index] = token_in_amount
            print(f"{token_in_amount=}")
            calc_token_amount = basepool._calc_token_amount(
                amounts=amount_array,
                deposit=True,
            )

            calc_token_amount_contract, *_ = eth_abi.abi.decode(
                types=["uint256"],
                data=fork_mainnet.w3.eth.call(
                    transaction={
                        "to": basepool.address,
                        "data": Web3.keccak(
                            text=f"calc_token_amount(uint256[{len(basepool.tokens)}],bool)"
                        )[:4]
                        + eth_abi.abi.encode(
                            types=[f"uint256[{len(basepool.tokens)}]", "bool"],
                            args=[amount_array, True],
                        ),
                    }
                ),
            )
            assert calc_token_amount == calc_token_amount_contract


def test_factory_stableswap_pools(fork_mainnet: AnvilFork):
    """
    Test the user-deployed pools deployed by the factory
    """

    set_web3(fork_mainnet.w3)
    stableswap_factory: Contract = fork_mainnet.w3.eth.contract(
        address=CURVE_V1_FACTORY_ADDRESS, abi=CURVE_V1_FACTORY_ABI
    )
    pool_count = stableswap_factory.functions.pool_count().call()

    with fork_mainnet.w3.batch_requests() as batch:
        batch.add_mapping(
            {
                stableswap_factory.functions.pool_list: list(range(pool_count)),
            }
        )
        pool_addresses = batch.execute()

    for i, pool_address in enumerate(pool_addresses):
        print(f"Testing factory pool {i}/{pool_count} @ {pool_address}")

        try:
            lp = CurveStableswapPool(address=cast(str, pool_address), silent=True)
            _test_calculations(lp)
        except (BrokenPool, NoLiquidity):
            continue
        except Exception as e:
            print(f"{type(e)}: {e} - pool {i}, {pool_address=}")
            raise


def test_base_registry_pools(fork_mainnet: AnvilFork):
    """
    Test the custom pools deployed by Curve
    """
    set_web3(fork_mainnet.w3)

    registry: Contract = fork_mainnet.w3.eth.contract(
        address=CURVE_V1_REGISTRY_ADDRESS,
        abi=CURVE_V1_REGISTRY_ABI,
    )
    pool_count = registry.functions.pool_count().call()

    with fork_mainnet.w3.batch_requests() as batch:
        batch.add_mapping(
            {
                registry.functions.pool_list: [pool for pool in range(pool_count)],
            }
        )
        pool_addresses = batch.execute()

    for i, pool_address in enumerate(pool_addresses):
        print(f"Testing registry pool {i}/{pool_count} @ {pool_address}")
        lp = CurveStableswapPool(address=cast(str, pool_address), silent=True)
        _test_calculations(lp)


def test_get_d(tripool: CurveStableswapPool):
    # Check that D=0 for an empty pool
    assert tripool._get_d(_xp=[0, 0, 0], _amp=1000) == 0
