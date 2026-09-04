"""Display rendering for the driver's revert and diagnostic output.

Private leaf module (underscore name): pure renderers over dispatch
outcomes and engine failure records. The dispatch path
(:mod:`~degenbot.runner._dispatch`) imports :func:`_dump_failure_fixture`,
:func:`_render_profit_logs`, and :func:`_render_sim_failures`; outside
that, only tests import this module. Nothing here touches the Rust core.

Carved out of ``dispatch.py`` / ``config.py`` by epic Y7PA5A (task
34XJ6C) so that ``degenbot.runner`` presents one face: the driver
cockpit.

"""

from __future__ import annotations

import json
import os
import sys
from typing import TYPE_CHECKING, Any

from degenbot.logging import logger as bot_logger
from degenbot.runner._driver_constants import _SIM_FAIL_RENDER_CAP

if TYPE_CHECKING:
    from degenbot.dispatch import Dispatcher, DispatchOutcome


def _hop_display_addr(hop: dict[str, Any]) -> str:
    """Return a short display address for logging (WEFVGE: plain-dict hop)."""

    family = hop["family"]

    if family in {"V2", "V3"}:
        return hop["pool_address"]

    return hop["pool_id_hex"]


def _hop_token_summary(hops: list[dict[str, Any]] | tuple[dict[str, Any], ...]) -> str:
    """One-line summary of hop input→output tokens for sim-fail diagnostics.


    WEFVGE: reads plain dicts (the ``outcome.path_infos`` render shape).

    """

    parts: list[str] = []

    for h in hops:
        family = h["family"]

        if family in {"V2", "V3"}:
            t0, t1 = h["token0_address"], h["token1_address"]

        else:
            t0, t1 = h["currency0_address"], h["currency1_address"]

        parts.append(f"{t0}→{t1}{'↗' if h['zfo'] else '↘'}")

    return " ".join(parts)


def _render_sim_summary(outcome: DispatchOutcome) -> None:
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

    bot_logger.info(
        f"[sim] {outcome.candidate_count} candidates: "
        f"{sim_ok} ok ({len(profitable)} profitable, "
        f"{outcome.gas_unprofitable_count} below threshold), "
        f"{outcome.fail_count} failed, {outcome.exception_count} exceptions"
        f"{f' — best net={best_net // 10**9}gwei' if profitable else ''}"
        f"{f' — by reason: {breakdown}' if breakdown else ''}"
        f"{extra}",
    )


def _render_profit_logs(outcome: DispatchOutcome) -> None:
    """Render the ``[profit]`` per-path hop-detail log (D4 stay-Python)."""

    for cand in outcome.gas_profitable:
        path_info = outcome.path_infos.get(cand.path_id)

        hop_details = []

        if path_info is not None:
            for i, h in enumerate(path_info["hops"]):
                family = h["family"]

                if family == "V2":
                    hop_details.append(
                        f"  hop[{i}] V2 addr={h['pool_address']} "
                        f"t0={h['token0_address']} t1={h['token1_address']} "
                        f"fee={h['fee']} zfo={h['zfo']}",
                    )

                elif family == "V3":
                    hop_details.append(
                        f"  hop[{i}] V3 addr={h['pool_address']} "
                        f"t0={h['token0_address']} t1={h['token1_address']} "
                        f"fee={h['fee']} zfo={h['zfo']}",
                    )

                elif family == "V4":
                    hop_details.append(
                        f"  hop[{i}] V4 pm={h['pool_manager_address']} "
                        f"pid={h['pool_id_hex']} "
                        f"c0={h['currency0_address']} c1={h['currency1_address']} "
                        f"fee={h['fee']} ts={h['tick_spacing']} zfo={h['zfo']}",
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
    """Dump the full hop detail for a failing candidate — the W2UWZO trap."""

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
        family = h.get("family")

        if family in {"V2", "V3"}:
            addr = h.get("pool_address")

            t0, t1 = h.get("token0_address", "?"), h.get("token1_address", "?")

            bot_logger.error(
                f"[sim-fixture] hop[{i}] {family} pool={addr} "
                f"t0={t0} t1={t1} fee={h.get('fee')} zfo={h.get('zfo')}",
            )

        else:  # V4
            pm = h.get("pool_manager_address", "?")

            pid = h.get("pool_id_hex", "?")

            c0, c1 = h.get("currency0_address", "?"), h.get("currency1_address", "?")

            bot_logger.error(
                f"[sim-fixture] hop[{i}] V4 pool_manager={pm} pool_id={pid} "
                f"c0={c0} c1={c1} fee={h.get('fee')} "
                f"tick_spacing={h.get('tick_spacing')} zfo={h.get('zfo')}",
            )

    for j, s in enumerate(captured):
        bot_logger.error(
            f"[sim-fixture] captured[{j}] family={s.get('family')} "
            f"emitter={s.get('emitter')} amount0={s.get('amount0')} "
            f"amount1={s.get('amount1')} sqrt_price={s.get('sqrt_price_x96')} "
            f"liquidity={s.get('liquidity')} tick={s.get('tick')}",
        )


def _render_sim_failures(outcome: DispatchOutcome, *, current_block: int) -> None:
    """Render one ``[sim-fail]`` + one ``[sim-diag]`` line per reverted / failed

    candidate (D3 + AM5AJW). Capped at :data:`_SIM_FAIL_RENDER_CAP` records.


    If ``DEGENBOT_SIM_EXIT_ON_FAIL=1`` is set, dump the full hop-detail for the

    FIRST failing record then ``sys.exit(3)`` — a trap for capturing a mainnet

    fixture to pin a RED byte-exact calc test (ergo W2UWZO).

    """

    failures = outcome.failures

    if not failures:
        return

    cap = _SIM_FAIL_RENDER_CAP

    path_infos = outcome.path_infos

    for rec in failures[:cap]:
        path_id = rec["path_id"]

        bucket = rec["bucket"]

        fail_idx = rec["fail_index"]

        revert_hex = rec["revert_data"]

        path_info = path_infos.get(path_id)

        path_type = path_info["path_type"] if path_info is not None else "?"

        hops = (
            _hop_token_summary(path_info["hops"])
            if path_info is not None
            else "(path_info missing)"
        )

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

        rs = rec.get("reverted_swaps") or []

        if rs:
            brief = ";".join(
                f"{s.get('family')}:{str(s.get('emitter'))[0:10]}:a0={s.get('amount0')}:a1={s.get('amount1')}"
                for s in rs
            )

            bot_logger.debug(f"[sim-revswaps] path={path_id} n={len(rs)} {brief}")

        bot_logger.debug(
            format_sim_diag_line(
                rec,
                path_id=path_id,
                path_type=path_type,
                solve_block=current_block,
                block=current_block,
                age=0,
            )
        )

    if os.environ.get("DEGENBOT_SIM_EXIT_ON_FAIL", "1") == "1":
        # Fail HARD and LOUD: ANY un-ignored failure bucket halts the bot

        # (ADR-021 / ergo W2UWZO — detect/classify/stop loudly, never mask).

        # There is NO default ignore set; the operator OPT-IN dumbs the tripwire

        # down per-bucket via DEGENBOT_SIM_EXIT_IGNORE_BUCKETS.

        ignore = {
            b.strip()
            for b in os.environ.get("DEGENBOT_SIM_EXIT_IGNORE_BUCKETS", "").split(",")
            if b.strip()
        }

        trap_failures = [f for f in failures if f.get("bucket") not in ignore]

        if trap_failures:
            first = trap_failures[0]

            _dump_failure_fixture(first, path_infos.get(first["path_id"]), current_block)

            # ADR-040: the PER-BUCKET policy decides what happens next — the
            # Rust core owns the closed bucket matrix (single source of truth);
            # Python only consults it. A sim failure's effective action is the
            # `sim_failure` bucket's (reason sub-split lands at the sim seam).
            from degenbot.diagnostics import failure_action as _policy

            action = _policy("sim_failure", None)
            if action == "exit":
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

    overflow = len(failures) - cap

    if overflow > 0:
        bot_logger.info(f"[sim-fail] … (+{overflow} more)")


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


def format_sim_diag_line(
    failure: dict[str, object],
    *,
    path_id: int,
    path_type: str,
    solve_block: int,
    block: int,
    age: int,
) -> str:
    """Render one always-on ``[sim-diag]`` JSON line per reverted candidate.


    Ergo epic 63I7WJ (task AM5AJW): re-pointed at the inspector's captured

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
        "solve_block": solve_block,
        "block": block,
        "age": age,
        "revert_info": failure.get("bucket", "") or "",
        "optimal_input": failure.get("optimal_input"),
        "hop_outputs": failure.get("hop_outputs", []),
        "captured_swaps": failure.get("captured_swaps", []),
    }

    return "[sim-diag] " + json.dumps(payload, default=str, separators=(",", ":"))
