"""Anvil fork management utilities for local test chains."""

import contextlib
import os
import pathlib
import shutil
import signal
import socket
import subprocess  # noqa: S404
import tempfile
import time
import weakref
from collections.abc import AsyncIterator, Iterable
from typing import IO, TYPE_CHECKING, Any, ClassVar, Literal, cast

import tenacity
from eth_typing import HexAddress, HexStr
from hexbytes import HexBytes
from pydantic import validate_call
from web3 import AsyncBaseProvider, AsyncIPCProvider, AsyncWeb3, IPCProvider, Web3
from web3.middleware import Middleware
from web3.types import RPCEndpoint

from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.infrastructure import AnvilError, IPCSocketTimeout, Web3ConnectionTimeout
from degenbot.logging import logger
from degenbot.types.aliases import BlockNumber
from degenbot.validation.evm_values import ValidatedUint256


class AnvilNotFound(Exception):
    """AnvilNotFound class."""

    def __init__(self) -> None:  # pragma: no cover
        """Initialize the instance."""
        super().__init__("Anvil path could not be located.")


class _AnvilStartupCrash(Exception):
    """Internal: the anvil subprocess exited before its IPC socket appeared.

    Raised by :meth:`AnvilFork._launch_anvil` when ``process.poll()`` returns
    non-None during the socket-wait loop — e.g. anvil panicked while installing
    its Ctrl-C/SIGINT handler under heavy concurrent startup (``EAGAIN``) and
    exited without ever creating the IPC socket. :meth:`_setup_process` retries
    the whole launch on this, so it is never surfaced to callers.
    """

    def __init__(self, *, returncode: int | None) -> None:
        """Initialize the instance."""
        self.returncode = returncode
        super().__init__(
            f"Anvil process exited (code {returncode}) before its IPC socket appeared.",
        )


type AnvilOptions = list[str]


class AnvilFork:
    """Launch an Anvil fork as a separate process and expose methods for commonly-used RPC calls.

    Provides a `Web3` connector to Anvil's IPC socket endpoint at the `.w3` attribute.
    """

    # Every live AnvilFork is tracked here (weakly) so :meth:`close_all` can reap
    # forks a caller forgot to ``close()`` — mirroring
    # ``DatabaseSessionManager.dispose_all()``. Without this, direct constructions
    # in tests defer cleanup to non-deterministic ``__del__``/GC; under xdist
    # fan-out the deferred anvil subprocesses (each ~27 pids) pile up against the
    # container ``pids.max`` and crash it.
    _LIVE: ClassVar["weakref.WeakSet[AnvilFork]"] = weakref.WeakSet()

    def __init__(
        self,
        *,
        localhost: str = "127.0.0.1",
        fork_url: str | None = None,
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
        setup_timeout: int = 300,
    ) -> None:
        """Initialize the instance.

        Raises:
            AnvilNotFound: See function documentation.

        """

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
                            message="Interval mining mode was specified without an interval value.",
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

        if (which_path := shutil.which("anvil")) is None:  # pragma: no cover
            raise AnvilNotFound
        anvil_path = pathlib.Path(which_path).absolute()

        tmp_dir = pathlib.Path(tempfile.gettempdir())
        self.ipc_path = tmp_dir if ipc_path is None else ipc_path
        self.capture_path = tmp_dir if capture_path is None else capture_path
        self.preserve_capture = preserve_capture

        if ipc_provider_kwargs is not None:
            self.ipc_provider_kwargs = ipc_provider_kwargs
        else:
            self.ipc_provider_kwargs = {}

        # Per-launch budget for the anvil subprocess to come up: how long to wait
        # for the IPC socket file to appear, for the process to respond via IPC,
        # and how long the crash-retry loop keeps trying. Bumped from 10s because
        # under heavy ``pytest-xdist`` parallelism (16+ workers each spawning a
        # remote-forking anvil) a single worker's anvil can take >10s to fetch
        # initial state from the remote node, *and* anvil can transiently crash
        # at startup (its Ctrl-C/SIGINT handler returns EAGAIN under contention)
        # — the retry loop below needs headroom to re-launch.
        self._setup_timeout = setup_timeout

        self.localhost = localhost
        self.port = self._get_free_port_number()

        command: AnvilOptions = [
            str(anvil_path),
            "--auto-impersonate",
            f"--port={self.port}",
            f"--ipc={self.ipc_filename}",
            f"--mnemonic={mnemonic}",
        ]

        # Only add fork_url and --no-rate-limit if provided (standalone mode when None)
        if fork_url is not None:
            command.append(f"--fork-url={fork_url}")
            command.append("--no-rate-limit")

        _parse_base_fee_arg(command)
        _parse_block_number_arg(command)
        _parse_mining_mode_arg(command)
        _parse_storage_caching_arg(command)
        _parse_transaction_hash_arg(command)
        if anvil_opts:
            command.extend(anvil_opts)

        self._anvil_command = command
        self._setup_process(self._anvil_command, timeout=self._setup_timeout)
        self._setup_w3(timeout=self._setup_timeout)

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
    def fork_url(self) -> str | None:
        """Return fork url."""
        return self._fork_url

    @property
    def http_url(self) -> str:
        """Return http url."""
        return f"http://{self.localhost}:{self.port}"

    @property
    def ipc_filename(self) -> pathlib.Path:
        """Ipc filename."""
        return self.ipc_path / f"anvil-{self.port}.ipc"

    @property
    def stderr_capture_filename(self) -> pathlib.Path:
        """Stderr capture filename."""
        return self.capture_path / f"anvil-{self.port}.stderr"

    @property
    def stdout_capture_filename(self) -> pathlib.Path:
        """Stdout capture filename."""
        return self.capture_path / f"anvil-{self.port}.stdout"

    @property
    def ws_url(self) -> str:
        """Return ws url."""
        return f"ws://{self.localhost}:{self.port}"

    @staticmethod
    def _get_free_port_number() -> int:
        with socket.socket() as sock:
            sock.bind(("", 0))
            _, port = sock.getsockname()
            return cast("int", port)

    def _setup_w3(self, timeout: int = 10) -> None:
        try:
            # network I/O is less reliable, so wait with an exponential delay and jitter
            w3 = Web3(IPCProvider(ipc_path=self.ipc_filename, **self.ipc_provider_kwargs))
            w3_connected_check_with_retry = tenacity.Retrying(
                stop=tenacity.stop_after_delay(timeout),
                wait=tenacity.wait_exponential_jitter(),
                retry=tenacity.retry_if_result(lambda result: result is False),
            )
            w3_connected_check_with_retry(fn=w3.is_connected)
        except tenacity.RetryError as exc:
            raise Web3ConnectionTimeout(timeout_seconds=timeout) from exc

        self.w3 = w3

    def _setup_process(self, anvil_command: AnvilOptions, timeout: int = 30) -> None:
        """Launch an Anvil subprocess, waiting for the IPC socket to be created.

        See :meth:`_launch_anvil` for the per-launch contract; this method wraps
        it in a bounded retry for transient ``BlockingIOError`` / anvil startup
        crashes.

        """
        # Log the command being executed for debugging
        logger.debug(f"Launching Anvil with command: {' '.join(anvil_command)}")

        # Anvil must come up from underneath us in (at least) two transient ways
        # under high ``pytest-xdist`` parallelism, so retry the whole launch with
        # backoff rather than failing the test:
        #
        #   1. ``subprocess.Popen`` returns ``BlockingIOError(EAGAIN)`` when the
        #      container's cgroup ``pids.max`` is transiently exhausted (each anvil
        #      process holds ~27 pids/threads; a burst at fixture setup can hit the
        #      ceiling). Peer forks are concurrently tearing down and free slots.
        #
        #   2. The anvil process spawns successfully but *crashes during its own
        #      startup* — notably it panics installing its Ctrl-C/SIGINT handler
        #      (``Error setting Ctrl-C handler: System(... EAGAIN ...)``) under
        #      contention — and exits before ever creating the IPC socket. The old
        #      code waited the full ``timeout`` for a socket file that would never
        #      appear; instead we detect the early death (``poll()`` returns
        #      non-None) and re-launch.
        #
        # The retry is intentionally **bounded by attempt count, not duration**.
        # Under sustained contention (e.g. the test-suite tail where every xdist
        # worker is simultaneously launching a remote-forking anvil) a long
        # duration-budget retry holds each worker for the full budget while it
        # re-crashes, piling up *more* concurrent launches and deepening the
        # very contention that caused the crashes — a death spiral that also
        # drags the suite toward timeout. A small attempt cap with short waits
        # recovers a single transient crash quickly (anvil exits in milliseconds)
        # and lets sustained contention fail fast so the worker is released
        # instead of held.
        #
        # The wait is **jittered** rather than fixed: a fixed delay re-collides
        # — when several workers crash on the same contention burst they all
        # retry together at +0.25s, re-triggering the burst. Randomised waits
        # desynchronise the recoveries so concurrent crash-retries don't
        # re-panick anvil's Ctrl-C handler, which is what fails under EAGAIN.
        # ``timeout`` still bounds the *per-attempt* IPC socket wait for an anvil
        # that is alive but slow to finish initialising.
        boot_retry = tenacity.Retrying(
            stop=tenacity.stop_after_attempt(6),
            wait=tenacity.wait_random(min=0.05, max=0.5),
            retry=tenacity.retry_if_exception_type((BlockingIOError, _AnvilStartupCrash)),
            reraise=True,
        )

        # Capture files are opened once and reused across retry attempts so a
        # crashed anvil's panic trace is preserved in the stderr capture for
        # debugging rather than truncated by the relaunch.
        with (
            self.stderr_capture_filename.open("w") as stderr_capture,
            self.stdout_capture_filename.open("w") as stdout_capture,
        ):
            process = boot_retry(
                self._launch_anvil,
                anvil_command,
                stderr_capture,
                stdout_capture,
                timeout,
            )

        self._process = process
        AnvilFork._LIVE.add(self)

    def _launch_anvil(
        self,
        anvil_command: AnvilOptions,
        stderr_capture: IO[str],
        stdout_capture: IO[str],
        timeout: int,
    ) -> subprocess.Popen:
        """Spawn one anvil process and wait for its IPC socket to appear.

        Returns:
            The live ``subprocess.Popen`` once the IPC socket exists.

        Raises:
            _AnvilStartupCrash: if the process exits before creating the
                socket, so :meth:`_setup_process` retries the whole launch.
            IPCSocketTimeout: if the socket does not appear within ``timeout``
                seconds (the process is alive but slow to finish initialising).

        ``BlockingIOError`` may also propagate from ``subprocess.Popen`` when the
        cgroup pid budget is transiently exhausted; :meth:`_setup_process`
        retries that upstream.

        """
        # start_new_session puts anvil in its own process group/session so
        # close() can reap the entire group; this also prevents anvil from
        # surviving a SIGKILL'd pytest-xdist worker (which bypasses __del__).
        process = subprocess.Popen(  # noqa: S603
            anvil_command,
            stderr=stderr_capture,
            stdout=stdout_capture,
            text=True,
            start_new_session=True,
        )

        # Poll for the IPC socket file, but bail out immediately if the process
        # died (e.g. anvil panicked during Ctrl-C handler setup) instead of
        # waiting the full ``timeout`` for a file that will never appear. Using
        # a plain deadline loop (rather than a tenacity closure) keeps the
        # crash-detection raise at this scope — no nested closure to trip lint —
        # and lets sustained contention fail fast once the budget elapses.
        deadline = time.monotonic() + timeout
        while True:
            if self.ipc_filename.exists():
                return process
            returncode = process.poll()
            if returncode is not None:
                # Reap the dead process before relaunching so we don't leak a
                # zombie or hold the capture fds against the cgroup pid budget.
                with contextlib.suppress(subprocess.TimeoutExpired):
                    process.wait(timeout=timeout)
                raise _AnvilStartupCrash(returncode=returncode)
            if time.monotonic() >= deadline:
                process.terminate()
                raise IPCSocketTimeout(timeout_seconds=timeout)
            time.sleep(0.01)

    def __del__(self) -> None:
        """Implement __del__."""
        self.close()

    def close(self, timeout: int = 10) -> None:
        """Perform close."""
        # Close the web3 IPC socket so it is not left for GC to finalize (which raises
        # a ResourceWarning under strict warning filters).
        provider = getattr(self, "w3", None)
        if provider is not None:
            provider_socket = getattr(getattr(provider, "provider", None), "_socket", None)
            sock = getattr(provider_socket, "sock", None)
            if sock is not None:
                with contextlib.suppress(OSError):
                    sock.close()

        if getattr(self, "_process", None):
            # Reap anvil's whole process group (it runs in its own session via
            # start_new_session). terminate() sends SIGTERM to the leader; if the
            # group spawned helpers they'd survive a leader-only signal, so also
            # signal the group explicitly as a fallback before waiting.
            try:
                os.killpg(os.getpgid(self._process.pid), signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                self._process.terminate()
            self._process.wait(timeout)
            self.ipc_filename.unlink(missing_ok=True)
            del self._process

        if not self.preserve_capture:
            self.stderr_capture_filename.unlink(missing_ok=True)
            self.stdout_capture_filename.unlink(missing_ok=True)

    @classmethod
    def close_all(cls) -> None:
        """Close every still-live :class:`AnvilFork`.

        Test-suite safety net (mirrors ``DatabaseSessionManager.dispose_all``):
        called by the autouse teardown so an AnvilFork constructed inline and
        never ``close()`` ed is reaped deterministically instead of waiting on
        non-deterministic ``__del__``/GC. An assertion failure that holds the
        frame alive defeats GC, so without this the anvil subprocesses pile up
        under xdist fan-out and exhaust the container ``pids.max``. ``close()``
        is idempotent, so reaping an already-closed fork is a no-op.
        """
        for fork in list(cls._LIVE):
            fork.close()

    def mine(self) -> None:
        """Perform mine.

        Raises:
            AnvilError: See function documentation.

        """
        method = "evm_mine"
        resp = self.w3.provider.make_request(
            method=RPCEndpoint(method),
            params=[],
        )
        if "error" in resp:
            raise AnvilError(method=method, error=str(resp["error"]))

    async def mine_async(self) -> None:
        """Mine a single block asynchronously."""
        async with self.async_w3() as async_w3:
            await async_w3.provider.make_request(
                method=RPCEndpoint("evm_mine"),
                params=[],
            )

    @contextlib.asynccontextmanager
    async def async_w3(self) -> AsyncIterator[AsyncWeb3[AsyncBaseProvider]]:
        """Yield an async Web3 instance connected via IPC.

        Yields:
            AsyncWeb3: An async Web3 instance connected via IPC.

        """
        async with AsyncWeb3(AsyncIPCProvider(self.ipc_filename)) as async_w3:
            if TYPE_CHECKING:
                assert isinstance(async_w3, AsyncWeb3)
            yield async_w3

    async def reset_async(
        self,
        block_number: BlockNumber,
    ) -> None:
        """Reset to a new block number.

        Raises:
            AnvilError: See function documentation.

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
        """Fork from a new endpoint, block number, or transaction hash.

        Resetting to a new block number only can be done in-place without relaunching the Anvil
        process or recreating the Web3 object. Resetting to a new endpoint or from a transaction
        hash will create a new Anvil process, which is slower.

        Raises:
            AnvilError: See function documentation.
            DegenbotValueError: See function documentation.

        """
        if fork_url is not None or transaction_hash is not None:
            self.close()

            if block_number is not None:
                logger.warning(
                    f"Forking from transaction hash {transaction_hash}, ignoring provided block number.",  # noqa:E501
                )

            # Sanitize the command by stripping options that may conflict
            self._anvil_command = [
                option
                for option in self._anvil_command.copy()
                if all((
                    "--fork-url" not in option,
                    "--fork-block-number" not in option,
                    "--fork-transaction-hash" not in option,
                ))
            ]

            # Fork URL must be provided since a new process is being launched
            if fork_url is not None:
                self._fork_url = fork_url
            self._anvil_command.append(f"--fork-url={self._fork_url}")

            if block_number is not None:
                self._anvil_command.append(f"--fork-block-number={block_number}")

            if transaction_hash is not None:
                self._anvil_command.append(f"--fork-transaction-hash={transaction_hash}")

            self._setup_process(self._anvil_command, timeout=self._setup_timeout)
            self._setup_w3(timeout=self._setup_timeout)

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
        """Perform return to snapshot.

        Raises:
            AnvilError: See function documentation.
            DegenbotValueError: See function documentation.

        """
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
        """Set balance."""
        self.w3.provider.make_request(
            method=RPCEndpoint("anvil_setBalance"),
            params=[address, hex(balance)],
        )

    def set_code(self, address: str, bytecode: bytes) -> None:
        """Set code."""
        self.w3.provider.make_request(
            method=RPCEndpoint("anvil_setCode"),
            params=[address, bytecode],
        )

    def set_coinbase(self, address: str) -> None:
        """Set coinbase."""
        self.w3.provider.make_request(
            method=RPCEndpoint("anvil_setCoinbase"),
            params=[address],
        )

    def set_block_timestamp_interval(self, interval: int) -> None:
        """Set block timestamp interval."""
        self.w3.provider.make_request(
            method=RPCEndpoint("anvil_setBlockTimestampInterval"),
            params=[interval],
        )

    @validate_call
    async def set_next_base_fee_async(
        self,
        fee: ValidatedUint256,
    ) -> None:
        """Set the next block base fee asynchronously.

        Raises:
            AnvilError: See function documentation.

        """
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
        """Set next base fee.

        Raises:
            AnvilError: See function documentation.

        """
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
        """Set the next block timestamp asynchronously.

        Raises:
            AnvilError: See function documentation.

        """
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
        """Set next block timestamp.

        Raises:
            AnvilError: See function documentation.

        """
        method = "evm_setNextBlockTimestamp"
        resp = self.w3.provider.make_request(
            method=RPCEndpoint(method),
            params=[timestamp],
        )
        if "error" in resp:
            raise AnvilError(method=method, error=str(resp["error"]))

    def set_nonce(self, address: str, nonce: int) -> None:
        """Set nonce.

        Raises:
            AnvilError: See function documentation.

        """
        method = "anvil_setNonce"
        resp = self.w3.provider.make_request(
            method=RPCEndpoint(method),
            params=[address, nonce],
        )
        if "error" in resp:
            raise AnvilError(method=method, error=str(resp["error"]))

    def set_snapshot(self) -> int:
        """Set snapshot.

        Returns:
            The computed integer value.

        """
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
        """Set storage."""
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
