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
import degenbot.types
from degenbot.anvil_fork import AnvilFork
from degenbot.registry.all_pools import pool_registry
from degenbot.registry.all_tokens import token_registry

env_file = dotenv.find_dotenv("tests.env")
env_values = dotenv.dotenv_values(env_file)

ARBITRUM_FULL_NODE_HTTP_URI = "https://arbitrum-one-rpc.publicnode.com"
ARBITRUM_FULL_NODE_WS_URI = "wss://arbitrum-one-rpc.publicnode.com"

ETHEREUM_ARCHIVE_NODE_HTTP_URI = "https://ethereum-rpc.publicnode.com"
ETHEREUM_ARCHIVE_NODE_WS_URI = "wss://ethereum-rpc.publicnode.com"
ETHEREUM_FULL_NODE_HTTP_URI = "http://localhost:8545"
ETHEREUM_FULL_NODE_WS_URI = "ws://localhost:8546"

BASE_ARCHIVE_NODE_HTTP_URI = "https://base-rpc.publicnode.com"
BASE_ARCHIVE_NODE_WS_URI = "wss://base-rpc.publicnode.com"
BASE_FULL_NODE_HTTP_URI = "http://localhost:8544"
BASE_FULL_NODE_WS_URI = "ws://localhost:8548"


# Set up a web3 connection to an Arbitrum full node
@pytest.fixture(scope="session")
def arbitrum_full_node_web3() -> web3.Web3:
    return web3.Web3(web3.LegacyWebSocketProvider(ARBITRUM_FULL_NODE_WS_URI))


# Set up a web3 connection to a Base archive node
@pytest.fixture(scope="session")
def base_archive_node_web3() -> web3.Web3:
    return web3.Web3(web3.LegacyWebSocketProvider(BASE_ARCHIVE_NODE_WS_URI))


# Set up a web3 connection to a Base full node
@pytest.fixture(scope="session")
def base_full_node_web3() -> web3.Web3:
    return web3.Web3(web3.LegacyWebSocketProvider(BASE_FULL_NODE_WS_URI))


# Set up a web3 connection to an Ethereum archive node
@pytest.fixture(scope="session")
def ethereum_archive_node_web3() -> web3.Web3:
    return web3.Web3(web3.LegacyWebSocketProvider(ETHEREUM_ARCHIVE_NODE_WS_URI))


# Set up a web3 connection to an Ethereum full node
@pytest.fixture(scope="session")
def ethereum_full_node_web3() -> web3.Web3:
    return web3.Web3(web3.LegacyWebSocketProvider(ETHEREUM_FULL_NODE_WS_URI))


# Set up an async web3 connection to an Ethereum archive node
@pytest.fixture
async def ethereum_archive_node_async_web3() -> web3.AsyncWeb3:
    async_w3: web3.AsyncWeb3 = await web3.AsyncWeb3(
        web3.WebSocketProvider(ETHEREUM_ARCHIVE_NODE_WS_URI)
    )
    return async_w3


@pytest.fixture(autouse=True)
def _initialize_and_reset_after_each_test():
    """
    Before each test, clear/reset global values and singletons
    """
    degenbot.config.connection_manager.connections.clear()
    degenbot.config.connection_manager._default_chain_id = None
    degenbot.managers.erc20_token_manager.Erc20TokenManager._state.clear()
    pool_registry._all_pools.clear()
    pool_registry._v4_pool_registry._all_v4_pools.clear()
    token_registry._all_tokens.clear()


@pytest.fixture(scope="session", autouse=True)
def _set_degenbot_logging():
    """
    Set the logging level to DEBUG for the test run
    """
    degenbot.logging.logger.setLevel(logging.DEBUG)


@pytest.fixture
def fork_arbitrum_full() -> AnvilFork:
    return AnvilFork(fork_url=ARBITRUM_FULL_NODE_WS_URI)


@pytest.fixture
def fork_base_archive() -> AnvilFork:
    return AnvilFork(
        fork_url=BASE_ARCHIVE_NODE_WS_URI,
        ipc_provider_kwargs={
            "timeout": 300,
        },
    )


@pytest.fixture
def fork_base_full() -> AnvilFork:
    return AnvilFork(fork_url=BASE_FULL_NODE_WS_URI)


@pytest.fixture
def fork_mainnet_archive() -> AnvilFork:
    return AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_WS_URI,
        ipc_provider_kwargs={
            "timeout": 300,
        },
    )


@pytest.fixture
def fork_mainnet_full() -> AnvilFork:
    return AnvilFork(fork_url=ETHEREUM_FULL_NODE_WS_URI)


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
