import pytest
from degenbot.config import set_web3
from degenbot.erc20_token import EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE, Erc20Token
from degenbot.fork.anvil_fork import AnvilFork
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes

VITALIK_ADDRESS = to_checksum_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
WETH_ADDRESS = to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
WBTC_ADDRESS = to_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")


@pytest.fixture
def wbtc(ethereum_full_node_web3):
    set_web3(ethereum_full_node_web3)
    return Erc20Token(WBTC_ADDRESS)


@pytest.fixture
def weth(ethereum_full_node_web3):
    set_web3(ethereum_full_node_web3)
    return Erc20Token(WETH_ADDRESS)


def test_erc20token_comparisons(wbtc, weth):
    assert weth != wbtc

    assert weth == WETH_ADDRESS
    assert weth == WETH_ADDRESS.lower()
    assert weth == WETH_ADDRESS.upper()
    assert weth == to_checksum_address(WETH_ADDRESS)
    assert weth == HexBytes(WETH_ADDRESS)

    assert wbtc == WBTC_ADDRESS
    assert wbtc == WBTC_ADDRESS.lower()
    assert wbtc == WBTC_ADDRESS.upper()
    assert wbtc == to_checksum_address(WBTC_ADDRESS)
    assert wbtc == HexBytes(WBTC_ADDRESS)

    assert weth > wbtc
    assert weth > WBTC_ADDRESS
    assert weth > WBTC_ADDRESS.lower()
    assert weth > WBTC_ADDRESS.upper()
    assert weth > to_checksum_address(WBTC_ADDRESS)
    assert weth > HexBytes(WBTC_ADDRESS)

    assert wbtc < weth
    assert wbtc < WETH_ADDRESS
    assert wbtc < WETH_ADDRESS.lower()
    assert wbtc < WETH_ADDRESS.upper()
    assert wbtc < to_checksum_address(WETH_ADDRESS)
    assert wbtc < HexBytes(WETH_ADDRESS)


def test_non_compliant_tokens(ethereum_full_node_web3):
    set_web3(ethereum_full_node_web3)
    for token_address in [
        "0x043942281890d4876D26BD98E2BB3F662635DFfb",
        "0x1da4858ad385cc377165A298CC2CE3fce0C5fD31",
        "0x9A2548335a639a58F4241b85B5Fc6c57185C428A",
        "0xC19B6A4Ac7C7Cc24459F08984Bbd09664af17bD1",
        "0xC36442b4a4522E871399CD717aBDD847Ab11FE88",
        "0xf5BF148Be50f6972124f223215478519A2787C8E",
        "0xfCf163B5C68bE47f702432F0f54B58Cd6E18D10B",
        "0x431ad2ff6a9C365805eBaD47Ee021148d6f7DBe0",
        "0x89d24A6b4CcB1B6fAA2625fE562bDD9a23260359",
        "0xEB9951021698B42e4399f9cBb6267Aa35F82D59D",
        "0x9f8F72aA9304c8B593d555F12eF6589cC3A579A2",
    ]:
        Erc20Token(token_address)


def test_erc20token_with_price_feed(ethereum_full_node_web3):
    set_web3(ethereum_full_node_web3)
    Erc20Token(
        address="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        oracle_address="0x5f4ec3df9cbd43714fe2740f5e3616155c5b8419",
    )


def test_erc20token_functions(ethereum_full_node_web3):
    set_web3(ethereum_full_node_web3)
    weth = Erc20Token(
        address="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        oracle_address="0x5f4ec3df9cbd43714fe2740f5e3616155c5b8419",
    )
    weth.get_total_supply()
    weth.get_approval(VITALIK_ADDRESS, weth.address)
    weth.get_balance(VITALIK_ADDRESS)
    weth.update_price()


def test_ether_placeholder(ethereum_full_node_web3):
    set_web3(ethereum_full_node_web3)
    ether = EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE()
    ether.get_balance(VITALIK_ADDRESS)
