from collections.abc import Callable
from typing import Any

from degenbot._ffi.cancel import CancelHandle

def run_pool_update(
    database_path: str,
    chain_id: int,
    to_block: int | None,
    chunk_size: int,
    rpc_url: str,
    progress_callback: Callable[..., None],
    cancel_handle: CancelHandle,
    verify_chunk: bool = False,
    *,
    verify_all_interval: int | None = None,
    verify_all_at_completion: bool = False,
) -> dict[str, Any]:
    """Drive the Rust-owned pool-updater chunk loop (epic 2SFL6I).

    Advance every active exchange's ``last_update_block`` to ``to_block`` (or
    the chain tip if ``None``). Each chunk: RPC-fetch pool creations +
    V3/V4 liquidity, decode, write under ONE ``Transaction`` (atomicity),
    stamp ``last_update_block`` LAST (restart-invariance). The GIL is released
    across the whole run; only ``progress_callback`` re-acquires it briefly,
    once per chunk.

    Args:
        database_path: The writeable ``DegenbotDb`` path (already migrated
            to the Rust-owned schema).
        chain_id: The chain to advance.
        to_block: ``int`` to advance to a specific block; ``None`` to
            advance to the chain tip (``eth_blockNumber``).
        chunk_size: Blocks per chunk.
        rpc_url: The HTTP RPC endpoint.
        progress_callback: A callable invoked with a per-chunk ``dict``
            ``{chain_id, chunk_start, chunk_end, pools_written,
            liquidity_apply_count, committed, is_final}`` once per chunk boundary.
        cancel_handle: A ``CancelHandle`` constructed up front; a SIGINT
            handler calls ``cancel_handle.cancel()`` to stop at the next
            chunk boundary.
        verify_chunk: When ``True``, run the pre-commit per-chunk on-chain-truth gate
            (Full per-pool touched-pool verification) before each chunk's persist
            commits; a divergence rolls back the chunk + does NOT advance
            ``last_update_block``. ``False`` (default) = the no-gate path.
        verify_all_interval: When set, run a pre-commit FULL (market-wide, all
            in-scope pools) verification when a chunk crosses/lands-on a multiple
            of this block interval. A divergence rolls back the chunk.
        verify_all_at_completion: When ``True``, run a pre-commit FULL
            verification on the run's final chunk. A divergence rolls back the
            chunk.

    Returns:
        ``dict {chain_id, from_block, to_block, chunks_committed,
        total_pools_written, total_liquidity_applies}``.

    Raises:
        ValueError: For a DB or RPC failure (in-flight chunk rolled back).
        RuntimeError: If cancelled (committed chunks stay durable).

    Note:
        Must NOT be called from an existing tokio runtime; the CLI runs this
        from a worker thread with no ambient runtime.

    """

def verify_v3_liquidity_map(
    database_path: str,
    rpc_url: str,
    chain_id: int,
    pool_address: str,
    block_number: int,
) -> list[dict[str, Any]]:
    """Stand-alone on-chain-truth verification of a V3 pool's COMMITTED map.

    Opens the DB read-only, fetches the pool's ``liquidity_positions`` +
    ``initialization_maps`` rows, and compares every tick + bitmap word
    against on-chain ``ticks(int24)`` / ``tickBitmap(int16)`` at
    ``block_number`` (batched via Multicall3). Returns the divergence list
    (empty = GREEN — the DB matches the chain).

    This is the ad-hoc / spot-check sibling of the pre-commit gate
    (``run_pool_update(verify=True)``); the gate runs the SAME compare BEFORE
    the write commits, while this reads the already-committed state.

    Args:
        database_path: The ``DegenbotDb`` path (opened read-only).
        rpc_url: The HTTP RPC endpoint.
        chain_id: The chain the pool lives on.
        pool_address: The V3 pool contract address (checksummed or not).
        block_number: The block to verify against.

    Returns:
        A list of divergence dicts (empty = GREEN). Each dict carries
        ``variant`` (``TickGross`` / ``TickNet`` / ``BitmapWord`` /
        ``TickCallReverted`` / ``BitmapCallReverted``) + the named fields
        (``tick`` / ``word``, ``expected``, ``actual``) for bisect-able triage.

    Raises:
        ValueError: For a DB or RPC failure.
        KeyError: If the pool is not found on the chain.

    """

def verify_v4_liquidity_map(
    database_path: str,
    rpc_url: str,
    chain_id: int,
    pool_hash: str,
    pool_manager_address: str,
    block_number: int,
) -> list[dict[str, Any]]:
    """Stand-alone on-chain-truth verification of a V4 pool's COMMITTED map.

    Mirrors :func:`verify_v3_liquidity_map` for V4: reads the ``managed_pool``
    liquidity rows + compares every tick slot + bitmap-word slot against the
    singleton ``PoolManager`` storage via ``extsload(bytes32[])`` at
    ``block_number``. Returns the divergence list (empty = GREEN).

    Args:
        database_path: The ``DegenbotDb`` path (opened read-only).
        rpc_url: The HTTP RPC endpoint.
        chain_id: The chain the pool lives on.
        pool_hash: The V4 ``PoolId`` (bytes32 hex, ``0x…``).
        pool_manager_address: The deployed V4 ``PoolManager`` singleton
            (the V4 exchange's ``factory``).
        block_number: The block to verify against.

    Returns:
        A list of divergence dicts (same shape as :func:`verify_v3_liquidity_map`).

    Raises:
        ValueError: For a DB or RPC failure.
        KeyError: If the pool is not found on the chain.

    """

__all__ = [
    "run_pool_update",
    "verify_v3_liquidity_map",
    "verify_v4_liquidity_map",
]
