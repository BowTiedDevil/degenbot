"""ADR-005 sparse-map feature parity — slice 3a PyO3 fetch-callback seam.

End-to-end gate for the Rust→PyO3→Python fetcher bridge:

* registering a V3 pool via ``RustBot`` always creates a *sparse* pool (empty
  ``tick_data`` → no tick-bitmap words known), so the very first
  ``calculate_tokens_out`` must miss on the starting word and return ``0``;
* ``LiquidityPool.calculate_tokens_out_with_fetch`` drives the Rust
  fetch+retry loop: on the miss it calls the injected Python fetcher
  ``fetcher(word, block) -> dict | None`` for the missing word, merges the
  returned tick data into ``BotState``, and retries;
* after the word is filled, the no-fetch direct path must no longer miss and
  must return the same amount as the fetch+retry path (the merge persisted the
  word as known);
* a fetcher that cannot satisfy the miss (returns an error / missing data) must
  give up with ``0`` rather than panic or spin.

This proves the FFI seam works (GIL + Python dict → ``FetchedTickWord`` → Rust
loop). Slice 3b routes the V3/V4 companions' non-override calc through this seam;
slice 4 retires the Python simulators' swap-loop behind a dense-map parity
corpus.
"""

from __future__ import annotations

import pytest

from degenbot.bot import RustBot

_ZERO_ADDRESS = "0x" + "00" * 20
_TOKEN1_ADDRESS = "0x" + "11" * 20


def _register_sparse_v3_pool(bot: RustBot, *, tick_data_fetcher=None) -> int:
    """Register a sparse V3 pool at tick 0, ratio 1:1, 0.3% fee, with liquidity.

    ``RustBot.register_v3_pool`` always registers sparse with empty ``tick_data``,
    so the starting tick-bitmap word (0) is unknown → the first calc misses.
    The optional ``tick_data_fetcher`` is stored Rust-side on ``V3PoolState``
    (ADR-006 I/O trait object) so the fetch+retry loop in
    ``calculate_tokens_out_with_fetch`` uses it.
    """
    return bot.register_v3_pool(
        _ZERO_ADDRESS,  # address
        _ZERO_ADDRESS,  # token0
        _TOKEN1_ADDRESS,  # token1
        3000,  # fee (0.3%)
        60,  # tick_spacing
        _ZERO_ADDRESS,  # factory
        1 << 96,  # sqrt_price_x96 == 1.0 price ratio
        1_000_010_000_000,  # liquidity (~1e13)
        0,  # tick
        tick_data_fetcher=tick_data_fetcher,
    )


def test_sparse_pool_misses_without_fetcher() -> None:
    bot = RustBot(chain_id=1)
    pool_id = _register_sparse_v3_pool(bot)
    pool = bot.get_pool(pool_id)

    # No fetcher: the starting word is unknown → miss → 0 (the no-fetch
    # sentinel). This is the precondition the fetch seam exists to recover.
    assert int(pool.calculate_tokens_out(zero_for_one=True, amount_in=1000)) == 0


def test_calculate_tokens_out_with_fetch_fills_word_and_retries() -> None:
    bot = RustBot(chain_id=1)
    calls: list[tuple[int, int]] = []

    def fake_fetcher(word: int, block: int) -> dict[int, tuple[int, int, int]]:
        # Marks the word known with no initialized ticks (an empty bitmap
        # word). Returns the (gross, net, block) tuple shape the adapter
        # extracts; an empty dict still marks the word known.
        calls.append((word, block))
        return {}

    pool_id = _register_sparse_v3_pool(bot, tick_data_fetcher=fake_fetcher)
    pool = bot.get_pool(pool_id)

    amount = int(
        pool.calculate_tokens_out_with_fetch(
            zero_for_one=True,
            amount_in=1000,
            block=0,
        ),
    )

    # The fetcher was invoked for the missed starting word (0).
    assert (0, 0) in calls
    # The fetch+retry result is non-zero (not the miss sentinel).
    assert amount > 0
    # After the fetch, the direct no-fetch path must no longer miss and must
    # return the same amount (the merge persisted word 0 as known).
    assert int(pool.calculate_tokens_out(zero_for_one=True, amount_in=1000)) == amount


def test_calculate_tokens_out_with_fetch_failing_fetcher_returns_zero() -> None:
    bot = RustBot(chain_id=1)

    def failing_fetcher(word: int, block: int) -> dict[int, tuple[int, int, int]]:
        raise RuntimeError

    pool_id = _register_sparse_v3_pool(bot, tick_data_fetcher=failing_fetcher)
    pool = bot.get_pool(pool_id)

    # A failing fetcher must give up with 0 rather than panic or propagate.
    assert (
        int(
            pool.calculate_tokens_out_with_fetch(
                zero_for_one=True,
                amount_in=1000,
                block=0,
            ),
        )
        == 0
    )


if __name__ == "__main__":
    pytest.main([__file__, "-vv"])
