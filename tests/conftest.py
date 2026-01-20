import logging
from collections.abc import Generator
from typing import Any

import dotenv
import pytest
from _pytest.config import Config, Parser
from _pytest.nodes import Item

from degenbot.anvil_fork import AnvilFork
from degenbot.connection import connection_manager
from degenbot.erc20.manager import Erc20TokenManager
from degenbot.logging import logger
from degenbot.registry import pool_registry, token_registry
from degenbot.types.abstract.pool_manager import AbstractPoolManager
from degenbot.types.concrete import AbstractPublisherMessage, Publisher

env_file = dotenv.find_dotenv("tests.env")
env_values = dotenv.dotenv_values(env_file)


ARBITRUM_FULL_NODE_HTTP_URI: str = env_values.get(
    "ARBITRUM_FULL_NODE_HTTP_URI", "https://arbitrum-one-rpc.publicnode.com"
)
ARBITRUM_FULL_NODE_WS_URI: str = env_values.get(
    "ARBITRUM_FULL_NODE_WS_URI", "wss://arbitrum-one-rpc.publicnode.com"
)

BASE_ARCHIVE_NODE_HTTP_URI: str = env_values.get(
    "BASE_ARCHIVE_NODE_HTTP_URI", "https://base.llamarpc.com/"
)
BASE_ARCHIVE_NODE_WS_URI: str = env_values.get(
    "BASE_ARCHIVE_NODE_WS_URI", "wss://base.llamarpc.com/"
)
BASE_FULL_NODE_HTTP_URI: str = env_values.get("BASE_FULL_NODE_HTTP_URI", "http://localhost:8544/")
BASE_FULL_NODE_WS_URI: str = env_values.get("BASE_FULL_NODE_WS_URI", "ws://localhost:8548/")

ETHEREUM_ARCHIVE_NODE_HTTP_URI: str = env_values.get(
    "ETHEREUM_ARCHIVE_NODE_HTTP_URI", "https://eth.llamarpc.com/"
)
ETHEREUM_ARCHIVE_NODE_WS_URI: str = env_values.get(
    "ETHEREUM_ARCHIVE_NODE_WS_URI", "wss://eth.llamarpc.com/"
)
ETHEREUM_FULL_NODE_HTTP_URI: str = env_values.get(
    "ETHEREUM_FULL_NODE_HTTP_URI", "http://localhost:8545/"
)
ETHEREUM_FULL_NODE_WS_URI: str = env_values.get("ETHEREUM_FULL_NODE_WS_URI", "ws://localhost:8546/")


def pytest_addoption(parser: Parser):
    parser.addoption(
        "--skip-fixture",
        action="store",
        default="",
        help="Comma-separated list of fixture names to skip",
    )


def pytest_collection_modifyitems(config: Config, items: list[Item]):
    """
    Modify the
    """
    skip_fixtures: str = config.getoption("--skip-fixture")
    if not skip_fixtures:
        return  # nothing to skip

    # Convert comma-separated string into a set of fixture names
    ignore_fixtures = {name.strip() for name in skip_fixtures.split(",") if name.strip()}

    if not ignore_fixtures:
        return

    remaining_items = []
    deselected_items = []

    for item in items:
        if any(fix in ignore_fixtures for fix in item.fixturenames):
            deselected_items.append(item)
        else:
            remaining_items.append(item)

    if deselected_items:
        items[:] = remaining_items
        config.hook.pytest_deselected(items=deselected_items)


@pytest.fixture(autouse=True)
def _initialize_and_reset_after_each_test():
    """
    Before each test, clear/reset global values and singletons
    """
    connection_manager.connections.clear()
    connection_manager._default_chain_id = None
    AbstractPoolManager.instances.clear()
    Erc20TokenManager._state.clear()
    pool_registry._all_pools.clear()
    pool_registry._v4_pool_registry._all_v4_pools.clear()
    token_registry._all_tokens.clear()


@pytest.fixture(scope="session", autouse=True)
def _set_degenbot_logging():
    """
    Set the logging level to DEBUG for the test run
    """
    logger.setLevel(logging.DEBUG)


@pytest.fixture  # (scope="session")
def fork_arbitrum_full() -> Generator[AnvilFork, None, None]:
    fork = AnvilFork(
        fork_url=ARBITRUM_FULL_NODE_HTTP_URI,
        ipc_provider_kwargs={"timeout": None},
        storage_caching=False,
        anvil_opts=["--accounts=0"],
    )
    yield fork
    fork.close()


@pytest.fixture  # (scope="session")
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


@pytest.fixture  # (scope="session")
def fork_base_full() -> Generator[AnvilFork, None, None]:
    fork = AnvilFork(
        fork_url=BASE_FULL_NODE_HTTP_URI,
        storage_caching=False,
        anvil_opts=["--accounts=0", "--optimism"],
    )
    yield fork
    fork.close()


@pytest.fixture  # (scope="session")
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


@pytest.fixture  # (scope="session")
def fork_mainnet_full():  # -> Generator[AnvilFork, None, None]:
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
        self.inbox.append(
            {
                "from": publisher,
                "message": message,
            }
        )

    def subscribe(self, publisher: Publisher) -> None:
        publisher.subscribe(self)

    def unsubscribe(self, publisher: Publisher) -> None:
        publisher.unsubscribe(self)
