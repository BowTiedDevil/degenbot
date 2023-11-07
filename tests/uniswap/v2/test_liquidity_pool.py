from fractions import Fraction
from typing import Dict

import degenbot
import pytest
import web3
from degenbot import Erc20Token
from degenbot.exceptions import NoPoolStateAvailable, ZeroSwapError
from degenbot.uniswap import (
    LiquidityPool,
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
)
from eth_utils import to_checksum_address

_w3 = web3.Web3(web3.HTTPProvider(("http://localhost:8545")))
degenbot.set_web3(_w3)


class MockErc20Token(Erc20Token):
    def __init__(self):
        pass


# Test is based on the WBTC-WETH Uniswap V2 pool on Ethereum mainnet,
# evaluated against the results from the Uniswap V2 Router 2 contract
# functions `getAmountsOut` and `getAmountsIn`
#
# Pool address: 0xBb2b8038a1640196FbE3e38816F3e67Cba72D940
# Router address: 0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D

UNISWAP_V2_WBTC_WETH_POOL_ADDRESS = to_checksum_address(
    "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
)
UNISWAPV2_FACTORY_ADDRESS = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
UNISWAPV2_FACTORY_POOL_INIT_HASH = (
    "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
)


@pytest.fixture
def wbtc_weth_liquiditypool() -> LiquidityPool:
    token0 = MockErc20Token()
    token0.address = to_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
    token0.decimals = 8
    token0.name = "Wrapped BTC"
    token0.symbol = "WBTC"

    token1 = MockErc20Token()
    token1.address = to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
    token1.decimals = 18
    token1.name = "Wrapped Ether"
    token1.symbol = "WETH"

    try:
        del degenbot.AllPools(chain_id=1)[UNISWAP_V2_WBTC_WETH_POOL_ADDRESS]
    except KeyError:
        pass

    lp = LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
        update_method="external",
        tokens=[token0, token1],
        name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY_ADDRESS,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        fee=Fraction(3, 1000),
        empty=True,
    )
    lp.update_reserves(
        external_token0_reserves=16231137593,
        external_token1_reserves=2571336301536722443178,
        update_block=1,
    )

    return lp


def test_create_pool(wbtc_weth_liquiditypool):
    _ = wbtc_weth_liquiditypool


def test_calculate_tokens_out_from_tokens_in(wbtc_weth_liquiditypool):
    lp: LiquidityPool = wbtc_weth_liquiditypool

    # Reserve values for this test are taken at block height 17,600,000

    assert (
        lp.calculate_tokens_out_from_tokens_in(
            lp.token0,
            8000000000,
        )
        == 847228560678214929944
    )
    assert (
        lp.calculate_tokens_out_from_tokens_in(
            lp.token1,
            1200000000000000000000,
        )
        == 5154005339
    )


def test_calculate_tokens_out_from_tokens_in_with_override(wbtc_weth_liquiditypool):
    lp: LiquidityPool = wbtc_weth_liquiditypool

    # Overridden reserve values for this test are taken at block height 17,650,000
    # token0 reserves: 16027096956
    # token1 reserves: 2602647332090181827846

    pool_state_override = UniswapV2PoolState(
        pool=lp,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    assert (
        lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token0,
            token_in_quantity=8000000000,
            override_state=pool_state_override,
        )
        == 864834865217768537471
    )

    with pytest.raises(
        ValueError,
        match="Must provide reserve override values for both tokens",
    ):
        lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token0,
            token_in_quantity=8000000000,
            override_reserves_token0=0,
            override_reserves_token1=10,
        )


def test_calculate_tokens_in_from_tokens_out(wbtc_weth_liquiditypool):
    lp: LiquidityPool = wbtc_weth_liquiditypool

    # Reserve values for this test are taken at block height 17,600,000

    assert (
        lp.calculate_tokens_in_from_tokens_out(
            8000000000,
            lp.token1,
        )
        == 2506650866141614297072
    )

    assert (
        lp.calculate_tokens_in_from_tokens_out(
            1200000000000000000000,
            lp.token0,
        )
        == 14245938804
    )


def test_calculate_tokens_in_from_tokens_out_with_override(wbtc_weth_liquiditypool):
    lp: LiquidityPool = wbtc_weth_liquiditypool

    # Overridden reserve values for this test are taken at block height 17,650,000
    # token0 reserves: 16027096956
    # token1 reserves: 2602647332090181827846

    pool_state_override = UniswapV2PoolState(
        pool=lp,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    assert (
        lp.calculate_tokens_in_from_tokens_out(
            token_in=lp.token0,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )
        == 13752842264
    )

    with pytest.raises(
        ValueError,
        match="Must provide reserve override values for both tokens",
    ):
        lp.calculate_tokens_in_from_tokens_out(
            token_in=lp.token0,
            token_out_quantity=1200000000000000000000,
            override_reserves_token0=0,
            override_reserves_token1=10,
        )


def test_comparisons(wbtc_weth_liquiditypool):
    lp: LiquidityPool = wbtc_weth_liquiditypool

    assert lp == "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
    assert lp == "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940".lower()

    del degenbot.AllPools(chain_id=1)[lp]

    other_lp = LiquidityPool(
        address="0xBb2b8038a1640196FbE3e38816F3e67Cba72D940",
        update_method="external",
        tokens=[lp.token0, lp.token1],
        name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY_ADDRESS,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        fee=Fraction(3, 1000),
        empty=True,
    )

    assert lp == other_lp

    with pytest.raises(NotImplementedError):
        assert lp == 420

    # sets depend on __hash__ dunder method
    set([lp, other_lp])


def test_reorg(wbtc_weth_liquiditypool) -> None:
    lp: LiquidityPool = wbtc_weth_liquiditypool

    _START_BLOCK = 2
    _END_BLOCK = 10

    # Provide some dummy updates, then simulate a reorg back to the starting state
    starting_state = lp.state
    starting_token0_reserves = starting_state.reserves_token0
    starting_token1_reserves = starting_state.reserves_token1

    block_states: Dict[int, UniswapV2PoolState] = {1: lp.state}

    for block_number in range(_START_BLOCK, _END_BLOCK + 1, 1):
        lp.update_reserves(
            external_token0_reserves=starting_token0_reserves + 10_000 * block_number,
            external_token1_reserves=starting_token1_reserves + 10_000 * block_number,
            print_ratios=False,
            print_reserves=False,
            update_block=block_number,
        )
        # lp.external_update(
        #     update=UniswapV2PoolExternalUpdate(
        #         block_number=block_number,
        #         liquidity=starting_liquidity + 10_000 * block_number,
        #     ),
        # )
        block_states[block_number] = lp.state

    last_block_state = lp.state

    # Cannot restore to a pool state before the first
    with pytest.raises(NoPoolStateAvailable):
        lp.restore_state_before_block(0)

    # Last state is at block 10, so this will succedd but have no effect on the current state
    lp.restore_state_before_block(11)
    assert lp.state == last_block_state

    # Unwind the updates and compare to the stored states at previous blocks
    for block_number in range(_END_BLOCK + 1, 1, -1):
        lp.restore_state_before_block(block_number)
        assert lp.state == block_states[block_number - 1]

    # Verify the pool has been returned to the starting state
    assert lp.state == starting_state

    # Unwind all states
    lp.restore_state_before_block(1)
    assert lp.state == UniswapV2PoolState(lp, 0, 0)


def test_simulations(wbtc_weth_liquiditypool):
    lp: LiquidityPool = wbtc_weth_liquiditypool

    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=8000000000,
        amount1_delta=-847228560678214929944,
        current_state=lp.state,
        future_state=UniswapV2PoolState(
            pool=lp,
            reserves_token0=lp.reserves_token0 + 8000000000,
            reserves_token1=lp.reserves_token1 - 847228560678214929944,
        ),
    )

    # token_in = lp.token0 should have same result as token_out = lp.token1
    assert (
        lp.simulate_swap(
            token_in=lp.token0,
            token_in_quantity=8000000000,
        )
        == sim_result
    )
    assert (
        lp.simulate_swap(
            token_out=lp.token1,
            token_in_quantity=8000000000,
        )
        == sim_result
    )

    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=-5154005339,
        amount1_delta=1200000000000000000000,
        current_state=lp.state,
        future_state=UniswapV2PoolState(
            pool=lp,
            reserves_token0=lp.reserves_token0 - 5154005339,
            reserves_token1=lp.reserves_token1 + 1200000000000000000000,
        ),
    )

    assert (
        lp.simulate_swap(
            token_in=lp.token1,
            token_in_quantity=1200000000000000000000,
        )
        == sim_result
    )

    assert (
        lp.simulate_swap(
            token_out=lp.token0,
            token_in_quantity=1200000000000000000000,
        )
        == sim_result
    )


def test_simulations_with_override(wbtc_weth_liquiditypool):
    lp: LiquidityPool = wbtc_weth_liquiditypool

    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=8000000000,
        amount1_delta=-864834865217768537471,
        current_state=lp.state,
        future_state=UniswapV2PoolState(
            pool=lp,
            reserves_token0=lp.reserves_token0 + 8000000000,
            reserves_token1=lp.reserves_token1 - 864834865217768537471,
        ),
    )

    pool_state_override = UniswapV2PoolState(
        pool=lp,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    assert (
        lp.simulate_swap(
            token_in=lp.token0,
            token_in_quantity=8000000000,
            override_state=pool_state_override,
        )
        == sim_result
    )

    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=13752842264,
        amount1_delta=-1200000000000000000000,
        current_state=lp.state,
        future_state=UniswapV2PoolState(
            pool=lp,
            reserves_token0=lp.reserves_token0 + 13752842264,
            reserves_token1=lp.reserves_token1 - 1200000000000000000000,
        ),
    )

    assert (
        lp.simulate_swap(
            token_out=lp.token1,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )
        == sim_result
    )


def test_swap_for_all(wbtc_weth_liquiditypool):
    lp: LiquidityPool = wbtc_weth_liquiditypool

    # The last token in a pool can never be swapped for
    assert (
        lp.calculate_tokens_out_from_tokens_in(
            lp.token1,
            2**256 - 1,
        )
        == lp.reserves_token0 - 1
    )
    assert (
        lp.calculate_tokens_out_from_tokens_in(
            lp.token0,
            2**256 - 1,
        )
        == lp.reserves_token1 - 1
    )


def test_zero_swaps(wbtc_weth_liquiditypool):
    lp: LiquidityPool = wbtc_weth_liquiditypool

    with pytest.raises(ZeroSwapError):
        assert (
            lp.calculate_tokens_out_from_tokens_in(
                lp.token0,
                0,
            )
            == 0
        )

    with pytest.raises(ZeroSwapError):
        assert (
            lp.calculate_tokens_out_from_tokens_in(
                lp.token1,
                0,
            )
            == 0
        )
