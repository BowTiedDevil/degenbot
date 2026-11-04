"""Configuration reads decided before anything could influence them.

Four values used to be read at import time, so the only value a process could
ever get was the one its import happened to see: a test could not set the
knob, a boot could not, and — where the frozen value doubled as a default
argument — every caller that omitted the argument silently inherited it
through a second path to the same setting. Each test here names the TIMING
property, not the value, so a correct-by-accident value still fails when the
timing regresses.

The operator/executor identity sits on the other side of that line: it is
declared config (``driver.*``), decided by the verdict the process installed
at FFI module init, so a name installed after the import reaches no build.
The identity tests below pin that timing: a fresh interpreter for the
process-level claim, and the hypothetical door for a caller that means a
specific layer.

The fresh-interpreter probes exist because the verdict installs once at FFI
module init: a child process is the only place a layer can be observed
honestly, and a name inherited from this process would be a layer no probe
declared.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys
from pathlib import Path

import pytest

from degenbot.runner.build_paths import BuildPathsOptions, crawl_config
from degenbot.runner.config import (
    _RETIRED_SHELL_KNOBS,
    ArbitrageConfig,
    RpcCascadeOverrides,
)
from tests.helpers import verdict_probe as probe

_REPO_ROOT = Path(__file__).resolve().parents[2]

#: The explicit-override layer, so a config load in this suite never depends on
#: whichever operator file the machine running it happens to carry.
_OVERRIDE = RpcCascadeOverrides(chain_id=1, node="wss://override.example")

#: The env-read gate's Python sanction for the retired-knob refusal. The gate
#: is a cross-language scanner and cannot import the runner's list, so it names
#: the home instead; the names themselves live only in ``config``.
_ENV_READ_GATE = _REPO_ROOT / "rust/crates/foundation/degenbot-config/tests/no_stray_env_reads.rs"

#: The declared identity keys and their pre-prefix spellings: a child must not
#: inherit one, or "the verdict saw nothing" would be a claim about this
#: process's shell rather than about the probe.
_IDENTITY_KEYS = frozenset({
    "OPERATOR_ADDRESS",
    "OPERATOR_PRIVATE_KEY",
    "EXECUTOR_CONTRACT_ADDRESS",
    "INJECTED_EXECUTOR_ADDRESS",
    "EXECUTOR_OWNER_ADDRESS",
    "EXECUTOR_RUNTIME",
})


def _run(code: str, **env: str) -> subprocess.CompletedProcess[str]:
    """Run ``code`` in a fresh interpreter with no inherited config name."""
    child_env = {
        name: value
        for name, value in os.environ.items()
        if not name.startswith("DEGENBOT_") and name not in _IDENTITY_KEYS
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


def test_the_config_requires_a_resolved_path_cap() -> None:
    """No cap argument, no options: there is nothing left to inherit."""
    with pytest.raises(TypeError):
        BuildPathsOptions(discovery_batch_size=1000)


class _Context:
    """The construction-context slice the configuration resolution reads."""

    chain_id = 1

    class construction_route:
        factories = ()
        generic = True

    def __init__(self, database_path: pathlib.Path) -> None:
        self.database_path = database_path


class _Registry:
    verify_snapshot_block = None
    path_predicate = None


def _resolved_cap(max_paths: int, database_path: pathlib.Path):
    return crawl_config(
        context=_Context(database_path),
        engine_registry=_Registry(),
        max_paths=max_paths,
        discovery_batch_size=1000,
        retry_policy=None,
        pool_types=[],
        pool_type_per_depth=None,
    )["path_cap"]


def test_the_cap_the_caller_resolved_is_the_cap_the_engine_gets(
    tmp_path: pathlib.Path,
) -> None:
    """One cap, one owner: the config resolves it and the crawl is configured with it."""
    cfg = probe.build_config(env={"DEGENBOT_MAX_PATHS": "1234"})
    assert cfg.max_registered_paths == 1234
    assert _resolved_cap(cfg.max_registered_paths, tmp_path / "unused.db") == 1234


def test_an_explicitly_uncapped_crawl_tells_the_engine_uncapped(
    tmp_path: pathlib.Path,
) -> None:
    """``0`` is the config's uncapped spelling; the engine's is ``None``."""
    assert _resolved_cap(0, tmp_path / "unused.db") is None


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


# ── Operator/executor identity: decided by the verdict, not by the build ──

_IDENTITY_PROBE = """import os

from degenbot.config import resolved_config
from degenbot.runner.config import ArbitrageConfig, RpcCascadeOverrides



# Installed AFTER the verdict read the environment at FFI module init.

os.environ["OPERATOR_ADDRESS"] = "0x" + "1" * 40

os.environ["OPERATOR_PRIVATE_KEY"] = "0x" + "2" * 32



cfg = ArbitrageConfig.build(

    live=False, permutation=None, rpc=RpcCascadeOverrides(chain_id=1, node="wss://probe.example")

)

print("VERDICT", resolved_config().values.driver.operator_address)

print("CONFIG", cfg.operator_address)

"""


def test_the_identity_is_decided_by_the_installed_verdict() -> None:
    """A name set after the import reaches neither the verdict nor a build.

    The identity is declared config now, so its read timing is the verdict's:
    the cascade installs once, at FFI module init, and every key (``driver.*``
    included) is decided there. A caller that means a specific identity layer
    asks the hypothetical door with that layer.
    """
    proc = _run(_IDENTITY_PROBE)
    assert proc.returncode == 0, f"stdout={proc.stdout!r} stderr={proc.stderr!r}"
    assert "VERDICT None" in proc.stdout, (
        "a name installed after the module init must not reach the installed "
        f"verdict, got {proc.stdout!r}"
    )
    assert "0x1111111111111111111111111111111111111111" not in proc.stdout, (
        f"the late name must not reach a build either, got {proc.stdout!r}"
    )


def test_an_identity_name_before_the_import_reaches_the_verdict() -> None:
    """The complement: the layer present at module init is the layer the verdict keeps."""
    proc = _run(_IDENTITY_PROBE, OPERATOR_ADDRESS="0x" + "3" * 40)
    assert proc.returncode == 0, f"stdout={proc.stdout!r} stderr={proc.stderr!r}"
    assert "VERDICT 0x" + "3" * 40 in proc.stdout, (
        f"the declared key must carry the layer the process booted with, got {proc.stdout!r}"
    )


# live-invariant gate (ADR-087 D2): watches the live `config._RETIRED_SHELL_KNOBS`
# home, which the cross-language env gate must point at rather than re-list; not
# tripwire precedent.
def test_the_env_gate_points_at_the_retired_knob_home() -> None:
    """The names' one functional home is ``config._RETIRED_SHELL_KNOBS``.

    The env-read gate is a cross-language scanner, so it cannot import the
    list; it must point at the home rather than re-list the names.
    """
    gate = _ENV_READ_GATE.read_text(encoding="utf-8")
    for knob in _RETIRED_SHELL_KNOBS:
        assert knob not in gate, (
            "the env-read gate must point at the runner's retired-knob home, "
            f"not carry its own copy of {knob}"
        )
    assert "_RETIRED_SHELL_KNOBS" in gate, (
        "the gate's textual sanction must name the home it points at"
    )
