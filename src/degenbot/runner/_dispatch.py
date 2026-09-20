"""Dispatch + sim-render helpers for the settlement-arbitrage ``BotRunner``.

Owns the encode→simulate→submit leaf
(:func:`_dispatch_profitable` — the ``dispatch_profitable`` /
``dispatch_and_submit`` Rust seam) and the ``[sim]``/``[profit]``/``[sim-fail]``
renderers that contextualize ``DispatchOutcome``.

The renderers are display-only (``stays-python``); all sim/submit arithmetic
runs in the Rust core. Only candidate-list shaping + log rendering happen
here — the inline-sim payload arm routes through the same Rust sim seam
(``merge_payload_results`` → the FFI batch's
``join_sim_result``/``derive_path_pools`` + the Rust ``MIN_PROFIT_NET``
gate), so pool-key derivation and threshold categorization are owned once,
Rust-side, for both entry arms.
"""

from __future__ import annotations

import dataclasses
import os
import pathlib
import time
from dataclasses import dataclass
from enum import Enum, auto
from typing import TYPE_CHECKING, Any

from degenbot.runner._relay_posture import relay_urls_from_env
from degenbot.runner._render import (
    _render_fot_tokens,
    _render_profit_logs,
    _render_sim_failures,
    _render_sim_summary,
    _SimOutcome,
)
from degenbot.runner.config import ArbitrageConfig

if TYPE_CHECKING:
    from degenbot.dispatch import DispatchOutcome
    from degenbot.runner.bot_runner import _SessionState

from degenbot.dispatch import (
    DispatchCandidate,
    SkippedRecord,
    SubmitCandidate,
    SubmitContext,
    SubmitSkipReason,
    SubmittedRecord,
    TxSigner,
    assemble_dispatch_candidates,
    dispatch_and_submit,
    dispatch_profitable,
    merge_payload_results,
)
from degenbot.logging import logger as bot_logger

# Cached relay submit providers (broadcast fan-out). Built lazily on the
# first gate-clearing candidate; reused so streaming batches never re-dial
# the builder endpoints per batch.
_RELAY_SUBMIT_PROVIDERS: list[tuple[str, Any]] | None = None

#: One raw engine-result row (path_id, optimal_input, profit, hop_outputs,
#: consumed_inputs, solve_block, state_nonces) - the tuple shape the result
#: batch stream delivers.
_RawResult = tuple[int, int, int, tuple[int, ...], tuple[int, ...], int, tuple[int, ...]]

from degenbot.runner._driver_constants import (  # ruff: ignore[module-import-not-at-top-of-file] - after the type alias block
    MIN_PROFIT_NET,
)

# The executor runtime bytecode file (one canonical filename in any
# contracts directory).
_EXECUTOR_RUNTIME_FILE = "cmd_executor_runtime_bytecode.txt"


def _resolve_executor_runtime_path(cfg: ArbitrageConfig) -> pathlib.Path:
    """Resolve the executor-runtime bytecode path — explicit, NO filesystem walk.

    Resolution order (first hit wins):
    1. ``cfg.executor_runtime`` — the operator's explicit path.
    2. ``$DEGENBOT_CONTRACTS_DIR/<file>`` — one explicit contracts dir.
    3. Exactly one computed candidate for the source layout: the repo root
       reached by a fixed-depth hop from this module
       (``<root>/src/degenbot/runner/dispatch.py`` -> ``<root>``), then
       ``contracts/<file>``. A wheel install has no such candidate — the
       operator must pass ``executor_runtime`` explicitly.
    """
    if cfg.executor_runtime is not None:
        return pathlib.Path(cfg.executor_runtime)
    env_dir = os.environ.get("DEGENBOT_CONTRACTS_DIR")
    if env_dir:
        return pathlib.Path(env_dir) / _EXECUTOR_RUNTIME_FILE
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
            "DEGENBOT_CONTRACTS_DIR to the directory containing "
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


@dataclasses.dataclass(frozen=True)
class BatchContext:
    """Per-batch economics carried by the serial dispatch leaf's callers."""

    block_timestamp: int
    base_fee_next: int


async def _dispatch_profitable(
    session: _SessionState,
    results: list[_RawResult],
    *,
    context: BatchContext,
    operator_nonce: int,
    payloads: dict[int, dict] | None = None,
) -> None:
    """Encode - simulate - submit one batch of profitable results serially.

    The serial composition; production drives
    :mod:`degenbot.runner._sim_submit_pipeline` (K-way concurrent sims over
    the same seam contracts, ordered submit fan-in). All session coordination
    state is read from the single ``session`` owner (CONTEXT.md: *session
    state*), never re-passed.

    ``payloads`` are the engine's inline-sim results — those entries skip the
    FFI sim and their submit records are stitched straight into the outcome
    (per-entry presence decides).
    """
    candidates = _build_dispatch_candidates(session, results, payloads=payloads)
    outcome: DispatchOutcome | None = None
    if candidates:
        current_block = session.dispatcher.current_block
        outcome = await _simulate_batch(
            session,
            candidates,
            block_timestamp=context.block_timestamp,
            base_fee_next=context.base_fee_next,
            current_block=current_block,
        )
    merged = _merge_payload_outcome(session, outcome, payloads)
    if not merged:
        return
    _render_outcome(session, merged, session.dispatcher.current_block)
    await _submit_batch_records(
        session,
        merged,
        operator_nonce=operator_nonce,
    )


def _build_dispatch_candidates(
    session: _SessionState,
    results: list[_RawResult],
    *,
    payloads: dict[int, dict] | None = None,
) -> list[DispatchCandidate]:
    """Shape a batch of raw engine results into Rust-seam candidates.

    Shared by the serial leaf (:func:`_dispatch_profitable`) and the concurrent
    pipeline (``_sim_submit_pipeline``). The whole batch is assembled by the
    Rust seam in one call: path resolution, per-row field construction, the
    empty-hop skip, and the payload-served skip all run in the core. Only the
    display-only ``[sim-none]`` log and the operator policy bools stay Python.
    Returns an EMPTY list when nothing is dispatchable (the caller skips sim +
    submit).

    Path ids present in ``payloads`` were ALREADY simulated
    inline in the Rust engine — they never enter the FFI sim batch (the
    payload derives their submit records directly; per-entry presence
    decides, so a mixed batch only degrades the payload-less entries).
    """
    if not results:
        return []
    assembly = assemble_dispatch_candidates(
        engine=session.engine_registry.engine,
        results=results,
        # The operator's ERC6909 vault-capture toggle - the Rust seam defaults
        # it to False (custody capture, the long-standing production behavior);
        # env-gated opt-in.
        erc6909_profit=session.cfg.erc6909_profit,
        skip_path_ids=sorted(payloads) if payloads else None,
    )
    for path_id in assembly.empty_hop_path_ids:
        bot_logger.debug(f"[sim-none] path={path_id}: empty hop_outputs")
    return list(assembly.candidates)


class MergedOutcome:
    """A ``DispatchOutcome``-protocol view: the FFI outcome + payload records.

    Entries the engine simulated inline never enter the FFI batch, so the
    batch outcome alone under-reports. This adapter stitches
    the payload-derived submit candidates/failure records into the base
    outcome's tallies so the renderers and the submit leaf see one
    attribute-uniform object (the attribute parity the
    ``[sim]``/``[profit]``/``[sim-fail]`` render contract demands). When every entry was
    payload-served (no FFI batch ran), ``base`` is ``None`` and only the
    payload records surface.
    """

    def __init__(
        self,
        base: DispatchOutcome | None,
        candidates: list[SubmitCandidate],
        failures: list[dict[str, Any]],
        path_infos: dict[int, dict[str, Any]],
        unprofitable_count: int,
    ) -> None:
        self._base: DispatchOutcome | None = base
        self._candidates = candidates
        self._failures = failures
        self._path_infos = path_infos
        self._unprofitable_count = unprofitable_count

    def __bool__(self) -> bool:
        # A merged outcome with no base and no payload records is empty
        # ( callers skip render+submit on falsy outcomes).
        if self._base is not None:
            return True
        return bool(self._candidates or self._failures or self._unprofitable_count)

    @property
    def gas_profitable(self) -> list[SubmitCandidate]:
        base = self._base
        base_candidates = [] if base is None else list(base.gas_profitable)
        return base_candidates + self._candidates

    @property
    def gas_unprofitable_count(self) -> int:
        base = self._base
        base_count = 0 if base is None else base.gas_unprofitable_count
        return base_count + self._unprofitable_count

    @property
    def exception_count(self) -> int:
        base = self._base
        return 0 if base is None else base.exception_count

    @property
    def fail_count(self) -> int:
        base = self._base
        base_count = 0 if base is None else base.fail_count
        return base_count + len(self._failures)

    @property
    def candidate_count(self) -> int:
        base = self._base
        base_count = 0 if base is None else base.candidate_count
        return base_count + len(self._candidates) + self._unprofitable_count + len(self._failures)

    @property
    def suppressed_count(self) -> int:
        base = self._base
        return 0 if base is None else base.suppressed_count

    @property
    def thin_dropped(self) -> int:
        base = self._base
        return 0 if base is None else base.thin_dropped

    @property
    def divergent_dropped(self) -> int:
        base = self._base
        return 0 if base is None else base.divergent_dropped

    @property
    def fot_dropped(self) -> int:
        base = self._base
        return 0 if base is None else base.fot_dropped

    @property
    def fail_buckets(self) -> dict[str, int]:
        base = self._base
        buckets = {} if base is None else dict(base.fail_buckets)
        for rec in self._failures:
            bucket = rec["bucket"]
            buckets[bucket] = buckets.get(bucket, 0) + 1
        return buckets

    @property
    def failures(self) -> list[dict[str, Any]]:
        base = self._base
        base_failures = [] if base is None else list(base.failures)
        return base_failures + self._failures

    @property
    def path_infos(self) -> dict[int, dict[str, Any]]:
        base = self._base
        merged = {} if base is None else dict(base.path_infos)
        merged.update(self._path_infos)
        return merged


def _merge_payload_outcome(
    session: _SessionState,
    base_outcome: DispatchOutcome | None,
    payloads: dict[int, dict] | None,
) -> _SimOutcome | None:
    """Stitch inline-sim payload records into (or over) the FFI batch outcome.

    The payload arm routes through the SAME sim seam the FFI batch uses
    (:func:`merge_payload_results`), so the mutual-exclusion pool keys
    (the Rust `derive_path_pools` walk over the engine's typed hops) and the
    net-profit threshold (the Rust-owned `MIN_PROFIT_NET` constant) are
    evaluated exactly once, Rust-side, for BOTH entry arms. This function
    only renders/stitches the returned record rows — honoring this module's
    docstring contract.

    Per-entry presence decides: each payload yields a submit row (built by
    the Rust `join_sim_result` FFI join over the engine's registered
    `PathInfo`), a gas-unprofitable tally entry (Rust-categorized), or a
    `[sim-fail]` record (the FFI row shape, built by the same seam). The
    below-threshold verdict arrives as a Rust `kind` string — no threshold
    value crosses to Python.

    `base_outcome` is the FFI outcome for the REMAINING (payload-less)
    entries — `None` only when there was nothing to send through the FFI
    batch at all.
    """
    if not payloads:
        return base_outcome or None
    sim_ctx = session.sim_ctx
    if sim_ctx is None:
        msg = "SimulateContext is required to merge payload records"
        raise RuntimeError(msg)

    # THE SIM SEAM: the payload records route through the SAME Rust
    # join the FFI batch uses — the mutual-exclusion path_pools derive from
    # the engine's typed hops (derive_path_pools) and the MIN_PROFIT_NET
    # gate applies there, ONCE. The submit rows arrive as PySubmitCandidate
    # (the gas_profitable element type) ready for dispatch_and_submit.
    outcome = merge_payload_results(
        [payloads[pid] for pid in sorted(payloads)],
        session.engine_registry.engine,
        sim_ctx.executor_address,
    )
    candidates = list(outcome.candidates)
    failures = list(outcome.failures)
    path_infos = dict(outcome.path_infos)
    unprofitable = outcome.unprofitable_count

    return MergedOutcome(base_outcome, candidates, failures, path_infos, unprofitable)


async def _simulate_batch(
    session: _SessionState,
    candidates: list[DispatchCandidate],
    *,
    block_timestamp: int,
    base_fee_next: int,
    current_block: int,
) -> DispatchOutcome:
    """Run the Rust simulate fan-out for a candidate batch (one DispatchOutcome)."""
    if session.sim_ctx is None:
        msg = "SimulateContext is required to dispatch (non-Alloy provider or sim context unbuilt)"
        raise RuntimeError(msg)
    return await dispatch_profitable(
        candidates=candidates,
        context=session.sim_ctx,
        dispatcher=session.dispatcher,
        base_fee_next=base_fee_next,
        current_block=current_block,
        block_timestamp=block_timestamp,
        min_profit_net=MIN_PROFIT_NET,
        min_profit_margin_bps=session.cfg.min_profit_margin_bps,
        engine=session.engine_registry.engine,
    )


def _render_outcome(
    session: _SessionState,
    outcome: _SimOutcome,
    current_block: int,
) -> None:
    """The display-only renderers over a sim outcome (``stays-python``)."""
    _render_sim_summary(outcome)
    _render_sim_failures(outcome, current_block=current_block)
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
    last_warn: float = 0.0

    def observe(self, *, vetoed: bool, now: float) -> SubmissionSmokeVerdict:
        """Advance the smoke FSM by one batch and return the typed verdict."""
        if not vetoed:
            self.streak = 0
            return SubmissionSmokeVerdict.QUIET
        self.streak += 1
        if self.streak >= _STALL_STREAK and now - self.last_warn >= _STALL_WARN_INTERVAL_S:
            self.last_warn = now
            return SubmissionSmokeVerdict.WARN
        return SubmissionSmokeVerdict.STREAK


async def _submit_batch_records(
    session: _SessionState,
    outcome: _SimOutcome,
    *,
    operator_nonce: int,
    submitter: Any = None,
    relay_providers: Any = None,
) -> None:
    """Submit gas-profitable candidates via the Rust submit leaf + render records.

    Shared by the serial leaf and the pipeline's ordered submitter. Expects
    the operator nonce fetched AT submit time (serialized consumers only) and
    forwards it unchanged: the Rust authority seeds from that chain read and
    leases the sign-time nonce, so no nonce is computed Python-side.

    ``submitter``/``relay_providers`` are the DI seams (tests inject a
    recording submitter + opaque providers; production runs the default
    ``dispatch_and_submit`` + the cached relay provider set).
    """
    async_alloy = session.async_w3.as_async_alloy()
    if async_alloy is None:
        bot_logger.error("[dispatch] async_w3 is not an Alloy-backed provider; cannot submit")
        return
    # Relay submission seam (ADR-025 companion): same signed bytes, dedicated
    # broadcast URL, revert-protecting private builder endpoints instead of
    # the public mempool. The POSTURE is owned by the session's RelayPosture
    # (built once from the relay env at session start — see _relay_posture);
    # sessions without one (bare test fakes) fall back to the env read. The
    # operator nonce is forwarded unchanged: the Rust authority issues it.
    relay_posture = getattr(session, "relay_posture", None)
    relay_urls = relay_posture.relay_urls if relay_posture is not None else relay_urls_from_env()
    if relay_urls and outcome.gas_profitable:
        broadcast_providers = await _resolve_relay_providers(relay_urls, relay_providers)
    else:
        broadcast_providers = None

    _log_submit_arm(outcome.gas_profitable, session.dispatcher.current_block)

    signer = TxSigner(key=session.cfg.operator_private_key, chain_id=session.cfg.chain_id)
    records = await (submitter if submitter is not None else dispatch_and_submit)(
        candidates=outcome.gas_profitable,
        dispatcher=session.dispatcher,
        provider=async_alloy,
        context=SubmitContext(
            signer=signer,
            operator_nonce=operator_nonce,
            current_block=session.dispatcher.current_block,
            dry_run=session.cfg.dry_run,
            inject_code=session.cfg.inject_executor_code,
            broadcast_providers=broadcast_providers,
        ),
    )
    submitted_count = sum(isinstance(record, SubmittedRecord) for record in records)
    skip_histogram = _render_submit_records(records)
    _track_submission_smoke(session, outcome, submitted_count, skip_histogram)


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


def _log_submit_arm(candidates: list[Any], solve_block: int) -> None:
    """Forensic capture (fork-replay): the exact calldata + candidate economics.

    One INFO line per gate-clearing candidate BEFORE broadcast, so any later tx
    can be replayed at its solve block.
    """
    for c in candidates:
        calldata = getattr(c, "execute_calldata", None)
        bot_logger.info(
            f"[submit-arm] path={c.path_id} solve_block={solve_block} "
            f"net_wei={c.net_profit} gas={c.gas_used} "
            f"calldata={calldata.hex() if calldata else '<unavailable>'}",
        )


def _render_submit_records(records: list[Any]) -> dict[str, int]:
    """Render one log line per submit record; return the skip-reason histogram."""
    skip_histogram: dict[str, int] = {}
    for record in records:
        if isinstance(record, SkippedRecord):
            skip_histogram[record.reason.name] = skip_histogram.get(record.reason.name, 0) + 1
        match record:
            case SubmittedRecord(path_id=path_id, tx_hash=tx_hash, nonce=nonce):
                bot_logger.info(f"Submitted path {path_id} hash={tx_hash} nonce={nonce}")
            case SkippedRecord(path_id=path_id, reason=SubmitSkipReason.POOLS_CLAIMED):
                bot_logger.debug(f"[dispatch] skip path={path_id}: pools claimed after sim")
            case SkippedRecord(reason=SubmitSkipReason.DRY_RUN):
                pass  # dry_run skip already logged above
            case SkippedRecord(path_id=path_id, reason=SubmitSkipReason.INJECT_CODE):
                bot_logger.warning(
                    f"[dispatch] path={path_id}: skipping submission - "
                    "executor code injection is active (simulation.inject_executor_code)",
                )
            case SkippedRecord(reason=SubmitSkipReason.BROADCAST_FAILED, detail=detail):
                bot_logger.warning(f"[dispatch] broadcast failed: {detail or 'no detail'}")
    return skip_histogram


def _track_submission_smoke(
    session: _SessionState,
    outcome: _SimOutcome,
    submitted_count: int,
    skip_histogram: dict[str, int],
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
        bot_logger.warning(
            f"[dispatch] live-armed with gate-clearing candidates but no submissions in "
            f"{smoke.streak} consecutive batches; skip reasons "
            f"{skip_histogram or '{}'} — a configuration-level veto is likely"
        )
