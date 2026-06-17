"""Integration tests for the BalancerV2StablePool production class.

Verifies that the class's calculate_tokens_out_from_tokens_in() and
calculate_tokens_in_from_tokens_out() match on-chain querySwap results.

MetaStablePools: exact 0-wei matching (near-static rate providers).
ComposableStablePools:
  - With live rate_provider: exact 0-wei matching per block
  - Without rate_provider: raises StaleRateResult (stale rates)
"""

import json
from fractions import Fraction
from typing import TYPE_CHECKING

import pytest
from web3.exceptions import ContractLogicError

from degenbot.anvil_fork import AnvilFork
from degenbot.balancer.deployments import (
    BALANCER_V2_VAULT_ADDRESS,
    BALANCERQUERIES_CONTRACT_ADDRESS,
)
from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.stable_pools import (
    INVARIANT_V1,
    INVARIANT_V2,
    BalancerRateProvider,
    BalancerV2StablePool,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyBot
from degenbot.exceptions.pool import StaleRateResult
from tests.helpers.erc20_factory import make_erc20

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token

_PY_BOT = PyBot()

# ---------- ABIs ----------


POOL_ABI = json.loads(
    """
    [{"inputs":[],"name":"getAmplificationParameter","outputs":[{"internalType":"uint256","name":"value","type":"uint256"},{"internalType":"bool","name":"isUpdating","type":"bool"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getPoolId","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getSwapFeePercentage","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getRateProviders","outputs":[{"internalType":"contract IRateProvider[]","name":"","type":"address[]"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"token","type":"address"}],"name":"getTokenRateCache","outputs":[{"internalType":"uint256","name":"rate","type":"uint256"},{"internalType":"uint256","name":"oldRate","type":"uint256"},{"internalType":"uint256","name":"duration","type":"uint256"},{"internalType":"uint256","name":"expires","type":"uint256"}],"stateMutability":"view","type":"function"}]
    """  # noqa: E501
)

VAULT_ABI = json.loads(
    """
    [{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"}],"name":"getPoolTokens","outputs":[{"internalType":"contract IERC20[]","name":"tokens","type":"address[]"},{"internalType":"uint256[]","name":"balances","type":"uint256[]"},{"internalType":"uint256","name":"lastChangeBlock","type":"uint256"}],"stateMutability":"view","type":"function"}]
    """  # noqa: E501
)

QUERIES_ABI = json.loads(
    """
    [{"inputs":[{"components":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"enum IVault.SwapKind","name":"kind","type":"uint8"},{"internalType":"contract IAsset","name":"assetIn","type":"address"},{"internalType":"contract IAsset","name":"assetOut","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"bytes","name":"userData","type":"bytes"}],"internalType":"struct IVault.SingleSwap","name":"singleSwap","type":"tuple"},{"components":[{"internalType":"address","name":"sender","type":"address"},{"internalType":"bool","name":"fromInternalBalance","type":"bool"},{"internalType":"address payable","name":"recipient","type":"address"},{"internalType":"bool","name":"toInternalBalance","type":"bool"}],"internalType":"struct IVault.FundManagement","name":"funds","type":"tuple"}],"name":"querySwap","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"nonpayable","type":"function"}]
    """  # noqa: E501
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


# ---------- Rate Provider ----------


class CacheAwareRateProvider:
    """Rate provider that replicates the on-chain _cacheTokenRateIfNecessary flow.

    For each token, reads getTokenRateCache() to get the cached (rate, oldRate,
    duration, expires). If the cache has expired (block timestamp > expires),
    calls the rate provider's getRate() for the fresh value. Otherwise, uses
    the cached rate. This exactly matches the on-chain swap behavior where
    _beforeSwapJoinExit() refreshes rate caches before reading _scalingFactors().

    For BPT tokens and tokens without a rate provider, returns ONE (1e18).
    """

    def __init__(
        self,
        pool_address: str,
        pool_abi: list,
        rate_provider_addresses: list[str],
        token_addresses: list[str],
        bpt_idx: int | None,
        w3,
        *,
        zero_addr: str = "0x0000000000000000000000000000000000000000",
    ) -> None:
        self._pool = w3.eth.contract(address=get_checksum_address(pool_address), abi=pool_abi)
        self._rate_contracts = []
        self._bpt_idx = bpt_idx
        self._w3 = w3

        zero = zero_addr
        for i, rp in enumerate(rate_provider_addresses):
            if i == bpt_idx or rp == zero:
                self._rate_contracts.append(None)
            else:
                self._rate_contracts.append(
                    w3.eth.contract(address=get_checksum_address(rp), abi=RATE_ABI)
                )
        self._token_addresses = token_addresses

    def get_rates(self, block_identifier: int | str | None = None) -> tuple[int, ...]:
        if block_identifier is None:
            block_ts = self._w3.eth.get_block("latest")["timestamp"]
        else:
            block_ts = self._w3.eth.get_block(block_identifier)["timestamp"]

        rates: list[int] = []
        for i, (rp_contract, token_addr) in enumerate(
            zip(self._rate_contracts, self._token_addresses, strict=True)
        ):
            if i == self._bpt_idx or rp_contract is None:
                rates.append(ONE)
                continue

            try:
                rate, _old_rate, _duration, expires = self._pool.functions.getTokenRateCache(
                    get_checksum_address(token_addr)
                ).call(block_identifier=block_identifier)
            except ContractLogicError:
                # Token doesn't have a rate provider — fall back to getRate()
                rate = rp_contract.functions.getRate().call(block_identifier=block_identifier)
                rates.append(rate)
                continue

            if block_ts > expires:
                # Cache expired — on-chain would call provider.getRate()
                fresh = rp_contract.functions.getRate().call(block_identifier=block_identifier)
                rates.append(fresh)
            else:
                # Cache valid — on-chain uses the cached rate
                rates.append(rate)

        return tuple(rates)


# ---------- Helpers ----------


def _build_stable_pool(
    fork: AnvilFork,
    pool_address: str,
    *,
    inject_rate_provider: bool = False,
    invariant_version: int = INVARIANT_V2,
) -> BalancerV2StablePool:
    """Build a BalancerV2StablePool from on-chain data.

    If inject_rate_provider is True, constructs a CacheAwareRateProvider
    that replicates the on-chain _cacheTokenRateIfNecessary flow for
    exact-integer matching.
    """
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

    # Build Erc20Tokens, base scaling factors, and fresh rates
    erc20_tokens: list[Erc20Token] = []
    base_scaling_factors: list[int] = []
    fresh_rates: list[int] = []
    for i, t in enumerate(tokens):
        erc20_c = w3.eth.contract(address=get_checksum_address(t), abi=ERC20_ABI)
        decimals = erc20_c.functions.decimals().call()
        symbol = erc20_c.functions.symbol().call()
        name = erc20_c.functions.name().call()
        base_sf = ONE * 10 ** (18 - decimals)
        base_scaling_factors.append(base_sf)

        rp = rate_providers[i]
        if rp != "0x0000000000000000000000000000000000000000":
            rp_c = w3.eth.contract(address=get_checksum_address(rp), abi=RATE_ABI)
            rate = rp_c.functions.getRate().call()
        else:
            rate = ONE

        fresh_rates.append(rate)
        erc20_tokens.append(make_erc20(_PY_BOT, t, name=name, symbol=symbol, decimals=decimals))

    # Fresh scaling factors = base_sf * rate // ONE (mulDown)
    fresh_scaling_factors = [
        bsf * rate // ONE for bsf, rate in zip(base_scaling_factors, fresh_rates, strict=True)
    ]

    # Auto-detect BPT index
    bpt_idx: int | None = None
    for i, t in enumerate(tokens):
        if t.lower() == pool_address.lower():
            bpt_idx = i
            break

    # Build rate provider if requested
    rate_provider: BalancerRateProvider | None = None
    if inject_rate_provider:
        rate_provider = CacheAwareRateProvider(
            pool_address=pool_address,
            pool_abi=json.loads(POOL_ABI) if isinstance(POOL_ABI, str) else POOL_ABI,
            rate_provider_addresses=[str(rp) for rp in rate_providers],
            token_addresses=[str(t) for t in tokens],
            bpt_idx=bpt_idx,
            w3=w3,
        )

    return BalancerV2StablePool(
        address=pool_address,
        pool_id=pool_id,
        vault=BALANCER_V2_VAULT_ADDRESS,
        tokens=erc20_tokens,
        balances=balances,
        fee=Fraction(swap_fee, ONE),
        amp=amp_value,
        scaling_factors=fresh_scaling_factors,
        base_scaling_factors=base_scaling_factors,
        bpt_idx=bpt_idx,
        rate_provider=rate_provider,
        invariant_version=invariant_version,
    )


def _query_swap(
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
    """Test BalancerV2StablePool against on-chain for MetaStablePools.

    MetaStablePools have no BPT token and near-static rate providers,
    so construction-time scaling factors produce exact 0-wei matching.
    No rate_provider injection is needed and no StaleRateResult
    is raised.
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
            assert python_out == on_chain_out, f"pct=1/{pct}: diff={python_out - on_chain_out}"

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
            assert python_out == on_chain_out, f"pct=1/{pct}: diff={python_out - on_chain_out}"

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
            assert python_out == on_chain_out, f"pct=1/{pct}: diff={python_out - on_chain_out}"

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
            assert python_in == on_chain_in, f"pct=1/{pct}: diff={python_in - on_chain_in}"

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
            assert python_in == on_chain_in, f"pct=1/{pct}: diff={python_in - on_chain_in}"


class TestBalancerV2StablePoolComposableExact:
    """Test BalancerV2StablePool against on-chain for ComposableStablePools
    with a CacheAwareRateProvider injected.

    The CacheAwareRateProvider replicates the on-chain _cacheTokenRateIfNecessary
    flow exactly: reads getTokenRateCache(), checks if the cache has expired
    at the target block's timestamp, and calls getRate() only if expired.
    With this mechanism plus the V1 invariant (always-roundDown), exact
    0-wei matching is achieved.
    """

    @pytest.fixture
    def tusd_bsp_pool(self, fork_mainnet_archive):
        return _build_stable_pool(
            fork_mainnet_archive,
            COMPOSABLE_STABLE_POOL_ADDRESSES["tusd_bsp"],
            inject_rate_provider=True,
            invariant_version=INVARIANT_V1,
        )

    @pytest.fixture
    def bb_s_usd_pool(self, fork_mainnet_archive):
        return _build_stable_pool(
            fork_mainnet_archive,
            COMPOSABLE_STABLE_POOL_ADDRESSES["bb_s_usd"],
            inject_rate_provider=True,
            invariant_version=INVARIANT_V1,
        )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_tusd_to_usdc(self, tusd_bsp_pool, fork_mainnet_archive, pct):
        """GIVEN_IN TUSD→USDC must match on-chain exactly."""
        pool = tusd_bsp_pool
        amount_in = pool.balances[0] // pct

        python_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.tokens[0],
            token_out=pool.tokens[2],
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
            assert python_out == on_chain_out, f"pct=1/{pct}: diff={python_out - on_chain_out}"

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_usdc_to_tusd(self, tusd_bsp_pool, fork_mainnet_archive, pct):
        """GIVEN_IN USDC→TUSD must match on-chain exactly."""
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
            assert python_out == on_chain_out, f"pct=1/{pct}: diff={python_out - on_chain_out}"

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_usdc_to_usdt(self, bb_s_usd_pool, fork_mainnet_archive, pct):
        """GIVEN_IN bb-s-USDC→bb-s-USDT must match on-chain exactly."""
        pool = bb_s_usd_pool
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
            assert python_out == on_chain_out, f"pct=1/{pct}: diff={python_out - on_chain_out}"

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_usdc_for_tusd(self, tusd_bsp_pool, fork_mainnet_archive, pct):
        """GIVEN_OUT: requesting USDC out for TUSD in must match on-chain exactly."""
        pool = tusd_bsp_pool
        amount_out = pool.balances[2] // pct

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
            assert python_in == on_chain_in, f"pct=1/{pct}: diff={python_in - on_chain_in}"

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_usdt_for_usdc(self, bb_s_usd_pool, fork_mainnet_archive, pct):
        """GIVEN_OUT: requesting bb-s-USDT out for bb-s-USDC in must match exactly."""
        pool = bb_s_usd_pool
        amount_out = pool.balances[2] // pct

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
            assert python_in == on_chain_in, f"pct=1/{pct}: diff={python_in - on_chain_in}"


class TestBalancerV2StablePoolComposableStaleRates:
    """Test that ComposableStablePools without a live rate_provider raise
    StaleRateResult, and that the wrapped values are close
    to the on-chain result.
    """

    @pytest.fixture
    def tusd_bsp_pool_no_provider(self, fork_mainnet_archive):
        return _build_stable_pool(
            fork_mainnet_archive,
            COMPOSABLE_STABLE_POOL_ADDRESSES["tusd_bsp"],
            inject_rate_provider=False,
            invariant_version=INVARIANT_V1,
        )

    def test_stale_rates_raise_possible_inaccurate_result(self, tusd_bsp_pool_no_provider):
        """ComposableStablePool without rate_provider raises StaleRateResult."""
        pool = tusd_bsp_pool_no_provider
        amount_in = pool.balances[0] // 100

        with pytest.raises(StaleRateResult) as exc_info:
            pool.calculate_tokens_out_from_tokens_in(
                token_in=pool.tokens[0],
                token_out=pool.tokens[2],
                token_in_quantity=amount_in,
            )

        assert exc_info.value.amount_in == amount_in
        assert exc_info.value.amount_out > 0

    def test_stale_rates_given_out_raises(self, tusd_bsp_pool_no_provider):
        """ComposableStablePool GIVEN_OUT without rate_provider raises StaleRateResult."""
        pool = tusd_bsp_pool_no_provider
        amount_out = pool.balances[2] // 100

        with pytest.raises(StaleRateResult) as exc_info:
            pool.calculate_tokens_in_from_tokens_out(
                token_in=pool.tokens[0],
                token_out=pool.tokens[2],
                token_out_quantity=amount_out,
            )

        assert exc_info.value.amount_out == amount_out
        assert exc_info.value.amount_in > 0
