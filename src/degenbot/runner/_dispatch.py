"""Submit-lane records, the smoke FSM, and the executor boundary's leaves.

The production path is the Rust core's batch executor
(:mod:`degenbot.runner._sim_submit` builds it; the session consumer derives and
enqueues each batch, and a drain task renders). This module owns the submit side
of one drained batch: the ``[dispatch]`` line render over the typed submit
records, the silent-veto smoke FSM, and the relay broadcast fan-out.

The render view (:class:`~degenbot.runner._render.MergedOutcome`) and the
display policy (:class:`~degenbot.runner._render.DisplayPolicy`) live in
:mod:`degenbot.runner._render`; this module imports them one way, so the two
leaves carry no import cycle.

The renderers are display-only (``stays-python``); all sim/submit arithmetic
runs in the Rust core.
"""

from __future__ import annotations

import time
from dataclasses import dataclass
from enum import Enum, auto
from typing import TYPE_CHECKING, Any

from degenbot.dispatch import SkippedRecord, SubmitSkipReason, SubmittedRecord
from degenbot.logging import logger as bot_logger

if TYPE_CHECKING:
    from degenbot.runner._render import DisplayPolicy, MergedOutcome

# Cached relay submit providers (broadcast fan-out). Built lazily on the
# first gate-clearing candidate; reused so streaming batches never re-dial
# the builder endpoints per batch.
_RELAY_SUBMIT_PROVIDERS: list[tuple[str, Any]] | None = None


#: Silent-veto smoke detector: a live-armed session whose candidates clear
#: the sim gates but whose every dispatch batch ends in skips is running a
#: configuration veto (the class the retired injection-flag divergence was
#: the sharpest instance of). A streak of fully-vetoed live batches earns one
#: throttled WARN naming the skip histogram; per-batch detail lines carry the
#: specifics.
_STALL_STREAK = 3
_STALL_WARN_INTERVAL_S = 300.0


class SubmissionSmokeVerdict(Enum):
    """The silent-veto smoke FSM's typed observe result."""

    QUIET = auto()
    STREAK = auto()
    WARN = auto()


@dataclass
class SubmissionSmoke:
    """Per-session silent-veto smoke FSM (``Quiet | Streak | Warn``).

    Owned by the session, so a streak never leaks across sessions and the
    throttle clock resets with the session. :meth:`observe` returns the typed
    verdict; the dispatch leaf owns the display-only WARN render, so the
    decision and its rendering stay separable.
    """

    streak: int = 0
    # `None` (not 0.0) is "never warned": the monotonic clock's origin is the
    # boot, so a fresh host reads `now < _STALL_WARN_INTERVAL_S` and a 0.0
    # anchor would park the FIRST warn inside the throttle window (never
    # fired until the host is 300s up - the CI-runner flake).
    last_warn: float | None = None

    def observe(self, *, vetoed: bool, now: float) -> SubmissionSmokeVerdict:
        """Advance the smoke FSM by one batch and return the typed verdict.

        Args:
            vetoed: Whether the batch was fully vetoed (gate-clearing
                candidates, no submission) in a live-armed session.
            now: The monotonic timestamp of the batch.

        Returns:
            The smoke verdict for this batch (``QUIET``, ``WARN``, or
            ``STREAK``).

        """
        if not vetoed:
            self.streak = 0
            return SubmissionSmokeVerdict.QUIET
        self.streak += 1
        warned_recently = (
            self.last_warn is not None and now - self.last_warn < _STALL_WARN_INTERVAL_S
        )
        if self.streak >= _STALL_STREAK and not warned_recently:
            self.last_warn = now
            return SubmissionSmokeVerdict.WARN
        return SubmissionSmokeVerdict.STREAK


async def _resolve_relay_providers(
    relay_urls: list[str],
    relay_providers: Any,
) -> list[Any]:
    """Resolve this batch's relay broadcast providers.

    Args:
        relay_urls: The posture's relay endpoints.
        relay_providers: Pre-built relay providers (``None`` = dial the URLs).

    Returns:
        One Alloy-backed broadcast provider per relay URL (the cached fan-out
        rebuilt when the URL list changes).

    """
    if relay_providers is not None:
        broadcast_providers = [
            relay_provider.as_async_alloy() for relay_provider in relay_providers
        ]
    else:
        global _RELAY_SUBMIT_PROVIDERS
        if _RELAY_SUBMIT_PROVIDERS is None or [u for u, _ in _RELAY_SUBMIT_PROVIDERS] != relay_urls:
            from degenbot.provider import AsyncAlloyProvider as _AsyncAlloyProvider

            _RELAY_SUBMIT_PROVIDERS = [
                (relay_url, await _AsyncAlloyProvider.create(rpc_url=relay_url))
                for relay_url in relay_urls
            ]
            endpoints = ",".join(
                relay_url.split("//", 1)[1].split("/", 1)[0]
                for relay_url, _ in _RELAY_SUBMIT_PROVIDERS
            )
            bot_logger.info(f"[submit] relay fan-out: {endpoints}")
        broadcast_providers = [
            relay_provider.as_async_alloy() for _, relay_provider in _RELAY_SUBMIT_PROVIDERS
        ]
    return broadcast_providers


def render_submit_records(
    records: list[Any],
    *,
    logger: Any = bot_logger,
) -> dict[str, int]:
    """Render one log line per submit record.

    Args:
        records: The batch's typed submit records.
        logger: The logger to render through.

    Returns:
        The skip-reason histogram over the batch's submit records.

    """
    skip_histogram: dict[str, int] = {}
    for record in records:
        if isinstance(record, SkippedRecord):
            skip_histogram[record.reason.name] = skip_histogram.get(record.reason.name, 0) + 1
        match record:
            case SubmittedRecord(path_id=path_id, tx_hash=tx_hash, nonce=nonce):
                logger.info("Submitted path %s hash=%s nonce=%s", path_id, tx_hash, nonce)
            case SkippedRecord(path_id=path_id, reason=SubmitSkipReason.POOLS_CLAIMED):
                logger.debug("[dispatch] skip path=%s: pools claimed after sim", path_id)
            case SkippedRecord(reason=SubmitSkipReason.DRY_RUN):
                pass  # dry_run skip already logged above
            case SkippedRecord(path_id=path_id, reason=SubmitSkipReason.INJECT_CODE):
                logger.warning(
                    "[dispatch] path=%s: skipping submission - "
                    "executor code injection is active (simulation.inject_executor_code)",
                    path_id,
                )
            case SkippedRecord(reason=SubmitSkipReason.BROADCAST_FAILED, detail=detail):
                logger.warning("[dispatch] broadcast failed: %s", detail or "no detail")
    return skip_histogram


def track_submission_smoke(
    smoke: SubmissionSmoke,
    outcome: MergedOutcome,
    records: list[Any],
    *,
    policy: DisplayPolicy,
    logger: Any = bot_logger,
) -> None:
    """Render one drained batch's submit records and advance the smoke FSM.

    The wait/no-wait decision is :meth:`DisplayPolicy.vetoed` (composed from the
    resolved policy + the batch's render fold); the streak/clock decision is the
    session's :class:`SubmissionSmoke` FSM; this leaf renders the ``[dispatch]``
    lines and the typed ``WARN``.

    Args:
        smoke: The session's silent-veto FSM.
        outcome: The batch's render view.
        records: The batch's typed submit records.
        policy: The resolved display policy.
        logger: The logger to render through.

    """
    skip_histogram = render_submit_records(records, logger=logger)
    submitted_count = sum(isinstance(record, SubmittedRecord) for record in records)
    verdict = smoke.observe(
        vetoed=policy.vetoed(outcome, submitted_count),
        now=time.monotonic(),
    )
    if verdict is SubmissionSmokeVerdict.WARN:
        logger.warning(
            "[dispatch] live-armed with gate-clearing candidates but no submissions in "
            "%s consecutive batches; skip reasons %s — a configuration-level veto is likely",
            smoke.streak,
            skip_histogram or "{}",
        )
