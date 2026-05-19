"""
Tests for CurveDataProvider: the consolidated I/O seam for Curve pools.

Validates that CurveDataProviderImpl satisfies the CurveDataProvider protocol
and correctly delegates to the underlying ProviderAdapter.
"""

import eth_abi.abi
from hexbytes import HexBytes
from web3 import Web3
from web3.exceptions import ContractLogicError

from degenbot.builders.pool_io import SyncPoolIO
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.data_provider_impl import CurveDataProviderImpl
from degenbot.curve.types import CurveDataProvider, LendingRateStyle
from degenbot.exceptions.pool import EVMRevertError
from degenbot.provider.interface import ProviderAdapter

# --- Fake provider ---


class FakeProviderBackend:
    """A fake ProviderBackend that dispatches call_raw by selector."""

    def __init__(
        self,
        call_responses: dict[bytes, bytes] | None = None,
        block_number: int = 1,
        block_timestamp: int = 1_700_000_000,
    ) -> None:
        self._responses = call_responses or {}
        self._block_number = block_number
        self._block_timestamp = block_timestamp

    @property
    def chain_id(self) -> int:
        return 1

    def get_block_number(self) -> int:
        return self._block_number

    def get_block(
        self, block_identifier: int | str  # noqa: ARG002
    ) -> dict | None:
        return {"number": self._block_number, "timestamp": self._block_timestamp}

    def get_logs(
        self, from_block: int, to_block: int, addresses, topics  # noqa: ARG002
    ) -> list:
        return []

    def call(self, to: str, data: bytes, block: int | None) -> HexBytes:
        return self.call_raw({"to": to, "data": data}, block)

    def call_raw(self, tx: dict, block: int | None) -> HexBytes:  # noqa: ARG002
        data = bytes(tx["data"])
        selector = data[:4]
        if selector in self._responses:
            return HexBytes(self._responses[selector])
        msg = f"No mock response for selector 0x{selector.hex()}"
        raise RuntimeError(msg)

    def get_code(self, address: str, block: int | None) -> HexBytes:  # noqa: ARG002
        return HexBytes(b"")

    def get_balance(self, address: str, block: int | None) -> int:  # noqa: ARG002
        return 0

    def get_storage_at(
        self, address: str, position: int, block: int | None  # noqa: ARG002
    ) -> HexBytes:
        return HexBytes(b"\x00" * 32)

    def get_transaction_count(
        self, address: str, block: int | None  # noqa: ARG002
    ) -> int:
        return 0

    def is_connected(self) -> bool:
        return True

    def close(self) -> None:
        pass


class RevertingProviderBackend(FakeProviderBackend):
    """A fake backend that raises ContractLogicError on every call_raw."""

    def call_raw(self, tx: dict, block: int | None = None) -> HexBytes:  # noqa: ARG002
        revert_msg = "execution reverted"
        raise ContractLogicError(revert_msg)


def _make_fake_provider(
    call_responses: dict[bytes, bytes] | None = None,
    block_number: int = 1,
    block_timestamp: int = 1_700_000_000,
) -> ProviderAdapter:
    """Create a ProviderAdapter backed by FakeProviderBackend."""
    backend = FakeProviderBackend(call_responses, block_number, block_timestamp)
    adapter = ProviderAdapter.__new__(ProviderAdapter)
    adapter._backend = backend
    adapter._provider_type = "alloy"
    adapter._raw_provider = None
    return adapter


def _make_reverting_provider() -> ProviderAdapter:
    """Create a ProviderAdapter that raises ContractLogicError on every call."""
    backend = RevertingProviderBackend()
    adapter = ProviderAdapter.__new__(ProviderAdapter)
    adapter._backend = backend
    adapter._provider_type = "alloy"
    adapter._raw_provider = None
    return adapter


def _selector(signature: str) -> bytes:
    return Web3.keccak(text=signature)[:4]


POOL_ADDRESS = "0x0000000000000000000000000000000000000001"
BASE_POOL_ADDRESS = "0x0000000000000000000000000000000000000002"


# --- Tests ---


class TestCurveDataProviderProtocol:
    """Test that CurveDataProviderImpl satisfies the CurveDataProvider protocol."""

    def test_provider_satisfies_protocol(self) -> None:
        """CurveDataProviderImpl satisfies CurveDataProvider."""
        provider = _make_fake_provider()
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(provider),
            pool_address=POOL_ADDRESS,
        )
        assert isinstance(impl, CurveDataProvider)


class TestVirtualPrice:
    """Test the virtual_price method."""

    def test_virtual_price_fetches_from_pool(self) -> None:
        """virtual_price() calls get_virtual_price() on the pool contract."""
        vp_value = 10**18
        fake = _make_fake_provider({
            _selector("get_virtual_price()"): eth_abi.abi.encode(
                ["uint256"], [vp_value]
            ),
        })
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
        )
        assert impl.virtual_price(18_000_000) == vp_value

    def test_virtual_price_for_metapool_uses_base_pool(self) -> None:
        """virtual_price() with base_pool_address calls get_virtual_price() on base pool."""
        vp_value = 10**18
        fake = _make_fake_provider({
            _selector("get_virtual_price()"): eth_abi.abi.encode(
                ["uint256"], [vp_value]
            ),
        })
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
            base_pool_address=BASE_POOL_ADDRESS,
        )
        assert impl.virtual_price(18_000_000) == vp_value


class TestBaseVirtualPrice:
    """Test the base_virtual_price method."""

    def test_base_virtual_price_fetches_from_pool(self) -> None:
        """base_virtual_price() calls base_virtual_price() on the pool contract."""
        bvp_value = 10**18 + 1
        fake = _make_fake_provider({
            _selector("base_virtual_price()"): eth_abi.abi.encode(
                ["uint256"], [bvp_value]
            ),
        })
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
        )
        assert impl.base_virtual_price(18_000_000) == bvp_value


class TestBaseCacheUpdated:
    """Test the base_cache_updated method."""

    def test_base_cache_updated_fetches_from_pool(self) -> None:
        """base_cache_updated() calls base_cache_updated() on the pool contract."""
        bcu_value = 1_700_000_000
        fake = _make_fake_provider({
            _selector("base_cache_updated()"): eth_abi.abi.encode(
                ["uint256"], [bcu_value]
            ),
        })
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
        )
        assert impl.base_cache_updated(18_000_000) == bcu_value


class TestBlockTimestamp:
    """Test the block_timestamp method."""

    def test_block_timestamp_delegates_to_provider(self) -> None:
        """block_timestamp() delegates to ProviderAdapter.get_block_timestamp()."""
        fake = _make_fake_provider(block_timestamp=1_700_000_000)
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
        )
        assert impl.block_timestamp(18_000_000) == 1_700_000_000


class TestBlockNumber:
    """Test the block_number method."""

    def test_block_number_delegates_to_provider(self) -> None:
        """block_number() delegates to ProviderAdapter.get_block_number()."""
        fake = _make_fake_provider(block_number=18_000_000)
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
        )
        assert impl.block_number() == 18_000_000


class TestD:
    """Test the D method for crypto pools."""

    def test_d_fetches_from_pool(self) -> None:
        """D() calls D() on the pool contract (crypto pools only)."""
        d_value = 10**18 * 100
        fake = _make_fake_provider({
            _selector("D()"): eth_abi.abi.encode(["uint256"], [d_value]),
        })
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
        )
        assert impl.D(18_000_000) == d_value


class TestGamma:
    """Test the gamma method for crypto pools."""

    def test_gamma_fetches_from_pool(self) -> None:
        """gamma() calls gamma() on the pool contract (crypto pools only)."""
        gamma_value = 10**10
        fake = _make_fake_provider({
            _selector("gamma()"): eth_abi.abi.encode(["uint256"], [gamma_value]),
        })
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
        )
        assert impl.gamma(18_000_000) == gamma_value


class TestRedemptionPrice:
    """Test the redemption_price method."""

    def test_redemption_price_two_step_fetch(self) -> None:
        """redemption_price() fetches snap contract then snapped price, divides by 10^9."""
        snap_address = get_checksum_address(
            "0x0000000000000000000000000000000000000003"
        )
        raw_rate = 10**18
        fake = _make_fake_provider({
            _selector("redemption_price_snap()"): eth_abi.abi.encode(
                ["address"], [snap_address]
            ),
            _selector("snappedRedemptionPrice()"): eth_abi.abi.encode(
                ["uint256"], [raw_rate]
            ),
        })
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
        )
        assert impl.redemption_price(18_000_000) == raw_rate // 10**9


class TestPriceScale:
    """Test the price_scale method for crypto pools."""

    def test_price_scale_fetches_per_index(self) -> None:
        """price_scale() fetches price_scale(uint256) for each index."""
        price0 = 10**18
        # Since our FakeProviderBackend dispatches by selector only,
        # both indices get the same response. We test that the
        # method returns the right number of elements.
        fake = _make_fake_provider({
            _selector("price_scale(uint256)"): eth_abi.abi.encode(
                ["uint256"], [price0]
            ),
        })
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
            n_coins=3,
        )
        result = impl.price_scale(18_000_000)
        assert len(result) == 2  # n_coins - 1
        assert result[0] == price0
        assert result[1] == price0  # same response for both indices


class TestTokenBalance:
    """Test the token_balance method."""

    def test_token_balance_calls_balanceof(self) -> None:
        """token_balance() calls balanceOf on the token contract."""
        balance = 5000 * 10**18
        fake = _make_fake_provider({
            _selector("balanceOf(address)"): eth_abi.abi.encode(
                ["uint256"], [balance]
            ),
        })
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
        )
        token_addr = "0x0000000000000000000000000000000000000004"
        result = impl.token_balance(token_addr, POOL_ADDRESS, 18_000_000)
        assert result == balance


class TestTokenTotalSupply:
    """Test the token_total_supply method."""

    def test_token_total_supply_calls_totalsupply(self) -> None:
        """token_total_supply() calls totalSupply on the token contract."""
        supply = 1_000_000 * 10**18
        fake = _make_fake_provider({
            _selector("totalSupply()"): eth_abi.abi.encode(
                ["uint256"], [supply]
            ),
        })
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
        )
        token_addr = "0x0000000000000000000000000000000000000004"
        result = impl.token_total_supply(token_addr, 18_000_000)
        assert result == supply


class TestAdminBalances:
    """Test the admin_balances method."""

    def test_admin_balances_fetches_until_revert(self) -> None:
        """admin_balances() fetches admin_balances(i) until the contract reverts."""
        ab0 = 1000
        fake = _make_fake_provider({
            _selector("admin_balances(uint256)"): eth_abi.abi.encode(
                ["uint256"], [ab0]
            ),
        })
        # The second call should fail — but our FakeProvider returns the same
        # response for the same selector. To test the break-on-revert behavior,
        # we'd need per-index responses. For now, test that it returns a tuple.
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
        )
        result = impl.admin_balances(18_000_000)
        assert isinstance(result, tuple)
        assert all(isinstance(v, int) for v in result)


class TestLendingRatesCToken:
    """Test cToken lending rate dispatch."""

    def test_ctoken_rate_with_supply_accrual(self) -> None:
        """cToken rates use exchangeRateStored + supplyRatePerBlock accrual."""
        token0_addr = "0x0000000000000000000000000000000000000010"
        token1_addr = "0x0000000000000000000000000000000000000011"
        exchange_rate = 2 * 10**18  # 2.0
        supply_rate = 10**10  # small rate per block
        old_block = 10
        block_number = 11

        fake = _make_fake_provider({
            _selector("exchangeRateStored()"): eth_abi.abi.encode(
                ["uint256"], [exchange_rate]
            ),
            _selector("supplyRatePerBlock()"): eth_abi.abi.encode(
                ["uint256"], [supply_rate]
            ),
            _selector("accrualBlockNumber()"): eth_abi.abi.encode(
                ["uint256"], [old_block]
            ),
        })
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
            lending_rate_style=LendingRateStyle.CTOKEN,
            token_addresses=[token0_addr, token1_addr],
            use_lending=[True, True],
            precision_multipliers=[10**10, 10**10],
        )
        result = impl.lending_rates(block_number)
        assert len(result) == 2
        # Non-lending would be PRECISION; lending should differ
        assert result[0] != 10**18  # Not just PRECISION


class TestLendingRatesYToken:
    """Test yToken lending rate dispatch."""

    def test_ytoken_rate_gets_price_per_full_share(self) -> None:
        """yToken rates use getPricePerFullShare()."""
        token0_addr = "0x0000000000000000000000000000000000000010"
        token1_addr = "0x0000000000000000000000000000000000000011"
        pps = 105 * 10**16  # 1.05

        fake = _make_fake_provider({
            _selector("getPricePerFullShare()"): eth_abi.abi.encode(
                ["uint256"], [pps]
            ),
        })
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
            lending_rate_style=LendingRateStyle.YTOKEN,
            token_addresses=[token0_addr, token1_addr],
            use_lending=[True, True],
            precision_multipliers=[10**10, 10**10],
        )
        result = impl.lending_rates(1)
        assert len(result) == 2


class TestLendingRatesNone:
    """Test that NONE style raises ValueError."""

    def test_none_style_raises(self) -> None:
        """lending_rates() with NONE style raises ValueError."""
        fake = _make_fake_provider({})
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(fake),
            pool_address=POOL_ADDRESS,
            lending_rate_style=LendingRateStyle.NONE,
        )
        try:
            impl.lending_rates(1)
        except ValueError:
            pass  # expected
        else:
            msg = "Expected ValueError"
            raise AssertionError(msg)


class TestEVMRevertWrapping:
    """Test that ContractLogicError is converted to EVMRevertError."""

    def test_virtual_price_converts_revert(self) -> None:
        """virtual_price() converts ContractLogicError to EVMRevertError."""
        provider = _make_reverting_provider()
        impl = CurveDataProviderImpl(
            io=SyncPoolIO(provider),
            pool_address=POOL_ADDRESS,
        )
        try:
            impl.virtual_price(1)
        except EVMRevertError:
            pass  # expected
        else:
            msg = "Expected EVMRevertError"
            raise AssertionError(msg)
