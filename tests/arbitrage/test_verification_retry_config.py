"""The verification retry policy is a declared key family, resolved once.

The four ``verify.verify_retry_*`` keys carry the ``VERIFICATION_RETRY_*`` env
names, so the schema's declared default is the only copy of the numbers. They
used to be encoded in three places (the config factory, its parse helpers, and
the policy dataclass), and only the dataclass agreed with the core.

``build_paths`` reads the resolved policy off ``cfg.verification_retry_policy``
and injects it into the core-owned bounded retry dance, which retries a
transient ``VerificationRpcError`` instead of crashing on the first blip.
"""

from __future__ import annotations

import pytest

from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.runner.config import _DEFAULTS, VerificationRetryPolicy
from degenbot.runner.identity import UNISWAP_V4_POOL_MANAGER_ADDRESS
from tests.helpers import verdict_probe as probe

#: The four policy fields, as the config names them.
_POLICY_FIELDS = tuple(
    f"verification_retry_policy.{name}"
    for name in ("max_attempts", "base_delay", "max_delay", "jitter")
)


class TestVerificationRetryConfig:
    """The policy is resolved from the four declared keys."""

    def test_the_declared_defaults_match_the_core_policy(self) -> None:
        """The schema declaration and the core default must be one number.
        The dataclass keeps reading the core through the FFI module function
        and the schema declares the same four values; a test that compares them
        is what stops a second copy from drifting.
        """
        values = probe.config_values(_POLICY_FIELDS)

        assert values["verification_retry_policy.max_attempts"] == _DEFAULTS.max_attempts
        assert values["verification_retry_policy.base_delay"] == pytest.approx(
            _DEFAULTS.base_delay
        )
        assert values["verification_retry_policy.max_delay"] == pytest.approx(_DEFAULTS.max_delay)
        assert values["verification_retry_policy.jitter"] == pytest.approx(_DEFAULTS.jitter)

    def test_the_env_layer_overrides_each_knob(self) -> None:
        values = probe.config_values(
            _POLICY_FIELDS,
            env={
                "VERIFICATION_RETRY_MAX_ATTEMPTS": "6",
                "VERIFICATION_RETRY_BASE_DELAY": "0.25",
                "VERIFICATION_RETRY_MAX_DELAY": "8.0",
                "VERIFICATION_RETRY_JITTER": "0.3",
            },
        )

        assert values == {
            "verification_retry_policy.max_attempts": 6,
            "verification_retry_policy.base_delay": 0.25,
            "verification_retry_policy.max_delay": 8.0,
            "verification_retry_policy.jitter": 0.3,
        }

    def test_the_file_layer_overrides_each_knob(self) -> None:
        """The file layer reaches the policy too, which it never did before."""

        body = """[verify]
verify_retry_max_attempts = 7
verify_retry_base_delay = 0.75
verify_retry_max_delay = 9.0
verify_retry_jitter = 0.2
 """
        with probe.operator_file(body) as written:
            values = probe.config_values(_POLICY_FIELDS, operator_file=written)

        assert values["verification_retry_policy.max_attempts"] == 7
        assert values["verification_retry_policy.base_delay"] == pytest.approx(0.75)
        assert values["verification_retry_policy.max_delay"] == pytest.approx(9.0)
        assert values["verification_retry_policy.jitter"] == pytest.approx(0.2)

    def test_a_non_integer_attempt_count_is_refused_at_boot(self) -> None:
        """A typo must not silently fall back to the default."""

        # Process-level: the refusal is the process exit code at boot.
        completed = probe.run(
            "import degenbot", env={"VERIFICATION_RETRY_MAX_ATTEMPTS": "not-an-int"}
        )

        assert completed.returncode == 2, completed.stderr
        assert "verify.verify_retry_max_attempts" in completed.stderr, completed.stderr
        assert "not-an-int" in completed.stderr, completed.stderr

    def test_below_one_attempt_never_reaches_the_crawler(self) -> None:
        """Zero attempts is a count the loader accepts and the policy refuses.

        The refusal is the policy object's own bound check, raised while the
        config is built, so a misconfigured budget cannot reach
        ``build_paths``.

        """
        with pytest.raises(ValueError, match="max_attempts"):
            probe.build_config(env={"VERIFICATION_RETRY_MAX_ATTEMPTS": "0"})


def test_the_engine_registry_injects_the_parsed_policy() -> None:
    """The registry forwards every resolved knob to the retry-wrapped lifecycle.

    The retry dance lives in the core; the driver shell only injects the
    policy values, so this pins the adapter's forwarding and the stashed
    snapshot block.
    """
    class _Recorder:
        def __init__(self) -> None:
            self.calls: list[dict[str, object]] = []

        def run_v4_registration_lifecycle_with_retry_sync(
            self,
            pool_manager: str,
            pool_id_hex: str,
            snapshot_block: int | None,
            max_attempts: int,
            base_delay: float,
            max_delay: float,
            jitter: float,
        ) -> None:
            self.calls.append(
                {
                    "pool_manager": pool_manager,
                    "pool_id_hex": pool_id_hex,
                    "snapshot_block": snapshot_block,
                    "max_attempts": max_attempts,
                    "base_delay": base_delay,
                    "max_delay": max_delay,
                    "jitter": jitter,
                }
            )

    engine = _Recorder()
    registry = EngineRegistry(engine=engine)  # type: ignore[arg-type]
    registry._verify_snapshot_block = 18_000_050
    policy = VerificationRetryPolicy(
        max_attempts=6, base_delay=0.25, max_delay=8.0, jitter=0.3
    )

    registry.run_v4_verify_lifecycle_sync_with_retry(
        UNISWAP_V4_POOL_MANAGER_ADDRESS, "0x" + "ab" * 32, policy
    )

    assert engine.calls == [
        {
            "pool_manager": UNISWAP_V4_POOL_MANAGER_ADDRESS,
            "pool_id_hex": "0x" + "ab" * 32,
            "snapshot_block": 18_000_050,
            "max_attempts": 6,
            "base_delay": 0.25,
            "max_delay": 8.0,
            "jitter": 0.3,
        }
    ]
