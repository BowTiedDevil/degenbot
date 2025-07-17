import pytest
import web3

from degenbot import (
    AnvilFork,
    async_connection_manager,
    connection_manager,
    get_web3,
    set_async_web3,
    set_web3,
)
from degenbot.exceptions import DegenbotValueError

from .conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI


def test_disconnected_web3():
    w3 = web3.Web3(web3.HTTPProvider("https://google.com"))
    with pytest.raises(DegenbotValueError, match="Web3 instance is not connected."):
        set_web3(w3)

    with pytest.raises(DegenbotValueError, match="Web3 instance is not connected."):
        connection_manager.register_web3(w3)


def test_legacy_interface(fork_mainnet_full: AnvilFork):
    with pytest.raises(
        DegenbotValueError, match="A default Web3 instance has not been registered."
    ):
        get_web3()

    set_web3(fork_mainnet_full.w3)
    assert get_web3() is fork_mainnet_full.w3


def test_optimized_web3():
    w3 = web3.Web3(web3.HTTPProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI))
    middlewares = w3.middleware_onion.middleware
    set_web3(w3)
    assert w3.middleware_onion.middleware == []

    w3 = web3.Web3(web3.HTTPProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI))
    middlewares = w3.middleware_onion.middleware
    set_web3(w3, optimize_middleware=False)
    assert w3.middleware_onion.middleware == middlewares


def test_connection_manager(fork_mainnet_full: AnvilFork):
    with pytest.raises(DegenbotValueError):
        _ = connection_manager.default_chain_id

    set_web3(fork_mainnet_full.w3)
    assert connection_manager.default_chain_id == fork_mainnet_full.w3.eth.chain_id
    assert connection_manager.get_web3(fork_mainnet_full.w3.eth.chain_id) is fork_mainnet_full.w3

    with pytest.raises(DegenbotValueError):
        connection_manager.get_web3(69)


async def test_async_connection_manager(fork_mainnet_full: AnvilFork):
    async with fork_mainnet_full.async_w3 as async_w3:
        await set_async_web3(async_w3)
        assert async_connection_manager.default_chain_id == await async_w3.eth.chain_id
        assert async_connection_manager.get_web3(await async_w3.eth.chain_id) is async_w3
