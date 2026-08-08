import pytest
from hexbytes import HexBytes

from degenbot.bot import Bot, PyBot
from degenbot.chainlink import ChainlinkPriceContract
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.infrastructure import NoPriceOracle
from degenbot.fork import AnvilFork
from tests.helpers.bot_factory import make_bot_with_provider
from tests.helpers.erc20_factory import make_erc20, make_ether_placeholder
from tests.standalone_anvil import seed as seed_catalog

_PY_BOT = PyBot()

VITALIK_ADDRESS = get_checksum_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
WBTC_ADDRESS = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")


def _wbtc() -> Erc20Token:
    return make_erc20(
        _PY_BOT,
        WBTC_ADDRESS,
        name="Wrapped BTC",
        symbol="WBTC",
        decimals=8,
        chain_id=1,
    )


def _weth() -> Erc20Token:
    return make_erc20(
        _PY_BOT,
        WETH_ADDRESS,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
        chain_id=1,
    )


@pytest.fixture
def wbtc() -> Erc20Token:
    return _wbtc()


@pytest.fixture
def weth() -> Erc20Token:
    return _weth()


def test_bad_address(standalone_anvil: AnvilFork):
    """A random (empty) address on the seeded chain has no code, so build raises."""
    bot = make_bot_with_provider(standalone_anvil.provider)
    random_address = get_checksum_address("0x1234567890123456789012345678901234567890")
    with pytest.raises(DegenbotValueError, match="No contract deployed at this address"):
        bot.build_erc20token(random_address)


def test_caches(wbtc: Erc20Token):
    # Test cache setters and getters (pure in-memory; no chain data needed).
    fake_balance = 69_420_000
    current_block = 100

    # Test balance cache
    wbtc.set_cached_balance(VITALIK_ADDRESS, current_block, fake_balance)
    assert wbtc.get_cached_balance(VITALIK_ADDRESS, current_block) == fake_balance
    wbtc._cached_balance.clear()
    assert wbtc.get_cached_balance(VITALIK_ADDRESS, current_block) is None

    # Test total supply cache
    fake_supply = 69_420_000_000
    wbtc.set_cached_total_supply(current_block, fake_supply)
    assert wbtc.get_cached_total_supply(current_block) == fake_supply
    wbtc._cached_total_supply.clear()
    assert wbtc.get_cached_total_supply(current_block) is None

    # Test approval cache
    wbtc.set_cached_approval(current_block, VITALIK_ADDRESS, VITALIK_ADDRESS, fake_balance)
    assert wbtc.get_cached_approval(current_block, VITALIK_ADDRESS, VITALIK_ADDRESS) == fake_balance


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


@pytest.mark.online_rpc
def test_non_compliant_tokens(bot_mainnet_full: Bot):
    """Non-compliant mainnet tokens can still be built.

    Genuinely requires the real mainnet contracts (a non-compliant token's
    name/symbol/decimals behaviour can't be reproduced off a seed), so it stays
    an on-demand live test.
    """
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
        token = bot_mainnet_full.build_erc20token(token_address)
        assert token.address == get_checksum_address(token_address)


def test_erc20token_with_price_feed(standalone_anvil: AnvilFork):
    """A price feed loads from the seeded mock Chainlink aggregator (no upstream)."""
    bot = make_bot_with_provider(standalone_anvil.provider)
    weth = _weth()
    # Set price oracle directly on the token
    weth._price_oracle = ChainlinkPriceContract(
        address=seed_catalog.CHAINLINK,
        bot=bot,
    )
    _ = weth.price


def test_erc20token_without_price_feed(weth: Erc20Token):
    with pytest.raises(NoPriceOracle):
        _ = weth.price


@pytest.mark.online_rpc
def test_ether_placeholder(fork_mainnet_full: AnvilFork):
    ether = make_ether_placeholder(_PY_BOT, ZERO_ADDRESS)

    fake_balance = 69_420_000
    current_block = fork_mainnet_full.provider.get_block_number()
    balance_actual = fork_mainnet_full.provider.get_balance(VITALIK_ADDRESS)
    ether.set_cached_balance(VITALIK_ADDRESS, current_block, fake_balance)
    balance_from_cache = ether.get_cached_balance(VITALIK_ADDRESS, current_block)
    assert balance_from_cache == fake_balance
    # clear cached balance and test reset
    ether._cached_balance.clear()
    assert ether.get_cached_balance(VITALIK_ADDRESS, current_block) is None
    ether.set_cached_balance(VITALIK_ADDRESS, current_block, balance_actual)
    assert ether.get_cached_balance(VITALIK_ADDRESS, current_block) == balance_actual
