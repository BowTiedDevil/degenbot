"""CLI commands for Aave V3 market management + position analysis."""

import signal
from collections.abc import Callable
from typing import TYPE_CHECKING, Any, Literal, cast

import click
import tqdm
from eth_typing import ChainId
from sqlalchemy import select
from sqlalchemy.orm import joinedload
from tqdm.contrib.logging import logging_redirect_tqdm

from degenbot._ffi.aave import (
    activate_aave_market,
    deactivate_aave_market,
    run_aave_update,
)
from degenbot._ffi.aave import (
    cleanup_zero_balance_positions as rs_cleanup_zero_balance_positions,
)
from degenbot.aave.analysis.core import UserPositionSummary
from degenbot.aave.analysis.orchestrator import analyze_positions_for_market
from degenbot.aave.deployments import EthereumMainnetAaveV3
from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli import cli
from degenbot.config import resolve_http_rpc_uri
from degenbot.database.models.aave import (
    AaveV3CollateralPosition,
    AaveV3DebtPosition,
    AaveV3Market,
    AaveV3User,
)
from degenbot.database.operations import backup_sqlite_database
from degenbot.exceptions import DegenbotValueError
from degenbot.logging import logger
from degenbot.provider.block_helpers import get_number_for_block_identifier
from degenbot.provider.factory import get_provider_from_config
from degenbot.updater import CancelHandle

if TYPE_CHECKING:
    from eth_typing.evm import BlockParams

# Display limit for ``aave position risk`` output (the sole survivor of the
# former ``constants.py`` — the rest was deleted by the §4.2 writer
# retirement, CZM7TI, which moved every writer path to the Rust core).
POSITION_RISK_DISPLAY_LIMIT = 20


__all__ = [
    "aave",
    "aave_update",
    "activate",
    "activate_ethereum_aave_v3",
    "deactivate",
    "deactivate_mainnet_aave_v3",
    "market",
    "market_show",
    "position",
    "position_risk",
    "position_show",
]


# Block tags the `--to-block` option accepts (mirrors `cli/pool.py` — the
# pool-updater cutover, task QZHNZQ). A pure tag (no offset) resolves to
# `None` -> the Rust core fetches the chain tip via `eth_blockNumber` itself
# (no Python-side RPC round-trip). A tag with an offset (`latest:-64`,
# `safe:128`) is resolved in Python to a concrete block number (the Rust core
# takes `Option<u64>`, not a tag string).
_BlockTag = Literal["latest", "earliest", "pending", "safe", "finalized"]

_BLOCK_TAGS: frozenset[_BlockTag] = frozenset(
    {"latest", "earliest", "pending", "safe", "finalized"},
)


def _resolve_to_block(to_block: str, *, chain_id: int, bot: Bot) -> int | None:
    """Resolve the `--to-block` CLI string to `int | None`.

    Mirrors `cli/pool.py::_resolve_to_block`. `None` means "advance to the chain
    tip" — the Rust core (`run_aave_update`) fetches the tip via
    `eth_blockNumber` itself (no Python-side round-trip). A pure block tag with
    no offset maps to `None`; a tag with an offset or a concrete integer
    resolves to a specific block number via the provider.

    Returns:
        `int` for a concrete block number; `None` to let the Rust core fetch
        the chain tip.

    Raises:
        ValueError: For a malformed tag.

    """
    if to_block.isdigit():
        return int(to_block)

    if ":" in to_block:
        parts = to_block.split(":", 1)
        block_tag, offset = cast("tuple[BlockParams, str]", parts)
        block_offset = int(offset.strip())
    else:
        block_tag = cast("BlockParams", to_block)
        block_offset = 0

    if block_tag not in _BLOCK_TAGS:
        msg = f"Invalid block tag: {block_tag}"
        raise ValueError(msg)

    if block_offset == 0:
        # Pure tag -> let the Rust core resolve the tip.
        return None

    provider = get_provider_from_config(chain_id=chain_id, config=bot.config)
    resolved = get_number_for_block_identifier(
        identifier=block_tag,
        provider=provider,
    )
    return int(resolved) + block_offset


@cli.group
def aave() -> None:
    """Aave commands."""


@aave.group
def activate() -> None:
    """Activate an Aave market.

    Positions for activated markets are included when running `degenbot aave position update`.
    """


@activate.command("ethereum_aave_v3")
@click.pass_obj
def activate_ethereum_aave_v3(bot: Bot, chain_id: ChainId = ChainId.ETH) -> None:
    """Activate Aave V3 on Ethereum mainnet."""
    # MPI6Q3: the ONE-TIME market-activation path (the last ORM writer on the
    # Aave path after the §4.2 retirement, CZM7TI) is now Rust-owned. Python is a
    # driver shell — it threads the chain + the two canonical addresses + the RPC
    # URL; Rust owns the `getMarketId()` RPC, the GHO metadata fetch, the
    # `aave_v3_markets` row, the `POOL_ADDRESS_PROVIDER` contract row, + the GHO
    # `erc20_tokens` / `aave_gho_tokens` rows (all in ONE transaction).
    gho_token_address = get_checksum_address("0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f")
    pool_address_provider = EthereumMainnetAaveV3.pool_address_provider
    rpc_url = resolve_http_rpc_uri(chain_id)

    result = activate_aave_market(
        database_path=str(bot.config.database.path),
        chain_id=chain_id,
        pool_address_provider=pool_address_provider,
        gho_token_address=gho_token_address,
        rpc_url=rpc_url,
    )

    click.echo(f"Activated Aave V3 on Ethereum (chain ID {chain_id}).")
    click.echo(
        f"  Market: {result['market_name']} (id={result['market_id']}, "
        f"created={result['created']}).",
    )


@aave.group
def deactivate() -> None:
    """Deactivate an Aave market.

    Positions for deactivated markets are not included when running `degenbot aave position update`.
    """


@deactivate.command("ethereum_aave_v3")
@click.pass_obj
def deactivate_mainnet_aave_v3(
    bot: Bot,
    chain_id: ChainId = ChainId.ETH,
    market_name: str = "Aave Ethereum Market",
) -> None:
    """Deactivate the Aave V3 Ethereum mainnet market."""
    # MPI6Q3: the deactivation write is Rust-owned. Python resolves the
    # `market_id` via a READ (the `aave_v3_markets` lookup by chain + name),
    # then hands off to the Rust seam which flips `active = False` in ONE
    # transaction.
    with bot.db() as session:
        market = session.scalar(
            select(AaveV3Market).where(
                AaveV3Market.chain_id == chain_id,
                AaveV3Market.name == market_name,
            ),
        )

        if market is None:
            click.echo(f"The database has no entry for Aave V3 on Ethereum (chain ID {chain_id}).")
            return

        if not market.active:
            return

    deactivate_aave_market(
        database_path=str(bot.config.database.path),
        market_id=market.id,
    )

    click.echo(f"Deactivated Aave V3 on {chain_id.name} (chain ID {chain_id}).")


@aave.command(
    "update",
    help="Update positions for active Aave markets.",
)
@click.option(
    "--chunk",
    "chunk_size",
    default=10_000,
    show_default=True,
    help="The maximum number of blocks to process before committing changes to the database.",
    envvar="DEGENBOT_CHUNK_SIZE",
    show_envvar=True,
)
@click.option(
    "--to-block",
    "to_block",
    default="latest:-64",
    show_default=True,
    help=(
        "The last block in the update range. Must be a valid block identifier: "
        "'earliest', 'finalized', 'safe', 'latest', 'pending'. An identifier can be given with an "
        "optional offset, e.g. 'latest:-64' stops 64 blocks before the chain tip, "
        "'safe:128' stops 128 blocks after the last 'safe' block."
    ),
)
@click.option(
    "--verify-chunk/--no-verify-chunk",
    "verify_chunk",
    default=True,
    show_default=True,
    help=(
        "Verify touched positions against on-chain truth pre-commit at each"
        " chunk boundary (scaled-token balance + last_index for touched"
        " users). A divergence rolls back the chunk + does NOT advance"
        " last_update_block."
    ),
    envvar="DEGENBOT_VERIFY_CHUNK",
    show_envvar=True,
)
@click.option(
    "--verify-all/--no-verify-all",
    "verify_all",
    default=False,
    show_default=True,
    help=(
        "Run a pre-commit FULL (market-wide, all-4-check) verification at the"
        " block boundary set by --verify-all-interval AND when the run"
        " completes its last block. A divergence rolls back the chunk + does"
        " NOT advance last_update_block. Off by default (operator opt-in)."
    ),
    envvar="DEGENBOT_VERIFY_ALL",
    show_envvar=True,
)
@click.option(
    "--verify-all-interval",
    "verify_all_interval",
    default=1_000_000,
    show_default=True,
    type=int,
    help=(
        "Block interval for the --verify-all full-verification gate. A chunk"
        " that crosses or lands-on a multiple of this interval triggers a"
        " pre-commit market-wide verify. Ignored unless --verify-all is set."
    ),
    envvar="DEGENBOT_VERIFY_ALL_INTERVAL",
    show_envvar=True,
)
@click.option(
    "--one-chunk",
    "stop_after_one_chunk",
    is_flag=True,
    default=False,
    show_default=True,
    help="Stop processing after the first chunk.",
    envvar="DEGENBOT_ONE_CHUNK",
    show_envvar=True,
)
@click.option(
    "--progress-bar/--no-progress-bar",
    "show_progress",
    default=True,
    show_default=True,
    help="Show progress bars.",
    envvar="DEGENBOT_PROGRESS_BAR",
    show_envvar=True,
)
@click.option(
    "--dry-run",
    "dry_run",
    is_flag=True,
    default=False,
    show_default=True,
    help="Preview changes without committing to the database.",
    envvar="DEGENBOT_DRY_RUN",
    show_envvar=True,
)
@click.option(
    "--backup/--no-backup",
    "enable_backup",
    default=False,
    show_default=True,
    help=(
        "Create a database backup after the completion verification runs"
        " (once per market, at the end of the run)."
    ),
    envvar="DEGENBOT_BACKUP",
    show_envvar=True,
)
@click.pass_obj
def aave_update(
    bot: Bot,
    *,
    chunk_size: int,
    to_block: str,
    verify_chunk: bool,
    verify_all: bool,
    verify_all_interval: int,
    stop_after_one_chunk: bool,
    show_progress: bool,
    dry_run: bool,
    enable_backup: bool,
) -> None:
    """Update positions for active Aave markets.

    Processes blockchain events from the last updated block to the specified block,
    updating all user positions, interest rates, and indices in the database.

    Args:
        bot: Active Bot session for on-chain data access and database queries.
        chunk_size: Maximum number of blocks to process before committing changes.
        to_block: Target block identifier (e.g., 'latest', 'latest:-64', 'finalized:128').
        verify_chunk: If True, verify touched positions pre-commit at chunk
            boundaries.
        verify_all: If True, run a pre-commit FULL verification at
            --verify-all-interval block boundaries + at run completion.
        verify_all_interval: Block interval for the --verify-all full gate.
        stop_after_one_chunk: If True, stop after processing the first chunk.
        show_progress: Toggle display of progress bars.
        dry_run: If True, preview changes without committing to the database.
        enable_backup: If True, create a database backup after the completion
            verification runs.

    Raises:
        DegenbotValueError: If no active Aave markets are found.
        RuntimeError: If the run is cancelled via ``cancel_handle`` (the
            in-flight chunk completed atomically first; committed chunks stay
            durable). Other ``RuntimeError``s + ``ValueError`` propagate
            from the Rust core (DB/RPC/parse failures) + ``_resolve_to_block``
            (a malformed ``--to-block``) — see their docstrings.

    A ``--verify-chunk`` (default) or ``--verify-all`` pre-commit divergence
    rolls back the chunk + surfaces as ``AssertionError`` (raised by the Rust
    core, not this shell): ``last_update_block`` does NOT advance, so the
    next run re-processes the same chunk; committed chunks stay durable.

    """
    # AVS4DR (epic AZGJUN): the per-market chunk loop is delegated to the Rust
    # core (`degenbot._ffi.run_aave_update`). Python is a driver shell —
    # market selection (READ), `--to-block` resolution, SIGINT -> cancel flag,
    # tqdm-on-callback, a per-market report echo, + post-run cleanup/backup
    # hygiene. The chunk loop, the RPC fetches, the decode, the DB writes, +
    # the per-chunk transaction all live in the Rust core.
    #
    # §4.2 retirement (CZM7TI): the Python writer pipeline (`update_aave_market`,
    # `db_*.py`, `event_handlers._process_*`, `transaction_processor`/
    # `operations_parser`/`token_processor`, the parity harness) is DELETED —
    # the Rust writer is proven GREEN to the live chain tip with full
    # verification, so the §4.2 cross-check oracle retired with it. The
    # standalone `aave update-py` legacy command is gone (no Python writer left
    # to drive).
    #
    # Behavior: verification policy is enforced by the Rust core.
    # - `--verify-chunk` (default ON): pre-commit touched-position verify
    #   per chunk (Rust `verify_touched_positions_on_conn`).
    # - `--verify-all` (default OFF, opt-in): pre-commit FULL market-wide
    #   verify (all 4 checks) at `--verify-all-interval` block boundaries +
    #   at run completion (Rust `verify_all_positions_on_conn`). A divergence
    #   rolls back the chunk + does NOT advance `last_update_block`.
    # - With both off (default), `aave update` performs only the per-chunk
    #   touched-user verify — no market-wide on-chain sweep, matching
    #   `pool update`'s default. The old post-run full-market verify (serial
    #   per-position `eth_call`s, O(all positions) RPC) is removed; the
    #   pre-commit `--verify-all` gate supersedes it (stronger: pre-commit +
    #   rolled-back-on-divergence).
    # - `--dry-run`: shell-level preview (skips the Rust call entirely). The
    #   Rust core has no dry-run flag; a full write-preview needs one (RQXEKH).
    # - `--one-chunk` is wired through the Rust core's `max_chunks` loop cap
    #   (stops after committing one chunk).
    # - `last_update_block` stamp: owned by the Rust core (was Python).
    # - Markets with `last_update_block is None` are SKIPPED (the Rust core
    #   raises `NotBootstrapped`); bootstrap the stamp first.
    if stop_after_one_chunk:
        # Restored first-class support: the Rust core's `max_chunks` loop
        # cap (run.rs) stops after committing one chunk. Pre-cutover the
        # Python loop honored this inline; the 0.6.x cutover shell downgraded
        # it to a warning because the Rust core had no per-chunk hook; the
        # `max_chunks` parameter restores it (HQF5NQ-gap).
        pass

    handle = CancelHandle()
    prior_int_handler = signal.getsignal(signal.SIGINT)

    def _on_sigint(*_args: Any) -> None:
        # Cooperative cancel: the Rust loop polls the flag between chunks (NOT
        # mid-chunk) so a SIGINT never breaks chunk atomicity — the in-flight
        # chunk completes (commit OR rollback) before the run returns
        # (migration-guide §3.3 interrupt contract).
        handle.cancel()

    def _make_progress(
        pbar: tqdm.tqdm,
        chunk_counter: list[int],
    ) -> Callable[[dict[str, Any]], None]:
        def _on_progress(progress: dict[str, Any]) -> None:
            chunk_counter[0] += 1
            if not progress["committed"]:
                pbar.set_postfix_str(
                    f"chunk {progress['chunk_start']}-{progress['chunk_end']} rolled back",
                    refresh=True,
                )
                return
            delta = progress["chunk_end"] - progress["chunk_start"] + 1
            pbar.update(delta)
            pbar.set_postfix_str(
                f"+{progress['events_applied']} events (chunk {chunk_counter[0]})",
                refresh=True,
            )

        return _on_progress

    cancelled = False
    signal.signal(signal.SIGINT, _on_sigint)
    try:  # noqa: PLR1702 — market loop nests under try/with/for/with/for/try/with
        with logging_redirect_tqdm(loggers=[logger]):
            # Active chains (READ — orchestration only; the Rust core owns the
            # per-market write transaction).
            with bot.db() as session:
                active_chains = set(
                    session.scalars(
                        select(AaveV3Market.chain_id).where(
                            AaveV3Market.active,
                            AaveV3Market.name.contains("aave"),
                        ),
                    ).all(),
                )

            if not active_chains:
                msg = "No active Aave markets found."
                raise DegenbotValueError(message=msg)

            database_path = str(bot.config.database.path)

            for chain_id in active_chains:
                if handle.is_cancelled():
                    break
                rpc_url = resolve_http_rpc_uri(chain_id, config=bot.config)
                resolved_to_block = _resolve_to_block(
                    to_block,
                    chain_id=chain_id,
                    bot=bot,
                )

                # Active markets for this chain (READ).
                with bot.db() as session:
                    active_markets = session.scalars(
                        select(AaveV3Market).where(
                            AaveV3Market.active,
                            AaveV3Market.chain_id == chain_id,
                            AaveV3Market.name.contains("aave"),
                        ),
                    ).all()

                if not active_markets:
                    click.echo(f"No active Aave markets on chain {chain_id}.")
                    continue

                for market in active_markets:
                    if handle.is_cancelled():
                        cancelled = True
                        break
                    if market.last_update_block is None:
                        click.echo(
                            f"Chain {chain_id} market {market.id} ({market.name}): "
                            "needs bootstrapping (last_update_block is None); "
                            "skipping. Bootstrap the stamp before running.",
                        )
                        continue

                    if dry_run:
                        # Shell-level dry-run: skip the Rust call entirely (the
                        # core has no dry-run flag — tracked for RQXEKH). This
                        # is a shell-level preview, NOT a full write-preview.
                        click.echo(
                            f"Dry run: would advance chain {chain_id} market "
                            f"{market.id} ({market.name}) from block "
                            f"{market.last_update_block} to "
                            f"{resolved_to_block!r} (no changes committed).",
                        )
                        continue

                    pbar = tqdm.tqdm(
                        desc=f"Market {market.id} ({market.name})",
                        total=None,
                        bar_format="{desc}: {n_fmt} blocks |{bar}| {postfix}",
                        leave=False,
                        disable=not show_progress,
                    )
                    chunk_counter = [0]
                    try:
                        report = run_aave_update(
                            database_path=database_path,
                            chain_id=chain_id,
                            market_id=market.id,
                            to_block=resolved_to_block,
                            chunk_size=chunk_size,
                            rpc_url=rpc_url,
                            progress_callback=_make_progress(
                                pbar,
                                chunk_counter,
                            ),
                            cancel_handle=handle,
                            verify_chunk=verify_chunk,
                            max_chunks=1 if stop_after_one_chunk else None,
                            verify_all_interval=(verify_all_interval if verify_all else None),
                            verify_all_at_completion=verify_all,
                        )
                    except RuntimeError as exc:
                        # Cooperative cancel (RuntimeError per the .pyi):
                        # a user SIGINT. The in-flight chunk completed
                        # atomically first; committed chunks stay durable.
                        # NOTE: a `--verify-chunk` divergence is raised as an
                        # AssertionError (by the Rust core), NOT a
                        # RuntimeError — the chunk is rolled back, so
                        # `last_update_block` did NOT advance.
                        if "cancel" in str(exc).lower():
                            cancelled = True
                            click.echo(
                                f"Chain {chain_id} market {market.id}: "
                                "cancelled (committed chunks stay durable).",
                            )
                            break
                        raise
                    finally:
                        pbar.close()

                    click.echo(
                        f"Chain {chain_id} market {market.id} ({market.name}): "
                        f"advanced {report['from_block']}->{report['to_block']} "
                        f"in {report['chunks_committed']} chunks "
                        f"({report['total_events_applied']} events applied).",
                    )

                    # Post-run hygiene. The market-wide on-chain verify is owned by
                    # the Rust core's pre-commit gate (`--verify-all` drives
                    # `verify_all_at_completion`): it runs BEFORE the chunk
                    # commits + rolls back on divergence — strictly stronger
                    # than a post-commit re-read. With `--verify-all` off (the
                    # default), `aave update` performs only the per-chunk
                    # touched-user verify (matching `pool update`'s default —
                    # no market-wide verify, fast). Cleanup + backup run
                    # unconditionally / opt-in regardless.
                    rs_cleanup_zero_balance_positions(
                        database_path=database_path,
                        market_id=market.id,
                    )

                    if enable_backup:
                        with bot.db() as session:
                            backup_sqlite_database(
                                session=session,
                                suffix=f"{report['to_block']}",
                                skip_confirmation=True,
                            )
                        logger.info(
                            f"Created database backup at block {report['to_block']:,}.",
                        )

                if cancelled:
                    break
    finally:
        signal.signal(signal.SIGINT, prior_int_handler)


@aave.group()
def position() -> None:
    """Position commands."""


@position.command("show")
@click.pass_obj
@click.argument("address", type=str)
@click.option(
    "--market",
    type=str,
    default="Aave Ethereum Market",
    show_default=True,
    help="Market name to query (default: Aave Ethereum Market).",
)
@click.option(
    "--chain-id",
    type=int,
    default=1,
    show_default=True,
    help="Chain ID to query (default: 1 for Ethereum mainnet).",
)
def position_show(bot: Bot, address: str, market: str, chain_id: int) -> None:
    """Display current Aave positions for a user.

    Shows collateral and debt positions for the specified address on the given market.

    Raises:
        Abort: See function documentation.

    """
    try:
        user_address = get_checksum_address(address)
    except Exception as exc:
        click.echo(f"Invalid address: {address}")
        raise click.Abort from exc

    with bot.db() as session:
        # Find the market first
        market_obj = session.scalar(
            select(AaveV3Market).where(
                AaveV3Market.name == market,
                AaveV3Market.chain_id == chain_id,
            ),
        )

        if market_obj is None:
            click.echo(f"No market found with name '{market}' on chain {chain_id}.")
            return

        # Find the user for this specific market
        user = session.scalar(
            select(AaveV3User).where(
                AaveV3User.address == user_address,
                AaveV3User.market_id == market_obj.id,
            ),
        )

        if user is None:
            click.echo(
                f"No Aave user found for address {user_address} in market '{market}' "
                f"on chain {chain_id}.",
            )
            return

        # Get collateral positions
        collateral_positions = session.scalars(
            select(AaveV3CollateralPosition)
            .where(AaveV3CollateralPosition.user_id == user.id)
            .options(joinedload(AaveV3CollateralPosition.asset)),
        ).all()

        # Get debt positions
        debt_positions = session.scalars(
            select(AaveV3DebtPosition)
            .where(AaveV3DebtPosition.user_id == user.id)
            .options(joinedload(AaveV3DebtPosition.asset)),
        ).all()

        click.echo(f"\nAave V3 Positions for {user_address}")
        click.echo(f"Market: {market} (Chain: {chain_id})")
        click.echo("=" * 60)

        # Display collateral positions
        if collateral_positions:
            click.echo("\nCollateral Positions:")
            click.echo("-" * 60)
            for collateral_pos in collateral_positions:
                asset_symbol = (
                    collateral_pos.asset.underlying_token.symbol
                    if collateral_pos.asset.underlying_token
                    else "Unknown"
                )
                click.echo(f"  {asset_symbol}: {collateral_pos.balance} (scaled)")
        else:
            click.echo("\nNo collateral positions found.")

        # Display debt positions
        if debt_positions:
            click.echo("\nDebt Positions:")
            click.echo("-" * 60)
            for debt_pos in debt_positions:
                asset_symbol = (
                    debt_pos.asset.underlying_token.symbol
                    if debt_pos.asset.underlying_token
                    else "Unknown"
                )
                click.echo(f"  {asset_symbol}: {debt_pos.balance} (scaled)")
        else:
            click.echo("\nNo debt positions found.")

        click.echo()


@position.command("risk")
@click.pass_obj
@click.option(
    "--market",
    type=str,
    default="Aave Ethereum Market",
    show_default=True,
    help="Market name to analyze (default: Aave Ethereum Market).",
)
@click.option(
    "--chain-id",
    type=int,
    default=1,
    show_default=True,
    help="Chain ID (default: 1 for Ethereum mainnet).",
)
@click.option(
    "--threshold",
    type=float,
    default=1.1,
    show_default=True,
    help="Health factor threshold for 'at risk' classification.",
)
@click.option(
    "--limit",
    type=int,
    default=None,
    help="Maximum number of users to analyze (for testing).",
)
@click.option(
    "--show-positions",
    is_flag=True,
    default=False,
    help="Show detailed position information for at-risk users.",
)
@click.option(
    "--skip-prices",
    is_flag=True,
    default=False,
    help="Skip fetching prices from oracle (faster, but HF values are relative).",
)
def position_risk(  # noqa: PLR0917
    bot: Bot,
    market: str,
    chain_id: int,
    threshold: float,
    limit: int | None,
    show_positions: bool,  # noqa: FBT001
    skip_prices: bool,  # noqa: FBT001
) -> None:
    """Analyze positions for liquidation risk.

    Identifies users with low health factors who are at risk of liquidation.
    Users are categorized as:
    - Liquidatable: Health factor < 1.0
    - At risk: Health factor < threshold (default 1.1)
    - Safe: Health factor >= threshold

    By default, prices are fetched from the Aave oracle for accurate health
    factor calculations. Use --skip-prices for faster analysis when you only
    need relative risk comparisons.
    """
    # Get provider for price fetching
    provider = None if skip_prices else get_provider_from_config(chain_id=chain_id)

    with bot.db() as session:
        # Find the market
        market_obj = session.scalar(
            select(AaveV3Market).where(
                AaveV3Market.name == market,
                AaveV3Market.chain_id == chain_id,
            ),
        )

        if market_obj is None:
            click.echo(f"No market found with name '{market}' on chain {chain_id}.")
            return

        click.echo(f"\nAnalyzing positions for market: {market} (Chain: {chain_id})")
        click.echo(f"Health factor threshold: {threshold}")
        click.echo("=" * 60)

        # Analyze positions
        result = analyze_positions_for_market(
            session=session,
            market_id=market_obj.id,
            health_factor_threshold=threshold,
            limit=limit,
            provider=provider,
        )

        # Display summary
        click.echo("\nAnalysis Summary:")
        click.echo(f"  Total users with debt: {result.total_users}")
        click.echo(f"  Safe users:            {len(result.safe_users)}")
        click.echo(f"  At risk (HF < {threshold}):  {result.at_risk_count}")
        click.echo(f"  Liquidatable (HF < 1): {result.liquidatable_count}")

        # Display liquidatable users
        if result.liquidatable_users:
            click.echo(f"\n{'=' * 60}")
            click.echo("LIQUIDATABLE POSITIONS (HF < 1.0)")
            click.echo("=" * 60)
            for user_summary in result.liquidatable_users[:POSITION_RISK_DISPLAY_LIMIT]:
                _display_user_risk(user_summary, show_positions=show_positions)

            if len(result.liquidatable_users) > POSITION_RISK_DISPLAY_LIMIT:
                remaining = len(result.liquidatable_users) - POSITION_RISK_DISPLAY_LIMIT
                click.echo(f"  ... and {remaining} more")

        # Display at-risk users
        if result.at_risk_users:
            click.echo(f"\n{'=' * 60}")
            click.echo(f"AT-RISK POSITIONS (HF < {threshold})")
            click.echo("=" * 60)
            for user_summary in result.at_risk_users[:POSITION_RISK_DISPLAY_LIMIT]:
                _display_user_risk(user_summary, show_positions=show_positions)

            if len(result.at_risk_users) > POSITION_RISK_DISPLAY_LIMIT:
                remaining = len(result.at_risk_users) - POSITION_RISK_DISPLAY_LIMIT
                click.echo(f"  ... and {remaining} more")


def _display_user_risk(
    user_summary: UserPositionSummary,
    *,
    show_positions: bool,
) -> None:
    """Display risk information for a single user."""
    hf_str = f"{user_summary.health_factor:.4f}" if user_summary.health_factor else "N/A"
    ltv_str = f"{user_summary.max_ltv_ratio:.2%}" if user_summary.max_ltv_ratio else "N/A"

    click.echo(f"\n  User: {user_summary.user_address}")
    click.echo(f"    Health Factor: {hf_str}")
    click.echo(f"    Max LTV Ratio: {ltv_str}")

    if user_summary.emode_category_id:
        click.echo(f"    eMode Category: {user_summary.emode_category_id}")
    if user_summary.is_isolation_mode:
        click.echo("    Isolation Mode: Yes")

    if show_positions:
        if user_summary.collateral_positions:
            click.echo("    Collateral:")
            for collateral_pos in user_summary.collateral_positions:
                if collateral_pos.actual_balance > 0:
                    enabled_str = "" if collateral_pos.is_enabled_as_collateral else " (disabled)"
                    emode_str = " [eMode]" if collateral_pos.in_emode else ""
                    click.echo(
                        f"      {collateral_pos.asset_symbol}: {collateral_pos.actual_balance:,} "
                        f"(LT: {collateral_pos.liquidation_threshold / 100:.0f}%)"
                        f"{enabled_str}{emode_str}",
                    )

        if user_summary.debt_positions:
            click.echo("    Debt:")
            for debt_pos in user_summary.debt_positions:
                if debt_pos.actual_balance > 0:
                    emode_str = " [eMode]" if debt_pos.in_emode else ""
                    click.echo(
                        f"      {debt_pos.asset_symbol}: {debt_pos.actual_balance:,}{emode_str}",
                    )


@aave.group()
def market() -> None:
    """Market commands."""


@market.command("show")
@click.pass_obj
@click.option(
    "--chain-id",
    type=int,
    default=None,
    show_default=True,
    help="Filter by chain ID (default: show all chains).",
)
@click.option(
    "--name",
    type=str,
    default=None,
    help="Filter by market name (default: show all markets).",
)
def market_show(bot: Bot, chain_id: int | None, name: str | None) -> None:
    """Display Aave market information.

    Shows all markets or filters by chain ID and/or market name.
    """
    with bot.db() as session:
        query = select(AaveV3Market)

        if chain_id is not None:
            query = query.where(AaveV3Market.chain_id == chain_id)
        if name is not None:
            query = query.where(AaveV3Market.name == name)

        markets = session.scalars(query).all()

        if not markets:
            filters = []
            if chain_id is not None:
                filters.append(f"chain_id={chain_id}")
            if name is not None:
                filters.append(f"name='{name}'")
            filter_str = ", ".join(filters) if filters else "no filters"
            click.echo(f"No Aave markets found ({filter_str}).")
            return

        click.echo("\nAave V3 Markets")
        click.echo("=" * 80)

        for market_obj in markets:
            status = "active" if market_obj.active else "inactive"
            last_update = (
                f"{market_obj.last_update_block:,}"
                if market_obj.last_update_block is not None
                else "never"
            )

            click.echo(f"\nMarket: {market_obj.name}")
            click.echo(f"  Chain ID: {market_obj.chain_id}")
            click.echo(f"  Status: {status}")
            click.echo(f"  Last Update Block: {last_update}")

            # Count users and assets
            user_count = len(market_obj.users) if market_obj.users else 0
            asset_count = len(market_obj.assets) if market_obj.assets else 0

            click.echo(f"  Users: {user_count}")
            click.echo(f"  Assets: {asset_count}")

            if market_obj.assets:
                click.echo("  Asset List:")
                for asset in market_obj.assets:
                    token_symbol = (
                        asset.underlying_token.symbol if asset.underlying_token else "Unknown"
                    )
                    click.echo(f"    - {token_symbol}")
