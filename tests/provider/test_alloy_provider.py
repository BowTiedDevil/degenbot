"""Behavior tests for AlloyProvider against the seeded standalone anvil.

The static API surface (properties, callable methods, call/get_code/storage
signatures) is pinned by ``tests/provider/test_alloy_provider_api.py``; these
tests cover what that pin cannot: return-type mapping, default-parameter
behavior, connection semantics, and revert → ContractLogicError classification.
"""

import inspect

import pytest

from degenbot.crypto import function_selector
from degenbot.fork import AnvilFork
from degenbot.provider import AlloyProvider
from degenbot.utils.bytes import to_bytes
from tests.standalone_anvil import seed as seed_catalog


@pytest.fixture
def alloy_provider(standalone_anvil: AnvilFork) -> AlloyProvider:
    """Create an AlloyProvider from the seeded standalone anvil (no upstream RPC)."""
    return AlloyProvider(standalone_anvil.http_url)


class TestAlloyProviderTransactionSignatures:
    """Transaction-fetch signatures (not pinned by the API-surface test)."""

    def test_get_transaction_signature(self, alloy_provider: AlloyProvider):
        """Test get_transaction accepts tx_hash parameter."""
        sig = inspect.signature(alloy_provider.get_transaction)
        params = list(sig.parameters.keys())
        assert "tx_hash" in params

    def test_get_transaction_receipt_signature(self, alloy_provider: AlloyProvider):
        """Test get_transaction_receipt accepts tx_hash parameter."""
        sig = inspect.signature(alloy_provider.get_transaction_receipt)
        params = list(sig.parameters.keys())
        assert "tx_hash" in params


class TestAlloyProviderReturnTypes:
    """Test that Rust return types map correctly to Python types."""

    def test_get_gas_price_returns_int(self, alloy_provider: AlloyProvider):
        """get_gas_price should return int (not str)."""
        result = alloy_provider.get_gas_price()
        assert isinstance(result, int), f"Expected int, got {type(result)}"
        assert result >= 0

    def test_get_block_number_returns_int(self, alloy_provider: AlloyProvider):
        """get_block_number should return int."""
        result = alloy_provider.get_block_number()
        assert isinstance(result, int)
        assert result > 0

    def test_get_chain_id_returns_int(self, alloy_provider: AlloyProvider):
        """get_chain_id should return int."""
        result = alloy_provider.get_chain_id()
        assert isinstance(result, int)

    def test_get_storage_at_returns_bytes(self, alloy_provider: AlloyProvider):
        """get_storage_at should return bytes (functional, not stub)."""
        result = alloy_provider.get_storage_at("0x742d35Cc6634C0532925a3b8D4C9db96590d6B75", 0)
        assert isinstance(result, bytes)
        assert len(result) == 32

    def test_get_transaction_returns_dict_or_none(self, alloy_provider: AlloyProvider):
        """get_transaction should return dict or None for missing tx."""
        result = alloy_provider.get_transaction("0x" + "00" * 32)
        assert result is None or isinstance(result, dict)

    def test_estimate_gas_returns_int(self, alloy_provider: AlloyProvider):
        """estimate_gas should return int."""
        result = alloy_provider.estimate_gas(
            to="0x742d35Cc6634C0532925a3b8D4C9db96590d6B75",
            data=to_bytes(b""),
        )
        assert isinstance(result, int)
        assert result >= 0


class TestAlloyProviderBalanceAndNonceMethods:
    """Test balance and transaction count methods."""

    def test_get_balance_returns_int(self, alloy_provider: AlloyProvider):
        """Test get_balance returns int."""
        result = alloy_provider.get_balance("0x742d35Cc6634C0532925a3b8D4C9db96590d6B75")
        assert isinstance(result, int)
        assert result >= 0

    def test_get_balance_with_block_returns_int(self, alloy_provider: AlloyProvider):
        """Test get_balance with block returns int (block 0 = genesis on the standalone chain)."""
        result = alloy_provider.get_balance("0x742d35Cc6634C0532925a3b8D4C9db96590d6B75", 0)
        assert isinstance(result, int)
        assert result >= 0

    def test_get_transaction_count_returns_int(self, alloy_provider: AlloyProvider):
        """Test get_transaction_count returns int."""
        result = alloy_provider.get_transaction_count("0x742d35Cc6634C0532925a3b8D4C9db96590d6B75")
        assert isinstance(result, int)
        assert result >= 0

    def test_get_transaction_count_with_block_returns_int(self, alloy_provider: AlloyProvider):
        """Test get_transaction_count with block returns int (block 0 on the standalone chain)."""
        result = alloy_provider.get_transaction_count(
            "0x742d35Cc6634C0532925a3b8D4C9db96590d6B75",
            0,
        )
        assert isinstance(result, int)
        assert result >= 0


class TestAlloyProviderConnection:
    """Test connection-related methods."""

    def test_is_connected_returns_true(self, alloy_provider: AlloyProvider):
        """Test is_connected returns True for AlloyProvider."""
        assert alloy_provider.is_connected() is True


class TestAlloyProviderContextManager:
    """Test context manager functionality."""

    def test_context_manager_enter_exit(self, standalone_anvil: AnvilFork):
        """Test AlloyProvider works as context manager."""
        with AlloyProvider(standalone_anvil.http_url) as provider:
            assert provider is not None
            assert isinstance(provider, AlloyProvider)


class TestProviderDefaults:
    """Test default parameter values."""

    def test_get_code_default_block(self, alloy_provider: AlloyProvider):
        """Test get_code has None default for block (latest)."""
        sig = inspect.signature(alloy_provider.get_code)
        default = sig.parameters["block"].default
        assert default is None

    def test_call_default_block(self, alloy_provider: AlloyProvider):
        """Test call has None default for block (latest)."""
        sig = inspect.signature(alloy_provider.call)
        default = sig.parameters["block"].default
        assert default is None

    def test_get_block_default_block(self, alloy_provider: AlloyProvider):
        """Test get_block accepts a block identifier (number or tag)."""
        sig = inspect.signature(alloy_provider.get_block)
        # block_identifier is required, no default
        assert sig.parameters["block_identifier"].default is inspect.Parameter.empty

    def test_get_storage_at_default_block(self, alloy_provider: AlloyProvider):
        """Test get_storage_at has None default for block (latest)."""
        sig = inspect.signature(alloy_provider.get_storage_at)
        default = sig.parameters["block"].default
        assert default is None


class TestAlloyProviderRevertRaisesContractLogicError:
    """eth_call execution reverts raise degenbot.exceptions.ContractLogicError.

    Replaces the Python adapter-seam string-scraping (alloy_errors): the Rust
    core classifies reverts as ProviderError::ExecutionReverted, and the FFI
    layer raises the degenbot-owned ContractLogicError directly. Probe sites
    catch RpcError (the base class) to mean "method not implemented."
    """

    @staticmethod
    def _revert_calldata() -> bytes:
        """Calldata for the seeded ``Reverter.alwaysRevert()`` (Revert(string)).

        Always reverts with "boom" — a reliable, portable revert trigger that
        needs no upstream RPC.
        """
        return function_selector("alwaysRevert()")

    def test_sync_call_revert_raises_contract_logic_error(self, alloy_provider):
        """AlloyProvider.call raises ContractLogicError on an EVM revert."""
        from degenbot.exceptions import ContractLogicError

        with pytest.raises(ContractLogicError, match="reverted"):
            alloy_provider.call(seed_catalog.REVERTER, self._revert_calldata())

    def test_sync_call_revert_is_catchable_as_rpc_error(self, alloy_provider):
        """ContractLogicError is a subclass of RpcError (probe-site contract)."""
        from degenbot.exceptions import RpcError

        with pytest.raises(RpcError):
            alloy_provider.call(seed_catalog.REVERTER, self._revert_calldata())

    @pytest.mark.asyncio
    async def test_async_call_revert_raises_contract_logic_error(self, alloy_provider):
        """AsyncAlloyProvider.call raises ContractLogicError on an EVM revert."""
        from degenbot._ffi.provider import AsyncAlloyProvider
        from degenbot.exceptions import ContractLogicError

        async_alloy = await AsyncAlloyProvider.create(alloy_provider.rpc_url)
        with pytest.raises(ContractLogicError, match="reverted"):
            await async_alloy.call(seed_catalog.REVERTER, self._revert_calldata())
