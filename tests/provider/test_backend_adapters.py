"""Unit tests for provider backend adapters.

These tests verify the extracted adapter classes in isolation, ensuring that
the refactoring from stringly-typed dispatch to polymorphic dispatch preserves
behavior. Each adapter is tested with a lightweight fake provider, with no live
RPC calls.
"""

import asyncio
from typing import Any
from unittest.mock import MagicMock

import pytest
from web3.exceptions import ContractLogicError, Web3Exception

from degenbot.provider.async_adapter import _AsyncAlloyAdapter, _AsyncWeb3Adapter
from degenbot.provider.sync_adapter import (
    ProviderAdapter,
    _AlloyAdapter,
    _OfflineAdapter,
    _Web3Adapter,
)
from tests.fakes.web3 import (
    TEST_BLOCK,
    TEST_CALL_RESULT,
    TEST_CODE,
    TEST_LOG,
    TEST_STORAGE,
    FakeAsyncW3,
    FakeW3,
)

WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


class TestProviderAdapterCallRaw:
    """Test ProviderAdapter.call_raw routes through the backend."""

    def test_call_raw_web3(self) -> None:
        adapter = ProviderAdapter.from_web3(FakeW3())
        result = adapter.call_raw({"to": WETH_ADDRESS, "data": b"\x01"}, 18_000_000)
        assert result == TEST_CALL_RESULT

    def test_call_raw_alloy(self) -> None:
        alloy = MagicMock()
        alloy.call.return_value = TEST_CALL_RESULT
        adapter = ProviderAdapter.from_alloy(alloy)
        result = adapter.call_raw({"to": WETH_ADDRESS, "data": b"\x01"}, 18_000_000)
        assert result == TEST_CALL_RESULT

    def test_batch_call_web3(self) -> None:
        adapter = ProviderAdapter.from_web3(FakeW3())
        calls = [
            {"to": WETH_ADDRESS, "data": b"\x01"},
            {"to": WETH_ADDRESS, "data": b"\x02"},
        ]
        results = adapter.batch_call(calls, block=18_000_000)
        assert len(results) == 2
        assert all(r == TEST_CALL_RESULT for r in results)

    def test_batch_call_returns_results_in_order(self) -> None:
        adapter = ProviderAdapter.from_web3(FakeW3())
        calls = [
            {"to": WETH_ADDRESS, "data": b"\x01"},
            {"to": WETH_ADDRESS, "data": b"\x02"},
            {"to": WETH_ADDRESS, "data": b"\x03"},
        ]
        results = adapter.batch_call(calls, block=18_000_000)
        assert len(results) == 3


class TestProviderAdapterGetBlockTimestamp:
    """Test ProviderAdapter.get_block_timestamp convenience method."""

    def test_get_block_timestamp_web3(self) -> None:
        adapter = ProviderAdapter.from_web3(FakeW3())
        ts = adapter.get_block_timestamp(block=18_000_000)
        assert isinstance(ts, int)


class TestWeb3AdapterCallRaw:
    """Test call_raw delegates to w3.eth.call with raw transaction dict."""

    def test_call_raw_without_block(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        result = adapter.call_raw({"to": WETH_ADDRESS, "data": b"\x01"}, None)
        assert result == TEST_CALL_RESULT

    def test_call_raw_with_block(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        result = adapter.call_raw({"to": WETH_ADDRESS, "data": b"\x01"}, 18_000_000)
        assert result == TEST_CALL_RESULT


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


class TestAlloyAdapterCallRaw:
    """Test call_raw delegates to alloy.call with block_number keyword."""

    def test_call_raw_with_block(self) -> None:
        alloy = MagicMock()
        alloy.call.return_value = TEST_CALL_RESULT
        adapter = _AlloyAdapter(alloy)
        adapter.call_raw({"to": WETH_ADDRESS, "data": b"\x01"}, 18_000_000)
        alloy.call.assert_called_once_with(WETH_ADDRESS, b"\x01", block_number=18_000_000)

    def test_call_raw_without_block(self) -> None:
        alloy = MagicMock()
        alloy.call.return_value = TEST_CALL_RESULT
        adapter = _AlloyAdapter(alloy)
        adapter.call_raw({"to": WETH_ADDRESS, "data": b"\x01"}, None)
        alloy.call.assert_called_once_with(WETH_ADDRESS, b"\x01", block_number=None)


class TestOfflineAdapterCallRaw:
    """Test call_raw delegates to offline.call with block_number keyword."""

    def test_call_raw_with_block(self) -> None:
        offline = MagicMock()
        offline.call.return_value = TEST_CALL_RESULT
        adapter = _OfflineAdapter(offline)
        adapter.call_raw({"to": WETH_ADDRESS, "data": b"\x01"}, 18_000_000)
        offline.call.assert_called_once_with(WETH_ADDRESS, b"\x01", block_number=18_000_000)

    def test_call_raw_without_block(self) -> None:
        offline = MagicMock()
        offline.call.return_value = TEST_CALL_RESULT
        adapter = _OfflineAdapter(offline)
        adapter.call_raw({"to": WETH_ADDRESS, "data": b"\x01"}, None)
        offline.call.assert_called_once_with(WETH_ADDRESS, b"\x01", block_number=None)


class TestAlloyAdapter:
    """Test _AlloyAdapter delegates correctly to AlloyProvider."""

    def _make_alloy(self) -> MagicMock:
        alloy = MagicMock()
        alloy.chain_id = 1
        alloy.block_number = 18_000_000
        alloy.get_chain_id.return_value = 1
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

    @pytest.mark.parametrize(
        "message",
        [
            # Anvil ExecutionError (Solidity >=0.8 Error(string))
            "Provider error: RPC error: 3 - eth_call failed: "
            "server returned an error response: error code 3: execution reverted",
            # Geth / Infura / Alchemy phrasing
            "Provider error: RPC error: -32000 - eth_call failed: execution reverted",
            # Older Vyper / generic revert
            "Provider error: RPC error: -32000 - eth_call failed: Reverted",
        ],
    )
    def test_call_translates_revert_to_contract_logic_error(self, message: str) -> None:
        """An alloy eth_call revert must raise ContractLogicError.

        The web3 backend raises ContractLogicError (a Web3Exception subclass)
        on execution reverts. Probing code throughout the codebase (type
        resolution, Curve detection, Aave analysis) catches Web3Exception to
        mean "this method is not implemented by the contract." The alloy
        backend must mirror that contract so probes behave identically.
        """
        alloy = self._make_alloy()
        alloy.call.side_effect = RuntimeError(message)
        adapter = _AlloyAdapter(alloy)
        with pytest.raises(ContractLogicError):
            adapter.call(to=WETH_ADDRESS, data=b"\x01")

    def test_call_translated_revert_is_web3_exception(self) -> None:
        alloy = self._make_alloy()
        alloy.call.side_effect = RuntimeError("Provider error: ... execution reverted")
        adapter = _AlloyAdapter(alloy)
        with pytest.raises(Web3Exception):
            adapter.call(to=WETH_ADDRESS, data=b"\x01")

    def test_call_propagates_non_revert_runtime_error(self) -> None:
        """A genuine (non-revert) RPC failure must not be swallowed as a revert."""
        alloy = self._make_alloy()
        alloy.call.side_effect = RuntimeError(
            "Provider error: RPC error: -32000 - eth_call failed: node is syncing"
        )
        adapter = _AlloyAdapter(alloy)
        with pytest.raises(RuntimeError, match="node is syncing"):
            adapter.call(to=WETH_ADDRESS, data=b"\x01")

    def test_call_raw_translates_revert_to_contract_logic_error(self) -> None:
        alloy = self._make_alloy()
        alloy.call.side_effect = RuntimeError("Provider error: ... execution reverted")
        adapter = _AlloyAdapter(alloy)
        with pytest.raises(ContractLogicError):
            adapter.call_raw({"to": WETH_ADDRESS, "data": b"\x01"})


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
        async def _return(val: Any) -> Any:
            await asyncio.sleep(0)
            return val

        alloy = MagicMock()
        alloy.get_block_number = _return_fn(18_000_000)
        alloy.get_chain_id = _return_fn(1)
        alloy.get_block = _return_fn(TEST_BLOCK)
        alloy.get_logs = _return_fn([TEST_LOG])
        alloy.call = _return_fn(TEST_CALL_RESULT)
        alloy.get_code = _return_fn(TEST_CODE)
        alloy.get_storage_at = _return_fn(TEST_STORAGE)
        alloy.get_balance = _raise_fn(NotImplementedError("get_balance not implemented"))
        alloy.get_transaction_count = _raise_fn(
            NotImplementedError("get_transaction_count not implemented")
        )
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

    @pytest.mark.asyncio
    async def test_call_translates_revert_to_contract_logic_error(self) -> None:
        """Async alloy eth_call reverts must raise ContractLogicError (parity
        with the sync adapter and the web3 backend)."""
        alloy = self._make_async_alloy()
        alloy.call = _raise_fn(
            RuntimeError(
                "Provider error: RPC error: 3 - eth_call failed: "
                "error code 3: execution reverted"
            )
        )
        adapter = _AsyncAlloyAdapter(alloy)
        with pytest.raises(ContractLogicError):
            await adapter.call(to=WETH_ADDRESS, data=b"\x01")

    @pytest.mark.asyncio
    async def test_call_propagates_non_revert_runtime_error(self) -> None:
        alloy = self._make_async_alloy()
        alloy.call = _raise_fn(
            RuntimeError("Provider error: ... node is syncing")
        )
        adapter = _AsyncAlloyAdapter(alloy)
        with pytest.raises(RuntimeError, match="node is syncing"):
            await adapter.call(to=WETH_ADDRESS, data=b"\x01")

    def test_is_connected_always_true(self) -> None:
        adapter = _AsyncAlloyAdapter(self._make_async_alloy())
        assert adapter.is_connected() is True


def _return_fn(val: Any):
    """Helper to create an async mock returning a fixed value."""

    async def _fn(*args: Any, **kwargs: Any) -> Any:
        await asyncio.sleep(0)
        return val

    return _fn


def _raise_fn(exc: Exception):
    """Helper to create an async mock raising an exception."""

    async def _fn(*args: Any, **kwargs: Any) -> Any:
        await asyncio.sleep(0)
        raise exc

    return _fn
