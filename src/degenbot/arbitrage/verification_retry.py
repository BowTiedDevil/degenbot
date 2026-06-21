"""Bounded retry-with-backoff for transient verification RPC failures (VP42BP).

The Rust core (commit ``61a67158``) splits verification failures into two
Python exception *types*:

- ``VerificationMismatchError`` — a genuine on-chain tick-data divergence.
  Fatal; never retried (trading on stale/incorrect tick data loses money).
- ``VerificationRpcError`` — a per-call RPC transport failure
  (``eth_call`` / ``getTickBitmap`` / ``getTickLiquidity`` could not reach the
  node) or a verify-provider construction failure. Transient; a retry/backoff
  candidate.

``build_paths`` used to crash loudly on *both* — the safe default absent a
retry policy. This module adds a bounded retry-with-backoff that retries only
the transient ``VerificationRpcError`` category, leaving
``VerificationMismatchError`` fatal. After exhausting attempts the last
``VerificationRpcError`` is re-raised (``reraise=True``) — the bot still does
NOT silently continue on unverified data.

Uses ``tenacity`` (the codebase's existing retry idiom, also used by
``degenbot.provider.log_fetching.fetch_logs_retrying``) so the policy composes
with the rest of the retry machinery: exponential wait with jitter, bounded
attempts, retry only on ``VerificationRpcError``.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING

from tenacity import (
    Retrying,
    retry_if_exception_type,
    stop_after_attempt,
    wait_exponential_jitter,
)

from degenbot.degenbot_rs import VerificationRpcError  # type: ignore[attr-defined]

if TYPE_CHECKING:
    from collections.abc import Callable

__all__ = ["VerificationRetryPolicy", "retry_verification_call"]

# Default policy — sane for a local/edge node recovering from a transient
# transport blip: up to 4 attempts, ~0.5s → ~1s → ~2s base delays (before
# jitter), capped at 4s. Total worst-case wait ≈ 7.5s before giving up loudly.
# Chosen to comfortably cover a single dropped/retried ``eth_call`` round-trip
# or a brief node GC pause without stalling ``build_paths`` startup.
_DEFAULT_MAX_ATTEMPTS = 4
_DEFAULT_BASE_DELAY = 0.5
_DEFAULT_MAX_DELAY = 4.0
_DEFAULT_JITTER = 0.5


@dataclasses.dataclass(frozen=True, slots=True)
class VerificationRetryPolicy:
    """Bounded retry-with-backoff policy for transient verification RPC failures.

    Retries only ``VerificationRpcError`` (transient per-call transport /
    provider-init). ``VerificationMismatchError`` (genuine on-chain
    divergence) is never retried — it propagates immediately on the first
    attempt. Frozen so a policy can be shared across registration calls without
    risk of mutation.

    Defaults (``max_attempts=4``, ``base_delay=0.5s``, ``max_delay=4.0s``,
    ``jitter=0.5``) suit a local/edge node recovering from a transient
    transport blip; override via the constructor or ``BackrunConfig.from_env``
    (``VERIFICATION_RETRY_*`` env vars) to tune for production node latency.
    """

    max_attempts: int = _DEFAULT_MAX_ATTEMPTS
    base_delay: float = _DEFAULT_BASE_DELAY
    max_delay: float = _DEFAULT_MAX_DELAY
    jitter: float = _DEFAULT_JITTER

    def __post_init__(self) -> None:
        """Validate the policy's bounds at construction (fail fast).

        Raises:
            ValueError: If any field is out of its valid range
                (``max_attempts < 1``, ``base_delay``/``max_delay`` negative,
                ``jitter`` outside ``[0, 1]``, or ``base_delay > max_delay``).

        """
        if self.max_attempts < 1:
            msg = f"max_attempts must be >= 1, got {self.max_attempts}"
            raise ValueError(msg)
        if self.base_delay < 0:
            msg = f"base_delay must be >= 0, got {self.base_delay}"
            raise ValueError(msg)
        if self.max_delay < 0:
            msg = f"max_delay must be >= 0, got {self.max_delay}"
            raise ValueError(msg)
        if not 0 <= self.jitter <= 1:
            msg = f"jitter must be in [0, 1], got {self.jitter}"
            raise ValueError(msg)
        if self.base_delay > self.max_delay and self.max_delay > 0:
            msg = f"base_delay ({self.base_delay}) must be <= max_delay ({self.max_delay})"
            raise ValueError(msg)

    def build_retrier(self) -> Retrying:
        """Build the ``tenacity`` retrier for this policy.

        Returns:
            A ``tenacity.Retrying`` configured to retry only
            ``VerificationRpcError``, bounded by ``max_attempts`` with
            exponential backoff + jitter.

        Wrapped in a method so callers/tests can inspect the retry config;
        ``retry_verification_call`` is the thin public entry point.

        """
        return Retrying(
            stop=stop_after_attempt(self.max_attempts),
            wait=wait_exponential_jitter(
                initial=self.base_delay,
                max=self.max_delay,
                jitter=self.jitter,
            ),
            retry=retry_if_exception_type(VerificationRpcError),
            reraise=True,
        )


def retry_verification_call[T](  # type: ignore[misc]
    policy: VerificationRetryPolicy,
    fn: Callable[..., T],
    /,
    *args: object,
    **kwargs: object,
) -> T:
    """Call ``fn(*args, **kwargs)`` with bounded retry on ``VerificationRpcError``.

    - ``VerificationRpcError`` is retried (transient transport / provider-init)
      up to ``policy.max_attempts`` with exponential backoff + jitter.
    - ``VerificationMismatchError`` propagates immediately (genuine on-chain
      divergence — never retried; it is not in the retry set).
    - Any other exception propagates immediately (not retried).
    - After exhausting attempts, the last ``VerificationRpcError`` is re-raised
      (``reraise=True``) so the bot still crashes loudly rather than silently
      skipping verification — the safe default is preserved.

    Returns:
        The value returned by ``fn`` once it succeeds.

    """
    return policy.build_retrier()(fn, *args, **kwargs)
