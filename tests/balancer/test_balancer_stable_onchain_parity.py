"""Balancer V2 stable-pool on-chain parity — golden record/replay (B3).

Golden conversion of the exact-equality parity tests in
``tests/balancer/test_stable_pool_class.py``: the MetaStable pools
(``wsteth_weth``, ``cbeth_wsteth`` — no BPT token, near-static rates) and the
ComposableStable pools (``tusd_bsp``, ``bb_s_usd`` — BPT token + time-varying
rate providers). Each asserts the Python
``calculate_tokens_out_from_tokens_in`` / ``calculate_tokens_in_from_tokens_out``
exactly equals the on-chain ``BalancerQueries.querySwap`` (GIVEN_IN +
GIVEN_OUT) over all non-BPT swap directions and the amount-multiplier grid.

Pool construction: built I/O-free (ADR-005) via
:func:`make_balancer_stable_pool` from a cassette recorded at the pinned block
carrying the immutable config (address, pool_id, amp, fee, tokens,
``base_scaling_factors``, ``resolved_rates``, ``bpt_idx``,
``invariant_version``) + the live balances.

Rate resolution — the key subtlety
----------------------------------
``BalancerQueries.querySwap`` calls the pool's ``_beforeSwapJoinExit`` which
runs ``_cacheTokenRateIfNecessary``: each rate-bearing token's rate is read
from ``getTokenRateCache``; if the cache has **not** expired, the **cached**
rate is used, otherwise the rate provider's fresh ``getRate()`` is used. A
naive conversion that captures fresh ``getRate()`` for every token mismatches
by a few wei on Composable pools whose cache is valid (cached != fresh). The
cassette therefore stores **resolved rates** — the exact rate each token would
resolve to at the pinned block (cached-if-valid, fresh-if-expired) — and a
non-static :class:`_CapturedRateProvider` injects them so the pool's
``requires_io_at_calculation_time`` path (which the StaleRateResult guard
mandates for Composable pools) returns the same rates querySwap used. Verified
offline: 216/216 exact matches (4 pools x all non-BPT directions x multipliers).

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

from degenbot._ffi import Bot
from degenbot.balancer.deployments import (
    BALANCERQUERIES_CONTRACT_ADDRESS,
)
from degenbot.balancer.stable_pools import INVARIANT_V1, INVARIANT_V2
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import ContractLogicError
from degenbot.exceptions.pool import EVMRevertError
from degenbot.fork import AnvilFork
from degenbot.utils.bytes import to_bytes
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI
from tests.helpers.balancer_pool_factory import make_balancer_stable_pool
from tests.helpers.contract_compat import ContractCompat
from tests.helpers.erc20_factory import make_erc20

if TYPE_CHECKING:
    from degenbot.balancer.stable_pools import BalancerV2StablePool

BALANCER_PARITY_BLOCK = 24_407_242  # tip minus ~1M

_CASSETTE_DIR = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "chain_data" / "1"
_STABLE_CASSETTES = {
    "wsteth_weth": _CASSETTE_DIR / "balancer_stable_wsteth_weth.json",
    "cbeth_wsteth": _CASSETTE_DIR / "balancer_stable_cbeth_wsteth.json",
    "tusd_bsp": _CASSETTE_DIR / "balancer_stable_tusd_bsp.json",
    "bb_s_usd": _CASSETTE_DIR / "balancer_stable_bb_s_usd.json",
}

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
_SWAP_KIND_GIVEN_IN = 0
_SWAP_KIND_GIVEN_OUT = 1

# Minimal ABI for the deferred on-chain oracle call (record mode only).
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


class _CapturedRateProvider:
    """Non-static rate provider returning the recorded resolved rates.

    On-chain ``querySwap`` resolves each token's rate via
    ``getTokenRateCache`` (cached-if-valid, fresh-if-expired) at the pinned
    block. The cassette stores those **resolved** rates; this provider returns
    them so the pool's ``requires_io_at_calculation_time`` path (which the
    StaleRateResult guard mandates for ComposableStable pools) yields the same
    rates querySwap used. For MetaStable pools (``bpt_idx is None``) no provider
    is needed — construction-time scaling factors are exact.

    Deliberately NOT a ``_StaticRateProvider`` so the pool treats it as a live
    provider (avoiding the ``StaleRateResult`` guard that fires for Composable
    pools with static providers).
    """

    def __init__(self, rates: tuple[int, ...]) -> None:
        self._rates = rates

    def get_rates(self, block_identifier: int | str | None = None) -> tuple[int, ...]:
        return self._rates


def _load_cassette(path: pathlib.Path) -> dict[str, Any]:
    return json.loads(path.read_bytes())


def _build_stable_pool(cassette: dict[str, Any]) -> BalancerV2StablePool:
    """Build a Balancer V2 stable pool I/O-free from a recorded cassette.

    A fresh ``Bot`` per call (isolation across parametrized tests).
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
    bpt_idx = cassette["bpt_idx"]
    # Composable pools (bpt_idx is not None) need a non-static rate provider so
    # the StaleRateResult guard does not fire; MetaStable pools don't.
    rate_provider = (
        _CapturedRateProvider(tuple(cassette["resolved_rates"])) if bpt_idx is not None else None
    )
    invariant_version = INVARIANT_V1 if cassette["invariant_version"] == 1 else INVARIANT_V2
    return make_balancer_stable_pool(
        address=cassette["address"],
        pool_id=to_bytes(cassette["pool_id"]),
        vault=cassette["vault"],
        tokens=tokens,
        balances=cassette["balances"],
        fee=Fraction(cassette["fee"]),
        amp=cassette["amp"],
        scaling_factors=cassette["resolved_scaling_factors"],
        base_scaling_factors=cassette["base_scaling_factors"],
        bpt_idx=bpt_idx,
        rate_provider=rate_provider,
        invariant_version=invariant_version,
        state_block=cassette["block"],
        py_bot=pybot,
    )


class _RecordFork(AbstractContextManager):
    """One pinned Ethereum fork shared across the test's whole record pass.

    In replay ``fork`` stays ``None``; the deferred ``contract=`` callables
    are never invoked.
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


def _query_swap_callable(
    fork: _RecordFork,
    pool_id_hex: str,
    token_in: str,
    token_out: str,
    amount: int,
    swap_kind: int,
) -> Any:
    """BalancerQueries ``querySwap`` oracle call (GIVEN_IN or GIVEN_OUT)."""

    def _call() -> int:
        assert fork.fork is not None
        query_contract = ContractCompat(
            BALANCERQUERIES_CONTRACT_ADDRESS,
            _BALANCERQUERIES_ABI,
            fork.fork.provider,
        )
        return query_contract.functions.querySwap(
            (to_bytes(pool_id_hex), swap_kind, token_in, token_out, amount, b""),
            (VITALIK_ADDRESS, False, VITALIK_ADDRESS, False),
        ).call()

    return _call


def _run_stable_parity(
    golden_factory,
    *,
    cassette_path: pathlib.Path,
    given_out: bool,
) -> None:
    """Run GIVEN_IN or GIVEN_OUT querySwap parity for a Balancer V2 stable pool.

    Iterates all non-BPT ``N*(N-1)`` swap directions, matching the original
    ``test_stable_pool_class.py`` loop. Reverts from the on-chain oracle are
    recorded as golden entries and replayed as skips; when the oracle reverts,
    the helper is expected to revert too.
    """
    golden = golden_factory(chain_id=1, block_number=BALANCER_PARITY_BLOCK)
    cassette = _load_cassette(cassette_path)
    lp = _build_stable_pool(cassette)
    bpt_idx = cassette["bpt_idx"]
    n = len(lp.tokens)
    swap_kind = _SWAP_KIND_GIVEN_OUT if given_out else _SWAP_KIND_GIVEN_IN
    label = "GIVEN_OUT" if given_out else "GIVEN_IN"

    with _RecordFork(recording=golden.is_recording) as fork:
        for token_in_idx, token_out_idx in itertools.permutations(range(n), 2):
            if token_in_idx == bpt_idx or token_out_idx == bpt_idx:
                continue  # skip BPT swaps
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
                        pass
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
@pytest.mark.parametrize("pool_key", list(_STABLE_CASSETTES))
def test_balancer_v2_stable_query_swap_given_in(
    golden_factory,
    pool_key: str,
) -> None:
    """Balancer V2 stable pool: GIVEN_IN == golden(querySwap).

    Covers MetaStable pools (wstETH/WETH, cbETH/wstETH — near-static rates, no
    BPT) and ComposableStable pools (TUSD BSP, bb-s-USD — BPT token +
    time-varying rate providers with cache-aware rate resolution).
    """
    _run_stable_parity(
        golden_factory,
        cassette_path=_STABLE_CASSETTES[pool_key],
        given_out=False,
    )


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
@pytest.mark.parametrize("pool_key", list(_STABLE_CASSETTES))
def test_balancer_v2_stable_query_swap_given_out(
    golden_factory,
    pool_key: str,
) -> None:
    """Balancer V2 stable pool: GIVEN_OUT == golden(querySwap)."""
    _run_stable_parity(
        golden_factory,
        cassette_path=_STABLE_CASSETTES[pool_key],
        given_out=True,
    )
