"""Display rendering for the driver's revert and diagnostic output.

Private leaf module (underscore name): pure renderers over dispatch
outcomes and engine failure records. The dispatch path
(:mod:`~degenbot.runner._dispatch`) imports :func:`_dump_failure_fixture`,
:func:`_render_profit_logs`, and :func:`_render_sim_failures`; outside
that, only tests import this module. Nothing here calls the Rust core — the
single FFI-typed import is ``PoolKind`` (type identity only, for the hop
family taxonomy).

The sim-failure tripwire reads its armed state and ignore set as values the
caller resolved, never the process verdict: the arm/disarm decision is the
driver's, made once at the construction boundary and handed down.
"""

from __future__ import annotations

import dataclasses
import json
import sys
from typing import TYPE_CHECKING, Any

from degenbot.logging import logger as bot_logger
from degenbot.pathfinding import PoolKind

if TYPE_CHECKING:
    from degenbot.diagnostics import FailureAction
    from degenbot.dispatch import Dispatcher, DispatchOutcome
    from degenbot.runner._dispatch import MergedOutcome

# Cap on per-batch `[sim-fail]` lines emitted by the renderer. A thin-margin
# revert storm can otherwise flood the log during a stalled head.
_SIM_FAIL_RENDER_CAP = 25

# Sentinel for `hop_fields`' strict-read mode (distinct from any field value,
# including None).
_UNSET: Any = object()

# The hop ``family`` wire string → the FFI PoolKind taxonomy (the V2/V3/V4
# axis — NOT the PoolFamily invariant axis). The ONE conversion for the render
# seam; consumers compare members.
_FAMILY_TO_POOL_KIND: dict[str, PoolKind] = {
    "V2": PoolKind.V2,
    "V3": PoolKind.V3,
    "V4": PoolKind.V4,
}

# Render labels for PoolKind members (the pyclass str() spells "PoolKind.V2";
# the operator-facing lines keep the bare version spelling).
_POOL_KIND_LABEL: dict[PoolKind, str] = {
    kind: family for family, kind in _FAMILY_TO_POOL_KIND.items()
}


def _family_label(family: Any) -> str:
    """Render one HopFields family: the PoolKind label, or the lenient default."""

    if isinstance(family, PoolKind):
        return _POOL_KIND_LABEL[family]
    return str(family)


# A sim-dispatch outcome handed to the renderers: the FFI batch outcome or
# MergedOutcome (the payload-stitched adapter — structurally identical view).
type _SimOutcome = DispatchOutcome | MergedOutcome


@dataclasses.dataclass(frozen=True, slots=True)
class HopFields:
    """The typed view of one plain-dict hop (the ``outcome.path_infos`` shape).

    The ONE home for the family-conditional field names: V2/V3 hops carry
    ``pool_address``/``token0_address``/``token1_address``, V4 hops carry
    ``pool_id_hex``/``currency0_address``/``currency1_address`` (plus the
    pool manager + tick spacing) — the render consumers read the named
    fields instead of re-branching on the dict keys.
    """

    family: PoolKind | Any
    # V2/V3 pool_address | V4 pool_id_hex.
    pool_ref: str
    # token0_address | currency0_address.
    token0: str
    # token1_address | currency1_address.
    token1: str
    fee: int
    zfo: bool
    # V4-only fields (None on the V2/V3 variants).
    pool_manager: str | None = None
    tick_spacing: int | None = None


def hop_fields(hop: dict[str, Any], *, default: Any = _UNSET) -> HopFields:
    """Read one plain-dict hop into the typed :class:`HopFields` view.

    The wire ``family`` string converts to the FFI PoolKind taxonomy at this
    ONE point; consumers compare members. With ``default`` the read is lenient
    (``dict.get`` semantics, every field falling back to that value) — the
    failure-fixture dump renders malformed hops instead of raising on them.

    Raises:
        ValueError: On a family string outside the PoolKind taxonomy in strict
            mode — an unknown render-seam family is loud, never silently
            branched.

    """

    def get(key: str) -> Any:
        return hop[key] if default is _UNSET else hop.get(key, default)

    family = get("family")
    kind = _FAMILY_TO_POOL_KIND.get(family) if isinstance(family, str) else None
    if kind is None:
        if default is _UNSET:
            msg = f"Unsupported hop family: {family!r}"
            raise ValueError(msg)
        # Lenient read (the failure-fixture dump): an unknown family keeps the
        # caller's default and takes the V4-field branch, as before.
        kind = family

    if isinstance(kind, PoolKind) and kind in {PoolKind.V2, PoolKind.V3}:
        return HopFields(
            family=kind,
            pool_ref=get("pool_address"),
            token0=get("token0_address"),
            token1=get("token1_address"),
            fee=get("fee"),
            zfo=get("zfo"),
        )

    return HopFields(
        family=kind,
        pool_ref=get("pool_id_hex"),
        token0=get("currency0_address"),
        token1=get("currency1_address"),
        fee=get("fee"),
        zfo=get("zfo"),
        pool_manager=get("pool_manager_address"),
        tick_spacing=get("tick_spacing"),
    )


def _hop_token_summary(hops: list[dict[str, Any]] | tuple[dict[str, Any], ...]) -> str:
    """One-line summary of hop input→output tokens for sim-fail diagnostics.


    Reads plain dicts (the ``outcome.path_infos`` render shape).

    """

    parts: list[str] = []

    for h in hops:
        hf = hop_fields(h)

        parts.append(f"{hf.token0}→{hf.token1}{'↗' if hf.zfo else '↘'}")

    return " ".join(parts)


def _render_sim_summary(outcome: _SimOutcome) -> None:
    """Render the ``[sim]`` line from ``DispatchOutcome`` fields (D4 stay-Python).


    Ports the prior ``[sim] N candidates: X ok (Y profitable, Z below

    threshold), W failed, V exceptions …`` summary. Appends the

    suppressed/thin/divergent drops when non-zero.

    """

    profitable = outcome.gas_profitable

    best_net = max((c.net_profit for c in profitable), default=0)

    breakdown = format_failure_breakdown(outcome.fail_buckets)

    sim_ok = len(profitable) + outcome.gas_unprofitable_count

    extra = ""

    if (
        outcome.suppressed_count
        or outcome.thin_dropped
        or outcome.divergent_dropped
        or outcome.fot_dropped
    ):
        extra = (
            f" — suppressed={outcome.suppressed_count}, "
            f"thin={outcome.thin_dropped}, "
            f"divergent={outcome.divergent_dropped}, "
            f"fot={outcome.fot_dropped}"
        )

    # Sim-ready timestamp: the render fires the instant the merged outcome is
    # consumed — pairing this with the block header time (pump.block INFO ts)
    # gives the B-arm first-submit-proxy latency for the inline stance.
    import time as _time

    bot_logger.info(
        f"[sim] ready_ms={_time.time():.3f} {outcome.candidate_count} candidates: "
        f"{sim_ok} ok ({len(profitable)} profitable, "
        f"{outcome.gas_unprofitable_count} below threshold), "
        f"{outcome.fail_count} failed, {outcome.exception_count} exceptions"
        f"{f' — best net={best_net // 10**9}gwei' if profitable else ''}"
        f"{f' — by reason: {breakdown}' if breakdown else ''}"
        f"{extra}",
    )


def _render_profit_logs(outcome: _SimOutcome) -> None:
    """Render the ``[profit]`` per-path hop-detail log (D4 stay-Python)."""

    for cand in outcome.gas_profitable:
        path_info = outcome.path_infos.get(cand.path_id)

        hop_details = []

        if path_info is not None:
            for i, h in enumerate(path_info["hops"]):
                hf = hop_fields(h)

                if hf.family == PoolKind.V4:
                    hop_details.append(
                        f"  hop[{i}] V4 pm={hf.pool_manager} "
                        f"pid={hf.pool_ref} "
                        f"c0={hf.token0} c1={hf.token1} "
                        f"fee={hf.fee} ts={hf.tick_spacing} zfo={hf.zfo}",
                    )

                else:
                    hop_details.append(
                        f"  hop[{i}] {_family_label(hf.family)} addr={hf.pool_ref} "
                        f"t0={hf.token0} t1={hf.token1} "
                        f"fee={hf.fee} zfo={hf.zfo}",
                    )

        hops_str = "\n".join(hop_details)

        bot_logger.info(
            f"[profit] path={cand.path_id} "
            f"{path_info['path_type'] if path_info else '?'} "
            f"gross={cand.gross_profit / 1e18:.6f}ETH ({cand.gross_profit // 10**9}gwei) "
            f"net={cand.net_profit / 1e18:.6f}ETH ({cand.net_profit // 10**9}gwei) "
            f"gas={cand.gas_used} prio={cand.priority_fee // 10**9}gwei\n{hops_str}",
        )


def _dump_failure_fixture(
    rec: dict[str, Any],
    path_info: dict[str, Any] | None,
    current_block: int,
) -> None:
    """Dump the full hop detail for a failing candidate — the sim-failure trap."""

    path_id = rec["path_id"]

    captured = rec.get("captured_swaps") or []

    hop_outputs = rec.get("hop_outputs")

    optimal_input = rec.get("optimal_input")

    bot_logger.error(
        f"[sim-fixture] path={path_id} block={current_block} "
        f"bucket={rec.get('bucket')} fail_index={rec.get('fail_index')} "
        f"optimal_input={optimal_input} "
        f"revert={rec.get('revert_data', '')[:10]}…",
    )

    if path_info is None:
        bot_logger.error("[sim-fixture] (path_info missing — cannot dump hops)")

        return

    hops = path_info.get("hops", [])

    bot_logger.error(
        f"[sim-fixture] path_type={path_info.get('path_type')} hops={len(hops)} "
        f"hop_outputs={hop_outputs}",
    )

    for i, h in enumerate(hops):
        hf = hop_fields(h, default="?")

        if isinstance(hf.family, PoolKind) and hf.family in {PoolKind.V2, PoolKind.V3}:
            bot_logger.error(
                f"[sim-fixture] hop[{i}] {_family_label(hf.family)} pool={hf.pool_ref} "
                f"t0={hf.token0} t1={hf.token1} fee={hf.fee} zfo={hf.zfo}",
            )

        else:  # V4
            bot_logger.error(
                f"[sim-fixture] hop[{i}] V4 pool_manager={hf.pool_manager} "
                f"pool_id={hf.pool_ref} "
                f"c0={hf.token0} c1={hf.token1} fee={hf.fee} "
                f"tick_spacing={hf.tick_spacing} zfo={hf.zfo}",
            )

    for j, s in enumerate(captured):
        bot_logger.error(
            f"[sim-fixture] captured[{j}] family={s.get('family')} "
            f"emitter={s.get('emitter')} amount0={s.get('amount0')} "
            f"amount1={s.get('amount1')} sqrt_price={s.get('sqrt_price_x96')} "
            f"liquidity={s.get('liquidity')} tick={s.get('tick')}",
        )


def _render_sim_failures(
    outcome: _SimOutcome,
    *,
    current_block: int,
    sim_exit_on_fail: bool,
    exit_ignore_buckets: str,
    sim_failure_action: FailureAction | None = None,
) -> None:
    """Render one ``[sim-fail]`` + one ``[sim-diag]`` line per reverted / failed
    candidate (D3). Capped at :data:`_SIM_FAIL_RENDER_CAP` records.

    ``sim_exit_on_fail`` and ``exit_ignore_buckets`` are the resolved
    ``simulation.*`` values the caller threads down: the tripwire never reads
    the process verdict, so the arm/disarm decision is a value at the
    consumption boundary rather than an ambient reach. ``sim_failure_action``
    is the resolved ``sim_failure`` bucket action handed down the same way;
    ``None`` (production) consults the Rust policy matrix at trap time.

    When the operator has armed the sim-failure trap
    (``simulation.sim_exit_on_fail``), dump the full hop-detail for the FIRST
    failing record and then follow the ``sim_failure`` bucket's action —
    ``sys.exit(3)`` under an ``exit`` policy. The trap exists to capture a
    mainnet fixture to pin a byte-exact calc test.
    """
    failures = outcome.failures
    if not failures:
        return

    cap = _SIM_FAIL_RENDER_CAP
    path_infos = outcome.path_infos
    for rec in failures[:cap]:
        _render_one_failure(rec, path_infos, current_block)

    _enforce_sim_failure_policy(
        failures,
        path_infos,
        current_block,
        sim_exit_on_fail=sim_exit_on_fail,
        exit_ignore_buckets=exit_ignore_buckets,
        sim_failure_action=sim_failure_action,
    )

    overflow = len(failures) - cap
    if overflow > 0:
        bot_logger.info(f"[sim-fail] … (+{overflow} more)")


def _render_one_failure(
    rec: dict[str, Any],
    path_infos: dict[int, dict[str, Any]],
    current_block: int,
) -> None:
    """Render the always-on ``[sim-fail]`` + ``[sim-diag]`` pair for one record."""
    path_id = rec["path_id"]
    bucket = rec["bucket"]
    fail_idx = rec["fail_index"]
    revert_hex = rec["revert_data"]
    path_info = path_infos.get(path_id)
    path_type = path_info["path_type"] if path_info is not None else "?"
    hops = _hop_token_summary(path_info["hops"]) if path_info is not None else "(path_info missing)"
    rf = rec.get("reverting_frame")
    swaps = rec.get("captured_swaps") or []
    if rf is not None:
        revert_line = (
            f"revert@depth={rf['depth']} target={rf['target']} "
            f"sel={rf['selector']} label={rf['label']} kind={rf.get('outcome_kind')} "
            f"gas={rf.get('gas_used')} "
            f"swaps_before={len(swaps)} revert={rf['revert_data']}"
        )
    else:
        revert_line = f"fail_idx={fail_idx} revert={revert_hex}"
    bot_logger.info(
        f"[sim-fail] path={path_id} type={path_type} bucket={bucket} {revert_line} hops={hops}",
    )
    _render_failure_debug(rec, path_id)
    bot_logger.debug(
        format_sim_diag_line(
            rec,
            path_id=path_id,
            path_type=path_type,
            window=SimDiagWindow(solve_block=current_block, block=current_block, age=0),
        )
    )


def _render_failure_debug(rec: dict[str, Any], path_id: int) -> None:
    """Render the per-failure debug detail lines (trace, balances, swap counts)."""
    ct = rec.get("call_trace") or []
    if ct:
        # 2026-08-22 audit: per-failure detail rides at debug; the [sim-fail]
        # bucket summary above is the operator-grade line.
        bot_logger.debug(f"[sim-trace] path={path_id} frames={';'.join(str(x) for x in ct)}")

    weth_before = rec.get("weth_before")
    weth_after = rec.get("weth_after")
    if weth_before is not None and weth_after is not None:
        eb, ea = rec.get("eth_before") or 0, rec.get("eth_after") or 0
        fb, fa = rec.get("erc6909_before") or 0, rec.get("erc6909_after") or 0
        d_w, d_e, d_f = weth_after - weth_before, ea - eb, fa - fb
        bot_logger.debug(
            f"[sim-bals] path={path_id} weth {weth_before}->{weth_after} (d={d_w:+d}) "
            f"| eth {eb}->{ea} (d={d_e:+d}) | erc6909 {fb}->{fa} (d={d_f:+d}) "
            f"| combined d={d_w + d_e + d_f:+d}"
        )

    if rec.get("log_full_count") is not None:
        n_swap = len(rec.get("captured_swaps") or [])
        n_rev = len(rec.get("reverted_swaps") or [])
        bot_logger.debug(
            f"[sim-logfull] path={path_id} log_full={rec.get('log_full_count')} "
            f"captured={n_swap} reverted={n_rev} "
            "(dropped if log_full>captured+reverted)"
        )

    _render_reverted_swaps(rec, path_id)


def _render_reverted_swaps(rec: dict[str, Any], path_id: int) -> None:
    """Render the debug line naming swaps reverted within the simulation."""
    rs = rec.get("reverted_swaps") or []
    if not rs:
        return
    brief = ";".join(
        f"{s.get('family')}:{str(s.get('emitter'))[0:10]}:a0={s.get('amount0')}:a1={s.get('amount1')}"
        for s in rs
    )
    bot_logger.debug(f"[sim-revswaps] path={path_id} n={len(rs)} {brief}")


def _enforce_sim_failure_policy(  # ruff:ignore[too-many-arguments]
    failures: list[dict[str, Any]],
    path_infos: dict[int, dict[str, Any]],
    current_block: int,
    *,
    sim_exit_on_fail: bool,
    exit_ignore_buckets: str,
    sim_failure_action: FailureAction | None = None,
) -> None:
    """Apply the sim-failure tripwire over one failure batch."""
    if not sim_exit_on_fail:
        return
    # Fail HARD and LOUD: ANY un-ignored failure bucket reaches the bot's
    # stop decision below (ADR-021 — detect/classify/stop loudly, never mask).
    # There is NO default ignore set; the operator OPT-IN dumbs the tripwire
    # down per-bucket through the declared `simulation.exit_ignore_buckets`
    # key, so the file layer narrows the trap as readily as the environment.
    ignore = {bucket.strip() for bucket in exit_ignore_buckets.split(",") if bucket.strip()}
    trap_failures = [f for f in failures if f.get("bucket") not in ignore]
    if not trap_failures:
        return
    first = trap_failures[0]
    _dump_failure_fixture(first, path_infos.get(first["path_id"]), current_block)
    # ADR-040: the PER-BUCKET policy decides what happens next — the
    # Rust core owns the closed bucket matrix (single source of truth);
    # Python only consults it. A sim failure's effective action is the
    # `sim_failure` bucket's (reason sub-split lands at the sim seam).
    from degenbot.diagnostics import FailureAction
    from degenbot.diagnostics import failure_action as _policy

    # The resolved action rides the injection seam when the caller handed it
    # down; the default consults the Rust policy matrix at trap time.
    action = sim_failure_action if sim_failure_action is not None else _policy("sim_failure", None)
    if action == FailureAction.EXIT:
        bot_logger.error(
            f"[sim-trap] exiting on first sim failure at block={current_block} "
            f"(failure_policy sim_failure bucket action=exit) "
            f"— see [sim-fixture] above",
        )
        for h in bot_logger.handlers:
            h.flush()
        sys.exit(3)
    else:
        bot_logger.error(
            f"[sim-trap] {len(trap_failures)} sim failure(s) at block={current_block} "
            f"(failure_policy sim_failure action={action}) — continuing; "
            f"failures surface via OTel (degenbot.errors{{kind=sim_failure}}). "
            f"See [sim-fixture] above.",
        )


def _render_fot_tokens(dispatcher: Dispatcher, current_block: int) -> None:
    """Render one ``[fot]`` line per confirmed fee-on-transfer token."""

    fot_tokens = dispatcher.fot_tokens(current_block)

    for token in fot_tokens:
        bot_logger.info(f"[fot] confirmed fee-on-transfer token: {token}")

    if fot_tokens:
        bot_logger.debug(f"[fot] total dropped (lifetime): {dispatcher.total_fot_dropped}")


def format_failure_breakdown(buckets: dict[str, int]) -> str:
    """Render a ``name=count`` breakdown, highest count first (name breaks ties).


    Returns ``""`` for an empty tally so the caller can skip the suffix when no

    failures were classified.


    Returns:

        ``"name=count name=count…"`` ordered by descending count, or ``""``.


    """

    if not buckets:
        return ""

    ordered = sorted(buckets.items(), key=lambda kv: (-kv[1], kv[0]))

    return " ".join(f"{name}={count}" for name, count in ordered)


# Basis points denominator (10_000 = 100%).


@dataclasses.dataclass(frozen=True)
class SimDiagWindow:
    """Block-clock context for a ``[sim-diag]`` line."""

    solve_block: int
    block: int
    age: int


def format_sim_diag_line(
    failure: dict[str, object],
    *,
    path_id: int,
    path_type: str,
    window: SimDiagWindow,
) -> str:
    """Render one always-on ``[sim-diag]`` JSON line per reverted candidate.


    Compares the inspector's captured

    swap amounts (the ACTUAL amounts the in-process EVM emitted) vs the

    solver's reported ``hop_outputs`` (the EXPECTED amounts). No

    ``fetch_onchain``, no ``recompute`` — the captured swaps ARE the ground

    truth (proven byte-exact against mainnet receipts by the

    ``swap_capture_correctness`` probe).


    The line is one compact, machine-parseable JSON object (``json.loads`` on

    the text after the ``[sim-diag] `` prefix) carrying: ``path_id``,

    ``path_type``, ``solve_block``, ``block``, ``age``, ``revert_info``

    (the reverting-frame label — the ``failure["bucket"]``), ``optimal_input``

    (the solver's expected input), ``hop_outputs`` (the solver's expected

    per-hop outputs), and ``captured_swaps`` (the inspector-captured actual

    per-swap amounts). ``logs/permutation_analyzer.py::classify_candidate``

    compares ``hop_outputs[i]`` vs the i-th captured swap's output amount to

    classify SolverCalc / Encoding / Unknown. Never raises — a malformed

    failure emits a best-effort line with the fields it has, so emission never

    blocks the revert path.


    Returns:

        The full ``[sim-diag] ``-prefixed JSON line string.


    """

    payload = {
        "path_id": path_id,
        "path_type": path_type,
        "solve_block": window.solve_block,
        "block": window.block,
        "age": window.age,
        "revert_info": failure.get("bucket", "") or "",
        "optimal_input": failure.get("optimal_input"),
        "hop_outputs": failure.get("hop_outputs", []),
        "captured_swaps": failure.get("captured_swaps", []),
    }

    return "[sim-diag] " + json.dumps(payload, default=str, separators=(",", ":"))
