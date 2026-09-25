"""The session chain id and the one Bot per chain invariant (ADR-006 D5).

One Bot per chain. The chain identity resolves from the installed typed config
unless a caller passes the explicit override, and the connected RPC's
``eth_chainId`` is enforced by the Rust core when a provider is bound, so the
check is the same for the console, a pure-Rust consumer, and the Python
bindings. The binding refusal is exercised against fake nodes in
``tests/provider/test_chain_binding.py``; these tests cover the resolution and
the facade wiring.
"""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING

import pytest

from degenbot._ffi import resolve_chain_id as _ffi_resolve_chain_id
from degenbot.bot import Bot
from degenbot.config import declared_database_path, resolve_chain_id, resolve_database_path
from degenbot.provider import get_provider_from_config
from degenbot.provider.factory import get_provider_from_config as factory_get

if TYPE_CHECKING:
    from pathlib import Path


class _BoundaryProvider:
    """A stand-in for the external RPC boundary (no network, observable chain)."""

    def __init__(self, chain_id: int = 1) -> None:
        self.chain_id = chain_id

    def close(self) -> None:
        """No-op: the real provider's close releases an Arc with no flag."""


class TestSessionChainId:
    def test_the_explicit_override_wins(self) -> None:
        """The override is the top layer, so it is what the session adopts."""
        resolved = _ffi_resolve_chain_id("4242")

        assert resolved.chain_id == 4242
        assert resolved.source == "cli"

    def test_the_installed_layers_answer_when_no_override_is_given(self) -> None:
        """A chain is always resolvable in a suite whose ambient config is pinned."""
        assert resolve_chain_id(None) > 0

    def test_the_python_delegation_matches_the_core(self) -> None:
        """Python adds no layer of its own on top of the core's answer."""
        assert resolve_chain_id("8453") == _ffi_resolve_chain_id("8453").chain_id


class TestFactoryReExport:
    def test_factory_re_exported_from_lib(self) -> None:
        # The canonical factory lives in the lib layer (degenbot.provider),
        # not cli. ``degenbot.provider.get_provider_from_config`` and
        # ``degenbot.provider.factory.get_provider_from_config`` are the same
        # function — lib callers reach it without importing cli.
        assert get_provider_from_config is factory_get


class TestBotChainWiring:
    def test_the_session_adopts_the_resolved_chain(self, tmp_path: Path) -> None:
        with Bot(
            chain_id=4242,
            database=str(tmp_path / "t.db"),
            provider=_BoundaryProvider(4242),
        ) as bot:
            assert bot.chain_id == 4242
            assert bot._py_bot.chain_id == 4242

    def test_a_provider_from_another_chain_is_refused(self, tmp_path: Path) -> None:
        from degenbot.exceptions.base import DegenbotValueError

        with pytest.raises(DegenbotValueError, match="wrong chain"), Bot(
            chain_id=4242,
            database=str(tmp_path / "t.db"),
            provider=_BoundaryProvider(1),
        ):
            pass


class TestDatabasePath:
    """The declared key and the cascade answer are two different questions.

    ``declared_database_path`` is what the operator wrote, with no cascade and
    no expansion; ``resolve_database_path`` is the path a session opens. Asking
    for the declared value when the cascade is what you need is how a ``~``
    ends up resolved against the process cwd.
    """

    def test_the_declared_key_is_not_expanded(self) -> None:
        """The declared value is the operator's text; the resolver expands it."""
        declared = declared_database_path()
        resolved = resolve_database_path(None)

        assert declared.endswith("degenbot.db")
        if declared.startswith("~"):
            assert resolved == str(Path(declared).expanduser())
            assert resolved != declared

    def test_an_explicit_database_override_is_the_top_layer(self) -> None:
        assert resolve_database_path(":memory:") == ":memory:"
