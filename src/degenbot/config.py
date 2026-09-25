"""Python's view of the Rust-owned configuration cascade.

``degenbot-config`` owns the operator file, the environment, and the resolution
order end to end (ADR-062 D7/D10). The Python driver is a consumer of that
verdict, never a second authority: this module translates the FFI surface into
the shapes Python callers use and adds nothing to the resolution itself.

The cascade has four layers -- an explicit override, the environment, the
operator file, and a declared default -- and reports the layer that won. The
config is installed once at FFI module init, so a process resolves the same
file and the same environment as the Rust console no matter which entry path
started it.
"""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING

from degenbot import _ffi
from degenbot._ffi import ResolvedChainId, ResolvedDatabasePath, ResolvedNodeUri
from degenbot.db import db_create_new_database
from degenbot.logging import logger

if TYPE_CHECKING:
    from degenbot.types.aliases import ChainId

__all__ = [
    "ResolvedChainId",
    "ResolvedDatabasePath",
    "ResolvedNodeUri",
    "RpcNotConfiguredError",
    "config_file_path",
    "declared_database_path",
    "resolve_chain_id",
    "resolve_database_path",
    "resolve_http_rpc_uri",
    "resolve_node",
    "resolve_rpc_uris",
    "resolve_ws_rpc_uri",
]

# A caller asks for a request (pool reads, ``eth_callMany``, submission) or a
# subscription (a feed that must not degrade to polling). The core owns the
# vocabulary; these are the two names this module uses to ask for it.
_REQUEST_SCOPE = "request"
_SUBSCRIPTION_SCOPE = "subscription"

_RETIRED_OVERRIDE_NAMES = (
    "cli_http",
    "cli_ws",
    "fallback_http",
    "fallback_ws",
    "config",
)
_RETIRED_OVERRIDE_REPLACEMENT = (
    "the cascade has one explicit-override layer, the scheme-classified node argument: "
    "pass node='http://host:8545', node='wss://host:8546', or node='ipc:///tmp/anvil.ipc' "
    "and the core classifies the transport from the value itself"
)


class RpcNotConfiguredError(ValueError):
    """No endpoint configured for a chain in any cascade layer.

    Subclasses :class:`ValueError` so callers that already catch a misconfigured
    endpoint keep working. The core's refusal names the scope, the transports it
    consulted, the layers each was read through, and what to declare or export,
    so a fresh environment fails fast with a pointer at what to set instead of
    silently degrading.
    """


def _refuse_retired_overrides(fn_name: str, retired: dict[str, object]) -> None:
    """Refuse a pre-0.6 override keyword, naming the one that replaced it.

    The retired per-transport keywords split a single override layer in two, and
    a caller-supplied "fallback" was a second, unrankable position in the
    cascade. Both are gone rather than translated (ADR-062 D13).

    Args:
        fn_name: The public function the caller reached for, for the message.
        retired: The keyword arguments the caller passed.

    Raises:
        TypeError: When a retired override keyword carries a value, or when an
            unrecognized keyword is passed.

    """
    for name in _RETIRED_OVERRIDE_NAMES:
        if retired.get(name) is not None:
            msg = f"{fn_name}() no longer accepts {name!r}: {_RETIRED_OVERRIDE_REPLACEMENT}"
            raise TypeError(msg)
    unknown = sorted(set(retired) - set(_RETIRED_OVERRIDE_NAMES))
    if unknown:
        msg = f"{fn_name}() got an unexpected keyword argument {unknown[0]!r}"
        raise TypeError(msg)


def resolve_node(
    chain_id: ChainId,
    scope: str,
    *,
    node: str | None = None,
) -> ResolvedNodeUri:
    """Resolve one endpoint for ``chain_id`` and report the layer that won.

    The full verdict: the endpoint plus its ``Source`` (``default``, ``file``,
    ``env``, or ``cli``), so a diagnostic can name the layer rather than
    re-deriving it. ``scope`` is the caller's capability -- ``"request"`` or
    ``"subscription"`` -- and a subscription never selects an ``http`` entry.

    Args:
        chain_id: The chain the endpoint serves.
        scope: ``"request"`` or ``"subscription"``.
        node: The explicit override, classified by its own value.

    Returns:
        The endpoint and the layer that supplied it.

    Raises:
        RpcNotConfiguredError: When no layer supplied an endpoint for the chain.

    """
    try:
        return _ffi.resolve_node_uri(int(chain_id), scope, node)
    except ValueError as exc:
        raise RpcNotConfiguredError(str(exc)) from exc


def resolve_http_rpc_uri(
    chain_id: ChainId, /, *, node: str | None = None, **retired: object
) -> str:
    """Resolve the request-scope endpoint for ``chain_id``.

    Request scope is what a pool read, an ``eth_callMany``, or a transaction
    submission needs; it prefers an IPC socket, then a WS endpoint, then HTTP.
    A caller that must not depend on a subscription (a CLI ``pool update``)
    resolves through this one rather than :func:`resolve_rpc_uris`.

    Args:
        chain_id: The chain the endpoint serves.
        node: The explicit override, classified by its own value.
        **retired: A pre-0.6 per-transport override spelling. Carrying a value
            raises :class:`TypeError` that names the scheme-classified
            ``node`` argument replacing it; the catch-all exists so the hard
            cutover fails loudly instead of silently ignoring a spelling no
            layer reads.

    Returns:
        The resolved request endpoint as a string.

    Raises:
        RpcNotConfiguredError: When no layer supplied one.

    """
    _refuse_retired_overrides("resolve_http_rpc_uri", retired)
    try:
        return _ffi.resolve_node_uri(int(chain_id), _REQUEST_SCOPE, node).uri
    except ValueError as exc:
        raise RpcNotConfiguredError(str(exc)) from exc


def resolve_ws_rpc_uri(chain_id: ChainId, /, *, node: str | None = None, **retired: object) -> str:
    """Resolve the subscription-scope endpoint for ``chain_id``.

    The subscription scope is the transports a feed can hold open -- IPC or WS
    -- and never an HTTP entry, so a poll-only endpoint is refused here instead
    of being handed to a subscriber that would silently degrade.

    Args:
        chain_id: The chain the endpoint serves.
        node: The explicit override, classified by its own value.
        **retired: A pre-0.6 per-transport override spelling. Carrying a value
            raises :class:`TypeError` that names the scheme-classified
            ``node`` argument replacing it; the catch-all exists so the hard
            cutover fails loudly instead of silently ignoring a spelling no
            layer reads.

    Returns:
        The resolved subscription endpoint as a string.

    Raises:
        RpcNotConfiguredError: When no layer supplied one.

    """
    _refuse_retired_overrides("resolve_ws_rpc_uri", retired)
    try:
        return _ffi.resolve_node_uri(int(chain_id), _SUBSCRIPTION_SCOPE, node).uri
    except ValueError as exc:
        raise RpcNotConfiguredError(str(exc)) from exc


def resolve_rpc_uris(
    chain_id: ChainId, /, *, node: str | None = None, **retired: object
) -> tuple[str, str]:
    """Resolve the ``(request, subscription)`` endpoint pair for ``chain_id``.

    The two capabilities resolve independently, each through the same four
    layers, so a session can take its request endpoint from the file and its
    subscription endpoint from the environment. There is no ``localhost``
    default: a chain with no endpoint in any layer raises.

    Args:
        chain_id: The chain the endpoints serve.
        node: The explicit override, classified by its own value.
        **retired: A pre-0.6 per-transport override spelling. Carrying a value
            raises :class:`TypeError` that names the scheme-classified
            ``node`` argument replacing it; the catch-all exists so the hard
            cutover fails loudly instead of silently ignoring a spelling no
            layer reads.

    Returns:
        The resolved ``(request_uri, subscription_uri)`` pair.

    Raises:
        RpcNotConfiguredError: When either capability is unresolved.

    """
    _refuse_retired_overrides("resolve_rpc_uris", retired)
    try:
        request = _ffi.resolve_node_uri(int(chain_id), _REQUEST_SCOPE, node).uri
        subscription = _ffi.resolve_node_uri(int(chain_id), _SUBSCRIPTION_SCOPE, node).uri
    except ValueError as exc:
        raise RpcNotConfiguredError(str(exc)) from exc
    return request, subscription


def resolve_chain_id(chain_id: int | str | None = None) -> int:
    """Resolve the session chain id.

    Refuses with a :class:`ValueError` -- naming what to declare, export, or
    pass -- both when no layer named a chain and when the explicit value is not
    an integer.

    Args:
        chain_id: The explicit override; the core parses it as text so a
            non-integer is refused by the layer that owns the spelling.

    Returns:
        The chain id the session targets.

    """
    return _ffi.resolve_chain_id(None if chain_id is None else str(chain_id)).chain_id


def resolve_database_path(database: str | None = None) -> str:
    """Resolve the database path a session opens.

    The winning value already has ``~`` and the XDG state home expanded by the
    resolver, so a caller needs no second expansion.

    Args:
        database: The explicit override.

    Returns:
        The resolved database path.

    """
    return _ffi.resolve_database_path(database).path


def config_file_path() -> str | None:
    """Return the operator file the loader selected.

    A free-form reader (the deployment-registry overlay) reads the same file the
    typed load read. ``None`` means the process has no file layer, which is
    contractually the schema defaults.

    Returns:
        The selected file's path, or ``None``.

    """
    return _ffi.config_file_path()


def declared_database_path() -> str:
    """Return the declared ``database.path`` key, with no cascade.

    The value the operator wrote, with no ``~`` expansion. A caller that wants
    the path a session opens asks :func:`resolve_database_path`.

    Returns:
        The declared ``database.path`` value.

    """
    return _ffi.declared_database_path()


def _init_config() -> str:
    """Bootstrap the session database, returning the resolved path.

    Python never writes the operator file -- an absent file is contractually
    schema defaults -- so the only bootstrap left here is the database: its
    parent directory is created and an empty file is initialized. A ``:memory:``
    database has neither and is returned as-is.

    Returns:
        The resolved database path.

    """
    path = resolve_database_path()
    if Path(path).name == ":memory:":
        return path
    parent = Path(path).parent
    parent.mkdir(parents=True, exist_ok=True)
    if not Path(path).exists():
        db_create_new_database(path)
        logger.info(f"Initialized new SQLite database at {path}")
    return path
