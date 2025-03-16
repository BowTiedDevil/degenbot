from typing import TYPE_CHECKING

import pytest
import web3.middleware
from hexbytes import HexBytes

from degenbot import AnvilFork
from degenbot.cache import get_checksum_address
from degenbot.config import set_web3
from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.exceptions import DegenbotValueError, EVMRevertError, InvalidUint256

from .conftest import BASE_ARCHIVE_NODE_HTTP_URI, ETHEREUM_ARCHIVE_NODE_HTTP_URI

if TYPE_CHECKING:
    from web3.providers.ipc import IPCProvider


VITALIK_ADDRESS = get_checksum_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")


def test_anvil_forks():
    # Basic constructor
    AnvilFork(fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI)

    # Test optional arguments
    AnvilFork(fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI, fork_block=18_000_000)
    AnvilFork(fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI, chain_id=1)
    AnvilFork(fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI, base_fee=10 * 10**9)


def test_http_and_endpoints():
    fork = AnvilFork(fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI)
    assert fork.http_url == f"http://localhost:{fork.port}"
    assert fork.ws_url == f"ws://localhost:{fork.port}"

    current_block = fork.w3.eth.block_number
    assert web3.Web3(web3.HTTPProvider(fork.http_url)).eth.block_number == current_block
    assert web3.Web3(web3.LegacyWebSocketProvider(fork.ws_url)).eth.block_number == current_block


def test_set_bytecode():
    fake_bytecode = HexBytes("0x42069")
    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        bytecode_overrides=[
            (VITALIK_ADDRESS, fake_bytecode),
        ],
    )
    assert fork.w3.eth.get_code(VITALIK_ADDRESS) == fake_bytecode


def test_rpc_methods(fork_mainnet: AnvilFork):
    with pytest.raises(InvalidUint256):
        fork_mainnet.set_next_base_fee(-1)
    with pytest.raises(InvalidUint256):
        fork_mainnet.set_next_base_fee(MAX_UINT256 + 1)
    fork_mainnet.set_next_base_fee(11 * 10**9)

    # Set several snapshot IDs and return to them
    snapshot_ids = [fork_mainnet.set_snapshot() for _ in range(10)]
    for snapshot_id in snapshot_ids:
        assert fork_mainnet.return_to_snapshot(snapshot_id) is True
    # No snapshot ID with this value
    assert fork_mainnet.return_to_snapshot(100) is False

    # Negative IDs are not allowed
    with pytest.raises(DegenbotValueError, match="ID cannot be negative"):
        fork_mainnet.return_to_snapshot(-1)

    for balance in [MIN_UINT256, MAX_UINT256]:
        fork_mainnet.set_balance(VITALIK_ADDRESS, balance)
        assert fork_mainnet.w3.eth.get_balance(VITALIK_ADDRESS) == balance

    # Balances outside of uint256 should be rejected
    with pytest.raises(EVMRevertError):
        fork_mainnet.set_balance(VITALIK_ADDRESS, MIN_UINT256 - 1)
    with pytest.raises(EVMRevertError):
        fork_mainnet.set_balance(VITALIK_ADDRESS, MAX_UINT256 + 1)

    fake_coinbase = get_checksum_address("0x0420042004200420042004200420042004200420")
    fork_mainnet.set_coinbase(fake_coinbase)
    # @dev the eth_coinbase method fails when called on Anvil,
    # so check by mining a block and comparing the miner address

    fork_mainnet.mine()
    block = fork_mainnet.w3.eth.get_block("latest")
    assert block.get("miner") == fake_coinbase


def test_mine_and_reset(fork_mainnet: AnvilFork):
    starting_block = fork_mainnet.w3.eth.get_block_number()
    fork_mainnet.mine()
    fork_mainnet.mine()
    fork_mainnet.mine()
    assert fork_mainnet.w3.eth.get_block_number() == starting_block + 3
    fork_mainnet.reset(block_number=starting_block)
    assert fork_mainnet.w3.eth.get_block_number() == starting_block


def test_fork_from_transaction_hash():
    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        fork_transaction_hash="0x12167fa2a4cd676a6e740edb09427469ecb8718d84ef4d0d5819fe8b527964d6",
    )
    assert fork.w3.eth.block_number == 20987963


def test_set_next_block_base_fee(fork_mainnet: AnvilFork):
    base_fee_override = 69 * 10**9

    fork_mainnet.set_next_base_fee(base_fee_override)
    fork_mainnet.mine()
    assert fork_mainnet.w3.eth.get_block("latest")["baseFeePerGas"] == base_fee_override


def test_reset_and_set_next_block_base_fee(fork_mainnet: AnvilFork):
    base_fee_override = 69 * 10**9

    starting_block = fork_mainnet.w3.eth.get_block_number()
    fork_mainnet.reset(block_number=starting_block - 10)
    fork_mainnet.set_next_base_fee(base_fee_override)
    fork_mainnet.mine()
    assert fork_mainnet.w3.eth.get_block_number() == starting_block - 9
    assert fork_mainnet.w3.eth.get_block(starting_block - 9)["baseFeePerGas"] == base_fee_override


def test_reset_to_new_endpoint(fork_mainnet: AnvilFork):
    fork_mainnet.reset(fork_url=BASE_ARCHIVE_NODE_HTTP_URI)
    assert fork_mainnet.fork_url == BASE_ARCHIVE_NODE_HTTP_URI


def test_reset_to_new_transaction_hash(fork_mainnet: AnvilFork):
    fork_mainnet.reset(
        transaction_hash="0x12167fa2a4cd676a6e740edb09427469ecb8718d84ef4d0d5819fe8b527964d6"
    )
    assert fork_mainnet.w3.eth.block_number == 20987963


def test_ipc_kwargs():
    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        ipc_provider_kwargs=dict(timeout=None),
    )
    if TYPE_CHECKING:
        assert isinstance(fork.w3.provider, IPCProvider)
    assert fork.w3.provider.timeout is None


def test_balance_overrides_in_constructor():
    fake_balance = 100 * 10**18
    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        balance_overrides=[
            (VITALIK_ADDRESS, fake_balance),
        ],
    )
    assert fork.w3.eth.get_balance(VITALIK_ADDRESS) == fake_balance


def test_nonce_overrides_in_constructor():
    fake_nonce = 69
    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        nonce_overrides=[
            (VITALIK_ADDRESS, fake_nonce),
        ],
    )
    assert fork.w3.eth.get_transaction_count(VITALIK_ADDRESS) == fake_nonce


def test_bytecode_overrides_in_constructor():
    fake_address = get_checksum_address("0x6969696969696969696969696969696969696969")
    fake_bytecode = HexBytes("0x0420")

    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI, bytecode_overrides=[(fake_address, fake_bytecode)]
    )
    assert fork.w3.eth.get_code(fake_address) == fake_bytecode


def test_coinbase_override_in_constructor():
    fake_coinbase = get_checksum_address("0x6969696969696969696969696969696969696969")

    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        coinbase=fake_coinbase,
    )
    fork.mine()
    block = fork.w3.eth.get_block("latest")
    assert block["miner"] == fake_coinbase


def test_injecting_middleware():
    fork = AnvilFork(
        fork_url="https://rpc.ankr.com/polygon",
        fork_block=53178474 - 1,
        middlewares=[
            (web3.middleware.ExtraDataToPOAMiddleware, 0),
        ],
    )
    set_web3(fork.w3)
