import pytest
import web3
from eth_utils.address import to_checksum_address

from degenbot.config import set_web3
from degenbot.constants import ZERO_ADDRESS
from degenbot.manager.token_manager import Erc20TokenHelperManager
from degenbot.registry.all_tokens import AllTokens

WETH_ADDRESS = to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
WBTC_ADDRESS = to_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
ETHER_PLACEHOLDER_ADDRESS = to_checksum_address("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE")


def test_get_erc20tokens(ethereum_archive_node_web3: web3.Web3):
    set_web3(ethereum_archive_node_web3)
    token_manager = Erc20TokenHelperManager(chain_id=ethereum_archive_node_web3.eth.chain_id)
    token_registry = AllTokens(chain_id=ethereum_archive_node_web3.eth.chain_id)

    weth = token_manager.get_erc20token(address=WETH_ADDRESS)
    assert weth.symbol == "WETH"
    assert weth.address == WETH_ADDRESS
    assert token_manager.get_erc20token(WETH_ADDRESS) is weth
    assert token_manager.get_erc20token(WETH_ADDRESS.lower()) is weth
    assert token_manager.get_erc20token(WETH_ADDRESS.upper()) is weth
    assert token_registry.get(WETH_ADDRESS) is weth

    wbtc = token_manager.get_erc20token(address=WBTC_ADDRESS)
    assert wbtc.symbol == "WBTC"
    assert wbtc.address == WBTC_ADDRESS
    assert token_manager.get_erc20token(WBTC_ADDRESS) is wbtc
    assert token_manager.get_erc20token(WBTC_ADDRESS.lower()) is wbtc
    assert token_manager.get_erc20token(WBTC_ADDRESS.upper()) is wbtc
    assert token_registry.get(WBTC_ADDRESS) is wbtc


def test_get_bad_token(ethereum_archive_node_web3: web3.Web3):
    set_web3(ethereum_archive_node_web3)
    token_manager = Erc20TokenHelperManager(chain_id=ethereum_archive_node_web3.eth.chain_id)
    with pytest.raises(ValueError):
        token_manager.get_erc20token(address=ZERO_ADDRESS)


def test_get_ether_placeholder(ethereum_archive_node_web3: web3.Web3):
    set_web3(ethereum_archive_node_web3)
    token_manager = Erc20TokenHelperManager(chain_id=ethereum_archive_node_web3.eth.chain_id)
    token_registry = AllTokens(chain_id=ethereum_archive_node_web3.eth.chain_id)

    ether_placeholder = token_manager.get_erc20token(address=ETHER_PLACEHOLDER_ADDRESS)
    assert ether_placeholder.symbol == "ETH"
    assert ether_placeholder.address == ETHER_PLACEHOLDER_ADDRESS
    assert token_manager.get_erc20token(ETHER_PLACEHOLDER_ADDRESS) is ether_placeholder
    assert token_manager.get_erc20token(ETHER_PLACEHOLDER_ADDRESS.lower()) is ether_placeholder
    assert token_manager.get_erc20token(ETHER_PLACEHOLDER_ADDRESS.upper()) is ether_placeholder
    assert token_registry.get(ETHER_PLACEHOLDER_ADDRESS) is ether_placeholder
    assert token_registry.get(ETHER_PLACEHOLDER_ADDRESS.lower()) is ether_placeholder
    assert token_registry.get(ETHER_PLACEHOLDER_ADDRESS.upper()) is ether_placeholder
