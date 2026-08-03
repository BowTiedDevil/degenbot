"""Regression: V3 seed-block anchoring (H1 — post-drain verify false positive).

`V3PoolBuilder.resolve_seed_block` picks the block at which the seed snapshot
(update_block + scalars + assembled tick map + post-registration apply_swap)
is anchored. For a DB-seeded pool the seed must anchor at the DB liquidity
snapshot's `liquidity_update_block`, NOT the live WS head — otherwise the
post-drain `verify_v3_post_drain_snapshot` reads on-chain at head against
stale tick data and false-positives on every tick that moved in
`(liquidity_update_block, head]`.

Live repro (2026-08-03, run captured in logs/bot_run.log): V3 pool
0x56534741cd8b152df6d48adf7ac51f75169a83b2 (WBTC/USDT, DB pool 601533,
`liquidity_update_block=25675348`) had a tracked-tick (63970) Mint at block
25676145. Engine seeded `update_block=25676145` (head) over DB tick data from
25675348, so step-2 verify compared tick-data@75348 against on-chain@76145 and
failed on exactly that Mint (lg 222112996911 -> 222131654925).
"""

from __future__ import annotations

from degenbot.builders.seed_block_resolver import resolve_seed_block


def test_resolve_seed_block_anchors_at_db_liquidity_update_block_by_default() -> None:
    """A DB-seeded pool must anchor at its liquidity snapshot block, not head.

    `db_liquidity_update_block=25_675_348` (DB pool 601533), head 25_676_145;
    a tracked-tick Mint at 25_676_145 is the exact boundary event that the
    pre-fix seed (anchored at head) failed on.
    """
    assert resolve_seed_block(None, 25_675_348, 25_676_145) == 25_675_348


def test_resolve_seed_block_honors_explicit_request_state_block() -> None:
    """An explicit caller-pinned `request.state_block` wins over the DB anchor."""
    assert resolve_seed_block(123, 25_675_348, 25_676_145) == 123
    assert resolve_seed_block(25_676_000, 25_675_348, 25_676_145) == 25_676_000


def test_resolve_seed_block_falls_back_to_head_without_db_block() -> None:
    """No DB liquidity block (chain-fetched sparse pool) keeps head as the anchor."""
    assert resolve_seed_block(None, None, 25_676_145) == 25_676_145
    assert resolve_seed_block(None, 0, 25_676_145) == 25_676_145


def test_v3_and_v4_builders_share_resolve_seed_block() -> None:
    """Both pool builders must consume the same single anchor definition.

    A V4 pool routed through the DB path gets the same treatment as V3.
    """
    from degenbot.builders.v3_pool_builder import (
        resolve_seed_block as v3_resolve_seed_block,
    )
    from degenbot.builders.v4_pool_builder import (
        resolve_seed_block as v4_resolve_seed_block,
    )

    assert v3_resolve_seed_block is resolve_seed_block
    assert v4_resolve_seed_block is resolve_seed_block
