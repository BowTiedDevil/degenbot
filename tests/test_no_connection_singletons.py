"""
Test that the connection module does not export module-level singletons or
legacy convenience functions.

Users should construct their own ConnectionManager or use Bot-owned connections.
"""

import pytest


def test_connection_module_has_no_connection_manager_singleton() -> None:
    import degenbot.connection

    # The module 'connection_manager' (connection_manager.py) is still importable.
    # What we removed is the module-level *instance*, not the class.
    from degenbot.connection.connection_manager import ConnectionManager
    assert ConnectionManager is not None

    # No singleton instance at the package level
    assert not isinstance(degenbot.connection.connection_manager, ConnectionManager)


def test_connection_module_has_no_async_connection_manager_singleton() -> None:
    import degenbot.connection

    from degenbot.connection.async_connection_manager import AsyncConnectionManager
    assert AsyncConnectionManager is not None

    # No singleton instance at the package level
    assert not isinstance(degenbot.connection.async_connection_manager, AsyncConnectionManager)


def test_connection_module_has_no_set_web3() -> None:
    import degenbot.connection

    with pytest.raises(AttributeError):
        degenbot.connection.set_web3  # type: ignore[attr-defined]


def test_connection_module_has_no_get_web3() -> None:
    import degenbot.connection

    with pytest.raises(AttributeError):
        degenbot.connection.get_web3  # type: ignore[attr-defined]


def test_connection_module_has_no_set_provider() -> None:
    import degenbot.connection

    with pytest.raises(AttributeError):
        degenbot.connection.set_provider  # type: ignore[attr-defined]


def test_connection_module_has_no_get_provider() -> None:
    import degenbot.connection

    with pytest.raises(AttributeError):
        degenbot.connection.get_provider  # type: ignore[attr-defined]


def test_connection_module_has_no_set_async_web3() -> None:
    import degenbot.connection

    with pytest.raises(AttributeError):
        degenbot.connection.set_async_web3  # type: ignore[attr-defined]


def test_connection_module_has_no_get_async_web3() -> None:
    import degenbot.connection

    with pytest.raises(AttributeError):
        degenbot.connection.get_async_web3  # type: ignore[attr-defined]


def test_connection_module_still_exports_classes() -> None:
    """The connection classes remain importable."""
    from degenbot.connection import AsyncConnectionManager, ConnectionManager

    assert ConnectionManager is not None
    assert AsyncConnectionManager is not None


def test_top_level_init_has_no_connection_singletons() -> None:
    """from degenbot import connection_manager etc. should fail."""
    import degenbot

    with pytest.raises(AttributeError):
        degenbot.connection_manager  # type: ignore[attr-defined]

    with pytest.raises(AttributeError):
        degenbot.async_connection_manager  # type: ignore[attr-defined]

    with pytest.raises(AttributeError):
        degenbot.set_web3  # type: ignore[attr-defined]

    with pytest.raises(AttributeError):
        degenbot.get_web3  # type: ignore[attr-defined]
