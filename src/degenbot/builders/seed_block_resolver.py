"""Resolve the block a pooled seed snapshot is anchored at (V3 + V4).

Shared by `V3PoolBuilder` and `V4PoolBuilder` (task BS6KFF): when either
builder seeds a concentrated-liquidity pool's tick/liquidity data from the
database, the seed's `update_block` (and every consumer anchored to it —
slot0/liquidity fetch, `assemble_v3/v4_tick_map`, and the post-registration
`apply_swap`) must reflect the block that DB snapshot is exact at, i.e. the
pool row's `liquidity_update_block`. Using the live WS head instead makes the
post-drain `verify_v3/v4_post_drain_snapshot` read on-chain at head against
stale tick data and false-positive on every tick that moved in the
`(liquidity_update_block, head]` window.
"""

from __future__ import annotations


def resolve_seed_block(
    request_state_block: int | None,
    db_liquidity_update_block: int | None,
    head_block: int,
) -> int:
    """Resolve the block at which a pooled seed snapshot is anchored.

    For a DB-seeded pool that is the DB liquidity snapshot's
    `liquidity_update_block` (tick data is exact at that block); using the live
    head instead makes the post-drain verify compare stale tick data against
    on-chain at head and false-positive on every tick that moved in the gap
    (H1, run 2026-08-03, V3 pool 0x56534741cd8b152df6d48adf7ac51f75169a83b2
    tick 63970 Mint @ 25676145).

    Precedence (highest first): an explicit caller-pinned `request_state_block`
    is honored as-is; otherwise a DB `liquidity_update_block` (> 0) wins over
    the live head. Conservative: anchoring to an older snapshot defers the
    pool's paths via the AV42C7 freshness gate until the pump backfill catches
    up — it never false-positives.

    Returns:
        The effective seed-anchor block: `request_state_block` if the caller
        pinned it; else `db_liquidity_update_block` when it is a positive DB
        liquidity snapshot block; else `head_block`.

    """
    if request_state_block is not None:
        return request_state_block
    if db_liquidity_update_block is not None and db_liquidity_update_block > 0:
        return db_liquidity_update_block
    return head_block
