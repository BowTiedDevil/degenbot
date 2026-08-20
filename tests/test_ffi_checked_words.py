"""T1 (3WTDFK): the FFI checked-word invariant — known words in, checked-empty words out.

``LiquidityPool.update_tick_data`` (the FFI boundary) must record the checked
bitmap words the caller passes into Rust ``known_bitmap_words`` (Sparse pools
only — a Tracked pool's bitmap is complete, so nothing is recorded) and
tick_bitmap_snapshot() must surface a known-but-empty word as a ``(0, block)``
entry (Sparse only). That is the contract that retires the companion's
``_bitmap_override`` shadow: a caller-checked word is never re-fetched.

Offline (no anvil, no live RPC): pools register directly in an in-memory
``Bot`` — the same seam as ``test_v3_sparse_fetch_seam.py``.
"""

from __future__ import annotations

from degenbot._ffi import Bot

_ZERO_ADDRESS = "0x" + "00" * 20
_TOKEN1_ADDRESS = "0x" + "11" * 20


def _register_sparse_v3(bot: Bot, *, tick_data_fetcher=None) -> int:
    """Sparse V3 pool at tick 0, ratio 1:1, 0.3% fee, empty `tick_data`."""
    return bot.register_v3_pool(
        _ZERO_ADDRESS,
        _ZERO_ADDRESS,
        _TOKEN1_ADDRESS,
        3000,
        60,
        _ZERO_ADDRESS,
        1 << 96,
        1_000_010_000_000,
        0,
        tick_data_fetcher=tick_data_fetcher,
    )


def _snapshot(pool: object) -> dict[int, tuple[int, int]]:
    """Normalize a `tick_bitmap_snapshot()` dict to plain python ints."""
    return {
        int(word): (int(row[0]), int(row[1]))
        for word, row in pool.tick_bitmap_snapshot().items()
    }


def test_sparse_checked_zero_word_survives_in_snapshot() -> None:
    bot = Bot(chain_id=1)
    pool = bot.get_pool(_register_sparse_v3(bot))
    # Words 0 and 1 were each checked on-chain and came back empty.
    pool.update_tick_data({1: (0, 100), 0: (0, 100)}, {}, 100)
    snap = _snapshot(pool)
    assert snap.get(1) == (0, 100), f"checked-empty word 1 must survive: {snap}"
    assert snap.get(0) == (0, 100), f"checked-empty word 0 must survive: {snap}"


def test_sparse_checked_zero_word_prevents_refetch() -> None:
    bot = Bot(chain_id=1)
    calls: list[tuple[int, int]] = []

    def fake_fetcher(word: int, block: int) -> dict[int, tuple[int, int, int]]:
        calls.append((word, block))
        return {}

    pool = bot.get_pool(_register_sparse_v3(bot, tick_data_fetcher=fake_fetcher))
    pool.update_tick_data({0: (0, 100)}, {}, 100)
    snap = _snapshot(pool)
    assert snap.get(0) == (0, 100), f"checked word must appear present-but-zero: {snap}"
    # The swap starts in word 0: without the check it would fetch once
    # (baseline in test_v3_sparse_fetch_seam); with it, never.
    amount = int(
        pool.calculate_tokens_out_with_fetch(
            zero_for_one=True,
            amount_in=1000,
            block=100,
        ),
    )
    assert amount > 0, "a checked (empty) word must compute, not miss"
    # The swap also crosses the adjacent (genuinely unknown) word -1 — that
    # fetch is correct. The contract under test: the caller-checked word 0
    # must never be a fetch target.
    assert all(w != 0 for (w, _b) in calls), (
        f"the caller-checked word 0 must not be fetched: {calls}",
    )


def test_tracked_update_tick_data_adds_no_bitmap_entries() -> None:
    bot = Bot(chain_id=1)
    rows = {0: (10_000_000_000_000, 5_000_000_000_000, 0)}
    pool_id = bot.register_v3_pool(
        _ZERO_ADDRESS,
        _ZERO_ADDRESS,
        _TOKEN1_ADDRESS,
        3000,
        60,
        _ZERO_ADDRESS,
        1 << 96,
        1_000_010_000_000,
        0,
        tick_data=rows,
        update_block=0,
        coverage="tracked",
    )
    pool = bot.get_pool(pool_id)
    pool.update_tick_data({3: (0, 50)}, dict(rows), 50)
    words = set(_snapshot(pool))
    assert words == {0}, f"Tracked bitmap is derived from tick rows only: {words}"


def test_v4_sparse_checked_zero_word_survives_in_snapshot() -> None:
    from degenbot.abi import encode
    from degenbot.crypto import keccak256
    from tests.helpers.erc20_factory import make_erc20
    from tests.helpers.v4_pool_factory import make_v4_pool

    c0, c1 = _TOKEN1_ADDRESS, "0x" + "22" * 20
    pool_manager = "0x" + "33" * 20
    pool_id_hex = "0x" + keccak256(
        encode(
            types=["address", "address", "uint24", "int24", "address"],
            args=[c0, c1, 3000, 60, _ZERO_ADDRESS],
        )
    ).hex()
    token0 = make_erc20(Bot(), c0, name="a", symbol="A", decimals=18, chain_id=1)
    token1 = make_erc20(Bot(), c1, name="b", symbol="B", decimals=18, chain_id=1)
    pool = make_v4_pool(
        pool_id=pool_id_hex,
        pool_manager_address=pool_manager,
        token0=token0,
        token1=token1,
        fee=3000,
        tick_spacing=60,
        sqrt_price_x96=1 << 96,
        tick=0,
        liquidity=1_000_010_000_000,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=0,
        state_block=0,
        coverage="sparse",
    )
    handle = pool._py_pool  # ruff:ignore[private-member-access] — factory seams the same way
    handle.update_tick_data({1: (0, 100)}, {}, 100)
    snap = _snapshot(handle)
    assert snap.get(1) == (0, 100), f"V4 checked-empty word must survive: {snap}"


if __name__ == "__main__":
    import pytest

    pytest.main([__file__, "-vv"])
