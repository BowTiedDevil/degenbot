"""API surface tests for :class:`degenbot.provider.AlloyProvider`.

AlloyProvider is the codebase's provider backend. These tests pin the public
surface callers depend on (properties, callable methods, and the provider
protocol members) so a rename or removal on the PyO3 side fails the suite
instead of surfacing as scattered runtime breaks.
"""

import inspect

import pytest

from degenbot.provider import AlloyProvider

# Provider protocol members callers depend on.
PROVIDER_PROTOCOL_METHODS: list[str] = [
    "chain_id",
    "block_number",
    "get_block_number",
    "get_block",
    "get_logs",
    "call",
    "get_code",
    "get_balance",
    "get_storage_at",
    "get_transaction_count",
    "is_connected",
]

# Beyond the protocol: methods the codebase and test tooling rely on.
EXTRA_METHODS: list[str] = [
    "close",
    "estimate_gas",
    "get_gas_price",
    "get_transaction",
    "get_transaction_receipt",
    "make_request",
]


class TestAlloyProviderApi:
    """AlloyProvider exposes the surface callers rely on."""

    def test_chain_id_is_property(self) -> None:
        assert isinstance(inspect.getattr_static(AlloyProvider, "chain_id"), property)

    def test_block_number_is_property(self) -> None:
        assert isinstance(inspect.getattr_static(AlloyProvider, "block_number"), property)

    @pytest.mark.parametrize(
        "method_name",
        [m for m in PROVIDER_PROTOCOL_METHODS if m not in {"chain_id", "block_number"}],
    )
    def test_has_protocol_member(self, method_name: str) -> None:
        assert callable(getattr(AlloyProvider, method_name)), (
            f"AlloyProvider missing protocol member: {method_name}"
        )

    @pytest.mark.parametrize("method_name", EXTRA_METHODS)
    def test_has_extra_method(self, method_name: str) -> None:
        assert callable(getattr(AlloyProvider, method_name)), (
            f"AlloyProvider missing method: {method_name}"
        )

    def test_call_signature(self) -> None:
        """``call`` accepts (to, data, block)."""
        params = list(inspect.signature(AlloyProvider.call).parameters)
        assert "to" in params
        assert "data" in params
        assert "block" in params

    def test_get_code_signature(self) -> None:
        """``get_code`` accepts (address, block)."""
        params = list(inspect.signature(AlloyProvider.get_code).parameters)
        assert "address" in params
        assert "block" in params

    def test_get_storage_at_signature(self) -> None:
        """``get_storage_at`` accepts (address, position, block)."""
        params = list(inspect.signature(AlloyProvider.get_storage_at).parameters)
        assert "address" in params
        assert "position" in params
        assert "block" in params

    def test_get_block_signature(self) -> None:
        """``get_block`` accepts a block identifier (number or tag)."""
        params = list(inspect.signature(AlloyProvider.get_block).parameters)
        assert "block_identifier" in params

    def test_get_logs_signature(self) -> None:
        """``get_logs`` accepts ``LogFilter`` or keyword args."""
        params = inspect.signature(AlloyProvider.get_logs).parameters
        assert "filter_param" in params
        assert "from_block" in params
        assert params["from_block"].kind == inspect.Parameter.KEYWORD_ONLY
        assert "to_block" in params
        assert params["to_block"].kind == inspect.Parameter.KEYWORD_ONLY
