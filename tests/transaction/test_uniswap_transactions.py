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
        (
            19195157 - 1,
            dict(
                chain_id=1,
                tx_hash="0x4ecbc9a61b15f0a6e6ade2d871e1badcb9d89b3f58d70054d076541a7aa4af5d",
                tx_nonce=170,
                tx_value=0,
                tx_sender="0x4364C9257Bb1bD856B237EC6D7AB80bC0241705C",
                func_name="swapTokensForExactTokens",
                func_params={
                    "amountOut": 5532860500000000442368,
                    "amountInMax": 4733071650,
                    "path": [
                        "0xdAC17F958D2ee523a2206206994597C13D831ec7",
                        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                        "0x0F7B3F5a8FeD821c5eb60049538a548dB2D479ce",
                    ],
                    "to": "0x4364C9257Bb1bD856B237EC6D7AB80bC0241705C",
                    "deadline": 1707538366,
                },
                router_address="0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
            ),
            None,
        ),
    ],
)
def test_v2_router_transactions(
    fork_mainnet_archive: AnvilFork,
    block_number,
    tx_dict: dict,
    exception_match: str | None,
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
    "block_number, tx_dict,exception_match",
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
            None,
        ),
        (
            # duplicate of the above, but in 8-tuple format (Router01)
            17560200,
            dict(
                chain_id=1,
                tx_hash="0x44391e7f2ba292cb4ff42d33b3cff859c93a9ebf0e3ed7120d27b144d3787b4f",
                tx_nonce=57,
                tx_value=0,
                tx_sender="0x42ED7246690EA1429e887CC246C460F35315a72b",
                func_name="exactOutputSingle",
                func_params={
                    "params": (
                        "0x6E975115250B05C828ecb8edeDb091975Fc20a5d",
                        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                        500,
                        "0x42ED7246690EA1429e887CC246C460F35315a72b",
                        1687744791,
                        692000000000000000,
                        2695875027951196591488,
                        0,
                    ),
                },
                router_address="0xE592427A0AEce92De3Edee1F18E0157C05861564",
            ),
            None,
        ),
        (
            # duplicate of the above, but in 7-tuple format (Router02)
            17560200,
            dict(
                chain_id=1,
                tx_hash="0x44391e7f2ba292cb4ff42d33b3cff859c93a9ebf0e3ed7120d27b144d3787b4f",
                tx_nonce=57,
                tx_value=0,
                tx_sender="0x42ED7246690EA1429e887CC246C460F35315a72b",
                func_name="exactOutputSingle",
                func_params={
                    "params": (
                        "0x6E975115250B05C828ecb8edeDb091975Fc20a5d",
                        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                        500,
                        "0x42ED7246690EA1429e887CC246C460F35315a72b",
                        692000000000000000,
                        2695875027951196591488,
                        0,
                    ),
                },
                router_address="0xE592427A0AEce92De3Edee1F18E0157C05861564",
            ),
            None,
        ),
        (
            19195876 - 1,
            dict(
                chain_id=1,
                tx_hash="0x0492ef965901b7bc9c1b9d02868ea3e642f84a399f4d0179b006088cd2942d99",
                tx_nonce=672,
                tx_value=0,
                tx_sender="0xff46Bc0A888233028915b6Ce84d6209092Ba9b58",
                func_name="multicall",
                func_params={
                    "data": [
                        HexBytes(
                            "0x414bf38900000000000000000000000073576a927cd93a578a9dfd61c75671d97c779da7000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000000000000000000000000000000000000000271000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000065c71bfb00000000000000000000000000000000000000000000000148c9793ff2b8b0b100000000000000000000000000000000000000000000000011fca96ba7d8d08c0000000000000000000000000000000000000000000000000000000000000000"
                        ),
                        HexBytes(
                            "0x49404b7c00000000000000000000000000000000000000000000000011fca96ba7d8d08c000000000000000000000000ff46bc0a888233028915b6ce84d6209092ba9b58"
                        ),
                    ],
                },
                router_address="0xE592427A0AEce92De3Edee1F18E0157C05861564",
            ),
            None,
        ),
        (
            19195864 - 1,
            dict(
                chain_id=1,
                tx_hash="0xf684531981c2169c168249db3a0e0ae92c8763d19bc7f01a40e4d42997e1b62c",
                tx_nonce=3424,
                tx_value=int(0.2743754838560736 * 10**18),
                tx_sender="0xfbe6Ed1942B03eF4fBa780890550dB1F0c43Bd32",
                func_name="multicall",
                func_params={
                    "deadline": 1686667218,
                    "data": [
                        HexBytes(
                            "0xf28c0498000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000a0000000000000000000000000fbe6ed1942b03ef4fba780890550db1f0c43bd320000000000000000000000000000000000000000000000000000000065c7190a0000000000000000000000000000000000000000000000000000000028eface000000000000000000000000000000000000000000000000003cec70c82513380000000000000000000000000000000000000000000000000000000000000002ba0b86991c6218b36c1d19d4a2e9eb0ce3606eb480001f4c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000000000000000000000"
                        ),
                        HexBytes("0x12210e8a"),
                    ],
                },
                router_address="0xE592427A0AEce92De3Edee1F18E0157C05861564",
            ),
            None,
        ),
        (
            19200547 - 1,
            dict(
                chain_id=1,
                tx_hash="0x0348df6b2a08e73d356774a005c952dd6fa5c5403f34106c10d27d1b4aca56e7",
                tx_nonce=1323,
                tx_value=0,
                tx_sender="0xd7452CE7652f353c150ddd2427B6680052467d3d",
                func_name="exactOutput",
                func_params={
                    "params": {
                        "path": HexBytes(
                            "0xda31d0d1bc934fc34f7189e38a413ca0a5e8b44f002710c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20001f4dac17f958d2ee523a2206206994597c13d831ec7"
                        ),
                        "recipient": "0xd7452CE7652f353c150ddd2427B6680052467d3d",
                        "deadline": 1707603210,
                        "amountOut": 599620000000000000000,
                        "amountInMaximum": 537159816,
                    },
                },
                router_address="0xE592427A0AEce92De3Edee1F18E0157C05861564",
            ),
            None,
        ),
        (
            19231984 - 1,
            dict(
                chain_id=1,
                tx_hash="0xf9b1a43c34c400090cf695121b4b060324ea520aad2e0fee67365c5c462aacd2",
                tx_nonce=2490,
                tx_value=0,
                tx_sender="0xf9b306b5Ef6Be6c7E93d7Daffa4a806E3015c58A",
                func_name="exactOutput",
                func_params={
                    "params": (
                        HexBytes(
                            "0x3c3a81e81dc49a522a592e7622a7e711c06bf354000bb8c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                        ),
                        "0xf9b306b5Ef6Be6c7E93d7Daffa4a806E3015c58A",
                        1707983833,
                        20000000000000000000000,
                        5685474000000000000,
                    ),
                },
                router_address="0xE592427A0AEce92De3Edee1F18E0157C05861564",
            ),
            None,
        ),
        (
            # same as above, but in 4-tuple format (Router02)
            19231984 - 1,
            dict(
                chain_id=1,
                tx_hash="0xf9b1a43c34c400090cf695121b4b060324ea520aad2e0fee67365c5c462aacd2",
                tx_nonce=2490,
                tx_value=0,
                tx_sender="0xf9b306b5Ef6Be6c7E93d7Daffa4a806E3015c58A",
                func_name="exactOutput",
                func_params={
                    "params": (
                        HexBytes(
                            "0x3c3a81e81dc49a522a592e7622a7e711c06bf354000bb8c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                        ),
                        "0xf9b306b5Ef6Be6c7E93d7Daffa4a806E3015c58A",
                        20000000000000000000000,
                        5685474000000000000,
                    ),
                },
                router_address="0xE592427A0AEce92De3Edee1F18E0157C05861564",
            ),
            None,
        ),
        (
            19229539 - 1,
            dict(
                chain_id=1,
                tx_hash="0x91f6955f167d8e79af01c4b1a0cddf714933075d750a66043f67348e6f926974",
                tx_nonce=1408,
                tx_value=0,
                tx_sender="0xd7452CE7652f353c150ddd2427B6680052467d3d",
                func_name="exactOutput",
                func_params={
                    "params": {
                        "path": HexBytes(
                            "0xda31d0d1bc934fc34f7189e38a413ca0a5e8b44f002710c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20001f4dac17f958d2ee523a2206206994597c13d831ec7"
                        ),
                        "recipient": "0xd7452CE7652f353c150ddd2427B6680052467d3d",
                        "deadline": 1707954771,
                        "amountOut": 1384200000000000000000,
                        "amountInMaximum": 1257174724,
                    },
                },
                router_address="0xE592427A0AEce92De3Edee1F18E0157C05861564",
            ),
            None,
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
            None,
        ),
        (
            17588171 - 1,
            dict(
                chain_id=1,
                tx_hash="0x7fdc920e5a8a1335ad97abe752e5421c3093b37d177cee333bd90e0ac1c78657",
                tx_nonce=970,
                tx_value=int(0.15 * 10**18),
                tx_sender="0x68ecA53134299f4c6E099C6A50250C76C5fEfBe7",
                func_name="multicall",
                func_params={
                    "deadline": 1688082383,
                    "data": [
                        HexBytes(
                            "0x472b43f30000000000000000000000000000000000000000000000000214e8348c4f0000000000000000000000000000000000000000000000038c5566a92a2bc65d9f43000000000000000000000000000000000000000000000000000000000000008000000000000000000000000068eca53134299f4c6e099c6a50250c76c5fefbe70000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20000000000000000000000005cd40aa65e0f1c3daf333fc334d2de93c1a399f2"
                        ),
                    ],
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            18000000,
            dict(
                chain_id=1,
                tx_hash="0xbdb36f71b3fc55b269414887210274ddef91479819a3e5e27c8b64793937ddd5",
                tx_nonce=69,
                tx_value=int(0.05 * 10**18),
                tx_sender="0x68ecA53134299f4c6E099C6A50250C76C5fEfBe7",
                func_name="multicall",
                func_params={
                    "deadline": 99999999999999,
                    "data": [
                        HexBytes(
                            "0xac9650d800000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000001a45ae401dc00000000000000000000000000000000000000000000000000000000644b3a3c00000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000e404e45aaf000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000f1b99e3e573a1a9c5e6b2ce818b617f0e664e86b0000000000000000000000000000000000000000000000000000000000000bb8000000000000000000000000997b9dbded32c79b15e2ba07fadfbc2f91da0a9d00000000000000000000000000000000000000000000000000b1a2bc2ec500000000000000000000000000000000000000000000000000000b0961da046e379f00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
                        ),
                    ],
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            19195614 - 1,
            dict(
                chain_id=1,
                tx_hash="0x62ede4ae1a1a92d07f580c46f74565e5bf2af2a49e8030b1c80e3f120169128e",
                tx_nonce=10473,
                tx_value=int(0.696590690514831983 * 10**18),
                tx_sender="0x9B228B4F71B3Bc7e4b478251f218060D7B70Dc25",
                func_name="multicall",
                func_params={
                    "deadline": 99999999999999,
                    "data": [
                        HexBytes(
                            "0xdb3e2198000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000413530a7beb9ff6c44e9e6c9001c93b785420c320000000000000000000000000000000000000000000000000000000000000bb80000000000000000000000009b228b4f71b3bc7e4b478251f218060d7b70dc250000000000000000000000000000000000000000000000000000000065c70fa70000000000000000000000000000000000000000000004a89f54ef0121c0000000000000000000000000000000000000000000000000000009aac98ad5fb0a6f0000000000000000000000000000000000000000000000000000000000000000"
                        ),
                    ],
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            19195847 - 1,
            dict(
                chain_id=1,
                tx_hash="0x37709f0ea96b092fba26048e16a190d378dd9d5c2367fff0842ca5f42b3a9e8a",
                tx_nonce=903,
                tx_value=0,
                tx_sender="0xaE8bCABeaC4acc3bBaf1799EaE07e1f2985B07D6",
                func_name="multicall",
                func_params={
                    "deadline": 1707547283,
                    "data": [
                        HexBytes(
                            "0x472b43f300000000000000000000000000000000000000000000000000001da17a434054000000000000000000000000000000000000000000000000017682063d9c0bcb00000000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000200000000000000000000000044face2e310e543f6d85867eb06fb251e3bfe1fc000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                        ),
                        HexBytes(
                            "0x49404b7c000000000000000000000000000000000000000000000000017682063d9c0bcb000000000000000000000000ae8bcabeac4acc3bbaf1799eae07e1f2985b07d6"
                        ),
                    ],
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            19199283 - 1,
            dict(
                chain_id=1,
                tx_hash="0x07eac30cb8d9d96faef41c4cd04397195a93bfc53729806626b76dbc5f0146c1",
                tx_nonce=21347,
                tx_value=0,
                tx_sender="0x4cb6F0ef0Eeb503f8065AF1A6E6D5DD46197d3d9",
                func_name="exactInput",
                func_params={
                    "params": {
                        "path": HexBytes(
                            "0x31e4efe290973ebe91b3a875a7994f650942d28f000bb8c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20001f4a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                        ),
                        "recipient": "0x4cb6F0ef0Eeb503f8065AF1A6E6D5DD46197d3d9",
                        "deadline": 99999999999999,
                        "amountIn": 29738575235245025056471,
                        "amountOutMinimum": 7579379036,
                    }
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            # duplicate of the above, but in 5-tuple format (Router01)
            19199283 - 1,
            dict(
                chain_id=1,
                tx_hash="0x07eac30cb8d9d96faef41c4cd04397195a93bfc53729806626b76dbc5f0146c1",
                tx_nonce=21347,
                tx_value=0,
                tx_sender="0x4cb6F0ef0Eeb503f8065AF1A6E6D5DD46197d3d9",
                func_name="exactInput",
                func_params={
                    "params": (
                        HexBytes(
                            "0x31e4efe290973ebe91b3a875a7994f650942d28f000bb8c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20001f4a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                        ),
                        "0x4cb6F0ef0Eeb503f8065AF1A6E6D5DD46197d3d9",
                        99999999999999,
                        29738575235245025056471,
                        7579379036,
                    )
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            # duplicate of the above, but in 4-tuple format (Router02)
            19199283 - 1,
            dict(
                chain_id=1,
                tx_hash="0x07eac30cb8d9d96faef41c4cd04397195a93bfc53729806626b76dbc5f0146c1",
                tx_nonce=21347,
                tx_value=0,
                tx_sender="0x4cb6F0ef0Eeb503f8065AF1A6E6D5DD46197d3d9",
                func_name="exactInput",
                func_params={
                    "params": (
                        HexBytes(
                            "0x31e4efe290973ebe91b3a875a7994f650942d28f000bb8c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20001f4a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                        ),
                        "0x4cb6F0ef0Eeb503f8065AF1A6E6D5DD46197d3d9",
                        29738575235245025056471,
                        7579379036,
                    )
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            19199376 - 1,
            dict(
                chain_id=1,
                tx_hash="0xaa74821d005281080966e29c3a93ad0f4cb36cb86fd1f08d11c0e560dce5d90a",
                tx_nonce=110,
                tx_value=0,
                tx_sender="0xaF1F9Db296f6E0F144bc414EB748678d548fD320",
                func_name="multicall",
                func_params={
                    "deadline": 1707590171,
                    "data": [
                        HexBytes(
                            "0xb858183f00000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000080000000000000000000000000af1f9db296f6e0f144bc414eb748678d548fd320000000000000000000000000000000000000000000000000000014bc89e6a0000000000000000000000000000000000000000000000000000000000092d3181100000000000000000000000000000000000000000000000000000000000000422b591e99afe9f32eaa6214f7b7629768c40eeb39000bb8c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20001f4dac17f958d2ee523a2206206994597c13d831ec7000000000000000000000000000000000000000000000000000000000000"
                        ),
                    ],
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            19199448 - 1,
            dict(
                chain_id=1,
                tx_hash="0x7f93ec1914d4c4d5db9d69693bb452b0ea1e2635906e5ecb448e571ebe4f0786",
                tx_nonce=49283,
                tx_value=0,
                tx_sender="0xb3382eC98b0C4453c6A81BD095D9696FC3C7eC46",
                func_name="multicall",
                func_params={
                    "deadline": 1707589785,
                    "data": [
                        HexBytes(
                            "0x04e45aaf0000000000000000000000002b591e99afe9f32eaa6214f7b7629768c40eeb39000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20000000000000000000000000000000000000000000000000000000000000bb800000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000142983cc726b0000000000000000000000000000000000000000000000000cf4a03fa101cf910000000000000000000000000000000000000000000000000000000000000000"
                        ),
                        HexBytes(
                            "0x49404b7c0000000000000000000000000000000000000000000000000cf4a03fa101cf91000000000000000000000000077d360f11d220e4d5d831430c81c26c9be7c4a4"
                        ),
                    ],
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            19199470 - 1,
            dict(
                chain_id=1,
                tx_hash="0x9549021fb039810f8da32d210c32f12e1e688747e9155b81238f6b9b2b84c88d",
                tx_nonce=1393,
                tx_value=0,
                tx_sender="0x86A79Be5CB85cC5DE48bB953cf0B1a01a40d8732",
                func_name="exactInputSingle",
                func_params={
                    "params": {
                        "tokenIn": "0x2B591E99AFE9F32EAA6214F7B7629768C40EEB39",
                        "tokenOut": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                        "fee": 3000,
                        "recipient": "0x86A79Be5CB85cC5DE48bB953cf0B1a01a40d8732",
                        "deadline": 1707589615,
                        "amountIn": 38179275262415,
                        "amountOutMinimum": 1631204831946501888,
                        "sqrtPriceLimitX96": 0,
                    }
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            # duplicate of the above, but in 8-tuple format (Router01)
            19199470 - 1,
            dict(
                chain_id=1,
                tx_hash="0x9549021fb039810f8da32d210c32f12e1e688747e9155b81238f6b9b2b84c88d",
                tx_nonce=1393,
                tx_value=0,
                tx_sender="0x86A79Be5CB85cC5DE48bB953cf0B1a01a40d8732",
                func_name="exactInputSingle",
                func_params={
                    "params": (
                        "0x2B591E99AFE9F32EAA6214F7B7629768C40EEB39",
                        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                        3000,
                        "0x86A79Be5CB85cC5DE48bB953cf0B1a01a40d8732",
                        1707589615,
                        38179275262415,
                        1631204831946501888,
                        0,
                    )
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            # duplicate of the above, but in 7-tuple format (Router02)
            19199470 - 1,
            dict(
                chain_id=1,
                tx_hash="0x9549021fb039810f8da32d210c32f12e1e688747e9155b81238f6b9b2b84c88d",
                tx_nonce=1393,
                tx_value=0,
                tx_sender="0x86A79Be5CB85cC5DE48bB953cf0B1a01a40d8732",
                func_name="exactInputSingle",
                func_params={
                    "params": (
                        "0x2B591E99AFE9F32EAA6214F7B7629768C40EEB39",
                        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                        3000,
                        "0x86A79Be5CB85cC5DE48bB953cf0B1a01a40d8732",
                        38179275262415,
                        1631204831946501888,
                        0,
                    )
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            19211846 - 1,
            dict(
                chain_id=1,
                tx_hash="0x1b3b1470a6fb67d4032fecab43f932c171a25a0079388b7b3b664ffc8141b1e5",
                tx_nonce=13251,
                tx_value=0,
                tx_sender="0x80C1969588bD9a017190ff4ed669e4e4b70e7768",
                func_name="multicall",
                func_params={
                    "deadline": 1707741498,
                    "data": [
                        HexBytes(
                            "0x472b43f30000000000000000000000000000000000000000000000000e92596fd62900000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc200000000000000000000000026c8afbbfe1ebaca03c2bb082e69d0476bffe099"
                        ),
                        HexBytes(
                            "0x472b43f30000000000000000000000000000000000000000000000000214e8348c4f00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000003000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb4800000000000000000000000026c8afbbfe1ebaca03c2bb082e69d0476bffe099"
                        ),
                        HexBytes(
                            "0x472b43f3000000000000000000000000000000000000000000000000010a741a462780000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000003000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec700000000000000000000000026c8afbbfe1ebaca03c2bb082e69d0476bffe099"
                        ),
                        HexBytes(
                            "0xb858183f000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000031f5c4ed276800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000042c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000bb8dac17f958d2ee523a2206206994597c13d831ec700271026c8afbbfe1ebaca03c2bb082e69d0476bffe099000000000000000000000000000000000000000000000000000000000000"
                        ),
                        HexBytes(
                            "0xdf2ab5bb00000000000000000000000026c8afbbfe1ebaca03c2bb082e69d0476bffe0990000000000000000000000000000000000000000000000e67a857849c4cd223f0000000000000000000000004400b633e90947c59903759e2121abcd83ddfa22"
                        ),
                    ],
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
        (
            19229227 - 1,
            dict(
                chain_id=1,
                tx_hash="0x68d27a8dbbf74d1cbead370da978c68e92a7f698107f7cb179eb82d82aeaf5bd",
                tx_nonce=38,
                tx_value=0,
                tx_sender="0x43a2241335584c46b5a3C75CF9895c92c0AED74B",
                func_name="multicall",
                func_params={
                    "deadline": 1707952175,
                    "data": [
                        HexBytes(
                            "0x04e45aaf000000000000000000000000f9ca9523e5b5a42c3018c62b084db8543478c400000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20000000000000000000000000000000000000000000000000000000000000bb80000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000002a5a058fc295ed00000000000000000000000000000000000000000000000000000022a867bfd4dcd2540000000000000000000000000000000000000000000000000000000000000000"
                        ),
                        HexBytes(
                            "0x9b2c0a3700000000000000000000000000000000000000000000000022a867bfd4dcd25400000000000000000000000043a2241335584c46b5a3c75cf9895c92c0aed74b0000000000000000000000000000000000000000000000000000000000000064000000000000000000000000d62ba193d0c0c556d4d37dbbc5e431330471a557"
                        ),
                    ],
                },
                router_address="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
            ),
            None,
        ),
    ],
)
def test_v3_router_transactions(
    fork_mainnet_archive: AnvilFork,
    block_number,
    tx_dict,
    exception_match: str | None,
) -> None:
    fork_mainnet_archive.reset(block_number=block_number)
    assert fork_mainnet_archive.w3.eth.get_block_number() == block_number
    set_web3(fork_mainnet_archive.w3)
    tx = UniswapTransaction(**tx_dict)

    if exception_match is not None:
        with pytest.raises(TransactionError, match=exception_match):
            tx.simulate()
    else:
        tx.simulate()


@pytest.mark.parametrize(
    "block_number, tx_dict, exception_match",
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
                    "commands": HexBytes("0x0008"),
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
            None,
        ),
        (
            19190900 - 1,
            dict(
                chain_id=1,
                tx_hash="0x6bebd3432ebc56a6ba95c90959a227a7bd5865be93dc5167d8afc6468cd731fc",
                tx_nonce=96,
                tx_value=int(0.3 * 10**18),
                tx_sender="0xd2d85D5aca1123d19420AB0769A90cFa3774074f",
                func_name="execute",
                func_params={
                    "commands": HexBytes("0x0b00"),
                    "inputs": [
                        HexBytes(
                            "0x00000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000429d069189e0000"
                        ),
                        HexBytes(
                            "0x00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000429d069189e00000000000000000000000000000000000000000000000000002889950ccd6b55ab00000000000000000000000000000000000000000000000000000000000000a00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002bc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2002710e8c55226badf5bb4f9d310879284992a5a1acab5000000000000000000000000000000000000000000"
                        ),
                    ],
                    "deadline": 1707486263,
                },
                router_address="0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD",
            ),
            None,
        ),
        (
            19195827 - 1,
            dict(
                chain_id=1,
                tx_hash="0xbc96aa9e03d61d6a3ea1928274e2b1df1533a8c5c30f6fb936ea3c04c512329f",
                tx_nonce=499,
                tx_value=int(0.067163565815370351 * 10**18),
                tx_sender="0xc9F869C08e6303340118C1B9eb498DAeA2505E60",
                func_name="execute",
                func_params={
                    "commands": HexBytes("0x0b010c"),
                    "inputs": [
                        HexBytes(
                            "0x000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000ee9ce84246126f"
                        ),
                        HexBytes(
                            "0x000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000003782dace9d9000000000000000000000000000000000000000000000000000000ee9ce84246126f00000000000000000000000000000000000000000000000000000000000000a00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002b6c061d18d2b5bbfbe8a8d1eeb9ee27efd544cc5d002710c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000000000000000000000"
                        ),
                        HexBytes(
                            "0x00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000"
                        ),
                    ],
                    "deadline": 1707545831,
                },
                router_address="0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD",
            ),
            None,
        ),
        (
            19196501 - 1,
            dict(
                chain_id=1,
                tx_hash="0xfff49e25c2f14293f7ca7a9c71211982abf9baecd5a73ae4b63c6a1a14455c38",
                tx_nonce=17,
                tx_value=int(0.042106940365974302 * 10**18),
                tx_sender="0xD1F9C1Db23A8A468Cb68571d20d1A852c415F6f7",
                func_name="execute",
                func_params={
                    "commands": HexBytes("0x0b0905040c"),
                    "inputs": [
                        HexBytes(
                            "0x00000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000095980a0717ab1e"
                        ),
                        HexBytes(
                            "0x00000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000005f82af00000000000000000000000000000000000000000000000000095980a0717ab1e00000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                        ),
                        HexBytes(
                            "0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb4800000000000000000000000027213e28d7fda5c57fe9e5dd923818dbccf71c4700000000000000000000000000000000000000000000000000000000000249f0"
                        ),
                        HexBytes(
                            "0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb4800000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000005f5e100"
                        ),
                        HexBytes(
                            "0x00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000"
                        ),
                    ],
                    "deadline": 1707553979,
                },
                router_address="0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD",
            ),
            None,
        ),
        (
            19196536 - 1,
            dict(
                chain_id=1,
                tx_hash="0x65fc99c9ed97036910f82b996bce94c7b3237de689485925ec57762ba52d5adc",
                tx_nonce=269,
                tx_value=0,
                tx_sender="0x5Bc77Aa665E4ac891243452a4db73101E90756Fc",
                func_name="execute",
                func_params={
                    "commands": HexBytes("0x000604"),
                    "inputs": [
                        HexBytes(
                            "0x000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000010a741a46278000000000000000000000000000000000000000000000000000000000000b178cd0700000000000000000000000000000000000000000000000000000000000000a00000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000002bc02aaa39b223fe8d0a0e5c4f27ead9083c756cc20001f4dac17f958d2ee523a2206206994597c13d831ec7000000000000000000000000000000000000000000"
                        ),
                        HexBytes(
                            "0x000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec700000000000000000000000037a8f295612602f2774d331e562be9e61b83a327000000000000000000000000000000000000000000000000000000000000000f"
                        ),
                        HexBytes(
                            "0x000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec7000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000b178cd07"
                        ),
                    ],
                    "deadline": 1707553979,
                },
                router_address="0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD",
            ),
            None,
        ),
        (
            19198600,
            dict(
                chain_id=1,
                tx_hash="0xa7f6b08077dd5ca6270ecc7371b463671bfe34fbb9bf6ca27cab71ae8c22727d",
                tx_nonce=2024,
                tx_value=int(0.327834453655960088 * 10**18),
                tx_sender="0xfB552CeF511C3289C5240e09458eAf660D06AB43",
                func_name="execute",
                func_params={
                    "commands": HexBytes("0x0b090c"),
                    "inputs": [
                        HexBytes(
                            "0x0000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000048cb3b2db4c3a18"
                        ),
                        HexBytes(
                            "0x00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000de0b6b3a7640000000000000000000000000000000000000000000000000000048cb3b2db4c3a1800000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000b87b96868644d99cc70a8565ba7311482edebf6e"
                        ),
                        HexBytes(
                            "0x00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000"
                        ),
                    ],
                    "deadline": 1707579635,
                },
                router_address="0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD",
            ),
            "exceeds maximum",
        ),
        (
            19469214 - 1,
            dict(
                chain_id=1,
                tx_hash="0x1d2a3cd8f5f480993f96b96e43e6567398e62f1edb30b8103b05b11cd83d4c9f",
                tx_nonce=11,
                tx_value=int(0.1 * 10**18),
                tx_sender="0xfeBD9f8e4E00B72B27f2B9BF452267155F865777",
                func_name="execute",
                func_params={
                    "commands": HexBytes("0x0b00"),
                    "inputs": [
                        HexBytes(
                            "0x0000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000016345785d8a0000"
                        ),
                        HexBytes(
                            "0x0000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000016345785d8a00000000000000000000000000000000000000000000000000000000013eb37668f100000000000000000000000000000000000000000000000000000000000000a00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002bc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000064119df13971e4120215b2aa5eaa00a59885a96dc3000000000000000000000000000000000000000000"
                        ),
                    ],
                    "deadline": 1710856955,
                },
                router_address="0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD",
            ),
            "Insufficient output for swap!",
        ),
    ],
)
def test_universal_router_transactions(
    fork_mainnet_archive: AnvilFork, block_number, tx_dict, exception_match
) -> None:
    fork_mainnet_archive.reset(block_number=block_number)
    assert fork_mainnet_archive.w3.eth.get_block_number() == block_number
    fork_mainnet_archive.w3.provider.timeout = 600  # type: ignore[attr-defined]
    set_web3(fork_mainnet_archive.w3)
    tx = UniswapTransaction(**tx_dict)

    if exception_match is not None:
        with pytest.raises(TransactionError, match=exception_match):
            tx.simulate()
    else:
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


def test_invalid_router():
    INVALID_ROUTER_ADDRESS = "0x6969696969696969696969696969696969696969"

    with pytest.raises(ValueError, match=f"address {INVALID_ROUTER_ADDRESS} unknown"):
        UniswapTransaction(
            chain_id=1,
            tx_hash="0xbc96aa9e03d61d6a3ea1928274e2b1df1533a8c5c30f6fb936ea3c04c512329f",
            tx_nonce=499,
            tx_value=int(0.067163565815370351 * 10**18),
            tx_sender="0xc9F869C08e6303340118C1B9eb498DAeA2505E60",
            func_name="execute",
            func_params={
                "commands": HexBytes("0x0b010c"),
                "inputs": [
                    HexBytes(
                        "0x000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000ee9ce84246126f"
                    ),
                    HexBytes(
                        "0x000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000003782dace9d9000000000000000000000000000000000000000000000000000000ee9ce84246126f00000000000000000000000000000000000000000000000000000000000000a00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002b6c061d18d2b5bbfbe8a8d1eeb9ee27efd544cc5d002710c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000000000000000000000"
                    ),
                    HexBytes(
                        "0x00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000"
                    ),
                ],
                "deadline": 1707545831,
            },
            router_address=INVALID_ROUTER_ADDRESS,
        )


def test_expired_transaction(fork_mainnet_archive: AnvilFork):
    block_number = 19195827 - 1
    fork_mainnet_archive.reset(block_number=block_number)
    assert fork_mainnet_archive.w3.eth.get_block_number() == block_number
    fork_mainnet_archive.w3.provider.timeout = 600  # type: ignore[attr-defined]
    set_web3(fork_mainnet_archive.w3)

    with pytest.raises(TransactionError, match="Deadline expired"):
        tx = UniswapTransaction(
            chain_id=1,
            tx_hash="0xbc96aa9e03d61d6a3ea1928274e2b1df1533a8c5c30f6fb936ea3c04c512329f",
            tx_nonce=499,
            tx_value=int(0.067163565815370351 * 10**18),
            tx_sender="0xc9F869C08e6303340118C1B9eb498DAeA2505E60",
            func_name="execute",
            func_params={
                "commands": HexBytes("0x0b010c"),
                "inputs": [
                    HexBytes(
                        "0x000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000ee9ce84246126f"
                    ),
                    HexBytes(
                        "0x000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000003782dace9d9000000000000000000000000000000000000000000000000000000ee9ce84246126f00000000000000000000000000000000000000000000000000000000000000a00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002b6c061d18d2b5bbfbe8a8d1eeb9ee27efd544cc5d002710c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000000000000000000000"
                    ),
                    HexBytes(
                        "0x00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000"
                    ),
                ],
                "deadline": 1707545831 - 1000,
            },
            router_address="0x3FC91A3AFD70395CD496C647D5A6CC9D4B2B7FAD",
        )
        tx.simulate()
