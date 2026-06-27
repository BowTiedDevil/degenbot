"""Curve on-chain parity — golden record/replay (T6).

Golden conversion of the Curve exact-equality parity tests in
``test_curve_stableswap_pool.py``: ``test_tripool`` (``get_dy`` over token
permutations x amount multipliers), ``test_base_pool``
(``calc_withdraw_one_coin`` + ``calc_token_amount``), and ``test_tricrypto_pool``
(the Tricrypto crypto-pool ``get_dy`` with its non-standard ``uint256``
signature). One representative pool per Curve variant — plain stableswap
(3pool DAI/USDC/USDT) and crypto (Tricrypto WETH/WBTC/USDT) — with the full
``_test_calculations`` permutation x multiplier loop. The broad live discovery
loops (``test_factory_stableswap_pools`` / ``test_base_registry_pools``) stay
as live gates (excluded from CI per the design doc's exclusion rule).

Pool construction: built I/O-free (ADR-005) via :func:`make_curve_pool` +
:class:`FakeCurveDataProvider` from a **cassette** recorded at the pinned block
(``tests/fixtures/chain_data/1/curve_*_block_24407242.json``) carrying the
config (tokens, balances, A + ramping times, fee, strategies, virtual_price,
block_timestamp) and — for crypto pools — the on-chain D / gamma /
price_scale, plus the LP-token total supply for ``calc_withdraw_one_coin`` /
``calc_token_amount``. The data provider is seeded with these captured values
so the pool's calc runs offline with no RPC. Verified: 18/18 ``get_dy`` matches
per pool + 18/18 ``calc_withdraw_one_coin``/``calc_token_amount`` matches for
3pool, all offline.

- **Replay mode** (default, CI): reads recorded ints; the deferred
  ``contract=`` callables are never invoked, so no fork is created. Offline.
- **Record mode** (``--golden-mode=record``): one Anvil fork of Ethereum
  mainnet at the pinned block is shared across the whole loop.

Pinned to Ethereum mainnet block 24,407,242 (tip minus ~1M), served by the
local archive node (``host.containers.internal:8545`` in the devcontainer).
"""

from __future__ import annotations

import itertools
import json
import pathlib
from contextlib import AbstractContextManager
from typing import TYPE_CHECKING, Any, Self

import eth_abi.abi
import pytest
from web3 import Web3

from degenbot.anvil_fork import AnvilFork
from degenbot.curve.strategies import (
    DVariant,
    LendingRateStyle,
    MetapoolRateStyle,
    MetapoolUnderlyingStyle,
    PoolStrategies,
    SwapStyle,
    YDVariant,
    YVariant,
)
from degenbot.degenbot_rs import PyBot
from tests.fakes.curve_data_provider import FakeCurveDataProvider
from tests.helpers.curve_pool_factory import make_curve_pool
from tests.helpers.erc20_factory import make_erc20

if TYPE_CHECKING:
    from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool

ETHEREUM_RPC_URI = "http://host.containers.internal:8545/"
CURVE_PARITY_BLOCK = 24_407_242  # tip minus ~1M

_CASSETTE_DIR = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "chain_data" / "1"
_TRIPOOL_CASSETTE = _CASSETTE_DIR / "curve_tripool_block_24407242.json"
_TRICRYPTO_CASSETTE = _CASSETTE_DIR / "curve_tricrypto_block_24407242.json"

TRIPOOL_ADDRESS = "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"
TRICRYPTO_ADDRESS = "0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5"

_AMOUNT_MULTIPLIERS = (0.01, 0.05, 0.25)
_BASE_POOL_AMOUNT_MULTIPLIERS = (0.01, 0.10, 0.25)

# Cassette short-name keys mapped to make_curve_pool kwargs (crypto-only /
# lending keys absent from the plain-pool cassette are skipped).
KWARG_MAP = (
    ("use_lending", "use_lending"),
    ("fee_gamma", "fee_gamma"),
    ("mid_fee", "mid_fee"),
    ("out_fee", "out_fee"),
    ("gamma", "gamma"),
    ("offpeg_fee_multiplier", "offpeg_fee_multiplier"),
    ("create_timestamp", "create_timestamp"),
)

_CURVE_V1_POOL_GET_DY_ABI = [
    {
        "inputs": [
            {"name": "i", "type": "int128"},
            {"name": "j", "type": "int128"},
            {"name": "dx", "type": "uint256"},
        ],
        "name": "get_dy",
        "outputs": [{"name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function",
    },
]


class _DataProvider(FakeCurveDataProvider):
    """FakeCurveDataProvider extended with the LP-token total supply.

    ``calc_withdraw_one_coin`` / ``calc_token_amount`` consult the LP-token
    total supply via the data provider; ``get_dy`` does not. The plain-pool
    cassette carries the single LP-token supply to satisfy both.
    """

    def __init__(self, *, lp_token_total_supply: int, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self._lp_token_total_supply = lp_token_total_supply

    def token_total_supply(self, token_address: str, block_number: int) -> int:
        return self._lp_token_total_supply


_PYBOT = PyBot()


def _load_cassette(path: pathlib.Path) -> dict[str, Any]:
    return json.loads(path.read_bytes())


def _strategies_from_cassette(cassette: dict[str, Any]) -> PoolStrategies:
    st = cassette["strategies"]
    return PoolStrategies(
        d_variant=DVariant(st["d_variant"]),
        y_variant=YVariant(st["y_variant"]),
        yd_variant=YDVariant(st["yd_variant"]),
        swap_style=SwapStyle(st["swap_style"]),
        metapool_rate_style=MetapoolRateStyle(st["metapool_rate_style"]),
        metapool_underlying_style=MetapoolUnderlyingStyle(
            st["metapool_underlying_style"],
        ),
        lending_rate_style=LendingRateStyle(st["lending_rate_style"]),
    )


def _build_curve_io_free(cassette: dict[str, Any]) -> CurveStableswapPool:
    """Build a Curve pool I/O-free from a recorded cassette.

    Seeds a :class:`_DataProvider` with the captured virtual_price,
    block_timestamp, (for crypto pools) D/gamma/price_scale, and the LP-token
    total supply, then constructs the companion via :func:`make_curve_pool`.
    """
    tokens = [
        make_erc20(
            _PYBOT,
            t["address"],
            name=t["name"],
            symbol=t["symbol"],
            decimals=t["decimals"],
            chain_id=1,
        )
        for t in cassette["tokens"]
    ]
    dp_kwargs: dict[str, Any] = {
        "virtual_price": cassette["virtual_price"],
        "block_timestamp": cassette["block_timestamp"],
        "lp_token_total_supply": cassette["lp_token_total_supply"],
    }
    if "d" in cassette:
        dp_kwargs["d"] = cassette["d"]
        dp_kwargs["gamma"] = cassette["gamma"]
        dp_kwargs["price_scale"] = tuple(cassette["price_scale"])
    data_provider = _DataProvider(**dp_kwargs)

    extra: dict[str, Any] = {}
    if "lp_token" in cassette:
        extra["lp_token"] = make_erc20(
            _PYBOT,
            cassette["lp_token"]["address"],
            name=cassette["lp_token"]["name"],
            symbol=cassette["lp_token"]["symbol"],
            decimals=cassette["lp_token"]["decimals"],
            chain_id=1,
        )
    # Map cassette keys (short names) → make_curve_pool kwargs, skipping
    # crypto-only / lending keys absent in the cassette.
    extra: dict[str, Any] = {}
    for cass_key, kwarg in KWARG_MAP:
        if cass_key in cassette:
            extra[kwarg] = cassette[cass_key]
    # A-ramping: cassette uses short names; make_curve_pool expects the long
    # kwarg names. These MUST be forwarded or the pool's _a(timestamp) used by
    # get_dy falls back to defaults and the calc drifts.
    if "initial_a" in cassette:
        extra["initial_a_coefficient"] = cassette["initial_a"]
        extra["future_a_coefficient"] = cassette["future_a"]
        extra["initial_a_coefficient_time"] = cassette["initial_a_time"]
        extra["future_a_coefficient_time"] = cassette["future_a_time"]
    extra["strategies"] = _strategies_from_cassette(cassette)

    return make_curve_pool(
        cassette["address"],
        tokens=tokens,
        a_coefficient=cassette["a_coefficient"],
        fee=cassette["fee"],
        admin_fee=cassette["admin_fee"],
        balances=cassette["balances"],
        chain_id=1,
        state_block=cassette["block"],
        data_provider=data_provider,
        **extra,
    )


class _RecordFork(AbstractContextManager):
    """One pinned Ethereum fork shared across the test's whole record pass.

    In replay ``fork`` stays ``None`` (no fork is created); the deferred
    ``contract=`` callables are never invoked.
    """

    def __init__(self, *, recording: bool) -> None:
        self._recording = recording
        self.fork: AnvilFork | None = None

    def __enter__(self) -> Self:
        if self._recording:
            self.fork = AnvilFork(
                fork_url=ETHEREUM_RPC_URI,
                fork_block=CURVE_PARITY_BLOCK,
                storage_caching=True,
                anvil_opts=["--accounts=0"],
            )
        return self

    def __exit__(self, *exc: object) -> None:
        if self.fork is not None:
            self.fork.close()

    def raw_call(self, to: str, data: bytes) -> bytes:
        assert self.fork is not None
        return self.fork.w3.eth.call(transaction={"to": to, "data": data.hex()})


def _get_dy_standard_callable(
    fork: _RecordFork,
    pool_addr: str,
    i: int,
    j: int,
    amount: int,
) -> Any:
    """Standard ``get_dy(int128,int128,uint256)`` quoter (plain stableswap)."""

    def _call() -> int:
        data = Web3.keccak(text="get_dy(int128,int128,uint256)")[:4] + (
            eth_abi.abi.encode(
                types=["int128", "int128", "uint256"],
                args=[i, j, amount],
            )
        )
        res = fork.raw_call(pool_addr, data)
        amount_out, *_ = eth_abi.abi.decode(types=["uint256"], data=res)
        return amount_out

    return _call


def _get_dy_uint256_callable(
    fork: _RecordFork,
    pool_addr: str,
    i: int,
    j: int,
    amount: int,
) -> Any:
    """Crypto-pool ``get_dy(uint256,uint256,uint256)`` (Tricrypto)."""

    def _call() -> int:
        data = Web3.keccak(text="get_dy(uint256,uint256,uint256)")[:4] + (
            eth_abi.abi.encode(
                types=["uint256", "uint256", "uint256"],
                args=[i, j, amount],
            )
        )
        res = fork.raw_call(pool_addr, data)
        amount_out, *_ = eth_abi.abi.decode(types=["uint256"], data=res)
        return amount_out

    return _call


def _calc_withdraw_one_coin_callable(
    fork: _RecordFork,
    pool_addr: str,
    amount: int,
    i: int,
    n_tokens: int,
) -> Any:
    def _call() -> int:
        data = Web3.keccak(text="calc_withdraw_one_coin(uint256,int128)")[:4] + (
            eth_abi.abi.encode(types=["uint256", "int128"], args=[amount, i])
        )
        res = fork.raw_call(pool_addr, data)
        out, *_ = eth_abi.abi.decode(types=["uint256"], data=res)
        return out

    return _call


def _calc_token_amount_callable(
    fork: _RecordFork,
    pool_addr: str,
    amounts: list[int],
    n_tokens: int,
) -> Any:
    def _call() -> int:
        data = Web3.keccak(
            text=f"calc_token_amount(uint256[{n_tokens}],bool)",
        )[:4] + eth_abi.abi.encode(
            types=[f"uint256[{n_tokens}]", "bool"],
            args=[amounts, True],
        )
        res = fork.raw_call(pool_addr, data)
        out, *_ = eth_abi.abi.decode(types=["uint256"], data=res)
        return out

    return _call


def _run_get_dy_parity(
    golden_factory,
    cassette_path: pathlib.Path,
    get_dy_callable,
) -> None:
    golden = golden_factory(chain_id=1, block_number=CURVE_PARITY_BLOCK)
    cassette = _load_cassette(cassette_path)
    lp = _build_curve_io_free(cassette)
    n = len(lp.tokens)

    with _RecordFork(recording=golden.is_recording) as fork:
        for i, j in itertools.permutations(range(n), 2):
            for mult in _AMOUNT_MULTIPLIERS:
                amount = int(mult * lp.balances[i])
                key = f"{lp.address}|get_dy|{lp.tokens[i].symbol}->{lp.tokens[j].symbol}|{amount}"
                oracle = golden.check(
                    key,
                    contract=get_dy_callable(
                        fork,
                        lp.address,
                        i,
                        j,
                        amount,
                    ),
                )
                if oracle.reverted:
                    continue
                calc = lp.calculate_tokens_out_from_tokens_in(
                    token_in=lp.tokens[i],
                    token_out=lp.tokens[j],
                    token_in_quantity=amount,
                )
                assert calc == oracle.value, f"{key}: helper={calc} contract={oracle.value}"


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
def test_curve_tripool_get_dy(golden_factory) -> None:
    """Curve 3pool (DAI/USDC/USDT): local calc == golden(get_dy)."""
    _run_get_dy_parity(
        golden_factory,
        _TRIPOOL_CASSETTE,
        _get_dy_standard_callable,
    )


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
def test_curve_tricrypto_get_dy(golden_factory) -> None:
    """Curve Tricrypto (WETH/WBTC/USDT): local calc == golden(get_dy uint256)."""
    _run_get_dy_parity(
        golden_factory,
        _TRICRYPTO_CASSETTE,
        _get_dy_uint256_callable,
    )


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
def test_curve_tripool_calc_withdraw_and_token_amount(golden_factory) -> None:
    """Curve 3pool: calc_withdraw_one_coin + calc_token_amount == golden.

    Covers the ``test_base_pool`` parity (LP-token-math oracle calls) on the
    3pool, which is also the pool ``test_tripool`` exercises — so this test
    reuses the TRIPOOL cassette.
    """
    golden = golden_factory(chain_id=1, block_number=CURVE_PARITY_BLOCK)
    cassette = _load_cassette(_TRIPOOL_CASSETTE)
    lp = _build_curve_io_free(cassette)
    n = len(lp.tokens)

    with _RecordFork(recording=golden.is_recording) as fork:
        # calc_withdraw_one_coin
        for i in range(n):
            for mult in _BASE_POOL_AMOUNT_MULTIPLIERS:
                amount = int(mult * lp.balances[i])
                key = f"{lp.address}|calc_withdraw_one_coin|i={i}|{amount}"
                oracle = golden.check(
                    key,
                    contract=_calc_withdraw_one_coin_callable(
                        fork,
                        lp.address,
                        amount,
                        i,
                        n,
                    ),
                )
                if oracle.reverted:
                    continue
                calc, *_ = lp.calc_withdraw_one_coin(_token_amount=amount, i=i)
                assert calc == oracle.value, f"{key}: helper={calc} contract={oracle.value}"
        # LP-token math: deposit amount-array (single token_in slot)
        for i in range(n):
            for mult in _BASE_POOL_AMOUNT_MULTIPLIERS:
                amount = int(mult * lp.balances[i])
                amounts = [0] * n
                amounts[i] = amount
                key = f"{lp.address}|calc_token_amount|deposit|i={i}|{amount}"
                oracle = golden.check(
                    key,
                    contract=_calc_token_amount_callable(
                        fork,
                        lp.address,
                        amounts,
                        n,
                    ),
                )
                if oracle.reverted:
                    continue
                calc = lp.calc_token_amount(amounts=amounts, deposit=True)
                assert calc == oracle.value, f"{key}: helper={calc} contract={oracle.value}"
