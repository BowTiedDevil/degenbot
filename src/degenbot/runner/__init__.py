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
- :class:`ArbitrageConfig` — the unified frozen config (``build``).
- The build family (``build_paths`` / ``PathRegistrationPipeline`` /
  ``ConstructionContext`` / ``resolve_directions``) and the CLI arg parser
  (:mod:`degenbot.runner.cli`). PRG-5: the bounded crawl shell retired —
  the crawl is the fleet-hosted intake now.

Everything else is private by name (``_consume`` / ``_dispatch`` / ``_render``
/ ``_driver_constants``) and is imported directly by name from its private
module — nothing is smuggled in via the package root.
"""

from degenbot.runner.bot_runner import BotRunner
from degenbot.runner.build_paths import (
    ConstructionContext,
    PathRegistrationPipeline,
    build_paths,
    resolve_directions,
)
from degenbot.runner.config import ArbitrageConfig

__all__ = [
    "ArbitrageConfig",
    "BotRunner",
    "ConstructionContext",
    "PathRegistrationPipeline",
    "build_paths",
    "resolve_directions",
]
