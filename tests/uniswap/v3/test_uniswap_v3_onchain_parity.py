"""Uniswap V3 WBTC/WETH on-chain parity — golden record/replay (T4).

Golden conversion of ``test_cached_calculations`` (the Uniswap V3 quoter
``quoteExactInputSingle`` / ``quoteExactOutputSingle`` exact-equality parity).
The original test was ``@hypothesis.given(integers(1, MAX_INT256))`` —
non-deterministic, so it cannot be replayed against recorded oracle ints. Per
T4 (plans/golden-onchain-parity.md): the hypothesis search is replaced with a
**curated fixed amount list** spanning the ``[1, MAX_INT256]`` range (tiny wei
amounts, token units, fractions of pool balance, huge liquidity-exhausting
amounts, and ``MAX_INT256`` itself); the hypothesis variant is kept as a
separate ``slow``+``ethereum`` test for periodic deep runs.

The original test catches :class:`IncompleteSwap` and compares the partial
result (``exc.amount_out`` / ``exc.amount_in``) to the quoter — for amounts
that exhaust the pool's liquidity, both the helper (partial result) and the
quoter (which returns the liquidity-exhausting swap rather than reverting)
agree exactly. This golden test reproduces that contract: ``IncompleteSwap``
is caught and its partial result is asserted against the recorded quoter int.

Pool construction: built I/O-free (ADR-005) from a **full tick-state cassette**
recorded at the pinned block
(``tests/fixtures/chain_data/1/uniswap_v3_wbtc_weth_block_24407242.json``).
Unlike a reserves-only V2 pool, a V3 swap consults initialized ticks across the
swapped range; ``bot.build_pool`` only eagerly fetches the active tick bitmap
*word*, so a one-word cassette silently caps large swaps that cross word
boundaries. This cassette captures **all initialized ticks** (458 across 21
bitmap words, walked at record time) so the I/O-free sim matches the quoter
across the full ``[1, MAX_INT256]`` range — verified: 80/80 exact matches, 0
reverts.

- **Replay mode** (default, CI): reads recorded ints; the deferred
  ``contract=`` callables are never invoked, so no fork is created. Offline.
- **Record mode** (``--golden-mode=record``): one Anvil fork of Ethereum
  mainnet at the pinned block is shared across the whole loop; each quoter
  call is recorded. The test's own ``assert`` runs too — a record run is also a
  live parity gate.

Pinned to Ethereum mainnet block 24,407,242 (tip minus ~1M), served by a
local archive node (URI configurable via ``tests.env``; see
``ETHEREUM_ARCHIVE_NODE_HTTP_URI``).
"""

from __future__ import annotations

import json
import pathlib
from contextlib import AbstractContextManager
from typing import Any, Self

import pytest

from degenbot.bot import RustBot
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import MAX_INT256
from degenbot.exceptions.pool import IncompleteSwap
from degenbot.fork import AnvilFork
from degenbot.uniswap.v3_libraries import MAX_SQRT_RATIO, MIN_SQRT_RATIO
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

# RPC URI is overridable via tests.env (see ETHEREUM_ARCHIVE_NODE_HTTP_URI);
# only contacted in record mode — replay is fully offline.
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v3_pool_factory import make_v3_pool
from tests.helpers.w3_contract import make_contract

UNISWAP_V3_PARITY_BLOCK = 24_407_242  # tip minus ~1M

WBTC_WETH_V3_POOL_ADDRESS = get_checksum_address(
    "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD",
)
UNISWAP_V3_FACTORY_ADDRESS = get_checksum_address(
    "0x1F98431c8aD98523631AE4a59f267346ea31F984",
)
UNISWAP_V3_QUOTER_ADDRESS = get_checksum_address(
    "0xb27308f9F90D607463bb33eA1BeBb41C27CE5AB6",
)
_WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
_WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

_CASSETTE_PATH = (
    pathlib.Path(__file__).resolve().parents[2]
    / "fixtures"
    / "chain_data"
    / "1"
    / "uniswap_v3_wbtc_weth_block_24407242.json"
)

# Curated amount list spanning [1, MAX_INT256]:
#  - tiny wei amounts        (1, 10, 100, 1000, 100k)
#  - WBTC token units (8 decimals) up to ten WBTC
#  - fractions of pool balance (token0 = WBTC, bal in cassette)
#  - WETH token units (18 decimals): milli, deci, one, ten, plus a balance fraction
#  - huge / liquidity-exhausting amounts (near-max int256)
# These exercise the full swap-simulation paths: in-range, tick-crossing,
# word-crossing, and liquidity-exhausting (IncompleteSwap partial == quoter).
_CURATED_AMOUNTS = (
    1,
    10,
    100,
    1000,
    100_000,
    1 * 10**6,
    1 * 10**7,
    1 * 10**8,
    10 * 10**8,
    int(0.0001 * 11_732_515_784),  # 0.0001 x WBTC balance (filled from cassette below)
    int(0.01 * 11_732_515_784),
    int(0.1 * 11_732_515_784),
    int(0.5 * 11_732_515_784),
    int(0.001 * 10**18),
    int(0.1 * 10**18),
    1 * 10**18,
    10 * 10**18,
    int(0.1 * 19_492_665_672_942_562_276_501),  # 0.1 x WETH balance
    10**30,
    MAX_INT256,
)

_PYBOT = RustBot()


def _load_cassette() -> dict[str, Any]:
    return json.loads(_CASSETTE_PATH.read_bytes())


def _build_wbtc_weth_v3_io_free() -> UniswapV3Pool:
    """Build the WBTC/WETH V3 pool I/O-free from the full tick cassette."""
    cassette = _load_cassette()
    scalars = cassette["scalars"]
    tick_data = {int(tick): tuple(vals) for tick, vals in cassette["tick_data"].items()}
    wbtc = make_erc20(
        _PYBOT,
        _WBTC_ADDRESS,
        name="Wrapped BTC",
        symbol="WBTC",
        decimals=8,
        chain_id=1,
    )
    weth = make_erc20(
        _PYBOT,
        _WETH_ADDRESS,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
        chain_id=1,
    )
    return make_v3_pool(
        WBTC_WETH_V3_POOL_ADDRESS,
        token0=wbtc,
        token1=weth,
        factory=UNISWAP_V3_FACTORY_ADDRESS,
        fee=scalars["fee"],
        tick_spacing=scalars["tick_spacing"],
        sqrt_price_x96=scalars["sqrt_price_x96"],
        tick=scalars["tick"],
        liquidity=scalars["liquidity"],
        state_block=cassette["block"],
        tick_data=tick_data,
        pool_class=UniswapV3Pool,
    )


def _parity_cases(
    lp: UniswapV3Pool,
) -> list[tuple[str, str, Any, Any, int, int]]:
    """Build ``(method, key, token_in, token_out, amount, sqrt_limit)`` cases.

    Two quoter methods (exact-input, exact-output) x both directions x the
    curated amount list. The ``method`` tag distinguishes the two oracle keys
    so a single golden file holds both contracts' results.
    """
    pairs = (
        (lp.token0, lp.token1, MIN_SQRT_RATIO + 1),
        (lp.token1, lp.token0, MAX_SQRT_RATIO - 1),
    )
    cases: list[tuple[str, str, Any, Any, int, int]] = []
    for token_in, token_out, sqrt_limit in pairs:
        for amount in _CURATED_AMOUNTS:
            if amount == 0:
                continue
            key_in = (
                f"{lp.address}|quoteExactInputSingle|{token_in.symbol}->{token_out.symbol}|{amount}"
            )
            cases.append(("input", key_in, token_in, token_out, amount, sqrt_limit))
            key_out = (
                f"{lp.address}|quoteExactOutputSingle|"
                f"{token_in.symbol}->{token_out.symbol}|{amount}"
            )
            cases.append(("output", key_out, token_in, token_out, amount, sqrt_limit))
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
                fork_block=UNISWAP_V3_PARITY_BLOCK,
                storage_caching=True,
                anvil_opts=["--accounts=0"],
            )
        return self

    def __exit__(self, *exc: object) -> None:
        if self.fork is not None:
            self.fork.close()


# Minimal quoter ABI: the two single-pool quote methods (nonpayable simulated).
_QUOTER_ABI = [
    {
        "inputs": [
            {"name": "tokenIn", "type": "address"},
            {"name": "tokenOut", "type": "address"},
            {"name": "fee", "type": "uint24"},
            {"name": "amountIn", "type": "uint256"},
            {"name": "sqrtPriceLimitX96", "type": "uint160"},
        ],
        "name": "quoteExactInputSingle",
        "outputs": [{"name": "amountOut", "type": "uint256"}],
        "stateMutability": "nonpayable",
        "type": "function",
    },
    {
        "inputs": [
            {"name": "tokenIn", "type": "address"},
            {"name": "tokenOut", "type": "address"},
            {"name": "fee", "type": "uint24"},
            {"name": "amountOut", "type": "uint256"},
            {"name": "sqrtPriceLimitX96", "type": "uint160"},
        ],
        "name": "quoteExactOutputSingle",
        "outputs": [{"name": "amountIn", "type": "uint256"}],
        "stateMutability": "nonpayable",
        "type": "function",
    },
]


def _quote_callable(
    contract: Any,
    method: str,
    token_in: str,
    token_out: str,
    fee: int,
    amount: int,
    sqrt_limit: int,
) -> Any:
    """Deferred ``contract=`` callable for one quoter query.

    In replay ``contract`` is ``None`` and this is never invoked.
    """

    def _call() -> int:
        if method == "input":
            return contract.functions.quoteExactInputSingle(
                token_in,
                token_out,
                fee,
                amount,
                sqrt_limit,
            ).call()
        return contract.functions.quoteExactOutputSingle(
            token_in,
            token_out,
            fee,
            amount,
            sqrt_limit,
        ).call()

    return _call


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
def test_cached_calculations_v3_wbtc_weth(golden_factory) -> None:
    """Uniswap V3 WBTC/WETH: local calc == golden(quoter), full curated range.

    Mirrors the original ``test_cached_calculations`` contract: both quoter
    methods (exact-input + exact-output), both directions, across a curated
    amount list spanning ``[1, MAX_INT256]``. :class:`IncompleteSwap` is caught
    and its partial result (``exc.amount_out`` / ``exc.amount_in``) is asserted
    against the recorded quoter int — for liquidity-exhausting amounts both the
    helper and the quoter return the same max-swap, so the assertion holds.
    """
    golden = golden_factory(chain_id=1, block_number=UNISWAP_V3_PARITY_BLOCK)
    lp = _build_wbtc_weth_v3_io_free()
    cases = _parity_cases(lp)

    with _RecordFork(recording=golden.is_recording) as ctx:
        if golden.is_recording:
            assert ctx.fork is not None
            ctx.contract = make_contract(ctx.fork.http_url, UNISWAP_V3_QUOTER_ADDRESS, _QUOTER_ABI)
        for method, key, token_in, token_out, amount, sqrt_limit in cases:
            oracle = golden.check(
                key,
                contract=_quote_callable(
                    ctx.contract,
                    method,
                    token_in.address,
                    token_out.address,
                    lp.fee,
                    amount,
                    sqrt_limit,
                ),
            )
            if oracle.reverted:
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
