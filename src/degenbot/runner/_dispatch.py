"""Render-fold + record-render helpers for the settlement-arbitrage cockpit.

The production path is the Rust core's batch executor
(:mod:`degenbot.runner._sim_submit` builds it; the consumer enqueues batches
and a drain task renders). This module owns the DISPLAY fold: one drained
batch's Batch outcome records fold into the :class:`MergedOutcome` render
view (the counters are exact folds over the records — never stored fields),
and the submit-lane records render the ``[dispatch]`` lines + the
silent-veto smoke FSM.

The renderers are display-only (``stays-python``); all sim/submit arithmetic
runs in the Rust core.
"""

from __future__ import annotations

import pathlib
import time
from dataclasses import dataclass
from enum import Enum, auto
from typing import TYPE_CHECKING, Any

from degenbot.runner._render import (
    _render_fot_tokens,
    _render_profit_logs,
    _render_sim_failures,
    _render_sim_summary,
    _SimOutcome,
)
from degenbot.runner.config import ArbitrageConfig

if TYPE_CHECKING:
    from degenbot.runner.bot_runner import _SessionState

from degenbot.dispatch import (
    AssemblyVerdict,
    SimulateVerdict,
    SkippedRecord,
    SubmitSkipReason,
    SubmittedRecord,
)
from degenbot.logging import logger as bot_logger

# Cached relay submit providers (broadcast fan-out). Built lazily on the
# first gate-clearing candidate; reused so streaming batches never re-dial
# the builder endpoints per batch.
_RELAY_SUBMIT_PROVIDERS: list[tuple[str, Any]] | None = None


@dataclass(frozen=True, slots=True)
class RawEngineResult:
    """One raw engine-result row the solver result batch stream delivers.

    The named record is constructed at the batch-stream conversion point
    (``_consume._engine_result``) and extracted by field name by the Rust
    ``assemble_dispatch_candidates`` seam — the field order never crosses
    the boundary positionally.
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


@dataclass(frozen=True, slots=True)
class _RenderCandidate:
    """The display-fold view of one gas-profitable Batch outcome record.

    The renderers read ``path_id``/``gross_profit``/``net_profit``/
    ``gas_used``/``priority_fee`` off a submit candidate; the record's typed
    ``SimReceipt`` projects onto exactly that surface (the calldata/encoding
    lives in the core now — a rendered candidate is no longer an encodable
    payload).
    """

    path_id: int
    gross_profit: int
    net_profit: int
    gas_used: int
    priority_fee: int


@dataclass(frozen=True, slots=True)
class MergedOutcome:
    """The named sim/submit render view over one drained batch.

    The view is an exact FOLD over the batch's Batch outcome records
    (:meth:`from_records`) — the renderers keep ``DispatchOutcome`` attribute
    parity (the ``[sim]``/``[profit]``/``[sim-fail]`` contract) without
    re-tallying on every property access. Falsy when empty: the drain skips
    render+submit bookkeeping on falsy outcomes.
    """

    gas_profitable: list[_RenderCandidate]
    gas_unprofitable_count: int
    exception_count: int
    fail_count: int
    candidate_count: int
    suppressed_count: int
    thin_dropped: int
    divergent_dropped: int
    fot_dropped: int
    fail_buckets: dict[str, int]
    failures: list[dict[str, Any]]
    path_infos: dict[int, dict[str, Any]]

    def __bool__(self) -> bool:
        return bool(
            self.gas_profitable
            or self.failures
            or self.gas_unprofitable_count
            or self.candidate_count
        )

    @classmethod
    def from_records(cls, records: Any) -> MergedOutcome:
        """Fold one drained batch's Batch outcome records into the render view.

        Every counter is a FOLD over the records (never a stored field); the
        failure rows and ``path_infos`` decode through the seam's one
        serializers (``FailureDetail.record()`` / ``path_info_dict()``), so
        the pool-key derivation and threshold categorization the renderers
        display are Rust-owned end to end. The pre-assembly skip verdicts
        carry their display-only ``[sim-none]`` log here.
        """
        gas_profitable: list[_RenderCandidate] = []
        gas_unprofitable_count = 0
        exception_count = 0
        fail_count = 0
        fail_buckets: dict[str, int] = {}
        failures: list[dict[str, Any]] = []
        path_infos: dict[int, dict[str, Any]] = {}
        suppressed = thin = divergent = fot = 0
        for rec in records:
            match rec.assembly:
                case AssemblyVerdict.SkipEmptyHops:
                    bot_logger.debug(f"[sim-none] path={rec.path_id}: empty hop_outputs")
                    continue
                case (
                    AssemblyVerdict.SkipResolveMiss
                    | AssemblyVerdict.SkipPayloadServed
                    | AssemblyVerdict.SkipThinMargin
                    | AssemblyVerdict.SkipDivergentPool
                    | AssemblyVerdict.SkipFeeOnTransfer
                ):
                    continue
                case AssemblyVerdict.SkipSuppressed:
                    suppressed += 1
                    continue
                case _:
                    pass
            # Assembled: the display context + the sim verdict fold.
            path_infos[rec.path_id] = rec.path_info_dict()
            match rec.simulate:
                case SimulateVerdict.Profitable(receipt=receipt):
                    gas_profitable.append(
                        _RenderCandidate(
                            path_id=rec.path_id,
                            gross_profit=int(receipt.gross_profit),
                            net_profit=int(receipt.net_profit),
                            gas_used=receipt.gas_used,
                            priority_fee=receipt.priority_fee,
                        )
                    )
                case SimulateVerdict.GasUnprofitable():
                    gas_unprofitable_count += 1
                case SimulateVerdict.Failed(detail=detail):
                    fail_count += 1
                    record = detail.record()
                    failures.append(record)
                    bucket = record["bucket"]
                    fail_buckets[bucket] = fail_buckets.get(bucket, 0) + 1
                case SimulateVerdict.Exception():
                    exception_count += 1
                case _:
                    # Post-assembly drop (the solve-snapshot staleness gate /
                    # the per-batch cap) — the same drain-invisible drop the
                    # pre-cut-over outcome applied.
                    path_infos.pop(rec.path_id, None)
        return cls(
            gas_profitable=gas_profitable,
            gas_unprofitable_count=gas_unprofitable_count,
            exception_count=exception_count,
            fail_count=fail_count,
            candidate_count=(
                len(gas_profitable) + gas_unprofitable_count + fail_count + exception_count
            ),
            suppressed_count=suppressed,
            thin_dropped=thin,
            divergent_dropped=divergent,
            fot_dropped=fot,
            fail_buckets=fail_buckets,
            failures=failures,
            path_infos=path_infos,
        )


def _render_outcome(
    session: _SessionState,
    outcome: _SimOutcome,
    current_block: int,
) -> None:
    """The display-only renderers over a sim outcome (``stays-python``)."""
    _render_sim_summary(outcome)
    _render_sim_failures(
        outcome,
        current_block=current_block,
        sim_exit_on_fail=session.cfg.sim_exit_on_fail,
        exit_ignore_buckets=session.cfg.sim_exit_ignore_buckets,
    )
    _render_fot_tokens(session.dispatcher, current_block)
    _render_profit_logs(outcome)


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

    Owned by the session state, so a streak never leaks across sessions and
    the throttle clock resets with the session. :meth:`observe` returns the
    typed verdict; the dispatch leaf owns the display-only WARN render, so the
    decision and its rendering stay separable.
    """

    streak: int = 0
    # `None` (not 0.0) is "never warned": the monotonic clock's origin is the
    # boot, so a fresh host reads `now < _STALL_WARN_INTERVAL_S` and a 0.0
    # anchor would park the FIRST warn inside the throttle window (never
    # fired until the host is 300s up - the CI-runner flake).
    last_warn: float | None = None

    def observe(self, *, vetoed: bool, now: float) -> SubmissionSmokeVerdict:
        """Advance the smoke FSM by one batch and return the typed verdict."""
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
    """Resolve this batch's relay broadcast providers."""
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


def _render_submit_records(
    records: list[Any],
    *,
    logger: Any = bot_logger,
) -> dict[str, int]:
    """Render one log line per submit record; return the skip-reason histogram."""
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


def _track_submission_smoke(
    session: _SessionState,
    outcome: _SimOutcome,
    submitted_count: int,
    skip_histogram: dict[str, int],
    *,
    logger: Any = bot_logger,
) -> None:
    """Throttle the silent-veto WARN over a fully-vetoed live-batch streak.

    The streak/clock decision is the session's :class:`SubmissionSmoke` FSM; this
    leaf only composes the veto predicate and renders the typed ``WARN``.
    """
    vetoed = (
        not session.cfg.dry_run
        and not session.cfg.inject_executor_code
        and bool(outcome.gas_profitable)
        and submitted_count == 0
    )
    smoke = session.submission_smoke
    verdict = smoke.observe(vetoed=vetoed, now=time.monotonic())
    if verdict is SubmissionSmokeVerdict.WARN:
        logger.warning(
            "[dispatch] live-armed with gate-clearing candidates but no submissions in "
            "%s consecutive batches; skip reasons %s — a configuration-level veto is likely",
            smoke.streak,
            skip_histogram or "{}",
        )
