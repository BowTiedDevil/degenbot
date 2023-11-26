import os
import socket
import subprocess
from typing import Any, Dict, Optional

import ujson
from web3 import IPCProvider, Web3


class AnvilFork:
    """
    Launch an Anvil fork via subprocess and provide various methods for
    interacting with it via JSON-RPC over the built-in IPC socket.
    """

    _SOCKET_BUFFER_SIZE = 4096  # https://docs.python.org/3/library/socket.html#socket.socket.recv

    def __init__(
        self,
        fork_url: str,
        hardfork: str = "shanghai",
        gas_limit: int = 30_000_000,
        port: Optional[int] = None,
        fork_block: Optional[int] = None,
        chain_id: Optional[int] = None,
        base_fee: Optional[int] = None,
        ipc_path: Optional[str] = None,
        mnemonic: str = "patient rude simple dog close planet oval animal hunt sketch suspect slim",
    ):
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

        # Web3.py v5 method is 'isConnected', v6 is 'is_connected'
        for method_name in ("isConnected", "is_connected"):
            if is_connected_method := getattr(self.w3, method_name, None):
                break
        if is_connected_method is None:  # pragma: no cover
            raise ValueError("Web3 provider cannot be tested")
        while is_connected_method() is False:
            continue

        self.block = fork_block if fork_block is not None else self.w3.eth.get_block_number()
        self.base_fee = (
            base_fee if base_fee is not None else self.w3.eth.get_block(self.block)["baseFeePerGas"]
        )
        self.base_fee_next: Optional[int] = None
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.connect(self.ipc_path)

    def __del__(self):
        self._process.terminate()
        self._process.wait()
        if os.path.exists(self.ipc_path):
            os.remove(self.ipc_path)

    def create_access_list(self, transaction: Dict) -> list:
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

        self.socket.sendall(
            bytes(
                ujson.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "eth_createAccessList",
                        "params": [sanitized_tx],
                    }
                ),
                encoding="utf-8",
            ),
        )
        result: dict = ujson.loads(self.socket.recv(self._SOCKET_BUFFER_SIZE))
        if result.get("error"):  # pragma: no cover
            raise Exception(f"Error creating access list! Response: {result}")
        return result["result"]["accessList"]

    def reset(
        self,
        fork_url: Optional[str] = None,
        block_number: Optional[int] = None,
    ) -> None:
        forking_params: Dict[str, Any] = dict()
        forking_params["jsonRpcUrl"] = fork_url if fork_url is not None else self.fork_url
        if block_number is not None:
            forking_params["blockNumber"] = block_number

        self.socket.sendall(
            bytes(
                ujson.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "anvil_reset",
                        "params": [
                            {
                                "forking": forking_params,
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            ),
        )
        result: dict = ujson.loads(self.socket.recv(self._SOCKET_BUFFER_SIZE))
        if result.get("error"):  # pragma: no cover
            raise Exception(f"Error: {result}")
        else:
            if block_number:
                self.block = block_number
            if fork_url:
                self.fork_url = fork_url

    def return_to_snapshot(self, id: str) -> bool:
        self.socket.sendall(
            bytes(
                ujson.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "evm_revert",
                        "params": [id],
                    }
                ),
                encoding="utf-8",
            ),
        )
        response: dict = ujson.loads(self.socket.recv(self._SOCKET_BUFFER_SIZE))
        if response.get("error"):
            raise Exception(f"Error reverting to previous snapshot! Response: {response}")

        return response["result"]

    def set_next_base_fee(self, fee: int) -> None:
        self.socket.sendall(
            bytes(
                ujson.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "anvil_setNextBlockBaseFeePerGas",
                        "params": [hex(fee)],
                    }
                ),
                encoding="utf-8",
            ),
        )
        result: dict = ujson.loads(self.socket.recv(self._SOCKET_BUFFER_SIZE))
        if result.get("error"):
            raise Exception(f"Error setting next block base fee! Response: {result}")
        else:
            self.base_fee_next = fee

    def set_snapshot(self) -> int:
        self.socket.sendall(
            bytes(
                ujson.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "evm_snapshot",
                        "params": [],
                    }
                ),
                encoding="utf-8",
            ),
        )
        result: dict = ujson.loads(self.socket.recv(self._SOCKET_BUFFER_SIZE))

        if result.get("error"):  # pragma: no cover
            raise Exception(f"Error setting snapshot! Response: {result}")

        return int(result["result"], 16)
