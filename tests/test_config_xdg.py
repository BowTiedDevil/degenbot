"""XDG Base Directory defaults for the Python config surface.

Config lives under ``$XDG_CONFIG_HOME`` (else ``$HOME/.config``); the
connector-index database and other durable state live under
``$XDG_STATE_HOME`` (else ``$HOME/.local/state``). A relative or empty XDG
variable is ignored per the spec.
"""

from pathlib import Path

from degenbot.config import DB_PATH, _xdg_config_home, _xdg_state_home


def _clear_xdg(monkeypatch) -> None:
    monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
    monkeypatch.delenv("XDG_STATE_HOME", raising=False)


def test_xdg_state_home_absolute_wins(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "state"))
    assert _xdg_state_home() == tmp_path / "state"


def test_xdg_state_home_unset_falls_back_to_home(monkeypatch) -> None:
    _clear_xdg(monkeypatch)
    assert _xdg_state_home() == Path.home() / ".local" / "state"


def test_xdg_state_home_empty_or_relative_is_ignored(monkeypatch) -> None:
    for value in ("", "relative/state"):
        monkeypatch.setenv("XDG_STATE_HOME", value)
        assert _xdg_state_home() == Path.home() / ".local" / "state"


def test_xdg_config_home_absolute_wins(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "cfg"))
    assert _xdg_config_home() == tmp_path / "cfg"


def test_xdg_config_home_unset_falls_back_to_home(monkeypatch) -> None:
    _clear_xdg(monkeypatch)
    assert _xdg_config_home() == Path.home() / ".config"


def test_xdg_config_home_empty_or_relative_is_ignored(monkeypatch) -> None:
    for value in ("", "relative/config"):
        monkeypatch.setenv("XDG_CONFIG_HOME", value)
        assert _xdg_config_home() == Path.home() / ".config"


def test_db_default_is_state_home_rooted(monkeypatch) -> None:
    _clear_xdg(monkeypatch)
    assert Path.home() / ".local" / "state" / "degenbot" / "db" / "degenbot.db" == DB_PATH
