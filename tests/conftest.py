import logging
import os
from collections.abc import Generator
from typing import Any

import dotenv
import pytest
from _pytest.config import Config, Parser
from _pytest.nodes import Item

from degenbot.anvil_fork import AnvilFork
from degenbot.connection import connection_manager
from degenbot.logging import logger
from degenbot.registry import managed_pool_registry, pool_registry, token_registry
from degenbot.types.abstract.pool_manager import AbstractPoolManager
from degenbot.types.concrete import AbstractPublisherMessage, Publisher

LIVE_RPC_FIXTURES = frozenset({
    "fork_arbitrum_full",
    "fork_base_archive",
    "fork_base_full",
    "fork_mainnet_archive",
    "fork_mainnet_full",
})
LIVE_RPC_MARKERS = frozenset({"arbitrum", "base", "ethereum"})
LIVE_RPC_NODEID_PREFIXES = (
    "tests/rust/test_alloy_async_integration.py",
    "tests/rust/test_alloy_integration.py",
    "tests/rust/test_provider_interface.py",
    "tests/test_anvil_fork.py",
    "tests/test_config.py",
    "tests/test_provider_parallel.py",
)
DATABASE_NODEID_PREFIXES = (
    "tests/database/",
    "tests/test_pathfinding.py",
    "tests/uniswap/v3/test_uniswap_v3_snapshot.py",
)

env_file = dotenv.find_dotenv("tests.env")
env_values = dotenv.dotenv_values(env_file)


def _rpc_env(name: str, default: str) -> str:
    return os.environ.get(name) or env_values.get(name) or default


ARBITRUM_FULL_NODE_HTTP_URI: str = _rpc_env(
    "ARBITRUM_FULL_NODE_HTTP_URI", "https://arbitrum-one-rpc.publicnode.com"
)
ARBITRUM_FULL_NODE_WS_URI: str = _rpc_env(
    "ARBITRUM_FULL_NODE_WS_URI", "wss://arbitrum-one-rpc.publicnode.com"
)

BASE_ARCHIVE_NODE_HTTP_URI: str = _rpc_env(
    "BASE_ARCHIVE_NODE_HTTP_URI", "https://base-rpc.publicnode.com"
)
BASE_ARCHIVE_NODE_WS_URI: str = _rpc_env(
    "BASE_ARCHIVE_NODE_WS_URI", "wss://base-rpc.publicnode.com"
)
BASE_FULL_NODE_HTTP_URI: str = _rpc_env(
    "BASE_FULL_NODE_HTTP_URI", "https://base-rpc.publicnode.com"
)
BASE_FULL_NODE_WS_URI: str = _rpc_env("BASE_FULL_NODE_WS_URI", "wss://base-rpc.publicnode.com")

ETHEREUM_ARCHIVE_NODE_HTTP_URI: str = _rpc_env(
    "ETHEREUM_ARCHIVE_NODE_HTTP_URI", "https://eth.drpc.org"
)
ETHEREUM_ARCHIVE_NODE_WS_URI: str = _rpc_env("ETHEREUM_ARCHIVE_NODE_WS_URI", "wss://eth.drpc.org")
ETHEREUM_FULL_NODE_HTTP_URI: str = _rpc_env("ETHEREUM_FULL_NODE_HTTP_URI", "https://eth.drpc.org")
ETHEREUM_FULL_NODE_WS_URI: str = _rpc_env("ETHEREUM_FULL_NODE_WS_URI", "wss://eth.drpc.org")


def pytest_addoption(parser: Parser):
    parser.addoption(
        "--skip-fixture",
        action="store",
        default="",
        help="Comma-separated list of fixture names to skip",
    )
    parser.addoption(
        "--run-live-rpc",
        action="store_true",
        default=False,
        help="Include tests that require public or operator-provided live RPC endpoints.",
    )
    parser.addoption(
        "--run-database",
        action="store_true",
        default=False,
        help="Include tests that require an operator-populated degenbot SQLite database.",
    )


def pytest_collection_modifyitems(config: Config, items: list[Item]):
    skip_fixtures: str = config.getoption("--skip-fixture")
    ignore_fixtures = {name.strip() for name in skip_fixtures.split(",") if name.strip()}
    run_live_rpc = bool(config.getoption("--run-live-rpc"))
    run_database = bool(config.getoption("--run-database"))

    remaining_items: list[Item] = []
    deselected_items: list[Item] = []

    for item in items:
        if any(fix in ignore_fixtures for fix in item.fixturenames):
            deselected_items.append(item)
            continue
        if _is_live_rpc_test(item) and not run_live_rpc:
            deselected_items.append(item)
            continue
        if _is_database_test(item) and not run_database:
            deselected_items.append(item)
            continue
        remaining_items.append(item)

    if deselected_items:
        items[:] = remaining_items
        config.hook.pytest_deselected(items=deselected_items)


def _is_live_rpc_test(item: Item) -> bool:
    return (
        bool(LIVE_RPC_FIXTURES.intersection(item.fixturenames))
        or any(item.get_closest_marker(marker) is not None for marker in LIVE_RPC_MARKERS)
        or item.nodeid.startswith(LIVE_RPC_NODEID_PREFIXES)
    )


def _is_database_test(item: Item) -> bool:
    return item.nodeid.startswith(DATABASE_NODEID_PREFIXES)


@pytest.fixture(autouse=True)
def _initialize_and_reset_after_each_test():
    """
    Before each test, clear/reset global values and singletons
    """
    connection_manager._reset()
    AbstractPoolManager.instances.clear()
    pool_registry._reset()
    managed_pool_registry._reset()
    token_registry._reset()


@pytest.fixture(scope="session", autouse=True)
def _set_degenbot_logging():
    """
    Set the logging level to DEBUG for the test run
    """
    logger.setLevel(logging.DEBUG)


@pytest.fixture
def fork_arbitrum_full() -> Generator[AnvilFork, None, None]:
    fork = AnvilFork(
        fork_url=ARBITRUM_FULL_NODE_HTTP_URI,
        ipc_provider_kwargs={"timeout": None},
        storage_caching=False,
        anvil_opts=["--accounts=0"],
    )
    yield fork
    fork.close()


@pytest.fixture
def fork_base_archive(request: pytest.FixtureRequest) -> Generator[AnvilFork, None, None]:
    """
    An AnvilFork using the default mainnet archive node. To fork from a specific block, parametrize
    the test with an indirect parameter for this fixture, e.g.:

    ```
    @pytest.mark.parametrize(
        "fork_base_archive", [block_number], indirect=True
    )
    def test_using_fork(
        fork_base_archive: AnvilFork
    ):
        ...
    ```
    """

    block_number = getattr(request, "param", None)

    fork = AnvilFork(
        fork_url=BASE_ARCHIVE_NODE_HTTP_URI,
        storage_caching=True,
        fork_block=block_number,
        ipc_provider_kwargs={"timeout": None},
        anvil_opts=["--accounts=0", "--optimism"],
    )
    yield fork
    fork.close()


@pytest.fixture
def fork_base_full() -> Generator[AnvilFork, None, None]:
    fork = AnvilFork(
        fork_url=BASE_FULL_NODE_HTTP_URI,
        storage_caching=False,
        anvil_opts=["--accounts=0", "--optimism"],
    )
    yield fork
    fork.close()


@pytest.fixture
def fork_mainnet_archive(request: pytest.FixtureRequest) -> Generator[AnvilFork, None, None]:
    """
    An AnvilFork using the default mainnet archive node. To fork from a specific block, parametrize
    the test with an indirect parameter for this fixture, e.g.:

    ```
    @pytest.mark.parametrize(
        "fork_mainnet_archive", [block_number], indirect=True
    )
    def test_using_fork(
        fork_mainnet_archive: AnvilFork
    ):
        ...
    ```
    """

    block_number = getattr(request, "param", None)

    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        storage_caching=True,
        fork_block=block_number,
        ipc_provider_kwargs={"timeout": None},
        anvil_opts=["--accounts=0"],
    )
    yield fork
    fork.close()


@pytest.fixture
def fork_mainnet_full() -> Generator[AnvilFork, None, None]:
    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        ipc_provider_kwargs={"timeout": None},
        storage_caching=False,
        anvil_opts=["--accounts=0"],
    )
    yield fork
    fork.close()


class FakeSubscriber:
    """
    This subscriber class provides a record of received messages, and can be used to test that
    publisher/subscriber methods operate as expected.
    """

    def __init__(self) -> None:
        self.inbox: list[dict[str, Any]] = []

    def notify(self, publisher: Publisher, message: AbstractPublisherMessage) -> None:
        self.inbox.append({
            "from": publisher,
            "message": message,
        })

    def subscribe(self, publisher: Publisher) -> None:
        publisher.subscribe(self)

    def unsubscribe(self, publisher: Publisher) -> None:
        publisher.unsubscribe(self)
