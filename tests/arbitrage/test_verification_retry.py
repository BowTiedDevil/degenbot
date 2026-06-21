"""VP42BP (AC item 4): bounded retry-with-backoff for transient verification
RPC failures.

Background — the Rust core now distinguishes (commit ``61a67158``) two
verification failure *types* at the Python seam:

- ``VerificationMismatchError`` — a genuine on-chain tick-data divergence.
  Fatal: the bot must not operate on stale/incorrect tick data. Never retried.
- ``VerificationRpcError`` — a per-call RPC transport failure (``eth_call`` /
  ``getTickBitmap`` / ``getTickLiquidity`` could not reach the node) OR a
  verify-provider construction failure. Transient: a retry/backoff candidate.

``build_paths`` previously crashed loudly on *both* (the safe default while
no retry policy existed). This module adds a bounded retry-with-backoff that
retries only the transient ``VerificationRpcError`` category, leaving
``VerificationMismatchError`` fatal. After exhausting attempts the last
``VerificationRpcError`` is re-raised — the bot still does NOT silently
continue on unverified data.

These tests pin the retry helper's behavior through its public interface
(``retry_verification_call``). The helper builds a ``tenacity.Retrying``
(reusing the codebase's existing retry idiom from
``provider.log_fetching.fetch_logs_retrying``); a ``base_delay=0`` policy keeps
the tests fast/deterministic (no real sleeping).
"""

from __future__ import annotations

import pytest

from degenbot.degenbot_rs import (  # type: ignore[attr-defined]
    VerificationMismatchError,
    VerificationRpcError,
)


class TestRetryVerificationCall:
    """``retry_verification_call`` retries transient RPC failures only."""

    def test_succeeds_after_transient_rpcs_then_success(self) -> None:
        """A callable that raises ``VerificationRpcError`` N-1 times then
        succeeds is retried up to ``max_attempts`` and returns the success value.

        The headline VP42BP AC item 4 behavior: a transient per-call RPC blip
        does NOT abort ``build_paths`` — it's retried, and the registration
        proceeds once the node is reachable again.
        """
        from degenbot.arbitrage.verification_retry import (
            VerificationRetryPolicy,
            retry_verification_call,
        )

        calls = 0

        def flaky() -> str:
            nonlocal calls
            calls += 1
            if calls < 3:
                raise VerificationRpcError(f"tickBitmap(0) RPC call failed (blip {calls})")
            return "ok"

        policy = VerificationRetryPolicy(max_attempts=4, base_delay=0.0)
        result = retry_verification_call(policy, flaky)

        assert result == "ok"
        assert calls == 3, "retried twice (3 attempts) then succeeded"

    def test_exhausted_attempts_reraises_last_rpc_error(self) -> None:
        """Persistent ``VerificationRpcError`` is retried up to ``max_attempts``
        then re-raised — the bot still crashes loudly rather than silently
        continuing on unverified data (the safe default is preserved).
        """
        from degenbot.arbitrage.verification_retry import (
            VerificationRetryPolicy,
            retry_verification_call,
        )

        calls = 0
        last_msg = "tickBitmap(0) RPC call failed: timeout (persistent)"

        def always_fails() -> None:
            nonlocal calls
            calls += 1
            raise VerificationRpcError(last_msg)

        policy = VerificationRetryPolicy(max_attempts=3, base_delay=0.0)
        with pytest.raises(VerificationRpcError, match="persistent"):
            retry_verification_call(policy, always_fails)

        assert calls == 3, "tried exactly max_attempts times then gave up"

    def test_genuine_mismatch_propagates_immediately_without_retry(self) -> None:
        """``VerificationMismatchError`` (genuine on-chain divergence) is fatal —
        it raises on the first attempt with NO retry, even though ``max_attempts``
        is large. Retrying a mismatch would only re-observe the same divergence.
        """
        from degenbot.arbitrage.verification_retry import (
            VerificationRetryPolicy,
            retry_verification_call,
        )

        calls = 0

        def mismatches() -> None:
            nonlocal calls
            calls += 1
            raise VerificationMismatchError("tick 5 lg mismatch: on-chain=1 vs engine=2")

        policy = VerificationRetryPolicy(max_attempts=10, base_delay=0.0)
        with pytest.raises(VerificationMismatchError, match="lg mismatch"):
            retry_verification_call(policy, mismatches)

        assert calls == 1, "a genuine mismatch must NOT be retried"

    def test_non_verification_exception_propagates_without_retry(self) -> None:
        """A non-verification exception (e.g. a ``ValueError`` from a bad pool
        address) is not in the retry set — it propagates immediately on the
        first attempt, so admission/parsing failures aren't masked as
        transient RPC blips.
        """
        from degenbot.arbitrage.verification_retry import (
            VerificationRetryPolicy,
            retry_verification_call,
        )

        calls = 0

        def raises_value_error() -> None:
            nonlocal calls
            calls += 1
            raise ValueError("bad pool address")

        policy = VerificationRetryPolicy(max_attempts=5, base_delay=0.0)
        with pytest.raises(ValueError, match="bad pool address"):
            retry_verification_call(policy, raises_value_error)

        assert calls == 1, "non-verification exceptions must not be retried"


class TestVerificationRetryPolicy:
    """``VerificationRetryPolicy`` carries sane defaults + validates inputs."""

    def test_defaults_are_sensible_for_edge_node_recovery(self) -> None:
        """The default policy bounds a single ``eth_call`` round-trip + node GC
        pause without stalling ``build_paths`` startup: 4 attempts, ~0.5s base,
        capped at 4s, jittered. These are the values wired into ``BackrunConfig``
        when the user sets no ``VERIFICATION_RETRY_*`` env vars.
        """
        from degenbot.arbitrage.verification_retry import VerificationRetryPolicy

        p = VerificationRetryPolicy()
        assert p.max_attempts == 4
        assert p.base_delay == 0.5
        assert p.max_delay == 4.0
        assert 0 < p.jitter <= 1.0

    def test_policy_is_frozen(self) -> None:
        """A policy shared across registration calls must be immutable (a mid-run
        mutation would silently change retry semantics). Mirrors ``BackrunConfig``'s
        frozen contract (test_backrun_config.py::TestImmutability).
        """
        import dataclasses

        from degenbot.arbitrage.verification_retry import VerificationRetryPolicy

        p = VerificationRetryPolicy()
        with pytest.raises(dataclasses.FrozenInstanceError):
            p.max_attempts = 99  # type: ignore[misc]

    def test_rejects_invalid_bounds(self) -> None:
        """Bad policy inputs raise ``ValueError`` at construction — fail fast
        rather than silently retrying zero times or sleeping negatively.
        """
        from degenbot.arbitrage.verification_retry import VerificationRetryPolicy

        with pytest.raises(ValueError, match="max_attempts"):
            VerificationRetryPolicy(max_attempts=0)
        with pytest.raises(ValueError, match="base_delay"):
            VerificationRetryPolicy(base_delay=-0.1)
        with pytest.raises(ValueError, match="max_delay"):
            VerificationRetryPolicy(max_delay=-1.0)
        with pytest.raises(ValueError, match="jitter"):
            VerificationRetryPolicy(jitter=1.5)

    def test_build_retrier_only_retries_verification_rpc_error(self) -> None:
        """The ``tenacity`` retrier's retry predicate is ``VerificationRpcError``
        only — structural pin on the category the runtime config relies on
        (a future maintainer widening this to ``RuntimeError`` would re-introduce
        the mismatch-retry footgun). Mismatch + other exceptions fall through to
        immediate propagation (covered by ``TestRetryVerificationCall``).
        """
        from degenbot.arbitrage.verification_retry import VerificationRetryPolicy

        p = VerificationRetryPolicy(max_attempts=2, base_delay=0.0)
        retrier = p.build_retrier()
        # ``retry_if_exception_type`` exposes the accepted type(s) on
        # ``exception_types`` (a single class or a tuple). Assert the retry
        # set is EXACTLY {VerificationRpcError} — a future maintainer widening
        # it to ``RuntimeError`` would re-introduce the mismatch-retry footgun.
        from degenbot.degenbot_rs import (  # type: ignore[attr-defined]
            VerificationMismatchError,
            VerificationRpcError,
        )
        accepted = retrier.retry.exception_types
        accepted_tuple = accepted if isinstance(accepted, tuple) else (accepted,)
        assert VerificationRpcError in accepted_tuple
        assert VerificationMismatchError not in accepted_tuple
        assert len(accepted_tuple) == 1, "only VerificationRpcError may be retried"
