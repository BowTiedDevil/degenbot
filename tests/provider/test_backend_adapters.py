"""
Unit tests for provider backend adapters.

These tests verify the extracted adapter classes in isolation, ensuring that
the refactoring from stringly-typed dispatch to polymorphic dispatch preserves
behavior. Each adapter is tested with a lightweight fake provider, with no live
RPC calls.
"""

from typing import Any
from unittest.mock import MagicMock

import pytest
from hexbytes import HexBytes

from degenbot.provider.interface import (
    _AlloyAdapter,
    _AsyncAlloyAdapter,
    _AsyncWeb3Adapter,
    _OfflineAdapter,
    _Web3Adapter,
)

# Fake data shared across tests
TEST_BLOCK = {"number": 18_000_000, "hash": HexBytes(b"\x01" * 32)}
TEST_LOG = {"address": "0x1234", "topics": ["0xabcd"]}
TEST_CODE = HexBytes(b"\x00" * 100)
TEST_CALL_RESULT = HexBytes(b"\x01" * 32)
TEST_STORAGE = HexBytes(b"\x02" * 32)
WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


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
    def chain_id(self):  # type: ignore[override]
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

    def close(self) -> None:
        pass


class TestWeb3Adapter:
    """Test _Web3Adapter delegates correctly to web3.eth namespace."""

    def test_chain_id(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        assert adapter.chain_id == 1

    def test_block_number(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        assert adapter.block_number == 18_000_000

    def test_get_block_number(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        assert adapter.get_block_number() == 18_000_000

    def test_get_block_int(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        block = adapter.get_block(18_000_000)
        assert block == TEST_BLOCK

    def test_get_block_string_identifier(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        block = adapter.get_block("earliest")
        assert block == {"number": 0}

    def test_get_logs(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        logs = adapter.get_logs(18_000_000, 18_000_010, None, None)
        assert logs == [TEST_LOG]

    def test_get_logs_with_addresses(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        logs = adapter.get_logs(18_000_000, 18_000_010, [WETH_ADDRESS], None)
        assert logs == [TEST_LOG]

    def test_call_without_block(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        result = adapter.call(to=WETH_ADDRESS, data=b"\x01", block=None)
        assert result == TEST_CALL_RESULT

    def test_call_with_block(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        result = adapter.call(to=WETH_ADDRESS, data=b"\x01", block=18_000_000)
        assert result == TEST_CALL_RESULT

    def test_get_code_without_block(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        code = adapter.get_code(WETH_ADDRESS, None)
        assert code == TEST_CODE

    def test_get_balance_with_block(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        balance = adapter.get_balance(WETH_ADDRESS, 18_000_000)
        assert balance == 10**18

    def test_get_storage_at(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        storage = adapter.get_storage_at(WETH_ADDRESS, 0, 18_000_000)
        assert storage == TEST_STORAGE

    def test_get_transaction_count(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        count = adapter.get_transaction_count(WETH_ADDRESS, 18_000_000)
        assert count == 42

    def test_is_connected(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        assert adapter.is_connected() is True

    def test_close(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        # Should not raise
        adapter.close()


class TestAlloyAdapter:
    """Test _AlloyAdapter delegates correctly to AlloyProvider."""

    def _make_alloy(self) -> MagicMock:
        alloy = MagicMock()
        alloy.chain_id = 1
        alloy.block_number = 18_000_000
        alloy.get_block_number.return_value = 18_000_000
        alloy.get_block.return_value = TEST_BLOCK
        alloy.get_logs.return_value = [TEST_LOG]
        alloy.call.return_value = TEST_CALL_RESULT
        alloy.get_code.return_value = TEST_CODE
        alloy.get_balance.return_value = 10**18
        alloy.get_storage_at.return_value = TEST_STORAGE
        alloy.get_transaction_count.return_value = 42
        return alloy

    def test_chain_id(self) -> None:
        adapter = _AlloyAdapter(self._make_alloy())
        assert adapter.chain_id == 1

    def test_block_number(self) -> None:
        adapter = _AlloyAdapter(self._make_alloy())
        assert adapter.block_number == 18_000_000

    def test_get_block_with_string_identifier_latest(self) -> None:
        alloy = self._make_alloy()
        adapter = _AlloyAdapter(alloy)
        block = adapter.get_block("latest")
        assert block == TEST_BLOCK
        alloy.get_block_number.assert_called()  # needed to resolve "latest"

    def test_get_block_with_string_identifier_earliest(self) -> None:
        alloy = self._make_alloy()
        adapter = _AlloyAdapter(alloy)
        adapter.get_block("earliest")
        alloy.get_block.assert_called_with(0)

    def test_call_uses_block_number_keyword(self) -> None:
        alloy = self._make_alloy()
        adapter = _AlloyAdapter(alloy)
        adapter.call(to=WETH_ADDRESS, data=b"\x01", block=18_000_000)
        alloy.call.assert_called_once_with(WETH_ADDRESS, b"\x01", block_number=18_000_000)

    def test_is_connected_always_true(self) -> None:
        adapter = _AlloyAdapter(self._make_alloy())
        assert adapter.is_connected() is True

    def test_close_calls_provider_close(self) -> None:
        alloy = self._make_alloy()
        adapter = _AlloyAdapter(alloy)
        adapter.close()
        alloy.close.assert_called_once()


class TestOfflineAdapter:
    """Test _OfflineAdapter delegates correctly to OfflineProvider."""

    def _make_offline(self) -> MagicMock:
        offline = MagicMock()
        offline.chain_id = 1
        offline.block_number = 18_000_000
        offline.get_block.return_value = TEST_BLOCK
        offline.get_logs.return_value = [TEST_LOG]
        offline.call.return_value = TEST_CALL_RESULT
        offline.get_code.return_value = TEST_CODE
        offline.get_balance.return_value = 10**18
        offline.get_storage_at.return_value = TEST_STORAGE
        offline.get_transaction_count.return_value = 42
        return offline

    def test_chain_id(self) -> None:
        adapter = _OfflineAdapter(self._make_offline())
        assert adapter.chain_id == 1

    def test_get_block_passes_through_string_identifier(self) -> None:
        offline = self._make_offline()
        adapter = _OfflineAdapter(offline)
        adapter.get_block("latest")
        offline.get_block.assert_called_once_with("latest")

    def test_call_uses_block_number_keyword(self) -> None:
        offline = self._make_offline()
        adapter = _OfflineAdapter(offline)
        adapter.call(WETH_ADDRESS, b"\x01", 18_000_000)
        offline.call.assert_called_once_with(WETH_ADDRESS, b"\x01", block_number=18_000_000)

    def test_is_connected_always_true(self) -> None:
        adapter = _OfflineAdapter(self._make_offline())
        assert adapter.is_connected() is True


class TestAsyncWeb3Adapter:
    """Test _AsyncWeb3Adapter delegates correctly to async web3.eth namespace."""

    @pytest.mark.asyncio
    async def test_get_block_number(self) -> None:
        adapter = _AsyncWeb3Adapter(FakeAsyncW3())
        block_number = await adapter.get_block_number()
        assert block_number == 18_000_000

    @pytest.mark.asyncio
    async def test_get_chain_id(self) -> None:
        adapter = _AsyncWeb3Adapter(FakeAsyncW3())
        chain_id = await adapter.get_chain_id()
        assert chain_id == 1

    @pytest.mark.asyncio
    async def test_get_block(self) -> None:
        adapter = _AsyncWeb3Adapter(FakeAsyncW3())
        block = await adapter.get_block(18_000_000)
        assert block == TEST_BLOCK

    @pytest.mark.asyncio
    async def test_get_logs(self) -> None:
        adapter = _AsyncWeb3Adapter(FakeAsyncW3())
        logs = await adapter.get_logs(18_000_000, 18_000_010, None, None)
        assert logs == [TEST_LOG]

    @pytest.mark.asyncio
    async def test_call(self) -> None:
        adapter = _AsyncWeb3Adapter(FakeAsyncW3())
        result = await adapter.call(to=WETH_ADDRESS, data=b"\x01", block=18_000_000)
        assert result == TEST_CALL_RESULT

    def test_is_connected(self) -> None:
        adapter = _AsyncWeb3Adapter(FakeAsyncW3())
        assert adapter.is_connected() is True


class TestAsyncAlloyAdapter:
    """Test _AsyncAlloyAdapter delegates correctly to AsyncAlloyProvider."""

    def _make_async_alloy(self) -> MagicMock:
        async def _return(val: Any) -> Any:  # noqa: RUF029
            return val

        alloy = MagicMock()
        alloy.get_block_number = _return_fn(18_000_000)
        alloy.get_chain_id = _return_fn(1)
        alloy.get_block = _return_fn(TEST_BLOCK)
        alloy.get_logs = _return_fn([TEST_LOG])
        alloy.call = _return_fn(TEST_CALL_RESULT)
        alloy.get_code = _return_fn(TEST_CODE)
        alloy.get_storage_at = _return_fn(TEST_STORAGE)
        alloy.close = MagicMock()
        return alloy

    @pytest.mark.asyncio
    async def test_get_block_number(self) -> None:
        adapter = _AsyncAlloyAdapter(self._make_async_alloy())
        block_number = await adapter.get_block_number()
        assert block_number == 18_000_000

    @pytest.mark.asyncio
    async def test_get_balance_raises_not_implemented(self) -> None:
        adapter = _AsyncAlloyAdapter(self._make_async_alloy())
        with pytest.raises(NotImplementedError, match="get_balance not implemented"):
            await adapter.get_balance(WETH_ADDRESS, 18_000_000)

    @pytest.mark.asyncio
    async def test_get_transaction_count_raises_not_implemented(self) -> None:
        adapter = _AsyncAlloyAdapter(self._make_async_alloy())
        with pytest.raises(NotImplementedError, match="get_transaction_count not implemented"):
            await adapter.get_transaction_count(WETH_ADDRESS, 18_000_000)

    def test_is_connected_always_true(self) -> None:
        adapter = _AsyncAlloyAdapter(self._make_async_alloy())
        assert adapter.is_connected() is True


def _return_fn(val: Any):
    """Helper to create an async mock returning a fixed value."""
    async def _fn(*args: Any, **kwargs: Any) -> Any:  # noqa: RUF029
        return val

    return _fn
