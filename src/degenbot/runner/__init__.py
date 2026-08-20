"""Settlement-arbitrage runtime driver (``BotRunner``) companion package.

Extracted from ``examples/eth_backrun_v2_v3_v4_rust.py`` / ``eth_backrun_helpers.py``
(epic 5TSYKN). This is the Python-companion ``stays-python`` cockpit over the
Rust-owned engine: it owns config, discovery/registration, result consumption,
and dispatch orchestration — never pool/engine state (ADR-003: ``Bot`` is the
single Rust state owner; ADR-006: ``Bot`` is the per-chain orchestrator, this
package is its deployment cockpit).

The package presents one face: the driver cockpit. Public surface (what this
module re-exports):

- :class:`BotRunner` — the runtime driver facade (the ``start / build_paths /
  consume / dispatch`` seams).
- :class:`ArbitrageConfig` — the unified frozen config (``from_env``).
- :func:`classify_revert` — the public revert-taxonimizer leaf.
- The build family (``build_paths`` / ``PathRegistrationPipeline`` /
  ``ConstructionContext`` / ``run_registration_pipeline`` /
  ``resolve_directions``) and the CLI arg parser (:mod:`degenbot.runner.cli`).

Everything else is private by name (``_consume`` / ``_dispatch`` / ``_render``
/ ``_driver_constants``) and is imported directly by name from its private
module — nothing is smuggled in via the package root. (Epic Y7PA5A, task
34XJ6C.)
"""

from degenbot.runner.bot_runner import BotRunner
from degenbot.runner.build_paths import (
    ConstructionContext,
    PathRegistrationPipeline,
    build_paths,
    resolve_directions,
    run_registration_pipeline,
)
from degenbot.runner.config import ArbitrageConfig, classify_revert

__all__ = [
    "ArbitrageConfig",
    "BotRunner",
    "ConstructionContext",
    "PathRegistrationPipeline",
    "build_paths",
    "classify_revert",
    "resolve_directions",
    "run_registration_pipeline",
]
