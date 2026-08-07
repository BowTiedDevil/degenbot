"""Dispatch + sim-render helpers for the backrun ``BotRunner``.

Extracted from ``examples/eth_backrun_v2_v3_v4_rust.py`` (epic 5TSYKN, task
DZTFSJ). Owns the encode→simulate→submit leaf
(:func:`_dispatch_profitable` — the ``dispatch_profitable`` /
``dispatch_and_submit`` Rust seam) and the ``[sim]``/``[profit]``/``[sim-fail]``
renderers that contextualize ``DispatchOutcome``.

The renderers are display-only (``stays-python``); all sim/submit arithmetic
runs in the Rust core. Only candidate-list shaping + log rendering happen here.
"""

from __future__ import annotations

import os
import pathlib
import sys
from typing import Any

from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.dispatch import (
    DispatchCandidate,
    Dispatcher,
    DispatchOutcome,
    SimulateContext,
    TxSigner,
    dispatch_and_submit,
    dispatch_profitable,
)
from degenbot.logging import logger as bot_logger
from degenbot.provider import AsyncAlloyProvider
from degenbot.runner.config import format_failure_breakdown, format_sim_diag_line
from degenbot.runner.driver_constants import (
    _SIM_FAIL_RENDER_CAP,
    INJECT_EXECUTOR_CODE,
    MIN_PROFIT_MARGIN_BPS,
    MIN_PROFIT_NET,
)

# Cached runtime bytecode (loaded once, reused across all simulations).
_runtime_bytecode_cache: str | None = None


def _contracts_dir() -> pathlib.Path:
    """Resolve the repository's ``contracts/`` directory robustly.

    Locates the directory holding the cmd_executor runtime bytecode by walking
    up from this module's location. The driver originally lived in
    ``examples/`` where ``parent.parent`` reached the repo root; from the
    package the bytecode dir is several levels up, so search upward instead of
    assuming a fixed depth.
    """
    here = pathlib.Path(__file__).resolve().parent
    for candidate in (here, *here.parents):
        p = candidate / "contracts" / "cmd_executor_runtime_bytecode.txt"
        if p.exists():
            return p
    msg = "cmd_executor_runtime_bytecode.txt not found under contracts/"
    raise FileNotFoundError(msg)


def _load_executor_runtime_bytecode() -> str:
    """Load the patched runtime bytecode from the contracts/ directory.

    The bytecode has all 5 immutable slots baked in: OWNER_ADDR, WETH_ADDR,
    POOL_MANAGER_ADDR, and 2 precomputed delta slots (WETH, NATIVE).
    See contracts/recompile.py for the full layout.
    """
    global _runtime_bytecode_cache
    if _runtime_bytecode_cache is not None:
        return _runtime_bytecode_cache

    bytecode_path = _contracts_dir()
    code = bytecode_path.read_text(encoding="utf-8").strip()
    if not code.startswith("0x"):
        msg = f"Runtime bytecode file must start with 0x, got: {code[:20]}..."
        raise ValueError(msg)
    _runtime_bytecode_cache = code
    bot_logger.info(
        f"[inject] Loaded executor runtime bytecode: "
        f"{len(code) // 2 - 1} bytes from {bytecode_path}",
    )
    return _runtime_bytecode_cache


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


async def _dispatch_profitable(
    *,
    results: list[tuple[int, int, int, tuple[int, ...], tuple[int, ...], int, tuple[int, ...]]],
    engine_registry: EngineRegistry,
    async_w3: AsyncAlloyProvider,
    sim_ctx: SimulateContext | None,
    operator_private_key: str,
    operator_nonce: int,
    dispatcher: Dispatcher,
    current_block: int,
    block_timestamp: int,
    base_fee_next: int,
    dry_run: bool,
) -> None:
    """Encode → simulate → submit a batch of profitable results via the Rust seam.

    The A5 cutover: replaces the Python ``dispatch_profitable_results`` chain
    with ``dispatch_profitable`` (simulate) → ``dispatch_and_submit``
    (submit). The sim fan-out, profit arithmetic, market-aware priority fee,
    path suppression, and thin-margin pre-filter run in the Rust core; Python
    only builds the candidate list, renders the summaries, and chains to the
    submit seam.
    """
    candidates: list[DispatchCandidate] = []
    for pid, inp, prof, ho, ci, sb, sn in results:
        if not ho:
            bot_logger.debug(f"[sim-none] path={pid}: empty hop_outputs")
            continue
        candidates.append(
            DispatchCandidate(
                engine=engine_registry.engine,
                path_id=pid,
                optimal_input=inp,
                engine_profit=prof,
                hop_outputs=list(ho),
                consumed_inputs=list(ci),
                solve_block=sb,
                state_nonces=list(sn),
            ),
        )

    if not candidates:
        return

    if sim_ctx is None:
        msg = "SimulateContext is required to dispatch (non-Alloy provider or sim context unbuilt)"
        raise RuntimeError(msg)

    outcome = await dispatch_profitable(
        candidates=candidates,
        context=sim_ctx,
        dispatcher=dispatcher,
        base_fee_next=base_fee_next,
        current_block=current_block,
        block_timestamp=block_timestamp,
        min_profit_net=MIN_PROFIT_NET,
        min_profit_margin_bps=MIN_PROFIT_MARGIN_BPS,
        engine=engine_registry.engine,
    )
    _render_sim_summary(outcome)
    _render_sim_failures(outcome, current_block=current_block)
    _render_fot_tokens(dispatcher, current_block)
    _render_profit_logs(outcome)

    # ── Submit gas-profitable via the Rust submit leaf ───
    async_alloy = async_w3.as_async_alloy()
    if async_alloy is None:
        bot_logger.error("[dispatch] async_w3 is not an Alloy-backed provider; cannot submit")
        return
    signer = TxSigner(key=operator_private_key, chain_id=1)
    records = await dispatch_and_submit(
        candidates=outcome.gas_profitable,
        dispatcher=dispatcher,
        provider=async_alloy,
        signer=signer,
        operator_nonce=operator_nonce,
        current_block=current_block,
        dry_run=dry_run,
        inject_code=INJECT_EXECUTOR_CODE,
    )
    for record in records:
        if record["kind"] == "submitted":
            bot_logger.info(
                f"Submitted path {record['path_id']} "
                f"hash={record['tx_hash']} nonce={record['nonce']}",
            )
        elif record["reason"] == "pools_claimed":
            bot_logger.debug(f"[dispatch] skip path={record['path_id']}: pools claimed after sim")
        elif record["reason"] == "dry_run":
            pass  # dry_run skip already logged above
        elif record["reason"] == "inject_code":
            bot_logger.warning(
                f"[dispatch] path={record['path_id']}: skipping submission — "
                "INJECT_EXECUTOR_CODE is active",
            )
        elif record["reason"] == "broadcast_failed":
            bot_logger.debug(f"Send failed: {record.get('detail', '')}")


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
            bot_logger.info(f"[sim-trace] path={path_id} frames={';'.join(str(x) for x in ct)}")
        weth_before = rec.get("weth_before")
        weth_after = rec.get("weth_after")
        if weth_before is not None and weth_after is not None:
            eb, ea = rec.get("eth_before") or 0, rec.get("eth_after") or 0
            fb, fa = rec.get("erc6909_before") or 0, rec.get("erc6909_after") or 0
            d_w, d_e, d_f = weth_after - weth_before, ea - eb, fa - fb
            bot_logger.info(
                f"[sim-bals] path={path_id} weth {weth_before}->{weth_after} (d={d_w:+d}) "
                f"| eth {eb}->{ea} (d={d_e:+d}) | erc6909 {fb}->{fa} (d={d_f:+d}) "
                f"| combined d={d_w + d_e + d_f:+d}"
            )
        if rec.get("log_full_count") is not None:
            n_swap = len(rec.get("captured_swaps") or [])
            n_rev = len(rec.get("reverted_swaps") or [])
            bot_logger.info(
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
            bot_logger.info(f"[sim-revswaps] path={path_id} n={len(rs)} {brief}")
        bot_logger.info(
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
            bot_logger.error(
                f"[sim-trap] exiting on first sim failure at block={current_block} "
                f"(DEGENBOT_SIM_EXIT_ON_FAIL=1) — see [sim-fixture] above",
            )
            for h in bot_logger.handlers:
                h.flush()
            sys.exit(3)
    overflow = len(failures) - cap
    if overflow > 0:
        bot_logger.info(f"[sim-fail] … (+{overflow} more)")


def _render_fot_tokens(dispatcher: Dispatcher, current_block: int) -> None:
    """Render one ``[fot]`` line per confirmed fee-on-transfer token."""
    fot_tokens = dispatcher.fot_tokens(current_block)
    for token in fot_tokens:
        bot_logger.info(f"[fot] confirmed fee-on-transfer token: {token}")
    if fot_tokens:
        bot_logger.info(f"[fot] total dropped (lifetime): {dispatcher.total_fot_dropped}")
