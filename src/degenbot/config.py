"""Application configuration loaded from TOML files."""

import tomllib
from pathlib import Path
from typing import Annotated

import tomlkit
from pydantic import BaseModel, HttpUrl, PlainSerializer, WebsocketUrl, field_validator
from pydantic_settings import BaseSettings, SettingsConfigDict

from degenbot.database.operations import create_new_sqlite_database
from degenbot.logging import logger
from degenbot.types.aliases import ChainId

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
