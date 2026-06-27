import errno
import os
import subprocess  # noqa:S404
from typing import TYPE_CHECKING

import pytest
import web3.middleware
from hexbytes import HexBytes
from pydantic import ValidationError

from degenbot import anvil_fork as anvil_fork_module
from degenbot.anvil_fork import AnvilFork
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.exceptions.base import DegenbotError, DegenbotValueError

from .conftest import (
    BASE_FULL_NODE_HTTP_URI,
    ETHEREUM_ARCHIVE_NODE_HTTP_URI,
    ETHEREUM_FULL_NODE_HTTP_URI,
)

pytestmark = pytest.mark.online_rpc

if TYPE_CHECKING:
    from web3.providers.ipc import IPCProvider


VITALIK_ADDRESS = get_checksum_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")


def _pid_alive(pid: int) -> bool:
    """Return True if ``pid`` is a live process."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def test_fork_captures_output():
    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
        preserve_capture=True,
    )
    capture_path = fork.capture_path
    fork_port = fork.port
    fork.close()

    expected_stderr_path = capture_path / f"anvil-{fork_port}.stderr"
    expected_stdout_path = capture_path / f"anvil-{fork_port}.stdout"

    try:
        assert expected_stderr_path.exists()
        # stderr should be empty
        assert not expected_stderr_path.read_text()
    finally:
        expected_stderr_path.unlink(missing_ok=True)

    try:
        assert expected_stdout_path.exists()
        # stdout should have text from normal startup
        assert expected_stdout_path.read_text()
    finally:
        expected_stdout_path.unlink(missing_ok=True)


def test_web3_endpoints():
    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
    )
    assert fork.http_url == f"http://127.0.0.1:{fork.port}"
    assert fork.ws_url == f"ws://127.0.0.1:{fork.port}"

    current_block = fork.w3.eth.block_number
    assert web3.Web3(web3.HTTPProvider(fork.http_url)).eth.block_number == current_block
    assert web3.Web3(web3.LegacyWebSocketProvider(fork.ws_url)).eth.block_number == current_block


def test_set_bytecode():
    fake_bytecode = HexBytes("0x42069")
    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
        bytecode_overrides=[
            (VITALIK_ADDRESS, fake_bytecode),
        ],
    )
    assert fork.w3.eth.get_code(VITALIK_ADDRESS) == fake_bytecode


def test_set_storage():
    storage_position = 0
    new_storage_value = HexBytes("0x42069")
    new_storage_value_padded = new_storage_value.hex().zfill(64)

    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
    )
    assert fork.w3.eth.get_storage_at(
        account=WETH_ADDRESS,
        position=storage_position,
    ) != HexBytes(new_storage_value_padded)
    fork.set_storage(WETH_ADDRESS, position=storage_position, value=new_storage_value)

    assert fork.w3.eth.get_storage_at(
        account=WETH_ADDRESS,
        position=storage_position,
    ) == HexBytes(new_storage_value_padded)

    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
        storage_overrides=[(WETH_ADDRESS, storage_position, new_storage_value)],
    )
    assert fork.w3.eth.get_storage_at(
        account=WETH_ADDRESS,
        position=storage_position,
    ) == HexBytes(new_storage_value_padded)


def test_rpc_methods(fork_mainnet_full: AnvilFork):
    with pytest.raises(ValidationError):
        fork_mainnet_full.set_next_base_fee(MIN_UINT256 - 1)
    with pytest.raises(ValidationError):
        fork_mainnet_full.set_next_base_fee(MAX_UINT256 + 1)
    fork_mainnet_full.set_next_base_fee(11 * 10**9)

    # Set several snapshot IDs and return to them
    snapshot_ids = [fork_mainnet_full.set_snapshot() for _ in range(10)]
    for snapshot_id in snapshot_ids:
        fork_mainnet_full.return_to_snapshot(snapshot_id)

    with pytest.raises(DegenbotError, match="Anvil RPC call to evm_revert failed:"):
        fork_mainnet_full.return_to_snapshot(100)

    # Negative IDs are not allowed
    with pytest.raises(DegenbotValueError, match="ID cannot be negative"):
        fork_mainnet_full.return_to_snapshot(-1)

    for balance in [MIN_UINT256, MAX_UINT256]:
        fork_mainnet_full.set_balance(VITALIK_ADDRESS, balance)
        assert fork_mainnet_full.w3.eth.get_balance(VITALIK_ADDRESS) == balance

    # Balances outside of uint256 should be rejected
    with pytest.raises(ValidationError):
        fork_mainnet_full.set_balance(VITALIK_ADDRESS, MIN_UINT256 - 1)
    with pytest.raises(ValidationError):
        fork_mainnet_full.set_balance(VITALIK_ADDRESS, MAX_UINT256 + 1)

    fake_coinbase = get_checksum_address("0x0420042004200420042004200420042004200420")
    fork_mainnet_full.set_coinbase(fake_coinbase)
    # @dev the eth_coinbase method fails when called on Anvil,
    # so check by mining a block and comparing the miner address

    fork_mainnet_full.mine()
    block = fork_mainnet_full.w3.eth.get_block("latest")
    assert block.get("miner") == fake_coinbase


def test_mine_and_reset():
    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
    )
    starting_block = fork.w3.eth.get_block_number()
    fork.mine()
    fork.mine()
    fork.mine()
    assert fork.w3.eth.get_block_number() == starting_block + 3
    fork.reset(block_number=starting_block)
    assert fork.w3.eth.get_block_number() == starting_block


def test_fork_from_transaction_hash():
    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        fork_transaction_hash="0x12167fa2a4cd676a6e740edb09427469ecb8718d84ef4d0d5819fe8b527964d6",
    )
    assert fork.w3.eth.block_number == 20987963


def test_set_next_block_base_fee(fork_mainnet_full: AnvilFork):
    base_fee_override = 69 * 10**9

    fork_mainnet_full.set_next_base_fee(base_fee_override)
    fork_mainnet_full.mine()
    assert fork_mainnet_full.w3.eth.get_block("latest")["baseFeePerGas"] == base_fee_override


def test_set_next_block_base_fee_in_constructor():
    base_fee_override = 69 * 10**9

    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
        base_fee=base_fee_override,
    )
    fork.mine()
    assert fork.w3.eth.get_block("latest")["baseFeePerGas"] == base_fee_override


def test_reset_and_set_next_block_base_fee():
    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
    )
    base_fee_override = 69 * 10**9

    starting_block = fork.w3.eth.get_block_number()
    fork.reset(block_number=starting_block - 10)
    fork.set_next_base_fee(base_fee_override)
    fork.mine()
    assert fork.w3.eth.get_block_number() == starting_block - 9
    assert fork.w3.eth.get_block(starting_block - 9)["baseFeePerGas"] == base_fee_override


@pytest.mark.base
def test_reset_to_new_endpoint():
    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
    )
    assert fork.w3.eth.chain_id == 1

    fork.reset(fork_url=BASE_FULL_NODE_HTTP_URI)
    assert fork.w3.eth.chain_id == 8453


def test_reset_to_new_transaction_hash():
    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
    )
    fork.reset(
        transaction_hash="0x12167fa2a4cd676a6e740edb09427469ecb8718d84ef4d0d5819fe8b527964d6",
    )
    assert fork.w3.eth.block_number == 20987963


def test_ipc_kwargs():
    fork = AnvilFork(
        localhost="127.0.0.1",
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
        ipc_provider_kwargs={"timeout": None},
    )
    if TYPE_CHECKING:
        assert isinstance(fork.w3.provider, IPCProvider)
    assert fork.w3.provider.timeout is None


def test_balance_overrides_in_constructor():
    fake_balance = 100 * 10**18
    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
        balance_overrides=[
            (VITALIK_ADDRESS, fake_balance),
        ],
    )
    assert fork.w3.eth.get_balance(VITALIK_ADDRESS) == fake_balance


def test_nonce_overrides_in_constructor():
    fake_nonce = 69
    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
        nonce_overrides=[
            (VITALIK_ADDRESS, fake_nonce),
        ],
    )
    assert fork.w3.eth.get_transaction_count(VITALIK_ADDRESS) == fake_nonce


def test_bytecode_overrides_in_constructor():
    fake_address = get_checksum_address("0x6969696969696969696969696969696969696969")
    fake_bytecode = HexBytes("0x0420")

    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
        bytecode_overrides=[(fake_address, fake_bytecode)],
    )
    assert fork.w3.eth.get_code(fake_address) == fake_bytecode


def test_coinbase_override_in_constructor():
    fake_coinbase = get_checksum_address("0x6969696969696969696969696969696969696969")

    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
        coinbase=fake_coinbase,
    )
    fork.mine()
    block = fork.w3.eth.get_block("latest")
    assert block["miner"] == fake_coinbase


def test_launch_retries_on_pid_budget_exhaustion(monkeypatch):
    """AnvilFork retries the anvil launch when the cgroup PID budget is transiently exhausted.

    Under ``pytest-xdist --numprocesses auto`` many AnvilFork fixtures boot concurrently;
    each anvil process holds ~27 pids (threads) against the container's ``pids.max``. A
    burst at fixture setup can hit the ceiling, so ``subprocess.Popen`` returns
    ``BlockingIOError(EAGAIN)``. The launch must retry with backoff rather than fail the
    test, since peer forks are concurrently tearing down and will free slots.
    """
    real_popen = subprocess.Popen
    calls = {"n": 0}

    def flaky_popen(args, *rest, **kwargs):
        calls["n"] += 1
        if calls["n"] < 3:
            raise BlockingIOError(errno.EAGAIN, "Resource temporarily unavailable")
        return real_popen(args, *rest, **kwargs)

    monkeypatch.setattr(anvil_fork_module.subprocess, "Popen", flaky_popen)

    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
    )
    try:
        assert fork.w3.eth.block_number > 0
        assert calls["n"] >= 3
    finally:
        fork.close()


def test_launch_retries_when_anvil_crashes_at_startup(monkeypatch):
    """AnvilFork retries when the anvil process crashes during startup.

    Under high ``pytest-xdist`` parallelism anvil can panic while installing its
    Ctrl-C (SIGINT) handler — ``Error setting Ctrl-C handler: System(... EAGAIN
    ...)`` — and exit immediately without ever creating its IPC socket. The launch
    must detect this early death (rather than waiting the full socket timeout for
    a file that will never appear) and retry with backoff so a transient startup
    crash doesn't fail the test.
    """
    real_popen = subprocess.Popen
    calls = {"n": 0}

    def crash_then_real_popen(args, *rest, **kwargs):
        calls["n"] += 1
        if calls["n"] < 2:
            # Replace the anvil command with one that exits immediately,
            # mimicking an anvil panic during Ctrl-C handler setup (no IPC
            # socket is ever created).
            return real_popen(["bash", "-c", "exit 1"], *rest, **kwargs)
        return real_popen(args, *rest, **kwargs)

    monkeypatch.setattr(anvil_fork_module.subprocess, "Popen", crash_then_real_popen)

    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
    )
    try:
        assert fork.w3.eth.block_number > 0
        assert calls["n"] >= 2
    finally:
        fork.close()


def test_close_all_reaps_forks_leaked_by_unclosed_construction():
    """``AnvilFork.close_all()`` reaps forks a test forgot to ``close()``.

    Mirrors ``DatabaseSessionManager.dispose_all()``: tests that construct an
    AnvilFork directly (rather than via a yielding fixture) and never call
    ``close()`` rely on non-deterministic ``__del__``/GC. Under xdist fan-out the
    deferred reaping lets anvil subprocesses (each ~27 pids) pile up against
    the container ``pids.max`` and crash it. ``close_all()`` is the safety net
    the autouse teardown calls so leaked forks are reaped deterministically,
    even when an assertion failure holds the frame alive (defeating GC).
    """
    # Construct a fork and deliberately do NOT close it, mimicking a leak.
    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
    )
    anvil_pid = fork._process.pid
    assert anvil_pid is not None
    try:
        AnvilFork.close_all()
        # The process should be gone within the reap.
        assert not _pid_alive(anvil_pid)
    finally:
        if _pid_alive(anvil_pid):
            fork.close()
