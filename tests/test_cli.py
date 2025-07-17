import json

import pytest
from click.testing import CliRunner

from degenbot import __version__
from degenbot.cli import cli


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


def test_cli_help(runner: CliRunner):
    result = runner.invoke(cli, ["--help"])
    assert result.exit_code == 0


def test_cli_version(runner: CliRunner):
    result = runner.invoke(cli, ["--version"])
    assert result.exit_code == 0
    assert __version__ in result.output


def test_cli_config_show_default(runner: CliRunner):
    result = runner.invoke(cli, ["config", "show"])
    assert result.exit_code == 0
    assert "[database]" in result.output


def test_cli_config_show_json(runner: CliRunner):
    result = runner.invoke(cli, ["config", "show", "--json"])
    assert result.exit_code == 0
    assert json.loads(result.output)


def test_cli_config_show_toml(runner: CliRunner):
    result = runner.invoke(cli, ["config", "show", "--toml"])
    assert result.exit_code == 0
    assert "[database]" in result.output


# TODO: create fake database for reset/upgrade tests


def test_cli_database_reset(runner: CliRunner):
    result = runner.invoke(cli, ["database", "reset"], input="n")
    assert result.exit_code == 1

    result = runner.invoke(cli, ["database", "reset"], input="")
    assert result.exit_code == 1


def test_cli_database_upgrade(runner: CliRunner):
    result = runner.invoke(cli, ["database", "upgrade"], input="n")
    assert result.exit_code == 1

    result = runner.invoke(cli, ["database", "upgrade"], input="")
    assert result.exit_code == 1
