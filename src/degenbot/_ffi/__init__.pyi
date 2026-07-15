"""Type stubs for the degenbot Rust extension module (degenbot_rs).

This module provides high-performance implementations of common operations
used by the degenbot Python package.
"""

from collections.abc import Callable, Coroutine
from typing import Any, Literal, overload

from hexbytes import HexBytes

from degenbot.builders.pool_io import PoolIO
from degenbot.types.rpc_types import (
    BlockData,
    BlockIdentifier,
    LogData,
    TransactionData,
    TransactionReceiptData,
    TxParams,
)
from degenbot.types.rpc_types import BlockData as Web3BlockData

# ------------------------------------------------------------------
# ABI encoding / decoding
# ------------------------------------------------------------------

# ------------------------------------------------------------------
# Concentrated-liquidity math (cl_*)
# ------------------------------------------------------------------

def cl_most_significant_bit(x: int) -> int:
    """Find the index of the most significant bit set in x.

    Args:
        x: A non-negative integer

    Returns:
        The index (0-255) of the highest set bit

    Raises:
        ValueError: If x is zero

    """

def cl_least_significant_bit(x: int) -> int:
    """Find the index of the least significant bit set in x.

    Args:
        x: A non-negative integer

    Returns:
        The index (0-255) of the lowest set bit

    Raises:
        ValueError: If x is zero

    """

def cl_muldiv(a: int, b: int, denominator: int) -> int:
    """Compute floor(a * b / denominator) with full 512-bit precision.

    Args:
        a: First multiplicand
        b: Second multiplicand
        denominator: Divisor

    Returns:
        The floored result as a Python int

    Raises:
        ValueError: On division by zero or overflow

    """

def cl_muldiv_rounding_up(a: int, b: int, denominator: int) -> int:
    """Compute ceil(a * b / denominator) with full 512-bit precision.

    Args:
        a: First multiplicand
        b: Second multiplicand
        denominator: Divisor

    Returns:
        The ceiling result as a Python int

    Raises:
        ValueError: On division by zero or overflow

    """

def cl_div_rounding_up(x: int, y: int) -> int:
    """Compute ceil(x / y) without overflow checking.

    Args:
        x: Dividend
        y: Divisor

    Returns:
        The ceiling result as a Python int

    Raises:
        ValueError: If y is zero

    """

def cl_simple_mul_div(a: int, b: int, denominator: int) -> int:
    """Compute (a * b) / denominator without overflow checking.

    Args:
        a: First multiplicand
        b: Second multiplicand
        denominator: Divisor

    Returns:
        The result as a Python int

    Raises:
        ValueError: If denominator is zero

    """

def cl_add_delta(x: int, y: int) -> int:
    """Add a signed delta y to x, checking that the result fits in uint128.

    Args:
        x: Base value (must fit in uint128)
        y: Signed delta (must fit in int128)

    Returns:
        The result as a Python int

    Raises:
        ValueError: If the result overflows or inputs are out of range

    """

def cl_get_amount0_delta(
    sqrt_price_a: int,
    sqrt_price_b: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    """Get the amount0 delta between two prices for a given liquidity.

    Args:
        sqrt_price_a: First sqrt price (X96)
        sqrt_price_b: Second sqrt price (X96)
        liquidity: Liquidity value
        round_up: Whether to round up

    Returns:
        The token0 amount delta as a Python int

    Raises:
        ValueError: On invalid input (zero price, overflow, etc.)

    """

def cl_get_amount1_delta(
    sqrt_price_a: int,
    sqrt_price_b: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    """Get the amount1 delta between two prices for a given liquidity.

    Args:
        sqrt_price_a: First sqrt price (X96)
        sqrt_price_b: Second sqrt price (X96)
        liquidity: Liquidity value
        round_up: Whether to round up

    Returns:
        The token1 amount delta as a Python int

    Raises:
        ValueError: On invalid input (negative liquidity, overflow, etc.)

    """

def cl_get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    """Get the next sqrt price given a delta of token0, rounding up.

    Args:
        sqrt_price_x96: Current sqrt price (X96)
        liquidity: Liquidity value
        amount: Token0 amount
        add: Whether to add (True) or remove (False)

    Returns:
        The next sqrt price (X96) as a Python int

    Raises:
        ValueError: On overflow or insufficient liquidity

    """

def cl_get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    """Get the next sqrt price given a delta of token1, rounding down.

    Args:
        sqrt_price_x96: Current sqrt price (X96)
        liquidity: Liquidity value
        amount: Token1 amount
        add: Whether to add (True) or remove (False)

    Returns:
        The next sqrt price (X96) as a Python int

    Raises:
        ValueError: On overflow or insufficient liquidity

    """

def cl_get_next_sqrt_price_from_input(
    sqrt_price_x96: int,
    liquidity: int,
    amount_in: int,
    zero_for_one: bool,
) -> int:
    """Get the next sqrt price given an input amount.

    Args:
        sqrt_price_x96: Current sqrt price (X96)
        liquidity: Liquidity value
        amount_in: Input amount
        zero_for_one: Direction flag

    Returns:
        The next sqrt price (X96) as a Python int

    Raises:
        ValueError: On invalid price/liquidity or overflow

    """

def cl_get_next_sqrt_price_from_output(
    sqrt_price_x96: int,
    liquidity: int,
    amount_out: int,
    zero_for_one: bool,
) -> int:
    """Get the next sqrt price given an output amount.

    Args:
        sqrt_price_x96: Current sqrt price (X96)
        liquidity: Liquidity value
        amount_out: Output amount
        zero_for_one: Direction flag

    Returns:
        The next sqrt price (X96) as a Python int

    Raises:
        ValueError: On invalid price/liquidity or overflow

    """

def cl_compute_swap_step_v3(
    sqrt_price_current: int,
    sqrt_price_target: int,
    liquidity: int,
    amount_remaining: int,
    fee_pips: int,
) -> tuple[int, int, int, int]:
    """Compute a V3-style swap step.

    Args:
        sqrt_price_current: Current sqrt price (X96)
        sqrt_price_target: Target sqrt price (X96)
        liquidity: Liquidity value
        amount_remaining: Remaining amount (signed)
        fee_pips: Fee in pips

    Returns:
        Tuple of (sqrt_price_next, amount_in, amount_out, fee_amount)

    Raises:
        ValueError: On invalid input, overflow, or if liquidity exceeds int128

    """

def cl_compute_swap_step_v4(
    sqrt_price_current: int,
    sqrt_price_target: int,
    liquidity: int,
    amount_remaining: int,
    fee_pips: int,
) -> tuple[int, int, int, int]:
    """Compute a V4-style swap step.

    Args:
        sqrt_price_current: Current sqrt price (X96)
        sqrt_price_target: Target sqrt price (X96)
        liquidity: Liquidity value
        amount_remaining: Remaining amount (signed)
        fee_pips: Fee in pips

    Returns:
        Tuple of (sqrt_price_next, amount_in, amount_out, fee_amount)

    Raises:
        ValueError: On invalid input, overflow, or if liquidity exceeds int128

    """

def cl_max_usable_tick(tick_spacing: int) -> int:
    """Compute the maximum usable tick for a given tick spacing.

    Args:
        tick_spacing: The tick spacing value

    Returns:
        The maximum usable tick as an int

    """

def cl_min_usable_tick(tick_spacing: int) -> int:
    """Compute the minimum usable tick for a given tick spacing.

    Args:
        tick_spacing: The tick spacing value

    Returns:
        The minimum usable tick as an int

    """

def cl_get_tick_word_and_bit_position(tick: int, tick_spacing: int) -> tuple[int, int]:
    """Compute the tick word and bit position for a compressed tick.

    Args:
        tick: The tick value
        tick_spacing: The tick spacing value

    Returns:
        A ``(word, bit)`` tuple where ``word`` is the bitmap mapping key
        and ``bit`` is in ``0..=255``.

    """

def cl_apply_liquidity_mapping_update(
    tick_bitmap: dict[int, Any],
    tick_data: dict[int, Any],
    tick_spacing: int,
    tick: int,
    liquidity: int,
    initial_state_block: int,
    update_block: int,
    tick_lower: int,
    tick_upper: int,
    liquidity_delta: int,
) -> dict[str, Any]:
    """Apply a liquidity-mapping update (mint/burn) to the tick bitmap & data.

    A thin PyO3 wrapper over the pure-Rust
    ``degenbot_cl_math::cl_lib::liquidity_mapping::apply_liquidity_mapping_update``.
    Mirrors ``degenbot.calculations.concentrated_liquidity.apply_liquidity_mapping_update``.
    Values may be plain dicts or pydantic ``BitmapAtWord`` / ``LiquidityAtTick``
    models (the seam reads by key or attribute). ``initial_state_block`` values
    exceeding ``u64::MAX`` (e.g. ``MAX_UINT256`` used to disable the in-range
    adjustment) are clamped to ``u64::MAX``, preserving the skip behavior.

    Args:
        tick_bitmap: Word → ``{"bitmap": int, "block": int}`` (or models)
        tick_data: Tick → ``{"liquidity_net": int,
            "liquidity_gross": int, "block": int}`` (or models)
        tick_spacing: Tick spacing
        tick: Active tick (for the in-range check)
        liquidity: Active in-range liquidity (uint128)
        initial_state_block: State block at which the active liquidity was last settled
        update_block: Block of this liquidity event
        tick_lower: Position lower tick
        tick_upper: Position upper tick
        liquidity_delta: Signed delta (mint positive, burn negative)

    Returns:
        ``{"tick_bitmap": {...}, "tick_data": {...}, "liquidity": int}``

    Raises:
        ValueError: On invalid input types, non-uint128 liquidity, or non-i32 ticks

    """

# ── Balancer V2 math (feature = "balancer-math"). ──
# Pure-math wrappers over the degenbot-balancer-math leaf, registered on a
# real Python submodule (`degenbot._ffi.balancer_math`) with un-prefixed names
# — the `balancer_` prefix was an artifact of the old flat root registration.
# The `version` discriminant (1=V1, 2=V2) is the bytecode-detected PowVersion;
# `round_up` is the stable-invariant V1(always-roundDown)/V2(roundUp) axis.
# Reverts surface as ValueError/OverflowError carrying the Solidity revert tag.
from . import balancer_math as balancer_math  # noqa: E402
from . import curve_math as curve_math  # noqa: E402
from . import solidly_math as solidly_math  # noqa: E402

# ── Curve StableSwap math (feature = "curve-math"). ──
# Pure-math wrappers over the degenbot-curve-math leaf, registered on a real
# Python submodule (`degenbot._ffi.curve_math`) with un-prefixed names — the
# `curve_` prefix was an artifact of the flat root registration.
# See `curve_math.pyi` for the function signatures.
# The variant discriminants (`d_variant`/`y_variant`/`yd_variant`) are 1-based
# `auto()` enum `.value`s matching the Rust `try_from_u8`. Vyper reverts
# (overflow / non-convergence / index / unsafe-value) all surface as
# `ValueError` — the shape the Python `DyCalculator` catches and wraps as
# `EVMRevertError(error=str(e))`.

# ── Solidly / Aerodrome / Camelot stable math (feature = "solidly-math"). ──
# Pure-math wrappers over the degenbot-solidly-math leaf, registered on a real
# Python submodule (`degenbot._ffi.solidly_math`) with un-prefixed names — the
# `solidly_` prefix was an artifact of the flat root registration; `camelot_`
# stays because camelot is a distinct variant in the same crate.
# See `solidly_math.pyi` for the function signatures.
# The Solidly / Camelot invariant math (calc_d / calc_k / calc_f /
# get_y_solidly / camelot_f / camelot_k / camelot_get_y_camelot) is pure
# integer arithmetic (Newton's method on the Solidly `x^3*y + y^3*x >= k`
# invariant); code that reverts on uint256 overflow / non-convergence /
# invalid token_in surfaces as ValueError / ZeroDivisionError carrying the
# Solidity revert tag. The Solidly + Camelot amount-out orchestration is
# exposed as two pre-baked entrypoints (one per (k_func, get_y_func) pair the
# companions actually use); `fee` is split into ``fee_numer``/``fee_denom`` at
# the seam so the pure-math leaf stays `num-rational`-free.

# ── Solady LibZip (FastLZ) (degenbot-core, no feature gate). ──
# Thin wrappers over `degenbot_core::libzip`. Accept a hex string (with
# optional `0x` prefix), `bytes`, `bytearray`, or `HexBytes`; return `HexBytes`.
# Truncated/invalid back-references surface as `ValueError`. The Python
# companion `degenbot.utils.solady.libzip` delegates here (sub-step C routing).
def flz_compress(uncompressed_data: str | bytes) -> HexBytes: ...
def flz_decompress(compressed_data: bytes | bytearray | str) -> HexBytes: ...
def build_path_graph(
    database_path: str,
    chain_id: int,
    pool_kinds: set[int],
    allowed_intermediate_token_ids: set[int] | None = ...,
) -> dict[str, Any]: ...
def find_paths_rust(
    edges: list[tuple[int, int, int, int]],
    start_token_id: int,
    end_token_id: int,
    min_depth: int,
    max_depth: int | None,
    include_reverse: bool,
    pool_type_per_depth: list[set[int] | None] | None = ...,
) -> PathIterator: ...

# ------------------------------------------------------------------
# SQLite file operations (feature = "db").
# ------------------------------------------------------------------
# Thin PyO3 wrappers over `degenbot_db::ops`. The CLI (`degenbot.cli.database`)
# delegates here; the GIL is released during file I/O. Raise `ValueError` on
# any connection / DDL / backup / integrity-check failure.
# `db_upgrade_database` returns a discriminant string.

def db_create_new_database(path: str) -> None:
    """Create a fresh degenbot SQLite DB: WAL + head DDL + VACUUM + Alembic stamp.

    Args:
        path: Filesystem path for the new database (created if absent)

    Raises:
        ValueError: On any connection / PRAGMA / DDL / stamp failure

    """

def db_backup_database(src: str, dst: str) -> None:
    """Back up one SQLite DB into another via online backup.

    Asserts `PRAGMA integrity_check == "ok"` on both source (before) and
    destination (after). `dst` is created if absent and overwritten if present.

    Args:
        src: Source database path
        dst: Destination database path (created/overwritten)

    Raises:
        ValueError: On an open / backup / integrity-check failure

    """

def db_compact_database(path: str) -> None:
    """Compact a SQLite database via `VACUUM` (no-op for `:memory:`).

    Args:
        path: Database path

    Raises:
        ValueError: On a connection / VACUUM failure

    """

def db_upgrade_database(path: str) -> str:
    """Ensure the database is at the Alembic schema head.

    Returns ``"already_at_head"`` if the DB was current (no-op), or
    ``"created_fresh"`` if an empty file was brought up to head. A stale
    Alembic DB raises ``ValueError`` (run ``alembic upgrade head`` from Python).

    Args:
        path: Database path

    Returns:
        ``"already_at_head"`` or ``"created_fresh"``

    Raises:
        ValueError: On a stale / unrecognized schema, or an I/O failure

    """

def db_inspect_schema_state(database_path: str) -> str:
    """Inspect the schema state WITHOUT writing.

    The read-only dry-run companion to
    :func:`db_convert_alembic_to_rust_owned`. Never refuses (reports even
    stale / unrecognized states). Returns one of ``"alembic_current"``,
    ``"alembic_stale"``, ``"fresh_standalone"``, ``"rust_owned"``,
    ``"unrecognized"``.

    Args:
        database_path: Database path

    Returns:
        The schema-state label.

    Raises:
        ValueError: On an open / query failure.

    """

def db_convert_alembic_to_rust_owned(database_path: str) -> str:
    """Perform the opt-in one-way cutover (ADR-010).

    Flip an Alembic-stamped DB into Rust ownership — drops
    ``alembic_version``, stamps ``_degenbot_db_schema_version``.

    Args:
        database_path: Database path

    Returns:
        ``"converted"`` (was AlembicCurrent) or ``"already_rust_owned"``
        (was already Rust-owned → idempotent no-op).

    Raises:
        DatabaseSchemaStale: For a stale Alembic DB (run
            ``degenbot database upgrade`` first).
        ValueError: For an unrecognized (foreign) file or I/O failure.

    """

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

class CancelHandle:
    """Cooperative cancel flag for the long-running updater loops.

    Construct up front; pass to ``run_pool_update`` / ``run_aave_update``; a
    ``signal.SIGINT`` handler (installed by the CLI driver) calls ``.cancel()``
    to stop the run at the next chunk boundary. The loop polls the flag
    between chunks (NOT mid-chunk) so a cancel never breaks chunk atomicity.
    """

    def __init__(self) -> None: ...
    def cancel(self) -> None:
        """Request cancellation (honored at the next chunk boundary)."""
    def is_cancelled(self) -> bool:
        """Whether cancellation was requested."""
    def reset(self) -> None:
        """Reset the flag to ``False`` (reuse the handle)."""

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

def db_heal_database(database_path: str) -> dict[str, Any]:
    """Out-of-place dump-and-restore heal (ADR-011).

    Rebuild the DB at the Rust head schema, copy user rows preserving PKs +
    FK integrity (in FK-dependency order), stamp RustOwned directly (never
    runs Alembic code), then atomically swap with a ``*.bak`` backup. Never
    mutates the old DB in place — a read-only open feeds the copy, so the
    old file is left byte-identical until the final ``rename``.

    Args:
        database_path: Database path

    Returns:
        ``{"old_state": str, "rows_copied": dict[str, int],
        "bak_path": str, "new_state": str, "warnings": list[str]}``.
        No-op if old is already ``rust_owned`` (returns
        ``old_state == new_state == "rust_owned"``, empty ``rows_copied``,
        ``bak_path == database_path``).

    Raises:
        ValueError: For an unrecognized (foreign) file, an I/O failure, or a
            post-copy row-count verification failure (live DB untouched in
            both cases).

    """

# ------------------------------------------------------------------
# V3/V4 DB-aware liquidity updater seam (feature = "db").
# ------------------------------------------------------------------
# Thin PyO3 wrappers over `degenbot-db`'s `apply_v3_liquidity_updates` /
# `apply_v4_liquidity_updates` (the Rust apply-and-persist core). The Python
# `cli/pool.py::apply_v3/v4_liquidity_updates` decode the raw `LogReceipt`s
# (Burn/Mint negation) + delegate the reconstitute→apply-math→persist here.
# Each call opens its own write handle on `database_path` (SQLite WAL allows
# the concurrent connection); the driver's session stays open for its reads.
# Events are pre-decoded ``(block_number, log_index, tick_lower, tick_upper,
# liquidity_delta)`` tuples — the ABI decode stays in Python per the seam
# boundary (`degenbot-db` is pure I/O+math, no ABI decode).

def db_apply_v3_liquidity_updates(
    database_path: str,
    chain_id: int,
    pool_address: str,
    events: list[LiquidityUpdateEvent],
) -> bool:
    """Apply pre-decoded V3 liquidity events; persist positions/init-maps/marker.

    Returns ``False`` if the pool at ``(chain_id, pool_address)`` isn't found
    (mirrors the Python early-return); ``True`` after a successful apply.
    Raises ``ValueError`` on a DB failure.
    """

def db_apply_v4_liquidity_updates(
    database_path: str,
    pool_hash_hex: str,
    pool_manager_chain: int,
    events: list[LiquidityUpdateEvent],
) -> bool:
    """Apply pre-decoded V4 liquidity events; persist positions/init-maps/marker.

    Returns ``False`` if the pool at ``(pool_hash, pool_manager_chain)`` isn't
    found; ``True`` after a successful apply. Raises ``ValueError`` on a DB
    failure.
    """

# ------------------------------------------------------------------
# Aave V3 DB-aware writer seam (feature = "db").
# ------------------------------------------------------------------
# Thin PyO3 wrappers over `degenbot-db`'s `write` core (Sub-step B of
# AZGJUN/RQXEKH — the Aave V3 lending-market DB writers). The Python
# `cli/aave/db_*.py` / `event_handlers.py` callers (the driver loop + RPC
# event fetch stay Python) delegate the per-event upsert + apply here. Each
# call opens its OWN write handle on `database_path` via `open_for_writes`
# (NOT held across calls), releases the GIL across the SQLite I/O, and raises
# ``ValueError`` on a DB failure. No business logic.
#
# RPC-coupling carve-out: `db_get_or_create_erc20_token` + `db_get_or_create_user`
# take caller-supplied metadata / GHO-discount `Option` params; the Python
# driver stays the RPC authority (`degenbot-db` has no `degenbot-rpc` dep).

def db_get_or_create_e_mode_category(
    database_path: str,
    market_id: int,
    category_id: int,
) -> int:
    """Get-or-create an ``aave_v3_emode_categories`` row.

    On create, inserts the Python ORM defaults (``label=""``, ``ltv=0``,
    ``liquidation_threshold=0``, ``liquidation_bonus=0``). Returns the row id.
    Raises ``ValueError`` on a DB failure.
    """

def db_get_or_create_asset_config(database_path: str, asset_id: int) -> int:
    """Get-or-create an ``aave_v3_asset_configs`` row by ``asset_id``.

    On create, inserts the Python ORM defaults (all zero/false/None). Returns
    the row id. Raises ``ValueError`` on a DB failure.
    """

def db_get_or_create_user_collateral_config(
    database_path: str,
    user_id: int,
    asset_id: int,
) -> int:
    """Get-or-create an ``aave_v3_user_collateral_configs`` row.

    On create, inserts ``enabled=False`` (the Python default). Returns the row
    id. Raises ``ValueError`` on a DB failure.
    """

def db_get_or_create_user(
    database_path: str,
    market_id: int,
    address: str,
    gho_discount: int,
) -> int:
    """Get-or-create an ``aave_v3_users`` row by ``(market_id, address)``.

    On create, inserts the Python ORM defaults with the caller-supplied
    ``gho_discount`` (the GHO discount is RPC-fetched by the Python driver —
    `stays-python`; pass ``0`` for non-GHO markets). Returns the row id.
    Raises ``ValueError`` on a DB failure.
    """

def db_get_or_create_erc20_token(
    database_path: str,
    chain: int,
    address: str,
    name: str | None = None,
    symbol: str | None = None,
    decimals: int | None = None,
) -> int:
    """Get-or-create an ``erc20_tokens`` row by ``(chain, address)``.

    On create, inserts the caller-supplied metadata (``name`` / ``symbol`` /
    ``decimals``; the Python driver RPC-fetches them — `stays-python`). Pass
    ``None`` to leave a column NULL. A second call returns the existing row
    id without overwriting metadata. Metadata write-back
    (``update_erc20_token_metadata``) already lives on ``PyBotIo``. Raises
    ``ValueError`` on a DB failure.
    """

def db_get_or_create_collateral_position(
    database_path: str,
    user_id: int,
    asset_id: int,
) -> int:
    """Get-or-create an ``aave_v3_collateral_positions`` row.

    On create, inserts ``balance='0'``, ``last_index=None``. Returns the row id.
    Raises ``ValueError`` on a DB failure.
    """

def db_get_or_create_debt_position(
    database_path: str,
    user_id: int,
    asset_id: int,
) -> int:
    """Get-or-create an ``aave_v3_debt_positions`` row.

    On create, inserts ``balance='0'``, ``last_index=None``. Returns the row
    id. Raises ``ValueError`` on a DB failure.
    """

def db_apply_collateral_configuration_changed(
    database_path: str,
    asset_id: int,
    config_bitmap: int,
) -> int:
    """Apply a ``CollateralConfigurationChanged`` event's decoded bitmap.

    ``config_bitmap`` is the raw ``uint256`` the Python driver RPC-fetches
    via ``Pool.getConfiguration`` (the fetch is `stays-python`); decoded via
    ``db_decode_reserve_configuration_bitmap``. On a new row, inserts every
    decoded field; on an existing row, updates every field. Returns the row
    id. Raises ``ValueError`` on a DB failure or if the bitmap isn't a
    non-negative 256-bit integer.
    """

def db_apply_e_mode_category_added(
    database_path: str,
    market_id: int,
    category_id: int,
    ltv: int,
    liquidation_threshold: int,
    liquidation_bonus: int,
    price_source: str | None = None,
    label: str = "",
) -> int:
    """Apply an ``EModeCategoryAdded`` event's decoded fields.

    On create, inserts ``(label, ltv, liquidation_threshold, liquidation_bonus,
    price_source)``; on existing, updates the same five fields. ``price_source``
    is the oracle address (``None`` when zero / no oracle). Returns the row id.
    Raises ``ValueError`` on a DB failure.
    """

def db_apply_emode_asset_category_changed(
    database_path: str,
    asset_id: int,
    new_category_id: int,
) -> int:
    """Apply an ``EModeAssetCategoryChanged`` event (the older Aave variant).

    Unconditionally sets ``e_mode_category_id`` to the new category (``None``
    when ``new_category_id == 0``). Returns the row id. Raises ``ValueError``
    on a DB failure.
    """

def db_apply_asset_collateral_in_emode_changed(
    database_path: str,
    asset_id: int,
    category_id: int,
    is_collateral: bool,
) -> int:
    """Apply an ``AssetCollateralInEModeChanged`` event (the v3.4+ variant).

    Sets ``e_mode_category_id`` to the category ONLY when
    ``is_collateral and category_id > 0``; otherwise the row is left
    unchanged (the Python ``elif`` branch). Returns the row id. Raises
    ``ValueError`` on a DB failure.
    """

def db_apply_reserve_used_as_collateral(
    database_path: str,
    user_id: int,
    asset_id: int,
    enabled: bool,
) -> int:
    """Apply a ``ReserveUsedAsCollateralEnabled``/``Disabled`` event.

    Sets the ``aave_v3_user_collateral_configs.enabled`` flag (``True`` for
    enabled, ``False`` for disabled). On create, inserts with the given
    ``enabled``; on existing, updates the flag. Returns the row id. Raises
    ``ValueError`` on a DB failure.
    """

def db_apply_user_e_mode_set(
    database_path: str,
    user_id: int,
    e_mode: int,
) -> int:
    """Apply a ``UserEModeSet`` event: set the user's ``e_mode`` column.

    The user row must already exist (the Python path ``get_or_create_user``s
    first; the driver passes the existing ``user_id``). Returns ``user_id``.
    Raises ``ValueError`` on a DB failure.
    """

def db_apply_price_oracle_updated(
    database_path: str,
    market_id: int,
    new_oracle_address: str,
) -> int:
    """Apply a ``PriceOracleUpdated`` event: register the new ``PRICE_ORACLE``.

    Upserts the ``aave_v3_contracts`` row for ``market_id`` (inserts, or
    updates the address if a ``PRICE_ORACLE`` row already exists). Returns
    the row id. Raises ``ValueError`` on a DB failure.
    """

def db_apply_asset_source_updated(
    database_path: str,
    asset_id: int,
    source_address: str,
) -> int:
    """Apply an ``AssetSourceUpdated`` event: set ``price_source``.

    The ``asset_id`` must already exist (the Python ``assert asset is not
    None``). Returns ``asset_id``. Raises ``ValueError`` on a DB failure.
    """

def db_decode_reserve_configuration_bitmap(config_bitmap: int) -> dict[str, Any]:
    """Decode the Aave V3 reserve-configuration ``uint256`` bitmap into a dict.

    Pure CPU. The returned dict's keys match the Python
    ``_decode_reserve_configuration_bitmap`` oracle 1:1 (``ltv``,
    ``liquidation_threshold``, ``liquidation_bonus``, ``decimals``,
    ``is_active``, ``is_frozen``, ``borrowing_enabled``,
    ``stable_rate_borrowing_enabled``, ``reserve_factor``, ``borrow_cap``,
    ``supply_cap``, ``debt_ceiling``, ``liquidation_protocol_fee``,
    ``unbacked_mint_cap``, ``e_mode_category_id`` (``None`` when the decoded
    byte is ``0``), ``flash_loan_enabled``, ``isolation_mode``,
    ``borrowable_in_isolation``). ``config_bitmap`` is the raw ``uint256``
    the caller RPC-fetches via ``Pool.getConfiguration``. Raises
    ``ValueError`` if the bitmap isn't a non-negative 256-bit integer.
    """

def db_fetch_pool_row(
    database_path: str,
    chain_id: int,
    address: str,
) -> LiquidityPoolRow | None:
    """Fetch a `pools` row by ``(chain_id, address)`` (QJSCA5 §4.3).

    The V3 `apply_3_liquidity_updates` shell uses this to read the pool's
    `exchange_id` for the `exchanges_in_scope` precondition. Raises
    ``ValueError`` on a DB failure.
    """

def db_fetch_exchange(
    database_path: str,
    exchange_id: int,
) -> ExchangeRow | None:
    """Fetch an `exchanges` row by its FK id.

    The `cli/pool.py::pool_update` discovery loop reads `last_update_block`
    ground-truth here (a fresh connection → fresh WAL snapshot) rather than
    trusting the long-lived SQLAlchemy session's stale ORM cache, since the
    stamp is written by the Rust `db_set_exchange_last_update_block` seam on
    its own connection. Raises ``ValueError`` on a DB failure.
    """

def db_fetch_exchange_by_name(
    database_path: str,
    chain_id: int,
    name: str,
) -> ExchangeRow | None:
    """Fetch an `exchanges` row by `(chain_id, name)` (the deactivate-CLI resolution)."""

# ------------------------------------------------------------------
# Pool discovery writers (WR7EA6 — split out of QJSCA5).
# Thin PyO3 wrappers over `degenbot-db`'s `discovery` substrate
# (`upsert_v2/v3/v4_pools` + `set_exchange_last_update_block`). The Python
# `cli/pool_updater_configs.py::update_v2/v3/v4_pools` shells decode the raw
# `PoolCreated` `LogReceipt`s + do the RPC fee lookup, then build row-input
# lists + delegate here — the Rust core owns the `erc20_tokens` get-or-create
# escalate + the polymorphic pool-row insert + the exchange stamp. Raises
# ``ValueError`` on a DB failure or an unknown `kind` discriminator.

class V2PoolRowInput:
    """One V2 pool-row to upsert (WR7EA6)."""

    def __init__(
        self,
        address: str,
        token0_address: str,
        token1_address: str,
        fee_token0: int,
        fee_token1: int,
        stable: bool | None = ...,
    ) -> None: ...

class V3PoolRowInput:
    """One V3 pool-row to upsert (WR7EA6)."""

    def __init__(
        self,
        address: str,
        token0_address: str,
        token1_address: str,
        fee: int,
        tick_spacing: int,
    ) -> None: ...

class V4PoolRowInput:
    """One V4 pool-row to upsert (WR7EA6)."""

    def __init__(
        self,
        pool_hash: str,
        hooks: str,
        currency0_address: str,
        currency1_address: str,
        fee: int,
        tick_spacing: int,
    ) -> None: ...

def db_upsert_v2_pools(
    database_path: str,
    chain_id: int,
    kind: str,
    exchange_id: int,
    fee_denominator: int,
    rows: list[V2PoolRowInput],
) -> None:
    """Insert a batch of V2 pool rows (WR7EA6).

    The Rust core get-or-create's the two `erc20_tokens` per row + inserts the
    polymorphic base `pools` row + the subclass detail row. Raises
    ``ValueError`` if `kind` is not a known V2 family discriminator.
    """

def db_upsert_v3_pools(
    database_path: str,
    chain_id: int,
    kind: str,
    exchange_id: int,
    fee_denominator: int,
    rows: list[V3PoolRowInput],
) -> None:
    """Insert a batch of V3 pool rows (WR7EA6).

    Same shape as `db_upsert_v2_pools`; subclass detail row carries `tick_spacing`
    + the fee columns. Raises ``ValueError`` if `kind` is not a V3 family.
    """

def db_upsert_v4_pools(
    database_path: str,
    chain_id: int,
    pool_manager_address: str,
    fee_denominator: int,
    rows: list[V4PoolRowInput],
) -> None:
    """Insert a batch of V4 pool rows (WR7EA6).

    The Rust core resolves the `PoolManagerTable` id from
    `(chain_id, pool_manager_address)`, then per row inserts the `managed_pools`
    base + `uniswap_v4_pools` detail row. Raises ``ValueError`` if no
    `PoolManager` row matches.
    """

def db_set_exchange_last_update_block(
    database_path: str,
    chain_id: int,
    exchange_id: int,
    block: int,
) -> None:
    """Stamp an `ExchangeTable.last_update_block` (WR7EA6)."""

def db_upsert_exchange(
    database_path: str,
    chain_id: int,
    name: str,
    factory: str,
    deployer: str | None,
) -> ExchangeRow:
    """Resolve an `exchanges` row by `(chain_id, name)`, inserting `active=False` if absent."""

def db_set_exchange_active(
    database_path: str,
    exchange_id: int,
    active: bool,
) -> None:
    """Flip an `exchanges` row's `active` flag by id (activate/deactivate primitive)."""

def db_upsert_pool_manager(
    database_path: str,
    address: str,
    chain: int,
    kind: str,
    state_view: str | None,
    exchange_id: int,
) -> PoolManagerRow:
    """Upsert a `pool_managers` row by `(address, chain)` (V4 manager get-or-create)."""

# ------------------------------------------------------------------
# Thin PyO3 wrappers over `degenbot_executor` (the cmd-executor core).
# The Python encoder (`examples/eth_backrun_helpers.py::encode_cmd_stream` +
# `examples/cmd_stream.py`) was deleted in the §4.3 oracle-retirement cutover;
# byte-for-byte parity (§4.2) is pinned by golden-file tests in
# `cargo test -p degenbot-executor`. The GIL is released during the encode /
# warmup-slot compute.

# PathInfo is imported from `degenbot.arbitrage.hop_info`.
from degenbot.arbitrage.hop_info import PathInfo  # noqa: E402

type BytesOrNone = bytes | None
type WarmupDict = dict[str, dict[str, Any]]

def encode_cmd_stream(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: list[int],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    *,
    erc6909_profit: bool = ...,
    use_v4_batch: bool = ...,
) -> BytesOrNone: ...
def compute_simulation_warmup_slots(
    executor_address: str,
    weth_address: str,
    pool_manager_address: str,
) -> WarmupDict:
    """Compute ``eth_simulateV1`` ``stateDiff`` overrides.

    Replicates ``cmd_executor.initialize()``'s three warmed storage slots.

    Args:
        executor_address: The cmd_executor contract address.
        weth_address: The WETH9 contract address.
        pool_manager_address: The Uniswap V4 PoolManager address.

    Returns:
        A dict keyed by checksummed contract addresses, with ``stateDiff``
        sub-dicts mapping slot hex to 1-wei value hex, plus the executor's
        residual balance entry.

    """

def pack_config(
    check_mode: int = ...,
    expected_value: int = ...,
    bribe_bips: int = ...,
    bribe_recipient_idx: int = ...,
) -> int:
    """Pack the ``execute(commands, config)`` ABI ``config`` uint256."""

def pack_expected_balance(check_mode: int, expected_value: int) -> int:
    """Return a deprecated alias for ``pack_config``.

    Uses ``bribe_bips=0`` / ``bribe_recipient_idx=0``.
    """

def mapping_slot(base_slot: int, key: int) -> int:
    """Compute a Solidity mapping storage slot (``keccak256(pad(key,32) || pad(base,32))``)."""

def nested_mapping_slot(base_slot: int, key1: int, key2: int) -> int:
    """Compute a nested Solidity mapping storage slot."""

def v4_input_is_native(hop: object) -> bool:
    """Return whether the V4 hop's input currency is native ETH (address(0))."""

def v4_output_is_native(hop: object) -> bool:
    """Return whether the V4 hop's output currency is native ETH (address(0))."""

class PyDatabaseSnapshot:
    """Read-only V3/V4 snapshot handle over a degenbot SQLite DB file.

    Opens its own connection (WAL, ``query_only=on``) from ``database_path``;
    the Python ``DatabaseSnapshot`` shell constructs one per chain and
    delegates every read to it.

    """

    def __init__(self, chain_id: int, database_path: str) -> None: ...
    def get_liquidity_map_v3(self, pool_address: str) -> dict[str, Any] | None: ...
    def get_liquidity_map_v4(
        self, pool_manager: str, pool_id: bytes | str
    ) -> dict[str, Any] | None: ...
    def get_all_liquidity_maps_v3(self) -> dict[str, dict[int, tuple[int, int]]]: ...
    def get_all_liquidity_maps_v4(
        self,
    ) -> dict[tuple[str, str], dict[int, tuple[int, int]]]: ...
    def get_newest_block_v3(self) -> int | None: ...
    def get_newest_block_v4(self) -> int | None: ...
    def get_pools_v3(self) -> set[str]: ...
    def get_pools_v4(self) -> set[str]: ...

class PyDatabasePositionQuery:
    """Read-only Aave V3 position-query handle over a degenbot SQLite DB file.

    Opens its own connection (WAL, ``query_only=on``) from ``database_path``;
    the Python ``DatabasePositionQuery`` shell constructs one and delegates
    every read to it.

    """

    def __init__(self, database_path: str) -> None: ...
    def get_users_with_debt(
        self, market_id: int, limit: int | None = None
    ) -> list[dict[str, Any]]: ...
    def get_collateral_positions(self, user_id: int) -> list[dict[str, Any]]: ...
    def get_debt_positions(self, user_id: int) -> list[dict[str, Any]]: ...
    def get_collateral_config_map(self, user_id: int) -> dict[int, bool]: ...
    def get_oracle_address(self, market_id: int) -> str | None: ...
    def get_asset_addresses(self, market_id: int) -> list[str]: ...

class PathIterator:
    def __iter__(self) -> PathIterator: ...
    def __next__(self) -> list[tuple[int, int]]: ...

def get_sqrt_ratio_at_tick(tick: int) -> int:
    """Convert a tick value to its corresponding sqrt price (X96 format).

    Args:
        tick: The tick value in range [-887272, 887272]

    Returns:
        A Python int representing the sqrt price X96 value

    Raises:
        ValueError: If the tick value is invalid (out of range)

    """

#: Minimum Uniswap V3 tick. Canonical source: ``degenbot-cl-math`` core.
MIN_TICK: int
#: Maximum Uniswap V3 tick. Canonical source: ``degenbot-cl-math`` core.
MAX_TICK: int
#: Minimum sqrt price ratio (X96). Canonical source: ``degenbot-cl-math`` core.
MIN_SQRT_RATIO: int
#: Maximum sqrt price ratio (X96). Canonical source: ``degenbot-cl-math`` core.
MAX_SQRT_RATIO: int

@overload
def get_tick_at_sqrt_ratio(sqrt_price_x96: int) -> int: ...
@overload
def get_tick_at_sqrt_ratio(sqrt_price_x96: bytes) -> int: ...
@overload
def to_checksum_address(address: str) -> str: ...
@overload
def to_checksum_address(address: bytes) -> str: ...

# ── ABI decoder/encoder (feature = "abi"). ──
# Pure-math wrappers over the degenbot-abi leaf, registered on a real Python
# submodule (`degenbot._ffi.abi`) with un-prefixed names.
# See `abi.pyi` for the function signatures (decode/decode_single/encode/
# encode_single). ValueError on invalid data; NotImplementedError for
# unsupported types (e.g. fixed-point).
from . import abi as abi  # noqa: E402

def encode_function_call(function_signature: str, args: list[str]) -> bytes:
    """Encode function arguments into calldata.

    Args:
        function_signature: Function signature like "transfer(address,uint256)"
        args: List of arguments as strings

    Returns:
        Encoded calldata as bytes (selector + encoded args)

    Raises:
        ValueError: If the signature or arguments are invalid

    """

def decode_return_data(data: bytes, output_types: list[str]) -> list[str]:
    """Decode return data from a contract call.

    Args:
        data: Return data as bytes
        output_types: List of output type strings like ["uint256", "address"]

    Returns:
        List of decoded values as strings

    Raises:
        ValueError: If data is invalid or cannot be decoded

    """

def get_function_selector(function_signature: str) -> str:
    """Parse a function signature and return its selector.

    Args:
        function_signature: Function signature like "transfer(address,uint256)"

    Returns:
        4-byte function selector as hex string (e.g., "0xa9059cbb")

    Raises:
        ValueError: If the function signature is invalid

    """

class Contract:
    """Synchronous wrapper for smart contract interactions."""

    def __init__(self, address: str, provider_url: str | None = None) -> None: ...
    @staticmethod
    def from_provider(address: str, provider: AlloyProvider) -> Contract: ...
    @property
    def address(self) -> str: ...
    def call(
        self,
        function_signature: str,
        args: list[str],
        block_number: int | None = None,
    ) -> list[str]:
        """Execute a contract call.

        Args:
            function_signature: Function signature like "balanceOf(address)"
            args: List of arguments as strings
            block_number: Optional block number to query

        Returns:
            List of decoded return values as strings

        """

class PyChainlinkPriceFeed:
    """Typed Chainlink aggregator price-feed reader."""

    def __init__(
        self,
        address: str,
        provider: AlloyProvider,
        chain_id: int | None = None,
    ) -> None: ...
    @property
    def address(self) -> str: ...
    @property
    def chain_id(self) -> int | None: ...
    def decimals(self) -> int:
        """Call ``decimals()`` and return the feed's decimal places (``uint8``)."""
    def latest_round_data(self) -> tuple[int, int, int, int, int]:
        """Call ``latestRoundData()`` and return the round tuple.

        ``(round_id, answer, started_at, updated_at, answered_in_round)``
        as Python ints (``answer`` is the raw ``int256`` in feed decimals).
        """
    def price(self) -> int:
        """Decimal-corrected whole-unit price as an int.

        ``int(answer // 10**decimals)``; negative clamps to 0.
        """

class PyAavePriceOracle:
    """Typed Aave price-oracle reader."""

    def __init__(self, oracle_address: str, provider: AlloyProvider) -> None: ...
    @property
    def address(self) -> str: ...
    def get_asset_price(self, asset_address: str) -> int:
        """Call ``getAssetPrice(asset)`` and return the ``uint256`` price."""
    def fetch(self, asset_addresses: list[str]) -> dict[str, int]:
        """Fetch prices for a batch of assets, tolerantly.

        A failure on one asset is logged and skipped; successfully-fetched
        prices are returned keyed by checksummed asset address.
        """

class LogFilter:
    """Filter for log queries."""

    def __init__(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> None: ...
    @property
    def from_block(self) -> int | None: ...
    @property
    def to_block(self) -> int | None: ...
    @property
    def addresses(self) -> list[str]: ...
    @property
    def topics(self) -> list[list[str]]: ...

class AlloyProvider:
    r"""Synchronous Ethereum RPC provider.

    Automatically detects connection type from URL:
    - HTTP/HTTPS URLs use HTTP transport with connection pooling
    - WS/WSS URLs use WebSocket transport
    - File paths (Unix: /path, Windows: \\.\pipe\...) use IPC transport

    Rate limiting is opt-in: pass ``requests_per_second`` and ``burst``
    together to enable transport-level throttling on HTTP connections.
    """

    def __init__(
        self,
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
        requests_per_second: int | None = None,
        burst: int | None = None,
    ) -> None: ...
    @property
    def rpc_url(self) -> str: ...
    def get_block_number(self) -> int: ...
    def get_chain_id(self) -> int: ...
    def get_gas_price(self) -> int: ...
    def get_block(self, block_number: int) -> BlockData | None: ...
    def get_transaction(self, tx_hash: str) -> TransactionData | None: ...
    def get_transaction_receipt(self, tx_hash: str) -> TransactionReceiptData | None: ...
    def get_logs(
        self,
        *,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogData]: ...
    def call(
        self,
        to: str,
        data: bytes,
        block_number: int | None = None,
    ) -> HexBytes: ...
    def get_code(
        self,
        address: str,
        block_number: int | None = None,
    ) -> HexBytes: ...
    def estimate_gas(
        self,
        to: str,
        data: bytes,
        from_: str | None = None,
        value: int | None = None,
        block_number: int | None = None,
    ) -> int: ...
    def get_storage_at(
        self,
        address: str,
        position: int,
        block_number: int | None = None,
    ) -> HexBytes: ...
    def get_balance(
        self,
        address: str,
        block_number: int | None = None,
    ) -> int: ...
    def get_transaction_count(
        self,
        address: str,
        block_number: int | None = None,
    ) -> int: ...
    @staticmethod
    def offline_from_json_file(path: str) -> AlloyProvider: ...
    @staticmethod
    def offline_from_json_string(s: str) -> AlloyProvider: ...
    def make_request(
        self,
        method: str,
        params: list[str | bool | int | bytes | None],
    ) -> str | bool | int | bytes | list[Any] | dict[str, Any] | None: ...
    def close(self) -> None: ...
    def subscribe_blocks(self) -> AlloySubscription: ...
    def subscribe_full_blocks(self) -> AlloySubscription: ...
    def subscribe_pending_transactions(self) -> AlloySubscription: ...
    def subscribe_full_pending_transactions(self) -> AlloySubscription: ...
    def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> AlloySubscription: ...

class AsyncAlloyProvider:
    """Async wrapper for AlloyProvider operations."""

    def __init__(self, sync_provider: AlloyProvider) -> None: ...
    @staticmethod
    def create(
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
        requests_per_second: int | None = None,
        burst: int | None = None,
    ) -> Coroutine[Any, Any, AsyncAlloyProvider]: ...
    @property
    def rpc_url(self) -> str: ...
    def get_block_number(self) -> Coroutine[Any, Any, int]: ...
    def get_chain_id(self) -> Coroutine[Any, Any, int]: ...
    def get_gas_price(self) -> Coroutine[Any, Any, int]: ...
    def get_block(self, block_number: int) -> Coroutine[Any, Any, BlockData | None]: ...
    def get_transaction(self, tx_hash: str) -> Coroutine[Any, Any, TransactionData | None]: ...
    def get_transaction_receipt(
        self,
        tx_hash: str,
    ) -> Coroutine[Any, Any, TransactionReceiptData | None]: ...
    def get_logs(
        self,
        *,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> Coroutine[Any, Any, list[LogData]]: ...
    def call(
        self,
        to: str,
        data: bytes,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, HexBytes]: ...
    def get_code(
        self,
        address: str,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, HexBytes]: ...
    def estimate_gas(
        self,
        to: str,
        data: bytes,
        from_: str | None = None,
        value: int | None = None,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, int]: ...
    def get_storage_at(
        self,
        address: str,
        position: int,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, HexBytes]: ...
    def get_balance(
        self,
        address: str,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, int]: ...
    def get_transaction_count(
        self,
        address: str,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, int]: ...
    def make_request(
        self,
        method: str,
        params: list[str | bool | int | bytes | None],
    ) -> Coroutine[Any, Any, str | bool | int | bytes | list[Any] | dict[str, Any] | None]: ...
    def close(self) -> None: ...
    def subscribe_blocks(self) -> AlloySubscription: ...
    def subscribe_full_blocks(self) -> AlloySubscription: ...
    def subscribe_pending_transactions(self) -> AlloySubscription: ...
    def subscribe_full_pending_transactions(self) -> AlloySubscription: ...
    def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> AlloySubscription: ...

class AlloySubscription:
    """Python wrapper for a subscription from the Rust layer."""

    def __aiter__(self) -> AlloySubscription: ...
    def __anext__(self) -> Coroutine[Any, Any, BlockData | LogData | str]: ...
    def drain(self) -> list[BlockData | LogData | str]: ...
    async def started(self) -> None: ...
    def unsubscribe(self) -> None: ...

class AsyncContract:
    """Async wrapper for contract interactions."""

    @staticmethod
    def create(
        address: str,
        provider_url: str,
        max_retries: int | None = None,
    ) -> Coroutine[Any, Any, AsyncContract]: ...
    @staticmethod
    def from_provider(address: str, provider: AlloyProvider) -> AsyncContract: ...
    @property
    def address(self) -> str: ...
    def call(
        self,
        function_signature: str,
        args: list[str],
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, list[str]]: ...
    def batch_call(
        self,
        calls: list[tuple[str, list[str]]],
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, list[list[str]]]: ...

class AnvilFork:
    """Rust-owned Anvil fork handle (PyO3 seam over ``degenbot-fork``).

    Spawns an ``anvil`` subprocess via ``alloy::node_bindings::Anvil`` and
    connects an alloy ``DynProvider`` over IPC. The 12 dev-RPC methods
    (``evm_mine``/``anvil_snapshot``/``anvil_set_balance``/…) are exposed
    sync; the underlying alloy Provider is async and the PyO3 wrapper drives
    it via the shared degenbot-core tokio runtime.

    Post-spawn state overrides (``balance_overrides``/``code_overrides``/
    ``nonce_overrides``/``storage_overrides``) are NOT exposed on the
    constructor — apply them after construction via ``set_balance``/
    ``set_code``/``set_nonce``/``set_storage_at``.

    The legacy Python ``AnvilFork.w3`` handle is replaced in FF4
    (``WXRNHH``) by an ``AlloyProvider`` constructed over
    ``AnvilFork.ipc_path``.

    """

    def __init__(
        self,
        *,
        fork_url: str | None = None,
        fork_block: int | None = None,
        fork_transaction_hash: str | None = None,
        mining_mode: Literal["auto", "interval", "none"] = "auto",
        mining_interval: int | None = None,
        storage_caching: bool = True,
        base_fee: int | None = None,
        ipc_path: str | None = None,
        port: int | None = None,
        host: str = "127.0.0.1",
        mnemonic: str = ...,
        chain_id: int | None = None,
        anvil_opts: list[str] | None = None,
    ) -> None: ...
    @property
    def ipc_path(self) -> str: ...
    @property
    def http_url(self) -> str: ...
    @property
    def ws_url(self) -> str: ...
    @property
    def port(self) -> int: ...
    def mine(self) -> None: ...
    def reset(self, block_number: int | None = None) -> None: ...
    def snapshot(self) -> int: ...
    def revert(self, snapshot_id: int) -> bool: ...
    def set_balance(self, address: str, balance: int) -> None: ...
    def set_code(self, address: str, code: bytes) -> None: ...
    def set_nonce(self, address: str, nonce: int) -> None: ...
    def set_storage_at(self, address: str, slot: int, value: int) -> None: ...
    def set_next_base_fee(self, basefee: int) -> None: ...
    def set_next_block_timestamp(self, timestamp: int) -> None: ...
    def set_block_timestamp_interval(self, seconds: int) -> None: ...
    def set_coinbase(self, address: str) -> None: ...

class PyErc20Token:
    """Thin PyO3 handle to a token registered in the Rust `Bot`.

    All metadata lives in Rust; reads cross PyO3 on every access. Not directly
    constructible — obtain one via `PyBot.register_token` / `PyBot.get_token`.
    """

    @property
    def address(self) -> str: ...
    @property
    def decimals(self) -> int: ...
    @property
    def symbol(self) -> str: ...
    @property
    def name(self) -> str: ...
    @property
    def chain_id(self) -> int: ...

class PyDexIdentity:
    """Frozen Python view over a `DexIdentity` preset (ADR-005 slice 6).

    Read-only deployment identity (factory, deployer, init hash, default fees,
    reserve ABI shape, variant string). Constructable via `PyDexIdentity(...)`
    for custom identities (tests / ad-hoc deployments) or resolved via
    `dex_identity(variant)` for canonical presets.
    """

    def __init__(
        self,
        factory: str,
        init_hash: str,
        fee_token0: tuple[int, int],
        fee_token1: tuple[int, int],
        variant: str,
        reserves_abi: list[str] | None = None,
    ) -> None: ...
    @property
    def factory(self) -> str: ...
    @property
    def deployer(self) -> str: ...
    @property
    def init_hash(self) -> str: ...
    @property
    def fee_token0(self) -> tuple[int, int]: ...
    @property
    def fee_token1(self) -> tuple[int, int]: ...
    @property
    def reserves_abi(self) -> list[str]: ...
    @property
    def variant(self) -> str: ...

def dex_identity(variant: str) -> PyDexIdentity | None:
    """Look up a DEX deployment-identity preset by kebab-case variant string.

    Case-insensitive. Returns `None` for an unrecognized variant.
    """

def init_hash_for(chain_id: int, factory: str) -> str | None:
    """JSON-sourced CREATE2 init code hash for a `(chain_id, factory)` pair.

    Returns `None` if the pair is not in the shipped JSON.
    """

def deployer_for(chain_id: int, factory: str) -> str | None:
    """JSON-sourced effective CREATE2 deployer for a `(chain_id, factory)` pair.

    Returns `None` if the pair is not in the shipped JSON.
    """

def resolve_deployer(chain_id: int, factory: str) -> str:
    """Resolve the effective CREATE2 deployer for a `(chain_id, factory)` pair.

    Applies the `None -> factory` convention; returns the factory itself for
    non-JSON pools (Fork A, P62DKO).
    """

def resolve_v3_init_hash(chain_id: int, factory: str) -> str:
    """Resolve the CREATE2 init code hash for a V3 `(chain_id, factory)` pair.

    Returns the JSON row's init hash when shipped, else the Uniswap V3
    mainnet fallback (Fork A, P62DKO).
    """

def resolve_v2_init_hash(chain_id: int, factory: str) -> str:
    """Resolve the CREATE2 init code hash for a V2 `(chain_id, factory)` pair.

    Returns the JSON row's init hash when shipped, else the Uniswap V2
    mainnet fallback (Fork A, NSAZ4X).
    """

class PyLiquidityPool:
    """Thin PyO3 handle to a pool registered in the Rust `Bot`.

    Owns no state — calculation/encoding calls cross PyO3 on every access,
    reading the shared Rust-owned `Bot` under a read guard. Not directly
    constructible — obtain one via `PyBot.get_pool`.
    """

    @property
    def pool_id(self) -> int: ...
    @property
    def address(self) -> str: ...
    @property
    def factory(self) -> str: ...
    @property
    def deployer(self) -> str: ...
    @property
    def init_hash(self) -> str: ...
    @property
    def fee_token0(self) -> tuple[int, int]: ...
    @property
    def fee_token1(self) -> tuple[int, int]: ...
    @property
    def variant(self) -> str: ...
    @property
    def pool_family(self) -> str: ...
    @property
    def stable_swap(self) -> bool: ...
    @property
    def fee_denominator(self) -> int | None: ...
    @property
    def dex(self) -> PyDexIdentity | None: ...
    def get_token0(self) -> PyErc20Token | None: ...
    def get_token1(self) -> PyErc20Token | None: ...
    @property
    def reserve0(self) -> int: ...
    @property
    def reserve1(self) -> int: ...
    @property
    def update_block(self) -> int: ...
    @property
    def sqrt_price_x96(self) -> int: ...
    @property
    def liquidity(self) -> int: ...
    @property
    def tick(self) -> int: ...
    @property
    def fee(self) -> int: ...
    @property
    def tick_spacing(self) -> int: ...
    @property
    def pool_manager_address(self) -> str: ...
    @property
    def pool_id_hex(self) -> str: ...
    @property
    def hook_address(self) -> str: ...
    @property
    def balancer_address(self) -> str: ...
    @property
    def balancer_vault(self) -> str: ...
    @property
    def balancer_pool_id_hex(self) -> str: ...
    @property
    def balancer_token_addresses(self) -> list[str]: ...
    @property
    def balancer_weights(self) -> list[int]: ...
    @property
    def balancer_scaling_factors(self) -> list[int]: ...
    @property
    def balancer_swap_fee(self) -> int: ...
    @property
    def balancer_pow_version(self) -> int: ...
    def get_balancer_tokens(self) -> list[PyErc20Token] | None: ...
    @property
    def aerodrome_stable(self) -> bool: ...
    @property
    def aerodrome_fee(self) -> tuple[int, int]: ...
    @property
    def aerodrome_reserve0(self) -> int: ...
    @property
    def aerodrome_reserve1(self) -> int: ...
    def snapshot_aerodrome(self) -> tuple[int, int, int] | None: ...
    def apply_aerodrome_sync(self, reserve0: int, reserve1: int, block_number: int) -> None: ...
    def discard_aerodrome_before_block(self, block: int) -> None: ...
    def restore_aerodrome_before_block(self, block: int) -> tuple[int, int, int] | None: ...
    def snapshot(self) -> tuple[int, int, int] | None: ...
    def snapshot_v3(self) -> tuple[int, int, int, int] | None: ...
    def calculate_tokens_out(self, zero_for_one: bool, amount_in: int) -> int: ...
    def calculate_tokens_in(self, zero_for_one: bool, amount_out: int) -> int: ...
    def calculate_tokens_out_with_fetch(
        self,
        zero_for_one: bool,
        amount_in: int,
        block: int,
    ) -> int: ...
    def simulate_swap_with_fetch(
        self,
        zero_for_one: bool,
        amount_in: int,
        block: int,
        sqrt_price_limit_x96: int | None = None,
    ) -> tuple[int, int, int, int, int] | None: ...
    def simulate_swap_with_override(
        self,
        zero_for_one: bool,
        amount_in: int,
        block: int,
        override_sqrt_price_x96: int,
        override_liquidity: int,
        override_tick: int,
        override_tick_data: dict[int, tuple[int, int, int]],
        sqrt_price_limit_x96: int | None = None,
    ) -> tuple[int, int, int, int, int] | None: ...
    def simulate_exact_output_swap_with_override(
        self,
        zero_for_one: bool,
        amount_out: int,
        block: int,
        override_sqrt_price_x96: int,
        override_liquidity: int,
        override_tick: int,
        override_tick_data: dict[int, tuple[int, int, int]],
        sqrt_price_limit_x96: int | None = None,
    ) -> tuple[int, int, int, int, int] | None: ...
    def simulate_exact_output_swap_with_fetch(
        self,
        zero_for_one: bool,
        amount_out: int,
        block: int,
        sqrt_price_limit_x96: int | None = None,
    ) -> tuple[int, int, int, int, int] | None: ...
    def encode_swap(
        self,
        zero_for_one: bool,
        amount_out: int,
        recipient: str,
    ) -> tuple[str, str, int] | None: ...
    def sync_reserves(self, reserve0: int, reserve1: int, block_number: int) -> None: ...
    def apply_swap(
        self,
        sqrt_price_x96: int,
        liquidity: int,
        tick: int,
        block_number: int,
    ) -> bool: ...
    def apply_liquidity_update(
        self,
        tick_lower: int,
        tick_upper: int,
        liquidity_delta: int,
        block_number: int,
    ) -> bool: ...
    def update_tick_data(
        self,
        tick_bitmap: dict[int, Any],
        tick_data: dict[int, tuple[int, int, int]],
        block: int,
    ) -> bool: ...
    def tick_data_snapshot(self) -> dict[int, tuple[int, int, int]]: ...
    def tick_bitmap_snapshot(self) -> dict[int, tuple[int, int]]: ...
    @property
    def n_coins(self) -> int: ...
    @property
    def balances(self) -> list[int]: ...
    def snapshot_curve(self) -> tuple[list[int], int] | None: ...
    def apply_curve_balance_update(self, balances: list[int], block_number: int) -> bool: ...
    def curve_a_ramp(
        self,
    ) -> tuple[int | None, int | None, int | None, int | None, int | None] | None: ...
    def curve_crypto_fees(
        self,
    ) -> tuple[int | None, int | None, int | None, int | None, int | None] | None: ...
    def curve_lp_token(self) -> str | None: ...
    @property
    def curve_use_lending(self) -> list[bool]: ...
    @property
    def curve_precision_multipliers(self) -> list[int]: ...
    @property
    def curve_has_data_provider(self) -> bool: ...
    @property
    def curve_a_coefficient(self) -> int: ...
    @property
    def curve_fee(self) -> int: ...
    @property
    def curve_admin_fee(self) -> int: ...
    @property
    def curve_rate_multipliers(self) -> list[int]: ...
    @property
    def curve_swap_style(self) -> int: ...
    @property
    def curve_lending_rate_style(self) -> int: ...
    @property
    def curve_d_variant(self) -> int: ...
    @property
    def curve_y_variant(self) -> int: ...
    @property
    def curve_yd_variant(self) -> int: ...
    @property
    def curve_metapool_rate_style(self) -> int: ...
    @property
    def curve_metapool_underlying_style(self) -> int: ...
    def curve_base_pool_address(self) -> str | None: ...
    def get_curve_tokens(self) -> list[PyErc20Token] | None: ...
    def get_curve_tokens_underlying(self) -> list[PyErc20Token] | None: ...
    def get_curve_lp_token(self) -> PyErc20Token | None: ...
    def curve_base_pool(self) -> PyLiquidityPool | None: ...
    def fetch_curve_block_number(self) -> int | None: ...
    def fetch_curve_block_timestamp(self, block_number: int) -> int | None: ...
    def fetch_curve_token_balance(
        self, token_address: str, holder_address: str, block_number: int
    ) -> int | None: ...
    def fetch_curve_token_total_supply(
        self, token_address: str, block_number: int
    ) -> int | None: ...
    def fetch_curve_lending_rates(self, block_number: int) -> list[int]: ...
    def fetch_curve_d(self, block_number: int) -> int | None: ...
    def fetch_curve_gamma(self, block_number: int) -> int | None: ...
    def fetch_curve_price_scale(self, block_number: int) -> list[int]: ...
    def fetch_curve_admin_balances(self, block_number: int) -> list[int]: ...
    def fetch_curve_redemption_price(self, block_number: int) -> int | None: ...
    def fetch_curve_base_cache_updated(self, block_number: int) -> int | None: ...
    def fetch_curve_base_virtual_price(self, block_number: int) -> int | None: ...
    def fetch_curve_virtual_price(self, block_number: int) -> int | None: ...
    @property
    def n_balancer_tokens(self) -> int: ...
    @property
    def balancer_balances(self) -> list[int]: ...
    def snapshot_balancer_weighted(self) -> tuple[list[int], int] | None: ...
    def apply_balancer_weighted_balance_update(
        self,
        balances: list[int],
        block_number: int,
    ) -> bool: ...
    @property
    def n_balancer_stable_tokens(self) -> int: ...
    @property
    def balancer_stable_balances(self) -> list[int]: ...
    @property
    def balancer_bpt_index(self) -> int | None: ...
    @property
    def balancer_amp(self) -> int: ...
    @property
    def balancer_invariant_version(self) -> int: ...
    @property
    def balancer_stable_vault(self) -> str: ...
    @property
    def balancer_stable_pool_id_hex(self) -> str: ...
    @property
    def balancer_stable_token_addresses(self) -> list[str]: ...
    def get_balancer_stable_tokens(self) -> list[PyErc20Token] | None: ...
    @property
    def balancer_stable_scaling_factors(self) -> list[int]: ...
    @property
    def balancer_stable_swap_fee(self) -> int: ...
    @property
    def balancer_stable_rate_provider_is_static(self) -> bool: ...
    def fetch_balancer_stable_rates(self, block_identifier: int | None) -> list[int] | None: ...
    def snapshot_balancer_stable(self) -> tuple[list[int], int] | None: ...
    def apply_balancer_stable_balance_update(
        self,
        balances: list[int],
        block_number: int,
    ) -> bool: ...
    def journal_len(self) -> int: ...
    def discard_before_block(self, block: int) -> None: ...
    def restore_before_block(self, block: int) -> tuple[int, int, int] | None: ...
    def discard_v3_before_block(self, block: int) -> None: ...
    def restore_v3_before_block(self, block: int) -> tuple[int, int, int, int] | None: ...

class Erc20TokenRow:
    """A typed `erc20_tokens` DB row (QVMWQC).

    Returned by `PyBotIo.fetch_erc20_token`; mirrors the SQLAlchemy
    `Erc20TokenTable` ORM attributes the builders read.
    """

    @property
    def id(self) -> int: ...
    @property
    def chain(self) -> int: ...
    @property
    def address(self) -> str: ...
    @property
    def name(self) -> str | None: ...
    @property
    def symbol(self) -> str | None: ...
    @property
    def decimals(self) -> int | None: ...

class LiquidityPoolRow:
    """A typed `pools` DB row (QVMWQC)."""

    @property
    def id(self) -> int: ...
    @property
    def address(self) -> str: ...
    @property
    def chain(self) -> int: ...
    @property
    def kind(self) -> str: ...
    @property
    def token0_id(self) -> int: ...
    @property
    def token1_id(self) -> int: ...
    @property
    def exchange_id(self) -> int: ...

class PoolKindRow:
    """A per-DEX subclass row (V2/V3/V4) (QVMWQC)."""

    @property
    def variant(self) -> str: ...
    @property
    def pool_id(self) -> int: ...
    @property
    def fee_token0(self) -> int: ...
    @property
    def fee_token1(self) -> int: ...
    @property
    def fee_denominator(self) -> int: ...
    @property
    def tick_spacing(self) -> int: ...
    @property
    def liquidity_update_block(self) -> int | None: ...
    @property
    def liquidity_update_log_index(self) -> int | None: ...
    @property
    def stable(self) -> bool | None: ...
    @property
    def pool_hash(self) -> str | None: ...
    @property
    def hooks(self) -> str | None: ...
    @property
    def currency0_id(self) -> int | None: ...
    @property
    def currency1_id(self) -> int | None: ...
    @property
    def managed_pool_id(self) -> int | None: ...

class ExchangeRow:
    """A typed `exchanges` DB row (QVMWQC)."""

    @property
    def id(self) -> int: ...
    @property
    def chain_id(self) -> int: ...
    @property
    def name(self) -> str: ...
    @property
    def active(self) -> bool: ...
    @property
    def last_update_block(self) -> int | None: ...
    @property
    def factory(self) -> str: ...
    @property
    def deployer(self) -> str | None: ...

class PoolManagerRow:
    """A typed `pool_managers` DB row (V4) (QVMWQC)."""

    @property
    def id(self) -> int: ...
    @property
    def address(self) -> str: ...
    @property
    def chain(self) -> int: ...
    @property
    def kind(self) -> str: ...
    @property
    def state_view(self) -> str | None: ...
    @property
    def exchange_id(self) -> int: ...

class LiquidityPositionRow:
    """A `liquidity_positions` row (V3 tick liquidity) (QVMWQC)."""

    @property
    def tick(self) -> int: ...
    @property
    def liquidity_net(self) -> int: ...
    @property
    def liquidity_gross(self) -> int: ...

class InitializationMapRow:
    """An `initialization_maps` row (V3 tick bitmap) (QVMWQC)."""

    @property
    def word(self) -> int: ...
    @property
    def bitmap(self) -> int: ...

class LiquidityUpdateEvent:
    """A decoded liquidity-update event record (QJSCA5 §4.3).

    The `(block_number, log_index, tick_lower, tick_upper, liquidity_delta)`
    tuple the Rust apply loop consumes. `liquidity_delta` is the signed delta
    (V3 Burn negated; V4 Modify decoded signed). Built by the Python apply
    shells; the Rust core applies + persists.
    """

    def __init__(
        self,
        block_number: int,
        log_index: int,
        tick_lower: int,
        tick_upper: int,
        liquidity_delta: int,
    ) -> None: ...

class PyBotIo(PoolIO):
    """PyO3 wrapper (exposed as `PyBotIo` in Python) holding a provider + optional DB.

    The Rust I/O facade for pool builders (ADR-005 slice 14a). Builders
    receive this in place of the Python ``SyncPoolIO`` adapter; it delegates
    the 7-method ``PoolIO`` surface (``get_block_number``, ``get_block``,
    ``get_block_timestamp``, ``get_code``, ``get_balance``, ``call``,
    ``call_raw``) to the held ``ProviderAdapter`` so any backend (web3 /
    alloy / offline) works unchanged. The calling convention mirrors
    ``SyncPoolIO`` exactly (positional leading args + ``block=`` kwarg), so
    ``OfflineProvider``-style keyword-only ``call(*, to, data, block_number)``
    backends and ``MagicMock(side_effect=...)`` test doubles designed against
    ``SyncPoolIO``'s kw-call shape aren't broken by the swap.
    """

    def __init__(
        self, provider: object, db: object | None = None, database_path: str | None = None
    ) -> None: ...
    @property
    def provider(self) -> object: ...
    @property
    def db(self) -> object | None: ...
    @property
    def database_path(self) -> str | None: ...
    def fetch_erc20_token(self, chain_id: int, address: str) -> Erc20TokenRow | None: ...
    def update_erc20_token_metadata(
        self,
        chain_id: int,
        address: str,
        name: str | None,
        symbol: str | None,
        decimals: int | None,
    ) -> None: ...
    def fetch_pool_row(self, chain_id: int, address: str) -> LiquidityPoolRow | None: ...
    def fetch_pool_kind(self, kind: str, pool_id: int) -> PoolKindRow | None: ...
    def fetch_token_by_id(self, token_id: int) -> Erc20TokenRow | None: ...
    def fetch_exchange(self, exchange_id: int) -> ExchangeRow | None: ...
    def fetch_liquidity_positions(self, pool_id: int) -> list[LiquidityPositionRow]: ...
    def fetch_initialization_maps(self, pool_id: int) -> list[InitializationMapRow]: ...
    def fetch_pool_manager(self, chain_id: int, address: str) -> PoolManagerRow | None: ...
    def fetch_v4_pool_by_pool_hash(self, pool_hash_hex: str) -> PoolKindRow | None: ...
    def fetch_managed_liquidity_positions(
        self, managed_pool_id: int
    ) -> list[LiquidityPositionRow]: ...
    def fetch_managed_initialization_maps(
        self, managed_pool_id: int
    ) -> list[InitializationMapRow]: ...
    def get_block_number(self) -> int: ...
    def get_block(self, block_identifier: int | str) -> Web3BlockData | None: ...
    def get_block_timestamp(self, block: int | None = None) -> int: ...
    def get_code(self, address: str, block: int | None = None) -> HexBytes: ...
    def get_balance(self, address: str, block: int | None = None) -> int: ...
    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes: ...
    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes: ...

class PyBot:
    """PyO3 wrapper (exposed as `PyBot` in Python) holding `Arc<RwLock<Bot>>`.

    The Polars-style middle layer between the pure-Rust `Bot` and the Python
    `Bot` session. The Python ``Bot.__init__`` constructs one; `PyLiquidityPool`/`PyErc20Token`
    handles share the same `Arc`. Queries/calcs take a read guard on the
    shared `Bot`; mutations take a write guard.
    """

    def __init__(self, chain_id: int = 0) -> None:
        """Construct a ``PyBot`` for ``chain_id`` (ADR-006 D4).

        The default ``chain_id = 0`` keeps the bare ``PyBot()`` test fixtures
        (which only exercise the Rust core) working without a chain invariant.
        """
    def load_snapshot_from_db(self, db_path: str, chain_id: int) -> None:
        """Load V3+V4 DB snapshot into the core `BotState` at construction time.

        Called from Python ``Bot.__init__`` when ``config.database.path`` exists.
        Opens a read-only ``DegenbotDb`` handle from ``db_path`` and calls the
        core ``Bot::load_snapshot_from_db`` — streams V3+V4 pools into the core
        ``SnapshotStore`` via ``stream_liquidity_maps`` (one pool at a time, no
        materialized ``Vec``) + records ``S = min(fetch_newest_update_block(V3), V4)``.
        After this, pool registration auto-seeds from the store via ``take()``;
        the Python builder passes ``tick_data=None, coverage="tracked"``.

        ``None``/cold-start (no pools) is NOT an error — the pump anchors on
        ``first_observed_block`` at resume.
        """
    @property
    def snapshot_seed_block(self) -> int | None:
        """The snapshot seed block `S` (or ``None`` cold-start).

        Python reads this in ``engine_registry.start()`` to stash
        ``_verify_snapshot_block``.
        """

    @snapshot_seed_block.setter
    def snapshot_seed_block(self, block: int | None) -> None:
        """Set the snapshot seed block `S` (non-DB path, 2SM4Y7).

        Called by ``engine_registry.start()`` after ``load_*_from_py`` when a
        non-DB (file/memory) snapshot is supplied — records ``S =
        min(newest_block)`` so the core auto-backfill inside
        ``BlockPump::resume_from_subscribe`` closes the snapshot→WS gap (J3FMDO).
        The DB path's ``load_snapshot_from_db`` already sets `S` itself.
        """

    def register_v2_pool(
        self,
        address: str,
        token0: str,
        token1: str,
        reserve0: int,
        reserve1: int,
        gamma_numer0: int,
        fee_denom0: int,
        gamma_numer1: int,
        fee_denom1: int,
        factory: str,
        update_block: int = 0,
        variant: str = "uniswap-v2",
        stable_swap: bool = False,
        fee_denominator: int | None = None,
    ) -> int: ...
    def update_v2_pool(
        self,
        address: str,
        reserve0: int,
        reserve1: int,
        block_number: int,
    ) -> None: ...
    def calculate_tokens_out(self, pool_id: int, zero_for_one: bool, amount_in: int) -> int: ...
    def calculate_tokens_in(self, pool_id: int, zero_for_one: bool, amount_out: int) -> int: ...
    def pool_count(self) -> int: ...
    def chain_id(self) -> int: ...
    def dispatch_log(
        self,
        address: str,
        topics: list[str],
        data: str,
        block_number: int = 0,
    ) -> None: ...
    def get_pool(self, pool_id: int) -> PyLiquidityPool | None: ...
    def unregister_pool(self, address: str, pool_id: bytes | None = None) -> bool: ...
    def register_v3_pool(
        self,
        address: str,
        token0: str,
        token1: str,
        fee: int,
        tick_spacing: int,
        factory: str,
        sqrt_price_x96: int,
        liquidity: int,
        tick: int,
        tick_data: dict[int, tuple[int, int, int]] | None = None,
        update_block: int = 0,
        coverage: str = "sparse",
        tick_data_fetcher: Callable[[int, int], dict[int, tuple[int, int, int]] | None]
        | None = None,
    ) -> int: ...
    def update_v3_pool(
        self,
        address: str,
        sqrt_price_x96: int,
        liquidity: int,
        tick: int,
        block_number: int,
    ) -> None: ...
    def register_v4_pool(
        self,
        pool_manager: str,
        pool_id_hex: str,
        currency0: str,
        currency1: str,
        fee: int,
        tick_spacing: int,
        hook_flags: int,
        sqrt_price_x96: int,
        liquidity: int,
        tick: int,
        block: int,
        tick_data: dict[int, tuple[int, int, int]] | None = None,
        coverage: str = "tracked",
        tick_data_fetcher: Callable[[int, int], dict[int, tuple[int, int, int]] | None]
        | None = None,
    ) -> int: ...

    # ADR-006 D4 (T3+T4): pump lifecycle + verify plumbing drive the shared
    # PumpState attached when a UniswapArbEngine(py_bot=...) is constructed.
    def subscribe(self, rpc_url: str) -> int: ...
    def resume(self) -> None: ...
    def stop(self) -> None: ...
    def set_verify_rpc_url(self, rpc_url: str) -> None: ...
    def set_verify_state_view(self, state_view_address: str) -> None: ...
    def verify_liquidity_maps(
        self,
        rpc_url: str,
        tick_lens_address: str,
        state_view_address: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v3_liquidity_maps(
        self,
        rpc_url: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v4_liquidity_maps(
        self,
        rpc_url: str,
        state_view_address: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v3_snapshot_seed(
        self,
        address: str,
        rpc_url: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v4_snapshot_seed(
        self,
        pool_manager_address: str,
        pool_id_hex: str,
        rpc_url: str,
        state_view_address: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v3_post_drain_snapshot(
        self,
        address: str,
        rpc_url: str,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v4_post_drain_snapshot(
        self,
        pool_manager_address: str,
        pool_id_hex: str,
        rpc_url: str,
        state_view_address: str,
    ) -> Coroutine[Any, Any, None]: ...
    def register_curve_pool(
        self,
        address: str,
        tokens: list[str],
        a_coefficient: int,
        fee: int,
        admin_fee: int,
        rate_multipliers: list[int],
        balances: list[int],
        update_block: int,
        swap_style: int = 0,
        lending_rate_style: int = 0,
        d_variant: int = 0,
        y_variant: int = 0,
        yd_variant: int = 0,
        base_pool: str | None = None,
        initial_a_coefficient: int | None = None,
        future_a_coefficient: int | None = None,
        initial_a_coefficient_time: int | None = None,
        future_a_coefficient_time: int | None = None,
        create_timestamp: int | None = None,
        fee_gamma: int | None = None,
        mid_fee: int | None = None,
        offpeg_fee_multiplier: int | None = None,
        out_fee: int | None = None,
        gamma: int | None = None,
        lp_token: str | None = None,
        use_lending: list[bool] | None = None,
        precision_multipliers: list[int] | None = None,
        tokens_underlying: list[str] | None = None,
        metapool_rate_style: int = 1,
        metapool_underlying_style: int = 1,
        data_provider: Any = None,  # noqa: ANN401 — pyo3 accepts PyAny
    ) -> int: ...
    def register_balancer_weighted_pool(
        self,
        address: str,
        vault: str,
        pool_id_hex: str,
        tokens: list[str],
        weights: list[int],
        scaling_factors: list[int],
        swap_fee: int,
        pow_version: int,
        balances: list[int],
        update_block: int,
    ) -> int: ...
    def register_balancer_stable_pool(
        self,
        address: str,
        vault: str,
        pool_id_hex: str,
        tokens: list[str],
        amp: int,
        scaling_factors: list[int],
        swap_fee: int,
        bpt_idx: int | None,
        invariant_version: int,
        balances: list[int],
        update_block: int,
        rate_provider: Any = None,  # noqa: ANN401 — pyo3 accepts PyAny
    ) -> int: ...
    def register_aerodrome_pool(
        self,
        address: str,
        token0: str,
        token1: str,
        factory: str,
        variant: str,
        stable: bool,
        fee_numer: int,
        fee_denom: int,
        reserve0: int,
        reserve1: int,
        update_block: int,
    ) -> int: ...
    def v3_journal_len(self, pool_id: int) -> int: ...
    def v3_discard_before_block(self, pool_id: int, block: int) -> None: ...
    def v3_restore_before_block(
        self,
        pool_id: int,
        block: int,
    ) -> tuple[int, int, int, int] | None: ...
    def register_token(
        self,
        address: str,
        name: str,
        symbol: str,
        decimals: int,
        chain_id: int,
    ) -> PyErc20Token: ...
    def get_token(self, address: str) -> PyErc20Token | None: ...
    def encode_swap(
        self,
        pool_id: int,
        zero_for_one: bool,
        amount_out: int,
        recipient: str,
    ) -> tuple[str, str, int] | None: ...
    def v2_journal_len(self, pool_id: int) -> int: ...
    def v2_discard_before_block(self, pool_id: int, block: int) -> None: ...
    def v2_restore_before_block(self, pool_id: int, block: int) -> tuple[int, int, int] | None: ...

class UniswapArbEngine:
    """Rust-side engine for Uniswap arbitrage path solving.

    ADR-006 D1+D4: ``py_bot`` adopts a shared ``PyBot``'s ``BotState`` so the
    engine reads/writes the SAME core that ``PyBot``/``PyLiquidityPool``/
    ``PyErc20Token`` share — dissolving the dual-``BotState`` split (the
    ``rust-owned-bot.md`` §17 stale-state root cause). Omitted → a standalone
    core + fresh ``Bot`` (legacy / no-pyo3 path).
    """

    def __init__(self, py_bot: PyBot | None = None) -> None: ...

    # ── Snapshot loading (single whole-dict crossing; DADWUP retired the
    #    per-pool begin/insert/finish pyo3 surface). ──
    def clear_v3_snapshot(self) -> None: ...
    def clear_v4_snapshot(self) -> None: ...
    def load_v3_snapshot_from_py(self, py_data: dict[str, dict[int, tuple[int, int]]]) -> None: ...
    def load_v4_snapshot_from_py(
        self,
        py_data: dict[str, dict[str, dict[int, tuple[int, int]]]],
    ) -> None: ...
    @property
    def snapshot_seed_block(self) -> int | None:
        """The snapshot seed block `S` (set at `Bot.__init__` by `load_snapshot_from_db`).

        ``None`` = cold-start. `engine_registry.start()` reads this to drive
        the snapshot→WS backfill + stash `_verify_snapshot_block`.
        """

    @snapshot_seed_block.setter
    def snapshot_seed_block(self, block: int | None) -> None:
        """Set the snapshot seed block `S` (non-DB path, 2SM4Y7)."""

    # ── Phase / startup ritual (Plan 102: the canonical two-phase flow). ──
    def subscribe(self, rpc_url: str) -> int: ...
    def resume(self) -> None: ...
    def stop(self) -> None: ...
    def last_processed_block(self) -> int | None: ...
    def set_last_processed_block(self, block: int) -> None: ...

    # ── Solve / pool counts / state sync (test + backfill seams). ──
    def solve_all_paths(self, block_number: int) -> None: ...
    def set_event_buffer_max_age(self, max_age: int | None) -> None: ...
    def flush_event_buffer(self) -> None: ...
    def v2_pool_count(self) -> int: ...
    def v3_pool_count(self) -> int: ...
    def v4_pool_count(self) -> int: ...
    def path_count(self) -> int: ...
    def sync_v3_pool_states(
        self,
        v3_sync_updates: list[tuple[str, int, int, int, dict[int, tuple[int, int]]]],
        block_number: int,
    ) -> None: ...
    def sync_v4_pool_states(
        self,
        v4_sync_updates: list[tuple[str, str, int, int, int, dict[int, tuple[int, int]]]],
        block_number: int,
    ) -> None: ...
    def process_logs(
        self,
        v2_sync_updates: list[tuple[str, int, int]],
        v3_swap_updates: list[tuple[str, int, int, int, list[tuple[int, tuple[int, int]]]]],
        v4_swap_updates: list[tuple[str, str, int, int, int, list[tuple[int, tuple[int, int]]]]],
        block_number: int,
    ) -> None: ...

    # ── Backfill/pump buffer drain (restores what the d65c43f6 bypass dropped). ──
    def apply_buffer_v3(self, pool_address: str) -> None: ...
    def apply_buffer_v4(self, pool_manager: str, pool_id_hex: str) -> None: ...
    def debug_buffer_v3_liquidity_update(
        self,
        pool_address: str,
        tick_lower: int,
        tick_upper: int,
        liquidity_delta: int,
        block_number: int,
    ) -> None: ...
    def debug_v3_buffer_count(self, pool_address: str) -> int: ...
    def debug_v3_tick_data(self, pool_address: str) -> dict[int, tuple[int, int]] | None: ...

    # ── Result channel (path inspection + async result-batch iteration). ──
    def latest_results(
        self,
    ) -> tuple[list[tuple[int, int, int, list[int], list[int]]], int]: ...
    def inspect_path(self, path_id: int) -> dict[str, Any] | None: ...
    def deregister_path(self, path_id: int) -> bool: ...
    def set_profit_thresholds(
        self,
        min_profit: int,
        max_profit: int | None = None,
    ) -> None: ...
    def diag_v2_pool(self, address_hex: str) -> tuple[int, str, str] | None: ...
    def diagnostic_inspect_path(
        self,
        path_id: int,
        rpc_url: str | None = None,
    ) -> dict[str, Any]: ...
    def block_stream(self) -> BlockStream: ...
    def __aiter__(self) -> UniswapArbEngine: ...
    def __anext__(self) -> Coroutine[Any, Any, dict[str, Any]]: ...

    # ── Verify config (consumer-safe: nothing emits before resume). ──
    def set_verify_rpc_url(self, rpc_url: str) -> None: ...
    def set_verify_state_view(self, state_view_address: str) -> None: ...

    # ── Batch liquidity-map verification (reads BotState directly; the path
    # that actually runs verification under the shared-BotState design — the
    # register-gated verify is architecturally orphaned). ──
    def verify_liquidity_maps(
        self,
        rpc_url: str,
        tick_lens_address: str,
        state_view_address: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v3_liquidity_maps(
        self,
        rpc_url: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v4_liquidity_maps(
        self,
        rpc_url: str,
        state_view_address: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v3_pool(
        self,
        address: str,
        rpc_url: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v3_snapshot_seed(
        self,
        address: str,
        rpc_url: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v4_snapshot_seed(
        self,
        pool_manager_address: str,
        pool_id_hex: str,
        rpc_url: str,
        state_view_address: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v3_post_drain_snapshot(
        self,
        address: str,
        rpc_url: str,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v4_post_drain_snapshot(
        self,
        pool_manager_address: str,
        pool_id_hex: str,
        rpc_url: str,
        state_view_address: str,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v4_pool(
        self,
        pool_id_hex: str,
        rpc_url: str,
        state_view_address: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...

    # ── Pool + path registration ──
    def register_path(self, pool_refs: list[tuple[int, bool]]) -> int: ...
    def register_and_solve_path(self, pool_refs: list[tuple[int, bool]]) -> int: ...

class BlockStream:
    """Async iterator over `newHeads` block notifications from the pump.

    The authoritative block clock for the backrun bot (epic 6W35AI) —
    obtained via `UniswapArbEngine.block_stream()`. Yields one dict per
    accepted block header: `number`, `timestamp`, `base_fee_per_gas`
    (int | None), `gas_used`, `gas_limit`. Raises `StopAsyncIteration` when
    the channel closes (pump stopped).
    """

    def __aiter__(self) -> BlockStream: ...
    def __anext__(self) -> Coroutine[Any, Any, dict[str, Any]]: ...

class VerificationMismatchError(RuntimeError):
    """On-chain verification mismatch: engine tick data != on-chain state.

    Raised by the Rust seam (`VerifyError::Snapshot`) when
    ``verify_on_register`` finds the engine's tick data does not match
    on-chain. Fatal at the bot level — do not trade on stale tick data.
    Subclasses ``RuntimeError`` so broad ``except RuntimeError`` handlers
    still catch it; classify by ``isinstance`` for the fatal path.
    """

class VerificationRpcError(RuntimeError):
    """RPC/transport failure during on-chain verification.

    Covers provider construction failure, etc. Raised by the Rust seam
    (`VerifyError::Provider`). The bot could not reach the node to verify —
    also not safe to silently skip (an unverifiable pool is no safer than a
    mismatched one), but a distinct type so the caller can choose
    retry/backoff vs abort. Subclasses ``RuntimeError``.
    """

class PoolRegistrationError(ValueError):
    """Pool registration was refused at admission time.

    The unified base of the F2EVV6 hierarchy — a pool was rejected at
    ``register_vx_pool`` for one of four reasons (duplicate address, out-of-spec
    field, V4 amount-modifying hook, or V4 dynamic fee) and the specific reason
    is conveyed by the subclass:

    - :class:`PoolAlreadyRegisteredError` — V2/V3/V4 duplicate address.
    - :class:`SpecViolationError` — V2/V3/V4 out-of-spec field.
    - :class:`HookedPoolRejectedError` — V4 amount-modifying hook.
    - :class:`DynamicFeePoolRejectedError` — V4 dynamic fee.

    Subclasses ``ValueError`` so broad ``except ValueError:`` handlers (which
    skip one rejected pool at a time in ``build_paths``) keep working; scope
    admission refusals specifically with ``except PoolRegistrationError:``.
    """

class PoolAlreadyRegisteredError(PoolRegistrationError):
    """A pool at this address is already registered.

    Raised by the Rust seam (``RegisterV{2,3}PoolError::AlreadyRegistered``
    / ``RegisterV4PoolError::AlreadyRegistered``) when ``register_vx_pool``
    sees a duplicate address. Replaces the previous ``assert!`` panic on V2
    (MSTAT2) and V3 (24KNGF), and replaces the plain ``ValueError`` that
    ``register_v4_pool`` previously raised for duplicates (F2EVV6). Subclasses
    :class:`PoolRegistrationError` so a broad admission catch covers all four
    admission categories together.
    """

class SpecViolationError(PoolRegistrationError):
    """A field on the pool registration params violates its on-chain bound.

    Raised by the Rust seam when ``register_vx_pool`` sees an out-of-spec
    field — e.g. V2 ``reserve{0,1} > uint112(-1)``, V3/V4 ``sqrtPriceX96``
    outside ``[MIN_SQRT_RATIO, MAX_SQRT_RATIO)``, V3/V4 ``tick`` outside
    ``[MIN_TICK, MAX_TICK]``, V3/V4 ``fee`` above the family bound
    (V3 ``< 1_000_000``; V4 static-fee ``< 1 << 24``), or V3/V4
    ``tickSpacing`` outside ``[1, 32_767]``. The message names the offending
    field, its value, and the bound it violates. Replaces the
    ``PyValueError("Vx pool registration failed: …")`` stop-gap mappers
    (MSTAT2 / 24KNGF / K3IICB) with a typed exception in F2EVV6.
    """

class HookedPoolRejectedError(PoolRegistrationError):
    """A V4 pool with an amount-modifying hook was rejected at registration.

    Raised by the Rust seam (`RegisterV4PoolError::HookedPool`) when
    ``register_v4_pool`` sees ``hook_flags & 0xCC != 0``. The solver's V3-CL
    math assumes no hook intervention, so a hooked pool would produce phantom
    profits — admission is a *correctness floor* (enforced in the Rust core
    so a standalone Rust consumer is protected, per ADR-005). Reparented
    under :class:`PoolRegistrationError` in F2EVV6 (was directly under
    ``ValueError`` in Plan 102); a broad ``except ValueError:`` still catches
    it; classify by ``isinstance``.
    """

class DynamicFeePoolRejectedError(PoolRegistrationError):
    """A V4 pool with a dynamic fee was rejected at registration.

    Raised by the Rust seam (`RegisterV4PoolError::DynamicFee`) when
    ``register_v4_pool`` sees ``fee == 0x100000``. The solver assumes a fixed
    fee, so a dynamic-fee pool cannot be priced. Like
    :class:`HookedPoolRejectedError`, a correctness floor enforced in the
    Rust core (ADR-005) and reparented under :class:`PoolRegistrationError`
    in F2EVV6.
    """

class DatabaseSchemaStale(ValueError):
    """The DB is stamped at a prior Alembic revision.

    Raised by the degenbot-db PyO3 seam (``DbError::AlembicStale``) when a
    connexion is opened against a DB whose ``alembic_version`` predates the
    compiled head — e.g. a user upgrading from the published 0.6.0a2 schema
    (``e0aaad8ad486``) to the dev head (``2606a6c7f5ee``). The Rust core is
    a reader of Alembic-headed DBs, never a migrator, so it refuses with
    this typed exception instead. Subclasses ``ValueError`` so the
    ``database upgrade`` shell's broad catch keeps working; the CLI root
    group catches it to print a friendly one-line "run ``degenbot database
    upgrade``" hint instead of a Python traceback.
    """

class PyTxParams:
    """EIP-1559 transaction field set (minus ``chain_id``, owned by ``PyTxSigner``).

    Constructed with the recipient/calldata/gas/nonce/value/access-list, then
    fees are set by :func:`finalize_fees`, then signed by
    :meth:`PyTxSigner.sign_eip1559`.
    """

    def __init__(
        self,
        to: str,
        data: bytes,
        gas_limit: int,
        nonce: int,
        value: int = 0,
        access_list: list[dict[str, Any]] | None = None,
    ) -> None: ...
    @property
    def max_fee_per_gas(self) -> int: ...
    @property
    def max_priority_fee_per_gas(self) -> int: ...

class PyTxSigner:
    """EIP-1559 transaction signer holding the operator key once.

    The private key (hex string or 32 raw bytes) crosses into Rust at
    construction and never round-trips back to Python per transaction.
    """

    def __init__(self, key: str | bytes, chain_id: int) -> None: ...
    @property
    def address(self) -> str: ...
    @property
    def chain_id(self) -> int: ...
    def sign_eip1559(self, tx_params: PyTxParams) -> bytes: ...
    @staticmethod
    def recover_sender(raw_signed: bytes) -> str: ...

class PyDispatcher:
    """Coordination-state value object for the dispatch loop.

    Holds the Rust-owned ``Dispatcher`` behind ``Arc<Mutex<...>>`` so the
    submit/fee/monitor Rust leaves lock the SAME state Python drives. Construct
    via :meth:`for_block`.
    """

    def __init__(self) -> None: ...
    @staticmethod
    def for_block(current_block: int) -> PyDispatcher: ...
    # ── nonce coordination ──
    def claim_nonce(self, start: int) -> int: ...
    def release_nonce(self, nonce: int) -> None: ...
    # ── pool mutual exclusion ──
    def reserve_pools(self, path_pools: set[str]) -> None: ...
    def release_pools(self, pools: set[str]) -> None: ...
    def is_path_blocked(
        self,
        path_pools: set[str],
        committed_pools: set[str],
    ) -> bool: ...
    def release_tx(self, tx: tuple[int, set[str]]) -> None: ...
    # ── task tracking ──
    def active_task_count(self) -> int: ...
    def reap_finished(self) -> int: ...
    def abort_all_tasks(self) -> None: ...
    # ── block / fee recording ──
    @property
    def current_block(self) -> int: ...
    def advance_block(self, block: int) -> None: ...
    def record_block_time(self, block: int, timestamp: int) -> None: ...
    def record_priority_fees(self, block: int, fees: dict[int, int]) -> None: ...
    def latest_priority_fees(self) -> dict[int, int]: ...
    @property
    def block_priority_fees(self) -> dict[int, dict[int, int]]: ...
    # ── block-time ring ──
    def block_time_count(self) -> int: ...
    def block_times_oldest(self) -> tuple[int, int]: ...
    # ── PathSuppression delegation ──
    def record_success(self, path_id: int) -> None: ...
    def record_failure(self, path_id: int) -> None: ...
    def is_suppressed(self, path_id: int, current_block: int) -> bool: ...
    def total_suppressed(self) -> int: ...
    def discard_path(self, path_id: int) -> None: ...
    # ── introspection ──
    def pending_nonce_count(self) -> int: ...
    def pending_pool_count(self) -> int: ...

class PySubmitCandidate:
    """Per-path builder constructed from ``gas_profitable`` before the submit leaf."""

    def __init__(
        self,
        path_id: int,
        gross_profit: int,
        net_profit: int,
        gas_used: int,
        priority_fee: int,
        base_fee_next: int,
        execute_calldata: bytes,
        executor_address: str,
        access_list: list[dict[str, Any]] | None = None,
        path_pools: set[str] | None = None,
    ) -> None: ...
    # ── read-only getters (the `[dispatch]` per-path log reads these — A5) ──
    @property
    def path_id(self) -> int: ...
    @property
    def gross_profit(self) -> int: ...
    @property
    def net_profit(self) -> int: ...
    @property
    def gas_used(self) -> int: ...
    @property
    def priority_fee(self) -> int: ...

# ------------------------------------------------------------------
# Simulation seam (per-block profitability pipeline)
# ------------------------------------------------------------------
class PySimulateContext:
    """Session-static config bag for :func:`dispatch_profitable_py`.

    Construct once per session from the :class:`AsyncAlloyProvider` + the
    executor/WETH/PoolManager/Multicall3 addresses + the inject flag + the
    executor runtime bytecode. Handed to :func:`dispatch_profitable_py` each
    block alongside the per-block args.
    """

    def __init__(
        self,
        provider: AsyncAlloyProvider,
        executor_owner: str,
        executor_address: str,
        weth_address: str,
        pool_manager_address: str,
        multicall3_address: str,
        inject_code: bool,
        executor_runtime_bytecode: bytes,
        injected_address: str | None = None,
    ) -> None: ...
    @property
    def rpc_url(self) -> str: ...

class PyDispatchCandidate:
    """Pre-simulation candidate builder (engine result + resolved ``PathInfo``)."""

    def __init__(
        self,
        path_id: int,
        optimal_input: int,
        engine_profit: int,
        hop_outputs: list[int],
        solve_block: int,
        path_info: PathInfo,
        *,
        erc6909_profit: bool = False,
        use_v4_batch: bool = False,
    ) -> None: ...

class PyDispatchOutcome:
    """Read-only outcome of a block's profitable-dispatch fan-out."""

    @property
    def gas_profitable(self) -> list[PySubmitCandidate]: ...
    @property
    def gas_unprofitable_count(self) -> int: ...
    @property
    def exception_count(self) -> int: ...
    @property
    def fail_count(self) -> int: ...
    @property
    def candidate_count(self) -> int: ...
    @property
    def suppressed_count(self) -> int: ...
    @property
    def thin_dropped(self) -> int: ...
    @property
    def fail_buckets(self) -> dict[str, int]: ...
    @property
    def failures(self) -> list[dict[str, Any]]: ...
    @property
    def path_infos(self) -> dict[int, PathInfo]: ...

def dispatch_profitable_py(
    candidates: list[PyDispatchCandidate],
    context: PySimulateContext,
    dispatcher: PyDispatcher,
    base_fee_next: int,
    current_block: int,
    min_profit_net: int,
    min_profit_margin_bps: int,
) -> Coroutine[Any, Any, PyDispatchOutcome]: ...
def dispatch_and_submit_py(
    candidates: list[PySubmitCandidate],
    dispatcher: PyDispatcher,
    provider: AsyncAlloyProvider,
    signer: PyTxSigner,
    operator_nonce: int,
    current_block: int,
    dry_run: bool,
    inject_code: bool,
) -> Coroutine[Any, Any, list[dict[str, Any]]]: ...
def fetch_fee_history_py(
    provider: AsyncAlloyProvider,
    dispatcher: PyDispatcher,
    block_count: int,
    last_block: int,
    reward_percentiles: list[float],
) -> Coroutine[Any, Any, bool]: ...
def finalize_fees(
    params: PyTxParams,
    base_fee_next: int,
    priority_fee: int,
) -> None: ...

__all__ = [
    "MAX_SQRT_RATIO",
    "MAX_TICK",
    "MIN_SQRT_RATIO",
    "MIN_TICK",
    "AlloyProvider",
    "AlloySubscription",
    "AnvilFork",
    "AsyncAlloyProvider",
    "AsyncContract",
    "BlockData",
    "BlockStream",
    "CancelHandle",
    "Contract",
    "DatabaseSchemaStale",
    "DynamicFeePoolRejectedError",
    "Erc20TokenRow",
    "ExchangeRow",
    "HookedPoolRejectedError",
    "InitializationMapRow",
    "LiquidityPoolRow",
    "LiquidityPositionRow",
    "LiquidityUpdateEvent",
    "LogData",
    "LogFilter",
    "PoolKindRow",
    "PoolManagerRow",
    "PyBot",
    "PyBotIo",
    "PyDatabasePositionQuery",
    "PyDatabaseSnapshot",
    "PyDispatchCandidate",
    "PyDispatchOutcome",
    "PyDispatcher",
    "PyErc20Token",
    "PyLiquidityPool",
    "PySimulateContext",
    "PySubmitCandidate",
    "PyTxParams",
    "PyTxSigner",
    "TransactionData",
    "TransactionReceiptData",
    "UniswapArbEngine",
    "V2PoolRowInput",
    "V3PoolRowInput",
    "V4PoolRowInput",
    "VerificationMismatchError",
    "VerificationRpcError",
    "abi",
    "balancer_math",
    "build_path_graph",
    "cl_add_delta",
    "cl_apply_liquidity_mapping_update",
    "cl_compute_swap_step_v3",
    "cl_compute_swap_step_v4",
    "cl_div_rounding_up",
    "cl_get_amount0_delta",
    "cl_get_amount1_delta",
    "cl_get_next_sqrt_price_from_amount0_rounding_up",
    "cl_get_next_sqrt_price_from_amount1_rounding_down",
    "cl_get_next_sqrt_price_from_input",
    "cl_get_next_sqrt_price_from_output",
    "cl_get_tick_word_and_bit_position",
    "cl_least_significant_bit",
    "cl_max_usable_tick",
    "cl_min_usable_tick",
    "cl_most_significant_bit",
    "cl_muldiv",
    "cl_muldiv_rounding_up",
    "cl_simple_mul_div",
    "cleanup_zero_balance_positions",
    "compute_simulation_warmup_slots",
    "curve_math",
    "db_apply_asset_collateral_in_emode_changed",
    "db_apply_asset_source_updated",
    "db_apply_collateral_configuration_changed",
    "db_apply_e_mode_category_added",
    "db_apply_emode_asset_category_changed",
    "db_apply_price_oracle_updated",
    "db_apply_reserve_used_as_collateral",
    "db_apply_user_e_mode_set",
    "db_apply_v3_liquidity_updates",
    "db_apply_v4_liquidity_updates",
    "db_backup_database",
    "db_compact_database",
    "db_convert_alembic_to_rust_owned",
    "db_create_new_database",
    "db_decode_reserve_configuration_bitmap",
    "db_fetch_exchange",
    "db_fetch_exchange_by_name",
    "db_fetch_pool_row",
    "db_get_or_create_asset_config",
    "db_get_or_create_collateral_position",
    "db_get_or_create_debt_position",
    "db_get_or_create_e_mode_category",
    "db_get_or_create_erc20_token",
    "db_get_or_create_user",
    "db_get_or_create_user_collateral_config",
    "db_heal_database",
    "db_inspect_schema_state",
    "db_set_exchange_active",
    "db_set_exchange_last_update_block",
    "db_upgrade_database",
    "db_upsert_exchange",
    "db_upsert_pool_manager",
    "db_upsert_v2_pools",
    "db_upsert_v3_pools",
    "db_upsert_v4_pools",
    "decode_return_data",
    "dex_identity",
    "dispatch_and_submit_py",
    "dispatch_profitable_py",
    "encode_cmd_stream",
    "encode_function_call",
    "fetch_fee_history_py",
    "finalize_fees",
    "find_paths_rust",
    "flz_compress",
    "flz_decompress",
    "get_function_selector",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
    "mapping_slot",
    "nested_mapping_slot",
    "pack_config",
    "pack_expected_balance",
    "run_aave_update",
    "run_pool_update",
    "solidly_math",
    "to_checksum_address",
    "v4_input_is_native",
    "v4_output_is_native",
    "verify_all_positions_on_chain",
]
