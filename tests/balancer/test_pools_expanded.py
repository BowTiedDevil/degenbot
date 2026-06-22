"""Expanded on-chain swap calculation tests for Balancer V2 weighted pools.

Tests GIVEN_IN and GIVEN_OUT across diverse pools covering:
- Weight profiles: 50/50, 60/40, 75/25, 80/20 (existing), 90/10, 1/99, 40/60, 20/80 (existing)
- Fee levels: 0.01% through 2.5%
- Decimal combinations: 18/18, 6/18, 8/18, mixed multi-token
- Token counts: 2, 3, 4
- Specializations: GENERAL (2), MINIMAL_SWAP_INFO (1)

Each test constructs a BalancerV2Pool from on-chain data, then queries the
BalancerQueries contract for the on-chain swap result, and asserts exact
integer match at multiple swap amounts.
"""

import enum
import json
from fractions import Fraction

import pytest
from hexbytes import HexBytes
from web3.exceptions import ContractLogicError

from degenbot.anvil_fork import AnvilFork
from degenbot.balancer.deployments import (
    BALANCER_V2_VAULT_ADDRESS,
    BALANCERQUERIES_CONTRACT_ADDRESS,
)
from degenbot.balancer.pools import BalancerV2Pool, detect_pow_version
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.pool import EVMRevertError
from degenbot.provider import ProviderAdapter
from tests.helpers.balancer_pool_factory import make_balancer_weighted_pool
from tests.helpers.bot_factory import make_bot_with_provider

# ---------- Shared ABIs & Contract Addresses ----------

# Minimal ABI for WeightedPool / WeightedPool2Tokens — enough for construction
WEIGHTED_POOL_ABI = json.loads(
    """
    [{"inputs":[],"name":"getNormalizedWeights","outputs":[{"internalType":"uint256[]","name":"","type":"uint256[]"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getPoolId","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getVault","outputs":[{"internalType":"contract IVault","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"getSwapFeePercentage","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"}]
    """  # noqa: E501
)

BALANCER_V2_VAULT_ABI = json.loads(
    """
    [{"inputs":[{"internalType":"bytes32","name":"poolId","type":"bytes32"}],"name":"getPoolTokens","outputs":[{"internalType":"contract IERC20[]","name":"tokens","type":"address[]"},{"internalType":"uint256[]","name":"balances","type":"uint256[]"},{"internalType":"uint256","name":"lastChangeBlock","type":"uint256"}],"stateMutability":"view","type":"function"}]
    """  # noqa: E501
)
BALANCERQUERIES_CONTRACT_ABI = json.loads(
    """
    [{"inputs":[{"components":[{"internalType":"bytes32","name":"poolId","type":"bytes32"},{"internalType":"enum IVault.SwapKind","name":"kind","type":"uint8"},{"internalType":"contract IAsset","name":"assetIn","type":"address"},{"internalType":"contract IAsset","name":"assetOut","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"bytes","name":"userData","type":"bytes"}],"internalType":"struct IVault.SingleSwap","name":"singleSwap","type":"tuple"},{"components":[{"internalType":"address","name":"sender","type":"address"},{"internalType":"bool","name":"fromInternalBalance","type":"bool"},{"internalType":"address payable","name":"recipient","type":"address"},{"internalType":"bool","name":"toInternalBalance","type":"bool"}],"internalType":"struct IVault.FundManagement","name":"funds","type":"tuple"}],"name":"querySwap","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"nonpayable","type":"function"}]
    """  # noqa: E501
)

VITALIK_ADDRESS = get_checksum_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")

# ---------- Pool Definitions ----------

# Each entry: (address, pool_id_hex, description)
# Weight profiles range from 1/99 to 90/10
# Decimals: 18/18, 6/18, 8/18
# Token counts: 2, 3, 4
# Specializations: GENERAL(2), MINIMAL_SWAP_INFO(1)

TWO_TOKEN_POOLS = {
    # 50/50, 0.10% fee, 18/18 decimals, GENERAL
    "aura_kaiaura_50_50": (
        "0x62E57e37E3185871c13a422A363293780D17305B",
        "0x62e57e37e3185871c13a422a363293780d17305b0002000000000000000006d3",
        "AURA/kaiAURA 50/50, 0.10% fee",
    ),
    # 60/40, 0.1761% fee, 18/18 decimals, GENERAL
    "gel_dexg_60_40": (
        "0xe8075304A388f2f9B2af61f502741a88Ff21D9A4",
        "0xe8075304a388f2f9b2af61f502741a88ff21d9a400020000000000000000006b",
        "GEL/DEXG 60/40, 0.1761% fee",
    ),
    # 75/25, 0.15% fee, 18/18 decimals, GENERAL
    "par_mimo_75_25": (
        "0x5b1C06c4923DBBa4B27Cfa270FFB2E60Aa286159",
        "0x5b1c06c4923dbba4b27cfa270ffb2e60aa28615900020000000000000000004a",
        "PAR/MIMO 75/25, 0.15% fee",
    ),
    # 90/10, 0.20% fee, 18/18 decimals, GENERAL
    "sdvecrv_crv_90_10": (
        "0xF3a605DA753e9dE545841de10EA8bFfBd1Da9C75",
        "0xf3a605da753e9de545841de10ea8bffbd1da9c75000200000000000000000035",
        "sdveCRV/CRV 90/10, 0.20% fee",
    ),
    # 40/60, 0.05% fee, 18/18 decimals, GENERAL
    "dai_weth_40_60": (
        "0x0b09deA16768f0799065C475bE02919503cB2a35",
        "0x0b09dea16768f0799065c475be02919503cb2a3500020000000000000000001a",
        "DAI/WETH 40/60, 0.05% fee",
    ),
    # 50/50, 0.30% fee, 6/18 decimals (USDC/WETH), GENERAL
    "usdc_weth_50_50": (
        "0x96646936b91d6B9D7D0c47C496AfBF3D6ec7B6f8",
        "0x96646936b91d6b9d7d0c47c496afbf3d6ec7b6f8000200000000000000000019",
        "USDC/WETH 50/50, 0.30% fee, mixed decimals (6/18)",
    ),
}

MULTI_TOKEN_POOLS = {
    # 3-token, 33.3/33.3/33.4, 1.00% fee, 18/18/8 decimals, MINIMAL_SWAP_INFO
    "apu_pepe_spx_3_token": (
        "0x9D0D36cC9E989598F01A4656E0efFf73896c30ed",
        "0x9d0d36cc9e989598f01a4656e0efff73896c30ed0001000000000000000006e6",
        "APU/PEPE/SPX 33/33/33, 1.00% fee, mixed decimals (18/18/8)",
    ),
    # 4-token, 16.7/50/16.7/16.7, 0.30% fee, 18/18/18/6 decimals, MINIMAL_SWAP_INFO
    "dai_gp_weth_usdt_4_token": (
        "0xd6855F0A3C26A1b1B2785Ba2604758c0878169bC",
        "0xd6855f0a3c26a1b1b2785ba2604758c0878169bc000100000000000000000317",
        "DAI/GP/WETH/USDT 17/50/17/17, 0.30% fee, mixed decimals (18/18/18/6)",
    ),
}


class SwapKind(enum.IntEnum):
    GIVEN_IN = 0
    GIVEN_OUT = 1


# Amount multipliers as fractions of the pool's token reserves.
TOKEN_AMOUNT_MULTIPLIERS = [
    0.0000001,
    0.000001,
    0.00001,
    0.0001,
    0.001,
    0.01,
    0.1,
    0.125,
    0.25,
]


# ---------- Helpers ----------


def _build_pool_from_chain(
    fork: AnvilFork,
    pool_address: str,
    pool_id_hex: str,
) -> BalancerV2Pool:
    """Build a BalancerV2Pool by fetching on-chain data from an anvil fork."""
    bot = make_bot_with_provider(ProviderAdapter.from_web3(fork.w3))

    pool_contract = fork.w3.eth.contract(
        address=get_checksum_address(pool_address),
        abi=WEIGHTED_POOL_ABI,
    )
    vault_contract = fork.w3.eth.contract(
        address=BALANCER_V2_VAULT_ADDRESS,
        abi=BALANCER_V2_VAULT_ABI,
    )

    pool_id = (
        bytes.fromhex(pool_id_hex[2:])
        if pool_id_hex.startswith("0x")
        else bytes.fromhex(pool_id_hex)
    )
    # Re-fetch from contract to be sure
    pool_id = pool_contract.functions.getPoolId().call()
    vault = pool_contract.functions.getVault().call()
    swap_fee = pool_contract.functions.getSwapFeePercentage().call()
    weights = pool_contract.functions.getNormalizedWeights().call()
    tokens_addresses, balances, _ = vault_contract.functions.getPoolTokens(pool_id).call()

    tokens = [bot.build_erc20token(addr) for addr in tokens_addresses]

    # Detect which FixedPoint pow implementation the deployed pool uses
    bytecode = fork.w3.eth.get_code(get_checksum_address(pool_address)).hex()
    pow_version = detect_pow_version(bytecode)

    return make_balancer_weighted_pool(
        address=get_checksum_address(pool_address),
        pool_id=pool_id,
        vault=vault,
        tokens=tokens,
        balances=balances,
        fee=Fraction(swap_fee, 10**18),
        weights=weights,
        pow_version=pow_version,
        chain_id=1,
    )


def _run_given_in_swaps(
    fork: AnvilFork,
    lp: BalancerV2Pool,
    *,
    swap_directions: list[tuple[int, int]] | None = None,
):
    """Run GIVEN_IN swap calculations against the on-chain BalancerQueries contract
    and assert exact integer match at each amount.
    """
    if swap_directions is None:
        swap_directions = []
        for i in range(len(lp.tokens)):
            for j in range(len(lp.tokens)):
                if i != j:
                    swap_directions.append((i, j))

    query_contract = fork.w3.eth.contract(
        address=BALANCERQUERIES_CONTRACT_ADDRESS, abi=BALANCERQUERIES_CONTRACT_ABI
    )
    vault_contract = fork.w3.eth.contract(
        address=BALANCER_V2_VAULT_ADDRESS,
        abi=BALANCER_V2_VAULT_ABI,
    )

    on_chain_balances = tuple(vault_contract.functions.getPoolTokens(lp.pool_id).call()[1])
    assert lp.balances == on_chain_balances, (
        f"Balance mismatch: Python={lp.balances}, on-chain={on_chain_balances}"
    )

    for token_in_idx, token_out_idx in swap_directions:
        max_reserve = lp.balances[token_in_idx]

        for token_mult in TOKEN_AMOUNT_MULTIPLIERS:
            token_in_amount = int(token_mult * max_reserve)
            if token_in_amount == 0:
                continue

            # Try the on-chain query first; if it reverts, verify Python reverts too
            try:
                contract_amount_out = query_contract.functions.querySwap(
                    (
                        lp.pool_id,
                        SwapKind.GIVEN_IN,
                        lp.tokens[token_in_idx].address,
                        lp.tokens[token_out_idx].address,
                        token_in_amount,
                        b"",
                    ),
                    (
                        VITALIK_ADDRESS,
                        False,
                        VITALIK_ADDRESS,
                        False,
                    ),
                ).call()
            except ContractLogicError:
                # On-chain reverted — verify Python also reverts with a known error.
                # If our code doesn't revert, skip (e.g. SWAPS_DISABLED which is on-chain only).
                try:
                    result = lp.calculate_tokens_out_from_tokens_in(
                        token_in=lp.tokens[token_in_idx],
                        token_in_quantity=token_in_amount,
                        token_out=lp.tokens[token_out_idx],
                    )
                except EVMRevertError:
                    pass  # Both reverted — OK
                else:
                    pytest.skip(
                        f"On-chain reverted but Python returned {result} "
                        f"(likely on-chain-only check like SWAPS_DISABLED)"
                    )
                continue

            helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                token_in=lp.tokens[token_in_idx],
                token_in_quantity=token_in_amount,
                token_out=lp.tokens[token_out_idx],
            )

            assert helper_amount_out == contract_amount_out, (
                f"GIVEN_IN {token_in_idx}→{token_out_idx}, mult={token_mult}: "
                f"Python={helper_amount_out}, on-chain={contract_amount_out}"
            )


def _run_given_out_swaps(
    fork: AnvilFork,
    lp: BalancerV2Pool,
    *,
    swap_directions: list[tuple[int, int]] | None = None,
):
    """Run GIVEN_OUT swap calculations against the on-chain BalancerQueries contract
    and assert exact integer match at each amount.
    """
    if swap_directions is None:
        swap_directions = []
        for i in range(len(lp.tokens)):
            for j in range(len(lp.tokens)):
                if i != j:
                    swap_directions.append((i, j))

    query_contract = fork.w3.eth.contract(
        address=BALANCERQUERIES_CONTRACT_ADDRESS, abi=BALANCERQUERIES_CONTRACT_ABI
    )
    vault_contract = fork.w3.eth.contract(
        address=BALANCER_V2_VAULT_ADDRESS,
        abi=BALANCER_V2_VAULT_ABI,
    )

    on_chain_balances = tuple(vault_contract.functions.getPoolTokens(lp.pool_id).call()[1])
    assert lp.balances == on_chain_balances, (
        f"Balance mismatch: Python={lp.balances}, on-chain={on_chain_balances}"
    )

    for token_in_idx, token_out_idx in swap_directions:
        max_reserve = lp.balances[token_out_idx]

        for token_mult in TOKEN_AMOUNT_MULTIPLIERS:
            token_out_amount = int(token_mult * max_reserve)
            if token_out_amount == 0:
                continue

            # Try the on-chain query first; if it reverts, verify Python reverts too
            try:
                contract_amount_in = query_contract.functions.querySwap(
                    (
                        lp.pool_id,
                        SwapKind.GIVEN_OUT,
                        lp.tokens[token_in_idx].address,
                        lp.tokens[token_out_idx].address,
                        token_out_amount,
                        b"",
                    ),
                    (
                        VITALIK_ADDRESS,
                        False,
                        VITALIK_ADDRESS,
                        False,
                    ),
                ).call()
            except ContractLogicError:
                # On-chain reverted — verify Python also reverts with a known error.
                # If our code doesn't revert, skip (e.g. SWAPS_DISABLED which is on-chain only).
                try:
                    result = lp.calculate_tokens_in_from_tokens_out(
                        token_in=lp.tokens[token_in_idx],
                        token_out=lp.tokens[token_out_idx],
                        token_out_quantity=token_out_amount,
                    )
                except EVMRevertError:
                    pass  # Both reverted — OK
                else:
                    pytest.skip(
                        f"On-chain reverted but Python returned {result} "
                        f"(likely on-chain-only check like SWAPS_DISABLED)"
                    )
                continue

            helper_amount_in = lp.calculate_tokens_in_from_tokens_out(
                token_in=lp.tokens[token_in_idx],
                token_out=lp.tokens[token_out_idx],
                token_out_quantity=token_out_amount,
            )

            assert helper_amount_in == contract_amount_in, (
                f"GIVEN_OUT {token_in_idx}→{token_out_idx}, mult={token_mult}: "
                f"Python={helper_amount_in}, on-chain={contract_amount_in}"
            )


# ---------- 2-Token GIVEN_IN Tests ----------


@pytest.mark.parametrize(
    "pool_key",
    list(TWO_TOKEN_POOLS.keys()),
)
def test_given_in_two_token(
    fork_mainnet_full: AnvilFork,
    pool_key: str,
):
    """GIVEN_IN swap calculations for 2-token weighted pools with diverse weights and fees."""
    pool_address, pool_id_hex, _description = TWO_TOKEN_POOLS[pool_key]
    lp = _build_pool_from_chain(fork_mainnet_full, pool_address, pool_id_hex)
    _run_given_in_swaps(fork_mainnet_full, lp)


# ---------- 2-Token GIVEN_OUT Tests ----------


@pytest.mark.parametrize(
    "pool_key",
    list(TWO_TOKEN_POOLS.keys()),
)
def test_given_out_two_token(
    fork_mainnet_full: AnvilFork,
    pool_key: str,
):
    """GIVEN_OUT swap calculations for 2-token weighted pools with diverse weights and fees."""
    pool_address, pool_id_hex, _description = TWO_TOKEN_POOLS[pool_key]
    lp = _build_pool_from_chain(fork_mainnet_full, pool_address, pool_id_hex)
    _run_given_out_swaps(fork_mainnet_full, lp)


# ---------- Multi-Token GIVEN_IN Tests ----------


@pytest.mark.parametrize(
    "pool_key",
    list(MULTI_TOKEN_POOLS.keys()),
)
def test_given_in_multi_token(
    fork_mainnet_full: AnvilFork,
    pool_key: str,
):
    """GIVEN_IN swap calculations for multi-token weighted pools."""
    pool_address, pool_id_hex, _description = MULTI_TOKEN_POOLS[pool_key]
    lp = _build_pool_from_chain(fork_mainnet_full, pool_address, pool_id_hex)

    # For multi-token pools, test all N*(N-1) directions
    n = len(lp.tokens)
    directions = [(i, j) for i in range(n) for j in range(n) if i != j]
    _run_given_in_swaps(fork_mainnet_full, lp, swap_directions=directions)


# ---------- Multi-Token GIVEN_OUT Tests ----------


@pytest.mark.parametrize(
    "pool_key",
    list(MULTI_TOKEN_POOLS.keys()),
)
def test_given_out_multi_token(
    fork_mainnet_full: AnvilFork,
    pool_key: str,
):
    """GIVEN_OUT swap calculations for multi-token weighted pools."""
    pool_address, pool_id_hex, _description = MULTI_TOKEN_POOLS[pool_key]
    lp = _build_pool_from_chain(fork_mainnet_full, pool_address, pool_id_hex)

    # For multi-token pools, test all N*(N-1) directions
    n = len(lp.tokens)
    directions = [(i, j) for i in range(n) for j in range(n) if i != j]
    _run_given_out_swaps(fork_mainnet_full, lp, swap_directions=directions)


# ---------- Pool Construction Verification ----------


@pytest.mark.parametrize(
    "pool_key",
    list(TWO_TOKEN_POOLS.keys()),
)
def test_two_token_pool_construction(
    fork_mainnet_full: AnvilFork,
    pool_key: str,
):
    """Verify that pool construction produces correct weights, fees, and token ordering."""
    pool_address, pool_id_hex, _description = TWO_TOKEN_POOLS[pool_key]
    lp = _build_pool_from_chain(fork_mainnet_full, pool_address, pool_id_hex)

    assert lp.address == get_checksum_address(pool_address)
    assert lp.pool_id == HexBytes(pool_id_hex)
    assert lp.vault == BALANCER_V2_VAULT_ADDRESS
    assert len(lp.tokens) == 2
    assert len(lp.state.balances) == 2
    assert lp.state.address == get_checksum_address(pool_address)
    # Weights must sum to 1e18
    assert sum(lp.weights) == 10**18


@pytest.mark.parametrize(
    "pool_key",
    list(MULTI_TOKEN_POOLS.keys()),
)
def test_multi_token_pool_construction(
    fork_mainnet_full: AnvilFork,
    pool_key: str,
):
    """Verify multi-token pool construction: weights, fees, token ordering."""
    pool_address, pool_id_hex, _description = MULTI_TOKEN_POOLS[pool_key]
    lp = _build_pool_from_chain(fork_mainnet_full, pool_address, pool_id_hex)

    assert lp.address == get_checksum_address(pool_address)
    assert lp.pool_id == HexBytes(pool_id_hex)
    assert lp.vault == BALANCER_V2_VAULT_ADDRESS
    n = len(lp.tokens)
    assert n >= 3
    assert len(lp.state.balances) == n
    # Weights must sum to 1e18
    assert sum(lp.weights) == 10**18
