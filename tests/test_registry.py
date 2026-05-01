import pytest

from degenbot.anvil_fork import AnvilFork
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import set_web3
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.registry import managed_pool_registry, pool_registry, token_registry
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

UNISWAP_V2_WBTC_WETH_POOL = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")


class FakeUniswapV4Pool(AbstractLiquidityPool):
    """
    Minimal fake Uniswap V4 pool for testing.
    """

    def __init__(self, address: str, pool_id: str):
        self.address = address
        self.pool_id = pool_id
        self.name = f"FakeUniswapV4Pool-{address}"

    def __eq__(self, other: object) -> bool:
        if isinstance(other, FakeUniswapV4Pool):
            return self.address == other.address and self.pool_id == other.pool_id
        return False

    def __hash__(self) -> int:
        return hash(self.address + self.pool_id)


def test_singleton(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)

    new_pool_registry = type(pool_registry)()
    new_token_registry = type(token_registry)()

    assert new_pool_registry is not pool_registry
    assert new_token_registry is not token_registry


def test_adding_pool(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)
    lp = UniswapV2Pool(UNISWAP_V2_WBTC_WETH_POOL)
    assert (
        pool_registry.get(pool_address=lp.address, chain_id=fork_mainnet_full.w3.eth.chain_id) is lp
    )

    with pytest.raises(DegenbotValueError):
        pool_registry.add(
            pool_address=lp.address, chain_id=fork_mainnet_full.w3.eth.chain_id, pool=lp
        )


def test_deleting_pool(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)
    lp = UniswapV2Pool(UNISWAP_V2_WBTC_WETH_POOL)
    assert (
        pool_registry.get(pool_address=lp.address, chain_id=fork_mainnet_full.w3.eth.chain_id) is lp
    )
    pool_registry.remove(pool_address=lp.address, chain_id=fork_mainnet_full.w3.eth.chain_id)
    assert (
        pool_registry.get(pool_address=lp.address, chain_id=fork_mainnet_full.w3.eth.chain_id)
        is None
    )


def test_adding_token(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)
    weth = Erc20Token(WETH_ADDRESS)
    assert (
        token_registry.get(token_address=weth.address, chain_id=fork_mainnet_full.w3.eth.chain_id)
        is weth
    )

    with pytest.raises(DegenbotValueError):
        token_registry.add(
            token_address=weth.address, chain_id=fork_mainnet_full.w3.eth.chain_id, token=weth
        )


def test_deleting_token(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)
    weth = Erc20Token(WETH_ADDRESS)
    assert (
        token_registry.get(token_address=weth.address, chain_id=fork_mainnet_full.w3.eth.chain_id)
        is weth
    )
    token_registry.remove(token_address=weth.address, chain_id=fork_mainnet_full.w3.eth.chain_id)
    assert (
        token_registry.get(token_address=weth.address, chain_id=fork_mainnet_full.w3.eth.chain_id)
        is None
    )


def test_v4_pool_add_and_removal():
    fake_pool_manager_address = "0x1234567890123456789012345678901234567890"
    fake_pool_id = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"

    # Create mock V4 pool
    fake_pool = FakeUniswapV4Pool(
        address=fake_pool_manager_address,
        pool_id=fake_pool_id,
    )

    # Define V4 pool parameters
    chain_id = 1
    pool_id = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"

    # Add the V4 pool to the managed pool registry
    managed_pool_registry.add(
        pool=fake_pool,
        chain_id=chain_id,
        pool_manager_address=fake_pool_manager_address,
        pool_id=pool_id,
    )

    # Verify the pool was added
    retrieved_pool = managed_pool_registry.get(
        chain_id=chain_id,
        pool_manager_address=fake_pool_manager_address,
        pool_id=pool_id,
    )
    assert retrieved_pool is fake_pool, "V4 pool should be added to managed registry"

    # Remove the V4 pool
    managed_pool_registry.remove(
        chain_id=chain_id,
        pool_manager_address=fake_pool_manager_address,
        pool_id=pool_id,
    )

    # Verify the pool is removed from managed pool registry
    pool_after_removal = managed_pool_registry.get(
        chain_id=chain_id,
        pool_manager_address=fake_pool_manager_address,
        pool_id=pool_id,
    )
    assert pool_after_removal is None, "V4 pool should be removed from managed registry"

    # Test that removing a non-existent V4 pool doesn't raise an exception
    managed_pool_registry.remove(
        pool_manager_address="0x0000000000000000000000000000000000000000",
        chain_id=chain_id,
        pool_id="0x0000000000000000000000000000000000000000000000000000000000000000",
    )
