"""PancakeSwap V2 on-chain parity — golden record/replay (T7).

Golden conversion of ``test_pancakeswap_calculations`` (the Pancake V2 router
``getAmountsOut`` exact-equality parity). The original module is module-level
skipped (deleted ``PancakeswapV2Pool`` subclass) — this is the revival under
the ``UniswapV2Pool`` + ``dex`` (ADR-005) model, as a golden-gated offline
parity test. One representative pool (WETH/USDbC) with the full
13-multiplier x both-directions loop; the broad 200-pool live loop stays as a
``@pytest.mark.slow`` discovery gate.

Pool construction: built I/O-free via :func:`make_v2_pool` from a
reserves/+metadata cassette recorded at the pinned block
(``tests/fixtures/chain_data/8453/pancakeswap_v2_weth_usdbc_block_*.json``).
Pancake V2 uses the standard Uniswap V2 constant-product formula with a 0.25%
fee (``fee_token0=fee_token1=Fraction(25, 10000)``); the Base factory
(``0x02a84c…``) is passed explicitly — the ``pancakeswap-v2`` DexIdentity
preset encodes the Ethereum-mainnet factory (chain 1), irrelevant to the calc
but cosmetically wrong for Base, so the cassette carries the Base factory.

- **Replay mode** (default, CI): reads recorded ints; the deferred
  ``contract=`` callables are never invoked, so no fork is created. Offline.
- **Record mode** (``--golden-mode=record``): one Anvil fork of Base at the
  pinned block is shared across the whole loop; each ``getAmountsOut`` call is
  recorded. The test's own ``assert`` runs too — a record run is also a live
  parity gate.

Pinned to Base block 46,875,151 (tip minus ~1M), inside
``https://mainnet.base.org``'s keyless archive window.
"""

from __future__ import annotations

import json
import pathlib
from contextlib import AbstractContextManager
from fractions import Fraction
from typing import TYPE_CHECKING, Any, Self

import pytest

from degenbot.bot import RustBot
from degenbot.checksum_cache import get_checksum_address
from degenbot.fork import AnvilFork
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool
from tests.helpers.w3_contract import make_contract

if TYPE_CHECKING:
    from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

PANCAKE_V2_PARITY_BLOCK = 46_875_151
BASE_RPC_URI = "https://mainnet.base.org"

PANCAKE_V2_ROUTER_ADDRESS = get_checksum_address(
    "0x8cFe327CEc66d1C090Dd72bd0FF11d690C33a2Eb",
)
_CASSETTE_PATH = (
    pathlib.Path(__file__).resolve().parents[1]
    / "fixtures"
    / "chain_data"
    / "8453"
    / "pancakeswap_v2_weth_usdbc_block_46875151.json"
)

# Minimal router ABI: factory() + getAmountsOut().
_ROUTER_ABI = [
    {
        "inputs": [],
        "name": "factory",
        "outputs": [{"type": "address"}],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "inputs": [{"type": "uint256"}, {"type": "address[]"}],
        "name": "getAmountsOut",
        "outputs": [{"type": "uint256[]"}],
        "stateMutability": "view",
        "type": "function",
    },
]

_TOKEN_AMOUNT_MULTIPLIERS = (
    0.000000001,
    0.00000001,
    0.0000001,
    0.000001,
    0.00001,
    0.0001,
    0.001,
    0.01,
    0.1,
    0.125,
    0.25,
    0.5,
    0.75,
)

_PYBOT = RustBot()


def _load_cassette() -> dict[str, Any]:
    return json.loads(_CASSETTE_PATH.read_bytes())


def _build_pancake_v2_io_free(cassette: dict[str, Any]) -> UniswapV2Pool:
    """Build the Pancake V2 pool I/O-free from the reserves cassette.

    Standard Uniswap V2 constant-product formula; fee passed explicitly
    (Pancake 0.25% = ``Fraction(25, 10000)``). The Base factory is carried in
    the cassette (the DexIdentity preset points at Ethereum mainnet).
    """
    t0 = cassette["token0"]
    t1 = cassette["token1"]
    tok0 = make_erc20(
        _PYBOT,
        t0["address"],
        name=t0["name"],
        symbol=t0["symbol"],
        decimals=t0["decimals"],
        chain_id=cassette["chain_id"],
    )
    tok1 = make_erc20(
        _PYBOT,
        t1["address"],
        name=t1["name"],
        symbol=t1["symbol"],
        decimals=t1["decimals"],
        chain_id=cassette["chain_id"],
    )
    fee = Fraction(cassette["fee_numer"], cassette["fee_denom"])
    return make_v2_pool(
        get_checksum_address(cassette["pool"]),
        token0=tok0,
        token1=tok1,
        factory=get_checksum_address(cassette["factory"]),
        fee_token0=fee,
        fee_token1=fee,
        reserves_token0=cassette["reserves_token0"],
        reserves_token1=cassette["reserves_token1"],
        chain_id=cassette["chain_id"],
        state_block=cassette["block"],
    )


def _parity_cases(lp: UniswapV2Pool) -> list[tuple[str, Any, Any, int]]:
    """Build ``(oracle_key, token_in, token_out, amount_in)`` cases.

    Amounts derive from the recorded reserves (cassette constants), so replay
    reproduces the exact inputs the record run used.
    """
    cases: list[tuple[str, Any, Any, int]] = []
    for token_in, token_out, reserve_in, reserve_out in (
        (lp.token0, lp.token1, lp.reserves_token0, lp.reserves_token1),
        (lp.token1, lp.token0, lp.reserves_token1, lp.reserves_token0),
    ):
        if reserve_out < 2:
            continue
        for mult in _TOKEN_AMOUNT_MULTIPLIERS:
            amount_in = int(mult * reserve_in)
            if amount_in == 0 or reserve_in <= 1:
                continue
            key = f"{lp.address}|getAmountsOut|{token_in.symbol}->{token_out.symbol}|{amount_in}"
            cases.append((key, token_in, token_out, amount_in))
    return cases


class _RecordFork(AbstractContextManager):
    """One pinned Base fork shared across the test's whole record pass.

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
                fork_url=BASE_RPC_URI,
                fork_block=PANCAKE_V2_PARITY_BLOCK,
                storage_caching=True,
                anvil_opts=["--accounts=0", "--optimism"],
            )
        return self

    def __exit__(self, *exc: object) -> None:
        if self.fork is not None:
            self.fork.close()


def _get_amounts_out_callable(
    contract: Any,
    amount: int,
    token_in: str,
    token_out: str,
) -> Any:
    """Deferred ``contract=`` callable for one ``getAmountsOut`` query.

    Returns the final ``amounts[-1]`` (the swap output). In replay
    ``contract`` is ``None`` and this is never invoked.
    """

    def _call() -> int:
        amounts = contract.functions.getAmountsOut(
            amount,
            [token_in, token_out],
        ).call()
        return amounts[-1]

    return _call


@pytest.mark.base
@pytest.mark.onchain_oracle
def test_pancakeswap_v2_router_get_amounts_out(golden_factory) -> None:
    """PancakeSwap V2 router: local calc == golden(getAmountsOut[-1])."""
    golden = golden_factory(chain_id=8453, block_number=PANCAKE_V2_PARITY_BLOCK)
    cassette = _load_cassette()
    lp = _build_pancake_v2_io_free(cassette)
    cases = _parity_cases(lp)

    with _RecordFork(recording=golden.is_recording) as ctx:
        if golden.is_recording:
            assert ctx.fork is not None
            ctx.contract = make_contract(ctx.fork.http_url, PANCAKE_V2_ROUTER_ADDRESS, _ROUTER_ABI)
        for key, token_in, token_out, amount_in in cases:
            oracle = golden.check(
                key,
                contract=_get_amounts_out_callable(
                    ctx.contract,
                    amount_in,
                    token_in.address,
                    token_out.address,
                ),
            )
            if oracle.reverted:
                continue
            calc = lp.calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=amount_in,
            )
            assert calc == oracle.value, f"{key}: helper={calc} router={oracle.value}"
