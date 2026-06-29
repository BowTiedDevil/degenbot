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
from contextlib import AbstractContextManager
from fractions import Fraction
from typing import TYPE_CHECKING, Any, Self

import pytest
from hexbytes import HexBytes
from web3 import Web3
from web3.exceptions import ContractLogicError

from degenbot.anvil_fork import AnvilFork
from degenbot.balancer.deployments import (
    BALANCERQUERIES_CONTRACT_ADDRESS,
)
from degenbot.balancer.libraries.constants import PowVersion
from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyBot
from degenbot.exceptions.pool import EVMRevertError
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI
from tests.helpers.balancer_pool_factory import make_balancer_weighted_pool
from tests.helpers.erc20_factory import make_erc20

if TYPE_CHECKING:
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

# Minimal ABIs for the deferred on-chain oracle calls (record mode only).
_BALANCERQUERIES_ABI: list[dict[str, Any]] = [
    {
        "inputs": [
            {
                "components": [
                    {"name": "poolId", "type": "bytes32"},
                    {"name": "kind", "type": "uint8"},
                    {"name": "assetIn", "type": "address"},
                    {"name": "assetOut", "type": "address"},
                    {"name": "amount", "type": "uint256"},
                    {"name": "userData", "type": "bytes"},
                ],
                "internalType": "struct IVault.SingleSwap",
                "name": "singleSwap",
                "type": "tuple",
            },
            {
                "components": [
                    {"name": "sender", "type": "address"},
                    {"name": "fromInternalBalance", "type": "bool"},
                    {"name": "recipient", "type": "address"},
                    {"name": "toInternalBalance", "type": "bool"},
                ],
                "internalType": "struct IVault.FundManagement",
                "name": "funds",
                "type": "tuple",
            },
        ],
        "name": "querySwap",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "nonpayable",
        "type": "function",
    },
]


def _load_cassette(path: pathlib.Path) -> dict[str, Any]:
    return json.loads(path.read_bytes())


def _build_weighted_pool(cassette: dict[str, Any]) -> BalancerV2Pool:
    """Build a Balancer V2 weighted pool I/O-free from a recorded cassette.

    A fresh ``PyBot`` is created per call (the factory's documented isolation
    model) so the 19 parametrized tests don't share mutable registration state
    across the same ``Bot`` — registering overlapping token addresses
    (WETH/USDC appear in multiple pools) into one shared ``PyBot`` collides and
    corrupts later tests' handles.
    """
    pybot = PyBot()
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
        pool_id=HexBytes(cassette["pool_id"]),
        vault=cassette["vault"],
        tokens=tokens,
        balances=cassette["balances"],
        fee=Fraction(cassette["fee"]),
        weights=cassette["weights"],
        pow_version=PowVersion(cassette["pow_version"]),
        chain_id=1,
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
        query_contract = fork.fork.w3.eth.contract(  # type: ignore[union-attr]
            address=BALANCERQUERIES_CONTRACT_ADDRESS,
            abi=_BALANCERQUERIES_ABI,
        )
        return query_contract.functions.querySwap(
            (Web3.to_bytes(hexstr=pool_id_hex), swap_kind, token_in, token_out, amount, b""),
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
