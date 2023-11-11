import pytest
import web3
from degenbot import set_web3
from degenbot.manager import Erc20TokenHelperManager
from eth_utils.address import to_checksum_address

WETH_ADDRESS = to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
WBTC_ADDRESS = to_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")


# Set up a web3 connection to Ankr endpoint
@pytest.fixture
def ankr_archive_web3() -> web3.Web3:
    w3 = web3.Web3(web3.HTTPProvider("https://rpc.ankr.com/eth"))
    return w3


def test_create_erc20tokenmanager(ankr_archive_web3: web3.Web3):
    set_web3(ankr_archive_web3)
    Erc20TokenHelperManager(chain_id=ankr_archive_web3.eth.chain_id)


def test_get_erc20tokens(ankr_archive_web3: web3.Web3):
    set_web3(ankr_archive_web3)
    token_manager = Erc20TokenHelperManager(chain_id=ankr_archive_web3.eth.chain_id)

    weth = token_manager.get_erc20token(address=WETH_ADDRESS)
    assert weth.symbol == "WETH"
    assert weth.address == WETH_ADDRESS

    wbtc = token_manager.get_erc20token(address=WBTC_ADDRESS)
    assert wbtc.symbol == "WBTC"
    assert wbtc.address == WBTC_ADDRESS
