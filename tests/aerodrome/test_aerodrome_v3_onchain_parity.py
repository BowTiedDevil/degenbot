"""Aerodrome V3 on-chain parity — golden record/replay (T7).

Golden conversion of the Aerodrome V3 ``quoteExactInputSingle`` exact-equality
parity (``test_aerodrome_v3_pool_calculation`` in ``test_aerodrome_pools.py``).
One representative pool (cbETH/WETH, 0.01%) with the full 13-multiplier x
both-directions loop; the broad live loop stays as a live discovery gate.

Unlike the V2 tracers (reserves-only), a V3 (concentrated-liquidity) pool
needs tick state to simulate a swap, so the pool is built I/O-free (ADR-005)
from a **tick-state cassette** recorded at the pinned block
(``tests/fixtures/chain_data/8453/aerodrome_v3_cbeth_weth_block_46875151.json``)
— scalar state (``sqrt_price_x96``/``tick``/``liquidity``/``fee``/
``tick_spacing``) plus the initialized ticks. The swap-input amounts derive
from the pool's token balances at the pinned block (also in the cassette), so
replay reproduces the exact inputs the record run used.

- **Replay mode** (default, CI): reads recorded ints; the deferred
  ``contract=`` callables are never invoked, so no fork is created. Offline.
- **Record mode** (``--golden-mode=record``): one Anvil fork of Base at the
  pinned block is shared across the whole loop; each ``quoteExactInputSingle``
  call is recorded. The test's own ``assert`` runs too — a record run is also a
  live parity gate. Reverts are recorded as ``reverted`` entries and skipped
  (the 0.75 x balance cbETH->WETH swap exceeds the pool's available liquidity
  and reverts in the quoter itself).

Pinned to Base block 46,875,151 (tip minus ~1M), inside
``https://mainnet.base.org``'s keyless archive window.
"""

from __future__ import annotations

import json
import pathlib
from contextlib import AbstractContextManager
from typing import Any, Self

import pytest

from degenbot._ffi import Bot
from degenbot.aerodrome.pools import AerodromeV3Pool
from degenbot.checksum_cache import get_checksum_address
from degenbot.fork import AnvilFork
from degenbot.uniswap.v3_libraries import MAX_SQRT_RATIO, MIN_SQRT_RATIO
from tests.aerodrome.test_aerodrome_pools import AERODROME_V3_QUOTER_ABI
from tests.helpers.contract_compat import make_contract
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v3_pool_factory import make_v3_pool

AERODROME_V3_PARITY_BLOCK = 46_875_151
BASE_RPC_URI = "https://mainnet.base.org"

AERODROME_V3_CBETH_WETH_POOL_ADDRESS = get_checksum_address(
    "0x47cA96Ea59C13F72745928887f84C9F52C3D7348",
)
AERODROME_V3_FACTORY_ADDRESS = get_checksum_address(
    "0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A",
)
AERODROME_V3_QUOTER_ADDRESS = get_checksum_address(
    "0x254cF9E1E6e233aa1AC962CB9B05b2cfeAaE15b0",
)
_CBETH_ADDRESS = "0x2Ae3F1Ec7F1F5012CFEab0185bfc7aa3cf0DEc22"
_WETH_ADDRESS = "0x4200000000000000000000000000000000000006"

_CASSETTE_PATH = (
    pathlib.Path(__file__).resolve().parents[1]
    / "fixtures"
    / "chain_data"
    / "8453"
    / "aerodrome_v3_cbeth_weth_block_46875151.json"
)

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

_PYBOT = Bot()


def _load_cassette() -> dict[str, Any]:
    return json.loads(_CASSETTE_PATH.read_bytes())


def _build_aerodrome_v3_cbeth_weth_io_free() -> AerodromeV3Pool:
    """Build the Aerodrome V3 cbETH/WETH pool I/O-free from the tick cassette."""
    cassette = _load_cassette()
    scalars = cassette["scalars"]
    tick_data = {int(tick): tuple(vals) for tick, vals in cassette["tick_data"].items()}
    cbeth = make_erc20(
        _PYBOT,
        _CBETH_ADDRESS,
        name="Coinbase Wrapped Staked ETH",
        symbol="cbETH",
        decimals=18,
        chain_id=8453,
    )
    weth = make_erc20(
        _PYBOT,
        _WETH_ADDRESS,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
        chain_id=8453,
    )
    return make_v3_pool(
        AERODROME_V3_CBETH_WETH_POOL_ADDRESS,
        token0=cbeth,
        token1=weth,
        factory=AERODROME_V3_FACTORY_ADDRESS,
        fee=scalars["fee"],
        tick_spacing=scalars["tick_spacing"],
        sqrt_price_x96=scalars["sqrt_price_x96"],
        tick=scalars["tick"],
        liquidity=scalars["liquidity"],
        state_block=cassette["block"],
        tick_data=tick_data,
        pool_class=AerodromeV3Pool,
    )


def _parity_cases(
    lp: AerodromeV3Pool,
    cassette: dict[str, Any],
) -> list[tuple[str, Any, Any, int, int]]:
    """Build ``(key, token_in, token_out, amount_in, sqrt_price_limit)`` cases.

    Amounts derive from the pool's recorded token balances at the pinned block.
    """
    pairs = (
        (lp.token0, lp.token1, cassette["balance_token0"], MIN_SQRT_RATIO + 1),
        (lp.token1, lp.token0, cassette["balance_token1"], MAX_SQRT_RATIO - 1),
    )
    cases: list[tuple[str, Any, Any, int, int]] = []
    for token_in, token_out, balance_in, sqrt_limit in pairs:
        for mult in _TOKEN_AMOUNT_MULTIPLIERS:
            amount_in = int(mult * balance_in)
            if amount_in == 0:
                continue
            key = (
                f"{lp.address}|quoteExactInputSingle|"
                f"{token_in.symbol}->{token_out.symbol}|{amount_in}"
            )
            cases.append((key, token_in, token_out, amount_in, sqrt_limit))
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
                fork_block=AERODROME_V3_PARITY_BLOCK,
                storage_caching=True,
                anvil_opts=["--accounts=0", "--optimism"],
            )
        return self

    def __exit__(self, *exc: object) -> None:
        if self.fork is not None:
            self.fork.close()


def _quote_callable(
    contract: Any,
    amount: int,
    token_in: str,
    token_out: str,
    tick_spacing: int,
    sqrt_limit: int,
) -> Any:
    """Deferred ``contract=`` callable for one ``quoteExactInputSingle`` query.

    Returns ``amounts_out[0]``. In replay ``contract`` is ``None`` and this is
    never invoked.
    """

    def _call() -> int:
        amount_out, *_ = contract.functions.quoteExactInputSingle(
            [
                token_in,
                token_out,
                amount,
                tick_spacing,
                sqrt_limit,
            ],
        ).call()
        return amount_out

    return _call


@pytest.mark.base
@pytest.mark.onchain_oracle
def test_aerodrome_v3_cbeth_weth_quote(golden_factory) -> None:
    """Aerodrome V3 cbETH/WETH: local calc == golden(quoteExactInputSingle)."""
    golden = golden_factory(chain_id=8453, block_number=AERODROME_V3_PARITY_BLOCK)
    cassette = _load_cassette()
    lp = _build_aerodrome_v3_cbeth_weth_io_free()
    cases = _parity_cases(lp, cassette)

    with _RecordFork(recording=golden.is_recording) as ctx:
        if golden.is_recording:
            assert ctx.fork is not None
            ctx.contract = make_contract(
                ctx.fork.http_url, AERODROME_V3_QUOTER_ADDRESS, AERODROME_V3_QUOTER_ABI
            )
        for key, token_in, token_out, amount_in, sqrt_limit in cases:
            oracle = golden.check(
                key,
                contract=_quote_callable(
                    ctx.contract,
                    amount_in,
                    token_in.address,
                    token_out.address,
                    lp.tick_spacing,
                    sqrt_limit,
                ),
            )
            if oracle.reverted:
                # Mirrors the live test's revert handling: the quoter reverts
                # on swaps exceeding available liquidity (e.g. 0.75 x balance).
                continue
            calc = lp.calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=amount_in,
            )
            assert calc == oracle.value, f"{key}: helper={calc} quoter={oracle.value}"
