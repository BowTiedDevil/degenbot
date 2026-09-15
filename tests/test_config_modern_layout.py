"""Tests for the 0.6 modern config.toml layout loading in Python.

The shared operator file ~/.config/degenbot/config.toml is now the typed
Rust BotConfig file layer: its tables are the schema sections (telemetry,
failure_policy, ...), and the pre-0.6 Python-domain sections ([database],
[rpc]/[ws], default_chain_id) have LEFT the file (they resolve through the
env/cascade layers). The Python DegenbotConfig therefore must load a
modern-layout file that carries NO Python-domain sections at all, defaulting
database to the standard DB_PATH and leaving rpc/ws empty (the RPC cascade
then resolves endpoints from env or the caller).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from degenbot import config as config_module
from degenbot.config import CONFIG_DIR, _init_config, load_config_from_file

if TYPE_CHECKING:
    from pathlib import Path


# The envvar name of the Python-cascade chain-id layer (env > retired file key).
_CHAIN_ID_ENV_VAR = "DEGENBOT_DEFAULT_CHAIN_ID"


# The modern layout: ONLY typed-Rust schema sections (both are also read by
# the live Python readers — failure_policy by the Rust log layer, telemetry
# by nothing on the Python side — plus it exercises the default extra-key
# posture for every other schema section).
MODERN_LAYOUT = """\
[telemetry]
otel = true
jaeger_endpoint = "http://localhost:4318"
metrics_addr = "0.0.0.0:9464"

[failure_policy]
"""


def test_modern_layout_file_loads_in_python(tmp_path: Path) -> None:
    """A modern-layout file with no Python-domain sections loads with defaults."""
    cfg = tmp_path / "config.toml"
    cfg.write_text(MODERN_LAYOUT, encoding="utf-8")

    config = load_config_from_file(cfg)

    from degenbot.config import DB_PATH

    assert config.database.path == DB_PATH, "database defaults to the standard path"
    assert config.rpc == {}, "rpc is empty (env/cascade layer owns endpoints)"
    assert config.ws == {}, "ws is empty (env/cascade layer owns endpoints)"
    assert config.default_chain_id is None
    assert config.failure_policy == {}


def test_modern_layout_sections_are_ignored_not_errors(tmp_path: Path) -> None:
    """Unknown-to-Python schema sections (telemetry ...) load without error."""
    cfg = tmp_path / "config.toml"
    cfg.write_text(MODERN_LAYOUT + "\n[runtime]\nio_workers = 4\n", encoding="utf-8")

    config = load_config_from_file(cfg)
    assert config.rpc == {}
    assert CONFIG_DIR.name == "degenbot", "sanity: standard config dir"


# ──────────────────────────────────────────────────────────────────────
# The Python-cascade default_chain_id env layer (JLFE2F follow-up)
#
# The 0.6 cutover retired top-level `default_chain_id` from the operator
# file (the typed Rust loader refuses it at boot — degenbot-python/src/
# lib.rs exits 2), but the Python CLI boot (`_init_config()`) could only
# obtain the chain id from that same file, so every `Bot.from_config_file()`
# subcommand was unresolvable. The cascade gains the env layer the migration
# doc promised: env `DEGENBOT_DEFAULT_CHAIN_ID` > (retired) file key.
# ──────────────────────────────────────────────────────────────────────


@pytest.fixture
def isolated_config_file(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Point ``CONFIG_FILE`` at a modern-layout file in a tmp path."""
    cfg = tmp_path / "config.toml"
    cfg.write_text(MODERN_LAYOUT, encoding="utf-8")
    monkeypatch.setattr(config_module, "CONFIG_FILE", cfg)
    return cfg


def test_env_supplies_default_chain_id_when_file_has_none(
    isolated_config_file: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A modern-layout file (no Python-domain keys) plus env gives a chain id.

    This is the devcontainer/default-operator setup: config.toml carries only
    typed-Rust sections, and the chain id comes from the environment.
    """
    monkeypatch.setenv(_CHAIN_ID_ENV_VAR, "8453")

    config = _init_config()

    assert config.default_chain_id == 8453


def test_env_overrides_a_retired_file_key(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The env layer outranks the retired file key (mirrors the Rust cascade)."""
    cfg = tmp_path / "config.toml"
    cfg.write_text(
        "default_chain_id = 137\n\n" + MODERN_LAYOUT, encoding="utf-8"
    )
    monkeypatch.setattr(config_module, "CONFIG_FILE", cfg)
    monkeypatch.setenv(_CHAIN_ID_ENV_VAR, "1")

    config = _init_config()

    assert config.default_chain_id == 1


def test_malformed_env_value_raises_pointed_error(
    isolated_config_file: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A non-integer chain id fails with a message naming the envvar."""
    monkeypatch.setenv(_CHAIN_ID_ENV_VAR, "mainnet")

    with pytest.raises(ValueError, match=_CHAIN_ID_ENV_VAR):
        _init_config()


def test_unset_env_leaves_default_chain_id_none(
    isolated_config_file: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Without the env layer the modern-layout file yields no chain id.

    ``Bot`` then refuses to construct with its own pointed error — this test
    pins the resolver contract, not the Bot refusal (tested by the Bot).
    """
    monkeypatch.delenv(_CHAIN_ID_ENV_VAR, raising=False)

    config = _init_config()

    assert config.default_chain_id is None


def test_fresh_init_creates_no_config_file(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Fresh bootstrap writes NO operator file (the typed Rust layer owns it).

    The pre-0.6 bootstrap saved the full ``DegenbotConfig`` dump — including
    the retired ``default_chain_id`` key the typed loader refuses, and which
    TOML cannot even represent when unset (tomlkit raises on ``None``). The
    modern bootstrap creates the directory + database only; an absent user
    file is contractually schema defaults. The env chain id still applies in
    memory (not persisted to the shared file).
    """
    config_dir = tmp_path / "cfgdir"
    config_file = config_dir / "config.toml"
    monkeypatch.setattr(config_module, "CONFIG_DIR", config_dir)
    monkeypatch.setattr(config_module, "CONFIG_FILE", config_file)
    monkeypatch.setattr(
        config_module,
        "create_new_sqlite_database",
        lambda **_kwargs: None,
    )
    monkeypatch.setenv(_CHAIN_ID_ENV_VAR, "1")

    config = _init_config()

    assert config_dir.is_dir(), "the configuration directory is still created"
    assert not config_file.exists(), "no operator file is written by the bootstrap"
    assert config.default_chain_id == 1, "the env chain id applies in memory"
    # Whatever lands in the file tree must stay loadable by the typed loader's
    # TOML subset — sanity via a round-trip (nothing was written).
    assert not any(config_dir.glob("*.toml")), "no TOML artifacts at all"
