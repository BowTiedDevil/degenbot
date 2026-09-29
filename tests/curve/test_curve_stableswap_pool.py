"""Curve stableswap pool — fork-integration and recorded-state regressions.

Role split after the golden conversion (survey T8):

- The exact-equality parity loops this module used to run against a live fork
  (``test_tripool``, ``test_base_pool``, ``test_tricrypto_pool`` and the
  metapool parity) are golden record/replay tests in
  ``tests/curve/test_curve_onchain_parity.py`` — replay reads recorded ints
  from ``tests/golden/data/tests/curve/`` and builds pools from the
  ``tests/fixtures/chain_data/1/curve_*`` cassettes, fully offline (see
  ``docs/architecture/golden-onchain-parity.md``).
- This module keeps what legitimately needs a live fork (marked
  ``online_rpc``): pool construction over a live node, pinned-block state
  reads, ``bot.update`` across a fork advance, and the live registry
  discovery sweeps (the design doc's exclusion rule — discovery, not
  regression).
- The A-ramp and metapool base-cache regressions replay the recorded on-chain
  state from cassettes (no fork, no RPC). Re-record a cassette against a live
  fork with ``scripts/record_curve_tripool_cassette.py --block <N>`` when the
  pinned block changes.
"""

import itertools
from typing import cast

import pytest

from degenbot.abi import AbiDecodeError
from degenbot.abi import decode as abi_decode
from degenbot.abi import encode as abi_encode
from degenbot.checksum_cache import get_checksum_address
from degenbot.crypto import function_selector
from degenbot.curve.abi import CURVE_V1_FACTORY_ABI, CURVE_V1_POOL_ABI, CURVE_V1_REGISTRY_ABI
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.exceptions import ContractLogicError
from degenbot.exceptions.arbitrage import NoLiquidity
from degenbot.exceptions.pool import (
    BrokenPool,
    EVMRevertError,
    InvalidSwapInputAmount,
    MissingCurveData,
)
from degenbot.fork import AnvilFork
from degenbot.provider import AlloyProvider
from degenbot.types.rpc_types import TxParams
from tests.curve.test_curve_onchain_parity import (
    _METAPOOL_CASSETTE,
    METAPOOL_PARITY_BLOCK,
    _build_curve_io_free,
    _build_metapool_io_free,
    _load_cassette,
    _metapool_immutable_and_state,
)
from tests.helpers.bot_factory import make_bot_with_provider
from tests.helpers.contract_compat import ContractCompat, make_contract

Timestamp = int

CRYPTO_POOL_ADDRESSES = {"0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5"}
CURVE_V1_FACTORY_ADDRESS = get_checksum_address("0x127db66E7F0b16470Bec194d0f496F9Fa065d0A9")
CURVE_V1_REGISTRY_ADDRESS = get_checksum_address("0x90E00ACe148ca3b23Ac1bC8C240C2a7Dd9c2d7f5")
TRIPOOL_ADDRESS = get_checksum_address("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")

# Cassette recorded from a live fork at the A-ramp regression block
# (scripts/record_curve_tripool_cassette.py --block 14900000).
_A_RAMP_CASSETTE = _METAPOOL_CASSETTE.parent / "curve_tripool_block_14900000.json"


def _build_pool(fork: AnvilFork, address: str) -> CurveStableswapPool:
    """Helper to build a Curve pool using the Bot builder."""
    bot = make_bot_with_provider(fork.provider)
    return bot.build_pool(address)


def _test_calculations(lp: CurveStableswapPool, provider: AlloyProvider):
    state_block = lp.update_block
    contract_compat = ContractCompat(lp.address, CURVE_V1_POOL_ABI, provider)

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
                )
            except (
                InvalidSwapInputAmount,
                NoLiquidity,
                EVMRevertError,
                ContractLogicError,
                AbiDecodeError,
            ):
                continue
            except Exception:
                print(
                    f"Failure simulating swap (in-pool) at block {state_block} for {lp.address}. "
                    f"Reproduce with test_single_pool @ {lp.address}",
                )
                raise

            try:
                if lp.address == "0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5":
                    tx = TxParams(
                        to=lp.address,
                        data=function_selector("get_dy(uint256,uint256,uint256)")
                        + abi_encode(
                            types=["uint256", "uint256", "uint256"],
                            args=[token_in_index, token_out_index, amount],
                        ),
                    )

                    contract_amount, *_ = abi_decode(
                        data=provider.call_raw(tx),
                        types=["uint256"],
                    )
                else:
                    contract_amount = contract_compat.functions.get_dy(
                        token_in_index,
                        token_out_index,
                        amount,
                    ).call()
            except (ContractLogicError, AbiDecodeError):
                # The on-chain contract reverts for broken/unsupported pools
                continue

            assert calc_amount == contract_amount, (
                f"Swap mismatch at block {state_block} for {lp.address}: "
                f"{amount} {token_in} for {token_out} — "
                f"got {calc_amount}, expected {contract_amount}. "
                f"Reproduce with test_single_pool @ {lp.address}"
            )

    if lp.base_pool is not None:
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
                        * lp.base_pool.balances[lp.base_pool.tokens.index(token_in)],
                    )

                try:
                    calc_amount = lp.calculate_tokens_out_from_tokens_in(
                        token_in=token_in,
                        token_out=token_out,
                        token_in_quantity=amount,
                    )
                except (InvalidSwapInputAmount, NoLiquidity, EVMRevertError, ContractLogicError):
                    continue

                try:
                    contract_amount = contract_compat.functions.get_dy_underlying(
                        token_in_index,
                        token_out_index,
                        amount,
                    ).call()
                except (ContractLogicError, AbiDecodeError):
                    continue

                assert calc_amount == contract_amount, (
                    f"Metapool swap mismatch at block {state_block} for {lp.address}: "
                    f"{amount} {token_in} for {token_out} — "
                    f"got {calc_amount}, expected {contract_amount}. "
                    f"Reproduce with test_single_pool @ {lp.address}"
                )


@pytest.mark.online_rpc
def test_create_pool(fork_mainnet_full: AnvilFork):
    """Live construction smoke: the builder assembles TRIPOOL from chain state."""
    _build_pool(fork_mainnet_full, TRIPOOL_ADDRESS)


@pytest.mark.online_rpc
@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [18849426],
    indirect=True,
)
def test_pool_state_at_different_blocks(fork_mainnet_archive: AnvilFork):
    # Build the pool at a known historical block
    block_number = fork_mainnet_archive.provider.get_block_number()

    tripool = _build_pool(fork_mainnet_archive, TRIPOOL_ADDRESS)

    assert fork_mainnet_archive.provider.get_block_number() == block_number
    assert tripool.update_block == block_number

    expected_balances = (75010632422398781503259123, 76382820384826, 34653521595900)
    assert tripool.balances == expected_balances

    fork = AnvilFork(
        fork_url=fork_mainnet_archive.fork_url,
        fork_block=block_number + 1,
    )
    tripool = _build_pool(fork, TRIPOOL_ADDRESS)
    assert tripool.update_block == block_number + 1
    assert tripool.balances == (75010632422398781503259123, 76437030384826, 34599346168546)


@pytest.mark.online_rpc
@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [18849426],
    indirect=True,
)
def test_bot_update_curve_pool(fork_mainnet_archive: AnvilFork):
    """bot.update(pool) fetches fresh balances and applies them via external_update."""
    block_number = fork_mainnet_archive.provider.get_block_number()
    bot = make_bot_with_provider(fork_mainnet_archive.provider)

    tripool = bot.build_pool(TRIPOOL_ADDRESS)
    assert tripool.update_block == block_number
    initial_balances = tripool.balances

    # Advance the fork by one block
    fork = AnvilFork(
        fork_url=fork_mainnet_archive.fork_url,
        fork_block=block_number + 1,
    )
    # Rebuild bot with the advanced fork
    advanced_bot = make_bot_with_provider(fork.provider)

    # The original pool still has old balances
    assert tripool.balances == initial_balances

    # Update the pool via bot.update()
    changed = advanced_bot.update(tripool)
    assert changed is True
    assert tripool.update_block == block_number + 1

    # Verify balances match what a fresh build would give
    fresh_pool = advanced_bot.build_pool(TRIPOOL_ADDRESS)
    assert tripool.balances == fresh_pool.balances

    # Updating again at the same block should return False
    changed = advanced_bot.update(tripool)
    assert changed is False


@pytest.mark.ethereum
def test_a_ramping():
    # A range:      5000 -> 2000
    # A time :      1653559305 -> 1654158027
    # Replayed from the cassette recorded at the pinned block 14_900_000
    # (scripts/record_curve_tripool_cassette.py --block 14900000); the ramp
    # parameters are the on-chain values at that block.
    initial_a = 5000
    final_a = 2000

    initial_a_time = 1653559305
    final_a_time = 1654158027

    tripool = _build_curve_io_free(_load_cassette(_A_RAMP_CASSETTE))
    tripool._create_timestamp = cast("Timestamp", 0)  # defeat the timestamp optimization

    assert tripool._a(timestamp=initial_a_time) == initial_a
    assert tripool._a(timestamp=final_a_time) == final_a
    assert tripool._a(timestamp=(initial_a_time + final_a_time) // 2) == (initial_a + final_a) // 2


@pytest.mark.online_rpc
@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [None],  # Provide block number here if testing against a specific block
    indirect=True,
)
def test_single_pool(
    fork_mainnet_archive: AnvilFork,
):
    pool_address = ""
    if not pool_address:
        return

    lp = _build_pool(fork_mainnet_archive, pool_address)
    _test_calculations(lp=lp, provider=fork_mainnet_archive.provider)


@pytest.mark.ethereum
def test_metapool_with_valid_base_cache():
    """Regression test: virtual_price must resolve correctly when the
    base cache has not expired.

    At block 25144000 the RAI/3Crv metapool's base_cache_updated is only ~180s old
    (vs the 600s expiry), so the contract uses its cached virtual_price. Our pool
    must do the same — PerBlockCache.get_cached_virtual_price() resolves this
    internally without side-effect mirrors.

    Replayed from the recorded cassette at the pinned block (no fork); the
    get_dy/get_dy_underlying parity at this same block is the golden test
    ``test_curve_onchain_parity.py::test_curve_metapool_get_dy``.
    """
    block = METAPOOL_PARITY_BLOCK

    immutable, state = _metapool_immutable_and_state(_load_cassette(_METAPOOL_CASSETTE), block)
    lp = _build_metapool_io_free(immutable, state, block)
    assert lp.update_block == block

    # Verify the base cache has not expired at this block (recorded values)
    block_timestamp = lp._data_provider.block_timestamp(block)
    base_cache_updated = lp._cache.get_cached_base_cache_updated(block)
    assert block_timestamp <= base_cache_updated + lp._cache.BASE_CACHE_EXPIRES


@pytest.mark.online_rpc
def test_factory_stableswap_pools(fork_mainnet_full: AnvilFork):
    """Test the user-deployed pools deployed by the factory"""
    stableswap_factory = ContractCompat(
        CURVE_V1_FACTORY_ADDRESS, CURVE_V1_FACTORY_ABI, fork_mainnet_full.provider
    )
    pool_count = stableswap_factory.functions.pool_count().call()

    pool_addresses = [stableswap_factory.functions.pool_list(i).call() for i in range(pool_count)]

    for i, pool_address in enumerate(pool_addresses, start=1):
        print(f"Testing factory pool {i}/{pool_count} @ {pool_address}")

        try:
            lp = _build_pool(fork_mainnet_full, pool_address)
            _test_calculations(lp=lp, provider=fork_mainnet_full.provider)
        except (BrokenPool, NoLiquidity):
            continue
        except Exception as e:
            block_number = fork_mainnet_full.provider.get_block_number()
            msg = (
                f"{type(e).__name__}: {e} — "
                f"pool {i}/{pool_count} @ {pool_address}, block {block_number}. "
                f'Reproduce: set pool_address="{pool_address}" in test_single_pool '
                f"with @pytest.mark.parametrize('fork_mainnet_archive', [{block_number}], "
                f"indirect=True)"
            )
            print(msg)
            raise AssertionError(msg) from e


@pytest.mark.online_rpc
def test_base_registry_pools(fork_mainnet_full: AnvilFork):
    """Test the custom pools deployed by Curve"""
    registry = make_contract(
        fork_mainnet_full.http_url, CURVE_V1_REGISTRY_ADDRESS, CURVE_V1_REGISTRY_ABI
    )
    pool_count = registry.functions.pool_count().call()

    pool_addresses = [registry.functions.pool_list(i).call() for i in range(pool_count)]

    for i, pool_address in enumerate(pool_addresses, start=1):
        print(f"Testing registry pool {i}/{pool_count} @ {pool_address}")
        try:
            lp = _build_pool(fork_mainnet_full, pool_address)
        except MissingCurveData:
            print("  Skipping pool with missing data")
            continue
        except Exception as e:
            block_number = fork_mainnet_full.provider.get_block_number()
            msg = (
                f"{type(e).__name__}: {e} — "
                f"registry pool {i}/{pool_count} @ {pool_address}, block {block_number}. "
                f'Reproduce: set pool_address="{pool_address}" in test_single_pool '
                f"with @pytest.mark.parametrize('fork_mainnet_archive', [{block_number}], "
                f"indirect=True)"
            )
            print(msg)
            raise AssertionError(msg) from e
        try:
            _test_calculations(lp=lp, provider=fork_mainnet_full.provider)
        except Exception as e:
            block_number = fork_mainnet_full.provider.get_block_number()
            msg = (
                f"{type(e).__name__}: {e} — "
                f"registry pool {i}/{pool_count} @ {pool_address}, block {block_number}. "
                f'Reproduce: set pool_address="{pool_address}" in test_single_pool '
                f"with @pytest.mark.parametrize('fork_mainnet_archive', [{block_number}], "
                f"indirect=True)"
            )
            print(msg)
            raise AssertionError(msg) from e
