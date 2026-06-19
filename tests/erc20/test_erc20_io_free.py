"""Tests for I/O-free Erc20Token extraction (Phase 2)."""

import pathlib
from unittest.mock import MagicMock

import eth_abi.abi

from degenbot.bot import Bot
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.database.operations import create_new_sqlite_database
from degenbot.degenbot_rs import PyBot
from degenbot.erc20 import Erc20Token
from tests.helpers.erc20_factory import make_erc20, make_ether_placeholder

_PY_BOT = PyBot()


def _make_test_config(tmp_path: pathlib.Path) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: "https://eth.llamarpc.com/"},
    )


class TestErc20TokenDataOnlyConstructor:
    """Erc20Token companion reads metadata through the PyErc20Token handle."""

    def test_constructor_with_data(self) -> None:
        token = make_erc20(
            _PY_BOT,
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            chain_id=1,
            name="Wrapped Ether",
            symbol="WETH",
            decimals=18,
        )
        assert token.name == "Wrapped Ether"
        assert token.symbol == "WETH"
        assert token.decimals == 18
        assert token.chain_id == 1
        assert token.address == "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

    def test_constructor_normalizes_address(self) -> None:
        token = make_erc20(
            _PY_BOT,
            "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
            chain_id=1,
            name="Wrapped Ether",
            symbol="WETH",
            decimals=18,
        )
        assert token.address == "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

    def test_constructor_defaults_chain_id(self) -> None:
        """make_erc20 defaults chain_id to 1 (the standard test chain)."""
        token = make_erc20(
            _PY_BOT,
            "0x6810e776880C02933D47DB1b9fc05908e5386b96",
            name="Gnosis",
            symbol="GNO",
            decimals=18,
        )
        assert token.chain_id == 1

    def test_cache_accessors_balance(self) -> None:
        token = make_erc20(
            _PY_BOT,
            "0x6B175474E89094C44Da98b954EedeAC495271d0F",
            chain_id=1,
            name="DAI",
            symbol="DAI",
            decimals=18,
        )
        # No cached value initially
        assert token.get_cached_balance("0x" + "11" * 20, block_number=100) is None

        # Set and retrieve
        token.set_cached_balance("0x" + "11" * 20, block_number=100, balance=10**18)
        assert token.get_cached_balance("0x" + "11" * 20, block_number=100) == 10**18

    def test_cache_accessors_approval(self) -> None:
        token = make_erc20(
            _PY_BOT,
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            chain_id=1,
            name="USD Coin",
            symbol="USDC",
            decimals=6,
        )
        owner = "0x" + "11" * 20
        spender = "0x" + "22" * 20

        assert token.get_cached_approval(block_number=100, owner=owner, spender=spender) is None

        token.set_cached_approval(block_number=100, owner=owner, spender=spender, amount=500)
        assert token.get_cached_approval(block_number=100, owner=owner, spender=spender) == 500

    def test_cache_accessors_total_supply(self) -> None:
        token = make_erc20(
            _PY_BOT,
            "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            chain_id=1,
            name="Tether USD",
            symbol="USDT",
            decimals=6,
        )
        assert token.get_cached_total_supply(block_number=100) is None

        token.set_cached_total_supply(block_number=100, total_supply=10**27)
        assert token.get_cached_total_supply(block_number=100) == 10**27


class TestBotBuildErc20Token:
    """Bot.build_erc20token() fetches metadata and constructs the companion."""

    def test_build_token_from_chain(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        create_new_sqlite_database(config.database.path)
        bot = Bot(config)

        # Mock the provider to return token metadata
        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        provider.get_code.return_value = b"\x01"  # contract exists

        # Mock batched RPC call to return name, symbol, decimals
        def mock_call(*, to, data, block=None):
            if data[:4] == b"\x06\xfd\xde\x03":  # name()
                return eth_abi_encode(["string"], ["Wrapped Ether"])
            if data[:4] == b"\x95\xd8\x9b\x41":  # symbol()
                return eth_abi_encode(["string"], ["WETH"])
            if data[:4] == b"\x31\x3c\xe5\x67":  # decimals()
                return eth_abi_encode(["uint256"], [18])
            return b""

        def eth_abi_encode(types, args):
            return eth_abi.abi.encode(types=types, args=args)

        provider.call.side_effect = mock_call
        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        token = bot.build_erc20token(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        )
        assert isinstance(token, Erc20Token)
        assert token.chain_id == 1
        # Token should be registered in bot's registry
        assert bot.tokens.get(token_address=token.address, chain_id=1) is token


class TestEtherPlaceholderDataOnly:
    """EtherPlaceholder delegates metadata through the PyErc20Token handle."""

    def test_constructor_with_data(self) -> None:
        placeholder = make_ether_placeholder(
            _PY_BOT,
            "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE",
            chain_id=1,
        )
        assert placeholder.chain_id == 1
        assert placeholder.symbol == "ETH"
        assert placeholder.name == "Ether Placeholder"
        assert placeholder.decimals == 18


class TestBotTokenIOMethods:
    """Bot.get_token_balance/approval/total_supply use cache + RPC."""

    def test_get_token_balance_cache_hit(self, tmp_path: pathlib.Path) -> None:
        """Balance returned from cache without RPC call."""
        token = make_erc20(
            _PY_BOT,
            "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
            chain_id=1,
            name="Wrapped BTC",
            symbol="WBTC",
            decimals=8,
        )
        holder = "0x" + "11" * 20
        token.set_cached_balance(holder, block_number=100, balance=10**18)

        # Create a Bot with a mock provider — it should never be called
        config = DegenbotConfig(
            database=DatabaseSettings(path=tmp_path / "test-bot-io"),
            rpc={1: "https://eth.llamarpc.com/"},
        )
        bot = Bot(config)
        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        balance = bot.get_token_balance(token, holder, block_identifier=100)
        assert balance == 10**18
        provider.call.assert_not_called()

    def test_get_token_balance_cache_miss(self, tmp_path: pathlib.Path) -> None:
        """Balance fetched from chain on cache miss, then cached."""
        token = make_erc20(
            _PY_BOT,
            "0xDe30239bB7673E3021A8cF8c6B7Df7Af60e9C0d6",
            chain_id=1,
            name="Golem",
            symbol="GLM",
            decimals=18,
        )
        holder = "0x" + "11" * 20

        config = DegenbotConfig(
            database=DatabaseSettings(path=tmp_path / "test-bot-io2"),
            rpc={1: "https://eth.llamarpc.com/"},
        )
        bot = Bot(config)
        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 200

        # Mock provider.call to return a balance
        encoded_balance = eth_abi.abi.encode(types=["uint256"], args=[5 * 10**18])
        provider.call.return_value = encoded_balance

        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        balance = bot.get_token_balance(token, holder)
        assert balance == 5 * 10**18
        # Now cached
        assert token.get_cached_balance(holder, block_number=200) == 5 * 10**18

    def test_get_token_approval_cache_hit(self, tmp_path: pathlib.Path) -> None:
        token = make_erc20(
            _PY_BOT,
            "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",
            chain_id=1,
            name="Uniswap",
            symbol="UNI",
            decimals=18,
        )
        owner = "0x" + "11" * 20
        spender = "0x" + "22" * 20
        token.set_cached_approval(block_number=100, owner=owner, spender=spender, amount=500)

        config = DegenbotConfig(
            database=DatabaseSettings(path=tmp_path / "test-bot-io3"),
            rpc={1: "https://eth.llamarpc.com/"},
        )
        bot = Bot(config)
        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        approval = bot.get_token_approval(token, owner, spender, block_identifier=100)
        assert approval == 500
        provider.call.assert_not_called()

    def test_get_token_total_supply_cache_hit(self, tmp_path: pathlib.Path) -> None:
        token = make_erc20(
            _PY_BOT,
            "0x514910771AF9Ca656af840dff83E8264EcF986CA",
            chain_id=1,
            name="Chainlink",
            symbol="LINK",
            decimals=18,
        )
        token.set_cached_total_supply(block_number=100, total_supply=10**27)

        config = DegenbotConfig(
            database=DatabaseSettings(path=tmp_path / "test-bot-io4"),
            rpc={1: "https://eth.llamarpc.com/"},
        )
        bot = Bot(config)
        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        supply = bot.get_token_total_supply(token, block_identifier=100)
        assert supply == 10**27
        provider.call.assert_not_called()
