"""Stub for the dynamically-created ``degenbot._ffi.aave`` submodule.

Created at runtime by ``add_aave_updater_module`` in the PyO3 wrapper crate
(``degenbot-python/src/aave_updater.rs``). Holds the Aave V3 updater
loop + on-chain verification functions over the ``degenbot-aave-updater``
core crate.
"""

from collections.abc import Callable
from typing import Any

from degenbot._ffi.cancel import CancelHandle

def run_aave_update(
    database_path: str,
    chain_id: int,
    market_id: int,
    to_block: int | None,
    chunk_size: int,
    rpc_url: str,
    progress_callback: Callable[..., None],
    cancel_handle: CancelHandle,
    verify_chunk: bool = False,
    max_chunks: int | None = None,
    *,
    verify_all_interval: int | None = None,
    verify_all_at_completion: bool = False,
) -> dict[str, Any]:
    """Drive the Rust-owned Aave V3 updater chunk loop (epic AZGJUN, 5XNTC5).

    Advance ``aave_v3_markets.last_update_block`` for ``market_id`` to
    ``to_block`` (or the chain tip if ``None``). Each chunk: RPC-fetch the 7
    Aave event passes, group by transaction, run the per-tx discount
    pre-pass + config dispatch + operations parser, write under ONE
    ``Transaction`` (§3.4 atomicity), stamp ``last_update_block`` LAST
    (restart-invariance). The GIL is released across the whole run; only
    ``progress_callback`` re-acquires it briefly, once per chunk.

    Args:
        database_path: The writeable ``DegenbotDb`` path.
        chain_id: The chain.
        market_id: The ``aave_v3_markets.id`` to advance.
        to_block: ``int`` to advance to a specific block; ``None`` for the
            chain tip (``eth_blockNumber``).
        chunk_size: Blocks per chunk.
        rpc_url: The HTTP RPC endpoint.
        progress_callback: A callable invoked with a per-chunk ``dict``
            ``{chain_id, market_id, chunk_start, chunk_end, events_applied,
            committed, is_final}`` once per chunk boundary.
        cancel_handle: A ``CancelHandle`` (shared with ``run_pool_update``);
            a SIGINT handler calls ``cancel_handle.cancel()``.
        verify_chunk: If ``True``, run pre-commit verification on each chunk
            ("scaled-token balance + last_index" against the on-chain
            truth at ``chunk_end``). A divergence drops the transaction
            (rollback) so ``last_update_block`` does NOT advance + the next
            run re-processes the same chunk. If ``False``, verification is
            skipped.
        max_chunks: ``None`` to advance to ``to_block``/tip; ``int`` to stop
            after committing that many chunks (one-chunk mode).
            ``last_update_block`` is advanced to the last committed chunk's
            end, so the next run resumes from there.
        verify_all_interval: When set, run a pre-commit FULL (market-wide,
            all 4-check) verification when a chunk crosses/lands-on a multiple
            of this block interval. A divergence drops the transaction
            (rollback) so ``last_update_block`` does NOT advance.
        verify_all_at_completion: When ``True``, run a pre-commit FULL
            verification on the run's final chunk. A divergence rolls back the
            chunk (``last_update_block`` does NOT advance).

    Returns:
        ``dict {chain_id, market_id, from_block, to_block,
        chunks_committed, total_events_applied}``.

    Raises:
        ValueError: For a DB / RPC / config-dispatch / parse failure
            (in-flight chunk rolled back; committed chunks stay durable).
        RuntimeError: If cancelled (committed chunks stay durable).
        AssertionError: If ``verify_chunk=True`` and pre-commit verification
            found divergences (in-flight chunk rolled back;
            ``last_update_block`` did NOT advance).
        ValueError: If the market has no ``last_update_block`` (NotBootstrapped
            — bootstrap the stamp first).

    Note:
        Must NOT be called from within an existing tokio runtime. The CLI
        runs this from a worker thread with no ambient runtime.

    """

def verify_all_positions_on_chain(
    database_path: str,
    rpc_url: str,
    market_id: int,
    chain_id: int,
    block_number: int,
    touched_users: list[str] | None = None,
) -> list[dict[str, Any]]:
    """Full on-chain-truth verification (Rust port of Python ``verify_all_positions``).

    Runs all 4 checks:
    1. Collateral scaled-token balance + last_index (``scaledBalanceOf`` +
       ``getPreviousIndex`` on each aToken).
    2. Debt scaled-token balance + last_index (same calls on each vToken).
    3. stkAAVE balance (``balanceOf`` on the discount token).
    4. GHO discount percent (``getDiscountPercent`` on the GHO vToken,
       with a revision-based skip guard at revision >= 4).

    Args:
        database_path: The ``DegenbotDb`` path (opened read-only).
        rpc_url: The HTTP RPC endpoint.
        market_id: The ``aave_v3_markets.id`` to verify.
        chain_id: The chain ID (needed to resolve the GHO asset row).
        block_number: The block to verify against.
        touched_users: ``None`` (default) verifies ALL positions/users;
            a list of address strings verifies only those users.

    Returns:
        A list of divergence dicts (empty = GREEN). Each dict has a ``check``
        field (``"scaled_token"``, ``"stk_aave_balance"``, or
        ``"gho_discount"``) plus the relevant fields for that check type.

    Raises:
        ValueError: For a DB/RPC failure that prevents verification.

    """

def verify_touched_positions_on_chain(
    database_path: str,
    rpc_url: str,
    market_id: int,
    chain_id: int,
    block_number: int,
    touched_users: list[str] | None = None,
) -> list[dict[str, Any]]:
    """On-chain-truth verification scoped to a set of touched users.

    Like ``verify_all_positions_on_chain`` but limited to the addresses in
    ``touched_users`` (collateral + debt checks only for those users).

    Args:
        database_path: The ``DegenbotDb`` path (opened read-only).
        rpc_url: The HTTP RPC endpoint.
        market_id: The ``aave_v3_markets.id`` to verify.
        chain_id: The chain ID (needed to resolve the GHO asset row).
        block_number: The block to verify against.
        touched_users: A list of address strings to verify; ``None`` verifies
            all (equivalent to ``verify_all_positions_on_chain``).

    Returns:
        A list of divergence dicts (empty = GREEN).

    Raises:
        ValueError: For a DB/RPC failure that prevents verification.

    """

def cleanup_zero_balance_positions(
    database_path: str,
    market_id: int,
) -> None:
    """Delete all zero-balance collateral + debt positions for ``market_id``.

    Mirrors the Python ``cleanup_zero_balance_positions``. Opens the DB for
    writes, deletes zero-balance rows, + commits. The GIL is released across
    the call.

    Args:
        database_path: The writeable ``DegenbotDb`` path.
        market_id: The ``aave_v3_markets.id`` to clean up.

    Raises:
        ValueError: For a DB failure.

    """

def activate_aave_market(
    database_path: str,
    chain_id: int,
    pool_address_provider: str,
    gho_token_address: str,
    rpc_url: str,
) -> dict[str, Any]:
    """Seed (or re-activate) an Aave V3 market (MPI6Q3).

    The ONE-TIME setup the chunk loop's ``run_aave_update`` bootstraps from.
    Rust-owned replacement for the Python ``activate_ethereum_aave_v3``
    (commands.py) — the last ORM writer on the Aave path after the §4.2
    retirement (CZM7TI). RPC-fetches ``getMarketId()`` on the pool address
    provider + the GHO token's ``name()``/``symbol()``/``decimals()``, then
    seeds — in ONE transaction — the ``aave_v3_markets`` row, the
    ``POOL_ADDRESS_PROVIDER`` contract row, + the GHO ``erc20_tokens`` +
    ``aave_gho_tokens`` rows. Idempotent: re-activating an existing market
    sets ``active = True`` + inserts no duplicate rows.

    The GIL is released across the whole call (the core owns its tokio
    runtime + does the RPC fetches + DB writes internally).

    Args:
        database_path: The writeable ``DegenbotDb`` path.
        chain_id: The chain.
        pool_address_provider: The ``PoolAddressProvider`` contract address
            (checksummed).
        gho_token_address: The chain's GHO token address (checksummed).
        rpc_url: The HTTP RPC endpoint.

    Returns:
        A ``dict`` ``{market_id, market_name, created}``. ``market_id`` is
        the ``aave_v3_markets.id`` to pass to ``run_aave_update``. ``created``
        is ``True`` if the market was newly created, ``False`` if it
        pre-existed (re-activation).

    Raises:
        ValueError: For a DB / RPC / address-parse failure.

    """

def deactivate_aave_market(
    database_path: str,
    market_id: int,
) -> None:
    """Set ``active = False`` for ``market_id`` (MPI6Q3).

    Rust-owned replacement for the Python ``deactivate_mainnet_aave_v3``
    (commands.py). The GIL is released across the call.

    Args:
        database_path: The writeable ``DegenbotDb`` path.
        market_id: The ``aave_v3_markets.id`` to deactivate.

    Raises:
        ValueError: If ``market_id`` doesn't exist or on a DB failure.

    """

__all__ = [
    "activate_aave_market",
    "cleanup_zero_balance_positions",
    "deactivate_aave_market",
    "run_aave_update",
    "verify_all_positions_on_chain",
    "verify_touched_positions_on_chain",
]
