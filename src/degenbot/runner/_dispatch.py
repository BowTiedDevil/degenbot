"""Submit-lane records, the smoke FSM, and the executor boundary's leaves.

The production path is the Rust core's batch executor
(:mod:`degenbot.runner._sim_submit` builds it; the consumer enqueues batches
and a drain task renders). This module owns the submit side of one drained
batch: the raw engine-result record the assembly seam extracts, the
``[dispatch]`` line render over the typed submit records, the silent-veto smoke
FSM, the relay broadcast fan-out, and the executor-runtime bytecode resolution.

The render view (:class:`~degenbot.runner._render.MergedOutcome`) and the
display policy (:class:`~degenbot.runner._render.DisplayPolicy`) live in
:mod:`degenbot.runner._render`; this module imports them one way, so the two
leaves carry no import cycle.

The renderers are display-only (``stays-python``); all sim/submit arithmetic
runs in the Rust core.
"""

from __future__ import annotations

import pathlib
import time
from dataclasses import dataclass
from enum import Enum, auto
from typing import TYPE_CHECKING, Any

from degenbot.dispatch import SkippedRecord, SubmitSkipReason, SubmittedRecord
from degenbot.logging import logger as bot_logger

if TYPE_CHECKING:
    from degenbot.runner._render import DisplayPolicy, MergedOutcome
    from degenbot.runner.config import ArbitrageConfig

# Cached relay submit providers (broadcast fan-out). Built lazily on the
# first gate-clearing candidate; reused so streaming batches never re-dial
# the builder endpoints per batch.
_RELAY_SUBMIT_PROVIDERS: list[tuple[str, Any]] | None = None


@dataclass(frozen=True, slots=True)
class RawEngineResult:
    """One raw engine-result row the solver result batch stream delivers.

    The named record is constructed at the batch-stream conversion point
    (``_consume._engine_result``) and extracted by field name by the Rust
    batch executor's ``enqueue`` seam — the field order never crosses the
    boundary positionally.
    """

    path_id: int
    optimal_input: int
    engine_profit: int
    hop_outputs: tuple[int, ...]
    consumed_inputs: tuple[int, ...]
    solve_block: int
    state_nonces: tuple[int, ...]


# The executor runtime bytecode file (one canonical filename in any
# contracts directory).
_EXECUTOR_RUNTIME_FILE = "cmd_executor_runtime_bytecode.txt"


def _resolve_executor_runtime_path(cfg: ArbitrageConfig) -> pathlib.Path:
    """Resolve the executor-runtime bytecode path — explicit, NO filesystem walk.

    Resolution order (first hit wins):
    1. ``cfg.executor_runtime`` — the operator's explicit path.
    2. The declared ``dispatch.contracts_dir`` key.
    3. Exactly one computed candidate for the source layout: the repo root
       reached by a fixed-depth hop from this module
       (``<root>/src/degenbot/runner/dispatch.py`` -> ``<root>``), then
       ``contracts/<file>``. A wheel install has no such candidate — the
       operator must pass ``executor_runtime`` explicitly.

    Args:
        cfg: The session's typed config.

    Returns:
        The resolved bytecode-file path (existence is not checked here).

    """
    if cfg.executor_runtime is not None:
        return pathlib.Path(cfg.executor_runtime)
    contracts_dir = cfg.contracts_dir
    if contracts_dir:
        return pathlib.Path(contracts_dir) / _EXECUTOR_RUNTIME_FILE
    root = pathlib.Path(__file__).resolve().parents[3]
    return root / "contracts" / _EXECUTOR_RUNTIME_FILE


def _load_executor_runtime_bytecode(cfg: ArbitrageConfig) -> str:
    """Load the patched runtime bytecode (0x-prefixed hex text).

    The bytecode has all 5 immutable slots baked in: OWNER_ADDR, WETH_ADDR,
    POOL_MANAGER_ADDR, and 2 precomputed delta slots (WETH, NATIVE).
    See contracts/recompile.py for the full layout.

    Args:
        cfg: The session's typed config.

    Returns:
        The 0x-prefixed hex runtime bytecode text.

    Raises:
        RuntimeError: If the bytecode file does not exist.
        ValueError: If the file's contents are not 0x-prefixed hex text.

    """
    bytecode_path = _resolve_executor_runtime_path(cfg)
    if not bytecode_path.exists():
        msg = (
            f"executor runtime bytecode not found at {bytecode_path}. "
            "Set ArbitrageConfig.executor_runtime to the file path, or set "
            "DEGENBOT_CONTRACTS_DIR (dispatch.contracts_dir) to the directory containing "
            f"{_EXECUTOR_RUNTIME_FILE} (wheel installs: pass executor_runtime explicitly)."
        )
        raise RuntimeError(msg)
    code = bytecode_path.read_text(encoding="utf-8").strip()
    if not code.startswith("0x"):
        msg = f"Runtime bytecode file must start with 0x, got: {code[:20]}..."
        raise ValueError(msg)
    bot_logger.info(
        f"[inject] Loaded executor runtime bytecode: "
        f"{len(code) // 2 - 1} bytes from {bytecode_path}",
    )
    return code


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
