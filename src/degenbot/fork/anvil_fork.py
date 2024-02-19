import os
import shutil
import socket
import subprocess
from typing import Any, Dict, Iterable, List, Tuple

import ujson
from eth_typing import HexAddress
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes
from web3 import IPCProvider, Web3
from web3.types import Middleware

from ..constants import MAX_UINT256

_SOCKET_READ_BUFFER_SIZE = 4096  # https://docs.python.org/3/library/socket.html#socket.socket.recv


class AnvilFork:
    """
    Launch an Anvil fork via subprocess and provide various methods for
    interacting with it via JSON-RPC over the built-in IPC socket.
    """

    def __init__(
        self,
        fork_url: str,
        fork_block: int | None = None,
        hardfork: str = "latest",
        gas_limit: int = 30_000_000,
        port: int | None = None,
        chain_id: int | None = None,
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
    ):
        if shutil.which("anvil") is None:  # pragma: no cover
            raise Exception("Anvil is not installed or not accessible in the current path.")

        if not port:
            with socket.socket() as sock:
                sock.bind(("", 0))
                _, port = sock.getsockname()
        self.port = port

        if not ipc_path:
            ipc_path = f"/tmp/anvil-{self.port}.ipc"

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

        self._process = subprocess.Popen(command)
        self.fork_url = fork_url
        self.http_url = f"http://localhost:{self.port}"
        self.ws_url = f"ws://localhost:{self.port}"
        self.ipc_path = ipc_path
        self.w3 = Web3(IPCProvider(ipc_path))

        if middlewares is not None:
            for middleware, layer in middlewares:
                self.w3.middleware_onion.inject(middleware, layer=layer)

        while self.w3.is_connected() is False:
            continue

        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.connect(self.ipc_path)

        self.block_number = fork_block if fork_block is not None else self.w3.eth.get_block_number()
        self.base_fee = (
            base_fee
            if base_fee is not None
            else self.w3.eth.get_block(self.block_number)["baseFeePerGas"]
        )
        self.base_fee_next: int | None = None
        self.chain_id: int = chain_id if chain_id is not None else self.w3.eth.chain_id

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
        if os.path.exists(self.ipc_path):
            os.remove(self.ipc_path)

    def _send_request(self, method: str, params: List[Any] | None = None) -> None:
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

    def _get_response(self) -> Any:
        """
        Read the response payload from socket and return the JSON-decoded result.
        """
        raw_response = b""
        response: Dict[str, Any]
        while True:
            try:
                raw_response += self.socket.recv(_SOCKET_READ_BUFFER_SIZE).rstrip()
            except socket.timeout:
                continue

            if raw_response.endswith(
                (
                    b"}",
                    b"]",
                )
            ):
                # Valid JSON RPC responses will end in } or ]
                # ref: http://www.jsonrpc.org/specification
                try:
                    response = ujson.loads(raw_response)
                except ujson.JSONDecodeError:
                    # The end of the response might end in } or ] if the last byte is the closing
                    # character of an array or map. If there are more bytes remaining, the JSON
                    # string will fail to decode correctly, so continue reading from the socket.
                    continue
                else:
                    break

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
            if key in sanitized_tx:
                if isinstance(sanitized_tx[key], int):
                    sanitized_tx[key] = hex(sanitized_tx[key])

        self._send_request(
            method="eth_createAccessList",
            params=[sanitized_tx],
        )
        response: Dict[Any, Any] = self._get_response()
        return response["accessList"]

    def reset(
        self,
        fork_url: str | None = None,
        block_number: int | None = None,
    ) -> None:
        forking_params: Dict[str, Any] = {
            "jsonRpcUrl": fork_url if fork_url is not None else self.fork_url,
            "blockNumber": block_number if block_number is not None else self.block_number,
        }

        self._send_request(
            method="anvil_reset",
            params=[{"forking": forking_params}],
        )
        self._get_response()

        if block_number:
            self.block_number = block_number
        if fork_url:
            self.fork_url = fork_url

    def mine(self) -> None:
        self._send_request(method="evm_mine")
        self._get_response()

    def return_to_snapshot(self, id: int) -> bool:
        if id < 0:
            raise ValueError("ID cannot be negative")
        self._send_request(
            method="evm_revert",
            params=[id],
        )
        return bool(self._get_response())

    def set_balance(self, address: str, balance: int) -> None:
        if not (0 <= balance <= MAX_UINT256):
            raise ValueError("Invalid balance, must be within range: 0 <= balance <= 2**256 - 1")
        self._send_request(
            method="anvil_setBalance",
            params=[
                to_checksum_address(address),
                hex(balance),
            ],
        )
        self._get_response()

    def set_coinbase(self, address: str) -> None:
        self._send_request(
            method="anvil_setCoinbase",
            params=[HexBytes(address).hex()],
        )
        self._get_response()

    def set_code(self, address: str, bytecode: bytes) -> None:
        self._send_request(
            method="anvil_setCode",
            params=[
                HexBytes(address).hex(),
                HexBytes(bytecode).hex(),
            ],
        )
        self._get_response()

    def set_next_base_fee(self, fee: int) -> None:
        if not (0 <= fee <= MAX_UINT256):
            raise ValueError("Fee outside valid range 0 <= fee <= 2**256-1")
        self._send_request(
            method="anvil_setNextBlockBaseFeePerGas",
            params=[hex(fee)],
        )
        self._get_response()
        self.base_fee_next = fee

    def set_snapshot(self) -> int:
        self._send_request(
            method="evm_snapshot",
        )
        return int(self._get_response(), 16)
