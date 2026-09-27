"""Configuration reads decided before anything could influence them.

Four values used to be read at import time, so the only value a process could
ever get was the one its import happened to see: a test could not set the
knob, a boot could not, and — where the frozen value doubled as a default
argument — every caller that omitted the argument silently inherited it
through a second path to the same setting. Each test here names the TIMING
property, not the value, so a correct-by-accident value still fails when the
timing regresses.

The fresh-interpreter probes exist because the verdict installs once at FFI
module init: a child process is the only place a layer can be observed
honestly, and a name inherited from this process would be a layer no probe
declared.
"""

from __future__ import annotations

import importlib
import os
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from degenbot.runner.build_paths import PathRegistrationPipeline
from degenbot.runner.config import ArbitrageConfig, RpcCascadeOverrides
from tests.fakes.engine import FakeEngine
from tests.helpers import verdict_probe as probe

_REPO_ROOT = Path(__file__).resolve().parents[2]

#: The MODULE, reached by import name: ``degenbot.runner`` re-exports the
#: ``build_paths`` FUNCTION under the module's own name, so ``from
#: degenbot.runner import build_paths`` binds the function and every attribute
#: probe against it answers about the wrong object.
_BUILD_PATHS_MODULE = importlib.import_module("degenbot.runner.build_paths")

#: The explicit-override layer, so a config load in this suite never depends on
#: whichever operator file the machine running it happens to carry.
_OVERRIDE = RpcCascadeOverrides(chain_id=1, node="wss://override.example")

_RETIRED_SHELL_KNOBS = ("DEGENBOT_REG_QUEUE_BOUND", "DEGENBOT_REG_WORKERS")


def _run(code: str, **env: str) -> subprocess.CompletedProcess[str]:
    """Run ``code`` in a fresh interpreter with no inherited ``DEGENBOT_*`` name."""
    child_env = {
        name: value for name, value in os.environ.items() if not name.startswith("DEGENBOT_")
    }
    child_env.update(env)
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] — trusted source, args list, no shell
        [sys.executable, "-X", "utf8", "-c", code],
        capture_output=True,
        text=True,
        cwd=_REPO_ROOT,
        env=child_env,
        timeout=120,
        check=False,
    )


def _context(bot: Any) -> SimpleNamespace:
    """A construction context carrying only what the pipeline constructor reads."""
    return SimpleNamespace(
        bot=bot,
        chain_id=1,
        database_path=Path("unused.db"),
        uniswap_v3_tracker=None,
        sushiswap_v3_tracker=None,
        pancakeswap_v3_tracker=None,
        weth=None,
    )


class _FleetHostedBot:
    """The pipeline refuses a bot the registration intake is not hosted on."""

    def registration_fleet_hosted(self) -> bool:
        return True


# ── DEGENBOT_DEBUG: the console level ────────────────────────────────────

_BASE_LEVEL_PROBE = """import logging
import os

import degenbot.logging as dlog

print("IMPORT", logging.getLevelName(dlog.logger.level),
      logging.getLevelName(dlog.logger.handlers[0].level))
os.environ["DEGENBOT_DEBUG"] = "1"
dlog.apply_base_log_level()
print("REAPPLY", logging.getLevelName(dlog.logger.level),
      logging.getLevelName(dlog.logger.handlers[0].level))
"""


def test_the_debug_knob_is_read_when_the_base_level_is_applied() -> None:
    """The knob survives; nothing is frozen at import any more.

    ``DEGENBOT_DEBUG`` used to land in a module constant that the logger
    levels and the queue handler were pinned to, so re-deciding the level was
    impossible after the import. The base config is applied by a function that
    reads the knob when it runs, and the queued handler's level moves with it:
    a logger-only level would leave the import-time ceiling in place and the
    knob would still be a no-op for the records it names.
    """
    proc = _run(_BASE_LEVEL_PROBE)
    assert proc.returncode == 0, f"stdout={proc.stdout!r} stderr={proc.stderr!r}"
    assert "IMPORT INFO INFO" in proc.stdout, (
        f"with the knob unset the base level is INFO on the logger and the queue "
        f"handler, got {proc.stdout!r}"
    )
    assert "REAPPLY DEBUG DEBUG" in proc.stdout, (
        "setting the knob after import and re-applying must move the level the "
        f"operator asked for, got {proc.stdout!r}"
    )


# ── DEGENBOT_MAX_PATHS: the registered-path cap ──────────────────────────


def test_the_path_cap_is_not_an_import_time_constant() -> None:
    """The cap is a resolved config value, not a module constant.

    The constant also supplied ``PathRegistrationPipeline``'s ``max_paths``
    default, so a caller that omitted the argument inherited the import's
    value through a path that never touched the config — the same split the
    config object already resolves.
    """
    assert not hasattr(_BUILD_PATHS_MODULE, "MAX_REGISTERED_PATHS"), (
        "the import-time constant is retired: the cap arrives from the caller"
    )


def test_the_pipeline_refuses_to_invent_its_own_path_cap() -> None:
    """No cap argument, no pipeline: there is nothing left to inherit."""
    with pytest.raises(TypeError):
        PathRegistrationPipeline(
            context=_context(_FleetHostedBot()),
            engine_registry=None,
        )


class _CapEngine:
    """Records the cap the pipeline installs, so the test reads it back."""

    def __init__(self) -> None:
        self.path_cap = None

    def set_path_cap(self, cap) -> None:
        self.path_cap = cap


def test_the_cap_the_caller_resolved_is_the_cap_the_engine_gets() -> None:
    """One cap, one owner: the config resolves it and the pipeline installs it."""
    cfg = probe.build_config(env={"DEGENBOT_MAX_PATHS": "1234"})
    engine = _CapEngine()
    PathRegistrationPipeline(
        context=_context(_FleetHostedBot()),
        engine_registry=SimpleNamespace(engine=engine),
        max_paths=cfg.max_registered_paths,
        discovery_batch_size=cfg.discovery_batch_size,
    )

    assert cfg.max_registered_paths == 1234
    assert engine.path_cap == 1234


def test_an_explicitly_uncapped_pipeline_tells_the_engine_uncapped() -> None:
    """``0`` is the config's uncapped spelling; the engine's is ``None``."""
    engine = FakeEngine()
    PathRegistrationPipeline(
        context=_context(_FleetHostedBot()),
        engine_registry=SimpleNamespace(engine=engine),
        max_paths=0,
        discovery_batch_size=1000,
    )
    assert engine.path_cap is None


# ── The retired crawl-shell knobs ────────────────────────────────────────

_IMPORT_PROBE = """import degenbot.runner.identity as dc

print("IMPORTED", dc.WETH_ADDRESS)
"""


@pytest.mark.parametrize("knob", _RETIRED_SHELL_KNOBS)
def test_a_retired_shell_knob_does_not_break_an_unrelated_import(knob: str) -> None:
    """The refusal is the config load's, not the import's.

    At import it fired for anything that touched the module — a config dump, a
    log tail, a test collection — and raised ``RuntimeError``, the one
    exception type no other misconfiguration in this package produces.
    """
    proc = _run(_IMPORT_PROBE, **{knob: "8"})
    assert proc.returncode == 0, (
        f"{knob} must not break an import: stdout={proc.stdout!r} stderr={proc.stderr!r}"
    )
    assert "IMPORTED 0x" in proc.stdout


@pytest.mark.parametrize("knob", _RETIRED_SHELL_KNOBS)
def test_building_a_config_refuses_a_retired_shell_knob(
    monkeypatch: pytest.MonkeyPatch, knob: str
) -> None:
    """A caller building a config is told, in the shape every other refusal uses."""
    monkeypatch.setenv(knob, "8")
    with pytest.raises(ValueError, match=knob) as excinfo:
        ArbitrageConfig.build(live=False, permutation=None, rpc=_OVERRIDE)
    message = str(excinfo.value)
    assert "retired" in message.lower(), message
    assert "PoolStateUpdater" in message, (
        "the refusal must name what replaced the knob, not only that it is gone"
    )


def test_a_config_load_succeeds_with_no_retired_knob_present(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """No retired knob in the environment, no refusal: presence is the signal."""
    for knob in _RETIRED_SHELL_KNOBS:
        monkeypatch.delenv(knob, raising=False)
    # A devcontainer exports its own path cap, and the cap is a declared key,
    # so the value this build carries is whatever the process installed. What
    # this test is about is that the load completes at all, so it asserts the
    # declared shape rather than an ambient number.
    cfg = ArbitrageConfig.build(live=False, permutation=None, rpc=_OVERRIDE)

    assert isinstance(cfg.max_registered_paths, int)
    assert cfg.max_registered_paths >= 0
