import pytest
import web3

from degenbot.config import connection_manager, get_web3, set_web3
from degenbot.exceptions import DegenbotValueError

from .conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI


def test_disconnected_web3():
    w3 = web3.Web3(web3.HTTPProvider("https://google.com"))
    with pytest.raises(DegenbotValueError, match="Web3 instance is not connected."):
        set_web3(w3)

    with pytest.raises(DegenbotValueError, match="Web3 instance is not connected."):
        connection_manager.register_web3(w3)


def test_legacy_interface(ethereum_archive_node_web3: web3.Web3):
    with pytest.raises(
        DegenbotValueError, match="A default Web3 instance has not been registered."
    ):
        get_web3()

    set_web3(ethereum_archive_node_web3)
    assert get_web3() is ethereum_archive_node_web3


def test_optimized_web3():
    w3 = web3.Web3(web3.HTTPProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI))
    middlewares = w3.middleware_onion.middleware
    set_web3(w3)
    assert w3.middleware_onion.middleware == []

    w3 = web3.Web3(web3.HTTPProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI))
    middlewares = w3.middleware_onion.middleware
    set_web3(w3, optimize_middleware=False)
    assert w3.middleware_onion.middleware == middlewares


def test_connection_manager(ethereum_archive_node_web3: web3.Web3):
    with pytest.raises(DegenbotValueError):
        _ = connection_manager.default_chain_id

    set_web3(ethereum_archive_node_web3)
    assert connection_manager.default_chain_id == ethereum_archive_node_web3.eth.chain_id

    assert (
        connection_manager.get_web3(ethereum_archive_node_web3.eth.chain_id)
        is ethereum_archive_node_web3
    )

    with pytest.raises(DegenbotValueError):
        connection_manager.get_web3(69)
