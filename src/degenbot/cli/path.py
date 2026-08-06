"""CLI commands to steer a LIVE bot over the operator command channel (NWTUM3).

``degenbot path add`` enqueues one specific path into the running bot's
registration pipeline; ``degenbot path discover`` triggers a bounded on-demand
discovery sweep. Both talk to the bot's `OperatorServer` Unix domain socket — a
separate process from the CLI — over the JSON-lines wire protocol in
:mod:`degenbot.operator.operator_channel`. No local pool/db work happens here:
the client is a thin :func:`send_command` shell.

The target bot must be running with ``--operator-socket <path>``. The CLI
connects to that path. The `degenbot` root group loads the normal bot config on
every invocation, so a config file is still required even though the path
commands themselves only need the socket path.

Hop spelling (repeatable ``--hop``): ``FAMILY:ADDRESS`` for V2/V3, and
``V4:ADDRESS:HASH`` (``HASH`` = the 0x+64 pool id) for V4. ``FAMILY`` is
case-insensitive.
"""

from __future__ import annotations

import asyncio

import click

from degenbot.cli import cli
from degenbot.operator.operator_channel import send_command

#: Accepted hop families (case-insensitive on the wire, uppercased here).
_FAMILIES = frozenset({"V2", "V3", "V4"})
#: Index of ADDRESS within a ``FAMILY:ADDRESS`` ``--hop`` split.
_INDEX_ADDRESS = 1
#: Minimum ``:``-split parts to hold a FAMILY + ADDRESS.
_MIN_PARTS = 2
#: Minimum split parts to also carry a V4 pool HASH.
_HASH_PARTS = 3


def _parse_hop(hop: str) -> dict[str, str]:
    """Split one ``--hop FAMILY:ADDRESS[:HASH]`` token into wire step fields.

    Args:
        hop: A ``FAMILY:ADDRESS[:HASH]`` string with case-insensitive FAMILY.

    Returns:
        A wire ``steps`` entry: ``{"family": "V2|V3|V4", "address": ...,}``
        plus ``"hash"`` when the hop carries a V4 pool id.

    Raises:
        click.UsageError: if the hop is malformed or the family is unknown.

    """
    parts = hop.split(":")
    family = parts[0].upper()
    if family not in _FAMILIES:
        msg = f"--hop family must be V2|V3|V4, got {parts[0]!r}"
        raise click.UsageError(msg)
    if len(parts) < _MIN_PARTS or not parts[_INDEX_ADDRESS]:
        msg = f"--hop {hop!r} is missing an address"
        raise click.UsageError(msg)
    step: dict[str, str] = {"family": family, "address": parts[_INDEX_ADDRESS]}
    if family == "V4" and len(parts) >= _HASH_PARTS and parts[_HASH_PARTS - 1]:
        step["hash"] = parts[_HASH_PARTS - 1]
    return step


@cli.group()
def path() -> None:
    """Steer a live bot over the operator command channel."""


@path.command("add")
@click.option(
    "--socket",
    "socket_path",
    required=True,
    help="Unix domain socket path of the running bot's OperatorServer.",
)
@click.option(
    "--hop",
    "hops",
    multiple=True,
    required=True,
    help=(
        "A path hop as FAMILY:ADDRESS, or V4:ADDRESS:HASH (pool id). Repeat for "
        "each hop, in path order."
    ),
)
@click.option(
    "--direction",
    "direction",
    type=click.Choice(["zfo", "ozf"]),
    default=None,
    help=(
        "Applied to every hop when set: 'zfo' (zero-for-one, True for each hop) "
        "or 'ozf' (one-for-zero, False for each hop). Omit to let the bot "
        "auto-resolve directions."
    ),
)
def path_add(socket_path: str, hops: tuple[str, ...], direction: str | None) -> None:
    """Add ONE specific path to the live bot mid-run.

    Enqueues the path into the running bot's registration pipeline (build ->
    register + verify -> per-path release -> register). The bot continues
    solving previously-added paths while this is processed.

    Raises:
        click.ClickException: if the bot rejects the command (e.g. no live
            registration pipeline) or the socket is unreachable.

    """
    steps = [_parse_hop(h) for h in hops]
    directions = None
    if direction is not None:
        directions = [direction == "zfo"] * len(steps)
    response = asyncio.run(
        send_command(socket_path, "add_path", {"steps": steps, "directions": directions})
    )
    if not response.get("ok"):
        raise click.ClickException(response.get("error", "add_path failed"))
    click.echo(response.get("detail", "path enqueued"))


@path.command("discover")
@click.option(
    "--socket",
    "socket_path",
    required=True,
    help="Unix domain socket path of the running bot's OperatorServer.",
)
@click.option(
    "--bound",
    "bound",
    type=int,
    default=None,
    help="Maximum number of paths to process in this discovery sweep.",
)
def path_discover(socket_path: str, bound: int | None) -> None:
    """Trigger a bounded on-demand discovery sweep on the live bot.

    Distinct from the bot's unbounded background producer: this runs one bounded
    sweep and reports how many paths it processed.

    Raises:
        click.ClickException: if the bot rejects the command or the socket is
            unreachable.

    """
    response = asyncio.run(send_command(socket_path, "discover", {"bound": bound}))
    if not response.get("ok"):
        raise click.ClickException(response.get("error", "discover failed"))
    click.echo(response.get("detail", "discovery complete"))
