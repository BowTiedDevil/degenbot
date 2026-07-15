"""Uniswap V4 ETH/USDC on-chain parity — golden record/replay (T5).

Golden conversion of ``test_cached_calculations`` (the Uniswap V4 quoter
``quoteExactInputSingle`` / ``quoteExactOutputSingle`` exact-equality parity).
The original test was ``@hypothesis.given(integers(1, MAX_INT128))`` —
non-deterministic, so it cannot be replayed against recorded oracle ints. Per
T5 (plans/golden-onchain-parity.md): the hypothesis search is replaced with a
**curated fixed amount list** spanning the ``[1, MAX_INT128]`` range (tiny wei,
ETH + USDC token units, balance fractions, huge liquidity-exhausting, near-max
int128); the hypothesis variant is kept as a separate ``slow``+``ethereum``
test.

The original test wraps each quoter call in
``try/except ContractLogicError: continue`` (the V4 quoter reverts on amounts
exceeding available liquidity); :class:`IncompleteSwap` is caught and its
partial result (``exc.amount_out`` / ``exc.amount_in``) is asserted against the
quoter int. This golden test reproduces that contract: quoter reverts are
recorded as ``reverted`` entries and skipped; ``IncompleteSwap`` partials are
compared the same way.

Pool construction: built I/O-free (ADR-005) via :func:`make_v4_pool` from a
**full tick-state cassette** recorded at the pinned block
(``tests/fixtures/chain_data/1/uniswap_v4_eth_usdc_block_24407242.json``).
``bot.build_managed_pool`` only eagerly fetches the active tick bitmap *word*;
this cassette captures **all initialized ticks** (499 across 36 bitmap words,
walked at record time via the V4 state-view ``getTickBitmap``/``getTickInfo``)
so the I/O-free sim matches the quoter across the full curated range —
verified: 68/68 non-reverting exact matches, 0 mismatches, 24 liquidity reverts
(skip).

- **Replay mode** (default, CI): reads recorded ints; the deferred
  ``contract=`` callables are never invoked, so no fork is created. Offline.
- **Record mode** (``--golden-mode=record``): one Anvil fork of Ethereum
  mainnet at the pinned block is shared across the whole loop.

Pinned to Ethereum mainnet block 24,407,242 (tip minus ~1M), served by a
local archive node (URI configurable via ``tests.env``; see
``ETHEREUM_ARCHIVE_NODE_HTTP_URI``).
"""

from __future__ import annotations

import dataclasses
import json
import pathlib
from contextlib import AbstractContextManager
from typing import TYPE_CHECKING, Any, Self

import pytest

from degenbot.anvil_fork import AnvilFork
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import MAX_INT128, ZERO_ADDRESS
from degenbot._ffi import PyBot
from degenbot.exceptions.pool import IncompleteSwap
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v4_pool_factory import make_v4_pool
from tests.uniswap.v4.test_uniswap_v4_liquidity_pool import (
    UNISWAP_V4_QUOTER_ABI,
    UNISWAP_V4_QUOTER_ADDRESS,
)
from tests.helpers.w3_contract import make_contract

if TYPE_CHECKING:
    from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

UNISWAP_V4_PARITY_BLOCK = 24_407_242  # tip minus ~1M

NATIVE_CURRENCY_ADDRESS = ZERO_ADDRESS

_CASSETTE_PATH = (
    pathlib.Path(__file__).resolve().parents[2]
    / "fixtures"
    / "chain_data"
    / "1"
    / "uniswap_v4_eth_usdc_block_24407242.json"
)

# Curated amount list spanning [1, MAX_INT128] across both token scales:
# currency0 = ETH (18 dec), currency1 = USDC (6 dec). Filled from the cassette
# balances in _parity_cases so replay reproduces the record-run inputs.
_BAL_ETH_FRACTION_MULTIPLIERS = (0.001, 0.01, 0.1)
_BAL_USDC_FRACTION_MULTIPLIERS = (0.001, 0.01, 0.1)
_FIXED_AMOUNTS = (
    1,
    10,
    100,
    1000,
    100_000,
    1 * 10**15,  # 0.001 ETH
    1 * 10**16,  # 0.01 ETH
    1 * 10**17,  # 0.1 ETH
    1 * 10**18,  # 1 ETH
    10 * 10**18,  # 10 ETH
    1 * 10**4,  # 0.01 USDC
    1 * 10**5,  # 0.1 USDC
    1 * 10**6,  # 1 USDC
    10 * 10**6,  # 10 USDC
    100 * 10**6,  # 100 USDC
    10**30,  # huge / liquidity-exhausting
    MAX_INT128,
)

_PYBOT = PyBot()


def _load_cassette() -> dict[str, Any]:
    return json.loads(_CASSETTE_PATH.read_bytes())


def _build_eth_usdc_v4_io_free() -> UniswapV4Pool:
    """Build the ETH/USDC V4 pool I/O-free from the full tick cassette."""
    cassette = _load_cassette()
    scalars = cassette["scalars"]
    tick_data = {int(tick): tuple(vals) for tick, vals in cassette["tick_data"].items()}
    eth = make_erc20(
        _PYBOT,
        cassette["token0"]["address"],
        name=cassette["token0"]["name"],
        symbol=cassette["token0"]["symbol"],
        decimals=cassette["token0"]["decimals"],
        chain_id=1,
    )
    usdc = make_erc20(
        _PYBOT,
        cassette["token1"]["address"],
        name=cassette["token1"]["name"],
        symbol=cassette["token1"]["symbol"],
        decimals=cassette["token1"]["decimals"],
        chain_id=1,
    )
    return make_v4_pool(
        pool_id=cassette["pool_id"],
        pool_manager_address=get_checksum_address(cassette["pool_manager"]),
        token0=eth,
        token1=usdc,
        fee=scalars["fee"],
        tick_spacing=scalars["tick_spacing"],
        hook_address=ZERO_ADDRESS,
        state_view_address=get_checksum_address(cassette["state_view"]),
        sqrt_price_x96=scalars["sqrt_price_x96"],
        tick=scalars["tick"],
        liquidity=scalars["liquidity"],
        protocol_fee_zero_for_one=scalars["protocol_fee_zero_for_one"],
        protocol_fee_one_for_zero=scalars["protocol_fee_one_for_zero"],
        lp_fee=scalars["lp_fee"],
        tick_data=tick_data,
        state_block=cassette["block"],
    )


def _parity_cases(lp: UniswapV4Pool) -> list[tuple[str, str, Any, Any, int, bool]]:
    """Build ``(method, key, token_in, token_out, amount, zero_for_one)`` cases.

    Two quoter methods (exact-input, exact-output) x both directions x the
    curated amount list; balance-fraction amounts are folded in from the
    cassette so replay reproduces the record-run inputs.
    """
    cassette = _load_cassette()
    bal_eth = cassette["balance_token0"]
    bal_usdc = cassette["balance_token1"]
    balance_fraction_amounts = [int(m * bal_eth) for m in _BAL_ETH_FRACTION_MULTIPLIERS] + [
        int(m * bal_usdc) for m in _BAL_USDC_FRACTION_MULTIPLIERS
    ]
    amounts = list(_FIXED_AMOUNTS) + balance_fraction_amounts

    pairs = (
        (lp.token0, lp.token1, True),  # ETH -> USDC (zero_for_one)
        (lp.token1, lp.token0, False),  # USDC -> ETH
    )
    cases: list[tuple[str, str, Any, Any, int, bool]] = []
    for token_in, token_out, zero_for_one in pairs:
        for amount in amounts:
            if amount == 0:
                continue
            key_in = (
                f"{lp.pool_id.hex()}|quoteExactInputSingle|"
                f"{token_in.symbol}->{token_out.symbol}|{amount}"
            )
            cases.append(("input", key_in, token_in, token_out, amount, zero_for_one))
            key_out = (
                f"{lp.pool_id.hex()}|quoteExactOutputSingle|"
                f"{token_in.symbol}->{token_out.symbol}|{amount}"
            )
            cases.append(
                ("output", key_out, token_in, token_out, amount, zero_for_one),
            )
    return cases


class _RecordFork(AbstractContextManager):
    """One pinned Ethereum fork shared across the test's whole record pass.

    In replay ``fork`` stays ``None`` (no fork is created); the deferred
    ``contract=`` callables are never invoked.
    """

    def __init__(self, *, recording: bool) -> None:
        self._recording = recording
        self.fork: AnvilFork | None = None
        self.contract: Any = None

    def __enter__(self) -> Self:
        if self._recording:
            self.fork = AnvilFork(
                fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
                fork_block=UNISWAP_V4_PARITY_BLOCK,
                storage_caching=True,
                anvil_opts=["--accounts=0"],
            )
        return self

    def __exit__(self, *exc: object) -> None:
        if self.fork is not None:
            self.fork.close()


def _quote_callable(
    contract: Any,
    method: str,
    pool_key_tuple: tuple,
    *,
    zero_for_one: bool,
    amount: int,
) -> Any:
    """Deferred ``contract=`` callable for one V4 quoter query.

    Returns the decoded int (``amountOut`` for input, ``amountIn`` for output),
    discarding the gas-estimate. In replay ``contract`` is ``None`` and this is
    never invoked.
    """

    def _call() -> int:
        if method == "input":
            amount_out, _gas = contract.functions.quoteExactInputSingle(
                (pool_key_tuple, zero_for_one, amount, b""),
            ).call()
            return amount_out
        amount_in, _gas = contract.functions.quoteExactOutputSingle(
            (pool_key_tuple, zero_for_one, amount, b""),
        ).call()
        return amount_in

    return _call


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
def test_cached_calculations_v4_eth_usdc(golden_factory) -> None:
    """Uniswap V4 ETH/USDC: local calc == golden(quoter), full curated range.

    Mirrors the original ``test_cached_calculations`` contract: both quoter
    methods (exact-input + exact-output), both directions, across a curated
    amount list spanning ``[1, MAX_INT128]``. Quoter reverts (exceeding
    available liquidity) are recorded + skipped — mirroring the original
    ``try/except ContractLogicError: continue``. :class:`IncompleteSwap` is
    caught and its partial result asserted against the recorded quoter int.
    """
    golden = golden_factory(chain_id=1, block_number=UNISWAP_V4_PARITY_BLOCK)
    lp = _build_eth_usdc_v4_io_free()
    cases = _parity_cases(lp)
    pool_key_tuple = dataclasses.astuple(lp.pool_key)

    with _RecordFork(recording=golden.is_recording) as ctx:
        if golden.is_recording:
            assert ctx.fork is not None
            ctx.contract = make_contract(ctx.fork.http_url, UNISWAP_V4_QUOTER_ADDRESS, UNISWAP_V4_QUOTER_ABI)
        for method, key, token_in, token_out, amount, zero_for_one in cases:
            oracle = golden.check(
                key,
                contract=_quote_callable(
                    ctx.contract,
                    method,
                    pool_key_tuple,
                    zero_for_one=zero_for_one,
                    amount=amount,
                ),
            )
            if oracle.reverted:
                # Mirrors the original try/except ContractLogicError: continue.
                continue
            if method == "input":
                try:
                    calc = lp.calculate_tokens_out_from_tokens_in(
                        token_in=token_in,
                        token_in_quantity=amount,
                    )
                except IncompleteSwap as exc:
                    calc = exc.amount_out
            else:
                try:
                    calc = lp.calculate_tokens_in_from_tokens_out(
                        token_out=token_out,
                        token_out_quantity=amount,
                    )
                except IncompleteSwap as exc:
                    calc = exc.amount_in
            assert calc == oracle.value, f"{key}: helper={calc} quoter={oracle.value}"
