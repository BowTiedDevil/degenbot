"""The verification retry policy is a declared key family, resolved once.

The four ``verify.verify_retry_*`` keys carry the ``VERIFICATION_RETRY_*`` env
names, so the schema's declared default is the only copy of the numbers. They
used to be encoded in three places (the config factory, its parse helpers, and
the policy dataclass), and only the dataclass agreed with the core.

``build_paths`` reads the resolved policy off ``cfg.verification_retry_policy``
and retries a transient ``VerificationRpcError`` per registration instead of
crashing on the first blip.
"""

from __future__ import annotations

import pytest

from degenbot.arbitrage.verification_retry import _DEFAULTS
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
