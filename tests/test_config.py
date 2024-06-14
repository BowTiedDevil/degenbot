import pytest
import web3
from degenbot.config import set_web3
from degenbot.exceptions import DegenbotError


def test_disconnected_web3():
    w3 = web3.Web3(web3.HTTPProvider("https://google.com"))
    with pytest.raises(DegenbotError, match="Web3 object is not connected."):
        set_web3(w3)
