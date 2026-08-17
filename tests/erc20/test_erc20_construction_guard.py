"""Erc20Token direct construction is forbidden (ADR-005 Polars-style seam).

An ``Erc20Token`` is a Python companion over a Rust-owned ``Erc20Token``
handle. The handle can only be produced by registering token metadata in a
``Bot`` (production: ``Bot.get_token()``; tests: ``make_erc20``). Direct
constructor calls are rejected so the only paths to a token instance are the
ones that wire the handle — mirroring Polars' ``_from_pydf`` pattern.
"""

from unittest.mock import MagicMock

import pytest

from degenbot.erc20 import Erc20Token


class TestDirectConstructionForbidden:
    """``Erc20Token(...)`` raises ``TypeError`` with a helpful message."""

    def test_no_args_raises_type_error(self) -> None:
        """Calling the constructor with no arguments is rejected."""
        with pytest.raises(TypeError) as exc_info:
            Erc20Token()

        message = str(exc_info.value)
        assert "Bot.get_token" in message
        assert "make_erc20" in message

    def test_with_py_token_handle_raises_type_error(self) -> None:
        """Passing a fake ``Erc20Token`` handle (the old call shape) is rejected."""
        fake_handle = MagicMock()
        with pytest.raises(TypeError) as exc_info:
            Erc20Token(fake_handle, oracle_address="0x" + "1" * 40, state_cache_depth=4)

        message = str(exc_info.value)
        assert "Bot.get_token" in message
        assert "make_erc20" in message
