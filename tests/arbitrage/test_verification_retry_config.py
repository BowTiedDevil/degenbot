"""VP42BP (AC item 4): runtime-configurable verification retry policy.

``BackrunConfig`` exposes the bounded retry-with-backoff policy (see
``degenbot.arbitrage.verification_retry``) as read-from-env knobs
(``VERIFICATION_RETRY_MAX_ATTEMPTS`` / ``_BASE_DELAY`` / ``_MAX_DELAY`` /
``_JITTER``) with the same sane defaults as ``VerificationRetryPolicy()``.
``build_paths`` reads the resolved policy off ``cfg.verification_retry_policy``
and retries transient ``VerificationRpcError`` per registration instead of
crashing on the first blip.
"""

from __future__ import annotations

import pytest

from degenbot.arbitrage.verification_retry import VerificationRetryPolicy
from examples.eth_backrun_helpers import BackrunConfig


def _full_env() -> dict[str, str]:
    """A complete env dict exercising every field (mirrors test_backrun_config)."""
    return {
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
        "NODE_HOST_HTTP": "https://eth.example.com",
        "NODE_PORT_HTTP": "8545",
        "NODE_HOST_WEBSOCKET": "wss://ws.eth.example.com",
        "NODE_PORT_WEBSOCKET": "8546",
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
        "INJECT_EXECUTOR_CODE": "0",
        "INJECTED_EXECUTOR_ADDRESS": "0x0D6d4c3cF3BD3b769De1821f2BE0d7d99913E4F1",
        "EXECUTOR_OWNER_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
    }


class TestVerificationRetryConfig:
    """``BackrunConfig.from_env`` resolves a ``VerificationRetryPolicy``."""

    def test_defaults_match_verification_retry_policy_defaults(self) -> None:
        """When no ``VERIFICATION_RETRY_*`` env vars are set, the resolved policy
        equals ``VerificationRetryPolicy()`` — the documented sane defaults.
        """
        cfg = BackrunConfig.from_env(_full_env(), live=False, permutation=None)
        assert cfg.verification_retry_policy == VerificationRetryPolicy()

    def test_env_overrides_each_knob(self) -> None:
        """Each ``VERIFICATION_RETRY_*`` env var overrides its policy field."""
        env = _full_env() | {
            "VERIFICATION_RETRY_MAX_ATTEMPTS": "6",
            "VERIFICATION_RETRY_BASE_DELAY": "0.25",
            "VERIFICATION_RETRY_MAX_DELAY": "8.0",
            "VERIFICATION_RETRY_JITTER": "0.3",
        }
        cfg = BackrunConfig.from_env(env, live=False, permutation=None)
        assert cfg.verification_retry_policy == VerificationRetryPolicy(
            max_attempts=6,
            base_delay=0.25,
            max_delay=8.0,
            jitter=0.3,
        )

    def test_invalid_env_value_raises(self) -> None:
        """A non-integer ``VERIFICATION_RETRY_MAX_ATTEMPTS`` raises — fail fast
        rather than silently falling back to defaults (masking a typo'd env var).
        """
        env = _full_env() | {"VERIFICATION_RETRY_MAX_ATTEMPTS": "not-an-int"}
        with pytest.raises(ValueError, match="VERIFICATION_RETRY_MAX_ATTEMPTS"):
            BackrunConfig.from_env(env, live=False, permutation=None)

    def test_below_one_attempts_raises(self) -> None:
        """``max_attempts < 1`` is rejected by ``VerificationRetryPolicy``'s
        validation, surfaced at ``from_env`` time so a misconfigured env never
        reaches ``build_paths``.
        """
        env = _full_env() | {"VERIFICATION_RETRY_MAX_ATTEMPTS": "0"}
        with pytest.raises(ValueError, match="max_attempts"):
            BackrunConfig.from_env(env, live=False, permutation=None)
