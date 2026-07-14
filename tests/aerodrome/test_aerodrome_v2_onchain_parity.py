"""Aerodrome V2 on-chain parity — golden record/replay (T7).

Golden conversion of the Aerodrome V2 ``getAmountOut`` exact-equality parity
(``test_calculation_volatile`` / ``test_calculation_stable`` in
``test_aerodrome_pools.py``). The broad live loops over the 200-pool JSON
fixture stay as ``@pytest.mark.slow`` discovery gates (out of CI per the design
doc's ``first_200_*`` rule); these modules convert one representative pool per
class (volatile + stable) with the full multiplier x both-directions loop,
recorded once into golden files, replayed offline.

Pool construction (ADR-005): built I/O-free as a pure-Python
:class:`AerodromeV2Pool` companion from a reserves/+metadata cassette recorded
at the pinned block (``tests/fixtures/chain_data/8453/aerodrome_v2_*.json``) —
**no Rust registration**. This is the same path ``Bot.build_pool()`` takes
(the Aerodrome V2 builder constructs the Python companion directly); the
generic ``make_v2_pool``/Rust path is NOT used because the Rust V2 calc rounds
differently than Aerodrome's on-chain ``getAmountOut`` (the Python companion
matches exactly — verified across the full multiplier loop both directions).

- **Replay mode** (default, CI): reads recorded ints; the deferred
  ``contract=`` callables are never invoked, so no fork is created. Fully
  offline.
- **Record mode** (``--golden-mode=record``): one Anvil fork of Base at the
  pinned block is shared across the whole loop; each ``getAmountOut`` call is
  recorded. The test's own ``assert`` still runs, so a record run is also a
  live parity gate. Reverts are recorded as ``reverted`` entries and skipped,
  mirroring the live test's ``try/except … continue``.

Pinned to Base block 46,875,151 (tip minus ~1M), inside
``https://mainnet.base.org``'s keyless archive window.
"""

from __future__ import annotations

import json
import pathlib
from contextlib import AbstractContextManager
from fractions import Fraction
from typing import Any, Self

import pytest

from degenbot.aerodrome.abi import AERODROME_V2_POOL_ABI
from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.anvil_fork import AnvilFork
from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyBot
from tests.helpers.aerodrome_pool_factory import make_aerodrome_v2_pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.w3_contract import make_contract

# Pinned well inside mainnet.base.org's keyless archive window (tip ~47.8M).
AERODROME_V2_PARITY_BLOCK = 46_875_151
BASE_RPC_URI = "https://mainnet.base.org"
AERODROME_V2_FACTORY_ADDRESS = get_checksum_address(
    "0x420DD381b31aEf6683db6B902084cB0FFECe40Da",
)

_CASSETTE_DIR = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "chain_data" / "8453"
_VOLATILE_CASSETTE = _CASSETTE_DIR / "aerodrome_v2_tbtc_weth_volatile_block_46875151.json"
_STABLE_CASSETTE = _CASSETTE_DIR / "aerodrome_v2_dola_usdbc_stable_block_46875151.json"

# Same multiplier coverage as the live ``test_calculation_*`` loops.
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

_PYBOT = PyBot()


def _load_cassette(path: pathlib.Path) -> dict[str, Any]:
    return json.loads(path.read_bytes())


def _build_aerodrome_v2_io_free(cassette: dict[str, Any]) -> AerodromeV2Pool:
    """Build an Aerodrome V2 pool I/O-free (pure-Python companion, no Rust).

    Mirrors ``Bot.build_pool`` (the Aerodrome V2 builder constructs the
    Python companion directly with ``fee``/``stable``/``reserves``; no
    ``register_v2_pool``). The generic ``make_v2_pool``/Rust path is avoided
    because its V2 calc rounds differently than Aerodrome's on-chain
    ``getAmountOut``.
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
    pool_address = get_checksum_address(cassette["pool"])
    factory = get_checksum_address(cassette.get("factory", AERODROME_V2_FACTORY_ADDRESS))
    return make_aerodrome_v2_pool(
        address=pool_address,
        token0=tok0,
        token1=tok1,
        factory=factory,
        fee=Fraction(cassette["fee_raw"], AerodromeV2Pool.FEE_DENOMINATOR),
        stable=cassette["stable"],
        reserves_token0=cassette["reserves_token0"],
        reserves_token1=cassette["reserves_token1"],
        deployer_address=factory,
        state_block=cassette["block"],
        py_bot=_PYBOT,
    )


def _parity_cases(lp: AerodromeV2Pool) -> list[tuple[str, Any, int]]:
    """Build ``(oracle_key, token_in, amount_in)`` cases for the full loop.

    Amounts derive from the recorded reserves (cassette constants), so replay
    reproduces the exact same inputs the record run used.
    """
    cases: list[tuple[str, Any, int]] = []
    for token_in, token_out, reserve_in, reserve_out in (
        (lp.token0, lp.token1, lp.reserves_token0, lp.reserves_token1),
        (lp.token1, lp.token0, lp.reserves_token1, lp.reserves_token0),
    ):
        if reserve_out < 2:  # mirror the live test's guard
            continue
        for mult in _TOKEN_AMOUNT_MULTIPLIERS:
            amount_in = int(mult * reserve_in)
            if amount_in == 0:
                continue
            if reserve_in <= 1:  # mirror the live test's guard
                continue
            key = f"{lp.address}|getAmountOut|{token_in.symbol}->{token_out.symbol}|{amount_in}"
            cases.append((key, token_in, amount_in))
    return cases


class _RecordFork(AbstractContextManager):
    """One pinned Base fork shared across a test's whole record pass.

    In replay mode the test passes ``None`` (no fork is created); the
    ``contract=`` callables are never invoked, so no fork is needed.
    """

    def __init__(self, *, recording: bool) -> None:
        self._recording = recording
        self.fork: AnvilFork | None = None
        self.contract: Any = None

    def __enter__(self) -> Self:
        if self._recording:
            self.fork = AnvilFork(
                fork_url=BASE_RPC_URI,
                fork_block=AERODROME_V2_PARITY_BLOCK,
                storage_caching=True,
                anvil_opts=["--accounts=0", "--optimism"],
            )
        return self

    def __exit__(self, *exc: object) -> None:
        if self.fork is not None:
            self.fork.close()


def _get_amount_out_callable(contract: Any, amount: int, token_in: str) -> Any:
    """Deferred ``contract=`` callable for one ``getAmountOut`` query.

    In replay ``contract`` is ``None`` and this is never invoked.
    """

    def _call() -> int:
        return contract.functions.getAmountOut(amount, token_in).call()

    return _call


def _run_parity(
    golden_factory,
    cassette_path: pathlib.Path,
) -> None:
    golden = golden_factory(chain_id=8453, block_number=AERODROME_V2_PARITY_BLOCK)
    cassette = _load_cassette(cassette_path)
    lp = _build_aerodrome_v2_io_free(cassette)
    cases = _parity_cases(lp)

    with _RecordFork(recording=golden.is_recording) as ctx:
        if golden.is_recording:
            assert ctx.fork is not None
            ctx.contract = make_contract(ctx.fork.http_url, lp.address, AERODROME_V2_POOL_ABI)
        for key, token_in, amount_in in cases:
            oracle = golden.check(
                key,
                contract=_get_amount_out_callable(ctx.contract, amount_in, token_in.address),
            )
            if oracle.reverted:
                # Mirror the live test's try/except-DegenbotError … continue.
                continue
            calc = lp.calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=amount_in,
            )
            assert calc == oracle.value, f"{key}: helper={calc} contract={oracle.value}"


@pytest.mark.base
@pytest.mark.onchain_oracle
def test_aerodrome_v2_volatile_get_amount_out(golden_factory) -> None:
    """Aerodrome V2 volatile (tBTC v2/WETH): local calc == golden(getAmountOut)."""
    _run_parity(golden_factory, _VOLATILE_CASSETTE)


@pytest.mark.base
@pytest.mark.onchain_oracle
def test_aerodrome_v2_stable_get_amount_out(golden_factory) -> None:
    """Aerodrome V2 stable (DOLA/USDbC): local calc == golden(getAmountOut)."""
    _run_parity(golden_factory, _STABLE_CASSETTE)
