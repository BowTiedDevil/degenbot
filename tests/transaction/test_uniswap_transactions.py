import degenbot
import pytest
from degenbot.transaction.uniswap_transaction import TransactionError, UniswapTransaction
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


def test_v2_router_transactions() -> None:
    FORK_URL = "http://localhost:8543"
    FORK_BLOCK = 17581149
    # FORK_URL = f"https://rpc.ankr.com/eth/{load_env['ANKR_API_KEY']}"
    fork = degenbot.AnvilFork(fork_url=FORK_URL, fork_block=FORK_BLOCK)
    degenbot.set_web3(fork.w3)

    tx = UniswapTransaction(
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
    )
    with pytest.raises(
        TransactionError, match="Required input 823900578 exceeds maximum 710077583"
    ):
        tx.simulate()

    fork.reset(block_number=17582974)

    tx = UniswapTransaction(
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
    )
    tx.simulate()


def test_v3_router_transactions() -> None:
    FORK_URL = "http://localhost:8543"
    FORK_BLOCK = 17560200
    # FORK_URL = f"https://rpc.ankr.com/eth/{load_env['ANKR_API_KEY']}"
    fork = degenbot.AnvilFork(fork_url=FORK_URL, fork_block=FORK_BLOCK)
    degenbot.set_web3(fork.w3)

    tx = UniswapTransaction(
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
    )
    tx.simulate()

    fork.reset(block_number=17471674)

    tx = UniswapTransaction(
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
    )
    tx.simulate()


def test_universal_router_transactions() -> None:
    FORK_URL = "http://localhost:8543"
    FORK_BLOCK = 17715663
    # FORK_URL = f"https://rpc.ankr.com/eth/{load_env['ANKR_API_KEY']}"
    fork = degenbot.AnvilFork(fork_url=FORK_URL, fork_block=FORK_BLOCK)
    degenbot.set_web3(fork.w3)

    tx = UniswapTransaction(
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
    )
    tx.simulate()
