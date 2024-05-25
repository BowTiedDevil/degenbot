import os
import shutil
import socket
import subprocess
from typing import Any, Dict, Iterable, List, Literal, Tuple, cast

import ujson
from eth_typing import HexAddress
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes
from web3 import IPCProvider, Web3
from web3.types import Middleware

from ..constants import MAX_UINT256
from ..logging import logger

_SOCKET_READ_BUFFER_SIZE = 4096  # https://docs.python.org/3/library/socket.html#socket.socket.recv


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
        gas_limit: int = 30_000_000,
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
        middlewares: List[Tuple[Middleware, int]] | None = None,
        balance_overrides: Iterable[Tuple[HexAddress, int]] | None = None,
        bytecode_overrides: Iterable[Tuple[HexAddress, bytes]] | None = None,
        ipc_provider_kwargs: Dict[str, Any] | None = None,
    ):
        def get_free_port_number() -> int:
            with socket.socket() as sock:
                sock.bind(("", 0))
                _, port = sock.getsockname()
                return cast(int, port)

        if shutil.which("anvil") is None:  # pragma: no cover
            raise Exception("Anvil is not installed or not accessible in the current path.")

        self.port = port if port is not None else get_free_port_number()

        ipc_path = f"/tmp/anvil-{self.port}.ipc" if ipc_path is None else ipc_path

        command = []
        command.append("anvil")
        command.append("--silent")
        command.append("--auto-impersonate")
        command.append("--no-rate-limit")
        command.append(f"--fork-url={fork_url}")
        command.append(f"--hardfork={hardfork}")
        command.append(f"--gas-limit={gas_limit}")
        command.append(f"--port={self.port}")
        command.append(f"--ipc={ipc_path}")
        command.append(f"--mnemonic={mnemonic}")
        if fork_block:
            command.append(f"--fork-block-number={fork_block}")
        if chain_id:
            command.append(f"--chain-id={chain_id}")
        if base_fee:
            command.append(f"--base-fee={base_fee}")
        if storage_caching is False:
            command.append("--no-storage-caching")
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

        self._process = subprocess.Popen(command)
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

        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.connect(self.ipc_path)

        self._initial_block_number = (
            fork_block if fork_block is not None else self.w3.eth.get_block_number()
        )
        self.chain_id = chain_id if chain_id is not None else self.w3.eth.chain_id

        if balance_overrides is not None:
            for account, balance in balance_overrides:
                self.set_balance(account, balance)

        if bytecode_overrides is not None:
            for account, bytecode in bytecode_overrides:
                self.set_code(account, bytecode)

        if coinbase is not None:
            self.set_coinbase(coinbase)

    def __del__(self) -> None:
        self._process.terminate()
        self._process.wait()
        try:
            os.remove(self.ipc_path)
        except Exception:
            pass

    def _socket_request(self, method: str, params: List[Any] | None = None) -> None:
        """
        Send a JSON-formatted request through the socket.
        """
        if params is None:
            params = []

        self.socket.sendall(
            bytes(
                ujson.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": None,
                        "method": method,
                        "params": params,
                    }
                ),
                encoding="utf-8",
            ),
        )

    def _socket_response(self) -> Any:
        """
        Read the response payload from socket and return the JSON-decoded result.
        """
        raw_response = b""
        response: Dict[str, Any]
        while True:
            try:
                raw_response += self.socket.recv(_SOCKET_READ_BUFFER_SIZE).rstrip()
                response = ujson.loads(raw_response)
                break
            except socket.timeout:  # pragma: no cover
                continue
            except ujson.JSONDecodeError:  # pragma: no cover
                # Typically thrown if a long response does not fit in buffer length
                continue

        if response.get("error"):
            raise Exception(f"Error in response: {response}")
        return response["result"]

    def create_access_list(self, transaction: Dict[Any, Any]) -> Any:
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

        self._socket_request(
            method="eth_createAccessList",
            params=[sanitized_tx],
        )
        response: Dict[Any, Any] = self._socket_response()
        return response["accessList"]

    def mine(self) -> None:
        self._socket_request(method="evm_mine")
        self._socket_response()

    def reset(
        self,
        fork_url: str | None = None,
        block_number: int | None = None,
        base_fee: int | None = None,
    ) -> None:
        forking_params: Dict[str, Any] = {
            "jsonRpcUrl": fork_url if fork_url is not None else self.fork_url,
            "blockNumber": block_number if block_number is not None else self._initial_block_number,
        }

        self._socket_request(
            method="anvil_reset",
            params=[{"forking": forking_params}],
        )
        self._socket_response()

        if fork_url is not None:
            self.fork_url = fork_url
        if base_fee is not None:
            self.set_next_base_fee(base_fee)

    def return_to_snapshot(self, id: int) -> bool:
        if id < 0:
            raise ValueError("ID cannot be negative")
        self._socket_request(
            method="evm_revert",
            params=[id],
        )
        return bool(self._socket_response())

    def set_balance(self, address: str, balance: int) -> None:
        if not (0 <= balance <= MAX_UINT256):
            raise ValueError("Invalid balance, must be within range: 0 <= balance <= 2**256 - 1")
        self._socket_request(
            method="anvil_setBalance",
            params=[
                to_checksum_address(address),
                hex(balance),
            ],
        )
        self._socket_response()

    def set_code(self, address: str, bytecode: bytes) -> None:
        self._socket_request(
            method="anvil_setCode",
            params=[
                HexBytes(address).hex(),
                HexBytes(bytecode).hex(),
            ],
        )
        self._socket_response()

    def set_coinbase(self, address: str) -> None:
        self._socket_request(
            method="anvil_setCoinbase",
            params=[HexBytes(address).hex()],
        )
        self._socket_response()

    def set_next_base_fee(self, fee: int) -> None:
        if not (0 <= fee <= MAX_UINT256):
            raise ValueError("Fee outside valid range 0 <= fee <= 2**256-1")
        self._socket_request(
            method="anvil_setNextBlockBaseFeePerGas",
            params=[hex(fee)],
        )
        self._socket_response()
        self.base_fee_next = fee

    def set_snapshot(self) -> int:
        self._socket_request(
            method="evm_snapshot",
        )
        return int(self._socket_response(), 16)
