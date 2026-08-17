"""Settlement-arbitrage runtime driver (``BotRunner``) companion package.

Extracted from ``examples/eth_backrun_v2_v3_v4_rust.py`` / ``eth_backrun_helpers.py``
(epic 5TSYKN). This is the Python-companion ``stays-python`` cockpit over the
Rust-owned engine: it owns config, discovery/registration, result consumption,
and dispatch orchestration — never pool/engine state (ADR-003: ``Bot`` is the
single Rust state owner; ADR-006: ``Bot`` is the per-chain orchestrator, this
package is its deployment cockpit).

Public surface (re-exported here):
- :class:`BotRunner` — the runtime driver facade (the ``start/build_paths/
  consume/dispatch`` seams).
- :class:`ArbitrageConfig` + config/display helpers (:mod:`degenbot.runner.config`)
- Discovery + registration (:mod:`degenbot.runner.build_paths`)
- The permanent main loop (:mod:`degenbot.runner.consume`)
"""

from degenbot.runner.bot_runner import BotRunner
from degenbot.runner.build_paths import (
    ConstructionContext,
    PathRegistrationPipeline,
    build_paths,
    resolve_directions,
    run_registration_pipeline,
)
from degenbot.runner.config import (
    BPS_DENOM,
    ArbitrageConfig,
    EngineResult,
    classify_revert,
    filter_thin_margin_results,
    format_failure_breakdown,
    format_sim_diag_line,
)
from degenbot.runner.consume import consume_result_batches

__all__ = [
    "BPS_DENOM",
    "ArbitrageConfig",
    "BotRunner",
    "ConstructionContext",
    "EngineResult",
    "PathRegistrationPipeline",
    "build_paths",
    "classify_revert",
    "consume_result_batches",
    "filter_thin_margin_results",
    "format_failure_breakdown",
    "format_sim_diag_line",
    "resolve_directions",
    "run_registration_pipeline",
]
