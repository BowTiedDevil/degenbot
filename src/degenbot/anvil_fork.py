import contextlib
import pathlib
import shutil
import socket
import subprocess
import tempfile
from collections.abc import AsyncIterator, Iterable
from typing import TYPE_CHECKING, Any, Literal, cast

import tenacity
from eth_typing import HexAddress, HexStr
from hexbytes import HexBytes
from pydantic import validate_call
from web3 import AsyncBaseProvider, AsyncIPCProvider, AsyncWeb3, IPCProvider, Web3
from web3.middleware import Middleware
from web3.types import RPCEndpoint

from degenbot.exceptions.anvil import AnvilError
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.connection import IPCSocketTimeout, Web3ConnectionTimeout
from degenbot.logging import logger
from degenbot.types.aliases import BlockNumber
from degenbot.validation.evm_values import ValidatedUint256


class AnvilNotFound(Exception):
    def __init__(self) -> None:  # pragma: no cover
        super().__init__("Anvil path could not be located.")


type AnvilOptions = list[str]


class AnvilFork:
    """
    Launch an Anvil fork as a separate process and expose methods for commonly-used RPC calls.

    Provides a `Web3` connector to Anvil's IPC socket endpoint at the `.w3` attribute.
    """

    def __init__(
        self,
        *,
        fork_url: str,
        fork_block: BlockNumber | None = None,
        fork_transaction_hash: str | None = None,
        mining_mode: Literal["auto", "interval", "none"] = "auto",
        mining_interval: int | None = None,
        storage_caching: bool = True,
        base_fee: int | None = None,
        ipc_path: pathlib.Path | None = None,
        capture_path: pathlib.Path | None = None,
        preserve_capture: bool = False,
        mnemonic: str = (
            # Default mnemonic used by Brownie for Ganache forks
            "patient rude simple dog close planet oval animal hunt sketch suspect slim"
        ),
        coinbase: HexAddress | None = None,
        middlewares: list[tuple[Middleware, int]] | None = None,
        balance_overrides: Iterable[tuple[HexAddress, int]] | None = None,
        bytecode_overrides: Iterable[tuple[HexAddress, bytes]] | None = None,
        nonce_overrides: Iterable[tuple[HexAddress, int]] | None = None,
        storage_overrides: Iterable[tuple[HexAddress | bytes, int, HexStr | bytes | int]]
        | None = None,
        ipc_provider_kwargs: dict[str, Any] | None = None,
        anvil_opts: list[str] | None = None,  # Additional options passed to the Anvil command
    ) -> None:
        def _parse_base_fee_arg(command: AnvilOptions) -> None:
            if base_fee:
                command.append(f"--base-fee={base_fee}")

        def _parse_block_number_arg(command: AnvilOptions) -> None:
            if fork_block is not None:
                command.append(f"--fork-block-number={fork_block}")

        def _parse_mining_mode_arg(command: AnvilOptions) -> None:
            match mining_mode:
                case "auto":
                    return
                case "interval":
                    if mining_interval is None:
                        raise DegenbotValueError(
                            message="Interval mining mode was specified without an interval value."
                        )
                    command.append(f"--block-time={mining_interval}")
                case "none":
                    command.append("--no-mining")
                    command.append("--order=fifo")
                case _:
                    raise DegenbotValueError(message=f"Unknown mining mode '{mining_mode}'.")

        def _parse_storage_caching_arg(command: AnvilOptions) -> None:
            if storage_caching is False:
                command.append("--no-storage-caching")

        def _parse_transaction_hash_arg(command: AnvilOptions) -> None:
            if fork_transaction_hash:
                command.append(f"--fork-transaction-hash={fork_transaction_hash}")

        _path_to_anvil = shutil.which("anvil")
        if _path_to_anvil is None:  # pragma: no cover
            raise AnvilNotFound
        path_to_anvil = pathlib.Path(_path_to_anvil)

        tmp_dir = pathlib.Path(tempfile.gettempdir())
        self.ipc_path = tmp_dir if ipc_path is None else ipc_path
        self.capture_path = tmp_dir if capture_path is None else capture_path
        self.preserve_capture = preserve_capture

        if ipc_provider_kwargs is not None:
            self.ipc_provider_kwargs = ipc_provider_kwargs
        else:
            self.ipc_provider_kwargs = {}

        self.port = self._get_free_port_number()

        command: AnvilOptions = [
            str(path_to_anvil),
            "--auto-impersonate",
            "--no-rate-limit",
            f"--fork-url={fork_url}",
            f"--port={self.port}",
            f"--ipc={self.ipc_filename}",
            f"--mnemonic={mnemonic}",
        ]
        _parse_base_fee_arg(command)
        _parse_block_number_arg(command)
        _parse_mining_mode_arg(command)
        _parse_storage_caching_arg(command)
        _parse_transaction_hash_arg(command)
        if anvil_opts:
            command.extend(anvil_opts)

        self._anvil_command = command
        self._setup_process(self._anvil_command)
        self._setup_w3()

        self._fork_url = fork_url

        if middlewares is not None:
            for middleware, layer in middlewares:
                self.w3.middleware_onion.inject(middleware, layer=layer)

        if balance_overrides is not None:
            for account, balance in balance_overrides:
                self.set_balance(account, balance)

        if bytecode_overrides is not None:
            for account, bytecode in bytecode_overrides:
                self.set_code(account, bytecode)

        if nonce_overrides is not None:
            for account, nonce in nonce_overrides:
                self.set_nonce(account, nonce)

        if storage_overrides is not None:
            for address, position, value in storage_overrides:
                self.set_storage(
                    address=address,
                    position=position,
                    value=value,
                )

        if coinbase is not None:
            self.set_coinbase(coinbase)

        if mining_interval:
            self.set_block_timestamp_interval(mining_interval)

    @property
    def fork_url(self) -> str:
        return self._fork_url

    @property
    def http_url(self) -> str:
        return f"http://localhost:{self.port}"

    @property
    def ipc_filename(self) -> pathlib.Path:
        return self.ipc_path / f"anvil-{self.port}.ipc"

    @property
    def stderr_capture_filename(self) -> pathlib.Path:
        return self.capture_path / f"anvil-{self.port}.stderr"

    @property
    def stdout_capture_filename(self) -> pathlib.Path:
        return self.capture_path / f"anvil-{self.port}.stdout"

    @property
    def ws_url(self) -> str:
        return f"ws://localhost:{self.port}"

    @staticmethod
    def _get_free_port_number() -> int:
        with socket.socket() as sock:
            sock.bind(("", 0))
            _, _port = sock.getsockname()
            return cast("int", _port)

    def _setup_w3(self) -> None:
        """
        Create a Web3 connection to the IPC socket used by Anvil, waiting for the connection to be
        established.
        """

        try:
            # network I/O is less reliable, so wait with an exponential delay and jitter
            w3 = Web3(IPCProvider(ipc_path=self.ipc_filename, **self.ipc_provider_kwargs))
            w3_connected_check_with_retry = tenacity.Retrying(
                stop=tenacity.stop_after_delay(10),
                wait=tenacity.wait_exponential_jitter(),
                retry=tenacity.retry_if_result(lambda result: result is False),
            )
            w3_connected_check_with_retry(fn=w3.is_connected)
        except tenacity.RetryError as exc:
            raise Web3ConnectionTimeout(timeout_seconds=timeout) from exc

        self.w3 = w3

    def _setup_process(self, anvil_command: AnvilOptions) -> None:
        """
        Launch an Anvil subprocess, waiting for the IPC socket to be created.
        """

        with (
            self.stderr_capture_filename.open("w") as stderr_capture,
            self.stdout_capture_filename.open("w") as stdout_capture,
        ):
            process = subprocess.Popen(  # noqa: S603
                anvil_command,
                stderr=stderr_capture,
                stdout=stdout_capture,
                text=True,
            )

        try:
            # Storage I/O should be fast, so use a low fixed wait time
            filename_check_with_retry = tenacity.Retrying(
                stop=tenacity.stop_after_delay(10),
                wait=tenacity.wait_fixed(0.01),
                retry=tenacity.retry_if_result(lambda result: result is False),
            )
            filename_check_with_retry(fn=self.ipc_filename.exists)
        except tenacity.RetryError as exc:
            process.terminate()
            raise DegenbotError(message="Timed out waiting for IPC socket to be created.") from exc
        else:
            self._process = process

    def __del__(self) -> None:
        self.close()

    def close(self) -> None:
        if getattr(self, "_process", None):
            self._process.terminate()
            self._process.wait(10)
            self.ipc_filename.unlink(missing_ok=True)
            del self._process

        if not self.preserve_capture:
            self.stderr_capture_filename.unlink(missing_ok=True)
            self.stdout_capture_filename.unlink(missing_ok=True)

    def mine(self) -> None:
        method = "evm_mine"
        resp = self.w3.provider.make_request(
            method=RPCEndpoint(method),
            params=[],
        )
        if "error" in resp:
            raise AnvilError(method=method, error=str(resp["error"]))

    async def mine_async(self) -> None:
        async with self.async_w3() as async_w3:
            await async_w3.provider.make_request(
                method=RPCEndpoint("evm_mine"),
                params=[],
            )

    @contextlib.asynccontextmanager
    async def async_w3(self) -> AsyncIterator[AsyncWeb3[AsyncBaseProvider]]:
        async with AsyncWeb3(AsyncIPCProvider(self.ipc_filename)) as async_w3:
            if TYPE_CHECKING:
                assert isinstance(async_w3, AsyncWeb3)
            yield async_w3

    async def reset_async(
        self,
        block_number: BlockNumber,
    ) -> None:
        """
        Reset to a new block number.
        """

        method = "anvil_reset"
        async with self.async_w3() as async_w3:
            resp = await async_w3.provider.make_request(
                method=RPCEndpoint(method),
                params=[{"forking": {"blockNumber": block_number}}],
            )
            if "error" in resp:
                raise AnvilError(method=method, error=str(resp["error"]))

    def reset(
        self,
        fork_url: str | None = None,
        block_number: BlockNumber | None = None,
        transaction_hash: str | None = None,
    ) -> None:
        """
        Fork from a new endpoint, block number, or transaction hash.

        Resetting to a new block number only can be done in-place without relaunching the Anvil
        process or recreating the Web3 object. Resetting to a new endpoint or from a transaction
        hash will create a new Anvil process, which is slower.
        """

        if fork_url is not None or transaction_hash is not None:
            self.close()

            if block_number is not None:
                logger.warning(
                    f"Forking from transaction hash {transaction_hash}, ignoring provided block number."  # noqa:E501
                )

            # Sanitize the command by stripping options that may conflict
            self._anvil_command = [
                option
                for option in self._anvil_command.copy()
                if all(
                    (
                        "--fork-url" not in option,
                        "--fork-block-number" not in option,
                        "--fork-transaction-hash" not in option,
                    )
                )
            ]

            # Fork URL must be provided since a new process is being launched
            if fork_url is not None:
                self._fork_url = fork_url
            self._anvil_command.append(f"--fork-url={self._fork_url}")

            if block_number is not None:
                self._anvil_command.append(f"--fork-block-number={block_number}")

            if transaction_hash is not None:
                self._anvil_command.append(f"--fork-transaction-hash={transaction_hash}")

            self._setup_process(self._anvil_command)
            self._setup_w3()

        elif block_number is not None:
            # Otherwise, the fork can be reset in place without launching a new process
            fork_params = {}
            if block_number:
                fork_params["blockNumber"] = block_number

            method = "anvil_reset"
            resp = self.w3.provider.make_request(
                method=RPCEndpoint(method),
                params=[{"forking": fork_params}],
            )
            if "error" in resp:
                raise AnvilError(method=method, error=str(resp["error"]))

        else:
            raise DegenbotValueError(message="No options provided.")

    def return_to_snapshot(self, snapshot_id: int) -> None:
        if snapshot_id < 0:
            raise DegenbotValueError(message="ID cannot be negative")

        method = "evm_revert"
        resp = self.w3.provider.make_request(
            method=RPCEndpoint(method),
            params=[snapshot_id],
        )
        if "error" in resp:
            raise AnvilError(method=method, error=str(resp["error"]))

        # Check if the revert was successful (Anvil returns False for invalid snapshots)
        if resp.get("result") is False:
            raise AnvilError(
                method=method,
                error=f"Failed to revert to snapshot {snapshot_id}",
            )

    @validate_call
    def set_balance(
        self,
        address: str,
        balance: ValidatedUint256,
    ) -> None:
        self.w3.provider.make_request(
            method=RPCEndpoint("anvil_setBalance"),
            params=[address, hex(balance)],
        )

    def set_code(self, address: str, bytecode: bytes) -> None:
        self.w3.provider.make_request(
            method=RPCEndpoint("anvil_setCode"),
            params=[address, bytecode],
        )

    def set_coinbase(self, address: str) -> None:
        self.w3.provider.make_request(
            method=RPCEndpoint("anvil_setCoinbase"),
            params=[address],
        )

    def set_block_timestamp_interval(self, interval: int) -> None:
        self.w3.provider.make_request(
            method=RPCEndpoint("anvil_setBlockTimestampInterval"),
            params=[interval],
        )

    @validate_call
    async def set_next_base_fee_async(
        self,
        fee: ValidatedUint256,
    ) -> None:
        method = "anvil_setNextBlockBaseFeePerGas"
        async with self.async_w3() as async_w3:
            resp = await async_w3.provider.make_request(
                method=RPCEndpoint(method),
                params=[fee],
            )
            if "error" in resp:
                raise AnvilError(method=method, error=str(resp["error"]))

    @validate_call
    def set_next_base_fee(
        self,
        fee: ValidatedUint256,
    ) -> None:
        method = "anvil_setNextBlockBaseFeePerGas"
        resp = self.w3.provider.make_request(
            method=RPCEndpoint(method),
            params=[fee],
        )
        if "error" in resp:
            raise AnvilError(method=method, error=str(resp["error"]))

    @validate_call
    async def set_next_block_timestamp_async(
        self,
        timestamp: ValidatedUint256,
    ) -> None:
        method = "evm_setNextBlockTimestamp"
        async with self.async_w3() as async_w3:
            resp = await async_w3.provider.make_request(
                method=RPCEndpoint(method),
                params=[timestamp],
            )
            if "error" in resp:
                raise AnvilError(method=method, error=str(resp["error"]))

    @validate_call
    def set_next_block_timestamp(
        self,
        timestamp: ValidatedUint256,
    ) -> None:
        method = "evm_setNextBlockTimestamp"
        resp = self.w3.provider.make_request(
            method=RPCEndpoint(method),
            params=[timestamp],
        )
        if "error" in resp:
            raise AnvilError(method=method, error=str(resp["error"]))

    def set_nonce(self, address: str, nonce: int) -> None:
        method = "anvil_setNonce"
        resp = self.w3.provider.make_request(
            method=RPCEndpoint(method),
            params=[address, nonce],
        )
        if "error" in resp:
            raise AnvilError(method=method, error=str(resp["error"]))

    def set_snapshot(self) -> int:
        return int(
            self.w3.provider.make_request(
                method=RPCEndpoint("evm_snapshot"),
                params=[],
            )["result"],
            16,
        )

    def set_storage(
        self,
        address: HexAddress | bytes,
        position: int,
        value: HexStr | bytes | int,
    ) -> None:
        self.w3.provider.make_request(
            method=RPCEndpoint("anvil_setStorageAt"),
            params=[
                address,
                position,
                (
                    # Storage value must be padded to 32 bytes
                    HexBytes(value).hex().zfill(64)
                ),
            ],
        )
