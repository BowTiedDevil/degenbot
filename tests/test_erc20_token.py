import pytest
from hexbytes import HexBytes
from web3 import AsyncWeb3, Web3

from degenbot.cache import get_checksum_address
from degenbot.config import async_connection_manager, set_web3
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20_token import Erc20Token, EtherPlaceholder
from degenbot.exceptions import DegenbotValueError, NoPriceOracle
from degenbot.types import BoundedCache

VITALIK_ADDRESS = get_checksum_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
WBTC_ADDRESS = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
CHAINLINK_WETH_PRICE_FEED = get_checksum_address("0x5f4ec3df9cbd43714fe2740f5e3616155c5b8419")


@pytest.fixture
def wbtc(ethereum_archive_node_web3: Web3) -> Erc20Token:
    set_web3(ethereum_archive_node_web3)
    return Erc20Token(WBTC_ADDRESS)


@pytest.fixture
def weth(ethereum_archive_node_web3: Web3) -> Erc20Token:
    set_web3(ethereum_archive_node_web3)
    return Erc20Token(WETH_ADDRESS)


def test_bad_address(ethereum_archive_node_web3):
    set_web3(ethereum_archive_node_web3)
    with pytest.raises(DegenbotValueError, match="No contract deployed at this address"):
        Erc20Token(VITALIK_ADDRESS)


def test_caches(ethereum_archive_node_web3: Web3, wbtc: Erc20Token):
    fake_balance = 69_420_000
    current_block = ethereum_archive_node_web3.eth.block_number
    balance_actual = wbtc.get_balance(VITALIK_ADDRESS)
    wbtc._cached_balance[VITALIK_ADDRESS] = BoundedCache(max_items=5)
    wbtc._cached_balance[VITALIK_ADDRESS][current_block] = fake_balance
    assert wbtc.get_balance(VITALIK_ADDRESS) == fake_balance
    wbtc._cached_balance.clear()
    assert wbtc.get_balance(VITALIK_ADDRESS) == balance_actual

    current_total_supply = wbtc.get_total_supply()
    fake_supply = 69_420_000_000
    wbtc._cached_total_supply[current_block] = fake_supply
    assert wbtc.get_total_supply() == fake_supply
    wbtc._cached_total_supply.clear()
    assert wbtc.get_total_supply() == current_total_supply

    wbtc.get_approval(VITALIK_ADDRESS, VITALIK_ADDRESS)
    wbtc.get_approval(VITALIK_ADDRESS, VITALIK_ADDRESS)


def test_erc20token_comparisons(wbtc: Erc20Token, weth: Erc20Token):
    with pytest.raises(AssertionError):
        assert weth == 69

    with pytest.raises(TypeError):
        assert weth < 69

    with pytest.raises(TypeError):
        assert weth > 69

    assert weth != wbtc

    assert weth == WETH_ADDRESS
    assert weth == WETH_ADDRESS.lower()
    assert weth == WETH_ADDRESS.upper()
    assert weth == get_checksum_address(WETH_ADDRESS)
    assert weth == HexBytes(WETH_ADDRESS)
    assert weth == bytes.fromhex(WETH_ADDRESS[2:])

    assert wbtc == WBTC_ADDRESS
    assert wbtc == WBTC_ADDRESS.lower()
    assert wbtc == WBTC_ADDRESS.upper()
    assert wbtc == get_checksum_address(WBTC_ADDRESS)
    assert wbtc == HexBytes(WBTC_ADDRESS)

    assert weth > wbtc
    assert weth > WBTC_ADDRESS
    assert weth > WBTC_ADDRESS.lower()
    assert weth > WBTC_ADDRESS.upper()
    assert weth > get_checksum_address(WBTC_ADDRESS)
    assert weth > HexBytes(WBTC_ADDRESS)
    assert weth > bytes.fromhex(WBTC_ADDRESS[2:])

    assert wbtc < weth
    assert wbtc < WETH_ADDRESS
    assert wbtc < WETH_ADDRESS.lower()
    assert wbtc < WETH_ADDRESS.upper()
    assert wbtc < get_checksum_address(WETH_ADDRESS)
    assert wbtc < HexBytes(WETH_ADDRESS)
    assert wbtc < bytes.fromhex(WETH_ADDRESS[2:])


def test_non_compliant_tokens(ethereum_archive_node_web3: Web3):
    set_web3(ethereum_archive_node_web3)
    for token_address in [
        "0x0d88eD6E74bbFD96B831231638b66C05571e824F",
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


def test_erc20token_with_price_feed(ethereum_archive_node_web3: Web3):
    set_web3(ethereum_archive_node_web3)
    weth = Erc20Token(address=WETH_ADDRESS, oracle_address=CHAINLINK_WETH_PRICE_FEED)
    _ = weth.price


def test_erc20token_without_price_feed(ethereum_archive_node_web3: Web3, weth: Erc20Token):
    set_web3(ethereum_archive_node_web3)

    with pytest.raises(NoPriceOracle):
        _ = weth.price


def test_erc20token_functions(ethereum_archive_node_web3: Web3, weth: Erc20Token):
    set_web3(ethereum_archive_node_web3)
    weth.get_total_supply()
    weth.get_approval(VITALIK_ADDRESS, weth.address)
    weth.get_balance(VITALIK_ADDRESS)


async def test_async_erc20_functions(ethereum_archive_node_async_web3: AsyncWeb3, weth: Erc20Token):
    await async_connection_manager.register_web3(w3=ethereum_archive_node_async_web3)
    await weth.get_total_supply_async()
    await weth.get_approval_async(VITALIK_ADDRESS, weth.address)
    await weth.get_balance_async(VITALIK_ADDRESS)


def test_ether_placeholder(ethereum_archive_node_web3: Web3):
    set_web3(ethereum_archive_node_web3)
    ether = EtherPlaceholder(ZERO_ADDRESS)

    fake_balance = 69_420_000
    current_block = ethereum_archive_node_web3.eth.block_number
    balance_actual = ether.get_balance(VITALIK_ADDRESS)
    ether._cached_balance[VITALIK_ADDRESS] = BoundedCache(max_items=5)
    ether._cached_balance[VITALIK_ADDRESS][current_block] = fake_balance
    assert ether.get_balance(VITALIK_ADDRESS) == fake_balance
    ether._cached_balance.clear()
    assert ether.get_balance(VITALIK_ADDRESS) == balance_actual
