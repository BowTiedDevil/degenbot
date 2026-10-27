"""The FFI checked-word invariant — known words in, checked-empty words out.

Registration (``Bot.register_v3_pool`` / ``register_v4_pool``) must record the
checked bitmap words the caller passes in ``known_bitmap_words`` (Sparse pools
only — a Tracked pool's bitmap is complete, so nothing is recorded) and the
``tick_bitmap`` getter must surface a known-but-empty word as a known WORD
(Sparse only). That is the contract that retires the companion's
``_bitmap_override`` shadow: a caller-checked word is never re-fetched.

Offline (no anvil, no live RPC): pools register directly in an in-memory
``Bot`` — the same seam as ``test_v3_sparse_fetch_seam.py``.
"""

from __future__ import annotations

from tests.helpers.seam import Bot

_ZERO_ADDRESS = "0x" + "00" * 20
_TOKEN1_ADDRESS = "0x" + "11" * 20


def _register_sparse_v3(bot: Bot, *, tick_data_fetcher=None, known_bitmap_words=None) -> int:
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
        known_bitmap_words=known_bitmap_words,
    )


def _snapshot(pool: object) -> set[int]:
    """The `tick_bitmap` getter's known-WORD set, normalized to plain ints."""
    return {int(word) for word in pool.concentrated_liquidity().tick_bitmap}


def test_sparse_registration_known_bitmap_words_survive_in_snapshot() -> None:
    bot = Bot(chain_id=1)
    # Words 0 and 1 were each checked on-chain at construction and came back
    # empty.
    pool = bot.get_pool(_register_sparse_v3(bot, known_bitmap_words=[1, 0]))
    snap = _snapshot(pool)
    assert 1 in snap, f"registration-seeded checked word 1 must survive: {snap}"
    assert 0 in snap, f"registration-seeded checked word 0 must survive: {snap}"


def test_sparse_registration_known_bitmap_words_prevent_refetch() -> None:
    bot = Bot(chain_id=1)
    calls: list[tuple[int, int]] = []

    def fake_fetcher(word: int, block: int) -> dict[int, tuple[int, int, int]]:
        calls.append((word, block))
        return {}

    pool = bot.get_pool(
        _register_sparse_v3(
            bot,
            tick_data_fetcher=fake_fetcher,
            known_bitmap_words=[0],
        ),
    )
    snap = _snapshot(pool)
    assert 0 in snap, f"registration-seeded word must appear present: {snap}"
    amount = int(
        pool.calculate_tokens_out_with_fetch(
            zero_for_one=True,
            amount_in=1000,
            block=100,
        ),
    )
    assert amount > 0, "a registration-checked (empty) word must compute, not miss"
    # The swap also crosses the adjacent (genuinely unknown) word -1 — that
    # fetch is correct. The contract under test: the word checked at
    # registration must never be a fetch target.
    assert all(w != 0 for (w, _b) in calls), (
        f"the registration-checked word 0 must not be fetched: {calls}",
    )


def test_tracked_registration_known_bitmap_words_ignored() -> None:
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
        known_bitmap_words=[3],
    )
    words = _snapshot(bot.get_pool(pool_id))
    assert words == {0}, f"Tracked pools ignore checked words: {words}"


def _v4_pool_id_hex(currency0: str, currency1: str, fee: int, tick_spacing: int) -> str:
    from degenbot.abi import encode
    from degenbot.crypto import keccak256

    return (
        "0x"
        + keccak256(
            encode(
                types=["address", "address", "uint24", "int24", "address"],
                args=[currency0, currency1, fee, tick_spacing, _ZERO_ADDRESS],
            )
        ).hex()
    )


def test_v4_sparse_registration_known_bitmap_words_survive_in_snapshot() -> None:
    bot = Bot(chain_id=1)
    currency0, currency1 = _TOKEN1_ADDRESS, "0x" + "22" * 20
    pool = bot.get_pool(
        bot.register_v4_pool(
            "0x" + "33" * 20,
            _v4_pool_id_hex(currency0, currency1, 3000, 60),
            currency0,
            currency1,
            3000,
            60,
            None,
            1 << 96,
            1_000_010_000_000,
            0,
            0,
            known_bitmap_words=[1],
        ),
    )
    snap = _snapshot(pool)
    assert 1 in snap, f"V4 registration-seeded checked word must survive: {snap}"


# ── T3 — Tracked intake gate (chain truth) ────────────────────────────
# The snapshot-internal bit<->row tautology is RETIRED as an intake gate
# (schema/decode validation owns it now): a Tracked snapshot's rows are now
# reconciled against the word bitmaps READ FROM CHAIN at the snapshot block.
# With no chain transport wired the gate refuses with a typed RuntimeError —
# never a silent pass, and never a ValueError claiming a snapshot conflict.


def test_assemble_v3_tracked_snapshot_without_chain_transport_refused(tmp_path):
    import sqlite3

    import pytest

    from tests.helpers.seam import Bot, db_upgrade_database

    pool_addr = "0x2222222222222222222222222222222222222222"
    db_path = str(tmp_path / "inconsistent.sqlite")
    db_upgrade_database(db_path)
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address, name, symbol, decimals) "
            "VALUES (1, 1, ?, NULL, NULL, NULL), (2, 1, ?, NULL, NULL, NULL)",
            [
                "0xA0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48",
                "0xC02aaA39b223FE8D0A0e5C4f27eAD9083C756Cc2",
            ],
        )
        conn.execute(
            "INSERT INTO exchanges (id, chain_id, name, active, last_update_block, factory, deployer) "
            "VALUES (1, 1, 'uniswap_v3', 1, NULL, ?, NULL)",
            ["0x1F98431c8aD98523631AE4a59f267346ea31F984"],
        )
        conn.execute(
            "INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) "
            "VALUES (1, ?, 1, 'uniswap_v3', 1, 2, 1)",
            [pool_addr],
        )
        conn.execute(
            "INSERT INTO uniswap_v3_pools "
            "(pool_id, tick_spacing, liquidity_update_block, liquidity_update_log_index, fee_token0, fee_token1, fee_denominator) "
            "VALUES (1, 10, NULL, NULL, 0, 0, 1)",
        )
        # A live row at tick 20 (compressed position 2 at spacing 10). The
        # retired `initialization_maps` word-VALUE rows are gone with the
        # tables — only the tick rows remain.
        conn.execute(
            "INSERT INTO liquidity_positions "
            "(id, pool_id, tick, liquidity_net, liquidity_gross) "
            "VALUES (1, 1, 20, '100', '100')",
        )
        conn.commit()
    finally:
        conn.close()

    bot = Bot(1)
    bot.load_snapshot_from_db(db_path, 1)

    # The snapshot-internal bit<->row tautology is retired as an intake gate
    # (schema/decode validation owns it now), and the word-VALUE tables are
    # dropped. What refuses is the MISSING CHAIN TRANSPORT: a typed
    # RuntimeError, never a silent pass and never a ValueError naming a
    # snapshot conflict.
    with pytest.raises(RuntimeError, match=r"chain transport"):
        bot.assemble_v3_tick_map(pool_addr, tick_spacing=10)


if __name__ == "__main__":
    import pytest

    pytest.main([__file__, "-vv"])
