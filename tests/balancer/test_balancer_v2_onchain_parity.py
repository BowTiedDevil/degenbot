"""Balancer V2 weighted-pool on-chain parity — golden record/replay (B1).

Golden conversion of the exact-equality parity tests in
``tests/balancer/test_pools.py``: ``test_calculations_weth_bal``,
``test_calculations_usdc_weth``, ``test_calculations_weth_rpl`` — three
WeightedPool2Tokens / WeightedPool variants covering both-even-decimals,
mixed-decimals, and different fee tiers. Each asserts the Python
``calculate_tokens_out_from_tokens_in`` exactly equals the on-chain
``BalancerQueries.querySwap`` (``SwapKind.GIVEN_IN``) over both swap
directions and the ``TOKEN_AMOUNT_MULTIPLIERS`` grid.

Pool construction: built I/O-free (ADR-005) via
:func:`make_balancer_weighted_pool` from a **cassette** recorded at the pinned
block (``tests/fixtures/chain_data/1/balancer_weighted_*.json``) carrying the
immutable config (address, pool_id, vault, tokens, weights, fee,
``pow_version``) + the live balances. Verified offline: 54/54 exact matches (3
pools x 2 directions x 9 multipliers).

- **Replay mode** (default, CI): reads recorded ints; the deferred
  ``contract=`` callable is never invoked, so no fork is created. Offline.
- **Record mode** (``--golden-mode=record``): one Anvil fork of Ethereum
  mainnet at the pinned block is shared across the whole loop.

Pinned to Ethereum mainnet block 24,407,242, served by a local archive node
(URI configurable via ``tests.env``; see ``ETHEREUM_ARCHIVE_NODE_HTTP_URI``).
"""

from __future__ import annotations

import itertools
import json
import pathlib
import socket
from contextlib import AbstractContextManager
from fractions import Fraction
from typing import TYPE_CHECKING, Any, Self

import pytest

from degenbot._ffi import Bot
from degenbot.balancer.deployments import (
    BALANCERQUERIES_CONTRACT_ADDRESS,
)
from degenbot.balancer.libraries.constants import PowVersion
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import ContractLogicError
from degenbot.exceptions.pool import EVMRevertError
from degenbot.fork import AnvilFork, ForkLaunchConfig
from degenbot.utils.bytes import to_bytes
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI
from tests.golden.oracle import GOLDEN_ROOT, _nodeid_to_path
from tests.helpers.balancer_pool_factory import make_balancer_weighted_pool
from tests.helpers.balancer_queries_abi import BALANCERQUERIES_ABI
from tests.helpers.contract_compat import ContractCompat
from tests.helpers.erc20_factory import make_erc20

if TYPE_CHECKING:
    from collections.abc import Iterator

    from degenbot.balancer.pools import BalancerV2Pool

BALANCER_PARITY_BLOCK = 24_407_242  # tip minus ~1M

_CASSETTE_DIR = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "chain_data" / "1"
_WETH_BAL_CASSETTE = _CASSETTE_DIR / "balancer_weighted_weth_bal.json"
_USDC_WETH_CASSETTE = _CASSETTE_DIR / "balancer_weighted_usdc_weth.json"
_WETH_RPL_CASSETTE = _CASSETTE_DIR / "balancer_weighted_weth_rpl.json"

# B2 — expanded weighted pools (test_pools_expanded.py): diverse weight profiles,
# fee tiers, decimal combos, and 2/3/4-token pools.
_EXPANDED_TWO_TOKEN_CASSETTES = {
    "aura_kaiaura_50_50": _CASSETTE_DIR / "balancer_weighted_aura_kaiaura_50_50.json",
    "gel_dexg_60_40": _CASSETTE_DIR / "balancer_weighted_gel_dexg_60_40.json",
    "par_mimo_75_25": _CASSETTE_DIR / "balancer_weighted_par_mimo_75_25.json",
    "sdvecrv_crv_90_10": _CASSETTE_DIR / "balancer_weighted_sdvecrv_crv_90_10.json",
    "dai_weth_40_60": _CASSETTE_DIR / "balancer_weighted_dai_weth_40_60.json",
    "usdc_weth_50_50": _CASSETTE_DIR / "balancer_weighted_usdc_weth_50_50.json",
}
_EXPANDED_MULTI_TOKEN_CASSETTES = {
    "apu_pepe_spx_3_token": _CASSETTE_DIR / "balancer_weighted_apu_pepe_spx_3_token.json",
    "dai_gp_weth_usdt_4_token": _CASSETTE_DIR / "balancer_weighted_dai_gp_weth_usdt_4_token.json",
}
_SWAP_KIND_GIVEN_IN = 0
_SWAP_KIND_GIVEN_OUT = 1

VITALIK_ADDRESS = get_checksum_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")

_AMOUNT_MULTIPLIERS = (
    0.0000001,
    0.000001,
    0.00001,
    0.0001,
    0.001,
    0.01,
    0.1,
    0.125,
    0.25,
)


def _load_cassette(path: pathlib.Path) -> dict[str, Any]:
    return json.loads(path.read_bytes())


def _build_weighted_pool(cassette: dict[str, Any]) -> BalancerV2Pool:
    """Build a Balancer V2 weighted pool I/O-free from a recorded cassette.

    A fresh ``Bot`` is created per call (the factory's documented isolation
    model) so the 19 parametrized tests don't share mutable registration state
    across the same ``Bot`` — registering overlapping token addresses
    (WETH/USDC appear in multiple pools) into one shared ``Bot`` collides and
    corrupts later tests' handles.
    """
    pybot = Bot()
    tokens = [
        make_erc20(
            pybot,
            t["address"],
            name=t["name"],
            symbol=t["symbol"],
            decimals=t["decimals"],
            chain_id=1,
        )
        for t in cassette["tokens"]
    ]
    return make_balancer_weighted_pool(
        address=cassette["address"],
        pool_id=to_bytes(cassette["pool_id"]),
        vault=cassette["vault"],
        tokens=tokens,
        balances=cassette["balances"],
        fee=Fraction(cassette["fee"]),
        weights=cassette["weights"],
        pow_version=PowVersion(cassette["pow_version"]),
        state_block=cassette["block"],
        py_bot=pybot,
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
                fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
                fork_block=BALANCER_PARITY_BLOCK,
                launch=ForkLaunchConfig(
                    storage_caching=True,
                    anvil_opts=["--accounts=0"],
                ),
            )
        return self

    def __exit__(self, *exc: object) -> None:
        if self.fork is not None:
            self.fork.close()

    def raw_call(self, to: str, data: bytes) -> bytes:
        assert self.fork is not None
        return self.fork.provider.call(to, data)


def _query_swap_callable(
    fork: _RecordFork,
    pool_id_hex: str,
    token_in: str,
    token_out: str,
    amount: int,
    swap_kind: int = _SWAP_KIND_GIVEN_IN,
) -> Any:
    """BalancerQueries ``querySwap`` oracle call (GIVEN_IN or GIVEN_OUT)."""

    def _call() -> int:
        assert fork.fork is not None
        query_contract = ContractCompat(
            BALANCERQUERIES_CONTRACT_ADDRESS,
            BALANCERQUERIES_ABI,
            fork.fork.provider,
        )
        return query_contract.functions.querySwap(
            (to_bytes(pool_id_hex), swap_kind, token_in, token_out, amount, b""),
            (VITALIK_ADDRESS, False, VITALIK_ADDRESS, False),
        ).call()

    return _call


def _run_weighted_parity(
    golden_factory,
    *,
    cassette_path: pathlib.Path,
) -> None:
    golden = golden_factory(chain_id=1, block_number=BALANCER_PARITY_BLOCK)
    cassette = _load_cassette(cassette_path)
    lp = _build_weighted_pool(cassette)
    n = len(lp.tokens)

    with _RecordFork(recording=golden.is_recording) as fork:
        for i, j in itertools.permutations(range(n), 2):
            for mult in _AMOUNT_MULTIPLIERS:
                amount = int(mult * lp.balances[i])
                if amount == 0:
                    continue
                key = (
                    f"{lp.address}|querySwap|GIVEN_IN|"
                    f"{lp.tokens[i].symbol}->{lp.tokens[j].symbol}|{amount}"
                )
                oracle = golden.check(
                    key,
                    contract=_query_swap_callable(
                        fork,
                        cassette["pool_id"],
                        lp.tokens[i].address,
                        lp.tokens[j].address,
                        amount,
                    ),
                )
                if oracle.reverted:
                    continue
                calc = lp.calculate_tokens_out_from_tokens_in(
                    token_in=lp.tokens[i],
                    token_in_quantity=amount,
                    token_out=lp.tokens[j],
                )
                assert calc == oracle.value, f"{key}: helper={calc} contract={oracle.value}"


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
def test_balancer_v2_weth_bal_query_swap(golden_factory) -> None:
    """Balancer V2 WETH/BAL 80/20 weighted: GIVEN_IN == golden(querySwap)."""
    _run_weighted_parity(
        golden_factory,
        cassette_path=_WETH_BAL_CASSETTE,
    )


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
def test_balancer_v2_usdc_weth_query_swap(golden_factory) -> None:
    """Balancer V2 USDC/WETH 50/50 weighted (mixed decimals): GIVEN_IN == golden."""
    _run_weighted_parity(
        golden_factory,
        cassette_path=_USDC_WETH_CASSETTE,
    )


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
def test_balancer_v2_weth_rpl_query_swap(golden_factory) -> None:
    """Balancer V2 WETH/RPL 80/20 weighted: GIVEN_IN == golden(querySwap)."""
    _run_weighted_parity(
        golden_factory,
        cassette_path=_WETH_RPL_CASSETTE,
    )


def _run_expanded_parity(
    golden_factory,
    *,
    cassette_path: pathlib.Path,
    given_out: bool,
) -> None:
    """Run GIVEN_IN or GIVEN_OUT querySwap parity for an expanded weighted pool.

    Multi-token pools iterate all ``N*(N-1)`` directions, matching the original
    ``test_pools_expanded.py`` loop. Reverts from the on-chain oracle are
    recorded as golden entries and replayed as skips; when the oracle reverts,
    the helper is expected to revert too (``EVMRevertError`` / any pool error),
    mirroring the original ``ContractLogicError`` handling.
    """
    golden = golden_factory(chain_id=1, block_number=BALANCER_PARITY_BLOCK)
    cassette = _load_cassette(cassette_path)
    lp = _build_weighted_pool(cassette)
    n = len(lp.tokens)
    directions = [(i, j) for i in range(n) for j in range(n) if i != j]
    swap_kind = _SWAP_KIND_GIVEN_OUT if given_out else _SWAP_KIND_GIVEN_IN
    label = "GIVEN_OUT" if given_out else "GIVEN_IN"

    with _RecordFork(recording=golden.is_recording) as fork:
        for token_in_idx, token_out_idx in directions:
            # GIVEN_IN scales amount from the token_in balance; GIVEN_OUT from
            # the token_out balance (the requested output quantity).
            reserve = lp.balances[token_out_idx] if given_out else lp.balances[token_in_idx]
            for mult in _AMOUNT_MULTIPLIERS:
                amount = int(mult * reserve)
                if amount == 0:
                    continue
                key = f"{lp.address}|querySwap|{label}|i={token_in_idx}|j={token_out_idx}|{amount}"
                oracle = golden.check(
                    key,
                    contract=_query_swap_callable(
                        fork,
                        cassette["pool_id"],
                        lp.tokens[token_in_idx].address,
                        lp.tokens[token_out_idx].address,
                        amount,
                        swap_kind,
                    ),
                )
                if oracle.reverted:
                    # On-chain reverted — the helper must revert too (or skip,
                    # for on-chain-only checks like SWAPS_DISABLED). Mirrors
                    # the original test's ContractLogicError branch.
                    try:
                        if given_out:
                            lp.calculate_tokens_in_from_tokens_out(
                                token_in=lp.tokens[token_in_idx],
                                token_out=lp.tokens[token_out_idx],
                                token_out_quantity=amount,
                            )
                        else:
                            lp.calculate_tokens_out_from_tokens_in(
                                token_in=lp.tokens[token_in_idx],
                                token_in_quantity=amount,
                                token_out=lp.tokens[token_out_idx],
                            )
                    except (EVMRevertError, ContractLogicError):
                        pass  # both reverted — OK
                    continue
                if given_out:
                    calc = lp.calculate_tokens_in_from_tokens_out(
                        token_in=lp.tokens[token_in_idx],
                        token_out=lp.tokens[token_out_idx],
                        token_out_quantity=amount,
                    )
                else:
                    calc = lp.calculate_tokens_out_from_tokens_in(
                        token_in=lp.tokens[token_in_idx],
                        token_in_quantity=amount,
                        token_out=lp.tokens[token_out_idx],
                    )
                assert calc == oracle.value, f"{key}: helper={calc} contract={oracle.value}"


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
@pytest.mark.parametrize("pool_key", list(_EXPANDED_TWO_TOKEN_CASSETTES))
def test_balancer_v2_expanded_two_token_given_in(
    golden_factory,
    pool_key: str,
) -> None:
    """Balancer V2 expanded 2-token weighted: GIVEN_IN == golden(querySwap).

    Covers diverse weight profiles (50/50, 60/40, 75/25, 90/10, 40/60), fee
    tiers (0.05%-0.30%), and mixed decimals (6/18, 8/18).
    """
    _run_expanded_parity(
        golden_factory,
        cassette_path=_EXPANDED_TWO_TOKEN_CASSETTES[pool_key],
        given_out=False,
    )


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
@pytest.mark.parametrize("pool_key", list(_EXPANDED_TWO_TOKEN_CASSETTES))
def test_balancer_v2_expanded_two_token_given_out(
    golden_factory,
    pool_key: str,
) -> None:
    """Balancer V2 expanded 2-token weighted: GIVEN_OUT == golden(querySwap)."""
    _run_expanded_parity(
        golden_factory,
        cassette_path=_EXPANDED_TWO_TOKEN_CASSETTES[pool_key],
        given_out=True,
    )


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
@pytest.mark.parametrize("pool_key", list(_EXPANDED_MULTI_TOKEN_CASSETTES))
def test_balancer_v2_expanded_multi_token_given_in(
    golden_factory,
    pool_key: str,
) -> None:
    """Balancer V2 expanded multi-token weighted (3+ tokens): GIVEN_IN == golden.

    All ``N*(N-1)`` swap directions. Covers 3-token (APU/PEPE/SPX, 18/18/8) and
    4-token (DAI/GP/WETH/USDT, 18/18/18/6) pools.
    """
    _run_expanded_parity(
        golden_factory,
        cassette_path=_EXPANDED_MULTI_TOKEN_CASSETTES[pool_key],
        given_out=False,
    )


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
@pytest.mark.parametrize("pool_key", list(_EXPANDED_MULTI_TOKEN_CASSETTES))
def test_balancer_v2_expanded_multi_token_given_out(
    golden_factory,
    pool_key: str,
) -> None:
    """Balancer V2 expanded multi-token weighted (3+ tokens): GIVEN_OUT == golden."""
    _run_expanded_parity(
        golden_factory,
        cassette_path=_EXPANDED_MULTI_TOKEN_CASSETTES[pool_key],
        given_out=True,
    )


_REPLAY_DIAL_MSG = "golden replay is offline by contract; a network dial is a defect"


def _refuse_connection(*_args: object, **_kwargs: object) -> None:
    raise AssertionError(_REPLAY_DIAL_MSG)


@pytest.fixture(autouse=True)
def _replay_makes_no_network_calls(
    request: pytest.FixtureRequest,
    monkeypatch: pytest.MonkeyPatch,
) -> Iterator[None]:
    """Arm a hard dial-block in replay; only --golden-mode=record may touch a node.

    Replay asserts recorded ints against pools built I/O-free, so any connection
    attempt means the offline contract broke — failing at the dial beats hanging
    on an unreachable endpoint. Record mode is the one sanctioned dialer (a fork
    pinned to the recorded block), so the block is armed only for replay."""
    if request.config.getoption("--golden-mode") == "record":
        yield
        return
    monkeypatch.setattr(socket, "create_connection", _refuse_connection)
    monkeypatch.setattr(socket, "getaddrinfo", _refuse_connection)
    monkeypatch.setattr(socket.socket, "connect", _refuse_connection)
    monkeypatch.setattr(socket.socket, "connect_ex", _refuse_connection)
    yield


def _weighted_golden_keys(cassette: dict[str, Any]) -> set[str]:
    """The oracle keys one base weighted parity run drives: both swap
    directions over the multiplier grid, keyed by token symbols."""
    address = cassette["address"]
    symbols = [token["symbol"] for token in cassette["tokens"]]
    balances = cassette["balances"]
    keys: set[str] = set()
    for i, j in itertools.permutations(range(len(symbols)), 2):
        for mult in _AMOUNT_MULTIPLIERS:
            amount = int(mult * balances[i])
            if amount == 0:
                continue
            keys.add(f"{address}|querySwap|GIVEN_IN|{symbols[i]}->{symbols[j]}|{amount}")
    return keys


def _expanded_golden_keys(cassette: dict[str, Any], *, given_out: bool) -> set[str]:
    """The oracle keys one expanded weighted parity run drives: all token
    directions over the multiplier grid, keyed by token indices."""
    address = cassette["address"]
    balances = cassette["balances"]
    label = "GIVEN_OUT" if given_out else "GIVEN_IN"
    keys: set[str] = set()
    for token_in_idx, token_out_idx in itertools.permutations(range(len(balances)), 2):
        # GIVEN_IN scales the amount from the token-in balance; GIVEN_OUT from
        # the token-out balance (the requested output quantity).
        reserve = balances[token_out_idx] if given_out else balances[token_in_idx]
        for mult in _AMOUNT_MULTIPLIERS:
            amount = int(mult * reserve)
            if amount == 0:
                continue
            keys.add(f"{address}|querySwap|{label}|i={token_in_idx}|j={token_out_idx}|{amount}")
    return keys


def _v2_golden_file(request: pytest.FixtureRequest, test_name: str) -> pathlib.Path:
    """One of this module's golden files, resolved like golden_factory."""
    file_part = pathlib.Path(request.path).relative_to(request.config.rootpath).as_posix()
    nodeid = f"{file_part}::{test_name}"
    rel = _nodeid_to_path(nodeid, GOLDEN_ROOT).relative_to(GOLDEN_ROOT)
    return pathlib.Path(request.config.getoption("--golden-root")) / rel


@pytest.mark.parametrize(
    ("test_name", "cassette_paths", "given_out"),
    [
        ("test_balancer_v2_weth_bal_query_swap", [_WETH_BAL_CASSETTE], None),
        ("test_balancer_v2_usdc_weth_query_swap", [_USDC_WETH_CASSETTE], None),
        ("test_balancer_v2_weth_rpl_query_swap", [_WETH_RPL_CASSETTE], None),
        (
            "test_balancer_v2_expanded_two_token_given_in",
            list(_EXPANDED_TWO_TOKEN_CASSETTES.values()),
            False,
        ),
        (
            "test_balancer_v2_expanded_two_token_given_out",
            list(_EXPANDED_TWO_TOKEN_CASSETTES.values()),
            True,
        ),
        (
            "test_balancer_v2_expanded_multi_token_given_in",
            list(_EXPANDED_MULTI_TOKEN_CASSETTES.values()),
            False,
        ),
        (
            "test_balancer_v2_expanded_multi_token_given_out",
            list(_EXPANDED_MULTI_TOKEN_CASSETTES.values()),
            True,
        ),
    ],
)
def test_golden_keys_exactly_match_the_oracle_surface(
    request: pytest.FixtureRequest,
    test_name: str,
    cassette_paths: list[pathlib.Path],
    *,
    given_out: bool | None,
) -> None:
    """The golden files hold exactly the keys this module's replay drives.

    Each expanded golden file aggregates the whole pool parametrization, so
    the expected surface is the union over its cassettes. Replay fails loud
    on a missing key but stays silent on a stale extra one — nothing looks it
    up. Set-equality against the recorded file closes that drift: a shrunken
    case list cannot leave orphaned oracle entries behind."""
    if request.config.getoption("--golden-mode") == "record":
        pytest.skip("the record run rewrites the golden file this test diffs")
    expected: set[str] = set()
    for cassette_path in cassette_paths:
        cassette = _load_cassette(cassette_path)
        if given_out is None:
            expected |= _weighted_golden_keys(cassette)
        else:
            expected |= _expanded_golden_keys(cassette, given_out=given_out)
    recorded = set(json.loads(_v2_golden_file(request, test_name).read_text())["entries"])
    assert recorded == expected, (
        f"stale={sorted(recorded - expected)} missing={sorted(expected - recorded)}"
    )
