import degenbot
import pytest
import web3
from degenbot.config import set_web3
from degenbot.erc20_token import Erc20Token
from degenbot.exceptions import DegenbotError


def test_disconnected_web3():
    w3 = web3.Web3(web3.HTTPProvider("https://google.com"))
    with pytest.raises(DegenbotError, match="Web3 object is not connected."):
        set_web3(w3)


def test_unset_web3():
    WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

    del degenbot.config._web3

    with pytest.raises(DegenbotError, match="A Web3 instance has not been provided."):
        Erc20Token(WETH_ADDRESS)
