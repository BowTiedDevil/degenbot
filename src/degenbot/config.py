import pathlib
import tomllib
from typing import Annotated

import tomlkit
from pydantic import BaseModel, FilePath, PlainSerializer
from pydantic_settings import BaseSettings, SettingsConfigDict

CONFIG_DIR = pathlib.Path.home() / ".config" / "degenbot"
CONFIG_FILE = CONFIG_DIR / "config.toml"
DEFAULT_DB_PATH = CONFIG_DIR / "degenbot.db"


class DatabaseSettings(BaseModel):
    # Serialize the path as a string representation of the absolute path
    path: Annotated[
        FilePath,
        PlainSerializer(lambda path: str(path.absolute()), return_type=str),
    ]


class Settings(BaseSettings):
    model_config = SettingsConfigDict()

    database: DatabaseSettings


def load_config_from_file(config_path: pathlib.Path) -> Settings:
    return Settings.model_validate(
        tomllib.loads(
            config_path.read_text(),
        ),
    )


def save_config_to_file(config: Settings) -> None:
    if not CONFIG_DIR.exists():
        CONFIG_DIR.mkdir(parents=True, exist_ok=True)

    CONFIG_FILE.write_text(
        tomlkit.dumps(
            config.model_dump(),
        ),
    )


if not CONFIG_DIR.exists():
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)

if not CONFIG_FILE.exists():
    from degenbot.database import create_new_sqlite_database

    # Pydantic will validate that the DB path exists, so initialize it first
    create_new_sqlite_database(DEFAULT_DB_PATH)

    _default_settings = Settings(
        database=DatabaseSettings(
            path=DEFAULT_DB_PATH,
        ),
    )

    save_config_to_file(_default_settings)

settings: Settings = load_config_from_file(CONFIG_FILE)
