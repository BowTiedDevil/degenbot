"""Application configuration loaded from TOML files."""

import os
import tomllib
from pathlib import Path
from typing import Annotated

from pydantic import (
    BaseModel,
    Field,
    HttpUrl,
    PlainSerializer,
    WebsocketUrl,
    field_validator,
    model_validator,
)
from pydantic_settings import BaseSettings, SettingsConfigDict

from degenbot.db import db_create_new_database
from degenbot.logging import logger
from degenbot.types.aliases import ChainId

_HTTP_ENV_PREFIX = "DEGENBOT_RPC_HTTP_CHAINID_"
_WS_ENV_PREFIX = "DEGENBOT_RPC_WS_CHAINID_"

# JLFE2F follow-up: the 0.6 cutover retired top-level `default_chain_id` from
# the shared operator file (the typed Rust loader refuses it at boot —
# degenbot-python/src/lib.rs exits 2), so the Python cascade owns the chain id
# through this env name instead (docs/config-migration.md replacement table).
_DEFAULT_CHAIN_ID_ENV_VAR = "DEGENBOT_DEFAULT_CHAIN_ID"


def _xdg_config_home() -> Path:
    """Return ``$XDG_CONFIG_HOME`` (absolute only) or ``$HOME/.config``.

    The XDG Base Directory spec ignores a relative or empty value, so a bogus
    variable falls back to the home-relative default.

    Returns:
        The XDG configuration base directory.

    """
    raw = os.environ.get("XDG_CONFIG_HOME")
    if raw and Path(raw).is_absolute():
        return Path(raw)
    return Path.home() / ".config"


def _xdg_state_home() -> Path:
    """Return ``$XDG_STATE_HOME`` (absolute only) or ``$HOME/.local/state``.

    State-like data (the connector-index database, durable journals) belongs
    in the XDG state home, never in ``~/.config``. A relative or empty
    variable is ignored per the XDG spec.

    Returns:
        The XDG state base directory.

    """
    raw = os.environ.get("XDG_STATE_HOME")
    if raw and Path(raw).is_absolute():
        return Path(raw)
    return Path.home() / ".local" / "state"


CONFIG_DIR = _xdg_config_home() / "degenbot"
CONFIG_FILE = CONFIG_DIR / "config.toml"
# The connector-index database is durable state, not configuration: it lives
# under the XDG state home, not ``~/.config``.
DB_PATH = _xdg_state_home() / "degenbot" / "db" / "degenbot.db"


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

        The config file is hand-edited on a user's machine, where a ``~``-prefixed path is
        the natural thing to write. Left as a literal ``~``, ``Path.absolute()`` (used downstream
        by the SQLite URL builders) resolves it against the process cwd (e.g.
        ``<repo>/~/.config/...``) rather than the home directory, so SQLite emits ``unable to open
        database file``. RPC file-path endpoints already get ``expanduser().absolute()`` in
        ``validate_paths``; the database path must do the same. ``:memory:`` databases are
        unaffected (``expanduser()`` is a no-op on them).

        Returns:
            The path with a leading ``~`` expanded to the home directory.

        """
        return path.expanduser()


class OtelSettings(BaseModel):
    """OpenTelemetry telemetry settings (epic RMH23E T5).

    Read by the Rust pymodule init when the dev-only ``otel`` feature is
    compiled (spans + Prometheus scrape endpoint default to enabled in dev;
    opt out with ``DEGENBOT_OTEL=0``). Only ``endpoint`` is defined today.
    """

    # OTLP span export endpoint. Precedence: OTEL_EXPORTER_OTLP_ENDPOINT env
    # var wins over this key; when both are unset the exporter defaults to
    # http://localhost:4318.
    endpoint: HttpUrl | None = None

    # Master switch for dev OTel telemetry (spans + metrics). Precedence:
    # DEGENBOT_OTEL env var wins over this key ("0" disables, any other
    # value enables); when both are unset, telemetry defaults to ON in dev
    # builds (the otel feature does not compile into release wheels).
    enabled: bool | None = None

    # Prometheus scrape endpoint for drain-path metrics. Precedence:
    # DEGENBOT_METRICS_ADDR env var wins over this key; default 127.0.0.1:9464.
    metrics_addr: str | None = None


class DegenbotConfig(BaseSettings):
    """DegenbotConfig class."""

    # JLFE2F: carry the modern layout's Rust-domain sections (telemetry,
    # runtime, solve, ...). Only extra=forbid would refuse every file the
    # typed Rust loader accepts.
    model_config = SettingsConfigDict(extra="ignore")

    # JLFE2F (0.6 modern layout): the Python-domain sections [database]/
    # [rpc]/[ws] LEFT the shared operator file (now the typed Rust BotConfig
    # file layer — docs/config-migration.md). Fields default instead: the
    # database to the standard DB_PATH, rpc/ws to empty (the cascade then
    # resolves endpoints from env or the caller).
    database: DatabaseSettings = Field(default_factory=lambda: DatabaseSettings(path=DB_PATH))
    rpc: dict[
        ChainId,
        HttpUrl | WebsocketUrl | Path,
    ] = {}
    ws: dict[
        ChainId,
        WebsocketUrl,
    ] = {}
    # The chain this Bot session targets (ADR-006 D5 — one Bot per chain).
    # `None` on a freshly-initialized config (no RPCs configured yet); a
    # `Bot` refuses to construct without it + enforces the connected RPC's
    # `eth_chainId` matches it (fail-fast on a misconfigured endpoint).
    default_chain_id: ChainId | None = None
    # OpenTelemetry settings (epic RMH23E T5). Optional so existing config
    # files without an [otel] section keep loading unchanged.
    otel: OtelSettings = OtelSettings()
    # Per-bucket failure-reaction overrides (ADR-040 D3; docs/failure-policy.md).
    # Flat form: bucket or quoted "kind.reason" -> action string; nested form:
    # kind = { reason = "action" }. Bucket/action *name* validation is the Rust
    # core's boot-time job (unknown bucket/action exits 2); this model only
    # carries the table so a config file the Rust side accepts also loads in
    # Python (the RPC cascade reads this file via load_config_from_file).
    failure_policy: dict[str, str | dict[str, str]] = {}

    @model_validator(mode="before")
    @classmethod
    def _refuse_retired_strategy_selector(cls, data: object) -> object:
        """Refuse the retired single-arm ``strategy.name`` selector.

        The typed Rust schema no longer declares ``strategy.name`` (per-facet
        ``strategy.<facet>.active`` flags own selection, and the Rust loader is
        fail-closed on the unknown key). Mirror that here so a surviving
        selector spelling in the operator file (or an init kwarg) is a pointed
        error, never a silently absorbed value.

        Returns:
            The validated input object, unchanged when no retired selector is present.

        Raises:
            ValueError: when a retired ``strategy_name`` or ``strategy.name``
                selector spelling is present in the input.

        """
        if isinstance(data, dict):
            strategy = data.get("strategy")
            retired_selector = "strategy_name" in data or (
                isinstance(strategy, dict) and "name" in strategy
            )
            if retired_selector:
                msg = (
                    "the retired single-arm strategy selector is not supported: "
                    "select strategies with the per-facet "
                    "strategy.<facet>.active flags (strategy.settlement.active, "
                    "strategy.mevblocker_backrun.active, "
                    "strategy.txpool_backrun.active)."
                )
                raise ValueError(msg)
        return data

    @field_validator("rpc", mode="after")
    def validate_paths(
        cls,  # ruff:ignore[invalid-first-argument-name-for-method]
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


def _env_default_chain_id() -> int | None:
    """Resolve the session chain id from the OS env layer of the cascade.

    The 0.6 cutover (JLFE2F) retired top-level ``default_chain_id`` from the
    shared operator file, but the Python CLI boot (``_init_config``) has no
    other source for it. The cascade therefore carries the env layer the
    migration doc's replacement names: ``DEGENBOT_DEFAULT_CHAIN_ID`` outranks
    the retired file key.

    Returns:
        The parsed chain id, or ``None`` when the variable is unset or empty.

    Raises:
        ValueError: When the variable is set to a non-integer value.

    """
    raw = os.environ.get(_DEFAULT_CHAIN_ID_ENV_VAR)
    if raw is None or not raw.strip():
        return None
    try:
        return int(raw.strip())
    except ValueError:
        msg = (
            f"{_DEFAULT_CHAIN_ID_ENV_VAR}={raw!r} is not a valid chain id. "
            f"Set it to an integer, e.g. {_DEFAULT_CHAIN_ID_ENV_VAR}=1."
        )
        raise ValueError(msg) from None


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


def _load_config_for_cascade(config: DegenbotConfig | None) -> DegenbotConfig | None:
    """Return ``config`` as-is, or load it from :data:`CONFIG_FILE` when absent.

    Centralizes the "load config.toml only if it exists" rule so every resolver
    layer (http-only, ws-only, and the combined pair) reads the file at most
    once and honors a caller-supplied override (e.g. an injected test config).

    Returns:
        The caller-supplied config, the config loaded from disk, or ``None``
        when no config was passed and :data:`CONFIG_FILE` does not exist.

    """
    if config is not None:
        return config
    if CONFIG_FILE.exists():
        return load_config_from_file(CONFIG_FILE)
    return None


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


def resolve_http_rpc_uri(
    chain_id: ChainId,
    /,
    *,
    cli_http: str | None = None,
    fallback_http: str | None = None,
    config: DegenbotConfig | None = None,
) -> str:
    """Resolve the HTTP/IPC RPC URI for ``chain_id`` through the standard cascade.

    Same precedence as :func:`resolve_rpc_uris` for the HTTP URI, but resolves
    **only** the HTTP/IPC endpoint without requiring a WS endpoint. The provider
    factory (:func:`degenbot.provider.get_provider_from_config`) uses this so an
    HTTP-only operation such as the ``degenbot pool update`` CLI is not blocked
    by a missing/unconfigured WS layer.

    ``cli_http`` is the highest-priority override; the OS env
    ``DEGENBOT_RPC_HTTP_CHAINID_{chain_id}`` layer (read via :data:`os.environ`,
    so a plain ``export`` in the devcontainer takes effect and the .env-file
    dict is NOT consulted) ranks below it; ``fallback_http`` is a caller-supplied
    lower-priority candidate; config.toml ``rpc[chain_id]``
    (:func:`load_config_from_file`, only when :data:`CONFIG_FILE` exists) is the
    final non-raising layer. A caller-supplied ``config`` short-circuits the
    disk read so the resolver stays cheap when the factory already holds one.

    Returns:
        The resolved HTTP/IPC URI as a string.

    Raises:
        RpcNotConfiguredError: if HTTP is unresolved through every layer. The
            message names the chain-id envvar and the config.toml layer.

    """
    loaded = _load_config_for_cascade(config)
    http_env = _env_http_var(chain_id)
    http = _resolve_one(
        http_env,
        cli_http,
        fallback_http,
        _config_uri(loaded, "http", chain_id),
    )
    if http is None:
        msg = (
            f"No HTTP RPC endpoint configured for chain {chain_id}. "
            f"Set {http_env} in the environment, pass --node-http, supply a "
            f"fallback, or add an `rpc` chain-id entry to {CONFIG_FILE} "
            f"(config.toml layer)."
        )
        raise RpcNotConfiguredError(msg)
    return http


def resolve_ws_rpc_uri(
    chain_id: ChainId,
    /,
    *,
    cli_ws: str | None = None,
    fallback_ws: str | None = None,
    config: DegenbotConfig | None = None,
) -> str:
    """Resolve the WS RPC URI for ``chain_id`` through the standard cascade.

    WS-only counterpart of :func:`resolve_http_rpc_uri`: same precedence list
    (substituting ``cli_ws`` / the OS env ``DEGENBOT_RPC_WS_CHAINID_{chain_id}``
    layer / ``fallback_ws`` / config.toml ``ws[chain_id]``), resolving **only**
    the WS endpoint. A caller-supplied ``config`` short-circuits the disk read.

    Returns:
        The resolved WS URI as a string.

    Raises:
        RpcNotConfiguredError: if WS is unresolved through every layer.

    """
    loaded = _load_config_for_cascade(config)
    ws_env = _env_ws_var(chain_id)
    ws = _resolve_one(
        ws_env,
        cli_ws,
        fallback_ws,
        _config_uri(loaded, "ws", chain_id),
    )
    if ws is None:
        msg = (
            f"No WS RPC endpoint configured for chain {chain_id}. "
            f"Set {ws_env} in the environment, pass --node-ws, supply a "
            f"fallback, or add a `ws` chain-id entry to {CONFIG_FILE} "
            f"(config.toml layer)."
        )
        raise RpcNotConfiguredError(msg)
    return ws


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
       candidates (a public extension point; no in-repo caller since the
       ``NODE_HOST_*`` retirement).
    4. config.toml ``rpc[chain_id]`` / ``ws[chain_id]`` via
       :func:`load_config_from_file` (only when :data:`CONFIG_FILE` exists).
    5. raise :class:`RpcNotConfiguredError` — no ``localhost`` default.

    Returns:
        The resolved ``(http_uri, ws_uri)`` pair as strings.

    """
    loaded = _load_config_for_cascade(None)
    http = resolve_http_rpc_uri(
        chain_id,
        cli_http=cli_http,
        fallback_http=fallback_http,
        config=loaded,
    )
    ws = resolve_ws_rpc_uri(
        chain_id,
        cli_ws=cli_ws,
        fallback_ws=fallback_ws,
        config=loaded,
    )
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


def _init_config() -> DegenbotConfig:
    """Load — or bootstrap — the Python-domain config for the CLI boot.

    The shared operator file is the typed Rust BotConfig file layer (JLFE2F,
    docs/config-migration.md): it carries the typed schema sections, and the
    Python-domain keys it once carried are refused there at boot. Python never
    writes the file — an absent operator file is contractually schema defaults
    — so the bootstrap creates the directory + database only. The session
    chain id arrives from the env layer (``_env_default_chain_id``), which
    outranks a surviving retired file key.

    Returns:
        The computed value.

    """
    if not CONFIG_DIR.exists():
        CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        logger.info(f"Created a configuration directory at {CONFIG_DIR}.")

    if CONFIG_FILE.exists():
        config = load_config_from_file(CONFIG_FILE)
    else:
        # No operator file: the legacy bootstrap saved a DegenbotConfig dump
        # here, which both refused to serialize (tomlkit raises on the unset
        # chain id TOML cannot represent) and would have written the retired
        # ``default_chain_id`` key the typed loader rejects at boot. The
        # modern layout is file-absent-tolerant: defaults + DB only.
        logger.info(
            f"No operator configuration file at {CONFIG_FILE}; defaults apply. "
            "Typed sections: docs/rust-config-keys.md. Python-domain keys "
            "(chain id, RPC endpoints) live in the environment — see "
            "docs/config-migration.md."
        )
        config = DegenbotConfig(
            database=DatabaseSettings(
                path=DB_PATH,
            ),
            rpc={},
        )

    env_chain_id = _env_default_chain_id()
    if env_chain_id is not None:
        config.default_chain_id = env_chain_id

    # Skip database creation for in-memory databases. The DB parent lives
    # under the XDG state home, which frequently does not exist yet, so create
    # it before the SQLite file.
    if config.database.path.name != ":memory:":
        config.database.path.parent.mkdir(parents=True, exist_ok=True)
        if not config.database.path.exists():
            db_create_new_database(str(config.database.path))
            logger.info(f"Initialized new SQLite database at {config.database.path}")

    return config
