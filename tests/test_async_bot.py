"""
Tests for Phase 7: AsyncBot with async build_* and I/O methods.
"""

import pathlib
from unittest.mock import AsyncMock

import eth_abi.abi
import pytest
from hexbytes import HexBytes
from web3 import Web3

from degenbot.async_bot import AsyncBot
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20.erc20 import Erc20Token
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool


def _make_test_config(tmp_path: pathlib.Path) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: "https://eth.llamarpc.com/"},
    )


def _make_weth() -> Erc20Token:
    return Erc20Token(
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        chain_id=1,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
    )


def _make_usdc() -> Erc20Token:
    return Erc20Token(
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        chain_id=1,
        name="USD Coin",
        symbol="USDC",
        decimals=6,
    )


WETH_ADDR = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC_ADDR = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
UNISWAP_V2_FACTORY = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
UNISWAP_V3_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
USDC_WETH_V2_POOL = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
USDC_WETH_V3_POOL = "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8"


class TestAsyncBotInit:
    """AsyncBot can be constructed with a config."""

    def test_init(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = AsyncBot(config)
        assert bot.config is config
        assert bot.pools is not None
        assert bot.tokens is not None
        assert bot.managed_pools is not None

    def test_from_config_file(self) -> None:
        # from_config_file() requires a real config file, just verify it exists
        assert hasattr(AsyncBot, "from_config_file")


class TestAsyncBotBuildErc20Token:
    """AsyncBot.build_erc20token() fetches token data async."""

    @pytest.mark.asyncio
    async def test_build_erc20token(self, tmp_path: pathlib.Path) -> None:

        config = _make_test_config(tmp_path)
        bot = AsyncBot(config)

        provider = AsyncMock()
        provider.get_chain_id.return_value = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        await bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        # Mock async RPC responses
        name_calldata = encode_function_calldata("name()", None)
        symbol_calldata = encode_function_calldata("symbol()", None)
        decimals_calldata = encode_function_calldata("decimals()", None)

        name_encoded = eth_abi.abi.encode(types=["string"], args=["Wrapped Ether"])
        symbol_encoded = eth_abi.abi.encode(types=["string"], args=["WETH"])
        decimals_encoded = eth_abi.abi.encode(types=["uint8"], args=[18])

        responses = {
            name_calldata: name_encoded,
            symbol_calldata: symbol_encoded,
            decimals_calldata: decimals_encoded,
        }

        async def mock_call(*, to, data, block=None):
            return responses[data]

        provider.call = AsyncMock(side_effect=mock_call)

        token = await bot.build_erc20token(WETH_ADDR, chain_id=1)

        assert isinstance(token, Erc20Token)
        assert token.address == get_checksum_address(WETH_ADDR)
        assert token.name == "Wrapped Ether"
        assert token.symbol == "WETH"
        assert token.decimals == 18

        # Token should be in bot's token registry
        assert bot.tokens.get(token_address=WETH_ADDR, chain_id=1) is token


class TestAsyncBotBuildV2Pool:
    """AsyncBot.build_v2_pool() fetches pool data async."""

    @pytest.mark.asyncio
    async def test_build_v2_pool(self, tmp_path: pathlib.Path) -> None:

        config = _make_test_config(tmp_path)
        bot = AsyncBot(config)

        provider = AsyncMock()
        provider.get_chain_id.return_value = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        await bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        # Pre-register tokens
        weth = _make_weth()
        usdc = _make_usdc()
        bot.tokens.add(token_address=WETH_ADDR, chain_id=1, token=weth)
        bot.tokens.add(token_address=USDC_ADDR, chain_id=1, token=usdc)

        # Mock responses
        factory_calldata = encode_function_calldata("factory()", None)
        token0_calldata = encode_function_calldata("token0()", None)
        token1_calldata = encode_function_calldata("token1()", None)
        reserves_calldata = encode_function_calldata("getReserves()", None)

        reserves_encoded = eth_abi.abi.encode(
            types=["uint256", "uint256", "uint256"],
            args=[1000000000000000000, 2000000000, 18_000_000],
        )

        responses = {
            factory_calldata: eth_abi.abi.encode(types=["address"], args=[UNISWAP_V2_FACTORY]),
            token0_calldata: eth_abi.abi.encode(types=["address"], args=[WETH_ADDR]),
            token1_calldata: eth_abi.abi.encode(types=["address"], args=[USDC_ADDR]),
            reserves_calldata: reserves_encoded,
        }

        async def mock_call(*, to, data, block=None):
            return responses[data]

        provider.call = AsyncMock(side_effect=mock_call)

        pool = await bot.build_v2_pool(pool_address=USDC_WETH_V2_POOL, chain_id=1)

        assert isinstance(pool, UniswapV2Pool)
        assert pool.address == get_checksum_address(USDC_WETH_V2_POOL)


class TestAsyncBotBuildV3Pool:
    """AsyncBot.build_v3_pool() fetches pool data async."""

    @pytest.mark.asyncio
    async def test_build_v3_pool(self, tmp_path: pathlib.Path) -> None:

        config = _make_test_config(tmp_path)
        bot = AsyncBot(config)

        provider = AsyncMock()
        provider.get_chain_id.return_value = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        await bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        # Pre-register tokens
        weth = _make_weth()
        usdc = _make_usdc()
        bot.tokens.add(token_address=WETH_ADDR, chain_id=1, token=weth)
        bot.tokens.add(token_address=USDC_ADDR, chain_id=1, token=usdc)

        # Mock responses
        sqrt_price = 2198666895605149686863
        tick = -76020
        liquidity = 1234567890
        fee = 3000
        tick_spacing = 60

        factory_calldata = encode_function_calldata("factory()", None)
        token0_calldata = encode_function_calldata("token0()", None)
        token1_calldata = encode_function_calldata("token1()", None)
        fee_calldata = encode_function_calldata("fee()", None)
        tick_spacing_calldata = encode_function_calldata("tickSpacing()", None)
        slot0_calldata = encode_function_calldata("slot0()", None)
        liquidity_calldata = encode_function_calldata("liquidity()", None)

        slot0_encoded = eth_abi.abi.encode(
            types=["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
            args=[sqrt_price, tick, 0, 0, 0, 0, False],
        )
        liquidity_encoded = eth_abi.abi.encode(types=["uint128"], args=[liquidity])

        tick_bitmap_selector = Web3.keccak(text="tickBitmap(int16)")[:4]

        responses = {
            factory_calldata: eth_abi.abi.encode(types=["address"], args=[UNISWAP_V3_FACTORY]),
            token0_calldata: eth_abi.abi.encode(types=["address"], args=[WETH_ADDR]),
            token1_calldata: eth_abi.abi.encode(types=["address"], args=[USDC_ADDR]),
            fee_calldata: eth_abi.abi.encode(types=["uint24"], args=[fee]),
            tick_spacing_calldata: eth_abi.abi.encode(types=["int24"], args=[tick_spacing]),
            slot0_calldata: slot0_encoded,
            liquidity_calldata: liquidity_encoded,
        }

        async def mock_call(*, to, data, block=None):
            if data in responses:
                return responses[data]
            if data[:4] == tick_bitmap_selector:
                return eth_abi.abi.encode(types=["uint256"], args=[0])
            msg = f"Unexpected call: data={data!r}"
            raise ValueError(msg)

        provider.call = AsyncMock(side_effect=mock_call)

        pool = await bot.build_v3_pool(pool_address=USDC_WETH_V3_POOL, chain_id=1)

        assert isinstance(pool, UniswapV3Pool)
        assert pool.address == get_checksum_address(USDC_WETH_V3_POOL)
        assert pool.sqrt_price_x96 == sqrt_price
        assert pool.tick == tick
        assert pool.liquidity == liquidity


class TestAsyncBotBuildV4Pool:
    """AsyncBot.build_v4_pool() fetches pool data async."""

    @pytest.mark.asyncio
    async def test_build_v4_pool(self, tmp_path: pathlib.Path) -> None:

        V4_POOL_MANAGER = "0x000000000004444c5dc75cB358380D2e3dE08A90"
        V4_STATE_VIEW = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"
        V4_POOL_ID = "0x21c67e77068de97969ba93d4aab21826d33ca12bb9f565d8496e8fda8a82ca27"
        V4_FEE = 500
        V4_TICK_SPACING = 10

        native_eth = Erc20Token(
            ZERO_ADDRESS,
            chain_id=1,
            name="Ether",
            symbol="ETH",
            decimals=18,
        )
        usdc = _make_usdc()

        config = _make_test_config(tmp_path)
        bot = AsyncBot(config)

        provider = AsyncMock()
        provider.get_chain_id.return_value = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        await bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        bot.tokens.add(token_address=ZERO_ADDRESS, chain_id=1, token=native_eth)
        bot.tokens.add(token_address=USDC_ADDR, chain_id=1, token=usdc)

        sqrt_price = 2198666895605149686863
        tick = -76020
        liquidity = 1234567890
        lp_fee = 500000

        slot0_calldata = encode_function_calldata("getSlot0(bytes32)", [HexBytes(V4_POOL_ID)])
        liquidity_calldata = encode_function_calldata(
            "getLiquidity(bytes32)", [HexBytes(V4_POOL_ID)]
        )
        tick_bitmap_selector = Web3.keccak(text="getTickBitmap(bytes32,int16)")[:4]

        slot0_encoded = eth_abi.abi.encode(
            types=["uint160", "int24", "uint24", "uint24"],
            args=[sqrt_price, tick, 0, lp_fee],
        )
        liquidity_encoded = eth_abi.abi.encode(types=["uint256"], args=[liquidity])

        responses = {
            slot0_calldata: slot0_encoded,
            liquidity_calldata: liquidity_encoded,
        }

        async def mock_call(*, to, data, block=None):
            if data in responses:
                return responses[data]
            if data[:4] == tick_bitmap_selector:
                return eth_abi.abi.encode(types=["uint256"], args=[0])
            msg = f"Unexpected call: data={data!r}"
            raise ValueError(msg)

        provider.call = AsyncMock(side_effect=mock_call)

        pool = await bot.build_v4_pool(
            pool_id=V4_POOL_ID,
            pool_manager_address=V4_POOL_MANAGER,
            state_view_address=V4_STATE_VIEW,
            chain_id=1,
            fee=V4_FEE,
            tick_spacing=V4_TICK_SPACING,
            hook_address=ZERO_ADDRESS,
            tokens=[ZERO_ADDRESS, USDC_ADDR],
        )

        assert isinstance(pool, UniswapV4Pool)
        assert pool.sqrt_price_x96 == sqrt_price


class TestAsyncBotIOMethods:
    """AsyncBot provides async I/O methods for token queries."""

    @pytest.mark.asyncio
    async def test_get_token_balance(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = AsyncBot(config)

        provider = AsyncMock()
        provider.get_chain_id.return_value = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        await bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        weth = _make_weth()
        bot.tokens.add(token_address=WETH_ADDR, chain_id=1, token=weth)

        # Mock balanceOf response
        holder_address = "0x1234567890123456789012345678901234567890"
        balance_calldata = encode_function_calldata("balanceOf(address)", [holder_address])
        balance_encoded = eth_abi.abi.encode(types=["uint256"], args=[1000000000])

        async def mock_call(*, to, data, block=None):
            if data == balance_calldata:
                return balance_encoded
            msg = f"Unexpected call: data={data!r}"
            raise ValueError(msg)

        provider.call = AsyncMock(side_effect=mock_call)

        balance = await bot.get_token_balance(
            token_address=WETH_ADDR,
            holder_address=holder_address,
            chain_id=1,
        )
        assert balance == 1000000000

    @pytest.mark.asyncio
    async def test_get_token_approval(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = AsyncBot(config)

        provider = AsyncMock()
        provider.get_chain_id.return_value = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        await bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        weth = _make_weth()
        bot.tokens.add(token_address=WETH_ADDR, chain_id=1, token=weth)

        # Mock allowance response
        owner_address = "0x1234567890123456789012345678901234567890"
        spender_address = "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd"
        approval_calldata = encode_function_calldata(
            "allowance(address,address)", [owner_address, spender_address]
        )
        approval_encoded = eth_abi.abi.encode(types=["uint256"], args=[500000000])

        async def mock_call(*, to, data, block=None):
            if data == approval_calldata:
                return approval_encoded
            msg = f"Unexpected call: data={data!r}"
            raise ValueError(msg)

        provider.call = AsyncMock(side_effect=mock_call)

        approval = await bot.get_token_approval(
            token_address=WETH_ADDR,
            owner=owner_address,
            spender=spender_address,
            chain_id=1,
        )
        assert approval == 500000000

    @pytest.mark.asyncio
    async def test_get_token_total_supply(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = AsyncBot(config)

        provider = AsyncMock()
        provider.get_chain_id.return_value = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        await bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        weth = _make_weth()
        bot.tokens.add(token_address=WETH_ADDR, chain_id=1, token=weth)

        # Mock totalSupply response
        total_supply_calldata = encode_function_calldata("totalSupply()", None)
        total_supply_encoded = eth_abi.abi.encode(types=["uint256"], args=[10**27])

        async def mock_call(*, to, data, block=None):
            if data == total_supply_calldata:
                return total_supply_encoded
            msg = f"Unexpected call: data={data!r}"
            raise ValueError(msg)

        provider.call = AsyncMock(side_effect=mock_call)

        total_supply = await bot.get_token_total_supply(
            token_address=WETH_ADDR,
            chain_id=1,
        )
        assert total_supply == 10**27
