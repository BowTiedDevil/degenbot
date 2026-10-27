import pytest
from pydantic import ValidationError

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.fork import AnvilFork, ForkLaunchConfig
from degenbot.provider import AlloyProvider
from degenbot.utils.bytes import to_bytes
from tests.standalone_anvil import seed as seed_catalog

from .conftest import (
    TEST_BASE_FULL_NODE_HTTP_URI,
    TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI,
    TEST_ETHEREUM_FULL_NODE_HTTP_URI,
    AnvilFactory,
)

VITALIK_ADDRESS = get_checksum_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")


def test_web3_endpoints(standalone_anvil: AnvilFork):
    fork = standalone_anvil
    assert fork.http_url == f"http://127.0.0.1:{fork.port}"
    assert fork.ws_url == f"ws://127.0.0.1:{fork.port}"

    current_block = fork.provider.get_block_number()
    assert AlloyProvider(fork.http_url).get_block_number() == current_block
    assert AlloyProvider(fork.ws_url).get_block_number() == current_block


def test_set_bytecode(anvil_factory: AnvilFactory):
    fake_bytecode = to_bytes("0x42069")
    fork = anvil_factory(
        launch=ForkLaunchConfig(
            chain_id=seed_catalog.CHAIN_ID,
            bytecode_overrides=[
                (VITALIK_ADDRESS, fake_bytecode),
            ],
        ),
    )
    assert fork.provider.get_code(VITALIK_ADDRESS) == fake_bytecode


def test_set_storage(standalone_anvil: AnvilFork, anvil_factory: AnvilFactory):
    storage_position = 0
    new_storage_value = to_bytes("0x42069")
    new_storage_value_padded = new_storage_value.hex().zfill(64)

    fork = standalone_anvil
    assert fork.provider.get_storage_at(
        address=WETH_ADDRESS,
        position=storage_position,
    ) != to_bytes(new_storage_value_padded)
    fork.set_storage(WETH_ADDRESS, position=storage_position, value=new_storage_value)

    assert fork.provider.get_storage_at(
        address=WETH_ADDRESS,
        position=storage_position,
    ) == to_bytes(new_storage_value_padded)

    fork = anvil_factory(
        launch=ForkLaunchConfig(
            chain_id=seed_catalog.CHAIN_ID,
            storage_overrides=[(WETH_ADDRESS, storage_position, new_storage_value)],
        ),
    )
    assert fork.provider.get_storage_at(
        address=WETH_ADDRESS,
        position=storage_position,
    ) == to_bytes(new_storage_value_padded)


def test_rpc_methods(standalone_anvil: AnvilFork):
    with pytest.raises(ValidationError):
        standalone_anvil.set_next_base_fee(MIN_UINT256 - 1)
    with pytest.raises(ValidationError):
        standalone_anvil.set_next_base_fee(MAX_UINT256 + 1)
    standalone_anvil.set_next_base_fee(11 * 10**9)

    for balance in [MIN_UINT256, MAX_UINT256]:
        standalone_anvil.set_balance(VITALIK_ADDRESS, balance)
        assert standalone_anvil.provider.get_balance(VITALIK_ADDRESS) == balance

    # Balances outside of uint256 should be rejected
    with pytest.raises(ValidationError):
        standalone_anvil.set_balance(VITALIK_ADDRESS, MIN_UINT256 - 1)
    with pytest.raises(ValidationError):
        standalone_anvil.set_balance(VITALIK_ADDRESS, MAX_UINT256 + 1)

    fake_coinbase = get_checksum_address("0x0420042004200420042004200420042004200420")
    standalone_anvil.set_coinbase(fake_coinbase)
    # @dev the eth_coinbase method fails when called on Anvil,
    # so check by mining a block and comparing the miner address

    standalone_anvil.mine()
    block = standalone_anvil.provider.get_block("latest")
    assert block.get("miner") == fake_coinbase


@pytest.mark.online_rpc
def test_mine_and_reset(anvil_factory: AnvilFactory):
    fork = anvil_factory(
        fork_url=TEST_ETHEREUM_FULL_NODE_HTTP_URI,
        launch=ForkLaunchConfig(storage_caching=False),
    )
    starting_block = fork.provider.get_block_number()
    fork.mine()
    fork.mine()
    fork.mine()
    assert fork.provider.get_block_number() == starting_block + 3
    fork.reset(block_number=starting_block)
    assert fork.provider.get_block_number() == starting_block


@pytest.mark.online_rpc
def test_fork_from_transaction_hash(anvil_factory: AnvilFactory):
    fork = anvil_factory(
        fork_url=TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        launch=ForkLaunchConfig(
            fork_transaction_hash=(
                "0x12167fa2a4cd676a6e740edb09427469ecb8718d84ef4d0d5819fe8b527964d6"
            ),
        ),
    )
    assert fork.provider.get_block_number() == 20987963


def test_set_next_block_base_fee(standalone_anvil: AnvilFork):
    base_fee_override = 69 * 10**9

    standalone_anvil.set_next_base_fee(base_fee_override)
    standalone_anvil.mine()
    assert standalone_anvil.provider.get_block("latest")["base_fee_per_gas"] == base_fee_override


def test_set_next_block_base_fee_in_constructor(anvil_factory: AnvilFactory):
    base_fee_override = 69 * 10**9

    fork = anvil_factory(
        launch=ForkLaunchConfig(chain_id=seed_catalog.CHAIN_ID, base_fee=base_fee_override),
    )
    fork.mine()
    assert fork.provider.get_block("latest")["base_fee_per_gas"] == base_fee_override


@pytest.mark.online_rpc
def test_reset_and_set_next_block_base_fee(anvil_factory: AnvilFactory):
    fork = anvil_factory(
        fork_url=TEST_ETHEREUM_FULL_NODE_HTTP_URI,
        launch=ForkLaunchConfig(storage_caching=False),
    )
    base_fee_override = 69 * 10**9

    starting_block = fork.provider.get_block_number()
    fork.reset(block_number=starting_block - 10)
    fork.set_next_base_fee(base_fee_override)
    fork.mine()
    assert fork.provider.get_block_number() == starting_block - 9
    assert fork.provider.get_block(starting_block - 9)["base_fee_per_gas"] == base_fee_override


@pytest.mark.base
def test_reset_to_new_endpoint(anvil_factory: AnvilFactory):
    fork = anvil_factory(
        fork_url=TEST_ETHEREUM_FULL_NODE_HTTP_URI,
        launch=ForkLaunchConfig(storage_caching=False),
    )
    assert fork.provider.get_chain_id() == 1

    fork.reset(fork_url=TEST_BASE_FULL_NODE_HTTP_URI)
    assert fork.provider.get_chain_id() == 8453


@pytest.mark.online_rpc
def test_reset_to_new_transaction_hash(anvil_factory: AnvilFactory):
    fork = anvil_factory(
        fork_url=TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI,
    )
    fork.reset(
        transaction_hash="0x12167fa2a4cd676a6e740edb09427469ecb8718d84ef4d0d5819fe8b527964d6",
    )
    assert fork.provider.get_block_number() == 20987963


@pytest.mark.online_rpc
def test_ipc_kwargs(anvil_factory: AnvilFactory) -> None:
    # IPCProvider type check retired — AnvilFork now uses Rust DynProvider
    # timeout check retired — Rust provider manages its own timeouts
    anvil_factory(
        fork_url=TEST_ETHEREUM_FULL_NODE_HTTP_URI,
        launch=ForkLaunchConfig(localhost="127.0.0.1", storage_caching=False),
    )


def test_balance_overrides_in_constructor(anvil_factory: AnvilFactory):
    fake_balance = 100 * 10**18
    fork = anvil_factory(
        launch=ForkLaunchConfig(
            chain_id=seed_catalog.CHAIN_ID,
            balance_overrides=[
                (VITALIK_ADDRESS, fake_balance),
            ],
        ),
    )
    assert fork.provider.get_balance(VITALIK_ADDRESS) == fake_balance


def test_nonce_overrides_in_constructor(anvil_factory: AnvilFactory):
    fake_nonce = 69
    fork = anvil_factory(
        launch=ForkLaunchConfig(
            chain_id=seed_catalog.CHAIN_ID,
            nonce_overrides=[
                (VITALIK_ADDRESS, fake_nonce),
            ],
        ),
    )
    assert fork.provider.get_transaction_count(VITALIK_ADDRESS) == fake_nonce


def test_bytecode_overrides_in_constructor(anvil_factory: AnvilFactory):
    fake_address = get_checksum_address("0x6969696969696969696969696969696969696969")
    fake_bytecode = to_bytes("0x0420")

    fork = anvil_factory(
        launch=ForkLaunchConfig(
            chain_id=seed_catalog.CHAIN_ID,
            bytecode_overrides=[(fake_address, fake_bytecode)],
        ),
    )
    assert fork.provider.get_code(fake_address) == fake_bytecode


def test_coinbase_override_in_constructor(anvil_factory: AnvilFactory):
    fake_coinbase = get_checksum_address("0x6969696969696969696969696969696969696969")

    fork = anvil_factory(
        launch=ForkLaunchConfig(chain_id=seed_catalog.CHAIN_ID, coinbase=fake_coinbase),
    )
    fork.mine()
    block = fork.provider.get_block("latest")
    assert block["miner"] == fake_coinbase
