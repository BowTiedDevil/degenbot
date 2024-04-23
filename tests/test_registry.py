from degenbot.config import set_web3
from degenbot.erc20_token import Erc20Token
from degenbot.fork.anvil_fork import AnvilFork
from degenbot.registry.all_pools import AllPools
from degenbot.registry.all_tokens import AllTokens
from degenbot.uniswap.v2_liquidity_pool import LiquidityPool
from eth_utils.address import to_checksum_address

UNISWAP_V2_WBTC_WETH_POOL = to_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")


def test_adding_pool(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)
    all_pools = AllPools(fork_mainnet.w3.eth.chain_id)
    lp = LiquidityPool(UNISWAP_V2_WBTC_WETH_POOL)
    assert lp in all_pools
    assert lp.address in all_pools
    assert all_pools[lp.address] is lp


def test_deleting_pool(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)
    all_pools = AllPools(fork_mainnet.w3.eth.chain_id)
    lp = LiquidityPool(UNISWAP_V2_WBTC_WETH_POOL)
    assert lp in all_pools
    del all_pools[lp]
    assert lp not in all_pools


def test_deleting_pool_by_address(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)
    all_pools = AllPools(fork_mainnet.w3.eth.chain_id)
    lp = LiquidityPool(UNISWAP_V2_WBTC_WETH_POOL)
    assert lp in all_pools
    del all_pools[lp.address]
    assert lp not in all_pools


def test_adding_token(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)
    all_tokens = AllTokens(fork_mainnet.w3.eth.chain_id)
    weth = Erc20Token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
    assert weth in all_tokens
    assert weth.address in all_tokens
    assert all_tokens[weth.address] is weth


def test_deleting_token(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)
    all_tokens = AllTokens(fork_mainnet.w3.eth.chain_id)
    weth = Erc20Token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
    assert weth in all_tokens
    del all_tokens[weth]
    assert weth not in all_tokens


def test_deleting_token_by_address(fork_mainnet: AnvilFork):
    set_web3(fork_mainnet.w3)
    all_tokens = AllTokens(fork_mainnet.w3.eth.chain_id)
    weth = Erc20Token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
    assert weth in all_tokens
    del all_tokens[weth.address]
    assert weth not in all_tokens
