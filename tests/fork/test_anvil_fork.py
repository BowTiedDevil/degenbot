from degenbot.fork import AnvilFork
import pytest
import ujson

VITALIK_ADDRESS = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


def test_anvil_forks(load_env):
    ANKR_URL = f"https://rpc.ankr.com/eth/{load_env['ANKR_API_KEY']}"

    # Basic constructor
    AnvilFork(fork_url=ANKR_URL)

    # Test optional arguments
    AnvilFork(fork_url=ANKR_URL, chain_id=1)
    AnvilFork(fork_url=ANKR_URL, fork_block=18_000_000)
    AnvilFork(fork_url=ANKR_URL, base_fee=10 * 10**9)


def test_rpc_methods(load_env):
    ANKR_URL = f"https://rpc.ankr.com/eth/{load_env['ANKR_API_KEY']}"

    fork = AnvilFork(fork_url=ANKR_URL)

    fork.set_next_base_fee(11 * 10**9)
    with pytest.raises(Exception, match="Error setting next block base fee!"):
        fork.set_next_base_fee(-1)

    # Set several snapshot IDs and return to them
    snapshot_ids = []
    for _ in range(10):
        snapshot_ids.append(fork.set_snapshot())
    for id in snapshot_ids:
        assert fork.return_to_snapshot(id) is True
    # No snapshot ID with this value
    assert fork.return_to_snapshot(100) is False

    # Negative IDs are not allowed
    with pytest.raises(Exception, match="Error reverting to previous snapshot!"):
        fork.return_to_snapshot(-1)

    # Generate a 1 wei WETH deposit transaction from Vitalik.eth
    weth_contract = fork.w3.eth.contract(
        address=WETH_ADDRESS,
        abi=ujson.loads(
            """
            [{"constant":true,"inputs":[],"name":"name","outputs":[{"name":"","type":"string"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"name":"guy","type":"address"},{"name":"wad","type":"uint256"}],"name":"approve","outputs":[{"name":"","type":"bool"}],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[],"name":"totalSupply","outputs":[{"name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"name":"src","type":"address"},{"name":"dst","type":"address"},{"name":"wad","type":"uint256"}],"name":"transferFrom","outputs":[{"name":"","type":"bool"}],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":false,"inputs":[{"name":"wad","type":"uint256"}],"name":"withdraw","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[],"name":"decimals","outputs":[{"name":"","type":"uint8"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[{"name":"","type":"address"}],"name":"balanceOf","outputs":[{"name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"symbol","outputs":[{"name":"","type":"string"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"name":"dst","type":"address"},{"name":"wad","type":"uint256"}],"name":"transfer","outputs":[{"name":"","type":"bool"}],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":false,"inputs":[],"name":"deposit","outputs":[],"payable":true,"stateMutability":"payable","type":"function"},{"constant":true,"inputs":[{"name":"","type":"address"},{"name":"","type":"address"}],"name":"allowance","outputs":[{"name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"payable":true,"stateMutability":"payable","type":"fallback"},{"anonymous":false,"inputs":[{"indexed":true,"name":"src","type":"address"},{"indexed":true,"name":"guy","type":"address"},{"indexed":false,"name":"wad","type":"uint256"}],"name":"Approval","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"name":"src","type":"address"},{"indexed":true,"name":"dst","type":"address"},{"indexed":false,"name":"wad","type":"uint256"}],"name":"Transfer","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"name":"dst","type":"address"},{"indexed":false,"name":"wad","type":"uint256"}],"name":"Deposit","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"name":"src","type":"address"},{"indexed":false,"name":"wad","type":"uint256"}],"name":"Withdrawal","type":"event"}]
            """
        ),
    )
    deposit_transaction = weth_contract.functions.deposit().build_transaction(
        {
            "from": VITALIK_ADDRESS,
            "value": 1,
        },
    )
    access_list = fork.create_access_list(transaction=deposit_transaction)
    assert isinstance(access_list, list)

    fork.reset(fork_url=ANKR_URL, block_number=18_500_000)
    with pytest.raises(Exception):
        fork.reset(
            fork_url="http://google.com",  # <--- Bad RPC URI
        )
