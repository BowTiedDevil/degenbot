"""CLI commands for pool state queries.

Task `JJ232N` (epic `2SFL6I`): `pool_update` is now a thin boot + hand-off
to the Rust-owned chunk loop (`degenbot_rs.run_pool_update`, Task
`QZHNZQ`). Python is a driver shell -- config bootstrap, SIGINT -> cancel
flag, tqdm-on-callback, + a user-facing summary. The chunk loop, the RPC
fetches, the decode, the DB writes, + the per-chunk transaction all live in
the Rust core (`degenbot-pool-updater`, Task `CKXCOB`). The SQLAlchemy
session-for-writes, the per-call `db_*` dispatch, the
`fresh_last_update_block` re-read workaround, + the per-event Python tqdm
iteration are all retired (migration-guide `pool-updater-chunk-atomicity`
section 3.1 + section 4 Task 5).

The standalone PyO3 seams (`db_apply_v*_liquidity_updates`,
`db_upsert_v*_pools`, etc.) stay in `degenbot_rs` for ad-hoc/test uses
(`tests/rust/test_discovery_seam.py` exercises the `update_v*_pools`
shells in `pool_updater_configs.py`); only the chunk-loop *usages* in this
file are gone.
"""

from __future__ import annotations

import signal
from typing import TYPE_CHECKING, Any, Literal, cast

import click
import tqdm
from tqdm.contrib.logging import logging_redirect_tqdm

from degenbot.cli import cli
from degenbot.cli.utils import get_provider_from_config
from degenbot.config import resolve_http_rpc_uri
from degenbot.degenbot_rs import (
    CancelHandle,
    run_pool_update,
    verify_v3_liquidity_map,
    verify_v4_liquidity_map,
)
from degenbot.logging import logger
from degenbot.provider.block_helpers import get_number_for_block_identifier

if TYPE_CHECKING:
    from degenbot.bot import Bot

# Block tags the `--to-block` option accepts (mirrors the prior Python loop).
# A pure tag (no offset) resolves to `None` -> the Rust core fetches the chain
# tip via `eth_blockNumber` itself (no Python-side RPC round-trip). A tag with
# an offset (`latest:-64`, `safe:128`) is resolved in Python to a concrete
# block number (the Rust core takes `Option<u64>`, not a tag string).
_BlockTag = Literal["latest", "earliest", "pending", "safe", "finalized"]

_BLOCK_TAGS: frozenset[_BlockTag] = frozenset(
    {"latest", "earliest", "pending", "safe", "finalized"},
)


@cli.group()
def pool() -> None:
    """Pool commands."""


@pool.command("update")
@click.option(
    "--chunk",
    "chunk_size",
    default=10_000,
    show_default=True,
    help="The maximum number of blocks to process before committing changes to the database.",
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
    "--verify/--no-verify",
    "verify",
    default=False,
    show_default=True,
    help=(
        "Run the pre-commit on-chain-truth gate (Full per-pool per-chunk "
        "verification) before each chunk's liquidity persist commits. A "
        "divergence rolls back the chunk + does NOT advance "
        "last_update_block (the corrupt state never lands). Catches the "
        "6SRJRL ghost-row class + other apply-math corruption before write."
    ),
)
@click.pass_obj
def pool_update(bot: Bot, chunk_size: int, to_block: str, verify: bool) -> None:  # noqa: FBT001
    """Update liquidity pool information for activated exchanges.

    Boot + hand-off: read the bot config, install a SIGINT -> cancel-flag
    handler, build a tqdm-ticking progress callback, + delegate the whole
    chunk loop to the Rust core (`degenbot_rs.run_pool_update`). The core
    owns the RPC fetches, the decode, the per-chunk transaction (atomicity),
    + the `last_update_block` stamp (restart-invariance). The GIL is
    released across the whole run; only the progress callback re-acquires
    it briefly, once per chunk.

    Raises:
        ValueError: For a malformed `--to-block` or an RPC/DB failure (the
            in-flight chunk is rolled back before returning; committed
            chunks stay durable).

    """
    chain_id = bot.config.default_chain_id
    if chain_id is None:
        msg = (
            "Bot requires a default_chain_id in the config. Set "
            "`default_chain_id` in your config file or pass a config with it set."
        )
        raise ValueError(msg)

    database_path = str(bot.config.database.path)
    rpc_url = resolve_http_rpc_uri(chain_id, config=bot.config)
    resolved_to_block = _resolve_to_block(to_block, chain_id=chain_id, bot=bot)

    handle = CancelHandle()
    prior_int_handler = signal.getsignal(signal.SIGINT)
    n_chunks = 0

    def _on_sigint(*_args: Any) -> None:
        # Cooperative cancel: the Rust loop polls the flag between chunks (NOT
        # mid-chunk) so a SIGINT never breaks chunk atomicity -- the in-flight
        # chunk completes (commit OR rollback) before the run returns
        # (migration-guide section 3.3 interrupt contract).
        handle.cancel()

    def _on_progress(progress: dict[str, Any]) -> None:
        # tqdm ticks once per chunk (not per event) -- the Rust core reports the
        # chunk boundary; this closure updates the bar's position + postfix.
        nonlocal n_chunks
        n_chunks += 1
        if not progress["committed"]:
            # A rolled-back chunk: don't advance the bar (the next run
            # re-processes it); show the skip in the postfix so the user sees it.
            pbar.set_postfix_str(
                f"chunk {progress['chunk_start']}-{progress['chunk_end']} rolled back",
                refresh=True,
            )
            return
        delta = progress["chunk_end"] - progress["chunk_start"] + 1
        pbar.update(delta)
        pbar.set_postfix_str(
            f"+{progress['pools_written']} pools, "
            f"{progress['liquidity_apply_count']} liq applies "
            f"(chunk {n_chunks})",
            refresh=True,
        )

    total = None  # indeterminate: the core resolves the tip; shows a per-chunk
    # postfix (pools, liquidity applies, chunk number), not a %.
    pbar = tqdm.tqdm(
        desc="Processing new blocks",
        total=total,
        bar_format="{desc}: {n_fmt} blocks |{bar}| {postfix}",
        leave=False,
    )

    signal.signal(signal.SIGINT, _on_sigint)
    try:
        with logging_redirect_tqdm(loggers=[logger]):
            report = run_pool_update(
                database_path=database_path,
                chain_id=chain_id,
                to_block=resolved_to_block,
                chunk_size=chunk_size,
                rpc_url=rpc_url,
                progress_callback=_on_progress,
                cancel_handle=handle,
                verify=verify,
            )
    finally:
        # Restore the prior SIGINT handler (or a default- disposition if there
        # wasn't one) so a subsequent Ctrl+C in the same shell behaves normally.
        signal.signal(signal.SIGINT, prior_int_handler)
        pbar.close()

    click.echo(
        f"Chain {report['chain_id']}: advanced {report['from_block']}->"
        f"{report['to_block']} in {report['chunks_committed']} chunks "
        f"({report['total_pools_written']} pools written, "
        f"{report['total_liquidity_applies']} liquidity applies).",
    )


def _resolve_to_block(to_block: str, *, chain_id: int, bot: Bot) -> int | None:
    """Resolve the `--to-block` CLI string to `int | None`.

    `None` means "advance to the chain tip" -- the Rust core fetches the tip
    via `eth_blockNumber` itself (no Python-side round-trip). A pure block tag
    (`latest`, `safe`, ...) with no offset maps to `None`; a tag with an offset
    (`latest:-64`, `safe:128`) or a concrete integer resolves to a specific
    block number via the provider.

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
        block_tag, offset = parts[0], parts[1]
        block_offset = int(offset.strip())
    else:
        block_tag = to_block
        block_offset = 0

    if block_tag not in _BLOCK_TAGS:
        msg = f"Invalid block tag: {block_tag}"
        raise ValueError(msg)

    if block_offset == 0:
        # Pure tag -> let the Rust core resolve the tip.
        return None

    # Tag + offset -> resolve to a concrete block number in Python (the core
    # takes `Option<u64>`, not a tag string).
    provider = get_provider_from_config(chain_id=chain_id, config=bot.config)
    resolved = get_number_for_block_identifier(
        identifier=cast("_BlockTag", block_tag),
        provider=provider,
    )
    return int(resolved) + block_offset


@pool.command("verify")
@click.option(
    "--rpc-url",
    "rpc_url",
    required=True,
    help="The HTTP RPC endpoint to read on-chain truth from.",
)
@click.option(
    "--chain",
    "chain_id",
    type=int,
    required=True,
    help="The chain id the pool lives on.",
)
@click.option(
    "--block",
    "block_number",
    required=True,
    type=int,
    help="The block number to read on-chain truth at.",
)
@click.option(
    "--pool",
    "pool",
    required=True,
    help=(
        "The pool to verify. V3: the pool contract address. "
        "V4: the PoolId (pool_hash, bytes32 hex 0x…)."
    ),
)
@click.option(
    "--family",
    "family",
    type=click.Choice(["v3", "v4"]),
    required=True,
    help="The pool family (selects ticks()/tickBitmap() vs PoolManager extsload).",
)
@click.option(
    "--pool-manager",
    "pool_manager",
    default=None,
    help=(
        "(V4 only) The deployed V4 PoolManager singleton address (the V4 "
        "exchange's factory). Required for --family v4."
    ),
)
@click.pass_obj
def pool_verify(  # noqa: PLR0917
    bot: Bot,
    rpc_url: str,
    chain_id: int,
    block_number: int,
    pool: str,
    family: str,
    pool_manager: str | None,
) -> None:
    """Verify a pool's committed liquidity map against on-chain truth.

    Stand-alone read-only check (no event apply, no SQL writes): opens the DB,
    fetches the pool's committed tick + bitmap rows, and compares the FULL
    map against on-chain truth (V3: ``ticks()``/``tickBitmap()``; V4:
    ``PoolManager.extsload``) at ``--block``. Prints GREEN if the DB matches
    the chain, else the named divergence list (bisect-able triage).

    This is the ad-hoc / spot-check sibling of the pre-commit gate
    (``pool update --verify``); the gate runs the SAME compare before the
    write commits, while this reads the already-committed state.

    Raises:
        ValueError: For a DB or RPC failure, or a missing V4 pool-manager.

    """
    database_path = str(bot.config.database.path)
    if family == "v3":
        divergences = verify_v3_liquidity_map(
            database_path=database_path,
            rpc_url=rpc_url,
            chain_id=chain_id,
            pool_address=pool,
            block_number=block_number,
        )
    else:
        if pool_manager is None:
            msg = "--pool-manager is required for --family v4 (the PoolManager singleton)."
            raise ValueError(msg)
        divergences = verify_v4_liquidity_map(
            database_path=database_path,
            rpc_url=rpc_url,
            chain_id=chain_id,
            pool_hash=pool,
            pool_manager_address=pool_manager,
            block_number=block_number,
        )
    if not divergences:
        click.echo(f"GREEN: {family} pool {pool} matches on-chain truth at block {block_number}.")
        return
    click.echo(
        f"RED: {len(divergences)} divergence(s) for {family} pool {pool} at block {block_number}:"
    )
    for d in divergences:
        variant = d["variant"]
        if variant in {"TickGross", "TickNet"}:
            click.echo(
                f"  tick {d['tick']}: {variant} expected={d['expected']} actual={d['actual']}"
            )
        elif variant == "BitmapWord":
            click.echo(
                f"  word {d['word']}: {variant} expected={d['expected']} actual={d['actual']}"
            )
        elif variant == "TickCallReverted":
            click.echo(f"  tick {d['tick']}: {variant}")
        elif variant == "BitmapCallReverted":
            click.echo(f"  word {d['word']}: {variant}")
