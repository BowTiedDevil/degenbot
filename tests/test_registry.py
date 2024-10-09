from eth_utils.address import to_checksum_address

from degenbot import AnvilFork, Erc20Token, UniswapV2Pool
from degenbot.config import set_web3
from degenbot.registry.all_pools import pool_registry
from degenbot.registry.all_tokens import token_registry

UNISWAP_V2_WBTC_WETH_POOL = to_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
WETH_ADDRESS = to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")


def test_adding_pool(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)
    lp = UniswapV2Pool(UNISWAP_V2_WBTC_WETH_POOL)
    assert pool_registry.get(pool_address=lp.address, chain_id=fork_mainnet.w3.eth.chain_id) is lp


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
