import tomllib
from pathlib import Path
from typing import Annotated

import tomlkit
from pydantic import (
    BaseModel,
    Field,
    HttpUrl,
    PlainSerializer,
    SecretStr,
    WebsocketUrl,
    field_validator,
)
from pydantic_settings import BaseSettings, SettingsConfigDict

from degenbot.adapters.config import DegenbotSettings
from degenbot.logging import logger
from degenbot.types.aliases import ChainId

__all__ = (
    "DatabaseSettings",
    "DegenbotSettings",
    "Settings",
    "load_config_from_file",
    "save_config_to_file",
    "settings",
)

CONFIG_DIR = Path.home() / ".config" / "degenbot"
CONFIG_FILE = CONFIG_DIR / "config.toml"
DB_PATH = CONFIG_DIR / "degenbot.db"


class DatabaseSettings(BaseModel):
    # Serialize the path as a string representation of the absolute path
    path: Annotated[
        Path,
        PlainSerializer(lambda path: str(path.absolute()), return_type=str),
    ]


class Settings(BaseSettings):
    model_config = SettingsConfigDict()

    database: DatabaseSettings = Field(default_factory=lambda: DatabaseSettings(path=DB_PATH))
    rpc: dict[
        ChainId,
        HttpUrl | WebsocketUrl | Path,
    ] = Field(default_factory=dict)
    cow_api_base: HttpUrl = Field(default=HttpUrl("https://api.cow.fi/arbitrum_one"))
    cow_api_key: SecretStr | None = None
    cow_solver_address: str | None = None
    cow_solver_private_key: SecretStr | None = None
    chain_id: int = 42161

    @field_validator("rpc", mode="after")
    def validate_paths(
        cls,  # noqa: N805
        rpc_dict: dict[ChainId, HttpUrl | WebsocketUrl | Path],
    ) -> dict[ChainId, HttpUrl | WebsocketUrl | Path]:
        """
        Validate the endpoints.

        This will convert all file paths to an absolute reference, leaving HTTP and WS URLs as-is.
        """

        return {
            chain_id: endpoint.expanduser().absolute() if isinstance(endpoint, Path) else endpoint
            for chain_id, endpoint in rpc_dict.items()
        }


def load_config_from_file(config_path: Path) -> Settings:
    return Settings.model_validate(
        tomllib.loads(
            config_path.read_text(encoding="utf-8"),
        ),
    )


def save_config_to_file(config: Settings) -> None:
    CONFIG_FILE.write_text(
        tomlkit.dumps(
            config.model_dump(mode="json", exclude_none=True),
        ),
        encoding="utf-8",
    )


def _init_settings() -> Settings:
    if not CONFIG_DIR.exists():
        CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        logger.info(f"Created a configuration directory at {CONFIG_DIR}.")

    if CONFIG_FILE.exists():
        return load_config_from_file(CONFIG_FILE)

    settings = Settings(
        database=DatabaseSettings(
            path=DB_PATH,
        ),
        rpc={},
    )

    save_config_to_file(settings)
    logger.info(f"Created a configuration file at {CONFIG_FILE}.")

    if not settings.database.path.exists():
        from degenbot.database.operations import create_new_sqlite_database

        create_new_sqlite_database(db_path=settings.database.path)

    return settings


_settings: list[Settings] = []


def _load_settings() -> Settings:
    if not _settings:
        _settings.append(_init_settings())
    return _settings[0]


def _reset_settings() -> None:
    _settings.clear()


class _SettingsProxy:
    def __getattr__(self, name: str) -> object:
        return getattr(_load_settings(), name)

    def __repr__(self) -> str:
        return repr(_load_settings())


settings = _SettingsProxy()
