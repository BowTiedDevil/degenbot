"""A refused build names WHY — and never names the wrong family.

A broken Curve V1 pool, a broken Balancer V2 pool, and a Balancer pool of a
shape no construction arm models are all refused with the same exception type
(``SpecViolationError``), so a caller can skip by type. The message carries the
reason: "pool build refused: out of spec (curve broken pool)", "(balancer broken
pool)", "(balancer unmodeled family)". A bare "pool build refused: out of spec"
cannot tell a decommissioned Curve pool from a Balancer pool whose swaps are
disabled at the contract — and a Balancer identity that fell through to the
Curve arm named a family that never owned the pool.

The reason vocabulary is family-spanning (one refusal taxonomy), so this leg
lives beside the Balancer suites but asserts one address per family.
"""

import pytest

from degenbot.exceptions import SpecViolationError
from degenbot.fork import AnvilFork
from tests.sweep_pin import SWEEP_FORK_BLOCK, assert_fork_at_sweep_pin

pytestmark = pytest.mark.online_rpc


# `BROKEN_CURVE_V1_POOLS[0]` — a decommissioned V1 pool whose getters no
# longer answer.
_BROKEN_CURVE_POOL = "0x110cc323ca53d622469EdD217387E2E6B33F1dF5"

# `BROKEN_BALANCER_V2_POOLS[0]` — swaps disabled at the contract (BAL#327).
_BROKEN_BALANCER_POOL = "0x753BD6a5bF0b14ae7e5d2877e5cD6a3398aA2AAB"

# `bb-s-DAI` — an ERC4626 linear pool. `getPoolId()` answers (the Vault
# registered it, and no other family the engine builds defines that getter)
# while neither `getNormalizedWeights()` nor `getAmplificationParameter()`
# does, so the identity is positively Balancer and the shape is one no
# construction arm models.
_BALANCER_LINEAR_POOL = "0x2B218683178d029BAB6c9789b1073aA6c96E5176"


@pytest.mark.parametrize("fork_mainnet_archive", [SWEEP_FORK_BLOCK], indirect=True)
def test_broken_pools_are_refused_with_distinct_named_reasons(
    bot_mainnet_archive,
    fork_mainnet_archive: AnvilFork,
) -> None:
    assert_fork_at_sweep_pin(fork_mainnet_archive)

    with pytest.raises(SpecViolationError) as curve_err:
        bot_mainnet_archive.build_pool(_BROKEN_CURVE_POOL, state_block=SWEEP_FORK_BLOCK)
    with pytest.raises(SpecViolationError) as balancer_err:
        bot_mainnet_archive.build_pool(_BROKEN_BALANCER_POOL, state_block=SWEEP_FORK_BLOCK)

    curve_message = str(curve_err.value)
    balancer_message = str(balancer_err.value)

    assert "curve broken pool" in curve_message, curve_message
    assert "balancer broken pool" in balancer_message, balancer_message
    assert curve_message != balancer_message, "the two families must not render one message"


@pytest.mark.parametrize("fork_mainnet_archive", [SWEEP_FORK_BLOCK], indirect=True)
def test_unmodeled_balancer_family_is_refused_by_its_own_name(
    bot_mainnet_archive,
    fork_mainnet_archive: AnvilFork,
) -> None:
    """A positively identified Balancer pool of an unmodeled shape refuses as
    its own family — never as a Curve pool.

    The identity (``getPoolId()`` answered) is what keeps the address out of the
    Curve arm: the refusal must name the Balancer family, because a caller
    cannot act on "curve required read reverted" for a pool no Curve builder
    ever owned.
    """
    assert_fork_at_sweep_pin(fork_mainnet_archive)

    with pytest.raises(SpecViolationError) as refusal:
        bot_mainnet_archive.build_pool(_BALANCER_LINEAR_POOL, state_block=SWEEP_FORK_BLOCK)

    message = str(refusal.value)
    assert message == "pool build refused: out of spec (balancer unmodeled family)", message
    assert "curve" not in message, message
