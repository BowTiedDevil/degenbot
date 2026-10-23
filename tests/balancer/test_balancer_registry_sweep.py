"""The Balancer V2 registry sweep: every registered pool builds or is refused typed.

The epic's compliance statement for this family is "every registry/factory pool
either builds or is refused as typed Spec with a named reason". Balancer V2 has
no enumerable on-chain registry to sweep — `getPoolCount()`, `getPools()` and
`getPoolAt()` revert on every factory revision, and only
`isPoolFromFactory(address)` answers — so the pool universe is pinned as a
checked-in corpus instead of discovered at run time.

`tests/balancer/data/balancer_v2_pool_registrations_<pin>.json` is one scan of
the Vault's `PoolRegistered(bytes32 indexed poolId, address indexed poolAddress,
uint8 specialization)` log from the Vault's deployment to the pin
(`scripts/record_balancer_pool_registrations.py`), with each pool's family
classified at the pin by the production identity probe (a decodable `getPoolId`
first, then `probe_balancer_pool_type`). The corpus is data, not a curated
allowlist: a pool's absence would hide a failure, so the generator keys one row
per registered address and the test replays every row.

A row whose family is not `weighted`/`stable` is EXCLUDED from the build attempt
— no construction arm models it. That bucket is the generator's mechanical
classification at the pin, not a hand-curated list, and the sweep prints its
size: linear pools, boosted pools' linear constituents, gyro/quant-amm shapes,
and spam registrations whose address answers no pool getter at all (an EIP-7702
delegation designator with no pool code).

The pin is `tests/sweep_pin.py`'s `SWEEP_FORK_BLOCK` — one replayable number
for both families, and the one place that records the block's semantics and any
advance. A head-forking sweep is not a gate: the historical Balancer fork
failures were exactly head-dependent, see
`.ergo-notes/balancer-builder-compliance-analysis.md`.

The refusal check is BY TYPE, never by message: a `SpecViolationError` is the
one legitimate refusal, and its reason must be one of the named
`SpecRefusal` strings (Rust `registration_refusal.rs`). Every other exception
is a failure — that is the leg that keeps a registered pool's shape from
crashing instead of refusing. It is what turned three measured shapes into
named refusals rather than sweep failures: a composable weighted pool (weights
one short of the Vault token vector), a pool whose rate provider reverts (a
nested pool killed or in recovery mode), and a pool whose token answers
`decimals()` with nothing (self-destructed after registration).
"""

from __future__ import annotations

import json
from collections import Counter
from pathlib import Path

import pytest

from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.exceptions import SpecViolationError
from degenbot.fork import AnvilFork
from tests.sweep_pin import SWEEP_FORK_BLOCK, assert_fork_at_sweep_pin

_CORPUS_PATH = (
    Path(__file__).parent / "data" / f"balancer_v2_pool_registrations_{SWEEP_FORK_BLOCK}.json"
)

# `SpecRefusal::as_str` for every arm of the refusal taxonomy. A refusal the
# builders can raise but this set does not name would be a silent vocabulary
# drift between the Rust enum and this test, so the check fails closed.
_NAMED_SPEC_REFUSALS = frozenset({
    "v2 reserve out of bounds",
    "balancer broken pool",
    "balancer composable weighted pool",
    "balancer rate provider unreadable",
    "curve broken pool",
    "curve required read reverted",
    "curve too few coins",
    "curve coins/balances mismatch",
    "curve coin decimals reverted",
    "no-code coin",
})

_SPEC_MESSAGE_PREFIX = "pool build refused: out of spec"

# The built pool's class must match the corpus family: a weighted pool that
# builds as a stable pool (or vice versa) is a misclassification, not a build.
_FAMILY_CLASS = {
    "stable": BalancerV2StablePool,
    "weighted": BalancerV2Pool,
}


def _load_corpus() -> dict:
    with _CORPUS_PATH.open() as f:
        return json.load(f)


def _named_refusal_reason(exc: SpecViolationError) -> str:
    """The refusal's named reason, asserted to be a known one.

    The FFI renders a refusal as ``pool build refused: out of spec (<reason>)``
    (`errmap.rs::map_build_failure`), so the reason is the parenthesized suffix.
    """
    message = str(exc)
    assert message.startswith(_SPEC_MESSAGE_PREFIX), message
    suffix = message[len(_SPEC_MESSAGE_PREFIX) :].strip()
    assert suffix.startswith("(") and suffix.endswith(")"), message
    reason = suffix[1:-1]
    assert reason in _NAMED_SPEC_REFUSALS, (
        f"unnamed spec refusal {reason!r} — the Rust SpecRefusal vocabulary and "
        f"this test's _NAMED_SPEC_REFUSALS are out of lock-step ({message})"
    )
    return reason


@pytest.mark.online_rpc
@pytest.mark.parametrize("fork_mainnet_archive", [SWEEP_FORK_BLOCK], indirect=True)
def test_every_registered_pool_builds_or_is_refused_typed(
    bot_mainnet_archive,
    fork_mainnet_archive: AnvilFork,
) -> None:
    """Build every pool the Vault registered before the pin.

    The census is the epic's evidence line: n built (by family) / n refused (by
    named reason). Any non-`SpecViolationError` exception is collected and
    raised at the end, so one bad pool names itself without hiding the rest of
    the census.
    """
    assert_fork_at_sweep_pin(fork_mainnet_archive)

    corpus = _load_corpus()
    assert corpus["pin_block"] == SWEEP_FORK_BLOCK
    pools = corpus["pools"]
    assert pools, "empty corpus"

    built: Counter[str] = Counter()
    refused: Counter[str] = Counter()
    excluded: list[str] = []
    failures: list[str] = []

    for index, row in enumerate(pools, start=1):
        address = row["address"]
        family = row["family"]
        if family not in _FAMILY_CLASS:
            # The corpus family is the production identity probe's answer at the
            # pin, so "not a modelled family" means no construction arm can
            # build this address: a linear pool, a boosted pool's linear
            # constituent, a gyro/quant-amm shape, or a spam registration that
            # answers no pool getter at all. The set is the generator's
            # mechanical classification, never a hand-curated list.
            excluded.append(address)
            continue
        if index % 250 == 0:
            print(
                f"  swept {index}/{len(pools)} — built={sum(built.values())} "
                f"refused={sum(refused.values())} failed={len(failures)}",
                flush=True,
            )
        try:
            pool = bot_mainnet_archive.build_pool(address, state_block=SWEEP_FORK_BLOCK)
        except SpecViolationError as refusal:
            refused[_named_refusal_reason(refusal)] += 1
            continue
        except Exception as error:  # noqa: BLE001 — collected, reported as a census line
            failures.append(
                f"{type(error).__name__}: {error} — pool {index}/{len(pools)} @ {address} "
                f"(corpus family {family!r}), block {SWEEP_FORK_BLOCK}. Reproduce: "
                f'bot.build_pool("{address}", state_block={SWEEP_FORK_BLOCK})'
            )
            continue

        expected_class = _FAMILY_CLASS[family]
        if not isinstance(pool, expected_class):
            failures.append(
                f"misclassified: built as {type(pool).__name__}, corpus family {family!r} — "
                f"pool {index}/{len(pools)} @ {address}"
            )
            continue
        built[family] += 1

    print(
        f"Balancer V2 registry sweep census @ block {SWEEP_FORK_BLOCK}: "
        f"{len(pools)} registered pools — "
        f"built {sum(built.values())} {dict(sorted(built.items()))}, "
        f"refused {sum(refused.values())} {dict(sorted(refused.items()))}, "
        f"excluded {len(excluded)} (no weighted/stable identity at the pin), "
        f"failed {len(failures)}",
        flush=True,
    )
    assert sum(built.values()) + sum(refused.values()) + len(excluded) == len(pools)
    for failure in failures:
        print(f"  FAILURE {failure}", flush=True)

    assert not failures, (
        f"{len(failures)} of {len(pools)} registered Balancer pools neither built nor were "
        f"refused with a named spec reason; first: {failures[0]}"
    )


def test_corpus_is_the_pinned_vault_registration_scan() -> None:
    """The corpus is the Vault's own registration log, not a curated list.

    Offline: pins the provenance header so a hand-edited or re-scanned corpus
    cannot quietly change what the sweep claims to cover.
    """
    corpus = _load_corpus()

    assert corpus["vault"] == "0xBA12222222228d8Ba445958a75a0704d566BF2C8"
    assert corpus["event"] == "PoolRegistered(bytes32,address,uint8)"
    assert corpus["topic0"] == (
        "0x3c13bc30b8e878c53fd2a36b679409c073afd75950be43d8858768e956fbc20e"
    )
    assert corpus["pin_block"] == SWEEP_FORK_BLOCK
    pools = corpus["pools"]
    # One registration per pool address, and every row classified.
    addresses = [row["address"] for row in pools]
    assert len(addresses) == len(set(addresses))
    assert all(row["family"] in {*_FAMILY_CLASS, "unknown"} for row in pools)
    census = Counter(row["family"] for row in pools)
    assert dict(census) == corpus["census"], (dict(census), corpus["census"])
    # Every pool row's id embeds its address (the Vault's pool-id layout).
    assert all(row["pool_id"][2:42].lower() == row["address"][2:].lower() for row in pools)
