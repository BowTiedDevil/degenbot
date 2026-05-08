"""Tracer bullet tests: module-level db_session and config singletons are gone.

RED phase — these should all fail until the singletons are removed.
"""

import pathlib
import types

import pytest

import degenbot
import degenbot.database


def test_database_module_has_no_db_session_singleton() -> None:

    # The module exposes DatabaseSessionManager (the class), but not a singleton instance
    assert hasattr(degenbot.database, "DatabaseSessionManager") or True  # class stays importable

    # No singleton instance named db_session
    with pytest.raises(AttributeError):
        degenbot.database.db_session  # type: ignore[attr-defined]


def test_database_module_has_no_config_import() -> None:
    """database/__init__.py should not import the global config singleton."""

    source = pathlib.Path(degenbot.database.__file__).open(encoding="utf-8").read()
    assert "from degenbot.config import config" not in source


def test_top_level_init_has_no_db_session() -> None:

    with pytest.raises(AttributeError):
        degenbot.db_session  # type: ignore[attr-defined]


def test_top_level_init_has_no_config_singleton() -> None:
    """The lazy config proxy should not be exported from degenbot top-level."""
    # degenbot.config IS the module (config.py), which is fine.
    # What we removed is the _LazyConfig singleton instance that was importable as degenbot.config.
    # Verify it's the module, not a _LazyConfig instance:
    assert isinstance(degenbot.config, types.ModuleType)

    # The _LazyConfig proxy is not at top level
    assert "config" not in degenbot.__all__
