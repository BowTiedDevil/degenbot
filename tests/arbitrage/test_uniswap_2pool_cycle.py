import dataclasses

import pytest
import web3

from degenbot import (
    AnvilFork,
    Erc20Token,
    EtherPlaceholder,
    UniswapV2Pool,
    UniswapV3Pool,
    UniswapV4Pool,
    get_checksum_address,
    pool_registry,
    set_web3,
    token_registry,
)
from degenbot.arbitrage.types import UniswapV2PoolSwapAmounts, UniswapV4PoolSwapAmounts
from degenbot.arbitrage.uniswap_2pool_cycle_testing import _UniswapTwoPoolCycleTesting
from degenbot.exceptions.arbitrage import RateOfExchangeBelowMinimum
from degenbot.exceptions.liquidity_pool import PossibleInaccurateResult
from tests.conftest import env_values

# Token addresses
NATIVE_ADDRESS = get_checksum_address("0x0000000000000000000000000000000000000000")
WBTC_ADDRESS = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

# Uniswap V2 & V3 WBTC-WETH pools
WBTC_WETH_V2_POOL_ADDRESS = "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
WBTC_WETH_V3_POOL_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"

# Uniswap V4
V4_POOL_MANAGER = get_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90")
V4_STATE_VIEW_ADDRESS = get_checksum_address("0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227")

# Uniswap V4 WBTC-Ether pool
WBTC_ETH_V4_POOL_ID = "0x54c72c46df32f2cc455e84e41e191b26ed73a29452cdd3d82f511097af9f427e"
WBTC_ETH_V4_POOL_FEE = 3000
WBTC_ETH_V4_POOL_TICK_SPACING = 60
WBTC_ETH_V4_POOL_HOOKS = "0x0000000000000000000000000000000000000000"


@pytest.fixture
def wbtc(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)

    token = token_registry.get(
        chain_id=fork_mainnet_full.w3.eth.chain_id,
        token_address=WBTC_ADDRESS,
    )
    if token is None:
        token = Erc20Token(WBTC_ADDRESS)
    return token


@pytest.fixture
def weth(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)

    token = token_registry.get(
        chain_id=fork_mainnet_full.w3.eth.chain_id,
        token_address=WETH_ADDRESS,
    )
    if token is None:
        token = Erc20Token(WETH_ADDRESS)
    return token


@pytest.fixture
def ether_placeholder(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)

    token = token_registry.get(
        chain_id=fork_mainnet_full.w3.eth.chain_id,
        token_address=NATIVE_ADDRESS,
    )
    if token is None:
        token = EtherPlaceholder(NATIVE_ADDRESS)
    return token


@pytest.fixture
def wbtc_weth_v2_lp(fork_mainnet_full: AnvilFork) -> UniswapV2Pool:
    set_web3(fork_mainnet_full.w3)

    pool = pool_registry.get(
        chain_id=fork_mainnet_full.w3.eth.chain_id,
        pool_address=WBTC_WETH_V2_POOL_ADDRESS,
    )
    if pool is None:
        pool = UniswapV2Pool(WBTC_WETH_V2_POOL_ADDRESS)
    assert isinstance(pool, UniswapV2Pool)
    return pool


@pytest.fixture
def wbtc_weth_v3_lp(fork_mainnet_full: AnvilFork) -> UniswapV3Pool:
    set_web3(fork_mainnet_full.w3)

    pool = pool_registry.get(
        chain_id=fork_mainnet_full.w3.eth.chain_id,
        pool_address=WBTC_WETH_V3_POOL_ADDRESS,
    )
    if pool is None:
        pool = UniswapV3Pool(WBTC_WETH_V3_POOL_ADDRESS)
    assert isinstance(pool, UniswapV3Pool)
    return pool


@pytest.fixture
def wbtc_ether_v4_lp(fork_mainnet_full: AnvilFork) -> UniswapV4Pool:
    set_web3(fork_mainnet_full.w3)

    pool = pool_registry.get(
        chain_id=fork_mainnet_full.w3.eth.chain_id,
        pool_address=V4_POOL_MANAGER,
        pool_id=WBTC_ETH_V4_POOL_ID,
    )
    if pool is None:
        pool = UniswapV4Pool(
            pool_id=WBTC_ETH_V4_POOL_ID,
            pool_manager_address=V4_POOL_MANAGER,
            state_view_address=V4_STATE_VIEW_ADDRESS,
            tokens=(WBTC_ADDRESS, NATIVE_ADDRESS),
            fee=WBTC_ETH_V4_POOL_FEE,
            tick_spacing=WBTC_ETH_V4_POOL_TICK_SPACING,
        )
    assert isinstance(pool, UniswapV4Pool)
    return pool


@pytest.fixture
def arb_v4_v2(
    wbtc_ether_v4_lp: UniswapV3Pool,
    wbtc_weth_v2_lp: UniswapV2Pool,
    ether_placeholder: EtherPlaceholder,
):
    return _UniswapTwoPoolCycleTesting(
        id="V4 -> V2",
        input_token=ether_placeholder,
        swap_pools=[
            wbtc_ether_v4_lp,
            wbtc_weth_v2_lp,
        ],
        max_input=100 * 10**18,
    )


@pytest.fixture
def arb_v2_v4(
    wbtc_weth_v2_lp: UniswapV2Pool,
    wbtc_ether_v4_lp: UniswapV3Pool,
    weth: Erc20Token,
):
    return _UniswapTwoPoolCycleTesting(
        id="V2 -> V4",
        input_token=weth,
        swap_pools=[
            wbtc_weth_v2_lp,
            wbtc_ether_v4_lp,
        ],
        max_input=100 * 10**18,
    )


def test_create_arb_with_either_token_input_or_pools_in_any_order(
    wbtc_weth_v2_lp: UniswapV2Pool,
    wbtc_ether_v4_lp: UniswapV4Pool,
    ether_placeholder: Erc20Token,
    weth: Erc20Token,
):
    arb = _UniswapTwoPoolCycleTesting(
        id="test_arb",
        input_token=ether_placeholder,
        swap_pools=[
            wbtc_ether_v4_lp,
            wbtc_weth_v2_lp,
        ],
        max_input=100 * 10**18,
    )
    assert arb.swap_pools[0] is wbtc_ether_v4_lp
    assert arb.swap_pools[1] is wbtc_weth_v2_lp

    _UniswapTwoPoolCycleTesting(
        id="test_arb",
        input_token=ether_placeholder,
        swap_pools=[
            wbtc_ether_v4_lp,
            wbtc_weth_v2_lp,
        ],
        max_input=100 * 10**18,
    )
    assert arb.swap_pools[0] is wbtc_ether_v4_lp
    assert arb.swap_pools[1] is wbtc_weth_v2_lp

    arb = _UniswapTwoPoolCycleTesting(
        id="test_arb",
        input_token=weth,
        swap_pools=[
            wbtc_weth_v2_lp,
            wbtc_ether_v4_lp,
        ],
        max_input=100 * 10**18,
    )
    assert arb.swap_pools[0] is wbtc_weth_v2_lp
    assert arb.swap_pools[1] is wbtc_ether_v4_lp

    arb = _UniswapTwoPoolCycleTesting(
        id="test_arb",
        input_token=weth,
        swap_pools=[
            wbtc_weth_v2_lp,
            wbtc_ether_v4_lp,
        ],
        max_input=100 * 10**18,
    )
    assert arb.swap_pools[0] is wbtc_weth_v2_lp
    assert arb.swap_pools[1] is wbtc_ether_v4_lp


def test_v2_v4_calculation(
    arb_v2_v4: _UniswapTwoPoolCycleTesting,
    weth: Erc20Token,
    ether_placeholder: EtherPlaceholder,
):
    arb = arb_v2_v4
    v2_pool, v4_pool = arb.swap_pools

    assert isinstance(v2_pool, UniswapV2Pool)
    assert isinstance(v4_pool, UniswapV4Pool)

    # Manipulate the V2 reserves by donating WBTC to create an opportunity:
    # The Ether/WBTC exchange rate at the V4 pool is higher than the WETH/WBTC exchange rate at
    # the V2 pool, because its WBTC value is depressed by the surplus.
    # So WETH/Ether can be cycled at a profit:
    #   - trade WETH -> WBTC  @ V2 (sell "overpriced" WETH for "underpriced" WBTC)
    #   - trade WBTC -> Ether @ V4 (sell "overpriced" WBTC for "underpriced" Ether)

    assert v2_pool.token0 == WBTC_ADDRESS
    wbtc_donation_amount = 10 * 10**v2_pool.token0.decimals
    starting_state = v2_pool.state
    v2_pool._state = dataclasses.replace(
        v2_pool.state,
        reserves_token0=v2_pool.reserves_token0 + wbtc_donation_amount,
    )
    assert v2_pool.state.reserves_token0 == starting_state.reserves_token0 + wbtc_donation_amount

    v2_exchange_rate = v2_pool.get_absolute_exchange_rate(token=weth)
    v4_exchange_rate = v4_pool.get_absolute_exchange_rate(token=ether_placeholder)
    assert v4_exchange_rate > v2_exchange_rate

    arb.calculate()

    v2_pool._state = starting_state


def test_v2_v4_calculation_rejects_unprofitable_opportunity(
    arb_v2_v4: _UniswapTwoPoolCycleTesting,
):
    arb = arb_v2_v4

    v2_pool, _ = arb.swap_pools
    assert isinstance(v2_pool, UniswapV2Pool)

    starting_state = v2_pool.state

    # Manipulate the V2 reserves by donating 100 WETH, which creates a V2 ROE > V4 ROE opportunity
    assert v2_pool.token1 == WETH_ADDRESS
    starting_weth_reserves = v2_pool.reserves_token1
    donation_amount = 10 * 10**v2_pool.token1.decimals
    v2_pool._state = dataclasses.replace(
        v2_pool.state,
        reserves_token1=v2_pool.reserves_token1 + donation_amount,
    )
    assert v2_pool.reserves_token1 == starting_weth_reserves + donation_amount

    # The arbitrage path flow is opposite of the opportunity, so the calculation should raise an
    # exception
    with pytest.raises(RateOfExchangeBelowMinimum):
        arb.calculate()

    v2_pool._state = starting_state


def test_v4_v2_calculation(
    arb_v4_v2: _UniswapTwoPoolCycleTesting,
    weth: Erc20Token,
    ether_placeholder: EtherPlaceholder,
):
    arb = arb_v4_v2
    v4_pool, v2_pool = arb.swap_pools

    assert isinstance(v2_pool, UniswapV2Pool)
    assert isinstance(v4_pool, UniswapV4Pool)

    # Manipulate the V2 reserves by donating WETH to create an opportunity:
    # The WETH/WBTC exchange rate at the V2 pool is higher than the Ether/WBTC exchange rate at
    # the V4 pool, because its WETH value is depressed by the surplus.
    # So WETH/Ether can be cycled at a profit:
    #   - trade Ether -> WBTC @ V4 (sell "overpriced" Ether for "underpriced" WBTC)
    #   - trade WBTC  -> WETH @ V2 (sell "overpriced" WBTC  for "underpriced" WETH)

    assert v2_pool.token1 == WETH_ADDRESS
    weth_donation_amount = 100 * 10**v2_pool.token1.decimals
    starting_state = v2_pool.state
    v2_pool._state = dataclasses.replace(
        v2_pool.state,
        reserves_token1=v2_pool.reserves_token1 + weth_donation_amount,
    )
    assert v2_pool.state.reserves_token1 == starting_state.reserves_token1 + weth_donation_amount

    v2_exchange_rate = v2_pool.get_absolute_exchange_rate(token=weth)
    v4_exchange_rate = v4_pool.get_absolute_exchange_rate(token=ether_placeholder)
    assert v2_exchange_rate > v4_exchange_rate

    arb.calculate()

    v2_pool._state = starting_state


def test_v4_v2_calculation_rejects_unprofitable_opportunity(arb_v4_v2: _UniswapTwoPoolCycleTesting):
    arb = arb_v4_v2

    _, v2_pool = arb.swap_pools

    assert isinstance(v2_pool, UniswapV2Pool)

    starting_state = v2_pool.state

    # Manipulate the V2 reserves by donating WBTC, which creates a V4 ROE > V2 ROE opportunity
    assert v2_pool.token0 == WBTC_ADDRESS
    wbtc_donation_amount = 10 * 10**v2_pool.token0.decimals
    v2_pool._state = dataclasses.replace(
        v2_pool.state,
        reserves_token0=v2_pool.reserves_token0 + wbtc_donation_amount,
    )

    # The arbitrage path flow is opposite of the opportunity, so the calculation should raise an
    # exception
    with pytest.raises(RateOfExchangeBelowMinimum):
        arb.calculate()

    v2_pool._state = starting_state


def test_v4_v2_dai_arb_base(fork_base_archive: AnvilFork):
    set_web3(fork_base_archive.w3)
    fork_base_archive.reset(block_number=28197806)

    v4_v2_arb_id = "0x69ca7e8fe6804a17e0f38eb1d5013ad10307f9f792f6435200c7a8e6fedefbb3"
    v4_dai_address = "0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb"
    v4_base_eth_dai_pool_id = "0x4b882f394f112820aee2dcb9c17e74af5c5696373c0d029446bd5285077aa00c"
    v4_base_eth_dai_pool_fee = 100
    v4_base_eth_dai_pool_tick_spacing = 1
    v4_base_eth_dai_pool_hooks = "0x0000000000000000000000000000000000000000"
    v4_base_pool_manager = get_checksum_address("0x498581fF718922c3f8e6A244956aF099B2652b2b")
    v4_base_state_view = get_checksum_address("0xA3c0c9b65baD0b08107Aa264b0f3dB444b867A71")
    v2_pool_address = get_checksum_address("0x18D7EEb9B63664c06251dd8141eC1D0c14f0C012")

    v4_pool = UniswapV4Pool(
        pool_id=v4_base_eth_dai_pool_id,
        pool_manager_address=v4_base_pool_manager,
        state_view_address=v4_base_state_view,
        fee=v4_base_eth_dai_pool_fee,
        tick_spacing=v4_base_eth_dai_pool_tick_spacing,
        hook_address=v4_base_eth_dai_pool_hooks,
        tokens=(v4_dai_address, NATIVE_ADDRESS),
    )
    v2_pool = UniswapV2Pool(v2_pool_address)

    base_native = token_registry.get(
        token_address=NATIVE_ADDRESS, chain_id=fork_base_archive.w3.eth.chain_id
    )
    assert isinstance(base_native, Erc20Token)

    v4_v2_arb = _UniswapTwoPoolCycleTesting(
        input_token=base_native,
        swap_pools=(
            v4_pool,
            v2_pool,
        ),
        id=v4_v2_arb_id,
    )

    calc_result = v4_v2_arb.calculate()
    v4_swap_amount, v2_swap_amount = calc_result.swap_amounts
    assert isinstance(v4_swap_amount, UniswapV4PoolSwapAmounts)
    assert isinstance(v2_swap_amount, UniswapV2PoolSwapAmounts)

    arbitrage_payloads = v4_v2_arb.generate_payloads(
        from_address=NATIVE_ADDRESS,
        forward_token_amount=calc_result.input_amount,
        pool_swap_amounts=calc_result.swap_amounts,
    )

    v4_amount_in = v4_pool.calculate_tokens_in_from_tokens_out(
        token_out=v4_pool.token1 if v4_swap_amount.zero_for_one else v4_pool.token0,
        token_out_quantity=v4_swap_amount.amount_specified,
    )

    v2_amount_out = max(v2_swap_amount.amounts_out)

    assert v4_amount_in < v2_amount_out

    v4_v2_executor_contract_address = get_checksum_address(
        env_values["V4_V2_EXECUTOR_CONTRACT_ADDRESS"]
    )
    v4_v2_executor_contract_abi = env_values["V4_V2_EXECUTOR_CONTRACT_ABI"]
    operator_address = get_checksum_address(env_values["OPERATOR_ADDRESS"])

    executor_contract = fork_base_archive.w3.eth.contract(
        address=v4_v2_executor_contract_address,
        abi=v4_v2_executor_contract_abi,
    )
    v4_payload, v2_payload = arbitrage_payloads
    arbitrage_transaction_params = executor_contract.functions.execute(
        v2_payload=dataclasses.astuple(v2_payload),
        v4_payload=dataclasses.astuple(v4_payload),
    ).build_transaction(
        transaction={
            "from": operator_address,
            "chainId": fork_base_archive.w3.eth.chain_id,
            "type": 2,
        }
    )
    arbitrage_transaction_params["gas"] = int(
        # bugfix: some TX run out of gas on chain because the gas estimation is too tight
        1.5 * arbitrage_transaction_params["gas"]
    )

    tx = fork_base_archive.w3.eth.send_transaction(arbitrage_transaction_params)
    tx_receipt = fork_base_archive.w3.eth.wait_for_transaction_receipt(tx)
    assert tx_receipt["status"] == 1


def test_v2_v4_usdc_arb_base(fork_base_archive: AnvilFork):
    set_web3(fork_base_archive.w3)
    fork_base_archive.reset(block_number=28203703)

    base_weth_address = get_checksum_address("0x4200000000000000000000000000000000000006")

    v2_weth_usdc_pool_address = get_checksum_address("0x88A43bbDF9D098eEC7bCEda4e2494615dfD9bB9C")
    v4_base_eth_usdc_pool_id = "0xa931b046d1c703ef898b4b7c93400e621059bec0d06f17ad9f1e5d0503041af6"
    v2_v4_arb_id = "0x779849c47b47dcb667d000aa8ddee2fd40a6b8ce6d1c5eb46086fb72d3bf0ada"

    v4_usdc_address = get_checksum_address("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")
    v4_base_eth_usdc_pool_fee = 3000
    v4_base_eth_usdc_pool_tick_spacing = 60
    v4_base_eth_usdc_pool_hooks = "0x1b1ad7c283c95028062fd07ba95ff5205269c888"
    v4_base_pool_manager = get_checksum_address("0x498581fF718922c3f8e6A244956aF099B2652b2b")
    v4_base_state_view = get_checksum_address("0xA3c0c9b65baD0b08107Aa264b0f3dB444b867A71")

    v4_pool = UniswapV4Pool(
        pool_id=v4_base_eth_usdc_pool_id,
        pool_manager_address=v4_base_pool_manager,
        state_view_address=v4_base_state_view,
        fee=v4_base_eth_usdc_pool_fee,
        tick_spacing=v4_base_eth_usdc_pool_tick_spacing,
        hook_address=v4_base_eth_usdc_pool_hooks,
        tokens=(v4_usdc_address, base_weth_address),
    )
    v2_pool = UniswapV2Pool(v2_weth_usdc_pool_address)

    base_weth = token_registry.get(
        token_address=base_weth_address, chain_id=fork_base_archive.w3.eth.chain_id
    )
    assert isinstance(base_weth, Erc20Token)

    v2_v4_arb = _UniswapTwoPoolCycleTesting(
        input_token=base_weth,
        swap_pools=(
            v2_pool,
            v4_pool,
        ),
        id=v2_v4_arb_id,
    )

    calc_result = v2_v4_arb.calculate()
    arbitrage_payloads = v2_v4_arb.generate_payloads(
        from_address=NATIVE_ADDRESS,
        forward_token_amount=calc_result.input_amount,
        pool_swap_amounts=calc_result.swap_amounts,
    )

    v2_swap_amount, v4_swap_amount = calc_result.swap_amounts
    assert isinstance(v2_swap_amount, UniswapV2PoolSwapAmounts)
    assert isinstance(v4_swap_amount, UniswapV4PoolSwapAmounts)

    try:
        v4_amount_in = v4_pool.calculate_tokens_in_from_tokens_out(
            token_out=v4_pool.token1 if v4_swap_amount.zero_for_one else v4_pool.token0,
            token_out_quantity=-v4_swap_amount.amount_specified,
        )
    except PossibleInaccurateResult as exc:
        v4_amount_in = exc.amount_in

    v2_amount_out = max(v2_swap_amount.amounts_out)

    assert v4_amount_in < v2_amount_out

    v4_v2_executor_contract_address = get_checksum_address(
        env_values["V4_V2_EXECUTOR_CONTRACT_ADDRESS"]
    )
    v4_v2_executor_contract_abi = env_values["V4_V2_EXECUTOR_CONTRACT_ABI"]
    operator_address = get_checksum_address(env_values["OPERATOR_ADDRESS"])

    assert isinstance(v4_v2_executor_contract_abi, str)

    executor_contract = fork_base_archive.w3.eth.contract(
        address=v4_v2_executor_contract_address,
        abi=v4_v2_executor_contract_abi,
    )
    v4_payload, v2_payload = arbitrage_payloads

    # The transaction should fail because the hook is not modeled
    with pytest.raises(web3.exceptions.Web3Exception):
        executor_contract.functions.execute(
            v2_payload=dataclasses.astuple(v2_payload),
            v4_payload=dataclasses.astuple(v4_payload),
        ).build_transaction(
            transaction={
                "from": operator_address,
                "chainId": fork_base_archive.w3.eth.chain_id,
                "type": 2,
            }
        )
