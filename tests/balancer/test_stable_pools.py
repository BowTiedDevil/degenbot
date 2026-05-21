"""
On-chain swap calculation tests for Balancer V2 Stable pools.

Tests GIVEN_IN and GIVEN_OUT swap calculations using the StableMath library,
verified against the BalancerQueries contract.

Uses _calculate_invariant_deployed (with round_up=True) which matches the deployed
contract's invariant computation exactly. The deployed MetaStablePool calls
_calculateInvariant with roundUp=True for swaps.

The monorepo's _calculate_invariant (always roundDown) produces invariant values
that differ by ~1 wei from the deployed version, cascading into ~0.01% swap output
error. The deployed version eliminates this by using the same rounding as the
on-chain contract.

Swap fee application order for MetaStablePool and ComposableStablePool:
  GIVEN_IN:  subtractFee → upscale → compute(outGivenIn) → downscaleDown
  GIVEN_OUT: upscale → compute(inGivenOut) → downscaleUp → addFee

ComposableStablePool additional handling:
  - BPT token is included in the pool's token list but must be dropped for invariant calc
  - Token indices must be adjusted to skip the BPT item
  - Virtual BPT supply is computed (total supply - BPT balance in pool) but not needed for
    token-to-token swaps
"""

import json

import pytest
from web3.exceptions import ContractLogicError

from degenbot.anvil_fork import AnvilFork
from degenbot.balancer.deployments import (
    BALANCER_V2_VAULT_ADDRESS,
    BALANCERQUERIES_CONTRACT_ADDRESS,
)
from degenbot.balancer.libraries.fixed_point import ONE
from degenbot.balancer.libraries.stable_math import (
    _calc_in_given_out,
    _calc_out_given_in,
    _calculate_invariant_deployed,
)
from degenbot.checksum_cache import get_checksum_address

# ---------- ABIs ----------

POOL_ABI = json.loads(
    """
    [{"inputs":[],"name":"getAmplificationParameter","outputs":[{"internalType":"uint256","name":"value","type":"uint256"},{"internalType":"bool","name":"isUpdating","type":"bool"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getPoolId","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getSwapFeePercentage","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getScalingFactors","outputs":[{"internalType":"uint256[]","name":"","type":"uint256[]"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getRateProviders","outputs":[{"internalType":"contract IRateProvider[]","name":"","type":"address[]"}],"stateMutability":"view","type":"function"}]
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

RATE_PROVIDER_ABI = json.loads(
    """
    [{"inputs":[],"name":"getRate","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"}]
    """
)

ERC20_ABI = json.loads(
    """
    [{"inputs":[],"name":"decimals","outputs":[{"internalType":"uint8","name":"","type":"uint8"}],"stateMutability":"view","type":"function"}]
    """
)

VITALIK_ADDRESS = get_checksum_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")

# ---------- Pool definitions ----------

# MetaStablePools (all amp=50000, fee=0.04%, GENERAL specialization, 2 tokens)
META_STABLE_POOLS = {
    "wsteth_weth": {
        "address": "0x32296969Ef14EB0c6d29669C550D4a0449130230",
        "description": "wstETH/WETH",
    },
    "cbeth_wsteth": {
        "address": "0x9c6d47Ff73e0F5E51BE5FD53236e3F595C5793F2",
        "description": "cbETH/wstETH",
    },
    "ankreth_weth": {
        "address": "0x8A34b5ad76F528bfEc06c80D85EF3b53dA7FC300",
        "description": "ankrETH/WETH",
    },
    "reth_weth": {
        "address": "0x1E19CF2D73a72Ef1332C882F20534B6519Be0276",
        "description": "rETH/WETH",
    },
}


def _build_pool_data(fork: AnvilFork, pool_address: str) -> dict:
    """Fetch all needed on-chain data for a stable pool."""
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

    # Compute base scaling factors (decimal adjustment only, no rate)
    # and fresh rates from rate providers.
    # The deployed MetaStablePool's _scalingFactors() returns
    # base_sf.mulDown(cached_rate) = (base_sf * cached_rate) // ONE.
    # On querySwap, the cache is refreshed first, then scaling factors
    # use the fresh cached rates. We must replicate this exactly.
    base_scaling_factors = []
    fresh_rates = []
    for i, rp in enumerate(rate_providers):
        erc20 = w3.eth.contract(
            address=get_checksum_address(tokens[i]),
            abi=ERC20_ABI,
        )
        token_decimals = erc20.functions.decimals().call()
        base_sf = ONE * 10 ** (18 - token_decimals)
        base_scaling_factors.append(base_sf)

        if rp != "0x0000000000000000000000000000000000000000":
            rp_contract = w3.eth.contract(
                address=get_checksum_address(rp),
                abi=RATE_PROVIDER_ABI,
            )
            fresh_rates.append(rp_contract.functions.getRate().call())
        else:
            fresh_rates.append(ONE)  # No rate provider → rate = 1

    # Fresh scaling factors = base_sf * rate // ONE (mulDown, matching deployed contract)
    fresh_scaling_factors = [
        b * r // ONE for b, r in zip(base_scaling_factors, fresh_rates, strict=False)
    ]

    # Upscale balances using FRESH scaling factors
    upscaled_balances = [
        b * s // ONE for b, s in zip(balances, fresh_scaling_factors, strict=False)
    ]

    # Compute invariant using deployed version (roundUp=True, matching on-chain)
    invariant = _calculate_invariant_deployed(amp_value, upscaled_balances, round_up=True)

    return {
        "pool_id": pool_id,
        "pool_address": pool_address,
        "amp": amp_value,
        "swap_fee": swap_fee,
        "tokens": tokens,
        "balances": balances,
        "scaling_factors": fresh_scaling_factors,
        "upscaled_balances": upscaled_balances,
        "invariant": invariant,
    }


def _query_swap(  # noqa: PLR0917
    fork: AnvilFork,
    pool_id: bytes,
    token_in: str,
    token_out: str,
    amount: int,
    kind: int,  # 0=GIVEN_IN, 1=GIVEN_OUT
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


def _assert_close(
    python_result: int,
    on_chain_result: int,
    max_wei_diff: int = 3,
    label: str = "",
) -> None:
    """
    Assert that Python and on-chain results are within max_wei_diff wei.

    Used for ComposableStablePools where rate caching can cause small
    differences between our fresh-rate computation and the on-chain result.
    """
    diff = abs(python_result - on_chain_result)
    assert diff <= max_wei_diff, (
        f"{label}Python={python_result}, On-chain={on_chain_result}, "
        f"diff={diff} (max={max_wei_diff})"
    )


def _compute_given_in(
    pool_data: dict,
    token_in_idx: int,
    token_out_idx: int,
    amount_in_raw: int,
) -> int:
    """
    Compute the GIVEN_IN swap result using StableMath.

    MetaStablePool flow (fees before scaling):
    1. Subtract swap fee from raw input
       (feeAmount = amount * fee / ONE, using mulUp; net = amount - feeAmount)
    2. Upscale balances and fee-adjusted input
    3. Compute outGivenIn (in scaled space)
    4. Downscale down the output
    """
    amp = pool_data["amp"]
    sf = pool_data["scaling_factors"]
    swap_fee = pool_data["swap_fee"]
    upscaled = pool_data["upscaled_balances"]
    inv = pool_data["invariant"]

    # Step 1: Subtract fee from raw amount (mulUp for fee amount)
    fee_amount = (amount_in_raw * swap_fee + ONE - 1) // ONE  # mulUp
    amount_after_fee_raw = amount_in_raw - fee_amount

    # Step 2: Upscale fee-adjusted amount
    amount_in_scaled = amount_after_fee_raw * sf[token_in_idx] // ONE  # mulDown

    # Step 3: Compute out given in
    out_scaled = _calc_out_given_in(
        amp, list(upscaled), token_in_idx, token_out_idx, amount_in_scaled, inv
    )

    # Step 4: Downscale down
    return out_scaled * ONE // sf[token_out_idx]  # divDown


def _compute_given_out(
    pool_data: dict,
    token_in_idx: int,
    token_out_idx: int,
    amount_out_raw: int,
) -> int:
    """
    Compute the GIVEN_OUT swap result using StableMath.

    MetaStablePool flow (scaling before fees):
    1. Upscale output amount
    2. Compute inGivenOut (in scaled space)
    3. Downscale UP the input amount
    4. Add swap fee to raw amount (divUp: amount / (1 - fee))
    """
    amp = pool_data["amp"]
    sf = pool_data["scaling_factors"]
    swap_fee = pool_data["swap_fee"]
    upscaled = pool_data["upscaled_balances"]
    inv = pool_data["invariant"]

    # Step 1: Upscale
    amount_out_scaled = amount_out_raw * sf[token_out_idx] // ONE

    # Step 2: Compute in given out
    in_scaled = _calc_in_given_out(
        amp, list(upscaled), token_in_idx, token_out_idx, amount_out_scaled, inv
    )

    # Step 3: Downscale up (round up since it's an amount in)
    in_raw = (in_scaled * ONE + sf[token_in_idx] - 1) // sf[token_in_idx]

    # Step 4: Add fee (deployed contract uses divUp: amount.divUp(ONE - fee))
    numerator = in_raw * ONE
    denominator = ONE - swap_fee
    return numerator // denominator + (1 if numerator % denominator > 0 else 0)


# ---------- Test classes ----------


class TestMetaStablePoolSwaps:
    """
    Test swap calculations against BalancerQueries for MetaStablePools.

    MetaStablePool uses roundUp=True for the invariant during swaps.
    Using _calculate_invariant_deployed(round_up=True) should match on-chain
    exactly for well-balanced pools. For pools with rate caching, there may
    be a small residual error from stale vs fresh rateProvider values.
    """

    @pytest.fixture
    def wsteth_weth_data(self, fork_mainnet_archive):
        return _build_pool_data(fork_mainnet_archive, META_STABLE_POOLS["wsteth_weth"]["address"])

    @pytest.fixture
    def cbeth_wsteth_data(self, fork_mainnet_archive):
        return _build_pool_data(fork_mainnet_archive, META_STABLE_POOLS["cbeth_wsteth"]["address"])

    @pytest.fixture
    def ankreth_weth_data(self, fork_mainnet_archive):
        return _build_pool_data(fork_mainnet_archive, META_STABLE_POOLS["ankreth_weth"]["address"])

    @pytest.fixture
    def reth_weth_data(self, fork_mainnet_archive):
        return _build_pool_data(fork_mainnet_archive, META_STABLE_POOLS["reth_weth"]["address"])

    # --- wstETH/WETH ---

    @pytest.mark.parametrize("pct", [100, 1000, 10000])
    def test_given_in_wsteth_to_weth(self, wsteth_weth_data, fork_mainnet_archive, pct):
        """Test GIVEN_IN swap from wstETH to WETH at various sizes."""
        i_in, i_out = 0, 1
        amount_in = wsteth_weth_data["balances"][i_in] // pct

        python_result = _compute_given_in(wsteth_weth_data, i_in, i_out, amount_in)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            wsteth_weth_data["pool_id"],
            wsteth_weth_data["tokens"][i_in],
            wsteth_weth_data["tokens"][i_out],
            amount_in,
            kind=0,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    @pytest.mark.parametrize("pct", [100, 1000, 10000])
    def test_given_in_weth_to_wsteth(self, wsteth_weth_data, fork_mainnet_archive, pct):
        """Test GIVEN_IN swap from WETH to wstETH at various sizes."""
        i_in, i_out = 1, 0
        amount_in = wsteth_weth_data["balances"][i_in] // pct

        python_result = _compute_given_in(wsteth_weth_data, i_in, i_out, amount_in)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            wsteth_weth_data["pool_id"],
            wsteth_weth_data["tokens"][i_in],
            wsteth_weth_data["tokens"][i_out],
            amount_in,
            kind=0,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    @pytest.mark.parametrize("pct", [100, 1000, 10000])
    def test_given_out_weth_for_wsteth(self, wsteth_weth_data, fork_mainnet_archive, pct):
        """Test GIVEN_OUT swap: requesting WETH out, paying wstETH."""
        i_in, i_out = 0, 1
        amount_out = wsteth_weth_data["balances"][i_out] // pct

        python_result = _compute_given_out(wsteth_weth_data, i_in, i_out, amount_out)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            wsteth_weth_data["pool_id"],
            wsteth_weth_data["tokens"][i_in],
            wsteth_weth_data["tokens"][i_out],
            amount_out,
            kind=1,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    @pytest.mark.parametrize("pct", [100, 1000, 10000])
    def test_given_out_wsteth_for_weth(self, wsteth_weth_data, fork_mainnet_archive, pct):
        """Test GIVEN_OUT swap: requesting wstETH out, paying WETH."""
        i_in, i_out = 1, 0
        amount_out = wsteth_weth_data["balances"][i_out] // pct

        python_result = _compute_given_out(wsteth_weth_data, i_in, i_out, amount_out)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            wsteth_weth_data["pool_id"],
            wsteth_weth_data["tokens"][i_in],
            wsteth_weth_data["tokens"][i_out],
            amount_out,
            kind=1,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    # --- cbETH/wstETH ---

    @pytest.mark.parametrize("pct", [100, 1000, 10000])
    def test_given_in_cbeth_to_wsteth(self, cbeth_wsteth_data, fork_mainnet_archive, pct):
        """Test GIVEN_IN swap from cbETH to wstETH."""
        i_in, i_out = 0, 1
        amount_in = cbeth_wsteth_data["balances"][i_in] // pct

        python_result = _compute_given_in(cbeth_wsteth_data, i_in, i_out, amount_in)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            cbeth_wsteth_data["pool_id"],
            cbeth_wsteth_data["tokens"][i_in],
            cbeth_wsteth_data["tokens"][i_out],
            amount_in,
            kind=0,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    @pytest.mark.parametrize("pct", [100, 1000, 10000])
    def test_given_in_wsteth_to_cbeth(self, cbeth_wsteth_data, fork_mainnet_archive, pct):
        """Test GIVEN_IN swap from wstETH to cbETH."""
        i_in, i_out = 1, 0
        amount_in = cbeth_wsteth_data["balances"][i_in] // pct

        python_result = _compute_given_in(cbeth_wsteth_data, i_in, i_out, amount_in)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            cbeth_wsteth_data["pool_id"],
            cbeth_wsteth_data["tokens"][i_in],
            cbeth_wsteth_data["tokens"][i_out],
            amount_in,
            kind=0,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_wsteth_for_cbeth(self, cbeth_wsteth_data, fork_mainnet_archive, pct):
        """Test GIVEN_OUT swap: requesting wstETH out, paying cbETH."""
        i_in, i_out = 0, 1
        amount_out = cbeth_wsteth_data["balances"][i_out] // pct

        python_result = _compute_given_out(cbeth_wsteth_data, i_in, i_out, amount_out)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            cbeth_wsteth_data["pool_id"],
            cbeth_wsteth_data["tokens"][i_in],
            cbeth_wsteth_data["tokens"][i_out],
            amount_out,
            kind=1,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    @pytest.mark.parametrize("pct", [100, 1000, 10000])
    def test_given_out_cbeth_for_wsteth(self, cbeth_wsteth_data, fork_mainnet_archive, pct):
        """Test GIVEN_OUT swap: requesting cbETH out, paying wstETH."""
        i_in, i_out = 1, 0
        amount_out = cbeth_wsteth_data["balances"][i_out] // pct

        python_result = _compute_given_out(cbeth_wsteth_data, i_in, i_out, amount_out)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            cbeth_wsteth_data["pool_id"],
            cbeth_wsteth_data["tokens"][i_in],
            cbeth_wsteth_data["tokens"][i_out],
            amount_out,
            kind=1,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    # --- rETH/WETH ---

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_reth_to_weth(self, reth_weth_data, fork_mainnet_archive, pct):
        """Test GIVEN_IN swap from rETH to WETH."""
        i_in, i_out = 0, 1
        amount_in = reth_weth_data["balances"][i_in] // pct

        python_result = _compute_given_in(reth_weth_data, i_in, i_out, amount_in)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            reth_weth_data["pool_id"],
            reth_weth_data["tokens"][i_in],
            reth_weth_data["tokens"][i_out],
            amount_in,
            kind=0,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_weth_to_reth(self, reth_weth_data, fork_mainnet_archive, pct):
        """Test GIVEN_IN swap from WETH to rETH."""
        i_in, i_out = 1, 0
        amount_in = reth_weth_data["balances"][i_in] // pct

        python_result = _compute_given_in(reth_weth_data, i_in, i_out, amount_in)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            reth_weth_data["pool_id"],
            reth_weth_data["tokens"][i_in],
            reth_weth_data["tokens"][i_out],
            amount_in,
            kind=0,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_weth_for_reth(self, reth_weth_data, fork_mainnet_archive, pct):
        """Test GIVEN_OUT swap: requesting WETH out, paying rETH."""
        i_in, i_out = 0, 1
        amount_out = reth_weth_data["balances"][i_out] // pct

        python_result = _compute_given_out(reth_weth_data, i_in, i_out, amount_out)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            reth_weth_data["pool_id"],
            reth_weth_data["tokens"][i_in],
            reth_weth_data["tokens"][i_out],
            amount_out,
            kind=1,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_reth_for_weth(self, reth_weth_data, fork_mainnet_archive, pct):
        """Test GIVEN_OUT swap: requesting rETH out, paying WETH."""
        i_in, i_out = 1, 0
        amount_out = reth_weth_data["balances"][i_out] // pct

        python_result = _compute_given_out(reth_weth_data, i_in, i_out, amount_out)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            reth_weth_data["pool_id"],
            reth_weth_data["tokens"][i_in],
            reth_weth_data["tokens"][i_out],
            amount_out,
            kind=1,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    # --- ankrETH/WETH ---

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_ankreth_to_weth(self, ankreth_weth_data, fork_mainnet_archive, pct):
        """Test GIVEN_IN swap from ankrETH to WETH."""
        i_in, i_out = 0, 1
        amount_in = ankreth_weth_data["balances"][i_in] // pct

        python_result = _compute_given_in(ankreth_weth_data, i_in, i_out, amount_in)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            ankreth_weth_data["pool_id"],
            ankreth_weth_data["tokens"][i_in],
            ankreth_weth_data["tokens"][i_out],
            amount_in,
            kind=0,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_weth_to_ankreth(self, ankreth_weth_data, fork_mainnet_archive, pct):
        """Test GIVEN_IN swap from WETH to ankrETH."""
        i_in, i_out = 1, 0
        amount_in = ankreth_weth_data["balances"][i_in] // pct

        python_result = _compute_given_in(ankreth_weth_data, i_in, i_out, amount_in)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            ankreth_weth_data["pool_id"],
            ankreth_weth_data["tokens"][i_in],
            ankreth_weth_data["tokens"][i_out],
            amount_in,
            kind=0,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_weth_for_ankreth(self, ankreth_weth_data, fork_mainnet_archive, pct):
        """Test GIVEN_OUT swap: requesting WETH out, paying ankrETH."""
        i_in, i_out = 0, 1
        amount_out = ankreth_weth_data["balances"][i_out] // pct

        python_result = _compute_given_out(ankreth_weth_data, i_in, i_out, amount_out)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            ankreth_weth_data["pool_id"],
            ankreth_weth_data["tokens"][i_in],
            ankreth_weth_data["tokens"][i_out],
            amount_out,
            kind=1,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_ankreth_for_weth(self, ankreth_weth_data, fork_mainnet_archive, pct):
        """Test GIVEN_OUT swap: requesting ankrETH out, paying WETH."""
        i_in, i_out = 1, 0
        amount_out = ankreth_weth_data["balances"][i_out] // pct

        python_result = _compute_given_out(ankreth_weth_data, i_in, i_out, amount_out)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            ankreth_weth_data["pool_id"],
            ankreth_weth_data["tokens"][i_in],
            ankreth_weth_data["tokens"][i_out],
            amount_out,
            kind=1,
        )

        if on_chain_result > 0:
            assert python_result == on_chain_result, (
                f"pct=1/{pct}: Python={python_result}, On-chain={on_chain_result}, "
                f"diff={python_result - on_chain_result}"
            )


# ---------- ComposableStablePool ----------

# ComposableStablePools have BPT as one of the tokens. The BPT item must be
# dropped before computing the invariant and swap amounts.
COMPOSABLE_STABLE_POOLS = {
    "bb_s_usd": {
        "address": "0x779d01F939D78a918A3de18cC236ee89221dfd4E",
        "description": "bb-s-USD (BPT + bb-s-USDC/USDT/DAI)",
    },
    "tusd_bsp": {
        "address": "0x53BC3cBa3832ebeCBFa002c12023F8ab1AA3a3a0",
        "description": "TUSD BSP (TUSD + BPT + USDC)",
    },
    "usdc_usdt": {
        "address": "0x652D486B80C461C397B0d95612A404da936f3db3",
        "description": "bb-a-USDC+USDT (BPT + USDC + USDT)",
    },
}


def _build_composable_pool_data(fork: AnvilFork, pool_address: str) -> dict:
    """
    Fetch on-chain data for a ComposableStablePool.

    Handles BPT dropping: identifies the BPT token (same address as pool),
    drops it from balances before computing the invariant.
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

    # Compute base scaling factors (decimal adjustment only, no rate)
    # and fresh rates from rate providers.
    # The deployed ComposableStablePool's _scalingFactors() returns
    # base_sf.mulDown(cached_rate) = (base_sf * cached_rate) // ONE.
    # On querySwap, the cache is refreshed first, then scaling factors
    # use the fresh cached rates. We must replicate this exactly.
    base_scaling_factors = []
    fresh_rates = []
    for i, rp in enumerate(rate_providers):
        # Base scaling factor from token decimals
        if tokens[i].lower() == pool_address.lower():
            # BPT always has 18 decimals → base_sf = 1e18
            base_sf = ONE
        else:
            erc20 = w3.eth.contract(
                address=get_checksum_address(tokens[i]),
                abi=ERC20_ABI,
            )
            token_decimals = erc20.functions.decimals().call()
            base_sf = ONE * 10 ** (18 - token_decimals)

        base_scaling_factors.append(base_sf)

        if rp != "0x0000000000000000000000000000000000000000":
            rp_contract = w3.eth.contract(
                address=get_checksum_address(rp),
                abi=RATE_PROVIDER_ABI,
            )
            fresh_rates.append(rp_contract.functions.getRate().call())
        else:
            fresh_rates.append(ONE)  # No rate provider → rate = 1 (for BPT)

    # Build fresh scaling factors = base_sf * rate // ONE (mulDown, matching deployed contract)
    fresh_scaling_factors = [
        b * r // ONE for b, r in zip(base_scaling_factors, fresh_rates, strict=False)
    ]

    # Find BPT index (token whose address matches the pool)
    bpt_idx = next(i for i, t in enumerate(tokens) if t.lower() == pool_address.lower())

    # Upscale all balances
    upscaled_balances_with_bpt = [
        b * s // ONE for b, s in zip(balances, fresh_scaling_factors, strict=False)
    ]

    # Drop BPT item
    non_bpt_indices = [i for i in range(len(tokens)) if i != bpt_idx]
    upscaled_balances = [upscaled_balances_with_bpt[i] for i in non_bpt_indices]
    adjusted_sf = [fresh_scaling_factors[i] for i in non_bpt_indices]
    adjusted_tokens = [tokens[i] for i in non_bpt_indices]
    adjusted_balances = [balances[i] for i in non_bpt_indices]

    # Compute invariant on non-BPT balances with round_up=True
    invariant = _calculate_invariant_deployed(amp_value, upscaled_balances, round_up=True)

    return {
        "pool_id": pool_id,
        "pool_address": pool_address,
        "amp": amp_value,
        "swap_fee": swap_fee,
        "tokens": tokens,
        "balances": balances,
        "scaling_factors": fresh_scaling_factors,
        "bpt_idx": bpt_idx,
        # Adjusted views (BPT dropped)
        "non_bpt_indices": non_bpt_indices,
        "adjusted_tokens": adjusted_tokens,
        "adjusted_balances": adjusted_balances,
        "adjusted_scaling_factors": adjusted_sf,
        "upscaled_balances": upscaled_balances,
        "invariant": invariant,
    }


def _compute_composable_given_in(
    pool_data: dict,
    token_in_idx: int,
    token_out_idx: int,
    amount_in_raw: int,
) -> int:
    """
    Compute GIVEN_IN swap for a ComposableStablePool (token-to-token swap).

    Flow (same as MetaStablePool but with BPT dropping):
    1. Subtract swap fee from raw input
    2. Upscale balances and fee-adjusted input (drop BPT)
    3. Compute outGivenIn (in scaled space, using adjusted indices)
    4. Downscale down the output
    """
    amp = pool_data["amp"]
    sf = pool_data["scaling_factors"]
    swap_fee = pool_data["swap_fee"]
    upscaled = pool_data["upscaled_balances"]
    inv = pool_data["invariant"]
    non_bpt_indices = pool_data["non_bpt_indices"]

    # Step 1: Subtract fee
    fee_amount = (amount_in_raw * swap_fee + ONE - 1) // ONE  # mulUp
    amount_after_fee_raw = amount_in_raw - fee_amount

    # Step 2: Upscale fee-adjusted amount
    amount_in_scaled = amount_after_fee_raw * sf[token_in_idx] // ONE

    # Step 3: Adjust indices (skip BPT)
    adjusted_in = non_bpt_indices.index(token_in_idx)
    adjusted_out = non_bpt_indices.index(token_out_idx)

    out_scaled = _calc_out_given_in(
        amp, list(upscaled), adjusted_in, adjusted_out, amount_in_scaled, inv
    )

    # Step 4: Downscale down
    return out_scaled * ONE // sf[token_out_idx]  # divDown


def _compute_composable_given_out(
    pool_data: dict,
    token_in_idx: int,
    token_out_idx: int,
    amount_out_raw: int,
) -> int:
    """
    Compute GIVEN_OUT swap for a ComposableStablePool (token-to-token swap).

    Flow (same as MetaStablePool but with BPT dropping):
    1. Upscale output amount
    2. Compute inGivenOut (in scaled space, using adjusted indices)
    3. Downscale up the input amount
    4. Add swap fee to raw amount
    """
    amp = pool_data["amp"]
    sf = pool_data["scaling_factors"]
    swap_fee = pool_data["swap_fee"]
    upscaled = pool_data["upscaled_balances"]
    inv = pool_data["invariant"]
    non_bpt_indices = pool_data["non_bpt_indices"]

    # Step 1: Upscale
    amount_out_scaled = amount_out_raw * sf[token_out_idx] // ONE

    # Step 2: Adjust indices and compute
    adjusted_in = non_bpt_indices.index(token_in_idx)
    adjusted_out = non_bpt_indices.index(token_out_idx)

    in_scaled = _calc_in_given_out(
        amp, list(upscaled), adjusted_in, adjusted_out, amount_out_scaled, inv
    )

    # Step 3: Downscale up
    in_raw = (in_scaled * ONE + sf[token_in_idx] - 1) // sf[token_in_idx]

    # Step 4: Add fee (divUp: amount / (1 - fee))
    numerator = in_raw * ONE
    denominator = ONE - swap_fee
    return numerator // denominator + (1 if numerator % denominator > 0 else 0)


class TestComposableStablePoolSwaps:
    """
    Test swap calculations against BalancerQueries for ComposableStablePools.

    ComposableStablePools include the BPT token in the pool's token list.
    For token-to-token swaps, the BPT is dropped before computing the invariant
    and swap amounts. The deployed contract uses roundUp=True for the invariant.

    Rate caching means exact integer matching may not be achievable — the on-chain
    swap refreshes rate caches before computing, and our off-chain computation uses
    rates fetched separately. A small tolerance (≤3 wei) is used for assertions.
    """

    @pytest.fixture
    def bb_s_usd_data(self, fork_mainnet_archive):
        return _build_composable_pool_data(
            fork_mainnet_archive,
            COMPOSABLE_STABLE_POOLS["bb_s_usd"]["address"],
        )

    @pytest.fixture
    def tusd_bsp_data(self, fork_mainnet_archive):
        return _build_composable_pool_data(
            fork_mainnet_archive,
            COMPOSABLE_STABLE_POOLS["tusd_bsp"]["address"],
        )

    @pytest.fixture
    def usdc_usdt_data(self, fork_mainnet_archive):
        return _build_composable_pool_data(
            fork_mainnet_archive,
            COMPOSABLE_STABLE_POOLS["usdc_usdt"]["address"],
        )

    # --- bb-s-USD (4 tokens: BPT + bb-s-USDC + bb-s-USDT + bb-s-DAI) ---

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_usdc_to_usdt(self, bb_s_usd_data, fork_mainnet_archive, pct):
        """Test GIVEN_IN swap from bb-s-USDC to bb-s-USDT."""
        i_in, i_out = 1, 2  # token indices in the full token list
        amount_in = bb_s_usd_data["balances"][i_in] // pct

        python_result = _compute_composable_given_in(bb_s_usd_data, i_in, i_out, amount_in)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            bb_s_usd_data["pool_id"],
            bb_s_usd_data["tokens"][i_in],
            bb_s_usd_data["tokens"][i_out],
            amount_in,
            kind=0,
        )

        if on_chain_result > 0:
            _assert_close(
                python_result,
                on_chain_result,
                label=f"pct=1/{pct}: ",
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_usdt_to_dai(self, bb_s_usd_data, fork_mainnet_archive, pct):
        """Test GIVEN_IN swap from bb-s-USDT to bb-s-DAI."""
        i_in, i_out = 2, 3
        amount_in = bb_s_usd_data["balances"][i_in] // pct

        python_result = _compute_composable_given_in(bb_s_usd_data, i_in, i_out, amount_in)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            bb_s_usd_data["pool_id"],
            bb_s_usd_data["tokens"][i_in],
            bb_s_usd_data["tokens"][i_out],
            amount_in,
            kind=0,
        )

        if on_chain_result > 0:
            _assert_close(
                python_result,
                on_chain_result,
                label=f"pct=1/{pct}: ",
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_usdc_for_usdt(self, bb_s_usd_data, fork_mainnet_archive, pct):
        """Test GIVEN_OUT swap: requesting bb-s-USDC out, paying bb-s-USDT."""
        i_in, i_out = 2, 1  # USDT in, USDC out
        amount_out = bb_s_usd_data["balances"][i_out] // pct

        python_result = _compute_composable_given_out(bb_s_usd_data, i_in, i_out, amount_out)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            bb_s_usd_data["pool_id"],
            bb_s_usd_data["tokens"][i_in],
            bb_s_usd_data["tokens"][i_out],
            amount_out,
            kind=1,
        )

        if on_chain_result > 0:
            _assert_close(
                python_result,
                on_chain_result,
                label=f"pct=1/{pct}: ",
            )

    # --- TUSD BSP (3 tokens: TUSD + BPT + USDC) ---

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_tusd_to_usdc(self, tusd_bsp_data, fork_mainnet_archive, pct):
        """Test GIVEN_IN swap from TUSD to USDC."""
        i_in, i_out = 0, 2  # BPT is at index 1, so non-BPT tokens are 0 and 2
        amount_in = tusd_bsp_data["balances"][i_in] // pct

        python_result = _compute_composable_given_in(tusd_bsp_data, i_in, i_out, amount_in)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            tusd_bsp_data["pool_id"],
            tusd_bsp_data["tokens"][i_in],
            tusd_bsp_data["tokens"][i_out],
            amount_in,
            kind=0,
        )

        if on_chain_result > 0:
            _assert_close(
                python_result,
                on_chain_result,
                label=f"pct=1/{pct}: ",
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_usdc_for_tusd(self, tusd_bsp_data, fork_mainnet_archive, pct):
        """Test GIVEN_OUT swap: requesting USDC out for TUSD."""
        i_in, i_out = 0, 2  # BPT is at index 1
        amount_out = tusd_bsp_data["balances"][i_out] // pct

        python_result = _compute_composable_given_out(tusd_bsp_data, i_in, i_out, amount_out)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            tusd_bsp_data["pool_id"],
            tusd_bsp_data["tokens"][i_in],
            tusd_bsp_data["tokens"][i_out],
            amount_out,
            kind=1,
        )

        if on_chain_result > 0:
            _assert_close(
                python_result,
                on_chain_result,
                label=f"pct=1/{pct}: ",
            )

    # --- bb-a-USDC+USDT (3 tokens: BPT + USDC + USDT) ---

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_in_usdc_to_usdt_plain(self, usdc_usdt_data, fork_mainnet_archive, pct):
        """Test GIVEN_IN swap from USDC to USDT in bb-a-USDC+USDT pool."""
        i_in, i_out = 1, 2  # BPT is at index 0
        amount_in = usdc_usdt_data["balances"][i_in] // pct

        python_result = _compute_composable_given_in(usdc_usdt_data, i_in, i_out, amount_in)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            usdc_usdt_data["pool_id"],
            usdc_usdt_data["tokens"][i_in],
            usdc_usdt_data["tokens"][i_out],
            amount_in,
            kind=0,
        )

        if on_chain_result > 0:
            _assert_close(
                python_result,
                on_chain_result,
                label=f"pct=1/{pct}: ",
            )

    @pytest.mark.parametrize("pct", [100, 1000])
    def test_given_out_usdt_for_usdc_plain(self, usdc_usdt_data, fork_mainnet_archive, pct):
        """Test GIVEN_OUT swap: requesting USDT out for USDC."""
        i_in, i_out = 1, 2  # USDC in, USDT out; BPT at index 0
        amount_out = usdc_usdt_data["balances"][i_out] // pct

        python_result = _compute_composable_given_out(usdc_usdt_data, i_in, i_out, amount_out)
        on_chain_result = _query_swap(
            fork_mainnet_archive,
            usdc_usdt_data["pool_id"],
            usdc_usdt_data["tokens"][i_in],
            usdc_usdt_data["tokens"][i_out],
            amount_out,
            kind=1,
        )

        if on_chain_result > 0:
            _assert_close(
                python_result,
                on_chain_result,
                label=f"pct=1/{pct}: ",
            )
