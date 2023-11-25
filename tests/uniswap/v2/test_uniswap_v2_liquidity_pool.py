import pickle
from fractions import Fraction
from typing import Dict
import degenbot
import pytest
from degenbot import Erc20Token
from degenbot.exceptions import (
    ExternalUpdateError,
    LiquidityPoolError,
    NoPoolStateAvailable,
    ZeroSwapError,
)
from degenbot.fork import AnvilFork
from degenbot.uniswap import LiquidityPool, UniswapV2PoolSimulationResult, UniswapV2PoolState
from degenbot.uniswap.v2_liquidity_pool import CamelotLiquidityPool
from eth_utils import to_checksum_address
from web3 import Web3


class MockErc20Token(Erc20Token):
    def __init__(self):
        pass


# Tests are based on the WBTC-WETH Uniswap V2 pool on Ethereum mainnet,
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

WETH_CONTRACT_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
WBTC_CONTRACT_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"


@pytest.fixture(scope="function")
def wbtc_weth_liquiditypool(local_web3_ethereum_archive: Web3) -> LiquidityPool:
    degenbot.set_web3(local_web3_ethereum_archive)

    # token0 = MockErc20Token()
    # token0.address = to_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
    # token0.decimals = 8
    # token0.name = "Wrapped BTC"
    # token0.symbol = "WBTC"

    # token1 = MockErc20Token()
    # token1.address = to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
    # token1.decimals = 18
    # token1.name = "Wrapped Ether"
    # token1.symbol = "WETH"

    if UNISWAP_V2_WBTC_WETH_POOL_ADDRESS in degenbot.AllPools(chain_id=1).pools:
        del degenbot.AllPools(chain_id=1)[UNISWAP_V2_WBTC_WETH_POOL_ADDRESS]

    lp = LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
        update_method="external",
        # tokens=[token0, token1],
        # name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY_ADDRESS,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        # empty=True,
        state_block=17_600_000,
    )

    # # Reserve values for taken at block height 17,600,000
    # lp.update_reserves(
    #     external_token0_reserves=16231137593,
    #     external_token1_reserves=2571336301536722443178,
    #     update_block=1,
    # )

    return lp


def test_create_pool(local_web3_ethereum_full: Web3) -> None:
    degenbot.set_web3(local_web3_ethereum_full)

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

    LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
        tokens=[token0, token1],
        name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY_ADDRESS,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
    )

    # Omitting tokens
    LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
        # tokens=[token0, token1],
        name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY_ADDRESS,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
    )

    with pytest.raises(ValueError, match="Expected 2 tokens, found"):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
            tokens=[token0, token1, token0],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY_ADDRESS,
            factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        )

    with pytest.raises(ValueError, match="Expected 2 tokens, found"):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
            tokens=[token0],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY_ADDRESS,
            factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        )


def test_create_camelot_v2_stable_pool(load_env: dict) -> None:
    CAMELOT_MIM_USDC_LP_ADDRESS = "0x68A0859de50B4Dfc6EFEbE981cA906D38Cdb0D1F"
    FORK_BLOCK = 153_759_000
    fork = AnvilFork(
        f"https://rpc.ankr.com/arbitrum/{load_env['ANKR_API_KEY']}",
        fork_block=FORK_BLOCK,
    )
    assert fork.block == FORK_BLOCK
    assert fork.w3.eth.block_number == FORK_BLOCK
    degenbot.set_web3(fork.w3)

    lp = CamelotLiquidityPool(address=CAMELOT_MIM_USDC_LP_ADDRESS)
    assert lp.stable_swap is True

    token_in = lp.token0  # MIM token
    amount_in = 1000 * 10**token_in.decimals  # nominal value of $1000

    # Test that the swap output from the pool contract matches the off-chain calculation
    contract_amount = lp._w3_contract.functions.getAmountOut(
        amountIn=amount_in, tokenIn=token_in.address
    ).call()
    assert contract_amount == lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )
    current_reserves = lp.reserves_token0, lp.reserves_token1

    rewind_block_length = 500_000
    contract_amount_old = lp._w3_contract.functions.getAmountOut(
        amountIn=amount_in, tokenIn=token_in.address
    ).call(block_identifier=FORK_BLOCK - rewind_block_length)

    assert contract_amount != contract_amount_old

    old_reserves = lp._w3_contract.functions.getReserves().call(
        block_identifier=FORK_BLOCK - rewind_block_length
    )

    lp.reserves_token0 = old_reserves[0]
    lp.reserves_token1 = old_reserves[1]
    assert contract_amount_old == lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )

    # Override the state and verify the overridden amounts match the first test
    assert contract_amount == lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
        override_state=UniswapV2PoolState(
            pool=lp,
            reserves_token0=current_reserves[0],
            reserves_token1=current_reserves[1],
        ),
    )


def test_create_camelot_v2_pool(load_env: dict) -> None:
    CAMELOT_WETH_USDC_LP_ADDRESS = "0x84652bb2539513BAf36e225c930Fdd8eaa63CE27"
    fork = AnvilFork(f"https://rpc.ankr.com/arbitrum/{load_env['ANKR_API_KEY']}")
    degenbot.set_web3(fork.w3)
    lp = CamelotLiquidityPool(address=CAMELOT_WETH_USDC_LP_ADDRESS)
    assert lp.stable_swap is False

    token_in = lp.token1
    amount_in = 1000 * 10**token_in.decimals  # nominal value of $1000

    assert lp._w3_contract.functions.getAmountOut(
        amountIn=amount_in, tokenIn=token_in.address
    ).call() == lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )


def test_create_empty_pool(
    wbtc_weth_liquiditypool: LiquidityPool, local_web3_ethereum_full: Web3
) -> None:
    _pool: LiquidityPool = wbtc_weth_liquiditypool

    degenbot.set_web3(local_web3_ethereum_full)

    LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
        tokens=[_pool.token0, _pool.token1],
        name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY_ADDRESS,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        empty=True,
    )

    with pytest.raises(ValueError):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
            # tokens=[_pool.token0, _pool.token1],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY_ADDRESS,
            factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
            empty=True,
        )

    # Create pool without factory address
    with pytest.raises(ValueError):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
            tokens=[_pool.token0, _pool.token1],
            name="WBTC-WETH (V2, 0.30%)",
            # factory_address=UNISWAPV2_FACTORY_ADDRESS,
            factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
            empty=True,
        )

    # Create pool without init hash
    with pytest.raises(ValueError):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
            tokens=[_pool.token0, _pool.token1],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY_ADDRESS,
            # factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
            empty=True,
        )

    # Create pool with a malformed init hash
    bad_init_hash = UNISWAPV2_FACTORY_POOL_INIT_HASH.replace("a", "b")
    with pytest.raises(
        ValueError,
        match=f"Pool address {UNISWAP_V2_WBTC_WETH_POOL_ADDRESS} does not match deterministic address",
    ):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
            tokens=[_pool.token0, _pool.token1],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY_ADDRESS,
            factory_init_hash=bad_init_hash,
            empty=True,
        )

    # Create with non-standard fee
    LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
        tokens=[_pool.token0, _pool.token1],
        name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY_ADDRESS,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        fee=Fraction(2, 1000),
        empty=True,
    )

    # Create with float fee
    with pytest.raises(TypeError, match="LP fee was not correctly passed!"):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
            tokens=[_pool.token0, _pool.token1],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY_ADDRESS,
            factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
            fee=0.003,
            empty=True,
        )

    # Create with float fee in tuple format
    with pytest.raises(TypeError, match="LP fee was not correctly passed!"):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
            tokens=[_pool.token0, _pool.token1],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY_ADDRESS,
            factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
            fee=(0.003, 0.003),
            empty=True,
        )

    # Create split-fee pool of differing values
    lp = LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
        tokens=[_pool.token0, _pool.token1],
        factory_address=UNISWAPV2_FACTORY_ADDRESS,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        fee=(Fraction(3, 1000), Fraction(5, 1000)),
        empty=True,
    )
    assert lp.fee_token0 == Fraction(3, 1000)
    assert lp.fee_token1 == Fraction(5, 1000)

    # Create split-fee pool of equal values
    lp = LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL_ADDRESS,
        tokens=[_pool.token0, _pool.token1],
        factory_address=UNISWAPV2_FACTORY_ADDRESS,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        fee=(Fraction(6, 1000), Fraction(6, 1000)),
        empty=True,
    )
    assert lp.fee_token0 == Fraction(6, 1000)
    assert lp.fee_token1 == Fraction(6, 1000)


def test_dunder_methods(wbtc_weth_liquiditypool: LiquidityPool) -> None:
    wbtc_weth_liquiditypool.__str__()
    wbtc_weth_liquiditypool.__hash__()


def test_pickle_pool(wbtc_weth_liquiditypool: LiquidityPool) -> None:
    pickle.dumps(wbtc_weth_liquiditypool)
    state = wbtc_weth_liquiditypool.__getstate__()
    wbtc_weth_liquiditypool.__setstate__(state)


def test_calculate_tokens_out_from_ratio_out(load_env: dict) -> None:
    fork = AnvilFork(f"https://rpc.ankr.com/eth/{load_env['ANKR_API_KEY']}", fork_block=17_600_000)
    degenbot.set_web3(fork.w3)

    router_contract = fork.w3.eth.contract(
        address="0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
        abi=degenbot.uniswap.abi.UNISWAP_V2_ROUTER_ABI,
    )

    lp = LiquidityPool("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")

    for wbtc_amount_in in [
        int(0.1 * 10**8),
        1 * 10**8,
        10 * 10**8,
    ]:
        token_in = lp.token0  # WBTC
        token_out = lp.token1  # WETH

        weth_amount_out = router_contract.functions.getAmountsOut(
            wbtc_amount_in,
            [token_in.address, token_out.address],
        ).call()[-1]

        ratio = Fraction(weth_amount_out, wbtc_amount_in)

        calculated_input = lp.calculate_tokens_in_from_ratio_out(
            token_in=token_in,
            ratio_absolute=ratio,
        )
        assert pytest.approx(calculated_input / wbtc_amount_in, rel=1e-3) == 1

    for weth_amount_in in [1 * 10**18, 10 * 10**18, 100 * 10**18]:
        token_in = lp.token1  # WETH
        token_out = lp.token0  # WBTC

        wbtc_amount_out = router_contract.functions.getAmountsOut(
            weth_amount_in,
            [token_in.address, token_out.address],
        ).call()[-1]

        ratio = Fraction(wbtc_amount_out, weth_amount_in)

        calculated_input = lp.calculate_tokens_in_from_ratio_out(
            token_in=token_in,
            ratio_absolute=ratio,
        )
        assert pytest.approx(calculated_input / weth_amount_in, rel=1e-3) == 1


def test_calculate_tokens_out_from_tokens_in(wbtc_weth_liquiditypool: LiquidityPool) -> None:
    assert (
        wbtc_weth_liquiditypool.calculate_tokens_out_from_tokens_in(
            wbtc_weth_liquiditypool.token0,
            8000000000,
        )
        == 847228560678214929944
    )
    assert (
        wbtc_weth_liquiditypool.calculate_tokens_out_from_tokens_in(
            wbtc_weth_liquiditypool.token1,
            1200000000000000000000,
        )
        == 5154005339
    )

    dai_token = MockErc20Token()
    dai_token.address = to_checksum_address("0x6B175474E89094C44Da98b954EedeAC495271d0F")
    dai_token.decimals = 18
    dai_token.name = "Dai Stablecoin"
    dai_token.symbol = "DAI"

    with pytest.raises(ValueError, match="Could not identify token_in"):
        wbtc_weth_liquiditypool.calculate_tokens_out_from_tokens_in(
            token_in=dai_token,
            token_in_quantity=1 * 10**18,
        )


def test_calculate_tokens_out_from_tokens_in_with_override(
    wbtc_weth_liquiditypool: LiquidityPool
) -> None:
    # Overridden reserve values for this test are taken at block height 17,650,000
    # token0 reserves: 16027096956
    # token1 reserves: 2602647332090181827846

    pool_state_override = UniswapV2PoolState(
        pool=wbtc_weth_liquiditypool,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    assert (
        wbtc_weth_liquiditypool.calculate_tokens_out_from_tokens_in(
            token_in=wbtc_weth_liquiditypool.token0,
            token_in_quantity=8000000000,
            override_state=pool_state_override,
        )
        == 864834865217768537471
    )


def test_calculate_tokens_in_from_tokens_out(wbtc_weth_liquiditypool: LiquidityPool) -> None:
    """
    Reserve values for this test are taken at block height 17,600,000
    """

    assert (
        wbtc_weth_liquiditypool.calculate_tokens_in_from_tokens_out(
            token_out_quantity=8000000000,
            token_out=wbtc_weth_liquiditypool.token0,
        )
        == 2506650866141614297072
    )

    assert (
        wbtc_weth_liquiditypool.calculate_tokens_in_from_tokens_out(
            token_out_quantity=1200000000000000000000,
            token_out=wbtc_weth_liquiditypool.token1,
        )
        == 14245938804
    )


def test_calculate_tokens_in_from_tokens_out_with_override(
    wbtc_weth_liquiditypool: LiquidityPool
) -> None:
    # Overridden reserve values for this test are taken at block height 17,650,000
    # token0 reserves: 16027096956
    # token1 reserves: 2602647332090181827846

    pool_state_override = UniswapV2PoolState(
        pool=wbtc_weth_liquiditypool,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    assert (
        wbtc_weth_liquiditypool.calculate_tokens_in_from_tokens_out(
            token_out=wbtc_weth_liquiditypool.token1,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )
        == 13752842264
    )

    dai_token = MockErc20Token()
    dai_token.address = to_checksum_address("0x6B175474E89094C44Da98b954EedeAC495271d0F")
    dai_token.decimals = 18
    dai_token.name = "Dai Stablecoin"
    dai_token.symbol = "DAI"

    with pytest.raises(ValueError, match="Could not identify token_out"):
        wbtc_weth_liquiditypool.calculate_tokens_in_from_tokens_out(
            token_out=dai_token,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )


def test_comparisons(wbtc_weth_liquiditypool: LiquidityPool) -> None:
    assert wbtc_weth_liquiditypool == "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
    assert wbtc_weth_liquiditypool == "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940".lower()

    del degenbot.AllPools(chain_id=1)[wbtc_weth_liquiditypool]

    other_lp = LiquidityPool(
        address="0xBb2b8038a1640196FbE3e38816F3e67Cba72D940",
        update_method="external",
        tokens=[wbtc_weth_liquiditypool.token0, wbtc_weth_liquiditypool.token1],
        name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY_ADDRESS,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        fee=Fraction(3, 1000),
        empty=True,
    )

    assert wbtc_weth_liquiditypool == other_lp
    assert wbtc_weth_liquiditypool is not other_lp

    with pytest.raises(NotImplementedError):
        assert wbtc_weth_liquiditypool == 420

    # sets depend on __hash__ dunder method
    set([wbtc_weth_liquiditypool, other_lp])


def test_reorg(wbtc_weth_liquiditypool: LiquidityPool) -> None:
    _START_BLOCK = wbtc_weth_liquiditypool.update_block + 1
    _END_BLOCK = wbtc_weth_liquiditypool.update_block + 10

    # Provide some dummy updates, then simulate a reorg back to the starting state
    starting_state = wbtc_weth_liquiditypool.state
    starting_token0_reserves = starting_state.reserves_token0
    starting_token1_reserves = starting_state.reserves_token1

    block_states: Dict[int, UniswapV2PoolState] = {
        wbtc_weth_liquiditypool.update_block: wbtc_weth_liquiditypool.state
    }

    for block_number in range(_START_BLOCK, _END_BLOCK + 1, 1):
        wbtc_weth_liquiditypool.update_reserves(
            external_token0_reserves=starting_token0_reserves + 10_000 * block_number,
            external_token1_reserves=starting_token1_reserves + 10_000 * block_number,
            print_ratios=False,
            print_reserves=False,
            update_block=block_number,
        )
        block_states[block_number] = wbtc_weth_liquiditypool.state

    print(f"{block_states=}")
    last_block_state = wbtc_weth_liquiditypool.state

    # Cannot restore to a pool state before the first
    with pytest.raises(NoPoolStateAvailable):
        wbtc_weth_liquiditypool.restore_state_before_block(0)

    # Last state is before this block, so this will succeed but have no effect on the current state
    wbtc_weth_liquiditypool.restore_state_before_block(_END_BLOCK + 1)
    assert wbtc_weth_liquiditypool.state == last_block_state

    # Unwind the updates and compare to the stored states at previous blocks
    for block_number in range(_END_BLOCK + 1, _START_BLOCK - 1, -1):
        print(f"Restoring block before {block_number}")
        wbtc_weth_liquiditypool.restore_state_before_block(block_number)
        assert wbtc_weth_liquiditypool.state == block_states[block_number - 1]

    # Verify the pool has been returned to the starting state
    assert wbtc_weth_liquiditypool.state == starting_state

    # Unwind all states
    wbtc_weth_liquiditypool.restore_state_before_block(1)
    assert wbtc_weth_liquiditypool.state == UniswapV2PoolState(wbtc_weth_liquiditypool, 0, 0)


def test_simulations(wbtc_weth_liquiditypool: LiquidityPool) -> None:
    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=8000000000,
        amount1_delta=-847228560678214929944,
        current_state=wbtc_weth_liquiditypool.state,
        future_state=UniswapV2PoolState(
            pool=wbtc_weth_liquiditypool,
            reserves_token0=wbtc_weth_liquiditypool.reserves_token0 + 8000000000,
            reserves_token1=wbtc_weth_liquiditypool.reserves_token1 - 847228560678214929944,
        ),
    )

    # token_in = lp.token0 should have same result as token_out = lp.token1
    assert (
        wbtc_weth_liquiditypool.simulate_swap(
            token_in=wbtc_weth_liquiditypool.token0,
            token_in_quantity=8000000000,
        )
        == sim_result
    )
    assert (
        wbtc_weth_liquiditypool.simulate_swap(
            token_out=wbtc_weth_liquiditypool.token1,
            token_in_quantity=8000000000,
        )
        == sim_result
    )

    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=-5154005339,
        amount1_delta=1200000000000000000000,
        current_state=wbtc_weth_liquiditypool.state,
        future_state=UniswapV2PoolState(
            pool=wbtc_weth_liquiditypool,
            reserves_token0=wbtc_weth_liquiditypool.reserves_token0 - 5154005339,
            reserves_token1=wbtc_weth_liquiditypool.reserves_token1 + 1200000000000000000000,
        ),
    )

    assert (
        wbtc_weth_liquiditypool.simulate_swap(
            token_in=wbtc_weth_liquiditypool.token1,
            token_in_quantity=1200000000000000000000,
        )
        == sim_result
    )

    assert (
        wbtc_weth_liquiditypool.simulate_swap(
            token_out=wbtc_weth_liquiditypool.token0,
            token_in_quantity=1200000000000000000000,
        )
        == sim_result
    )

    ADDED_LIQUIDITY = 10_000_000
    assert wbtc_weth_liquiditypool.simulate_add_liquidity(
        added_reserves_token0=ADDED_LIQUIDITY, added_reserves_token1=ADDED_LIQUIDITY
    ) == UniswapV2PoolSimulationResult(
        amount0_delta=ADDED_LIQUIDITY,
        amount1_delta=ADDED_LIQUIDITY,
        current_state=wbtc_weth_liquiditypool.state,
        future_state=UniswapV2PoolState(
            pool=wbtc_weth_liquiditypool,
            reserves_token0=wbtc_weth_liquiditypool.reserves_token0 + ADDED_LIQUIDITY,
            reserves_token1=wbtc_weth_liquiditypool.reserves_token1 + ADDED_LIQUIDITY,
        ),
    )

    REMOVED_LIQUIDITY = 10_000_000
    assert wbtc_weth_liquiditypool.simulate_remove_liquidity(
        removed_reserves_token0=REMOVED_LIQUIDITY, removed_reserves_token1=REMOVED_LIQUIDITY
    ) == UniswapV2PoolSimulationResult(
        amount0_delta=-REMOVED_LIQUIDITY,
        amount1_delta=-REMOVED_LIQUIDITY,
        current_state=wbtc_weth_liquiditypool.state,
        future_state=UniswapV2PoolState(
            pool=wbtc_weth_liquiditypool,
            reserves_token0=wbtc_weth_liquiditypool.reserves_token0 - REMOVED_LIQUIDITY,
            reserves_token1=wbtc_weth_liquiditypool.reserves_token1 - REMOVED_LIQUIDITY,
        ),
    )


def test_simulations_with_override(wbtc_weth_liquiditypool: LiquidityPool) -> None:
    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=8000000000,
        amount1_delta=-864834865217768537471,
        current_state=wbtc_weth_liquiditypool.state,
        future_state=UniswapV2PoolState(
            pool=wbtc_weth_liquiditypool,
            reserves_token0=wbtc_weth_liquiditypool.reserves_token0 + 8000000000,
            reserves_token1=wbtc_weth_liquiditypool.reserves_token1 - 864834865217768537471,
        ),
    )

    pool_state_override = UniswapV2PoolState(
        pool=wbtc_weth_liquiditypool,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    assert (
        wbtc_weth_liquiditypool.simulate_swap(
            token_in=wbtc_weth_liquiditypool.token0,
            token_in_quantity=8000000000,
            override_state=pool_state_override,
        )
        == sim_result
    )

    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=13752842264,
        amount1_delta=-1200000000000000000000,
        current_state=wbtc_weth_liquiditypool.state,
        future_state=UniswapV2PoolState(
            pool=wbtc_weth_liquiditypool,
            reserves_token0=wbtc_weth_liquiditypool.reserves_token0 + 13752842264,
            reserves_token1=wbtc_weth_liquiditypool.reserves_token1 - 1200000000000000000000,
        ),
    )

    assert (
        wbtc_weth_liquiditypool.simulate_swap(
            token_out=wbtc_weth_liquiditypool.token1,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )
        == sim_result
    )


def test_swap_for_all(wbtc_weth_liquiditypool: LiquidityPool) -> None:
    # The last token in a pool can never be swapped for
    assert (
        wbtc_weth_liquiditypool.calculate_tokens_out_from_tokens_in(
            wbtc_weth_liquiditypool.token1,
            2**256 - 1,
        )
        == wbtc_weth_liquiditypool.reserves_token0 - 1
    )
    assert (
        wbtc_weth_liquiditypool.calculate_tokens_out_from_tokens_in(
            wbtc_weth_liquiditypool.token0,
            2**256 - 1,
        )
        == wbtc_weth_liquiditypool.reserves_token1 - 1
    )

    with pytest.raises(LiquidityPoolError):
        wbtc_weth_liquiditypool.calculate_tokens_in_from_tokens_out(
            token_out=wbtc_weth_liquiditypool.token0,
            token_out_quantity=wbtc_weth_liquiditypool.reserves_token0,
        )

    with pytest.raises(LiquidityPoolError):
        wbtc_weth_liquiditypool.calculate_tokens_in_from_tokens_out(
            token_out=wbtc_weth_liquiditypool.token1,
            token_out_quantity=wbtc_weth_liquiditypool.reserves_token1,
        )


def test_zero_swaps(wbtc_weth_liquiditypool: LiquidityPool) -> None:
    with pytest.raises(ZeroSwapError):
        assert (
            wbtc_weth_liquiditypool.calculate_tokens_out_from_tokens_in(
                wbtc_weth_liquiditypool.token0,
                0,
            )
            == 0
        )

    with pytest.raises(ZeroSwapError):
        assert (
            wbtc_weth_liquiditypool.calculate_tokens_out_from_tokens_in(
                wbtc_weth_liquiditypool.token1,
                0,
            )
            == 0
        )


def test_polling_update(wbtc_weth_liquiditypool: LiquidityPool, load_env: dict) -> None:
    fork = AnvilFork(f"https://rpc.ankr.com/eth/{load_env['ANKR_API_KEY']}", fork_block=18_000_000)
    degenbot.set_web3(fork.w3)
    wbtc_weth_liquiditypool._update_method = "polling"
    assert wbtc_weth_liquiditypool.update_reserves() is True
    assert wbtc_weth_liquiditypool.update_reserves() is False


def test_late_update(wbtc_weth_liquiditypool: LiquidityPool) -> None:
    # Provide some semi-random updates
    for block_number in range(
        wbtc_weth_liquiditypool.update_block, wbtc_weth_liquiditypool.update_block + 5
    ):
        wbtc_weth_liquiditypool.update_reserves(
            external_token0_reserves=wbtc_weth_liquiditypool.reserves_token0 + block_number * 10,
            external_token1_reserves=wbtc_weth_liquiditypool.reserves_token1 - block_number * 10,
            update_block=block_number,
        )

    # Send a late update
    with pytest.raises(ExternalUpdateError):
        wbtc_weth_liquiditypool.update_reserves(
            external_token0_reserves=wbtc_weth_liquiditypool.reserves_token0 + 1,
            external_token1_reserves=wbtc_weth_liquiditypool.reserves_token1 - 1,
            update_block=wbtc_weth_liquiditypool.update_block - 1,
        )

    with pytest.raises(ValueError):
        assert (
            wbtc_weth_liquiditypool.update_reserves(
                update_block=wbtc_weth_liquiditypool.update_block + 1,
            )
            is False
        )

    # with pytest.raises(ValueError):
    assert (
        wbtc_weth_liquiditypool.update_reserves(
            external_token0_reserves=wbtc_weth_liquiditypool.reserves_token0,
            external_token1_reserves=wbtc_weth_liquiditypool.reserves_token1,
            update_block=wbtc_weth_liquiditypool.update_block + 1,
        )
        is False
    )
