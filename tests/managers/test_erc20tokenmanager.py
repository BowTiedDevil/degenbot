import pytest

from degenbot.anvil_fork import AnvilFork
from degenbot.cache import get_checksum_address
from degenbot.config import set_web3
from degenbot.exceptions import DegenbotValueError
from degenbot.managers.erc20_token_manager import Erc20TokenManager
from degenbot.registry.all_tokens import token_registry

WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
WBTC_ADDRESS = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
ETHER_PLACEHOLDER_ADDRESS = get_checksum_address("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE")


def test_get_erc20tokens(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)
    token_manager = Erc20TokenManager(chain_id=fork_mainnet_full.w3.eth.chain_id)

    weth = token_manager.get_erc20token(address=WETH_ADDRESS)
    assert weth.symbol == "WETH"
    assert weth.address == WETH_ADDRESS
    assert token_manager.get_erc20token(WETH_ADDRESS) is weth
    assert token_manager.get_erc20token(WETH_ADDRESS.lower()) is weth
    assert token_manager.get_erc20token(WETH_ADDRESS.upper()) is weth
    assert (
        token_registry.get(token_address=WETH_ADDRESS, chain_id=fork_mainnet_full.w3.eth.chain_id)
        is weth
    )

    wbtc = token_manager.get_erc20token(address=WBTC_ADDRESS)
    assert wbtc.symbol == "WBTC"
    assert wbtc.address == WBTC_ADDRESS
    assert token_manager.get_erc20token(WBTC_ADDRESS) is wbtc
    assert token_manager.get_erc20token(WBTC_ADDRESS.lower()) is wbtc
    assert token_manager.get_erc20token(WBTC_ADDRESS.upper()) is wbtc
    assert (
        token_registry.get(token_address=WBTC_ADDRESS, chain_id=fork_mainnet_full.w3.eth.chain_id)
        is wbtc
    )


def test_get_bad_token(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)
    token_manager = Erc20TokenManager(chain_id=fork_mainnet_full.w3.eth.chain_id)
    bad_token_address = "0x0000000000000000000000000000000000000001"
    with pytest.raises(DegenbotValueError):
        token_manager.get_erc20token(address=bad_token_address)


def test_get_ether_placeholder(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)
    token_manager = Erc20TokenManager(chain_id=fork_mainnet_full.w3.eth.chain_id)

    ether_placeholder = token_manager.get_erc20token(address=ETHER_PLACEHOLDER_ADDRESS)
    assert ether_placeholder.symbol == "ETH"
    assert ether_placeholder.address == ETHER_PLACEHOLDER_ADDRESS
    assert token_manager.get_erc20token(ETHER_PLACEHOLDER_ADDRESS) is ether_placeholder
    assert token_manager.get_erc20token(ETHER_PLACEHOLDER_ADDRESS.lower()) is ether_placeholder
    assert token_manager.get_erc20token(ETHER_PLACEHOLDER_ADDRESS.upper()) is ether_placeholder
    assert (
        token_registry.get(
            token_address=ETHER_PLACEHOLDER_ADDRESS,
            chain_id=fork_mainnet_full.w3.eth.chain_id,
        )
        is ether_placeholder
    )

    assert (
        token_registry.get(
            token_address=ETHER_PLACEHOLDER_ADDRESS.lower(),
            chain_id=fork_mainnet_full.w3.eth.chain_id,
        )
        is ether_placeholder
    )
    assert (
        token_registry.get(
            token_address=ETHER_PLACEHOLDER_ADDRESS.upper(),
            chain_id=fork_mainnet_full.w3.eth.chain_id,
        )
        is ether_placeholder
    )
