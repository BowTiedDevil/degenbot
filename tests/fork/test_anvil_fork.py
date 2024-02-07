import pytest
import ujson
from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.fork import AnvilFork
from degenbot.config import set_web3
import web3.middleware

from ..conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI, ETHEREUM_FULL_NODE_HTTP_URI

VITALIK_ADDRESS = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


def test_anvil_forks():
    # Basic constructor
    AnvilFork(fork_url=ETHEREUM_FULL_NODE_HTTP_URI)

    # Test optional arguments
    AnvilFork(fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI, fork_block=18_000_000)
    AnvilFork(fork_url=ETHEREUM_FULL_NODE_HTTP_URI, chain_id=1)
    AnvilFork(fork_url=ETHEREUM_FULL_NODE_HTTP_URI, base_fee=10 * 10**9)


def test_rpc_methods(fork_mainnet_archive: AnvilFork):
    with pytest.raises(ValueError, match="Fee outside valid range"):
        fork_mainnet_archive.set_next_base_fee(-1)
    with pytest.raises(ValueError, match="Fee outside valid range"):
        fork_mainnet_archive.set_next_base_fee(MAX_UINT256 + 1)
    fork_mainnet_archive.set_next_base_fee(11 * 10**9)

    # Set several snapshot IDs and return to them
    snapshot_ids = []
    for _ in range(10):
        snapshot_ids.append(fork_mainnet_archive.set_snapshot())
    for id in snapshot_ids:
        assert fork_mainnet_archive.return_to_snapshot(id) is True
    # No snapshot ID with this value
    assert fork_mainnet_archive.return_to_snapshot(100) is False

    # Negative IDs are not allowed
    with pytest.raises(ValueError, match="ID cannot be negative"):
        fork_mainnet_archive.return_to_snapshot(-1)

    # Generate a 1 wei WETH deposit transaction from Vitalik.eth
    weth_contract = fork_mainnet_archive.w3.eth.contract(
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
    access_list = fork_mainnet_archive.create_access_list(transaction=deposit_transaction)
    assert isinstance(access_list, list)

    fork_mainnet_archive.reset(block_number=18_500_000)
    with pytest.raises(Exception):
        fork_mainnet_archive.reset(
            fork_url="http://google.com",  # <--- Bad RPC URI
        )

    # switch between two different endpoints
    for endpoint in (ETHEREUM_ARCHIVE_NODE_HTTP_URI, ETHEREUM_FULL_NODE_HTTP_URI):
        fork_mainnet_archive.reset(fork_url=endpoint)

    for balance in [MIN_UINT256, MAX_UINT256]:
        fork_mainnet_archive.set_balance(VITALIK_ADDRESS, balance)
        assert fork_mainnet_archive.w3.eth.get_balance(VITALIK_ADDRESS) == balance

    # Balances outside of uint256 should be rejected
    with pytest.raises(ValueError):
        fork_mainnet_archive.set_balance(VITALIK_ADDRESS, MIN_UINT256 - 1)
    with pytest.raises(ValueError):
        fork_mainnet_archive.set_balance(VITALIK_ADDRESS, MAX_UINT256 + 1)


def test_injecting_middleware():
    fork = AnvilFork(
        fork_url="https://rpc.ankr.com/polygon",
        fork_block=53178474 - 1,
        middlewares=[
            (web3.middleware.geth_poa_middleware, 0),
        ],
    )
    set_web3(fork.w3)