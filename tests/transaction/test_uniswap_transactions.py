from typing import Optional

import pytest
import web3
import web3.middleware
from degenbot import AnvilFork, UniswapV2LiquidityPoolManager, set_web3
from degenbot.dex.uniswap import FACTORY_ADDRESSES
from degenbot.transaction.uniswap_transaction import _ROUTERS, TransactionError, UniswapTransaction
from eth_typing import ChainId
from eth_utils import to_checksum_address
from hexbytes import HexBytes


def test_router_additions() -> None:
    # Create a new chain
    UniswapTransaction.add_chain(chain_id=69)

    # Add a new Uniswap V2/V3 compatible router on chain ID 69
    UniswapTransaction.add_router(
        chain_id=69,
        router_address="0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
        router_dict={
            "name": "Shitcoins R Us",
            "factory_address": {
                2: "0x02",
                3: "0x03",
            },
        },
    )

    # Test validations for bad router dicts
    with pytest.raises(ValueError, match="not found in router_dict"):
        UniswapTransaction.add_router(
            chain_id=69,
            router_address="0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
            router_dict={
                # "name": "Shitcoins R Us",
                "factory_address": {
                    2: "0x02",
                    3: "0x03",
                },
            },
        )
    with pytest.raises(ValueError, match="not found in router_dict"):
        UniswapTransaction.add_router(
            chain_id=69,
            router_address="0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
            router_dict={
                "name": "Shitcoins R Us",
                # "factory_address": {
                #     2: "0x02",
                #     3: "0x03",
                # },
            },
        )

    # Add a new wrapped token
    UniswapTransaction.add_wrapped_token(
        chain_id=69,
        token_address="0x6969696969696969696969696969696969696969",
    )


@pytest.mark.parametrize(
    "block_number, tx_dict, exception_match",
    [
        (
            17581149,
            dict(
                chain_id=1,
                tx_hash="0x49924bef8541e1d68a015db989083b27b0f879d73854b0ed5531270ad534750d",
                tx_nonce=148,
                tx_value=int(0.100099891804707939 * 10**18),
                tx_sender="0xb1b2d032AA2F52347fbcfd08E5C3Cc55216E8404",
                func_name="swapTokensForExactTokens",
                func_params={
                    "amountOut": 59029310000000000000000,
                    "amountInMax": 710077583,
                    "path": [
                        "0xdAC17F958D2ee523a2206206994597C13D831ec7",
                        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                        "0x8FAc8031e079F409135766C7d5De29cf22EF897C",
                    ],
                    "to": "0xfB0fce91022Ccf15f1CfC247B77047C21fC742C0",
                    "deadline": 1688175547,
                },
                router_address="0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
            ),
            "Required input 823900578 exceeds maximum 710077583",
        ),
        (
            17582974,
            dict(
                chain_id=1,
                tx_hash="0xf0ec8078db0a070c5e685278a9457bcfa8d2e6a72bf15be31bdd5f91211ae082",
                tx_nonce=56,
                tx_value=int(0.01454003642177978 * 10**18),
                tx_sender="0xA25d616a3c807f3524d45b217Abe366AEBdDF896",
                func_name="swapExactETHForTokensSupportingFeeOnTransferTokens",
                func_params={
                    "amountOutMin": 0,
                    "path": [
                        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                        "0x7b84E42E8f2a45026C5a1C6Ade52158c716EcDe8",
                    ],
                    "to": "0xA25d616a3c807f3524d45b217Abe366AEBdDF896",
                    # "deadline": 9999999999,
                },
                router_address="0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
            ),
            None,
        ),
    ],
)
def test_v2_router_transactions(
    fork_mainnet_archive: AnvilFork, block_number, tx_dict: dict, exception_match: Optional[str]
) -> None:
    fork_mainnet_archive.reset(block_number=block_number)
    set_web3(fork_mainnet_archive.w3)
    tx = UniswapTransaction(**tx_dict)

    if exception_match is not None:
        with pytest.raises(TransactionError, match=exception_match):
            tx.simulate()
    else:
        tx.simulate()


@pytest.mark.parametrize(
    "block_number, tx_dict",
    [
        (
            17560200,
            dict(
                chain_id=1,
                tx_hash="0x44391e7f2ba292cb4ff42d33b3cff859c93a9ebf0e3ed7120d27b144d3787b4f",
                tx_nonce=57,
                tx_value=0,
                tx_sender="0x42ED7246690EA1429e887CC246C460F35315a72b",
                func_name="exactOutputSingle",
                func_params={
                    "params": {
                        "tokenIn": "0x6E975115250B05C828ecb8edeDb091975Fc20a5d",
                        "tokenOut": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                        "fee": 500,
                        "recipient": "0x42ED7246690EA1429e887CC246C460F35315a72b",
                        "deadline": 1687744791,
                        "amountOut": 692000000000000000,
                        "amountInMaximum": 2695875027951196591488,
                        "sqrtPriceLimitX96": 0,
                    }
                },
                router_address="0xE592427A0AEce92De3Edee1F18E0157C05861564",
            ),
        ),
        (
            17471674,
            dict(
                chain_id=1,
                tx_hash="0x54534e3242c2b27ffe2eb32a3824a19c2060bd10cd82b6fe7aa02c43bd392f01",
                tx_nonce=593,
                tx_value=0,
                tx_sender="0x2A373E63aa5e2aee150B9b311443674e3250ab3B",
                func_name="multicall",
                func_params={
                    "deadline": 1686667218,
                    "data": [
                        HexBytes(
                            "0x42712a67000000000000000000000000000000000000000000000000010a741a462780000000000000000000000000000000000000000000000000000004574b1913eede0000000000000000000000000000000000000000000000000000000000000080000000000000000000000000e73cb605b880565477640b55fd752282cd1878220000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc200000000000000000000000034d31446a522252270b89b09016296ec4c98e23d"
                        ),
                    ],
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
        ),
    ],
)
def test_v3_router_transactions(
    fork_mainnet_archive: AnvilFork,
    block_number,
    tx_dict,
) -> None:
    fork_mainnet_archive.reset(block_number=block_number)
    assert fork_mainnet_archive.w3.eth.get_block_number() == block_number
    set_web3(fork_mainnet_archive.w3)
    tx = UniswapTransaction(**tx_dict)
    tx.simulate()


@pytest.mark.parametrize(
    "block_number, tx_dict",
    [
        (
            17715663,
            dict(
                chain_id=1,
                tx_hash="0x18aa6d274ff5bfa1c2676e2e82460158a2104ae3452b62f1ca6c46d2f55efd67",
                tx_nonce=46,
                tx_value=0,
                tx_sender="0x84b77488D7FB1Ae07Dc411a6a3EBd17ebc1faEBD",
                func_name="execute",
                func_params={
                    "commands": bytes.fromhex("0x0008"[2:]),
                    "inputs": [
                        HexBytes(
                            "0x000000000000000000000000e342253d5a0c1ac9da0203b0256e33c5cfe084f000000000000000000000000000000000000000000000152d02c7e14af6800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a0000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000423506424f91fd33084466f402d5d97f05f8e3b4af000bb8dac17f958d2ee523a2206206994597c13d831ec7000bb8c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000000000000000000000000000000000000000"
                        ),
                        HexBytes(
                            "0x00000000000000000000000084b77488d7fb1ae07dc411a6a3ebd17ebc1faebd80000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000005680118877fb7251d1b51400000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000d807f7e2818db8eda0d28b5be74866338eaedb86"
                        ),
                    ],
                },
                router_address="0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD",
            ),
        ),
    ],
)
def test_universal_router_transactions(
    fork_mainnet_archive: AnvilFork, block_number, tx_dict
) -> None:
    fork_mainnet_archive.reset(block_number=block_number)
    assert fork_mainnet_archive.w3.eth.get_block_number() == block_number
    fork_mainnet_archive.w3.provider.timeout = 600
    set_web3(fork_mainnet_archive.w3)
    tx = UniswapTransaction(**tx_dict)
    tx.simulate()


def test_adding_new_router_and_chain():
    QUICKSWAP_CHAIN = ChainId.MATIC
    QUICKSWAP_ROUTER_ADDRESS = to_checksum_address("0xa5E0829CaCEd8fFDD4De3c43696c57F7D7A678ff")
    QUICKSWAP_V2_FACTORY_ADDRESS = to_checksum_address("0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32")
    QUICKSWAP_ROUTER_INFO = {
        "name": "Quickswap: Router",
        "factory_address": {2: QUICKSWAP_V2_FACTORY_ADDRESS},
    }

    fork = AnvilFork(
        fork_url="https://rpc.ankr.com/polygon",
        fork_block=53178474 - 1,
        middlewares=[
            (web3.middleware.geth_poa_middleware, 0),
        ],
    )
    set_web3(fork.w3)

    UniswapTransaction.add_chain(QUICKSWAP_CHAIN)
    assert QUICKSWAP_CHAIN in _ROUTERS

    UniswapTransaction.add_router(
        chain_id=QUICKSWAP_CHAIN,
        router_address=QUICKSWAP_ROUTER_ADDRESS,
        router_dict=QUICKSWAP_ROUTER_INFO,
    )
    assert QUICKSWAP_ROUTER_ADDRESS in _ROUTERS[QUICKSWAP_CHAIN]

    # add the init hash for this factory
    UniswapV2LiquidityPoolManager.add_factory(
        chain_id=QUICKSWAP_CHAIN,
        factory_address=QUICKSWAP_V2_FACTORY_ADDRESS,
    )
    UniswapV2LiquidityPoolManager.add_pool_init_hash(
        chain_id=QUICKSWAP_CHAIN,
        factory_address=QUICKSWAP_V2_FACTORY_ADDRESS,
        pool_init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
    )
    assert QUICKSWAP_CHAIN in FACTORY_ADDRESSES

    tx = UniswapTransaction(
        chain_id=QUICKSWAP_CHAIN,
        tx_hash="0x997cf9f3ebc92f49dd005034220b2ea862d85d82b351bf3f1e4119220f2f9da2",
        tx_nonce=38834,
        tx_value=0,
        tx_sender="0x88fA4057386A787D098710ad0D4438C1e5266EA3",
        func_name="swapExactTokensForTokens",
        func_params={
            "amountIn": 12539452344359326161793,
            "amountOutMin": 45946433935200195305,
            "path": [
                "0x695FC8B80F344411F34bDbCb4E621aA69AdA384b",
                "0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270",
            ],
            "to": "0x88fA4057386A787D098710ad0D4438C1e5266EA3",
            "deadline": 1707194715,
        },
        router_address=QUICKSWAP_ROUTER_ADDRESS,
    )
    tx.simulate()
