"""Block-tag contract parity across the provider adapters (ADR-086).

``get_block`` answers ONE identifier contract on every adapter: the
``'latest'`` / ``'earliest'`` / ``'pending'`` ladder resolves against the
provider's own head, an identifier outside the ladder fails loud at the
boundary (``BlockTag.parse``'s ``ValueError``), and a valid numeric block the
backend does not serve returns ``None``. The recorded-data adapter keeps those
outcomes apart: an absent cassette row is a ``None``, a bad identifier is a
raise.

The offline contract is pinned node-free below; the cross-adapter agreement is
observed through each tree's public ``get_block`` over the seeded standalone
anvil plus a two-block recording, so no assertion reaches past the provider
surface.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

import pytest

from degenbot.provider import AlloyProvider, AsyncAlloyProvider, OfflineProvider

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

    from degenbot.fork import AnvilFork


# A recording with block 0 and head block 5: 'earliest' and 'latest' both have
# recorded rows, head + 1 ('pending') and any number past 5 do not.
_RECORDING = {
    "0": {"timestamp": 1_700_000_000, "calls": {}, "code": {}},
    "5": {"timestamp": 1_700_000_100, "calls": {}, "code": {}},
}

# The identifier ladder, stated as spec: where each tag resolves against the
# provider's own head.
_TAG_POSITION = {
    "latest": lambda head: head,
    "earliest": lambda head: 0,
    "pending": lambda head: head + 1,
}


def _offline_provider() -> OfflineProvider:
    return OfflineProvider(chain_id=1, blocks=_RECORDING)


@pytest.fixture
async def adapters(
    standalone_anvil: AnvilFork,
) -> AsyncIterator[tuple[AlloyProvider, AsyncAlloyProvider, OfflineProvider]]:
    """The three adapters, each over a backend that serves no head + 1 block."""
    sync = AlloyProvider(standalone_anvil.http_url)
    async_provider = await AsyncAlloyProvider.create(standalone_anvil.http_url)
    try:
        yield sync, async_provider, _offline_provider()
    finally:
        sync.close()
        async_provider.close()


class TestOfflineGetBlockContract:
    """The recorded-data adapter's identifier contract, node-free."""

    def test_latest_resolves_to_the_recorded_head(self) -> None:
        provider = _offline_provider()
        head = provider.get_block_number()
        assert head == 5

        block = provider.get_block("latest")
        assert block is not None
        assert block["number"] == head

    def test_earliest_resolves_to_block_zero(self) -> None:
        provider = _offline_provider()

        block = provider.get_block("earliest")
        assert block is not None
        assert block["number"] == 0

    def test_pending_resolves_just_past_the_head(self) -> None:
        provider = _offline_provider()
        head = provider.get_block_number()

        assert provider.get_block("pending") == provider.get_block(head + 1) is None

    @pytest.mark.parametrize("unsupported", ["finalized", "safe"])
    def test_unsupported_tag_fails_loud(self, unsupported: str) -> None:
        with pytest.raises(ValueError, match="Unsupported block identifier"):
            _offline_provider().get_block(unsupported)

    @pytest.mark.parametrize(
        ("malformed", "error"),
        [("banana", ValueError), (None, TypeError)],
    )
    def test_malformed_identifier_fails_loud(
        self,
        malformed: Any,
        error: type[Exception],
    ) -> None:
        with pytest.raises(error):
            _offline_provider().get_block(malformed)

    def test_absent_numeric_block_is_the_only_none(self) -> None:
        provider = _offline_provider()

        assert provider.get_block(99) is None
        assert provider.get_block(5) is not None


class TestAdapterGetBlockParity:
    """All three adapters answer the identifier contract identically."""

    @pytest.mark.parametrize("tag", ["latest", "earliest", "pending"])
    async def test_tags_resolve_against_each_own_head(
        self,
        adapters: tuple[AlloyProvider, AsyncAlloyProvider, OfflineProvider],
        tag: str,
    ) -> None:
        sync, async_provider, offline = adapters

        blocks = (
            sync.get_block(tag),
            await async_provider.get_block(tag),
            offline.get_block(tag),
        )
        heads = (
            sync.get_block_number(),
            await async_provider.get_block_number(),
            offline.get_block_number(),
        )

        for block, head in zip(blocks, heads, strict=True):
            expected = _TAG_POSITION[tag](head)
            if tag == "pending":
                assert block is None
            else:
                assert block is not None
                assert block["number"] == expected

    @pytest.mark.parametrize("identifier", ["finalized", "banana"])
    async def test_invalid_strings_refuse_identically(
        self,
        adapters: tuple[AlloyProvider, AsyncAlloyProvider, OfflineProvider],
        identifier: str,
    ) -> None:
        sync, async_provider, offline = adapters

        with pytest.raises(ValueError, match="Unsupported block identifier") as sync_refusal:
            sync.get_block(identifier)
        with pytest.raises(ValueError, match="Unsupported block identifier") as async_refusal:
            await async_provider.get_block(identifier)
        with pytest.raises(ValueError, match="Unsupported block identifier") as offline_refusal:
            offline.get_block(identifier)

        assert str(sync_refusal.value) == str(async_refusal.value) == str(offline_refusal.value)

    async def test_wrong_type_identifier_refuses_identically(
        self,
        adapters: tuple[AlloyProvider, AsyncAlloyProvider, OfflineProvider],
    ) -> None:
        sync, async_provider, offline = adapters

        with pytest.raises(TypeError) as sync_refusal:
            sync.get_block(None)
        with pytest.raises(TypeError) as async_refusal:
            await async_provider.get_block(None)
        with pytest.raises(TypeError) as offline_refusal:
            offline.get_block(None)

        assert str(sync_refusal.value) == str(async_refusal.value) == str(offline_refusal.value)

    async def test_absent_numeric_block_is_none_on_every_adapter(
        self,
        adapters: tuple[AlloyProvider, AsyncAlloyProvider, OfflineProvider],
    ) -> None:
        sync, async_provider, offline = adapters

        assert sync.get_block(99) is None
        assert await async_provider.get_block(99) is None
        assert offline.get_block(99) is None
