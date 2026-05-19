"""Tests for Bot passing SyncPoolIO to builders (Plan 048, Slice 2)."""

from __future__ import annotations

from unittest.mock import MagicMock

from degenbot.bot import Bot
from degenbot.builders.pool_io import SyncPoolIO
from degenbot.checksum_cache import get_checksum_address


class TestBotPassesPoolIO:
    """Bot.build_pool() creates SyncPoolIO and passes it to builders."""

    def test_dispatch_build_passes_io(self) -> None:
        """_dispatch_build forwards io= to the builder."""
        builder = MagicMock()
        builder.build.return_value = MagicMock()

        address = get_checksum_address("0x0000000000000000000000000000000000000001")
        io = MagicMock(spec=SyncPoolIO)

        Bot._dispatch_build(
            builder=builder,
            address=address,
            chain_id=1,
            io=io,
        )

        builder.build.assert_called_once_with(address, chain_id=1, io=io)
