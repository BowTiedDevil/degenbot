import logging
from typing import Any

import dotenv
import pytest
import web3

import degenbot
import degenbot.config
import degenbot.logging
import degenbot.managers
import degenbot.managers.erc20_token_manager
import degenbot.registry
import degenbot.types
import degenbot.uniswap.managers
from degenbot.anvil_fork import AnvilFork
from degenbot.registry.all_pools import pool_registry
from degenbot.registry.all_tokens import token_registry

env_file = dotenv.find_dotenv("tests.env")
env_values = dotenv.dotenv_values(env_file)

ARBITRUM_ARCHIVE_NODE_HTTP_URI = f"https://rpc.ankr.com/arbitrum/{env_values['ANKR_API_KEY']}"

ETHEREUM_ARCHIVE_NODE_HTTP_URI = "http://localhost:8545"
ETHEREUM_ARCHIVE_NODE_WS_URI = "ws://localhost:8546"

BASE_FULL_NODE_HTTP_URI = "http://localhost:8544"
BASE_FULL_NODE_WS_URI = "ws://localhost:8547"


# Set up a web3 connection to a Base full node
@pytest.fixture(scope="session")
def base_full_node_web3() -> web3.Web3:
    return web3.Web3(web3.LegacyWebSocketProvider(BASE_FULL_NODE_WS_URI))


# Set up a web3 connection to an Arbitrum archive node
@pytest.fixture(scope="session")
def arbitrum_archive_node_web3() -> web3.Web3:
    return web3.Web3(web3.HTTPProvider(ARBITRUM_ARCHIVE_NODE_HTTP_URI))


# Set up a web3 connection to an Ethereum archive node
@pytest.fixture(scope="session")
def ethereum_archive_node_web3() -> web3.Web3:
    return web3.Web3(web3.LegacyWebSocketProvider(ETHEREUM_ARCHIVE_NODE_WS_URI))


@pytest.fixture(autouse=True)
def _initialize_and_reset_after_each_test():
    """
    After each test, clear and reset global values & singletons to a fresh state
    """
    yield  # the fixture will pause here until the test completes
    degenbot.config.connection_manager.connections.clear()
    degenbot.config.connection_manager._default_chain_id = None
    degenbot.managers.erc20_token_manager.Erc20TokenManager._state.clear()
    degenbot.types.AbstractPoolManager.instances.clear()
    pool_registry._all_pools.clear()
    token_registry._all_tokens.clear()


@pytest.fixture(scope="session", autouse=True)
def _set_degenbot_logging():
    degenbot.logging.logger.setLevel(logging.DEBUG)


@pytest.fixture
def fork_base() -> AnvilFork:
    return AnvilFork(fork_url=BASE_FULL_NODE_WS_URI, storage_caching=False)


@pytest.fixture
def fork_mainnet() -> AnvilFork:
    return AnvilFork(fork_url=ETHEREUM_ARCHIVE_NODE_WS_URI, storage_caching=False)


@pytest.fixture
def fork_arbitrum() -> AnvilFork:
    return AnvilFork(fork_url=ARBITRUM_ARCHIVE_NODE_HTTP_URI, storage_caching=False)


class FakeSubscriber:
    """
    This subscriber class provides a record of received messages, and can be used to test that
    publisher/subscriber methods operate as expected.
    """

    def __init__(self) -> None:
        self.inbox: list[dict[str, Any]] = list()

    def notify(self, publisher: degenbot.types.Publisher, message: degenbot.types.Message) -> None:
        self.inbox.append(
            {
                "from": publisher,
                "message": message,
            }
        )

    def subscribe(self, publisher: degenbot.types.Publisher) -> None:
        publisher.subscribe(self)

    def unsubscribe(self, publisher: degenbot.types.Publisher) -> None:
        publisher.unsubscribe(self)
