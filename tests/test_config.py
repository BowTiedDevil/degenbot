import pytest
import web3

from degenbot.config import get_web3, set_web3, web3_connection_manager
from degenbot.exceptions import DegenbotError


def test_disconnected_web3():
    w3 = web3.Web3(web3.HTTPProvider("https://google.com"))
    with pytest.raises(DegenbotError, match="Web3 object is not connected."):
        set_web3(w3)


def test_legacy_interface(ethereum_archive_node_web3: web3.Web3):
    set_web3(ethereum_archive_node_web3)
    assert get_web3() is ethereum_archive_node_web3
    assert get_web3(ethereum_archive_node_web3.eth.chain_id) is ethereum_archive_node_web3
    with pytest.raises(DegenbotError):
        get_web3(chain_id=0)


def test_connection_manager(ethereum_archive_node_web3: web3.Web3):
    set_web3(ethereum_archive_node_web3)
    assert (
        web3_connection_manager.get(ethereum_archive_node_web3.eth.chain_id)
        is ethereum_archive_node_web3
    )
