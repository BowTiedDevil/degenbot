"""
Integration tests for the BalancerV2StablePool production class.

Verifies that the class's calculate_tokens_out_from_tokens_in() and
calculate_tokens_in_from_tokens_out() match on-chain querySwap results.

MetaStablePools: exact 0-wei matching (no rate caching uncertainty).
ComposableStablePools: ≤3000 wei tolerance (rate provider timestamp differences).
"""

import json
from fractions import Fraction

import pytest
from web3.exceptions import ContractLogicError

from degenbot.anvil_fork import AnvilFork
from degenbot.balancer.deployments import (
    BALANCER_V2_VAULT_ADDRESS,
    BALANCERQUERIES_CONTRACT_ADDRESS,
)
from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token

# ---------- ABIs ----------

POOL_ABI = json.loads(
    """
    [{"inputs":[],"name":"getAmplificationParameter","outputs":[{"internalType":"uint256","name":"value","type":"uint256"},{"internalType":"bool","name":"isUpdating","type":"bool"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getPoolId","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getSwapFeePercentage","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getRateProviders","outputs":[{"internalType":"contract IRateProvider[]","name":"","type":"address[]"}],"stateMutability":"view","type":"function"}]
    """  # noqa:E501
)

VAULT_ABI = json.loads(
    """
    [{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"}],"name":"getPoolTokens","outputs":[{"internalType":"contract IERC20[]","name":"tokens","type":"address[]"},{"internalType":"uint256[]","name":"balances","type":"uint256[]"},{"internalType":"uint256","name":"lastChangeBlock","type":"uint256"}],"stateMutability":"view","type":"function"}]
    """  # noqa:E501
)

QUERIES_ABI = json.loads(
    """
    [{"inputs":[{"components":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"enum IVault.SwapKind","name":"kind","type":"uint8"},{"internalType":"contract IAsset","name":"assetIn","type":"address"},{"internalType":"contract IAsset","name":"assetOut","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"bytes","name":"userData","type":"bytes"}],"internalType":"struct IVault.SingleSwap","name":"singleSwap","type":"tuple"},{"components":[{"internalType":"address","name":"sender","type":"address"},{"internalType":"bool","name":"fromInternalBalance","type":"bool"},{"internalType":"address payable","name":"recipient","type":"address"},{"internalType":"bool","name":"toInternalBalance","type":"bool"}],"internalType":"struct IVault.FundManagement","name":"funds","type":"tuple"}],"name":"querySwap","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"nonpayable","type":"function"}]
    """  # noqa:E501
)

RATE_ABI = json.loads(
    """
    [{"inputs":[],"name":"getRate","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"}]
    """
)

ERC20_ABI = json.loads(
    """
    [{"inputs":[],"name":"decimals","outputs":[{"internalType":"uint8","name":"","type":"uint8"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"symbol","outputs":[{"internalType":"string","name":"","type":"string"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"name","outputs":[{"internalType":"string","name":"","type":"string"}],"stateMutability":"view","type":"function"}]
    """
)

VITALIK_ADDRESS = get_checksum_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")

# ---------- Pool definitions ----------

META_STABLE_POOL_ADDRESSES = {
    "wsteth_weth": "0x32296969Ef14EB0c6d29669C550D4a0449130230",
    "cbeth_wsteth": "0x9c6d47Ff73e0F5E51BE5FD53236e3F595C5793F2",
}

COMPOSABLE_STABLE_POOL_ADDRESSES = {
    "tusd_bsp": "0x53BC3cBa3832ebeCBFa002c12023F8ab1AA3a3a0",
    "bb_s_usd": "0x779d01F939D78a918A3de18cC236ee89221dfd4E",
}


def _build_stable_pool(
    fork: AnvilFork,
    pool_address: str,
    bpt_idx: int | None = None,
) -> BalancerV2StablePool:
    """Build a BalancerV2StablePool from on-chain data."""
    w3 = fork.w3
    vault = w3.eth.contract(
        address=get_checksum_address(BALANCER_V2_VAULT_ADDRESS),
        abi=VAULT_ABI,
    )
    pool = w3.eth.contract(
        address=get_checksum_address(pool_address),
        abi=POOL_ABI,
    )

    pool_id = pool.functions.getPoolId().call()
    amp_value, _ = pool.functions.getAmplificationParameter().call()
    swap_fee = pool.functions.getSwapFeePercentage().call()
    rate_providers = pool.functions.getRateProviders().call()
    tokens, balances, _ = vault.functions.getPoolTokens(pool_id).call()

    # Build Erc20Tokens and fresh scaling factors
    erc20_tokens: list[Erc20Token] = []
    scaling_factors: list[int] = []
    for i, t in enumerate(tokens):
        erc20_c = w3.eth.contract(address=get_checksum_address(t), abi=ERC20_ABI)
        decimals = erc20_c.functions.decimals().call()
        symbol = erc20_c.functions.symbol().call()
        name = erc20_c.functions.name().call()
        base_sf = ONE * 10 ** (18 - decimals)

        rp = rate_providers[i]
        if rp != "0x0000000000000000000000000000000000000000":
            rp_c = w3.eth.contract(
                address=get_checksum_address(rp), abi=RATE_ABI
            )
            rate = rp_c.functions.getRate().call()
        else:
            rate = ONE

        scaling_factors.append(base_sf * rate // ONE)
        erc20_tokens.append(
            Erc20Token(address=t, name=name, symbol=symbol, decimals=decimals)
        )

    # Auto-detect BPT index if not provided
    if bpt_idx is None:
        for i, t in enumerate(tokens):
            if t.lower() == pool_address.lower():
                bpt_idx_actual = i
                break
        else:
            bpt_idx_actual = None
    else:
        bpt_idx_actual = bpt_idx

    return BalancerV2StablePool(
        address=pool_address,
        pool_id=pool_id,
        vault=BALANCER_V2_VAULT_ADDRESS,
        tokens=erc20_tokens,
        balances=balances,
        fee=Fraction(swap_fee, ONE),
        amp=amp_value,
        scaling_factors=scaling_factors,
        bpt_idx=bpt_idx_actual,
    )


def _query_swap(  # noqa: PLR0917
    fork: AnvilFork,
    pool_id: bytes,
    token_in: str,
    token_out: str,
    amount: int,
    kind: int,
) -> int:
    """Query the BalancerQueries contract for a swap result."""
    w3 = fork.w3
    queries = w3.eth.contract(
        address=get_checksum_address(BALANCERQUERIES_CONTRACT_ADDRESS),
        abi=QUERIES_ABI,
    )
    try:
        return queries.functions.querySwap(
            (pool_id, kind, token_in, token_out, amount, b""),
            (VITALIK_ADDRESS, False, VITALIK_ADDRESS, False),
        ).call()
    except ContractLogicError as e:
        pytest.skip(f"On-chain query reverted: {e}")


class TestBalancerV2StablePoolMetaStable:
    """
    Test BalancerV2StablePool against on-chain for MetaStablePools.

    MetaStablePools have no BPT token and no rate caching uncertainty,
    so we expect exact 0-wei matching.
    """

    @pytest.fixture
    def wsteth_weth_pool(self, fork_mainnet_archive):
        return _build_stable_pool(
            fork_mainnet_archive,
            META_STABLE_POOL_ADDRESSES["wsteth_weth"],
        )

    @pytest.fixture
    def cbeth_wsteth_pool(self, fork_mainnet_archive):
        return _build_stable_pool(
            fork_mainnet_archive,
            META_STABLE_POOL_ADDRESSES["cbeth_wsteth"],
        )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_wsteth_to_weth(self, wsteth_weth_pool, fork_mainnet_archive, pct):
        """GIVEN_IN wstETH→WETH must match on-chain exactly."""
        pool = wsteth_weth_pool
        amount_in = pool.balances[0] // pct

        python_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.tokens[0],
            token_out=pool.tokens[1],
            token_in_quantity=amount_in,
        )
        on_chain_out = _query_swap(
            fork_mainnet_archive,
            pool.pool_id,
            pool.tokens[0].address,
            pool.tokens[1].address,
            amount_in,
            kind=0,
        )

        if on_chain_out > 0:
            assert python_out == on_chain_out, (
                f"pct=1/{pct}: diff={python_out - on_chain_out}"
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_weth_to_wsteth(self, wsteth_weth_pool, fork_mainnet_archive, pct):
        """GIVEN_IN WETH→wstETH must match on-chain exactly."""
        pool = wsteth_weth_pool
        amount_in = pool.balances[1] // pct

        python_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.tokens[1],
            token_out=pool.tokens[0],
            token_in_quantity=amount_in,
        )
        on_chain_out = _query_swap(
            fork_mainnet_archive,
            pool.pool_id,
            pool.tokens[1].address,
            pool.tokens[0].address,
            amount_in,
            kind=0,
        )

        if on_chain_out > 0:
            assert python_out == on_chain_out, (
                f"pct=1/{pct}: diff={python_out - on_chain_out}"
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_cbeth_to_wsteth(self, cbeth_wsteth_pool, fork_mainnet_archive, pct):
        """GIVEN_IN cbETH→wstETH must match on-chain exactly."""
        pool = cbeth_wsteth_pool
        amount_in = pool.balances[0] // pct

        python_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.tokens[0],
            token_out=pool.tokens[1],
            token_in_quantity=amount_in,
        )
        on_chain_out = _query_swap(
            fork_mainnet_archive,
            pool.pool_id,
            pool.tokens[0].address,
            pool.tokens[1].address,
            amount_in,
            kind=0,
        )

        if on_chain_out > 0:
            assert python_out == on_chain_out, (
                f"pct=1/{pct}: diff={python_out - on_chain_out}"
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_weth_for_wsteth(self, wsteth_weth_pool, fork_mainnet_archive, pct):
        """GIVEN_OUT: requesting WETH out, paying wstETH must match on-chain exactly."""
        pool = wsteth_weth_pool
        amount_out = pool.balances[1] // pct

        python_in = pool.calculate_tokens_in_from_tokens_out(
            token_in=pool.tokens[0],
            token_out=pool.tokens[1],
            token_out_quantity=amount_out,
        )
        on_chain_in = _query_swap(
            fork_mainnet_archive,
            pool.pool_id,
            pool.tokens[0].address,
            pool.tokens[1].address,
            amount_out,
            kind=1,
        )

        if on_chain_in > 0:
            assert python_in == on_chain_in, (
                f"pct=1/{pct}: diff={python_in - on_chain_in}"
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_wsteth_for_weth(self, wsteth_weth_pool, fork_mainnet_archive, pct):
        """GIVEN_OUT: requesting wstETH out, paying WETH must match on-chain exactly."""
        pool = wsteth_weth_pool
        amount_out = pool.balances[0] // pct

        python_in = pool.calculate_tokens_in_from_tokens_out(
            token_in=pool.tokens[1],
            token_out=pool.tokens[0],
            token_out_quantity=amount_out,
        )
        on_chain_in = _query_swap(
            fork_mainnet_archive,
            pool.pool_id,
            pool.tokens[1].address,
            pool.tokens[0].address,
            amount_out,
            kind=1,
        )

        if on_chain_in > 0:
            assert python_in == on_chain_in, (
                f"pct=1/{pct}: diff={python_in - on_chain_in}"
            )


class TestBalancerV2StablePoolComposable:
    """
    Test BalancerV2StablePool against on-chain for ComposableStablePools.

    ComposableStablePools include a BPT token that is automatically dropped.
    Rate provider timestamps cause ≤3000 wei differences.
    """

    @pytest.fixture
    def tusd_bsp_pool(self, fork_mainnet_archive):
        return _build_stable_pool(
            fork_mainnet_archive,
            COMPOSABLE_STABLE_POOL_ADDRESSES["tusd_bsp"],
        )

    @pytest.fixture
    def bb_s_usd_pool(self, fork_mainnet_archive):
        return _build_stable_pool(
            fork_mainnet_archive,
            COMPOSABLE_STABLE_POOL_ADDRESSES["bb_s_usd"],
        )

    def _assert_close(
        self,
        python_result: int,
        on_chain_result: int,
        max_wei_diff: int = 3000,
        label: str = "",
    ) -> None:
        diff = abs(python_result - on_chain_result)
        assert diff <= max_wei_diff, (
            f"{label}Python={python_result}, On-chain={on_chain_result}, "
            f"diff={diff} (max={max_wei_diff})"
        )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_tusd_to_usdc(self, tusd_bsp_pool, fork_mainnet_archive, pct):
        """GIVEN_IN TUSD→USDC using BalancerV2StablePool must match on-chain."""
        pool = tusd_bsp_pool
        amount_in = pool.balances[0] // pct  # TUSD at index 0

        python_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.tokens[0],
            token_out=pool.tokens[2],  # USDC at index 2
            token_in_quantity=amount_in,
        )
        on_chain_out = _query_swap(
            fork_mainnet_archive,
            pool.pool_id,
            pool.tokens[0].address,
            pool.tokens[2].address,
            amount_in,
            kind=0,
        )

        if on_chain_out > 0:
            self._assert_close(python_out, on_chain_out, label=f"pct=1/{pct}: ")

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_usdc_to_tusd(self, tusd_bsp_pool, fork_mainnet_archive, pct):
        """GIVEN_IN USDC→TUSD using BalancerV2StablePool must match on-chain."""
        pool = tusd_bsp_pool
        amount_in = pool.balances[2] // pct

        python_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.tokens[2],
            token_out=pool.tokens[0],
            token_in_quantity=amount_in,
        )
        on_chain_out = _query_swap(
            fork_mainnet_archive,
            pool.pool_id,
            pool.tokens[2].address,
            pool.tokens[0].address,
            amount_in,
            kind=0,
        )

        if on_chain_out > 0:
            self._assert_close(python_out, on_chain_out, label=f"pct=1/{pct}: ")

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_usdc_to_usdt(self, bb_s_usd_pool, fork_mainnet_archive, pct):
        """GIVEN_IN bb-s-USDC→bb-s-USDT using BalancerV2StablePool."""
        pool = bb_s_usd_pool
        # BPT at index 0, USDC at 1, USDT at 2, DAI at 3
        amount_in = pool.balances[1] // pct

        python_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.tokens[1],
            token_out=pool.tokens[2],
            token_in_quantity=amount_in,
        )
        on_chain_out = _query_swap(
            fork_mainnet_archive,
            pool.pool_id,
            pool.tokens[1].address,
            pool.tokens[2].address,
            amount_in,
            kind=0,
        )

        if on_chain_out > 0:
            self._assert_close(python_out, on_chain_out, label=f"pct=1/{pct}: ")

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_usdc_for_tusd(self, tusd_bsp_pool, fork_mainnet_archive, pct):
        """GIVEN_OUT: requesting USDC out for TUSD in using BalancerV2StablePool."""
        pool = tusd_bsp_pool
        amount_out = pool.balances[2] // pct  # USDC at index 2

        python_in = pool.calculate_tokens_in_from_tokens_out(
            token_in=pool.tokens[0],
            token_out=pool.tokens[2],
            token_out_quantity=amount_out,
        )
        on_chain_in = _query_swap(
            fork_mainnet_archive,
            pool.pool_id,
            pool.tokens[0].address,
            pool.tokens[2].address,
            amount_out,
            kind=1,
        )

        if on_chain_in > 0:
            self._assert_close(python_in, on_chain_in, label=f"pct=1/{pct}: ")

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_usdt_for_usdc(self, bb_s_usd_pool, fork_mainnet_archive, pct):
        """GIVEN_OUT: requesting bb-s-USDT out for bb-s-USDC in."""
        pool = bb_s_usd_pool
        amount_out = pool.balances[2] // pct  # USDT at index 2

        python_in = pool.calculate_tokens_in_from_tokens_out(
            token_in=pool.tokens[1],
            token_out=pool.tokens[2],
            token_out_quantity=amount_out,
        )
        on_chain_in = _query_swap(
            fork_mainnet_archive,
            pool.pool_id,
            pool.tokens[1].address,
            pool.tokens[2].address,
            amount_out,
            kind=1,
        )

        if on_chain_in > 0:
            self._assert_close(python_in, on_chain_in, label=f"pct=1/{pct}: ")
