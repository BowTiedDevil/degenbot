import pytest
import web3

from degenbot.anvil_fork import AnvilFork
from degenbot.connection import (
    AsyncConnectionManager,
    ConnectionManager,
)
from degenbot.exceptions import DegenbotValueError
from degenbot.provider import AsyncProviderAdapter, ProviderAdapter

from .conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI


def test_disconnected_web3():
    w3 = web3.Web3(web3.HTTPProvider("https://google.com"))
    cm = ConnectionManager()
    with pytest.raises(DegenbotValueError, match=r"Provider is not connected."):
        provider = ProviderAdapter.from_web3(w3)
        cm.register_provider(provider)


def test_connection_manager(fork_mainnet_full: AnvilFork):
    cm = ConnectionManager()
    with pytest.raises(DegenbotValueError):
        _ = cm.default_chain_id

    provider = ProviderAdapter.from_web3(fork_mainnet_full.w3)
    cm.register_provider(provider)
    cm.set_default_chain(provider.chain_id)
    assert cm.default_chain_id == fork_mainnet_full.w3.eth.chain_id
    # get_web3() is deprecated but still works for Web3 providers
    with pytest.warns(DeprecationWarning, match="get_web3"):
        assert cm.get_web3(fork_mainnet_full.w3.eth.chain_id) is fork_mainnet_full.w3

    with pytest.warns(DeprecationWarning, match="get_web3"), pytest.raises(DegenbotValueError):
        cm.get_web3(69)


def test_optimized_web3():
    w3 = web3.Web3(web3.HTTPProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI))
    middlewares = w3.middleware_onion.middleware
    cm = ConnectionManager()
    provider = ProviderAdapter.from_web3(w3)
    cm.register_provider(provider, optimize=True)
    assert w3.middleware_onion.middleware == []

    w3 = web3.Web3(web3.HTTPProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI))
    middlewares = w3.middleware_onion.middleware
    provider = ProviderAdapter.from_web3(w3)
    cm.register_provider(provider, optimize=False)
    # optimize=False preserves middleware
    assert w3.middleware_onion.middleware == middlewares


async def test_async_connection_manager(fork_mainnet_full: AnvilFork):
    async with fork_mainnet_full.async_w3() as async_w3:
        acm = AsyncConnectionManager()
        provider = AsyncProviderAdapter.from_web3(async_w3)
        await acm.register_provider(provider)
        acm.set_default_chain(await provider.get_chain_id())
        assert acm.default_chain_id == await async_w3.eth.chain_id
