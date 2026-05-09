"""
Feature parity tests between AlloyProvider and Web3.

This test suite audits the codebase for direct web3/w3 usage and verifies
that AlloyProvider has equivalent methods available.

The test asserts that for each web3 method used in the codebase,
AlloyProvider provides an equivalent method/function.
"""

import inspect
import pathlib
from typing import Any

import pytest

from degenbot.provider import AlloyProvider

# =============================================================================
# Web3 methods discovered in the codebase
# =============================================================================

# These are the web3.eth methods found via: rg -n "w3\.eth\." src/
WEB3_ETH_METHODS: dict[str, dict[str, Any]] = {
    # Core blockchain queries
    "chain_id": {
        "web3_usage": "w3.eth.chain_id (property)",
        "locations": ["connection_manager.py", "cli/utils.py"],
        "alloy_equivalent": "chain_id (property)",
        "implemented": True,
    },
    "block_number": {
        "web3_usage": "w3.eth.block_number (property)",
        "locations": ["various"],
        "alloy_equivalent": "block_number (property)",
        "implemented": True,
    },
    "get_block_number": {
        "web3_usage": "w3.eth.get_block_number()",
        "locations": ["functions.py"],
        "alloy_equivalent": "get_block_number()",
        "implemented": True,
    },
    "get_block": {
        "web3_usage": "w3.eth.get_block(block_identifier)",
        "locations": ["bot.py"],
        "alloy_equivalent": "get_block(block_number: int)",
        "implemented": True,
        "note": "AlloyProvider only supports integer block numbers, not 'latest'/'earliest'/'pending' strings",
    },
    "get_logs": {
        "web3_usage": "w3.eth.get_logs(filter_params)",
        "locations": ["functions.py"],
        "alloy_equivalent": "get_logs(from_block, to_block, addresses, topics)",
        "implemented": True,
    },
    # Contract interaction
    "call": {
        "web3_usage": "w3.eth.call(tx, block)",
        "locations": ["bot.py", "aerodrome/pools.py"],
        "alloy_equivalent": "call(to: str, data: bytes, block_number: int | None)",
        "implemented": True,
        "note": "AlloyProvider uses (to, data, block_number) signature instead of tx dict",
    },
    "get_code": {
        "web3_usage": "w3.eth.get_code(address, block)",
        "locations": ["various"],
        "alloy_equivalent": "get_code(address: str, block_number: int | None)",
        "implemented": True,
    },
    "get_balance": {
        "web3_usage": "w3.eth.get_balance(address, block)",
        "locations": ["provider/interface.py (Web3Adapter)"],
        "alloy_equivalent": "get_balance(address, block_number)",
        "implemented": True,
    },
    "get_storage_at": {
        "web3_usage": "w3.eth.get_storage_at(address, position, block)",
        "locations": ["provider/interface.py (Web3Adapter)"],
        "alloy_equivalent": "get_storage_at(address, position, block_number)",
        "implemented": True,
    },
    "get_transaction_count": {
        "web3_usage": "w3.eth.get_transaction_count(address, block)",
        "locations": ["provider/interface.py (Web3Adapter)"],
        "alloy_equivalent": "get_transaction_count(address, block_number)",
        "implemented": True,
    },
    # Transaction queries
    "get_transaction": {
        "web3_usage": "Not directly used in src/",
        "locations": [],
        "alloy_equivalent": "get_transaction(tx_hash: str)",
        "implemented": True,
    },
    "get_transaction_receipt": {
        "web3_usage": "Not directly used in src/",
        "locations": [],
        "alloy_equivalent": "get_transaction_receipt(tx_hash: str)",
        "implemented": True,
    },
    "estimate_gas": {
        "web3_usage": "Not directly used in src/",
        "locations": [],
        "alloy_equivalent": "estimate_gas(to, data, from_, value, block_number)",
        "implemented": True,
    },
    "get_gas_price": {
        "web3_usage": "Not directly used in src/",
        "locations": [],
        "alloy_equivalent": "get_gas_price()",
        "implemented": True,
    },
    # Contract factory
    "contract": {
        "web3_usage": "w3.eth.contract(address=..., abi=...)",
        "locations": ["chainlink.py"],
        "alloy_equivalent": "None",
        "implemented": False,
        "note": "AlloyProvider does not provide contract factory. Use degenbot.contract module instead.",
    },
}

# These are the web3 instance methods found via: rg -n "w3\." src/
WEB3_INSTANCE_METHODS: dict[str, dict[str, Any]] = {
    "is_connected": {
        "web3_usage": "w3.is_connected()",
        "locations": ["anvil_fork.py", "provider/interface.py"],
        "alloy_equivalent": "is_connected()",
        "implemented": True,
        "note": "AlloyProvider returns True always",
    },
    "middleware_onion": {
        "web3_usage": "w3.middleware_onion.clear() / .inject()",
        "locations": ["connection_manager.py", "anvil_fork.py", "cli/utils.py"],
        "alloy_equivalent": "None",
        "implemented": False,
        "note": "AlloyProvider does not support middleware. Not applicable for its use case.",
    },
    "provider.make_request": {
        "web3_usage": "w3.provider.make_request(method, params)",
        "locations": ["anvil_fork.py"],
        "alloy_equivalent": "make_request(method, params)",
        "implemented": True,
        "note": "Raw RPC method for calling arbitrary endpoints",
    },
    "batch_requests": {
        "web3_usage": "with w3.batch_requests() as batch:",
        "locations": ["aerodrome/pools.py"],
        "alloy_equivalent": "None",
        "implemented": False,
        "note": "AlloyProvider does not support batch requests. Could be a future enhancement.",
    },
}

# Additional EthereumProvider protocol methods (from interface.py)
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


class TestWeb3MethodsInventory:
    """Document all web3 methods used in the codebase."""

    def test_eth_methods_inventory(self):
        """Print inventory of web3.eth methods found in codebase."""
        print("\n" + "=" * 80)
        print("WEB3.ETH METHODS INVENTORY")
        print("=" * 80)

        implemented = []
        not_implemented = []

        for method, info in WEB3_ETH_METHODS.items():
            status = "✓" if info["implemented"] else "✗"
            print(f"\n{status} {method}")
            print(f"  Web3 usage: {info['web3_usage']}")
            print(f"  Alloy equivalent: {info['alloy_equivalent']}")
            if info.get("locations"):
                print(f"  Used in: {', '.join(info['locations'])}")
            if info.get("note"):
                print(f"  Note: {info['note']}")

            if info["implemented"]:
                implemented.append(method)
            else:
                not_implemented.append(method)

        print("\n" + "-" * 80)
        print(f"Summary: {len(implemented)} implemented, {len(not_implemented)} not implemented")
        print(f"Not implemented: {not_implemented}")

    def test_instance_methods_inventory(self):
        """Print inventory of web3 instance methods found in codebase."""
        print("\n" + "=" * 80)
        print("WEB3 INSTANCE METHODS INVENTORY")
        print("=" * 80)

        implemented = []
        not_implemented = []

        for method, info in WEB3_INSTANCE_METHODS.items():
            status = "✓" if info["implemented"] else "✗"
            print(f"\n{status} {method}")
            print(f"  Web3 usage: {info['web3_usage']}")
            print(f"  Alloy equivalent: {info['alloy_equivalent']}")
            if info.get("locations"):
                print(f"  Used in: {', '.join(info['locations'])}")
            if info.get("note"):
                print(f"  Note: {info['note']}")

            if info["implemented"]:
                implemented.append(method)
            else:
                not_implemented.append(method)

        print("\n" + "-" * 80)
        print(f"Summary: {len(implemented)} implemented, {len(not_implemented)} not implemented")
        print(f"Not implemented: {not_implemented}")


class TestAlloyProviderFeatureParity:
    """Test that AlloyProvider implements equivalent methods for core web3 functionality."""

    @pytest.fixture
    def mock_alloy_provider(self):
        """Create a mock AlloyProvider for signature testing.

        We use a mock because we don't want to require a real RPC endpoint
        just to check method signatures.
        """
        # We'll just check the class has the methods
        return AlloyProvider

    def test_alloy_has_chain_id_property(self, mock_alloy_provider):
        """AlloyProvider must have chain_id property."""
        assert hasattr(mock_alloy_provider, "chain_id")
        assert isinstance(inspect.getattr_static(mock_alloy_provider, "chain_id"), property)

    def test_alloy_has_block_number_property(self, mock_alloy_provider):
        """AlloyProvider must have block_number property."""
        assert hasattr(mock_alloy_provider, "block_number")
        assert isinstance(inspect.getattr_static(mock_alloy_provider, "block_number"), property)

    def test_alloy_has_get_block_number_method(self, mock_alloy_provider):
        """AlloyProvider must have get_block_number method."""
        assert hasattr(mock_alloy_provider, "get_block_number")
        assert callable(mock_alloy_provider.get_block_number)

    def test_alloy_has_get_block_method(self, mock_alloy_provider):
        """AlloyProvider must have get_block method."""
        assert hasattr(mock_alloy_provider, "get_block")
        assert callable(mock_alloy_provider.get_block)

    def test_alloy_has_get_logs_method(self, mock_alloy_provider):
        """AlloyProvider must have get_logs method."""
        assert hasattr(mock_alloy_provider, "get_logs")
        assert callable(mock_alloy_provider.get_logs)

    def test_alloy_has_call_method(self, mock_alloy_provider):
        """AlloyProvider must have call method."""
        assert hasattr(mock_alloy_provider, "call")
        assert callable(mock_alloy_provider.call)

    def test_alloy_has_get_code_method(self, mock_alloy_provider):
        """AlloyProvider must have get_code method."""
        assert hasattr(mock_alloy_provider, "get_code")
        assert callable(mock_alloy_provider.get_code)

    def test_alloy_has_get_storage_at_method(self, mock_alloy_provider):
        """AlloyProvider must have get_storage_at method."""
        assert hasattr(mock_alloy_provider, "get_storage_at")
        assert callable(mock_alloy_provider.get_storage_at)

    def test_alloy_has_is_connected_method(self, mock_alloy_provider):
        """AlloyProvider must have is_connected method."""
        assert hasattr(mock_alloy_provider, "is_connected")
        assert callable(mock_alloy_provider.is_connected)

    def test_alloy_has_get_gas_price_method(self, mock_alloy_provider):
        """AlloyProvider must have get_gas_price method."""
        assert hasattr(mock_alloy_provider, "get_gas_price")
        assert callable(mock_alloy_provider.get_gas_price)

    def test_alloy_has_get_transaction_method(self, mock_alloy_provider):
        """AlloyProvider must have get_transaction method."""
        assert hasattr(mock_alloy_provider, "get_transaction")
        assert callable(mock_alloy_provider.get_transaction)

    def test_alloy_has_get_transaction_receipt_method(self, mock_alloy_provider):
        """AlloyProvider must have get_transaction_receipt method."""
        assert hasattr(mock_alloy_provider, "get_transaction_receipt")
        assert callable(mock_alloy_provider.get_transaction_receipt)

    def test_alloy_has_estimate_gas_method(self, mock_alloy_provider):
        """AlloyProvider must have estimate_gas method."""
        assert hasattr(mock_alloy_provider, "estimate_gas")
        assert callable(mock_alloy_provider.estimate_gas)

    def test_alloy_has_close_method(self, mock_alloy_provider):
        """AlloyProvider must have close method."""
        assert hasattr(mock_alloy_provider, "close")
        assert callable(mock_alloy_provider.close)


class TestAlloyProviderImplementedMethods:
    """Test methods that are now implemented."""

    @pytest.fixture
    def mock_alloy_provider(self):
        """Create a mock AlloyProvider for signature testing."""
        return AlloyProvider

    def test_alloy_has_get_balance_method(self, mock_alloy_provider):
        """AlloyProvider has get_balance method."""
        assert hasattr(mock_alloy_provider, "get_balance")
        assert callable(mock_alloy_provider.get_balance)

    def test_alloy_has_get_transaction_count_method(self, mock_alloy_provider):
        """AlloyProvider has get_transaction_count method."""
        assert hasattr(mock_alloy_provider, "get_transaction_count")
        assert callable(mock_alloy_provider.get_transaction_count)

    def test_alloy_does_not_have_contract_factory(self, mock_alloy_provider):
        """AlloyProvider does not have contract factory method."""
        assert not hasattr(mock_alloy_provider, "contract")

    def test_alloy_does_not_have_batch_requests(self, mock_alloy_provider):
        """AlloyProvider does not have batch_requests method."""
        assert not hasattr(mock_alloy_provider, "batch_requests")

    def test_alloy_does_not_have_middleware_onion(self, mock_alloy_provider):
        """AlloyProvider does not have middleware_onion attribute."""
        assert not hasattr(mock_alloy_provider, "middleware_onion")

    def test_alloy_has_make_request_method(self, mock_alloy_provider):
        """AlloyProvider has make_request method for raw RPC calls."""
        assert hasattr(mock_alloy_provider, "make_request")
        assert callable(mock_alloy_provider.make_request)


class TestAlloyProviderMethodSignatures:
    """Test that AlloyProvider method signatures match expected interface."""

    @pytest.fixture
    def mock_alloy_provider(self):
        """Create a mock AlloyProvider for signature testing."""
        return AlloyProvider

    def test_call_signature(self, mock_alloy_provider):
        """call method should accept (to, data, block_number)."""
        sig = inspect.signature(mock_alloy_provider.call)
        params = list(sig.parameters.keys())
        assert "to" in params
        assert "data" in params
        assert "block_number" in params

    def test_get_code_signature(self, mock_alloy_provider):
        """get_code method should accept (address, block_number)."""
        sig = inspect.signature(mock_alloy_provider.get_code)
        params = list(sig.parameters.keys())
        assert "address" in params
        assert "block_number" in params

    def test_get_storage_at_signature(self, mock_alloy_provider):
        """get_storage_at method should accept (address, position, block_number)."""
        sig = inspect.signature(mock_alloy_provider.get_storage_at)
        params = list(sig.parameters.keys())
        assert "address" in params
        assert "position" in params
        assert "block_number" in params

    def test_get_block_signature(self, mock_alloy_provider):
        """get_block method should accept (block_number)."""
        sig = inspect.signature(mock_alloy_provider.get_block)
        params = list(sig.parameters.keys())
        assert "block_number" in params

    def test_get_logs_signature(self, mock_alloy_provider):
        """get_logs method should accept LogFilter or keyword args."""
        sig = inspect.signature(mock_alloy_provider.get_logs)
        params = sig.parameters
        assert "filter_param" in params
        # from_block and to_block should be keyword-only
        assert "from_block" in params
        assert params["from_block"].kind == inspect.Parameter.KEYWORD_ONLY
        assert "to_block" in params
        assert params["to_block"].kind == inspect.Parameter.KEYWORD_ONLY


class TestEthereumProviderProtocol:
    """Test that AlloyProvider satisfies EthereumProvider protocol."""

    @pytest.fixture
    def mock_alloy_provider(self):
        """Create a mock AlloyProvider for protocol testing."""
        return AlloyProvider

    @pytest.mark.parametrize("method_name", PROVIDER_PROTOCOL_METHODS)
    def test_alloy_has_protocol_method(self, mock_alloy_provider, method_name: str):
        """AlloyProvider must have all EthereumProvider protocol methods."""
        # Properties are also valid protocol members
        has_method = hasattr(mock_alloy_provider, method_name)
        assert has_method, f"AlloyProvider missing protocol method: {method_name}"


class TestProviderAdapterIntegration:
    """Test that ProviderAdapter correctly wraps AlloyProvider."""

    def test_provider_adapter_from_alloy(self):
        """ProviderAdapter can be created from AlloyProvider."""
        from degenbot.provider import ProviderAdapter

        # Just test that the factory method exists and works
        # (we use a mock URL since we're not making real calls)
        assert hasattr(ProviderAdapter, "from_alloy")
        assert callable(ProviderAdapter.from_alloy)


class TestWeb3DirectUsageAudit:
    """Audit codebase for direct web3 usage that should migrate to provider interface."""

    def test_bot_py_uses_w3_eth_call(self):
        """Document that bot.py uses w3.eth.call directly."""
        # This is a documentation test - it should pass but documents what needs migration
        # Read the file to verify the usage exists
        import re

        content = pathlib.Path("src/degenbot/bot.py").read_text()

        # Count occurrences of w3.eth.call
        matches = re.findall(r"w3\.eth\.call\(", content)
        print(f"\nFound {len(matches)} direct w3.eth.call() usages in bot.py")
        assert len(matches) > 0, "Expected to find w3.eth.call usage in bot.py"

    def test_chainlink_py_uses_contract_factory(self):
        """Document that chainlink.py uses w3.eth.contract directly."""
        import re

        content = pathlib.Path("src/degenbot/chainlink.py").read_text()

        matches = re.findall(r"w3\.eth\.contract\(", content)
        print(f"\nFound {len(matches)} direct w3.eth.contract() usages in chainlink.py")
        assert len(matches) > 0, "Expected to find w3.eth.contract usage in chainlink.py"

    def test_anvil_fork_uses_make_request(self):
        """Document that anvil_fork.py uses w3.provider.make_request directly."""
        import re

        content = pathlib.Path("src/degenbot/anvil_fork.py").read_text()

        matches = re.findall(r"w3\.provider\.make_request\(", content)
        print(f"\nFound {len(matches)} direct w3.provider.make_request() usages in anvil_fork.py")
        assert len(matches) > 0, "Expected to find w3.provider.make_request usage in anvil_fork.py"

    def test_aerodrome_pools_uses_batch_requests(self):
        """Document that aerodrome/pools.py uses w3.batch_requests directly."""
        import re

        content = pathlib.Path("src/degenbot/aerodrome/pools.py").read_text()

        matches = re.findall(r"w3\.batch_requests\(\)", content)
        print(f"\nFound {len(matches)} direct w3.batch_requests() usages in aerodrome/pools.py")
        assert len(matches) > 0, "Expected to find w3.batch_requests usage in aerodrome/pools.py"


class TestFeatureParitySummary:
    """Generate a summary of feature parity status."""

    def test_feature_parity_summary(self):
        """Print a comprehensive feature parity summary."""
        print("\n" + "=" * 80)
        print("ALLOY PROVIDER vs WEB3 FEATURE PARITY SUMMARY")
        print("=" * 80)

        total_eth_methods = len(WEB3_ETH_METHODS)
        implemented_eth = sum(1 for m in WEB3_ETH_METHODS.values() if m["implemented"])

        total_instance_methods = len(WEB3_INSTANCE_METHODS)
        implemented_instance = sum(
            1 for m in WEB3_INSTANCE_METHODS.values() if m["implemented"]
        )

        print("\n## web3.eth methods")
        print(f"   Implemented: {implemented_eth}/{total_eth_methods}")

        missing_eth = [m for m, info in WEB3_ETH_METHODS.items() if not info["implemented"]]
        if missing_eth:
            print(f"   Missing: {missing_eth}")
        else:
            print("   ✓ All web3.eth methods implemented!")

        print("\n## web3 instance methods")
        print(f"   Implemented: {implemented_instance}/{total_instance_methods}")

        missing_instance = [
            m for m, info in WEB3_INSTANCE_METHODS.items() if not info["implemented"]
        ]
        if missing_instance:
            print(f"   Missing: {missing_instance}")
        else:
            print("   ✓ All web3 instance methods implemented!")

        print("\n## Priority for implementation")
        print("   1. batch_requests - Performance optimization for bulk queries")
        print("   2. contract factory - Not needed, use degenbot.contract module instead")
        print("   3. middleware_onion - Not applicable to AlloyProvider's design")

        print("\n" + "=" * 80)


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
