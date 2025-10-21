import tomllib
from pathlib import Path
from typing import Annotated

import tomlkit
from alembic.config import Config
from pydantic import BaseModel, HttpUrl, PlainSerializer, WebsocketUrl, field_validator
from pydantic_settings import BaseSettings, SettingsConfigDict

from degenbot.logging import logger
from degenbot.types.aliases import ChainId

CONFIG_DIR = Path.home() / ".config" / "degenbot"
CONFIG_FILE = CONFIG_DIR / "config.toml"
DEFAULT_DB_PATH = CONFIG_DIR / "degenbot.db"


class DatabaseSettings(BaseModel):
    # Serialize the path as a string representation of the absolute path
    path: Annotated[
        Path,
        PlainSerializer(lambda path: str(path.absolute()), return_type=str),
    ]


class Settings(BaseSettings):
    model_config = SettingsConfigDict()

    database: DatabaseSettings
    rpc: dict[
        ChainId,
        HttpUrl | WebsocketUrl | Path,
    ]

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
            config_path.read_text(),
        ),
    )


def save_config_to_file(config: Settings) -> None:
    CONFIG_FILE.write_text(
        tomlkit.dumps(
            config.model_dump(),
        ),
    )


if not CONFIG_DIR.exists():
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    logger.info(f"A configuration directory at {CONFIG_DIR} has been created.")

if not CONFIG_FILE.exists():
    _default_settings = Settings(
        database=DatabaseSettings(
            path=DEFAULT_DB_PATH,
        ),
        rpc={},
    )

    if not _default_settings.database.path.exists():
        from degenbot.database.operations import create_new_sqlite_database

        create_new_sqlite_database(db_path=_default_settings.database.path)
        logger.warning(
            "The database specified in the configuration file does not exist. An empty database "
            "has been initialized."
        )

    save_config_to_file(_default_settings)
    logger.info(f"A configuration file has been created at {CONFIG_FILE}.")

settings = load_config_from_file(CONFIG_FILE)


def get_alembic_config() -> Config:
    cfg = Config()
    cfg.set_main_option("sqlalchemy.url", f"sqlite:///{settings.database.path.absolute()}")
    cfg.set_main_option("script_location", "degenbot:migrations")

    return cfg
