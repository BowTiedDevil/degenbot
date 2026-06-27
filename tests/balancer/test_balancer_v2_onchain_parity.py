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

Pinned to Ethereum mainnet block 24,407,242, served by the local archive node
(``host.containers.internal:8545`` in the devcontainer).
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

from degenbot.anvil_fork import AnvilFork
from degenbot.balancer.deployments import (
    BALANCERQUERIES_CONTRACT_ADDRESS,
)
from degenbot.balancer.libraries.constants import PowVersion
from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyBot
from tests.helpers.balancer_pool_factory import make_balancer_weighted_pool
from tests.helpers.erc20_factory import make_erc20

if TYPE_CHECKING:
    from degenbot.balancer.pools import BalancerV2Pool

ETHEREUM_RPC_URI = "http://host.containers.internal:8545/"
BALANCER_PARITY_BLOCK = 24_407_242  # tip minus ~1M

_CASSETTE_DIR = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "chain_data" / "1"
_WETH_BAL_CASSETTE = _CASSETTE_DIR / "balancer_weighted_weth_bal.json"
_USDC_WETH_CASSETTE = _CASSETTE_DIR / "balancer_weighted_usdc_weth.json"
_WETH_RPL_CASSETTE = _CASSETTE_DIR / "balancer_weighted_weth_rpl.json"

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

_PYBOT = PyBot()


def _load_cassette(path: pathlib.Path) -> dict[str, Any]:
    return json.loads(path.read_bytes())


def _build_weighted_pool(cassette: dict[str, Any]) -> BalancerV2Pool:
    """Build a Balancer V2 weighted pool I/O-free from a recorded cassette."""
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
        py_bot=_PYBOT,
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
) -> Any:
    """BalancerQueries ``querySwap`` (GIVEN_IN) oracle call."""

    def _call() -> int:
        query_contract = fork.fork.w3.eth.contract(  # type: ignore[union-attr]
            address=BALANCERQUERIES_CONTRACT_ADDRESS,
            abi=_BALANCERQUERIES_ABI,
        )
        return query_contract.functions.querySwap(
            (Web3.to_bytes(hexstr=pool_id_hex), 0, token_in, token_out, amount, b""),
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
