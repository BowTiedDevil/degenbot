"""CDJEPJ-2: Erc20Builder.build_many batches per-pool token metadata reads.

`_build_v4_managed` used to call `_erc20_builder.build(currency0)` then
`_erc20_builder.build(currency1)` — two SERIAL network `fetch_erc20_metadata`
round-trips (each 3 eth_calls on a metadata miss). `build_many` collapses the
network-missing set into ONE `io.fetch_erc20_metadata_batch([a, b])` Multicall3
`aggregate3` eth_call.

RED: before CDJEPJ-2 there was no `build_many` (and two separate
`fetch_erc20_metadata` reads); GREEN: one `fetch_erc20_metadata_batch` call.
"""

from __future__ import annotations

from degenbot.bot import PyBot
from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.registry import TokenRegistry

TOKEN_A = "0x00000000000000000000000000000000000000A1"
TOKEN_B = "0x00000000000000000000000000000000000000B1"


class _RecFakeIo:
    """Minimal io double recording which network reads `build_many` issues.

    Returns `None` for the DB lookup (so both tokens need a network fetch) and
    canned metadata for the batch call.
    """

    def __init__(self) -> None:
        self.metadata_batch_calls: list[list[str]] = []
        self.single_metadata_calls = 0

    def fetch_erc20_token(self, chain_id: int, address: str) -> None:
        return None

    def update_erc20_token_metadata(  # noqa: D102 - delegated seam, unused here
        self, *args: object, **kwargs: object
    ) -> None:
        return None

    def fetch_erc20_metadata_batch(
        self, addresses: list[str]
    ) -> list[tuple[str, str, int] | None]:
        self.metadata_batch_calls.append(list(addresses))
        return [("Alpha", "ALPHA", 6), ("Beta", "BETA", 18)]

    def get_code(self, address: str) -> bytes:
        return b"0x0"

    def fetch_erc20_metadata(self, address: str) -> None:
        self.single_metadata_calls += 1
        return None

    def fetch_erc20_string_field(self, address: str, prototype: str) -> str:
        raise ValueError("revert")

    def fetch_erc20_uint_field(self, address: str, prototype: str) -> int:
        raise ValueError("revert")


def test_build_many_issues_single_batched_fetch() -> None:
    """Two DB/registry-missing tokens resolve via ONE batched metadata fetch."""
    py_bot = PyBot(chain_id=1)
    fake_db = object.__new__(DatabaseSessionManager)
    tokens = TokenRegistry()
    io = _RecFakeIo()
    erc20 = Erc20Builder(default_chain_id=1, db=fake_db, tokens=tokens, py_bot=py_bot)

    t_a, t_b = erc20.build_many([TOKEN_A, TOKEN_B], chain_id=1, silent=True, io=io)

    # ONE batched network fetch covering both tokens — no per-token fetches.
    assert io.metadata_batch_calls == [[TOKEN_A, TOKEN_B]]
    assert io.single_metadata_calls == 0
    assert t_a.symbol == "ALPHA" and t_a.decimals == 6
    assert t_b.symbol == "BETA" and t_b.decimals == 18


def test_build_many_falls_back_per_token_for_none_meta() -> None:
    """A token whose batched metadata is `None` falls back to a per-token build
    (contract-deployed check + alternate-prototype fallback preserved)."""
    py_bot = PyBot(chain_id=1)
    fake_db = object.__new__(DatabaseSessionManager)
    tokens = TokenRegistry()
    io = _RecFakeIo()
    erc20 = Erc20Builder(default_chain_id=1, db=fake_db, tokens=tokens, py_bot=py_bot)

    # Patch the batch to return None for the second token (simulating a revert
    # / decode failure in the multicall for TOKEN_B).
    io.fetch_erc20_metadata_batch = lambda addresses: [  # type: ignore[method-assign]
        ("Alpha", "ALPHA", 6),
        None,
    ]
    t_a, t_b = erc20.build_many([TOKEN_A, TOKEN_B], chain_id=1, silent=True, io=io)

    assert t_a.symbol == "ALPHA"
    # TOKEN_B fell back to the per-token path, which called fetch_erc20_metadata
    # (returns None here -> the alternate-prototype fallback -> UNKNOWN_*).
    assert io.single_metadata_calls == 1
    assert t_b.symbol == "UNKNOWN"  # UNKNOWN_SYMBOL const value
