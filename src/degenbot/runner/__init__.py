"""Backrun runtime driver (``BotRunner``) companion package.

Extracted from ``examples/eth_backrun_v2_v3_v4_rust.py`` / ``eth_backrun_helpers.py``
(epic 5TSYKN). This is the Python-companion ``stays-python`` cockpit over the
Rust-owned engine: it owns config, discovery/registration, result consumption,
and dispatch orchestration — never pool/engine state (ADR-003: ``Bot`` is the
single Rust state owner; ADR-006: ``Bot`` is the per-chain orchestrator, this
package is its deployment cockpit).

Public surface (re-exported here):
- :class:`BackrunConfig` + config/display helpers (:mod:`degenbot.runner.config`)
- :class:`BotRunner` — the runtime driver facade (added in task DKUOBL).
"""

from degenbot.runner.config import (
    BPS_DENOM,
    BackrunConfig,
    EngineResult,
    classify_revert,
    filter_thin_margin_results,
    format_failure_breakdown,
    format_sim_diag_line,
)

__all__ = [
    "BPS_DENOM",
    "BackrunConfig",
    "EngineResult",
    "classify_revert",
    "filter_thin_margin_results",
    "format_failure_breakdown",
    "format_sim_diag_line",
]
