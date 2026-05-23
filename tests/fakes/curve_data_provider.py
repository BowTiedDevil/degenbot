"""Reusable test double for CurveDataProvider.

Provides a configurable CurveDataProvider that returns pre-programmed values
for on-chain data access, enabling I/O-free CurveStableswapPool testing.

Usage:
    provider = FakeCurveDataProvider(block_timestamp=1_700_000_000)
    pool = CurveStableswapPool(
        address="0x...",
        tokens=(dai, usdc),
        a_coefficient=2000,
        fee=4000000,
        admin_fee=5000000000,
        balances=(10_000_000 * 10**18, 10_000_000 * 10**6),
        state_block=18_000_000,
        data_provider=provider,
    )
"""


class FakeCurveDataProvider:
    """A fake CurveDataProvider for testing that returns pre-programmed values."""

    def __init__(
        self,
        *,
        virtual_price: int | None = None,
        base_virtual_price: int | None = None,
        base_cache_updated: int | None = None,
        block_timestamp: int = 1_700_000_000,
        redemption_price: int | None = None,
        admin_balances: tuple[int, ...] | None = None,
        D: int | None = None,
        gamma: int | None = None,
        price_scale: tuple[int, ...] | None = None,
        lending_rates: tuple[int, ...] | None = None,
    ) -> None:
        self._virtual_price = virtual_price
        self._base_virtual_price = base_virtual_price
        self._base_cache_updated = base_cache_updated
        self._block_timestamp = block_timestamp
        self._redemption_price = redemption_price
        self._admin_balances = admin_balances
        self._D = D
        self._gamma = gamma
        self._price_scale = price_scale
        self._lending_rates = lending_rates

    def virtual_price(self, block_number: int) -> int:  # noqa: ARG002
        if self._virtual_price is None:
            msg = "virtual_price not configured"
            raise ValueError(msg)
        return self._virtual_price

    def base_virtual_price(self, block_number: int) -> int:  # noqa: ARG002
        if self._base_virtual_price is None:
            msg = "base_virtual_price not configured"
            raise ValueError(msg)
        return self._base_virtual_price

    def base_cache_updated(self, block_number: int) -> int:  # noqa: ARG002
        if self._base_cache_updated is None:
            msg = "base_cache_updated not configured"
            raise ValueError(msg)
        return self._base_cache_updated

    def block_timestamp(self, block_number: int) -> int:  # noqa: ARG002
        return self._block_timestamp

    def block_number(self) -> int:
        return 18_000_000

    def token_balance(self, token_address: str, holder_address: str, block_number: int) -> int:  # noqa: ARG002
        msg = "token_balance not configured"
        raise ValueError(msg)

    def token_total_supply(self, token_address: str, block_number: int) -> int:  # noqa: ARG002
        msg = "token_total_supply not configured"
        raise ValueError(msg)

    def lending_rates(self, block_number: int) -> tuple[int, ...]:  # noqa: ARG002
        if self._lending_rates is None:
            msg = "lending_rates not configured"
            raise ValueError(msg)
        return self._lending_rates

    def redemption_price(self, block_number: int) -> int:  # noqa: ARG002
        if self._redemption_price is None:
            msg = "redemption_price not configured"
            raise ValueError(msg)
        return self._redemption_price

    def admin_balances(self, block_number: int) -> tuple[int, ...]:  # noqa: ARG002
        if self._admin_balances is None:
            msg = "admin_balances not configured"
            raise ValueError(msg)
        return self._admin_balances

    def D(self, block_number: int) -> int:  # noqa: ARG002
        if self._D is None:
            msg = "D not configured"
            raise ValueError(msg)
        return self._D

    # Alias matching the CurveDataProvider protocol's lowercase `d`
    def d(self, block_number: int) -> int:
        return self.D(block_number)

    def gamma(self, block_number: int) -> int:  # noqa: ARG002
        if self._gamma is None:
            msg = "gamma not configured"
            raise ValueError(msg)
        return self._gamma

    def price_scale(self, block_number: int) -> tuple[int, ...]:  # noqa: ARG002
        if self._price_scale is None:
            msg = "price_scale not configured"
            raise ValueError(msg)
        return self._price_scale
