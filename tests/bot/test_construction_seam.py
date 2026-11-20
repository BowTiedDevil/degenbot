"""The bot construction seam admits one provider shape: alloy-backed.

`PyBot.attach_construction_io` builds the core `ConstructionIo` — a DB half and
an `AlloyRpcConstruction` RPC half — and every construction read/write on
`BotIo` delegates through that one handle. A provider with no native alloy
handle cannot back the RPC half, so a session booted over one has no
construction I/O at all. The driver therefore refuses that boot, naming what it
must supply, rather than constructing a session whose construction reads and
writes silently answer empty.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from degenbot.bot import Bot
from degenbot.provider import OfflineProvider
from tests.helpers.bot_factory import bot_session
from tests.helpers.seam import Bot as Engine
from tests.helpers.seam import BotIo

if TYPE_CHECKING:
    import pathlib


class _PythonNativeProvider:
    """A driver-shaped provider with no native alloy handle.

    It answers the one question a boot asks before the construction seam
    (`get_chain_id`) and exposes nothing the seam can extract an alloy provider
    from — the shape a hand-built driver provider takes.
    """

    def get_chain_id(self) -> int:
        """Report the chain this provider claims."""
        return 1


def test_a_session_refuses_a_provider_the_seam_cannot_back(tmp_path: pathlib.Path) -> None:
    """A real `Bot` boot refuses a Python-native provider, naming the contract."""
    with pytest.raises(RuntimeError, match="alloy provider"):
        Bot(**bot_session(tmp_path), provider=_PythonNativeProvider())


def test_the_engine_seam_refuses_a_provider_the_seam_cannot_back() -> None:
    """The engine-level attach is the same door and refuses the same shape."""
    with pytest.raises(RuntimeError, match="alloy provider"):
        Engine(chain_id=1).attach_construction_io(_PythonNativeProvider(), None)


def test_the_io_facade_refuses_a_provider_the_seam_cannot_back() -> None:
    """`BotIo` carries the same invariant: no alloy handle, no construction I/O."""
    with pytest.raises(RuntimeError, match="alloy provider"):
        BotIo(provider=_PythonNativeProvider())


def test_a_session_still_boots_over_the_offline_alloy_provider(
    tmp_path: pathlib.Path,
) -> None:
    """The alloy-backed double the suites use boots the production path."""
    provider = OfflineProvider(
        chain_id=1,
        blocks={"1": {"timestamp": 1, "calls": {}, "code": {}}},
    )
    bot = Bot(**bot_session(tmp_path), provider=provider)
    assert bot.provider is provider
