import dataclasses
import pathlib
from typing import TYPE_CHECKING, Any, cast

import pydantic_core
import pytest
from hexbytes import HexBytes
from web3.exceptions import ContractLogicError

from degenbot import pool_registry
from degenbot.anvil_fork import AnvilFork
from degenbot.cache import get_checksum_address
from degenbot.config import set_web3
from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions import IncompleteSwap, LiquidityPoolError, PossibleInaccurateResult
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

if TYPE_CHECKING:
    from eth_typing import HexStr

USDC_CONTRACT_ADDRESS = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
NATIVE_CURRENCY_ADDRESS = ZERO_ADDRESS
ETH_USDC_V4_POOL_ID = cast(
    "HexStr", "0x21c67e77068de97969ba93d4aab21826d33ca12bb9f565d8496e8fda8a82ca27"
)
ETH_USDC_V4_POOL_FEE = 500
ETH_USDC_V4_POOL_TICK_SPACING = 10
V4_POOL_MANAGER_ADDRESS = get_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90")
STATE_VIEW_ADDRESS = get_checksum_address("0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227")

UNISWAP_V4_QUOTER_ADDRESS = get_checksum_address("0x52F0E24D1c21C8A0cB1e5a5dD6198556BD9E1203")
UNISWAP_V4_QUOTER_ABI = """
[{"inputs":[{"internalType":"contract IPoolManager","name":"_poolManager","type":"address"}],"stateMutability":"nonpayable","type":"constructor"},{"inputs":[{"internalType":"PoolId","name":"poolId","type":"bytes32"}],"name":"NotEnoughLiquidity","type":"error"},{"inputs":[],"name":"NotPoolManager","type":"error"},{"inputs":[],"name":"NotSelf","type":"error"},{"inputs":[{"internalType":"uint256","name":"amount","type":"uint256"}],"name":"QuoteSwap","type":"error"},{"inputs":[],"name":"UnexpectedCallSuccess","type":"error"},{"inputs":[{"internalType":"bytes","name":"revertData","type":"bytes"}],"name":"UnexpectedRevertBytes","type":"error"},{"inputs":[{"components":[{"internalType":"Currency","name":"exactCurrency","type":"address"},{"components":[{"internalType":"Currency","name":"intermediateCurrency","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"contract IHooks","name":"hooks","type":"address"},{"internalType":"bytes","name":"hookData","type":"bytes"}],"internalType":"struct PathKey[]","name":"path","type":"tuple[]"},{"internalType":"uint128","name":"exactAmount","type":"uint128"}],"internalType":"struct IV4Quoter.QuoteExactParams","name":"params","type":"tuple"}],"name":"_quoteExactInput","outputs":[{"internalType":"bytes","name":"","type":"bytes"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"components":[{"internalType":"Currency","name":"currency0","type":"address"},{"internalType":"Currency","name":"currency1","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"contract IHooks","name":"hooks","type":"address"}],"internalType":"struct PoolKey","name":"poolKey","type":"tuple"},{"internalType":"bool","name":"zeroForOne","type":"bool"},{"internalType":"uint128","name":"exactAmount","type":"uint128"},{"internalType":"bytes","name":"hookData","type":"bytes"}],"internalType":"struct IV4Quoter.QuoteExactSingleParams","name":"params","type":"tuple"}],"name":"_quoteExactInputSingle","outputs":[{"internalType":"bytes","name":"","type":"bytes"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"internalType":"Currency","name":"exactCurrency","type":"address"},{"components":[{"internalType":"Currency","name":"intermediateCurrency","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"contract IHooks","name":"hooks","type":"address"},{"internalType":"bytes","name":"hookData","type":"bytes"}],"internalType":"struct PathKey[]","name":"path","type":"tuple[]"},{"internalType":"uint128","name":"exactAmount","type":"uint128"}],"internalType":"struct IV4Quoter.QuoteExactParams","name":"params","type":"tuple"}],"name":"_quoteExactOutput","outputs":[{"internalType":"bytes","name":"","type":"bytes"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"components":[{"internalType":"Currency","name":"currency0","type":"address"},{"internalType":"Currency","name":"currency1","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"contract IHooks","name":"hooks","type":"address"}],"internalType":"struct PoolKey","name":"poolKey","type":"tuple"},{"internalType":"bool","name":"zeroForOne","type":"bool"},{"internalType":"uint128","name":"exactAmount","type":"uint128"},{"internalType":"bytes","name":"hookData","type":"bytes"}],"internalType":"struct IV4Quoter.QuoteExactSingleParams","name":"params","type":"tuple"}],"name":"_quoteExactOutputSingle","outputs":[{"internalType":"bytes","name":"","type":"bytes"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[],"name":"poolManager","outputs":[{"internalType":"contract IPoolManager","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"components":[{"internalType":"Currency","name":"exactCurrency","type":"address"},{"components":[{"internalType":"Currency","name":"intermediateCurrency","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"contract IHooks","name":"hooks","type":"address"},{"internalType":"bytes","name":"hookData","type":"bytes"}],"internalType":"struct PathKey[]","name":"path","type":"tuple[]"},{"internalType":"uint128","name":"exactAmount","type":"uint128"}],"internalType":"struct IV4Quoter.QuoteExactParams","name":"params","type":"tuple"}],"name":"quoteExactInput","outputs":[{"internalType":"uint256","name":"amountOut","type":"uint256"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"components":[{"internalType":"Currency","name":"currency0","type":"address"},{"internalType":"Currency","name":"currency1","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"contract IHooks","name":"hooks","type":"address"}],"internalType":"struct PoolKey","name":"poolKey","type":"tuple"},{"internalType":"bool","name":"zeroForOne","type":"bool"},{"internalType":"uint128","name":"exactAmount","type":"uint128"},{"internalType":"bytes","name":"hookData","type":"bytes"}],"internalType":"struct IV4Quoter.QuoteExactSingleParams","name":"params","type":"tuple"}],"name":"quoteExactInputSingle","outputs":[{"internalType":"uint256","name":"amountOut","type":"uint256"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"internalType":"Currency","name":"exactCurrency","type":"address"},{"components":[{"internalType":"Currency","name":"intermediateCurrency","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"contract IHooks","name":"hooks","type":"address"},{"internalType":"bytes","name":"hookData","type":"bytes"}],"internalType":"struct PathKey[]","name":"path","type":"tuple[]"},{"internalType":"uint128","name":"exactAmount","type":"uint128"}],"internalType":"struct IV4Quoter.QuoteExactParams","name":"params","type":"tuple"}],"name":"quoteExactOutput","outputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"components":[{"internalType":"Currency","name":"currency0","type":"address"},{"internalType":"Currency","name":"currency1","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"contract IHooks","name":"hooks","type":"address"}],"internalType":"struct PoolKey","name":"poolKey","type":"tuple"},{"internalType":"bool","name":"zeroForOne","type":"bool"},{"internalType":"uint128","name":"exactAmount","type":"uint128"},{"internalType":"bytes","name":"hookData","type":"bytes"}],"internalType":"struct IV4Quoter.QuoteExactSingleParams","name":"params","type":"tuple"}],"name":"quoteExactOutputSingle","outputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes","name":"data","type":"bytes"}],"name":"unlockCallback","outputs":[{"internalType":"bytes","name":"","type":"bytes"}],"stateMutability":"nonpayable","type":"function"}]
"""  # noqa:E501

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
]


@pytest.fixture
def eth_usdc_v4(fork_mainnet_full: AnvilFork) -> UniswapV4Pool:
    set_web3(fork_mainnet_full.w3)

    if (
        pool := pool_registry.get(
            chain_id=fork_mainnet_full.w3.eth.chain_id,
            pool_address=V4_POOL_MANAGER_ADDRESS,
            pool_id=ETH_USDC_V4_POOL_ID,
        )
    ) is None:
        return UniswapV4Pool(
            pool_id=ETH_USDC_V4_POOL_ID,
            pool_manager_address=V4_POOL_MANAGER_ADDRESS,
            state_view_address=STATE_VIEW_ADDRESS,
            tokens=[USDC_CONTRACT_ADDRESS, NATIVE_CURRENCY_ADDRESS],
            fee=ETH_USDC_V4_POOL_FEE,
            tick_spacing=ETH_USDC_V4_POOL_TICK_SPACING,
        )

    assert isinstance(pool, UniswapV4Pool)
    return pool


@pytest.fixture
def testing_pools() -> list[dict[str, Any]]:
    pools = pydantic_core.from_json(
        pathlib.Path("tests/uniswap/v4/first_200_uniswap_v4_pools.json").read_bytes()
    )
    assert len(pools) == 200
    return pools


@pytest.fixture
def liquidity_snapshot() -> dict[str, Any]:
    snapshot: dict[str, Any] = pydantic_core.from_json(
        pathlib.Path("tests/uniswap/v4/mainnet_v4_liquidity_snapshot.json").read_bytes()
    )

    return snapshot


def _test_pool_exact_input(
    pool: dict[str, Any],
    fork: AnvilFork,
    snapshot: dict[str, Any] | None = None,
):
    quoter = fork.w3.eth.contract(address=UNISWAP_V4_QUOTER_ADDRESS, abi=UNISWAP_V4_QUOTER_ABI)

    pool_id: HexStr = pool["pool_id"]

    lp = pool_registry.get(
        chain_id=fork.w3.eth.chain_id,
        pool_address=V4_POOL_MANAGER_ADDRESS,
        pool_id=pool_id,
    )
    if lp is None:
        try:
            lp = UniswapV4Pool(
                pool_id=pool_id,
                pool_manager_address=V4_POOL_MANAGER_ADDRESS,
                tokens=(
                    pool["token0"],
                    pool["token1"],
                ),
                fee=pool["fee"],
                tick_spacing=pool["tick_spacing"],
                hook_address=pool["hooks"],
                state_view_address=STATE_VIEW_ADDRESS,
                tick_bitmap=snapshot[pool_id]["tick_bitmap"]
                if snapshot is not None and pool_id in snapshot
                else None,
                tick_data=snapshot[pool_id]["tick_data"]
                if snapshot is not None and pool_id in snapshot
                else None,
            )
        except Exception as exc:
            print(f"Caught exception {exc} building pool id {pool['pool_id']}")
            raise

    assert isinstance(lp, UniswapV4Pool)

    max_reserves_token0 = 1 * 10**lp.token0.decimals
    max_reserves_token1 = 1 * 10**lp.token1.decimals

    if max_reserves_token0 == 0 or max_reserves_token1 == 0:
        return

    for token_mult in TOKEN_AMOUNT_MULTIPLIERS:
        token0_in_amount = max(1, int(token_mult * max_reserves_token0))

        try:
            quoter_amount_out, _ = quoter.functions.quoteExactInputSingle(
                (
                    dataclasses.astuple(lp.pool_key),  # poolKey
                    True,  # zeroForOne
                    token0_in_amount,  # exactAmount
                    b"",  # hookData
                )
            ).call()
        except ContractLogicError:
            continue

        if quoter_amount_out == 0:
            continue

        try:
            helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                token_in=lp.token0,
                token_in_quantity=token0_in_amount,
            )
        except IncompleteSwap as exc:
            helper_amount_out = exc.amount_out
        except PossibleInaccurateResult:
            # The result might not match because a swap hook is not modeled
            continue
        except LiquidityPoolError as exc:
            if exc.message is not None and (
                "PriceLimitAlreadyExceeded" in exc.message or "PriceLimitOutOfBounds" in exc.message
            ):
                continue
            raise

        assert helper_amount_out == quoter_amount_out, (
            f"Failed calc with {token_mult}x mult, token0 in, {lp.pool_id=} {lp.pool_key.hooks=}"
        )

    for token_mult in TOKEN_AMOUNT_MULTIPLIERS:
        token1_in_amount = max(1, int(token_mult * max_reserves_token1))

        try:
            quoter_amount_out, _ = quoter.functions.quoteExactInputSingle(
                (
                    dataclasses.astuple(lp.pool_key),  # poolKey
                    False,  # zeroForOne
                    token1_in_amount,  # exactAmount
                    b"",  # hookData
                )
            ).call()
        except ContractLogicError:
            continue

        if quoter_amount_out == 0:
            continue

        try:
            helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                token_in=lp.token1,
                token_in_quantity=token1_in_amount,
            )
        except IncompleteSwap as exc:
            helper_amount_out = exc.amount_out
        except PossibleInaccurateResult:
            # The result might not match because a swap hook is not modeled
            continue
        except LiquidityPoolError as exc:
            if exc.message is not None and (
                "PriceLimitAlreadyExceeded" in exc.message or "PriceLimitOutOfBounds" in exc.message
            ):
                continue
            raise

        assert helper_amount_out == quoter_amount_out, (
            f"Failed calc with {token_mult}x mult, token1 in, {lp.pool_id=} {lp.pool_key.hooks=}"
        )


def _test_pool_exact_output(
    pool: dict[str, Any],
    fork: AnvilFork,
    snapshot: dict[str, Any] | None = None,
):
    quoter = fork.w3.eth.contract(address=UNISWAP_V4_QUOTER_ADDRESS, abi=UNISWAP_V4_QUOTER_ABI)

    pool_id: HexStr = pool["pool_id"]

    lp = pool_registry.get(
        chain_id=fork.w3.eth.chain_id,
        pool_address=V4_POOL_MANAGER_ADDRESS,
        pool_id=pool_id,
    )
    if lp is None:
        try:
            lp = UniswapV4Pool(
                pool_id=pool_id,
                pool_manager_address=V4_POOL_MANAGER_ADDRESS,
                tokens=(
                    pool["token0"],
                    pool["token1"],
                ),
                fee=pool["fee"],
                tick_spacing=pool["tick_spacing"],
                hook_address=pool["hooks"],
                state_view_address=STATE_VIEW_ADDRESS,
                tick_bitmap=snapshot[pool_id]["tick_bitmap"]
                if snapshot is not None and pool_id in snapshot
                else None,
                tick_data=snapshot[pool_id]["tick_data"]
                if snapshot is not None and pool_id in snapshot
                else None,
            )
        except Exception as exc:
            print(f"Caught exception {exc} building pool id {pool['pool_id']}")
            raise

    assert isinstance(lp, UniswapV4Pool)

    max_reserves_token0 = 1 * 10**lp.token0.decimals
    max_reserves_token1 = 1 * 10**lp.token1.decimals

    if max_reserves_token0 == 0 or max_reserves_token1 == 0:
        return

    for token_mult in TOKEN_AMOUNT_MULTIPLIERS:
        token0_out_amount = int(token_mult * max_reserves_token0)

        try:
            quoter_amount_in, _ = quoter.functions.quoteExactOutputSingle(
                (
                    dataclasses.astuple(lp.pool_key),  # poolKey
                    False,  # zeroForOne
                    token0_out_amount,  # exactAmount
                    b"",  # hookData
                )
            ).call()
        except ContractLogicError:
            continue

        if quoter_amount_in == 0:
            continue

        try:
            helper_amount_in = lp.calculate_tokens_in_from_tokens_out(
                token_out=lp.token0,
                token_out_quantity=token0_out_amount,
            )
        except IncompleteSwap as exc:
            helper_amount_in = exc.amount_in
        except PossibleInaccurateResult:
            # The result might not match because a swap hook is not modeled
            continue
        except LiquidityPoolError as exc:
            if exc.message is not None and (
                "PriceLimitAlreadyExceeded" in exc.message or "PriceLimitOutOfBounds" in exc.message
            ):
                continue
            raise

        assert helper_amount_in == quoter_amount_in, (
            f"Failed calc with {token_mult}x mult, token0 out, {lp.pool_id=} {lp.pool_key.hooks=}"
        )

    for token_mult in TOKEN_AMOUNT_MULTIPLIERS:
        token1_out_amount = int(token_mult * max_reserves_token1)

        try:
            quoter_amount_in, _ = quoter.functions.quoteExactOutputSingle(
                (
                    dataclasses.astuple(lp.pool_key),  # poolKey
                    True,  # zeroForOne
                    token1_out_amount,  # exactAmount
                    b"",  # hookData
                )
            ).call()
        except ContractLogicError:
            continue

        if quoter_amount_in == 0:
            continue

        try:
            helper_amount_in = lp.calculate_tokens_in_from_tokens_out(
                token_out=lp.token1,
                token_out_quantity=token1_out_amount,
            )
        except IncompleteSwap as exc:
            helper_amount_in = exc.amount_in
        except PossibleInaccurateResult:
            # The result might not match because a swap hook is not modeled
            continue
        except LiquidityPoolError as exc:
            if exc.message is not None and (
                "PriceLimitAlreadyExceeded" in exc.message or "PriceLimitOutOfBounds" in exc.message
            ):
                continue
            raise

        assert helper_amount_in == quoter_amount_in, (
            f"Failed calc with {token_mult}x mult, token1 out, {lp.pool_id=} {lp.pool_key.hooks=}"
        )


def test_pool_creation(eth_usdc_v4: UniswapV4Pool):
    assert eth_usdc_v4.pool_id == HexBytes(ETH_USDC_V4_POOL_ID)
    assert eth_usdc_v4.address == V4_POOL_MANAGER_ADDRESS
    assert eth_usdc_v4.tokens[0].address == NATIVE_CURRENCY_ADDRESS
    assert eth_usdc_v4.tokens[1].address == USDC_CONTRACT_ADDRESS


def test_pool_liquidity_checks(eth_usdc_v4: UniswapV4Pool):
    assert eth_usdc_v4.liquidity > 0


def test_pool_sqrt_price_checks(eth_usdc_v4: UniswapV4Pool):
    assert eth_usdc_v4.sqrt_price_x96 > 0


def test_first_200_pools(
    fork_mainnet_full: AnvilFork,
    testing_pools,
):
    set_web3(fork_mainnet_full.w3)

    for pool in testing_pools:
        _test_pool_exact_input(
            pool=pool,
            fork=fork_mainnet_full,
        )
        _test_pool_exact_output(
            pool=pool,
            fork=fork_mainnet_full,
        )


def test_first_200_pools_with_snapshot(
    fork_mainnet_archive: AnvilFork,
    testing_pools,
    liquidity_snapshot,
):
    fork_mainnet_archive.reset(block_number=liquidity_snapshot["snapshot_block"])
    set_web3(fork_mainnet_archive.w3)

    for pool in testing_pools:
        _test_pool_exact_input(
            pool=pool,
            fork=fork_mainnet_archive,
            snapshot=liquidity_snapshot,
        )
        _test_pool_exact_output(
            pool=pool,
            fork=fork_mainnet_archive,
            snapshot=liquidity_snapshot,
        )


SINGLE_POOL_ID = ""


def test_single_pool(
    fork_mainnet_archive: AnvilFork,
    testing_pools,
):
    if not SINGLE_POOL_ID:
        return

    [pool] = [pool for pool in testing_pools if pool["pool_id"] == SINGLE_POOL_ID]
    _test_pool_exact_input(
        pool=pool,
        fork=fork_mainnet_archive,
    )
    _test_pool_exact_output(
        pool=pool,
        fork=fork_mainnet_archive,
    )


def test_single_pool_with_snapshot(
    fork_mainnet_archive: AnvilFork,
    testing_pools,
    liquidity_snapshot,
):
    if not SINGLE_POOL_ID:
        return

    fork_mainnet_archive.reset(block_number=liquidity_snapshot["snapshot_block"])

    [pool] = [pool for pool in testing_pools if pool["pool_id"] == SINGLE_POOL_ID]
    _test_pool_exact_input(
        pool=pool,
        fork=fork_mainnet_archive,
        snapshot=liquidity_snapshot,
    )
    _test_pool_exact_output(
        pool=pool,
        fork=fork_mainnet_archive,
        snapshot=liquidity_snapshot,
    )
