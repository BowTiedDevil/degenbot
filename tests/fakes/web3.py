"""Fake Web3 instances for testing provider adapters.

Replaces the ad hoc fakes from tests/provider/test_backend_adapters.py.

Provides sync and async Web3 doubles that return deterministic values
without requiring a live RPC connection.
"""

from typing import Any

from hexbytes import HexBytes

# Shared fake data
TEST_BLOCK = {"number": 18_000_000, "hash": HexBytes(b"\x01" * 32), "timestamp": 1690000000}
TEST_LOG = {"address": "0x1234", "topics": ["0xabcd"]}
TEST_CODE = HexBytes(b"\x00" * 100)
TEST_CALL_RESULT = HexBytes(b"\x01" * 32)
TEST_STORAGE = HexBytes(b"\x02" * 32)


class FakeW3Eth:
    """Fake web3.eth namespace."""

    chain_id = 1
    block_number = 18_000_000

    def get_block_number(self) -> int:
        return self.block_number

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        if block_identifier == "earliest":
            return {"number": 0}
        return TEST_BLOCK

    def get_logs(self, filter_param: dict[str, Any]) -> list[dict[str, Any]]:
        return [TEST_LOG]

    def call(self, tx: dict[str, Any], block: int | None = None) -> HexBytes:
        return TEST_CALL_RESULT

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return TEST_CODE

    def get_balance(self, address: str, block: int | None = None) -> int:
        return 10**18

    def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> HexBytes:
        return TEST_STORAGE

    def get_transaction_count(self, address: str, block: int | None = None) -> int:
        return 42


class FakeW3:
    """Fake web3.py Web3 instance."""

    eth = FakeW3Eth()
    provider = "https://fake-rpc.example.com"

    def is_connected(self) -> bool:
        return True

    def close(self) -> None:
        pass


class FakeAsyncW3Eth(FakeW3Eth):
    """Fake async web3.eth namespace.

    Several attributes are async properties on the real AsyncWeb3 (e.g.
    ``eth.chain_id``).  Our fakes return a coroutine so ``await`` works.
    """

    @property
    def chain_id(self):
        async def _inner() -> int:  # noqa: RUF029
            return 1

        return _inner()

    async def get_block_number(self) -> int:
        return self.block_number

    async def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        if block_identifier == "earliest":
            return {"number": 0}
        return TEST_BLOCK

    async def get_logs(self, filter_param: dict[str, Any]) -> list[dict[str, Any]]:
        return [TEST_LOG]

    async def call(self, tx: dict[str, Any], block: int | None = None) -> HexBytes:
        return TEST_CALL_RESULT

    async def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return TEST_CODE

    async def get_balance(self, address: str, block: int | None = None) -> int:
        return 10**18

    async def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> HexBytes:
        return TEST_STORAGE

    async def get_transaction_count(self, address: str, block: int | None = None) -> int:
        return 42


class FakeAsyncW3:
    """Fake async web3.py Web3 instance."""

    eth = FakeAsyncW3Eth()
    provider = "https://fake-async-rpc.example.com"

    def close(self) -> None:
        pass
