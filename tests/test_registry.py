import pytest

from degenbot import AnvilFork, Erc20Token, UniswapV2Pool
from degenbot.cache import get_checksum_address
from degenbot.config import set_web3
from degenbot.exceptions import DegenbotValueError, RegistryAlreadyInitialized
from degenbot.registry.all_pools import PoolRegistry, pool_registry
from degenbot.registry.all_tokens import TokenRegistry, token_registry

UNISWAP_V2_WBTC_WETH_POOL = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")


def test_singleton(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)

    with pytest.raises(RegistryAlreadyInitialized):
        PoolRegistry()

    with pytest.raises(RegistryAlreadyInitialized):
        TokenRegistry()

    assert PoolRegistry.get_instance() is pool_registry
    assert TokenRegistry.get_instance() is token_registry


def test_adding_pool(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)
    lp = UniswapV2Pool(UNISWAP_V2_WBTC_WETH_POOL)
    assert pool_registry.get(pool_address=lp.address, chain_id=fork_mainnet.w3.eth.chain_id) is lp

    with pytest.raises(DegenbotValueError):
        pool_registry.add(pool_address=lp.address, chain_id=fork_mainnet.w3.eth.chain_id, pool=lp)


def test_deleting_pool(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)
    lp = UniswapV2Pool(UNISWAP_V2_WBTC_WETH_POOL)
    assert pool_registry.get(pool_address=lp.address, chain_id=fork_mainnet.w3.eth.chain_id) is lp
    pool_registry.remove(pool_address=lp.address, chain_id=fork_mainnet.w3.eth.chain_id)
    assert pool_registry.get(pool_address=lp.address, chain_id=fork_mainnet.w3.eth.chain_id) is None


def test_adding_token(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)
    weth = Erc20Token(WETH_ADDRESS)
    assert (
        token_registry.get(token_address=weth.address, chain_id=fork_mainnet.w3.eth.chain_id)
        is weth
    )

    with pytest.raises(DegenbotValueError):
        token_registry.add(
            token_address=weth.address, chain_id=fork_mainnet.w3.eth.chain_id, token=weth
        )


def test_deleting_token(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)
    weth = Erc20Token(WETH_ADDRESS)
    assert (
        token_registry.get(token_address=weth.address, chain_id=fork_mainnet.w3.eth.chain_id)
        is weth
    )
    token_registry.remove(token_address=weth.address, chain_id=fork_mainnet.w3.eth.chain_id)
    assert (
        token_registry.get(token_address=weth.address, chain_id=fork_mainnet.w3.eth.chain_id)
        is None
    )
