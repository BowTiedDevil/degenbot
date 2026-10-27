"""The registration-lifecycle value: one shape for target, mode, and policy.

``run_registration_lifecycle`` takes a :class:`RegistrationLifecycle` value
carrying the family, the target, the snapshot seed, the mode, and (for the
retry dance) the policy. The value's constructor refuses a family/mode/target
combination the core could not run, and the seam refuses a target the core
cannot parse with the historical ``ValueError`` vocabulary — before it ever
reaches the driver.
"""

from __future__ import annotations

import pytest

from tests.helpers.rpc_env import rpc_env
from tests.helpers.seam import ArbitrageEngine, Bot, RegistrationLifecycle, RetryPolicy

_V3_ADDRESS = "0x" + "11" * 20


@pytest.fixture(autouse=True)
def _rpc_env():
    with rpc_env():
        yield


def _engine() -> ArbitrageEngine:
    return ArbitrageEngine(py_bot=Bot(1))


def test_the_value_carries_the_family_target_mode_and_seed() -> None:
    """The mode and the target ride the value, not the method name."""
    value = RegistrationLifecycle(family="v3", mode="async", address=_V3_ADDRESS, snapshot_block=7)

    assert value.family == "v3"
    assert value.mode == "async"
    assert value.address == _V3_ADDRESS
    assert value.pool_manager is None
    assert value.pool_id_hex is None
    assert value.snapshot_block == 7
    assert value.policy is None


def test_a_target_that_does_not_match_the_family_is_refused() -> None:
    with pytest.raises(ValueError, match="v3 registration lifecycle targets an `address`"):
        RegistrationLifecycle(family="v3", mode="blocking")

    with pytest.raises(ValueError, match="pair"):
        RegistrationLifecycle(family="v4", mode="blocking", pool_manager=_V3_ADDRESS)


def test_the_retry_mode_and_the_policy_must_travel_together() -> None:
    with pytest.raises(ValueError, match="requires a policy"):
        RegistrationLifecycle(family="v3", mode="blocking_with_retry", address=_V3_ADDRESS)

    with pytest.raises(ValueError, match="only meaningful for mode 'blocking_with_retry'"):
        RegistrationLifecycle(
            family="v3",
            mode="blocking",
            address=_V3_ADDRESS,
            policy=RetryPolicy(),
        )


def test_an_unknown_family_or_mode_is_refused() -> None:
    with pytest.raises(ValueError, match="family must be 'v3' or 'v4'"):
        RegistrationLifecycle(family="v5", mode="blocking", address=_V3_ADDRESS)

    with pytest.raises(ValueError, match="mode must be"):
        RegistrationLifecycle(family="v3", mode="blocking-with-retry", address=_V3_ADDRESS)


def test_an_unparseable_v3_address_raises_the_typed_target_refusal() -> None:
    """The negative probe: an unusable target is refused through the value."""
    engine = _engine()
    value = RegistrationLifecycle(family="v3", mode="blocking", address="not-an-address")

    with pytest.raises(ValueError, match="Invalid V3 address"):
        engine.run_registration_lifecycle(value)


def test_a_bad_v4_pool_id_hex_raises_the_typed_target_refusal() -> None:
    engine = _engine()
    value = RegistrationLifecycle(
        family="v4", mode="blocking", pool_manager=_V3_ADDRESS, pool_id_hex="zz"
    )

    with pytest.raises(ValueError, match="Invalid pool_id"):
        engine.run_registration_lifecycle(value)
