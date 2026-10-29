"""Settlement-arbitrage runtime driver (``BotRunner``) companion package.

Extracted from ``examples/eth_backrun_v2_v3_v4_rust.py`` / ``eth_backrun_helpers.py``.
This is the Python-companion ``stays-python`` cockpit over the
Rust-owned engine: it owns config, discovery/registration, result consumption,
and dispatch orchestration — never pool/engine state (ADR-003: ``Bot`` is the
single Rust state owner; ADR-006: ``Bot`` is the per-chain orchestrator, this
package is its deployment cockpit).

The package presents one face: the driver cockpit. Public surface (what this
module re-exports):

- :class:`BotRunner` — the operator verbs over one session
  (``start / run / enqueue_path / trigger_discovery / shutdown``).
- :class:`Session` — the one owner of a pump session's coordination state, with
  its lifecycle FSM (:class:`SessionPhase`, and :class:`PhaseError` as the
  operator-facing refusal) and the test construction seam
  (:meth:`Session.for_test`).
- :class:`InjectedActors` / :class:`Actors` — the frozen DI value and the
  resolved collaborator record a session carries.
- :class:`ActivationGateRefused` — the activation gate's boot refusal.
- :class:`ArbitrageConfig` — the unified frozen config (``build``).
- The build family (``build_paths`` / ``PathRegistrationPipeline`` /
  ``ConstructionContext`` / ``resolve_directions``) and the CLI arg parser
  (:mod:`degenbot.runner.cli`). PRG-5: the bounded crawl shell retired —
  the crawl is the fleet-hosted intake now.

Everything else is private by name (``_consume`` / ``_dispatch`` / ``_render``
/ ``identity``) and is imported directly by name from its private
module — nothing is smuggled in via the package root.
"""

from degenbot.runner._actors import ActivationGateRefused
from degenbot.runner.bot_runner import BotRunner
from degenbot.runner.build_paths import (
    ConstructionContext,
    PathRegistrationPipeline,
    build_paths,
    resolve_directions,
)
from degenbot.runner.config import ArbitrageConfig
from degenbot.runner.session import Actors, InjectedActors, PhaseError, Session, SessionPhase

__all__ = [
    "ActivationGateRefused",
    "Actors",
    "ArbitrageConfig",
    "BotRunner",
    "ConstructionContext",
    "InjectedActors",
    "PathRegistrationPipeline",
    "PhaseError",
    "Session",
    "SessionPhase",
    "build_paths",
    "resolve_directions",
]
