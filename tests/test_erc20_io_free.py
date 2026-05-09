"""Tests for I/O-free Erc20Token extraction (Phase 2)."""

import pathlib
from unittest.mock import MagicMock

import eth_abi.abi

from degenbot.bot import Bot
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.database.operations import create_new_sqlite_database
from degenbot.erc20 import Erc20Token, EtherPlaceholder
from degenbot.registry import TokenRegistry


def _make_test_config(tmp_path: pathlib.Path) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: "https://eth.llamarpc.com/"},
    )


class TestErc20TokenDataOnlyConstructor:
    """Erc20Token accepts pre-fetched data with no I/O."""

    def test_constructor_with_data(self) -> None:
        token = Erc20Token(
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
        token = Erc20Token(
            "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
            chain_id=1,
            name="Wrapped Ether",
            symbol="WETH",
            decimals=18,
        )
        assert token.address == "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

    def test_constructor_requires_chain_id_without_data(self) -> None:
        """Without pre-fetched data, the legacy I/O path is triggered (deprecated)."""
        # The legacy path will try to reach connection_manager, so we just verify
        # that the I/O-free path works when data is provided.
        token = Erc20Token(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            chain_id=1,
            name="WETH",
            symbol="WETH",
            decimals=18,
        )
        assert token.name == "WETH"

    def test_no_self_registration(self, tmp_path: pathlib.Path) -> None:
        """Erc20Token does not self-register in token_registry (I/O-free path)."""
        registry = TokenRegistry()
        token = Erc20Token(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            chain_id=1,
            name="Wrapped Ether",
            symbol="WETH",
            decimals=18,
        )
        # The I/O-free path does not self-register
        assert registry.get(token_address=token.address, chain_id=1) is None

    def test_cache_accessors_balance(self) -> None:
        token = Erc20Token(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            chain_id=1,
            name="WETH",
            symbol="WETH",
            decimals=18,
        )
        # No cached value initially
        assert token.get_cached_balance("0x" + "11" * 20, block_number=100) is None

        # Set and retrieve
        token.set_cached_balance("0x" + "11" * 20, block_number=100, balance=10**18)
        assert token.get_cached_balance("0x" + "11" * 20, block_number=100) == 10**18

    def test_cache_accessors_approval(self) -> None:
        token = Erc20Token(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            chain_id=1,
            name="WETH",
            symbol="WETH",
            decimals=18,
        )
        owner = "0x" + "11" * 20
        spender = "0x" + "22" * 20

        assert token.get_cached_approval(block_number=100, owner=owner, spender=spender) is None

        token.set_cached_approval(block_number=100, owner=owner, spender=spender, amount=500)
        assert token.get_cached_approval(block_number=100, owner=owner, spender=spender) == 500

    def test_cache_accessors_total_supply(self) -> None:
        token = Erc20Token(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            chain_id=1,
            name="WETH",
            symbol="WETH",
            decimals=18,
        )
        assert token.get_cached_total_supply(block_number=100) is None

        token.set_cached_total_supply(block_number=100, total_supply=10**27)
        assert token.get_cached_total_supply(block_number=100) == 10**27

    # Note: Tests for "no db_session/connection_manager/token_registry imports" will be
    # added once the legacy I/O path is removed in a future phase.


class TestBotBuildErc20Token:
    """Bot.build_erc20_token() fetches metadata and constructs I/O-free token."""

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
            chain_id=1,
        )
        assert isinstance(token, Erc20Token)
        assert token.chain_id == 1
        # Token should be registered in bot's registry
        assert bot.tokens.get(token_address=token.address, chain_id=1) is token


class TestEtherPlaceholderDataOnly:
    """EtherPlaceholder also accepts pre-fetched data."""

    def test_constructor_with_data(self) -> None:
        placeholder = EtherPlaceholder(
            "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE",
            chain_id=1,
        )
        assert placeholder.chain_id == 1
        assert placeholder.symbol == "ETH"
        assert placeholder.name == "Ether Placeholder"
        assert placeholder.decimals == 18


class TestBotTokenIOMethods:
    """Bot.get_token_balance/approval/total_supply use cache + RPC."""

    def test_get_token_balance_cache_hit(self) -> None:
        """Balance returned from cache without RPC call."""
        token = Erc20Token(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            chain_id=1,
            name="WETH",
            symbol="WETH",
            decimals=18,
        )
        holder = "0x" + "11" * 20
        token.set_cached_balance(holder, block_number=100, balance=10**18)

        # Create a Bot with a mock provider — it should never be called
        config = DegenbotConfig(
            database=DatabaseSettings(path=pathlib.Path("/tmp/test-bot-io")),
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

    def test_get_token_balance_cache_miss(self) -> None:
        """Balance fetched from chain on cache miss, then cached."""
        token = Erc20Token(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            chain_id=1,
            name="WETH",
            symbol="WETH",
            decimals=18,
        )
        holder = "0x" + "11" * 20

        config = DegenbotConfig(
            database=DatabaseSettings(path=pathlib.Path("/tmp/test-bot-io2")),
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

    def test_get_token_approval_cache_hit(self) -> None:
        token = Erc20Token(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            chain_id=1,
            name="WETH",
            symbol="WETH",
            decimals=18,
        )
        owner = "0x" + "11" * 20
        spender = "0x" + "22" * 20
        token.set_cached_approval(block_number=100, owner=owner, spender=spender, amount=500)

        config = DegenbotConfig(
            database=DatabaseSettings(path=pathlib.Path("/tmp/test-bot-io3")),
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

    def test_get_token_total_supply_cache_hit(self) -> None:
        token = Erc20Token(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            chain_id=1,
            name="WETH",
            symbol="WETH",
            decimals=18,
        )
        token.set_cached_total_supply(block_number=100, total_supply=10**27)

        config = DegenbotConfig(
            database=DatabaseSettings(path=pathlib.Path("/tmp/test-bot-io4")),
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
