from degenbot import Erc20Token, set_web3
from eth_utils import to_checksum_address
from hexbytes import HexBytes

VITALIK_ADDRESS = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"


class MockErc20Token(Erc20Token):
    def __init__(self):
        pass


def test_erc20token_comparisons():
    token0 = MockErc20Token()
    token0.address = to_checksum_address(WETH_ADDRESS)

    token1 = MockErc20Token()
    token1.address = to_checksum_address(WBTC_ADDRESS)

    assert token0 != token1

    assert token0 == WETH_ADDRESS
    assert token0 == WETH_ADDRESS.lower()
    assert token0 == WETH_ADDRESS.upper()
    assert token0 == to_checksum_address(WETH_ADDRESS)
    assert token0 == HexBytes(WETH_ADDRESS)

    assert token1 == WBTC_ADDRESS
    assert token1 == WBTC_ADDRESS.lower()
    assert token1 == WBTC_ADDRESS.upper()
    assert token1 == to_checksum_address(WBTC_ADDRESS)
    assert token1 == HexBytes(WBTC_ADDRESS)

    assert token0 > token1
    assert token0 > WBTC_ADDRESS
    assert token0 > WBTC_ADDRESS.lower()
    assert token0 > WBTC_ADDRESS.upper()
    assert token0 > to_checksum_address(WBTC_ADDRESS)
    assert token0 > HexBytes(WBTC_ADDRESS)

    assert token1 < token0
    assert token1 < WETH_ADDRESS
    assert token1 < WETH_ADDRESS.lower()
    assert token1 < WETH_ADDRESS.upper()
    assert token1 < to_checksum_address(WETH_ADDRESS)
    assert token1 < HexBytes(WETH_ADDRESS)


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
