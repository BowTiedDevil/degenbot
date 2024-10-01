import contextlib
import os
import shutil
import socket
import subprocess
from collections.abc import Iterable
from typing import Any, Literal, cast

from eth_typing import HexAddress
from web3 import IPCProvider, Web3
from web3.middleware import Middleware
from web3.types import RPCEndpoint

from .constants import MAX_UINT256
from .logging import logger


class AnvilFork:
    """
    Launch an Anvil fork as a separate process and expose methods for commonly-used RPC calls.

    Provides a `Web3` connector to Anvil's IPC socket endpoint at the `.w3` attribute.
    """

    def __init__(
        self,
        fork_url: str,
        fork_block: int | None = None,
        hardfork: str = "latest",
        port: int | None = None,
        chain_id: int | None = None,
        mining_mode: Literal["auto", "interval", "none"] = "auto",
        mining_interval: int = 12,
        storage_caching: bool = True,
        base_fee: int | None = None,
        ipc_path: str | None = None,
        mnemonic: str = (
            # Default mnemonic used by Brownie for Ganache forks
            "patient rude simple dog close planet oval animal hunt sketch suspect slim"
        ),
        coinbase: HexAddress | None = None,
        middlewares: list[tuple[Middleware, int]] | None = None,
        balance_overrides: Iterable[tuple[HexAddress, int]] | None = None,
        bytecode_overrides: Iterable[tuple[HexAddress, bytes]] | None = None,
        nonce_overrides: Iterable[tuple[HexAddress, int]] | None = None,
        ipc_provider_kwargs: dict[str, Any] | None = None,
        prune_history: bool = False,
    ):
        def build_anvil_command() -> list[str]:  # pragma: no cover
            command = [
                "anvil",
                "--silent",
                "--auto-impersonate",
                "--no-rate-limit",
                f"--fork-url={fork_url}",
                f"--hardfork={hardfork}",
                f"--port={self.port}",
                f"--ipc={ipc_path}",
                f"--mnemonic={mnemonic}",
            ]
            if fork_block:
                command.append(f"--fork-block-number={fork_block}")
            if chain_id:
                command.append(f"--chain-id={chain_id}")
            if base_fee:
                command.append(f"--base-fee={base_fee}")
            if storage_caching is False:
                command.append("--no-storage-caching")
            if prune_history:
                command.append("--prune-history")
            match mining_mode:
                case "auto":
                    pass
                case "interval":
                    logger.debug(f"Using 'interval' mining with {mining_interval}s block times.")
                    command.append(f"--block-time={mining_interval}")
                case "none":
                    command.append("--no-mining")
                    command.append("--order=fifo")
                case _:
                    raise ValueError(f"Unknown mining mode '{mining_mode}'.")

            return command

        def get_free_port_number() -> int:
            with socket.socket() as sock:
                sock.bind(("", 0))
                _, _port = sock.getsockname()
                return cast(int, _port)

        if shutil.which("anvil") is None:  # pragma: no cover
            raise Exception("Anvil is not installed or not accessible in the current path.")

        self.port = port if port is not None else get_free_port_number()

        ipc_path = f"/tmp/anvil-{self.port}.ipc" if ipc_path is None else ipc_path

        self._process = subprocess.Popen(build_anvil_command())
        self.fork_url = fork_url
        self.http_url = f"http://localhost:{self.port}"
        self.ws_url = f"ws://localhost:{self.port}"
        self.ipc_path = ipc_path

        if ipc_provider_kwargs is None:
            ipc_provider_kwargs = dict()
        self.w3 = Web3(IPCProvider(ipc_path=ipc_path, **ipc_provider_kwargs))

        if middlewares is not None:
            for middleware, layer in middlewares:
                self.w3.middleware_onion.inject(middleware, layer=layer)

        while self.w3.is_connected() is False:
            continue

        self._initial_block_number = (
            fork_block if fork_block is not None else self.w3.eth.get_block_number()
        )

        if balance_overrides is not None:
            for account, balance in balance_overrides:
                self.set_balance(account, balance)

        if bytecode_overrides is not None:
            for account, bytecode in bytecode_overrides:
                self.set_code(account, bytecode)

        if nonce_overrides is not None:
            for account, nonce in nonce_overrides:
                self.set_nonce(account, nonce)

        if coinbase is not None:
            self.set_coinbase(coinbase)

    def __del__(self) -> None:
        self._process.terminate()
        self._process.wait()
        with contextlib.suppress(FileNotFoundError):
            os.remove(self.ipc_path)

    def create_access_list(self, transaction: dict[Any, Any]) -> Any:
        # Exclude transaction values that are irrelevant for the JSON-RPC method
        # ref: https://docs.infura.io/networks/ethereum/json-rpc-methods/eth_createaccesslist
        keys_to_drop = ("gasPrice", "maxFeePerGas", "maxPriorityFeePerGas", "gas", "chainId")
        sanitized_tx = {k: v for k, v in transaction.items() if k not in keys_to_drop}

        # Apply int->hex conversion to some transaction values
        # ref: https://docs.infura.io/networks/ethereum/json-rpc-methods/eth_createaccesslist
        keys_to_hexify = ("value", "nonce")
        for key in keys_to_hexify:
            if key in sanitized_tx and isinstance(sanitized_tx[key], int):
                sanitized_tx[key] = hex(sanitized_tx[key])

        return self.w3.provider.make_request(
            method=RPCEndpoint("eth_createAccessList"),
            params=[sanitized_tx],
        )["result"]["accessList"]

    def mine(self) -> None:
        self.w3.provider.make_request(
            method=RPCEndpoint("evm_mine"),
            params=[],
        )

    def reset(
        self,
        fork_url: str | None = None,
        block_number: int | None = None,
        base_fee: int | None = None,
    ) -> None:
        forking_params: dict[str, Any] = {
            "jsonRpcUrl": fork_url if fork_url is not None else self.fork_url,
            "blockNumber": block_number if block_number is not None else self._initial_block_number,
        }
        self.w3.provider.make_request(
            method=RPCEndpoint("anvil_reset"),
            params=[{"forking": forking_params}],
        )

        if fork_url is not None:
            self.fork_url = fork_url
        if base_fee is not None:
            self.set_next_base_fee(base_fee)

    def return_to_snapshot(self, id: int) -> bool:
        if id < 0:
            raise ValueError("ID cannot be negative")
        return bool(
            self.w3.provider.make_request(
                method=RPCEndpoint("evm_revert"),
                params=[id],
            )["result"]
        )

    def set_balance(self, address: str, balance: int) -> None:
        if not (0 <= balance <= MAX_UINT256):
            raise ValueError("Invalid balance, must be within range: 0 <= balance <= 2**256 - 1")

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

    def set_next_base_fee(self, fee: int) -> None:
        if not (0 <= fee <= MAX_UINT256):
            raise ValueError("Fee outside valid range 0 <= fee <= 2**256-1")
        self.w3.provider.make_request(
            method=RPCEndpoint("anvil_setNextBlockBaseFeePerGas"),
            params=[fee],
        )

    def set_nonce(self, address: str, nonce: int) -> None:
        self.w3.provider.make_request(
            method=RPCEndpoint("anvil_setNonce"),
            params=[address, nonce],
        )

    def set_snapshot(self) -> int:
        return int(
            self.w3.provider.make_request(
                method=RPCEndpoint("evm_snapshot"),
                params=[],
            )["result"],
            16,
        )
