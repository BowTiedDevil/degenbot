"""Application configuration loaded from TOML files."""

import os
import tomllib
from pathlib import Path
from typing import Annotated

import tomlkit
from pydantic import BaseModel, HttpUrl, PlainSerializer, WebsocketUrl, field_validator
from pydantic_settings import BaseSettings, SettingsConfigDict

from degenbot.database.operations import create_new_sqlite_database
from degenbot.logging import logger
from degenbot.types.aliases import ChainId

_HTTP_ENV_PREFIX = "DEGENBOT_RPC_HTTP_CHAINID_"
_WS_ENV_PREFIX = "DEGENBOT_RPC_WS_CHAINID_"

CONFIG_DIR = Path.home() / ".config" / "degenbot"
CONFIG_FILE = CONFIG_DIR / "config.toml"
DB_PATH = CONFIG_DIR / "degenbot.db"


class DatabaseSettings(BaseModel):
    """DatabaseSettings class."""

    # Serialize the path as a string representation of the absolute path
    path: Annotated[
        Path,
        PlainSerializer(lambda path: str(path.absolute()), return_type=str),
    ]

    @field_validator("path", mode="after")
    @classmethod
    def _expand_user(cls, path: Path) -> Path:
        """Expand a leading ``~`` to the user's home directory.

        The config file is hand-edited on a user's machine, where ``~/.config/degenbot/degenbot.db``
        is the natural thing to write. Left as a literal ``~``, ``Path.absolute()`` (used downstream
        by the SQLite URL builders) resolves it against the process cwd (e.g.
        ``<repo>/~/.config/...``) rather than the home directory, so SQLite emits ``unable to open
        database file``. RPC file-path endpoints already get ``expanduser().absolute()`` in
        ``validate_paths``; the database path must do the same. ``:memory:`` databases are
        unaffected (``expanduser()`` is a no-op on them).

        Returns:
            The path with a leading ``~`` expanded to the home directory.

        """
        return path.expanduser()


class DegenbotConfig(BaseSettings):
    """DegenbotConfig class."""

    model_config = SettingsConfigDict()

    database: DatabaseSettings
    rpc: dict[
        ChainId,
        HttpUrl | WebsocketUrl | Path,
    ]
    ws: dict[
        ChainId,
        WebsocketUrl,
    ] = {}
    # The chain this Bot session targets (ADR-006 D5 — one Bot per chain).
    # `None` on a freshly-initialized config (no RPCs configured yet); a
    # `Bot` refuses to construct without it + enforces the connected RPC's
    # `eth_chainId` matches it (fail-fast on a misconfigured endpoint).
    default_chain_id: ChainId | None = None

    @field_validator("rpc", mode="after")
    def validate_paths(
        cls,  # noqa: N805
        rpc_dict: dict[ChainId, HttpUrl | WebsocketUrl | Path],
    ) -> dict[ChainId, HttpUrl | WebsocketUrl | Path]:
        """Validate the endpoints.

        This will convert all file paths to an absolute reference, leaving HTTP and WS URLs as-is.

        Returns:
            The computed value.

        """
        return {
            chain_id: endpoint.expanduser().absolute() if isinstance(endpoint, Path) else endpoint
            for chain_id, endpoint in rpc_dict.items()
        }


class RpcNotConfiguredError(ValueError):
    """No RPC endpoint configured for a chain in any cascade layer.

    Subclasses :class:`ValueError` so existing ``pytest.raises(ValueError)`` callers keep working.
    The message names every source layer checked and the exact per-chain envvar, so a fresh
    environment fails fast with a pointer at what to set instead of silently falling back to
    ``localhost`` (which masks misconfiguration until a connect crashes).
    """


def _env_http_var(chain_id: ChainId) -> str:
    return f"{_HTTP_ENV_PREFIX}{chain_id}"


def _env_ws_var(chain_id: ChainId) -> str:
    return f"{_WS_ENV_PREFIX}{chain_id}"


def _resolve_one(
    env_var: str,
    cli_value: str | None,
    fallback: str | None,
    config_value: str | None,
) -> str | None:
    """Resolve a single URI through the cascade, independently of the other.

    Precedence: ``cli_value`` > OS env ``env_var`` > ``fallback`` >
    ``config_value``.

    Returns:
        The first non-empty value, or ``None`` if no layer provides one (the
        caller decides how to surface that).

    """
    if cli_value:
        return cli_value
    env_value = os.environ.get(env_var)
    if env_value:
        return env_value
    if fallback:
        return fallback
    return config_value


def _config_uri(config: DegenbotConfig | None, kind: str, chain_id: ChainId) -> str | None:
    """Render the URI for ``kind`` ("http"/"ws") from a loaded config, or None.

    Returns:
        The ``str()`` of the ``rpc``/``ws`` entry for ``chain_id``, or ``None``
        when no config or no entry exists for the chain.

    """
    if config is None:
        return None
    entry = config.rpc.get(chain_id) if kind == "http" else config.ws.get(chain_id)
    if entry is None:
        return None
    # ``Path`` IPC sockets render via str(); HttpUrl/WebsocketUrl via str().
    return str(entry)


def resolve_rpc_uris(
    chain_id: ChainId,
    /,
    *,
    cli_http: str | None = None,
    cli_ws: str | None = None,
    fallback_http: str | None = None,
    fallback_ws: str | None = None,
) -> tuple[str, str]:
    """Resolve the (http, ws) RPC URIs for ``chain_id`` via the standard cascade.

    Each URI resolves **independently** through the same precedence list:

    1. ``cli_http`` / ``cli_ws`` — explicit caller override (highest priority).
       Used, for example, by a ``--node-http`` CLI flag.
    2. OS env ``DEGENBOT_RPC_HTTP_CHAINID_{chain_id}`` /
       ``DEGENBOT_RPC_WS_CHAINID_{chain_id}`` — read via :data:`os.environ`, so a
       plain ``export`` in the devcontainer takes effect (the .env-file dict is
       NOT consulted).
    3. ``fallback_http`` / ``fallback_ws`` — caller-supplied lower-priority
       candidates. The backrun example uses this slot to inject the deprecated
       ``NODE_HOST_*`` rebuilt URIs without leaking that naming into the
       library resolver.
    4. config.toml ``rpc[chain_id]`` / ``ws[chain_id]`` via
       :func:`load_config_from_file` (only when :data:`CONFIG_FILE` exists).
    5. raise :class:`RpcNotConfiguredError` — no ``localhost`` default.

    Returns:
        The resolved ``(http_uri, ws_uri)`` pair as strings.

    Raises:
        RpcNotConfiguredError: if either URI is unresolved through every
            layer. http and ws are checked independently, so the message names
            which one was missing.

    """
    config: DegenbotConfig | None = None
    if CONFIG_FILE.exists():
        config = load_config_from_file(CONFIG_FILE)

    http_env = _env_http_var(chain_id)
    ws_env = _env_ws_var(chain_id)

    http = _resolve_one(
        http_env,
        cli_http,
        fallback_http,
        _config_uri(config, "http", chain_id),
    )
    if http is None:
        msg = (
            f"No HTTP RPC endpoint configured for chain {chain_id}. "
            f"Set {http_env} in the environment, pass --node-http, supply a "
            f"fallback, or add an `rpc` chain-id entry to {CONFIG_FILE} "
            f"(config.toml layer)."
        )
        raise RpcNotConfiguredError(msg)
    ws = _resolve_one(
        ws_env,
        cli_ws,
        fallback_ws,
        _config_uri(config, "ws", chain_id),
    )
    if ws is None:
        msg = (
            f"No WS RPC endpoint configured for chain {chain_id}. "
            f"Set {ws_env} in the environment, pass --node-ws, supply a "
            f"fallback, or add a `ws` chain-id entry to {CONFIG_FILE} "
            f"(config.toml layer)."
        )
        raise RpcNotConfiguredError(msg)
    return http, ws


def load_config_from_file(config_path: Path) -> DegenbotConfig:
    """Load config from file.

    Returns:
        The computed value.

    """
    return DegenbotConfig.model_validate(
        tomllib.loads(
            config_path.read_text(encoding="utf-8"),
        ),
    )


def save_config_to_file(config: DegenbotConfig) -> None:
    """Save config to file."""
    CONFIG_FILE.write_text(
        tomlkit.dumps(
            config.model_dump(),
        ),
    )


def _init_config() -> DegenbotConfig:
    if not CONFIG_DIR.exists():
        CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        logger.info(f"Created a configuration directory at {CONFIG_DIR}.")

    if CONFIG_FILE.exists():
        return load_config_from_file(CONFIG_FILE)

    config = DegenbotConfig(
        database=DatabaseSettings(
            path=DB_PATH,
        ),
        rpc={},
    )

    save_config_to_file(config)
    logger.info(f"Created a configuration file at {CONFIG_FILE}.")

    # Skip database creation for in-memory databases
    if config.database.path.name != ":memory:" and not config.database.path.exists():
        create_new_sqlite_database(db_path=config.database.path)

    return config
