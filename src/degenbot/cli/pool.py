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
from typing import TYPE_CHECKING, Any

import click
import tqdm
from tqdm.contrib.logging import logging_redirect_tqdm

from degenbot.cli import cli
from degenbot.cli.utils import get_provider_from_config
from degenbot.config import resolve_http_rpc_uri
from degenbot.degenbot_rs import CancelHandle, run_pool_update
from degenbot.logging import logger
from degenbot.provider.block_helpers import get_number_for_block_identifier

if TYPE_CHECKING:
    from degenbot.bot import Bot

# Block tags the `--to-block` option accepts (mirrors the prior Python loop).
# A pure tag (no offset) resolves to `None` -> the Rust core fetches the chain
# tip via `eth_blockNumber` itself (no Python-side RPC round-trip). A tag with
# an offset (`latest:-64`, `safe:128`) is resolved in Python to a concrete
# block number (the Rust core takes `Option<u64>`, not a tag string).
_BLOCK_TAGS: frozenset[str] = frozenset(
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
@click.pass_obj
def pool_update(bot: Bot, chunk_size: int, to_block: str) -> None:
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
        identifier=block_tag,  # type: ignore[arg-type]
        provider=provider,
    )
    return int(resolved) + block_offset
